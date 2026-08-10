//! lat.md importer: an "Agent Lattice" markdown knowledge graph → `authored`
//! facts.
//!
//! lat.md (<https://github.com/1st1/lat.md>) stores knowledge as markdown files
//! in a `lat.md/` directory: headings are sections, `[[file#Section]]` links join
//! sections, and `[[src/x.rs#Symbol]]` links reach into code. That model mirrors
//! Roteiro's own ADR `[[path#Symbol]]` + `@rto:` layer, so lat content imports as
//! **`authored`** facts — a `doc` node per file, a `lat_section` node per
//! heading, `contains` edges for structure, and `references` edges for links.
//! Links into code are validated by the durable-import layer, which prunes any
//! that dangle.
//!
//! lat also supports `// @lat: [[section]]` backlinks *from* source code (like
//! `@rto:`). [`scan_lat_annotations`] finds those on comment lines and
//! [`import_lat_backlinks`] resolves them (via [`resolve_lat_ref`]) into
//! `authored` `references` edges from the annotated file to the lat section,
//! stamped [`LAT_REF`] so they live in — and are re-derived with — the lat layer.

use std::collections::BTreeMap;

use rto_graph::{Edge, EdgeKind, FactSet, Node, NodeKind, Provenance};

use crate::annotate::is_comment_line;
use crate::text::{scan_wiki_links, slugify};

/// `src_ref` stamped on every edge imported from lat.md, so it can be told apart
/// from other `authored` edges (ADRs) and re-derived authoritatively on re-import.
pub const LAT_REF: &str = "import:lat";

/// The result of importing a lat.md directory: the authored facts and a report.
#[derive(Debug, Clone)]
pub struct LatImport {
    /// Authored nodes and edges to apply to the store.
    pub facts: FactSet,
    /// An auditable summary of what was imported.
    pub report: LatReport,
}

/// An auditable summary of a lat.md import.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct LatReport {
    /// lat.md markdown files processed.
    pub files: usize,
    /// Section (heading) nodes emitted.
    pub sections: usize,
    /// Total `[[…]]` links found.
    pub links_total: usize,
    /// Links resolved to another lat section/doc.
    pub links_to_sections: usize,
    /// Links resolved to a code symbol or file.
    pub links_to_code: usize,
    /// `@lat:` backlinks found in source comments that resolved to a lat section.
    pub backlinks_resolved: usize,
    /// `@lat:` backlinks whose reference named no known lat file (dropped).
    pub backlinks_unresolved: usize,
}

/// Import a lat.md directory. `files` are `(repo-relative path, content)` pairs
/// for the markdown under `lat.md/`. Cross-file section links are resolved
/// against the set of files provided.
#[must_use]
pub fn import_lat(files: &[(String, String)]) -> LatImport {
    let index = LatIndex::build(files);
    let mut facts = FactSet::new();
    let mut report = LatReport::default();
    for (path, content) in files {
        report.files += 1;
        import_file(path, content, &index, &mut facts, &mut report);
    }
    LatImport { facts, report }
}

/// The natural key of a lat.md file's `doc` node.
fn doc_key(path: &str) -> String {
    format!("lat:{path}")
}

/// The natural key of a section within a lat.md file.
fn section_key(path: &str, slug: &str) -> String {
    format!("lat:{path}#{slug}")
}

/// A lat reference (in a `[[…]]` link or a `@lat:` annotation) resolved to the
/// graph node key it targets, if it names a known lat file.
///
/// Resolves `stem` / `stem.md` / `stem#Section#Subsection` against the file set:
/// the doc node when no section is given, else the deepest section's node. Code
/// links (a path with a `/` or a non-`.md` extension) are **not** resolved here —
/// [`resolve_link`] handles those. Returns `None` when the file is unknown.
#[must_use]
pub fn resolve_lat_ref(files: &[(String, String)], raw: &str) -> Option<String> {
    LatIndex::build(files).resolve_section(raw)
}

/// The marker introducing a lat backlink in a source comment (`// @lat: [[…]]`).
const LAT_MARKER: &str = "@lat:";

/// A `@lat:` backlink found in a source file: a code→lat reference carried in a
/// `[[…]]` wiki-link on a comment line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatAnnotation {
    /// Repository-relative path of the file the annotation is in.
    pub path: String,
    /// The raw lat reference from the `[[…]]` link (e.g. `auth#OAuth Flow`).
    pub reference: String,
    /// 1-based line number.
    pub line: usize,
}

/// Find every `@lat:` backlink in `text`, tagged with `rel_path`. Recognition is
/// restricted to comment lines (as with `@rto:`) so example tokens in string
/// literals are not mistaken for real backlinks, and the reference must be a
/// `[[…]]` wiki-link so a lat name containing spaces is delimited unambiguously.
/// A single comment may carry several (`// @lat: [[a#x]] [[b#y]]`).
#[must_use]
pub fn scan_lat_annotations(rel_path: &str, text: &str) -> Vec<LatAnnotation> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if !is_comment_line(line) {
            continue;
        }
        // Strip inline code spans so a documented `` `@lat: [[x]]` `` example is
        // not counted, then take the wiki-links after the marker.
        let stripped = crate::text::strip_code_spans(line);
        let Some(pos) = stripped.find(LAT_MARKER) else {
            continue;
        };
        let after = &stripped[pos + LAT_MARKER.len()..];
        for reference in scan_wiki_links(after) {
            out.push(LatAnnotation {
                path: rel_path.to_owned(),
                reference,
                line: i + 1,
            });
        }
    }
    out
}

/// Resolve `@lat:` backlinks against the lat file set, returning `authored`
/// `references` edges (`file:<path>` → lat section, stamped [`LAT_REF`]) and the
/// number that named no known lat file. Only lat-section references resolve here;
/// a backlink to a non-lat target is dropped (and counted unresolved).
#[must_use]
pub fn import_lat_backlinks(
    files: &[(String, String)],
    annotations: &[LatAnnotation],
) -> (Vec<Edge>, usize) {
    let index = LatIndex::build(files);
    let mut edges = Vec::new();
    let mut unresolved = 0;
    // The same reference can appear several times in one file (repeated markers,
    // or several on a line); collapse to one edge per (file, section) pair so we
    // never persist duplicates.
    let mut seen = std::collections::BTreeSet::new();
    for ann in annotations {
        if let Some(target) = index.resolve_section(&ann.reference) {
            let src = format!("file:{}", ann.path);
            if seen.insert((src.clone(), target.clone())) {
                edges.push(lat_edge(src, target, EdgeKind::References));
            }
        } else {
            unresolved += 1;
        }
    }
    (edges, unresolved)
}

/// Index of lat file stems → repo-relative paths, for resolving `[[stem#…]]`
/// section references.
struct LatIndex {
    by_stem: BTreeMap<String, String>,
}

impl LatIndex {
    fn build(files: &[(String, String)]) -> Self {
        let mut by_stem = BTreeMap::new();
        for (path, _) in files {
            by_stem.entry(stem_of(path)).or_insert_with(|| path.clone());
        }
        Self { by_stem }
    }

    /// Whether `head` (the part before the first `#`) names a lat file rather
    /// than a code path: no path separator, and either no extension or `.md`.
    fn is_lat_file(&self, head: &str) -> bool {
        let bare = !head.contains('/')
            && head
                .rsplit_once('.')
                .is_none_or(|(_, ext)| ext.eq_ignore_ascii_case("md"));
        bare && self.by_stem.contains_key(&stem_of(head))
    }

    /// Resolve a lat section reference (`stem`, `stem#Section`, …) to a node key,
    /// or `None` if `head` is not a known lat file.
    fn resolve_section(&self, raw: &str) -> Option<String> {
        let (head, rest) = split_head(raw);
        if !self.is_lat_file(head) {
            return None;
        }
        let path = self.by_stem.get(&stem_of(head))?;
        match rest {
            // The deepest `#`-separated segment names the target heading.
            Some(section) => {
                let leaf = section.rsplit('#').next().unwrap_or(section).trim();
                Some(section_key(path, &slugify(leaf)))
            }
            None => Some(doc_key(path)),
        }
    }
}

/// Build an `authored` lat.md edge stamped with [`LAT_REF`] in `src_ref`, so a
/// re-import can clear and replace the whole lat layer authoritatively (the
/// store deletes prior edges by `src_ref` in `apply_import_layer`).
fn lat_edge(src: String, dst: String, kind: EdgeKind) -> Edge {
    let mut edge = Edge::authored(src, dst, kind);
    edge.src_ref = Some(LAT_REF.to_owned());
    edge
}

/// Split a reference into `(head, rest)` at the first `#`.
fn split_head(raw: &str) -> (&str, Option<&str>) {
    match raw.split_once('#') {
        Some((h, r)) => (h.trim(), Some(r.trim())),
        None => (raw.trim(), None),
    }
}

/// The file stem (basename without extension) of a path or bare name.
fn stem_of(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.')
        .map_or(name, |(stem, _)| stem)
        .to_ascii_lowercase()
}

/// Import one lat.md file into `facts`, emitting the doc node, section nodes with
/// `contains` structure, and `references` edges for its links.
fn import_file(
    path: &str,
    content: &str,
    index: &LatIndex,
    facts: &mut FactSet,
    report: &mut LatReport,
) {
    let doc = doc_key(path);
    // Section stack of (heading level, node key) for `contains` nesting.
    let mut stack: Vec<(usize, String)> = Vec::new();
    let mut title: Option<String> = None;
    let mut in_fence = false;

    for line in content.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if let Some((level, heading)) = heading(line) {
            title.get_or_insert_with(|| heading.to_owned());
            let key = section_key(path, &slugify(heading));
            let mut node = Node::new(key.clone(), NodeKind::Other("lat_section".into()), heading)
                .with_provenance(Provenance::Authored);
            node.path = Some(path.to_owned());
            facts.nodes.push(node);
            report.sections += 1;

            // Parent is the nearest shallower heading, else the doc.
            while stack.last().is_some_and(|(l, _)| *l >= level) {
                stack.pop();
            }
            let parent = stack.last().map_or(doc.clone(), |(_, k)| k.clone());
            facts
                .edges
                .push(lat_edge(parent, key.clone(), EdgeKind::Contains));
            stack.push((level, key));
            continue;
        }
        // A link is attributed to the enclosing section, or the doc if none yet.
        let from = stack.last().map_or(doc.clone(), |(_, k)| k.clone());
        for raw in scan_wiki_links(line) {
            report.links_total += 1;
            if let Some((target, to_code)) = resolve_link(index, &raw) {
                if to_code {
                    report.links_to_code += 1;
                } else {
                    report.links_to_sections += 1;
                }
                facts
                    .edges
                    .push(lat_edge(from.clone(), target, EdgeKind::References));
            }
        }
    }

    let name = title.unwrap_or_else(|| stem_of(path));
    let mut node =
        Node::new(doc.clone(), NodeKind::Doc, name).with_provenance(Provenance::Authored);
    node.path = Some(path.to_owned());
    // Emit the doc node last so it is present; order does not affect the store.
    facts.nodes.push(node);
}

/// Resolve a `[[…]]` link to `(target key, is_code)`. Lat section links resolve
/// via the file index; anything else is treated as a code/file reference like an
/// ADR wiki-link.
fn resolve_link(index: &LatIndex, raw: &str) -> Option<(String, bool)> {
    if let Some(section) = index.resolve_section(raw) {
        return Some((section, false));
    }
    let (head, rest) = split_head(raw);
    if head.is_empty() {
        return None;
    }
    let key = match rest.filter(|s| !s.is_empty()) {
        Some(symbol) => format!("sym:{}:{head}#{symbol}", crate::text::lang_for(head)),
        None => format!("file:{head}"),
    };
    Some((key, true))
}

/// A `#{1,6} ` heading's `(level, text)`, if `line` is an ATX heading.
fn heading(line: &str) -> Option<(usize, &str)> {
    let hashes = line.len() - line.trim_start_matches('#').len();
    if (1..=6).contains(&hashes) && line.as_bytes().get(hashes) == Some(&b' ') {
        Some((hashes, line[hashes + 1..].trim()))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{LAT_REF, import_lat, resolve_lat_ref};
    use rto_graph::{EdgeKind, NodeKind};

    fn files() -> Vec<(String, String)> {
        vec![
            (
                "lat.md/architecture.md".to_owned(),
                "# Architecture\n\nThe system. See [[auth#OAuth Flow]].\n\n\
                 ## Request Pipeline\n\nHandled in [[src/server.rs#run]].\n"
                    .to_owned(),
            ),
            (
                "lat.md/auth.md".to_owned(),
                "# Auth\n\n## OAuth Flow\n\nTokens via [[src/auth.rs#validate]].\n".to_owned(),
            ),
        ]
    }

    #[test]
    fn imports_docs_sections_and_contains() {
        let imp = import_lat(&files());
        let keys: Vec<_> = imp.facts.nodes.iter().map(|n| n.key.as_str()).collect();
        assert!(keys.contains(&"lat:lat.md/architecture.md"));
        assert!(keys.contains(&"lat:lat.md/architecture.md#architecture"));
        assert!(keys.contains(&"lat:lat.md/architecture.md#request-pipeline"));
        assert!(keys.contains(&"lat:lat.md/auth.md#oauth-flow"));
        // Section nodes are authored `lat_section`s; the file is a `doc`.
        let sec = imp
            .facts
            .nodes
            .iter()
            .find(|n| n.key == "lat:lat.md/auth.md#oauth-flow")
            .unwrap();
        assert_eq!(sec.kind, NodeKind::Other("lat_section".into()));
        // `contains` nests the subsection under the file (doc → section).
        assert!(imp.facts.edges.iter().any(|e| e.kind == EdgeKind::Contains
            && e.src == "lat:lat.md/auth.md"
            && e.dst == "lat:lat.md/auth.md#auth"));
        assert_eq!(imp.report.files, 2);
    }

    #[test]
    fn resolves_lat_and_code_links() {
        let imp = import_lat(&files());
        // A cross-file section link → the target section node (authored).
        assert!(
            imp.facts
                .edges
                .iter()
                .any(|e| e.kind == EdgeKind::References
                    && e.src == "lat:lat.md/architecture.md#architecture"
                    && e.dst == "lat:lat.md/auth.md#oauth-flow")
        );
        // A code link → a `sym:` key, attributed to its enclosing section.
        assert!(
            imp.facts
                .edges
                .iter()
                .any(|e| e.kind == EdgeKind::References
                    && e.src == "lat:lat.md/architecture.md#request-pipeline"
                    && e.dst == "sym:rust:src/server.rs#run")
        );
        assert_eq!(imp.report.links_to_sections, 1);
        assert_eq!(imp.report.links_to_code, 2);
        // Every imported edge is authored and stamped with LAT_REF, so a
        // re-import can clear the whole layer authoritatively by src_ref.
        assert!(imp.facts.edges.iter().all(|e| {
            e.provenance.as_str() == "authored" && e.src_ref.as_deref() == Some(LAT_REF)
        }));
    }

    #[test]
    fn resolve_ref_distinguishes_lat_from_code() {
        let f = files();
        // A bare stem resolves to a lat section.
        assert_eq!(
            resolve_lat_ref(&f, "auth#OAuth Flow").as_deref(),
            Some("lat:lat.md/auth.md#oauth-flow")
        );
        // A path with a slash is code, not a lat file → unresolved here.
        assert_eq!(resolve_lat_ref(&f, "src/auth.rs#validate"), None);
        // A bare file name resolves to the doc node.
        assert_eq!(
            resolve_lat_ref(&f, "architecture").as_deref(),
            Some("lat:lat.md/architecture.md")
        );
    }

    #[test]
    fn ref_marker_is_stable() {
        assert_eq!(LAT_REF, "import:lat");
    }

    #[test]
    fn scans_lat_backlinks_only_on_comment_lines() {
        use super::scan_lat_annotations;
        let src = "// @lat: [[auth#OAuth Flow]]\n\
                   fn f() {}\n\
                   let s = \"@lat: [[architecture]]\";\n\
                   /* see @lat: [[architecture#Request Pipeline]] and [[auth]] */\n";
        let anns = scan_lat_annotations("src/auth.rs", src);
        // The string-literal one is skipped; the comment ones (3 refs) are kept.
        assert_eq!(anns.len(), 3);
        assert_eq!(anns[0].reference, "auth#OAuth Flow");
        assert_eq!(anns[0].line, 1);
        assert_eq!(anns[1].reference, "architecture#Request Pipeline");
        assert_eq!(anns[1].line, 4);
        assert_eq!(anns[2].reference, "auth");
    }

    #[test]
    fn imports_backlinks_as_authored_file_to_section_edges() {
        use super::{import_lat_backlinks, scan_lat_annotations};
        let f = files();
        let anns = scan_lat_annotations("src/auth.rs", "// @lat: [[auth#OAuth Flow]]\n");
        let (edges, unresolved) = import_lat_backlinks(&f, &anns);
        assert_eq!(unresolved, 0);
        assert_eq!(edges.len(), 1);
        let e = &edges[0];
        assert_eq!(e.src, "file:src/auth.rs");
        assert_eq!(e.dst, "lat:lat.md/auth.md#oauth-flow");
        assert_eq!(e.kind, EdgeKind::References);
        assert_eq!(e.provenance.as_str(), "authored");
        assert_eq!(e.src_ref.as_deref(), Some(LAT_REF));
    }

    #[test]
    fn repeated_backlinks_in_a_file_collapse_to_one_edge() {
        use super::{import_lat_backlinks, scan_lat_annotations};
        let f = files();
        // The same reference twice (two comment lines) → a single edge.
        let anns = scan_lat_annotations(
            "src/auth.rs",
            "// @lat: [[auth#OAuth Flow]]\n// @lat: [[auth#OAuth Flow]]\n",
        );
        assert_eq!(anns.len(), 2, "both annotations are scanned");
        let (edges, unresolved) = import_lat_backlinks(&f, &anns);
        assert_eq!(unresolved, 0);
        assert_eq!(edges.len(), 1, "duplicate (file, section) edge collapsed");

        // The same reference twice on ONE comment line also collapses.
        let same_line = scan_lat_annotations(
            "src/auth.rs",
            "// @lat: [[auth#OAuth Flow]] [[auth#OAuth Flow]]\n",
        );
        assert_eq!(same_line.len(), 2, "both refs on the line are scanned");
        let (edges, _) = import_lat_backlinks(&f, &same_line);
        assert_eq!(edges.len(), 1, "same-line duplicate collapsed");
    }

    #[test]
    fn backlink_to_unknown_lat_file_is_unresolved() {
        use super::{import_lat_backlinks, scan_lat_annotations};
        let f = files();
        // `nope` is not a known lat file, and a code path never resolves as a
        // backlink target.
        let anns = scan_lat_annotations(
            "src/x.rs",
            "// @lat: [[nope#Section]]\n// @lat: [[src/auth.rs#validate]]\n",
        );
        let (edges, unresolved) = import_lat_backlinks(&f, &anns);
        assert!(edges.is_empty());
        assert_eq!(unresolved, 2);
    }
}

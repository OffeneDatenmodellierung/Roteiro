//! House-style ADR parsing: frontmatter metadata, section structure, and the
//! `[[path#Symbol]]` wiki-links that form the *authored* layer over code.
//!
//! The frontmatter is hand-parsed rather than run through a YAML crate: it is a
//! flat `key: value` block that also contains `#` comment lines (which a strict
//! YAML parser handles differently), and hand-parsing keeps `rto-spec`
//! dependency-free (no `serde_yaml`, which is unmaintained and would trip the
//! audit gate). See ADR-0001 / `docs/BUILD_PLAN.md` Q4.

use rto_graph::{Edge, EdgeKind, FactSet, Node, NodeKind, Provenance};
use serde::{Deserialize, Serialize};

/// ADR lifecycle states, exactly as the house style defines them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AdrStatus {
    /// Being drafted.
    Draft,
    /// Circulated for advisory review.
    ForReview,
    /// Decision accepted.
    Accepted,
    /// Decision rejected.
    Rejected,
    /// Replaced by a later ADR.
    Superseded,
}

impl AdrStatus {
    /// The canonical house-style label for this status.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "Draft",
            Self::ForReview => "For Review",
            Self::Accepted => "Accepted",
            Self::Rejected => "Rejected",
            Self::Superseded => "Superseded",
        }
    }

    /// Whether an ADR in this state is a valid target for a `@rto:` annotation
    /// (i.e. still authoritative — not rejected or superseded).
    #[must_use]
    pub fn is_active(self) -> bool {
        matches!(self, Self::Draft | Self::ForReview | Self::Accepted)
    }
}

/// Errors raised while parsing ADR metadata.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ParseError {
    /// The status string is not one of the five house-style states.
    #[error("unknown ADR status: {0}")]
    UnknownStatus(String),
    /// The frontmatter lacks the required `adr-id` field.
    #[error("missing required frontmatter field: adr-id")]
    MissingAdrId,
}

impl std::str::FromStr for AdrStatus {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Draft" => Ok(Self::Draft),
            "For Review" => Ok(Self::ForReview),
            "Accepted" => Ok(Self::Accepted),
            "Rejected" => Ok(Self::Rejected),
            "Superseded" => Ok(Self::Superseded),
            other => Err(ParseError::UnknownStatus(other.to_owned())),
        }
    }
}

/// A two-component ADR **document** version, e.g. `1.10`.
///
/// Compared component-wise rather than lexically or as a decimal, because
/// both of those readings sort `1.10` *below* `1.9` — and this repository has
/// an ADR that reached 1.11 one row at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DocVersion {
    /// The part before the dot.
    pub major: u32,
    /// The part after it. `10` is a later revision than `9`, not an earlier one.
    pub minor: u32,
}

impl DocVersion {
    /// Parse a string that is *exactly* `X.Y`.
    ///
    /// A third component makes this `None`: `1.13.0` is a crate release, and
    /// reading its first two parts as a document version is the mistake this
    /// function exists to refuse.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let (major, minor) = s.split_once('.')?;
        let digits = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
        if !digits(major) || !digits(minor) {
            return None;
        }
        Some(Self {
            major: major.parse().ok()?,
            minor: minor.parse().ok()?,
        })
    }

    /// Parse a leading `X.Y` from `s`, ignoring whatever follows — but still
    /// refusing a third `.N` component, for the reason [`Self::parse`] gives.
    fn parse_prefix(s: &str) -> Option<Self> {
        let b = s.as_bytes();
        let run = |from: usize| {
            let mut i = from;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            i
        };
        let major_end = run(0);
        if major_end == 0 || b.get(major_end) != Some(&b'.') {
            return None;
        }
        let minor_end = run(major_end + 1);
        if minor_end == major_end + 1 || b.get(minor_end) == Some(&b'.') {
            return None;
        }
        Self::parse(&s[..minor_end])
    }
}

impl std::fmt::Display for DocVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// One `(Update, vX.Y…)` note found in an ADR body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineVersionRef {
    /// 1-based line number **in the file**, frontmatter included.
    pub line: usize,
    /// The document version the note names.
    pub version: DocVersion,
}

/// Every claim an ADR makes about its own version, gathered so
/// [`crate::check::validate`] can cross-check them against each other.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VersionFacts {
    /// The version cell of the `| **Document version** | X.Y |` summary row.
    pub summary_row: Option<DocVersion>,
    /// The first cell of each version-history row, in document order.
    pub history: Vec<DocVersion>,
    /// `(Update, vX.Y…)` notes in the body, **excluding** the version-history
    /// section — a history row that describes removing such a note quotes it,
    /// and quoting it is not making the claim again.
    pub inline_refs: Vec<InlineVersionRef>,
}

/// Metadata for one ADR, as read from its frontmatter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdrMeta {
    /// Zero-padded ADR id, e.g. `0001`.
    pub id: String,
    /// Current title (evolves with the decision).
    pub title: String,
    /// Lifecycle state.
    pub status: AdrStatus,
    /// The `version:` frontmatter field, when it parses as `X.Y`.
    pub version: Option<DocVersion>,
}

/// One `## ` section of an ADR body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// URL-safe slug derived from the heading.
    pub slug: String,
    /// Heading text.
    pub title: String,
    /// The section's body: everything between this `## ` heading and the next
    /// one, verbatim and **uncapped**, with surrounding blank lines trimmed.
    ///
    /// The heading line itself is excluded — a section's note is already titled
    /// by it, and a `### ` subheading inside the span is body text and stays.
    /// [`AdrDoc::facts`] caps this before it reaches the store; the vault renders
    /// it whole. Empty when a heading is immediately followed by another.
    ///
    /// Populated by [`parse_adr`] only. [`crate::blueprint`] and [`crate::site`]
    /// share this struct and leave it empty — their section notes have the same
    /// defect #545 fixes here, and fixing them is the same shape of change on a
    /// different document class.
    pub text: String,
}

/// A `[[path#Symbol]]` (or `[[path]]`) authored link found in an ADR, resolved
/// to the graph node key it should point at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WikiLink {
    /// Node key of the ADR or section the link appears in.
    pub from: String,
    /// The raw link text between the brackets.
    pub raw: String,
    /// The graph node key the link targets.
    pub target_key: String,
}

/// A fully-parsed ADR: metadata, section structure, and authored links.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdrDoc {
    /// Frontmatter metadata.
    pub meta: AdrMeta,
    /// Repository-relative path of the ADR file.
    pub path: String,
    /// `## ` sections in document order.
    pub sections: Vec<Section>,
    /// The body text *before* the first `## ` heading — in house style, the `# `
    /// title and the summary table — verbatim and uncapped.
    ///
    /// This is the only part of an ADR that belongs to no section, which is
    /// exactly why the `adr` node carries it and not the whole document: the
    /// sections already hold the body between them, so nothing is stored twice.
    /// The whole document, for a reader who wants it, is on the `file:` node.
    pub preamble: String,
    /// Authored `[[…]]` links in document order.
    pub links: Vec<WikiLink>,
    /// What the document says about its own version, in three places.
    pub versions: VersionFacts,
}

impl AdrDoc {
    /// The natural key of this ADR's node (`adr:<id>`).
    #[must_use]
    pub fn key(&self) -> String {
        format!("adr:{}", self.meta.id)
    }

    /// The **full, uncapped** text the node `key` should show, or `None` if `key`
    /// names no part of this ADR (or names an empty one).
    ///
    /// The inverse of the key grammar [`Self::key`] and [`Self::facts`] build, and
    /// deliberately their neighbour: a renderer that re-split `adr:0015#consequences`
    /// with its own rule would be reimplementing the thing it is trying to read.
    ///
    /// The split is the point. `adr:0015` gets its preamble and a section gets its
    /// own span — never the whole document, which is what a path-only rule would
    /// hand to all twenty ADR notes and all 179 section notes alike, beside the
    /// `file:` note that already carries it once.
    #[must_use]
    pub fn text_for_key(&self, key: &str) -> Option<&str> {
        let rest = key.strip_prefix(&self.key())?;
        let text = if rest.is_empty() {
            self.preamble.as_str()
        } else {
            // `adr:00151` also strips the `adr:0015` prefix; requiring the `#` is
            // what refuses it rather than reading `1` as a slug.
            let slug = rest.strip_prefix('#')?;
            &self.sections.iter().find(|s| s.slug == slug)?.text
        };
        (!text.is_empty()).then_some(text)
    }

    /// The authored nodes and structural edges for this ADR: an `adr` node, one
    /// `adr_section` node per section, and `contains` edges between them. Wiki
    /// links are *not* included — they are validated against the code graph by
    /// [`crate::check`] before becoming edges.
    #[must_use]
    pub fn facts(&self) -> FactSet {
        let adr_key = self.key();
        let mut adr = Node::new(adr_key.clone(), NodeKind::Adr, self.meta.title.clone())
            .with_provenance(Provenance::Authored);
        adr.path = Some(self.path.clone());
        adr.meta = serde_json::json!({ "status": self.meta.status.as_str() });
        if let Some(content) = stored(&self.preamble) {
            adr.meta["content"] = content;
        }
        let mut fs = FactSet::new().with_node(adr);

        for section in &self.sections {
            let key = format!("{adr_key}#{}", section.slug);
            let mut node = Node::new(key.clone(), NodeKind::AdrSection, section.title.clone())
                .with_provenance(Provenance::Authored);
            node.path = Some(self.path.clone());
            if let Some(content) = stored(&section.text) {
                node.meta = serde_json::json!({ "content": content });
            }
            fs = fs.with_node(node).with_edge(Edge::authored(
                adr_key.clone(),
                key,
                EdgeKind::Contains,
            ));
        }
        fs
    }
}

/// The `meta.content` value for one span of an ADR, or `None` when the span is
/// empty (a heading immediately followed by another) — an empty string would be a
/// key that says nothing, and `search`/`duplicates` both gate on content being
/// non-empty.
///
/// Capped by [`rto_graph::cap_content`] rather than by a bound of this module's
/// own: the store is exportable and ships with the graph, so authored text lands
/// in it under the same budget the derived layer uses. The vault does not read
/// this — it renders the uncapped text from the blob (see [`AdrDoc::text_for_key`]).
fn stored(text: &str) -> Option<serde_json::Value> {
    let capped = rto_graph::cap_content(text);
    (!capped.is_empty()).then(|| serde_json::Value::from(capped))
}

/// Parse an ADR markdown document at `rel_path`.
///
/// # Errors
/// Returns [`ParseError::MissingAdrId`] if the frontmatter has no `adr-id`, or
/// [`ParseError::UnknownStatus`] if the `status` value is not a house state.
pub fn parse_adr(rel_path: &str, text: &str) -> Result<AdrDoc, ParseError> {
    let (frontmatter, body) = split_frontmatter(text);
    // Violations point at the file, so the frontmatter just consumed has to be
    // counted back in: `body` is a suffix of `text`, so its offset is the split.
    let body_offset = text.len() - body.len();
    let body_line1 = text[..body_offset].lines().count() + 1;

    let mut id = None;
    let mut status = AdrStatus::Draft;
    let mut fm_title = None;
    let mut fm_version = None;
    for line in frontmatter.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = clean_value(value);
        match key.trim().to_ascii_lowercase().as_str() {
            "adr-id" => id = Some(value.to_owned()),
            "status" if !value.is_empty() => status = value.parse()?,
            "title" => fm_title = Some(value.to_owned()),
            "version" => fm_version = DocVersion::parse(value),
            _ => {}
        }
    }
    let id = id
        .filter(|s| !s.is_empty())
        .ok_or(ParseError::MissingAdrId)?;

    let title = fm_title
        .filter(|s| !s.is_empty())
        .or_else(|| crate::text::first_h1(body))
        .unwrap_or_else(|| format!("ADR-{id}"));

    let scan = scan_body(&id, body, body_line1);

    Ok(AdrDoc {
        meta: AdrMeta {
            id,
            title,
            status,
            version: fm_version,
        },
        path: rel_path.to_owned(),
        sections: scan.sections,
        preamble: scan.preamble,
        links: scan.links,
        versions: scan.versions,
    })
}

/// Everything one pass over an ADR body yields.
///
/// Grouped into a struct rather than returned as a tuple of four because three of
/// the four are only meaningful together: a `## ` heading simultaneously ends one
/// span, opens the next, decides whether the rows that follow are version history,
/// and re-attributes every `[[…]]` link after it.
struct BodyScan {
    /// Body text before the first `## `.
    preamble: String,
    /// `## ` sections in document order, each carrying its own span.
    sections: Vec<Section>,
    /// Authored `[[…]]` links, attributed to the section they appear in.
    links: Vec<WikiLink>,
    /// What the document says about its own version, in three places.
    versions: VersionFacts,
}

/// Walk `body` once, tracking the current section so links are attributed to it
/// and each `## ` span can be sliced back out.
///
/// Fenced code blocks are skipped so documented examples of `[[…]]` syntax are not
/// mistaken for real authored links — and, for the same reason, a `## ` line inside
/// a fence is body text rather than a section boundary.
///
/// `body_line1` is the 1-based line number `body` starts at in the file, so a
/// violation can point at the file rather than at the post-frontmatter offset.
fn scan_body(id: &str, body: &str, body_line1: usize) -> BodyScan {
    let mut sections: Vec<Section> = Vec::new();
    let mut links = Vec::new();
    let mut versions = VersionFacts::default();
    let mut current: Option<String> = None;
    let mut in_fence = false;
    let mut in_history = false;
    // Byte offsets into `body`, so each `## ` span can be sliced back out of it:
    // `span_start` is where the open span's text begins (just past its heading
    // line), and `preamble_end` is fixed by the first heading. Tracked here rather
    // than re-derived by a second pass, because this loop already knows where
    // every heading is and which of them are inside a code fence.
    let mut byte_offset = 0usize;
    let mut span_start = 0usize;
    let mut preamble_end: Option<usize> = None;
    for (line_idx, line) in body.lines().enumerate() {
        // Advance the cursor first: every `continue` below still consumes a line,
        // and a fenced line's bytes belong to whichever section encloses it.
        // `str::lines` strips a `\r\n` terminator as well as a `\n`.
        let line_start = byte_offset;
        byte_offset += line.len();
        if body[byte_offset..].starts_with("\r\n") {
            byte_offset += 2;
        } else if body[byte_offset..].starts_with('\n') {
            byte_offset += 1;
        }

        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if let Some(heading) = line.strip_prefix("## ") {
            let title = heading.trim().to_owned();
            in_history = is_version_history(&title);
            let slug = crate::text::slugify(&title);
            current = Some(slug.clone());
            // Close the span this heading ends — the preamble if it is the first.
            match sections.last_mut() {
                Some(prev) => {
                    body[span_start..line_start]
                        .trim()
                        .clone_into(&mut prev.text);
                }
                None => preamble_end = Some(line_start),
            }
            span_start = byte_offset;
            sections.push(Section {
                slug,
                title,
                text: String::new(),
            });
        }
        if in_history {
            versions.history.extend(history_row_version(line));
        } else {
            versions.summary_row = versions.summary_row.or_else(|| summary_row_version(line));
            let file_line = body_line1 + line_idx;
            versions
                .inline_refs
                .extend(inline_version_refs(line).map(|version| InlineVersionRef {
                    line: file_line,
                    version,
                }));
        }
        for raw in crate::text::scan_wiki_links(line) {
            let from = match &current {
                Some(slug) => format!("adr:{id}#{slug}"),
                None => format!("adr:{id}"),
            };
            if let Some(target_key) = resolve_target(&raw) {
                links.push(WikiLink {
                    from,
                    raw,
                    target_key,
                });
            }
        }
    }

    // Close the last open span at end of document; with no `## ` at all the whole
    // body is preamble.
    if let Some(last) = sections.last_mut() {
        body[span_start..].trim().clone_into(&mut last.text);
    }
    let preamble = body[..preamble_end.unwrap_or(body.len())].trim().to_owned();

    BodyScan {
        preamble,
        sections,
        links,
        versions,
    }
}

/// Whether a `## ` heading opens the version-history table. Both spellings are
/// in use across this repository's own ADRs, so both are recognised rather than
/// one being declared canonical by a parser.
fn is_version_history(title: &str) -> bool {
    title.eq_ignore_ascii_case("Document version history")
        || title.eq_ignore_ascii_case("Version history")
}

/// The version in a table row's first cell, when that cell holds a version and
/// nothing else. The header (`| Version | …`) and separator (`|---|…`) rows fail
/// to parse, which is exactly how they are skipped.
fn history_row_version(line: &str) -> Option<DocVersion> {
    let rest = line.trim_start().strip_prefix('|')?;
    let (first, _) = rest.split_once('|')?;
    DocVersion::parse(first.trim())
}

/// The version in the `| **Document version** | X.Y |` row of the summary table.
fn summary_row_version(line: &str) -> Option<DocVersion> {
    let mut cells = line.trim_start().strip_prefix('|')?.split('|');
    if cells.next()?.trim() != "**Document version**" {
        return None;
    }
    DocVersion::parse(cells.next()?.trim())
}

/// Every `(Update, vX.Y…)` note on one line.
///
/// Anchored on the literal marker rather than on a bare `vX.Y`, because ADR
/// bodies are full of *software* versions — `v1.13.0` for a crate release,
/// `v0.9.7` for boxlite — and a loose scan reads their leading components as a
/// document version. On this repository's 20 ADRs the marker occurs 4 times and
/// a bare `vX.Y` scan occurs over 40, nearly all of them releases.
fn inline_version_refs(line: &str) -> impl Iterator<Item = DocVersion> + '_ {
    const MARK: &str = "(Update, v";
    line.match_indices(MARK)
        .filter_map(|(i, _)| DocVersion::parse_prefix(&line[i + MARK.len()..]))
}

/// Split leading `---`-delimited frontmatter from the body. Returns
/// `("", text)` when there is no frontmatter. Shared with [`crate::site`],
/// whose publication marker is a frontmatter field read the same way.
pub(crate) fn split_frontmatter(text: &str) -> (&str, &str) {
    let Some(rest) = text.strip_prefix("---\n") else {
        return ("", text);
    };
    match rest.find("\n---\n") {
        Some(end) => (&rest[..end], &rest[end + 5..]),
        // A closing fence with no trailing newline (end of file).
        None => match rest.strip_suffix("\n---") {
            Some(fm) => (fm, ""),
            None => ("", text),
        },
    }
}

/// Clean a raw frontmatter value: trim, drop a trailing ` #…` inline comment
/// (YAML-style) from unquoted values, then strip surrounding quotes. Quoted
/// values are left intact so a `#` inside quotes survives. Shared with
/// [`crate::site`] so a site page's frontmatter is read by the same rules as an
/// ADR's — a quoted slug, or a trailing comment, must not mean two things.
pub(crate) fn clean_value(raw: &str) -> &str {
    let raw = raw.trim();
    if raw.starts_with('"') || raw.starts_with('\'') {
        return strip_quotes(raw);
    }
    match raw.find(" #") {
        Some(idx) => raw[..idx].trim_end(),
        None => raw,
    }
}

/// Strip a single pair of surrounding single or double quotes.
fn strip_quotes(s: &str) -> &str {
    for q in ['"', '\''] {
        if let Some(inner) = s.strip_prefix(q).and_then(|s| s.strip_suffix(q)) {
            return inner;
        }
    }
    s
}

/// Resolve a wiki-link's inner text to a graph node key: `path#Symbol` →
/// `sym:<lang>:<path>#<Symbol>`, or `path` → `file:<path>`. Shared with
/// [`crate::blueprint`], whose links resolve the same way.
pub(crate) fn resolve_target(raw: &str) -> Option<String> {
    let (path, symbol) = match raw.split_once('#') {
        Some((p, s)) => (p.trim(), Some(s.trim())),
        None => (raw.trim(), None),
    };
    if path.is_empty() {
        return None;
    }
    match symbol.filter(|s| !s.is_empty()) {
        Some(symbol) => {
            let lang = crate::text::lang_for(path);
            Some(format!("sym:{lang}:{path}#{symbol}"))
        }
        None => Some(format!("file:{path}")),
    }
}

#[cfg(test)]
mod tests {
    use super::{AdrStatus, parse_adr};
    use crate::text::slugify;

    /// An ADR with a preamble, two sections of clearly distinguishable prose, a
    /// `## `-looking line inside a code fence, and an empty trailing section.
    const SPANS: &str = "---\nadr-id: \"0015\"\nstatus: Accepted\n---\n\n# ADR-0015: Spans\n\n| | |\n|---|---|\n| **State** | Accepted |\n\n## Context\n\nALPHA the context prose.\n\n```md\n## Not A Heading\nALPHA fenced.\n```\n\n## Consequences\n\nBRAVO the consequences prose.\n\n### A subheading\n\nBRAVO more.\n\n## Empty\n";

    /// The whole defect and the whole trap in one test: each section note gets
    /// **its own** span and never the document. A path-only rule — the shape the
    /// `prose_blob_oid` kind check in #544 exists to refuse — would put every one
    /// of these strings in every one of these nodes.
    #[test]
    fn a_section_carries_its_own_body_and_not_the_document() {
        let doc = parse_adr("docs/adr/0015-spans.md", SPANS).expect("parse");
        let by = |slug: &str| {
            doc.sections
                .iter()
                .find(|s| s.slug == slug)
                .unwrap_or_else(|| panic!("no section {slug}"))
        };

        let context = &by("context").text;
        assert!(
            context.contains("ALPHA the context prose."),
            "the section keeps its own prose: {context:?}"
        );
        assert!(
            !context.contains("BRAVO"),
            "and not the next section's: {context:?}"
        );

        let consequences = &by("consequences").text;
        assert!(
            consequences.contains("BRAVO the consequences prose."),
            "{consequences:?}"
        );
        assert!(
            consequences.contains("### A subheading"),
            "a `###` inside the span is body text, not a boundary: {consequences:?}"
        );
        assert!(
            !consequences.contains("ALPHA"),
            "and not the previous section's: {consequences:?}"
        );

        // A heading line ends the span before it and does not open the next one.
        assert!(
            !context.contains("## Consequences"),
            "the boundary heading is excluded: {context:?}"
        );
        assert!(
            !consequences.starts_with("## "),
            "a section note is already titled by its heading: {consequences:?}"
        );

        // A `## ` inside a fence is body text of the section that encloses it —
        // the same rule the link scanner already applies to `[[…]]`.
        assert_eq!(
            doc.sections
                .iter()
                .map(|s| s.slug.as_str())
                .collect::<Vec<_>>(),
            ["context", "consequences", "empty"],
            "a fenced `## ` line does not open a section"
        );
        assert!(
            context.contains("## Not A Heading"),
            "the fenced line stays inside the section that encloses it: {context:?}"
        );

        // A heading immediately followed by end-of-document has no text at all,
        // and `stored` turns that into no `content` key rather than an empty one.
        assert_eq!(by("empty").text, "");
    }

    /// The `adr:NNNN` node's share: the span belonging to no section. Storing the
    /// whole document here instead would hold every section's text twice — once on
    /// the ADR node and once on the section that owns it.
    #[test]
    fn the_preamble_is_the_span_that_belongs_to_no_section() {
        let doc = parse_adr("docs/adr/0015-spans.md", SPANS).expect("parse");
        assert!(
            doc.preamble.contains("# ADR-0015: Spans"),
            "{:?}",
            doc.preamble
        );
        assert!(
            doc.preamble.contains("| **State** | Accepted |"),
            "the summary table is ADR-level, not section-level: {:?}",
            doc.preamble
        );
        assert!(
            !doc.preamble.contains("ALPHA") && !doc.preamble.contains("BRAVO"),
            "no section body: {:?}",
            doc.preamble
        );
        // Frontmatter is not body text and never reaches a note.
        assert!(!doc.preamble.contains("adr-id"), "{:?}", doc.preamble);
    }

    /// A document with no `## ` at all is all preamble — the loop must close the
    /// open span at end-of-document rather than dropping it.
    #[test]
    fn a_sectionless_adr_is_all_preamble() {
        let doc = parse_adr(
            "docs/adr/0099-x.md",
            "---\nadr-id: \"0099\"\n---\n\n# ADR-0099\n\nJust prose.\n",
        )
        .expect("parse");
        assert!(doc.sections.is_empty());
        assert!(doc.preamble.contains("Just prose."), "{:?}", doc.preamble);
    }

    /// The store half. The text reaches `meta.content` **capped**, because the
    /// store is exportable and ships with the graph; the vault reads the uncapped
    /// text from the blob instead.
    #[test]
    fn facts_store_the_section_text_capped() {
        let long = "x".repeat(4000);
        let src = format!(
            "---\nadr-id: \"0021\"\nstatus: Accepted\n---\n\n# ADR-0021\n\n## Context\n\n{long}\n"
        );
        let doc = parse_adr("docs/adr/0021-x.md", &src).expect("parse");
        let facts = doc.facts();

        let section = facts
            .nodes
            .iter()
            .find(|n| n.key == "adr:0021#context")
            .expect("section node");
        let stored = section.meta["content"].as_str().expect("content");
        assert_eq!(
            stored.chars().count(),
            1500,
            "capped by the same budget the derived layer uses"
        );
        assert!(
            doc.sections[0].text.chars().count() > stored.chars().count(),
            "the parsed span itself stays whole — only the store is capped"
        );

        // The ADR node keeps its status and gains its preamble.
        let adr = facts
            .nodes
            .iter()
            .find(|n| n.key == "adr:0021")
            .expect("adr node");
        assert_eq!(adr.meta["status"], "Accepted");
        assert!(
            adr.meta["content"]
                .as_str()
                .expect("preamble")
                .contains("ADR-0021"),
            "{:?}",
            adr.meta
        );
        assert!(
            !adr.meta["content"]
                .as_str()
                .expect("preamble")
                .contains("xxxx"),
            "the ADR node does not restate its sections: {:?}",
            adr.meta
        );
    }

    /// An empty span stores no `content` key at all. `search` and
    /// `infer::duplicates` both gate on content being non-empty, so an empty
    /// string would be a key that says nothing while claiming to say something.
    #[test]
    fn an_empty_section_stores_no_content_key() {
        let doc = parse_adr("docs/adr/0015-spans.md", SPANS).expect("parse");
        let facts = doc.facts();
        let empty = facts
            .nodes
            .iter()
            .find(|n| n.key == "adr:0015#empty")
            .expect("node");
        assert!(empty.meta.get("content").is_none(), "{:?}", empty.meta);
    }

    /// The render half's seam: a node key maps back to exactly the span it names.
    #[test]
    fn text_for_key_maps_a_key_back_to_its_span() {
        let doc = parse_adr("docs/adr/0015-spans.md", SPANS).expect("parse");

        assert!(
            doc.text_for_key("adr:0015")
                .expect("preamble")
                .contains("# ADR-0015: Spans")
        );
        assert!(
            doc.text_for_key("adr:0015#consequences")
                .expect("section")
                .contains("BRAVO the consequences prose.")
        );
        assert!(
            !doc.text_for_key("adr:0015#consequences")
                .expect("section")
                .contains("ALPHA"),
            "a section key never resolves to the document"
        );

        // An empty span is `None`, so the note falls back rather than rendering a
        // blank `## Content`.
        assert_eq!(doc.text_for_key("adr:0015#empty"), None);
        // Keys that are not this ADR's.
        assert_eq!(doc.text_for_key("adr:0015#nosuch"), None);
        assert_eq!(doc.text_for_key("adr:0016#context"), None);
        assert_eq!(doc.text_for_key("file:docs/adr/0015-spans.md"), None);
        // `adr:00151` shares the `adr:0015` prefix; requiring the `#` refuses it
        // rather than reading `1` as a slug.
        assert_eq!(doc.text_for_key("adr:00151"), None);
    }

    #[test]
    fn parses_all_house_statuses() {
        for (s, want) in [
            ("Draft", AdrStatus::Draft),
            ("For Review", AdrStatus::ForReview),
            ("Accepted", AdrStatus::Accepted),
            ("Rejected", AdrStatus::Rejected),
            ("Superseded", AdrStatus::Superseded),
        ] {
            assert_eq!(s.parse::<AdrStatus>().expect("parse"), want);
        }
    }

    #[test]
    fn rejects_unknown_status() {
        assert!("Pending".parse::<AdrStatus>().is_err());
    }

    const ADR: &str = "---\nTitle: Example decision\ntype: adr\n# a comment line\nadr-id: \"0007\"\nstatus: Accepted\n---\n\n# ADR-0007: Example decision\n\n## Context\n\nThis relates to [[crates/rto-graph/src/store.rs#Store]].\n\n## Decision\n\nSee [[docs/adr/0001-x.md]] and a broken one [[]].\n";

    #[test]
    fn parses_frontmatter_sections_and_links() {
        let doc = parse_adr("docs/adr/0007-example.md", ADR).expect("parse");
        assert_eq!(doc.meta.id, "0007");
        assert_eq!(doc.meta.title, "Example decision");
        assert_eq!(doc.meta.status, AdrStatus::Accepted);

        let slugs: Vec<_> = doc.sections.iter().map(|s| s.slug.as_str()).collect();
        assert_eq!(slugs, ["context", "decision"]);

        // Two resolvable links (the empty `[[]]` is ignored).
        assert_eq!(doc.links.len(), 2);
        assert_eq!(doc.links[0].from, "adr:0007#context");
        assert_eq!(
            doc.links[0].target_key,
            "sym:rust:crates/rto-graph/src/store.rs#Store"
        );
        assert_eq!(doc.links[1].from, "adr:0007#decision");
        assert_eq!(doc.links[1].target_key, "file:docs/adr/0001-x.md");
    }

    #[test]
    fn adr_facts_carry_status_and_sections() {
        let doc = parse_adr("docs/adr/0007-example.md", ADR).expect("parse");
        let fs = doc.facts();
        assert!(fs.nodes.iter().any(|n| n.key == "adr:0007"));
        assert!(fs.nodes.iter().any(|n| n.key == "adr:0007#context"));
        let adr = fs
            .nodes
            .iter()
            .find(|n| n.key == "adr:0007")
            .expect("adr node");
        assert_eq!(adr.meta["status"], "Accepted");
        // Every ADR node is tagged as the authored layer (so a derived-only sync
        // leaves them alone).
        assert!(
            fs.nodes
                .iter()
                .all(|n| n.provenance == rto_graph::Provenance::Authored),
            "ADR nodes must be Authored"
        );
        // adr contains its sections.
        assert_eq!(fs.edges.iter().filter(|e| e.src == "adr:0007").count(), 2);
    }

    #[test]
    fn missing_adr_id_is_an_error() {
        let text = "---\nTitle: No id\nstatus: Draft\n---\n\n# Body\n";
        assert_eq!(
            parse_adr("x.md", text),
            Err(super::ParseError::MissingAdrId)
        );
    }

    #[test]
    fn slugify_collapses_punctuation() {
        assert_eq!(
            slugify("Options considered + consequences"),
            "options-considered-consequences"
        );
        assert_eq!(slugify("  Reference  "), "reference");
    }

    #[test]
    fn an_adr_title_falling_back_to_its_h1_carries_no_markup() {
        // `title:` in frontmatter wins; without it the H1 is the ADR node's
        // title, and an anchored or emphasised H1 must still name the decision.
        let adr = parse_adr(
            "docs/adr/0021-x.md",
            "---\nadr-id: 0021\nstatus: Accepted\n---\n\n# Sandboxed *linting* {#lint}\n",
        )
        .expect("parse");
        assert_eq!(adr.meta.title, "Sandboxed linting");
    }
}

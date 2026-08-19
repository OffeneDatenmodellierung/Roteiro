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
        let mut fs = FactSet::new().with_node(adr);

        for section in &self.sections {
            let key = format!("{adr_key}#{}", section.slug);
            let mut node = Node::new(key.clone(), NodeKind::AdrSection, section.title.clone())
                .with_provenance(Provenance::Authored);
            node.path = Some(self.path.clone());
            fs = fs.with_node(node).with_edge(Edge::authored(
                adr_key.clone(),
                key,
                EdgeKind::Contains,
            ));
        }
        fs
    }
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
        .or_else(|| first_h1(body))
        .unwrap_or_else(|| format!("ADR-{id}"));

    // Walk the body, tracking the current section so links are attributed to it.
    // Fenced code blocks are skipped so documented examples of `[[…]]` syntax
    // are not mistaken for real authored links.
    let mut sections = Vec::new();
    let mut links = Vec::new();
    let mut versions = VersionFacts::default();
    let mut current: Option<String> = None;
    let mut in_fence = false;
    let mut in_history = false;
    for (offset, line) in body.lines().enumerate() {
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
            sections.push(Section { slug, title });
        }
        if in_history {
            versions.history.extend(history_row_version(line));
        } else {
            versions.summary_row = versions.summary_row.or_else(|| summary_row_version(line));
            let file_line = body_line1 + offset;
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

    Ok(AdrDoc {
        meta: AdrMeta {
            id,
            title,
            status,
            version: fm_version,
        },
        path: rel_path.to_owned(),
        sections,
        links,
        versions,
    })
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

/// The text of the first `# ` heading in `body`, if any. Shared with
/// [`crate::blueprint`] for title extraction and blueprint detection.
pub(crate) fn first_h1(body: &str) -> Option<String> {
    body.lines()
        .find_map(|l| l.strip_prefix("# ").map(|h| h.trim().to_owned()))
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
}

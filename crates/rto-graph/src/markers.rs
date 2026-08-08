//! Intent-debt detection: the deterministic scan that turns comment markers,
//! stub macros, and deferral notes into `marker` nodes (see ADR-0001's provenance
//! model — these are `derived`, a pure function of the blob bytes).
//!
//! "Intent debt" is the class of signals that say *something is missing* or
//! *left for later*: `TODO`/`FIXME`/`HACK` comments, `todo!()`/`unimplemented!()`
//! stubs, "not implemented" panics, and deferral phrases (`for now`, `deferred`,
//! unchecked `- [ ]` items). The scan is line-based and language-agnostic so it
//! works uniformly over code, docs, and ADRs; [`augment`] attaches each finding
//! to its innermost enclosing symbol (or the file) via a `contains` edge.

use crate::{Edge, EdgeKind, FactSet, Node, NodeKind, Span};

/// The category of an intent-debt [`Marker`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerCategory {
    /// A `TODO`: planned work.
    Todo,
    /// A `FIXME`/`BUG`: a known defect to fix.
    Fixme,
    /// A `HACK`/`XXX`: a deliberate but unsatisfactory shortcut.
    Hack,
    /// A stub / not-yet-implemented placeholder (`todo!()`, `unimplemented!()`,
    /// "not implemented", "placeholder").
    Stub,
    /// Work deliberately deferred ("for now", "deferred", "follow-up", "TBD",
    /// unchecked `- [ ]` items).
    Deferred,
}

impl MarkerCategory {
    /// Stable string token, as stored in a marker node's `meta.category`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Todo => "todo",
            Self::Fixme => "fixme",
            Self::Hack => "hack",
            Self::Stub => "stub",
            Self::Deferred => "deferred",
        }
    }
}

/// One detected intent-debt finding, located within a single source blob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marker {
    /// What kind of debt this is.
    pub category: MarkerCategory,
    /// The (trimmed, length-capped) text of the line the marker was found on.
    pub text: String,
    /// 1-based line number within the blob.
    pub line: u32,
    /// Byte span of the line within the blob.
    pub span: Span,
}

/// How a rule's needle is matched against a line.
enum Mode {
    /// Case-sensitive substring (for the macro/call syntax `todo!(`).
    Substr,
    /// Case-sensitive whole word (for the uppercase `TODO`-style tags).
    Word,
    /// Case-insensitive whole word/phrase (for prose deferral notes).
    Phrase,
}

/// The detection table, checked top to bottom; the first hit on a line wins, so
/// more specific rules precede more general ones. Kept intentionally small and
/// high-signal — this is a report, not a gate, but noise still erodes trust.
const RULES: &[(&str, MarkerCategory, Mode)] = &[
    ("todo!(", MarkerCategory::Stub, Mode::Substr),
    ("unimplemented!(", MarkerCategory::Stub, Mode::Substr),
    ("TODO", MarkerCategory::Todo, Mode::Word),
    ("FIXME", MarkerCategory::Fixme, Mode::Word),
    ("BUG", MarkerCategory::Fixme, Mode::Word),
    ("HACK", MarkerCategory::Hack, Mode::Word),
    ("XXX", MarkerCategory::Hack, Mode::Word),
    ("not yet implemented", MarkerCategory::Stub, Mode::Phrase),
    ("not implemented", MarkerCategory::Stub, Mode::Phrase),
    ("placeholder", MarkerCategory::Stub, Mode::Phrase),
    ("for now", MarkerCategory::Deferred, Mode::Phrase),
    ("deferred", MarkerCategory::Deferred, Mode::Phrase),
    ("follow-up", MarkerCategory::Deferred, Mode::Phrase),
    ("followup", MarkerCategory::Deferred, Mode::Phrase),
    ("TBD", MarkerCategory::Deferred, Mode::Word),
];

/// Maximum stored marker text length (in characters), to keep nodes small.
const MAX_TEXT: usize = 200;

/// Scan `bytes` for intent-debt markers, one (highest-priority) per line, in
/// ascending line order. Deterministic: identical bytes always yield identical
/// markers.
#[must_use]
pub fn scan_markers(bytes: &[u8]) -> Vec<Marker> {
    let mut out = Vec::new();
    let mut offset: u32 = 0;
    for (idx, raw) in bytes.split(|&b| b == b'\n').enumerate() {
        let raw_len = u32::try_from(raw.len()).unwrap_or(u32::MAX);
        let decoded = String::from_utf8_lossy(raw);
        let line = decoded.trim_end_matches('\r');
        if let Some(category) = classify(line) {
            out.push(Marker {
                category,
                text: cap_text(line),
                line: u32::try_from(idx + 1).unwrap_or(u32::MAX),
                span: Span::new(offset, offset.saturating_add(raw_len)),
            });
        }
        // +1 for the '\n' that `split` consumed; the trailing empty element for a
        // file ending in a newline classifies to `None`, so the overrun is inert.
        offset = offset.saturating_add(raw_len).saturating_add(1);
    }
    out
}

/// Classify a single line into a marker category, or `None`.
fn classify(line: &str) -> Option<MarkerCategory> {
    for (needle, category, mode) in RULES {
        let hit = match mode {
            Mode::Substr => line.contains(needle),
            Mode::Word => find_bounded(line, needle, false).is_some(),
            Mode::Phrase => find_bounded(line, needle, true).is_some(),
        };
        if hit {
            return Some(*category);
        }
    }
    // A markdown unchecked task item is a deferred-work signal on its own.
    let t = line.trim_start();
    if t.starts_with("- [ ]") || t.starts_with("* [ ]") || t.starts_with("+ [ ]") {
        return Some(MarkerCategory::Deferred);
    }
    None
}

/// Find `needle` in `hay` at word boundaries (the byte before and after the
/// match must not be alphanumeric or `_`). When `ci`, the match is
/// ASCII-case-insensitive; `needle` must already be lowercase in that case.
/// `to_ascii_lowercase` preserves byte length, so match offsets stay aligned.
fn find_bounded(hay: &str, needle: &str, ci: bool) -> Option<usize> {
    let lowered;
    let h: &str = if ci {
        lowered = hay.to_ascii_lowercase();
        &lowered
    } else {
        hay
    };
    let bytes = h.as_bytes();
    let mut from = 0;
    while let Some(rel) = h[from..].find(needle) {
        let start = from + rel;
        let end = start + needle.len();
        let before_ok = start == 0 || !is_word_byte(bytes[start - 1]);
        let after_ok = end >= bytes.len() || !is_word_byte(bytes[end]);
        if before_ok && after_ok {
            return Some(start);
        }
        from = start + 1;
    }
    None
}

/// Whether `b` continues an identifier word (so a match touching it is not a
/// standalone token).
fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Trim surrounding whitespace and cap to [`MAX_TEXT`] characters (appending an
/// ellipsis when truncated).
fn cap_text(line: &str) -> String {
    let t = line.trim();
    if t.chars().count() > MAX_TEXT {
        let mut s: String = t.chars().take(MAX_TEXT).collect();
        s.push('…');
        s
    } else {
        t.to_owned()
    }
}

/// Scan `bytes` and append a `marker` node and a `contains` edge per finding to
/// `facts`. Each marker is attached to the innermost node in `facts` whose span
/// encloses it (a symbol when the extractor produced one, otherwise the file).
///
/// Called by [`crate::Registry`] after the language extractor runs, so markers
/// are cached alongside the rest of a blob's facts.
pub fn augment(facts: &mut FactSet, path: &str, blob_id: &str, bytes: &[u8]) {
    for m in scan_markers(bytes) {
        let key = format!("marker:{path}#{}", m.line);
        let container = innermost_container(&facts.nodes, m.span.start)
            .unwrap_or_else(|| format!("file:{path}"));
        facts.nodes.push(Node {
            key: key.clone(),
            kind: NodeKind::Marker,
            name: cap_name(&m.text),
            path: Some(path.to_owned()),
            lang: None,
            blob_hash: Some(blob_id.to_owned()),
            span: Some(m.span),
            meta: serde_json::json!({
                "category": m.category.as_str(),
                "text": m.text,
                "line": m.line,
            }),
        });
        facts
            .edges
            .push(Edge::derived(container, key, EdgeKind::Contains));
    }
}

/// The key of the node with the smallest span that encloses byte `offset`
/// (ties broken by key for determinism), if any.
fn innermost_container(nodes: &[Node], offset: u32) -> Option<String> {
    nodes
        .iter()
        .filter_map(|n| n.span.map(|s| (n, s)))
        .filter(|(_, s)| s.start <= offset && offset < s.end)
        .min_by(|(a, sa), (b, sb)| (sa.end - sa.start, &a.key).cmp(&(sb.end - sb.start, &b.key)))
        .map(|(n, _)| n.key.clone())
}

/// A compact node name: the first 80 characters of the marker text.
fn cap_name(text: &str) -> String {
    const MAX_NAME: usize = 80;
    if text.chars().count() > MAX_NAME {
        let mut s: String = text.chars().take(MAX_NAME).collect();
        s.push('…');
        s
    } else {
        text.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::{MarkerCategory, augment, scan_markers};
    use crate::{EdgeKind, FactSet, Node, NodeKind, Span};

    fn categories(src: &str) -> Vec<(u32, MarkerCategory)> {
        scan_markers(src.as_bytes())
            .into_iter()
            .map(|m| (m.line, m.category))
            .collect()
    }

    #[test]
    fn detects_each_category() {
        let src = "\
// TODO wire this up
let x = todo!();
// FIXME off-by-one
// HACK relies on ordering
// this is a placeholder for now
- [ ] finish the docs
plain line, nothing here
";
        let got = categories(src);
        assert_eq!(got[0], (1, MarkerCategory::Todo));
        assert_eq!(got[1], (2, MarkerCategory::Stub)); // todo!(
        assert_eq!(got[2], (3, MarkerCategory::Fixme));
        assert_eq!(got[3], (4, MarkerCategory::Hack));
        assert_eq!(got[4], (5, MarkerCategory::Stub)); // placeholder wins over "for now"
        assert_eq!(got[5], (6, MarkerCategory::Deferred)); // checkbox
        assert_eq!(got.len(), 6, "the plain line is not a marker");
    }

    #[test]
    fn word_boundaries_avoid_false_positives() {
        // Substrings inside larger identifiers/words must not match.
        assert!(categories("let bugfix = 1; // mastodon todos").is_empty());
        // But a real standalone tag does.
        assert_eq!(categories("x // BUG here")[0].1, MarkerCategory::Fixme);
    }

    #[test]
    fn scanning_is_deterministic() {
        let src = b"// TODO one\ncode\n// FIXME two\n";
        assert_eq!(scan_markers(src), scan_markers(src));
    }

    #[test]
    fn augment_attaches_to_innermost_symbol_then_file() {
        // A file node spanning everything, and a symbol node spanning bytes 10..40.
        let mut facts = FactSet::new()
            .with_node(Node {
                span: Some(Span::new(0, 100)),
                ..Node::new("file:a.rs", NodeKind::File, "a.rs")
            })
            .with_node(Node {
                span: Some(Span::new(10, 40)),
                ..Node::new("sym:rust:a.rs#f", NodeKind::Fn, "f")
            });
        // Line 1 (offset 0) is a plain line outside the symbol; line 2 starts at
        // offset 20, inside the symbol's 10..40 span.
        let bytes = b"aaaaaaaaaaaaaaaaaaa\n// FIXME inside fn f\n";
        augment(&mut facts, "a.rs", "blob", bytes);

        let marker = facts
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Marker)
            .expect("a marker node");
        assert_eq!(marker.meta["category"], "fixme");
        // The contains edge comes from the enclosing symbol, not the file.
        let edge = facts
            .edges
            .iter()
            .find(|e| e.kind == EdgeKind::Contains && e.dst == marker.key)
            .expect("a contains edge");
        assert_eq!(edge.src, "sym:rust:a.rs#f");
    }

    #[test]
    fn augment_falls_back_to_file_when_no_symbol_encloses() {
        let mut facts = FactSet::new().with_node(Node {
            span: Some(Span::new(0, 100)),
            ..Node::new("file:a.rs", NodeKind::File, "a.rs")
        });
        augment(&mut facts, "a.rs", "blob", b"// TODO top of file\n");
        let marker = facts
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Marker)
            .unwrap();
        let edge = facts
            .edges
            .iter()
            .find(|e| e.kind == EdgeKind::Contains && e.dst == marker.key)
            .unwrap();
        assert_eq!(edge.src, "file:a.rs");
    }
}

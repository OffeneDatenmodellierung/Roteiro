//! Small Markdown text helpers shared by the ADR and annotation scanners.

/// Best-effort language token from a file extension, **lowercased** to mirror the
/// extractor (which treats extensions case-insensitively, so `FOO.RS` and
/// `foo.rs` both yield `rust`) — otherwise an authored `[[FOO.RS#Bar]]` link would
/// build `sym:RS:…` while the graph holds `sym:rust:…` and never resolve. Shared
/// by the ADR and lat.md symbol-key builders.
pub(crate) fn lang_for(path: &str) -> String {
    match path
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("rs") => "rust".to_owned(),
        Some(other) => other.to_owned(),
        None => "text".to_owned(),
    }
}

/// A URL-safe slug for the ADR, site-page and lat.md section keys.
///
/// Re-exported from [`rto_graph::slugify`] rather than written here: the same
/// rule decides the `id` attribute `rto_render` puts on the rendered heading, and
/// a section key that disagrees with its own anchor is a link that resolves in
/// the graph and scrolls nowhere in the browser. See that function for the full
/// argument.
pub(crate) use rto_graph::slugify;

/// The `id` a `## ` heading claims — its explicit `{#id}` when the author wrote
/// one, else [`slugify`] of its visible text.
///
/// Re-exported for the same reason as [`slugify`], and it is the half that was
/// missing: this crate built every section key by slugifying the text even when
/// the heading declared an address of its own, so a page's `{#offline}` anchor
/// and its `site:modes#1-offline-mode-…` node key named different places (#524).
///
/// # What that does and does not now guarantee
///
/// The **base** id is one rule and both sides compute it here, so a heading that
/// declares an address is keyed by it. `rto_render` then applies two rules this
/// crate does not, because only a renderer emits elements: an id that would be
/// empty (`## ###`) falls back to the heading's position, and a repeat gets a
/// `-2` suffix, since two elements sharing an `id` means one is unreachable.
///
/// So a heading hitting either of those still diverges. Neither is reachable in
/// this repository today — no page has an untitled or a duplicated heading —
/// and `heading_anchor_agreement.rs` compares the two sides for every published
/// page, so it is that test rather than this sentence that says whether they
/// agree.
pub(crate) use rto_graph::heading_id;

/// The visible text of a document's first `# ` heading, and of a `## ` heading's
/// source content.
///
/// Re-exported from [`rto_graph`] for the same reason as [`slugify`] above, and
/// they are two halves of one rule: the title these return is what `slugify`
/// then turns into a key. Read with the Markdown parser rather than scanned, so
/// an `{#anchor}`, a code span or an inline link ends where the *dialect* says it
/// does — and so a heading cannot mean one thing in a graph node title and
/// another in the rendered page. See those functions for the full argument.
pub(crate) use rto_graph::{first_h1, heading_text};

/// The inner text of every `[[…]]` on a line, ignoring any inside an inline
/// code span (so `` `[[path#Symbol]]` `` written as a documentation example is
/// not treated as a real link). Read by the ADR, blueprint, site-page and lat.md
/// parsers.
///
/// Re-exported from [`rto_graph::wiki_link_targets`] for the reason [`slugify`]
/// above is, and it is a sharper case of it: "find a Markdown link" had **five**
/// implementations across this workspace sharing no code, and only this one knew
/// about code spans. A `[[…]]` scanner that disagrees with the renderer's about
/// where a link is does not fail — it resolves a link the site renders as
/// literal brackets, which is the shape #790's two Markdown walkers had.
///
/// [`rto_graph::markdown_links`] is the whole rule; this is the half this crate
/// reads. The inline `[text](destination)` half is not used here yet.
pub(crate) use rto_graph::wiki_link_targets;

/// The two halves of the inline-code-span rule: `strip_code_spans` returns a
/// line with its spans **removed**, and `code_spans` returns the **byte ranges**
/// of the spans themselves.
///
/// The first is why a token documented as an example (`` `[[path#Symbol]]` ``,
/// ``` ``@rto:0001`` ```) is not scanned as a real link or annotation; the
/// second is how [`crate::fmt`] tells a table row's column separator from a `|`
/// inside a code span.
///
/// Re-exported from [`rto_graph`] because the link scanner above is: stripping
/// code spans is the *first step* of finding a link, so leaving a copy of it
/// here would mean the one scanner and this crate's `@rto:`/table readers could
/// still disagree about where a code span ends. They are two functions rather
/// than one because removing a span and knowing where one *is* are different
/// questions; see [`rto_graph::code_spans`] for the `CommonMark` rule and the
/// escaping asymmetry #790 turned up.
pub(crate) use rto_graph::{code_spans, strip_code_spans};

/// Trim leading and trailing **blank lines** — lines that are empty or hold only
/// whitespace — from a Markdown span, and nothing else.
///
/// Deliberately not `str::trim`: that also eats the *indentation* of the first
/// content line, which in Markdown is meaning, not padding. A section opening on
/// a four-space-indented code block would be stored, and rendered into the vault
/// note, as ordinary prose. The one rule lives here rather than at each of the
/// three span closes in `crate::adr` (two section closes and the preamble) so
/// the next span to be sliced cannot get a fourth, slightly different one — and
/// so `blueprint` and `site`, which have the same defect on their own section
/// spans, have something to call when they are fixed.
///
/// The newline that *terminates* the last content line belongs to no blank line,
/// but is dropped too, so a span never ends in a bare terminator. Trailing
/// whitespace *on* a content line survives: two spaces before the terminator is
/// a Markdown hard break.
pub(crate) fn trim_blank_lines(span: &str) -> &str {
    let blank = |line: &&str| line.trim().is_empty();
    // `split_inclusive` keeps each terminator with its line, so summing the
    // lengths of the blank ones gives a byte offset directly.
    let leading: usize = span
        .split_inclusive('\n')
        .take_while(blank)
        .map(str::len)
        .sum();
    let span = &span[leading..];
    let trailing: usize = span
        .split_inclusive('\n')
        .rev()
        .take_while(blank)
        .map(str::len)
        .sum();
    let span = &span[..span.len() - trailing];
    span.strip_suffix('\n')
        .map_or(span, |s| s.strip_suffix('\r').unwrap_or(s))
}

#[cfg(test)]
mod tests {
    use super::{lang_for, trim_blank_lines};

    #[test]
    fn lang_for_lowercases_extension_to_match_the_extractor() {
        assert_eq!(lang_for("src/FOO.RS"), "rust", "case-insensitive rust");
        assert_eq!(lang_for("a/b.rs"), "rust");
        assert_eq!(lang_for("x.PY"), "py", "other extensions lowercased");
        assert_eq!(lang_for("README"), "text", "no extension");
    }

    #[test]
    fn trims_surrounding_blank_lines_and_keeps_indentation() {
        assert_eq!(
            trim_blank_lines("\n\n    code;\n\nprose.\n\n"),
            "    code;\n\nprose."
        );
        // Whitespace-only lines count as blank at either end...
        assert_eq!(trim_blank_lines("  \n\t\n\tcode;\n   \n"), "\tcode;");
        // ...but interior ones are body text and stay.
        assert_eq!(trim_blank_lines("a\n\nb"), "a\n\nb");
        // Trailing whitespace *on* a content line is a Markdown hard break.
        assert_eq!(trim_blank_lines("a  \n"), "a  ");
        // A `\r\n` terminator goes with its newline rather than leaving a stray CR.
        assert_eq!(trim_blank_lines("\r\na\r\n\r\n"), "a");
    }

    /// An all-blank span must still come out empty: `AdrDoc::text_for_key` gates on
    /// `is_empty` and `stored` on the capped string, so a span of two newlines has
    /// to yield no `content` key exactly as `str::trim` made it.
    #[test]
    fn an_all_blank_span_is_empty() {
        assert_eq!(trim_blank_lines(""), "");
        assert_eq!(trim_blank_lines("\n"), "");
        assert_eq!(trim_blank_lines("\n\n"), "");
        assert_eq!(trim_blank_lines("   \n\t  \n"), "");
    }
}

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
/// The section key now *is* the anchor, for every heading, by construction.
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

/// Extract the inner text of every `[[…]]` on `line`, ignoring any inside an
/// inline code span (so `` `[[path#Symbol]]` `` written as a documentation
/// example is not treated as a real link). Shared by the ADR and lat.md parsers.
#[must_use]
pub(crate) fn scan_wiki_links(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let stripped = strip_code_spans(line);
    let mut rest = stripped.as_str();
    while let Some(open) = rest.find("[[") {
        let after = &rest[open + 2..];
        if let Some(close) = after.find("]]") {
            let inner = after[..close].trim();
            if !inner.is_empty() {
                out.push(inner.to_owned());
            }
            rest = &after[close + 2..];
        } else {
            break;
        }
    }
    out
}

/// Return `line` with inline code spans removed, so tokens documented as
/// examples (e.g. `` `[[path#Symbol]]` `` or ``` ``@rto:0001`` ```) are not
/// scanned as real links or annotations.
///
/// Follows the `CommonMark` rule for code spans: a span opens with a run of *n*
/// backticks and closes with the next run of exactly *n* backticks. An opening
/// run with no matching close is literal text and is kept. Non-backtick text is
/// preserved verbatim (backticks are ASCII, so all slice boundaries are valid).
#[must_use]
pub(crate) fn strip_code_spans(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'`' {
            let start = i;
            while i < bytes.len() && bytes[i] != b'`' {
                i += 1;
            }
            out.push_str(&line[start..i]);
            continue;
        }
        // Measure the opening backtick run.
        let run_start = i;
        while i < bytes.len() && bytes[i] == b'`' {
            i += 1;
        }
        let run = i - run_start;
        // Find a closing run of exactly the same length.
        let mut j = i;
        let mut close = None;
        while j < bytes.len() {
            if bytes[j] == b'`' {
                let s = j;
                while j < bytes.len() && bytes[j] == b'`' {
                    j += 1;
                }
                if j - s == run {
                    close = Some(j);
                    break;
                }
            } else {
                j += 1;
            }
        }
        match close {
            // A matched span: drop it entirely.
            Some(end) => i = end,
            // Unmatched backticks are literal; keep them and continue.
            None => out.push_str(&line[run_start..i]),
        }
    }
    out
}

/// Trim leading and trailing **blank lines** — lines that are empty or hold only
/// whitespace — from a Markdown span, and nothing else.
///
/// Deliberately not `str::trim`: that also eats the *indentation* of the first
/// content line, which in Markdown is meaning, not padding. A section opening on
/// a four-space-indented code block would be stored, and rendered into the vault
/// note, as ordinary prose. The one rule lives here rather than at each of the
/// three span closes in [`crate::adr`] (two section closes and the preamble) so
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
    use super::{lang_for, strip_code_spans, trim_blank_lines};

    #[test]
    fn lang_for_lowercases_extension_to_match_the_extractor() {
        assert_eq!(lang_for("src/FOO.RS"), "rust", "case-insensitive rust");
        assert_eq!(lang_for("a/b.rs"), "rust");
        assert_eq!(lang_for("x.PY"), "py", "other extensions lowercased");
        assert_eq!(lang_for("README"), "text", "no extension");
    }

    #[test]
    fn removes_single_and_multi_backtick_spans() {
        assert_eq!(strip_code_spans("a `code` b"), "a  b");
        // A run of two backticks (used to embed a literal backtick) is a span too.
        assert_eq!(strip_code_spans("see ``@rto:0001`` here"), "see  here");
        assert_eq!(strip_code_spans("x ```fenced inline``` y"), "x  y");
    }

    #[test]
    fn keeps_unmatched_backticks_and_plain_text() {
        assert_eq!(strip_code_spans("no code here"), "no code here");
        assert_eq!(strip_code_spans("unmatched ` tick"), "unmatched ` tick");
        // Mismatched run lengths do not close the span.
        assert_eq!(strip_code_spans("``open ` mid"), "``open ` mid");
    }

    #[test]
    fn preserves_utf8_outside_spans() {
        assert_eq!(strip_code_spans("café `x` — ok"), "café  — ok");
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

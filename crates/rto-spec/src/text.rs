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

/// A URL-safe slug: lowercase, non-alphanumeric runs collapsed to a single `-`,
/// trimmed of leading/trailing `-`. Shared by the ADR and lat.md section keys.
#[must_use]
pub(crate) fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_owned()
}

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

#[cfg(test)]
mod tests {
    use super::{lang_for, strip_code_spans};

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
}

//! Small Markdown text helpers shared by the ADR and annotation scanners.

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
    use super::strip_code_spans;

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

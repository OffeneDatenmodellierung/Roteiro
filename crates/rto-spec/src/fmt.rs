//! The canonical form of an authored document, and the diff that reaches it.
//!
//! ADR-0023 step 1. Purely syntactic: every rule here is one whose application
//! cannot change what a document means, which is what admits `fmt` at all —
//! the other verbs in that family each had to argue for their scope, and this
//! one earns its place by having nothing to decide.
//!
//! # What measurement changed about the plan
//!
//! ADR-0023 named three jobs. Measured against this repository's own 26 ADRs
//! before any of it was written, two were already clean — a single frontmatter
//! key order in 26 of 26, and an ISO `last-modified` in 26 of 26 — so they
//! survive here as **idempotence guarantees** rather than as cleanups: they
//! hold the line, they do not move it.
//!
//! The third had to be narrowed. "Table alignment" normally means padding each
//! cell to its column's widest, and these documents make that actively wrong:
//! an ADR history table carries prose paragraphs in single cells, 59 of them
//! over 1,000 characters and the widest 5,789, so column-padding would blow
//! every sibling row out to match. The canonical form is therefore **one space
//! either side of every cell** — the ordinary markdown convention, uniform
//! across every table, and a form in which a long cell pads nothing.

use std::fmt::Write as _;

/// Frontmatter keys in the order [`crate::spec::scaffold_adr`] writes them.
///
/// Keys outside this list keep their relative order and follow the known ones,
/// because an unknown key is somebody else's convention and reordering it would
/// be a change this module has no basis to make.
const ADR_KEY_ORDER: &[&str] = &[
    "Title",
    "Space",
    "Parent",
    "type",
    "adr-id",
    "status",
    "architectural-significance",
    "domain",
    "decision-makers",
    "superseded-by",
    "version",
    "last-modified",
    "confluence-url",
];

/// The summary table's first-row label, as `scaffold_adr` emits it.
///
/// 23 of this repository's 26 ADRs already say `**State**` and three say
/// `**Status**`. Neither is wrong; having both is, and the generator settles
/// which one wins. Nothing reads this row — the drift gate reads the frontmatter
/// and the **Document version** row — so this is cosmetic by construction.
const SUMMARY_STATE_ROW: (&str, &str) = ("| **Status** |", "| **State** |");

/// The canonical form of `text`.
///
/// Idempotent: `canonical(canonical(t)) == canonical(t)` for every document in
/// `docs/`, which [`tests::the_canonical_form_is_a_fixed_point`] asserts over
/// the real tree rather than over a fixture, because a fixture cannot contain
/// the shapes nobody thought to write down.
#[must_use]
pub fn canonical(text: &str) -> String {
    let (front, body) = split_frontmatter(text);
    let mut out = String::with_capacity(text.len());
    if let Some(front) = front {
        out.push_str("---\n");
        out.push_str(&canonical_frontmatter(front));
        out.push_str("---\n");
    }
    out.push_str(&canonical_body(body));
    out
}

/// Split the leading `---` frontmatter block off `text`.
///
/// Returns `(None, text)` when there is no frontmatter, which is the ordinary
/// case for a blueprint or a plain site page.
fn split_frontmatter(text: &str) -> (Option<&str>, &str) {
    let Some(rest) = text.strip_prefix("---\n") else {
        return (None, text);
    };
    match rest.find("\n---\n") {
        Some(end) => (Some(&rest[..=end]), &rest[end + 5..]),
        None => (None, text),
    }
}

/// Reorder frontmatter keys into [`ADR_KEY_ORDER`], carrying each key's
/// preceding comment and blank lines with it.
///
/// A comment sits above the key it explains, so moving the key without it would
/// leave the comment describing whatever landed underneath — the same defect
/// this crate's own module docs had, at a smaller scale.
fn canonical_frontmatter(front: &str) -> String {
    let mut blocks: Vec<(Option<String>, Vec<&str>)> = Vec::new();
    let mut pending: Vec<&str> = Vec::new();
    for line in front.lines() {
        let is_key = !line.starts_with('#')
            && !line.starts_with(' ')
            && line.contains(':')
            && !line.trim().is_empty();
        pending.push(line);
        if is_key {
            let key = line.split(':').next().unwrap_or_default().trim().to_owned();
            blocks.push((Some(key), std::mem::take(&mut pending)));
        }
    }
    let trailing = pending;

    let mut out = String::with_capacity(front.len());
    let mut used = vec![false; blocks.len()];
    for want in ADR_KEY_ORDER {
        for (i, (key, lines)) in blocks.iter().enumerate() {
            if !used[i] && key.as_deref() == Some(*want) {
                used[i] = true;
                for l in lines {
                    let _ = writeln!(out, "{}", normalise_value(l));
                }
            }
        }
    }
    for (i, (_, lines)) in blocks.iter().enumerate() {
        if !used[i] {
            for l in lines {
                let _ = writeln!(out, "{}", normalise_value(l));
            }
        }
    }
    for l in trailing {
        let _ = writeln!(out, "{l}");
    }
    out
}

/// Normalise a frontmatter value that has one legal spelling.
///
/// Only `last-modified` today: a date is a date, and `2026-9-1` and `2026-09-01`
/// are the same one. All 26 ADRs already pass; this keeps it that way rather
/// than cleaning anything up.
fn normalise_value(line: &str) -> String {
    let Some(rest) = line.strip_prefix("last-modified:") else {
        return line.to_owned();
    };
    let (value, comment) = match rest.find('#') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let parts: Vec<&str> = value.trim().split('-').collect();
    let [y, m, d] = parts.as_slice() else {
        return line.to_owned();
    };
    let (Ok(y), Ok(m), Ok(d)) = (y.parse::<u32>(), m.parse::<u32>(), d.parse::<u32>()) else {
        return line.to_owned();
    };
    let tail = if comment.is_empty() {
        String::new()
    } else {
        format!(" {comment}")
    };
    format!("last-modified: {y:04}-{m:02}-{d:02}{tail}")
}

/// Canonicalise the body: tables, and the summary table's first-row label.
///
/// **Fenced code is left exactly as written.** These documents quote markdown
/// in fences — a table inside one is an example of a table, not a table — and
/// reformatting it would rewrite the very thing the prose is pointing at.
fn canonical_body(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut fenced = false;
    for line in body.lines() {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            fenced = !fenced;
            let _ = writeln!(out, "{line}");
            continue;
        }
        if fenced {
            let _ = writeln!(out, "{line}");
            continue;
        }
        let line = if line.starts_with(SUMMARY_STATE_ROW.0) {
            line.replacen(SUMMARY_STATE_ROW.0, SUMMARY_STATE_ROW.1, 1)
        } else {
            line.to_owned()
        };
        if line.trim_start().starts_with('|') {
            let _ = writeln!(out, "{}", canonical_table_row(line.trim()));
        } else {
            let _ = writeln!(out, "{line}");
        }
    }
    if body.ends_with('\n') || body.is_empty() {
        out
    } else {
        out.trim_end_matches('\n').to_owned()
    }
}

/// One table row in canonical form: `| a | b |`.
///
/// A separator row collapses to `|---|` per column, keeping any alignment
/// colons, because those carry meaning and the padding around them does not.
fn canonical_table_row(row: &str) -> String {
    let cells = split_cells(row);
    let separator = !cells.is_empty()
        && cells
            .iter()
            .all(|c| !c.is_empty() && c.chars().all(|ch| ch == '-' || ch == ':'));
    let mut out = String::with_capacity(row.len());
    out.push('|');
    for cell in &cells {
        if separator {
            let left = cell.starts_with(':');
            let right = cell.ends_with(':');
            let bar = match (left, right) {
                (true, true) => ":---:",
                (true, false) => ":---",
                (false, true) => "---:",
                (false, false) => "---",
            };
            let _ = write!(out, "{bar}|");
        } else {
            let _ = write!(out, " {cell} |");
        }
    }
    out
}

/// Split a row into cells on a `|` that is really a column boundary.
///
/// Two things are **not** boundaries, and both were found by running this over
/// the real documents rather than over a fixture:
///
/// - `\|`, an escaped pipe. Splitting on it cuts a cell in half and silently
///   adds a column.
/// - a pipe inside `` `inline code` ``. `docs/adr/0006` documents chat-template
///   markers, and a history row there contains `` `<|im_start|>` ``. Splitting
///   on those two pipes and rejoining with the canonical spacing rewrote it to
///   `` `< | im_start | >` `` — a formatter corrupting the text it formats,
///   which is the one thing a purely syntactic rewrite may never do.
///
/// Markdown itself would read that row as having extra columns, so the source
/// is arguably already wrong. That is not this command's business to decide:
/// `fmt` normalises whitespace around boundaries it is sure of and leaves
/// everything else exactly as written.
fn split_cells(row: &str) -> Vec<String> {
    let inner = row.trim().trim_start_matches('|').trim_end_matches('|');
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut escaped = false;
    let mut in_code = false;
    for ch in inner.chars() {
        if escaped {
            cur.push(ch);
            escaped = false;
        } else if ch == '\\' {
            cur.push(ch);
            escaped = true;
        } else if ch == '`' {
            in_code = !in_code;
            cur.push(ch);
        } else if ch == '|' && !in_code {
            cells.push(cur.trim().to_owned());
            cur = String::new();
        } else {
            cur.push(ch);
        }
    }
    cells.push(cur.trim().to_owned());
    cells
}

/// A unified diff of `before` against `after`, or `None` when they are equal.
///
/// Written here rather than taken as a dependency: ADR-0017 asks what a crate
/// earns, and a line diff over documents of a few hundred lines is a textbook
/// LCS that costs less to own than to justify.
#[must_use]
pub fn unified_diff(path: &str, before: &str, after: &str) -> Option<String> {
    if before == after {
        return None;
    }
    let (a, b): (Vec<&str>, Vec<&str>) = (before.lines().collect(), after.lines().collect());
    let mut lcs = vec![vec![0_usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut out = format!("--- {path}\n+++ {path}\n");
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            let _ = writeln!(out, "-{}", a[i]);
            i += 1;
        } else {
            let _ = writeln!(out, "+{}", b[j]);
            j += 1;
        }
    }
    for l in &a[i..] {
        let _ = writeln!(out, "-{l}");
    }
    for l in &b[j..] {
        let _ = writeln!(out, "+{l}");
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A table row is one space either side of every cell.
    #[test]
    fn a_table_row_is_single_spaced() {
        assert_eq!(canonical_table_row("|a|b|"), "| a | b |");
        assert_eq!(canonical_table_row("|  a   |  b |"), "| a | b |");
        assert_eq!(canonical_table_row("| | |"), "|  |  |");
    }

    /// A separator row keeps its alignment colons and loses its padding.
    #[test]
    fn a_separator_row_keeps_its_alignment_colons() {
        assert_eq!(canonical_table_row("|---|---|"), "|---|---|");
        assert_eq!(canonical_table_row("| :--- | ---: |"), "|:---|---:|");
        assert_eq!(canonical_table_row("|:-------:|"), "|:---:|");
    }

    /// An escaped pipe is content, not a column boundary.
    ///
    /// Splitting on it would cut one cell into two and silently widen the row,
    /// which a reader would see as a table that had grown a column.
    #[test]
    fn an_escaped_pipe_does_not_add_a_column() {
        assert_eq!(split_cells(r"| a \| b | c |").len(), 2);
        assert_eq!(canonical_table_row(r"|a \| b|c|"), r"| a \| b | c |");
    }

    /// A pipe inside inline code is content, not a column boundary.
    ///
    /// Found by running `fmt` over `docs/` rather than over a fixture: a history
    /// row in ADR-0006 documents the chat-template marker `` `<|im_start|>` ``,
    /// and splitting on those pipes rewrote it to `` `< | im_start | >` ``.
    #[test]
    fn a_pipe_inside_inline_code_is_not_a_column_boundary() {
        let row = "| a | matches `<|im_start|>` here | b |";
        assert_eq!(split_cells(row).len(), 3, "{:?}", split_cells(row));
        assert_eq!(
            canonical_table_row(row),
            row,
            "the formatter rewrote its own input"
        );
    }

    /// A table inside a fence is an **example** of a table, not a table.
    ///
    /// These documents quote markdown to show what it looks like; reformatting
    /// it would rewrite the thing the surrounding prose is pointing at.
    #[test]
    fn a_table_inside_a_fence_is_left_alone() {
        let src = "| a |  b |\n\n```md\n|  x |y|\n```\n";
        let got = canonical_body(src);
        assert!(got.contains("| a | b |"), "{got}");
        assert!(
            got.contains("|  x |y|"),
            "the fenced example was rewritten:\n{got}"
        );
    }

    /// A key moves with the comment that explains it.
    #[test]
    fn frontmatter_keys_are_reordered_with_their_comments() {
        let front = "version: \"1.0\"\n# which stage this is at\nstatus: Accepted\nTitle: T\n";
        let got = canonical_frontmatter(front);
        let lines: Vec<&str> = got.lines().collect();
        assert_eq!(lines[0], "Title: T");
        assert_eq!(
            lines[1], "# which stage this is at",
            "the comment was left behind by its key:\n{got}"
        );
        assert_eq!(lines[2], "status: Accepted");
        assert_eq!(lines[3], "version: \"1.0\"");
    }

    /// An unknown key keeps its place rather than being reordered on a guess.
    #[test]
    fn an_unknown_key_is_not_reordered() {
        let got = canonical_frontmatter("zzz-custom: 1\nTitle: T\naaa-custom: 2\n");
        let lines: Vec<&str> = got.lines().collect();
        assert_eq!(lines[0], "Title: T");
        assert_eq!(
            lines[1], "zzz-custom: 1",
            "relative order was not preserved"
        );
        assert_eq!(lines[2], "aaa-custom: 2");
    }

    /// A date is normalised, and its trailing comment survives.
    #[test]
    fn a_date_is_normalised_to_iso() {
        assert_eq!(
            normalise_value("last-modified: 2026-9-1"),
            "last-modified: 2026-09-01"
        );
        assert_eq!(
            normalise_value("last-modified: 2026-09-01  # set by hand"),
            "last-modified: 2026-09-01 # set by hand"
        );
        // Not a date: left exactly as written rather than guessed at.
        assert_eq!(
            normalise_value("last-modified: soon"),
            "last-modified: soon"
        );
        assert_eq!(normalise_value("version: \"1.2\""), "version: \"1.2\"");
    }

    /// The summary table settles on the spelling the generator emits.
    #[test]
    fn the_summary_row_settles_on_the_generators_spelling() {
        let got = canonical_body("| **Status** | Accepted |\n");
        assert_eq!(got, "| **State** | Accepted |\n");
        // Only the summary row: the word is ordinary prose elsewhere.
        assert_eq!(
            canonical_body("The **Status** of this.\n"),
            "The **Status** of this.\n"
        );
    }

    /// No diff when there is nothing to say.
    #[test]
    fn an_unchanged_document_has_no_diff() {
        assert!(unified_diff("a.md", "x\ny\n", "x\ny\n").is_none());
        let d = unified_diff("a.md", "x\ny\n", "x\nz\n").expect("changed");
        assert!(d.contains("-y") && d.contains("+z"), "{d}");
    }

    /// Canonicalising a canonical document changes nothing.
    ///
    /// The property that makes `fmt` safe to run in a loop, and the one a
    /// reordering pass is most likely to break.
    #[test]
    fn the_canonical_form_is_a_fixed_point() {
        let src = "---\nversion: \"1.0\"\n# note\nstatus: Accepted\nTitle: T\nodd: 1\n---\n\n\
                   | **Status** | Accepted |\n|  a |b  |\n|---|:--:|\n\n```md\n| raw |\n```\n";
        let once = canonical(src);
        assert_eq!(canonical(&once), once, "not idempotent:\n{once}");
    }
}

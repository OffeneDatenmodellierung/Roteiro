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
//!
//! # The rule this module is most at risk of breaking
//!
//! **A formatter may not change content.** Every narrowing below exists because
//! some plausible implementation of "canonicalise a table" quietly rewrote
//! something that was not a table, or was not the part of it that it looked
//! like. They are listed at the function that enforces each.

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

/// The canonical form of `text`.
///
/// Idempotent: `canonical(canonical(t)) == canonical(t)`, asserted here on
/// constructed input and in `roteiro`'s `docs_are_canonical` test over the real
/// `docs/` tree — which matters because those documents contain shapes nobody
/// thought to write down, and one of them already caught a defect that every
/// hand-written fixture missed.
#[must_use]
pub fn canonical(text: &str) -> String {
    let (front, body) = crate::adr::split_frontmatter(text);
    // `split_frontmatter` returns an empty `front` for **both** "there is no
    // frontmatter" and "the frontmatter block is empty". Telling them apart by
    // `front.is_empty()` treated `---\n\n---` as body text and dropped both
    // delimiters — a formatter deleting a document's frontmatter. What
    // distinguishes them is the body: when there is no frontmatter, it is the
    // whole input.
    let has_front = body.len() != text.len();
    // The shared rule, not a second spelling of it. Reading the key by
    // `strip_prefix("type:")` still missed `Type: adr` and `type : adr`, which
    // `declares_adr` accepts — so a document the rest of the crate treats as an
    // ADR would have kept a legacy summary row.
    let is_adr = crate::adr::declares_adr(text);
    let mut out = String::with_capacity(text.len());
    if has_front {
        out.push_str("---\n");
        out.push_str(&canonical_frontmatter(front));
        out.push_str("---\n");
    }
    out.push_str(&canonical_body(body, is_adr));
    out
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

/// Normalise a frontmatter value that has exactly one legal spelling.
///
/// Only `last-modified` today, and only when the value is **already a date this
/// repository would accept but for its padding**. Two narrowings, both because
/// a formatter that guesses is worse than one that declines:
///
/// - An inline comment starts at `" #"`, matching
///   [`crate::adr::clean_value`]. Splitting at every `#` would treat
///   `2026-09-01#tag` as a bare date and then "normalise" a value the shared
///   parser reads differently — two parsers disagreeing about one line.
/// - The year must be four digits and the month and day at most two, so
///   `12345-1-1` is left alone rather than padded into `12345-01-01`, which is
///   just as invalid and now looks deliberate.
fn normalise_value(line: &str) -> String {
    // The key is **parsed**, not prefix-matched. `parse_adr` trims and lowercases
    // it, so `last-modified : 2026-9-1` is the same key to the rest of the crate
    // — and matching the literal prefix left exactly that line non-ISO, making
    // the canonical date rule depend on punctuation.
    let Some((key, rest)) = line.split_once(':') else {
        return line.to_owned();
    };
    if !key.trim().eq_ignore_ascii_case("last-modified") || key.starts_with(char::is_whitespace) {
        return line.to_owned();
    }
    let (value, comment) = match rest.find(" #") {
        Some(i) => (&rest[..i], rest[i..].trim_start()),
        None => (rest, ""),
    };
    let parts: Vec<&str> = value.trim().split('-').collect();
    let [y, m, d] = parts.as_slice() else {
        return line.to_owned();
    };
    let ok = y.len() == 4 && (1..=2).contains(&m.len()) && (1..=2).contains(&d.len());
    let (Ok(y), Ok(m), Ok(d)) = (y.parse::<u32>(), m.parse::<u32>(), d.parse::<u32>()) else {
        return line.to_owned();
    };
    if !ok || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return line.to_owned();
    }
    let tail = if comment.is_empty() {
        String::new()
    } else {
        format!(" {comment}")
    };
    // Whatever separated the colon from the value, kept.
    let lead = &rest[..rest.len() - rest.trim_start().len()];
    let lead = if lead.is_empty() { " " } else { lead };
    // The key is written back **verbatim**. Trimming it would canonicalise
    // `last-modified :` to `last-modified:`, which is a rewrite this command
    // does not advertise — and unadvertised rewriting is where every defect in
    // this module has been.
    format!("{key}:{lead}{y:04}-{m:02}-{d:02}{tail}")
}

/// Canonicalise the body: tables, and the ADR summary table's first-row label.
///
/// **Fenced code is left exactly as written.** These documents quote markdown
/// in fences — a table inside one is an example of a table, not a table — and
/// reformatting it would rewrite the very thing the prose is pointing at. The
/// fence is matched by its own character and length, because a four-backtick
/// fence may legally contain a three-backtick line and that line is still code.
fn canonical_body(body: &str, is_adr: bool) -> String {
    let mut out = String::with_capacity(body.len());
    let mut fence: Option<(char, usize)> = None;
    let mut seen_table = false;
    let lines: Vec<&str> = body.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some((ch, len)) = fence {
            let _ = writeln!(out, "{line}");
            // A *closing* fence carries nothing but whitespace after its run.
            // ```` ```not-a-close ```` opens an info string, so treating it as a
            // close would put the rest of the block outside the fence and let
            // the pipe rows inside it be reformatted.
            if fence_of(line).is_some_and(|(c, n)| {
                c == ch && n >= len && line.trim_start().trim_start_matches(c).trim().is_empty()
            }) {
                fence = None;
            }
            i += 1;
            continue;
        }
        if let Some(f) = fence_of(line) {
            fence = Some(f);
            let _ = writeln!(out, "{line}");
            i += 1;
            continue;
        }
        // A **table** is a contiguous run of `|` lines containing a separator
        // row. Requiring the separator is what stops `fmt` rewriting a line
        // that merely begins with a pipe — a lone `|` in prose, or a pipe
        // inside an indented code block — into `| |`, which would be the
        // formatter inventing a table.
        if is_table_line(line) {
            let mut j = i;
            while j < lines.len() && is_table_line(lines[j]) {
                j += 1;
            }
            let block = &lines[i..j];
            if block.iter().any(|l| is_separator_row(l)) {
                // The relabel belongs to the **summary** table — the first one
                // in the document — and not to every table in it. A later table
                // may legitimately carry a `| **Status** | … |` row of its own,
                // and rewriting that is changing authored content.
                let summary = !seen_table;
                seen_table = true;
                for l in block {
                    let row = canonical_table_row(l.trim());
                    let row = if is_adr && summary {
                        relabel_state(&row)
                    } else {
                        row
                    };
                    let _ = writeln!(out, "{row}");
                }
            } else {
                for l in block {
                    let _ = writeln!(out, "{l}");
                }
            }
            i = j;
            continue;
        }
        let _ = writeln!(out, "{line}");
        i += 1;
    }
    if body.ends_with('\n') || body.is_empty() {
        out
    } else {
        out.trim_end_matches('\n').to_owned()
    }
}

/// The fence this line opens or closes, as `(character, length)`.
fn fence_of(line: &str) -> Option<(char, usize)> {
    // Four spaces — or a tab — is an indented code block, and a run of backticks
    // inside one is literal content. Trimming all indentation read it as a fence
    // opener, and an unmatched one there left every table after it "fenced" and
    // therefore never canonicalised.
    if !markdown_indented(line) {
        return None;
    }
    let t = line.trim_start();
    for ch in ['`', '~'] {
        let n = t.chars().take_while(|c| *c == ch).count();
        if n >= 3 {
            return Some((ch, n));
        }
    }
    None
}

/// A line that could be part of a table: `|`-led, indented no further than
/// markdown allows, and carrying a second pipe so a lone `|` is not a row.
fn is_table_line(line: &str) -> bool {
    let t = line.trim_start();
    markdown_indented(line) && t.starts_with('|') && t.trim_end().len() > 1 && t[1..].contains('|')
}

/// Whether `line` is indented little enough to be markup rather than code.
///
/// Markdown allows up to three spaces before a construct; four begins an
/// indented code block, and **a leading tab counts as four**. Counting bytes
/// gave a tab an indent of one, so a tab-indented example was stripped of its
/// tab and reformatted — the promise to leave indented code alone, broken by
/// the measurement rather than by the rule.
fn markdown_indented(line: &str) -> bool {
    let indent = &line[..line.len() - line.trim_start().len()];
    !indent.contains('\t') && indent.len() <= 3
}

/// A separator row: every cell all `-`/`:` with **at least three** hyphens.
///
/// The three are markdown's requirement, and enforcing it is what stops a real
/// data row like `| -- | -- |` being rewritten to `|---|---|` — which would
/// turn two cells of content into a structural row and change the table.
fn is_separator_row(line: &str) -> bool {
    let cells = split_cells(line.trim());
    !cells.is_empty()
        && cells.iter().all(|c| {
            let core = c.trim_start_matches(':').trim_end_matches(':');
            core.len() >= 3 && !core.is_empty() && core.chars().all(|ch| ch == '-')
        })
}

/// The ADR summary table's first-row label, settled on the spelling
/// [`crate::spec::scaffold_adr`] emits.
///
/// Applied **after** the row is canonicalised, so `|**Status**| Accepted |`
/// relabels on the first pass rather than the second — otherwise
/// `canonical(canonical(t)) != canonical(t)` for that input.
///
/// ADR-only: `canonical` also formats site pages and plain documents, and a
/// table of theirs whose first column happens to say **Status** is not this
/// table and is none of our business.
fn relabel_state(row: &str) -> String {
    if row.starts_with("| **Status** |") {
        row.replacen("| **Status** |", "| **State** |", 1)
    } else {
        row.to_owned()
    }
}

/// One table row in canonical form: `| a | b |`.
///
/// A separator row collapses to `|---|` per column, keeping any alignment
/// colons, because those carry meaning and the padding around them does not.
fn canonical_table_row(row: &str) -> String {
    let cells = split_cells(row);
    let separator = is_separator_row(row);
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
/// Three things are **not** boundaries, and each was found by a reader or a run
/// over the real documents rather than by writing a fixture:
///
/// - `\|`, an escaped pipe. Splitting on it cuts a cell in half and adds a
///   column.
/// - a pipe inside an inline-code span. `docs/adr/0006` documents chat-template
///   markers, and a history row there contains `` `<|im_start|>` ``; splitting
///   on those two pipes and rejoining rewrote it to `` `< | im_start | >` `` —
///   a formatter corrupting the text it formats. The span's delimiter is a
///   **run** of backticks, matched by length, because ```` ``a ` | b`` ```` is
///   one span containing a lone backtick and a pipe.
/// - the outer delimiters, of which **at most one** is stripped from each end.
///   `trim_start_matches('|')` eats them all, so `|| value |` loses its empty
///   first column and the table changes shape.
fn split_cells(row: &str) -> Vec<String> {
    let trimmed = row.trim();
    let body = trimmed.strip_prefix('|').unwrap_or(trimmed);
    // The **closing** delimiter is only a delimiter when it is unescaped. In a
    // row like `| a \|` the final pipe is content, and stripping it left a bare
    // backslash where the escape had been — the formatter corrupting the cell it
    // was protecting.
    let inner = match body.strip_suffix('|') {
        Some(rest) if !ends_escaped(rest) => rest,
        _ => body,
    };
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut escaped = false;
    let mut code: Option<usize> = None;
    let chars: Vec<char> = inner.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if escaped {
            cur.push(ch);
            escaped = false;
            i += 1;
        } else if ch == '\\' {
            cur.push(ch);
            escaped = true;
            i += 1;
        } else if ch == '`' {
            let run = chars[i..].iter().take_while(|c| **c == '`').count();
            match code {
                Some(open) if open == run => code = None,
                None => code = Some(run),
                Some(_) => {}
            }
            for _ in 0..run {
                cur.push('`');
            }
            i += run;
        } else if ch == '|' && code.is_none() {
            cells.push(cur.trim().to_owned());
            cur = String::new();
            i += 1;
        } else {
            cur.push(ch);
            i += 1;
        }
    }
    cells.push(cur.trim().to_owned());
    cells
}

/// Whether `s` ends with an unbalanced escape, so the character after it is
/// escaped rather than syntactic.
fn ends_escaped(s: &str) -> bool {
    s.chars().rev().take_while(|c| *c == '\\').count() % 2 == 1
}

/// A unified diff of `before` against `after`, or `None` when they are equal.
///
/// A **real** unified diff, with `@@` hunk headers and three lines of context,
/// so `git apply` and `patch` accept it. Written here rather than taken as a
/// dependency: ADR-0017 asks what a crate earns, and a line diff over documents
/// of a few hundred lines is a textbook LCS that costs less to own than to
/// justify.
#[must_use]
pub fn unified_diff(path: &str, before: &str, after: &str) -> Option<String> {
    /// Lines of context either side of a change, as `diff -u` uses.
    const CTX: usize = 3;
    if before == after {
        return None;
    }
    let (a, b): (Vec<&str>, Vec<&str>) = (before.lines().collect(), after.lines().collect());
    if a == b {
        // Same lines, different bytes: the difference is the file's final
        // newline, which `lines()` does not carry. Emitting two headers and no
        // hunk would be a diff that says nothing while the caller reports drift.
        let says = |s: &str| {
            if s.ends_with('\n') { "with" } else { "without" }
        };
        return Some(format!(
            "--- {path}\n+++ {path}\n@@ -0,0 +0,0 @@\n\\ file ended {} a trailing newline, now ends {} one\n",
            says(before),
            says(after)
        ));
    }
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
    // (tag, old index, new index) for every line, in order.
    let mut ops: Vec<(char, usize, usize)> = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            ops.push((' ', i, j));
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            ops.push(('-', i, j));
            i += 1;
        } else {
            ops.push(('+', i, j));
            j += 1;
        }
    }
    while i < a.len() {
        ops.push(('-', i, j));
        i += 1;
    }
    while j < b.len() {
        ops.push(('+', i, j));
        j += 1;
    }

    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, (t, _, _))| *t != ' ')
        .map(|(n, _)| n)
        .collect();
    let mut out = format!("--- {path}\n+++ {path}\n");
    let mut at = 0;
    while at < changed.len() {
        let start = changed[at].saturating_sub(CTX);
        let mut end = changed[at];
        while at + 1 < changed.len() && changed[at + 1] <= end + 2 * CTX {
            at += 1;
            end = changed[at];
        }
        at += 1;
        let end = (end + CTX).min(ops.len() - 1);
        let (mut old_n, mut new_n) = (0, 0);
        for (t, _, _) in &ops[start..=end] {
            if *t != '+' {
                old_n += 1;
            }
            if *t != '-' {
                new_n += 1;
            }
        }
        // An empty range starts at 0 — `@@ -0,0 +1,1 @@` for an insertion into an
        // empty file. Adding one unconditionally emits `-1,0`, which `patch`
        // rejects at exactly that boundary.
        let _ = writeln!(
            out,
            "@@ -{},{} +{},{} @@",
            if old_n == 0 { 0 } else { ops[start].1 + 1 },
            old_n,
            if new_n == 0 { 0 } else { ops[start].2 + 1 },
            new_n
        );
        for (t, oi, ni) in &ops[start..=end] {
            let text = if *t == '+' { b[*ni] } else { a[*oi] };
            let _ = writeln!(out, "{t}{text}");
        }
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
        let src = "| a |  b |\n|---|---|\n\n```md\n|  x |y|\n```\n";
        let got = canonical_body(src, true);
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
        let got = canonical_body("| **Status** | Accepted |\n|---|---|\n", true);
        assert_eq!(got, "| **State** | Accepted |\n|---|---|\n");
        // Only the summary row: the word is ordinary prose elsewhere.
        assert_eq!(
            canonical_body("The **Status** of this.\n", true),
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

#[cfg(test)]
mod review_regressions {
    use super::*;

    /// A run of `|` lines is only a table when it has a separator row.
    ///
    /// Without that, `fmt` invents tables: a lone `|` in prose becomes `| |`,
    /// and a pipe inside an indented code block gets reformatted. Raised on
    /// #790.
    #[test]
    fn a_pipe_line_is_not_a_table_without_a_separator() {
        assert_eq!(canonical_body("|\n", true), "|\n");
        assert_eq!(canonical_body("|  a |b|\n", true), "|  a |b|\n");
        // With one, it is a table and is canonicalised.
        assert_eq!(
            canonical_body("|  a |b|\n|---|---|\n", true),
            "| a | b |\n|---|---|\n"
        );
        // Indented four spaces is code, not a table.
        assert_eq!(canonical_body("    |  a |b|\n", true), "    |  a |b|\n");
    }

    /// `| -- | -- |` is data, not a separator: markdown wants three hyphens.
    #[test]
    fn two_hyphens_are_content_not_a_separator() {
        assert!(!is_separator_row("| -- | -- |"));
        assert!(is_separator_row("| --- | --- |"));
        assert_eq!(
            canonical_body("| a | b |\n|---|---|\n| -- | -- |\n", true),
            "| a | b |\n|---|---|\n| -- | -- |\n"
        );
    }

    /// Only one outer delimiter is stripped, so an empty first cell survives.
    #[test]
    fn an_empty_leading_cell_is_not_eaten() {
        assert_eq!(split_cells("|| value |"), vec!["", "value"]);
    }

    /// An inline-code span is delimited by a *run* of backticks.
    #[test]
    fn a_multi_backtick_span_protects_its_pipe() {
        let cells = split_cells("| ``a ` | b`` | c |");
        assert_eq!(cells.len(), 2, "{cells:?}");
        assert_eq!(cells[0], "``a ` | b``");
    }

    /// A four-backtick fence may contain a three-backtick line.
    #[test]
    fn a_longer_fence_survives_a_shorter_one_inside_it() {
        let src = "````md\n```\n|  x |y|\n```\n````\n|  a |b|\n|---|---|\n";
        let got = canonical_body(src, true);
        assert!(got.contains("|  x |y|"), "fenced example rewritten:\n{got}");
        assert!(
            got.contains("| a | b |"),
            "real table not formatted:\n{got}"
        );
    }

    /// The relabel happens after canonicalisation, so one pass suffices.
    #[test]
    fn the_state_relabel_reaches_a_fixed_point_in_one_pass() {
        let src = "|**Status**| Accepted |\n|---|---|\n";
        let once = canonical_body(src, true);
        assert_eq!(once, "| **State** | Accepted |\n|---|---|\n");
        assert_eq!(canonical_body(&once, true), once);
    }

    /// A non-ADR document's `**Status**` column is none of our business.
    #[test]
    fn the_relabel_is_adr_only() {
        let src = "| **Status** | Accepted |\n|---|---|\n";
        assert_eq!(canonical_body(src, false), src);
    }

    /// A date is padded only when it is already a date but for its padding.
    #[test]
    fn a_date_that_is_not_one_is_left_alone() {
        assert_eq!(
            normalise_value("last-modified: 12345-1-1"),
            "last-modified: 12345-1-1"
        );
        assert_eq!(
            normalise_value("last-modified: 2026-13-01"),
            "last-modified: 2026-13-01"
        );
        // `#` is a comment only after a space — the shared parser's rule.
        assert_eq!(
            normalise_value("last-modified: 2026-09-01#tag"),
            "last-modified: 2026-09-01#tag"
        );
    }

    /// Frontmatter whose closing fence ends the file is still frontmatter.
    #[test]
    fn frontmatter_closed_at_end_of_file_is_found() {
        let got = canonical("---\nversion: \"1.0\"\nTitle: T\n---");
        assert!(got.starts_with("---\nTitle: T\n"), "{got}");
    }

    /// The diff is a diff: `@@` hunks, and context around each change.
    #[test]
    fn the_diff_is_a_real_unified_diff() {
        let mut before = String::new();
        for n in 1..=20 {
            let _ = writeln!(before, "line {n}");
        }
        let after = before.replace("line 10\n", "changed\n");
        let d = unified_diff("a.md", &before, &after).expect("changed");
        assert!(d.starts_with("--- a.md\n+++ a.md\n@@ "), "{d}");
        assert!(d.contains("-line 10\n+changed\n"), "{d}");
        assert!(d.contains(" line 7\n"), "no leading context:\n{d}");
        assert!(!d.contains("line 1\nline 2"), "whole file emitted:\n{d}");
    }
}

#[cfg(test)]
mod second_round {
    use super::*;

    /// The declared kind is read, not pattern-matched.
    ///
    /// Substring matching on the raw `type:` line was both too loose and too
    /// tight — `type: not-adr` contains "adr" and `type: ADR` does not — so one
    /// non-ADR got the summary relabel and one ADR did not. Raised on #790.
    #[test]
    fn the_declared_kind_decides_the_relabel() {
        let row = "| **Status** | Accepted |\n|---|---|\n";
        let doc = |ty: &str| format!("---\ntype: {ty}\n---\n\n{row}");
        assert!(canonical(&doc("adr")).contains("**State**"));
        assert!(
            canonical(&doc("ADR")).contains("**State**"),
            "case-sensitive"
        );
        assert!(
            canonical(&doc("not-adr")).contains("**Status**"),
            "`not-adr` was treated as an ADR"
        );
        assert!(canonical(&doc("blueprint")).contains("**Status**"));
    }

    /// A closing fence carries nothing after its run.
    ///
    /// ```` ```not-a-close ```` opens an info string. Treating it as a close put
    /// the rest of the block outside the fence, where its pipe rows were
    /// reformatted — a fenced example being rewritten. Raised on #790.
    #[test]
    fn a_fence_is_not_closed_by_a_line_carrying_an_info_string() {
        let src = "```md\n```not-a-close\n|  x |y|\n|---|---|\n```\n";
        let got = canonical_body(src, true);
        assert!(
            got.contains("|  x |y|"),
            "a row inside the still-open fence was reformatted:\n{got}"
        );
    }

    /// A change of only the final newline still produces a diff that says so.
    ///
    /// `lines()` does not carry it, so the hunk loop saw no change and emitted
    /// two headers and nothing else while the CLI reported drift. Raised on
    /// #790.
    #[test]
    fn a_trailing_newline_change_is_reported_rather_than_shown_as_empty() {
        let d = unified_diff("a.md", "x\ny\n", "x\ny").expect("changed");
        assert!(d.contains("@@"), "a diff with no hunk:\n{d}");
        assert!(d.contains("trailing newline"), "{d}");
    }
}

#[cfg(test)]
mod third_round {
    use super::*;

    /// The relabel is the **summary** table's, not every table's.
    ///
    /// A later table may legitimately carry a `| **Status** | … |` row, and
    /// rewriting it is changing authored content. Raised on #790.
    #[test]
    fn only_the_first_table_is_the_summary_table() {
        let doc = "---\ntype: adr\n---\n\n| **Status** | Accepted |\n|---|---|\n\n\
                   ## Later\n\n| **Status** | what it means |\n|---|---|\n";
        let got = canonical(doc);
        assert!(got.contains("| **State** | Accepted |"), "{got}");
        assert!(
            got.contains("| **Status** | what it means |"),
            "a later table's own Status column was rewritten:\n{got}"
        );
    }

    /// ADR detection is the shared rule, so it accepts what that rule accepts.
    #[test]
    fn the_shared_declaration_rule_decides() {
        let row = "| **Status** | Accepted |\n|---|---|\n";
        for spelling in ["type: adr", "Type: adr", "type : adr", "type: ADR"] {
            let doc = format!("---\nadr-id: \"0001\"\n{spelling}\n---\n\n{row}");
            assert!(
                canonical(&doc).contains("**State**"),
                "`{spelling}` was not read as an ADR"
            );
        }
        let doc = format!("---\ntype: blueprint\n---\n\n{row}");
        assert!(
            canonical(&doc).contains("**Status**"),
            "a blueprint was relabelled"
        );
    }

    /// An empty range starts at zero, which is what `patch` accepts.
    #[test]
    fn the_hunk_header_is_valid_at_the_empty_file_boundary() {
        let d = unified_diff("a.md", "", "added\n").expect("changed");
        assert!(
            d.contains("@@ -0,0 +1,1 @@"),
            "insertion into an empty file:\n{d}"
        );
        let d = unified_diff("a.md", "gone\n", "").expect("changed");
        assert!(
            d.contains("@@ -1,1 +0,0 @@"),
            "deletion to an empty file:\n{d}"
        );
    }
}

#[cfg(test)]
mod properties {
    use super::*;
    use std::collections::BTreeMap;

    /// Fragments that have each, at some point, been formatted wrongly.
    ///
    /// Deterministic rather than random: every one of these is a shape a review
    /// round found, so the generator is a record of what this module has
    /// actually got wrong rather than a guess at what it might.
    const FRAGMENTS: &[&str] = &[
        "# Heading\n",
        "prose with a | pipe in it\n",
        "|\n",
        "| a | b |\n|---|---|\n| 1 | 2 |\n",
        "|  a |b |\n| --- | --- |\n",
        "| -- | -- |\n",
        "|| empty first |\n|---|---|\n",
        "| a \\| b | c |\n|---|---|\n",
        "| a \\|\n|---|\n",
        "| `<|im_start|>` | x |\n|---|---|\n",
        "| ``a ` | b`` | c |\n|---|---|\n",
        "| **Status** | Accepted |\n|---|---|\n",
        "```md\n|  x |y|\n```\n",
        "````md\n```\n|  x |y|\n```\n````\n",
        "```md\n```not-a-close\n|  x |y|\n```\n",
        "    |  indented |code|\n",
        "\t|  tab indented |code|\n",
        "    ```\n",
        "\t```\n",
        "last-modified : 2026-9-1\n",
        "| :--- | ---: | :---: |\n",
        "\n",
    ];

    /// The frontmatter blocks, including the two that were mishandled.
    const FRONTS: &[&str] = &[
        "",
        "---\n---\n",
        "---\n\n---\n",
        "---\ntype: adr\nadr-id: \"0001\"\nversion: \"1.0\"\nTitle: T\n---\n",
        "---\nType: adr\n# a comment\nlast-modified: 2026-9-1\n---\n",
        "---\nsite-page: x/y\nstatus: deprecated\n---\n",
    ];

    /// What the document *says*, with everything `fmt` is allowed to move
    /// removed.
    ///
    /// Whitespace-splitting is not enough: `|  a |b |` tokenises to `|b` and
    /// `| a | b |` does not, and normalising exactly that padding is the job. So
    /// a table row is compared as its **cells**, which is the thing that must
    /// survive — a lost column changes their count and a lost escape changes
    /// their content. Everything else is compared with its whitespace
    /// collapsed. Separator rows are dropped, because normalising their dashes
    /// is the point, and the ADR summary relabel is folded in as the one
    /// deliberate substitution, as is the `last-modified` padding.
    fn says(text: &str) -> BTreeMap<String, usize> {
        let mut out = BTreeMap::new();
        let mut fence: Option<(char, usize)> = None;
        for line in text.lines() {
            let in_fence = fence.is_some();
            if let Some((ch, len)) = fence {
                if fence_of(line).is_some_and(|(c, n)| {
                    c == ch && n >= len && line.trim_start().trim_start_matches(c).trim().is_empty()
                }) {
                    fence = None;
                }
            } else if let Some(f) = fence_of(line) {
                fence = Some(f);
            }
            // A separator row has a pipe. Without that test `---` — a
            // *frontmatter delimiter* — is read as a one-column separator and
            // skipped, which is how deleting a document's frontmatter went
            // unnoticed by this very invariant.
            let key = if !in_fence && line.contains('|') && is_separator_row(line) {
                continue;
            } else if !in_fence && is_table_line(line) {
                split_cells(line.trim()).join("\u{1}")
            } else {
                line.split_whitespace().collect::<Vec<_>>().join(" ")
            };
            // The two deliberate substitutions, applied to both sides so the
            // invariant measures *unintended* change only: the ADR summary
            // relabel, and the date padding.
            let key = normalise_value(&key).replace("**Status**", "**State**");
            if !key.is_empty() {
                *out.entry(key).or_default() += 1;
            }
        }
        out
    }

    /// Over every front × fragment-pair document: canonicalising is idempotent
    /// and loses nothing.
    ///
    /// # What this cannot see, and why that is stated rather than assumed
    ///
    /// [`says`] calls [`split_cells`] and [`is_separator_row`] — the functions
    /// under test — so a defect *inside them* applies to both sides of the
    /// comparison and cancels out. Verified rather than reasoned about:
    /// re-introducing the escaped-trailing-pipe bug leaves this test green.
    ///
    /// So it covers the shapes where a defect changes the **document** —
    /// a dropped line, an invented table, a lost frontmatter delimiter, a
    /// failure to converge — and the cell-splitting rules keep the explicit
    /// regression tests above, which do not share an implementation with what
    /// they check.
    #[test]
    fn canonicalising_is_idempotent_and_loses_no_content() {
        let mut checked = 0_usize;
        for front in FRONTS {
            for (i, a) in FRAGMENTS.iter().enumerate() {
                for b in FRAGMENTS.iter().skip(i) {
                    let doc = format!("{front}{a}\n{b}");
                    let once = canonical(&doc);
                    assert_eq!(
                        canonical(&once),
                        once,
                        "not idempotent for:\n{doc:?}\nfirst pass:\n{once:?}"
                    );
                    assert_eq!(
                        says(&doc),
                        says(&once),
                        "content changed for:\n{doc:?}\ninto:\n{once:?}"
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 500, "only {checked} documents generated");
    }
}

#[cfg(test)]
mod fifth_round {
    use super::*;

    /// An empty frontmatter block is frontmatter, and survives.
    ///
    /// `split_frontmatter` returns an empty `front` for both "no frontmatter"
    /// and "empty frontmatter", so testing `front.is_empty()` treated
    /// `---\n\n---` as body text and dropped both delimiters — `--write`
    /// deleting a document's frontmatter. Raised on #790.
    #[test]
    fn an_empty_frontmatter_block_is_not_deleted() {
        for src in ["---\n---\n# T\n", "---\n\n---\n# T\n"] {
            let got = canonical(src);
            assert!(
                got.starts_with("---\n") && got[4..].contains("---\n"),
                "frontmatter delimiters lost from {src:?}: {got:?}"
            );
        }
        // A document with no frontmatter must not gain one.
        assert_eq!(canonical("# T\n"), "# T\n");
    }

    /// A trailing `\|` is content, not the closing delimiter.
    ///
    /// `strip_suffix('|')` removed the pipe of the escape, leaving a bare
    /// backslash — the formatter corrupting the cell its escape handling exists
    /// to protect. Raised on #790.
    #[test]
    fn an_escaped_pipe_at_the_end_of_a_row_survives() {
        assert_eq!(split_cells(r"| a \|"), vec![r"a \|"]);
        assert!(
            !canonical_table_row(r"| a \|").contains(r"\ "),
            "the escape was split from its pipe: {}",
            canonical_table_row(r"| a \|")
        );
        // A genuine closing delimiter is still stripped.
        assert_eq!(split_cells("| a |"), vec!["a"]);
        // And a doubled backslash escapes itself, so that pipe *is* a delimiter.
        assert_eq!(split_cells(r"| a \\|"), vec![r"a \\"]);
    }
}

#[cfg(test)]
mod sixth_round {
    use super::*;

    /// Four spaces or a tab is code, and a run of backticks in it is content.
    ///
    /// `fence_of` trimmed all indentation, so an indented backtick run opened a fence
    /// that never closed — and every real table after it stayed unformatted,
    /// silently. Raised on #790.
    #[test]
    fn an_indented_backtick_run_is_not_a_fence() {
        assert!(fence_of("```md").is_some());
        assert!(fence_of("   ```").is_some(), "three spaces is still markup");
        assert!(fence_of("    ```").is_none(), "four spaces is code");
        assert!(fence_of("\t```").is_none(), "a tab is code");
        // A table after an indented backtick run is still formatted.
        let got = canonical_body("    ```\n\n|  a |b |\n|---|---|\n", true);
        assert!(
            got.contains("| a | b |"),
            "left fenced by an indented run:\n{got}"
        );
    }

    /// A leading tab is four columns, not one byte.
    #[test]
    fn a_tab_indented_row_is_code_not_a_table() {
        assert!(is_table_line("| a | b |"));
        assert!(is_table_line("   | a | b |"));
        assert!(!is_table_line("    | a | b |"), "four spaces is code");
        assert!(!is_table_line("\t| a | b |"), "a tab is code");
        assert_eq!(canonical_body("\t|  a |b |\n", true), "\t|  a |b |\n");
    }

    /// The frontmatter key is parsed the way the ADR parser parses it.
    #[test]
    fn the_date_key_is_parsed_not_prefix_matched() {
        assert_eq!(
            normalise_value("last-modified : 2026-9-1"),
            "last-modified : 2026-09-01"
        );
        assert_eq!(
            normalise_value("Last-Modified: 2026-9-1"),
            "Last-Modified: 2026-09-01"
        );
        // Indented keys are nested values, not top-level ones.
        assert_eq!(
            normalise_value("  last-modified: 2026-9-1"),
            "  last-modified: 2026-9-1"
        );
        assert_eq!(normalise_value("other: 2026-9-1"), "other: 2026-9-1");
    }
}

//! A controlled-English style lint for prose (ADR-0031 §2).
//!
//! In the spirit of ASD-STE100 Simplified Technical English — short sentences,
//! one instruction per sentence, plain words, one name per concept — and
//! deliberately **not** a certification against it: the official dictionary is
//! ASD's and is not shipped here; the word list below is Roteiro's own.
//!
//! [`lint`] is a pure function of its input: the same text always gets the same
//! report, and no model is consulted. It reads prose and skips what is not prose
//! — fenced blocks, code spans, wiki links, graph keys, paths and URLs, headings
//! and tables — which is most of what a Roteiro answer carries.
//!
//! **It is a heuristic and will misread some sentences.** That is why nothing
//! gates on it (ADR-0031): it reports, and a person decides.

use std::collections::BTreeMap;

use serde::Serialize;

/// Most words an instruction sentence should carry.
pub const PROCEDURAL_LIMIT: usize = 20;
/// Most words a descriptive sentence should carry.
pub const DESCRIPTIVE_LIMIT: usize = 25;

/// Which rule an issue breaks.
///
/// `#[non_exhaustive]`: the rule set is expected to grow — webSTE100 lists article
/// and subject checks as next — and a consumer should not have to change to keep
/// compiling when it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[non_exhaustive]
#[serde(rename_all = "kebab-case")]
pub enum StyleRule {
    /// Longer than [`PROCEDURAL_LIMIT`] or [`DESCRIPTIVE_LIMIT`] words.
    SentenceLength,
    /// An instruction that gives more than one instruction.
    MultipleInstructions,
    /// A word the list has a plainer replacement for.
    UnapprovedWord,
    /// A word or phrase that carries no information.
    Filler,
    /// A form of *be* followed by a past participle.
    PassiveVoice,
    /// More than one name for one concept in the same text.
    TermDrift,
}

impl StyleRule {
    /// The rule's stable token, as serialised.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SentenceLength => "sentence-length",
            Self::MultipleInstructions => "multiple-instructions",
            Self::UnapprovedWord => "unapproved-word",
            Self::Filler => "filler",
            Self::PassiveVoice => "passive-voice",
            Self::TermDrift => "term-drift",
        }
    }
}

/// One rule broken once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StyleIssue {
    /// The rule.
    pub rule: StyleRule,
    /// The 1-based sentence it was found in; `0` for a text-wide issue
    /// ([`StyleRule::TermDrift`]).
    pub sentence: usize,
    /// The 1-based line of the input the sentence starts on; `0` for a
    /// text-wide issue.
    pub line: usize,
    /// What to change, in a form a person or a model can act on.
    pub message: String,
}

/// What [`lint`] found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StyleReport {
    /// 0–100: the share of sentences that break no rule, less five per
    /// text-wide issue. Style only — it says nothing about whether the text is
    /// true or complete (ADR-0031).
    pub score: u8,
    /// Prose sentences read.
    pub sentences: usize,
    /// Every issue, in sentence order.
    pub issues: Vec<StyleIssue>,
}

/// Words with a plainer replacement. Roteiro's own list, not ASD's dictionary.
const UNAPPROVED: &[(&str, &str)] = &[
    ("utilize", "use"),
    ("utilise", "use"),
    ("leverage", "use"),
    ("facilitate", "help"),
    ("commence", "start"),
    ("terminate", "stop"),
    ("endeavour", "try"),
    ("endeavor", "try"),
    ("attempt", "try"),
    ("ensure", "make sure"),
    ("numerous", "many"),
    ("sufficient", "enough"),
    ("additional", "more"),
    ("subsequently", "then"),
    ("consequently", "so"),
    ("approximately", "about"),
    ("methodology", "method"),
    ("functionality", "function"),
    ("optimal", "best"),
    ("in order to", "to"),
    ("prior to", "before"),
    ("due to the fact that", "because"),
    ("in the event that", "if"),
    ("a number of", "some"),
    ("with regard to", "about"),
    ("is able to", "can"),
];

/// Words and phrases that carry no information.
const FILLER: &[&str] = &[
    "basically",
    "simply",
    "just",
    "very",
    "really",
    "actually",
    "obviously",
    "clearly",
    "of course",
    "in fact",
    "it should be noted that",
    "it is important to note that",
    "needless to say",
    "seamlessly",
];

/// Groups of names for one concept; using more than one in a text is drift.
const DRIFT: &[&[&str]] = &[
    &["repository", "repo"],
    &["directory", "folder"],
    &["delete", "erase"],
    &["start", "launch"],
];

/// Verbs that open an instruction.
const IMPERATIVE: &[&str] = &[
    "add",
    "build",
    "call",
    "change",
    "check",
    "clone",
    "close",
    "commit",
    "configure",
    "copy",
    "create",
    "delete",
    "disable",
    "do",
    "download",
    "edit",
    "enable",
    "export",
    "fetch",
    "find",
    "fix",
    "install",
    "keep",
    "load",
    "make",
    "merge",
    "move",
    "open",
    "pass",
    "pin",
    "pull",
    "push",
    "put",
    "read",
    "rebase",
    "remove",
    "rename",
    "replace",
    "restart",
    "run",
    "save",
    "select",
    "send",
    "set",
    "start",
    "stop",
    "tag",
    "test",
    "type",
    "update",
    "use",
    "verify",
    "write",
];

/// Participles that do not end in `-ed`.
const IRREGULAR_PARTICIPLES: &[&str] = &[
    "built", "chosen", "done", "drawn", "driven", "found", "given", "held", "kept", "known",
    "left", "made", "meant", "read", "run", "seen", "sent", "set", "shown", "split", "taken",
    "thrown", "told", "written",
];

/// `-ed` words that follow *be* as adjectives, not passives.
const ED_ADJECTIVES: &[&str] = &[
    "advanced",
    "based",
    "complicated",
    "concerned",
    "detailed",
    "experienced",
    "interested",
    "limited",
    "needed",
    "sophisticated",
    "supposed",
    "tired",
    "used",
];

const BE: &[&str] = &["am", "is", "are", "was", "were", "be", "been", "being"];

/// Lint `text`, which may be Markdown.
#[must_use]
pub fn lint(text: &str) -> StyleReport {
    let sentences = sentences(&prose_lines(text));
    let mut issues = Vec::new();
    for (i, s) in sentences.iter().enumerate() {
        check_sentence(i + 1, s, &mut issues);
    }
    let drift = drift_issues(&sentences);
    let failing: std::collections::BTreeSet<usize> = issues
        .iter()
        .filter(|i| i.rule != StyleRule::PassiveVoice)
        .map(|i| i.sentence)
        .collect();
    let score = if sentences.is_empty() {
        100
    } else {
        let clean = sentences.len() - failing.len();
        let base = clean * 100 / sentences.len();
        base.saturating_sub(drift.len() * 5)
    };
    issues.extend(drift);
    StyleReport {
        score: u8::try_from(score.min(100)).unwrap_or(100),
        sentences: sentences.len(),
        issues,
    }
}

/// One prose sentence: its words (lowercased, punctuation stripped, a masked
/// non-prose run counting as one word) and whether it opens as an instruction.
struct Sentence {
    words: Vec<String>,
    procedural: bool,
    line: usize,
}

/// The prose lines of `text` with their 1-based line numbers: fences,
/// headings, tables and HTML dropped, list markers stripped, and each non-prose
/// run masked to the single word `x`. A blank or dropped line is an empty entry,
/// which ends a paragraph.
fn prose_lines(text: &str) -> Vec<(String, bool, usize)> {
    let mut out = Vec::new();
    let mut fence: Option<&str> = None;
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        let n = i + 1;
        if let Some(open) = fence {
            if line.starts_with(open) {
                fence = None;
            }
            continue;
        }
        if line.starts_with("```") || line.starts_with("~~~") {
            fence = Some(&line[..3]);
            out.push((String::new(), false, n));
            continue;
        }
        if line.starts_with(['#', '|', '<']) {
            out.push((String::new(), false, n));
            continue;
        }
        let (body, item) = strip_list_marker(line.trim_start_matches('>').trim());
        out.push((
            if body.is_empty() {
                String::new()
            } else {
                mask(body)
            },
            item,
            n,
        ));
    }
    out
}

/// A list item's text, and whether there was a marker (`-`, `*`, `+`, `1.`).
fn strip_list_marker(line: &str) -> (&str, bool) {
    for m in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(m) {
            return (rest.trim(), true);
        }
    }
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if digits > 0
        && let Some(rest) = line[digits..]
            .strip_prefix(". ")
            .or(line[digits..].strip_prefix(") "))
    {
        return (rest.trim(), true);
    }
    (line, false)
}

/// Replace each code span, wiki link and Markdown link target with one word, and
/// each whitespace-delimited token that is a key, a path or a URL with `x`.
fn mask(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(c) = rest.chars().next() {
        let close = match c {
            '`' => Some("`"),
            '[' if rest.starts_with("[[") => Some("]]"),
            _ => None,
        };
        if let Some(close) = close {
            let open_len = if close == "]]" { 2 } else { 1 };
            if let Some(end) = rest[open_len..].find(close) {
                out.push('x');
                rest = &rest[open_len + end + close.len()..];
                continue;
            }
        }
        if rest.starts_with("](")
            && let Some(end) = rest.find(')')
        {
            rest = &rest[end + 1..];
            continue;
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out.split_whitespace()
        .map(|tok| if is_code_like(tok) { "x" } else { tok })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A token that names code rather than saying something: a graph key, a path,
/// a URL, a Rust path, or an identifier with an underscore.
fn is_code_like(tok: &str) -> bool {
    let t = tok.trim_matches(|c: char| !c.is_alphanumeric());
    t.contains("::")
        || t.contains('/')
        || t.contains('_')
        || t.contains("://")
        || ["sym:", "adr:", "file:", "fn:", "doc:"]
            .iter()
            .any(|p| t.starts_with(p))
}

/// Split prose lines into sentences. A paragraph ends at a blank line, and a
/// list item is its own paragraph, so an item with no full stop is still one
/// sentence.
fn sentences(lines: &[(String, bool, usize)]) -> Vec<Sentence> {
    let mut out = Vec::new();
    let mut para: Vec<(&str, usize)> = Vec::new();
    for (text, item, n) in lines {
        if (*item || text.is_empty()) && !para.is_empty() {
            split_sentences(&para, &mut out);
            para.clear();
        }
        para.extend(text.split_whitespace().map(|w| (w, *n)));
    }
    split_sentences(&para, &mut out);
    out
}

/// Split one paragraph's words on a closing `.`, `!` or `?`, guarding common
/// abbreviations.
fn split_sentences(para: &[(&str, usize)], out: &mut Vec<Sentence>) {
    const ABBREV: &[&str] = &["e.g.", "i.e.", "etc.", "vs.", "cf.", "approx.", "no."];
    let mut words = Vec::new();
    let mut line = 0;
    for (i, (w, n)) in para.iter().enumerate() {
        if words.is_empty() {
            line = *n;
        }
        let word = w
            .trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
            .to_lowercase();
        if !word.is_empty() {
            words.push(word);
        }
        let ends = w.ends_with(['.', '!', '?']) && !ABBREV.contains(&w.to_lowercase().as_str());
        if (ends || i + 1 == para.len()) && !words.is_empty() {
            let procedural = IMPERATIVE.contains(&words[0].as_str())
                || (words[0] == "do" && words.get(1).is_some_and(|w| w == "not"));
            out.push(Sentence {
                words: std::mem::take(&mut words),
                procedural,
                line,
            });
        }
    }
}

fn check_sentence(n: usize, s: &Sentence, issues: &mut Vec<StyleIssue>) {
    let (limit, kind) = if s.procedural {
        (PROCEDURAL_LIMIT, "an instruction")
    } else {
        (DESCRIPTIVE_LIMIT, "a description")
    };
    if s.words.len() > limit {
        issues.push(StyleIssue {
            rule: StyleRule::SentenceLength,
            sentence: n,
            line: s.line,
            message: format!(
                "{} words; at most {limit} for {kind} — split it",
                s.words.len()
            ),
        });
    }
    if s.procedural
        && let Some(second) = s.words.windows(2).find_map(|w| {
            (matches!(w[0].as_str(), "and" | "then") && IMPERATIVE.contains(&w[1].as_str()))
                .then(|| w[1].clone())
        })
    {
        issues.push(StyleIssue {
            rule: StyleRule::MultipleInstructions,
            sentence: n,
            line: s.line,
            message: format!("a second instruction (`{second}`) — give it its own sentence"),
        });
    }
    let joined = format!(" {} ", s.words.join(" "));
    for (word, plain) in UNAPPROVED {
        if contains_phrase(&joined, word) {
            issues.push(StyleIssue {
                rule: StyleRule::UnapprovedWord,
                sentence: n,
                line: s.line,
                message: format!("\"{word}\" — use \"{plain}\""),
            });
        }
    }
    for filler in FILLER {
        if contains_phrase(&joined, filler) {
            issues.push(StyleIssue {
                rule: StyleRule::Filler,
                sentence: n,
                line: s.line,
                message: format!("\"{filler}\" says nothing — remove it"),
            });
        }
    }
    if let Some(pp) = passive(&s.words) {
        issues.push(StyleIssue {
            rule: StyleRule::PassiveVoice,
            sentence: n,
            line: s.line,
            message: format!("passive (\"… {pp}\") — say who does it"),
        });
    }
}

/// `phrase` as whole words in ` joined ` (space-padded, lowercased), allowing a
/// plural or third-person `s`/`es`/`d`/`ing` on a single word.
fn contains_phrase(joined: &str, phrase: &str) -> bool {
    if phrase.contains(' ') {
        return joined.contains(&format!(" {phrase} "));
    }
    joined.split(' ').any(|w| {
        w == phrase
            || ["s", "es", "d", "ed", "ing"].iter().any(|suf| {
                w.strip_suffix(suf)
                    .is_some_and(|stem| stem == phrase || stem == phrase.trim_end_matches('e'))
            })
    })
}

/// The participle of the first *be* + participle pair, allowing one `-ly`
/// adverb between them.
fn passive(words: &[String]) -> Option<String> {
    words.iter().enumerate().find_map(|(i, w)| {
        if !BE.contains(&w.as_str()) {
            return None;
        }
        let mut next = words.get(i + 1)?;
        if next.ends_with("ly") {
            next = words.get(i + 2)?;
        }
        // `unchanged`, `unset`: an `un-` participle after *be* is a state, not
        // an action anyone performed.
        let participle = !next.starts_with("un")
            && ((next.ends_with("ed")
                && next.len() > 3
                && !ED_ADJECTIVES.contains(&next.as_str()))
                || IRREGULAR_PARTICIPLES.contains(&next.as_str()));
        participle.then(|| next.clone())
    })
}

/// One issue per [`DRIFT`] group the text uses more than one name from.
fn drift_issues(sentences: &[Sentence]) -> Vec<StyleIssue> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for s in sentences {
        for w in &s.words {
            for group in DRIFT {
                for name in *group {
                    if w == name || w.strip_suffix('s') == Some(name) {
                        *counts.entry(name).or_default() += 1;
                    }
                }
            }
        }
    }
    DRIFT
        .iter()
        .filter_map(|group| {
            let used: Vec<&str> = group
                .iter()
                .copied()
                .filter(|n| counts.contains_key(n))
                .collect();
            (used.len() > 1).then(|| {
                let keep = used
                    .iter()
                    .copied()
                    .max_by_key(|n| (counts[n], std::cmp::Reverse(*n)))
                    .unwrap_or(used[0]);
                StyleIssue {
                    rule: StyleRule::TermDrift,
                    sentence: 0,
                    line: 0,
                    message: format!(
                        "{} name one thing — use \"{keep}\" throughout",
                        used.iter()
                            .map(|n| format!("\"{n}\""))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                }
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{StyleRule, lint};

    fn rules(text: &str) -> Vec<StyleRule> {
        lint(text).issues.iter().map(|i| i.rule).collect()
    }

    #[test]
    fn plain_short_prose_is_clean() {
        let r = lint("The store keeps one row per node. Run `roteiro check` before you push.");
        assert_eq!(r.issues, vec![], "{r:?}");
        assert_eq!((r.score, r.sentences), (100, 2));
    }

    #[test]
    fn a_long_description_and_a_long_instruction_have_different_limits() {
        // Both are 25 words: over the instruction limit, at the description one.
        let instruction = "Run the tool on each file in the tree so that every line of output it \
                           gives you comes back to this one terminal window.";
        assert_eq!(rules(instruction), vec![StyleRule::SentenceLength]);
        let description = "The tool runs on each file in the tree so that every line of output it \
                           gives you comes back to this one terminal window.";
        assert!(rules(description).is_empty(), "{:?}", lint(description));
    }

    #[test]
    fn code_spans_keys_and_paths_count_as_one_word_and_are_never_judged() {
        let r = lint(
            "`utilize_buffer(self, leverage)` reads [[docs/adr/0007-x.md]] and sym:rust:a.rs#f in crates/rto-graph/src/prose.rs.",
        );
        assert!(r.issues.is_empty(), "{r:?}");
    }

    #[test]
    fn fenced_blocks_headings_and_tables_are_skipped() {
        let r = lint(
            "# Utilize\n\n```\nwe basically utilize it\n```\n| utilize | x |\n\nThe store is small.",
        );
        assert!(r.issues.is_empty(), "{r:?}");
        assert_eq!(r.sentences, 1);
    }

    #[test]
    fn unapproved_words_and_phrases_name_the_plain_one() {
        let r = lint("We utilize the cache in order to save time.");
        let msgs: Vec<&str> = r.issues.iter().map(|i| i.message.as_str()).collect();
        assert!(msgs.contains(&"\"utilize\" — use \"use\""), "{msgs:?}");
        assert!(msgs.contains(&"\"in order to\" — use \"to\""), "{msgs:?}");
        assert_eq!(r.score, 0);
    }

    #[test]
    fn an_instruction_with_a_second_instruction_is_flagged() {
        assert_eq!(
            rules("Open the file and delete the line."),
            vec![StyleRule::MultipleInstructions]
        );
        assert!(rules("The tool opens the file and deletes the line.").is_empty());
    }

    #[test]
    fn passive_is_reported_but_does_not_lower_the_score() {
        let r = lint("The row is written by the store.");
        assert_eq!(
            rules("The row is written by the store."),
            vec![StyleRule::PassiveVoice]
        );
        assert_eq!(r.score, 100);
        assert!(rules("The input is limited to one file.").is_empty());
        assert!(rules("The document is unchanged.").is_empty());
    }

    #[test]
    fn filler_is_flagged() {
        assert_eq!(rules("It basically works."), vec![StyleRule::Filler]);
    }

    #[test]
    fn two_names_for_one_concept_are_drift() {
        let r = lint("Clone the repository. The repo has one branch. The repository is small.");
        let drift: Vec<_> = r
            .issues
            .iter()
            .filter(|i| i.rule == StyleRule::TermDrift)
            .collect();
        assert_eq!(drift.len(), 1, "{r:?}");
        assert_eq!(
            drift[0].message,
            "\"repository\", \"repo\" name one thing — use \"repository\" throughout"
        );
        assert_eq!(r.score, 95);
    }

    #[test]
    fn list_items_are_sentences_even_without_a_full_stop() {
        let r = lint("- Open the file\n- Read the first line\n");
        assert_eq!(r.sentences, 2);
    }

    #[test]
    fn an_issue_names_the_line_its_sentence_starts_on() {
        let r = lint("---\n\nThe store is small.\n\nWe utilize\nthe cache.");
        assert_eq!(r.issues.len(), 1, "{r:?}");
        assert_eq!((r.issues[0].sentence, r.issues[0].line), (2, 5));
    }

    #[test]
    fn a_blank_line_ends_a_sentence_with_no_full_stop() {
        assert_eq!(
            lint("A heading-like line\n\nThe store is small.").sentences,
            2
        );
    }

    #[test]
    fn abbreviations_do_not_end_a_sentence() {
        assert_eq!(lint("Some tools, e.g. Clippy, read it.").sentences, 1);
    }

    #[test]
    fn the_same_text_always_gets_the_same_report() {
        let t =
            "We utilize the repo. The repository is basically written by hand and we then push it.";
        assert_eq!(lint(t), lint(t));
    }
}

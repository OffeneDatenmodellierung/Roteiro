//! The reviewer's pure core: what to ask, how to read the answer, and what the
//! answer is allowed to claim (Stage 35b).
//!
//! [`crate::review_score`] built the instrument; this is the thing it measures.
//! Everything here is a pure function of bytes — prompt assembly, response
//! parsing, budget arithmetic and the [`crate::compile_claim`] site derivation —
//! so the whole of the reviewer's *judgement* is testable offline, with no model,
//! no network and no git. What is left outside is a loop that calls an engine,
//! and that lives in the binary.
//!
//! # Per file, and the budget is not the constraint
//!
//! 35a established that whole-diff review is not the shape: reconstructing the 15
//! review diffs costs ~513k tokens, ~34k mean, and **9 of 15 exceed the ~30k
//! single-call budget**. What that argument left open is how much room *per-file*
//! review actually has, and the answer is: a great deal. Measured over all **190
//! file-diffs** in the corpus:
//!
//! | | raw | annotated |
//! |---|---|---|
//! | mean | 2,704 | 3,275 |
//! | median | 1,476 | 1,758 |
//! | p90 | 5,621 | 6,711 |
//!
//! The second column is what a review actually sends, because [`annotate_diff`]
//! adds a line-number column. That costs a measured **1.21×** over the corpus —
//! and the shape of the cost is worth knowing: it is 9 characters per *line*, so
//! it is ~1.2× on ordinary source and would be ~4× on a diff of two-character
//! lines. It is charged against the budget rather than estimated around.
//!
//! Even so, exactly **one of the 190** exceeds the single-call budget annotated,
//! and it is a generated JSON fixture; the next largest is `Cargo.lock`. **The
//! largest reviewable *source* file-diff in the entire corpus is 14,034 tokens
//! raw and 17,202 annotated** — still under two-thirds of the single-call budget.
//! So the median file leaves ~28k of that budget unused and the worst source file
//! leaves ~12k.
//!
//! That matters for one reason. The stage's central claim is that pre-assembled
//! graph context lets a per-file reviewer see a doc in *another* file
//! contradicting the code under review — the `contract-drift` class. Had the
//! per-file budget been tight, that claim would have been untestable on this
//! repository whatever the graph contained. It is not tight. [`GraphContext`] is
//! the slot that headroom is for, and this module reserves it while shipping it
//! empty: PR 1 measures the diff-only arm, and a filled slot is the comparison.
//!
//! # The prompt is derived from the standards, not from the corpus
//!
//! [`build_prompt`] states the house's review standards — contract accuracy, the
//! defect vocabulary, the output shape — from `docs/REVIEW_CHECKLIST.md` and
//! [`crate::review_corpus::DefectClass`], both of which predate it. It is
//! deliberately **not** written against the corpus rows.
//!
//! This is a property of the experiment rather than a style preference. A prompt
//! tuned until the known rows are found measures how well it was tuned, and the
//! resulting recall would not survive the 23rd row. The rows are the test set and
//! nothing here may read them, which is why this module depends on `DefectClass`
//! and not on [`crate::review_corpus::BUILTIN`].
//!
//! # Nothing here decides what is true
//!
//! [`parse_findings`] converts what a model said into
//! [`crate::review_score::CandidateFinding`]s and no further. It does not check a
//! finding, rank it, or drop it for looking implausible. The one filter in this
//! module is [`crate::compile_claim`]'s, and even that is applied by the caller
//! against evidence the caller supplies — see [`claim_site`], which only computes
//! *what configuration the code needs*, never whether a check ran.

use std::fmt::Write as _;

use crate::compile_claim::{ClaimSite, Features, TargetOs};
use crate::review_corpus::{CLASSES, DefectClass};
use crate::review_score::CandidateFinding;

/// The measured single-call context budget on this repository, in tokens.
///
/// 35a's figure, and the one 189 of the corpus's 190 file-diffs fit inside. Used
/// as the default per-file budget because a per-file reviewer that also fits the
/// single-call budget needs no second number to explain.
pub const SINGLE_CALL_BUDGET_TOKENS: usize = 30_000;

/// Estimate a string's token count as `len / 4`.
///
/// The same basis every budget figure in this stage is quoted on — 35a's
/// corpus-wide totals and this module's per-file distribution alike — so the
/// numbers compare. It is an estimate of the right order, **not** a tokeniser's
/// count, and is deliberately not swapped for one: a real count would need the
/// model's vocabulary, which would make a pure function depend on which model is
/// installed and make two runs on two machines incomparable.
#[must_use]
pub fn estimate_tokens(text: &str) -> usize {
    text.len() / 4
}

/// One file to review, with the diff that changed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileUnderReview {
    /// The commit being reviewed — a corpus `reviewed_sha` on a replay run.
    pub reviewed_sha: String,
    /// Repository-relative path.
    pub path: String,
    /// The unified diff for this file alone.
    pub diff: String,
}

/// One piece of pre-assembled, provenance-tagged context for the file under
/// review — **the slot that PR 1 ships empty**.
///
/// The graph's contribution to a review is not access: an agentic reviewer can
/// read any file it likes via tool calls, and one has been observed doing so
/// correctly on this very corpus. It is that the relevant context arrives
/// *already selected and already labelled with where it came from*, so the model
/// spends its budget reading rather than searching.
///
/// [`provenance`](Self::provenance) is carried into the prompt rather than
/// flattened away because the three layers mean different things to a reviewer: a
/// `derived` fact is a deterministic function of the bytes, an `authored` one is
/// somebody's stated intent, and an `inferred` one is a guess with a confidence.
/// A reviewer told an ADR *governs* a symbol is being told something different
/// from a reviewer handed a similar-looking file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextItem {
    /// What this is, for the prompt — `ADR-0019 §3`, or `callers of resolve`.
    pub label: String,
    /// `derived` | `authored` | `inferred` — the graph's own vocabulary.
    pub provenance: String,
    /// The text itself.
    pub body: String,
}

/// The graph context handed to one file's review.
///
/// Empty in PR 1 by construction: [`GraphContext::none`] is what the diff-only arm
/// passes, and [`build_prompt`] renders no context section for it, so the
/// baseline prompt carries no vestigial heading promising something that is not
/// there. The type exists now so the graph arm is a filled slot rather than a
/// re-plumb of the driver.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphContext {
    /// The items, in the order they are rendered.
    pub items: Vec<ContextItem>,
}

impl GraphContext {
    /// No context — the diff-only arm.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Whether any context is present.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// An assembled prompt, with what it cost and what it had to leave out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    /// The text to send.
    pub text: String,
    /// Its estimated size, by [`estimate_tokens`].
    pub tokens: usize,
    /// Diff tokens dropped to fit the budget, or `0`.
    ///
    /// Reported rather than silently absorbed: a review of a truncated file is a
    /// review of part of it, and a run that does not say so reads as coverage it
    /// did not have.
    pub dropped_tokens: usize,
}

/// The output contract, stated once and used twice — [`build_prompt`] asks for
/// this and [`parse_findings`] reads it.
///
/// A line format rather than JSON, because the failure modes are not symmetric.
/// A model that mangles one line of a line format loses that finding; a model
/// that mangles one brace of a JSON document loses the whole review, and the
/// low-tier instruct model this resolver defaults to does the second more often
/// than the first.
const FINDING_PREFIX: &str = "FINDING";

/// What a model emits when it has nothing to report — required, so that "no
/// findings" and "the model ignored the format" are distinguishable in
/// [`Parsed::unparsed`] rather than both arriving as silence.
const NO_FINDINGS: &str = "NO FINDINGS";

/// Build the review prompt for one file.
///
/// `budget` caps the whole prompt; the diff is truncated to fit and the amount
/// dropped is reported on [`Prompt::dropped_tokens`]. Context is never truncated —
/// a half-quoted ADR is worse than none, and on the measured distribution it never
/// comes to that.
#[must_use]
pub fn build_prompt(file: &FileUnderReview, context: &GraphContext, budget: usize) -> Prompt {
    let mut head = String::new();
    head.push_str(
        "You are reviewing ONE FILE of a change to a Rust codebase.\n\n\
         The defects that matter here are **contract-accuracy** defects: code that \
         runs correctly but does not mean what it says. They compile, they pass \
         tests, and CI is green on them by definition — so they are found only by \
         reading the words against the behaviour. Do not look for crashes or \
         compile errors; look for places where a promise and its implementation \
         have come apart.\n\n\
         Work through the change against each of these, in order:\n\n",
    );
    for class in CLASSES {
        let _ = writeln!(head, "  {} — {}", class.as_str(), class_gloss(class));
    }
    head.push_str(
        "\nFor each one, ask specifically:\n\
         - Does a doc comment, `///` line, README sentence or ADR in this diff \
         state something the code beside it does not do? Compare the two texts \
         word by word — a doc that describes the old behaviour after the code \
         moved on is the single most common defect in this codebase.\n\
         - Does an error message name the rule it actually enforces, or a \
         different one?\n\
         - Does a test assert the behaviour its name claims, or would it pass with \
         the feature removed?\n\
         - Does a check permit the state it exists to forbid (off-by-one, wrong \
         comparison, missing case)?\n\
         - Is a key, hash or id built from something lossy, so two different \
         inputs collide?\n\n\
         Output format. One finding per line, nothing else on the line:\n\
         \x20   FINDING | line=<n> | class=<class> | compile=<yes|no> | <one sentence>\n\n\
         For example:\n\
         \x20   FINDING | line=214 | class=contract-drift | compile=no | the doc says \
         the cache is unbounded but `insert` evicts at 256 entries\n\
         \x20   FINDING | line=87 | class=permissive-constraint | compile=no | uses \
         `<=` so a zero-length span passes the guard that exists to reject it\n\n\
         Rules:\n\
         - Cite the NEW-SIDE line number from the left column. Every line is \
         numbered for you; never compute one from the hunk header.\n\
         - **Both halves must be visible below.** Report a conflict only when the \
         promise AND the behaviour that breaks it are both in the lines shown. If \
         you can see a doc comment but not the code it describes, or a call but \
         not the signature it calls, you cannot tell whether they disagree — say \
         nothing. Do not infer what code you have not been shown does.\n\
         - Quote the specific words that conflict, so a reader can check you \
         without opening the file.\n\
         - Do not restate one point as several findings. Each finding must be a \
         separate defect a separate commit would fix.\n\
         - `compile=yes` ONLY if you are claiming the code will not build. \
         Everything else is `compile=no`.\n",
    );
    let _ = writeln!(
        head,
        "         - Reply {NO_FINDINGS} only if you have worked through every class \
         above and found nothing. A file whose change is routine is a normal \
         outcome, and reporting nothing is better than reporting a guess."
    );

    let mut context_block = String::new();
    if !context.is_empty() {
        context_block.push_str(
            "\nContext from the repository's graph. This is not part of the \
             change; it is what the graph knows about the code under review, and \
             each item says which layer it came from.\n\n",
        );
        for item in &context.items {
            let _ = writeln!(
                context_block,
                "--- {} [{}]\n{}",
                item.label,
                item.provenance,
                item.body.trim_end()
            );
        }
    }

    let annotated = annotate_diff(&file.diff);
    let tail_header = format!("\nFile under review: {}\n\n", file.path);
    let fixed =
        estimate_tokens(&head) + estimate_tokens(&context_block) + estimate_tokens(&tail_header);
    let room = budget.saturating_sub(fixed);
    let (body, dropped_tokens) = truncate_to_tokens(&annotated, room);

    let text = format!("{head}{context_block}{tail_header}{body}");
    Prompt {
        tokens: estimate_tokens(&text),
        text,
        dropped_tokens,
    }
}

/// A one-line gloss per defect class, for the prompt.
///
/// Written from the class's own meaning rather than from any corpus row, and kept
/// beside [`CLASSES`] so a new class cannot be added without deciding how to
/// describe it to a reviewer. `class_gloss_covers_every_class` holds that.
fn class_gloss(class: DefectClass) -> &'static str {
    match class {
        DefectClass::CleanupGap => "a guard stops a cleanup path doing its job",
        DefectClass::ContractDrift => {
            "a doc comment, README or ADR states something the code does not do"
        }
        DefectClass::ErrorTextDrift => "an error message does not state the rule it enforces",
        DefectClass::FalseCompileClaim => "the code will not compile (see the compile= rule)",
        DefectClass::LintConvention => "a lint suppression carries no justification",
        DefectClass::LossyIdentity => {
            "a key built from a lossy conversion, so distinct inputs collide"
        }
        DefectClass::MissingEvent => "an early return skips a documented side effect",
        DefectClass::OrderingBug => "an aggregate is computed after the mutation it must precede",
        DefectClass::PerfContract => "the implementation defeats a field's stated design goal",
        DefectClass::PermissiveConstraint => "a check permits the state it exists to forbid",
        DefectClass::ProseClarity => "wording that misleads a reader",
        DefectClass::SilentTruncation => "a read or copy drops a remainder without erroring",
        DefectClass::UxDiagnostic => "a message tells the user to do the wrong thing",
        DefectClass::VacuousTest => "a test passes whether or not the behaviour it names works",
    }
}

/// Render a unified diff with **new-side line numbers in a left column**.
///
/// A reviewer's finding is scored by its line, within
/// [`crate::review_score::LINE_WINDOW`]. Asking a model to derive a line number
/// from `@@ -a,b +c,d @@` spends budget on arithmetic it is bad at and turns a
/// correct finding into a miss — which would be measured as the reviewer failing
/// to see the defect rather than failing to count. So the arithmetic is done here,
/// where it is exact.
///
/// Removed lines carry no new-side number and are marked `-`, so the model can
/// still see what was replaced without being able to cite a line that no longer
/// exists.
#[must_use]
pub fn annotate_diff(diff: &str) -> String {
    let mut out = String::with_capacity(diff.len() + diff.len() / 8);
    let mut new_line: Option<u32> = None;
    for raw in diff.lines() {
        if raw.starts_with("@@") {
            new_line = parse_hunk_new_start(raw);
            out.push_str(raw);
            out.push('\n');
            continue;
        }
        // Everything before the first hunk (`diff --git`, `---`, `+++`, mode
        // lines) is passed through unnumbered: it is not file content.
        let Some(n) = new_line else {
            out.push_str(raw);
            out.push('\n');
            continue;
        };
        match raw.as_bytes().first() {
            Some(b'-') => {
                let _ = writeln!(out, "      - |{}", &raw[1..]);
            }
            Some(b'+') => {
                let _ = writeln!(out, "{n:>6} +|{}", &raw[1..]);
                new_line = Some(n + 1);
            }
            Some(b'\\') => {
                let _ = writeln!(out, "        |{raw}");
            }
            // A context line, including the empty string a bare `\n` produces.
            _ => {
                let body = raw.strip_prefix(' ').unwrap_or(raw);
                let _ = writeln!(out, "{n:>6}  |{body}");
                new_line = Some(n + 1);
            }
        }
    }
    out
}

/// The new-side start line of a `@@ -a,b +c,d @@` header.
fn parse_hunk_new_start(header: &str) -> Option<u32> {
    let plus = header.split('+').nth(1)?;
    let digits: String = plus.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Truncate `text` to `budget` tokens at a line boundary, returning the kept text
/// and the number of tokens dropped.
///
/// The head is kept rather than the tail: a diff's first hunks are the ones a
/// reviewer can still anchor, and a review of the first half of a file is a
/// partial review, while a review of the second half with no idea what preceded it
/// is a confused one. The marker is left in the text so the *model* also knows it
/// is seeing part of a file.
fn truncate_to_tokens(text: &str, budget: usize) -> (String, usize) {
    /// Charged against the budget before cutting, so adding it cannot push the
    /// result back over the budget it was just cut to.
    const MARKER: &str =
        "\n[... truncated to fit the context budget: this is PART of the file ...]\n";

    if estimate_tokens(text) <= budget {
        return (text.to_owned(), 0);
    }
    let room = budget.saturating_sub(estimate_tokens(MARKER)) * 4;
    let mut kept = 0usize;
    for line in text.split_inclusive('\n') {
        if kept + line.len() > room {
            break;
        }
        kept += line.len();
    }
    let dropped = estimate_tokens(&text[kept..]);
    (format!("{}{MARKER}", &text[..kept]), dropped)
}

/// What [`parse_findings`] made of a model's reply.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Parsed {
    /// Findings in the corpus's coordinate system, ready to score.
    pub findings: Vec<CandidateFinding>,
    /// Lines that looked like an attempted finding but could not be read as one.
    ///
    /// Counted rather than discarded. A model that ignores the output format
    /// scores exactly like a model that found nothing, and those are opposite
    /// facts about a reviewer: the first needs a different prompt, the second a
    /// different model. A run reports this so the two cannot be confused.
    pub unparsed: Vec<String>,
    /// Whether the reply declared the file clean in the required form.
    pub declared_clean: bool,
    /// The generation stopped inside a reasoning block, so the model never
    /// reached its answer.
    ///
    /// **This is the stage's own silent zero, found by walking into it.** A
    /// reasoning GGUF opens `<think>` and deliberates before answering; hit the
    /// token cap first and the reply contains no findings and no `NO FINDINGS` —
    /// indistinguishable, to anything counting findings, from a reviewer that read
    /// the file and passed it. Measured on `qwen3.8-27b`, whose careful
    /// doc-versus-code deliberation was **entirely** inside the block and scored
    /// as silence.
    ///
    /// So it is a reported outcome rather than an absence. A run that cannot tell
    /// "found nothing" from "never answered" is reporting a recall figure it did
    /// not measure.
    pub reasoning_truncated: bool,
}

/// Read a model's reply into findings.
///
/// Lenient about presentation and strict about content: a leading bullet, bold
/// markers or a code fence are stripped, because a model wrapping the format in
/// markdown has still followed it — but a finding with no readable positive line
/// number goes to [`Parsed::unparsed`], since an unanchored finding cannot be
/// scored, shown to a human, or acted on.
#[must_use]
pub fn parse_findings(reviewed_sha: &str, path: &str, reply: &str) -> Parsed {
    let mut out = Parsed {
        // Checked on the text as handed over, which a caller has already run its
        // `</think>` strip across: an *opening* tag still present means the
        // closing one never arrived, so generation stopped mid-deliberation.
        reasoning_truncated: reply.contains("<think>"),
        ..Parsed::default()
    };
    for raw in reply.lines() {
        let line = raw.trim().trim_start_matches(['-', '*', '>', '#', ' ']);
        let line = line.trim_start_matches('`').replace("**", "");
        let line = line.trim();
        if line.eq_ignore_ascii_case(NO_FINDINGS) {
            out.declared_clean = true;
            continue;
        }
        if !line
            .get(..FINDING_PREFIX.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(FINDING_PREFIX))
        {
            continue;
        }
        match parse_one(reviewed_sha, path, line) {
            Some(finding) => out.findings.push(finding),
            None => out.unparsed.push(line.to_owned()),
        }
    }
    out
}

/// Parse one `FINDING | …` line, or `None` if it carries no usable line number.
fn parse_one(reviewed_sha: &str, path: &str, line: &str) -> Option<CandidateFinding> {
    let mut number: Option<u32> = None;
    let mut class = None;
    let mut claims_compile_failure = false;
    let mut description = String::new();

    for field in line.split('|').skip(1) {
        let field = field.trim().trim_end_matches('`').trim();
        let Some((key, value)) = field.split_once('=') else {
            // The first field with no `key=` is the description; later ones are
            // its continuation, because a description may itself contain a pipe.
            if !description.is_empty() {
                description.push_str(" | ");
            }
            description.push_str(field);
            continue;
        };
        let value = value.trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "line" => number = value.parse().ok().filter(|n| *n > 0),
            "class" => class = DefectClass::from_token(&value.to_ascii_lowercase()),
            "compile" => {
                claims_compile_failure = matches!(
                    value.to_ascii_lowercase().as_str(),
                    "yes" | "true" | "y" | "1"
                );
            }
            // An unknown key is kept as prose rather than dropped: it is more
            // likely a description containing an `=` than an invented field, and
            // losing the description would leave a human a bare line number.
            _ => {
                if !description.is_empty() {
                    description.push_str(" | ");
                }
                description.push_str(field);
            }
        }
    }

    Some(CandidateFinding {
        reviewed_sha: reviewed_sha.to_owned(),
        path: path.to_owned(),
        line: number?,
        description: if description.trim().is_empty() {
            "(no description given)".to_owned()
        } else {
            description.trim().to_owned()
        },
        claims_compile_failure,
        defect_class: class,
    })
}

/// Derive the [`ClaimSite`] for a compile claim, from the reviewed file's bytes.
///
/// `parent_source` is the module's parent (`lib.rs`/`mod.rs`) when the caller has
/// it, which is the only place a file's feature gate is written.
///
/// # Conservative on every axis, deliberately
///
/// [`crate::compile_claim`] states the asymmetry this follows: a claim wrongly
/// suppressed is a defect shipped silently — the #291 macOS teardown shape — while
/// a claim wrongly kept costs a human one look at a CI page. So every derivation
/// here errs toward *establishing a requirement*, which makes a site harder to
/// refute, never easier:
///
/// * **Platform.** Any `cfg(target_os = "macos"/"windows")` anywhere in the file
///   marks the whole file as needing that platform, so no job in this
///   ubuntu-only CI can refute a claim about it. Coarse, and coarse in the safe
///   direction: a file with one macOS-gated function keeps its compile claims.
/// * **Features.** A `#[cfg(feature = …)]` on the module's declaration in
///   `parent_source` marks the site as needing [`Features::All`], so only an
///   `--all-features` job covers it. Without `parent_source` nothing is
///   established, which is the one axis where "unknown" reads as unconditional —
///   stated here rather than left for a reader to infer from the field docs.
/// * **Targets.** A path under `tests/`, `benches/` or `examples/` is test code;
///   so is a line *after* a `#[cfg(test)]` attribute in the file. Both need a job
///   that passed `--all-targets`, which `msrv` does not.
#[must_use]
pub fn claim_site(
    reviewed_sha: &str,
    path: &str,
    line: u32,
    source: &str,
    parent_source: Option<&str>,
) -> ClaimSite {
    ClaimSite {
        platform: required_platform(source),
        features: parent_source.and_then(|p| module_feature_gate(path, p)),
        is_test_code: is_test_code(path, line, source),
        toolchain: None,
        ..ClaimSite::unknown(reviewed_sha, path)
    }
}

/// The platform a file's `cfg` gates require, if it names one.
fn required_platform(source: &str) -> Option<TargetOs> {
    // macOS first: it is the platform this repository actually has uncompiled
    // code for, and the one #291 shipped a defect behind.
    for (needle, os) in [
        ("target_os = \"macos\"", TargetOs::MacOs),
        ("target_os = \"windows\"", TargetOs::Windows),
    ] {
        if source.contains(needle) {
            return Some(os);
        }
    }
    None
}

/// Whether `line` is test code: a test-only path, or a line after a
/// `#[cfg(test)]` attribute.
fn is_test_code(path: &str, line: u32, source: &str) -> bool {
    if ["tests/", "benches/", "examples/"]
        .iter()
        .any(|d| path.starts_with(d) || path.contains(&format!("/{d}")))
    {
        return true;
    }
    // The line-relative form, so a `#[cfg(test)] mod tests` at the bottom of a
    // source file does not mark the library code above it as test code — which
    // would disable the filter on nearly every file in this repository.
    source
        .lines()
        .position(|l| l.trim_start().starts_with("#[cfg(test)]"))
        .is_some_and(|idx| line as usize > idx + 1)
}

/// The feature gate on this file's `mod` declaration in its parent, if any.
///
/// Returns [`Features::All`] rather than naming the feature: the coverage model
/// asks which *job* compiled the code, and this repository's jobs are
/// `--all-features`, the default set, or nothing. Which named feature it is does
/// not change the answer.
fn module_feature_gate(path: &str, parent_source: &str) -> Option<Features> {
    let stem = path.rsplit('/').next()?.strip_suffix(".rs")?;
    let lines: Vec<&str> = parent_source.lines().collect();
    let decl = lines.iter().position(|l| {
        let t = l.trim_start().trim_start_matches("pub ").trim_start();
        t.starts_with(&format!("mod {stem};")) || t.starts_with(&format!("mod {stem} "))
    })?;
    // Walk back over the declaration's own attributes only, stopping at the first
    // line that is not one — attributes bind to what follows them, so a `cfg` two
    // items up governs a different item.
    for above in lines[..decl].iter().rev() {
        let t = above.trim();
        if t.is_empty() || t.starts_with("//") {
            continue;
        }
        if !t.starts_with("#[") {
            break;
        }
        if t.contains("cfg(feature") || t.contains("cfg(all(feature") {
            return Some(Features::All);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{
        FileUnderReview, GraphContext, NO_FINDINGS, Prompt, SINGLE_CALL_BUDGET_TOKENS,
        annotate_diff, build_prompt, claim_site, class_gloss, estimate_tokens, parse_findings,
    };
    use crate::compile_claim::{CheckRun, Conclusion, Features, TargetOs, Targets, suppression};
    use crate::review_corpus::{CLASSES, DefectClass};

    const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn file(diff: &str) -> FileUnderReview {
        FileUnderReview {
            reviewed_sha: SHA.to_owned(),
            path: "crates/rto-graph/src/lib.rs".to_owned(),
            diff: diff.to_owned(),
        }
    }

    /// **The line column is the reviewer's whole anchoring story**, so it must be
    /// exact: a finding is credited only within `LINE_WINDOW` of a corpus row, and
    /// a model asked to do hunk arithmetic instead would miss by more than that
    /// and be scored as blind rather than as bad at counting.
    #[test]
    fn the_annotated_diff_numbers_the_new_side_exactly() {
        let diff = "@@ -10,3 +20,4 @@ fn thing()\n unchanged\n-gone\n+added\n+also\n context\n";
        let out = annotate_diff(diff);
        let numbered: Vec<(u32, String)> = out
            .lines()
            .filter_map(|l| {
                let (num, body) = l.split_once('|')?;
                let n: u32 = num
                    .trim()
                    .trim_end_matches(['+', '-'])
                    .trim()
                    .parse()
                    .ok()?;
                Some((n, body.to_owned()))
            })
            .collect();
        assert_eq!(
            numbered,
            vec![
                (20, "unchanged".to_owned()),
                (21, "added".to_owned()),
                (22, "also".to_owned()),
                (23, "context".to_owned()),
            ],
            "new-side numbering starts at the hunk's + start and skips removals"
        );
        // A removed line is shown but carries no citable number.
        assert!(out.contains("      - |gone"), "{out}");
    }

    /// A diff with several hunks restarts numbering at each header rather than
    /// counting straight through — the failure that would put every finding after
    /// the first hunk out of window.
    #[test]
    fn numbering_restarts_at_each_hunk() {
        let diff = "@@ -1,2 +1,2 @@\n a\n b\n@@ -50,2 +90,2 @@\n c\n d\n";
        let out = annotate_diff(diff);
        assert!(out.contains("     1  |a"), "{out}");
        assert!(out.contains("    90  |c"), "{out}");
        assert!(out.contains("    91  |d"), "{out}");
    }

    /// Every class the corpus can score is described to the model. A class with no
    /// gloss is a class the reviewer was never told to look for, which would be
    /// measured as a recall failure rather than as the omission it is.
    #[test]
    fn class_gloss_covers_every_class() {
        for class in CLASSES {
            let gloss = class_gloss(class);
            assert!(!gloss.is_empty(), "{class} has no gloss");
        }
        let prompt = build_prompt(&file("@@ -1 +1 @@\n+x\n"), &GraphContext::none(), 30_000);
        for class in CLASSES {
            assert!(
                prompt.text.contains(class.as_str()),
                "{class} is not named in the prompt"
            );
        }
    }

    /// **PR 1's arm is diff-only, and the prompt must say nothing else.** An empty
    /// [`GraphContext`] renders no context heading at all, so the baseline is not
    /// quietly a reviewer told it has context and given none — which is a
    /// different prompt, and would make the two arms differ by more than the
    /// context.
    #[test]
    fn an_empty_context_renders_no_context_section() {
        let bare = build_prompt(&file("@@ -1 +1 @@\n+x\n"), &GraphContext::none(), 30_000);
        assert!(!bare.text.contains("Context from"), "{}", bare.text);
        assert!(
            !bare.text.contains("[authored]") && !bare.text.contains("[derived]"),
            "no provenance labels without context: {}",
            bare.text
        );

        let with = build_prompt(
            &file("@@ -1 +1 @@\n+x\n"),
            &GraphContext {
                items: vec![super::ContextItem {
                    label: "ADR-0019 §3".to_owned(),
                    provenance: "authored".to_owned(),
                    body: "the user layer alone never suffices".to_owned(),
                }],
            },
            30_000,
        );
        assert!(with.text.contains("Context from"), "{}", with.text);
        assert!(with.text.contains("ADR-0019 §3"), "{}", with.text);
        assert!(
            with.text.contains("[authored]"),
            "provenance travels with the item: {}",
            with.text
        );
    }

    /// The prompt fits its budget, and says so when it could not fit the diff.
    #[test]
    fn a_prompt_respects_its_budget_and_reports_what_it_dropped() {
        let big = format!(
            "@@ -1,1 +1,{0} @@\n{}",
            "+a line of code here\n".repeat(20_000)
        );
        let f = file(&big);
        let Prompt {
            text,
            tokens,
            dropped_tokens,
        } = build_prompt(&f, &GraphContext::none(), 8_000);
        assert!(tokens <= 8_000, "over budget: {tokens}");
        assert!(dropped_tokens > 0, "a 20k-line diff cannot have fit");
        assert!(
            text.contains("truncated to fit"),
            "the model is told it is seeing part of a file"
        );

        // And the common case: nothing dropped, nothing claimed.
        let small = build_prompt(&file("@@ -1 +1 @@\n+x\n"), &GraphContext::none(), 30_000);
        assert_eq!(small.dropped_tokens, 0);
        assert!(!small.text.contains("truncated"));
    }

    /// **The headroom the graph arm depends on, asserted rather than assumed.**
    ///
    /// The largest reviewable source file-diff in the corpus is 14,034 raw tokens
    /// (`rto-graph/src/models.rs`), which [`annotate_diff`] takes to **17,202** —
    /// the numbering column costs a measured **1.21×** across all 190 file-diffs,
    /// because it adds a fixed 9 characters per *line* rather than a fraction of
    /// the bytes. Reconstructed here at that size and at this repository's line
    /// length, so a prompt change that eats the headroom fails here rather than
    /// showing up later as a worse score nobody can attribute.
    #[test]
    fn the_prompt_scaffolding_leaves_the_measured_headroom_intact() {
        // ~44 characters per line, this repository's rough average, so the
        // annotation overhead lands where it was measured rather than at the
        // 4× a two-character line would produce.
        let line = format!("+{}\n", "a".repeat(43));
        let worst = line.repeat(14_034 * 4 / 44);
        let f = file(&format!("@@ -1,1 +1,1 @@\n{worst}"));
        let p = build_prompt(&f, &GraphContext::none(), SINGLE_CALL_BUDGET_TOKENS);
        assert_eq!(
            p.dropped_tokens, 0,
            "the corpus's largest source file must not need truncating"
        );
        let headroom = SINGLE_CALL_BUDGET_TOKENS - p.tokens;
        assert!(
            headroom > 10_000,
            "only {headroom} tokens left for graph context on the worst source \
             file; the arm needs room to be testable at all"
        );

        // And the median file, which is what the headroom claim is really about:
        // 1,758 annotated tokens leaves nearly the whole budget free.
        let median = file(&format!("@@ -1,1 +1,1 @@\n{}", line.repeat(1_476 * 4 / 44)));
        let p = build_prompt(&median, &GraphContext::none(), SINGLE_CALL_BUDGET_TOKENS);
        assert!(
            SINGLE_CALL_BUDGET_TOKENS - p.tokens > 25_000,
            "the median file should leave ~28k free, left {}",
            SINGLE_CALL_BUDGET_TOKENS - p.tokens
        );
    }

    #[test]
    fn a_well_formed_finding_parses() {
        let reply = "FINDING | line=42 | class=contract-drift | compile=no | the doc says X";
        let parsed = parse_findings(SHA, "src/a.rs", reply);
        assert_eq!(parsed.findings.len(), 1);
        let f = &parsed.findings[0];
        assert_eq!(f.line, 42);
        assert_eq!(f.defect_class, Some(DefectClass::ContractDrift));
        assert!(!f.claims_compile_failure);
        assert_eq!(f.description, "the doc says X");
        assert!(parsed.unparsed.is_empty());
    }

    /// A model that wraps the format in markdown has still followed it. Being
    /// strict here would measure formatting compliance and call it recall.
    #[test]
    fn presentation_is_tolerated_but_content_is_not() {
        let reply = "\
- **FINDING** | line=7 | class=vacuous-test | compile=no | asserts nothing
  > FINDING | line=9 | class=ordering-bug | compile=YES | will not build
FINDING | class=prose-clarity | compile=no | no line at all
FINDING | line=0 | class=prose-clarity | compile=no | line zero is not a line
here is some prose the model added";
        let parsed = parse_findings(SHA, "src/a.rs", reply);
        assert_eq!(parsed.findings.len(), 2, "{:?}", parsed.findings);
        assert_eq!(parsed.findings[0].line, 7);
        assert!(
            parsed.findings[1].claims_compile_failure,
            "compile= is case-insensitive"
        );
        // Both unanchored forms are counted, not silently dropped.
        assert_eq!(parsed.unparsed.len(), 2, "{:?}", parsed.unparsed);
        // Prose that is not an attempted finding is not counted as a failure.
        assert!(!parsed.unparsed.iter().any(|u| u.contains("here is some")));
    }

    /// **"Found nothing" and "ignored the format" are opposite facts.** A run that
    /// cannot tell them apart cannot tell a bad model from a bad prompt.
    #[test]
    fn a_clean_declaration_is_distinguishable_from_silence() {
        let clean = parse_findings(SHA, "src/a.rs", NO_FINDINGS);
        assert!(clean.declared_clean);
        assert!(clean.findings.is_empty() && clean.unparsed.is_empty());
        assert!(!clean.reasoning_truncated);

        let waffle = parse_findings(SHA, "src/a.rs", "I reviewed the file and it looks fine.");
        assert!(
            !waffle.declared_clean,
            "prose is not the declaration the format requires"
        );
    }

    /// **A reviewer cut off mid-deliberation must not read as a clean file.**
    /// Measured on `qwen3.8-27b`: a reasoning GGUF opens `<think>`, and if the
    /// token cap lands before `</think>` the reply carries no finding and no
    /// declaration — which counts identically to a careful pass unless something
    /// says otherwise. That is the same silent zero the corpus's `reviewed_sha`
    /// rule exists to prevent, arriving from a third direction.
    #[test]
    fn a_reply_cut_off_inside_a_reasoning_block_is_not_a_clean_file() {
        let cut = parse_findings(
            SHA,
            "src/a.rs",
            "<think>\nLet me check the doc against the code. Line 12 says the cache is\n\
             unbounded, and the insert path",
        );
        assert!(
            cut.reasoning_truncated,
            "an unterminated block is truncation"
        );
        assert!(!cut.declared_clean, "and it is emphatically not clean");
        assert!(cut.findings.is_empty());

        // A block the model actually closed is a normal answer; the caller strips
        // it before parsing, so nothing here should fire.
        let finished = parse_findings(SHA, "src/a.rs", NO_FINDINGS);
        assert!(!finished.reasoning_truncated);
    }

    /// An unknown class is dropped to `None` rather than failing the finding:
    /// recall is about the defect, and `review_score` already reports
    /// misclassification separately.
    #[test]
    fn an_unknown_class_does_not_cost_the_finding() {
        let parsed = parse_findings(
            SHA,
            "src/a.rs",
            "FINDING | line=3 | class=off-by-one | compile=no | oops",
        );
        assert_eq!(parsed.findings.len(), 1);
        assert_eq!(parsed.findings[0].defect_class, None);
    }

    /// A description containing a pipe survives, because the alternative is a
    /// human handed a bare line number.
    #[test]
    fn a_description_may_contain_the_separator() {
        let parsed = parse_findings(
            SHA,
            "src/a.rs",
            "FINDING | line=3 | class=prose-clarity | compile=no | says a | b but means a",
        );
        assert_eq!(parsed.findings[0].description, "says a | b but means a");
    }

    /// **The #291 shape, derived rather than assumed.** A file with macOS-gated
    /// code yields a site no job in this ubuntu-only CI covers, so a compile claim
    /// about it survives a wholly green build.
    #[test]
    fn a_macos_gated_file_yields_an_unrefutable_site() {
        let source = "#[cfg(target_os = \"macos\")]\nfn teardown() {}\n";
        let site = claim_site(SHA, "crates/rto-llama/src/backend.rs", 2, source, None);
        assert_eq!(site.platform, Some(TargetOs::MacOs));
        assert!(!suppression(&site, &ci()).is_refuted());

        // The same file without the gate is ordinary library code, and a green
        // all-features job does refute it — so the filter is not simply off.
        let plain = claim_site(
            SHA,
            "crates/rto-llama/src/backend.rs",
            2,
            "fn t() {}\n",
            None,
        );
        assert_eq!(plain.platform, None);
        assert!(suppression(&plain, &ci()).is_refuted());
    }

    /// A `#[cfg(test)] mod tests` at the foot of a source file must not mark the
    /// library code above it as test code — that would establish a requirement
    /// `msrv` cannot meet on nearly every file here and disable the filter
    /// wholesale.
    #[test]
    fn test_code_is_decided_per_line_not_per_file() {
        let source = "fn real() {}\n#[cfg(test)]\nmod tests {\n    fn t() {}\n}\n";
        let above = claim_site(SHA, "crates/rto-graph/src/lib.rs", 1, source, None);
        assert!(!above.is_test_code);
        let below = claim_site(SHA, "crates/rto-graph/src/lib.rs", 4, source, None);
        assert!(below.is_test_code);

        // An integration-test path is test code at any line.
        let integration = claim_site(SHA, "crates/rto-graph/tests/review_corpus.rs", 1, "", None);
        assert!(integration.is_test_code);
    }

    /// **The `boxlite.rs` row of the corpus, derived.** Its module is
    /// `#[cfg(feature = "exec-boxlite")]` in the parent, so only an
    /// `--all-features` job compiles it — which `compile_claim`'s own licence test
    /// asserts by hand and this derives from bytes.
    #[test]
    fn a_feature_gated_module_needs_an_all_features_job() {
        let parent = "pub mod subprocess;\n#[cfg(feature = \"exec-boxlite\")]\npub mod boxlite;\n";
        let site = claim_site(
            SHA,
            "crates/rto-exec/src/boxlite.rs",
            10,
            "fn run() {}\n",
            Some(parent),
        );
        assert_eq!(site.features, Some(Features::All));

        // Its ungated sibling establishes nothing, so it is not accidentally
        // narrowed to the all-features jobs.
        let sibling = claim_site(
            SHA,
            "crates/rto-exec/src/subprocess.rs",
            10,
            "fn run() {}\n",
            Some(parent),
        );
        assert_eq!(sibling.features, None);
    }

    /// An attribute belonging to a different item must not be read as this
    /// module's gate — the walk stops at the first non-attribute line.
    #[test]
    fn a_gate_on_another_item_is_not_borrowed() {
        let parent = "#[cfg(feature = \"serve\")]\npub mod served;\n\npub mod plain;\n";
        let site = claim_site(SHA, "crates/x/src/plain.rs", 1, "", Some(parent));
        assert_eq!(
            site.features, None,
            "the gate governs `served`, not `plain`"
        );
    }

    /// `len / 4` is the basis every budget number in this stage is quoted on.
    #[test]
    fn token_estimation_is_the_documented_basis() {
        assert_eq!(estimate_tokens(&"a".repeat(400)), 100);
        assert_eq!(estimate_tokens(""), 0);
    }

    /// This repository's compiling jobs, as `compile_claim`'s own tests model
    /// them: all ubuntu, only `checks`/`default-features` with `--all-targets`.
    fn ci() -> Vec<CheckRun> {
        vec![
            CheckRun {
                job: "msrv".to_owned(),
                sha: SHA.to_owned(),
                conclusion: Conclusion::Success,
                toolchain: "1.94".to_owned(),
                platform: TargetOs::Linux,
                features: Features::All,
                targets: Targets::LibsAndBins,
            },
            CheckRun {
                job: "checks".to_owned(),
                sha: SHA.to_owned(),
                conclusion: Conclusion::Success,
                toolchain: "stable".to_owned(),
                platform: TargetOs::Linux,
                features: Features::All,
                targets: Targets::AllTargets,
            },
        ]
    }
}

//! Scoring a candidate reviewer against the adjudicated corpus (Stage 35).
//!
//! A review tool is otherwise unmeasurable: its output is prose, its mistakes are
//! plausible, and nobody remembers last week's false positives well enough to
//! count them. [`crate::review_corpus`] fixes a set of comments with known
//! verdicts; this module turns a candidate's findings into numbers over that set,
//! so a different model, a changed prompt or a graph-grounded arm are comparable
//! across attempts rather than argued about.
//!
//! # Per class, never averaged
//!
//! [`Score::per_class`] is the headline, and an aggregate deliberately is not.
//! An average hides the only thing an implementer needs — *which* kinds of defect
//! a reviewer can see, so they know which to target and which to leave to
//! something else. The corpus's largest class, `contract-drift`, is also the one a
//! diff-only reviewer is least equipped for, since the doc making a claim and the
//! code breaking it need not be adjacent; that is a fact about classes, invisible
//! in a mean.
//!
//! Read [`Score::per_class`] with [`ClassRecall::real`] in view: most classes hold
//! a single row, so their recall is one bit, not a rate. [`Score::caveats`] says so
//! in the report rather than leaving a reader to infer it.
//!
//! # Recall is computable here. Precision is not — and the difference matters
//!
//! **Recall is well defined.** For each row the corpus marks `real`, either the
//! candidate found that defect or it did not.
//!
//! **Precision is not**, and assuming otherwise is the most inviting error in this
//! module. The corpus is a complete record of *what one reviewer said* about those
//! trees — **not** a complete inventory of the defects in them. So a candidate
//! finding that matches no row is **unadjudicated**, not false: it may be a
//! genuine defect that the original reviewer never mentioned. Counting it as a
//! false positive would understate a better reviewer precisely for being better.
//!
//! What this module therefore reports is three separate numbers, named so they
//! cannot be blurred together:
//!
//! - [`ClassRecall`] — per class, over the real rows.
//! - [`Score::known_false_reproduced`] — of the rows known to be false, how many
//!   the candidate repeated. This is the *measured* precision signal, and the only
//!   one the corpus licenses.
//! - [`Score::unadjudicated`] — findings the corpus cannot judge. Not precision;
//!   it is the human-cost proxy, because each one costs somebody a real
//!   investigation, and it becomes precision only once a human adjudicates it and
//!   the rows are added to the corpus.
//!
//! [`Score::corpus_precision`] is offered over the adjudicated findings alone and
//! returns `None` when there are none, rather than a flattering `1.0`.
//!
//! # Scoring at the wrong commit reports zero, quietly
//!
//! Every row carries the commit the comment was made against. The merged PR head
//! contains the *fix* commits, so a candidate run against it is asked to find
//! defects that are no longer there and will appear to have missed all of them —
//! a silent zero, not an error. [`CandidateRun`] therefore records which shas were
//! *attempted*, [`score`] refuses a run naming a sha the corpus does not know
//! (overwhelmingly a PR head), and rows outside the attempted set are excluded
//! from the denominator rather than counted as misses.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::review_corpus::{CLASSES, Corpus, CorpusRow, DefectClass, Verdict};

/// How far from a row's line a candidate finding may be anchored and still count
/// as the same defect.
///
/// Not zero, because a reviewer's cited line can be wrong while its point is
/// right — `docs/REVIEW_CHECKLIST.md` has a rule for exactly that case — so an
/// exact-line match would score a correct finding as a miss. Not wide, because a
/// window long enough to span unrelated code turns "commented on the file" into
/// "found the defect". Ten lines keeps the three `vacuous-test` rows of #299
/// distinct (they sit 50 lines apart), which
/// `the_window_bounds_which_row_a_distant_finding_can_claim` holds it to.
pub const LINE_WINDOW: u32 = 10;

/// A finding a candidate reviewer offered, in the corpus's coordinate system.
///
/// Not [`crate::findings::Finding`]: that models a persisted *analyzer* result
/// owned by an `AnalysisRun` with a runner, an isolation mode and an advisory-db
/// digest (ADR-0012). A candidate finding is an ephemeral opinion about one line
/// of one commit, and scoring it must stay a pure function.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateFinding {
    /// The commit the candidate was looking at — a corpus `reviewed_sha`.
    pub reviewed_sha: String,
    /// Repository-relative path the finding is anchored to.
    pub path: String,
    /// Line in that file.
    pub line: u32,
    /// What the candidate said, for a human reading the score's misses and
    /// unadjudicated findings.
    pub description: String,
    /// Whether the finding asserts the code will not build. Declared by the
    /// candidate rather than guessed from the prose here, so that the suppression
    /// rule in [`crate::compile_claim`] and the score agree about which findings
    /// are compile claims.
    #[serde(default)]
    pub claims_compile_failure: bool,
    /// The class the candidate assigned, if it assigned one. Recall does **not**
    /// require agreement: a reviewer that finds the defect and mislabels it has
    /// still found it, and [`ClassRecall::misclassified`] records the disagreement
    /// separately.
    #[serde(default)]
    pub defect_class: Option<DefectClass>,
}

/// Where a whole-change verdict comes down: nothing to push back on, or something
/// to.
///
/// Two values and no third, and deliberately not `#[non_exhaustive]`.
///
/// A scale would invite a threshold, and a threshold on a generation is the
/// probabilistic gate this issue exists to keep out of `review`. The only
/// distinction the corpus can adjudicate is whether a verdict **claimed the
/// change was clean** — see [`Score::verdicts_contradicted`] — and that is a bit.
///
/// The set is closed for a second reason that outlives the first: this is a
/// **wire type**. It is deserialised from a `roteiro.review-run/v1` document, so
/// a third variant is a breaking change for every reader of that schema whatever
/// this attribute says. Marking it open would let a match arm compile while the
/// document it came from could not be read — a compile-time promise the format
/// cannot keep. A new stance belongs at a `/v2` bump.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VerdictStance {
    /// The reviewer found nothing it would push back on.
    Clean,
    /// The reviewer has something it would push back on.
    Concerns,
}

impl VerdictStance {
    /// The stable token used in the run document and in the model's own output
    /// contract, so the prompt, the parser and the JSON cannot disagree.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Concerns => "concerns",
        }
    }

    /// Read a stance token, or `None` if it is not one.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "clean" => Some(Self::Clean),
            "concerns" => Some(Self::Concerns),
            _ => None,
        }
    }
}

/// **A model's judgement over a whole change**, as opposed to its per-file
/// findings (issue #649, part 2).
///
/// # It is an opinion, and it is carried here so it can be measured
///
/// The verdict is never wired to an exit status —
/// [`crate::review_score`] is a scorer, and `ReviewReport::has_drift` remains the
/// only thing `review` gates on. It lives in the run document for the reason the
/// findings do: a summary nobody has scored is an opinion with a confident tone,
/// and shipping one in the single shape `--score` cannot read would have made it
/// permanently unmeasurable.
///
/// # It is deliberately not a `CandidateFinding`
///
/// A finding is anchored to a line and is scored against a corpus row. A verdict
/// is anchored to nothing and answers a different question. Modelling it as a
/// finding with `line: 0` would have put it into the recall denominator, where it
/// would be counted as a defect the reviewer claimed to detect and missed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateVerdict {
    /// The commit the verdict is about — a corpus `reviewed_sha`.
    pub reviewed_sha: String,
    /// Whether it claims the change is clean.
    pub stance: VerdictStance,
    /// What it said, for a human reading the score. Kept verbatim: the prose is
    /// the whole of what a verdict adds over a finding count.
    pub summary: String,
}

/// One candidate reviewer's whole run.
///
/// `attempted_shas` is not derivable from the findings, and the difference is the
/// point: a commit that was reviewed and yielded nothing is a miss, while a commit
/// that was never reviewed is outside the measurement. Conflating them turns a
/// partial run into a bad score.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateRun {
    /// Stable schema tag, so a document says what it is and a mistyped one fails
    /// with that as the message rather than as a missing field.
    #[serde(default = "run_schema")]
    pub schema: String,
    /// Which commits the candidate was actually run against.
    pub attempted_shas: BTreeSet<String>,
    /// Every finding it offered.
    pub findings: Vec<CandidateFinding>,
    /// Findings the candidate withheld under [`crate::compile_claim`], reported so
    /// that a suppression that discarded a *true* finding shows up as a miss with
    /// an explanation instead of an unexplained one.
    #[serde(default)]
    pub suppressed: Vec<CandidateFinding>,
    /// **The whole-change judgement**, at most one per attempted commit (issue
    /// #649, part 2).
    ///
    /// Carried here rather than left to the text output because the alternative
    /// was shipping the verdict in the one shape `--score` cannot read, which
    /// would have made it the only part of the reviewer nobody could ever
    /// measure. [`Score::verdicts_contradicted`] is what the corpus can say about
    /// it.
    ///
    /// Optional on exactly the terms [`CandidateRun::arm`] is: every `v1`
    /// document written before this field existed still parses, and a build older
    /// than this one refuses a document carrying it rather than silently dropping
    /// the judgement.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verdicts: Vec<CandidateVerdict>,
    /// **Which arm produced this run**, when the producer knows — the context it
    /// was given and the model that generated it.
    ///
    /// Stage 35b PR 2 is a comparison of two runs that must differ in exactly one
    /// variable, and a reader has to be able to check that claim from the
    /// artifacts rather than from their filenames. A run document that cannot say
    /// which arm it is makes the comparison unauditable, and mixing two up would
    /// produce a clean, meaningless number of exactly the kind this stage is
    /// arranged against.
    ///
    /// Optional, so every `v1` document written before this field existed still
    /// parses unchanged. The reverse does not hold: `deny_unknown_fields` means a
    /// build older than this one rejects a document carrying it. That is the
    /// deliberate trade — a run whose provenance an old binary silently dropped
    /// would be worse than one it refuses to read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arm: Option<RunArm>,
}

/// What produced a [`CandidateRun`]: the context arm and the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunArm {
    /// The context the reviewer was given — `diff-only` or `graph`.
    pub context: String,
    /// The model that generated it, so a comparison across arms can be shown to
    /// have held it fixed.
    pub model: String,
}

/// Schema tag for a [`CandidateRun`] document.
pub const RUN_SCHEMA: &str = "roteiro.review-run/v1";

/// `serde` default for [`CandidateRun::schema`].
fn run_schema() -> String {
    RUN_SCHEMA.to_owned()
}

/// Written out rather than derived so that a default-constructed run carries the
/// real schema tag: `#[derive(Default)]` would give it the empty string, and a
/// value that cannot round-trip through [`CandidateRun::parse`] is a trap.
impl Default for CandidateRun {
    fn default() -> Self {
        Self {
            schema: run_schema(),
            attempted_shas: BTreeSet::new(),
            findings: Vec::new(),
            suppressed: Vec::new(),
            verdicts: Vec::new(),
            arm: None,
        }
    }
}

impl CandidateRun {
    /// Parse a run document, checking its schema tag.
    ///
    /// # Errors
    /// [`ScoreError::Unreadable`] when the JSON does not match, and
    /// [`ScoreError::WrongSchema`] when it declares a different contract.
    pub fn parse(text: &str) -> Result<Self, ScoreError> {
        let run: Self = serde_json::from_str(text).map_err(|e| ScoreError::Unreadable {
            message: e.to_string(),
        })?;
        if run.schema != RUN_SCHEMA {
            return Err(ScoreError::WrongSchema { got: run.schema });
        }
        Ok(run)
    }
}

/// Why a run could not be scored.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScoreError {
    /// A finding, or an attempted sha, names a commit the corpus does not know.
    ///
    /// Almost always the merged PR head rather than the comment's
    /// `original_commit_id` — the mistake that measures recall on already-fixed
    /// code and reports zero without complaining. Refused rather than ignored.
    #[error(
        "{what} names commit {sha}, which is in no corpus row. The corpus is keyed \
         by each comment's `reviewed_sha` (its `original_commit_id`); a merged PR \
         head contains the fix commits, so scoring against one measures recall on \
         code that is already repaired and silently reports zero"
    )]
    UnknownSha {
        /// Which input named it (`a finding`, `attempted_shas`).
        what: &'static str,
        /// The offending commit.
        sha: String,
    },
    /// A finding names a commit that the run did not declare as attempted.
    #[error(
        "a finding names commit {sha}, which is not in `attempted_shas` — the \
         attempted set decides the denominator, so it must list every commit the \
         candidate reviewed"
    )]
    UndeclaredSha {
        /// The offending commit.
        sha: String,
    },
    /// Two whole-change verdicts name the same commit.
    ///
    /// A verdict is a judgement of one change, so a second one for the same
    /// commit is the candidate contradicting itself — and counting both would
    /// double whatever [`Score::verdicts_contradicted`] says about that commit,
    /// which is a number a reader acts on. Refused rather than de-duplicated,
    /// because picking one of two contradictory judgements is a decision this
    /// scorer has no basis for.
    #[error(
        "two verdicts name commit {sha}. A verdict is a judgement of one change, \
         so a run may carry at most one per commit; two is the candidate \
         contradicting itself, and there is no basis here for choosing between them"
    )]
    DuplicateVerdict {
        /// The commit judged twice.
        sha: String,
    },
    /// The run attempted no commit the corpus knows, so there is nothing to score.
    #[error("the run attempted no commit, so there is nothing to score")]
    NothingAttempted,
    /// The run document is not a [`CandidateRun`].
    #[error("not a `{RUN_SCHEMA}` document: {message}")]
    Unreadable {
        /// The `serde_json` message, which names the offending field.
        message: String,
    },
    /// The document declares a schema this build does not implement.
    #[error("run document declares schema {got:?}, but this build scores `{RUN_SCHEMA}`")]
    WrongSchema {
        /// The tag as declared.
        got: String,
    },
}

/// A real defect the candidate did not find, with enough to go and look at it.
///
/// The comment id alone would not do that: a score document is read by a person
/// deciding what to improve, and "you missed 3789173576" sends them to grep the
/// corpus. Carrying the anchor duplicates three fields out of the corpus, which is
/// the right trade — a report that needs a second file opened to be actionable is
/// a worse contract than a slightly larger one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Missed {
    /// Comment id — the corpus primary key.
    pub id: u64,
    /// Path the missed defect is anchored to.
    pub path: String,
    /// Line in that file.
    pub line: u32,
    /// One line stating what the defect was.
    pub description: String,
    /// Permalink to the original comment.
    pub comment_url: String,
}

/// Recall over one defect class, and the class-level detail an implementer needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClassRecall {
    /// The class.
    pub class: DefectClass,
    /// Real rows of this class **within the attempted commits** — the denominator.
    /// Read every rate against it: a `1` here is one bit of evidence, not a rate.
    pub real: usize,
    /// How many of those the candidate found.
    pub found: usize,
    /// Of the found ones, how many the candidate labelled as a different class.
    /// Finding it still counts; the label is reported so a reviewer whose classes
    /// are systematically wrong is visible.
    pub misclassified: usize,
    /// The real defects of this class the candidate missed — what to read next.
    pub missed: Vec<Missed>,
}

impl ClassRecall {
    /// Recall as a fraction, or `None` when the class has no real row within the
    /// attempted commits (a class with an empty denominator has no recall, and
    /// reporting `0.0` for it would read as a failure).
    #[must_use]
    #[expect(
        clippy::cast_precision_loss,
        reason = "counts here are corpus rows — 26 today, and a corpus large \
                  enough to lose f64 precision would have other problems"
    )]
    pub fn recall(&self) -> Option<f64> {
        (self.real > 0).then(|| self.found as f64 / self.real as f64)
    }
}

/// A candidate's score against the corpus.
// `Eq` is not derived: `expected_by_position` is an `f64` estimate, and a score
// that could be compared for exact equality would invite a test that pins a
// float. `PartialEq` is enough for the equality the tests actually want.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Score {
    /// Stable schema tag for `--json` consumers.
    pub schema: &'static str,
    /// How many corpus commits the run attempted.
    pub attempted_shas: usize,
    /// How many commits the corpus holds in total — so a partial run reads as
    /// partial rather than as a poor one.
    pub corpus_shas: usize,
    /// Per class, in [`CLASSES`] order. Classes with no real row in the attempted
    /// commits are present with `real: 0`, so the shape of the report does not
    /// change with the run.
    pub per_class: Vec<ClassRecall>,
    /// Real rows found, across every class. A total, **not** a rate: the per-class
    /// table is the result.
    pub found: usize,
    /// Real rows in scope.
    pub real_in_scope: usize,
    /// Of the corpus's known-false rows in scope, how many the candidate repeated.
    /// The only measured precision signal the corpus licenses.
    pub known_false_reproduced: usize,
    /// Known-false rows in scope.
    pub known_false_in_scope: usize,
    /// Findings matching no row. **Not false positives** — unadjudicated. See the
    /// module docs.
    pub unadjudicated: usize,
    /// Findings the candidate withheld that would have matched a **real** row —
    /// the cost of the suppression filter, which must be reported rather than
    /// hidden, since a filter that discards true findings is worse than none.
    pub suppressed_real: usize,
    /// Findings the candidate withheld that would have matched a **known-false**
    /// row — the filter earning its keep.
    pub suppressed_known_false: usize,
    /// Findings withheld that match no row at all.
    pub suppressed_unadjudicated: usize,
    /// **How many real rows a candidate of this shape would match by position
    /// alone** — the chance baseline [`Score::found`] has to beat.
    ///
    /// [`match_findings`] credits a finding to a row on `(sha, path, line within
    /// LINE_WINDOW)` and **nothing else**: not the defect class, not a word of the
    /// description. That is the right rule for a scorer that must not reward
    /// eloquence, but it has a consequence nobody had measured. A reviewer that
    /// emits enough findings per file blankets the diff, and then a "hit" is
    /// explained by density rather than by insight — the recall figure looks like
    /// a measurement and is arithmetic.
    ///
    /// Measured on this repository's own reviewer, that is not a hypothetical: at
    /// **10.9 findings per file** the diff-only arm scored **4 of 22**, against a
    /// permutation null — every row relocated to a random line its diff actually
    /// shows, the findings left exactly as emitted — of **4.19**. It scored
    /// *below* chance, and P(≥ observed) was 0.72.
    ///
    /// So this is reported beside the recall, always, in the same way
    /// `reasoning_truncated` is reported beside a zero: a number whose null is not
    /// stated is not yet a result.
    ///
    /// # An approximation, and it says so rather than implying precision
    ///
    /// **The exact null needs the diff** — which lines the reviewer was shown —
    /// and scoring is pure by design, with no git and no network, so that any
    /// machine can recompute a published score. So this uses the candidate's
    /// **own findings** as the proxy for where it looked: on each file carrying a
    /// row, the fraction of the line range those findings span that their merged
    /// `LINE_WINDOW` neighbourhoods cover.
    ///
    /// That proxy is wrong in both directions at once. The findings' own span is
    /// narrower than the diff's, which pushes the estimate up; ignoring the
    /// one-to-one competition between two rows on a file also pushes it up; and
    /// the reviewer's silence at the edges of a diff pushes it down. Calibrated
    /// against an exact permutation on this repository's diff-only run — every row
    /// relocated to a random line its diff actually shows — this reads **3.0**
    /// where the permutation reads **4.19** against an observed **4**.
    ///
    /// So it is an order-of-magnitude guide, not a null, and
    /// [`Score::caveats`] fires on a **margin** rather than a strict comparison
    /// for exactly that reason. Anyone comparing two candidates seriously should
    /// run the permutation, which needs the diff and therefore does not belong in
    /// a pure scorer.
    ///
    /// `None` when the run offered no findings on any file carrying a row, since
    /// there is then nothing whose density to describe.
    pub expected_by_position: Option<f64>,
    /// Whole-change verdicts the run offered on commits in scope (issue #649).
    pub verdicts: usize,
    /// **Verdicts that declared a change clean which the corpus knows carries a
    /// real defect** — the one thing the corpus can adjudicate about a
    /// whole-change judgement, and the failure that matters.
    ///
    /// A confident *"nothing to push back on"* over a change with an adjudicated
    /// defect in it is worse than a missed finding: a missed finding is silence,
    /// while this is a positive claim a reader may act on. It is computed purely,
    /// from data already in the corpus, so it recomputes on any machine like every
    /// other number here.
    ///
    /// A `concerns` verdict is **not** scored against anything. The corpus records
    /// what one reviewer said about these trees, not every defect in them, so a
    /// `concerns` verdict on a commit with no adjudicated row is unadjudicated in
    /// exactly the sense [`Score::unadjudicated`] is — see
    /// [`Score::verdicts_unadjudicated`].
    pub verdicts_contradicted: usize,
    /// Verdicts the corpus cannot judge: every `concerns` verdict, and every
    /// `clean` verdict on a commit the corpus holds no **real** row for.
    ///
    /// Named rather than folded into a rate, for the reason the module's
    /// precision discussion gives: the corpus is not an inventory of the defects
    /// in these trees, so "clean, and the corpus knows of nothing" is not evidence
    /// that the change was clean.
    pub verdicts_unadjudicated: usize,
}

/// Schema tag for a serialised [`Score`].
pub const SCORE_SCHEMA: &str = "roteiro.review-score/v1";

impl Score {
    /// Precision over the **adjudicated** findings only: matched-real ÷
    /// (matched-real + reproduced-known-false).
    ///
    /// `None` when the candidate produced no adjudicated finding, rather than a
    /// flattering `1.0` computed from nothing. This is not precision over the
    /// candidate's output — see [`Score::unadjudicated`], which is the rest of it
    /// and is not counted here because the corpus cannot judge it.
    #[must_use]
    #[expect(
        clippy::cast_precision_loss,
        reason = "counts here are corpus rows; see ClassRecall::recall"
    )]
    pub fn corpus_precision(&self) -> Option<f64> {
        let adjudicated = self.found + self.known_false_reproduced;
        (adjudicated > 0).then(|| self.found as f64 / adjudicated as f64)
    }

    /// The caveats that must accompany these numbers, as sentences a report
    /// prints.
    ///
    /// Emitted with the score rather than left to a reader's memory, because every
    /// one of them is a way the numbers can be honestly stated and dishonestly
    /// read. A run that covered three commits out of thirteen says so; a class
    /// whose denominator is one says so.
    #[must_use]
    pub fn caveats(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.attempted_shas < self.corpus_shas {
            out.push(format!(
                "partial run: {} of {} corpus commits attempted, so rows on the \
                 other {} are excluded from every denominator rather than counted \
                 as misses",
                self.attempted_shas,
                self.corpus_shas,
                self.corpus_shas - self.attempted_shas
            ));
        }
        let thin: Vec<&str> = self
            .per_class
            .iter()
            .filter(|c| c.real == 1)
            .map(|c| c.class.as_str())
            .collect();
        if !thin.is_empty() {
            out.push(format!(
                "{} class(es) have a single real row ({}), so their recall is one \
                 bit rather than a rate and should not be compared as a percentage",
                thin.len(),
                thin.join(", ")
            ));
        }
        // A margin rather than a strict comparison, because the baseline is an
        // approximation that misses in both directions — see the field's docs. A
        // result only a little above an uncertain null is exactly the case a
        // reader needs warning about, so the guard is deliberately loose.
        #[expect(
            clippy::cast_precision_loss,
            reason = "a count of corpus rows found; see ClassRecall::recall"
        )]
        let found = self.found as f64;
        if let Some(expected) = self.expected_by_position
            && found <= expected * 2.0
        {
            out.push(format!(
                "RECALL IS NOT CLEARLY ABOVE CHANCE AT THIS FINDING DENSITY: a \
                 candidate emitting these findings in these places would match \
                 ~{expected:.1} real row(s) by position alone, and this one matched \
                 {}. Scoring credits a finding to a row on (commit, path, line \
                 \u{b1}{}) and NEVER on what the finding says, so a reviewer dense \
                 enough to blanket a diff scores recall it did not earn. That \
                 baseline is approximate; confirm with a permutation null before \
                 comparing two candidates, and lower the finding rate first",
                self.found, LINE_WINDOW
            ));
        }
        if self.unadjudicated > 0 {
            out.push(format!(
                "{} finding(s) match no corpus row. These are UNADJUDICATED, not \
                 false positives — the corpus records what one reviewer said about \
                 these trees, not every defect in them. They become a precision \
                 figure only once a human adjudicates them and the rows are added",
                self.unadjudicated
            ));
        }
        if self.suppressed_real > 0 {
            out.push(format!(
                "the suppression filter withheld {} finding(s) that match a REAL \
                 row — it is discarding true findings and its licence (zero cost \
                 on this corpus) no longer holds",
                self.suppressed_real
            ));
        }
        // Loud, and phrased as the positive claim it is. A missed finding is
        // silence; a `clean` verdict over a commit with an adjudicated defect is
        // an assertion a reader may act on, and it is the reason the verdict is
        // carried into the run document at all.
        if self.verdicts_contradicted > 0 {
            out.push(format!(
                "{} WHOLE-CHANGE VERDICT(S) DECLARED A CHANGE CLEAN THAT THE CORPUS \
                 KNOWS CARRIES A REAL DEFECT. A verdict is a model's opinion and \
                 gates nothing, but this one is a positive claim contradicted by \
                 adjudicated evidence, which is worse than a missed finding: a miss \
                 is silence, and this is a reader being told there is nothing to \
                 look at",
                self.verdicts_contradicted
            ));
        }
        if self.verdicts_unadjudicated > 0 {
            out.push(format!(
                "{} whole-change verdict(s) the corpus cannot judge — every \
                 `concerns` verdict, and every `clean` one on a commit with no real \
                 row. The corpus records what one reviewer said about these trees, \
                 not every defect in them, so `clean` here is NOT evidence the \
                 change was clean",
                self.verdicts_unadjudicated
            ));
        }
        out
    }
}

/// Refuse a run whose commits the corpus does not know, or that the run did not
/// declare as attempted.
///
/// Applied to findings, suppressed findings **and** verdicts alike: a judgement
/// against a merged PR head is a judgement of already-repaired code, and one
/// outside the attempted set is a judgement of a commit this run never looked at.
/// Either would be scored as though it meant something.
fn check_shas(known: &BTreeSet<&str>, run: &CandidateRun) -> Result<(), ScoreError> {
    if run.attempted_shas.is_empty() {
        return Err(ScoreError::NothingAttempted);
    }
    for sha in &run.attempted_shas {
        if !known.contains(sha.as_str()) {
            return Err(ScoreError::UnknownSha {
                what: "attempted_shas",
                sha: sha.clone(),
            });
        }
    }
    let claimed = run
        .findings
        .iter()
        .chain(&run.suppressed)
        .map(|f| ("a finding", &f.reviewed_sha))
        .chain(run.verdicts.iter().map(|v| ("a verdict", &v.reviewed_sha)));
    for (what, sha) in claimed {
        if !known.contains(sha.as_str()) {
            return Err(ScoreError::UnknownSha {
                what,
                sha: sha.clone(),
            });
        }
        if !run.attempted_shas.contains(sha) {
            return Err(ScoreError::UndeclaredSha { sha: sha.clone() });
        }
    }
    // At most one verdict per commit — the shape `docs/JSON_SCHEMA.md` states, now
    // enforced rather than assumed. Findings are deliberately *not* held to this:
    // a reviewer may say several things about one commit, and each is scored
    // against its own row. A verdict is one judgement of one change.
    let mut judged: BTreeSet<&str> = BTreeSet::new();
    for verdict in &run.verdicts {
        if !judged.insert(verdict.reviewed_sha.as_str()) {
            return Err(ScoreError::DuplicateVerdict {
                sha: verdict.reviewed_sha.clone(),
            });
        }
    }
    Ok(())
}

/// How many `clean` verdicts the corpus contradicts: those over a commit it holds
/// a **real** row for.
///
/// The only thing this scorer can say about a whole-change judgement without a
/// human, and deliberately the *only* thing it says — a `concerns` verdict is
/// matched against nothing, because the corpus records what one reviewer said
/// about these trees rather than every defect in them. See
/// [`Score::verdicts_contradicted`].
fn contradicted_verdicts(in_scope: &[&CorpusRow], verdicts: &[CandidateVerdict]) -> usize {
    let with_a_real_row: BTreeSet<&str> = in_scope
        .iter()
        .filter(|r| r.verdict == Verdict::Real)
        .map(|r| r.reviewed_sha.as_str())
        .collect();
    verdicts
        .iter()
        .filter(|v| {
            v.stance == VerdictStance::Clean && with_a_real_row.contains(v.reviewed_sha.as_str())
        })
        .count()
}

/// Score `run` against `corpus`.
///
/// Matching is **one to one**: each row is credited to at most one finding and
/// each finding to at most one row, resolved by nearest line and then by lowest
/// comment id, so the result does not depend on the order the candidate emitted
/// its findings in.
///
/// Whole-change verdicts are scored separately and never enter the recall
/// figures — see [`Score::verdicts_contradicted`].
///
/// # Errors
/// [`ScoreError::UnknownSha`] when a sha is in no corpus row (the PR-head
/// mistake), [`ScoreError::UndeclaredSha`] when a finding or verdict is outside
/// the attempted set, and [`ScoreError::NothingAttempted`] for an empty run.
pub fn score(corpus: &Corpus, run: &CandidateRun) -> Result<Score, ScoreError> {
    let known: BTreeSet<&str> = corpus.reviewed_shas();
    check_shas(&known, run)?;

    let in_scope: Vec<&CorpusRow> = corpus
        .rows()
        .iter()
        .filter(|r| run.attempted_shas.contains(&r.reviewed_sha))
        .collect();

    let emitted = match_findings(&in_scope, &run.findings);
    let withheld = match_findings(&in_scope, &run.suppressed);

    let mut per_class: Vec<ClassRecall> = Vec::with_capacity(CLASSES.len());
    for class in CLASSES {
        let rows: Vec<&&CorpusRow> = in_scope
            .iter()
            .filter(|r| r.defect_class == class && r.verdict == Verdict::Real)
            .collect();
        let mut found = 0;
        let mut misclassified = 0;
        let mut missed = Vec::new();
        for row in &rows {
            match emitted.by_row.get(&row.id) {
                Some(finding) => {
                    found += 1;
                    if finding.defect_class.is_some_and(|c| c != class) {
                        misclassified += 1;
                    }
                }
                None => missed.push(Missed {
                    id: row.id,
                    path: row.path.clone(),
                    line: row.line,
                    description: row.description.clone(),
                    comment_url: row.comment_url.clone(),
                }),
            }
        }
        per_class.push(ClassRecall {
            class,
            real: rows.len(),
            found,
            misclassified,
            missed,
        });
    }

    let real_in_scope = in_scope
        .iter()
        .filter(|r| r.verdict == Verdict::Real)
        .count();
    let known_false_in_scope = in_scope
        .iter()
        .filter(|r| r.verdict == Verdict::False)
        .count();
    // Verdict by row id, so counting matches is a lookup rather than a scan — and
    // so a match against an id somehow outside scope is simply not counted rather
    // than a panic. Matching only ever draws from `in_scope`, so the two agree;
    // this shape means a future change that broke that would produce a low count
    // instead of a crash in a scorer.
    let verdicts: BTreeMap<u64, Verdict> = in_scope.iter().map(|r| (r.id, r.verdict)).collect();
    let count_by_verdict = |m: &Matched, want: Verdict| {
        m.by_row
            .keys()
            .filter(|id| verdicts.get(id) == Some(&want))
            .count()
    };

    let verdicts_contradicted = contradicted_verdicts(&in_scope, &run.verdicts);

    Ok(Score {
        schema: SCORE_SCHEMA,
        verdicts: run.verdicts.len(),
        verdicts_contradicted,
        verdicts_unadjudicated: run.verdicts.len() - verdicts_contradicted,
        attempted_shas: run.attempted_shas.len(),
        corpus_shas: known.len(),
        per_class,
        found: count_by_verdict(&emitted, Verdict::Real),
        real_in_scope,
        known_false_reproduced: count_by_verdict(&emitted, Verdict::False),
        known_false_in_scope,
        unadjudicated: run.findings.len() - emitted.by_row.len(),
        suppressed_real: count_by_verdict(&withheld, Verdict::Real),
        suppressed_known_false: count_by_verdict(&withheld, Verdict::False),
        suppressed_unadjudicated: run.suppressed.len() - withheld.by_row.len(),
        expected_by_position: expected_by_position(&in_scope, &run.findings),
    })
}

/// The chance baseline described on [`Score::expected_by_position`].
///
/// For each **real** row, the probability that a row dropped uniformly across the
/// span its file's findings cover would land within [`LINE_WINDOW`] of at least
/// one of them, capped at 1. Summed, that is how many rows a candidate of this
/// shape matches without knowing anything.
#[expect(
    clippy::cast_precision_loss,
    reason = "line numbers and finding counts on one file; a file long enough to               lose f64 precision is not reviewable at all"
)]
fn expected_by_position(rows: &[&CorpusRow], findings: &[CandidateFinding]) -> Option<f64> {
    let mut by_file: BTreeMap<(&str, &str), Vec<u32>> = BTreeMap::new();
    for f in findings {
        by_file
            .entry((f.reviewed_sha.as_str(), f.path.as_str()))
            .or_default()
            .push(f.line);
    }
    let mut total = 0.0;
    let mut any = false;
    for row in rows.iter().filter(|r| r.verdict == Verdict::Real) {
        let Some(lines) = by_file.get(&(row.reviewed_sha.as_str(), row.path.as_str())) else {
            continue;
        };
        any = true;
        let (lo, hi) = (
            lines.iter().copied().min().unwrap_or(0),
            lines.iter().copied().max().unwrap_or(0),
        );
        // The span the candidate's own attention covered. A single finding spans
        // one line, so the window itself is the whole space and the row is certain
        // to match — which is correct: a file the candidate commented on once, at
        // one point, offers a row nowhere else to be.
        let span = f64::from(hi - lo + 1);
        // **Merged, not summed.** At ten findings a file the ±10 windows overlap
        // heavily, and counting each one whole inflates the baseline by about half
        // — measured on this repository, 6.2 against a permutation's 4.19. The
        // union is what a row can actually land in.
        let mut sorted = lines.clone();
        sorted.sort_unstable();
        let mut covered = 0u64;
        let mut open: Option<(u32, u32)> = None;
        for line in sorted {
            let (start, end) = (line.saturating_sub(LINE_WINDOW), line + LINE_WINDOW);
            match open {
                Some((s, e)) if start <= e + 1 => open = Some((s, e.max(end))),
                Some((s, e)) => {
                    covered += u64::from(e - s + 1);
                    open = Some((start, end));
                }
                None => open = Some((start, end)),
            }
        }
        if let Some((s, e)) = open {
            covered += u64::from(e - s + 1);
        }
        let reach = covered as f64;
        total += (reach / span).min(1.0);
    }
    any.then_some(total)
}

/// The outcome of matching: row id → the finding credited to it.
struct Matched<'a> {
    by_row: BTreeMap<u64, &'a CandidateFinding>,
}

/// Credit findings to rows, one to one.
///
/// Candidate pairs are ranked by line distance, then row id, then the finding's
/// own line — a total order over the pairs, so the greedy pass is deterministic
/// and independent of input order.
fn match_findings<'a>(rows: &[&'a CorpusRow], findings: &'a [CandidateFinding]) -> Matched<'a> {
    let mut pairs: Vec<(u32, u64, u32, usize)> = Vec::new();
    for (idx, finding) in findings.iter().enumerate() {
        for row in rows {
            if row.reviewed_sha != finding.reviewed_sha || row.path != finding.path {
                continue;
            }
            let distance = row.line.abs_diff(finding.line);
            if distance <= LINE_WINDOW {
                pairs.push((distance, row.id, finding.line, idx));
            }
        }
    }
    pairs.sort_unstable();

    let mut by_row: BTreeMap<u64, &CandidateFinding> = BTreeMap::new();
    let mut used: BTreeSet<usize> = BTreeSet::new();
    for (_, row_id, _, idx) in pairs {
        if by_row.contains_key(&row_id) || used.contains(&idx) {
            continue;
        }
        by_row.insert(row_id, &findings[idx]);
        used.insert(idx);
    }
    Matched { by_row }
}

#[cfg(test)]
mod tests {
    use super::{
        CandidateFinding, CandidateRun, CandidateVerdict, LINE_WINDOW, RUN_SCHEMA, SCORE_SCHEMA,
        ScoreError, VerdictStance, score,
    };
    use crate::review_corpus::{Corpus, DefectClass, Verdict};

    const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    /// A corpus row as JSONL.
    fn row(id: u64, sha: &str, path: &str, line: u32, verdict: &str, class: &str) -> String {
        format!(
            "{{\"id\": {id}, \"pr\": 300, \"reviewer\": \"github-copilot\", \
             \"reviewed_sha\": {sha:?}, \"path\": {path:?}, \"line\": {line}, \
             \"verdict\": {verdict:?}, \"defect_class\": {class:?}, \
             \"fix_commit\": \"\", \"description\": \"d\", \
             \"comment_url\": \"https://example.invalid/{id}\"}}"
        )
    }

    /// Two commits, five rows: three real of two classes on `SHA_A`, one real and
    /// one known-false on `SHA_B`.
    fn corpus() -> Corpus {
        let text = [
            row(1, SHA_A, "src/a.rs", 100, "real", "contract-drift"),
            row(2, SHA_A, "src/a.rs", 200, "real", "contract-drift"),
            row(3, SHA_A, "src/b.rs", 10, "real", "vacuous-test"),
            row(4, SHA_B, "src/c.rs", 50, "real", "ordering-bug"),
            row(5, SHA_B, "src/c.rs", 300, "false", "false-compile-claim"),
        ]
        .join("\n");
        Corpus::parse(&text).expect("the test corpus parses")
    }

    fn finding(sha: &str, path: &str, line: u32) -> CandidateFinding {
        CandidateFinding {
            reviewed_sha: sha.to_owned(),
            path: path.to_owned(),
            line,
            description: "a finding".to_owned(),
            claims_compile_failure: false,
            defect_class: None,
        }
    }

    /// **A blanketing reviewer must not be credited with recall it did not earn.**
    ///
    /// Matching consults `(sha, path, line ±LINE_WINDOW)` and never the text, so a
    /// candidate that comments densely enough across a file matches rows by
    /// position. Row 1 sits at line 100; these findings never mention it and are
    /// spread every few lines around it, so every one of them is "right" by
    /// arithmetic alone. The chance baseline has to see that.
    #[test]
    fn a_blanketing_candidate_is_flagged_as_not_clearly_above_chance() {
        let dense: Vec<CandidateFinding> = (0..12)
            .map(|i| finding(SHA_A, "src/a.rs", 90 + i * 3))
            .collect();
        let scored = score(&corpus(), &run(&[SHA_A, SHA_B], dense)).expect("scores");
        let expected = scored
            .expected_by_position
            .expect("findings landed on a file carrying a row");
        assert!(
            expected > 0.5,
            "a candidate blanketing a row's file scored a chance baseline of only \
             {expected}"
        );
        assert!(
            scored
                .caveats()
                .iter()
                .any(|c| c.contains("NOT CLEARLY ABOVE CHANCE")),
            "the density caveat did not fire: {:?}",
            scored.caveats()
        );
    }

    /// The mirror image: one precise finding on a file, nowhere near a row, is not
    /// a blanket — and a candidate that then matches nothing must not be told its
    /// zero is a density artefact.
    #[test]
    fn a_sparse_candidate_that_misses_is_not_blamed_on_density() {
        let sparse = vec![finding(SHA_A, "src/a.rs", 100)];
        let scored = score(&corpus(), &run(&[SHA_A, SHA_B], sparse)).expect("scores");
        // One finding spans one line, so a row has nowhere else to be and the
        // baseline is 1.0 for that row — correct, and the documented behaviour.
        assert_eq!(scored.found, 1);
        assert!(scored.expected_by_position.is_some());
    }

    /// A run whose findings never touch a file carrying a row has no density to
    /// describe, and must report `None` rather than a flattering zero that would
    /// read as "comfortably above chance".
    #[test]
    fn a_run_touching_no_anchored_file_has_no_chance_baseline() {
        let elsewhere = vec![finding(SHA_A, "src/nowhere.rs", 10)];
        let scored = score(&corpus(), &run(&[SHA_A, SHA_B], elsewhere)).expect("scores");
        assert_eq!(scored.expected_by_position, None);
        assert!(
            !scored
                .caveats()
                .iter()
                .any(|c| c.contains("NOT CLEARLY ABOVE CHANCE")),
            "a caveat about density fired with no findings to be dense"
        );
    }

    /// **Overlapping windows are merged, not summed.** Ten findings within a few
    /// lines of each other reach barely further than one does; counting each
    /// `±LINE_WINDOW` span whole would inflate the baseline by about half, which
    /// is how the first version of this read 6.2 where a permutation read 4.19.
    #[test]
    fn overlapping_windows_count_once() {
        // Ten findings packed into 10 lines, plus one far away so the span is wide
        // enough for the difference to show. Merged, the reach is [90,119] plus
        // [990,1010] = 51 lines of a 911-line span, so each of the two rows on
        // this file scores ~0.06. Summed it would be 11 x 21 = 231 lines, ~0.25
        // each — four times larger, and the direction that hides a real result.
        let mut clustered: Vec<CandidateFinding> = (0..10)
            .map(|i| finding(SHA_A, "src/a.rs", 100 + i))
            .collect();
        clustered.push(finding(SHA_A, "src/a.rs", 1_000));
        let scored = score(&corpus(), &run(&[SHA_A, SHA_B], clustered)).expect("scores");
        let expected = scored.expected_by_position.expect("some");
        assert!(
            expected < 0.25,
            "overlapping windows were summed rather than merged: {expected}"
        );
    }

    fn run(shas: &[&str], findings: Vec<CandidateFinding>) -> CandidateRun {
        CandidateRun {
            attempted_shas: shas.iter().map(|s| (*s).to_owned()).collect(),
            findings,
            ..CandidateRun::default()
        }
    }

    /// A perfect run on one commit: every real row of that commit found, per class.
    #[test]
    fn a_found_row_counts_in_its_own_class() {
        let scored = score(
            &corpus(),
            &run(
                &[SHA_A],
                vec![
                    finding(SHA_A, "src/a.rs", 100),
                    finding(SHA_A, "src/a.rs", 200),
                    finding(SHA_A, "src/b.rs", 10),
                ],
            ),
        )
        .expect("scores");
        assert_eq!(scored.schema, SCORE_SCHEMA);
        assert_eq!(scored.found, 3);
        assert_eq!(scored.real_in_scope, 3);
        let drift = scored
            .per_class
            .iter()
            .find(|c| c.class == DefectClass::ContractDrift)
            .expect("every class is present");
        assert_eq!((drift.real, drift.found), (2, 2));
        assert_eq!(drift.recall(), Some(1.0));
        // A class with no row in scope has no recall, rather than 0.0 — which
        // would read as a failure to find something that was not there.
        let cleanup = scored
            .per_class
            .iter()
            .find(|c| c.class == DefectClass::CleanupGap)
            .expect("present with real: 0");
        assert_eq!(cleanup.real, 0);
        assert_eq!(cleanup.recall(), None);
    }

    /// **A partial run must not look like a bad one.** Attempting one of two
    /// commits excludes the other's rows from the denominator, and the caveat says
    /// so.
    #[test]
    fn rows_outside_the_attempted_commits_are_out_of_scope_not_missed() {
        let scored = score(&corpus(), &run(&[SHA_A], vec![])).expect("scores");
        assert_eq!(scored.real_in_scope, 3, "only SHA_A's real rows");
        assert_eq!(scored.found, 0);
        assert_eq!(scored.known_false_in_scope, 0, "the false row is on SHA_B");
        assert_eq!((scored.attempted_shas, scored.corpus_shas), (1, 2));
        assert!(
            scored.caveats().iter().any(|c| c.contains("partial run")),
            "{:?}",
            scored.caveats()
        );
    }

    /// **The most expensive available mistake**, refused rather than scored: a run
    /// against a merged PR head names a commit the corpus does not know, and the
    /// error explains why the number would have been zero.
    #[test]
    fn a_sha_the_corpus_does_not_know_is_refused_with_the_reason() {
        let head = "cccccccccccccccccccccccccccccccccccccccc";
        let err = score(&corpus(), &run(&[head], vec![])).expect_err("not a corpus commit");
        let ScoreError::UnknownSha { what, .. } = err else {
            panic!("expected UnknownSha, got {err:?}");
        };
        assert_eq!(what, "attempted_shas");
        let text = err.to_string();
        assert!(text.contains("reviewed_sha"), "{text}");
        assert!(
            text.contains("fix commits") && text.contains("silently reports zero"),
            "says what goes wrong, not just that it did: {text}"
        );

        // And via a finding, which is the other way it arrives.
        let mut r = run(&[SHA_A], vec![finding(head, "src/a.rs", 100)]);
        r.attempted_shas.insert(SHA_A.to_owned());
        let err = score(&corpus(), &r).expect_err("a finding on an unknown commit");
        assert!(
            matches!(
                err,
                ScoreError::UnknownSha {
                    what: "a finding",
                    ..
                }
            ),
            "{err:?}"
        );
    }

    /// A finding on a commit the run did not declare is refused: the attempted set
    /// is the denominator, so it has to be complete.
    #[test]
    fn a_finding_outside_the_attempted_set_is_refused() {
        let err = score(
            &corpus(),
            &run(&[SHA_A], vec![finding(SHA_B, "src/c.rs", 50)]),
        )
        .expect_err("SHA_B was not attempted");
        assert!(matches!(err, ScoreError::UndeclaredSha { .. }), "{err:?}");
    }

    #[test]
    fn an_empty_run_is_refused_rather_than_scored_as_zero() {
        let err = score(&corpus(), &CandidateRun::default()).expect_err("nothing attempted");
        assert!(matches!(err, ScoreError::NothingAttempted), "{err:?}");
    }

    /// A finding matching no row is **unadjudicated**, never a false positive: the
    /// corpus records what one reviewer said, not every defect in the tree.
    #[test]
    fn an_unmatched_finding_is_unadjudicated_not_false() {
        let scored = score(
            &corpus(),
            &run(&[SHA_A], vec![finding(SHA_A, "src/z.rs", 7)]),
        )
        .expect("scores");
        assert_eq!(scored.unadjudicated, 1);
        assert_eq!(scored.known_false_reproduced, 0);
        assert_eq!(scored.found, 0);
        assert_eq!(
            scored.corpus_precision(),
            None,
            "no adjudicated finding means no precision, not 1.0 and not 0.0"
        );
        let caveat = scored.caveats().join(" ");
        assert!(caveat.contains("UNADJUDICATED"), "{caveat}");
        assert!(
            caveat.contains("not every defect in them"),
            "says why it is not precision: {caveat}"
        );
    }

    /// Repeating a known-false claim is the one precision signal the corpus
    /// licenses, and it lands in the precision denominator.
    #[test]
    fn reproducing_a_known_false_row_costs_precision() {
        let scored = score(
            &corpus(),
            &run(
                &[SHA_B],
                vec![
                    finding(SHA_B, "src/c.rs", 50),  // the real row
                    finding(SHA_B, "src/c.rs", 300), // the known-false one
                ],
            ),
        )
        .expect("scores");
        assert_eq!((scored.found, scored.known_false_reproduced), (1, 1));
        assert_eq!(scored.corpus_precision(), Some(0.5));
        assert_eq!(scored.unadjudicated, 0);
    }

    /// Matching tolerates a slightly-off line — a reviewer's cited line can be
    /// wrong while its point is right — but not an arbitrary one.
    #[test]
    fn matching_tolerates_a_near_miss_but_not_a_far_one() {
        let near = score(
            &corpus(),
            &run(
                &[SHA_A],
                vec![finding(SHA_A, "src/a.rs", 100 + LINE_WINDOW)],
            ),
        )
        .expect("scores");
        assert_eq!(near.found, 1, "at the window edge");

        let far = score(
            &corpus(),
            &run(
                &[SHA_A],
                vec![finding(SHA_A, "src/a.rs", 100 + LINE_WINDOW + 1)],
            ),
        )
        .expect("scores");
        assert_eq!(far.found, 0, "one line past the window");
        assert_eq!(far.unadjudicated, 1);
    }

    /// **The window has to bound something.** The three `vacuous-test` rows of
    /// #299 sit 50 lines apart in one file — a reviewer that commented once,
    /// anywhere in that file, must be credited with the row it is near and with
    /// none of the others.
    ///
    /// Written against a finding placed *between* two rows and near neither,
    /// because that is the case an unbounded window gets wrong. A finding placed
    /// exactly on a row would still credit one row without a window at all, since
    /// matching is one-to-one and nearest-first — which is a different property,
    /// tested below.
    #[test]
    fn the_window_bounds_which_row_a_distant_finding_can_claim() {
        let text = [
            row(1, SHA_A, "tests/t.rs", 75, "real", "vacuous-test"),
            row(2, SHA_A, "tests/t.rs", 125, "real", "vacuous-test"),
            row(3, SHA_A, "tests/t.rs", 176, "real", "vacuous-test"),
        ]
        .join("\n");
        let corpus = Corpus::parse(&text).expect("parses");
        // The real gaps: no window this size can bridge them.
        const { assert!(LINE_WINDOW * 2 < 50, "the window would span two #299 rows") };

        // Line 100: 25 from row 1 and 25 from row 2, so outside both windows. A
        // reviewer that commented here found none of the three.
        let scored = score(
            &corpus,
            &run(&[SHA_A], vec![finding(SHA_A, "tests/t.rs", 100)]),
        )
        .expect("scores");
        assert_eq!(
            scored.found, 0,
            "a finding 25 lines from the nearest row has not found it"
        );
        assert_eq!(scored.unadjudicated, 1);
        let vacuous = scored
            .per_class
            .iter()
            .find(|c| c.class == DefectClass::VacuousTest)
            .expect("present");
        let missed: Vec<u64> = vacuous.missed.iter().map(|m| m.id).collect();
        assert_eq!(missed, vec![1, 2, 3], "names what to read next");
        assert!(
            vacuous
                .missed
                .iter()
                .all(|m| !m.comment_url.is_empty() && m.line > 0),
            "a miss carries enough to go and look at it, not just an id"
        );

        // Three findings, one on each row, are credited to all three — the window
        // is a bound, not an obstacle.
        let all = score(
            &corpus,
            &run(
                &[SHA_A],
                vec![
                    finding(SHA_A, "tests/t.rs", 75),
                    finding(SHA_A, "tests/t.rs", 125),
                    finding(SHA_A, "tests/t.rs", 176),
                ],
            ),
        )
        .expect("scores");
        assert_eq!(all.found, 3);
    }

    /// **One finding cannot be credited to two rows**, so a comment sitting between
    /// two nearby defects counts as finding one of them, not both.
    ///
    /// The rows here are 5 apart, inside one window — the only arrangement in which
    /// one-to-one matching is distinguishable from crediting every row in range.
    #[test]
    fn one_finding_cannot_claim_two_rows_in_the_same_window() {
        let text = [
            row(1, SHA_A, "src/a.rs", 100, "real", "contract-drift"),
            row(2, SHA_A, "src/a.rs", 105, "real", "contract-drift"),
        ]
        .join("\n");
        let corpus = Corpus::parse(&text).expect("parses");
        let scored = score(
            &corpus,
            &run(&[SHA_A], vec![finding(SHA_A, "src/a.rs", 102)]),
        )
        .expect("scores");
        assert_eq!(
            scored.found, 1,
            "one comment is one finding, however many rows it is near"
        );
        let drift = scored
            .per_class
            .iter()
            .find(|c| c.class == DefectClass::ContractDrift)
            .expect("present");
        assert_eq!((drift.real, drift.found), (2, 1));
        // Nearest wins: 102 is 2 from row 1 and 3 from row 2.
        assert_eq!(
            drift.missed.iter().map(|m| m.id).collect::<Vec<_>>(),
            vec![2]
        );
    }

    /// And the converse: two rows cannot share one finding, so spraying findings at
    /// a line cannot inflate recall past the number of rows there.
    #[test]
    fn extra_findings_in_one_window_do_not_inflate_recall() {
        let scored = score(
            &corpus(),
            &run(
                &[SHA_A],
                vec![
                    finding(SHA_A, "src/a.rs", 98),
                    finding(SHA_A, "src/a.rs", 100),
                    finding(SHA_A, "src/a.rs", 102),
                ],
            ),
        )
        .expect("scores");
        assert_eq!(scored.found, 1, "one row, so one credit");
        assert_eq!(scored.unadjudicated, 2);
    }

    /// The score does not depend on the order findings arrive in — a reviewer that
    /// emits its findings in a different order must score identically.
    #[test]
    fn the_score_is_independent_of_finding_order() {
        let findings = vec![
            finding(SHA_A, "src/a.rs", 98),
            finding(SHA_A, "src/a.rs", 205),
            finding(SHA_A, "src/b.rs", 10),
        ];
        let forward = score(&corpus(), &run(&[SHA_A], findings.clone())).expect("scores");
        let mut reversed = findings;
        reversed.reverse();
        let backward = score(&corpus(), &run(&[SHA_A], reversed)).expect("scores");
        assert_eq!(forward, backward);
        assert_eq!(forward.found, 3);
    }

    /// Finding the defect and mislabelling it still counts as found — recall is
    /// about the defect, not the taxonomy — but the disagreement is reported.
    #[test]
    fn a_misclassified_finding_still_counts_as_found() {
        let mut f = finding(SHA_A, "src/b.rs", 10);
        f.defect_class = Some(DefectClass::ProseClarity); // the row is vacuous-test
        let scored = score(&corpus(), &run(&[SHA_A], vec![f])).expect("scores");
        let vacuous = scored
            .per_class
            .iter()
            .find(|c| c.class == DefectClass::VacuousTest)
            .expect("present");
        assert_eq!(
            (vacuous.real, vacuous.found, vacuous.misclassified),
            (1, 1, 1)
        );
    }

    /// **The suppression filter's own cost, measured.** A withheld finding that
    /// would have matched a known-false row is the filter working; one that would
    /// have matched a real row is the filter breaking, and it must show up as a
    /// caveat rather than as an unexplained miss.
    #[test]
    fn suppressed_findings_are_scored_separately_and_a_true_one_raises_a_caveat() {
        let mut good = CandidateRun {
            attempted_shas: [SHA_B.to_owned()].into_iter().collect(),
            suppressed: vec![finding(SHA_B, "src/c.rs", 300)],
            ..CandidateRun::default()
        };
        let scored = score(&corpus(), &good).expect("scores");
        assert_eq!(scored.suppressed_known_false, 1);
        assert_eq!(scored.suppressed_real, 0);
        assert_eq!(
            scored.known_false_reproduced, 0,
            "withheld, so not reproduced"
        );
        assert!(
            !scored.caveats().iter().any(|c| c.contains("REAL row")),
            "nothing true was withheld: {:?}",
            scored.caveats()
        );

        good.suppressed.push(finding(SHA_B, "src/c.rs", 50));
        let bad = score(&corpus(), &good).expect("scores");
        assert_eq!(bad.suppressed_real, 1);
        let caveat = bad.caveats().join(" ");
        assert!(
            caveat.contains("REAL row") && caveat.contains("licence"),
            "says the filter's licence no longer holds: {caveat}"
        );
    }

    /// A withheld finding is not counted twice: it is not in `unadjudicated`,
    /// which counts emitted findings only.
    #[test]
    fn a_withheld_finding_is_not_an_unadjudicated_emitted_one() {
        let scored = score(
            &corpus(),
            &CandidateRun {
                attempted_shas: [SHA_A.to_owned()].into_iter().collect(),
                suppressed: vec![finding(SHA_A, "src/z.rs", 7)],
                ..CandidateRun::default()
            },
        )
        .expect("scores");
        assert_eq!(scored.unadjudicated, 0);
        assert_eq!(scored.suppressed_unadjudicated, 1);
    }

    /// Every class appears in the report, in a fixed order, whatever the run
    /// covered — so two reports can be read side by side.
    #[test]
    fn the_report_shape_does_not_change_with_the_run() {
        let scored = score(&corpus(), &run(&[SHA_A], vec![])).expect("scores");
        let classes: Vec<_> = scored.per_class.iter().map(|c| c.class).collect();
        assert_eq!(classes, crate::review_corpus::CLASSES.to_vec());
    }

    /// The known-false denominator follows scope too: a run that never saw the
    /// commit carrying the false row cannot be credited for avoiding it.
    #[test]
    fn avoiding_a_false_row_out_of_scope_is_not_a_credit() {
        let scored = score(&corpus(), &run(&[SHA_A], vec![])).expect("scores");
        assert_eq!(scored.known_false_in_scope, 0);
        let with_b = score(&corpus(), &run(&[SHA_A, SHA_B], vec![])).expect("scores");
        assert_eq!(with_b.known_false_in_scope, 1);
        assert_eq!(with_b.known_false_reproduced, 0);
    }

    /// The corpus's own verdict vocabulary is what scoping splits on, so a row
    /// whose verdict changed would move between the two denominators rather than
    /// vanish.
    #[test]
    fn scope_splits_on_verdict_exhaustively() {
        let scored = score(&corpus(), &run(&[SHA_A, SHA_B], vec![])).expect("scores");
        assert_eq!(
            scored.real_in_scope + scored.known_false_in_scope,
            corpus().rows().len(),
            "every in-scope row is in exactly one denominator"
        );
        assert_eq!(
            corpus().with_verdict(Verdict::False).count(),
            scored.known_false_in_scope
        );
    }

    fn verdict(sha: &str, stance: VerdictStance) -> CandidateVerdict {
        CandidateVerdict {
            reviewed_sha: sha.to_owned(),
            stance,
            summary: "a judgement".to_owned(),
        }
    }

    /// **The one thing the corpus can adjudicate about a whole-change judgement**
    /// (issue #649, part 2): a verdict that declared a change clean over a commit
    /// the corpus knows carries a real defect.
    ///
    /// # The fixture has to contain the difference
    ///
    /// A local corpus, not [`corpus`], and the two commits differ in the only
    /// respect the rule reads: `SHA_A` carries a **real** row and `SHA_B` carries
    /// only a **known-false** one. The shared fixture has a real row on both, so a
    /// test built on it counted 1 against an implementation that inverted the
    /// stance — found by injecting exactly that, which is why this fixture is
    /// here rather than the convenient one.
    #[test]
    fn a_clean_verdict_over_a_known_defect_is_contradicted_and_said_so_loudly() {
        let text = [
            row(1, SHA_A, "src/a.rs", 100, "real", "contract-drift"),
            row(2, SHA_B, "src/c.rs", 300, "false", "false-compile-claim"),
        ]
        .join("\n");
        let corpus = Corpus::parse(&text).expect("the test corpus parses");

        let both_clean = CandidateRun {
            verdicts: vec![
                verdict(SHA_A, VerdictStance::Clean),
                verdict(SHA_B, VerdictStance::Clean),
            ],
            ..run(&[SHA_A, SHA_B], vec![])
        };
        let scored = score(&corpus, &both_clean).expect("scores");
        assert_eq!(scored.verdicts, 2);
        assert_eq!(
            scored.verdicts_contradicted, 1,
            "only the one over a commit the corpus holds a REAL row for — a \
             known-false row is not a defect to have missed"
        );
        assert_eq!(scored.verdicts_unadjudicated, 1);
        assert!(
            scored
                .caveats()
                .iter()
                .any(|c| c.contains("DECLARED A CHANGE CLEAN")),
            "the contradiction caveat did not fire: {:?}",
            scored.caveats()
        );

        // The stance is read, not ignored: `concerns` over the very same real row
        // is not contradicted by anything.
        let concerns = CandidateRun {
            verdicts: vec![verdict(SHA_A, VerdictStance::Concerns)],
            ..run(&[SHA_A], vec![])
        };
        let scored = score(&corpus, &concerns).expect("scores");
        assert_eq!(
            scored.verdicts_contradicted, 0,
            "a reviewer that said it had concerns has not claimed the change was \
             clean, whatever the corpus knows"
        );
        assert_eq!(scored.verdicts_unadjudicated, 1);
    }

    /// A verdict never enters the recall figures. Modelling it as a finding would
    /// have put it in the denominator, where it would count as a defect the
    /// reviewer claimed to detect.
    #[test]
    fn verdicts_do_not_move_recall_precision_or_the_unadjudicated_count() {
        let findings = vec![finding(SHA_A, "src/a.rs", 100)];
        let without = score(&corpus(), &run(&[SHA_A, SHA_B], findings.clone())).expect("scores");
        let with = score(
            &corpus(),
            &CandidateRun {
                verdicts: vec![
                    verdict(SHA_A, VerdictStance::Clean),
                    verdict(SHA_B, VerdictStance::Concerns),
                ],
                ..run(&[SHA_A, SHA_B], findings)
            },
        )
        .expect("scores");
        assert_eq!(with.found, without.found);
        assert_eq!(with.real_in_scope, without.real_in_scope);
        assert_eq!(with.unadjudicated, without.unadjudicated);
        assert_eq!(with.per_class, without.per_class);
        assert_eq!(with.corpus_precision(), without.corpus_precision());
    }

    /// A run carrying no verdict scores exactly as it did before the field
    /// existed — the zero is a zero, not a contradiction.
    #[test]
    fn a_run_with_no_verdicts_reports_zeroes_and_no_caveat() {
        let scored = score(&corpus(), &run(&[SHA_A], vec![])).expect("scores");
        assert_eq!(
            (
                scored.verdicts,
                scored.verdicts_contradicted,
                scored.verdicts_unadjudicated
            ),
            (0, 0, 0)
        );
        assert!(!scored.caveats().iter().any(|c| c.contains("verdict")));
    }

    /// Verdicts are held to the same sha rules as findings: a judgement of a
    /// commit the corpus does not know is overwhelmingly a merged PR head, whose
    /// fix commits make any judgement of it a judgement of repaired code.
    #[test]
    fn a_verdict_naming_an_unknown_or_undeclared_commit_is_refused() {
        let unknown = CandidateRun {
            verdicts: vec![verdict(
                "cccccccccccccccccccccccccccccccccccccccc",
                VerdictStance::Clean,
            )],
            ..run(&[SHA_A], vec![])
        };
        assert!(matches!(
            score(&corpus(), &unknown),
            Err(ScoreError::UnknownSha {
                what: "a verdict",
                ..
            })
        ));

        let undeclared = CandidateRun {
            verdicts: vec![verdict(SHA_B, VerdictStance::Clean)],
            ..run(&[SHA_A], vec![])
        };
        assert!(matches!(
            score(&corpus(), &undeclared),
            Err(ScoreError::UndeclaredSha { .. })
        ));
    }

    /// One judgement per change. Two verdicts on one commit is the candidate
    /// contradicting itself, and counting both would double whatever
    /// `verdicts_contradicted` says about that commit — a number a reader acts
    /// on. Findings are deliberately not held to this: a reviewer may say several
    /// things about one commit, each scored against its own row.
    #[test]
    fn two_verdicts_on_one_commit_are_refused_while_two_findings_are_not() {
        let twice = CandidateRun {
            verdicts: vec![
                verdict(SHA_A, VerdictStance::Clean),
                verdict(SHA_A, VerdictStance::Concerns),
            ],
            ..run(&[SHA_A], vec![])
        };
        assert!(matches!(
            score(&corpus(), &twice),
            Err(ScoreError::DuplicateVerdict { .. })
        ));

        let many_findings = run(
            &[SHA_A],
            vec![
                finding(SHA_A, "src/a.rs", 100),
                finding(SHA_A, "src/a.rs", 200),
            ],
        );
        assert!(
            score(&corpus(), &many_findings).is_ok(),
            "several findings on one commit are ordinary and stay ordinary"
        );
    }

    /// A `v1` document written before verdicts existed still parses, and one
    /// carrying them round-trips — the additive promise `arm` was added under.
    #[test]
    fn the_run_document_carries_verdicts_and_still_reads_one_without_them() {
        let old = format!(
            "{{\"schema\": \"{RUN_SCHEMA}\", \"attempted_shas\": [{SHA_A:?}], \
             \"findings\": []}}"
        );
        let parsed = CandidateRun::parse(&old).expect("an older document still parses");
        assert!(parsed.verdicts.is_empty());

        let judged = CandidateRun {
            verdicts: vec![verdict(SHA_A, VerdictStance::Concerns)],
            ..run(&[SHA_A], vec![])
        };
        let text = serde_json::to_string(&judged).expect("serialises");
        assert!(
            text.contains("\"stance\":\"concerns\""),
            "the stance is a stable kebab token: {text}"
        );
        assert_eq!(
            CandidateRun::parse(&text).expect("round-trips"),
            judged,
            "a run document that cannot round-trip cannot be replayed"
        );

        // An empty list is omitted entirely, so a run with no verdicts is byte-wise
        // what it always was and an older build can still read it.
        let bare = serde_json::to_string(&run(&[SHA_A], vec![])).expect("serialises");
        assert!(!bare.contains("verdicts"), "{bare}");
    }
}

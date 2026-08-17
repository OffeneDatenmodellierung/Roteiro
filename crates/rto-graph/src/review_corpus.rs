//! The adjudicated review corpus, as a type (Stage 35).
//!
//! `crates/rto-graph/tests/fixtures/review/review-corpus.jsonl` records what an
//! automated reviewer said about specific commits of this repository and what the
//! maintainer decided about each comment. It existed with **no consumer**, which
//! is how a fixture rots: nothing held its shape except a test that re-parsed it
//! as untyped JSON, and nothing could *score* a reviewer against it.
//!
//! This module is that consumer's foundation — the corpus as a value, so that
//! [`crate::review_score`] can compute recall from it and a future reviewer can be
//! measured rather than guessed at. See the fixture's `README.md` for what each
//! field means and `docs/REVIEW_CHECKLIST.md` for the adjudication rule that
//! decides a [`Verdict`].
//!
//! # The field set is enforced by the type, not by a test
//!
//! [`CorpusRow`] is `deny_unknown_fields` with no optional fields, so a row with
//! an extra, missing or misspelled key fails to deserialise. The schema check that
//! used to compare key sets by hand is thereby structural: a corpus this crate can
//! load is a corpus with exactly the eleven documented fields.
//!
//! # Loading never touches the network
//!
//! The corpus is a **historical record**: the rows describe what a reviewer said
//! about a particular tree at a particular moment, and that must not change
//! because a comment was later edited or a thread resolved. So there is no
//! "refresh from the GitHub API" here, and there must not be one — this crate's
//! `gix` dependency is pinned without transports precisely so that such a call
//! cannot be written (the same reasoning as [`crate::model_choice`]).
//!
//! # Not [`crate::findings`]
//!
//! `findings` models *analyzer* findings (ADR-0012): store-backed, keyed by
//! analyzer identity, owned by an [`crate::AnalysisRun`] that records a runner,
//! an isolation mode and an advisory-database digest. A corpus row is none of
//! that — it is an adjudicated opinion about a commit, held in a file, never in
//! `nodes`/`edges` and never in a table. Reusing `Finding` would drag persistence
//! and analyzer provenance into a scorer whose whole value is being pure and
//! offline.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// Highest pull-request number for which the corpus is the **complete, unfiltered**
/// set of that reviewer's comments.
///
/// Rows up to and including this PR are every comment the reviewer left on those
/// twelve PRs — nothing dropped — so a ratio computed over them means something.
/// Later rows are *selected* comments, added because they extended a class; a
/// ratio over all rows is therefore slightly biased toward the class that was
/// selected for. [`Corpus::complete_subset`] is how a caller restricts to the
/// meaningful part, and the fixture README states the same boundary in prose.
pub const COMPLETE_THROUGH_PR: u32 = 343;

/// The corpus this repository ships, embedded at compile time.
///
/// Embedded rather than read from disk so that `roteiro review --score` works from
/// any directory and from an installed binary: the corpus is a fixed historical
/// record, so there is nothing for a copy to go stale against. It also makes the
/// fixture a *shipped asset* rather than test-only data, which is the point of
/// giving it a consumer at all.
pub const BUILTIN: &str = include_str!("../tests/fixtures/review/review-corpus.jsonl");

/// The embedded corpus, parsed.
///
/// # Errors
/// [`CorpusError`] if the shipped file is malformed — which the crate's own tests
/// make impossible, so a caller may reasonably treat this as infallible.
pub fn builtin() -> Result<Corpus, CorpusError> {
    Corpus::parse(BUILTIN)
}

/// What the maintainer decided about a comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// A genuine defect: accepted, and fixed by a commit.
    Real,
    /// The claim was wrong: refuted in a maintainer reply.
    False,
}

impl Verdict {
    /// Stable token (`real` | `false`), as it appears in the corpus file.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Real => "real",
            Self::False => "false",
        }
    }
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The kind of defect a comment asserted.
///
/// The vocabulary is closed: adding a variant means the corpus README's class
/// table gains a row, and `review_corpus.rs`'s table check holds the two together.
/// Variants are named for what goes wrong, not for the subsystem it happens in,
/// because the useful question a score answers is "which *kinds* of defect can
/// this reviewer see?".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DefectClass {
    /// A guard stops a cleanup path doing its job.
    CleanupGap,
    /// A doc, comment or ADR contradicts the code it describes.
    ContractDrift,
    /// An error message does not state the rule it enforces.
    ErrorTextDrift,
    /// Asserts the code will not build. **Every row in this class is a false
    /// positive** — see [`crate::compile_claim`], which is the suppression rule
    /// that measurement licenses.
    FalseCompileClaim,
    /// A suppression lacks the justification the house style requires.
    LintConvention,
    /// A key derived from a lossy conversion, so distinct inputs collide.
    LossyIdentity,
    /// An early return skips a documented side effect.
    MissingEvent,
    /// An aggregate computed after the mutation it must precede.
    OrderingBug,
    /// The implementation defeats a field's stated design goal.
    PerfContract,
    /// A constraint permits the state it exists to forbid.
    PermissiveConstraint,
    /// Wording only.
    ProseClarity,
    /// A read or copy drops a remainder without erroring.
    SilentTruncation,
    /// A message tells the user to do the wrong thing.
    UxDiagnostic,
    /// A test passes while the behaviour it names is broken.
    VacuousTest,
}

/// Every class, in the order a report prints them (the corpus file's own
/// alphabetical order, so a diff of two reports lines up).
pub const CLASSES: [DefectClass; 14] = [
    DefectClass::CleanupGap,
    DefectClass::ContractDrift,
    DefectClass::ErrorTextDrift,
    DefectClass::FalseCompileClaim,
    DefectClass::LintConvention,
    DefectClass::LossyIdentity,
    DefectClass::MissingEvent,
    DefectClass::OrderingBug,
    DefectClass::PerfContract,
    DefectClass::PermissiveConstraint,
    DefectClass::ProseClarity,
    DefectClass::SilentTruncation,
    DefectClass::UxDiagnostic,
    DefectClass::VacuousTest,
];

impl DefectClass {
    /// Stable token as it appears in the corpus file (`contract-drift`, …).
    ///
    /// Written out rather than derived from the `serde` rename so that a report
    /// does not have to serialise a value to name it;
    /// `as_str_matches_the_serialised_form` holds the two together.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CleanupGap => "cleanup-gap",
            Self::ContractDrift => "contract-drift",
            Self::ErrorTextDrift => "error-text-drift",
            Self::FalseCompileClaim => "false-compile-claim",
            Self::LintConvention => "lint-convention",
            Self::LossyIdentity => "lossy-identity",
            Self::MissingEvent => "missing-event",
            Self::OrderingBug => "ordering-bug",
            Self::PerfContract => "perf-contract",
            Self::PermissiveConstraint => "permissive-constraint",
            Self::ProseClarity => "prose-clarity",
            Self::SilentTruncation => "silent-truncation",
            Self::UxDiagnostic => "ux-diagnostic",
            Self::VacuousTest => "vacuous-test",
        }
    }

    /// The class from its corpus token, or `None` if it names no class.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        CLASSES.into_iter().find(|c| c.as_str() == token)
    }
}

impl std::fmt::Display for DefectClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One adjudicated review comment.
///
/// `deny_unknown_fields` and no `Option`s: the eleven documented fields, exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorpusRow {
    /// GitHub review-comment id — the primary key, unique across the corpus.
    pub id: u64,
    /// Pull-request number.
    pub pr: u32,
    /// Which reviewer produced it.
    pub reviewer: String,
    /// **The commit the comment was made against** — the comment's
    /// `original_commit_id`, the tree the reviewer was looking at.
    ///
    /// Never the merged PR head. The merged head contains the *fix* commits, so a
    /// reviewer scored against it is asked to find defects that are no longer
    /// there and will appear to have missed all of them. The fixture README states
    /// how to reconstruct the diff this names, and the integration test
    /// `every_row_reconstructs_a_non_empty_reviewed_diff` holds that recipe to the
    /// data — the prose form of it had already gone wrong for most of the rows.
    pub reviewed_sha: String,
    /// File the comment is anchored to.
    pub path: String,
    /// Line in that file, new-side.
    pub line: u32,
    /// What the maintainer decided.
    pub verdict: Verdict,
    /// The kind of defect asserted.
    pub defect_class: DefectClass,
    /// Short sha of the commit that fixed it, or empty where no single commit is
    /// attributable (three rows legitimately have none — a blank is honest where a
    /// plausible-looking guess would corrupt every future score).
    pub fix_commit: String,
    /// One line stating the defect, or stating why the claim is wrong.
    pub description: String,
    /// Permalink to the original comment.
    pub comment_url: String,
}

/// Why a corpus file could not be loaded. Every variant names the 1-based line so
/// a maintainer is not left grepping.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CorpusError {
    /// A line was not a JSON object matching [`CorpusRow`] — a bad type, an
    /// unknown key, a missing key, or a token outside a documented vocabulary.
    #[error("line {line}: {message}")]
    Malformed {
        /// 1-based line number in the corpus file.
        line: usize,
        /// The `serde_json` message, which names the offending field.
        message: String,
    },
    /// A field was well-typed but not a usable value.
    #[error("line {line}: {field} {message}")]
    Invalid {
        /// 1-based line number in the corpus file.
        line: usize,
        /// The field at fault.
        field: &'static str,
        /// What is wrong with it.
        message: String,
    },
    /// Two rows carry the same comment id. The id is the primary key, and a
    /// repeat would double-count that comment in every score computed from the
    /// file.
    #[error("line {line}: duplicate comment id {id}, first seen on line {first}")]
    DuplicateId {
        /// 1-based line number of the repeat.
        line: usize,
        /// 1-based line number of the first occurrence.
        first: usize,
        /// The repeated id.
        id: u64,
    },
}

/// The corpus: adjudicated rows, in file order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Corpus {
    rows: Vec<CorpusRow>,
}

impl Corpus {
    /// Parse a corpus from JSONL text. Blank lines are skipped; everything else
    /// must be a valid [`CorpusRow`].
    ///
    /// # Errors
    /// [`CorpusError`] naming the offending line.
    pub fn parse(text: &str) -> Result<Self, CorpusError> {
        let mut rows = Vec::new();
        let mut seen: BTreeMap<u64, usize> = BTreeMap::new();
        for (idx, raw) in text.lines().enumerate() {
            let line = idx + 1;
            if raw.trim().is_empty() {
                continue;
            }
            let row: CorpusRow = serde_json::from_str(raw).map_err(|e| CorpusError::Malformed {
                line,
                message: e.to_string(),
            })?;
            validate(&row, line)?;
            if let Some(&first) = seen.get(&row.id) {
                return Err(CorpusError::DuplicateId {
                    line,
                    first,
                    id: row.id,
                });
            }
            seen.insert(row.id, line);
            rows.push(row);
        }
        Ok(Self { rows })
    }

    /// The rows, in file order.
    #[must_use]
    pub fn rows(&self) -> &[CorpusRow] {
        &self.rows
    }

    /// How many rows the corpus holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether the corpus is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The rows over which a *ratio* is meaningful: the PRs whose comments were
    /// captured completely and unfiltered (`pr <= `[`COMPLETE_THROUGH_PR`]).
    ///
    /// A caller reporting anything of the form "x out of the comments" should use
    /// this, or say plainly that it did not.
    #[must_use]
    pub fn complete_subset(&self) -> Self {
        Self {
            rows: self
                .rows
                .iter()
                .filter(|r| r.pr <= COMPLETE_THROUGH_PR)
                .cloned()
                .collect(),
        }
    }

    /// Rows with the given verdict.
    pub fn with_verdict(&self, verdict: Verdict) -> impl Iterator<Item = &CorpusRow> {
        self.rows.iter().filter(move |r| r.verdict == verdict)
    }

    /// Every distinct `reviewed_sha`, sorted — the trees a full scoring run has to
    /// reconstruct. Fewer than there are rows, since several comments were left on
    /// the same commit.
    #[must_use]
    pub fn reviewed_shas(&self) -> BTreeSet<&str> {
        self.rows.iter().map(|r| r.reviewed_sha.as_str()).collect()
    }

    /// `(real, false)` counts per class, over every class present in the data.
    ///
    /// The single computation behind both the fixture README's class table and a
    /// per-class score, so the documented counts and the scored counts cannot come
    /// from two different readings of the file.
    #[must_use]
    pub fn class_counts(&self) -> BTreeMap<DefectClass, (usize, usize)> {
        let mut counts: BTreeMap<DefectClass, (usize, usize)> = BTreeMap::new();
        for row in &self.rows {
            let entry = counts.entry(row.defect_class).or_insert((0, 0));
            match row.verdict {
                Verdict::Real => entry.0 += 1,
                Verdict::False => entry.1 += 1,
            }
        }
        counts
    }
}

/// Whether `s` is a full 40-character hex object id.
fn is_full_sha(s: &str) -> bool {
    s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Field-level checks `serde` cannot express: positive identifiers, a full-length
/// `reviewed_sha`, and no blank text where a reader needs text.
fn validate(row: &CorpusRow, line: usize) -> Result<(), CorpusError> {
    let invalid = |field: &'static str, message: String| CorpusError::Invalid {
        line,
        field,
        message,
    };
    for (field, value) in [
        ("id", row.id),
        ("pr", u64::from(row.pr)),
        ("line", u64::from(row.line)),
    ] {
        if value == 0 {
            return Err(invalid(field, "must be positive, got 0".to_owned()));
        }
    }
    if !is_full_sha(&row.reviewed_sha) {
        return Err(invalid(
            "reviewed_sha",
            format!(
                "{:?} is not a 40-character hex sha. It must be the comment's \
                 `original_commit_id` — the tree the reviewer saw — never the \
                 merged PR head, which contains the fix commits",
                row.reviewed_sha
            ),
        ));
    }
    // Optional by design, but when present it must look like a sha rather than a
    // note to the reader.
    let looks_like_sha =
        row.fix_commit.len() >= 7 && row.fix_commit.chars().all(|c| c.is_ascii_hexdigit());
    if !row.fix_commit.is_empty() && !looks_like_sha {
        return Err(invalid(
            "fix_commit",
            format!("{:?} is neither empty nor a hex sha", row.fix_commit),
        ));
    }
    for (field, value) in [
        ("reviewer", &row.reviewer),
        ("path", &row.path),
        ("description", &row.description),
        ("comment_url", &row.comment_url),
    ] {
        if value.trim().is_empty() {
            return Err(invalid(field, "must not be blank".to_owned()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        CLASSES, COMPLETE_THROUGH_PR, Corpus, CorpusError, CorpusRow, DefectClass, Verdict,
    };

    /// A row with every field valid, for a test to spoil one field of.
    fn row_json(overrides: &[(&str, &str)]) -> String {
        let mut fields: Vec<(&str, String)> = vec![
            ("id", "3788975371".to_owned()),
            ("pr", "292".to_owned()),
            ("reviewer", "\"github-copilot\"".to_owned()),
            (
                "reviewed_sha",
                "\"97938e013380d66f44ea0cb587b637d06fda1bbb\"".to_owned(),
            ),
            ("path", "\"crates/rto-graph/src/engine_slot.rs\"".to_owned()),
            ("line", "16".to_owned()),
            ("verdict", "\"real\"".to_owned()),
            ("defect_class", "\"contract-drift\"".to_owned()),
            ("fix_commit", "\"41cb5e9\"".to_owned()),
            (
                "description",
                "\"module doc contradicts the lock\"".to_owned(),
            ),
            ("comment_url", "\"https://example.invalid/1\"".to_owned()),
        ];
        for &(key, value) in overrides {
            if let Some(slot) = fields.iter_mut().find(|(k, _)| *k == key) {
                slot.1 = value.to_owned();
            } else {
                fields.push((key, value.to_owned()));
            }
        }
        let body: Vec<String> = fields.iter().map(|(k, v)| format!("{k:?}: {v}")).collect();
        format!("{{{}}}", body.join(", "))
    }

    /// `as_str` is written out by hand for reports; `serde` renames by rule. A
    /// mismatch would make a scored report and the corpus file disagree about a
    /// class name, so the two are held together here rather than trusted to stay
    /// in step.
    #[test]
    fn as_str_matches_the_serialised_form() {
        for class in CLASSES {
            let serialised = serde_json::to_string(&class).expect("a class serialises");
            assert_eq!(
                serialised,
                format!("{:?}", class.as_str()),
                "{class:?}: as_str and the serde rename disagree"
            );
            assert_eq!(DefectClass::from_token(class.as_str()), Some(class));
        }
        for verdict in [Verdict::Real, Verdict::False] {
            let serialised = serde_json::to_string(&verdict).expect("a verdict serialises");
            assert_eq!(serialised, format!("{:?}", verdict.as_str()));
        }
        assert_eq!(DefectClass::from_token("no-such-class"), None);
    }

    /// The class list and the enum cannot drift: every variant is listed exactly
    /// once. `CLASSES` is what a report iterates, so a variant missing from it
    /// would silently vanish from every score.
    #[test]
    fn every_class_is_listed_exactly_once() {
        let tokens: Vec<&str> = CLASSES.iter().map(|c| c.as_str()).collect();
        let mut sorted = tokens.clone();
        sorted.sort_unstable();
        // Order is asserted against the *original* list, not against a copy that has
        // already been sorted — the earlier form compared `sorted` with `sorted` and
        // could not have failed.
        assert_eq!(
            tokens, sorted,
            "CLASSES is not in token order, so two reports would not line up"
        );
        let mut deduped = sorted.clone();
        deduped.dedup();
        assert_eq!(deduped.len(), tokens.len(), "CLASSES repeats a class");
    }

    #[test]
    fn a_well_formed_row_parses() {
        let corpus = Corpus::parse(&row_json(&[])).expect("parses");
        assert_eq!(corpus.len(), 1);
        let row = &corpus.rows()[0];
        assert_eq!(row.verdict, Verdict::Real);
        assert_eq!(row.defect_class, DefectClass::ContractDrift);
        assert_eq!(row.pr, 292);
    }

    /// Blank lines are skipped, not parsed — a trailing newline is not a row.
    #[test]
    fn blank_lines_are_skipped() {
        let text = format!("{}\n\n   \n", row_json(&[]));
        assert_eq!(Corpus::parse(&text).expect("parses").len(), 1);
    }

    /// An unknown key is refused. This is the schema check the untyped test used
    /// to do by comparing key sets: a corpus this crate can load has exactly the
    /// documented fields.
    #[test]
    fn an_unknown_field_is_refused() {
        let err = Corpus::parse(&row_json(&[("severity", "\"high\"")]))
            .expect_err("an extra field is not the documented schema");
        let CorpusError::Malformed { line, ref message } = err else {
            panic!("expected Malformed, got {err:?}");
        };
        assert_eq!(line, 1);
        assert!(message.contains("severity"), "names the field: {message}");
    }

    /// A missing key is refused, and named.
    #[test]
    fn a_missing_field_is_refused() {
        let json = row_json(&[]).replace("\"fix_commit\": \"41cb5e9\", ", "");
        let err = Corpus::parse(&json).expect_err("a missing field is not the schema");
        assert!(
            err.to_string().contains("fix_commit"),
            "names the field: {err}"
        );
    }

    /// A token outside a documented vocabulary is refused rather than silently
    /// bucketed. A new class must be added to the enum *and* the README table.
    #[test]
    fn an_undocumented_class_or_verdict_is_refused() {
        for (field, value) in [
            ("defect_class", "\"off-by-one\""),
            ("verdict", "\"probably\""),
        ] {
            let err =
                Corpus::parse(&row_json(&[(field, value)])).expect_err("not a documented token");
            assert!(
                matches!(err, CorpusError::Malformed { .. }),
                "{field}: {err:?}"
            );
        }
    }

    /// **The most expensive mistake the corpus can absorb.** A truncated or
    /// short-form `reviewed_sha` is refused with a message that says what the
    /// field must be, because a row whose sha is the PR head scores every future
    /// reviewer against a tree that already contains the fix.
    #[test]
    fn a_short_reviewed_sha_is_refused_and_says_why() {
        let err = Corpus::parse(&row_json(&[("reviewed_sha", "\"97938e0\"")]))
            .expect_err("a short sha is not the review commit");
        let text = err.to_string();
        assert!(text.contains("reviewed_sha"), "names the field: {text}");
        assert!(
            text.contains("original_commit_id"),
            "says what the field is: {text}"
        );
        assert!(
            text.contains("fix commits"),
            "says why the head is wrong: {text}"
        );
    }

    #[test]
    fn a_zero_identifier_is_refused() {
        for field in ["id", "pr", "line"] {
            let err =
                Corpus::parse(&row_json(&[(field, "0")])).expect_err("zero is not an identifier");
            let CorpusError::Invalid { field: got, .. } = err else {
                panic!("expected Invalid, got {err:?}");
            };
            assert_eq!(got, field);
        }
    }

    #[test]
    fn a_fix_commit_that_is_not_a_sha_is_refused_but_blank_is_allowed() {
        // Three rows legitimately carry no fix commit.
        let ok = Corpus::parse(&row_json(&[("fix_commit", "\"\"")])).expect("blank is allowed");
        assert!(ok.rows()[0].fix_commit.is_empty());
        let err = Corpus::parse(&row_json(&[("fix_commit", "\"landed in a rework\"")]))
            .expect_err("prose is not a sha");
        assert!(err.to_string().contains("fix_commit"), "{err}");
    }

    #[test]
    fn a_blank_text_field_is_refused() {
        for field in ["reviewer", "path", "description", "comment_url"] {
            let err = Corpus::parse(&row_json(&[(field, "\"   \"")]))
                .expect_err("blank text is not text");
            assert!(err.to_string().contains(field), "{field}: {err}");
        }
    }

    /// The id is the primary key: a repeat would double-count that comment in
    /// every score computed from the file, so it is refused, and both lines are
    /// named.
    #[test]
    fn a_duplicate_id_is_refused_and_names_both_lines() {
        let text = format!("{}\n{}", row_json(&[]), row_json(&[("pr", "293")]));
        let err = Corpus::parse(&text).expect_err("the id repeats");
        let CorpusError::DuplicateId { line, first, id } = err else {
            panic!("expected DuplicateId, got {err:?}");
        };
        assert_eq!((line, first, id), (2, 1, 3_788_975_371));
    }

    /// `complete_subset` keeps only the PRs whose comments were captured
    /// completely, because that is the only subset over which a ratio means
    /// anything. The later selected row must not be in it.
    #[test]
    fn complete_subset_drops_selectively_added_rows() {
        let text = format!(
            "{}\n{}",
            row_json(&[]),
            row_json(&[("id", "9999"), ("pr", "352")])
        );
        let corpus = Corpus::parse(&text).expect("parses");
        assert_eq!(corpus.len(), 2);
        let complete = corpus.complete_subset();
        assert_eq!(complete.len(), 1);
        assert!(complete.rows().iter().all(|r| r.pr <= COMPLETE_THROUGH_PR));
    }

    /// Counting is one computation, used by both the README table check and a
    /// score, so the two cannot disagree about the same file.
    #[test]
    fn class_counts_splits_real_from_false() {
        let text = format!(
            "{}\n{}\n{}",
            row_json(&[]),
            row_json(&[
                ("id", "2"),
                ("defect_class", "\"false-compile-claim\""),
                ("verdict", "\"false\"")
            ]),
            row_json(&[
                ("id", "3"),
                ("defect_class", "\"false-compile-claim\""),
                ("verdict", "\"false\"")
            ]),
        );
        let counts = Corpus::parse(&text).expect("parses").class_counts();
        assert_eq!(counts[&DefectClass::ContractDrift], (1, 0));
        assert_eq!(counts[&DefectClass::FalseCompileClaim], (0, 2));
        assert_eq!(counts.len(), 2, "absent classes are not invented");
    }

    /// Several comments share a commit, so a scoring run reconstructs fewer trees
    /// than there are rows — and it must reconstruct each exactly once.
    #[test]
    fn reviewed_shas_are_deduplicated() {
        let text = format!("{}\n{}", row_json(&[]), row_json(&[("id", "2")]));
        let corpus = Corpus::parse(&text).expect("parses");
        assert_eq!(corpus.len(), 2);
        assert_eq!(corpus.reviewed_shas().len(), 1);
    }

    /// A round trip through `serde` preserves every field, so a tool may re-emit a
    /// row (into a report, a filtered corpus) without losing provenance.
    #[test]
    fn a_row_round_trips() {
        let corpus = Corpus::parse(&row_json(&[])).expect("parses");
        let json = serde_json::to_string(&corpus.rows()[0]).expect("serialises");
        let back: CorpusRow = serde_json::from_str(&json).expect("deserialises");
        assert_eq!(&back, &corpus.rows()[0]);
    }
}

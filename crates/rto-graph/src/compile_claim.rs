//! When a green check refutes "this will not compile" — and when it does not
//! (Stage 35).
//!
//! # The measurement this exists to spend
//!
//! On the adjudicated corpus ([`crate::review_corpus`]) *every* false positive was
//! a claim that the code would not build, and *every* claim that the code would not
//! build was a false positive — with no real defect anywhere in the class. That is
//! the filter's whole licence, so it is **asserted against the data** rather than
//! recorded here as a number that could go stale: see
//! `the_compile_claim_class_is_still_the_only_false_one_and_wholly_false` in
//! `tests/review_corpus.rs`, which fails the build if a real defect ever joins the
//! class.
//!
//! CI already computed the refutation each time: the `msrv` job had gone **green at
//! the very commit the comment was left on**, by 65 seconds on one and 83 on
//! another. So withholding such a claim while the relevant check is green costs no
//! extra compute and, on this evidence, discards nothing true. Every investigation
//! those comments triggered was avoidable by reading a status that already existed.
//!
//! # Why "the build is green" is the wrong rule
//!
//! `docs/REVIEW_CHECKLIST.md` records the trap, and it is not hypothetical: this
//! repository has already shipped a defect that a green build was structurally
//! blind to. The `GGML_ASSERT` engine-teardown abort of #291 was **macOS-only**,
//! and every compiling job here runs on `ubuntu-latest`. A filter keyed on "was
//! the build green" would have suppressed a report of it.
//!
//! So a check refutes a claim only when it **ran at that commit** and **covered
//! the configuration the claim is about**. Three axes decide coverage, each one a
//! way this project's CI is narrower than "the build":
//!
//! - **Platform** — every job is `ubuntu-latest`, so nothing here compiles
//!   `cfg(target_os = "macos")` code, of which this repo has a good deal (Metal,
//!   the engine teardown path, the sandbox backend).
//! - **Features** — `msrv` and `checks` are `--all-features`; `default-features`
//!   is the default set. Neither covers the other: turning features *on* cannot
//!   find a defect in code being cfg'd *out*, which is exactly why the
//!   `default-features` job exists.
//! - **Targets** — `msrv` is `cargo check --workspace --all-features`, with **no
//!   `--all-targets`**. It therefore never compiles `#[cfg(test)]` modules or
//!   `tests/` integration targets. A claim that *test* code will not build on the
//!   MSRV toolchain is refuted by no job in this repository: the jobs that compile
//!   test targets do so on `stable`. That gap falls out of the model here rather
//!   than being asserted, and [`Suppression::Unrefuted`] reports it.
//!
//! Deliberately conservative on every axis: an unknown site is never refuted, and
//! coverage is exact match rather than subsumption, because the cost of the two
//! errors is not symmetric. A claim wrongly suppressed is a defect shipped
//! silently — the #291 shape. A claim wrongly kept costs a human one look at a CI
//! page.
//!
//! # Deciding, not fetching
//!
//! Everything here is a pure function of evidence a caller supplies. This crate
//! cannot reach the network (its `gix` is pinned without transports), and a
//! suppression rule is precisely the code that would otherwise acquire a "just ask
//! the API" call. Whoever holds a GitHub token turns check runs into
//! [`CheckRun`]s; the policy lives here where it can be tested exhaustively and
//! offline.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// The platform a compilation covers, or that a code site requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TargetOs {
    /// Linux — every compiling job in this repository's CI.
    Linux,
    /// macOS. Nothing in CI compiles it; see the module docs.
    MacOs,
    /// Windows.
    Windows,
}

/// Which Cargo feature set a compilation used, or that a code site needs to be
/// compiled at all.
///
/// Not ordered by "more features": `--all-features` does not subsume the default
/// set, because code behind `cfg(not(feature = …))` is compiled by exactly one of
/// them. The `default-features` CI job exists because three `-D warnings` errors
/// had rotted in code that `--all-features` structurally cannot see.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Features {
    /// The crate's default feature set.
    Default,
    /// `--all-features`.
    All,
    /// `--no-default-features`.
    None,
}

/// Which targets a compilation built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Targets {
    /// Libraries and binaries only — `cargo check` with no `--all-targets`. Does
    /// **not** compile `#[cfg(test)]` modules or `tests/` integration targets.
    LibsAndBins,
    /// `--all-targets`: tests, benches and examples too.
    AllTargets,
}

impl Targets {
    /// Whether this scope compiles test code.
    #[must_use]
    pub fn compiles_tests(self) -> bool {
        self == Self::AllTargets
    }
}

/// How a check run finished. Only [`Conclusion::Success`] can refute anything; the
/// rest are spelled out so that "no check run at all" and "a check run that
/// failed" cannot be confused for each other by a caller mapping an API response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Conclusion {
    /// Green.
    Success,
    /// Red.
    Failure,
    /// Cancelled, timed out, skipped, or still running — anything that is not a
    /// statement about whether the code compiles.
    Inconclusive,
}

/// One compiling CI job, as it ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckRun {
    /// Job name, for the message a suppression prints (`msrv`, `checks`, …). A
    /// human told *which* job refutes the claim can go and look; a human told
    /// "CI is green" cannot.
    pub job: String,
    /// The commit this job ran on. Compared for equality with the claim's
    /// `reviewed_sha`: a green run on a *later* commit says nothing about the tree
    /// the reviewer saw.
    pub sha: String,
    /// How it finished.
    pub conclusion: Conclusion,
    /// The toolchain it used (`1.94`, `stable`), verbatim.
    pub toolchain: String,
    /// The platform it ran on.
    pub platform: TargetOs,
    /// The feature set it compiled.
    pub features: Features,
    /// The targets it compiled.
    pub targets: Targets,
}

/// The code a compile claim is about, in the terms that decide whether a job
/// compiled it.
///
/// Every field is a *requirement*, and `None` means "not established". An
/// unestablished requirement is never satisfied, so a claim whose site is unknown
/// is never suppressed — the default is to let the human look.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimSite {
    /// The commit the claim was made against — the corpus's `reviewed_sha`.
    pub sha: String,
    /// Path the claim is anchored to, for the suppression message.
    pub path: String,
    /// The platform whose `cfg` gates this code, or `None` if it is compiled on
    /// every platform.
    ///
    /// `Some(MacOs)` is the #291 shape: no CI job compiles it, so no CI job can
    /// refute a claim about it.
    pub platform: Option<TargetOs>,
    /// The feature set that compiles this code. `None` means unconditional — any
    /// feature set compiles it.
    ///
    /// `Some(Features::All)` covers a module behind a non-default feature, like
    /// `rto-exec`'s `#[cfg(feature = "exec-boxlite")] pub mod boxlite`.
    pub features: Option<Features>,
    /// Whether the site is test code (`#[cfg(test)]` or a `tests/` target). Test
    /// code needs a job that passed `--all-targets`.
    pub is_test_code: bool,
    /// The toolchain the claim is about, if it names one — a claim of the form
    /// "this is not on MSRV 1.94" is only refuted by a job that used that
    /// toolchain, not by a green `stable` build.
    pub toolchain: Option<String>,
}

impl ClaimSite {
    /// A site at `sha`/`path` with nothing else established — the conservative
    /// default, which no check run refutes.
    #[must_use]
    pub fn unknown(sha: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            sha: sha.into(),
            path: path.into(),
            ..Self::default()
        }
    }

    /// Whether `run` compiled this site's code, ignoring the run's conclusion and
    /// commit — the coverage half of the decision.
    #[must_use]
    pub fn covered_by(&self, run: &CheckRun) -> bool {
        // Platform: an unconditional site is compiled by every platform; a gated
        // site only by its own.
        if self.platform.is_some_and(|p| p != run.platform) {
            return false;
        }
        // Features: exact match, not subsumption. See `Features`.
        if self.features.is_some_and(|f| f != run.features) {
            return false;
        }
        // Targets: test code needs `--all-targets`.
        if self.is_test_code && !run.targets.compiles_tests() {
            return false;
        }
        // Toolchain: only a claim that names one constrains this.
        if self
            .toolchain
            .as_deref()
            .is_some_and(|t| t != run.toolchain)
        {
            return false;
        }
        true
    }
}

/// Whether a compile claim may be withheld, and why — or why not.
///
/// The negative variants carry their reason because that is the actionable half:
/// "unrefuted, because no green job compiled `cfg(target_os = "macos")` code at
/// that commit" tells a reviewer it owes the claim a real look, which is exactly
/// what the #291 teardown abort needed and did not get.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Suppression {
    /// A green job compiled this code at this commit. Withhold the claim.
    Refuted {
        /// The job that refutes it.
        job: String,
        /// One sentence naming the job, the commit and the configuration.
        reason: String,
    },
    /// No green job covered this configuration at this commit. Keep the claim.
    Unrefuted {
        /// Why the evidence falls short.
        reason: String,
    },
}

impl Suppression {
    /// Whether the claim should be withheld.
    #[must_use]
    pub fn is_refuted(&self) -> bool {
        matches!(self, Self::Refuted { .. })
    }

    /// The explanation, in either case.
    #[must_use]
    pub fn reason(&self) -> &str {
        match self {
            Self::Refuted { reason, .. } | Self::Unrefuted { reason } => reason,
        }
    }
}

/// Decide whether `checks` refute a compile claim about `site`.
///
/// Refuted only by a run that is [`Conclusion::Success`], ran at **exactly**
/// `site.sha`, and covered the site's configuration ([`ClaimSite::covered_by`]).
/// Among several qualifying runs the first in job-name order is reported, so the
/// answer does not depend on the order a caller happened to collect them in.
#[must_use]
pub fn suppression(site: &ClaimSite, checks: &[CheckRun]) -> Suppression {
    let at_sha: Vec<&CheckRun> = checks.iter().filter(|c| c.sha == site.sha).collect();
    if at_sha.is_empty() {
        return Suppression::Unrefuted {
            reason: format!(
                "no check run recorded at {} — a green run on any other commit says \
                 nothing about the tree the claim was made against",
                short(&site.sha)
            ),
        };
    }

    let mut green_covering: Vec<&CheckRun> = at_sha
        .iter()
        .copied()
        .filter(|c| c.conclusion == Conclusion::Success && site.covered_by(c))
        .collect();
    green_covering.sort_by(|a, b| a.job.cmp(&b.job));
    if let Some(run) = green_covering.first() {
        return Suppression::Refuted {
            job: run.job.clone(),
            reason: format!(
                "`{}` was green at {} and compiled {} ({}), so the claim that it \
                 does not build is already refuted",
                run.job,
                short(&site.sha),
                site.path,
                configuration(run),
            ),
        };
    }

    // Something ran at this commit, so say which axis fell short — a reviewer
    // reading "unrefuted" needs to know whether to look at the code or at CI.
    let covering: Vec<&CheckRun> = at_sha
        .iter()
        .copied()
        .filter(|c| site.covered_by(c))
        .collect();
    if covering.is_empty() {
        return Suppression::Unrefuted {
            reason: format!(
                "no check run at {} compiled {} ({}) — {}",
                short(&site.sha),
                site.path,
                requirement(site),
                "turning features on cannot find a defect in code cfg'd out, and \
                 no job here compiles another platform's code, so this claim is \
                 unrefuted and owes a real look",
            ),
        };
    }
    Suppression::Unrefuted {
        reason: format!(
            "the check run(s) covering {} at {} did not conclude green ({}), so \
             nothing refutes the claim",
            site.path,
            short(&site.sha),
            covering
                .iter()
                .map(|c| format!("{}: {:?}", c.job, c.conclusion))
                .collect::<Vec<_>>()
                .join(", "),
        ),
    }
}

/// The distinct job names that could ever refute a claim about `site`, given
/// `checks` — what to tell an operator whose CI does not cover a configuration.
#[must_use]
pub fn jobs_covering(site: &ClaimSite, checks: &[CheckRun]) -> BTreeSet<String> {
    checks
        .iter()
        .filter(|c| site.covered_by(c))
        .map(|c| c.job.clone())
        .collect()
}

/// A run's configuration, as one readable phrase.
fn configuration(run: &CheckRun) -> String {
    let features = match run.features {
        Features::Default => "default features",
        Features::All => "--all-features",
        Features::None => "--no-default-features",
    };
    let targets = match run.targets {
        Targets::LibsAndBins => "libs and bins",
        Targets::AllTargets => "--all-targets",
    };
    format!(
        "{:?}, {}, {}, toolchain {}",
        run.platform, features, targets, run.toolchain
    )
}

/// What a site needs compiled, as one readable phrase.
fn requirement(site: &ClaimSite) -> String {
    let mut parts = Vec::new();
    if let Some(p) = site.platform {
        parts.push(format!("needs {p:?}"));
    }
    if let Some(f) = site.features {
        parts.push(format!("needs {f:?} features"));
    }
    if site.is_test_code {
        parts.push("is test code, so needs --all-targets".to_owned());
    }
    if let Some(t) = &site.toolchain {
        parts.push(format!("the claim names toolchain {t}"));
    }
    if parts.is_empty() {
        "unconditional code".to_owned()
    } else {
        parts.join("; ")
    }
}

/// Short form of a sha for a message, without assuming it is 40 characters.
fn short(sha: &str) -> &str {
    sha.get(..8).unwrap_or(sha)
}

#[cfg(test)]
mod tests {
    use super::{
        CheckRun, ClaimSite, Conclusion, Features, Suppression, TargetOs, Targets, jobs_covering,
        suppression,
    };

    /// This repository's compiling jobs at a commit, as `.github/workflows/ci.yml`
    /// defines them. Written out because the whole filter turns on their exact
    /// narrowness: all three are `ubuntu-latest`, only `msrv` is on the MSRV
    /// toolchain, and only `msrv` omits `--all-targets`.
    fn ci_at(sha: &str) -> Vec<CheckRun> {
        vec![
            CheckRun {
                job: "msrv".to_owned(),
                sha: sha.to_owned(),
                conclusion: Conclusion::Success,
                toolchain: "1.94".to_owned(),
                platform: TargetOs::Linux,
                features: Features::All,
                targets: Targets::LibsAndBins,
            },
            CheckRun {
                job: "checks".to_owned(),
                sha: sha.to_owned(),
                conclusion: Conclusion::Success,
                toolchain: "stable".to_owned(),
                platform: TargetOs::Linux,
                features: Features::All,
                targets: Targets::AllTargets,
            },
            CheckRun {
                job: "default-features".to_owned(),
                sha: sha.to_owned(),
                conclusion: Conclusion::Success,
                toolchain: "stable".to_owned(),
                platform: TargetOs::Linux,
                features: Features::Default,
                targets: Targets::AllTargets,
            },
        ]
    }

    /// **The four corpus rows this filter is licensed by.** Each is unconditional
    /// library code — verified at its `reviewed_sha` — except `boxlite.rs`, whose
    /// module is `#[cfg(feature = "exec-boxlite")]`, so it needs an
    /// `--all-features` job. None is test code. Every one is refuted, which is the
    /// measured claim: the filter discards nothing true on this corpus.
    #[test]
    fn every_known_false_compile_claim_is_refuted() {
        let rows = [
            ("2b761ce7", "crates/rto-llama/src/slot.rs", None),
            ("5e25f921", "crates/rto-graph/src/media.rs", None),
            ("add397f2", "crates/roteiro/src/main.rs", None),
            (
                "c1481836",
                "crates/rto-exec/src/boxlite.rs",
                Some(Features::All),
            ),
        ];
        for (sha, path, features) in rows {
            let site = ClaimSite {
                features,
                ..ClaimSite::unknown(sha, path)
            };
            let verdict = suppression(&site, &ci_at(sha));
            assert!(
                verdict.is_refuted(),
                "{path} at {sha} should be refuted: {}",
                verdict.reason()
            );
            assert!(
                verdict.reason().contains(sha) && verdict.reason().contains(path),
                "the reason names the commit and the file: {}",
                verdict.reason()
            );
        }
    }

    /// The #352 claim named a toolchain — "not on MSRV 1.94". Only the `msrv` job
    /// can refute that; a green `stable` build cannot. The filter must pick the
    /// right job rather than any green one.
    #[test]
    fn a_claim_naming_the_msrv_toolchain_is_refuted_only_by_the_msrv_job() {
        let sha = "c1481836";
        let site = ClaimSite {
            features: Some(Features::All),
            toolchain: Some("1.94".to_owned()),
            ..ClaimSite::unknown(sha, "crates/rto-exec/src/boxlite.rs")
        };
        let Suppression::Refuted { ref job, .. } = suppression(&site, &ci_at(sha)) else {
            panic!("the msrv job compiled it");
        };
        assert_eq!(job, "msrv");

        // Strip the MSRV job and the same claim stands: the remaining green jobs
        // are `stable`, which says nothing about 1.94.
        let stable_only: Vec<CheckRun> =
            ci_at(sha).into_iter().filter(|c| c.job != "msrv").collect();
        let verdict = suppression(&site, &stable_only);
        assert!(!verdict.is_refuted(), "{}", verdict.reason());
        assert!(
            verdict.reason().contains("1.94"),
            "says which toolchain went uncovered: {}",
            verdict.reason()
        );
    }

    /// **The #291 case, and the reason this is not a "green build" check.** The
    /// `GGML_ASSERT` teardown abort was `cfg(target_os = "macos")`. Every CI job
    /// here is `ubuntu-latest`, so a wholly green CI must leave a claim about that
    /// code standing.
    #[test]
    fn a_macos_only_site_is_never_refuted_by_ci_here() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let site = ClaimSite {
            platform: Some(TargetOs::MacOs),
            ..ClaimSite::unknown(sha, "crates/rto-llama/src/backend.rs")
        };
        let verdict = suppression(&site, &ci_at(sha));
        assert!(
            !verdict.is_refuted(),
            "a green ubuntu CI must not refute macOS-only code: {}",
            verdict.reason()
        );
        assert!(
            verdict.reason().contains("MacOs"),
            "names the uncovered platform: {}",
            verdict.reason()
        );
        assert!(
            jobs_covering(&site, &ci_at(sha)).is_empty(),
            "no job in this repository compiles macOS code"
        );
    }

    /// A `--no-default-features` claim is unrefuted: no job here builds that set,
    /// and `--all-features` cannot cover it because the two compile different code.
    #[test]
    fn a_no_default_features_site_is_unrefuted() {
        let sha = "abcdefabcdefabcdefabcdefabcdefabcdefabcd";
        let site = ClaimSite {
            features: Some(Features::None),
            ..ClaimSite::unknown(sha, "crates/rto-graph/src/lib.rs")
        };
        assert!(!suppression(&site, &ci_at(sha)).is_refuted());
    }

    /// Turning features *on* cannot find a defect in code being cfg'd *out*: a
    /// default-set site is not covered by the `--all-features` jobs, only by
    /// `default-features`.
    #[test]
    fn a_default_features_site_is_covered_only_by_the_default_features_job() {
        let sha = "1111111111111111111111111111111111111111";
        let site = ClaimSite {
            features: Some(Features::Default),
            ..ClaimSite::unknown(sha, "crates/rto-llama/src/lib.rs")
        };
        assert_eq!(
            jobs_covering(&site, &ci_at(sha)),
            ["default-features".to_owned()].into_iter().collect()
        );
    }

    /// **The gap the model exposes.** `msrv` is `cargo check --workspace
    /// --all-features` with no `--all-targets`, so it never compiles test code;
    /// the jobs that do compile test code run on `stable`. A claim that test code
    /// will not build on the MSRV toolchain is therefore refuted by no job in this
    /// repository, and the filter must say so rather than suppress it.
    #[test]
    fn an_msrv_claim_about_test_code_is_refuted_by_no_job_here() {
        let sha = "2222222222222222222222222222222222222222";
        let site = ClaimSite {
            is_test_code: true,
            toolchain: Some("1.94".to_owned()),
            ..ClaimSite::unknown(sha, "crates/rto-graph/tests/review_corpus.rs")
        };
        let verdict = suppression(&site, &ci_at(sha));
        assert!(
            !verdict.is_refuted(),
            "no job compiles test code on the MSRV toolchain: {}",
            verdict.reason()
        );
        assert!(
            jobs_covering(&site, &ci_at(sha)).is_empty(),
            "msrv omits --all-targets; the --all-targets jobs are stable"
        );
        // The same site without a toolchain claim *is* covered — by the stable
        // jobs that pass `--all-targets`. So the gap is specifically MSRV-and-test,
        // not test code in general.
        let stable_claim = ClaimSite {
            toolchain: None,
            ..site
        };
        assert!(suppression(&stable_claim, &ci_at(sha)).is_refuted());
    }

    /// A green run on a different commit refutes nothing. This is the sibling of
    /// the corpus's `reviewed_sha` rule: the tree that matters is the one the
    /// reviewer saw.
    #[test]
    fn a_green_run_on_another_commit_refutes_nothing() {
        let site = ClaimSite::unknown(
            "3333333333333333333333333333333333333333",
            "crates/rto-graph/src/lib.rs",
        );
        let elsewhere = ci_at("4444444444444444444444444444444444444444");
        let verdict = suppression(&site, &elsewhere);
        assert!(!verdict.is_refuted());
        assert!(
            verdict.reason().contains("no check run recorded"),
            "{}",
            verdict.reason()
        );
    }

    /// A failing or still-running job is not a refutation, and the message says
    /// which it was — "unrefuted" alone would leave a reviewer unsure whether to
    /// look at the code or wait for CI.
    #[test]
    fn a_non_green_conclusion_is_not_a_refutation() {
        let sha = "5555555555555555555555555555555555555555";
        for conclusion in [Conclusion::Failure, Conclusion::Inconclusive] {
            let runs: Vec<CheckRun> = ci_at(sha)
                .into_iter()
                .map(|c| CheckRun { conclusion, ..c })
                .collect();
            let site = ClaimSite::unknown(sha, "crates/roteiro/src/main.rs");
            let verdict = suppression(&site, &runs);
            assert!(!verdict.is_refuted(), "{conclusion:?}");
            assert!(
                verdict.reason().contains(&format!("{conclusion:?}")),
                "names the conclusion: {}",
                verdict.reason()
            );
        }
    }

    /// With no evidence at all, nothing is suppressed. The filter is opt-in on
    /// evidence, so a caller that cannot reach CI loses the filter rather than
    /// gaining a blanket suppression.
    #[test]
    fn no_evidence_suppresses_nothing() {
        let site = ClaimSite::unknown(
            "6666666666666666666666666666666666666666",
            "crates/rto-graph/src/lib.rs",
        );
        assert!(!suppression(&site, &[]).is_refuted());
    }

    /// The reported job does not depend on the order the caller collected runs in.
    #[test]
    fn the_reported_job_is_order_independent() {
        let sha = "7777777777777777777777777777777777777777";
        let site = ClaimSite::unknown(sha, "crates/roteiro/src/main.rs");
        let mut reversed = ci_at(sha);
        reversed.reverse();
        let forward = suppression(&site, &ci_at(sha));
        let backward = suppression(&site, &reversed);
        assert_eq!(forward, backward);
        let Suppression::Refuted { ref job, .. } = forward else {
            panic!("unconditional code is refuted by a green all-features job");
        };
        assert_eq!(job, "checks", "job-name order, not collection order");
    }
}

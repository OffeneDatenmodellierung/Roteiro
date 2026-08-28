//! Guard: every job in `ci.yml` is bounded, and so is every apt fetch in it.
//!
//! Until the change that added this file there was no `timeout-minutes` anywhere
//! in `ci.yml`, so every job inherited GitHub's 6-hour default. That was not a
//! theoretical exposure. Eleven jobs across six of the last ~40 CI runs were
//! cancelled at the 6-hour mark, every one of them wedged on the same
//! `apt-get install` step — roughly 66 hours of runner time. `checks`,
//! `coverage` and `msrv` were all hit, and `msrv` is a required status check, so
//! a hung run also left a pull request unmergeable for six hours with nothing
//! showing red to explain why.
//!
//! Why a test rather than trusting the file: the defect is **silent and
//! cumulative**. An unbounded job costs nothing until the day something hangs,
//! and on that day it costs six hours per job. Nothing goes red, no reviewer
//! notices a missing key, and the next job added to the matrix inherits the
//! exposure by default — which is the same shape as #482, where a job was added
//! and nobody asked what release PRs should do with it. `ci_release_pr_parity`
//! made that class of omission impossible for release-PR arms; this does the
//! same for bounds.
//!
//! The ceiling is as load-bearing as the presence check. `timeout-minutes: 350`
//! would satisfy "is bounded" while restoring the whole defect, so a bound also
//! has to be *small enough to be a bound*. [`CEILING_MINUTES`] is set against
//! measured durations, not taste.
//!
//! Fault injection, both directions, confirmed against this file:
//!   - delete `timeout-minutes` from the `coverage` job -> `coverage` is named
//!     by `every_ci_job_is_bounded`
//!   - raise `checks` to 350 -> named by `a_job_bound_is_small_enough_to_be_one`
//!   - drop `timeout-minutes` from an apt step -> named by
//!     `every_apt_step_is_bounded`

mod common;

use yaml_rust2::Yaml;

/// The largest `timeout-minutes` that still counts as a bound.
///
/// Sized from the measured maximum successful job durations at the time of
/// writing (`gh api repos/:owner/:repo/actions/runs/<id>/jobs`, last ~40 runs):
/// `checks` 52.2 min was the slowest, then `coverage` 45.6 and `msrv` 30.6. The
/// file's own bounds are set at roughly 2x each job's maximum, so 120 leaves
/// room for every one of them plus a cold cache, while still being far enough
/// below GitHub's 6-hour default that a runaway job is caught the same hour it
/// starts rather than the same working day.
///
/// If a job legitimately needs more than this, raise it here **and** say in
/// `ci.yml` what was measured — a ceiling nobody can justify is one somebody
/// will quietly double.
const CEILING_MINUTES: i64 = 120;

/// The jobs of `ci.yml`, or `None` outside a repository checkout.
///
/// A packaged crate has no `.github/`, and that is the only legitimate absence.
/// Absent *in* a checkout is a failure: the file is committed, so it is supposed
/// to be there, and a green meaning "could not look" is worse than no guard.
fn ci_jobs() -> Option<Vec<(String, Yaml)>> {
    let rel = ".github/workflows/ci.yml";
    let text = common::repo_file(rel)?;
    let docs = yaml_rust2::YamlLoader::load_from_str(&text)
        .unwrap_or_else(|e| panic!("{rel} is not parseable YAML: {e}"));
    let doc = docs
        .first()
        .unwrap_or_else(|| panic!("{rel} is an empty YAML document"));
    let jobs = doc
        .as_hash()
        .and_then(|h| h.get(&Yaml::String("jobs".to_owned())))
        .and_then(Yaml::as_hash)
        .unwrap_or_else(|| panic!("{rel} has no `jobs:` mapping"));
    Some(
        jobs.iter()
            .filter_map(|(name, job)| Some((name.as_str()?.to_owned(), job.clone())))
            .collect(),
    )
}

/// A node's `timeout-minutes`, when it has one.
///
/// Read as an integer specifically. GitHub accepts an expression here, and a
/// `${{ … }}` string would parse as YAML but is not a bound this test can check
/// — treating it as absent is the safe reading, and the failure message says so.
fn timeout_minutes(node: &Yaml) -> Option<i64> {
    node.as_hash()?
        .get(&Yaml::String("timeout-minutes".to_owned()))?
        .as_i64()
}

/// A job's `steps:`, or an empty slice for a job that has none.
fn steps(job: &Yaml) -> &[Yaml] {
    job.as_hash()
        .and_then(|h| h.get(&Yaml::String("steps".to_owned())))
        .and_then(Yaml::as_vec)
        .map_or(&[], Vec::as_slice)
}

/// How a step is labelled in a failure message: its `name:`, else its `uses:`.
fn step_label(step: &Yaml) -> String {
    let get = |k: &str| {
        step.as_hash()
            .and_then(|h| h.get(&Yaml::String(k.to_owned())))
            .and_then(Yaml::as_str)
    };
    get("name")
        .or_else(|| get("uses"))
        .unwrap_or("<unnamed step>")
        .to_owned()
}

/// Every job carries a `timeout-minutes`.
#[test]
fn every_ci_job_is_bounded() {
    let Some(jobs) = ci_jobs() else {
        return; // not a source checkout
    };
    assert!(!jobs.is_empty(), "ci.yml parsed to zero jobs");

    let unbounded: Vec<&str> = jobs
        .iter()
        .filter(|(_, job)| timeout_minutes(job).is_none())
        .map(|(name, _)| name.as_str())
        .collect();

    assert!(
        unbounded.is_empty(),
        "job(s) {unbounded:?} in ci.yml have no integer `timeout-minutes`, so \
         they inherit GitHub's 6-hour default. That default has already cost \
         this repository ~66 hours of runner time in eleven jobs wedged on a \
         hung apt step — and when the job is a required check, six hours of an \
         unmergeable PR with nothing showing red. Add a `timeout-minutes:` \
         sized against the job's measured duration, as the others are."
    );
}

/// Every job's bound is small enough to actually be a bound.
#[test]
fn a_job_bound_is_small_enough_to_be_one() {
    let Some(jobs) = ci_jobs() else {
        return; // not a source checkout
    };

    let too_loose: Vec<(&str, i64)> = jobs
        .iter()
        .filter_map(|(name, job)| Some((name.as_str(), timeout_minutes(job)?)))
        .filter(|(_, mins)| *mins > CEILING_MINUTES)
        .collect();

    assert!(
        too_loose.is_empty(),
        "job(s) {too_loose:?} carry a `timeout-minutes` above the \
         {CEILING_MINUTES}-minute ceiling. A bound that large is not a bound: \
         the slowest successful job measured here was 52 minutes, so anything \
         approaching the 6-hour default reinstates the defect while looking \
         like it was fixed. If the work genuinely grew, raise CEILING_MINUTES \
         here and record the new measurement in ci.yml."
    );
}

/// Every step that reaches an apt mirror is bounded at the step level.
///
/// The job bound alone is not enough for these. A job bounded at 90 minutes
/// still burns 90 minutes on a wedged mirror, and it fails as "the job timed
/// out" rather than as "apt could not reach the mirror" — which sends the reader
/// looking at the build instead of at the network. Every six-hour cancellation
/// in this repository's history was one of these steps, so they carry their own,
/// much tighter bound.
#[test]
fn every_apt_step_is_bounded() {
    let Some(jobs) = ci_jobs() else {
        return; // not a source checkout
    };

    let mut unbounded: Vec<String> = Vec::new();
    for (name, job) in &jobs {
        for step in steps(job) {
            let run = step
                .as_hash()
                .and_then(|h| h.get(&Yaml::String("run".to_owned())))
                .and_then(Yaml::as_str)
                .unwrap_or_default();
            if run.contains("apt-get") && timeout_minutes(step).is_none() {
                unbounded.push(format!("{name} / {}", step_label(step)));
            }
        }
    }

    assert!(
        unbounded.is_empty(),
        "step(s) {unbounded:?} run `apt-get` with no step-level \
         `timeout-minutes`. Every six-hour job cancellation in this \
         repository's history was an unbounded apt step. The job bound is not a \
         substitute: it fails much later and reports a timed-out job rather \
         than an unreachable mirror."
    );
}

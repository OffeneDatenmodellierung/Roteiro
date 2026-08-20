//! Guard: every required-check job in `ci.yml` carries both release-PR arms
//! (issue #494).
//!
//! #481 taught `ci.yml` to skip expensive work on a release-plz version-bump PR.
//! The shape it settled on, and the shape this guard holds in place: a job that
//! is a **required status check** still RUNS — so branch protection is satisfied
//! by a genuine `success` from a job that verified something, rather than by
//! `skipped`-counts-as-success — but its expensive steps carry `if:
//! env.RELEASE_PR != 'true'`, and one cheap step carries `if: env.RELEASE_PR ==
//! 'true'` and runs `cargo metadata --locked`, which is what catches a lockfile
//! left behind by the bump or a path-dependency constraint moved ahead of the
//! member it points at.
//!
//! #482 then added the `no-default-features` job. The two PRs touched different
//! regions of the file and merged with **zero conflict markers** — a three-way
//! merge was checked before either landed — and the combination left the new job
//! with neither arm: 595 seconds on every release PR, where its three siblings
//! cost 42 seconds between them.
//!
//! Nothing failed, and `strict: true` branch protection could not catch it: it
//! re-ran #482 against the merged result and that run was legitimately green,
//! because the defect is **unreachable from any branch not named
//! `release-plz-*`**. A defect that only exists on one class of branch has no
//! detector that runs the pipeline. It has to be read out of the file, which is
//! what this test does — no release PR, no CI run, nothing to schedule.
//!
//! Fault injection: run these assertions against `ci.yml` as it stood at #482's
//! merge commit (`0db61fe`) and `no-default-features` is named red, at the only
//! moment the fix was cheap.

use std::collections::BTreeSet;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use yaml_rust2::Yaml;

/// The jobs that are **required status checks** on `main`.
///
/// SOURCE OF TRUTH: GitHub branch protection, which is not in this repository
/// and which a test cannot read offline. Re-read it with:
///
/// ```text
/// gh api repos/:owner/:repo/branches/main/protection --jq '.required_status_checks.contexts'
/// ```
///
/// This is therefore a second copy of a fact, which is the same rot the test
/// exists to prevent — so it is written down rather than inferred, and the
/// exhaustiveness check below is what keeps it from drifting silently. The list
/// cannot be derived from `ci.yml`: nothing in the workflow distinguishes
/// `coverage`, which is deliberately **not** required (#319 — it measures, and
/// there is no ratchet to gate on).
///
/// WHEN BRANCH PROTECTION CHANGES: edit both lists here in the same change.
/// Adding a context here without the job carrying both arms fails
/// [`every_required_job_verifies_the_manifests_on_a_release_pr`]; removing a job
/// from `ci.yml` without removing it here fails
/// [`every_ci_job_is_classified_as_required_or_not`].
const REQUIRED_CONTEXTS: &[&str] = &["checks", "default-features", "msrv", "no-default-features"];

/// The jobs that are deliberately **not** required checks, each with the reason.
///
/// Being on this list is not an exemption from thinking about release PRs — see
/// [`a_job_that_is_not_a_required_check_is_skipped_outright_on_a_release_pr`].
/// It is the other half of the classification, and the pair of lists is what
/// makes a new job impossible to add without deciding which it is.
const NOT_REQUIRED_JOBS: &[(&str, &str)] = &[(
    "coverage",
    "#319: measured, not gated — `continue-on-error: true` and no agreed \
     threshold, so nothing depends on its verdict",
)];

/// Steps that legitimately run on a release PR without a `RELEASE_PR` arm.
///
/// Both are seconds of runner setup with no build in them, and a job that
/// skipped them could not run its cheap arm either. Anything else — an apt
/// install, a cache restore, a tool download — belongs behind an arm. Matched as
/// a prefix of `uses:`, so the pinned SHA and version comment do not matter.
const ALWAYS_RUN_ACTIONS: &[&str] = &["actions/checkout@", "dtolnay/rust-toolchain@"];

/// The step-level guard on work a release PR must not pay for.
const EXPENSIVE_ARM: &str = "env.RELEASE_PR != 'true'";

/// The step-level guard on the cheap verification a release PR does instead.
const CHEAP_ARM: &str = "env.RELEASE_PR == 'true'";

/// The repository root, from this crate's manifest directory (`crates/roteiro`).
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root is two levels above crates/roteiro")
        .to_path_buf()
}

/// Whether this is a checkout of the Roteiro repository, rather than a packaged
/// crate unpacked from the registry.
///
/// The marker is the **workspace** manifest two levels above `crates/roteiro`. A
/// packaged crate has no such ancestor: `CARGO_MANIFEST_DIR` is then
/// `…/registry/src/<index>/roteiro-<version>`, and two levels above that is the
/// registry's source directory, which holds sibling crates and no workspace
/// manifest.
///
/// This marker has to be as loud as the thing it guards, or it is the same
/// defect one level up with more steps: a marker that read `false` on an IO
/// error would turn "cannot read the repository" into "this is not a
/// repository", and skip. So only `NotFound` means absent, and every other error
/// panics.
fn is_repository_checkout() -> bool {
    let manifest = repo_root().join("Cargo.toml");
    match std::fs::read_to_string(&manifest) {
        Ok(text) => text.lines().any(|line| line.trim() == "[workspace]"),
        Err(e) if e.kind() == ErrorKind::NotFound => false,
        Err(e) => panic!(
            "cannot read {} ({:?}: {e}). Without it this test cannot tell a \
             packaged crate from a repository checkout, and guessing would make \
             the guard skip in silence — which is the failure this whole file \
             exists to rule out.",
            manifest.display(),
            e.kind(),
        ),
    }
}

/// A repository file's contents, or `None` when this is not a repository
/// checkout at all.
///
/// The skip is legitimate: a packaged crate has no `.github/`, and this guard is
/// about *this repository*. Collapsing every IO error into that one meaning is
/// not. This guard exists to catch a defect that is **invisible by
/// construction** — #482's gap was unreachable from any branch not named
/// `release-plz-*`, which is why nothing else could find it — so a green that
/// actually means "could not read the subject" is worse than no guard at all,
/// because by then the green is load-bearing. #401's fragment guard set the
/// standard by failing on an empty scan rather than skipping.
///
/// So the skip is made *verifiable* rather than merely narrower. Absent **and**
/// not a checkout is the skip. Absent **in** a checkout is a failure — the file
/// is committed, so it is supposed to be there. Anything else — permissions, a
/// bad symlink, an IO error mid-read — panics naming the path and the kind.
fn read_repo_file(rel: &str) -> Option<String> {
    let path = repo_root().join(rel);
    match std::fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == ErrorKind::NotFound && !is_repository_checkout() => None,
        Err(e) => panic!(
            "cannot read {} ({:?}: {e}). This guard asserts a property of that \
             file, so skipping here would be a green that means \"could not \
             look\". If the file moved or was deliberately deleted, this test \
             moves or goes with it.",
            path.display(),
            e.kind(),
        ),
    }
}

/// The parsed `jobs:` mapping of the workflow at `rel`, or `None` outside a
/// repository checkout.
fn workflow_jobs(rel: &str) -> Option<Vec<(String, Yaml)>> {
    let text = read_repo_file(rel)?;
    let docs = yaml_rust2::YamlLoader::load_from_str(&text)
        .unwrap_or_else(|e| panic!("{rel} is not parseable YAML: {e}"));
    let doc = docs
        .first()
        .unwrap_or_else(|| panic!("{rel} is an empty YAML document"));
    let jobs = field(doc, "jobs")
        .and_then(Yaml::as_hash)
        .unwrap_or_else(|| panic!("{rel} has no `jobs:` mapping"));
    Some(
        jobs.iter()
            .filter_map(|(name, job)| Some((name.as_str()?.to_owned(), job.clone())))
            .collect(),
    )
}

/// The jobs of this repository's `ci.yml`.
fn ci_jobs() -> Option<Vec<(String, Yaml)>> {
    workflow_jobs(".github/workflows/ci.yml")
}

/// The value at `key` in a YAML mapping.
fn field<'a>(node: &'a Yaml, key: &str) -> Option<&'a Yaml> {
    node.as_hash()?.get(&Yaml::String(key.to_owned()))
}

/// The string value at `key` in a YAML mapping.
fn field_str<'a>(node: &'a Yaml, key: &str) -> Option<&'a str> {
    field(node, key)?.as_str()
}

/// A job's `steps:`, or an empty slice for a job that has none.
fn steps(job: &Yaml) -> &[Yaml] {
    field(job, "steps")
        .and_then(Yaml::as_vec)
        .map_or(&[], Vec::as_slice)
}

/// A step's or job's `if:` with the `${{ … }}` wrapper stripped and whitespace
/// collapsed, so `${{ env.RELEASE_PR != 'true' }}` and `env.RELEASE_PR != 'true'`
/// compare equal. Both spellings are valid in a workflow and both appear in the
/// wild; a guard that recognised only one would be satisfiable by accident.
fn condition(node: &Yaml) -> Option<String> {
    let raw = field_str(node, "if")?.trim();
    let inner = raw
        .strip_prefix("${{")
        .and_then(|s| s.strip_suffix("}}"))
        .unwrap_or(raw);
    Some(inner.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// How a step is labelled in a failure message: its `name:`, else its `uses:`.
fn step_label(step: &Yaml) -> String {
    field_str(step, "name")
        .or_else(|| field_str(step, "uses"))
        .unwrap_or("<unnamed step>")
        .to_owned()
}

/// Whether a step is one of the declared always-run setup actions.
fn is_always_run_setup(step: &Yaml) -> bool {
    field_str(step, "uses").is_some_and(|uses| {
        ALWAYS_RUN_ACTIONS
            .iter()
            .any(|allowed| uses.trim().starts_with(allowed))
    })
}

/// Every job in `ci.yml` is classified: required, or not-required-with-a-reason.
///
/// This is the assertion that makes the hard-coded [`REQUIRED_CONTEXTS`] list
/// honest rather than merely present. The union of the two lists must equal the
/// set of jobs in the file **exactly**, so a job cannot be added without
/// somebody deciding which it is — and the parity checks below then apply to it
/// without anyone having to remember to extend them. That is the step #482
/// missed: it added a job and nothing anywhere asked what release PRs should do
/// with it.
#[test]
fn every_ci_job_is_classified_as_required_or_not() {
    let Some(jobs) = ci_jobs() else {
        return; // not a source checkout
    };

    let in_file: BTreeSet<&str> = jobs.iter().map(|(name, _)| name.as_str()).collect();
    let classified: BTreeSet<&str> = REQUIRED_CONTEXTS
        .iter()
        .copied()
        .chain(NOT_REQUIRED_JOBS.iter().map(|(name, _)| *name))
        .collect();

    let unclassified: Vec<&str> = in_file.difference(&classified).copied().collect();
    assert!(
        unclassified.is_empty(),
        "ci.yml defines job(s) {unclassified:?} that this test does not classify. \
         Add each to REQUIRED_CONTEXTS if it is a required status check on `main` \
         (check with `gh api repos/:owner/:repo/branches/main/protection --jq \
         '.required_status_checks.contexts'`), or to NOT_REQUIRED_JOBS with the \
         reason it is not. A job nobody classified is how #482 shipped a job that \
         cost 595s on every release PR."
    );

    let missing: Vec<&str> = classified.difference(&in_file).copied().collect();
    assert!(
        missing.is_empty(),
        "this test names job(s) {missing:?} that ci.yml no longer defines. If a \
         job was renamed, rename it here too AND in branch protection — a \
         required context that names no job leaves a PR pending forever with \
         nothing showing red to explain why."
    );
}

/// Every required-check job does the cheap manifest verification on a release PR.
///
/// The `== 'true'` arm. Without it the job runs, skips everything, and reports a
/// `success` that verified nothing — which is worse than the 595 seconds,
/// because a version bump is not a no-op: it can desynchronise `Cargo.lock` from
/// the manifests, and it can move a `version =` constraint on a workspace path
/// dependency ahead of the member it points at. `cargo metadata --locked` catches
/// both without compiling anything.
#[test]
fn every_required_job_verifies_the_manifests_on_a_release_pr() {
    let Some(jobs) = ci_jobs() else {
        return; // not a source checkout
    };

    for context in REQUIRED_CONTEXTS {
        let Some((_, job)) = jobs.iter().find(|(name, _)| name == context) else {
            continue; // reported by the classification test
        };
        let verified = steps(job).iter().any(|step| {
            let guarded = condition(step).is_some_and(|c| c.contains(CHEAP_ARM));
            let run = field_str(step, "run").unwrap_or_default();
            guarded && run.contains("cargo metadata") && run.contains("--locked")
        });
        assert!(
            verified,
            "the `{context}` job is a required status check but has no step \
             guarded by `if: {CHEAP_ARM}` that runs `cargo metadata --locked`. On \
             a release-plz PR it would report `success` having verified nothing. \
             Copy the `Manifests and lockfile agree (release PR)` step from the \
             `checks` job."
        );
    }
}

/// A required-check job is never skipped at the job level.
///
/// Every other assertion here reads a job's **steps** — and a job that does not
/// run has no steps that run either, so all of them are vacuously satisfied by a
/// job that GitHub skips. A skipped job reports `skipped`, which branch
/// protection counts among the successful statuses, so the required context goes
/// green having verified nothing. That is precisely the failure mode this file
/// exists to prevent, reached by the one route the step-level checks are blind
/// to: adding `if: !startsWith(github.head_ref, 'release-plz-')` to a required
/// job would leave every assertion above passing.
///
/// Asserted as "no job-level `if:` at all" rather than "none that mentions
/// `release-plz-`". The narrow form is evadable — the same exclusion can be
/// spelled `github.event_name == 'push'`, or via an output of another job — and
/// the invariant really is the broader one. The comment at the top of `ci.yml`
/// argues at length that these jobs must RUN, so the required contexts are
/// satisfied by a genuine `success` from a job that verified something rather
/// than by `skipped`-counts-as-success. A condition that can skip one of them is
/// against that whether or not it names a release branch.
///
/// The counterpart for non-required jobs is the opposite and sits below: those
/// SHOULD be skipped outright, because nothing depends on them reporting.
#[test]
fn a_required_check_job_is_never_skipped_at_the_job_level() {
    let Some(jobs) = ci_jobs() else {
        return; // not a source checkout
    };

    for context in REQUIRED_CONTEXTS {
        let Some((_, job)) = jobs.iter().find(|(name, _)| name == context) else {
            continue; // reported by the classification test
        };
        let condition = condition(job);
        assert!(
            condition.is_none(),
            "the required job `{context}` carries a job-level `if:` ({:?}). A \
             skipped job reports `skipped`, which branch protection accepts as \
             success — so the context would go green having verified nothing, \
             and every step-level assertion in this file would pass vacuously \
             because none of the steps ran. Move the condition onto the steps: \
             `if: {EXPENSIVE_ARM}` on the expensive ones, `if: {CHEAP_ARM}` on \
             the manifest check. If the job genuinely should not report at all, \
             it is not a required check — take it out of branch protection and \
             move it to NOT_REQUIRED_JOBS in the same change.",
            condition.as_deref().unwrap_or("absent"),
        );
    }
}

/// No required-check job pays for expensive work on a release PR.
///
/// The `!= 'true'` arm, asserted over *every* step rather than over a list of
/// expensive ones — a step is guarded, or it is one of the two declared
/// always-run setup actions, and there is no third category. That totality is
/// the point: #482's `no-default-features` job had a cache restore, a clippy run
/// and a test run, and enumerating "expensive steps" would have needed somebody
/// to notice the job existed in the first place.
#[test]
fn no_required_job_pays_for_expensive_work_on_a_release_pr() {
    let Some(jobs) = ci_jobs() else {
        return; // not a source checkout
    };

    for context in REQUIRED_CONTEXTS {
        let Some((_, job)) = jobs.iter().find(|(name, _)| name == context) else {
            continue; // reported by the classification test
        };
        for step in steps(job) {
            if is_always_run_setup(step) {
                continue;
            }
            let condition = condition(step);
            let armed = condition
                .as_deref()
                .is_some_and(|c| c.contains(EXPENSIVE_ARM) || c.contains(CHEAP_ARM));
            assert!(
                armed,
                "step `{}` of the required job `{context}` carries no release-PR \
                 arm (`if:` is {:?}), so it runs in full on every release-plz \
                 version bump. Guard it with `if: {EXPENSIVE_ARM}`, or — if it is \
                 genuinely setup that both arms need — add its action to \
                 ALWAYS_RUN_ACTIONS with the reason.",
                step_label(step),
                condition.as_deref().unwrap_or("absent"),
            );
        }
    }
}

/// A job that is *not* a required check is skipped outright on a release PR.
///
/// The other half of the classification, and what stops [`NOT_REQUIRED_JOBS`]
/// being a place to park a job to get it past the checks above. Nothing depends
/// on a non-required job reporting, so it can take the full saving with a
/// job-level `if:` — and if it does not, it is simply expensive for no reason,
/// which is the defect in its purest form. `coverage` at 452s was the single most
/// expensive job in the matrix.
///
/// Note the predicate is spelled out longhand there rather than as
/// `env.RELEASE_PR`: the `env` context is not available in a job-level `if:`, so
/// the shorthand would silently never match. This test looks for the branch
/// prefix, which is what both spellings have in common.
#[test]
fn a_job_that_is_not_a_required_check_is_skipped_outright_on_a_release_pr() {
    let Some(jobs) = ci_jobs() else {
        return; // not a source checkout
    };

    for (name, reason) in NOT_REQUIRED_JOBS {
        let Some((_, job)) = jobs.iter().find(|(job_name, _)| job_name == name) else {
            continue; // reported by the classification test
        };
        let condition = condition(job);
        assert!(
            condition
                .as_deref()
                .is_some_and(|c| c.contains("release-plz-")),
            "the `{name}` job is classified not-required ({reason}) but its \
             job-level `if:` is {:?}, which does not exclude a release-plz \
             branch. Nothing depends on it reporting, so it should be skipped \
             outright rather than run and pay for itself. If it has become a \
             required check, move it to REQUIRED_CONTEXTS instead — a required \
             job must RUN, and skipping it would be a different argument.",
            condition.as_deref().unwrap_or("absent"),
        );
    }
}

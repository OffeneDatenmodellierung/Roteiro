//! Guard: the documented coverage story must match the pipeline (issue #319).
//!
//! `BUILD_PLAN.md` asserted for a long time that `cargo-llvm-cov` ran in CI with
//! an 85% per-file floor while `.github/workflows/ci.yml` contained no coverage
//! tooling at all. That is a false statement about the pipeline, not an
//! aspiration: it made every stage's definition of done citing "85% coverage"
//! unverifiable, and
//! three separate agents reported it independently while trying to comply.
//!
//! Nothing in the build could notice, because a claim about CI is not code. This
//! test makes it noticeable. It does not check coverage — it checks that the
//! *description* of coverage stays true:
//!
//! 1. CI still measures coverage (so the docs' "measured in CI" stays true), and
//! 2. that job is still non-blocking (so the docs' "not gated" stays true), and
//! 3. no document reinstates the claim that the floor is enforced.
//!
//! Making the coverage job blocking is a fine thing to do — but it has to happen
//! in the same change that rewrites §11, and this test is what forces the pair.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

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
             the guard skip in silence.",
            manifest.display(),
            e.kind(),
        ),
    }
}

/// Read a repo file, or `None` when this is not a repository checkout at all.
///
/// The skip is legitimate: a packaged crate has no `.github/` or `docs/`, and
/// this guard is about *this repository*. Collapsing every IO error into that
/// one meaning is not, and it used to — `.ok()` reported "absent" for a
/// permission error, a bad symlink or any other IO fault, so the guard could
/// report green having read nothing. For a test whose entire job is to notice a
/// claim quietly going false, that is the failure it exists to prevent, wearing
/// the guard's own clothes.
///
/// So the skip is verifiable rather than merely narrow. Absent **and** not a
/// checkout is the skip. Absent **in** a checkout is a failure: every path this
/// reads is committed, and a document deleted out from under the list here
/// should take the list entry with it rather than silently stop being checked.
/// Anything else panics naming the path and the error kind.
fn repo_file(rel: &str) -> Option<String> {
    let path = repo_root().join(rel);
    match std::fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == ErrorKind::NotFound && !is_repository_checkout() => None,
        Err(e) => panic!(
            "cannot read {} ({:?}: {e}). This guard asserts a property of that \
             file, so skipping here would be a green that means \"could not \
             look\". If the file moved or was deliberately deleted, update the \
             list in this test in the same change.",
            path.display(),
            e.kind(),
        ),
    }
}

/// `ci.yml` with every comment line dropped.
///
/// The comments in the coverage job discuss `cargo llvm-cov` and
/// `continue-on-error` at length — deliberately, since the reasoning is the point
/// — so a naive `contains` would keep passing after the *actual* step was
/// deleted, and this guard would be decorative. Fault injection caught exactly
/// that: replacing the run step with `cargo test` left the test green.
fn ci_without_comments() -> Option<String> {
    Some(
        repo_file(".github/workflows/ci.yml")?
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

#[test]
fn ci_measures_coverage_without_gating_it() {
    let Some(ci) = ci_without_comments() else {
        return; // not a source checkout
    };

    assert!(
        ci.contains("cargo llvm-cov"),
        "CI must still measure coverage: the docs say it does, and the whole \
         point of #319 is that the docs stopped being true. If coverage \
         measurement is being removed, remove the claim in \
         docs/history/BUILD_PLAN.md §11 in the same change."
    );
    assert!(
        ci.contains("continue-on-error: true"),
        "the coverage job must stay non-blocking while the docs say the 85% \
         floor is NOT enforced. Making it blocking is fine — but rewrite \
         docs/history/BUILD_PLAN.md §11 in the same change, or the docs go back to \
         describing a pipeline that does not exist."
    );
}

#[test]
fn no_document_claims_the_coverage_floor_is_enforced() {
    // The exact sentences that were false, plus the shapes they are most likely
    // to come back as. Each is only a lie while the job carries
    // `continue-on-error`.
    //
    // These match the *assertion* form, not the phrase: §11 now quotes the old
    // wording while explaining that it was untrue, and a needle that fired on the
    // quotation would make the honest correction unwritable.
    const FALSE_CLAIMS: &[&str] = &[
        "**Coverage ratchet:** `cargo-llvm-cov` in CI, 85% per-file floor",
        "85% per-file coverage ratchet, clippy",
        "Coverage ratchet held at 85%",
        "| Coverage | 85% per-file ratchet |",
    ];
    const DOCS: &[&str] = &[
        "docs/history/BUILD_PLAN.md",
        "docs/history/BUILD_PLAN_V2.md",
        "docs/adr/0001-build-roteiro-unified-codebase-knowledge-graph.md",
    ];

    for doc in DOCS {
        let Some(text) = repo_file(doc) else { continue };
        for claim in FALSE_CLAIMS {
            assert!(
                !text.contains(claim),
                "{doc} states {claim:?}, which asserts an enforced coverage gate. \
                 CI measures coverage and does not enforce a floor — say that, or \
                 make the gate real first."
            );
        }
    }
}

#[test]
fn the_build_plan_records_what_is_actually_enforced() {
    let Some(plan) = repo_file("docs/history/BUILD_PLAN.md") else {
        return;
    };
    // The corrected §11 must name the real gates, so a reader comparing the doc
    // to ci.yml finds them equal rather than nearly equal.
    for gate in [
        "cargo fmt --check",
        "cargo clippy",
        "cargo test --workspace",
        "roteiro check",
        "cargo audit",
        "cargo deny",
    ] {
        assert!(
            plan.contains(gate),
            "docs/history/BUILD_PLAN.md must list `{gate}` among the enforcing gates"
        );
    }
    assert!(
        plan.contains("87.51%"),
        "the measured workspace baseline belongs in the plan — it is the input \
         to deciding whether to switch a floor on"
    );
}

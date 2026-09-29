//! Guard: a CI cell that runs tests runs **all** of them, and says which it ran
//! (issue #906).
//!
//! Plain `cargo test` stops at the first failing test binary. Every later binary
//! is then never run — and cargo says nothing about it, because from cargo's
//! point of view stopping early is the feature. The job reports a failure, which
//! reads like honest reporting, but the set of tests that actually executed is
//! smaller than the set the cell appears to cover and **nothing anywhere says
//! so**.
//!
//! That is what happened here. The `--all-features` cell ran without
//! `--no-fail-fast` while a fixed-address flake in `serve_mode_selection_cli`
//! failed it four runs out of six, so the crates after `roteiro` were repeatedly
//! not run at all. Workers noticed the effect — `-p`-scoped runs had to be added
//! beside the full cell — without ever establishing the cause, because the cause
//! is an absence.
//!
//! Same family as #822 (a gate that never ran) and #854 (a gate covering one
//! axis reading as coverage across all of them), arriving through a third
//! mechanism: a gate that ran, covered less than it claimed, and truncated in
//! silence.
//!
//! # Why a guard rather than the fix alone
//!
//! Because the flag is one token and the failure is invisible. Five of the seven
//! test cells already carried `--no-fail-fast`; the two that did not were the
//! two nobody had had to think about since they were written. A fix that is not
//! held in place is a fix that lasts until the next cell is added by copying an
//! older one.
//!
//! The scan covers **every workflow file**, not `ci.yml` alone, so a test cell
//! added to a new workflow is covered from the commit that adds it.
//!
//! Fault injection: remove `--no-fail-fast` from any `cargo test` in
//! `.github/workflows/`, or delete the reporting step from the `--all-features`
//! cell, and the matching test below names it.

mod common;

/// Cargo subcommands that RUN tests, as opposed to merely compiling them.
///
/// `llvm-cov` is here because it is `cargo test` with instrumentation: it stops
/// at the first failing binary for the same reason and with a worse consequence,
/// since the number it then prints is a coverage figure measured over a
/// truncated run.
const RUNNING_SUBCOMMANDS: &[&str] = &["test", "llvm-cov"];

/// A cargo invocation that runs tests, and where it was found.
struct Cell {
    workflow: String,
    line: String,
}

/// Every workflow file in `.github/workflows/`, as `(name, contents)`.
///
/// Read from the directory rather than from a list, because a list is the thing
/// this guard exists to avoid: the two cells that lacked the flag were the two
/// nobody had enumerated.
fn workflows() -> Option<Vec<(String, String)>> {
    let dir = common::repo_root().join(".github/workflows");
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !common::is_repository_checkout() => {
            return None; // a packaged crate carries no `.github`
        }
        Err(e) => panic!("cannot read {} ({:?}: {e})", dir.display(), e.kind()),
    };
    let mut out = Vec::new();
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.extension().is_none_or(|e| e != "yml" && e != "yaml") {
            continue;
        }
        let name = path
            .file_name()
            .expect("file name")
            .to_string_lossy()
            .into_owned();
        out.push((name, std::fs::read_to_string(&path).expect("read workflow")));
    }
    assert!(
        !out.is_empty(),
        "no workflow files were found in {}. This guard reads the cells out of \
         that directory, so an empty read is a green that means \"could not \
         look\" — the exact shape of failure #906 is about.",
        dir.display()
    );
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Some(out)
}

/// Every test-running cargo invocation across all workflows.
///
/// Textual, deliberately. The alternative is walking the parsed YAML to find
/// `run:` scripts, which the sibling guards do — but this property is about the
/// **command line**, and a command line reached by any route (a `run:` block, a
/// composite step, a `with: args:`) truncates just the same. Comment lines are
/// dropped first: `ci.yml` discusses `--no-fail-fast` at length in prose, and a
/// scan that read those would pass on a file that had lost the flag.
fn cells() -> Option<Vec<Cell>> {
    let mut out = Vec::new();
    for (workflow, text) in workflows()? {
        // `\`-continued lines are one command; `cargo llvm-cov` carries its flags
        // over three lines in the coverage job.
        for line in text.replace("\\\n", " ").lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            let Some(rest) = line.split_once("cargo ") else {
                continue;
            };
            let Some(subcommand) = rest.1.split_whitespace().next() else {
                continue;
            };
            if !RUNNING_SUBCOMMANDS.contains(&subcommand) {
                continue;
            }
            // `--no-run` compiles and runs nothing, so it cannot truncate. The
            // `--all-features` cell uses one deliberately, to record which test
            // binaries exist before running them.
            if line.contains("--no-run") {
                continue;
            }
            out.push(Cell {
                workflow: workflow.clone(),
                line: line.to_owned(),
            });
        }
    }
    assert!(
        !out.is_empty(),
        "no test-running cargo invocation was found in any workflow. Either CI \
         stopped running tests entirely (it did not) or this scan no longer \
         matches how they are spelled, and a guard that matches nothing is a \
         guard that passes for the wrong reason."
    );
    Some(out)
}

/// Every cell that runs tests runs them all.
#[test]
fn every_test_cell_carries_no_fail_fast() {
    let Some(cells) = cells() else {
        return; // not a source checkout
    };

    let missing: Vec<&Cell> = cells
        .iter()
        .filter(|c| !c.line.contains("--no-fail-fast"))
        .collect();
    assert!(
        missing.is_empty(),
        "these CI cells run tests without `--no-fail-fast`, so they stop at the \
         first failing test binary and every later one is NEVER RUN — while the \
         job reports a failure that looks like it covered the whole workspace:\n\
         {}\n\nThat is issue #906. The cost of the flag is that a genuinely \
         broken commit runs the whole suite instead of stopping early; the cost \
         of not having it is a red run whose actual coverage nobody can state.",
        missing
            .iter()
            .map(|c| format!("  {} : {}", c.workflow, c.line))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// The `--all-features` cell states which test binaries it actually ran.
///
/// `--no-fail-fast` stops the truncation; this is what makes a truncation
/// *visible* if it happens by some other route — a killed binary, an OOM, a
/// harness that exits before its last target. The script compares the binaries
/// cargo built against the ones the log shows it started, which a check reading
/// only the log cannot do: a fail-fast truncation leaves the log internally
/// consistent, every `Running` line in it followed by a `test result:`.
#[test]
fn the_all_features_cell_reports_which_binaries_it_ran() {
    const SCRIPT: &str = "scripts/ci-test-cell-report.py";

    let Some(ci) = common::repo_file(".github/workflows/ci.yml") else {
        return; // not a source checkout
    };
    let uncommented: String = ci
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        uncommented.contains(SCRIPT),
        "no step in ci.yml runs `{SCRIPT}`, so the `--all-features` cell no \
         longer says which test binaries it ran. A cell that cannot state its \
         own coverage is one whose failures read as covering everything — see \
         issue #906. If the reporting moved somewhere else, point this guard at \
         it in the same change."
    );
    assert!(
        common::repo_file(SCRIPT).is_some(),
        "ci.yml runs `{SCRIPT}` and that file is not in the repository, so the \
         step fails on every run for a reason that has nothing to do with the \
         tests."
    );
}

//! End-to-end `roteiro lint`: it reports, and it stores **nothing**.
//!
//! The invariant these guard is a negative one, and negatives are the kind of
//! claim that quietly stops being true. So the store is not merely inspected —
//! it is first made **non-empty** by ingesting a real analyzer report, then
//! snapshotted, then compared byte-for-byte after a lint run. Comparing two
//! empty listings would pass whatever `lint` did, which is the vacuous shape of
//! test this repository has been bitten by; the ingest is what gives the
//! comparison something to lose. It is the same move Stage 28's
//! byte-identical-artifact tests make for the graph.
//!
//! The other half is the failure path: a linter that is not installed must be an
//! **error naming what to install**, never an empty report. Both paths assert
//! the store is untouched, because "nothing was written" has to hold when the
//! run fails as much as when it succeeds.

#![cfg(feature = "execution")]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_roteiro");

/// A throwaway git repository that is also a one-crate cargo workspace, with a
/// real analyzer report ingested so the findings tables are not empty.
struct Fixture {
    repo: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let repo = std::env::temp_dir().join(format!("roteiro-lint-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&repo).ok();
        std::fs::create_dir_all(repo.join("src")).expect("mkdir");
        std::fs::create_dir_all(repo.join(".home")).expect("mkdir");
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "test@example.invalid"]);
        git(&repo, &["config", "user.name", "Test"]);

        // A crate with one lint in it, and no dependencies — so `cargo clippy`
        // needs no registry, no network and about a second.
        std::fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"lintfix\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\
             \n[features]\nextra = []\n",
        )
        .expect("write");
        std::fs::write(
            repo.join("src/lib.rs"),
            "//! A crate with a lint in it.\npub fn count(v: &Vec<i32>) -> usize {\n    v.len()\n}\n",
        )
        .expect("write");
        std::fs::write(
            repo.join("audit.json"),
            std::fs::read(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../rto-exec/tests/fixtures/native/cargo-audit.json"),
            )
            .expect("read the cargo-audit capture"),
        )
        .expect("write");
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "init"]);

        let fixture = Self { repo };
        // The control: without this the findings tables are empty, and an
        // "unchanged" assertion over them would hold however much `lint` wrote.
        let ingest = fixture.roteiro(
            &[
                "security",
                "ingest",
                "audit.json",
                "--analyzer",
                "cargo-audit",
                "--json",
            ],
            &[],
        );
        assert!(ingest.status.success(), "ingest failed: {ingest:?}");
        fixture
    }

    /// Run the CLI in the fixture, optionally overriding environment variables.
    fn roteiro(&self, args: &[&str], env: &[(&str, &Path)]) -> std::process::Output {
        let mut command = Command::new(BIN);
        command
            .args(args)
            .current_dir(&self.repo)
            .env("ROTEIRO_HOME", self.repo.join(".home"))
            .env("HOME", self.repo.join(".home"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in env {
            command.env(key, value);
        }
        command.output().expect("run roteiro")
    }

    /// Everything the store will say about its findings, as bytes.
    fn findings_listing(&self) -> Vec<u8> {
        let out = self.roteiro(&["security", "list", "--json"], &[]);
        assert!(out.status.success(), "list failed: {out:?}");
        out.stdout
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.repo).ok();
    }
}

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("run git");
    assert!(out.status.success(), "git {args:?} failed: {out:?}");
}

/// Whether a failed lint run failed because this machine has no linter, as
/// opposed to because something is wrong.
///
/// A toolchain without the clippy component is a legitimate environment, and the
/// *store* assertions still hold there — so those tests check what they can and
/// say what they skipped, rather than failing on somebody's laptop. The error
/// wording itself is asserted in [`a_missing_toolchain_is_an_error_that_names_what_to_install`].
fn linter_is_absent(stderr: &str) -> bool {
    stderr.contains("rustup component add clippy") || stderr.contains("not found on PATH")
}

/// The headline invariant: a lint run leaves the findings tables byte-identical.
#[test]
fn a_lint_run_leaves_the_findings_tables_byte_identical() {
    let fixture = Fixture::new("nothing-stored");
    let before = fixture.findings_listing();
    assert!(
        String::from_utf8_lossy(&before).contains("cargo-audit"),
        "the control must be non-empty, or this test cannot fail"
    );

    let lint = fixture.roteiro(&["lint", "clippy", "--json"], &[]);
    let after = fixture.findings_listing();
    assert_eq!(
        String::from_utf8_lossy(&before),
        String::from_utf8_lossy(&after),
        "`roteiro lint` must leave the findings store byte-identical"
    );

    let stderr = String::from_utf8_lossy(&lint.stderr).into_owned();
    if !lint.status.success() {
        assert!(
            linter_is_absent(&stderr),
            "lint failed unexpectedly: {stderr}"
        );
        return;
    }
    let report: serde_json::Value = serde_json::from_slice(&lint.stdout).expect("lint emits JSON");
    assert_eq!(
        report["stored"], false,
        "the report must say it stored nothing"
    );
    assert_eq!(report["analyzer"], "clippy");
    assert_eq!(
        report["counts"]["reported"].as_u64().expect("a count"),
        report["findings"].as_array().expect("findings").len() as u64
    );
    // `clippy` must never become a layer, whatever else happened.
    assert!(
        !String::from_utf8_lossy(&after).contains("clippy"),
        "a clippy layer appeared in the store: {}",
        String::from_utf8_lossy(&after)
    );
}

/// Requirement 2: with no `AnalysisRun` to carry the evidence, the report has to.
#[test]
fn the_report_says_what_produced_it() {
    let fixture = Fixture::new("evidence");
    let lint = fixture.roteiro(&["lint", "clippy", "--all-features", "--json"], &[]);
    let stderr = String::from_utf8_lossy(&lint.stderr).into_owned();
    if !lint.status.success() {
        assert!(
            linter_is_absent(&stderr),
            "lint failed unexpectedly: {stderr}"
        );
        return;
    }
    let report: serde_json::Value = serde_json::from_slice(&lint.stdout).expect("lint emits JSON");

    assert!(
        report["analyzer_version"]
            .as_str()
            .is_some_and(|v| !v.is_empty()),
        "the linter's version"
    );
    for field in ["linter", "rustc", "host"] {
        assert!(
            report["toolchain"][field]
                .as_str()
                .is_some_and(|v| !v.is_empty()),
            "the toolchain must name its {field}: {report}"
        );
    }
    assert!(
        report["features"]
            .as_str()
            .expect("a feature set")
            .contains("all-features"),
        "the feature set actually used must be named: {report}"
    );
    assert_eq!(
        report["isolation"], "none",
        "and the boundary it really had"
    );
    // Whether the build completed, so a partial result is never read as a small
    // one. This command exits 0 either way — it reports, it does not gate.
    assert!(
        report["build_succeeded"].is_boolean(),
        "the report must say whether the build completed: {report}"
    );
    let command: Vec<String> = serde_json::from_value(report["command"].clone()).expect("argv");
    assert_eq!(command.first().map(String::as_str), Some("cargo"));
    assert!(
        command.contains(&"--all-features".to_owned()),
        "{command:?}"
    );
    // The three readings a lint count carries, in the payload rather than only
    // in the prose — a scripted consumer is told what a person is told.
    assert_eq!(
        report["caveats"].as_array().map(Vec::len),
        Some(3),
        "{report}"
    );
}

/// Requirement 3: an absent linter is a named failure, never an empty report.
///
/// Fault-injected by emptying `PATH` for the child, which is the same thing a
/// machine without a Rust toolchain has. An earlier version of this surface
/// would have reported "0 diagnostic(s)" here — the vacuous zero.
#[test]
fn a_missing_toolchain_is_an_error_that_names_what_to_install() {
    let fixture = Fixture::new("no-toolchain");
    let before = fixture.findings_listing();
    let empty = fixture.repo.join(".empty-path");
    std::fs::create_dir_all(&empty).expect("mkdir");

    let lint = fixture.roteiro(&["lint", "clippy"], &[("PATH", empty.as_path())]);
    assert!(
        !lint.status.success(),
        "a missing toolchain must fail: {lint:?}"
    );
    let stderr = String::from_utf8_lossy(&lint.stderr);
    assert!(stderr.contains("not found on PATH"), "{stderr}");
    assert!(stderr.contains("https://rustup.rs"), "{stderr}");
    assert!(
        stderr.contains("must never read as a clean tree"),
        "the refusal must say why it is not a result: {stderr}"
    );
    assert!(
        !String::from_utf8_lossy(&lint.stdout).contains("diagnostic(s)"),
        "nothing may be reported: {}",
        String::from_utf8_lossy(&lint.stdout)
    );
    assert_eq!(
        String::from_utf8_lossy(&before),
        String::from_utf8_lossy(&fixture.findings_listing()),
        "a failed lint must leave the store byte-identical too"
    );
}

/// `lint` and `security` are different commands over different stores, and the
/// refusal says so rather than listing names a user would then try.
#[test]
fn a_storing_analyzer_is_refused_and_the_disclosure_does_not_lie_about_it() {
    let fixture = Fixture::new("wrong-analyzer");
    let out = fixture.roteiro(&["lint", "semgrep"], &[]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("roteiro security run"), "{stderr}");
    // The pre-run disclosure names the argv that is about to execute. For an
    // analyzer with no argv there must be no line at all — announcing clippy's
    // command under semgrep's name would be a lie in the one place the user is
    // relying on being told the truth.
    assert!(
        !stderr.contains("running semgrep"),
        "no run was disclosed for an analyzer that cannot run: {stderr}"
    );
}

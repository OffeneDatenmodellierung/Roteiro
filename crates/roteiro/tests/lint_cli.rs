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
        // A committed lockfile, because `roteiro lint` passes `--locked` — it
        // must not write `Cargo.lock` into the tree it is reporting on. Cargo
        // generates it here rather than this file spelling one out, so the
        // fixture does not go stale against a lockfile format bump.
        let lock = Command::new("cargo")
            .args(["generate-lockfile"])
            .current_dir(&repo)
            .output()
            .expect("run cargo");
        assert!(lock.status.success(), "generate-lockfile failed: {lock:?}");

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

    /// Write (or remove) the **project** layer, `roteiro.toml`. Committed and
    /// shared — the file ADR-0020 §6 says may deny host execution and never
    /// grant it.
    fn project_layer(&self, allow_unsandboxed: Option<bool>) {
        write_layer(&self.repo.join("roteiro.toml"), allow_unsandboxed);
    }

    /// Write (or remove) the **user** layer — the one that may grant.
    ///
    /// `$ROTEIRO_HOME/config.toml`, which the fixture points at its own
    /// directory, rather than the literal `~/.roteiro/config.toml` the messages
    /// name: that is where the binary looks when `ROTEIRO_HOME` is set, and a
    /// test that wrote to the other path would silently exercise an absent
    /// layer — which is exactly how this test first passed for the wrong reason.
    fn user_layer(&self, allow_unsandboxed: Option<bool>) {
        let dir = self.repo.join(".home");
        std::fs::create_dir_all(&dir).expect("mkdir");
        write_layer(&dir.join("config.toml"), allow_unsandboxed);
    }

    /// Whether `cargo` ever ran in this fixture.
    ///
    /// The load-bearing assertion for "nothing fell back to the host": a refusal
    /// that still compiled the tree would leave `target/` behind, and no amount
    /// of reading the exit status would notice. Checking the *absence of a side
    /// effect* is the only way to tell a refusal from a quiet run.
    fn built_anything(&self) -> bool {
        self.repo.join("target").exists()
    }

    /// Run the CLI in the fixture, optionally overriding environment variables.
    ///
    /// `HOME` is the fixture's own, so the user config layer is the fixture's
    /// too — but `CARGO_HOME`/`RUSTUP_HOME` are inherited from the real
    /// environment, because rustup finds its toolchains through `HOME` and a
    /// fixture home has none. Without them a granted run fails for a reason that
    /// has nothing to do with what is being tested.
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
        for key in ["CARGO_HOME", "RUSTUP_HOME"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        // **Unset, always.** `roteiro lint` must put the build somewhere outside
        // the worktree by its own doing, and the defect that made this necessary
        // was a build directory that landed outside the tree only when the
        // developer's shell had already arranged it. Inheriting whatever this
        // test process happens to have would make the suite pass or fail
        // according to who ran it, and pass for the wrong reason on CI.
        command.env_remove("CARGO_TARGET_DIR");
        for (key, value) in env {
            command.env(key, value);
        }
        command.output().expect("run roteiro")
    }

    /// Every path in the working tree with a digest of its contents, sorted —
    /// the evidence for "nothing was written here".
    ///
    /// The whole tree rather than `target/` alone: a build writes more than one
    /// thing, and a test that named only the directory it knew about would keep
    /// passing while the next write went somewhere else. `.git` is skipped
    /// because git's own bookkeeping is not what is under test.
    ///
    /// **And contents rather than names alone.** An earlier version of this
    /// recorded paths only, which made it blind to an in-place modification of a
    /// file that already existed — and `Cargo.lock` is exactly that: cargo
    /// rewrites it where it sits, so the listing is identical before and after.
    /// Since stopping that rewrite is half of what this PR does (`--locked` is
    /// the other half of the guarantee `CARGO_TARGET_DIR` gives), a snapshot
    /// that could not see it was a test weaker than its own doc comment — which
    /// is the shape of defect this suite exists to catch, not a nit. Reported on
    /// #443.
    ///
    /// The digest is `DefaultHasher`, not a cryptographic one: it is compared
    /// against another snapshot taken by the same process moments earlier, so
    /// nothing here depends on the value being stable across runs or resistant
    /// to anyone. It keeps the failure output to one short line per entry.
    fn tree_snapshot(&self) -> Vec<String> {
        fn digest(path: &Path) -> String {
            use std::hash::{Hash, Hasher};
            match std::fs::read(path) {
                Ok(bytes) => {
                    let mut hasher = std::collections::hash_map::DefaultHasher::new();
                    bytes.hash(&mut hasher);
                    format!("{:016x}", hasher.finish())
                }
                // A path that cannot be read still has to be *some* value, and
                // one that differs from every readable file — an unreadable file
                // silently comparing equal to a readable one is the failure this
                // whole function is guarding against.
                Err(err) => format!("unreadable: {}", err.kind()),
            }
        }
        fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.file_name().is_some_and(|n| n == ".git") {
                    continue;
                }
                let rel = path
                    .strip_prefix(base)
                    .unwrap_or(&path)
                    .display()
                    .to_string();
                if path.is_dir() {
                    out.push(format!("{rel}/"));
                    walk(&path, base, out);
                } else {
                    out.push(format!("{rel}  {}", digest(&path)));
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.repo, &self.repo, &mut out);
        out.sort();
        out
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

/// Write a config layer holding just `[lint] allow_unsandboxed`, or remove the
/// file when the key is absent — so "this layer says nothing" is a state a test
/// can ask for rather than one it has to arrange by not calling anything.
fn write_layer(path: &Path, allow_unsandboxed: Option<bool>) {
    match allow_unsandboxed {
        Some(value) => {
            std::fs::write(path, format!("[lint]\nallow_unsandboxed = {value}\n")).expect("write");
        }
        None => {
            std::fs::remove_file(path).ok();
        }
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

/// The other headline invariant, and the one this module's documentation
/// claimed for a release without providing: a lint run leaves the **tree** alone.
///
/// `roteiro lint clippy` is a build. A build writes, and cargo's defaults write
/// `<worktree>/target` and `<worktree>/Cargo.lock` — into the tree under review,
/// which for the case this command exists for is a branch somebody else wrote.
/// The module said it did not do that. What actually stopped it was
/// `CARGO_TARGET_DIR` being set *in the invoking shell*: the code listed the
/// variable as one to inherit, and inheriting a name the parent does not have
/// configures nothing at all.
///
/// So this runs the shipped path with `CARGO_TARGET_DIR` unset — see
/// [`Fixture::roteiro`], which unsets it for every test here — and asserts the
/// tree is untouched. It fails if the fix is reverted, which is the only reason
/// it is worth having.
#[test]
fn a_lint_run_writes_nothing_into_the_tree_it_is_linting() {
    let fixture = Fixture::new("read-only-source");

    // Roteiro's state goes somewhere outside the fixture, so that "outside the
    // worktree" is a claim this test can actually see. With the default
    // `$repo/.home` the scratch directory would satisfy the assertions below
    // while still sitting inside the tree, which would pass for a reason that is
    // not the property being tested.
    let state = std::env::temp_dir().join(format!("roteiro-lint-state-{}", std::process::id()));
    std::fs::remove_dir_all(&state).ok();
    std::fs::create_dir_all(&state).expect("mkdir");

    let before = fixture.tree_snapshot();
    let names = |snapshot: &[String]| -> Vec<String> {
        snapshot
            .iter()
            .map(|e| e.split("  ").next().unwrap_or(e).to_owned())
            .collect()
    };
    assert!(
        names(&before).contains(&"Cargo.lock".to_owned())
            && names(&before).contains(&"src/".to_owned()),
        "the snapshot must actually see the tree: {before:?}"
    );
    // The snapshot has to carry contents, or it cannot see the `Cargo.lock`
    // rewrite that `--locked` exists to stop — see `tree_snapshot`.
    assert!(
        before.iter().any(|e| e.starts_with("Cargo.lock  ")
            && e.split("  ").nth(1).is_some_and(|d| d.len() == 16)),
        "the snapshot records names without contents, so an in-place edit would \
         leave it identical: {before:?}"
    );

    let lint = fixture.roteiro(
        &["lint", "clippy", "--allow-unsandboxed", "--json"],
        &[("ROTEIRO_HOME", &state)],
    );
    let stderr = String::from_utf8_lossy(&lint.stderr).into_owned();
    if !lint.status.success() {
        assert!(
            linter_is_absent(&stderr),
            "lint failed unexpectedly: {stderr}"
        );
        std::fs::remove_dir_all(&state).ok();
        return;
    }
    let report: serde_json::Value = serde_json::from_slice(&lint.stdout).expect("lint emits JSON");

    // The assertion, named: cargo's default target directory is `target/` under
    // the tree it builds, and it must not be there.
    assert!(
        !fixture.built_anything(),
        "`target/` was written into the tree being linted"
    );
    // And the general form, which also covers `Cargo.lock` and anything a future
    // cargo decides to drop next to it.
    assert_eq!(
        before,
        fixture.tree_snapshot(),
        "`roteiro lint` wrote into the tree it was reporting on"
    );

    // None of the above means anything if nothing was built. A clippy that never
    // ran writes nothing either, and would satisfy every assertion so far — this
    // is what separates the fix from a no-op.
    assert_eq!(report["build_succeeded"], true, "{report}");
    assert!(
        report["counts"]["reported"].as_u64().expect("a count") > 0,
        "the fixture crate has a lint in it; a run reporting none did not compile it: {report}"
    );

    // So the writes went somewhere, and the report says where — which is how
    // roteiro choosing the directory over the caller stays a disclosure rather
    // than a surprise.
    let scratch = PathBuf::from(report["scratch"].as_str().expect("a scratch path"));
    assert!(
        scratch.starts_with(&state),
        "the build directory {} is not under the state root {}",
        scratch.display(),
        state.display()
    );
    assert!(
        !scratch.starts_with(&fixture.repo),
        "the build directory is inside the worktree: {}",
        scratch.display()
    );
    assert!(
        scratch.join("debug").exists(),
        "nothing was built into {} — the build went somewhere this test cannot see",
        scratch.display()
    );

    std::fs::remove_dir_all(&state).ok();
}

/// The half of the guarantee `CARGO_TARGET_DIR` does not cover: cargo rewrites
/// `Cargo.lock` **in the tree**, and `--locked` is what stops it.
///
/// The stale state is produced by bumping the package's own version, so the
/// lockfile disagrees with the manifest over a purely local fact. That needs no
/// registry, no network and no cache — a fixture that added a dependency would
/// be asserting something about this machine's `CARGO_HOME` as much as about
/// roteiro.
///
/// **The tree assertion comes first, deliberately.** It is the one that has to
/// fail if `--locked` is reverted, and putting the exit-status assertions ahead
/// of it would let a weaker snapshot hide behind them: the test would go red for
/// the status, and nobody would learn whether the snapshot could see the write.
/// It also stands on its own — a refusal that rewrote the lockfile on its way to
/// refusing would be worth nothing.
#[test]
fn a_stale_lockfile_is_refused_and_the_tree_is_left_alone() {
    let fixture = Fixture::new("stale-lockfile");
    // Same manifest, one version bump: `Cargo.lock` still records 0.0.0.
    std::fs::write(
        fixture.repo.join("Cargo.toml"),
        "[package]\nname = \"lintfix\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\
         \n[features]\nextra = []\n",
    )
    .expect("write");

    let before = fixture.tree_snapshot();
    let lint = fixture.roteiro(&["lint", "clippy", "--allow-unsandboxed"], &[]);
    let stderr = String::from_utf8_lossy(&lint.stderr).into_owned();
    if linter_is_absent(&stderr) {
        return;
    }

    // First: nothing in the tree changed. Without `--locked` cargo rewrites
    // `Cargo.lock` in place — same path, different bytes — which only a
    // snapshot carrying contents can see.
    assert_eq!(
        before,
        fixture.tree_snapshot(),
        "`roteiro lint` wrote into the tree it was reporting on"
    );

    assert!(
        !lint.status.success(),
        "a stale lockfile must refuse: {lint:?}"
    );
    assert!(
        stderr.contains("--locked") && stderr.contains("generate-lockfile"),
        "the refusal must name the flag and the remedy: {stderr}"
    );
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

    let lint = fixture.roteiro(&["lint", "clippy", "--allow-unsandboxed", "--json"], &[]);
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
    let lint = fixture.roteiro(
        &[
            "lint",
            "clippy",
            "--allow-unsandboxed",
            "--all-features",
            "--json",
        ],
        &[],
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
        Some(4),
        "{report}"
    );
    // A host run carries no image, and the field's absence is the claim: there
    // was no boundary to name. `isolation: none` above says the same thing, and
    // the two must not be able to disagree.
    assert!(
        report.get("image").is_none(),
        "a host run must not name an image it did not have: {report}"
    );
    assert!(
        !command.contains(&"--offline".to_owned()),
        "the host path is unchanged by conditions 1-2: {command:?}"
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

    let lint = fixture.roteiro(
        &["lint", "clippy", "--allow-unsandboxed"],
        &[("PATH", empty.as_path())],
    );
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
    let out = fixture.roteiro(&["lint", "semgrep", "--allow-unsandboxed"], &[]);
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

/// **The inversion, end to end.** With nothing configured and no flag,
/// `roteiro lint` selects the sandbox — and with no image supplied there is no
/// sandbox to be had, so it refuses and **runs nothing** (ADR-0020 §6).
///
/// Before v1.3 this compiled the tree on the host. The assertion that carries
/// the weight is [`Fixture::built_anything`]: an exit status alone cannot tell a
/// refusal from a run that happened and then complained, and the failure this
/// test exists to catch is a selected boundary quietly becoming a host run.
#[test]
fn by_default_it_selects_the_sandbox_and_never_falls_back_to_the_host() {
    let fixture = Fixture::new("default-refuses");
    let before = fixture.findings_listing();
    assert!(!fixture.built_anything(), "the fixture starts unbuilt");

    let out = fixture.roteiro(&["lint", "clippy"], &[]);
    assert!(
        !out.status.success(),
        "a sandbox that cannot be had must refuse: {out:?}"
    );
    assert!(
        !fixture.built_anything(),
        "a refusal compiled the tree — nothing may fall back to the host"
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    // What was selected, and that it was not run here.
    assert!(stderr.contains("sandboxed"), "{stderr}");
    assert!(
        stderr.contains("nothing fell back to this host"),
        "the one promise this refusal exists to keep: {stderr}"
    );
    // Why it could not be had, and how to fix *that* — which is the way forward
    // a person actually wants, rather than only the way around it.
    assert!(stderr.contains("No image is configured"), "{stderr}");
    assert!(stderr.contains("[lint]"), "{stderr}");
    assert!(stderr.contains("@sha256:"), "{stderr}");
    assert!(stderr.contains("docs/SANDBOXED_LINTING.md"), "{stderr}");
    // And the way around it, both forms, with the sentence that stops them
    // reading as two steps.
    assert!(stderr.contains("--allow-unsandboxed"), "{stderr}");
    assert!(stderr.contains("allow_unsandboxed = true"), "{stderr}");
    assert!(stderr.contains("~/.roteiro/config.toml"), "{stderr}");
    assert!(
        stderr.contains("cannot grant"),
        "and that the committed file is not the place: {stderr}"
    );
    assert!(
        stderr.contains("do not need both"),
        "and that either remedy suffices — otherwise the config key reads as a \
         second step and nobody stops typing the flag: {stderr}"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "a refusal reports nothing at all"
    );
    assert_eq!(
        String::from_utf8_lossy(&before),
        String::from_utf8_lossy(&fixture.findings_listing()),
        "and it still writes nothing to the store"
    );
}

/// A **tag** is refused, wherever the image came from.
///
/// The image is the boundary — the container somebody else's build scripts
/// execute in — and a tag is a mutable pointer to it. This is the one check that
/// cannot be deferred to "it will fail later": a tag *works*, right up until the
/// day somebody replaces what is behind it, and the run reports success either
/// way.
#[test]
fn an_image_pinned_by_tag_is_refused_and_says_how_to_pin_it() {
    let fixture = Fixture::new("tagged-image");
    let out = fixture.roteiro(
        &["lint", "clippy", "--image", "docker.io/you/rust:1.97.1"],
        &[],
    );
    assert!(!out.status.success(), "a tag must be refused: {out:?}");
    assert!(!fixture.built_anything(), "and nothing may have been built");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("tag rather than a digest"), "{stderr}");
    assert!(
        stderr.contains("@sha256:"),
        "the refusal must show the shape it wants: {stderr}"
    );
    assert!(
        stderr.contains("imagetools inspect"),
        "and how to obtain it: {stderr}"
    );
}

/// An image that is not in the local store refuses, and **names the command that
/// provisions it** — a run never pulls.
///
/// The same rule every other pinned input follows: provisioning downloads,
/// running reads. A lint that could pull would be a lint that can fail because a
/// registry was unreachable, or succeed by quietly fetching something new.
#[test]
fn an_unprovisioned_image_refuses_and_names_the_prefetch_that_obtains_it() {
    let fixture = Fixture::new("unprovisioned-image");
    // Digest-shaped and certainly absent, so the refusal under test is the
    // store's rather than the pin check's.
    let absent = format!("docker.io/you/rust-clippy@sha256:{}", "0".repeat(64));
    let out = fixture.roteiro(&["lint", "clippy", "--image", &absent], &[]);
    assert!(!out.status.success(), "{out:?}");
    assert!(!fixture.built_anything(), "and nothing may have been built");

    let stderr = String::from_utf8_lossy(&out.stderr);
    // In a build without `exec-boxlite` the refusal is the missing feature
    // instead, and that is equally correct — what must never happen is a host
    // run. Both branches are checked; only the wording differs.
    assert!(
        stderr.contains("roteiro security prefetch") || stderr.contains("exec-boxlite"),
        "a refusal must name what is missing: {stderr}"
    );
    assert!(
        stderr.contains("nothing fell back to this host") || stderr.contains("Nothing ran"),
        "{stderr}"
    );
}

/// A **project** grant must not enable host execution — the row that makes the
/// layering real rather than documented. `roteiro.toml` is committed, so a
/// merged line would otherwise start running builds on every teammate's machine.
#[test]
fn a_committed_project_grant_does_not_enable_host_execution() {
    let fixture = Fixture::new("project-grant");
    fixture.project_layer(Some(true));

    let out = fixture.roteiro(&["lint", "clippy"], &[]);
    assert!(
        !out.status.success(),
        "a committed file may never grant host execution: {out:?}"
    );
    assert!(!fixture.built_anything(), "and nothing may have been built");

    // Ignored, but never silently: a team that wrote it needs telling.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("read and ignored"), "{stderr}");
    assert!(stderr.contains("roteiro.toml"), "{stderr}");
}

/// A **project deny** overrides a user grant. A repository that wants the
/// sandbox enforced gets it, and the person working in it is told to take it up
/// with the repository rather than sent to edit their own config uselessly.
#[test]
fn a_project_deny_overrides_a_user_grant() {
    let fixture = Fixture::new("project-deny");
    fixture.user_layer(Some(true));
    fixture.project_layer(Some(false));

    for args in [
        &["lint", "clippy"][..],
        &["lint", "clippy", "--allow-unsandboxed"][..],
    ] {
        let out = fixture.roteiro(args, &[]);
        assert!(
            !out.status.success(),
            "{args:?} must be refused by the project layer: {out:?}"
        );
        assert!(!fixture.built_anything(), "{args:?} built something");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("roteiro.toml"), "{args:?}: {stderr}");
        assert!(
            !stderr.contains("Pass `--allow-unsandboxed`"),
            "{args:?}: a remedy that would not work must not be offered: {stderr}"
        );
    }
}

/// The user layer grants on its own — no flag — which is the asymmetry with
/// ADR-0019 that makes the config key worth having.
#[test]
fn a_user_layer_grant_is_enough_on_its_own() {
    let fixture = Fixture::new("user-grant");
    fixture.user_layer(Some(true));

    let out = fixture.roteiro(&["lint", "clippy", "--json"], &[]);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    if !out.status.success() {
        assert!(
            linter_is_absent(&stderr),
            "lint failed unexpectedly: {stderr}"
        );
        return;
    }
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).expect("lint emits JSON");
    assert_eq!(
        report["stored"], false,
        "a grant changes who chose, not what is kept"
    );
    assert_eq!(
        report["isolation"], "none",
        "and it does not upgrade the isolation it reports"
    );
}

/// `--sandboxed` refuses rather than falling back, even over a standing grant.
/// Asking for isolation and getting execution is the one outcome ADR-0020 §6
/// forbids outright.
#[test]
fn asking_for_the_sandbox_refuses_and_never_falls_back_to_the_host() {
    let fixture = Fixture::new("sandboxed");
    fixture.user_layer(Some(true));

    let out = fixture.roteiro(&["lint", "clippy", "--sandboxed"], &[]);
    assert!(
        !out.status.success(),
        "no sandbox exists to honour it: {out:?}"
    );
    assert!(
        !fixture.built_anything(),
        "asking for isolation must never produce execution"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--sandboxed"), "{stderr}");
    assert!(
        stderr.contains("fell back") || stderr.contains("not produce"),
        "the refusal must say it did not downgrade: {stderr}"
    );
}

/// The report and the behaviour must not disagree, in the arrangement where they
/// most easily could: the project denies and the user grants.
///
/// Found by fault injection. Removing the project-deny short-circuit from
/// `as_effective` leaves the *gate* correct — it consults `project_denied`
/// first — while `roteiro config` starts echoing `Some(true)` over a run that
/// refuses. Every other test here passed. A report that contradicts the
/// behaviour is worse than no report, because ADR-0007's whole claim for this
/// command is that it answers "why did it do that?".
#[test]
fn the_reported_value_agrees_with_the_refusal_when_a_project_denies() {
    let fixture = Fixture::new("deny-echo");
    fixture.project_layer(Some(false));
    fixture.user_layer(Some(true));

    let refused = fixture.roteiro(&["lint", "clippy"], &[]);
    assert!(!refused.status.success(), "the project denied: {refused:?}");

    let out = fixture.roteiro(&["config"], &[]);
    assert!(out.status.success(), "config failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("allow_unsandboxed"))
        .unwrap_or_else(|| panic!("no allow_unsandboxed line in:\n{stdout}"));
    assert!(
        line.contains("allow_unsandboxed = Some(false)"),
        "the effective value must echo the denial that actually took effect, not \
         the user grant it overruled: {line}"
    );
    assert!(line.contains("project: Some(false)"), "{line}");
    assert!(line.contains("user: Some(true)"), "{line}");
}

/// ADR-0007's point: someone can ask "why did it do that?" and be answered.
/// `roteiro config` shows the key, both layers, and which one won.
#[test]
fn roteiro_config_shows_the_key_and_the_layer_that_decided_it() {
    let fixture = Fixture::new("config-report");
    fixture.project_layer(Some(true));
    fixture.user_layer(Some(false));

    let out = fixture.roteiro(&["config"], &[]);
    assert!(out.status.success(), "config failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let section: String = stdout
        .lines()
        .skip_while(|l| !l.starts_with("[lint]"))
        .take_while(|l| !l.starts_with("[debt]"))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(!section.is_empty(), "no [lint] section in:\n{stdout}");
    assert!(section.contains("allow_unsandboxed"), "{section}");
    // Per-layer, not just merged: the merged value alone cannot answer "why".
    assert!(section.contains("project: Some(true)"), "{section}");
    assert!(section.contains("user: Some(false)"), "{section}");
    assert!(
        section.contains("may deny, never grant"),
        "each layer must be labelled with what it is allowed to do: {section}"
    );
    assert!(
        section.contains("read and ignored"),
        "and the discarded project grant must be called out: {section}"
    );
    // The default is the whole story for this key, so the header states it.
    assert!(section.contains("sandboxed by default"), "{section}");
}

//! `roteiro lint` — run a linter over the tree in front of you, print what it
//! said, and keep none of it.
//!
//! This is a **reporting** surface, not an artifact one, and the difference is
//! the whole reason the module exists separately from [`crate::runner`]'s
//! backends. Nothing here can reach the findings store: there is no
//! [`rto_graph::AnalysisRun`], no layer key, no call to
//! [`rto_graph::Store::replace_findings_layer`], and the analyzer it drives is
//! deliberately absent from [`crate::adapter::ADAPTERS`], so `security ingest`
//! cannot file its output either. ADR-0020 v1.1 records the ruling: *"lint would
//! be local to the user running it, not an artifact we would store for later —
//! some of these tools are for the user to assess their own code at that point
//! in time."*
//!
//! # Why a lint is not a finding, in one paragraph
//!
//! An advisory id is **assigned**, and assignment is a promise:
//! `RUSTSEC-2020-0071` will mean the same thing in five years. A lint name is a
//! **symbol in a compiler**, renamed or removed at its discretion. The first is
//! a durable fact about the repository and earns a row in a store; the second is
//! a tool's opinion about the code as it stands today. Storing the second is
//! what produced every identity problem ADR-0020 catalogued — a layer key is
//! `<prefix>:<analyzer>:<worktree-id>` and `UNIQUE`, with `analyzer_version` in
//! neither the finding key nor the layer key, so two runs differing only in
//! toolchain or feature set would collide, replace each other, and report the
//! displaced findings as *removed*, which reads as **fixed**. Not storing
//! removes all of it.
//!
//! # It is not an [`AnalyzerRunner`], and that is not tidiness
//!
//! [`crate::check_request`] refuses any request whose worktree is not read-only,
//! and every backend runs it. A linter cannot honour that: `cargo clippy` writes
//! `target/`, which is inside the tree. The tempting move — relax the preflight
//! so a builder fits through it — is exactly the silent conversion ADR-0014
//! warns about, and ADR-0020's condition 1 forbids it (the relaxation belongs to
//! a sandboxed build directory that does not exist yet). So this module takes a
//! **different request shape** rather than a weakened one: no [`Consent`], no
//! [`rto_graph::CommandPolicy`], no claim of a read-only tree, and no
//! `AnalyzerRunner` impl to be handed to a caller that expects those promises.
//! `check_request` is untouched and still refuses everything it refused before.
//!
//! [`AnalyzerRunner`]: crate::AnalyzerRunner
//! [`Consent`]: crate::Consent
//!
//! # What isolation it has: none, and it says so
//!
//! The linter runs as a child process on this host. `cargo clippy` has `cargo
//! check` semantics, so it executes every build script in the resolved tree and
//! loads every proc macro as a dylib into the compiler — measured on this
//! repository at 54 build scripts and 7 proc macros by default, 87 and 33 under
//! `--all-features` (ADR-0020). For **your own tree** that is the code you were
//! going to build anyway, which is why this command does not gate on a consent
//! flag the way `security run --allow-unsandboxed` does. For **someone else's**
//! tree — reviewing an outside contributor's branch — it is that contributor's
//! code executing on your machine, and the sandboxed builder that would answer
//! it is unbuilt (ADR-0020 conditions 1–3). Every report says `isolation: none`
//! and every non-JSON run prints the argv before running it, so neither reading
//! depends on someone remembering this paragraph.
//!
//! @rto:0014
//! @rto:0020

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rto_graph::{Isolation, SourceIdentity};

use crate::adapter::clippy::{self, Clippy, FeatureSet, Summary};
use crate::adapter::{Invocation, NativeContext, UNKNOWN_VERSION};
use crate::clock::rfc3339_utc;
use crate::ingest::NormalizedReport;
use crate::runner::ExecError;
use crate::snippet::WorktreeSnippets;
use crate::subprocess::{SubprocessError, execute, scrub_environment, stderr_tail};

/// Every analyzer `roteiro lint` can run.
///
/// A separate list from [`crate::known_analyzers`], which answers a different
/// question — *what can be stored* — and would name `semgrep` and `cargo-audit`
/// here, sending a caller off to ask for a lint from an analyzer that files
/// layers.
pub const LINT_ANALYZERS: &[&str] = &[clippy::ANALYZER];

/// The environment variables a Rust toolchain needs to be found.
///
/// The scrubbed base ([`scrub_environment`]) keeps `PATH` and `HOME`, which is
/// all a parse-only analyzer needs. A toolchain driven through rustup needs
/// where its toolchains and its registry live, and a user who has moved either
/// would otherwise get a shim with nothing behind it. Each entry is a locator,
/// never a credential — `CARGO_REGISTRY_TOKEN` is conspicuously not here.
const TOOLCHAIN_ENV: &[&str] = &[
    "CARGO_HOME",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
    "CARGO_TARGET_DIR",
];

/// Something went wrong running the linter, or working out whether it could be
/// run at all.
///
/// Every variant that means *the tool is not here* names what to install. An
/// empty result that actually meant "the binary was missing" is the vacuous zero
/// this project has been bitten by repeatedly, and the way to not have it is for
/// absence to be an error rather than a number.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LintError {
    /// `roteiro lint` was asked for an analyzer it does not drive.
    #[error(
        "`{requested}` is not a linter roteiro can run (known: {known}). \
             For an analyzer whose findings are stored, use `roteiro security run`."
    )]
    UnknownAnalyzer {
        /// What was asked for.
        requested: String,
        /// What could have been asked for, comma-separated.
        known: String,
    },
    /// A toolchain program is not on `PATH`.
    #[error(
        "`{program}` was not found on PATH, so `{analyzer}` could not be run. Roteiro does not \
         install toolchains: install Rust (https://rustup.rs), or run the linter elsewhere. \
         Nothing is reported, because a missing tool must never read as a clean tree."
    )]
    ToolchainMissing {
        /// The program that was looked for.
        program: String,
        /// The linter it was needed for.
        analyzer: String,
    },
    /// The toolchain is present but the linter component is not installed.
    #[error(
        "`{analyzer}` is not installed for this toolchain: `{command}` failed. Install it with \
         `{install}`. Nothing is reported, because a missing linter must never read as a clean \
         tree.{stderr}"
    )]
    AnalyzerNotInstalled {
        /// The linter that is missing.
        analyzer: String,
        /// The probe that established it.
        command: String,
        /// The exact command that installs it.
        install: String,
        /// The tail of the probe's standard error, prefixed for display.
        stderr: String,
    },
    /// There is no cargo project at or above the directory the command ran in.
    #[error(
        "no cargo project found at or above {dir}: `{command}` failed, so there is nothing for \
         `{analyzer}` to lint.{stderr}"
    )]
    NoCargoProject {
        /// Where the search started.
        dir: String,
        /// The probe that failed.
        command: String,
        /// The linter that was asked for.
        analyzer: String,
        /// The tail of the probe's standard error.
        stderr: String,
    },
    /// A toolchain probe failed for some other reason.
    #[error("`{command}` failed while working out what would run.{stderr}")]
    ProbeFailed {
        /// The probe.
        command: String,
        /// The tail of its standard error.
        stderr: String,
    },
    /// The linter could not be executed, or exited with a status it does not use
    /// for a completed run.
    #[error(transparent)]
    Run(#[from] SubprocessError),
    /// The linter ran but its output could not be read as a report.
    #[error(transparent)]
    Report(#[from] ExecError),
    /// The build did not complete **and** produced no diagnostics explaining
    /// why — so there is nothing to report and nothing to conclude.
    ///
    /// Separated from a build that failed *with* diagnostics, which is the
    /// ordinary outcome of a repository that denies a lint group and is reported
    /// normally. This one is the shape that would otherwise print "0 finding(s)"
    /// over a run that never compiled anything.
    #[error(
        "`{command}` exited {status} without completing the build and without a single \
         diagnostic, so there is nothing to report — this is not a clean tree.{stderr}"
    )]
    BuildProducedNothing {
        /// The argv that ran.
        command: String,
        /// Its exit status.
        status: i32,
        /// The tail of its standard error.
        stderr: String,
    },
}

/// What produced a report, beyond the analyzer's own version.
///
/// A stored run carries this in its [`rto_graph::AnalysisRun`]; an ephemeral one
/// has no such record, so the report itself has to say it. Without it a count is
/// not comparable to any other count — the toolchain **is** the rule set here,
/// and there is no pinned asset with a digest standing behind the answer.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Toolchain {
    /// `cargo clippy --version`, verbatim.
    pub linter: String,
    /// `rustc --version`, verbatim.
    pub rustc: String,
    /// The host triple rustc reports.
    pub host: String,
}

/// A completed lint run: the findings, and every input that decided them.
///
/// Deliberately **not** an [`crate::AnalysisResponse`]: that type carries an
/// `AnalysisRun` whose purpose is to be persisted, and there is no persisting
/// here. What they share is the finding type, so a caller renders a lint exactly
/// as it renders a stored finding.
#[derive(Debug, Clone)]
pub struct LintOutcome {
    /// The linter that ran.
    pub analyzer: &'static str,
    /// Its findings, in the same normalised shape every analyzer produces —
    /// borrowed for its structure, never for its storage.
    pub report: NormalizedReport,
    /// What the output stream contained besides findings.
    pub summary: Summary,
    /// What produced it.
    pub toolchain: Toolchain,
    /// The feature set the build was resolved with.
    pub features: FeatureSet,
    /// The boundary the run actually had. Read from the runner rather than
    /// inferred by a caller, exactly as a stored run's isolation is.
    pub isolation: Isolation,
    /// The exact argv that ran, so the run is reproducible by hand.
    pub command: Vec<String>,
    /// The workspace root it ran in.
    pub worktree: PathBuf,
}

/// The argv `analyzer` would run at this feature set, or `None` if this is not a
/// linter roteiro drives.
///
/// Exists so a caller can disclose the command **before** running it without
/// having to know which analyzer maps to which argv — a caller that guessed
/// would eventually announce one linter's command under another one's name.
#[must_use]
pub fn invocation(analyzer: &str, features: &FeatureSet) -> Option<Invocation> {
    (analyzer == clippy::ANALYZER).then(|| Clippy::invocation(features))
}

/// Run `analyzer` over the cargo workspace containing `dir`, and return what it
/// said.
///
/// Writes nothing anywhere: no store is opened, no layer is keyed, and the
/// caller is handed a value to print. The one thing it does write is whatever
/// the build writes — `target/` — which is why this is not an
/// [`crate::AnalyzerRunner`]; see the module documentation.
///
/// The report describes the **working tree as it is on disk right now**, not a
/// commit. No source identity is recorded, because recording one would imply a
/// tie to a revision that a dirty tree does not have.
///
/// # Errors
/// Returns [`LintError`]: an unknown analyzer, a missing toolchain or linter
/// (each naming what to install), no cargo project, a failed probe, a failed
/// execution, output that is not a report, or a build that produced neither a
/// completion nor a diagnostic.
pub fn run(analyzer: &str, dir: &Path, features: &FeatureSet) -> Result<LintOutcome, LintError> {
    if analyzer != clippy::ANALYZER {
        return Err(LintError::UnknownAnalyzer {
            requested: analyzer.to_owned(),
            known: LINT_ANALYZERS.join(", "),
        });
    }
    let root = workspace_root(dir, analyzer)?;
    let toolchain = probe_toolchain(&root, analyzer)?;
    let invocation = Clippy::invocation(features);
    let command = argv(&invocation);

    let started_at = rfc3339_utc(std::time::SystemTime::now());
    let output = execute(&invocation, &root, analyzer, TOOLCHAIN_ENV)?;
    let ended_at = rfc3339_utc(std::time::SystemTime::now());

    let snippets = WorktreeSnippets::new(&root);
    let source = SourceIdentity::default();
    let ctx = NativeContext {
        started_at,
        ended_at,
        analyzer_version: Some(short_version(&toolchain.linter)),
        exit_status: output.status,
        source: &source,
        rules_digest: None,
        advisory_db: None,
        worktree: Some(&root),
        snippets: &snippets,
    };
    let (report, summary) = Clippy::parse(&output.stdout, &ctx)?;

    // A build that neither finished nor said anything is not a clean tree, and
    // this is where that stops being a number.
    if !summary.build_succeeded && report.findings.is_empty() {
        return Err(LintError::BuildProducedNothing {
            command: command.join(" "),
            status: output.status,
            stderr: stderr_tail(&output.stderr),
        });
    }

    Ok(LintOutcome {
        analyzer: clippy::ANALYZER,
        report,
        summary,
        toolchain,
        features: features.clone(),
        // The only honest answer, and it is stated by the code that ran the
        // process rather than assembled by whoever prints it.
        isolation: Isolation::None,
        command,
        worktree: root,
    })
}

/// The full argv of an invocation, program first.
fn argv(invocation: &Invocation) -> Vec<String> {
    let mut command = vec![invocation.program.clone()];
    command.extend(invocation.args.iter().cloned());
    command
}

/// Ask cargo where the workspace containing `dir` is.
///
/// Cargo's own answer rather than a walk up the tree looking for `Cargo.toml`:
/// it is the same resolution the build will use, and getting it from anywhere
/// else risks reporting paths relative to a root the compiler never saw.
fn workspace_root(dir: &Path, analyzer: &str) -> Result<PathBuf, LintError> {
    let args = ["locate-project", "--workspace", "--message-format", "plain"];
    let probe = probe("cargo", &args, dir, analyzer)?;
    let no_project = |why: String| LintError::NoCargoProject {
        dir: dir.display().to_string(),
        command: rendered("cargo", &args),
        analyzer: analyzer.to_owned(),
        stderr: why,
    };
    // Cargo's failure is checked first, so the message carries *its* reason
    // ("could not find `Cargo.toml`") rather than this function's observation
    // that the answer was empty.
    if !probe.ok {
        return Err(no_project(stderr_tail(probe.stderr.as_bytes())));
    }
    let manifest = probe.stdout.trim();
    if manifest.is_empty() {
        return Err(no_project(
            "\n  cargo exited cleanly and named no manifest".to_owned(),
        ));
    }
    let Some(root) = Path::new(manifest).parent() else {
        return Err(no_project(format!(
            "\n  cargo answered {manifest:?}, which has no directory"
        )));
    };
    Ok(root.to_path_buf())
}

/// Ask the toolchain what it is, before running it.
///
/// This doubles as the *is it installed* check, which is why it comes before the
/// run rather than being read out of the output: `cargo clippy` on a toolchain
/// without the component prints "no such command" and exits with a status cargo
/// also uses for a completed build, so a run alone cannot tell the two apart.
fn probe_toolchain(root: &Path, analyzer: &str) -> Result<Toolchain, LintError> {
    let clippy_args = ["clippy", "--version"];
    let clippy = probe("cargo", &clippy_args, root, analyzer)?;
    if !clippy.ok || clippy.stdout.trim().is_empty() {
        return Err(LintError::AnalyzerNotInstalled {
            analyzer: analyzer.to_owned(),
            command: rendered("cargo", &clippy_args),
            install: "rustup component add clippy".to_owned(),
            stderr: stderr_tail(clippy.stderr.as_bytes()),
        });
    }

    let rustc_args = ["-vV"];
    let rustc = probe("rustc", &rustc_args, root, analyzer)?;
    if !rustc.ok {
        return Err(LintError::ProbeFailed {
            command: rendered("rustc", &rustc_args),
            stderr: stderr_tail(rustc.stderr.as_bytes()),
        });
    }
    let (version, host) = parse_rustc_verbose(&rustc.stdout);
    Ok(Toolchain {
        linter: first_line(&clippy.stdout),
        rustc: version,
        host,
    })
}

/// What a probe produced.
struct Probe {
    stdout: String,
    stderr: String,
    ok: bool,
}

/// Run a short toolchain query with the same scrubbed environment the linter
/// gets, so a probe can never succeed in an environment the run would not have.
fn probe(program: &str, args: &[&str], dir: &Path, analyzer: &str) -> Result<Probe, LintError> {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    scrub_environment(&mut command, TOOLCHAIN_ENV);

    let output = command.output().map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            LintError::ToolchainMissing {
                program: program.to_owned(),
                analyzer: analyzer.to_owned(),
            }
        } else {
            LintError::ProbeFailed {
                command: rendered(program, args),
                stderr: format!("\n  {source}"),
            }
        }
    })?;
    Ok(Probe {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        ok: output.status.success(),
    })
}

/// A command rendered for a message.
fn rendered(program: &str, args: &[&str]) -> String {
    std::iter::once(program)
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The first non-empty line of some output, trimmed.
fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_owned()
}

/// The version number out of `clippy 0.1.94 (5e2a1e56d 2026-06-27)`.
///
/// Not the last whitespace token the way [`crate::subprocess`] reads a version:
/// that works for `semgrep 1.136.0` and would record the *date* for clippy. An
/// unrecognised shape is kept whole rather than guessed at.
fn short_version(line: &str) -> String {
    let mut tokens = line.split_whitespace();
    let version = match (tokens.next(), tokens.next()) {
        (Some("clippy" | "cargo-clippy"), Some(version)) => version,
        _ => line.trim(),
    };
    if version.is_empty() {
        UNKNOWN_VERSION.to_owned()
    } else {
        version.to_owned()
    }
}

/// The version line and host triple out of `rustc -vV`.
fn parse_rustc_verbose(text: &str) -> (String, String) {
    let host = text
        .lines()
        .find_map(|line| line.strip_prefix("host:"))
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .unwrap_or(UNKNOWN_VERSION)
        .to_owned();
    let version = first_line(text);
    let version = if version.is_empty() {
        UNKNOWN_VERSION.to_owned()
    } else {
        version
    };
    (version, host)
}

#[cfg(test)]
mod tests {
    use super::{
        LINT_ANALYZERS, LintError, TOOLCHAIN_ENV, first_line, parse_rustc_verbose, run,
        short_version,
    };
    use crate::adapter::clippy::FeatureSet;

    /// Asking a linter surface for a storing analyzer is a mistake worth a
    /// sentence: the two commands do different things to the store, so the
    /// refusal points at the other one rather than just listing names.
    #[test]
    fn refuses_an_analyzer_it_does_not_drive_before_running_anything() {
        let err = run(
            "semgrep",
            std::path::Path::new("/nonexistent"),
            &FeatureSet::Defaults,
        )
        .expect_err("must refuse");
        assert!(matches!(err, LintError::UnknownAnalyzer { .. }));
        let message = err.to_string();
        assert!(message.contains("clippy"), "{message}");
        assert!(message.contains("roteiro security run"), "{message}");
    }

    /// Requirement of the whole surface: absence is an error, and the error says
    /// what to install. An empty result meaning "the tool was not there" is the
    /// vacuous zero this project has been bitten by.
    #[test]
    fn every_absence_names_what_to_install_and_refuses_to_report() {
        let missing_toolchain = LintError::ToolchainMissing {
            program: "cargo".to_owned(),
            analyzer: "clippy".to_owned(),
        }
        .to_string();
        assert!(missing_toolchain.contains("not found on PATH"));
        assert!(missing_toolchain.contains("https://rustup.rs"));
        assert!(missing_toolchain.contains("must never read as a clean tree"));

        let missing_component = LintError::AnalyzerNotInstalled {
            analyzer: "clippy".to_owned(),
            command: "cargo clippy --version".to_owned(),
            install: "rustup component add clippy".to_owned(),
            stderr: String::new(),
        }
        .to_string();
        assert!(missing_component.contains("rustup component add clippy"));
        assert!(missing_component.contains("must never read as a clean tree"));

        let nothing = LintError::BuildProducedNothing {
            command: "cargo clippy".to_owned(),
            status: 101,
            stderr: String::new(),
        }
        .to_string();
        assert!(
            nothing.contains("this is not a clean tree"),
            "{nothing}: a build that said nothing must not read as zero findings"
        );
    }

    /// The two lists answer different questions, and conflating them would send
    /// a caller to ask for a lint from an analyzer that files layers.
    #[test]
    fn the_lint_registry_is_not_the_storable_one() {
        assert_eq!(LINT_ANALYZERS, &["clippy"]);
        for storable in crate::known_analyzers() {
            assert!(
                !LINT_ANALYZERS.contains(&storable),
                "{storable} stores its findings and must not be offered as a lint"
            );
        }
    }

    #[test]
    fn reads_a_clippy_version_without_mistaking_it_for_a_date() {
        assert_eq!(
            short_version("clippy 0.1.94 (5e2a1e56d 2026-06-27)"),
            "0.1.94"
        );
        assert_eq!(short_version("cargo-clippy 0.1.94"), "0.1.94");
        // An unrecognised shape is kept whole rather than guessed at.
        assert_eq!(
            short_version("something else entirely"),
            "something else entirely"
        );
        assert_eq!(short_version("   "), "unknown");
    }

    #[test]
    fn reads_the_rustc_version_and_host_triple() {
        let verbose = "rustc 1.94.0 (0123abcd 2026-06-26)\n\
                       binary: rustc\n\
                       commit-hash: 0123abcd\n\
                       host: aarch64-apple-darwin\n\
                       release: 1.94.0\n";
        let (version, host) = parse_rustc_verbose(verbose);
        assert_eq!(version, "rustc 1.94.0 (0123abcd 2026-06-26)");
        assert_eq!(host, "aarch64-apple-darwin");

        // Output that says neither is recorded as unknown, never as empty: a
        // blank field in a report reads as "there was nothing to say".
        let (version, host) = parse_rustc_verbose("");
        assert_eq!(version, "unknown");
        assert_eq!(host, "unknown");
    }

    #[test]
    fn takes_the_first_non_empty_line() {
        assert_eq!(first_line("\n\n  hello \nworld"), "hello");
        assert_eq!(first_line(""), "");
    }

    /// The extra variables are locators for a toolchain, not credentials. A
    /// token added here would be handed to every build script in the tree.
    #[test]
    fn the_toolchain_variables_are_locators_and_nothing_else() {
        for key in TOOLCHAIN_ENV {
            assert!(
                key.ends_with("_HOME")
                    || key.ends_with("_TOOLCHAIN")
                    || key.ends_with("_TARGET_DIR"),
                "{key} is not a locator"
            );
            assert!(!key.contains("TOKEN") && !key.contains("KEY"), "{key}");
        }
    }
}

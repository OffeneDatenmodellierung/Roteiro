//! The subprocess backend: run the analyzer on the host, and say so.
//!
//! ADR-0014 calls this "the explicit escape hatch". It executes a real analyzer
//! against a real worktree and produces real findings, and it provides **no
//! isolation whatsoever**. Both halves of that sentence are load-bearing, and
//! the second is the reason this backend needs `--allow-unsandboxed` and records
//! [`Isolation::None`].
//!
//! # What "network: deny" means here, precisely
//!
//! An [`rto_graph::AnalysisRun`] records a [`CommandPolicy`], and this backend
//! records `network: Deny`. That is a claim about **what the run was configured
//! to do**, not a kernel-enforced boundary:
//!
//! - the analyzer is invoked with its own egress switched off — `semgrep` gets
//!   `--metrics=off --disable-version-check` and a `--config` that is a local
//!   file rather than a registry id; `cargo audit` gets `--no-fetch`;
//! - its inputs are provisioned and digest-pinned before the run starts, so
//!   nothing it needs *would* require a fetch;
//! - but a subprocess on the host can open a socket, and nothing here stops it.
//!
//! Only the sandboxed backend can enforce egress denial. Until it lands, the
//! honest reading of a `subprocess` run's evidence is *isolation none, egress
//! configured off*, and `isolation=none` on the record is what says so. This is
//! written out rather than left implied because "network: deny" on a stored
//! record is exactly the kind of field a reader assumes was enforced.
//!
//! # What it does guarantee
//!
//! - **The environment is scrubbed.** The child gets a minimal, explicit
//!   environment, so ambient credentials in the parent's environment —
//!   `GITHUB_TOKEN`, `AWS_*`, `SEMGREP_APP_TOKEN` — are not handed to a
//!   third-party binary.
//! - **The worktree is not written.** Every shipped invocation is read-only, and
//!   the shared preflight refuses a request that asks for a writable tree. This
//!   is a property of the commands, not a mount option; the sandboxed backend is
//!   what makes it a boundary.
//! - **A failed run yields nothing.** A status the adapter did not declare
//!   successful is an error, never an empty finding set — a scan that fell over
//!   must not read as a clean bill of health.
//!
//! @rto:0014
//! @rto:0012

use std::path::PathBuf;
use std::process::{Command, Stdio};

use rto_graph::{Isolation, RunnerKind, rfc3339_utc};

use crate::adapter::{Adapter, AssetPaths, Invocation, NativeContext};
use crate::assets;
use crate::guidance::{Guidance, Line};
use crate::ingest::assemble;
use crate::runner::{AnalysisRequest, AnalysisResponse, AnalyzerRunner, ExecError, check_request};
use crate::snippet::WorktreeSnippets;

/// The largest analyzer report that will be read into memory.
///
/// A ceiling, not a target. `MAX_REPORT_FINDINGS` bounds the report once it is
/// parsed; this bounds it before, so a runaway analyzer cannot exhaust memory on
/// the way there.
pub const MAX_OUTPUT_BYTES: usize = 256 << 20;

/// ADR-0014's seam (c), said where it is needed rather than where it is
/// documented.
///
/// Appended to the missing-binary refusal **after** the install hint, and never
/// instead of it: the two answer different readers. Someone who wants the
/// analyzer here needs the install command; someone whose CI already runs it
/// needs to know that a normalized report from anywhere reads in through the
/// same adapter and produces the same findings. The old message had this half
/// and only this half, which is the part of it worth keeping.
const INGEST_INSTEAD: Guidance = Guidance::new(&[Line::Note(&[
    "Or run the analyzer elsewhere — CI, a colleague's machine — and read its",
    "report in with `roteiro security ingest`, which needs nothing on PATH and",
    "produces the same findings as a local run.",
])]);

/// What the missing-binary refusal says when no adapter declares the program.
///
/// Unreachable for anything shipped, and deliberately a *shorter* message rather
/// than a guessed command: a refusal that does not know the way forward should
/// say less, not invent. See [`SubprocessError::BinaryNotFound`].
const NO_HINT: Guidance = Guidance::new(&[Line::Note(&[
    "Roteiro does not install analyzers, and has not installed this one; this",
    "build knows no install command for that program.",
])]);

/// Something went wrong executing the analyzer, as opposed to something being
/// wrong with what it produced.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SubprocessError {
    /// The user did not pass `--allow-unsandboxed`.
    ///
    /// The flag is the whole consent mechanism for a backend with no boundary,
    /// so its absence is refused before anything is executed.
    #[error(
        "running `{analyzer}` as a subprocess provides no isolation: the analyzer executes on \
         this host with access to it. Pass --allow-unsandboxed to accept that; the run's evidence \
         will record isolation=none."
    )]
    UnsandboxedNotAllowed {
        /// The analyzer that was asked for.
        analyzer: String,
    },
    /// The analyzer binary is not on `PATH`.
    ///
    /// The refusal issue #430 is about. It used to end at "install it yourself",
    /// which is the obstacle stated twice and the way forward stated never — so
    /// it now carries the adapter's own [`crate::adapter::InstallHint`] for the
    /// program that is missing, and ADR-0014's seam (c) after it. Both halves
    /// matter: `security run` genuinely cannot proceed here, and `security
    /// ingest` genuinely can, so a message that named only the install would
    /// take a capability away from a reader who has a report already.
    ///
    /// `install` is `None` only for a program no adapter declares, which no
    /// shipped invocation reaches — see [`crate::adapter::install_hint`]. The
    /// fallback sentence is what a future one would get, and says less rather
    /// than guessing.
    #[error(
        "analyzer binary `{program}` not found on PATH (needed to run `{analyzer}`), so nothing \
         ran.{}{}",
        .install.unwrap_or(NO_HINT),
        INGEST_INSTEAD
    )]
    BinaryNotFound {
        /// The program that was looked for.
        program: String,
        /// The analyzer it belongs to.
        analyzer: String,
        /// How to obtain `program`, as its adapter declares it.
        install: Option<Guidance>,
    },
    /// The analyzer could not be started, for a reason other than not existing.
    #[error("could not execute `{program}`: {source}")]
    Spawn {
        /// The program that was tried.
        program: String,
        /// The underlying failure.
        source: std::io::Error,
    },
    /// The analyzer exited with a status it does not use for "ran successfully".
    #[error(
        "`{program}` exited with status {status}, which it does not use for a completed scan \
         (expected one of: {expected}). A scan that failed is not a clean result, so nothing was \
         stored.{stderr}"
    )]
    UnexpectedStatus {
        /// The program that ran.
        program: String,
        /// The status it exited with, or `-1` if it was killed by a signal.
        status: i32,
        /// The statuses the adapter declared usable.
        expected: String,
        /// The tail of its standard error, prefixed for display.
        stderr: String,
    },
    /// The analyzer produced more output than will be read.
    #[error("`{program}` produced more than {max} bytes of output; refusing to read it")]
    OutputTooLarge {
        /// The program that ran.
        program: String,
        /// The ceiling.
        max: usize,
    },
}

/// Executes an analyzer as a child process on the host.
#[derive(Debug)]
pub struct SubprocessRunner {
    adapter: &'static dyn Adapter,
    assets: Vec<(&'static str, PathBuf)>,
    /// Kept so the run reads its evidence from the *same* cache the assets were
    /// resolved from — a test cache and the user's cache must never mix.
    assets_root: PathBuf,
    allow_unsandboxed: bool,
}

impl SubprocessRunner {
    /// Build a runner for `analyzer`, resolving its pinned assets under `root`.
    ///
    /// `allow_unsandboxed` is the `--allow-unsandboxed` flag. It is taken at
    /// construction rather than read from a global so a caller cannot end up
    /// running unsandboxed by forgetting to check something.
    ///
    /// # Errors
    /// Returns [`ExecError::UnknownAnalyzer`] if this build cannot run the
    /// analyzer, [`ExecError::Subprocess`] if the unsandboxed flag was not
    /// given, or [`ExecError::AssetsUnavailableOffline`] if its pinned inputs
    /// are not provisioned. **Asset resolution happens here**, before anything
    /// is executed, so a cold cache fails without having started a process.
    pub fn new(
        analyzer: &str,
        assets_root: &std::path::Path,
        allow_unsandboxed: bool,
    ) -> Result<Self, ExecError> {
        let adapter =
            crate::adapter::adapter_for(analyzer).ok_or_else(|| ExecError::UnknownAnalyzer {
                requested: analyzer.to_owned(),
                known: crate::adapter::known_analyzers().join(", "),
            })?;
        if !allow_unsandboxed {
            return Err(SubprocessError::UnsandboxedNotAllowed {
                analyzer: analyzer.to_owned(),
            }
            .into());
        }
        Ok(Self {
            adapter,
            assets: assets::resolve(assets_root, analyzer)?,
            assets_root: assets_root.to_path_buf(),
            allow_unsandboxed,
        })
    }

    /// The adapter this runner drives.
    #[must_use]
    pub fn adapter(&self) -> &'static dyn Adapter {
        self.adapter
    }

    /// The invocation this runner will execute, for `--help`-style disclosure
    /// and for tests that assert the argv without running anything.
    #[must_use]
    pub fn invocation(&self) -> Invocation {
        self.adapter.command(&AssetPaths::new(&self.assets))
    }

    /// The digest recorded for the analyzer's rule set, where it has one.
    fn rules_digest(&self, root: &std::path::Path) -> Option<String> {
        self.assets.iter().find_map(|(id, _)| {
            let spec = assets::asset(id)?;
            (spec.kind == assets::AssetKind::Rules)
                .then(|| assets::installed(root, spec).map(|record| record.digest))
                .flatten()
        })
    }
}

impl AnalyzerRunner for SubprocessRunner {
    fn kind(&self) -> RunnerKind {
        RunnerKind::Subprocess
    }

    fn isolation(&self) -> Isolation {
        // The only honest answer. See the module docs: egress is configured off
        // and the environment is scrubbed, but nothing here is a boundary.
        Isolation::None
    }

    fn run(&self, request: &AnalysisRequest) -> Result<AnalysisResponse, ExecError> {
        check_request(request)?;
        // Belt and braces: `new` already refused without the flag, but a runner
        // is a value that can be moved around, and this check costs nothing.
        if !self.allow_unsandboxed {
            return Err(SubprocessError::UnsandboxedNotAllowed {
                analyzer: request.analyzer.clone(),
            }
            .into());
        }

        let invocation = self.invocation();
        let started_at = rfc3339_utc(std::time::SystemTime::now());
        let output = execute(
            &invocation,
            &request.worktree.path,
            &request.analyzer,
            &ChildEnv::default(),
        )?;
        let ended_at = rfc3339_utc(std::time::SystemTime::now());

        let snippets = WorktreeSnippets::new(&request.worktree.path);
        let ctx = NativeContext {
            started_at,
            ended_at,
            analyzer_version: analyzer_version(&invocation, &request.worktree.path),
            exit_status: output.status,
            source: &request.source,
            rules_digest: self.rules_digest(&self.assets_root),
            advisory_db: assets::advisory_db_evidence(&self.assets_root, &request.analyzer),
            // The tree the analyzer was pointed at, so an adapter whose analyzer
            // reports absolute paths can place them back inside it.
            worktree: Some(&request.worktree.path),
            snippets: &snippets,
        };

        // The same conversion `roteiro security ingest` runs over the same bytes.
        // That is what makes the two paths agree, rather than a test that checks
        // they happen to.
        let report = self.adapter.normalize(&output.stdout, &ctx)?;
        assemble(
            report,
            request,
            self.kind(),
            self.isolation(),
            &output.stdout,
        )
    }
}

/// What a completed analyzer run produced.
pub(crate) struct Captured {
    pub(crate) stdout: Vec<u8>,
    /// Kept even on a successful run, because a caller may need to explain an
    /// *empty* success: a tool that exited cleanly and said nothing has usually
    /// said why on its standard error, and that is the difference between "no
    /// findings" and "did not run".
    pub(crate) stderr: Vec<u8>,
    pub(crate) status: i32,
}

/// Run the analyzer and capture its stdout.
///
/// `env` says what reaches the child beyond the scrubbed minimum — which names
/// are inherited, and which variables Roteiro sets outright; see [`ChildEnv`]
/// for why those are two lists rather than one. A reader-class analyzer needs
/// neither and passes the default.
pub(crate) fn execute(
    invocation: &Invocation,
    worktree: &std::path::Path,
    analyzer: &str,
    env: &ChildEnv<'_>,
) -> Result<Captured, SubprocessError> {
    let mut command = Command::new(&invocation.program);
    command
        .args(&invocation.args)
        .current_dir(worktree)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    scrub_environment(&mut command, env);

    let output = command.output().map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            SubprocessError::BinaryNotFound {
                program: invocation.program.clone(),
                analyzer: analyzer.to_owned(),
                // Looked up by program, not by analyzer: `cargo-audit`'s
                // invocation is `cargo`, and its hint is rustup's page rather
                // than `cargo install cargo-audit`. Handing the reader the
                // second when the first is what is missing would be a way
                // forward that does not lead anywhere.
                install: crate::adapter::install_hint(&invocation.program),
            }
        } else {
            SubprocessError::Spawn {
                program: invocation.program.clone(),
                source,
            }
        }
    })?;

    if output.stdout.len() > MAX_OUTPUT_BYTES {
        return Err(SubprocessError::OutputTooLarge {
            program: invocation.program.clone(),
            max: MAX_OUTPUT_BYTES,
        });
    }

    // `None` means the child was killed by a signal. `-1` is not a status any
    // analyzer declares successful, so it falls through to the error below and
    // is reported rather than mistaken for a clean scan.
    let status = output.status.code().unwrap_or(-1);
    if !invocation.success_statuses.contains(&status) {
        return Err(SubprocessError::UnexpectedStatus {
            program: invocation.program.clone(),
            status,
            expected: invocation
                .success_statuses
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
            stderr: stderr_tail(&output.stderr),
        });
    }

    Ok(Captured {
        stdout: output.stdout,
        stderr: output.stderr,
        status,
    })
}

/// The last few lines of an analyzer's standard error, for a failure message.
///
/// Bounded, because a failing analyzer can be extremely talkative and a wall of
/// output in an error message hides the error.
pub(crate) fn stderr_tail(stderr: &[u8]) -> String {
    const MAX_LINES: usize = 8;
    const MAX_BYTES: usize = 4_000;
    let text = String::from_utf8_lossy(stderr);
    let trimmed = text.trim_end();
    if trimmed.is_empty() {
        return String::new();
    }
    let tail: Vec<&str> = trimmed
        .lines()
        .rev()
        .take(MAX_LINES)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let mut joined = tail.join("\n  ");
    if joined.len() > MAX_BYTES {
        joined.truncate(MAX_BYTES);
        joined.push('…');
    }
    format!("\n  its stderr ended:\n  {joined}")
}

// `ChildEnv` and `scrub_environment` used to live here, and moving them out is
// the point rather than a tidy-up. They were this backend's, while the guest
// backend built its environment from nothing in `boxlite.rs` — two mechanisms
// for one concept, which is the shape that let `CARGO_TARGET_DIR` be listed as
// a passthrough under a promise that it was configured. They are now one type
// with two consumers in `crate::child_env`, which is also where the reason a
// guest cannot have an `inherit` half is written down.
pub(crate) use crate::child_env::{ChildEnv, scrub_environment};

/// Ask the analyzer for its version, best effort.
///
/// A version is evidence, not a precondition: an analyzer that will not answer
/// `--version` can still produce a perfectly good report, and refusing to run it
/// over that would be absurd. `None` here becomes [`crate::UNKNOWN_VERSION`].
fn analyzer_version(invocation: &Invocation, worktree: &std::path::Path) -> Option<String> {
    // `cargo audit --version` needs the subcommand; `semgrep --version` does
    // not. Taking the leading non-flag arguments handles both without the
    // runner having to know which analyzer it is driving.
    let mut args: Vec<&String> = invocation
        .args
        .iter()
        .take_while(|a| !a.starts_with('-'))
        .collect();
    let version_flag = "--version".to_owned();
    args.push(&version_flag);

    let mut command = Command::new(&invocation.program);
    command
        .args(&args)
        .current_dir(worktree)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    scrub_environment(&mut command, &ChildEnv::default());

    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().find(|l| !l.trim().is_empty())?.trim();
    // `cargo audit --version` prints "cargo-audit-audit 0.21.2"; `semgrep
    // --version` prints "1.136.0". Take the last whitespace-separated token,
    // which is the number in both shapes.
    let version = line.split_whitespace().next_back().unwrap_or(line);
    (!version.is_empty()).then(|| version.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{
        ChildEnv, MAX_OUTPUT_BYTES, SubprocessError, SubprocessRunner, scrub_environment,
        stderr_tail,
    };
    use crate::assets;
    use crate::runner::ExecError;
    use std::path::PathBuf;

    struct Cache(PathBuf);

    impl Cache {
        fn warm(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("rto-exec-subprocess-{name}"));
            std::fs::remove_dir_all(&dir).ok();
            std::fs::create_dir_all(&dir).expect("create");
            let cache = Self(dir);
            assets::provision(&cache.0, assets::asset("semgrep-rules").expect("spec"))
                .expect("provision");
            cache
        }

        fn cold(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("rto-exec-subprocess-{name}"));
            std::fs::remove_dir_all(&dir).ok();
            std::fs::create_dir_all(&dir).expect("create");
            Self(dir)
        }
    }

    impl Drop for Cache {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    /// The flag is the entire consent mechanism for a backend with no boundary,
    /// so it is checked before assets, before spawning, before anything.
    #[test]
    fn refuses_to_exist_without_the_unsandboxed_flag() {
        let cache = Cache::warm("no-flag");
        let err = SubprocessRunner::new("semgrep", &cache.0, false)
            .expect_err("must refuse without the flag");
        assert!(matches!(
            err,
            ExecError::Subprocess(SubprocessError::UnsandboxedNotAllowed { .. })
        ));
        let message = err.to_string();
        assert!(message.contains("--allow-unsandboxed"), "{message}");
        assert!(message.contains("isolation=none"), "{message}");
    }

    /// A cold cache is refused at construction — before a process is started, so
    /// the failure cannot be confused with an analyzer problem.
    #[test]
    fn refuses_a_cold_cache_before_executing_anything() {
        let cache = Cache::cold("cold");
        let err = SubprocessRunner::new("semgrep", &cache.0, true).expect_err("cold cache");
        assert!(matches!(err, ExecError::AssetsUnavailableOffline { .. }));
        assert!(err.to_string().contains("assets-unavailable-offline"));
    }

    #[test]
    fn refuses_an_analyzer_this_build_cannot_run() {
        let cache = Cache::warm("unknown");
        let err = SubprocessRunner::new("no-such-analyzer", &cache.0, true).expect_err("unknown");
        let ExecError::UnknownAnalyzer { known, .. } = &err else {
            panic!("expected UnknownAnalyzer, got {err:?}");
        };
        assert!(known.contains("semgrep"), "{known}");
    }

    /// The isolation label is the honesty mechanism for this backend, so it is
    /// pinned by a test rather than left to a reviewer to notice.
    #[test]
    fn labels_itself_as_a_subprocess_with_no_isolation() {
        use crate::runner::AnalyzerRunner;
        let cache = Cache::warm("labels");
        let runner = SubprocessRunner::new("semgrep", &cache.0, true).expect("runner");
        assert_eq!(runner.kind(), rto_graph::RunnerKind::Subprocess);
        assert_eq!(runner.isolation(), rto_graph::Isolation::None);
    }

    #[test]
    fn the_invocation_points_at_the_provisioned_rules() {
        let cache = Cache::warm("invocation");
        let runner = SubprocessRunner::new("semgrep", &cache.0, true).expect("runner");
        let invocation = runner.invocation();
        let config = invocation
            .args
            .iter()
            .position(|a| a == "--config")
            .map(|i| invocation.args[i + 1].clone())
            .expect("a --config argument");
        assert_eq!(
            PathBuf::from(config),
            assets::asset_path(&cache.0, assets::asset("semgrep-rules").expect("spec"))
        );
    }

    /// Ambient credentials in the parent's environment are not an analyzer
    /// input, and this is the check that keeps them out.
    #[test]
    fn the_child_environment_carries_no_ambient_credentials() {
        let mut command = std::process::Command::new("true");
        scrub_environment(&mut command, &ChildEnv::default());
        let passed: Vec<String> = command
            .get_envs()
            .filter_map(|(k, v)| v.map(|_| k.to_string_lossy().into_owned()))
            .collect();
        for secret in [
            "GITHUB_TOKEN",
            "AWS_ACCESS_KEY_ID",
            "SEMGREP_APP_TOKEN",
            "SSH_AUTH_SOCK",
        ] {
            assert!(
                !passed.contains(&secret.to_owned()),
                "{secret} was passed through"
            );
        }
        assert!(
            passed.contains(&"PATH".to_owned()),
            "the child still needs PATH"
        );
        assert!(passed.contains(&"LC_ALL".to_owned()));
    }

    /// The extra pass-through is **by name**, so a caller that needs a variable
    /// the base list does not carry gets that one and no more. The linter needs
    /// this for `CARGO_HOME`/`RUSTUP_HOME`; nothing else may ride along.
    #[test]
    fn extra_variables_are_passed_through_only_when_named() {
        let base = [
            "PATH",
            "HOME",
            "USERPROFILE",
            "SystemRoot",
            "TMPDIR",
            "TEMP",
            "LC_ALL",
            "SEMGREP_SEND_METRICS",
        ];
        // Any variable this process really has that the base list does not
        // carry, so the assertion is about the mechanism rather than about one
        // machine's environment.
        let Some(candidate) = std::env::vars()
            .map(|(key, _)| key)
            .find(|key| !base.contains(&key.as_str()))
        else {
            return; // an environment with nothing else in it proves nothing
        };

        let passed = |extra: &[&str]| -> Vec<String> {
            let mut command = std::process::Command::new("true");
            scrub_environment(
                &mut command,
                &ChildEnv {
                    inherit: extra,
                    ..ChildEnv::default()
                },
            );
            command
                .get_envs()
                .filter_map(|(k, v)| v.map(|_| k.to_string_lossy().into_owned()))
                .collect()
        };
        assert!(
            !passed(&[]).contains(&candidate),
            "{candidate} reached the child without being named"
        );
        assert!(
            passed(&[candidate.as_str()]).contains(&candidate),
            "{candidate} was named and still did not reach the child"
        );
        // Naming something that does not exist invents nothing.
        assert!(
            !passed(&["ROTEIRO_NO_SUCH_VARIABLE"]).contains(&"ROTEIRO_NO_SUCH_VARIABLE".to_owned())
        );
    }

    /// The distinction [`ChildEnv`] exists to draw, pinned at the seam that
    /// implements it. Naming a variable can only ever pass the parent's value
    /// along — so a caller that needs a *particular* value and spells it as a
    /// name gets whatever the invoking shell had, including nothing at all.
    /// That is the defect this type was split to make unspellable, and this is
    /// the test that fails if the halves are merged back together.
    #[test]
    fn inheriting_a_name_cannot_set_a_value_and_setting_beats_inheriting() {
        let value = |env: &ChildEnv<'_>| -> Option<std::ffi::OsString> {
            let mut command = std::process::Command::new("true");
            scrub_environment(&mut command, env);
            command
                .get_envs()
                .find(|(k, _)| *k == std::ffi::OsStr::new("ROTEIRO_SEAM_PROBE"))
                .and_then(|(_, v)| v.map(std::ffi::OsStr::to_os_string))
        };

        // Naming a variable the parent does not have configures nothing. This
        // is the exact shape of the `CARGO_TARGET_DIR` defect: a passthrough
        // entry that reads as a setting and is a no-op.
        assert_eq!(
            value(&ChildEnv {
                inherit: &["ROTEIRO_SEAM_PROBE"],
                ..ChildEnv::default()
            }),
            None,
            "inheriting an unset name invented a value"
        );

        // Setting it does configure it, with no help from the parent.
        let chosen = [("ROTEIRO_SEAM_PROBE", std::ffi::OsString::from("/chosen"))];
        assert_eq!(
            value(&ChildEnv {
                set: &chosen,
                ..ChildEnv::default()
            }),
            Some(std::ffi::OsString::from("/chosen"))
        );

        // And when a caller says both, the constraint wins over the ambient
        // value — `HOME` stands in for "a name the parent really does have".
        let home = [("HOME", std::ffi::OsString::from("/chosen-home"))];
        let mut command = std::process::Command::new("true");
        scrub_environment(
            &mut command,
            &ChildEnv {
                inherit: &["HOME"],
                set: &home,
            },
        );
        let passed: Vec<(std::ffi::OsString, Option<std::ffi::OsString>)> = command
            .get_envs()
            .filter(|(k, _)| *k == std::ffi::OsStr::new("HOME"))
            .map(|(k, v)| (k.to_os_string(), v.map(std::ffi::OsStr::to_os_string)))
            .collect();
        assert_eq!(
            passed,
            vec![(
                std::ffi::OsString::from("HOME"),
                Some(std::ffi::OsString::from("/chosen-home"))
            )],
            "an inherited name overrode a value this process chose"
        );
    }

    #[test]
    fn a_failure_message_carries_a_bounded_tail_of_stderr() {
        assert_eq!(stderr_tail(b""), "");
        assert_eq!(stderr_tail(b"   \n  "), "");
        let tail = stderr_tail(b"line1\nline2\nline3");
        assert!(tail.contains("line3"), "{tail}");

        let noisy: Vec<String> = (0..500).map(|i| format!("line {i}")).collect();
        let tail = stderr_tail(noisy.join("\n").as_bytes());
        assert!(tail.contains("line 499"), "the tail must be the end");
        assert!(
            !tail.contains("line 100"),
            "and must not be the whole thing"
        );
    }

    /// The ceiling exists so a runaway analyzer cannot exhaust memory, and it
    /// has to sit far above any real report — a large monorepo scan is tens of
    /// megabytes of JSON, so anything under that would refuse honest work.
    #[test]
    fn the_output_ceiling_is_a_ceiling_not_a_target() {
        assert_eq!(MAX_OUTPUT_BYTES, 256 << 20);
    }

    /// Issue #430, asserted on the text the reader actually gets.
    ///
    /// Three things at once, because the refusal has to do all three and the
    /// checklist's failure modes are each of them alone: name the obstacle, name
    /// the *install* (not a rebuild, not a prefetch — this is a third-party
    /// binary), and keep the alternative that needs no install at all.
    #[test]
    fn a_missing_binary_names_the_install_and_keeps_the_ingest_alternative() {
        let message = SubprocessError::BinaryNotFound {
            program: "semgrep".to_owned(),
            analyzer: "semgrep".to_owned(),
            install: crate::adapter::install_hint("semgrep"),
        }
        .to_string();

        assert!(message.contains("not found on PATH"), "{message}");
        assert!(message.contains("so nothing ran"), "{message}");
        assert!(message.contains("pipx install semgrep"), "{message}");
        assert!(
            message.contains("https://docs.semgrep.dev/getting-started/quickstart"),
            "a command ages; the page it came from does not: {message}"
        );
        assert!(
            message.contains("roteiro security ingest"),
            "seam (c) is the best part of the old message and must survive: {message}"
        );
        // The rule that separates naming a way forward from taking it. Nothing
        // here runs an installer, and the message must not read as though
        // something did.
        assert!(message.contains("has not installed this one"), "{message}");
    }

    /// The refusal follows the program, not the analyzer.
    ///
    /// `cargo-audit`'s invocation is `cargo`, so this is the message a reader
    /// with no Rust toolchain gets — and `cargo install cargo-audit` would be
    /// useless to them. The wrong *kind* of way forward is the failure that has
    /// shipped here before, so it is asserted rather than assumed.
    #[test]
    fn the_hint_follows_the_missing_program_not_the_analyzer() {
        let message = SubprocessError::BinaryNotFound {
            program: "cargo".to_owned(),
            analyzer: "cargo-audit".to_owned(),
            install: crate::adapter::install_hint("cargo"),
        }
        .to_string();

        assert!(message.contains("https://rustup.rs"), "{message}");
        assert!(
            !message.contains("cargo install cargo-audit"),
            "a reader with no cargo cannot run a cargo subcommand install: {message}"
        );
    }

    /// `docs/OFFLINE_SETUP.md` quotes this refusal **verbatim**, so the message
    /// and the document are one contract and this is what holds them together.
    ///
    /// The document whose job is telling someone how to prepare is the worst
    /// place for a stale quote: it is read by a person who cannot check it,
    /// because they are reading it precisely because the tool is not in front of
    /// them yet. Editing the message alone fails here, which is the point —
    /// `lint_sandbox`'s Dockerfile-count test is the same guard for the same
    /// reason.
    ///
    /// **The quoted block, not the file.** The first version of this searched
    /// the whole document for each line, and passed over a corrupted quote
    /// because the same install command appears further down in the
    /// install-commands block — a guard that sampled something cheaper than what
    /// it claimed to check. The block is located by its opening line and
    /// compared whole.
    #[test]
    fn the_document_quotes_this_refusal_as_it_is_now() {
        let doc = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/OFFLINE_SETUP.md")
            .canonicalize()
            .expect("the document that quotes the refusal must exist");
        let text = std::fs::read_to_string(&doc).expect("readable");

        let rendered = SubprocessError::BinaryNotFound {
            program: "semgrep".to_owned(),
            analyzer: "semgrep".to_owned(),
            install: crate::adapter::install_hint("semgrep"),
        }
        .to_string();

        let quoted = text
            .split("```")
            .find(|block| block.trim_start().starts_with("Error: analyzer binary"))
            .unwrap_or_else(|| {
                panic!(
                    "{} no longer quotes this refusal at all — the document that tells \
                     someone how to prepare must show what they will actually see",
                    doc.display()
                )
            })
            .trim();

        assert_eq!(
            quoted,
            format!("Error: {rendered}").trim(),
            "the quote in {} has drifted from the message",
            doc.display()
        );

        // The sentence the old quote ended on, anywhere in the file. A reader
        // who finds it has been told to install something and not told how,
        // which is the whole of #430.
        assert!(
            !text.contains("install it yourself"),
            "{} still carries the refusal wording #430 replaced",
            doc.display()
        );
    }

    /// A program no adapter declares says less rather than guessing — and still
    /// keeps the alternative, because that one is true regardless.
    #[test]
    fn an_unknown_program_admits_it_rather_than_inventing_a_command() {
        let message = SubprocessError::BinaryNotFound {
            program: "some-future-analyzer".to_owned(),
            analyzer: "future".to_owned(),
            install: crate::adapter::install_hint("some-future-analyzer"),
        }
        .to_string();

        assert!(message.contains("knows no install command"), "{message}");
        assert!(message.contains("roteiro security ingest"), "{message}");
    }
}

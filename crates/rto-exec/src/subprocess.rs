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

use rto_graph::{Isolation, RunnerKind};

use crate::adapter::{Adapter, AssetPaths, Invocation, NativeContext};
use crate::assets;
use crate::clock::rfc3339_utc;
use crate::ingest::assemble;
use crate::runner::{AnalysisRequest, AnalysisResponse, AnalyzerRunner, ExecError, check_request};
use crate::snippet::WorktreeSnippets;

/// The largest analyzer report that will be read into memory.
///
/// A ceiling, not a target. `MAX_REPORT_FINDINGS` bounds the report once it is
/// parsed; this bounds it before, so a runaway analyzer cannot exhaust memory on
/// the way there.
pub const MAX_OUTPUT_BYTES: usize = 256 << 20;

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
    #[error(
        "analyzer binary `{program}` not found on PATH (needed to run `{analyzer}`). Roteiro does \
         not install analyzers; install it yourself, or produce the report elsewhere and use \
         `roteiro security ingest`."
    )]
    BinaryNotFound {
        /// The program that was looked for.
        program: String,
        /// The analyzer it belongs to.
        analyzer: String,
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
        let output = execute(&invocation, &request.worktree.path, &request.analyzer)?;
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
struct Captured {
    stdout: Vec<u8>,
    status: i32,
}

/// Run the analyzer and capture its stdout.
fn execute(
    invocation: &Invocation,
    worktree: &std::path::Path,
    analyzer: &str,
) -> Result<Captured, SubprocessError> {
    let mut command = Command::new(&invocation.program);
    command
        .args(&invocation.args)
        .current_dir(worktree)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    scrub_environment(&mut command);

    let output = command.output().map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            SubprocessError::BinaryNotFound {
                program: invocation.program.clone(),
                analyzer: analyzer.to_owned(),
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
        status,
    })
}

/// The last few lines of an analyzer's standard error, for a failure message.
///
/// Bounded, because a failing analyzer can be extremely talkative and a wall of
/// output in an error message hides the error.
fn stderr_tail(stderr: &[u8]) -> String {
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

/// Give the child a minimal, explicit environment.
///
/// A third-party binary running on a developer's machine inherits everything by
/// default, and a developer's environment is where `GITHUB_TOKEN`, `AWS_*`,
/// `SEMGREP_APP_TOKEN` and an SSH agent socket live. None of that is an analyzer
/// input, so none of it is passed. `PATH` is kept because the analyzer needs to
/// find its own helpers, and `HOME` because tools that cannot locate a home
/// directory fail in confusing ways; both are the parent's, unchanged.
///
/// This is a *reduction* in what the process can reach, not a boundary. It stops
/// an analyzer from picking up a credential by accident; it does not stop one
/// that goes looking.
fn scrub_environment(command: &mut Command) {
    command.env_clear();
    for key in [
        "PATH",
        "HOME",
        "USERPROFILE",
        "SystemRoot",
        "TMPDIR",
        "TEMP",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    // Deterministic, locale-independent output from anything that formats
    // numbers or sorts strings.
    command.env("LC_ALL", "C");
    // Semgrep reads this to decide whether to phone home. The argv says
    // `--metrics=off` too; saying it twice costs nothing and the environment is
    // the one an upgrade is less likely to rename.
    command.env("SEMGREP_SEND_METRICS", "off");
}

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
    scrub_environment(&mut command);

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
        MAX_OUTPUT_BYTES, SubprocessError, SubprocessRunner, scrub_environment, stderr_tail,
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
        scrub_environment(&mut command);
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
}

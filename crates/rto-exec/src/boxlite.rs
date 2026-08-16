//! The sandboxed backend: run the analyzer inside a digest-pinned microVM.
//!
//! ADR-0014 calls this "the reproducible local path". Unlike
//! [`crate::subprocess`], the guarantees here are **enforced by a boundary**
//! rather than configured on a cooperating process:
//!
//! - **Egress is denied by the hypervisor.** `NetworkSpec::Disabled` means the
//!   guest is brought up with no network interface at all — no `eth0`, and the
//!   userspace network proxy is never started. An analyzer inside cannot open a
//!   socket to anywhere, whatever it would like to do. That is what lets this
//!   backend record `network: Deny` and mean it, where the subprocess backend has
//!   to document that it means *configured off*.
//! - **The worktree is mounted read-only.** A write attempt fails with
//!   `EROFS` from the guest kernel, not from an analyzer's good manners.
//! - **The environment is not inherited at all.** A microVM does not share the
//!   host's environment block, so there is nothing to scrub: the guest sees
//!   exactly the variables passed to it. Ambient credentials cannot leak because
//!   they are never in the same address space, let alone the same kernel.
//!
//! # What makes findings identical to a subprocess run
//!
//! Nothing in this file parses analyzer output. It captures the analyzer's
//! stdout and hands those bytes to **the same [`crate::Adapter`]** the
//! subprocess backend and `roteiro security ingest` use, then to the same
//! [`crate::ingest::assemble`]. Equality of the resulting findings is therefore a
//! property of the code rather than a coincidence two test runs happened to
//! share (ADR-0012).
//!
//! Two details exist purely to keep it that way:
//!
//! - The rule set is the **same file**, mounted read-only into the guest, so the
//!   `rules_digest` on both runs is one digest of one artifact.
//! - Paths are relativised against the *guest* worktree root, so an analyzer
//!   that reports absolute paths yields `src/tls.rs` on both sides rather than
//!   `/work/src/tls.rs` here and `/home/you/repo/src/tls.rs` there. A finding key
//!   that embedded either would differ between backends *and* between machines.
//!
//! # What is not enforced here
//!
//! The image is pinned by digest, and this backend refuses to run against an
//! image that is not already in the local store — it never pulls implicitly, the
//! same rule the asset cache follows. But the *runtime* that starts the microVM
//! is a prebuilt binary that `boxlite` embeds; its integrity is established at
//! build time by [`crate::runtime_pins`] and `build.rs`, not here.
//!
//! @rto:0014
//! @rto:0012

use std::path::{Path, PathBuf};

use boxlite::runtime::options::VolumeSpec;
use boxlite::{
    BoxCommand, BoxOptions, BoxliteOptions, BoxliteRuntime, LiteBox, NetworkSpec, RootfsSpec,
};
use futures::StreamExt as _;
use rto_graph::{Isolation, RunnerKind};

use crate::adapter::{Adapter, AssetPaths, Invocation, NativeContext};
use crate::assets;
use crate::clock::rfc3339_utc;
use crate::ingest::assemble;
use crate::runner::{AnalysisRequest, AnalysisResponse, AnalyzerRunner, ExecError, check_request};
use crate::snippet::WorktreeSnippets;

/// Where the analyzed worktree is mounted inside the guest.
///
/// A fixed path, and deliberately not the host's: a finding's location must not
/// depend on where the checkout happens to live, and pinning the guest side is
/// what makes two machines produce the same key for the same code.
pub const GUEST_WORKTREE: &str = "/work";

/// Where the pinned asset cache is mounted inside the guest, read-only.
pub const GUEST_ASSETS: &str = "/assets";

/// The largest analyzer report that will be read into memory.
///
/// Matches [`crate::subprocess::MAX_OUTPUT_BYTES`] — the ceiling is a property
/// of what Roteiro will hold, not of how the analyzer was launched, and a test
/// asserts the two agree rather than trusting this comment.
pub const MAX_OUTPUT_BYTES: usize = 256 << 20;

/// How long an analyzer may run inside the guest before it is killed.
///
/// A microVM that wedges has no terminal to interrupt, so an unbounded wait
/// would hang a CI job rather than fail it. Generous enough for a real scan of a
/// large tree.
pub const EXEC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// A pinned OCI image an analyzer runs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SandboxImage {
    /// The analyzer it provides.
    pub analyzer: &'static str,
    /// The fully-qualified, digest-pinned reference. A tag is never used: a tag
    /// is a mutable pointer, and "reproducible" and "mutable pointer" cannot
    /// both be true.
    pub reference: &'static str,
    /// The manifest digest, recorded on every run this image produces.
    pub digest: &'static str,
    /// The analyzer version the image carries, which must match what a
    /// subprocess run on the same machine would report for the two to be
    /// comparable.
    pub analyzer_version: &'static str,
}

/// Every analyzer this build can run sandboxed.
///
/// Short by design. An analyzer earns an entry here when there is a published
/// image whose contents can be pinned by digest *and* whose analyzer version is
/// knowable — `cargo-audit` has no official image, and inventing one would make
/// Roteiro the publisher of a security tool's container, which is not a job it
/// is taking on.
pub static SANDBOX_IMAGES: &[SandboxImage] = &[SandboxImage {
    analyzer: crate::adapter::semgrep::ANALYZER,
    // Multi-arch index digest: the same reference resolves on linux/amd64 and
    // linux/arm64, so CI and an Apple Silicon laptop pin one identifier rather
    // than two that could drift apart.
    reference: "docker.io/semgrep/semgrep@sha256:67319956da3dcb58baf5b322899c15458e3963e7018a86aeeb5cd224e69cb77a",
    digest: "sha256:67319956da3dcb58baf5b322899c15458e3963e7018a86aeeb5cd224e69cb77a",
    analyzer_version: "1.173.0",
}];

/// The pinned image for `analyzer`, or `None` if this build has none.
#[must_use]
pub fn image_for(analyzer: &str) -> Option<&'static SandboxImage> {
    SANDBOX_IMAGES.iter().find(|i| i.analyzer == analyzer)
}

/// Whether this host can start a microVM at all.
///
/// The reason for `Unavailable` is meant to be *printed*: it is what a skipped
/// sandbox test says out loud, so a run that quietly covered nothing cannot be
/// mistaken for a run that passed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxProbe {
    /// A microVM can be started here.
    Available,
    /// It cannot, for this reason.
    Unavailable(String),
}

impl SandboxProbe {
    /// Whether a sandboxed run can be attempted.
    #[must_use]
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }

    /// The reason a sandbox is unavailable, if it is.
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Available => None,
            Self::Unavailable(why) => Some(why),
        }
    }
}

/// Ask the host whether it can run a microVM.
///
/// This is ADR-0014's runtime capability probe, and the reason `--all-features`
/// can be built and tested on a CI runner with no `/dev/kvm`: the sandbox tests
/// consult it and skip with a visible message rather than failing.
///
/// It is a real check, not a platform guess — `SystemCheck::run()` opens
/// `/dev/kvm` on Linux and probes `Hypervisor.framework` on macOS, so a machine
/// that has the device but cannot use it (no permission, nested virtualisation
/// disabled, another VMM holding it) is reported unavailable rather than
/// discovered to be broken halfway through a scan.
#[must_use]
pub fn sandbox_probe() -> SandboxProbe {
    match boxlite::system_check::SystemCheck::run() {
        Ok(_) => SandboxProbe::Available,
        Err(e) => SandboxProbe::Unavailable(e.to_string()),
    }
}

/// Something went wrong running the analyzer in a sandbox, as opposed to
/// something being wrong with what it produced.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SandboxError {
    /// This host cannot start a microVM.
    #[error(
        "no sandbox is available on this host: {reason}\n  \
         run the analyzer with `--allow-unsandboxed` to accept an unisolated run \
         (its evidence will record isolation=none), or ingest a report produced elsewhere."
    )]
    Unavailable {
        /// What the capability probe reported.
        reason: String,
    },
    /// This build has no pinned image for the analyzer.
    #[error(
        "no pinned sandbox image for analyzer {requested:?} in this build (available: {known})"
    )]
    NoImage {
        /// The analyzer that was asked for.
        requested: String,
        /// The analyzers that do have an image.
        known: String,
    },
    /// The pinned image is not in the local store, and this backend does not
    /// pull implicitly.
    #[error(
        "assets-unavailable-offline: the pinned image for {analyzer} is not in the local store\n  \
         image: {reference}\n  \
         fetch it with: roteiro security prefetch --allow-download\n  \
         (roteiro never pulls an image during a run, so a scan can never depend on \
         a registry being reachable)"
    )]
    ImageNotProvisioned {
        /// The analyzer whose run was refused.
        analyzer: String,
        /// The digest-pinned reference that is missing.
        reference: &'static str,
    },
    /// The sandbox itself failed at some stage.
    #[error("sandbox {stage}: {message}")]
    Runtime {
        /// What was being attempted — `create`, `exec`, `pull`.
        stage: &'static str,
        /// What boxlite reported.
        message: String,
    },
    /// The analyzer exited with a status it does not use for "ran successfully".
    #[error(
        "`{program}` exited with status {status} inside the sandbox, which it does not use for a \
         completed scan (expected one of: {expected}). A scan that failed is not a clean result, \
         so nothing was stored.{stderr}"
    )]
    UnexpectedStatus {
        /// The program that ran.
        program: String,
        /// The status it exited with.
        status: i32,
        /// The statuses the adapter declared usable.
        expected: String,
        /// The tail of its standard error, prefixed for display.
        stderr: String,
    },
    /// The analyzer produced more output than will be read.
    #[error("`{program}` produced more than {max} bytes of output in the sandbox; refusing to read it")]
    OutputTooLarge {
        /// The program that ran.
        program: String,
        /// The ceiling.
        max: usize,
    },
}

/// Executes an analyzer inside a digest-pinned OCI image in a microVM.
pub struct BoxliteRunner {
    adapter: &'static dyn Adapter,
    image: &'static SandboxImage,
    /// Host paths of the resolved assets — what `rules_digest` is read from.
    assets: Vec<(&'static str, PathBuf)>,
    /// The same assets as the guest sees them, which is what the argv names.
    guest_assets: Vec<(&'static str, PathBuf)>,
    assets_root: PathBuf,
    runtime: tokio::runtime::Runtime,
}

impl std::fmt::Debug for BoxliteRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoxliteRunner")
            .field("adapter", &self.adapter)
            .field("image", &self.image)
            .field("assets_root", &self.assets_root)
            .finish_non_exhaustive()
    }
}

impl BoxliteRunner {
    /// Build a runner for `analyzer`, resolving its pinned assets under `root`.
    ///
    /// Everything that can be refused is refused here, before a VM is started:
    /// the capability probe, the pinned image, and the asset cache. A cold cache
    /// or an absent hypervisor therefore costs nothing and says so precisely.
    ///
    /// # Errors
    /// Returns [`ExecError::UnknownAnalyzer`] if this build cannot normalise the
    /// analyzer, [`ExecError::Sandbox`] if there is no sandbox or no pinned image
    /// for it, or [`ExecError::AssetsUnavailableOffline`] if its pinned inputs
    /// are not provisioned.
    pub fn new(analyzer: &str, assets_root: &Path) -> Result<Self, ExecError> {
        let adapter =
            crate::adapter::adapter_for(analyzer).ok_or_else(|| ExecError::UnknownAnalyzer {
                requested: analyzer.to_owned(),
                known: crate::adapter::known_analyzers().join(", "),
            })?;

        if let SandboxProbe::Unavailable(reason) = sandbox_probe() {
            return Err(SandboxError::Unavailable { reason }.into());
        }

        let image = image_for(analyzer).ok_or_else(|| SandboxError::NoImage {
            requested: analyzer.to_owned(),
            known: SANDBOX_IMAGES
                .iter()
                .map(|i| i.analyzer)
                .collect::<Vec<_>>()
                .join(", "),
        })?;

        let assets = assets::resolve(assets_root, analyzer)?;
        let guest_assets = assets
            .iter()
            .map(|(id, host)| {
                let relative = host.strip_prefix(assets_root).unwrap_or(host.as_path());
                (*id, Path::new(GUEST_ASSETS).join(relative))
            })
            .collect();

        // A current-thread runtime: this backend does one thing at a time and
        // must not quietly acquire a thread pool inside a CLI that has none.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| SandboxError::Runtime {
                stage: "runtime",
                message: e.to_string(),
            })?;

        Ok(Self {
            adapter,
            image,
            assets,
            guest_assets,
            assets_root: assets_root.to_path_buf(),
            runtime,
        })
    }

    /// The adapter this runner drives.
    #[must_use]
    pub fn adapter(&self) -> &'static dyn Adapter {
        self.adapter
    }

    /// The image this runner executes in.
    #[must_use]
    pub fn image(&self) -> &'static SandboxImage {
        self.image
    }

    /// The invocation this runner will execute, with guest-side asset paths.
    #[must_use]
    pub fn invocation(&self) -> Invocation {
        self.adapter.command(&AssetPaths::new(&self.guest_assets))
    }

    /// The digest recorded for the analyzer's rule set.
    ///
    /// Read from the **host** copy, which is the same file the guest reads
    /// through a read-only mount — so the digest a sandboxed run stamps is the
    /// digest of the artifact it actually used, and is the same value a
    /// subprocess run would stamp.
    fn rules_digest(&self) -> Option<String> {
        self.assets.iter().find_map(|(id, _)| {
            let spec = assets::asset(id)?;
            (spec.kind == assets::AssetKind::Rules)
                .then(|| assets::installed(&self.assets_root, spec).map(|record| record.digest))
                .flatten()
        })
    }

    /// Where boxlite keeps its image store and per-box state.
    ///
    /// Under the asset cache rather than boxlite's own default, so a test cache
    /// and the user's cache never mix — the same discipline
    /// [`crate::subprocess::SubprocessRunner`] applies to asset roots.
    fn boxlite_home(&self) -> PathBuf {
        self.assets_root.join("boxlite-home")
    }

    fn open_runtime(&self) -> Result<BoxliteRuntime, SandboxError> {
        BoxliteRuntime::new(BoxliteOptions {
            home_dir: self.boxlite_home(),
            image_registries: Vec::new(),
        })
        .map_err(|e| SandboxError::Runtime {
            stage: "open",
            message: e.to_string(),
        })
    }
}

impl AnalyzerRunner for BoxliteRunner {
    fn kind(&self) -> RunnerKind {
        RunnerKind::Sandboxed
    }

    fn isolation(&self) -> Isolation {
        Isolation::MicroVm
    }

    fn run(&self, request: &AnalysisRequest) -> Result<AnalysisResponse, ExecError> {
        check_request(request)?;

        let invocation = self.invocation();
        let started_at = rfc3339_utc(std::time::SystemTime::now());
        let output = self.runtime.block_on(self.execute(&invocation, request))?;
        let ended_at = rfc3339_utc(std::time::SystemTime::now());

        let snippets = WorktreeSnippets::new(&request.worktree.path);
        let ctx = NativeContext {
            started_at,
            ended_at,
            // The image's analyzer version rather than one asked for at run
            // time: the image is immutable, so what it carries is known without
            // spending a second VM boot to ask it.
            analyzer_version: Some(self.image.analyzer_version.to_owned()),
            exit_status: output.status,
            source: &request.source,
            rules_digest: self.rules_digest(),
            advisory_db: assets::advisory_db_evidence(&self.assets_root, &request.analyzer),
            // The *guest* root. An adapter whose analyzer reports absolute paths
            // relativises against this, so it yields the same repo-relative path
            // a subprocess run yields — see the module docs.
            worktree: Some(Path::new(GUEST_WORKTREE)),
            snippets: &snippets,
        };

        let mut report = self.adapter.normalize(&output.stdout, &ctx)?;
        // The one field only this backend can supply. Adapters hardcode `None`
        // because they parse analyzer output, and no analyzer knows what image
        // it was put inside.
        report.image_digest = Some(self.image.digest.to_owned());

        assemble(
            report,
            request,
            self.kind(),
            self.isolation(),
            &output.stdout,
        )
    }
}

/// What a completed sandboxed run produced.
struct Captured {
    stdout: Vec<u8>,
    status: i32,
}

impl BoxliteRunner {
    /// Start the microVM, run the analyzer, and capture its stdout.
    async fn execute(
        &self,
        invocation: &Invocation,
        request: &AnalysisRequest,
    ) -> Result<Captured, SandboxError> {
        let runtime = self.open_runtime()?;
        self.require_image(&runtime).await?;

        let options = BoxOptions {
            rootfs: RootfsSpec::Image(self.image.reference.to_owned()),
            // The boundary, not a configuration flag: no interface is created.
            network: NetworkSpec::Disabled,
            volumes: vec![
                VolumeSpec {
                    host_path: request.worktree.path.to_string_lossy().into_owned(),
                    guest_path: GUEST_WORKTREE.to_owned(),
                    read_only: true,
                },
                VolumeSpec {
                    host_path: self.assets_root.to_string_lossy().into_owned(),
                    guest_path: GUEST_ASSETS.to_owned(),
                    read_only: true,
                },
            ],
            env: guest_environment(),
            working_dir: Some(GUEST_WORKTREE.to_owned()),
            // Nothing survives the scan. A box left behind would be a second
            // copy of the worktree's mount configuration lying around.
            auto_remove: true,
            detach: false,
            ..Default::default()
        };

        let boxed = runtime
            .create(options, None)
            .await
            .map_err(|e| SandboxError::Runtime {
                stage: "create",
                message: e.to_string(),
            })?;

        let result = self.exec_in(&boxed, invocation).await;

        // Tear down whatever happened, and let the run's own error win: a
        // failure to stop a box must not mask the reason the scan failed.
        let stopped = boxed.stop().await;
        let shutdown = runtime.shutdown(Some(10)).await;
        let captured = result?;
        stopped.map_err(|e| SandboxError::Runtime {
            stage: "stop",
            message: e.to_string(),
        })?;
        shutdown.map_err(|e| SandboxError::Runtime {
            stage: "shutdown",
            message: e.to_string(),
        })?;
        Ok(captured)
    }

    /// Refuse unless the pinned image is already in the local store.
    ///
    /// The same rule the asset cache follows: provisioning downloads, running
    /// never does. A scan must not be able to fail because a registry was
    /// unreachable, nor to succeed by silently fetching something new.
    async fn require_image(&self, runtime: &BoxliteRuntime) -> Result<(), SandboxError> {
        let images = runtime
            .images()
            .map_err(|e| SandboxError::Runtime {
                stage: "images",
                message: e.to_string(),
            })?
            .list()
            .await
            .map_err(|e| SandboxError::Runtime {
                stage: "images",
                message: e.to_string(),
            })?;

        let present = images
            .iter()
            .any(|i| i.id == self.image.digest || i.reference == self.image.reference);
        if present {
            Ok(())
        } else {
            Err(SandboxError::ImageNotProvisioned {
                analyzer: self.adapter.analyzer().to_owned(),
                reference: self.image.reference,
            })
        }
    }

    /// Run one command in the guest and collect its streams.
    async fn exec_in(
        &self,
        boxed: &LiteBox,
        invocation: &Invocation,
    ) -> Result<Captured, SandboxError> {
        let command = BoxCommand::new(&invocation.program)
            .args(invocation.args.clone())
            .working_dir(GUEST_WORKTREE)
            .timeout(EXEC_TIMEOUT);

        let mut execution = boxed
            .exec(command)
            .await
            .map_err(|e| SandboxError::Runtime {
                stage: "exec",
                message: e.to_string(),
            })?;

        let stdout = execution.stdout();
        let stderr = execution.stderr();
        let out = tokio::spawn(collect(stdout));
        let err = tokio::spawn(collect(stderr));

        let status = execution
            .wait()
            .await
            .map_err(|e| SandboxError::Runtime {
                stage: "wait",
                message: e.to_string(),
            })?;

        let joined = |handle: tokio::task::JoinHandle<String>, what: &'static str| async move {
            handle.await.map_err(|e| SandboxError::Runtime {
                stage: what,
                message: e.to_string(),
            })
        };
        let stdout = joined(out, "stdout").await?;
        let stderr = joined(err, "stderr").await?;

        if stdout.len() > MAX_OUTPUT_BYTES {
            return Err(SandboxError::OutputTooLarge {
                program: invocation.program.clone(),
                max: MAX_OUTPUT_BYTES,
            });
        }

        if !invocation.success_statuses.contains(&status.exit_code) {
            return Err(SandboxError::UnexpectedStatus {
                program: invocation.program.clone(),
                status: status.exit_code,
                expected: invocation
                    .success_statuses
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", "),
                stderr: stderr_tail(&stderr),
            });
        }

        Ok(Captured {
            stdout: stdout.into_bytes(),
            status: status.exit_code,
        })
    }
}

/// Reassemble a guest stream into the bytes the analyzer wrote.
///
/// **Concatenated, never joined by lines.** boxlite decodes raw chunks off the
/// vsock and forwards whatever arrived; the chunk boundaries are wherever the
/// guest's writes happened to land, not newlines. Inserting a separator here
/// would corrupt any output that is not line-shaped — which is every analyzer
/// this crate reads, since they all emit one JSON document.
async fn collect(stream: Option<impl futures::Stream<Item = String> + Unpin>) -> String {
    let mut out = String::new();
    if let Some(mut stream) = stream {
        while let Some(chunk) = stream.next().await {
            out.push_str(&chunk);
            // Bounded here as well as after the fact: a runaway analyzer must
            // not be able to exhaust memory before anyone gets to check.
            if out.len() > MAX_OUTPUT_BYTES {
                break;
            }
        }
    }
    out
}

/// The environment the guest process gets.
///
/// Built up rather than filtered down. [`crate::subprocess`] has to *remove*
/// things, because a child process inherits its parent's environment by default
/// and the risk is forgetting one. A guest inherits nothing, so the only
/// variables that exist are the ones named here — which makes "no ambient
/// credentials" structural rather than a list that has to stay complete.
fn guest_environment() -> Vec<(String, String)> {
    vec![
        // Deterministic, locale-independent formatting and sorting.
        ("LC_ALL".to_owned(), "C".to_owned()),
        // Semgrep reads this to decide whether to phone home. It cannot reach
        // anything from here regardless; saying so costs nothing and keeps the
        // two backends' configurations identical.
        ("SEMGREP_SEND_METRICS".to_owned(), "off".to_owned()),
    ]
}

/// The last few lines of an analyzer's standard error, for a failure message.
///
/// Mirrors [`crate::subprocess`]'s bound: a failing analyzer can be extremely
/// talkative, and a wall of output in an error message hides the error.
fn stderr_tail(stderr: &str) -> String {
    const MAX_LINES: usize = 8;
    const MAX_BYTES: usize = 4_000;
    let trimmed = stderr.trim_end();
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

/// Pull the pinned image for `analyzer` into the local store.
///
/// The **only** function in this module that can reach a network, and it is
/// never called by a run — `roteiro security prefetch --allow-download` calls
/// it, which is the same split the asset cache uses: provisioning fetches,
/// running reads.
///
/// # Errors
/// Returns [`SandboxError::NoImage`] if this build has no image for the
/// analyzer, [`SandboxError::Unavailable`] if no sandbox exists here, or
/// [`SandboxError::Runtime`] if the pull fails.
pub fn provision_image(analyzer: &str, assets_root: &Path) -> Result<String, SandboxError> {
    let image = image_for(analyzer).ok_or_else(|| SandboxError::NoImage {
        requested: analyzer.to_owned(),
        known: SANDBOX_IMAGES
            .iter()
            .map(|i| i.analyzer)
            .collect::<Vec<_>>()
            .join(", "),
    })?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| SandboxError::Runtime {
            stage: "runtime",
            message: e.to_string(),
        })?;

    runtime.block_on(async {
        let boxlite = BoxliteRuntime::new(BoxliteOptions {
            home_dir: assets_root.join("boxlite-home"),
            image_registries: Vec::new(),
        })
        .map_err(|e| SandboxError::Runtime {
            stage: "open",
            message: e.to_string(),
        })?;
        boxlite
            .images()
            .map_err(|e| SandboxError::Runtime {
                stage: "images",
                message: e.to_string(),
            })?
            .pull(image.reference)
            .await
            .map_err(|e| SandboxError::Runtime {
                stage: "pull",
                message: e.to_string(),
            })?;
        Ok(image.digest.to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::{
        GUEST_ASSETS, GUEST_WORKTREE, MAX_OUTPUT_BYTES, SANDBOX_IMAGES, guest_environment,
        image_for, sandbox_probe, stderr_tail,
    };

    /// Every pinned image must name a digest, and the reference must *be* that
    /// digest — a tag would make the "reproducible" claim false, and a
    /// reference and digest that disagree would stamp evidence describing an
    /// image that was not run.
    #[test]
    fn every_pinned_image_is_addressed_by_its_recorded_digest() {
        assert!(!SANDBOX_IMAGES.is_empty());
        for image in SANDBOX_IMAGES {
            assert!(
                image.digest.starts_with("sha256:"),
                "{} digest is not a sha256 reference: {}",
                image.analyzer,
                image.digest
            );
            assert!(
                image.reference.ends_with(image.digest),
                "{} is not pinned to the digest it records: {} vs {}",
                image.analyzer,
                image.reference,
                image.digest
            );
            assert!(
                !image.reference.contains(':') || image.reference.contains('@'),
                "{} must be pinned by digest, not by tag: {}",
                image.analyzer,
                image.reference
            );
            assert!(
                !image.analyzer_version.is_empty(),
                "{} does not say what version it carries",
                image.analyzer
            );
        }
    }

    /// The registry is what `image_for` answers from, so an entry nobody can
    /// look up would be an image that silently never runs.
    #[test]
    fn the_image_registry_answers_for_every_analyzer_it_lists() {
        for image in SANDBOX_IMAGES {
            assert_eq!(image_for(image.analyzer).expect("registered"), image);
        }
        assert!(image_for("no-such-analyzer").is_none());
    }

    /// The guest environment is built up, not filtered down — so this checks the
    /// *absence* of everything a developer's shell carries, which is the
    /// property that matters, rather than the presence of what we wrote.
    #[test]
    fn the_guest_environment_carries_no_ambient_credentials() {
        let env = guest_environment();
        let names: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        for secret in [
            "GITHUB_TOKEN",
            "AWS_ACCESS_KEY_ID",
            "SEMGREP_APP_TOKEN",
            "SSH_AUTH_SOCK",
            "HOME",
            "PATH",
        ] {
            assert!(!names.contains(&secret), "{secret} reaches the guest");
        }
        assert!(names.contains(&"LC_ALL"));
    }

    /// Both mount points must be absolute and distinct, or one would shadow the
    /// other and the analyzer would read the wrong tree.
    #[test]
    fn the_guest_mount_points_are_absolute_and_distinct() {
        assert!(GUEST_WORKTREE.starts_with('/'));
        assert!(GUEST_ASSETS.starts_with('/'));
        assert_ne!(GUEST_WORKTREE, GUEST_ASSETS);
        assert!(!GUEST_ASSETS.starts_with(&format!("{GUEST_WORKTREE}/")));
        assert!(!GUEST_WORKTREE.starts_with(&format!("{GUEST_ASSETS}/")));
    }

    /// The ceiling is a property of what Roteiro will hold in memory, not of how
    /// the analyzer was launched — so the two backends must agree on it.
    #[cfg(feature = "exec-subprocess")]
    #[test]
    fn both_backends_bound_analyzer_output_identically() {
        assert_eq!(MAX_OUTPUT_BYTES, crate::subprocess::MAX_OUTPUT_BYTES);
    }

    #[test]
    fn a_failure_message_carries_a_bounded_tail_of_stderr() {
        assert_eq!(stderr_tail(""), "");
        assert_eq!(stderr_tail("   \n  "), "");
        assert!(stderr_tail("line1\nline2\nline3").contains("line3"));

        let noisy: Vec<String> = (0..500).map(|i| format!("line {i}")).collect();
        let tail = stderr_tail(&noisy.join("\n"));
        assert!(tail.contains("line 499"), "the tail must be the end");
        assert!(!tail.contains("line 100"), "and not the whole thing");
    }

    /// The probe must answer rather than panic, whatever the host is. A probe
    /// that blew up on an unsupported machine would take `--all-features` down
    /// with it, which is the exact failure it exists to prevent.
    #[test]
    fn the_capability_probe_always_answers() {
        let probe = sandbox_probe();
        match &probe {
            super::SandboxProbe::Available => assert!(probe.reason().is_none()),
            super::SandboxProbe::Unavailable(why) => {
                assert!(!why.is_empty(), "an unavailable sandbox must say why");
                assert_eq!(probe.reason(), Some(why.as_str()));
            }
        }
    }
}

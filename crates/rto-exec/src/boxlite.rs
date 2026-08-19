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
//!   **This one is not currently exercised, and saying so is the point.** The
//!   only analyzer with a pinned image is `semgrep`, which reports paths
//!   relative to its working directory — so nothing consults `worktree` today.
//!   Fault injection confirmed it: setting it to `None` leaves the parity test
//!   green. It is here because `osv-scanner` reports absolute paths and is the
//!   next candidate for an image, and it will be load-bearing the moment one
//!   exists. Until then it is an untested claim, not a verified one.
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
use crate::child_env::ChildEnv;
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

/// The guest's init process: something that does nothing, slowly.
///
/// **This is load-bearing, and the reason is not obvious.** A box lives exactly
/// as long as its init process, and the init process is the image's own
/// `ENTRYPOINT`. An analyzer image's entrypoint *is* the analyzer — so left
/// alone, `semgrep` runs with no arguments, prints its usage, exits, and takes
/// the box down with it. Any `exec` in flight at that moment is killed by
/// `SIGKILL` and reports `-9` with empty stderr, which reads exactly like an
/// out-of-memory kill and is not one.
///
/// So the entrypoint is replaced with a shell that waits, and the analyzer runs
/// as an `exec` inside the box that is then guaranteed to outlive it. `sh` is the
/// one binary every analyzer image can be relied on to have; the loop is used
/// rather than `sleep infinity` because busybox's `sleep` does not always accept
/// it.
pub const GUEST_INIT: &[&str] = &["sh", "-c", "while : ; do sleep 86400 ; done"];

/// Memory given to the guest, in MiB.
///
/// boxlite's default is 2048, and `semgrep` is killed by the guest OOM killer at
/// that size — it exits `-9` with **empty stderr**, because the process that
/// would have written the message is the one that was killed. That failure is
/// indistinguishable from a crash unless you already know to look for it, which
/// is why the number is named here with its reason rather than tuned silently.
///
/// A scan of a large tree is the memory-hungry case; this is sized for it.
pub const GUEST_MEMORY_MIB: u32 = 4096;

/// Virtual CPUs given to the guest.
///
/// boxlite's default is 1. Two, because `semgrep` parallelises across files and
/// a single core turns a large scan into a timeout — while more would start
/// competing with whatever else the developer is running.
pub const GUEST_CPUS: u8 = 2;

/// How long an analyzer may run inside the guest before it is killed.
///
/// A microVM that wedges has no terminal to interrupt, so an unbounded wait
/// would hang a CI job rather than fail it. Generous enough for a real scan of a
/// large tree.
pub const EXEC_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(30);

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
        Err(e) => SandboxProbe::Unavailable(probe_reason(&e.to_string())),
    }
}

/// Normalise a probe failure into something worth printing.
///
/// Split out from [`sandbox_probe`] so it can be tested on **any** host. Testing
/// it through the probe alone is untestable in the direction that matters: on a
/// machine that *has* a hypervisor the failure branch never runs, so a
/// regression that emptied the reason would sail past every developer with
/// working virtualisation and only surface on the CI runner it was meant to
/// serve. (Fault injection found exactly that: emptying the reason here left
/// the original test green on this machine.)
fn probe_reason(raw: &str) -> String {
    if raw.trim().is_empty() {
        // An unavailable sandbox that will not say why is worse than useless: a
        // skipped test would print nothing, and a skip with no reason is
        // indistinguishable from a test that ran.
        return "the hypervisor probe failed without giving a reason".to_owned();
    }
    raw.trim().to_owned()
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
    /// An image reference names a tag rather than a digest.
    ///
    /// Refused for user-supplied images as well as pinned ones, and the reason
    /// is **not** the reproducibility argument ADR-0020 retires for builders. It
    /// is that the image *is* the boundary. A builder's guest is where somebody
    /// else's build scripts execute, and a tag is a mutable pointer to it: the
    /// contents can be replaced by whoever controls the tag, with no version
    /// change and no notice, and the run would go on reporting success. You may
    /// choose your own boundary; you may not choose one that can be swapped
    /// under you.
    #[error(
        "the image for {what} is {reference:?}, which is a tag rather than a digest.\n           An image is where somebody else's build scripts execute, and a tag is a mutable \
         pointer to it — whoever controls the tag can replace what runs, with no version \
         change and no notice.\n           Pin it: docker.io/you/image@sha256:<64 hex>.\n           `docker buildx imagetools inspect <reference>` prints the digest; use the index \
         digest, so one reference resolves on both amd64 and arm64."
    )]
    ImageNotPinned {
        /// What wanted the image, so the reader knows which setting to change.
        what: String,
        /// The reference as it was written.
        reference: String,
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
    /// The analyzer was killed by a signal inside the guest, saying nothing.
    ///
    /// Its own diagnostic variant because the shape is deeply misleading: an
    /// out-of-memory kill inside a microVM surfaces as a negative status with
    /// **empty stderr**, since the process that would have explained itself is
    /// the one that was killed. Reported as `UnexpectedStatus` it reads as "the
    /// analyzer failed for unknown reasons", and the actual cause — a guest
    /// sized too small for the scan — never occurs to the reader.
    #[error(
        "`{program}` was killed by signal {signal} inside the sandbox and wrote nothing to \
         stderr.\n  The usual cause is the guest running out of memory: it is given \
         {memory_mib} MiB, and a large tree can need more.\n  This is not a finding — nothing \
         was stored, because a scan that was killed is not a clean result."
    )]
    Killed {
        /// The program that ran.
        program: String,
        /// The signal number it was killed by.
        signal: i32,
        /// How much memory the guest had.
        memory_mib: u32,
    },
    /// The analyzer produced more output than will be read.
    #[error(
        "`{program}` produced more than {max} bytes of output in the sandbox; refusing to read it"
    )]
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
            cpus: Some(GUEST_CPUS),
            memory_mib: Some(GUEST_MEMORY_MIB),
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
            // Replace the image's entrypoint so the box outlives the scan — see
            // `GUEST_INIT`. `cmd` is cleared for the same reason: the image's
            // own `CMD` is arguments for an entrypoint that is no longer there.
            entrypoint: Some(GUEST_INIT.iter().map(|s| (*s).to_owned()).collect()),
            cmd: Some(Vec::new()),
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
        if image_present(runtime, self.image).await? {
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

        let status = execution.wait().await.map_err(|e| SandboxError::Runtime {
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
            // A negative status is a signal, and a signal with nothing on stderr
            // is almost always the guest OOM killer. Say so.
            if status.exit_code < 0 && stderr.trim().is_empty() {
                return Err(SandboxError::Killed {
                    program: invocation.program.clone(),
                    signal: -status.exit_code,
                    memory_mib: GUEST_MEMORY_MIB,
                });
            }
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

/// The environment a **reader**'s guest process gets.
///
/// Nothing beyond the base every analyzer gets on either backend, because a
/// parse-only analyzer needs nothing located and nothing constrained — it is
/// handed a read-only tree and a rule file and told to read them.
///
/// It goes through [`ChildEnv`] rather than assembling a list of its own, and
/// that is the point rather than ceremony. This function used to build its own
/// vector while [`crate::subprocess`] had `ChildEnv`: two mechanisms for one
/// concept, which is the shape that let `CARGO_TARGET_DIR` be listed as a name
/// to *inherit* under a promise that it was *set* (ADR-0020 v1.4). One type,
/// two consumers, and the difference between the consumers written down at
/// [`ChildEnv::guest_pairs`].
fn guest_environment() -> Vec<(String, String)> {
    ChildEnv::default().guest_pairs()
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

/// Whether the pinned image for `analyzer` is already in the local store.
///
/// The question that has to be asked *before* a run rather than discovered
/// during one. `provision_image` downloads and a run refuses — but a caller that
/// only wants to know which of those applies had no way to ask, so "the image
/// was never pulled" could only surface as a failed scan. That is how it reached
/// [`crate::boxlite`]'s parity test as an error indistinguishable from a
/// regression: a missing setup step wearing the costume of a broken backend.
///
/// Reads the local store and nothing else. Asking never pulls.
///
/// # Errors
/// [`SandboxError::NoImage`] if this build has no image for the analyzer, or
/// [`SandboxError::Runtime`] if the local image store cannot be opened or
/// listed. A store that cannot be read is deliberately an error rather than
/// `Ok(false)`: "definitely absent" and "could not tell" are different answers,
/// and only the first one is safe for a caller to treat as a skip.
pub fn image_is_provisioned(analyzer: &str, assets_root: &Path) -> Result<bool, SandboxError> {
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
        image_present(&boxlite, image).await
    })
}

/// Whether `image` is in `runtime`'s local store.
///
/// One place that knows how presence is decided, so the pre-run probe and the
/// run's own refusal cannot come to different conclusions about the same store.
async fn image_present(
    runtime: &BoxliteRuntime,
    image: &SandboxImage,
) -> Result<bool, SandboxError> {
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

    Ok(images
        .iter()
        .any(|i| i.id == image.digest || i.reference == image.reference))
}

/// The digest an image reference is pinned to, or a refusal naming what to fix.
///
/// The one place the rule is applied, so a user-supplied builder image and a
/// [`SANDBOX_IMAGES`] entry are held to the same standard — the difference
/// between them is *who chose*, never *how strong the pin is*.
///
/// Checked structurally rather than by looking for an `@`: a reference may carry
/// a registry port (`host:5000/repo`) and a tag, so "contains a colon" and
/// "names a digest" are different questions and only one of them is this one.
///
/// # Errors
/// Returns [`SandboxError::ImageNotPinned`] if `reference` carries no
/// `@sha256:<64 hex>` suffix.
pub fn pinned_digest<'a>(what: &str, reference: &'a str) -> Result<&'a str, SandboxError> {
    let unpinned = || SandboxError::ImageNotPinned {
        what: what.to_owned(),
        reference: reference.to_owned(),
    };
    let (_, digest) = reference.rsplit_once('@').ok_or_else(unpinned)?;
    let hex = digest.strip_prefix("sha256:").ok_or_else(unpinned)?;
    // Length *and* alphabet: `@sha256:` followed by anything at all would
    // otherwise satisfy a prefix check while naming nothing.
    if hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(digest)
    } else {
        Err(unpinned())
    }
}

/// Open the local image store under `assets_root`, pulling nothing.
///
/// The same home directory every other entry point uses, so a test cache and
/// the user's cache never mix and two callers can never disagree about which
/// store the question was asked of.
fn open_store(assets_root: &Path) -> Result<BoxliteRuntime, SandboxError> {
    BoxliteRuntime::new(BoxliteOptions {
        home_dir: assets_root.join("boxlite-home"),
        image_registries: Vec::new(),
    })
    .map_err(|e| SandboxError::Runtime {
        stage: "open",
        message: e.to_string(),
    })
}

/// A current-thread runtime for one blocking call into boxlite's async API.
///
/// Current-thread on purpose, for [`BoxliteRunner::new`]'s reason: these entry
/// points are called from a CLI that has no reactor, and must not quietly
/// acquire a thread pool.
fn blocking<T>(
    body: impl std::future::Future<Output = Result<T, SandboxError>>,
) -> Result<T, SandboxError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| SandboxError::Runtime {
            stage: "runtime",
            message: e.to_string(),
        })?
        .block_on(body)
}

/// Pull a digest-pinned image into the local store.
///
/// The user-supplied counterpart of [`provision_image`], and it reaches a
/// network for the same reason and under the same rule: **provisioning fetches,
/// running reads.** No run calls this. `roteiro security prefetch
/// --allow-download` does, having first printed the reference it is about to
/// pull — because which registry this machine talks to is the operator's
/// business, and an image chosen in a committed `roteiro.toml` should be named
/// out loud before it is fetched rather than after it has run.
///
/// # Errors
/// Returns [`SandboxError::ImageNotPinned`] if `reference` is not pinned by
/// digest, or [`SandboxError::Runtime`] if the store cannot be opened or the
/// pull fails.
pub fn pull_reference(what: &str, reference: &str, assets_root: &Path) -> Result<(), SandboxError> {
    pinned_digest(what, reference)?;
    blocking(async {
        open_store(assets_root)?
            .images()
            .map_err(|e| SandboxError::Runtime {
                stage: "images",
                message: e.to_string(),
            })?
            .pull(reference)
            .await
            .map_err(|e| SandboxError::Runtime {
                stage: "pull",
                message: e.to_string(),
            })?;
        Ok(())
    })
}

/// Whether a digest-pinned image is already in the local store.
///
/// Reads the store and nothing else; asking never pulls. A store that cannot be
/// read is an error rather than `Ok(false)` for [`image_is_provisioned`]'s
/// reason: "definitely absent" and "could not tell" are different answers and
/// only the first is safe to act on.
///
/// # Errors
/// Returns [`SandboxError::ImageNotPinned`] if `reference` is not pinned by
/// digest, or [`SandboxError::Runtime`] if the local store cannot be read.
pub fn reference_is_present(
    what: &str,
    reference: &str,
    assets_root: &Path,
) -> Result<bool, SandboxError> {
    let digest = pinned_digest(what, reference)?.to_owned();
    blocking(async move {
        let images = open_store(assets_root)?
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
        Ok(images
            .iter()
            .any(|i| i.id == digest || i.reference == reference))
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

    /// An image reference is pinned by digest or it is refused, and this is the
    /// one place that decides it — for a [`SANDBOX_IMAGES`] entry and for a
    /// user-supplied builder image alike. The difference between them is *who
    /// chose*, never *how strong the pin is*.
    ///
    /// The rejections matter more than the acceptance. A prefix check would let
    /// `@sha256:` followed by anything through, and a "contains a colon" check
    /// would reject a registry port — so both the alphabet and the length are
    /// checked, and a port is not confused for a tag.
    #[test]
    fn an_image_is_pinned_by_digest_or_it_is_refused() {
        let hex = "a".repeat(64);
        for pinned in [
            format!("docker.io/library/rust@sha256:{hex}"),
            // A registry with a port, which contains a colon and is not a tag.
            format!("registry.internal:5000/team/rust-clippy@sha256:{hex}"),
            // A tag *and* a digest: the digest is what resolves, so this is
            // pinned. Refusing it would reject what `docker pull` prints.
            format!("docker.io/library/rust:1.97.1@sha256:{hex}"),
        ] {
            assert_eq!(
                super::pinned_digest("test", &pinned).expect("pinned"),
                format!("sha256:{hex}"),
                "{pinned}"
            );
        }

        for unpinned in [
            "docker.io/library/rust".to_owned(),
            "docker.io/library/rust:1.97.1".to_owned(),
            "registry.internal:5000/team/rust-clippy:latest".to_owned(),
            // Digest-shaped and not a digest: a prefix check would pass these.
            "x@sha256:".to_owned(),
            "x@sha256:deadbeef".to_owned(),
            format!("x@sha256:{}", "a".repeat(63)),
            format!("x@sha256:{}", "a".repeat(65)),
            format!("x@sha256:{}z", "a".repeat(63)),
            format!("x@sha512:{hex}"),
        ] {
            let err = super::pinned_digest("`[lint] image`", &unpinned)
                .expect_err(&format!("{unpinned} must be refused"));
            let message = err.to_string();
            // A refusal names what to change and shows the shape it wants —
            // this one is met by people who have only ever typed a tag.
            assert!(message.contains("`[lint] image`"), "{message}");
            assert!(message.contains("@sha256:"), "{message}");
            assert!(message.contains("imagetools inspect"), "{message}");
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

    /// An unavailable sandbox must always carry a printable reason — checked on
    /// the normalisation directly, because the branch that produces one never
    /// executes on a host that *has* a hypervisor.
    ///
    /// The skip message in the sandbox tests is the only thing standing between
    /// "covered nothing" and "passed", so an empty reason is a real defect and
    /// not a cosmetic one.
    #[test]
    fn an_unavailable_sandbox_always_gives_a_printable_reason() {
        assert_eq!(super::probe_reason("no /dev/kvm"), "no /dev/kvm");
        assert_eq!(super::probe_reason("  padded  "), "padded");
        for empty in ["", "   ", "\n\t "] {
            let reason = super::probe_reason(empty);
            assert!(
                !reason.trim().is_empty(),
                "an empty probe failure must still print something"
            );
            assert!(reason.contains("without giving a reason"), "{reason}");
        }
    }
}

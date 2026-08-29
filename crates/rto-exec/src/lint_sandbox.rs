//! ADR-0020 conditions 1 and 2: run the repository's own build **inside the
//! boundary**, so that `roteiro lint`'s sandbox-by-default has something to
//! select.
//!
//! `cargo clippy` has `cargo check` semantics: it executes every build script in
//! the resolved tree and loads every proc macro as a dylib into the compiler —
//! measured on this repository at 54 build scripts and 7 proc macros by default,
//! 87 and 33 under `--all-features`. Linting a branch you are reviewing runs its
//! author's code, and that the *toolchain* is yours does not make the *code*
//! yours. That is the inversion ADR-0020 is about: for a reader the sandbox
//! guards against a malicious analyzer, which is a weak threat; for a builder it
//! guards against a malicious repository, which is neither weak nor rare.
//!
//! # What is added, and what is emphatically not removed
//!
//! **An added mount, not a removed guarantee**, and the distinction is the whole
//! reason ADR-0020 could narrow ADR-0014's non-goal rather than withdraw it.
//!
//! | mount | mode | why |
//! |---|---|---|
//! | [`GUEST_WORKTREE`] | read-only | the tree under review is an input |
//! | [`GUEST_SCRATCH`] | **writable** | the one thing condition 1 adds |
//! | [`GUEST_CARGO_REGISTRY`] | read-only | condition 2's dependency mount |
//! | [`GUEST_CARGO_GIT`] | read-only | the same, for git dependencies |
//!
//! [`crate::check_request`]'s read-only preflight is **untouched**, and this
//! module never calls it — it takes a directory rather than an
//! [`crate::AnalysisRequest`], exactly as [`crate::lint`] does. A malicious build
//! script gets a directory that is discarded when the run ends. It does not get
//! the working tree, and it does not get the package cache. If a future change
//! here seems to need the preflight relaxed for readers, that is the conversion
//! ADR-0014 predicted and the answer is this mount table.
//!
//! # Condition 2 is a mount of what is already on the host
//!
//! ADR-0020 anticipated `cargo vendor` — 414 MB to 1.10 GB for this repository,
//! regenerated per lockfile change. Measured instead: with `CARGO_HOME` and the
//! source tree both `chmod -R a-w`, `CARGO_TARGET_DIR` outside both, and
//! `--locked --offline`, `cargo clippy` exits **0** and executes build scripts
//! compiled from the read-only cache. So the package cache is a read-only mount
//! of the host's own, egress stays denied, and nothing is vendored.
//!
//! **The failure that follows is a refusal, not a build error.** A guest with no
//! network cannot fetch what the host's cache does not already hold, and cargo
//! reports that from inside a machine the user cannot see. [`cold_cache`] turns
//! it into the one sentence that helps: run `cargo fetch` on the host first.
//!
//! # The package cache is mounted; `CARGO_HOME` is not
//!
//! `$CARGO_HOME` also contains `credentials.toml` — a crates.io API token — and
//! a `config.toml` that may carry registry tokens of its own. Mounting the root
//! would put those in front of the build scripts this module exists to contain,
//! and *"egress is denied"* is not an answer: the run's own stdout comes back to
//! the host, and `cargo::warning=` is a channel a build script can write to.
//!
//! So the two subdirectories that hold **packages** are mounted and the root is
//! not. That is a real narrowing rather than a formality, and it costs one
//! capability honestly named here: a `$CARGO_HOME/config.toml` that redirects a
//! source is not seen by the guest. A project's own `.cargo/config.toml` is,
//! because it is inside the worktree.
//!
//! # The image is supplied, never chosen here
//!
//! `SANDBOX_IMAGES` states the rule an analyzer must meet to earn a pinned
//! entry: a **published** image, addressable by digest, whose analyzer version is
//! knowable — *"inventing one would make Roteiro the publisher of a security
//! tool's container, which is not a job it is taking on."*
//!
//! No image meets it for `clippy`, and the reason is worth stating precisely
//! rather than left to be rediscovered: `rust-lang/docker-rust` builds **every**
//! stable and nightly variant with `rustup-init --profile minimal`, which
//! installs `rustc`, `cargo` and `rust-std` and stops. There is no first-party
//! Rust image carrying the `clippy` component. Choosing a third party's on the
//! user's behalf would make somebody else's container the boundary in which
//! somebody else's build scripts execute — selected by Roteiro, noticed by
//! nobody — and building one would make Roteiro the publisher of it.
//!
//! So the image is `[lint] image`, and an image without the linter in it is
//! [`BuilderError::ImageLacksLinter`]: a refusal that says how to build one,
//! rather than a `cargo clippy` that mysteriously reports "no such command".
//!
//! # The guest's rustc is not yours, and the report says so
//!
//! A lint name is a symbol in a compiler, so which lints fire is decided by the
//! rustc in the image — which will not generally match the one on this machine.
//! `roteiro lint clippy` sandboxed can therefore disagree with `cargo clippy` run
//! in the same tree on the same day, with no defect on either side.
//!
//! Because nothing is stored (ADR-0020 v1.1, condition 4) that is a **surprise
//! rather than a corruption**: there is no series for a different compiler to
//! falsify, and no layer key for two toolchains to collide in. It is still a real
//! surprise, so the toolchain is read out of the **guest** by [`probe_toolchain`]
//! rather than assumed from the image reference, and printed with every report
//! beside the image digest it came from.
//!
//! # There is no fallback
//!
//! Every refusal in this module is terminal. Sandbox selected and unavailable —
//! no hypervisor, no image, an image without the linter, a cache too cold to
//! build from — refuses and names what is missing. It never becomes a host run.
//! ADR-0020 §6, and worse here than elsewhere: the person asked for isolation and
//! would get execution.
//!
//! @rto:0014
//! @rto:0020

use std::path::{Path, PathBuf};

use boxlite::runtime::options::VolumeSpec;
use boxlite::{
    BoxCommand, BoxOptions, BoxliteOptions, BoxliteRuntime, LiteBox, NetworkSpec, RootfsSpec,
};
use futures::StreamExt as _;
use rto_graph::{Isolation, SourceIdentity, rfc3339_utc};

use crate::adapter::NativeContext;
use crate::adapter::clippy::{self, Clippy, FeatureSet};
use crate::boxlite::{
    EXEC_TIMEOUT, GUEST_CPUS, GUEST_INIT, GUEST_MEMORY_MIB, GUEST_WORKTREE, MAX_OUTPUT_BYTES,
    SandboxError, SandboxProbe, pinned_digest, reference_is_present, sandbox_probe,
};
use crate::child_env::ChildEnv;
use crate::guidance::{Guidance, Line};
use crate::lint::{LintError, LintOutcome, Toolchain, scratch_dir};
use crate::lint_grant::Backend;
use crate::snippet::WorktreeSnippets;

/// Where the writable build directory is mounted inside the guest.
///
/// The one writable surface condition 1 adds. A fixed guest path for
/// [`GUEST_WORKTREE`]'s reason: nothing a build produces should depend on where
/// the host happened to put the directory.
pub const GUEST_SCRATCH: &str = "/scratch";

/// Where `CARGO_HOME` points inside the guest.
///
/// A directory the guest assembles out of mounts rather than a mount itself —
/// see the module documentation for why the host's `CARGO_HOME` root, which
/// holds `credentials.toml`, is not what is mounted here.
pub const GUEST_CARGO_HOME: &str = "/cargo";

/// Where the host's package cache is mounted, read-only.
pub const GUEST_CARGO_REGISTRY: &str = "/cargo/registry";

/// Where the host's cache of git dependencies is mounted, read-only.
pub const GUEST_CARGO_GIT: &str = "/cargo/git";

/// The one sentence that points at the document, and the **only** place the
/// Dockerfile's line count is claimed.
///
/// It was claimed in four places — this refusal, the one for an image without
/// the linter, a test's skip message, and `roteiro lint --help` — and one of
/// them said "three" over a document showing two. A count repeated four times is
/// a count that will be wrong somewhere, so it is stated once, checked against
/// the document by [`tests::the_two_line_dockerfile_claim_matches_the_document`],
/// and referenced everywhere else. `--help` no longer states a number at all,
/// because it is in another crate and this test cannot reach it.
const SEE_THE_DOCUMENT: Line = Line::Note(&[
    "See docs/SANDBOXED_LINTING.md for the two-line Dockerfile that satisfies this,",
    "and pin the image you build by digest.",
]);

/// How to provision an image once it exists, in the words `prefetch` uses.
const PREFETCH_THE_IMAGE: Line =
    Line::Command("roteiro security prefetch --analyzer clippy --allow-download");

/// The one command that fills a cold package cache, for the two refusals that
/// need it. `--locked` because the tree under review is not this command's to
/// re-resolve.
const FETCH_ON_THE_HOST: Line = Line::Command("cargo fetch --locked");

/// What [`crate::lint`] prints when the sandbox is selected and no image was
/// supplied.
///
/// It lives here rather than at the refusal because the *reason* there is no
/// default is this module's to explain, and a build without `exec-boxlite`
/// prints the same lines from the same place.
///
/// The line count in the last note is **checked against the document**, by
/// `the_two_line_dockerfile_claim_matches_the_document`. It said "three-line"
/// while the document showed two, which is the smallest possible version of the
/// defect this whole type exists for: a way forward that is wrong about the
/// thing it points at.
pub const NO_IMAGE_CONFIGURED: Guidance = Guidance::new(&[
    Line::Note(&[
        "No image is configured. `roteiro lint` runs the linter inside an OCI image,",
        "and roteiro ships no default: no first-party Rust image carries the `clippy`",
        "component (rust-lang/docker-rust builds every stable and nightly variant",
        "`--profile minimal`), and choosing a third party's would make somebody else's",
        "container the boundary your build scripts run in — picked here and noticed by",
        "nobody.",
    ]),
    Line::Note(&["Supply one, pinned by digest:"]),
    Line::Command("[lint]"),
    Line::Command("image = \"registry/you/rust-clippy@sha256:<64 hex>\""),
    Line::Note(&[
        "in ~/.roteiro/config.toml (yours) or the repository's roteiro.toml (your",
        "team's), then:",
    ]),
    PREFETCH_THE_IMAGE,
    SEE_THE_DOCUMENT,
]);

/// What to check first when a sandboxed build produced nothing at all.
///
/// The cause that is new rather than the causes a person already knows how to
/// look for. An image has to be able to **build** the tree, not merely lint it,
/// and a missing native build dependency fails inside a machine the reader
/// cannot open a shell in — so it is worth saying at the point of failure rather
/// than in a document they would have to know to go and read.
///
/// Measured on this repository: `--all-targets` compiles `rto-llama`'s
/// dev-dependency on vendored llama.cpp, whose build script needs `libclang`, and
/// an image carrying `cmake` but not `libclang` panics in `bindgen` with no
/// diagnostic to show for it.
const BUILD_DEPENDENCY_HINT: Guidance = Guidance::new(&[Line::Note(&[
    "It ran sandboxed, so check this first: the image has to be able to *build*",
    "your tree, not just lint it. `cargo clippy` has `cargo check` semantics, so",
    "every build script in the tree runs inside the image — and a native dependency",
    "the image lacks (libclang, cmake, a C toolchain, protoc) fails there, where you",
    "cannot open a shell. Add it to the image you supply; see",
    "docs/SANDBOXED_LINTING.md.",
])]);

/// Why an official Rust image will not do, and what to supply instead.
///
/// The refusal this whole design turns on, so it says *why* rather than only
/// *what*: a reader who is told "this image lacks clippy" will reach for the
/// official Rust image next, and get here again.
const IMAGE_LACKS_LINTER: Guidance = Guidance::new(&[
    Line::Note(&[
        "An official Rust image will not do — rust-lang/docker-rust builds every stable",
        "and nightly variant with `rustup-init --profile minimal`, which installs rustc,",
        "cargo and rust-std and stops. The `clippy` component has to be in the image you",
        "supply, and roteiro will not choose one for you: an image is the boundary your",
        "build scripts run in.",
    ]),
    SEE_THE_DOCUMENT,
    Line::Note(&["Then point `[lint] image` at it, and provision it:"]),
    PREFETCH_THE_IMAGE,
]);

/// Condition 2's failure mode, and the one command that fixes it.
///
/// Cargo reports this from inside a machine the reader cannot open a shell in,
/// in two wordings depending on whether the `.crate` file is missing or merely
/// unexpanded — and neither mentions the host, which is the only place it can be
/// fixed.
const COLD_CACHE: Guidance = Guidance::new(&[
    Line::Note(&[
        "Egress is denied by the hypervisor, and `--offline` is passed so cargo says so",
        "rather than hanging. Fetch them on the host first, in the tree you are linting:",
    ]),
    FETCH_ON_THE_HOST,
    Line::Note(&[
        "That both downloads and unpacks, which is what a read-only cache mount needs —",
        "a `.crate` file that is present but unexpanded fails just as a missing one does,",
        "because expanding it would be a write. Then lint again.",
    ]),
    Line::Note(&["Nothing was reported, because a build that did not happen is not a clean tree."]),
]);

/// No package cache at all, which is a setup step rather than a broken build.
const NO_CACHE: Guidance = Guidance::new(&[
    Line::Note(&[
        "The guest builds from a read-only mount of this machine's cache, so there has to",
        "be one. Create it by fetching this tree's dependencies on the host:",
    ]),
    FETCH_ON_THE_HOST,
]);

/// Something went wrong obtaining or using the boundary.
///
/// Every variant names what is missing **and what to do about it**, because a
/// sandboxed run that cannot happen is refused rather than downgraded, so the
/// message is the entire interface to that state (#426).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BuilderError {
    /// Something in [`crate::boxlite`]'s vocabulary went wrong: no hypervisor,
    /// an unpinned reference, an unreadable image store, a guest that failed to
    /// start.
    #[error(transparent)]
    Sandbox(#[from] SandboxError),
    /// The supplied image is not in the local store, and no run ever pulls.
    ///
    /// The same rule every other input follows — provisioning downloads, running
    /// reads — so that a lint can never depend on a registry being reachable,
    /// nor succeed by silently fetching something new.
    // The reference is in the sentence, not only in a field. Converting this to
    // a `Guidance` dropped it for one revision, and "an image is not in the
    // store" without saying *which* image is a refusal that names no way
    // forward — the reader has two configured layers and a flag to check.
    #[error(
        "the image {reference} is not in the local store, and a run never pulls one.{}",
        Guidance::new(&[
            Line::Note(&[
                "Provisioning fetches; running reads, so that a lint can never fail because a",
                "registry was unreachable, nor succeed by quietly fetching something new.",
                "Pull it first:",
            ]),
            PREFETCH_THE_IMAGE,
        ])
    )]
    ImageNotProvisioned {
        /// The linter whose run was refused.
        ///
        /// Not in the message: `prefetch --analyzer clippy` is the command
        /// whatever linter was asked for, because `clippy` is the only one, and
        /// a second name in a sentence that already carries a digest is noise.
        /// Kept because the moment there are two linters the command differs.
        analyzer: String,
        /// The digest-pinned reference that is missing.
        reference: String,
    },
    /// The image started, and does not contain the linter.
    ///
    /// The refusal this whole design turns on. `cargo clippy` in an image built
    /// `--profile minimal` prints *"no such command: `clippy`"* and exits with a
    /// status cargo also uses for a completed build — so without this check the
    /// outcome would be an empty report over a tree nobody linted, which is the
    /// vacuous zero this project has been bitten by repeatedly.
    #[error(
        "the image {reference} ran, and `{probe}` inside it did not work: it does not carry \
         `{analyzer}`.{}{stderr}",
        IMAGE_LACKS_LINTER
    )]
    ImageLacksLinter {
        /// The linter that is missing.
        analyzer: String,
        /// The image that was asked to provide it.
        reference: String,
        /// The probe that established it.
        probe: String,
        /// The tail of the probe's standard error, prefixed for display.
        stderr: String,
    },
    /// A toolchain probe inside the guest failed for some other reason.
    #[error("`{probe}` failed inside the image {reference}.{stderr}")]
    ProbeFailed {
        /// The probe.
        probe: String,
        /// The image it ran in.
        reference: String,
        /// The tail of its standard error.
        stderr: String,
    },
    /// The host's package cache does not hold what the build needs, and a guest
    /// with no network cannot go and get it.
    ///
    /// **Condition 2's failure mode, named rather than passed through.** Cargo
    /// reports it from inside a machine the user cannot see, in two different
    /// wordings depending on whether the `.crate` file is missing or merely
    /// unexpanded — and neither of them mentions the host, which is the only
    /// place the problem can be fixed.
    #[error(
        "the build needs a dependency that this machine's cargo cache does not hold, and the \
         guest has no network to fetch it with.{}{stderr}",
        COLD_CACHE
    )]
    ColdCache {
        /// The tail of cargo's standard error, which names the crate.
        stderr: String,
    },
    /// There is no package cache on this host at all.
    #[error(
        "there is no cargo package cache to mount: {path} does not exist.{}",
        NO_CACHE
    )]
    NoPackageCache {
        /// Where it was looked for.
        path: String,
    },
    /// The linter exited with a status it does not use for a completed run.
    #[error(
        "`{command}` exited {status} inside the sandbox, which `{analyzer}` does not use for a \
         completed run (expected one of: {expected}).{stderr}"
    )]
    UnexpectedStatus {
        /// The linter.
        analyzer: String,
        /// The argv that ran.
        command: String,
        /// The status it exited with.
        status: i32,
        /// The statuses the adapter declared usable.
        expected: String,
        /// The tail of its standard error.
        stderr: String,
    },
    /// The linter was killed by a signal inside the guest, saying nothing.
    #[error(
        "`{command}` was killed by signal {signal} inside the sandbox and wrote nothing to \
         stderr.\n  The usual cause is the guest running out of memory: it is given {memory_mib} \
         MiB, and compiling a large workspace can need more. This is not a finding — nothing was \
         reported, because a build that was killed is not a clean tree."
    )]
    Killed {
        /// The argv that ran.
        command: String,
        /// The signal number it was killed by.
        signal: i32,
        /// How much memory the guest had.
        memory_mib: u32,
    },
    /// The linter produced more output than will be read.
    #[error(
        "`{command}` produced more than {max} bytes of output in the sandbox; refusing to read it"
    )]
    OutputTooLarge {
        /// The argv that ran.
        command: String,
        /// The ceiling.
        max: usize,
    },
}

/// Run `analyzer` over the workspace at `root`, inside `image`.
///
/// `root` is a **host** path and is mounted read-only; nothing here writes to it,
/// and the guest kernel rather than the linter's good manners is what makes that
/// true.
///
/// # Errors
/// Returns [`LintError`]: a [`BuilderError`] for anything about the boundary —
/// no hypervisor, an unpinned or unprovisioned image, an image without the
/// linter, a package cache too cold to build from, a build that was killed — or
/// [`LintError::ScratchUnavailable`] if the writable build directory cannot be
/// created, or [`LintError::Report`] if the linter's output is not a report.
pub fn run(
    analyzer: &str,
    root: &Path,
    features: &FeatureSet,
    image: &str,
) -> Result<LintOutcome, LintError> {
    // Everything that can be refused without starting a VM is refused before
    // one is started, so a cold cache or an absent hypervisor costs nothing and
    // says so precisely. The order is cheapest-and-most-fundamental first.
    // Validated for its own sake: the digest is already inside `image`, and what
    // this call is for is refusing a *tag*. An image is where somebody else's
    // build scripts execute, and a tag is a mutable pointer to it.
    //
    // `reference_is_present` below checks the same thing, and this is **not**
    // redundancy to remove. It is here so the refusal comes first, before the
    // hypervisor probe: a tag is a mistake in a config file and has nothing to
    // do with this machine, so on a laptop with no virtualisation it should say
    // "that is a tag" rather than "there is no hypervisor" and leave the real
    // problem to be found later. Fault injection confirms the pair: deleting
    // either one alone changes no test, because the other still refuses.
    pinned_digest("`[lint] image`", image).map_err(BuilderError::from)?;
    if let SandboxProbe::Unavailable(reason) = sandbox_probe() {
        return Err(BuilderError::from(SandboxError::Unavailable { reason }).into());
    }
    let assets_root = crate::asset_paths::asset_root();
    if !reference_is_present("`[lint] image`", image, &assets_root).map_err(BuilderError::from)? {
        return Err(BuilderError::ImageNotProvisioned {
            analyzer: analyzer.to_owned(),
            reference: image.to_owned(),
        }
        .into());
    }
    let cache = PackageCache::on_this_host()?;
    // Its own directory, never the host runner's: the two put different
    // operating systems' executables under the same names. See `scratch_dir`.
    let scratch = scratch_dir(root, Backend::Sandbox)?;

    let runner = Builder {
        analyzer,
        image: image.to_owned(),
        root: root.to_path_buf(),
        scratch: scratch.clone(),
        cache,
        assets_root,
    };
    let outcome = runner.execute(features)?;
    Ok(outcome)
}

/// The host directories a guest builds from, and how they were found.
#[derive(Debug)]
struct PackageCache {
    /// `$CARGO_HOME/registry` — downloaded and unpacked crates.
    registry: PathBuf,
    /// `$CARGO_HOME/git`, when there is one. A tree with no git dependencies
    /// never has this, and mounting a directory that is not there is an error
    /// rather than an empty mount.
    git: Option<PathBuf>,
}

impl PackageCache {
    /// Locate this machine's package cache.
    ///
    /// `CARGO_HOME` if set, else `~/.cargo`, which is cargo's own precedence —
    /// read here rather than inherited by name, because a *guest* has no parent
    /// environment to inherit from and the path has to be resolved on this side
    /// of the boundary to be mounted at all. This is the one place where
    /// [`ChildEnv`]'s "inheriting locates" half becomes a mount instead of a
    /// variable.
    ///
    /// # Errors
    /// Returns [`BuilderError::NoPackageCache`] when there is no cache to mount,
    /// naming `cargo fetch` — a build cannot be assembled from a cache that does
    /// not exist, and discovering that inside a VM would be a confusing way to
    /// find out.
    fn on_this_host() -> Result<Self, BuilderError> {
        let home = std::env::var_os("CARGO_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .or_else(|| std::env::var_os("USERPROFILE"))
                    .map(|home| PathBuf::from(home).join(".cargo"))
            })
            .ok_or_else(|| BuilderError::NoPackageCache {
                path: "$CARGO_HOME (unset, and no home directory either)".to_owned(),
            })?;
        Self::under(&home)
    }

    /// Which subdirectories of `home` are mountable, deciding nothing from the
    /// environment.
    ///
    /// Split from [`PackageCache::on_this_host`] so a test can ask **this code**
    /// which directories it chose, rather than restating the `home.join(…)`
    /// arithmetic and checking that. That distinction is not pedantry here: an
    /// earlier version of the tests below built a `PackageCache` by hand and
    /// consequently went on passing while this function was changed to mount the
    /// `CARGO_HOME` root — the one thing it exists not to do. Fault injection
    /// found it; the split is the fix.
    ///
    /// # Errors
    /// Returns [`BuilderError::NoPackageCache`] when there is no package cache
    /// under `home` to mount, naming the directory it looked for.
    fn under(home: &Path) -> Result<Self, BuilderError> {
        let registry = home.join("registry");
        if !registry.is_dir() {
            return Err(BuilderError::NoPackageCache {
                path: registry.display().to_string(),
            });
        }
        // `git` only when it is there. A tree with no git dependencies never has
        // one, and a volume naming a directory that does not exist fails the box
        // rather than the lint — a setup problem wearing the costume of a broken
        // boundary.
        let git = home.join("git");
        Ok(Self {
            registry,
            git: git.is_dir().then_some(git),
        })
    }

    /// The read-only volumes this cache contributes.
    ///
    /// Note what is **not** here: `$CARGO_HOME` itself. See the module
    /// documentation — the root holds `credentials.toml`, and the build scripts
    /// this module exists to contain would be able to read it.
    fn volumes(&self) -> Vec<VolumeSpec> {
        std::iter::once((&self.registry, GUEST_CARGO_REGISTRY))
            .chain(self.git.as_ref().map(|git| (git, GUEST_CARGO_GIT)))
            .map(|(host, guest)| VolumeSpec {
                host_path: host.to_string_lossy().into_owned(),
                guest_path: guest.to_owned(),
                read_only: true,
            })
            .collect()
    }
}

/// One sandboxed lint, and everything it was resolved against.
struct Builder<'a> {
    analyzer: &'a str,
    image: String,
    root: PathBuf,
    scratch: PathBuf,
    cache: PackageCache,
    assets_root: PathBuf,
}

/// What a completed guest command produced.
struct Captured {
    stdout: String,
    stderr: String,
    status: i32,
}

impl Builder<'_> {
    /// The mounts the guest gets, in the order the module's table states them.
    fn volumes(&self) -> Vec<VolumeSpec> {
        let mut volumes = vec![
            VolumeSpec {
                host_path: self.root.to_string_lossy().into_owned(),
                guest_path: GUEST_WORKTREE.to_owned(),
                // Unchanged, and the point. Nine tools were measured and none
                // needed a writable source tree; `cargo clippy` completes
                // against one on which every write is refused, given
                // `CARGO_TARGET_DIR` outside it.
                read_only: true,
            },
            VolumeSpec {
                host_path: self.scratch.to_string_lossy().into_owned(),
                guest_path: GUEST_SCRATCH.to_owned(),
                // The single `false` in this file. A malicious build script gets
                // this directory and nothing else that it can change.
                read_only: false,
            },
        ];
        volumes.extend(self.cache.volumes());
        volumes
    }

    /// Boot the guest, probe it, lint in it, and tear it down.
    fn execute(&self, features: &FeatureSet) -> Result<LintOutcome, LintError> {
        // Current-thread, for `BoxliteRunner::new`'s reason: this backend does
        // one thing at a time and must not quietly acquire a thread pool inside
        // a CLI that has none.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| {
                BuilderError::from(SandboxError::Runtime {
                    stage: "runtime",
                    message: e.to_string(),
                })
            })?;
        runtime.block_on(self.inside(features))
    }

    /// Everything that happens with a guest running.
    async fn inside(&self, features: &FeatureSet) -> Result<LintOutcome, LintError> {
        let boxlite = BoxliteRuntime::new(BoxliteOptions {
            home_dir: self.assets_root.join("boxlite-home"),
            image_registries: Vec::new(),
        })
        .map_err(|e| {
            BuilderError::from(SandboxError::Runtime {
                stage: "open",
                message: e.to_string(),
            })
        })?;

        let options = BoxOptions {
            cpus: Some(GUEST_CPUS),
            memory_mib: Some(GUEST_MEMORY_MIB),
            rootfs: RootfsSpec::Image(self.image.clone()),
            // The boundary, not a configuration flag: no interface is created,
            // so the build scripts inside cannot open a socket to anywhere
            // whatever they would like to do. `--offline` on the argv is for the
            // error message, not for the guarantee.
            network: NetworkSpec::Disabled,
            volumes: self.volumes(),
            env: guest_environment(),
            working_dir: Some(GUEST_WORKTREE.to_owned()),
            // Replace the image's entrypoint so the box outlives the build — see
            // `GUEST_INIT`. Without it the image's own entrypoint runs, exits,
            // and takes the box down mid-build with a `-9` that reads exactly
            // like an out-of-memory kill and is not one.
            entrypoint: Some(GUEST_INIT.iter().map(|s| (*s).to_owned()).collect()),
            cmd: Some(Vec::new()),
            // Nothing survives the build. The scratch directory does, on the
            // host, because it is a mount rather than part of the box.
            auto_remove: true,
            detach: false,
            ..Default::default()
        };

        let boxed = boxlite.create(options, None).await.map_err(|e| {
            BuilderError::from(SandboxError::Runtime {
                stage: "create",
                message: e.to_string(),
            })
        })?;

        let result = self.lint_in(&boxed, features).await;

        // Tear down whatever happened, and let the run's own error win: a
        // failure to stop a box must not mask the reason the lint failed.
        let stopped = boxed.stop().await;
        let shutdown = boxlite.shutdown(Some(10)).await;
        let outcome = result?;
        stopped.map_err(|e| {
            BuilderError::from(SandboxError::Runtime {
                stage: "stop",
                message: e.to_string(),
            })
        })?;
        shutdown.map_err(|e| {
            BuilderError::from(SandboxError::Runtime {
                stage: "shutdown",
                message: e.to_string(),
            })
        })?;
        Ok(outcome)
    }

    /// Probe the guest's toolchain, then lint with it.
    async fn lint_in(
        &self,
        boxed: &LiteBox,
        features: &FeatureSet,
    ) -> Result<LintOutcome, LintError> {
        // **Before** the build, not read out of its output. An image without the
        // component prints "no such command" and exits with a status cargo also
        // uses for a completed build, so a build alone cannot tell the two
        // apart — the same reason the host path probes first.
        let toolchain = self.probe_toolchain(boxed).await?;

        let invocation = Clippy::offline_invocation(features);
        let command = crate::lint::argv(&invocation);
        let started_at = rfc3339_utc(std::time::SystemTime::now());
        let output = self
            .exec(boxed, &invocation.program, &invocation.args)
            .await?;
        let ended_at = rfc3339_utc(std::time::SystemTime::now());

        // Both before the parse, because the parse cannot tell either of them
        // from any other run that emitted nothing and would call it a malformed
        // report — an answer that is true and useless. Each is a condition this
        // module imposed, so explaining it is this module's job.
        if cold_cache(&output.stderr) {
            return Err(BuilderError::ColdCache {
                stderr: stderr_tail(&output.stderr),
            }
            .into());
        }
        if crate::lint::lockfile_refused(output.stderr.as_bytes()) {
            return Err(LintError::LockfileWouldBeWritten {
                command: command.join(" "),
                stderr: stderr_tail(&output.stderr),
            });
        }
        if !invocation.success_statuses.contains(&output.status) {
            return Err(self.failed(&command, &output, &invocation.success_statuses));
        }

        // Snippets come from the **host** copy of the tree — the same bytes,
        // reachable without a second guest command — while paths relativise
        // against the *guest* root, so a diagnostic about `/work/src/lib.rs`
        // reports `src/lib.rs` exactly as a host run would.
        let snippets = WorktreeSnippets::new(&self.root);
        let source = SourceIdentity::default();
        let ctx = NativeContext {
            started_at,
            ended_at,
            analyzer_version: Some(crate::lint::short_version(&toolchain.linter)),
            exit_status: output.status,
            source: &source,
            rules_digest: None,
            advisory_db: None,
            worktree: Some(Path::new(GUEST_WORKTREE)),
            snippets: &snippets,
        };
        let (report, summary) = Clippy::parse(output.stdout.as_bytes(), &ctx)?;

        if !summary.build_succeeded && report.findings.is_empty() {
            return Err(LintError::BuildProducedNothing {
                command: command.join(" "),
                status: output.status,
                hint: Some(BUILD_DEPENDENCY_HINT),
                stderr: stderr_tail(&output.stderr),
            });
        }

        Ok(LintOutcome {
            analyzer: clippy::ANALYZER,
            report,
            summary,
            toolchain,
            features: features.clone(),
            // Stated by the code that obtained the boundary, never assembled by
            // whoever prints it — ADR-0020 condition 3.
            isolation: Isolation::MicroVm,
            command,
            worktree: self.root.clone(),
            image: Some(self.image.clone()),
            scratch: self.scratch.clone(),
        })
    }

    /// Which failure a non-success status was.
    ///
    /// A negative status with nothing on stderr is almost always the guest OOM
    /// killer, and reported as an unexpected status it reads as "the linter
    /// failed for unknown reasons" while the actual cause — a guest sized too
    /// small for the build — never occurs to the reader.
    fn failed(&self, command: &[String], output: &Captured, expected: &[i32]) -> LintError {
        if output.status < 0 && output.stderr.trim().is_empty() {
            return BuilderError::Killed {
                command: command.join(" "),
                signal: -output.status,
                memory_mib: GUEST_MEMORY_MIB,
            }
            .into();
        }
        BuilderError::UnexpectedStatus {
            analyzer: self.analyzer.to_owned(),
            command: command.join(" "),
            status: output.status,
            expected: expected
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
            stderr: stderr_tail(&output.stderr),
        }
        .into()
    }

    /// Ask the **guest** what toolchain it has.
    ///
    /// Asked rather than derived from the image reference, and the difference
    /// matters: the reference is what somebody wrote in a config file, and what
    /// decides which lints fire is what is actually in the image. A tag that
    /// says `1.97` over a toolchain that is not is a mislabelled report rather
    /// than a failed run, which is the worse of the two outcomes.
    ///
    /// It doubles as the *is the linter here* check, which is why it comes
    /// before the build.
    async fn probe_toolchain(&self, boxed: &LiteBox) -> Result<Toolchain, BuilderError> {
        let clippy = self
            .exec(
                boxed,
                "cargo",
                &["clippy".to_owned(), "--version".to_owned()],
            )
            .await?;
        if clippy.status != 0 || clippy.stdout.trim().is_empty() {
            return Err(BuilderError::ImageLacksLinter {
                analyzer: self.analyzer.to_owned(),
                reference: self.image.clone(),
                probe: "cargo clippy --version".to_owned(),
                stderr: stderr_tail(&clippy.stderr),
            });
        }
        let rustc = self.exec(boxed, "rustc", &["-vV".to_owned()]).await?;
        if rustc.status != 0 {
            return Err(BuilderError::ProbeFailed {
                probe: "rustc -vV".to_owned(),
                reference: self.image.clone(),
                stderr: stderr_tail(&rustc.stderr),
            });
        }
        let (version, host) = crate::lint::parse_rustc_verbose(&rustc.stdout);
        Ok(Toolchain {
            linter: crate::lint::first_line(&clippy.stdout),
            rustc: version,
            host,
        })
    }

    /// Run one command in the guest and collect its streams.
    async fn exec(
        &self,
        boxed: &LiteBox,
        program: &str,
        args: &[String],
    ) -> Result<Captured, BuilderError> {
        let command = BoxCommand::new(program)
            .args(args.to_vec())
            .working_dir(GUEST_WORKTREE)
            .timeout(EXEC_TIMEOUT);

        let mut execution = boxed
            .exec(command)
            .await
            .map_err(|e| SandboxError::Runtime {
                stage: "exec",
                message: e.to_string(),
            })?;

        let out = tokio::spawn(collect(execution.stdout()));
        let err = tokio::spawn(collect(execution.stderr()));
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
            return Err(BuilderError::OutputTooLarge {
                command: program.to_owned(),
                max: MAX_OUTPUT_BYTES,
            });
        }
        Ok(Captured {
            stdout,
            stderr,
            status: status.exit_code,
        })
    }
}

/// The environment a guest build gets.
///
/// Everything is **set**; nothing is inherited, and [`ChildEnv::guest_pairs`] is
/// where the reason lives. The short version is that a guest shares neither this
/// machine's filesystem nor its environment block, so a name carried across would
/// point at a directory that is not there.
///
/// A free function rather than a method because it reads no state from the run:
/// both values are **guest** paths, fixed by this module for [`GUEST_WORKTREE`]'s
/// reason — nothing a build produces should depend on where the host happened to
/// put a directory.
///
/// Both variables are constraints rather than locators, which is why they belong
/// in the `set` half:
///
/// - `CARGO_TARGET_DIR` — where the build may write, which is this module's
///   guarantee and therefore this module's to choose. *Inheriting* it is how
///   `roteiro lint` came to write into the tree it was reviewing (ADR-0020 v1.4).
/// - `CARGO_HOME` — which package cache the build resolves from. Set to the
///   assembled [`GUEST_CARGO_HOME`] rather than left at the image's own, so that
///   the mounted cache is what cargo reads and the image's empty one is not
///   silently used instead — which would make every build look cold.
fn guest_environment() -> Vec<(String, String)> {
    let set = [
        ("CARGO_TARGET_DIR", std::ffi::OsString::from(GUEST_SCRATCH)),
        ("CARGO_HOME", std::ffi::OsString::from(GUEST_CARGO_HOME)),
    ];
    ChildEnv {
        inherit: &[],
        set: &set,
    }
    .guest_pairs()
}

/// Reassemble a guest stream into the bytes the process wrote.
///
/// **Concatenated, never joined by lines** — [`crate::boxlite::collect`]'s
/// reason: the chunk boundaries are wherever the guest's writes happened to
/// land, not newlines, and a separator inserted here would corrupt the one JSON
/// document cargo emits per line.
async fn collect(stream: Option<impl futures::Stream<Item = String> + Unpin>) -> String {
    let mut out = String::new();
    if let Some(mut stream) = stream {
        while let Some(chunk) = stream.next().await {
            out.push_str(&chunk);
            // Bounded here as well as after the fact: a runaway build must not
            // be able to exhaust memory before anyone gets to check.
            if out.len() > MAX_OUTPUT_BYTES {
                break;
            }
        }
    }
    out
}

/// Whether cargo failed because the mounted package cache does not hold what the
/// build needs.
///
/// Two clauses rather than one, because a read-only cache fails in two distinct
/// ways and only the first is obviously a network problem:
///
/// - the `.crate` file is **absent**, so cargo tries to download it and
///   `--offline` stops it: *"attempting to make an HTTP request, but --offline
///   was specified"*;
/// - the `.crate` file is **present but not unpacked**, so cargo tries to expand
///   it into `registry/src` and the read-only mount refuses: *"failed to unpack
///   package"*.
///
/// Both were measured. Both have the same remedy — `cargo fetch` on the host
/// downloads *and* unpacks — which is why they are one error variant and not
/// two. Matched on the clause each wording shares rather than on either message
/// in full, which would silently stop matching on a cargo release and send the
/// caller back to the unhelpful answer this exists to replace.
fn cold_cache(stderr: &str) -> bool {
    stderr.contains("but --offline was specified") || stderr.contains("failed to unpack package")
}

/// The last few lines of a guest process's standard error, for a message.
///
/// Delegates to [`crate::lint`]'s so the two backends' failures are shaped
/// identically — a bound that differed between them would be a difference in
/// how legible a failure is, decided by which backend you happened to use.
fn stderr_tail(stderr: &str) -> String {
    crate::subprocess::stderr_tail(stderr.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::{
        BUILD_DEPENDENCY_HINT, Builder, BuilderError, COLD_CACHE, GUEST_CARGO_GIT,
        GUEST_CARGO_HOME, GUEST_CARGO_REGISTRY, GUEST_SCRATCH, IMAGE_LACKS_LINTER, NO_CACHE,
        NO_IMAGE_CONFIGURED, PackageCache, cold_cache, guest_environment,
    };
    use crate::boxlite::GUEST_WORKTREE;
    use std::path::{Path, PathBuf};

    fn builder(git: bool) -> Builder<'static> {
        Builder {
            analyzer: "clippy",
            image: "registry/x@sha256:".to_owned() + &"a".repeat(64),
            root: PathBuf::from("/repo"),
            scratch: PathBuf::from("/scratch-on-host"),
            cache: PackageCache {
                registry: PathBuf::from("/home/you/.cargo/registry"),
                git: git.then(|| PathBuf::from("/home/you/.cargo/git")),
            },
            assets_root: PathBuf::from("/assets"),
        }
    }

    /// Condition 1, at the seam that decides it: **one** writable mount, and it
    /// is the scratch. If this test ever has to be relaxed, the change under it
    /// is the conversion ADR-0014 predicted rather than a refactor.
    #[test]
    fn the_scratch_is_the_only_thing_the_guest_may_write() {
        let volumes = builder(true).volumes();
        let writable: Vec<&str> = volumes
            .iter()
            .filter(|v| !v.read_only)
            .map(|v| v.guest_path.as_str())
            .collect();
        assert_eq!(writable, vec![GUEST_SCRATCH]);
    }

    /// The worktree and the package cache are inputs, and an input the guest can
    /// change is not an input.
    #[test]
    fn the_worktree_and_the_package_cache_are_read_only() {
        let volumes = builder(true).volumes();
        for guest in [GUEST_WORKTREE, GUEST_CARGO_REGISTRY, GUEST_CARGO_GIT] {
            let volume = volumes
                .iter()
                .find(|v| v.guest_path == guest)
                .unwrap_or_else(|| panic!("{guest} is not mounted"));
            assert!(volume.read_only, "{guest} is writable");
        }
    }

    /// A `$CARGO_HOME` laid out on disk, so the tests below ask **the code**
    /// which directories it chose rather than restating the arithmetic.
    fn cargo_home(git: bool, credentials: bool) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let home = std::env::temp_dir().join(format!(
            "rto-exec-cargo-home-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::remove_dir_all(&home).ok();
        std::fs::create_dir_all(home.join("registry")).expect("registry");
        if git {
            std::fs::create_dir_all(home.join("git")).expect("git");
        }
        if credentials {
            // The thing this must never expose: a real `CARGO_HOME` has one.
            std::fs::write(
                home.join("credentials.toml"),
                "[registry]\ntoken = \"secret\"\n",
            )
            .expect("credentials");
        }
        home
    }

    /// `$CARGO_HOME` holds `credentials.toml` — a crates.io API token — and a
    /// `config.toml` that may carry registry tokens of its own. The build
    /// scripts this module exists to contain must not be able to read either, so
    /// the **root** is never a mount; only the subdirectories that hold packages
    /// are.
    ///
    /// Driven through [`PackageCache::under`] rather than a hand-built value,
    /// and that is the whole point of the test. The version this replaces
    /// constructed a `PackageCache` itself, so it asserted a property of the
    /// fixture — fault injection changed `under` to mount the root and this test
    /// stayed green.
    #[test]
    fn the_cargo_home_root_is_never_mounted_so_credentials_stay_here() {
        let home = cargo_home(true, true);
        let volumes = PackageCache::under(&home).expect("a cache").volumes();
        assert!(!volumes.is_empty(), "nothing was mounted at all");
        let root = home.to_string_lossy().into_owned();
        for volume in &volumes {
            assert_ne!(
                volume.host_path, root,
                "the CARGO_HOME root is mounted, which puts credentials.toml in front of the \
                 build scripts this boundary exists to contain"
            );
            // And the token is not reachable *through* a mount either, which is
            // the same hazard one directory further in.
            assert!(
                !Path::new(&volume.host_path)
                    .join("credentials.toml")
                    .exists(),
                "credentials.toml is reachable inside {}",
                volume.host_path
            );
            assert!(volume.read_only, "{} is writable", volume.host_path);
        }
        std::fs::remove_dir_all(&home).ok();
    }

    /// A tree with no git dependencies has no `$CARGO_HOME/git`, and mounting a
    /// directory that is not there fails the box rather than the lint — a setup
    /// problem wearing the costume of a broken boundary.
    ///
    /// Also driven through [`PackageCache::under`], for the reason above.
    #[test]
    fn a_git_cache_is_mounted_when_it_exists_and_not_when_it_does_not() {
        let with = cargo_home(true, false);
        let volumes = PackageCache::under(&with).expect("a cache").volumes();
        assert!(
            volumes.iter().any(|v| v.guest_path == GUEST_CARGO_GIT),
            "a git cache that exists must be mounted"
        );

        let without = cargo_home(false, false);
        let volumes = PackageCache::under(&without).expect("a cache").volumes();
        assert!(
            volumes.iter().all(|v| v.guest_path != GUEST_CARGO_GIT),
            "a git cache that does not exist must not be mounted"
        );
        assert!(
            volumes.iter().any(|v| v.guest_path == GUEST_CARGO_REGISTRY),
            "the registry is not optional"
        );
        std::fs::remove_dir_all(&with).ok();
        std::fs::remove_dir_all(&without).ok();
    }

    /// No package cache is a refusal that names the directory **and** the
    /// command that creates it — the guest builds from a mount of this
    /// machine's cache, so a missing one is a setup step, not a broken build.
    ///
    /// Rendered through [`crate::LintError`] rather than as a bare
    /// [`BuilderError`], because that is the only way a person ever sees one:
    /// the wrapper is what appends the promise that nothing fell back to this
    /// host. Asserting on the bare error tested a string no user is shown, and
    /// went green while the wrapper was the thing under change.
    #[test]
    fn a_missing_package_cache_refuses_and_names_the_way_forward() {
        let absent = std::env::temp_dir().join("rto-exec-no-such-cargo-home");
        std::fs::remove_dir_all(&absent).ok();
        let err = PackageCache::under(&absent).expect_err("must refuse");
        let message = crate::LintError::from(err).to_string();
        assert!(message.contains("registry"), "{message}");
        assert!(message.contains("cargo fetch --locked"), "{message}");
        assert!(
            message.contains("nothing fell back to this host"),
            "{message}"
        );
    }

    /// Everything is set and nothing is inherited, and the two variables set are
    /// the two constraints the guest build is under.
    #[test]
    fn the_guest_environment_names_both_constraints() {
        let env = guest_environment();
        assert!(env.contains(&("CARGO_TARGET_DIR".to_owned(), GUEST_SCRATCH.to_owned())));
        assert!(env.contains(&("CARGO_HOME".to_owned(), GUEST_CARGO_HOME.to_owned())));
        let names: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        for ambient in ["PATH", "HOME", "SSH_AUTH_SOCK", "CARGO_REGISTRY_TOKEN"] {
            assert!(!names.contains(&ambient), "{ambient} reaches the guest");
        }
    }

    /// The mounted cache has to be where `CARGO_HOME` points, or cargo reads the
    /// image's empty one and every build looks cold.
    #[test]
    fn the_mounted_cache_is_underneath_the_cargo_home_the_guest_is_given() {
        for guest in [GUEST_CARGO_REGISTRY, GUEST_CARGO_GIT] {
            assert!(
                guest.starts_with(&format!("{GUEST_CARGO_HOME}/")),
                "{guest} is not inside {GUEST_CARGO_HOME}"
            );
        }
    }

    /// Both measured wordings, and neither of the two failures that must **not**
    /// be reported as a cold cache — a stale lockfile and an ordinary
    /// compilation error each have their own answer.
    #[test]
    fn both_cold_cache_wordings_are_recognised_and_nothing_else_is() {
        assert!(cold_cache(
            "error: failed to download `serde v1.0.229`\n\nCaused by:\n  attempting to make an \
             HTTP request, but --offline was specified"
        ));
        assert!(cold_cache(
            "error: failed to download `serde v1.0.229`\n\nCaused by:\n  failed to unpack \
             package `serde v1.0.229`"
        ));
        assert!(!cold_cache(
            "error: the lock file needs to be updated but --locked was passed to prevent this"
        ));
        assert!(!cold_cache("error[E0308]: mismatched types"));
        assert!(!cold_cache(""));
    }

    /// A refusal names the **thing it is about**, not only the category.
    ///
    /// Written after breaking it: moving `ImageNotProvisioned`'s body into a
    /// [`Guidance`] dropped `{reference}` from the sentence for one revision, so
    /// it said an image was missing without saying which. The reader has a user
    /// config, a project config and a flag to check, and the message narrowed it
    /// to none of them.
    ///
    /// A shape rule cannot catch that — the message was well formed — so this
    /// renders each variant with a distinctive value and asserts the value comes
    /// out the other side.
    #[test]
    fn every_refusal_names_the_thing_it_is_about() {
        let reference = "registry.example/you/rust-clippy@sha256:0123456789abcdef";
        let subjects: Vec<(String, &str)> = vec![
            (
                BuilderError::ImageNotProvisioned {
                    analyzer: "clippy".to_owned(),
                    reference: reference.to_owned(),
                }
                .to_string(),
                reference,
            ),
            (
                BuilderError::ImageLacksLinter {
                    analyzer: "clippy".to_owned(),
                    reference: reference.to_owned(),
                    probe: "cargo clippy --version".to_owned(),
                    stderr: String::new(),
                }
                .to_string(),
                reference,
            ),
            (
                BuilderError::ProbeFailed {
                    probe: "rustc -vV".to_owned(),
                    reference: reference.to_owned(),
                    stderr: String::new(),
                }
                .to_string(),
                reference,
            ),
            (
                BuilderError::NoPackageCache {
                    path: "/nowhere/registry".to_owned(),
                }
                .to_string(),
                "/nowhere/registry",
            ),
            (
                BuilderError::UnexpectedStatus {
                    analyzer: "clippy".to_owned(),
                    command: "cargo clippy --offline".to_owned(),
                    status: 42,
                    expected: "0, 101".to_owned(),
                    stderr: String::new(),
                }
                .to_string(),
                "cargo clippy --offline",
            ),
        ];
        for (message, subject) in subjects {
            assert!(
                message.contains(subject),
                "a refusal that does not name {subject:?} narrows nothing:\n{message}"
            );
        }
    }

    /// Every refusal that offers a way forward offers one that is **well
    /// formed**, and rendering is what checks it — `Display` asserts the rules.
    ///
    /// So this test is mostly here to *do the rendering*: it is the thing that
    /// drags every guidance in this module through the assertion, including ones
    /// a future change adds and forgets to test directly.
    #[test]
    fn every_guidance_in_this_module_renders_without_defects() {
        for guidance in [
            NO_IMAGE_CONFIGURED,
            BUILD_DEPENDENCY_HINT,
            IMAGE_LACKS_LINTER,
            COLD_CACHE,
            NO_CACHE,
        ] {
            assert!(guidance.defects().is_empty(), "{:?}", guidance.defects());
            assert!(!guidance.to_string().is_empty());
        }
    }

    /// The refusal claims the Dockerfile is **two lines**, and this is what makes
    /// the claim true rather than hopeful.
    ///
    /// It shipped saying *"three-line image"* over a document showing two. That
    /// is the smallest possible version of the defect this crate's guidance
    /// rules exist for — a way forward that is wrong about the thing it points
    /// at — and no shape rule can catch it, because the message is perfectly
    /// well formed. Only the document can answer, so the document is asked.
    ///
    /// Both directions are pinned: the count in the message and the count in the
    /// file. Changing either alone fails here.
    #[test]
    fn the_two_line_dockerfile_claim_matches_the_document() {
        let doc = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/SANDBOXED_LINTING.md")
            .canonicalize()
            .expect("the document the refusal points at must exist");
        let text = std::fs::read_to_string(&doc).expect("readable");

        // The first fenced dockerfile block is the one the message means.
        let fence = text
            .split("```dockerfile")
            .nth(1)
            .expect("the document must contain a dockerfile block")
            .split("```")
            .next()
            .expect("an unterminated fence");
        let lines = fence.lines().filter(|l| !l.trim().is_empty()).count();

        let numeral = match lines {
            2 => "two",
            3 => "three",
            4 => "four",
            n => panic!(
                "{} shows a {n}-line Dockerfile; teach this test the numeral",
                doc.display()
            ),
        };
        assert!(
            NO_IMAGE_CONFIGURED
                .to_string()
                .contains(&format!("{numeral}-line")),
            "{} shows a {lines}-line Dockerfile, and the refusal does not say {numeral:?}:\n{}",
            doc.display(),
            NO_IMAGE_CONFIGURED
        );
    }

    /// The refusal for an unconfigured image is the whole interface to that
    /// state, so it has to carry the key, a pinned example and the command that
    /// provisions it — not merely say that something is missing.
    #[test]
    fn the_unconfigured_image_refusal_says_what_to_do() {
        let rendered = NO_IMAGE_CONFIGURED.to_string();
        for needle in [
            "[lint]",
            "image = ",
            "@sha256:",
            "roteiro security prefetch",
            "docs/SANDBOXED_LINTING.md",
        ] {
            assert!(
                rendered.contains(needle),
                "the refusal does not mention {needle}"
            );
        }
    }
}

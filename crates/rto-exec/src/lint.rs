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
//! and every backend runs it. This module does not go through it — not because a
//! linter needs it relaxed, but because it has no [`crate::Worktree`] to present:
//! it takes a directory and a grant, not an [`crate::AnalysisRequest`], and it
//! produces no [`rto_graph::AnalysisRun`] for a policy to be recorded on. There
//! is therefore no [`Consent`], no [`rto_graph::CommandPolicy`], and no
//! `AnalyzerRunner` impl to be handed to a caller expecting those promises.
//!
//! ADR-0020 v1.1 hedged that a builder would need the preflight relaxed. v1.3
//! withdrew the hedge on measurement: with `CARGO_TARGET_DIR` outside the tree
//! and `--locked`, `cargo clippy` completes against a source tree on which every
//! write is refused.
//!
//! v1.4 records what that measurement did **not** establish, because this module
//! spent a release claiming it. A probe run by hand demonstrates a property of
//! the probe; it says nothing about whether the shipped path arranges the same
//! conditions. This one did not. `cargo clippy` writes wherever
//! `CARGO_TARGET_DIR` points and rewrites `Cargo.lock` unless told not to, and
//! this module *inherited* `CARGO_TARGET_DIR` by name rather than setting it —
//! a passthrough entry that reads as configuration and is a no-op when the
//! parent has no such variable, which is the ordinary case. Run from a shell
//! that had not set it, `roteiro lint clippy` wrote `target/` and `Cargo.lock`
//! into the tree it was reviewing, under a doc comment saying it did not.
//!
//! It is a property of this code now. [`run`] **sets** `CARGO_TARGET_DIR` to a
//! directory it chooses outside the tree ([`scratch_dir`]) and passes
//! `--locked`, which between them are the two writes a lint would otherwise make
//! into the tree under review. Neither depends on the caller's environment: the
//! seam that carries them is [`crate::subprocess::ChildEnv`]'s *set* half, and
//! its *inherit* half — the one that cannot express a value — is documented at
//! that type precisely because conflating the two is what produced this.
//!
//! So the writable surface a builder wants is an **added scratch directory**,
//! not a **removed guarantee**, and `check_request` keeps refusing every
//! writable worktree for readers and builders alike. Nothing here touches it. If
//! a future change to this module seems to need it relaxed, that is the
//! conversion ADR-0014 predicted, and the answer is the scratch mount.
//!
//! [`AnalyzerRunner`]: crate::AnalyzerRunner
//! [`Consent`]: crate::Consent
//!
//! # Sandboxed is the default; the host is opt-in
//!
//! `cargo clippy` has `cargo check` semantics, so it executes every build script
//! in the resolved tree and loads every proc macro as a dylib into the compiler —
//! measured on this repository at 54 build scripts and 7 proc macros by default,
//! 87 and 33 under `--all-features` (ADR-0020). That is code executing on the
//! host with the invoking user's filesystem and credentials, and **that the
//! toolchain is yours does not make the code yours**: linting a branch you are
//! reviewing runs its author's build scripts here.
//!
//! So ADR-0020 condition 6 puts the sandbox first and makes the host a thing a
//! person opts into. Conditions 1–2 are now built, so the default **selects
//! [`crate::lint_sandbox`]** rather than refusing — see [`decide`] for the
//! layering and [`Reason`] for what each layer tells the reader.
//!
//! What the layers say did not change; what "denied" *amounts to* did. A project
//! that denies host execution, a user config that denies it, or `--sandboxed`
//! all mean **run in the boundary**, where before they meant refuse, because
//! there was no boundary to run in. The refusal that remains is narrower and
//! sharper: a sandbox that was selected and **cannot be had** — no image, an
//! image without the linter, an image not in the local store, no hypervisor, a
//! build without `exec-boxlite`. That never becomes a host run. ADR-0020 §6, and
//! worse here than elsewhere: the person asked for isolation and would get
//! execution.
//!
//! A granted host run still records `isolation: none` and still prints its argv
//! first, because a grant changes who chose, not what happened.
//!
//! @rto:0014
//! @rto:0020

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rto_graph::{Isolation, SourceIdentity};

use crate::adapter::clippy::{self, Clippy, FeatureSet, Summary};
use crate::adapter::{Invocation, LINT_ANALYZERS, NativeContext, UNKNOWN_VERSION};
use crate::clock::rfc3339_utc;
use crate::guidance::{Guidance, Line};
use crate::ingest::NormalizedReport;
use crate::runner::{ExecError, worktree_id};
// The grant lives in its own ungated module (ADR-0020 §6 is policy, and policy
// outlives the capability it governs); it is used here as if it were local.
use crate::lint_grant::{Backend, Decision, Reason};
use crate::snippet::WorktreeSnippets;
use crate::subprocess::{ChildEnv, SubprocessError, execute, scrub_environment, stderr_tail};

/// The environment variables a Rust toolchain needs to be found.
///
/// The scrubbed base ([`scrub_environment`]) keeps `PATH` and `HOME`, which is
/// all a parse-only analyzer needs. A toolchain driven through rustup needs
/// where its toolchains and its registry live, and a user who has moved either
/// would otherwise get a shim with nothing behind it. Each entry is a locator,
/// never a credential — `CARGO_REGISTRY_TOKEN` is conspicuously not here.
///
/// Every entry is also a variable whose value is **the user's to choose**, which
/// is what makes inheriting the right verb for it. `CARGO_TARGET_DIR` was listed
/// here once and is deliberately not any more: where the build writes is not the
/// user's to choose here, it is this module's to guarantee, and a name in this
/// list can only ever pass along a value somebody else set — including, on the
/// overwhelmingly common path where nobody set one, no value at all. It is set
/// outright in [`run`] instead. See [`ChildEnv`].
const TOOLCHAIN_ENV: &[&str] = &["CARGO_HOME", "RUSTUP_HOME", "RUSTUP_TOOLCHAIN"];

/// The sentence every unavailable-sandbox refusal opens with.
///
/// A `const` rather than repeated in each variant, so the wording of *what
/// happened* cannot drift from the wording of *what to do about it*.
const NOT_AVAILABLE: &str =
    "`roteiro lint` runs the linter sandboxed, and the sandbox is not available here.";

/// The promise this refusal exists to keep, in the words ADR-0020 §6 uses.
///
/// Its own line, and last before the escape, so that the reader has been told
/// what did **not** happen before being offered the thing that would make it
/// happen deliberately.
const NEVER_FELL_BACK: Guidance = Guidance::new(&[Line::Note(&[
    "Nothing ran, and nothing fell back to this host: asking for isolation and",
    "getting execution is the one outcome this command will not produce.",
])]);

/// What [`LintError::ToolchainMissing`] says about a program no adapter
/// declares.
///
/// Unreachable while the only probes are `cargo` and `rustc`, both of which
/// [`clippy::Clippy`] declares. Kept because the alternative to "this build does
/// not know" is a guessed install line, and a guess that fails is worse than an
/// admission that does not.
const UNKNOWN_TOOL: Guidance = Guidance::new(&[Line::Note(&[
    "Roteiro does not install toolchains, and this build knows no install",
    "command for that program.",
])]);

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
    /// The sandbox was selected and cannot be had on this machine, so **nothing
    /// ran** (ADR-0020 §6).
    ///
    /// The one refusal this command must never turn into a completed run. A
    /// boundary that vanishes while the command still reports success is worse
    /// here than anywhere else in the system: the person asked for isolation and
    /// would get execution, on a tree whose build scripts are the reason they
    /// asked.
    ///
    /// Both bodies are [`Guidance`], so the template below is one short literal
    /// with no wrapping in it. It used to wrap, and leaked nine columns of source
    /// indentation into the middle of a sentence — see [`crate::guidance`].
    #[error("{}{what}{}{}", NOT_AVAILABLE, NEVER_FELL_BACK, .escape.map(|e| e.to_string()).unwrap_or_default())]
    SandboxUnavailable {
        /// What is missing, and what to do about it — because "the sandbox is
        /// unavailable" is a state and not an instruction.
        what: Guidance,
        /// How host execution could be reached instead, if it could.
        ///
        /// [`Reason::host_escape`], which is `None` for the person it would not
        /// help: nothing overrides a project's denial, so offering them a flag
        /// is the failure #426's refusals rule is about.
        escape: Option<Guidance>,
    },
    /// A sandboxed run failed inside the boundary, or could not obtain one.
    ///
    /// **Deliberately not `transparent`.** Every refusal on this path ends with
    /// the same promise — nothing ran, and nothing fell back to this host — and
    /// one wrapper appending it is what makes that true of *all* of them,
    /// including a variant somebody adds later without reading this. It was
    /// per-variant for one revision and a refusal shipped without it; the test
    /// that caught it now asserts the property here, once, rather than variant
    /// by variant.
    #[cfg(feature = "exec-boxlite")]
    #[error("{sandbox}{}", NEVER_FELL_BACK)]
    Sandbox {
        /// What went wrong with the boundary.
        #[from]
        sandbox: crate::lint_sandbox::BuilderError,
    },
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
    ///
    /// The way forward comes from the adapter's own
    /// [`crate::adapter::InstallHint`] rather than from a literal here. It used
    /// to be a literal, and a correct one — but the same URL written in two
    /// places is the drift #430 is about, one revision away from the two
    /// disagreeing about which is current.
    #[error(
        "`{program}` was not found on PATH, so `{analyzer}` could not be run. Nothing is \
         reported, because a missing tool must never read as a clean tree.{}",
        .install.unwrap_or(UNKNOWN_TOOL)
    )]
    ToolchainMissing {
        /// The program that was looked for.
        program: String,
        /// The linter it was needed for.
        analyzer: String,
        /// How to obtain `program`, as its adapter declares it.
        install: Option<Guidance>,
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
    /// The scratch build directory could not be created, so there is nowhere
    /// for the build to write that is not the tree under review.
    ///
    /// A hard failure rather than a fallback to cargo's default. The default is
    /// `<worktree>/target`, which is the one place this module promises not to
    /// write — so "carry on without it" would quietly trade the guarantee for a
    /// completed run, which is the shape of downgrade ADR-0020 §3 refuses.
    #[error(
        "could not create the scratch build directory {path}: {source}. Nothing ran — the build \
         would otherwise have written into the tree being linted, which `roteiro lint` does not do."
    )]
    ScratchUnavailable {
        /// The directory that could not be created.
        path: String,
        /// Why not.
        source: std::io::Error,
    },
    /// A candidate scratch root is a relative path, and a relative path does
    /// not name one directory here.
    ///
    /// Two processes would resolve it, against two different working
    /// directories: this one creates the directory, and **cargo** — running with
    /// its working directory set to the worktree — resolves `CARGO_TARGET_DIR`
    /// to decide where to write. So a relative root means the directory is
    /// created in one place and the build lands in another, and the other one is
    /// inside the tree under review.
    ///
    /// Refused rather than resolved because there is no resolution that is
    /// obviously right. Picking this process's working directory would silently
    /// disagree with cargo; picking cargo's would put the scratch inside the
    /// worktree by construction. When a value has two defensible readings and
    /// one of them is the defect, the answer is to say so, not to choose.
    #[error(
        "{variable} is set to a relative path ({path}), and `roteiro lint` needs an absolute \
         one. A relative build directory is resolved by cargo against the tree being linted, so \
         it would put the build inside it. Set {variable} to an absolute path."
    )]
    ScratchRootNotAbsolute {
        /// The variable that named it, so the reader knows what to change.
        ///
        /// Not called `source`: `thiserror` reads a field of that name as the
        /// underlying error, and this one is a variable name.
        variable: &'static str,
        /// The relative path it was set to.
        path: String,
    },
    /// Every candidate scratch root turned out to be inside the tree being
    /// linted, so there is nowhere to build that does not defeat the point.
    ///
    /// Reachable only by pointing `ROTEIRO_HOME` inside the worktree you are
    /// linting. It is an error rather than a shrug because the alternative is
    /// this module doing the exact thing it says it does not, and because a
    /// guarantee that quietly lapses in a corner is not one — the corner is
    /// where somebody eventually is.
    #[error(
        "every candidate build directory is inside {root}, which is the tree being linted: \
         {candidates}. `roteiro lint` will not build into the tree it is reporting on. Point \
         `ROTEIRO_HOME` somewhere outside it."
    )]
    ScratchWouldBeInsideTheTree {
        /// The worktree that would have been written to.
        root: String,
        /// The candidates that were rejected, in the order they were tried.
        candidates: String,
    },
    /// `cargo` refused to write `Cargo.lock`, because this module passed
    /// `--locked`.
    ///
    /// Named rather than left to fall through as a malformed report, because
    /// the two look identical from the output — an empty stdout and a 101 — and
    /// the causes could not be less alike. This one is a tree whose lockfile is
    /// missing or stale, and the remedy belongs to the person, not to roteiro:
    /// generating it is a **write into the tree under review**, which is the
    /// thing this command does not do on your behalf.
    #[error(
        "`{command}` needs `Cargo.lock` to be written, and `roteiro lint` passes `--locked` so \
         that it is not: a lint must not modify the tree it is reporting on. Run `cargo \
         generate-lockfile` (or `cargo check`) yourself and lint again.{stderr}"
    )]
    LockfileWouldBeWritten {
        /// The argv that ran.
        command: String,
        /// The tail of cargo's standard error, which names the lockfile.
        stderr: String,
    },
    /// The build did not complete **and** produced no diagnostics explaining
    /// why — so there is nothing to report and nothing to conclude.
    ///
    /// Separated from a build that failed *with* diagnostics, which is the
    /// ordinary outcome of a repository that denies a lint group and is reported
    /// normally. This one is the shape that would otherwise print "0 finding(s)"
    /// over a run that never compiled anything.
    #[error(
        "`{command}` exited {status} without completing the build and without a single \
         diagnostic, so there is nothing to report — this is not a clean tree.{}{stderr}",
        .hint.map(|h| h.to_string()).unwrap_or_default()
    )]
    BuildProducedNothing {
        /// The argv that ran.
        command: String,
        /// Its exit status.
        status: i32,
        /// What to look at first, when the backend narrows it.
        ///
        /// Empty for a host run, where a build that will not build is the user's
        /// own tree and their own machine and they know both. For a sandboxed
        /// one it names the cause that is **new** and therefore not yet obvious:
        /// the image has to be able to *build* the tree, not merely lint it, and
        /// a native build dependency the image lacks fails inside a machine the
        /// reader cannot inspect. Measured on this repository — `--all-targets`
        /// compiles a dev-dependency on vendored llama.cpp, whose build script
        /// needs `libclang`, and an image without it panics in `bindgen` with no
        /// diagnostic to show for it.
        hint: Option<Guidance>,
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
    /// The workspace root it ran in, as a **host** path — where the tree
    /// actually is, whatever a guest called it.
    pub worktree: PathBuf,
    /// The digest-pinned image the run happened inside, or `None` for a host
    /// run.
    ///
    /// `Some` is the whole of what makes [`LintOutcome::isolation`] checkable
    /// rather than asserted: an image reference names the thing the boundary was
    /// made of, and a reader who wants to know what executed their build scripts
    /// can look it up.
    pub image: Option<String>,
    /// Where the build wrote — the `CARGO_TARGET_DIR` [`run`] chose.
    ///
    /// Reported rather than kept private, and that is the whole answer to the
    /// one real objection to choosing it: overriding a variable the caller set
    /// is a surprise, and a surprise stops being one when it is stated. It also
    /// completes [`LintOutcome::command`]'s promise — the argv alone is not
    /// reproducible by hand if the environment it needs is a secret.
    pub scratch: PathBuf,
}

/// The argv `analyzer` would run on `backend` at this feature set, or `None` if
/// this is not a linter roteiro drives.
///
/// Exists so a caller can disclose the command **before** running it without
/// having to know which analyzer maps to which argv — a caller that guessed
/// would eventually announce one linter's command under another one's name.
///
/// `backend` is a parameter for the same reason, one level down. The two
/// backends' argvs differ by `--offline`, which the sandboxed one passes because
/// its network is denied by the hypervisor — so a disclosure that ignored the
/// backend would print a command that is one token away from the one that ran,
/// in the one line a user is relying on being told the truth.
#[must_use]
pub fn invocation(analyzer: &str, features: &FeatureSet, backend: Backend) -> Option<Invocation> {
    (analyzer == clippy::ANALYZER).then(|| match backend {
        Backend::Host => Clippy::invocation(features),
        Backend::Sandbox => Clippy::offline_invocation(features),
    })
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
/// `decision` is the outcome of [`decide`], and it is a **required argument
/// rather than something read from a global** for the reason
/// [`crate::SubprocessRunner::new`] takes its flag at construction: a caller must
/// not be able to execute a build here by forgetting to check something. It
/// chooses the backend and nothing else — whether that backend can be *had* is
/// asked below, and answered with a refusal rather than the other one.
///
/// `image` is `[lint] image`: the digest-pinned OCI image a sandboxed run
/// happens inside. Roteiro ships no default and will not choose one, because
/// there is no first-party Rust image carrying `clippy` — `rust-lang/docker-rust`
/// builds every stable and nightly variant `--profile minimal` — and picking a
/// third party's would make somebody else's container the boundary, chosen here
/// and noticed by nobody. `None` with the sandbox selected is a refusal that
/// says how to supply one.
///
/// # The guest's toolchain is not this machine's, and the count moves with it
///
/// A sandboxed run reports what the **image's** rustc and clippy said. Lint
/// names are symbols in a compiler, so a different compiler legitimately fires a
/// different set: `roteiro lint clippy` sandboxed can disagree with `cargo
/// clippy` run in the same tree on the same day, with no defect on either side.
/// Because nothing is stored (ADR-0020 v1.1) that is a **surprise rather than a
/// corruption** — there is no series for it to falsify — but it is a real
/// surprise, so [`LintOutcome::toolchain`] is read out of the guest and printed
/// with every report.
///
/// # Errors
/// Returns [`LintError`]: an unknown analyzer, no cargo project, **a selected
/// sandbox that is not available** (which never becomes a host run), a missing
/// toolchain or linter (each naming what to install), a failed probe, a failed
/// execution, output that is not a report, or a build that produced neither a
/// completion nor a diagnostic.
pub fn run(
    analyzer: &str,
    dir: &Path,
    features: &FeatureSet,
    decision: Decision,
    image: Option<&str>,
) -> Result<LintOutcome, LintError> {
    // The request shape first. This used to come *after* the grant, so that a
    // shut gate could not be probed for which analyzers were behind it — a
    // property that retired with the gate. There is no longer an outcome where
    // every request is refused: saying nothing selects the sandbox and the
    // sandbox runs, so `clippy` and `no-such-linter` were always going to
    // diverge, and naming the real error first is the more useful order.
    if !LINT_ANALYZERS.contains(&analyzer) {
        return Err(LintError::UnknownAnalyzer {
            requested: analyzer.to_owned(),
            known: LINT_ANALYZERS.join(", "),
        });
    }

    // Located on the host for both backends, because the sandboxed one has to
    // know **what to mount** before it has a guest to ask. Worth being exact
    // about what that spends: `cargo locate-project` reads manifests and exits.
    // It compiles nothing, so it runs none of the build scripts and loads none
    // of the proc macros that are the whole reason the sandbox exists — the
    // exposure ADR-0020 inverts the threat model over is not opened by finding
    // out where a `Cargo.toml` is.
    let root = workspace_root(dir, analyzer)?;

    match decision.backend() {
        Backend::Host => run_on_host(analyzer, &root, features),
        Backend::Sandbox => run_sandboxed(analyzer, &root, features, image, decision.reason),
    }
}

/// Run the linter in a sandbox, or refuse — never fall back (ADR-0020 §6).
///
/// The `exec-boxlite` half. Its counterpart below is the same function in a
/// build that has no sandboxed backend compiled in, and the two exist as a pair
/// rather than as one function with an `if` so that **there is no branch in
/// which a missing boundary reaches a host run**: in a build without the
/// feature this function cannot call the host runner, because it cannot see it.
#[cfg(feature = "exec-boxlite")]
fn run_sandboxed(
    analyzer: &str,
    root: &Path,
    features: &FeatureSet,
    image: Option<&str>,
    reason: Reason,
) -> Result<LintOutcome, LintError> {
    let Some(image) = image else {
        return Err(LintError::SandboxUnavailable {
            what: crate::lint_sandbox::NO_IMAGE_CONFIGURED,
            escape: reason.host_escape(),
        });
    };
    crate::lint_sandbox::run(analyzer, root, features, image)
}

/// What a build with no sandboxed backend says, and how to get one.
///
/// Compiled **only** into the build that needs it, so the sentence and the
/// `cfg` that produces it cannot drift apart. The bootstrap order is spelled out
/// rather than named because `exec-boxlite`'s build script requires the verified
/// runtime archive at compile time — a reader told only "enable the feature"
/// gets a build failure instead of a sandbox.
#[cfg(not(feature = "exec-boxlite"))]
const NO_SANDBOX_IN_THIS_BUILD: Guidance = Guidance::new(&[
    Line::Note(&[
        "This build was compiled without the `exec-boxlite` feature, so it contains no",
        "sandboxed backend at all — not one that failed, one that is not there.",
    ]),
    Line::Note(&["Provision the runtime first, then rebuild with the feature:"]),
    Line::Command("roteiro security prefetch --analyzer sandbox --allow-download"),
    Line::Command("cargo install roteiro --features exec-boxlite"),
    Line::Note(&[
        "That order is not arbitrary: the feature's build script verifies the runtime",
        "archive at compile time, so a rebuild without it fails rather than degrades.",
        "It also needs `protoc >= 3.12` on the build host.",
    ]),
    Line::Note(&[
        "Or ingest a report produced elsewhere, which needs no sandbox at all:",
        "`roteiro security ingest`.",
    ]),
]);

/// The same function in a build with no sandboxed backend.
///
/// A refusal that names the feature, rather than a host run. The alternative —
/// "the sandbox is not compiled in, so use the other backend" — is precisely the
/// silent downgrade ADR-0020 §6 forbids, and it would be reached by the people
/// least equipped to notice: a stock install, where `exec-boxlite` is off.
#[cfg(not(feature = "exec-boxlite"))]
fn run_sandboxed(
    _analyzer: &str,
    _root: &Path,
    _features: &FeatureSet,
    _image: Option<&str>,
    reason: Reason,
) -> Result<LintOutcome, LintError> {
    Err(LintError::SandboxUnavailable {
        what: NO_SANDBOX_IN_THIS_BUILD,
        escape: reason.host_escape(),
    })
}

/// Run the linter on this machine, with this user's toolchain and credentials.
///
/// Unchanged in substance by conditions 1-2: the host path is what it was, and
/// the sandbox is an addition beside it rather than a rewrite of it. What
/// changed is only that reaching this function now takes a **grant** rather than
/// being what happens by default.
fn run_on_host(
    analyzer: &str,
    root: &Path,
    features: &FeatureSet,
) -> Result<LintOutcome, LintError> {
    // Chosen before anything is run, and a failure to get one stops the run:
    // every process below this line is a cargo whose default target directory is
    // inside the tree being linted.
    let scratch = scratch_dir(root, Backend::Host)?;
    let set = [(
        "CARGO_TARGET_DIR",
        std::ffi::OsString::from(scratch.as_os_str()),
    )];
    let env = ChildEnv {
        inherit: TOOLCHAIN_ENV,
        set: &set,
    };

    let toolchain = probe_toolchain(root, analyzer, &env)?;
    let invocation = Clippy::invocation(features);
    let command = argv(&invocation);

    let started_at = rfc3339_utc(std::time::SystemTime::now());
    let output = execute(&invocation, root, analyzer, &env)?;
    let ended_at = rfc3339_utc(std::time::SystemTime::now());

    // Before the parse, because the parse cannot tell this apart from any other
    // run that emitted nothing and would report it as a malformed report — an
    // answer that is true and useless. `--locked` is this module's doing, so
    // explaining it is this module's job.
    if lockfile_refused(&output.stderr) {
        return Err(LintError::LockfileWouldBeWritten {
            command: command.join(" "),
            stderr: stderr_tail(&output.stderr),
        });
    }

    let snippets = WorktreeSnippets::new(root);
    let source = SourceIdentity::default();
    let ctx = NativeContext {
        started_at,
        ended_at,
        analyzer_version: Some(short_version(&toolchain.linter)),
        exit_status: output.status,
        source: &source,
        rules_digest: None,
        advisory_db: None,
        worktree: Some(root),
        snippets: &snippets,
    };
    let (report, summary) = Clippy::parse(&output.stdout, &ctx)?;

    // A build that neither finished nor said anything is not a clean tree, and
    // this is where that stops being a number.
    if !summary.build_succeeded && report.findings.is_empty() {
        return Err(LintError::BuildProducedNothing {
            command: command.join(" "),
            status: output.status,
            hint: None,
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
        worktree: root.to_path_buf(),
        image: None,
        scratch,
    })
}

/// Where this run's build artefacts go: a directory outside the tree under
/// review, keyed to that tree.
///
/// # Why it is not under the worktree
///
/// Because the module promises it is not. `cargo`'s default is
/// `<worktree>/target`, and `roteiro lint` reports on a tree it must leave
/// exactly as it found it — the more so for the case the command is *for*,
/// linting a branch somebody else wrote, where a stray `target/` is at best
/// noise in `git status` and at worst a build artefact attributed to the author.
///
/// # Why it is not under the asset cache
///
/// `~/.roteiro/security` is a sibling, not a parent. Everything under the asset
/// cache is re-obtainable from a **pinned digest**, and ADR-0014 v1.6 leans on
/// exactly that to make `roteiro security clear` safe to offer. Compiled build
/// scripts are re-obtainable but not from any digest, so filing them there would
/// weaken the property that makes a destructive verb acceptable — by putting
/// something under the root whose safety argument does not cover it.
///
/// # Why it is keyed per tree
///
/// ADR-0014 v1.6 draws the line at what the directory holds: content-addressed
/// and verified artefacts may be shared across repositories, a build scratch
/// holding *compiled build scripts* may not, because sharing one lets a build
/// script from one repository leave something a build in another picks up —
/// the execution boundary defeated through a cache rather than through a mount.
///
/// The key is [`crate::worktree_id`] of the workspace root, which is per
/// *checkout* and therefore strictly finer than per repository: two worktrees of
/// one repository do not share either. That is the safe direction to err in, it
/// reuses the derivation the findings store already uses for the same question
/// rather than inventing a second one, and it needs no git — `roteiro lint`
/// works on a directory that was never a repository at all.
///
/// # Why it is keyed per backend as well as per tree
///
/// Because the two backends put **different operating systems' executables** in
/// it under the same names. Cargo does not separate them: without `--target` a
/// build lands in `<scratch>/debug`, so a guest's `build-script-build` and a
/// host's occupy one path and each invalidates the other — which on a macOS host
/// is merely churn, and on a Linux host is the hazard ADR-0014 v1.6 draws the
/// line at, run backwards. A build script compiled *inside the boundary* would
/// be sitting in a directory a later host lint builds into, which is the
/// execution boundary defeated through a cache rather than through a mount.
///
/// So a sandboxed run gets its own, and the suffix is on the directory rather
/// than inside it so that the two can never be confused by a person reading
/// `~/.roteiro/lint/target` either.
///
/// # Errors
/// Returns [`LintError::ScratchUnavailable`] if the directory cannot be created,
/// or whatever [`scratch_path`] refused with.
pub(crate) fn scratch_dir(root: &Path, backend: Backend) -> Result<PathBuf, LintError> {
    let dir = scratch_path(
        &scratch_roots_from(
            std::env::var_os("ROTEIRO_HOME").map(PathBuf::from),
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(PathBuf::from),
            std::env::temp_dir(),
        ),
        root,
        backend,
    )?;
    std::fs::create_dir_all(&dir).map_err(|source| LintError::ScratchUnavailable {
        path: dir.display().to_string(),
        source,
    })?;
    Ok(dir)
}

/// Choose and key the scratch directory, touching neither the environment nor
/// the filesystem — so a test can ask the shipped code the question rather than
/// restating its arithmetic and checking that.
///
/// That distinction is not pedantry here: an earlier version of the test below
/// rebuilt the `<root>/<id>` join itself, and consequently went on passing while
/// this function handed every repository in the world the same directory.
///
/// # Errors
/// Returns [`LintError::ScratchRootNotAbsolute`] if a candidate it reaches is a
/// relative path, [`LintError::ScratchWouldBeInsideTheTree`] if every candidate
/// in `roots` lies within `root`, and [`LintError::ScratchUnavailable`] if no id
/// can be derived for `root`.
fn scratch_path(roots: &[Candidate], root: &Path, backend: Backend) -> Result<PathBuf, LintError> {
    let id = worktree_id(root).map_err(|source| LintError::ScratchUnavailable {
        path: root.display().to_string(),
        source: std::io::Error::other(source.to_string()),
    })?;
    // Appended to the id rather than added as a directory level, so that the
    // two are siblings a person can see side by side and neither can ever be a
    // prefix of the other's path.
    let leaf = match backend {
        Backend::Host => id.as_str().to_owned(),
        Backend::Sandbox => format!("{}.sandbox", id.as_str()),
    };
    for candidate in roots {
        // **Before** the containment check, and the order is the whole of it.
        // `is_inside` has to make its argument absolute to compare it with
        // anything, and the only working directory it can use is *this*
        // process's — while the path is destined for a cargo whose working
        // directory is the worktree. Handed a relative candidate the check
        // therefore answers a question about a directory that is not the one the
        // build would use, and answers it confidently. Refusing here means it is
        // never asked: every path reaching `is_inside` is already absolute, so
        // its answer is about the same directory cargo would write to.
        if !candidate.path.is_absolute() {
            return Err(LintError::ScratchRootNotAbsolute {
                variable: candidate.variable,
                path: candidate.path.display().to_string(),
            });
        }
        if !is_inside(&candidate.path, root) {
            return Ok(candidate.path.join(&leaf));
        }
    }
    Err(LintError::ScratchWouldBeInsideTheTree {
        root: root.display().to_string(),
        candidates: roots
            .iter()
            .map(|c| format!("{} ({})", c.path.display(), c.variable))
            .collect::<Vec<_>>()
            .join(", "),
    })
}

/// A candidate scratch root, and the input that produced it.
///
/// The variable travels with the path because both refusals are only useful
/// if they name the variable the reader has to change — "every candidate is
/// inside the tree" is a puzzle, and "`ROTEIRO_HOME` is inside the tree" is an
/// instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Candidate {
    /// The environment variable that named it.
    variable: &'static str,
    /// Where it points, exactly as that variable spelled it.
    path: PathBuf,
}

/// The scratch roots to try, best first, without touching the environment — so
/// it is testable.
///
/// The precedence is [`crate::asset_paths::asset_root`]'s, deliberately: a user
/// who has moved `ROTEIRO_HOME` has moved all of roteiro's state, and a second
/// scheme here would leave one of the two somewhere they did not put it.
///
/// It returns a **list** rather than an answer because every input here is the
/// caller's, and this module's guarantee may not be. `ROTEIRO_HOME` is an
/// environment variable like any other; pointed inside the worktree it would
/// reproduce the original defect through the fix's own front door. So the
/// candidates are checked against the tree rather than trusted, and the last one
/// is the system temporary directory — the only one that is outside a given
/// worktree for a structural reason rather than by convention.
///
/// The floor is emphatically **not** `.`, which is where
/// [`crate::asset_paths::asset_root`] ends and would be wrong here: cargo
/// resolves a relative `CARGO_TARGET_DIR` against the *child's* working
/// directory, and the child's working directory is the worktree.
///
/// Nothing here makes a candidate absolute, and that is deliberate — every
/// input is a path a user chose, and silently rewriting one is how a variable
/// comes to mean something other than what it says. [`scratch_path`] refuses a
/// relative candidate instead, which is the same hazard handled where it can be
/// reported rather than where it can only be papered over.
fn scratch_roots_from(
    roteiro_home: Option<PathBuf>,
    home: Option<PathBuf>,
    temp: PathBuf,
) -> Vec<Candidate> {
    roteiro_home
        .map(|dir| ("ROTEIRO_HOME", dir))
        .into_iter()
        .chain(home.map(|dir| ("HOME", dir.join(".roteiro"))))
        .chain(std::iter::once(("TMPDIR", temp)))
        .map(|(variable, dir)| Candidate {
            variable,
            path: dir.join("lint").join("target"),
        })
        .collect()
}

/// Whether `candidate` lies within `root`.
///
/// **`candidate` must be absolute**, and [`scratch_path`] refuses before calling
/// this rather than leaving it to be remembered. The reason is that this
/// function cannot honour a relative one even in principle: making it absolute
/// takes a working directory, the only one available here belongs to *this*
/// process, and the path is destined for a cargo whose working directory is the
/// worktree. It would compare the wrong directory and say so with confidence.
///
/// Both sides go through [`resolved`] first. Comparing the spellings as given
/// is not enough on any machine where a symlink stands between the two — on
/// macOS the system temporary directory is reached as `/var/...` and resolves to
/// `/private/var/...`, so a candidate genuinely inside the worktree compares as
/// outside it and the check passes something it should have caught. That is not
/// a hypothetical: it is what this repository's own fixtures do, and it is how
/// this function was found to be wrong the first time.
///
/// A false negative here is a build inside the tree under review, so where the
/// two disagree the answer is yes.
fn is_inside(candidate: &Path, root: &Path) -> bool {
    let candidate = resolved(candidate);
    std::iter::once(resolved(root))
        .chain(std::path::absolute(root).ok())
        .any(|spelling| candidate.starts_with(&spelling))
}

/// `path` with every symlink resolved as far as the filesystem can, which for a
/// path that does not exist yet is as far as its deepest existing ancestor.
///
/// [`Path::canonicalize`] refuses a path that is not there, and a scratch
/// directory is not there — that is the whole reason it is about to be created.
/// So the existing part is canonicalized and the rest is re-appended: enough to
/// compare two paths that are spelled differently and name the same place.
fn resolved(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut suffix = PathBuf::new();
    let mut here = absolute.as_path();
    loop {
        if let Ok(real) = here.canonicalize() {
            return real.join(&suffix);
        }
        let (Some(name), Some(parent)) = (here.file_name(), here.parent()) else {
            return absolute;
        };
        suffix = Path::new(name).join(&suffix);
        here = parent;
    }
}

/// Whether cargo refused to run because writing `Cargo.lock` was the only way
/// forward and `--locked` forbade it.
///
/// Matched on the clause common to both wordings cargo uses — the lockfile is
/// missing, and the lockfile is stale — rather than on either message in full,
/// which would silently stop matching on a cargo release and send the caller
/// back to the unhelpful answer this exists to replace.
pub(crate) fn lockfile_refused(stderr: &[u8]) -> bool {
    String::from_utf8_lossy(stderr).contains("--locked was passed")
}

/// The full argv of an invocation, program first.
pub(crate) fn argv(invocation: &Invocation) -> Vec<String> {
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
    let probe = probe(
        "cargo",
        &args,
        dir,
        analyzer,
        &ChildEnv {
            inherit: TOOLCHAIN_ENV,
            ..ChildEnv::default()
        },
    )?;
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
fn probe_toolchain(
    root: &Path,
    analyzer: &str,
    env: &ChildEnv<'_>,
) -> Result<Toolchain, LintError> {
    let clippy_args = ["clippy", "--version"];
    let clippy = probe("cargo", &clippy_args, root, analyzer, env)?;
    if !clippy.ok || clippy.stdout.trim().is_empty() {
        return Err(LintError::AnalyzerNotInstalled {
            analyzer: analyzer.to_owned(),
            command: rendered("cargo", &clippy_args),
            // The adapter's constant, not a literal: the same instruction is
            // printed by `clippy`'s install hint when the binary is absent
            // rather than the component, and one of the two would eventually be
            // updated alone.
            install: crate::adapter::clippy::COMPONENT_ADD.to_owned(),
            stderr: stderr_tail(clippy.stderr.as_bytes()),
        });
    }

    let rustc_args = ["-vV"];
    let rustc = probe("rustc", &rustc_args, root, analyzer, env)?;
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
///
/// `env` is passed in rather than rebuilt here for that reason: a probe that
/// assembled its own would be checking a different environment from the one the
/// run uses, which is the failure this function's whole existence guards
/// against.
fn probe(
    program: &str,
    args: &[&str],
    dir: &Path,
    analyzer: &str,
    env: &ChildEnv<'_>,
) -> Result<Probe, LintError> {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    scrub_environment(&mut command, env);

    let output = command.output().map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            LintError::ToolchainMissing {
                program: program.to_owned(),
                analyzer: analyzer.to_owned(),
                install: crate::adapter::install_hint(program),
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
pub(crate) fn first_line(text: &str) -> String {
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
pub(crate) fn short_version(line: &str) -> String {
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
pub(crate) fn parse_rustc_verbose(text: &str) -> (String, String) {
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
        Candidate, Guidance, LINT_ANALYZERS, Line, LintError, TOOLCHAIN_ENV, first_line, is_inside,
        lockfile_refused, parse_rustc_verbose, run, scratch_path, scratch_roots_from,
        short_version,
    };
    use crate::adapter::clippy::FeatureSet;
    use crate::lint_grant::{Backend, ConfigGrant, Requested, decide};
    use std::path::{Path, PathBuf};

    /// A decision that permits the host, for tests about everything *else*.
    fn granted() -> crate::lint_grant::Decision {
        let decision = decide(ConfigGrant::default(), Requested::Host);
        assert!(decision.granted(), "the fixture must actually grant");
        decision
    }

    /// ADR-0020 §6's **never fall back**, at the seam that enforces it.
    ///
    /// Saying nothing selects the sandbox, and a sandbox that cannot be had
    /// refuses. What this pins is the thing that must not happen: it must not
    /// come back `Ok`, and it must not come back as a host run. Asserted on the
    /// variant rather than on the message, because the message is allowed to
    /// improve and the outcome is not.
    ///
    /// It runs in a build that *has* `exec-boxlite`, where the refusal is a
    /// missing image rather than a missing feature — both are
    /// `SandboxUnavailable`, and both are terminal, which is the property.
    #[test]
    fn a_selected_sandbox_that_cannot_be_had_refuses_rather_than_running_here() {
        let sandboxed = decide(ConfigGrant::default(), Requested::Unset);
        assert!(!sandboxed.granted(), "the fixture must select the sandbox");
        let err = run(
            "clippy",
            std::path::Path::new("."),
            &FeatureSet::Defaults,
            sandboxed,
            // No image, which is the state every stock install is in.
            None,
        )
        .expect_err("a sandbox that cannot be had must refuse");
        assert!(
            matches!(err, LintError::SandboxUnavailable { .. }),
            "a selected sandbox must never become a host run: {err:?}"
        );
    }

    /// Every route to a selected-but-unavailable sandbox refuses, including the
    /// three that came from a layer the person did not type.
    ///
    /// Written across all four sandbox-selecting reasons because "it does not
    /// fall back" is a claim about the *outcome*, and an outcome that held for
    /// the default while quietly differing for a project denial would be the
    /// same defect with a smaller blast radius.
    #[test]
    fn no_route_to_the_sandbox_ends_up_on_the_host() {
        let routes = [
            (ConfigGrant::default(), Requested::Unset),
            (ConfigGrant::default(), Requested::Sandbox),
            (
                ConfigGrant::from_layers(None, Some(false)),
                Requested::Unset,
            ),
            // A project denial outranks even `--allow-unsandboxed`.
            (ConfigGrant::from_layers(Some(false), None), Requested::Host),
        ];
        for (config, requested) in routes {
            let decision = decide(config, requested);
            assert_eq!(decision.backend(), crate::lint_grant::Backend::Sandbox);
            let err = run(
                "clippy",
                std::path::Path::new("."),
                &FeatureSet::Defaults,
                decision,
                None,
            )
            .expect_err("must refuse");
            assert!(
                matches!(err, LintError::SandboxUnavailable { .. }),
                "{:?} fell out of the sandbox: {err:?}",
                decision.reason
            );
        }
    }

    /// A refusal has to name a way forward **that would work for this person**,
    /// and the one person it must not offer `--allow-unsandboxed` to is the one
    /// working in a repository that denies it (#426).
    #[test]
    fn a_refusal_offers_the_host_only_to_someone_who_could_take_it() {
        let denied = decide(ConfigGrant::from_layers(Some(false), None), Requested::Host);
        let err = run(
            "clippy",
            std::path::Path::new("."),
            &FeatureSet::Defaults,
            denied,
            None,
        )
        .expect_err("must refuse")
        .to_string();
        assert!(
            !err.contains("--allow-unsandboxed"),
            "a project denial cannot be escaped, so offering the flag wastes the reader's time: \
             {err}"
        );

        let ordinary = decide(ConfigGrant::default(), Requested::Unset);
        let err = run(
            "clippy",
            std::path::Path::new("."),
            &FeatureSet::Defaults,
            ordinary,
            None,
        )
        .expect_err("must refuse")
        .to_string();
        assert!(err.contains("--allow-unsandboxed"), "{err}");
    }

    /// Asking a linter surface for a storing analyzer is a mistake worth a
    /// sentence: the two commands do different things to the store, so the
    /// refusal points at the other one rather than just listing names.
    #[test]
    fn refuses_an_analyzer_it_does_not_drive_before_running_anything() {
        let err = run(
            "semgrep",
            std::path::Path::new("/nonexistent"),
            &FeatureSet::Defaults,
            granted(),
            None,
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
            install: crate::adapter::install_hint("cargo"),
        }
        .to_string();
        assert!(missing_toolchain.contains("not found on PATH"));
        assert!(missing_toolchain.contains("https://rustup.rs"));
        assert!(missing_toolchain.contains("must never read as a clean tree"));
        // The toolchain, not the component: a reader with no `cargo` cannot run
        // `rustup component add` against a toolchain they do not have, so the
        // two absences must not print the same instruction. #430's "right *kind*
        // of way forward".
        assert!(
            !missing_toolchain.contains(crate::adapter::clippy::COMPONENT_ADD),
            "{missing_toolchain}"
        );

        let missing_component = LintError::AnalyzerNotInstalled {
            analyzer: "clippy".to_owned(),
            command: "cargo clippy --version".to_owned(),
            install: crate::adapter::clippy::COMPONENT_ADD.to_owned(),
            stderr: String::new(),
        }
        .to_string();
        assert!(missing_component.contains("rustup component add clippy"));
        assert!(missing_component.contains("must never read as a clean tree"));

        let nothing = LintError::BuildProducedNothing {
            command: "cargo clippy".to_owned(),
            status: 101,
            hint: None,
            stderr: String::new(),
        }
        .to_string();
        assert!(
            nothing.contains("this is not a clean tree"),
            "{nothing}: a build that said nothing must not read as zero findings"
        );

        // A sandboxed run adds the cause a reader cannot look for themselves,
        // because it is inside a machine they cannot open a shell in. A host run
        // adds nothing: their tree, their machine, and they know both.
        let hinted = LintError::BuildProducedNothing {
            command: "cargo clippy".to_owned(),
            status: 101,
            hint: Some(Guidance::new(&[Line::Note(&[
                "It ran sandboxed, so check the image first.",
            ])])),
            stderr: String::new(),
        }
        .to_string();
        assert!(hinted.contains("ran sandboxed"), "{hinted}");
        assert!(
            !nothing.contains("sandboxed"),
            "a host failure must not be explained by a boundary it did not have: {nothing}"
        );
    }

    /// The disclosed argv is the one that will actually run.
    ///
    /// The two backends' argvs differ by `--offline`, which the sandboxed one
    /// passes because its network is denied by the hypervisor. A disclosure that
    /// ignored the backend would print a command one token away from the one
    /// that ran — in the one line a user is relying on being told the truth,
    /// which makes it worse than printing nothing.
    #[test]
    fn the_disclosed_argv_is_the_one_that_will_run() {
        let features = FeatureSet::Defaults;
        let host = super::invocation("clippy", &features, Backend::Host).expect("an argv");
        let sandbox = super::invocation("clippy", &features, Backend::Sandbox).expect("an argv");
        assert!(
            !host.args.contains(&"--offline".to_owned()),
            "the host may reach a registry, and conditions 1-2 did not change that: {:?}",
            host.args
        );
        assert!(
            sandbox.args.contains(&"--offline".to_owned()),
            "a guest with no interface must be told so: {:?}",
            sandbox.args
        );
        // Otherwise identical, so the difference is *only* the boundary — a
        // sandboxed run that quietly linted something else would be the harder
        // bug to see.
        let without: Vec<&String> = sandbox
            .args
            .iter()
            .filter(|a| a.as_str() != "--offline")
            .collect();
        assert_eq!(without, host.args.iter().collect::<Vec<_>>());
        assert_eq!(sandbox.program, host.program);
        assert_eq!(sandbox.success_statuses, host.success_statuses);
        // And an analyzer this build does not drive gets no argv on either
        // backend, so a caller cannot announce one linter's command under
        // another one's name.
        for backend in [Backend::Host, Backend::Sandbox] {
            assert!(super::invocation("semgrep", &features, backend).is_none());
        }
    }

    /// **Every** sandbox refusal ends with the promise, because one wrapper
    /// appends it rather than each variant remembering to.
    ///
    /// Written after breaking it. The promise was per-variant, converting one of
    /// them to a [`Guidance`] dropped it, and the refusal that shipped for a
    /// revision said an image was missing without saying that nothing had run
    /// here — which is the single thing ADR-0020 §6 asks this command to
    /// guarantee.
    ///
    /// Driven over every variant rather than a representative one, so a variant
    /// added later is covered by construction: it cannot reach a caller except
    /// through [`LintError::Sandbox`].
    #[cfg(feature = "exec-boxlite")]
    #[test]
    fn every_sandbox_refusal_promises_that_nothing_fell_back_to_the_host() {
        use crate::lint_sandbox::BuilderError;

        let variants: Vec<LintError> = vec![
            BuilderError::ImageNotProvisioned {
                analyzer: "clippy".to_owned(),
                reference: "x@sha256:0".to_owned(),
            }
            .into(),
            BuilderError::ImageLacksLinter {
                analyzer: "clippy".to_owned(),
                reference: "x@sha256:0".to_owned(),
                probe: "cargo clippy --version".to_owned(),
                stderr: String::new(),
            }
            .into(),
            BuilderError::ProbeFailed {
                probe: "rustc -vV".to_owned(),
                reference: "x@sha256:0".to_owned(),
                stderr: String::new(),
            }
            .into(),
            BuilderError::ColdCache {
                stderr: String::new(),
            }
            .into(),
            BuilderError::NoPackageCache {
                path: "/nowhere".to_owned(),
            }
            .into(),
            BuilderError::Killed {
                command: "cargo clippy".to_owned(),
                signal: 9,
                memory_mib: 4096,
            }
            .into(),
            BuilderError::OutputTooLarge {
                command: "cargo clippy".to_owned(),
                max: 1,
            }
            .into(),
            BuilderError::UnexpectedStatus {
                analyzer: "clippy".to_owned(),
                command: "cargo clippy".to_owned(),
                status: 42,
                expected: "0, 101".to_owned(),
                stderr: String::new(),
            }
            .into(),
        ];
        for error in variants {
            let message = error.to_string();
            assert!(
                message.contains("nothing fell back to this host"),
                "a sandbox refusal that does not say nothing ran here:\n{message}"
            );
            // And it is said **once** — the wrapper owns it, so a variant that
            // also says it is a duplicate a reader would read twice.
            assert_eq!(
                message.matches("nothing fell back to this host").count(),
                1,
                "said more than once:\n{message}"
            );
        }
    }

    /// A sandboxed run and a host run of the same tree must **not** share a
    /// scratch directory.
    ///
    /// They put different operating systems' executables under the same names:
    /// without `--target`, cargo lands both in `<scratch>/debug`, so a guest's
    /// `build-script-build` and a host's occupy one path. On a macOS host that
    /// is churn; on a Linux host it is ADR-0014 v1.6's hazard run backwards — a
    /// build script compiled *inside* the boundary sitting in a directory a
    /// later host lint builds into.
    ///
    /// Measured: the two really do differ in format. A guest-built build script
    /// is `ELF 64-bit … ARM aarch64 … GNU/Linux`; the same host's is `Mach-O`.
    #[test]
    fn the_two_backends_never_share_a_build_directory() {
        let roots = vec![Candidate {
            variable: "TMPDIR",
            path: PathBuf::from("/elsewhere/lint/target"),
        }];
        let root = Path::new("/repo");
        let host = scratch_path(&roots, root, Backend::Host).expect("host scratch");
        let sandbox = scratch_path(&roots, root, Backend::Sandbox).expect("sandbox scratch");
        assert_ne!(host, sandbox);
        // Neither may contain the other, or one backend's artefacts would be
        // inside the other's tree and `cargo clean`-shaped operations would
        // cross the boundary this separation exists to keep.
        assert!(
            !sandbox.starts_with(&host),
            "{sandbox:?} is inside {host:?}"
        );
        assert!(
            !host.starts_with(&sandbox),
            "{host:?} is inside {sandbox:?}"
        );
        // And they stay siblings, so a person reading `~/.roteiro/lint/target`
        // sees the pair rather than having to open one to find the other.
        assert_eq!(host.parent(), sandbox.parent());
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
                key.ends_with("_HOME") || key.ends_with("_TOOLCHAIN"),
                "{key} is not a locator"
            );
            assert!(!key.contains("TOKEN") && !key.contains("KEY"), "{key}");
        }
    }

    /// The regression, stated at the list that carried it.
    ///
    /// `CARGO_TARGET_DIR` was a passthrough entry here, and a passthrough entry
    /// can only forward a value the parent already had — so on the ordinary path
    /// where the parent had none, the build fell back to `<worktree>/target`.
    /// Putting it back would restore that silently, and this is what notices.
    #[test]
    fn the_target_directory_is_never_something_this_module_inherits() {
        assert!(
            !TOOLCHAIN_ENV.contains(&"CARGO_TARGET_DIR"),
            "`CARGO_TARGET_DIR` is inherited again — a name can only pass along \
             the caller's value, and where the build writes is this module's \
             guarantee to make, not the caller's to supply. Set it in `run`."
        );
    }

    /// ADR-0014 v1.6: a build scratch holds compiled build scripts, so it is
    /// per-repository and never shared. The key here is per *checkout*, which is
    /// finer — this pins that two trees never land in one directory.
    #[test]
    fn two_trees_never_share_a_scratch_directory() {
        let roots = scratch_roots_from(Some("/state".into()), None, "/tmp".into());
        // The shipped chooser, not a restatement of it: a test that rebuilt the
        // `<root>/<id>` join here would keep passing with the id dropped, which
        // is how this test first failed to catch anything.
        let for_tree = |path: &str| {
            scratch_path(&roots, Path::new(path), Backend::Host).expect("a scratch path")
        };
        assert_ne!(
            for_tree("/repos/alpha"),
            for_tree("/repos/beta"),
            "two repositories sharing a build scratch is the cache-shaped hole \
             in the execution boundary ADR-0014 draws"
        );
        // Two worktrees of one repository are two trees, and do not share
        // either: per-checkout is the safe direction to err in.
        assert_ne!(for_tree("/repos/alpha"), for_tree("/repos/alpha-wt"));
        // And the same tree resolves to the same place across runs, or the
        // cache would never be reused and every lint would be a cold build.
        assert_eq!(for_tree("/repos/alpha"), for_tree("/repos/alpha"));
    }

    /// The precedence is the asset cache's, and the **floor** deliberately is
    /// not: `.` would be resolved by cargo against the child's working
    /// directory, which is the worktree.
    #[test]
    fn the_scratch_root_never_falls_back_into_the_working_directory() {
        let paths = |roots: Vec<super::Candidate>| -> Vec<PathBuf> {
            roots.into_iter().map(|c| c.path).collect()
        };
        assert_eq!(
            paths(scratch_roots_from(
                Some("/state".into()),
                Some("/home/u".into()),
                "/tmp".into()
            )),
            vec![
                PathBuf::from("/state/lint/target"),
                PathBuf::from("/home/u/.roteiro/lint/target"),
                PathBuf::from("/tmp/lint/target"),
            ],
            "ROTEIRO_HOME first, as it is for the asset cache, then the home dir"
        );
        assert_eq!(
            paths(scratch_roots_from(
                None,
                Some("/home/u".into()),
                "/tmp".into()
            )),
            vec![
                PathBuf::from("/home/u/.roteiro/lint/target"),
                PathBuf::from("/tmp/lint/target"),
            ]
        );

        // Every candidate must be absolute. A relative one would be resolved by
        // cargo against the child's working directory — the worktree — which is
        // the original defect rebuilt inside its own fix.
        for candidate in scratch_roots_from(None, None, std::env::temp_dir()) {
            assert!(
                candidate.path.is_absolute(),
                "{} is relative, so cargo would resolve it against the worktree",
                candidate.path.display()
            );
        }
    }

    /// `ROTEIRO_HOME` is the caller's variable, and this module's guarantee is
    /// not the caller's to weaken. Pointed inside the tree under review it would
    /// reproduce the original defect through the front door of its own fix, so
    /// the candidates are checked rather than trusted.
    #[test]
    fn a_candidate_inside_the_tree_under_review_is_rejected() {
        let root = Path::new("/repos/alpha");
        assert!(is_inside(
            Path::new("/repos/alpha/.state/lint/target"),
            root
        ));
        assert!(is_inside(Path::new("/repos/alpha"), root));
        assert!(!is_inside(Path::new("/repos/alpha-sibling/x"), root));
        assert!(!is_inside(Path::new("/tmp/lint/target"), root));

        // The rejection is what makes the fallback list a list: with
        // `ROTEIRO_HOME` inside the tree, the next candidate is the one used.
        let candidates =
            scratch_roots_from(Some("/repos/alpha/.state".into()), None, "/tmp".into());
        let chosen = scratch_path(&candidates, root, Backend::Host).expect("a scratch path");
        assert!(
            chosen.starts_with("/tmp/lint/target"),
            "{} — the candidate inside the tree should have been skipped",
            chosen.display()
        );

        // And when every candidate is inside the tree, that is an error rather
        // than a build in the tree.
        let all_inside = scratch_roots_from(
            Some("/repos/alpha/.state".into()),
            Some("/repos/alpha".into()),
            "/repos/alpha/tmp".into(),
        );
        let err = scratch_path(&all_inside, root, Backend::Host).expect_err("must refuse");
        assert!(
            matches!(err, LintError::ScratchWouldBeInsideTheTree { .. }),
            "{err:?}"
        );
        assert!(err.to_string().contains("ROTEIRO_HOME"), "{err}");
    }

    /// The defect this PR exists to fix, arriving through the fix's own
    /// arithmetic — reported on #443.
    ///
    /// `scratch_path` returned its candidate unchanged, so a relative
    /// `ROTEIRO_HOME` produced a relative `CARGO_TARGET_DIR`. Cargo resolves
    /// that against the **worktree**, so the build lands in the tree under
    /// review: the exact hazard named as the reason for overriding a caller's
    /// value rather than honouring it, then not guarded against on the path this
    /// module computes itself.
    ///
    /// Two things are pinned here, and the second is the one that was subtle.
    #[test]
    fn a_relative_scratch_root_is_refused_rather_than_resolved() {
        let root = Path::new("/repos/alpha");
        for (source, roots) in [
            (
                "ROTEIRO_HOME",
                scratch_roots_from(Some("relstate".into()), None, "/tmp".into()),
            ),
            (
                "HOME",
                scratch_roots_from(None, Some("relhome".into()), "/tmp".into()),
            ),
            ("TMPDIR", scratch_roots_from(None, None, "reltmp".into())),
        ] {
            let err =
                scratch_path(&roots, root, Backend::Host).expect_err("a relative root must refuse");
            assert!(
                matches!(err, LintError::ScratchRootNotAbsolute { .. }),
                "{source}: {err:?}"
            );
            // The message has to name the variable, or the reader is told a
            // path is wrong and not which knob produced it.
            assert!(err.to_string().contains(source), "{err}");
        }

        // And the ordering: the refusal comes **before** the containment check,
        // which cannot answer for a relative path at all. `is_inside` resolves
        // against this process's working directory while cargo resolves against
        // the worktree, so on a relative candidate it compares a directory the
        // build would never use — and, run from outside the tree, says "not
        // inside" about a path that lands squarely within it.
        let outside_cwd_verdict = is_inside(Path::new("relstate/lint/target"), root);
        assert!(
            !outside_cwd_verdict,
            "this assertion documents the hazard: from a working directory \
             outside {root:?}, the containment check passes a relative candidate \
             that cargo would resolve into the tree. It is unreachable now only \
             because `scratch_path` refuses first."
        );
    }

    /// `scratch_path` never *returns* a relative path — it either produces an
    /// absolute one or refuses.
    ///
    /// The invariant is stated over the return value rather than over the
    /// inputs, because that is what the caller hands to a process with a
    /// different working directory. Relative inputs are included on purpose: for
    /// those the acceptable outcomes are an error or an absolute path, and
    /// silently returning what it was given is neither.
    #[test]
    fn an_accepted_scratch_path_is_always_absolute() {
        let root = Path::new("/repos/alpha");
        for roots in [
            scratch_roots_from(Some("/state".into()), None, "/tmp".into()),
            scratch_roots_from(None, Some("/home/u".into()), "/tmp".into()),
            scratch_roots_from(None, None, "/tmp".into()),
            // The first candidate is inside the tree, so the chosen one is a
            // later entry — the property has to hold for those too.
            scratch_roots_from(Some("/repos/alpha/.state".into()), None, "/tmp".into()),
            // And the relative ones, where returning the input is the defect.
            scratch_roots_from(Some("relstate".into()), None, "/tmp".into()),
            scratch_roots_from(None, Some("relhome".into()), "/tmp".into()),
            scratch_roots_from(None, None, "reltmp".into()),
        ] {
            if let Ok(path) = scratch_path(&roots, root, Backend::Host) {
                assert!(
                    path.is_absolute(),
                    "{} is relative — cargo would resolve it against the worktree",
                    path.display()
                );
            }
        }
    }

    /// The **order** of the two checks, which is the part that is easy to get
    /// wrong and silent when it is.
    ///
    /// Containment is decided by making the candidate absolute against *this*
    /// process's working directory. So when the tree under review happens to
    /// contain that working directory, a relative candidate looks contained,
    /// gets skipped as though it were the caller's own doing, and the next
    /// candidate is used — an explicitly set `ROTEIRO_HOME` silently ignored,
    /// with no error and no mention. Checking absoluteness first turns that into
    /// the refusal it should be.
    ///
    /// The fixture uses the current directory as the tree precisely so that the
    /// relative candidate falls inside it, which is the only arrangement in
    /// which the two orderings disagree.
    #[test]
    fn absoluteness_is_checked_before_containment_not_after() {
        let cwd = std::env::current_dir().expect("a working directory");
        let roots = scratch_roots_from(Some("relstate".into()), None, std::env::temp_dir());
        let err = scratch_path(&roots, &cwd, Backend::Host).expect_err(
            "a relative ROTEIRO_HOME must refuse, not be quietly skipped as \
             'inside the tree' and replaced by the next candidate",
        );
        assert!(
            matches!(err, LintError::ScratchRootNotAbsolute { .. }),
            "{err:?}"
        );
    }

    /// The same question asked of two spellings of one place.
    ///
    /// A comparison of the paths as written answers "outside" for a candidate
    /// that is plainly inside, whenever a symlink stands between them — which on
    /// macOS is every path under the system temporary directory, and therefore
    /// every fixture in this repository.
    #[test]
    fn a_candidate_reached_through_a_symlink_is_still_inside_the_tree() {
        let base = std::env::temp_dir().join(format!(
            "roteiro-inside-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&base).ok();
        let real = base.join("real");
        std::fs::create_dir_all(real.join("src")).expect("mkdir");
        let link = base.join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &link).expect("symlink");
        #[cfg(not(unix))]
        {
            std::fs::remove_dir_all(&base).ok();
            return;
        }

        // The tree named one way, the candidate named the other. Same directory.
        assert!(
            is_inside(&link.join(".state/lint/target"), &real),
            "a candidate under {} was not seen as inside {}",
            link.display(),
            real.display()
        );
        assert!(is_inside(&real.join(".state/lint/target"), &link));
        // And a genuinely separate directory still answers no.
        assert!(!is_inside(&base.join("elsewhere/lint/target"), &real));

        std::fs::remove_dir_all(&base).ok();
    }

    /// Both wordings cargo uses share one clause, and the named error depends on
    /// matching it — otherwise the caller is told the report was malformed.
    #[test]
    fn a_refused_lockfile_write_is_recognised_in_either_wording() {
        for stderr in [
            // All three observed from cargo 1.97: a missing lockfile, a stale
            // one, and the wording it uses when the manifest moved under it.
            "error: cannot create the lock file /r/Cargo.lock because --locked was passed to \
             prevent this",
            "error: cannot update the lock file /r/Cargo.lock because --locked was passed to \
             prevent this",
            "error: the lock file /r/Cargo.lock needs to be updated but --locked was passed to \
             prevent this",
        ] {
            assert!(lockfile_refused(stderr.as_bytes()), "{stderr}");
        }
        // An ordinary failing build is not this, and must keep its own message.
        assert!(!lockfile_refused(
            b"error: could not compile `x` due to 3 errors"
        ));
        assert!(!lockfile_refused(b""));
    }

    /// The two writes a lint would otherwise make into the tree under review.
    /// `--locked` is the half `CARGO_TARGET_DIR` does not cover.
    #[test]
    fn the_invocation_forbids_cargo_from_writing_the_lockfile() {
        for features in [FeatureSet::Defaults, FeatureSet::All] {
            let invocation = crate::adapter::clippy::Clippy::invocation(&features);
            assert!(
                invocation.args.iter().any(|a| a == "--locked"),
                "{:?} may rewrite Cargo.lock in the tree being linted",
                invocation.args
            );
        }
    }
}

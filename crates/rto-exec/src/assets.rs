//! Pinned analyzer assets: what a run needs before it can happen, and what
//! happens when it is not there.
//!
//! ADR-0014's working model is *mostly offline, degrade gracefully, pre-download
//! expected*. That is a provisioning contract, and this module is it:
//!
//! - **`roteiro security prefetch`** installs and verifies every pinned asset an
//!   analyzer needs, recording its digest and the time it was fetched.
//! - **`roteiro security status`** reports each digest and fetch time.
//! - **A run never provisions.** Cold cache fails with
//!   [`ExecError::AssetsUnavailableOffline`], which names the missing assets,
//!   their pinned digests, and the exact command to fix it. Never an implicit
//!   fetch; never a silent fall back to whatever the host happens to have
//!   installed.
//!
//! # The one rule that makes the rest work
//!
//! Provisioning writes; running reads. A run that quietly materialised its own
//! inputs would make "did this machine have the pinned rules?" unanswerable
//! after the fact — and the whole point of stamping `rules_digest` onto an
//! [`rto_graph::AnalysisRun`] is that the question has an answer.
//!
//! # Four kinds of asset
//!
//! [`AssetSource::Vendored`] is compiled into the binary — the baseline semgrep
//! rule set. Installing it needs no network at all, which is what makes a fresh
//! machine on a plane able to run `prefetch` and then scan.
//!
//! [`AssetSource::External`] is a directory Roteiro does **not** fetch: the
//! `RustSec` advisory database, which is a git checkout rather than a file with
//! a stable URL. `prefetch` verifies it is there, digests it, and records it, so
//! a run consults a database whose identity was pinned before it started —
//! rather than whatever `~/.cargo/advisory-db` happened to contain. If it is
//! absent, `prefetch` says exactly how to obtain it and refuses.
//!
//! [`AssetSource::Download`] is fetched by URL, and arrived with `osv-scanner`
//! in Stage 22b. Earlier revisions of this module said there was deliberately no
//! such source because "an unused fetch path is a security surface with no
//! user"; OSV's per-ecosystem databases are that user. They are single files at
//! stable URLs — exactly what a digest pin wants, and what the `RustSec` git
//! checkout could never be. The enum being `#[non_exhaustive]` is what made
//! adding it a non-breaking change.
//!
//! [`AssetSource::PinnedArchive`] is the one with a **compile-time digest**, and
//! it exists for the sandbox runtime (Stage 24). The difference from `Download`
//! is not the transport but the target: OSV rebuilds its databases daily, so the
//! only pin that can be honoured there is the snapshot this machine provisioned.
//! A published release artifact is immutable, so its correct bytes are knowable
//! in advance — and where they are knowable, they are checked.
//!
//! That closes the gap the [`Fetcher`] contract has to leave open elsewhere. A
//! fetcher that reports success over a truncated body can defeat a `Download`
//! asset's pin, because there is nothing to contradict it; it cannot defeat a
//! `PinnedArchive`, because the expected digest is compiled in and the archive
//! is verified here, in this crate, before it is installed.
//!
//! **Fetching is still confined to provisioning.** The transport is not in this
//! crate at all: [`provision_with`] takes the fetcher as an argument, and the
//! plain [`provision`] passes one that refuses. A run resolves assets through
//! [`resolve`], which has no fetcher to call even if it wanted one — so "a run
//! never provisions" is a property of the signatures rather than a rule someone
//! has to remember.
//!
//! @rto:0014

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::adapter::adapter_for;
use crate::clock::{age_in_days, rfc3339_utc};
use crate::runner::ExecError;
use crate::sha256_hex;

/// The baseline semgrep rule set, compiled in.
///
/// Vendoring the bytes rather than reading a file at runtime means the asset is
/// available on a machine that has only the binary — which is the case
/// `prefetch` exists to serve.
pub const BASELINE_RULES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/rules/roteiro-baseline.yml"
));

/// Where an asset comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AssetSource {
    /// Bytes compiled into this binary, installed as a single file.
    Vendored(&'static [u8]),
    /// A directory the operator provisions, which Roteiro verifies and pins but
    /// never fetches. `hint` is the exact command that obtains it.
    External {
        /// What to run to obtain it, quoted verbatim in every error.
        hint: &'static str,
    },
    /// A set of files downloaded by URL into one directory, digest-pinned at
    /// provisioning time.
    ///
    /// Downloading happens only in [`provision_with`], and only with a fetcher
    /// the caller supplied. There is no compile-time digest because the upstream
    /// files are republished continuously — OSV rebuilds its per-ecosystem
    /// databases daily — so what is pinned is the snapshot this machine
    /// provisioned, recorded in [`InstalledAsset::digest`] and re-checked on
    /// every run. That is the same pin the `RustSec` checkout gets, and it is
    /// the one that can actually be honoured.
    Download {
        /// Each file's path relative to the asset directory, and where it comes
        /// from. Order is preserved so `prefetch` reports progress in a stable
        /// sequence.
        files: &'static [DownloadFile],
    },
    /// A single published release artifact with a **compile-time SHA-256**,
    /// installed as one file and selected by host platform.
    ///
    /// Verified in this crate, before installation and again by `build.rs`
    /// before anything is built against it — so neither a lying fetcher nor a
    /// redirected URL can substitute different bytes. See
    /// [`crate::runtime_pins`] for what is pinned and why it has to be.
    PinnedArchive {
        /// One entry per supported host platform. A host not listed here cannot
        /// be provisioned, and is told which platforms are.
        archives: &'static [crate::runtime_pins::PinnedArchive],
    },
}

/// One file of an [`AssetSource::Download`] asset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadFile {
    /// Where it is installed, relative to the asset directory. Forward slashes;
    /// never absolute and never containing `..`, which [`provision_with`]
    /// enforces rather than trusts.
    pub path: &'static str,
    /// The URL it is fetched from.
    pub url: &'static str,
}

/// What kind of input an asset is — the axis along which it goes stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    /// A rule set. Changes only when someone changes it.
    Rules,
    /// An advisory database. Changes continuously and independently of the
    /// source tree, which is why results derived from it are labelled *possibly
    /// stale* rather than *current*.
    AdvisoryDb,
    /// The prebuilt sandbox runtime an analyzer is executed inside.
    ///
    /// Unlike the other two it is **immutable for a given release**: one
    /// published artifact with one correct digest, which is why it is the only
    /// kind carrying a compile-time pin.
    SandboxRuntime,
}

impl AssetKind {
    /// Stable token for display and `--json`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rules => "rules",
            Self::AdvisoryDb => "advisory-db",
            Self::SandboxRuntime => "sandbox-runtime",
        }
    }
}

/// One pinned asset.
#[derive(Debug, Clone, Copy)]
pub struct AssetSpec {
    /// Stable id, used as the cache directory name and in every error.
    pub id: &'static str,
    /// The analyzer that needs it.
    pub analyzer: &'static str,
    /// What it is.
    pub kind: AssetKind,
    /// Where it comes from.
    pub source: AssetSource,
    /// The file name it is installed under, for a [`AssetSource::Vendored`]
    /// asset. Empty for a directory asset.
    pub file: &'static str,
    /// Licence of the asset's contents, disclosed by `prefetch` before it
    /// installs anything — the same disclosure `roteiro model pull` makes.
    pub licence: &'static str,
}

/// Every asset this build knows how to provision.
pub static ASSETS: &[AssetSpec] = &[
    AssetSpec {
        id: crate::adapter::semgrep::RULES_ASSET,
        analyzer: crate::adapter::semgrep::ANALYZER,
        kind: AssetKind::Rules,
        source: AssetSource::Vendored(BASELINE_RULES),
        file: "roteiro-baseline.yml",
        // Written for this repository; see the rule file's own header for why no
        // Semgrep Registry rule is vendored.
        licence: "MIT OR Apache-2.0 (written for this repository)",
    },
    AssetSpec {
        id: crate::adapter::cargo_audit::ADVISORY_DB_ASSET,
        analyzer: crate::adapter::cargo_audit::ANALYZER,
        kind: AssetKind::AdvisoryDb,
        source: AssetSource::External {
            hint: "git clone --depth 1 https://github.com/RustSec/advisory-db \
                   ~/.roteiro/security/rustsec-advisory-db/db",
        },
        file: "",
        licence: "CC0-1.0 (RustSec advisory database)",
    },
    AssetSpec {
        id: crate::adapter::osv_scanner::DB_ASSET,
        analyzer: crate::adapter::osv_scanner::ANALYZER,
        kind: AssetKind::AdvisoryDb,
        source: AssetSource::Download {
            files: OSV_DATABASES,
        },
        file: "",
        // OSV.dev aggregates upstream databases and does not relicense them; each
        // record carries its own terms. The two that dominate this set are named
        // rather than flattened into one claim, because `cargo deny` governs
        // crates and would never have looked at an advisory file.
        licence: "per-record, as published by OSV.dev \
                  (CC0-1.0 for RustSec, CC-BY-4.0 for the GitHub Advisory Database)",
    },
    AssetSpec {
        id: crate::runtime_pins::RUNTIME_ASSET,
        // Not an analyzer's asset: every analyzer run under the sandboxed
        // backend needs the same one. No adapter declares it, so `assets_for`
        // never returns it and `prefetch --analyzer <name>` never selects it;
        // it is provisioned by a plain `prefetch`, and resolved directly by id.
        analyzer: SANDBOX,
        kind: AssetKind::SandboxRuntime,
        source: AssetSource::PinnedArchive {
            archives: crate::runtime_pins::RUNTIME_ARCHIVES,
        },
        file: crate::runtime_pins::RUNTIME_FILE,
        // The archive is a bundle of separately-licensed executables, and
        // flattening them into one claim is exactly what let 25 MB of GPL
        // binaries through a licence gate unnoticed. Each is named, and the
        // full record — including the source-offer duty this creates — is in
        // `crates/rto-exec/NOTICE-boxlite-runtime.md`, disclosed before install.
        licence: "mixed: Apache-2.0 (boxlite-shim, boxlite-guest), \
                  GPL-2.0 (mke2fs, debugfs, libkrunfw), \
                  LGPL-2.0-or-later (bwrap) — see NOTICE-boxlite-runtime.md",
    },
];

/// The `analyzer` field for an asset that belongs to no single analyzer.
///
/// A sentinel rather than an empty string, so `status` prints something a reader
/// can act on and `--analyzer <name>` cannot accidentally match it.
pub const SANDBOX: &str = "sandbox";

/// The OSV per-ecosystem databases this build provisions.
///
/// The layout is not ours to choose: `osv-scanner --local-db-path <dir>` looks
/// for `<dir>/osv-scalibr/<ECOSYSTEM>/all.zip`, with the ecosystem spelled
/// exactly as OSV spells it (`crates.io`, not `cargo`; `PyPI`, not `pypi`).
///
/// Four ecosystems, because that is what ADR-0018's matrix asks of this
/// analyzer: Python, Java and Node are the gap it closes, and `crates.io` is
/// what makes the Rust cross-reference with `cargo-audit` possible at all.
/// **`npm/all.zip` alone is roughly 210 MB**, and the four together are around
/// 260 MB — a real provisioning cost, disclosed by `prefetch` before it fetches
/// anything.
pub static OSV_DATABASES: &[DownloadFile] = &[
    DownloadFile {
        path: "osv-scalibr/crates.io/all.zip",
        url: "https://osv-vulnerabilities.storage.googleapis.com/crates.io/all.zip",
    },
    DownloadFile {
        path: "osv-scalibr/PyPI/all.zip",
        url: "https://osv-vulnerabilities.storage.googleapis.com/PyPI/all.zip",
    },
    DownloadFile {
        path: "osv-scalibr/Maven/all.zip",
        url: "https://osv-vulnerabilities.storage.googleapis.com/Maven/all.zip",
    },
    DownloadFile {
        path: "osv-scalibr/npm/all.zip",
        url: "https://osv-vulnerabilities.storage.googleapis.com/npm/all.zip",
    },
];

/// The spec for `id`, or `None`.
#[must_use]
pub fn asset(id: &str) -> Option<&'static AssetSpec> {
    ASSETS.iter().find(|a| a.id == id)
}

/// Every asset `analyzer` needs, in the order its adapter declares them.
#[must_use]
pub fn assets_for(analyzer: &str) -> Vec<&'static AssetSpec> {
    adapter_for(analyzer)
        .map(|adapter| {
            adapter
                .asset_ids()
                .iter()
                .filter_map(|id| asset(id))
                .collect()
        })
        .unwrap_or_default()
}

/// What was recorded about an asset when it was provisioned.
///
/// Persisted beside the asset as `installed.json`, so `status` reports what was
/// actually verified rather than re-deriving it and hoping the answer matches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledAsset {
    /// The asset id.
    pub id: String,
    /// What it is.
    pub kind: AssetKind,
    /// SHA-256 of the asset as installed. For a directory it is a digest over
    /// the sorted `(relative path, content digest)` list, so it changes when any
    /// file in the tree changes and does not depend on directory iteration
    /// order.
    pub digest: String,
    /// When `prefetch` verified and recorded it, RFC 3339 UTC.
    pub fetched_at: String,
    /// How many files the digest covers, for a directory asset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<usize>,
    /// When the asset's contents were published, RFC 3339 UTC — for an advisory
    /// database that is a git checkout, its `HEAD` commit time.
    ///
    /// This is **not** `fetched_at`. Fetching an eight-month-old database today
    /// does not make it current, and the difference between the two is exactly
    /// what a *possibly stale* label is about.
    ///
    /// It is recorded here because the analyzer will not report it: `cargo audit`
    /// returns `last-commit: null` and `last-updated: null` whenever it is
    /// pointed at a database with `--db` instead of resolving one itself —
    /// verified against cargo-audit 0.22.2, at both a shallow clone and its own
    /// managed checkout. Pinning the database is what makes a run reproducible,
    /// so the pinned configuration must not be the one that loses the staleness
    /// evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
}

/// An asset's state, as `roteiro security status` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AssetStatus {
    /// The asset id.
    pub id: &'static str,
    /// The analyzer that needs it.
    pub analyzer: &'static str,
    /// What it is.
    pub kind: AssetKind,
    /// Where it is (or would be) on disk.
    pub path: String,
    /// What was recorded at provisioning time, if it has been provisioned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed: Option<InstalledAsset>,
    /// Whole days since it was provisioned, when that can be computed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age_days: Option<i64>,
    /// Whether the bytes on disk still match the recorded digest. `None` when
    /// nothing is installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified: Option<bool>,
}

/// Resolve the root of the asset cache from its inputs, without touching the
/// environment — so it is testable.
fn root_from(
    security_root: Option<PathBuf>,
    roteiro_home: Option<PathBuf>,
    home: Option<PathBuf>,
) -> PathBuf {
    if let Some(dir) = security_root {
        return dir;
    }
    if let Some(dir) = roteiro_home {
        return dir.join("security");
    }
    home.unwrap_or_else(|| PathBuf::from("."))
        .join(".roteiro")
        .join("security")
}

/// Root of the asset cache (`~/.roteiro/security`), honouring
/// `ROTEIRO_SECURITY_ASSETS` and then `ROTEIRO_HOME`.
///
/// It sits beside the model store rather than inside the repository: assets are
/// per-user, are shared across every checkout, and must never be committed.
#[must_use]
pub fn asset_root() -> PathBuf {
    root_from(
        std::env::var_os("ROTEIRO_SECURITY_ASSETS").map(PathBuf::from),
        std::env::var_os("ROTEIRO_HOME").map(PathBuf::from),
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from),
    )
}

/// Directory a given asset lives in.
#[must_use]
pub fn asset_dir(root: &Path, spec: &AssetSpec) -> PathBuf {
    root.join(spec.id)
}

/// The path an analyzer is pointed at for this asset: the installed file for a
/// vendored asset, the directory itself for an external one.
#[must_use]
pub fn asset_path(root: &Path, spec: &AssetSpec) -> PathBuf {
    let dir = asset_dir(root, spec);
    match spec.source {
        AssetSource::Vendored(_) | AssetSource::PinnedArchive { .. } => dir.join(spec.file),
        AssetSource::External { .. } | AssetSource::Download { .. } => dir.join("db"),
    }
}

/// Where the provisioning record is kept.
fn record_path(root: &Path, spec: &AssetSpec) -> PathBuf {
    asset_dir(root, spec).join("installed.json")
}

/// Errors raised while provisioning.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AssetError {
    /// An [`AssetSource::External`] asset is not present, and Roteiro will not
    /// fetch it. The message names the command that obtains it.
    #[error(
        "asset {id:?} is not provisioned: expected a directory at {path}\n  \
         obtain it with: {hint}\n  \
         then run: roteiro security prefetch --analyzer {analyzer}"
    )]
    ExternalMissing {
        /// The asset id.
        id: &'static str,
        /// Where it was expected.
        path: String,
        /// The command that obtains it.
        hint: &'static str,
        /// The analyzer that needs it.
        analyzer: &'static str,
    },
    /// This build has no such asset.
    #[error("unknown asset {0:?}")]
    Unknown(String),
    /// A downloadable asset was asked for without a fetcher, which is what every
    /// path except `roteiro security prefetch` does.
    ///
    /// This is the offline contract stated as an error rather than as a comment:
    /// a run that finds a cold cache is told what to run, and is never quietly
    /// given a network connection instead.
    #[error(
        "asset {id:?} is not provisioned and this code path does not download \
         ({files} file(s), starting with {first})\n  \
         fetch it with: roteiro security prefetch --analyzer {analyzer}"
    )]
    FetchNotPermitted {
        /// The asset id.
        id: &'static str,
        /// How many files it is made of.
        files: usize,
        /// The first URL, so the message names something concrete.
        first: &'static str,
        /// The analyzer that needs it.
        analyzer: &'static str,
    },
    /// A download failed. The message is the fetcher's, because it knows what
    /// went wrong and this module deliberately knows no transport.
    #[error("downloading {url} for asset {id:?}: {message}")]
    Fetch {
        /// The asset id.
        id: &'static str,
        /// The URL that failed.
        url: &'static str,
        /// What the fetcher reported.
        message: String,
    },
    /// A [`DownloadFile::path`] is not a plain relative path.
    ///
    /// Checked rather than trusted: these paths are compiled in today, but they
    /// name where bytes from the network are written, and a `..` in one would
    /// write outside the asset cache.
    #[error("asset {id:?} declares an unsafe install path {path:?}")]
    UnsafeInstallPath {
        /// The asset id.
        id: &'static str,
        /// The offending path.
        path: &'static str,
    },
    /// No sandbox runtime is pinned for this host platform.
    ///
    /// Refused by name rather than left to fail as a link error later: a
    /// platform Roteiro has not pinned is a platform whose runtime bytes nobody
    /// has verified, and building against unverified bytes is the thing this
    /// whole path exists to prevent.
    #[error(
        "asset {id:?} has no pinned archive for this host ({os}/{arch}); \
         pinned platforms are: {supported}"
    )]
    UnsupportedPlatform {
        /// The asset id.
        id: &'static str,
        /// `std::env::consts::OS` for the host.
        os: &'static str,
        /// `std::env::consts::ARCH` for the host.
        arch: &'static str,
        /// The platforms that do have a pin, comma-separated.
        supported: String,
    },
    /// A pinned archive is not provisioned, and this code path does not
    /// download.
    #[error(
        "asset {id:?} ({target}) is not provisioned and this code path does not download\n  \
         expected at: {path}\n  \
         fetch it with: roteiro security prefetch --allow-download"
    )]
    ArchiveMissing {
        /// The asset id.
        id: &'static str,
        /// The host platform it would be fetched for.
        target: &'static str,
        /// Where it was expected.
        path: String,
    },
    /// A pinned archive's bytes are not the bytes that were pinned.
    ///
    /// This is the check that makes the sandbox runtime reproducible, so it is a
    /// hard failure with no override: a mismatch is either a truncated download,
    /// a redirected URL, or a substituted artifact, and none of those is
    /// something to carry on from. The size is reported alongside because a
    /// short body is the common case and two unequal digests do not say so.
    #[error(
        "asset {id:?} does not match its pinned digest — refusing it\n  \
         from:     {url}\n  \
         expected: {expected} ({expected_bytes} bytes)\n  \
         actual:   {actual} ({actual_bytes} bytes)"
    )]
    DigestMismatch {
        /// The asset id.
        id: &'static str,
        /// Where the bytes came from.
        url: String,
        /// The digest that was pinned.
        expected: &'static str,
        /// The size that was pinned.
        expected_bytes: u64,
        /// The digest of what arrived.
        actual: String,
        /// The size of what arrived.
        actual_bytes: u64,
    },
    /// Reading or writing the cache failed.
    #[error("asset cache I/O at {path}: {source}")]
    Io {
        /// What was being touched.
        path: String,
        /// The underlying failure.
        source: std::io::Error,
    },
    /// The provisioning record could not be read or written.
    #[error("asset record: {0}")]
    Record(#[from] serde_json::Error),
}

/// How bytes at a URL are written to a local path.
///
/// The transport is the caller's: this crate has no HTTP dependency and is not
/// going to acquire one for a single asset kind. `roteiro security prefetch`
/// supplies an implementation over the `ureq` client already in the tree; tests
/// supply one that writes fixture bytes and never opens a socket, which is how
/// the download path is exercised without a network.
///
/// # The contract, and why it cannot be checked here
///
/// **An implementation must write the whole file or fail.** A truncated download
/// that returned `Ok` would be renamed into place, digested, and recorded as the
/// asset's pin — and [`AssetSource::Download`] has no compile-time digest to
/// contradict it, so `status` would then report the short file as present and
/// matching. Staging through a `.partial` file guards against a crash, not
/// against a fetcher that misreports success.
///
/// Nothing in this crate can verify that: completeness is a property of the
/// transport's framing, and this crate deliberately has no transport. The
/// shipped implementation is `download_asset_file` in the CLI, which establishes
/// it from the response's declared length and refuses a body whose length cannot
/// be established at all.
pub type Fetcher<'a> = dyn Fn(&str, &Path) -> Result<(), String> + 'a;

/// Install and verify one asset, without any ability to download.
///
/// This is what every path except `roteiro security prefetch` calls. A
/// [`AssetSource::Download`] asset that is not already present therefore fails
/// with [`AssetError::FetchNotPermitted`] naming the prefetch command, which is
/// the offline contract expressed as a signature.
///
/// It is idempotent: re-running it re-digests and re-stamps, which is what makes
/// `prefetch` a safe thing to run whenever you are unsure.
///
/// # Errors
/// Returns [`AssetError::ExternalMissing`] when an operator-provisioned asset is
/// absent, [`AssetError::FetchNotPermitted`] when a downloadable one is, or
/// [`AssetError::Io`] if the cache cannot be written.
pub fn provision(root: &Path, spec: &AssetSpec) -> Result<InstalledAsset, AssetError> {
    provision_with(root, spec, None)
}

/// Install and verify one asset, downloading through `fetch` where the asset
/// needs it.
///
/// This is the **only** function that writes to the asset cache, and the only
/// one that can cause a network request. `fetch` is `None` for every caller that
/// must not fetch; see [`provision`].
///
/// # Errors
/// As [`provision`], plus [`AssetError::Fetch`] if a download fails and
/// [`AssetError::UnsafeInstallPath`] if a declared install path could escape the
/// asset directory.
pub fn provision_with(
    root: &Path,
    spec: &AssetSpec,
    fetch: Option<&Fetcher<'_>>,
) -> Result<InstalledAsset, AssetError> {
    let dir = asset_dir(root, spec);
    std::fs::create_dir_all(&dir).map_err(|source| AssetError::Io {
        path: dir.display().to_string(),
        source,
    })?;
    let target = asset_path(root, spec);

    let (digest, files) = match spec.source {
        AssetSource::Vendored(bytes) => {
            write_atomically(&target, bytes)?;
            (sha256_hex(bytes), None)
        }
        AssetSource::External { hint } => {
            if !target.is_dir() {
                return Err(AssetError::ExternalMissing {
                    id: spec.id,
                    path: target.display().to_string(),
                    hint,
                    analyzer: spec.analyzer,
                });
            }
            let (digest, count) = digest_tree(&target)?;
            (digest, Some(count))
        }
        AssetSource::Download { files } => {
            download_all(spec, files, &target, fetch)?;
            let (digest, count) = digest_tree(&target)?;
            (digest, Some(count))
        }
        AssetSource::PinnedArchive { archives } => {
            let digest = provision_archive(spec, archives, &target, fetch)?;
            (digest, None)
        }
    };
    let published_at = published_at(&target);

    let record = InstalledAsset {
        id: spec.id.to_owned(),
        kind: spec.kind,
        digest,
        fetched_at: rfc3339_utc(std::time::SystemTime::now()),
        files,
        published_at,
    };
    let json = serde_json::to_vec_pretty(&record)?;
    write_atomically(&record_path(root, spec), &json)?;
    Ok(record)
}

/// Fetch every file of a [`AssetSource::Download`] asset into `target`.
///
/// With no fetcher this refuses unless the files are *already* all there, which
/// is what makes `provision` idempotent for a downloadable asset without giving
/// it a network: a second `prefetch --offline`-style call over a warm cache
/// re-digests and re-stamps rather than failing.
fn download_all(
    spec: &AssetSpec,
    files: &'static [DownloadFile],
    target: &Path,
    fetch: Option<&Fetcher<'_>>,
) -> Result<(), AssetError> {
    for file in files {
        if !is_safe_relative(file.path) {
            return Err(AssetError::UnsafeInstallPath {
                id: spec.id,
                path: file.path,
            });
        }
    }

    let missing: Vec<&DownloadFile> = files
        .iter()
        .filter(|file| !target.join(file.path).is_file())
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    let Some(fetch) = fetch else {
        return Err(AssetError::FetchNotPermitted {
            id: spec.id,
            files: missing.len(),
            first: missing[0].url,
            analyzer: spec.analyzer,
        });
    };

    for file in missing {
        let destination = target.join(file.path);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|source| AssetError::Io {
                path: parent.display().to_string(),
                source,
            })?;
        }
        // Fetch beside the destination and rename, so an interrupted download
        // never leaves a half-file at the path the analyzer reads.
        //
        // Staging protects the pin from a *crash*; it cannot protect it from a
        // fetcher that returns `Ok` over a short body, because then the rename
        // happens and the truncated file is what gets digested. That half of the
        // contract is the fetcher's, and is stated on [`Fetcher`].
        let partial = destination.with_extension("partial");
        std::fs::remove_file(&partial).ok();
        fetch(file.url, &partial).map_err(|message| {
            // Leave nothing behind. The stray file is not at a path any analyzer
            // reads, but `digest_tree` covers the whole asset directory — so a
            // later successful provision (of the remaining files, or after the
            // operator placed this one by hand) would fold these bytes into the
            // recorded pin, and removing them afterwards would then read as
            // tampering.
            std::fs::remove_file(&partial).ok();
            AssetError::Fetch {
                id: spec.id,
                url: file.url,
                message,
            }
        })?;
        std::fs::rename(&partial, &destination).map_err(|source| {
            std::fs::remove_file(&partial).ok();
            AssetError::Io {
                path: destination.display().to_string(),
                source,
            }
        })?;
    }
    Ok(())
}

/// The pinned archive for the host this is running on.
///
/// # Errors
/// Returns [`AssetError::UnsupportedPlatform`] naming the platforms that are
/// pinned, for a host that is not one of them.
pub fn archive_for_host(
    spec: &AssetSpec,
    archives: &'static [crate::runtime_pins::PinnedArchive],
) -> Result<&'static crate::runtime_pins::PinnedArchive, AssetError> {
    // Searched in the slice the *spec* carries, not in the global table. They
    // are the same slice in production, and keeping the lookup parameterised is
    // what lets the pin be exercised without shipping a fake into the real one.
    crate::runtime_pins::runtime_target(std::env::consts::OS, std::env::consts::ARCH)
        .and_then(|target| archives.iter().find(|a| a.target == target))
        .ok_or_else(|| AssetError::UnsupportedPlatform {
            id: spec.id,
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            supported: archives
                .iter()
                .map(|a| a.target)
                .collect::<Vec<_>>()
                .join(", "),
        })
}

/// Install the host's pinned archive, verifying its digest before it counts.
///
/// Idempotent and offline over a warm cache: an archive already present *and
/// matching its pin* is accepted without a fetcher, which is what lets a machine
/// with no network re-run `prefetch` and get a clean bill rather than a refusal.
/// An archive present but **not** matching is refused rather than re-fetched —
/// silently replacing bytes that failed verification would turn a tamper signal
/// into a retry.
fn provision_archive(
    spec: &AssetSpec,
    archives: &'static [crate::runtime_pins::PinnedArchive],
    target: &Path,
    fetch: Option<&Fetcher<'_>>,
) -> Result<String, AssetError> {
    let archive = archive_for_host(spec, archives)?;

    if target.is_file() {
        // Present already: verify, and take it or refuse it. Either way no
        // network is touched, which is the whole point of a warm cache.
        return verify_archive(spec, archive, target, &target.display().to_string());
    }

    let Some(fetch) = fetch else {
        return Err(AssetError::ArchiveMissing {
            id: spec.id,
            target: archive.target,
            path: target.display().to_string(),
        });
    };

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|source| AssetError::Io {
            path: parent.display().to_string(),
            source,
        })?;
    }

    // Stage beside the destination, verify, and only then rename. A body that
    // fails its pin never appears at the path anything reads — so a failed
    // provision leaves a cold cache rather than a poisoned one.
    let partial = target.with_extension("partial");
    std::fs::remove_file(&partial).ok();
    fetch(archive.url, &partial).map_err(|message| {
        std::fs::remove_file(&partial).ok();
        AssetError::Fetch {
            id: spec.id,
            url: archive.url,
            message,
        }
    })?;

    let digest = match verify_archive(spec, archive, &partial, archive.url) {
        Ok(digest) => digest,
        Err(e) => {
            std::fs::remove_file(&partial).ok();
            return Err(e);
        }
    };

    std::fs::rename(&partial, target).map_err(|source| {
        std::fs::remove_file(&partial).ok();
        AssetError::Io {
            path: target.display().to_string(),
            source,
        }
    })?;
    Ok(digest)
}

/// Check a file against a pinned archive, returning its digest when it matches.
///
/// `origin` is what the failure message blames — a URL when the bytes just
/// arrived from one, a path when they were already on disk.
///
/// # Errors
/// Returns [`AssetError::DigestMismatch`] when the bytes are not the pinned
/// bytes, or [`AssetError::Io`] when the file cannot be read.
pub fn verify_archive(
    spec: &AssetSpec,
    archive: &crate::runtime_pins::PinnedArchive,
    path: &Path,
    origin: &str,
) -> Result<String, AssetError> {
    let bytes = std::fs::read(path).map_err(|source| AssetError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let digest = sha256_hex(&bytes);
    let actual_bytes = bytes.len() as u64;
    if digest != archive.sha256 || actual_bytes != archive.bytes {
        return Err(AssetError::DigestMismatch {
            id: spec.id,
            url: origin.to_owned(),
            expected: archive.sha256,
            expected_bytes: archive.bytes,
            actual: digest,
            actual_bytes,
        });
    }
    Ok(digest)
}

/// Whether a declared install path stays inside the asset directory.
///
/// Compiled-in paths today, but they name where bytes from the network land, and
/// the check costs nothing.
fn is_safe_relative(path: &str) -> bool {
    !path.is_empty()
        && !Path::new(path).components().any(|component| {
            matches!(
                component,
                std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
                    | std::path::Component::ParentDir
            )
        })
}

/// The provisioning record for an asset, or `None` if it was never provisioned
/// or the record is unreadable.
///
/// An unreadable record is treated as absent rather than as an error: the
/// remedy is the same — run `prefetch` — and a corrupt cache file should not
/// make `status` fail.
#[must_use]
pub fn installed(root: &Path, spec: &AssetSpec) -> Option<InstalledAsset> {
    let bytes = std::fs::read(record_path(root, spec)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// The state of every asset this build knows about, for `roteiro security
/// status`.
#[must_use]
pub fn status(root: &Path, analyzer: Option<&str>) -> Vec<AssetStatus> {
    ASSETS
        .iter()
        .filter(|spec| analyzer.is_none_or(|name| spec.analyzer == name))
        .map(|spec| {
            let installed = installed(root, spec);
            let now = rfc3339_utc(std::time::SystemTime::now());
            let age_days = installed
                .as_ref()
                .and_then(|record| age_in_days(&record.fetched_at, &now));
            // Re-digest what is on disk. A record that no longer matches the
            // bytes is exactly the case a status command exists to surface, and
            // reporting the record alone would hide it.
            let verified = installed.as_ref().map(|record| {
                current_digest(root, spec).as_deref() == Some(record.digest.as_str())
            });
            AssetStatus {
                id: spec.id,
                analyzer: spec.analyzer,
                kind: spec.kind,
                path: asset_path(root, spec).display().to_string(),
                installed,
                age_days,
                verified,
            }
        })
        .collect()
}

/// When the contents at `dir` were published, if that can be established.
///
/// A git checkout's `HEAD` commit time is the publication date. Anything that is
/// not a git checkout has no such date, and `None` is reported rather than
/// invented — a made-up publication date would make a stale database look fresh,
/// which is the one failure mode this whole field exists to prevent.
fn published_at(dir: &Path) -> Option<String> {
    let repo = rto_graph::Repo::discover(dir).ok()?;
    // `discover` walks upwards, so a directory that is merely *inside* a
    // repository would otherwise be dated by that repository's HEAD.
    if repo.workdir()? != dir {
        return None;
    }
    let seconds = repo.head_commit_time().ok()?;
    Some(rfc3339_utc(
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(u64::try_from(seconds).ok()?),
    ))
}

/// The advisory-database evidence recorded for `analyzer` at provisioning time.
///
/// Supplied to a run so its results carry a database identity and publication
/// date even though the analyzer itself reports neither.
#[must_use]
pub fn advisory_db_evidence(root: &Path, analyzer: &str) -> Option<rto_graph::AdvisoryDb> {
    let spec = assets_for(analyzer)
        .into_iter()
        .find(|s| s.kind == AssetKind::AdvisoryDb)?;
    let record = installed(root, spec)?;
    Some(rto_graph::AdvisoryDb {
        digest: record.digest,
        published_at: record.published_at,
    })
}

/// Digest of what is on disk right now, or `None` if it is not there.
fn current_digest(root: &Path, spec: &AssetSpec) -> Option<String> {
    let target = asset_path(root, spec);
    match spec.source {
        AssetSource::Vendored(_) | AssetSource::PinnedArchive { .. } => {
            Some(sha256_hex(&std::fs::read(target).ok()?))
        }
        AssetSource::External { .. } | AssetSource::Download { .. } => {
            digest_tree(&target).ok().map(|(digest, _)| digest)
        }
    }
}

/// Resolve every asset `analyzer` needs to a verified local path.
///
/// # Errors
/// Returns [`ExecError::AssetsUnavailableOffline`] naming every asset that is
/// missing or whose bytes no longer match what was recorded, together with the
/// exact prefetch command. It never fetches, and it never falls back to a
/// host-installed copy.
pub fn resolve(root: &Path, analyzer: &str) -> Result<Vec<(&'static str, PathBuf)>, ExecError> {
    let specs = assets_for(analyzer);
    let mut resolved = Vec::with_capacity(specs.len());
    let mut missing = Vec::new();

    for spec in specs {
        let path = asset_path(root, spec);
        match (installed(root, spec), current_digest(root, spec)) {
            // Provisioned, and the bytes still match what was recorded.
            (Some(record), Some(digest)) if digest == record.digest => {
                resolved.push((spec.id, path));
            }
            // Provisioned, but the bytes changed underneath the record. That is
            // not a warning: a run would stamp a digest that does not describe
            // what it read.
            (Some(record), Some(_)) => missing.push(MissingAsset {
                id: spec.id.to_owned(),
                digest: record.digest.clone(),
                reason: "the bytes on disk no longer match the recorded digest",
            }),
            (Some(record), None) => missing.push(MissingAsset {
                id: spec.id.to_owned(),
                digest: record.digest.clone(),
                reason: "recorded as provisioned, but nothing is there now",
            }),
            (None, _) => missing.push(MissingAsset {
                id: spec.id.to_owned(),
                digest: "not yet pinned".to_owned(),
                reason: "never provisioned",
            }),
        }
    }

    if missing.is_empty() {
        Ok(resolved)
    } else {
        Err(ExecError::AssetsUnavailableOffline {
            analyzer: analyzer.to_owned(),
            missing,
            command: format!("roteiro security prefetch --analyzer {analyzer}"),
        })
    }
}

/// One asset a run needed and did not have.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MissingAsset {
    /// The asset id.
    pub id: String,
    /// The digest that was pinned for it, or a note that none is.
    pub digest: String,
    /// Why it could not be used.
    pub reason: &'static str,
}

impl std::fmt::Display for MissingAsset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({}; {})", self.id, self.digest, self.reason)
    }
}

/// Digest of a directory tree, plus the number of files it covered.
///
/// The digest is over the sorted list of `(relative path, sha256(bytes))`, so it
/// is a function of the tree's contents alone: independent of directory
/// iteration order, of timestamps, and of where the tree happens to be mounted.
/// `.git` is skipped — it is bookkeeping, not advisory data, and including it
/// would make the digest churn on every fetch that changed nothing.
fn digest_tree(dir: &Path) -> Result<(String, usize), AssetError> {
    let mut entries: BTreeMap<String, String> = BTreeMap::new();
    walk(dir, dir, &mut entries)?;
    let mut manifest = String::new();
    for (path, digest) in &entries {
        use std::fmt::Write as _;
        let _ = writeln!(manifest, "{digest}  {path}");
    }
    Ok((sha256_hex(manifest.as_bytes()), entries.len()))
}

fn walk(root: &Path, dir: &Path, into: &mut BTreeMap<String, String>) -> Result<(), AssetError> {
    let read = std::fs::read_dir(dir).map_err(|source| AssetError::Io {
        path: dir.display().to_string(),
        source,
    })?;
    for entry in read {
        let entry = entry.map_err(|source| AssetError::Io {
            path: dir.display().to_string(),
            source,
        })?;
        let path = entry.path();
        // `symlink_metadata` rather than `metadata`: a symlink out of the tree
        // must not be followed into a file the digest has no business reading.
        let meta = std::fs::symlink_metadata(&path).map_err(|source| AssetError::Io {
            path: path.display().to_string(),
            source,
        })?;
        if meta.is_symlink() {
            continue;
        }
        if meta.is_dir() {
            if path.file_name().is_some_and(|n| n == ".git") {
                continue;
            }
            walk(root, &path, into)?;
        } else if meta.is_file() {
            let bytes = std::fs::read(&path).map_err(|source| AssetError::Io {
                path: path.display().to_string(),
                source,
            })?;
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            into.insert(relative, sha256_hex(&bytes));
        }
    }
    Ok(())
}

/// Write `bytes` to `path` via a temp file and a rename, so a reader never sees
/// a half-written asset — the same discipline `rto_graph::download_verified`
/// applies to a model file.
fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), AssetError> {
    let io = |source| AssetError::Io {
        path: path.display().to_string(),
        source,
    };
    let tmp = path.with_extension("partial");
    std::fs::write(&tmp, bytes).map_err(io)?;
    if path.exists() {
        std::fs::remove_file(path).map_err(io)?;
    }
    std::fs::rename(&tmp, path).map_err(|source| {
        std::fs::remove_file(&tmp).ok();
        AssetError::Io {
            path: path.display().to_string(),
            source,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{
        ASSETS, AssetError, AssetKind, AssetSource, asset, asset_path, assets_for, installed,
        provision, resolve, root_from, status,
    };
    use crate::runner::ExecError;
    use std::path::PathBuf;

    /// A throwaway cache root that removes itself.
    struct Cache(PathBuf);

    impl Cache {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("rto-exec-assets-{name}"));
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

    fn rules() -> &'static super::AssetSpec {
        asset("semgrep-rules").expect("the baseline rule set is a known asset")
    }

    fn advisory_db() -> &'static super::AssetSpec {
        asset("rustsec-advisory-db").expect("the advisory database is a known asset")
    }

    /// Every asset is reachable from something that wants it — either an
    /// analyzer's adapter, or the shared sandbox, which no single analyzer owns.
    ///
    /// The `SANDBOX` arm is not a loophole: an asset that claims to belong to an
    /// analyzer and is not in that analyzer's `asset_ids` would be provisioned
    /// and never used, which is the case this test exists to catch.
    #[test]
    fn every_asset_belongs_to_an_analyzer_that_asked_for_it() {
        for spec in ASSETS {
            if spec.analyzer == super::SANDBOX {
                assert!(
                    assets_for(spec.analyzer).is_empty(),
                    "{} uses the shared-asset sentinel, so no adapter may claim it",
                    spec.id
                );
            } else {
                assert!(
                    assets_for(spec.analyzer).iter().any(|s| s.id == spec.id),
                    "{} is not claimed by {}",
                    spec.id,
                    spec.analyzer
                );
            }
            assert!(!spec.licence.is_empty(), "{} discloses no licence", spec.id);
        }
    }

    /// The sandbox runtime's disclosure must name every licence family in the
    /// archive, not flatten them into one word.
    ///
    /// Flattening is precisely how 25 MB of GPL binaries travelled through a
    /// licence gate that reported `licenses ok`. A reader of `prefetch`'s output
    /// is entitled to see what they are about to install.
    #[test]
    fn the_sandbox_runtime_discloses_every_licence_it_carries() {
        let spec = asset(crate::runtime_pins::RUNTIME_ASSET).expect("the runtime is a known asset");
        assert_eq!(spec.kind, AssetKind::SandboxRuntime);
        for family in ["Apache-2.0", "GPL-2.0", "LGPL-2.0"] {
            assert!(
                spec.licence.contains(family),
                "the disclosure does not mention {family}: {}",
                spec.licence
            );
        }
        assert!(
            spec.licence.contains("NOTICE-boxlite-runtime.md"),
            "the disclosure must point at the full record: {}",
            spec.licence
        );
    }

    /// Every pinned archive must carry a full digest and a real size, and the
    /// set must cover exactly the platforms `runtime_target` claims — a target
    /// that maps to no archive would fail at build time with nothing to say.
    #[test]
    fn every_pinned_archive_is_complete_and_reachable() {
        use crate::runtime_pins::{RUNTIME_ARCHIVES, archive_for, runtime_target};
        assert!(!RUNTIME_ARCHIVES.is_empty());
        for archive in RUNTIME_ARCHIVES {
            assert_eq!(
                archive.sha256.len(),
                64,
                "{} has no full sha256",
                archive.target
            );
            assert!(
                archive
                    .sha256
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
                "{} digest must be lowercase hex",
                archive.target
            );
            assert!(
                archive.bytes > 1_000_000,
                "{} size looks wrong",
                archive.target
            );
            assert!(
                archive.url.ends_with(".tar.gz") && archive.url.contains(archive.target),
                "{} url does not name the target it is for: {}",
                archive.target,
                archive.url
            );
        }
        for (os, arch) in [
            ("macos", "aarch64"),
            ("linux", "x86_64"),
            ("linux", "aarch64"),
        ] {
            let target = runtime_target(os, arch).expect("a pinned platform");
            let archive = archive_for(os, arch).expect("must resolve to an archive");
            assert_eq!(archive.target, target);
        }
        assert!(runtime_target("windows", "x86_64").is_none());
        assert!(archive_for("windows", "x86_64").is_none());
    }

    /// A digest that does not match is refused, and the refusal says which
    /// bytes were expected — including the size, because a truncated body is
    /// the common failure and two unequal digests do not say so.
    #[test]
    fn a_pinned_archive_that_does_not_match_is_refused() {
        use crate::runtime_pins::PinnedArchive;
        let cache = Cache::new("pinned-mismatch");
        let spec = asset(crate::runtime_pins::RUNTIME_ASSET).expect("known asset");
        let archive = PinnedArchive {
            target: "test-target",
            url: "https://example.invalid/runtime.tar.gz",
            sha256: "0000000000000000000000000000000000000000000000000000000000000000",
            bytes: 999,
        };
        let path = cache.0.join("impostor.tar.gz");
        std::fs::write(&path, b"not the pinned bytes").expect("write");

        let err = super::verify_archive(spec, &archive, &path, archive.url)
            .expect_err("bytes that do not match the pin must be refused");
        let message = err.to_string();
        assert!(matches!(err, AssetError::DigestMismatch { .. }));
        assert!(message.contains(archive.sha256), "{message}");
        assert!(message.contains("999 bytes"), "{message}");
        assert!(message.contains("20 bytes"), "{message}");
    }

    /// Provisioning a pinned archive without a fetcher is refused by name, and
    /// names the command that fixes it — the same offline contract every other
    /// asset kind follows.
    #[test]
    fn a_pinned_archive_is_not_fetched_by_a_path_that_may_not_download() {
        let cache = Cache::new("pinned-cold");
        let spec = asset(crate::runtime_pins::RUNTIME_ASSET).expect("known asset");
        let err = provision(&cache.0, spec).expect_err("a cold cache must refuse");
        // A host with no pinned archive fails earlier, and differently — both
        // are correct refusals, and asserting the property rather than one
        // literal keeps this test honest on an unpinned platform.
        let message = err.to_string();
        match err {
            AssetError::ArchiveMissing { .. } => {
                assert!(message.contains("prefetch --allow-download"), "{message}");
            }
            AssetError::UnsupportedPlatform { .. } => {
                assert!(message.contains("pinned platforms are"), "{message}");
            }
            other => panic!("unexpected refusal: {other}"),
        }
    }

    /// A spec pinned to `body`, for the host platform, without touching the
    /// shipped pins.
    ///
    /// Leaked because [`AssetSource::PinnedArchive`] holds `&'static` data — a
    /// few bytes per test process, and the alternative is either a fake entry in
    /// the real table or not exercising the pin at all.
    fn pinned_to(body: &[u8]) -> Option<&'static super::AssetSpec> {
        let target =
            crate::runtime_pins::runtime_target(std::env::consts::OS, std::env::consts::ARCH)?;
        let archives: &'static [crate::runtime_pins::PinnedArchive] =
            Box::leak(Box::new([crate::runtime_pins::PinnedArchive {
                target,
                url: "https://example.invalid/runtime.tar.gz",
                sha256: Box::leak(crate::sha256_hex(body).into_boxed_str()),
                bytes: body.len() as u64,
            }]));
        Some(Box::leak(Box::new(super::AssetSpec {
            id: "test-pinned-archive",
            analyzer: super::SANDBOX,
            kind: AssetKind::SandboxRuntime,
            source: AssetSource::PinnedArchive { archives },
            file: "fixture.tar.gz",
            licence: "test fixture",
        })))
    }

    /// A warm cache provisions with **no fetcher at all**, and is still
    /// verified.
    ///
    /// This is what makes "no network, warm cache" a real claim rather than an
    /// aspiration — and the first half is the one that matters most: an archive
    /// already on disk whose bytes do not match the pin is *refused*, so a warm
    /// cache can never become a way around the pin.
    #[test]
    fn a_warm_pinned_archive_provisions_offline_and_is_still_verified() {
        let body = b"pretend this is a runtime archive".to_vec();
        let Some(spec) = pinned_to(&body) else {
            eprintln!(
                "SKIPPED: no sandbox runtime is pinned for {}/{}",
                std::env::consts::OS,
                std::env::consts::ARCH
            );
            return;
        };
        let cache = Cache::new("pinned-warm");
        let target = asset_path(&cache.0, spec);
        std::fs::create_dir_all(target.parent().expect("parent")).expect("mkdir");

        // Right pin, wrong bytes: refused, without a fetcher ever being offered.
        std::fs::write(&target, b"tampered").expect("write");
        let err = provision(&cache.0, spec).expect_err("a warm cache is still verified");
        assert!(matches!(err, AssetError::DigestMismatch { .. }), "{err}");

        // The pinned bytes: provisions offline, with no fetcher at all.
        std::fs::write(&target, &body).expect("write");
        let record = provision(&cache.0, spec).expect("a matching warm cache provisions offline");
        assert_eq!(record.kind, AssetKind::SandboxRuntime);
        assert_eq!(record.digest, crate::sha256_hex(&body));

        // And `resolve`-style re-verification agrees the bytes are still right.
        assert_eq!(
            super::current_digest(&cache.0, spec).as_deref(),
            Some(record.digest.as_str())
        );
    }

    /// A fetcher that returns success over the wrong bytes cannot poison the
    /// cache: the archive is verified *before* it is renamed into place, so a
    /// failed provision leaves a cold cache rather than a bad one.
    ///
    /// This is the case [`Fetcher`]'s contract cannot cover for a `Download`
    /// asset, and the reason `PinnedArchive` exists.
    #[test]
    fn a_lying_fetcher_cannot_install_a_pinned_archive() {
        let body = b"the real runtime archive".to_vec();
        let Some(spec) = pinned_to(&body) else {
            eprintln!("SKIPPED: no sandbox runtime is pinned for this platform");
            return;
        };
        let cache = Cache::new("pinned-lying-fetcher");

        let liar: &super::Fetcher<'_> = &|_url: &str, dest: &std::path::Path| {
            std::fs::write(dest, b"truncated").map_err(|e| e.to_string())
        };
        let err = super::provision_with(&cache.0, spec, Some(liar))
            .expect_err("bytes that do not match the pin must be refused");
        assert!(matches!(err, AssetError::DigestMismatch { .. }), "{err}");

        // Nothing was left behind at the path anything reads, and no staging
        // file survived to be folded into a later digest.
        let target = asset_path(&cache.0, spec);
        assert!(!target.exists(), "a refused archive must not be installed");
        assert!(
            !target.with_extension("partial").exists(),
            "staging file left behind"
        );

        // An honest fetcher then provisions normally.
        let honest: &super::Fetcher<'_> = &|_url: &str, dest: &std::path::Path| {
            std::fs::write(dest, b"the real runtime archive").map_err(|e| e.to_string())
        };
        let record = super::provision_with(&cache.0, spec, Some(honest)).expect("provision");
        assert_eq!(record.digest, crate::sha256_hex(&body));
    }

    #[test]
    fn the_cache_root_prefers_the_explicit_override_then_roteiro_home() {
        assert_eq!(
            root_from(
                Some("/explicit".into()),
                Some("/home/.roteiro".into()),
                None
            ),
            PathBuf::from("/explicit")
        );
        assert_eq!(
            root_from(None, Some("/home/.roteiro".into()), None),
            PathBuf::from("/home/.roteiro/security")
        );
        assert_eq!(
            root_from(None, None, Some("/home/me".into())),
            PathBuf::from("/home/me/.roteiro/security")
        );
    }

    #[test]
    fn provisioning_a_vendored_asset_installs_and_records_it() {
        let cache = Cache::new("vendored");
        let record = provision(&cache.0, rules()).expect("provision");
        assert_eq!(record.kind, AssetKind::Rules);
        assert_eq!(record.digest.len(), 64);
        assert!(!record.fetched_at.is_empty());

        // The file is really there, and is really the vendored bytes.
        let AssetSource::Vendored(bytes) = rules().source else {
            panic!("the rule set is a vendored asset");
        };
        assert_eq!(
            std::fs::read(asset_path(&cache.0, rules())).expect("read"),
            bytes
        );
        assert_eq!(installed(&cache.0, rules()), Some(record));
    }

    /// `prefetch` is a thing you run when unsure, so running it twice must be
    /// harmless and must not change what a run will read.
    #[test]
    fn provisioning_is_idempotent() {
        let cache = Cache::new("idempotent");
        let first = provision(&cache.0, rules()).expect("first");
        let second = provision(&cache.0, rules()).expect("second");
        assert_eq!(first.digest, second.digest);
    }

    /// The headline offline contract: a cold cache fails, names what is missing,
    /// and prints the exact command that fixes it.
    #[test]
    fn a_cold_cache_fails_with_the_named_offline_error() {
        let cache = Cache::new("cold");
        let err = resolve(&cache.0, "semgrep").expect_err("a cold cache must fail");
        let ExecError::AssetsUnavailableOffline {
            analyzer,
            missing,
            command,
        } = &err
        else {
            panic!("expected the offline error, got {err:?}");
        };
        assert_eq!(analyzer, "semgrep");
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].id, "semgrep-rules");
        assert_eq!(command, "roteiro security prefetch --analyzer semgrep");

        // The rendered message has to carry all of it, because that is what a
        // user on a plane actually reads.
        let message = err.to_string();
        assert!(message.contains("assets-unavailable-offline"), "{message}");
        assert!(message.contains("semgrep-rules"), "{message}");
        assert!(
            message.contains("roteiro security prefetch --analyzer semgrep"),
            "{message}"
        );
    }

    #[test]
    fn a_warm_cache_resolves_to_the_provisioned_path() {
        let cache = Cache::new("warm");
        provision(&cache.0, rules()).expect("provision");
        let resolved = resolve(&cache.0, "semgrep").expect("a warm cache must resolve");
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].0, "semgrep-rules");
        assert_eq!(resolved[0].1, asset_path(&cache.0, rules()));
    }

    /// A record that no longer describes the bytes is worse than no record: a
    /// run would stamp a `rules_digest` that does not match what it read.
    #[test]
    fn an_asset_edited_after_provisioning_is_refused_not_warned_about() {
        let cache = Cache::new("tampered");
        provision(&cache.0, rules()).expect("provision");
        std::fs::write(asset_path(&cache.0, rules()), b"rules: []\n").expect("tamper");

        let err = resolve(&cache.0, "semgrep").expect_err("tampering must be refused");
        let ExecError::AssetsUnavailableOffline { missing, .. } = &err else {
            panic!("expected the offline error");
        };
        assert!(
            missing[0].reason.contains("no longer match"),
            "{}",
            missing[0].reason
        );
    }

    /// Roteiro never fetches the advisory database. Absent, it says where it
    /// looked and what to run — and does not go and get it.
    #[test]
    fn an_absent_external_asset_is_explained_never_fetched() {
        let cache = Cache::new("external");
        let err = provision(&cache.0, advisory_db()).expect_err("must not be fetched");
        let AssetError::ExternalMissing { hint, analyzer, .. } = &err else {
            panic!("expected ExternalMissing, got {err:?}");
        };
        assert_eq!(*analyzer, "cargo-audit");
        assert!(hint.contains("advisory-db"), "{hint}");
        assert!(
            err.to_string().contains("roteiro security prefetch"),
            "{err}"
        );
    }

    #[test]
    fn a_directory_asset_is_digested_by_content_not_by_layout() {
        let cache = Cache::new("tree");
        let db = asset_path(&cache.0, advisory_db());
        std::fs::create_dir_all(db.join("crates/openssl")).expect("create");
        std::fs::write(db.join("crates/openssl/RUSTSEC-2026-0031.md"), b"a").expect("write");
        std::fs::write(db.join("README.md"), b"b").expect("write");

        let first = provision(&cache.0, advisory_db()).expect("provision");
        assert_eq!(first.files, Some(2));

        // A `.git` directory is bookkeeping, not advisory data: adding one must
        // not move the digest.
        std::fs::create_dir_all(db.join(".git")).expect("create");
        std::fs::write(db.join(".git/HEAD"), b"ref: refs/heads/main").expect("write");
        assert_eq!(
            provision(&cache.0, advisory_db()).expect("again").digest,
            first.digest
        );

        // Changing an advisory does move it.
        std::fs::write(db.join("README.md"), b"c").expect("write");
        assert_ne!(
            provision(&cache.0, advisory_db()).expect("third").digest,
            first.digest
        );
    }

    #[test]
    fn status_reports_what_is_provisioned_and_what_is_not() {
        let cache = Cache::new("status");
        let cold = status(&cache.0, Some("semgrep"));
        assert_eq!(cold.len(), 1);
        assert!(cold[0].installed.is_none());
        assert!(cold[0].verified.is_none());
        assert!(cold[0].age_days.is_none());

        provision(&cache.0, rules()).expect("provision");
        let warm = status(&cache.0, Some("semgrep"));
        assert_eq!(warm[0].verified, Some(true));
        assert_eq!(warm[0].age_days, Some(0));
        assert_eq!(warm[0].installed.as_ref().map(|r| r.digest.len()), Some(64));

        // …and it notices when the bytes stop matching.
        std::fs::write(asset_path(&cache.0, rules()), b"rules: []\n").expect("tamper");
        assert_eq!(status(&cache.0, Some("semgrep"))[0].verified, Some(false));
    }

    #[test]
    fn status_covers_every_analyzer_when_none_is_named() {
        let cache = Cache::new("status-all");
        assert_eq!(status(&cache.0, None).len(), ASSETS.len());
        assert!(status(&cache.0, Some("no-such-analyzer")).is_empty());
    }

    /// The install paths of a downloadable asset name where bytes from the
    /// network are written, so a `..` in one would write outside the asset
    /// cache. They are compiled in today, which is exactly why the check is
    /// worth having: nothing else would notice a typo that escaped.
    #[test]
    fn a_download_path_that_escapes_the_asset_directory_is_refused() {
        static ESCAPING: &[super::DownloadFile] = &[super::DownloadFile {
            path: "../../outside.zip",
            url: "https://example.invalid/outside.zip",
        }];
        let cache = Cache::new("escape");
        let spec = super::AssetSpec {
            id: "escaping-asset",
            analyzer: "osv-scanner",
            kind: AssetKind::AdvisoryDb,
            source: AssetSource::Download { files: ESCAPING },
            file: "",
            licence: "n/a",
        };
        let fetched = std::cell::Cell::new(false);
        let fetch = |_: &str, _: &std::path::Path| {
            fetched.set(true);
            Ok(())
        };
        let err = super::provision_with(&cache.0, &spec, Some(&fetch))
            .expect_err("an escaping path must be refused");
        assert!(
            matches!(err, AssetError::UnsafeInstallPath { .. }),
            "{err:?}"
        );
        assert!(
            !fetched.get(),
            "the path is checked before anything is fetched"
        );
    }

    /// An analyzer this build cannot run has no assets, and asking for them is
    /// not an error — it is simply an empty answer.
    #[test]
    fn an_unknown_analyzer_needs_nothing() {
        assert!(assets_for("no-such-analyzer").is_empty());
        let cache = Cache::new("unknown");
        assert!(
            resolve(&cache.0, "no-such-analyzer")
                .expect("no assets")
                .is_empty()
        );
    }
}

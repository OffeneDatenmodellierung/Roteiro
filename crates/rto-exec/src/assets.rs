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
//! # Two kinds of asset, and why there is no third yet
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
//! There is deliberately no download-by-URL source yet: nothing shipped here
//! needs one, and an unused fetch path is a security surface with no user. The
//! enum is `#[non_exhaustive]`, so adding one later is not a breaking change.
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
}

impl AssetKind {
    /// Stable token for display and `--json`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rules => "rules",
            Self::AdvisoryDb => "advisory-db",
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
        AssetSource::Vendored(_) => dir.join(spec.file),
        AssetSource::External { .. } => dir.join("db"),
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

/// Install and verify one asset, returning what was recorded.
///
/// This is the **only** function that writes to the asset cache. It is
/// idempotent: re-running it re-digests and re-stamps, which is what makes
/// `prefetch` a safe thing to run whenever you are unsure.
///
/// # Errors
/// Returns [`AssetError::ExternalMissing`] when an operator-provisioned asset is
/// absent — never a fetch — or [`AssetError::Io`] if the cache cannot be
/// written.
pub fn provision(root: &Path, spec: &AssetSpec) -> Result<InstalledAsset, AssetError> {
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
    };

    let record = InstalledAsset {
        id: spec.id.to_owned(),
        kind: spec.kind,
        digest,
        fetched_at: rfc3339_utc(std::time::SystemTime::now()),
        files,
    };
    let json = serde_json::to_vec_pretty(&record)?;
    write_atomically(&record_path(root, spec), &json)?;
    Ok(record)
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

/// Digest of what is on disk right now, or `None` if it is not there.
fn current_digest(root: &Path, spec: &AssetSpec) -> Option<String> {
    let target = asset_path(root, spec);
    match spec.source {
        AssetSource::Vendored(_) => Some(sha256_hex(&std::fs::read(target).ok()?)),
        AssetSource::External { .. } => digest_tree(&target).ok().map(|(digest, _)| digest),
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

    #[test]
    fn every_asset_belongs_to_an_analyzer_that_asked_for_it() {
        for spec in ASSETS {
            assert!(
                assets_for(spec.analyzer).iter().any(|s| s.id == spec.id),
                "{} is not claimed by {}",
                spec.id,
                spec.analyzer
            );
            assert!(!spec.licence.is_empty(), "{} discloses no licence", spec.id);
        }
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

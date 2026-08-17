//! The analyzer execution seam: one contract, interchangeable backends.
//!
//! Running an external analyzer (`cargo-audit`, `semgrep`, successors) can happen
//! in CI, on a developer's machine, or — later — locally inside a sandbox. This
//! crate exists so those stop being competing architectures: every backend
//! implements one [`AnalyzerRunner`] trait, takes one [`AnalysisRequest`], and
//! returns one [`AnalysisResponse`] of normalized findings plus run evidence. A
//! caller never learns which backend produced a result, so adding the sandboxed
//! and subprocess backends later changes no call site.
//!
//! Today there is exactly one implementation, [`IngestRunner`], which consumes a
//! normalized report produced elsewhere. It is the zero-install default, not a
//! fallback: it needs no container runtime and adds no isolation surface, and
//! what it produces is byte-for-byte the shape a sandboxed run will produce.
//!
//! # What this crate does not do
//!
//! It does not decide how results are *stored*. Persistence lives in `rto-graph`,
//! which files findings in their own tables — never `nodes`/`edges`, never a
//! provenance class, never in the exported graph artifact (ADR-0012). Nothing
//! here can move the published `GraphArtifact` by a byte, and that is checked by
//! test rather than assumed.
//!
//! No analyzer is implemented here, and no sandbox dependency is pulled in; the
//! backends arrive behind their own features (ADR-0014).
//!
//! @rto:0014
//! @rto:0012
//!
//! # Example
//!
//! ```
//! use rto_exec::{AnalysisRequest, AnalyzerRunner, Consent, IngestRunner, Worktree};
//! use rto_graph::SourceIdentity;
//!
//! let report = br#"{
//!   "schema": "roteiro.findings/v1",
//!   "analyzer": "cargo-audit",
//!   "analyzer_version": "0.21.0",
//!   "started_at": "2026-08-15T09:00:00Z",
//!   "ended_at": "2026-08-15T09:00:04Z",
//!   "exit_status": 1,
//!   "findings": [{
//!     "identity": ["RUSTSEC-2024-0001", "openssl", "0.10.5", "lock123"],
//!     "rule": "RUSTSEC-2024-0001",
//!     "severity": "high",
//!     "title": "openssl is vulnerable",
//!     "message": "upgrade to 0.10.66"
//!   }]
//! }"#;
//!
//! let request = AnalysisRequest {
//!     analyzer: "cargo-audit".to_owned(),
//!     worktree: Worktree::read_only("/repo".as_ref()).expect("worktree"),
//!     network: rto_graph::NetworkPolicy::Deny,
//!     consent: Consent::Granted,
//!     source: SourceIdentity::default(),
//! };
//! let response = IngestRunner::new(report.to_vec()).run(&request).expect("ingest");
//! assert_eq!(response.findings.len(), 1);
//! assert_eq!(response.run.isolation, rto_graph::Isolation::Ingested);
//! ```

pub mod adapter;
/// Where the pinned-asset cache lives, and the precedence that decides it.
///
/// Its source carries no `//!` header because `build.rs` pulls the same file in
/// with `include!`, where an inner doc comment is a syntax error — so the module
/// documentation lives here instead. `build.rs` needs it to find the sandbox
/// runtime `roteiro security prefetch` installed, which is the same cache
/// [`asset_paths::asset_root`] names; read the file's own comments for why that
/// is shared rather than copied.
pub mod asset_paths;
// Asset provisioning is **always compiled**, behind no feature at all.
//
// It used to be `cfg(any(exec-subprocess, exec-boxlite))`, on the reading that
// provisioning belongs to whichever backend consumes the assets. That was the
// wrong shape and this module half-said so already: it is shared between the
// backends and owned by neither, and the note on `SANDBOX_RUNTIME_NOTICE` below
// records that an `exec-subprocess`-only build provisions *for a later
// `exec-boxlite` build* — provisioning already served a backend that was not
// compiled in.
//
// The bootstrap argument settles it. `AGENTS.md` tells a contributor to run
// `roteiro security prefetch --allow-download` *before* building
// `--features exec-boxlite`, because that build script requires the verified
// archive at compile time. If prefetch lived behind an execution feature, you
// would need a build with a *different* execution backend compiled in before you
// could provision the one you actually wanted. That is circular.
//
// Nothing here executes anything: it downloads, digests, pins and reports. Every
// `Command::new` in this crate is in `subprocess.rs` or `boxlite.rs`, and both
// stay behind their features. Provisioning is not execution.
pub mod assets;
#[cfg(feature = "exec-boxlite")]
pub mod boxlite;
mod clock;
pub mod crossref;
/// Emitting a `file://` URL for a local path, and reading one back.
///
/// Its source carries no `//!` header because `build.rs` pulls the same file in
/// with `include!`, where an inner doc comment is a syntax error — so the module
/// documentation lives here instead. `build.rs` is this crate's only emitter: it
/// prints the `BOXLITE_RUNTIME_URL=` recipe an operator pastes, and parses that
/// variable back when it is set. What reads the URL in between is `boxlite`'s
/// own `curl`, which percent-decodes and rejects an unencoded space outright —
/// read the file's own comments for the measurements, and for why the encoder
/// and the decoder have to be one file rather than two.
pub mod file_url;
mod ingest;
mod runner;
/// The per-file digests of the extracted sandbox runtime — **generated**.
///
/// Derived from the archives in [`runtime_pins`] by
/// `scripts/derive-runtime-file-pins.py`, and verified by `build.rs` against
/// what `boxlite` actually extracted, since those files rather than the archive
/// are what `include_bytes!` puts in the binary. Same `include!` arrangement,
/// and so the same standalone constraint; its module documentation lives here
/// for the same reason [`runtime_pins`]'s does.
pub mod runtime_file_pins;
/// The pinned sandbox-runtime archives, and the host-platform selection.
///
/// Its source carries no `//!` header because `build.rs` pulls the same file in
/// with `include!`, where an inner doc comment is a syntax error — so the module
/// documentation lives here instead. Read the file's own comments for what is
/// pinned and why it has to be.
pub mod runtime_pins;
pub mod snippet;
#[cfg(feature = "exec-subprocess")]
pub mod subprocess;

pub use adapter::{
    ADAPTERS, Adapter, AssetPaths, Invocation, NO_SNIPPET, NativeContext, UNKNOWN_VERSION,
    adapter_for, known_analyzers, snippet_hash, snippet_hash_at,
};
pub use assets::{
    ASSETS, AssetKind, AssetSource, AssetSpec, AssetStatus, DownloadFile, Fetcher, InstalledAsset,
    MissingAsset, SANDBOX, asset, asset_path, asset_root, assets_for, provision, provision_with,
    resolve, status,
};
#[cfg(feature = "exec-boxlite")]
pub use boxlite::{BoxliteRunner, SandboxError, SandboxProbe, sandbox_probe};
pub use clock::{age_in_days, rfc3339_from_unix, rfc3339_utc, unix_from_rfc3339};
pub use crossref::{Correspondence, Report, cross_reference};
pub use ingest::{
    IngestRunner, MAX_REPORT_FINDINGS, NormalizedReport, REPORT_SCHEMA, ReportFinding,
    normalize_native,
};
pub use runner::{
    AnalysisRequest, AnalysisResponse, AnalyzerRunner, Consent, ExecError, Worktree,
    check_reported_path, check_request, worktree_id,
};
pub use runtime_file_pins::{
    PinnedFile, PinnedRuntimeFiles, RUNTIME_FILES, RUNTIME_FILES_VERSION, runtime_files_for,
};
pub use runtime_pins::{
    PinnedArchive, RUNTIME_ARCHIVES, RUNTIME_ASSET, RUNTIME_FILE, RUNTIME_VERSION, archive_for,
    runtime_target,
};
pub use snippet::{NoSnippets, SnippetSource, WorktreeSnippets};
#[cfg(feature = "exec-subprocess")]
pub use subprocess::{SubprocessError, SubprocessRunner};

/// The licence notice for the third-party binaries an `exec-boxlite` build
/// embeds, compiled in so it cannot be separated from what it describes.
///
/// `roteiro security prefetch` prints it before installing the sandbox runtime,
/// which is the same disclose-then-consent shape `roteiro model pull` uses. It
/// is compiled into every build, because every build can provision the runtime —
/// including one with no execution backend at all, which prefetches it for a
/// later `exec-boxlite` build — so the obligations travel with the artifact
/// rather than living only in the repository.
pub const SANDBOX_RUNTIME_NOTICE: &str = include_str!("../NOTICE-boxlite-runtime.md");

/// Lowercase hex SHA-256 of `bytes`.
///
/// Used for the report digest that ties an `AnalysisRun` to the exact bytes it
/// was derived from, and for deriving an opaque worktree id from a path.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::sha256_hex;

    #[test]
    fn hashes_the_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}

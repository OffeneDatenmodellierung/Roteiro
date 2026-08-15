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
#[cfg(feature = "exec-subprocess")]
pub mod assets;
mod clock;
mod ingest;
mod runner;
pub mod snippet;
#[cfg(feature = "exec-subprocess")]
pub mod subprocess;

pub use adapter::{
    ADAPTERS, Adapter, AssetPaths, Invocation, NO_SNIPPET, NativeContext, UNKNOWN_VERSION,
    adapter_for, known_analyzers, snippet_hash, snippet_hash_at,
};
#[cfg(feature = "exec-subprocess")]
pub use assets::{
    ASSETS, AssetKind, AssetSpec, AssetStatus, InstalledAsset, MissingAsset, asset, asset_path,
    asset_root, assets_for, provision, resolve, status,
};
pub use clock::{age_in_days, rfc3339_from_unix, rfc3339_utc, unix_from_rfc3339};
pub use ingest::{
    IngestRunner, MAX_REPORT_FINDINGS, NormalizedReport, REPORT_SCHEMA, ReportFinding,
    normalize_native,
};
pub use runner::{
    AnalysisRequest, AnalysisResponse, AnalyzerRunner, Consent, ExecError, Worktree,
    check_reported_path, check_request, worktree_id,
};
pub use snippet::{NoSnippets, SnippetSource, WorktreeSnippets};
#[cfg(feature = "exec-subprocess")]
pub use subprocess::{SubprocessError, SubprocessRunner};

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

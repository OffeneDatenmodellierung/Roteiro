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
//! It does not *always* produce something to store, either. `lint` — named
//! rather than linked, because that module is behind `exec-subprocess` and this
//! page is read from builds without it — runs a linter and returns a report the
//! caller prints: no [`AnalysisRun`], no layer, no store. A lint name is a
//! symbol in a compiler rather than an assigned identifier, so it is an opinion
//! about the code as it stands today rather than a durable fact about the
//! repository (ADR-0020 v1.1) — and everything below about persistence simply
//! does not apply to it.
//!
//! [`AnalysisRun`]: rto_graph::AnalysisRun
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
pub mod asset_paths;
// Asset provisioning is **always compiled**, behind no feature at all: it
// downloads, digests, pins and reports, and executes nothing. Every
// `Command::new` in this crate is behind `exec-subprocess`. Provisioning is not
// execution, so a build with no execution backend can still prefetch the rules
// and advisory databases a later run, or CI, will use.
pub mod assets;
#[cfg(feature = "exec-subprocess")]
mod child_env;
pub mod crossref;
pub mod guidance;
mod ingest;
#[cfg(feature = "exec-subprocess")]
pub mod lint;
pub mod lint_grant;
mod runner;
pub mod snippet;
#[cfg(feature = "exec-subprocess")]
pub mod subprocess;
pub mod tool_security;

pub use adapter::{
    ADAPTERS, Adapter, AssetPaths, Invocation, LINT_ANALYZERS, NO_SNIPPET, NativeContext,
    UNKNOWN_VERSION, adapter_for, known_analyzers, snippet_hash, snippet_hash_at,
};
// The linter's adapter is re-exported like any other, and — unlike any other —
// is **not** in [`ADAPTERS`], so `ingest` cannot resolve it and nothing can file
// its output as a layer. See [`adapter::clippy`].
pub use adapter::clippy::{Clippy, FeatureSet};
pub use assets::{
    ASSETS, AssetKind, AssetSource, AssetSpec, AssetStatus, DownloadFile, Fetcher, InstalledAsset,
    MissingAsset, asset, asset_path, asset_root, assets_for, provision, provision_with, resolve,
    status,
};
// The RFC 3339 UTC formatter moved to `rto-graph` in #667: `rto-exec` is an
// optional dependency of the CLI, and the CLI's render path — which is gated on
// nothing — needs the same function to stamp a bundle's timestamps. It is
// `okf_instant` that needs it today; the workspace-vault renderer that needed it
// when #667 was filed was deleted by #671, whose OKF replacement reintroduced the
// same unlinked-crate error at a new line. The move is aimed at that recurrence,
// not at either call site.
//
// Re-exported under the names it has always had so this crate's own callers, and
// `rto_exec::rfc3339_utc` in the CLI's `execution`/`remote` paths, did not have
// to move with it.
pub use crossref::{
    Correspondence, Report, across_analyzers as cross_reference_across_analyzers, cross_reference,
};
pub use guidance::{Guidance, Line as GuidanceLine};
pub use ingest::{
    IngestRunner, MAX_REPORT_FINDINGS, NormalizedReport, REPORT_SCHEMA, ReportFinding,
    normalize_native,
};
#[cfg(feature = "exec-subprocess")]
pub use lint::{LintError, LintOutcome, Toolchain, invocation as lint_invocation, run as run_lint};
pub use rto_graph::{age_in_days, rfc3339_from_unix, rfc3339_utc, unix_from_rfc3339};
// ADR-0020 §6's grant, re-exported under `lint_`-prefixed names so a reader who
// meets `ConfigGrant` in the binary can see which key it governs.
pub use lint_grant::{
    ConfigGrant as LintConfigGrant, Decision as LintDecision, Reason as LintReason,
    Requested as LintRequested, decide as decide_lint_host,
};
pub use runner::{
    AnalysisRequest, AnalysisResponse, AnalyzerRunner, Consent, ExecError, Worktree,
    check_reported_path, check_request, worktree_id,
};
pub use snippet::{NoSnippets, SnippetSource, WorktreeSnippets};
#[cfg(feature = "exec-subprocess")]
pub use subprocess::{SubprocessError, SubprocessRunner};
pub use tool_security::{
    AnalyzerCoverage, Coverage, CrossReference, CrossReferenceReport, LayerStaleness, MachineScope,
    Readiness, RepositoryScope, SecurityListReport, TOOL_SECURITY_LIST_SCHEMA,
    TOOL_SECURITY_STATUS_SCHEMA, ToolFindingsLayer, ToolSecurityList, ToolSecurityStatus,
    coverage_matrix, coverage_matrix_with, layer_staleness, security_list, security_status,
};

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

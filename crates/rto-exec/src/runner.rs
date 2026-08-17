//! The contract every analyzer backend satisfies.

use std::path::{Component, Path, PathBuf};

use rto_graph::{
    AnalysisRun, Finding, FindingsError, Isolation, NetworkPolicy, RunnerKind, SourceIdentity,
    WorktreeAccess, WorktreeId, analyzer_id_error, is_valid_analyzer_id,
};

use crate::sha256_hex;

/// Errors an analyzer backend can raise.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ExecError {
    /// The request did not carry explicit user consent. Running an analyzer is
    /// never implicit, whatever the backend.
    #[error("analyzer run requires explicit user consent")]
    ConsentRequired,
    /// The request asked for a network policy this backend will not honour.
    /// Egress is denied; an analyzer's inputs are pre-provisioned, never fetched
    /// mid-run.
    #[error("unsupported network policy: this runner only accepts `deny`")]
    UnsupportedNetworkPolicy,
    /// The request asked for a writable worktree. Analyzers parse source,
    /// manifests and lockfiles; none of them needs to write to the tree.
    #[error("the analyzed worktree must be read-only")]
    WorktreeNotReadOnly,
    /// The requested analyzer id is not well-formed: an analyzer id is
    /// 1..=`MAX_ANALYZER_ID` characters of lowercase `[a-z0-9._-]`.
    ///
    /// The message is produced by [`rto_graph::analyzer_id_error`], the same
    /// function `rto-graph`'s own rejection uses, so an id refused here reads
    /// exactly as it would had the store caught it — and it names the rule that
    /// was broken, not just the contract.
    #[error("{}", analyzer_id_error(.0))]
    InvalidAnalyzerId(String),
    /// The report describes a different analyzer than the one requested — a
    /// mixed-up file, or a report substituted for another.
    #[error("report is from analyzer {reported:?}, but {requested:?} was requested")]
    AnalyzerMismatch {
        /// The analyzer the caller asked for.
        requested: String,
        /// The analyzer the report claims to be from.
        reported: String,
    },
    /// The report's schema tag is not one this build understands.
    #[error("unsupported report schema: {found:?} (expected {expected:?})")]
    UnsupportedSchema {
        /// The tag the report carried.
        found: String,
        /// The tag this build accepts.
        expected: &'static str,
    },
    /// The report is structurally valid JSON but does not describe a usable run.
    #[error("malformed report: {0}")]
    MalformedReport(String),
    /// The report declares more findings than will be accepted in one run.
    #[error("report declares {count} findings, more than the {max} accepted in one run")]
    TooManyFindings {
        /// How many the report declared.
        count: usize,
        /// The accepted ceiling.
        max: usize,
    },
    /// Two findings in one report share an identity, so one would silently
    /// shadow the other.
    #[error("duplicate finding identity in report: {0}")]
    DuplicateFinding(String),
    /// A finding claimed a path outside the analyzed worktree.
    #[error("finding path escapes the worktree: {0:?}")]
    PathEscapesWorktree(String),
    /// A finding's identity components were not usable as a stable key.
    #[error("finding identity: {0}")]
    Identity(#[from] FindingsError),
    /// The analyzer's pinned inputs are not provisioned, and Roteiro will not
    /// fetch them mid-run.
    ///
    /// This is ADR-0014's named cold-cache failure. The message carries
    /// everything needed to act on it without a second command: which analyzer,
    /// which assets, the digest pinned for each, why each one could not be used,
    /// and the exact `prefetch` invocation. The `assets-unavailable-offline`
    /// token is part of the message so the failure is greppable and scriptable
    /// rather than merely readable.
    #[error(
        "assets-unavailable-offline: {analyzer} cannot run because its pinned inputs are not \
         provisioned\n  missing: {}\n  fix it with: {command}\n  \
         (roteiro never fetches analyzer assets during a run, and never falls back to whatever \
         the host has installed)",
        .missing.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n           ")
    )]
    AssetsUnavailableOffline {
        /// The analyzer whose run was refused.
        analyzer: String,
        /// Every asset that was missing, unverifiable, or changed underneath its
        /// record.
        missing: Vec<crate::assets::MissingAsset>,
        /// The exact command that provisions them.
        command: String,
    },
    /// The analyzer binary could not be executed, or exited with a status that
    /// does not carry a usable report.
    #[cfg(feature = "exec-subprocess")]
    #[error(transparent)]
    Subprocess(#[from] crate::subprocess::SubprocessError),
    /// Provisioning an asset failed.
    #[error(transparent)]
    Asset(#[from] crate::assets::AssetError),
    /// The sandboxed backend could not run the analyzer.
    #[cfg(feature = "exec-boxlite")]
    #[error(transparent)]
    Sandbox(#[from] crate::boxlite::SandboxError),
    /// This build has no adapter for the requested analyzer, so it can neither
    /// run it nor read its native output.
    #[error("no adapter for analyzer {requested:?} in this build (known: {known})")]
    UnknownAnalyzer {
        /// The analyzer the caller asked for.
        requested: String,
        /// The analyzer ids this build does know, comma-separated.
        known: String,
    },
    /// The report was not valid JSON.
    #[error("report is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// Explicit user consent to run an analyzer.
///
/// Consent is part of the *request*, not of a backend, so no backend can be
/// wired up in a way that skips it. For `roteiro security ingest` the user's
/// invocation naming a report file **is** the consent; a backend that fetches
/// assets or executes a container will need an interactive grant instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consent {
    /// The user explicitly asked for this run.
    Granted,
    /// No consent was given; the run must not proceed.
    Withheld,
}

/// The worktree an analyzer is pointed at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    /// Filesystem location of the checkout.
    pub path: PathBuf,
    /// The opaque id that scopes this checkout's findings layer.
    pub id: WorktreeId,
    /// How the tree is exposed to the analyzer.
    pub access: WorktreeAccess,
}

impl Worktree {
    /// A read-only worktree at `path`, with its id derived from that path by
    /// [`worktree_id`].
    ///
    /// # Errors
    /// Returns [`ExecError::Identity`] if the derived id is not well-formed,
    /// which cannot happen for a hex digest but is surfaced rather than
    /// unwrapped.
    pub fn read_only(path: &Path) -> Result<Self, ExecError> {
        Ok(Self {
            path: path.to_path_buf(),
            id: worktree_id(path)?,
            access: WorktreeAccess::ReadOnly,
        })
    }
}

/// Derive a stable, opaque id for the checkout at `path`.
///
/// The id is the first 16 hex characters of the SHA-256 of the path in absolute
/// form. It is deliberately *not* the path itself: a layer key is stored and
/// printed, and a local filesystem path is user-identifying data that has no
/// business in a persisted record. Resolution is lexical (`std::path::absolute`),
/// so the id is stable and does not depend on the checkout existing.
///
/// # Errors
/// Returns [`ExecError::Identity`] if the derived token is somehow not a
/// well-formed [`WorktreeId`].
pub fn worktree_id(path: &Path) -> Result<WorktreeId, ExecError> {
    // A path that cannot be made absolute (no working directory) still has a
    // usable lexical form; fall back to it rather than failing the run.
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let digest = sha256_hex(absolute.to_string_lossy().as_bytes());
    Ok(WorktreeId::new(&digest[..16])?)
}

/// What a caller asks a backend to do.
///
/// The same request shape serves every backend, which is the whole point of the
/// seam: a caller that ingests a CI report today and runs a sandboxed analyzer
/// tomorrow builds the identical value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisRequest {
    /// Which analyzer to run.
    pub analyzer: String,
    /// The read-only worktree to analyze.
    pub worktree: Worktree,
    /// Egress policy for the run.
    pub network: NetworkPolicy,
    /// Explicit user consent.
    pub consent: Consent,
    /// The source identity the run is against (commit / tree / lockfile blob),
    /// as far as the caller knows it. A backend may fill in more.
    pub source: SourceIdentity,
}

/// What a backend returns: normalized findings plus the evidence for the run
/// that produced them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisResponse {
    /// The run record, ready to persist.
    pub run: AnalysisRun,
    /// The findings it produced, ordered by their stable identity key.
    pub findings: Vec<Finding>,
}

/// One analyzer backend.
///
/// Implementations differ only in *where* the analyzer ran; the request and the
/// response are the same, so CI ingestion and a local sandboxed run are the same
/// code path from a caller's point of view. Every implementation must call
/// [`check_request`] before doing any work, so the consent, network and
/// worktree-access guarantees hold uniformly rather than per-backend.
pub trait AnalyzerRunner {
    /// Which backend this is — recorded on every run it produces.
    fn kind(&self) -> RunnerKind;

    /// The isolation boundary this backend actually provides. Recorded honestly:
    /// a backend with no boundary reports [`Isolation::None`], never something
    /// stronger.
    fn isolation(&self) -> Isolation;

    /// Execute the request.
    ///
    /// # Errors
    /// Returns [`ExecError`] if the request violates the shared contract (see
    /// [`check_request`]) or the backend cannot produce a usable result. A failed
    /// run yields no partial result: either a complete [`AnalysisResponse`] or an
    /// error.
    fn run(&self, request: &AnalysisRequest) -> Result<AnalysisResponse, ExecError>;
}

/// The preflight every backend shares: explicit consent, denied egress, a
/// read-only worktree, and a well-formed analyzer id.
///
/// It lives outside the trait so the guarantees are stated once and cannot drift
/// between backends — a subprocess backend that forgot the consent check would
/// otherwise be a one-line omission.
///
/// # Errors
/// Returns [`ExecError::ConsentRequired`], [`ExecError::UnsupportedNetworkPolicy`],
/// [`ExecError::WorktreeNotReadOnly`], or [`ExecError::InvalidAnalyzerId`] — the
/// last when the analyzer id is not 1..=[`rto_graph::MAX_ANALYZER_ID`]
/// characters of lowercase `[a-z0-9._-]`.
pub fn check_request(request: &AnalysisRequest) -> Result<(), ExecError> {
    if request.consent != Consent::Granted {
        return Err(ExecError::ConsentRequired);
    }
    if request.network != NetworkPolicy::Deny {
        return Err(ExecError::UnsupportedNetworkPolicy);
    }
    if request.worktree.access != WorktreeAccess::ReadOnly {
        return Err(ExecError::WorktreeNotReadOnly);
    }
    if !is_valid_analyzer_id(&request.analyzer) {
        return Err(ExecError::InvalidAnalyzerId(request.analyzer.clone()));
    }
    Ok(())
}

/// Reject a reported path that is absolute or climbs out of the worktree.
///
/// A finding is a claim about a file *in the analyzed tree*. A report that names
/// `/etc/shadow` or `../../secrets` is either broken or hostile, and either way
/// its claim cannot be checked, so it is refused rather than stored.
///
/// # Errors
/// Returns [`ExecError::PathEscapesWorktree`] for an empty, absolute, prefixed or
/// parent-climbing path.
pub fn check_reported_path(path: &str) -> Result<(), ExecError> {
    let escapes = path.is_empty()
        || Path::new(path).components().any(|c| {
            matches!(
                c,
                Component::RootDir | Component::Prefix(_) | Component::ParentDir
            )
        });
    if escapes {
        return Err(ExecError::PathEscapesWorktree(path.to_owned()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        AnalysisRequest, Consent, ExecError, Worktree, check_reported_path, check_request,
        worktree_id,
    };
    use rto_graph::{NetworkPolicy, SourceIdentity, WorktreeAccess};

    fn request() -> AnalysisRequest {
        AnalysisRequest {
            analyzer: "cargo-audit".to_owned(),
            worktree: Worktree::read_only("/repo".as_ref()).expect("worktree"),
            network: NetworkPolicy::Deny,
            consent: Consent::Granted,
            source: SourceIdentity::default(),
        }
    }

    #[test]
    fn a_well_formed_request_passes_preflight() {
        check_request(&request()).expect("preflight");
    }

    #[test]
    fn preflight_refuses_a_run_without_consent() {
        let mut req = request();
        req.consent = Consent::Withheld;
        assert!(matches!(
            check_request(&req),
            Err(ExecError::ConsentRequired)
        ));
    }

    #[test]
    fn preflight_refuses_a_writable_worktree() {
        let mut req = request();
        req.worktree.access = WorktreeAccess::ReadWrite;
        assert!(matches!(
            check_request(&req),
            Err(ExecError::WorktreeNotReadOnly)
        ));
    }

    #[test]
    fn preflight_refuses_a_malformed_analyzer_id() {
        let mut req = request();
        req.analyzer = "Cargo Audit".to_owned();
        assert!(matches!(
            check_request(&req),
            Err(ExecError::InvalidAnalyzerId(_))
        ));
    }

    /// The preflight enforces a length limit as well as a character set, so the
    /// rejection has to say so. Being told an over-long id must be "non-empty" —
    /// which it plainly was — is no help at all.
    #[test]
    fn preflight_refuses_an_over_long_analyzer_id_and_says_why() {
        let mut req = request();
        req.analyzer = "a".repeat(rto_graph::MAX_ANALYZER_ID + 1);
        let err = check_request(&req).expect_err("an over-long id must be refused");
        assert!(matches!(err, ExecError::InvalidAnalyzerId(_)));
        let message = err.to_string();
        assert!(
            message.contains("over the 64-character limit"),
            "the rejection must name the length rule: {message}"
        );
        assert!(
            message.contains("1 to 64 characters of lowercase [a-z0-9._-]"),
            "and state the whole contract: {message}"
        );
    }

    /// One rejection, one wording. Both layers format through
    /// `rto_graph::analyzer_id_error`, so an id refused at the seam reads exactly
    /// as it would had the store caught it — a caller cannot be told two stories
    /// about the same input depending on how deep the check happened to run.
    #[test]
    fn the_two_layers_word_a_rejection_identically() {
        for id in [
            "",
            "Semgrep",
            "a:b",
            &"a".repeat(rto_graph::MAX_ANALYZER_ID + 1),
        ] {
            let seam = ExecError::InvalidAnalyzerId(id.to_owned()).to_string();
            let store = rto_graph::FindingsError::InvalidAnalyzerId(id.to_owned()).to_string();
            assert_eq!(seam, store, "{id:?} reads differently in the two layers");
            assert_eq!(seam, rto_graph::analyzer_id_error(id));
        }
    }

    #[test]
    fn worktree_ids_are_opaque_stable_and_path_scoped() {
        let a = worktree_id("/repo/one".as_ref()).expect("a");
        let b = worktree_id("/repo/two".as_ref()).expect("b");
        assert_ne!(a, b, "different checkouts get different layers");
        assert_eq!(a, worktree_id("/repo/one".as_ref()).expect("again"));
        assert_eq!(a.as_str().len(), 16);
        assert!(
            !a.as_str().contains("repo"),
            "the id must not embed the path"
        );
    }

    #[test]
    fn reported_paths_must_stay_inside_the_worktree() {
        check_reported_path("src/tls.rs").expect("relative path is fine");
        for bad in ["", "/etc/shadow", "../../secrets", "src/../../etc/passwd"] {
            assert!(
                matches!(
                    check_reported_path(bad),
                    Err(ExecError::PathEscapesWorktree(_))
                ),
                "{bad:?} should be refused"
            );
        }
    }
}

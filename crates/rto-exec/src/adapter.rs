//! Per-analyzer adapters: native analyzer output in, a [`NormalizedReport`] out.
//!
//! An adapter is the **only** analyzer-specific code in this crate. It knows one
//! tool's native JSON, how to name that tool's findings so they are recognisable
//! across runs, and which argv produces that JSON. Everything downstream — the
//! validation, the identity keys, the ordering, the store — is shared.
//!
//! # Why this is the seam, and not the runner
//!
//! ADR-0012 requires that "a finding is the same artifact whether it was produced
//! locally in a sandbox or ingested from a CI report". The cheap way to satisfy
//! that is to write the conversion twice and add a test comparing the two. This
//! crate does the other thing: **there is one conversion**, and both paths call
//! it. A subprocess run captures the analyzer's stdout and hands those bytes to
//! the adapter; `roteiro security ingest` reads a file of the same native bytes
//! and hands them to the same adapter. Equality of the resulting [`Finding`]s is
//! therefore a property of the code, and the tests that assert it are guarding
//! against a future refactor rather than establishing the invariant.
//!
//! [`Finding`]: rto_graph::Finding
//!
//! # Adding an analyzer needs no migration
//!
//! [`rto_graph::FindingKey`] is `finding:<analyzer>:<that analyzer's own ordered
//! identity components>`. An adapter chooses the recipe; the schema never learns
//! what the components mean. So a new analyzer is a new file in `adapters/`, an
//! entry in [`ADAPTERS`], and nothing else — no schema change, no migration.
//!
//! @rto:0012
//! @rto:0014

use rto_graph::SourceIdentity;

use crate::ingest::NormalizedReport;
use crate::runner::ExecError;
use crate::snippet::SnippetSource;

pub mod cargo_audit;
pub mod semgrep;

/// Everything an adapter may need that is *not* in the analyzer's own output.
///
/// Native analyzer output is missing things the evidence chain requires — no
/// mainstream analyzer stamps its report with the wall-clock window it ran in,
/// and `cargo audit` does not even record its own version. Rather than let an
/// adapter invent them, the caller supplies what it actually knows, and an
/// adapter that has nothing better says so ([`UNKNOWN_VERSION`]).
#[derive(Clone)]
pub struct NativeContext<'a> {
    /// When the run started, RFC 3339 UTC. A subprocess run measures it; an
    /// ingest of a report file uses the file's modification time, which is the
    /// only timestamp evidence a bare report carries.
    pub started_at: String,
    /// When the run ended, RFC 3339 UTC.
    pub ended_at: String,
    /// The analyzer's version, where the caller learned it out of band (a
    /// subprocess run asks the binary). `None` leaves the adapter to use
    /// whatever the report itself carries.
    pub analyzer_version: Option<String>,
    /// The analyzer's process exit status, where the caller observed it.
    pub exit_status: i32,
    /// The source identity the run was against. Some identity recipes need it —
    /// `cargo-audit` keys findings by lockfile blob, so a finding stays distinct
    /// when the lockfile changes underneath the same advisory.
    pub source: &'a SourceIdentity,
    /// Digest of the rule set the analyzer ran with, where one applies.
    pub rules_digest: Option<String>,
    /// The pinned advisory database the caller provisioned, where one applies.
    ///
    /// A fallback, not an override: an adapter prefers what the analyzer's own
    /// report says about the database it consulted, and uses this only when the
    /// report says nothing. `cargo audit` says nothing whenever it is pointed at
    /// a database with `--db`, which is every pinned run — so without this, the
    /// reproducible configuration would be the one with no staleness evidence.
    pub advisory_db: Option<rto_graph::AdvisoryDb>,
    /// Where to read the source a finding points at, for identity recipes that
    /// include a snippet hash.
    ///
    /// It is here rather than inside an adapter because the *caller* knows which
    /// checkout the report describes, and because both execution paths must read
    /// the same one — that is what makes a subprocess run and an ingest of its
    /// output produce identical finding keys.
    pub snippets: &'a dyn SnippetSource,
}

// Hand-written because `&dyn SnippetSource` is not `Debug` and does not need to
// be: what a debug print of a context should show is the evidence it carries,
// not the identity of the thing that reads files.
impl std::fmt::Debug for NativeContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeContext")
            .field("started_at", &self.started_at)
            .field("ended_at", &self.ended_at)
            .field("analyzer_version", &self.analyzer_version)
            .field("exit_status", &self.exit_status)
            .field("source", self.source)
            .field("rules_digest", &self.rules_digest)
            .field("advisory_db", &self.advisory_db)
            .finish_non_exhaustive()
    }
}

impl NativeContext<'_> {
    /// The version to record: what the caller learned, else what the report
    /// carried, else [`UNKNOWN_VERSION`].
    ///
    /// Never empty — [`crate::IngestRunner`] refuses a report that cannot say
    /// what version produced it, and "unknown" is a truthful answer where an
    /// empty string is a missing one.
    #[must_use]
    pub fn version_or(&self, from_report: Option<&str>) -> String {
        self.analyzer_version
            .as_deref()
            .or(from_report)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or(UNKNOWN_VERSION)
            .to_owned()
    }
}

/// Recorded as an analyzer's version when neither the caller nor the report
/// knows it — which is the ordinary case for a `cargo audit` report ingested
/// from CI, since its JSON has no version field.
pub const UNKNOWN_VERSION: &str = "unknown";

/// How an analyzer is invoked as a child process.
///
/// Returned by [`Adapter::command`] and consumed by the subprocess runner, so
/// the argv lives beside the parser that understands its output rather than in
/// the runner, which knows no analyzer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// The program to execute, looked up on `PATH` unless it is a path.
    pub program: String,
    /// Its arguments, in order.
    pub args: Vec<String>,
    /// Exit statuses that mean "the analyzer ran and produced a report".
    ///
    /// Analyzers overload the exit status: `semgrep` exits `1` when it found
    /// something, `cargo audit` exits `1` on a vulnerability. Treating non-zero
    /// as failure would discard exactly the runs that matter, so each adapter
    /// declares which statuses carry a usable report and every other status is a
    /// hard failure.
    pub success_statuses: Vec<i32>,
}

/// One analyzer's native output format and invocation.
pub trait Adapter: Sync + std::fmt::Debug {
    /// The analyzer id — the value that appears in every layer key and finding
    /// key this adapter produces.
    fn analyzer(&self) -> &'static str;

    /// A one-line description of what it looks for, for `roteiro security
    /// status` and `--help`.
    fn summary(&self) -> &'static str;

    /// The languages this adapter produces findings for, as the coverage matrix
    /// in ADR-0016 states them. Reported by the CLI so the claim is inspectable
    /// rather than only documented.
    fn languages(&self) -> &'static [&'static str];

    /// Which pinned assets the analyzer needs before it can run offline (see
    /// [`crate::assets`]). An empty slice means it needs none.
    fn asset_ids(&self) -> &'static [&'static str];

    /// The argv that makes the analyzer emit the native format
    /// [`Adapter::normalize`] parses, with egress configured off.
    ///
    /// `assets` maps an id from [`Adapter::asset_ids`] to the verified local
    /// path it was provisioned to.
    fn command(&self, assets: &AssetPaths<'_>) -> Invocation;

    /// Parse native output into a normalized report.
    ///
    /// # Errors
    /// Returns [`ExecError::MalformedReport`] when the bytes are not this
    /// analyzer's format, or [`ExecError::Json`] when they are not JSON at all.
    /// A partially-parsed report is never returned: either the whole thing
    /// converts or the run fails.
    fn normalize(
        &self,
        native: &[u8],
        ctx: &NativeContext<'_>,
    ) -> Result<NormalizedReport, ExecError>;
}

/// Verified local paths of an analyzer's provisioned assets, keyed by asset id.
#[derive(Debug, Clone, Copy, Default)]
pub struct AssetPaths<'a> {
    entries: &'a [(&'a str, std::path::PathBuf)],
}

impl<'a> AssetPaths<'a> {
    /// Wrap a resolved id → path list.
    #[must_use]
    pub fn new(entries: &'a [(&'a str, std::path::PathBuf)]) -> Self {
        Self { entries }
    }

    /// The path provisioned for `id`, or `None` if it was not resolved.
    ///
    /// An adapter that asked for an asset in [`Adapter::asset_ids`] will always
    /// find it here, because the runner refuses to start otherwise — see
    /// [`ExecError::AssetsUnavailableOffline`].
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&std::path::Path> {
        self.entries
            .iter()
            .find(|(key, _)| *key == id)
            .map(|(_, path)| path.as_path())
    }

    /// The path provisioned for `id` as a string, or an empty string. Adapters
    /// build argv from this; an unresolved asset cannot reach here.
    #[must_use]
    pub fn arg(&self, id: &str) -> String {
        self.get(id)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

/// Every analyzer this build knows how to normalise.
///
/// Ingest consults this table, so a report from any of them can be read in from
/// CI whether or not this build can *execute* the analyzer — which is the whole
/// point of ADR-0014's "ingest is always available".
pub static ADAPTERS: &[&dyn Adapter] = &[&semgrep::Semgrep, &cargo_audit::CargoAudit];

/// The adapter for `analyzer`, or `None` if this build has none.
#[must_use]
pub fn adapter_for(analyzer: &str) -> Option<&'static dyn Adapter> {
    ADAPTERS.iter().copied().find(|a| a.analyzer() == analyzer)
}

/// Every analyzer id this build can normalise, sorted — for error messages that
/// tell a caller what it *could* have asked for.
#[must_use]
pub fn known_analyzers() -> Vec<&'static str> {
    let mut ids: Vec<&'static str> = ADAPTERS.iter().map(|a| a.analyzer()).collect();
    ids.sort_unstable();
    ids
}

/// Recorded in place of a snippet hash when the source could not be read — an
/// ingested report about a tree this checkout does not have.
///
/// A named marker rather than a hash of the empty string, so a reader of a
/// finding key can tell "the code was empty" from "the code was unavailable".
pub const NO_SNIPPET: &str = "no-snippet";

/// Short SHA-256 prefix of a snippet, used by identity recipes that need to
/// notice that the *code* at a location changed even though the location did
/// not.
///
/// Sixteen hex characters is 64 bits — far more than enough to keep two
/// snippets at the same rule and offset distinct, and short enough that a
/// rendered key stays readable in a terminal. Leading and trailing whitespace is
/// stripped first, so a reformat that only moved indentation is not a new
/// finding.
#[must_use]
pub fn snippet_hash(snippet: &str) -> String {
    crate::sha256_hex(snippet.trim().as_bytes())[..16].to_owned()
}

/// [`snippet_hash`] of what `snippets` holds for the span, or [`NO_SNIPPET`].
#[must_use]
pub fn snippet_hash_at(snippets: &dyn SnippetSource, path: &str, start: u32, end: u32) -> String {
    snippets
        .snippet(path, start, end)
        .map_or_else(|| NO_SNIPPET.to_owned(), |text| snippet_hash(&text))
}

#[cfg(test)]
mod tests {
    use super::{
        AssetPaths, NO_SNIPPET, NativeContext, UNKNOWN_VERSION, adapter_for, known_analyzers,
        snippet_hash, snippet_hash_at,
    };
    use rto_graph::SourceIdentity;

    fn ctx(version: Option<&str>) -> NativeContext<'static> {
        static SOURCE: std::sync::LazyLock<SourceIdentity> =
            std::sync::LazyLock::new(SourceIdentity::default);
        NativeContext {
            started_at: "2026-08-15T09:00:00Z".to_owned(),
            ended_at: "2026-08-15T09:00:04Z".to_owned(),
            analyzer_version: version.map(str::to_owned),
            exit_status: 0,
            source: &SOURCE,
            rules_digest: None,
            advisory_db: None,
            snippets: &crate::snippet::NoSnippets,
        }
    }

    #[test]
    fn the_registry_answers_for_every_analyzer_it_lists() {
        for id in known_analyzers() {
            assert_eq!(adapter_for(id).expect("registered").analyzer(), id);
        }
        assert!(adapter_for("no-such-analyzer").is_none());
    }

    /// Every shipped adapter claims at least one language and a summary, because
    /// `roteiro security status` prints the coverage matrix from this table —
    /// an adapter that claims nothing would silently shrink the reported
    /// coverage.
    #[test]
    fn every_adapter_states_its_coverage() {
        for id in known_analyzers() {
            let adapter = adapter_for(id).expect("registered");
            assert!(!adapter.languages().is_empty(), "{id} claims no language");
            assert!(!adapter.summary().is_empty(), "{id} has no summary");
        }
    }

    #[test]
    fn a_version_is_taken_from_the_caller_then_the_report_then_unknown() {
        assert_eq!(ctx(Some("1.2.3")).version_or(Some("0.0.1")), "1.2.3");
        assert_eq!(ctx(None).version_or(Some("0.0.1")), "0.0.1");
        assert_eq!(ctx(None).version_or(None), UNKNOWN_VERSION);
        // Whitespace is not a version: an all-blank field would be refused
        // downstream as missing evidence, so it is treated as absent here.
        assert_eq!(ctx(Some("  ")).version_or(None), UNKNOWN_VERSION);
    }

    #[test]
    fn snippet_hashes_are_short_stable_and_whitespace_insensitive() {
        let hash = snippet_hash("eval(user_input)");
        assert_eq!(hash.len(), 16);
        assert_eq!(hash, snippet_hash("  eval(user_input)\n"));
        assert_ne!(hash, snippet_hash("eval(other_input)"));
    }

    /// A report about a tree this checkout does not have still yields a
    /// well-formed identity, and one that says why it is weaker.
    #[test]
    fn an_unavailable_snippet_is_named_not_hashed_as_empty() {
        let hash = snippet_hash_at(&crate::snippet::NoSnippets, "a.py", 0, 4);
        assert_eq!(hash, NO_SNIPPET);
        assert_ne!(hash, snippet_hash(""));
    }

    #[test]
    fn asset_paths_resolve_only_what_was_provisioned() {
        let entries = [("semgrep-rules", std::path::PathBuf::from("/cache/r.yaml"))];
        let paths = AssetPaths::new(&entries);
        assert_eq!(paths.arg("semgrep-rules"), "/cache/r.yaml");
        assert!(paths.get("advisory-db").is_none());
        assert!(paths.arg("advisory-db").is_empty());
    }
}

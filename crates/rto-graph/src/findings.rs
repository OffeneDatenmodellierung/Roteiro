//! Analyzer findings — a separate artifact model, never a graph fact.
//!
//! External analyzers (`cargo-audit`, `semgrep`, and successors) assert results
//! at a point in time, against a rule set and an advisory database that both
//! change independently of the source tree. That is a fourth production model,
//! not one of the graph's three provenance classes, so the results live here — in
//! their own tables, with their own retrieval surface — and never in
//! `nodes`/`edges` (ADR-0012).
//!
//! Two consequences are load-bearing, and both are asserted by tests rather than
//! assumed:
//!
//! - [`crate::Store::export_factset`] — and therefore the published
//!   [`crate::GraphArtifact`] — stays a pure function of the tree, because nothing
//!   in this module writes a node or an edge.
//! - No finding acquires the `authored` relevance boost that [`crate::search`]
//!   applies, because `search` ranks `nodes` and a finding is not one.
//!
//! Nothing here adds a [`crate::Provenance`] variant, and nothing here is
//! extraction output, so `EXTRACT_VERSION` is untouched.
//!
//! # The replaceable layer
//!
//! Findings form a layer keyed `security:<analyzer>:<worktree-id>` (see
//! [`layer_key`]). Exactly one [`AnalysisRun`] is live per layer: a successful
//! re-ingest replaces the previous one **wholesale, including the rows it owned**,
//! so a finding that has been fixed disappears rather than lingering. The
//! established import path (`Store::apply_import_layer`) deletes a layer's edges
//! but *not* its obsolete owned nodes; that gap is deliberately not inherited —
//! see [`crate::Store::replace_findings_layer`].
//!
//! @rto:0012

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::model::Span;
use crate::store::StoreError;

/// The layer-key prefix under which every findings layer is filed.
pub const SECURITY_LAYER_PREFIX: &str = "security";

/// The prefix of every [`FindingKey`].
pub const FINDING_KEY_PREFIX: &str = "finding";

/// Longest permitted identity component, in bytes. Identity parts are rule ids,
/// paths, offsets and digests; anything longer is a malformed or hostile report,
/// not a finding, and is refused before it can bloat the store.
pub const MAX_IDENTITY_PART: usize = 512;

/// Errors raised when constructing the identity values this store is keyed by.
///
/// These are *validation* errors, raised by the constructors, so the store never
/// has to accept an ill-formed key: by the time a value reaches
/// [`crate::Store::replace_findings_layer`] it is already well-formed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FindingsError {
    /// An analyzer id was empty or contained characters outside
    /// `[a-z0-9]` plus `.`, `_` and `-`.
    #[error("invalid analyzer id: {0:?} (expected lowercase [a-z0-9._-], non-empty)")]
    InvalidAnalyzerId(String),
    /// A worktree id was empty or contained characters outside `[a-z0-9-]`.
    #[error("invalid worktree id: {0:?} (expected lowercase [a-z0-9-], 1..=64 chars)")]
    InvalidWorktreeId(String),
    /// A finding was offered with no identity components at all, so it would have
    /// no stable identity across runs.
    #[error("finding identity is empty: a finding needs at least one identity component")]
    EmptyIdentity,
    /// An identity component was empty, over-long, or contained a control
    /// character.
    #[error("invalid finding identity component {0:?}")]
    InvalidIdentityPart(String),
    /// A rendered finding key could not be parsed back (a corrupt row, or a key
    /// produced by something other than [`FindingKey`]).
    #[error("malformed finding key: {0}")]
    MalformedKey(String),
}

/// Which backend produced an [`AnalysisRun`].
///
/// All three names from ADR-0014 exist from the start so the stored token set is
/// stable; only [`RunnerKind::Ingested`] is produced by this crate today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunnerKind {
    /// A normalized report produced elsewhere (CI, a developer's own tooling)
    /// and read in by `roteiro security ingest`.
    Ingested,
    /// The analyzer was executed as a child process on the host.
    Subprocess,
    /// The analyzer was executed inside a sandbox (a pinned OCI image in a
    /// microVM).
    Sandboxed,
}

impl RunnerKind {
    /// Stable string token used in the `SQLite` store.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ingested => "ingested",
            Self::Subprocess => "subprocess",
            Self::Sandboxed => "sandboxed",
        }
    }

    /// Parse a runner kind from its stable token, `None` for an unrecognised
    /// value (a corrupt row).
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        match s {
            "ingested" => Some(Self::Ingested),
            "subprocess" => Some(Self::Subprocess),
            "sandboxed" => Some(Self::Sandboxed),
            _ => None,
        }
    }
}

/// The isolation boundary a run actually had — recorded honestly, so a result
/// produced with no boundary can never read as if it had one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Isolation {
    /// No local execution happened at all: the report was produced elsewhere.
    Ingested,
    /// A microVM around a pinned OCI image.
    #[serde(rename = "microvm")]
    MicroVm,
    /// None — the analyzer ran directly on the host.
    None,
}

impl Isolation {
    /// Stable string token used in the `SQLite` store.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ingested => "ingested",
            Self::MicroVm => "microvm",
            Self::None => "none",
        }
    }

    /// Parse an isolation label from its stable token, `None` for an
    /// unrecognised value (a corrupt row).
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        match s {
            "ingested" => Some(Self::Ingested),
            "microvm" => Some(Self::MicroVm),
            "none" => Some(Self::None),
            _ => None,
        }
    }
}

/// What network access a run was permitted.
///
/// `Deny` is the only policy today and the only one any shipped runner requests.
/// Marked `#[non_exhaustive]` because a later backend may need an explicit
/// allow-list for an advisory-database refresh, and that must not be a breaking
/// change; match with equality rather than exhaustively.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum NetworkPolicy {
    /// No egress. Analyzer inputs are pre-provisioned, never fetched mid-run.
    #[default]
    Deny,
}

/// How the analyzed worktree was exposed to the analyzer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeAccess {
    /// Read-only: analyzers parse source, manifests and lockfiles; none of them
    /// needs to write to the tree.
    #[default]
    ReadOnly,
    /// Writable — recorded for honesty if a future analyzer ever needs it.
    ReadWrite,
}

/// How the analyzer's process environment was prepared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentPolicy {
    /// Scrubbed: no ambient credentials are passed through.
    #[default]
    Scrubbed,
    /// The ambient environment was inherited as-is.
    Inherited,
}

/// The command policy a run was executed under. Part of the evidence chain: it
/// records what the run was *allowed* to do, not merely what it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CommandPolicy {
    /// Egress policy.
    pub network: NetworkPolicy,
    /// How the worktree was mounted.
    pub worktree: WorktreeAccess,
    /// How the process environment was prepared.
    pub environment: EnvironmentPolicy,
}

/// The pinned advisory database a run consulted, and when it was published.
///
/// `published_at` exists so a result can be labelled *possibly stale* rather than
/// *current*: re-running the same analyzer at the same commit with a newer
/// advisory database legitimately yields a different answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdvisoryDb {
    /// Digest of the advisory database as consulted.
    pub digest: String,
    /// Publication timestamp of that database, if the producer recorded one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
}

/// The source identity a run was executed against.
///
/// All three components are optional because different analyzers pin different
/// things: `semgrep` is meaningful against a commit/tree, `cargo-audit` against a
/// lockfile blob.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceIdentity {
    /// Hex commit id the analyzer ran against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// Hex tree id the analyzer ran against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree: Option<String>,
    /// Hex blob id of the lockfile the analyzer resolved dependencies from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lockfile_blob: Option<String>,
}

/// The severity an analyzer assigned to a finding.
///
/// This is a **tool judgement**, deliberately kept away from the graph: it is not
/// the confidence score `inferred` edges carry, and it must never be read as one.
/// Known levels have stable tokens; anything else round-trips verbatim through
/// [`Severity::Other`], so a new analyzer's vocabulary is not lost.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// The analyzer's highest level.
    Critical,
    /// High.
    High,
    /// Medium / moderate.
    Medium,
    /// Low / minor.
    Low,
    /// Informational — not a defect claim.
    Info,
    /// Any level not covered above, kept verbatim.
    Other(String),
}

impl Severity {
    /// Stable string token used in the `SQLite` store.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Critical => "critical",
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
            Self::Info => "info",
            Self::Other(s) => s,
        }
    }

    /// Parse a severity from its token. Unknown tokens become
    /// [`Severity::Other`], so this is infallible.
    #[must_use]
    pub fn from_token(s: &str) -> Self {
        match s {
            "critical" => Self::Critical,
            "high" => Self::High,
            "medium" => Self::Medium,
            "low" => Self::Low,
            "info" => Self::Info,
            other => Self::Other(other.to_owned()),
        }
    }
}

// Severity (de)serializes as its bare token so reports, the database and `--json`
// output all agree on one representation.
impl Serialize for Severity {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Severity {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(Self::from_token(&s))
    }
}

/// An identifier for one checkout, used as the last component of a layer key.
///
/// It is an opaque token rather than a path on purpose: a layer key is stored and
/// printed, and a local filesystem path is user-identifying data that has no
/// business in a record. Producers derive it however they like (a digest of the
/// canonical worktree path is the obvious choice); this type only guarantees the
/// token is well-formed, so a layer key can never be ambiguous.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct WorktreeId(String);

impl WorktreeId {
    /// Validate and wrap a worktree id: 1..=64 characters of `[a-z0-9-]`.
    ///
    /// # Errors
    /// Returns [`FindingsError::InvalidWorktreeId`] if `raw` is empty, too long,
    /// or contains anything else.
    pub fn new(raw: &str) -> Result<Self, FindingsError> {
        let ok = !raw.is_empty()
            && raw.len() <= 64
            && raw
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if ok {
            Ok(Self(raw.to_owned()))
        } else {
            Err(FindingsError::InvalidWorktreeId(raw.to_owned()))
        }
    }

    /// The token.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for WorktreeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Whether `id` is a well-formed analyzer id: non-empty, and lowercase
/// `[a-z0-9]` plus `.`, `_`, `-`.
#[must_use]
pub fn is_valid_analyzer_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

/// Render the layer key a findings layer is filed under:
/// `security:<analyzer>:<worktree-id>`.
///
/// A successful re-ingest under the same key replaces the previous layer
/// wholesale (see [`crate::Store::replace_findings_layer`]).
///
/// # Errors
/// Returns [`FindingsError::InvalidAnalyzerId`] if `analyzer` is not a
/// well-formed analyzer id.
pub fn layer_key(analyzer: &str, worktree: &WorktreeId) -> Result<String, FindingsError> {
    if !is_valid_analyzer_id(analyzer) {
        return Err(FindingsError::InvalidAnalyzerId(analyzer.to_owned()));
    }
    Ok(format!(
        "{SECURITY_LAYER_PREFIX}:{analyzer}:{}",
        worktree.as_str()
    ))
}

/// A finding's stable identity across runs.
///
/// The key is `finding:<analyzer>:<component>…` — the analyzer id followed by
/// **that analyzer's own** ordered identity components. The schema deliberately
/// does not know what those components mean, so a new analyzer is a new recipe in
/// its adapter rather than a schema change:
///
/// ```text
/// finding:semgrep:<rule>:<path>:<start-byte>:<snippet-hash>
/// finding:cargo-audit:<advisory>:<pkg>:<version>:<lockfile-blob>
/// ```
///
/// Components may themselves contain `:` (a rule id with a namespace, a Windows
/// path), so on rendering `\` and `:` are escaped with a backslash. Parsing
/// reverses that exactly, which makes the rendering injective: two different
/// component lists can never collide on one key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FindingKey {
    analyzer: String,
    parts: Vec<String>,
}

impl FindingKey {
    /// Build a key from an analyzer id and its ordered identity components.
    ///
    /// # Errors
    /// Returns [`FindingsError::InvalidAnalyzerId`] for a malformed analyzer id,
    /// [`FindingsError::EmptyIdentity`] if `parts` is empty, or
    /// [`FindingsError::InvalidIdentityPart`] if a component is empty, longer
    /// than [`MAX_IDENTITY_PART`], or contains a control character.
    pub fn new<S: AsRef<str>>(analyzer: &str, parts: &[S]) -> Result<Self, FindingsError> {
        if !is_valid_analyzer_id(analyzer) {
            return Err(FindingsError::InvalidAnalyzerId(analyzer.to_owned()));
        }
        if parts.is_empty() {
            return Err(FindingsError::EmptyIdentity);
        }
        let mut owned = Vec::with_capacity(parts.len());
        for part in parts {
            let part = part.as_ref();
            if part.is_empty()
                || part.len() > MAX_IDENTITY_PART
                || part.chars().any(char::is_control)
            {
                return Err(FindingsError::InvalidIdentityPart(part.to_owned()));
            }
            owned.push(part.to_owned());
        }
        Ok(Self {
            analyzer: analyzer.to_owned(),
            parts: owned,
        })
    }

    /// The analyzer that produced the finding.
    #[must_use]
    pub fn analyzer(&self) -> &str {
        &self.analyzer
    }

    /// The analyzer-specific identity components, in order.
    #[must_use]
    pub fn parts(&self) -> &[String] {
        &self.parts
    }

    /// Render the key to its stable string form.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::from(FINDING_KEY_PREFIX);
        out.push(':');
        push_escaped(&mut out, &self.analyzer);
        for part in &self.parts {
            out.push(':');
            push_escaped(&mut out, part);
        }
        out
    }

    /// Parse a rendered key back into its components — the exact inverse of
    /// [`FindingKey::render`].
    ///
    /// # Errors
    /// Returns [`FindingsError::MalformedKey`] if the prefix is wrong, an escape
    /// is dangling, or there is no identity component; or the same validation
    /// errors [`FindingKey::new`] raises.
    pub fn parse(rendered: &str) -> Result<Self, FindingsError> {
        let segments = split_escaped(rendered)?;
        let mut it = segments.into_iter();
        match it.next() {
            Some(prefix) if prefix == FINDING_KEY_PREFIX => {}
            _ => {
                return Err(FindingsError::MalformedKey(format!(
                    "{rendered:?} does not start with `{FINDING_KEY_PREFIX}:`"
                )));
            }
        }
        let analyzer = it.next().ok_or_else(|| {
            FindingsError::MalformedKey(format!("{rendered:?} names no analyzer"))
        })?;
        let parts: Vec<String> = it.collect();
        if parts.is_empty() {
            return Err(FindingsError::MalformedKey(format!(
                "{rendered:?} carries no identity component"
            )));
        }
        Self::new(&analyzer, &parts)
    }
}

impl std::fmt::Display for FindingKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.render())
    }
}

// A key (de)serializes as its rendered string, so a report, a database row and
// `--json` output all show the same identity.
impl Serialize for FindingKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.render())
    }
}

impl<'de> Deserialize<'de> for FindingKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

/// Append `raw` to `out`, escaping the two characters that would otherwise make a
/// rendered key ambiguous.
fn push_escaped(out: &mut String, raw: &str) {
    for ch in raw.chars() {
        if ch == '\\' || ch == ':' {
            out.push('\\');
        }
        out.push(ch);
    }
}

/// Split on unescaped `:`, unescaping as it goes.
fn split_escaped(rendered: &str) -> Result<Vec<String>, FindingsError> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut chars = rendered.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => match chars.next() {
                Some(escaped) => current.push(escaped),
                None => {
                    return Err(FindingsError::MalformedKey(format!(
                        "{rendered:?} ends in a dangling escape"
                    )));
                }
            },
            ':' => out.push(std::mem::take(&mut current)),
            other => current.push(other),
        }
    }
    out.push(current);
    Ok(out)
}

/// One analyzer execution, plus everything needed to reproduce or distrust it.
///
/// This is the evidence chain graph provenance was never designed to hold: what
/// ran, at what version, under what isolation and command policy, against which
/// rules and advisory database, over which source identity, and with what result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisRun {
    /// The replaceable layer this run owns: `security:<analyzer>:<worktree-id>`
    /// (see [`layer_key`]). Unique — one live run per layer.
    pub layer: String,
    /// The analyzer id (`cargo-audit`, `semgrep`, …).
    pub analyzer: String,
    /// The analyzer's own version string, as reported by the producer.
    pub analyzer_version: String,
    /// Which backend produced this run.
    pub runner: RunnerKind,
    /// The isolation boundary the run actually had.
    pub isolation: Isolation,
    /// Digest of the container image, when one was used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_digest: Option<String>,
    /// Digest of the rule set the analyzer was run with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules_digest: Option<String>,
    /// The pinned advisory database consulted, and its publication date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advisory_db: Option<AdvisoryDb>,
    /// What the run was permitted to do.
    pub command_policy: CommandPolicy,
    /// The source identity the run was executed against.
    pub source: SourceIdentity,
    /// Producer-supplied start timestamp.
    pub started_at: String,
    /// Producer-supplied end timestamp.
    pub ended_at: String,
    /// The analyzer's process exit status.
    pub exit_status: i32,
    /// Digest of the raw report this run was derived from — the tie between the
    /// stored findings and the exact bytes they came from.
    pub report_digest: String,
}

/// One finding, belonging to an [`AnalysisRun`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// Stable identity across runs.
    pub key: FindingKey,
    /// The rule, advisory or check id the analyzer fired.
    pub rule: String,
    /// The severity the analyzer assigned — a tool judgement, not a confidence.
    pub severity: Severity,
    /// One-line summary.
    pub title: String,
    /// The analyzer's full message.
    pub message: String,
    /// Repository-relative path the finding is about, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Byte span within that path, if the analyzer located one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<Span>,
    /// Anything else the analyzer reported, kept verbatim.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub meta: serde_json::Value,
}

/// A live layer: its run and the findings that run owns, ordered by key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingsLayer {
    /// The run that owns this layer.
    pub run: AnalysisRun,
    /// Its findings, ordered by [`FindingKey`].
    pub findings: Vec<Finding>,
}

/// A summary of replacing a findings layer (see
/// [`crate::Store::replace_findings_layer`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingsApplied {
    /// The layer key written.
    pub layer: String,
    /// Findings written by this ingest.
    pub findings: usize,
    /// Owned finding rows deleted from the previous run of this layer. This is
    /// the number that proves obsolete records are removed rather than orphaned.
    pub removed: usize,
    /// Whether a previous run of this layer existed and was replaced.
    pub replaced: bool,
}

// --- Persistence. Free helpers over a `Connection` (a `Transaction` derefs to
// one), mirroring how the node/edge store is written. Every one of these touches
// `analysis_runs`/`findings` and nothing else: no statement in this module reads
// or writes `nodes` or `edges`. ---

/// Columns of `analysis_runs`, in the order [`run_from_row`] decodes them.
const RUN_COLS: &str = "r.id, r.layer, r.analyzer, r.analyzer_version, r.runner, r.isolation, \
     r.image_digest, r.rules_digest, r.advisory_db_digest, r.advisory_db_published_at, \
     r.command_policy, r.source_commit, r.source_tree, r.source_lockfile_blob, \
     r.started_at, r.ended_at, r.exit_status, r.report_digest";

/// Columns of `findings`, in the order [`finding_from_row`] decodes them.
const FINDING_COLS: &str = "f.key, f.rule, f.severity, f.title, f.message, f.path, \
     f.span_start, f.span_end, f.meta";

/// Replace this layer's live run and all the finding rows it owns, in one
/// transaction. See [`crate::Store::replace_findings_layer`] for the contract.
pub(crate) fn replace_layer(
    conn: &Connection,
    run: &AnalysisRun,
    findings: &[Finding],
) -> Result<FindingsApplied, StoreError> {
    let previous: Option<i64> = conn
        .query_row(
            "SELECT id FROM analysis_runs WHERE layer = ?1",
            [&run.layer],
            |r| r.get(0),
        )
        .optional()?;

    // Owned-record cleanup, done explicitly. The `ON DELETE CASCADE` on
    // `findings.run_id` would also remove these rows, but relying on it would
    // repeat the mistake this store exists to avoid: the import path deletes a
    // layer's edges and leaves its obsolete nodes behind. Deleting the owned rows
    // by hand — and reporting how many — is what makes "a fixed finding
    // disappears" a tested fact rather than a hope.
    let mut removed = 0usize;
    if let Some(id) = previous {
        removed = conn.execute("DELETE FROM findings WHERE run_id = ?1", [id])?;
        conn.execute("DELETE FROM analysis_runs WHERE id = ?1", [id])?;
    }

    let policy = serde_json::to_string(&run.command_policy)?;
    let (advisory_digest, advisory_published) = match &run.advisory_db {
        Some(db) => (Some(db.digest.as_str()), db.published_at.as_deref()),
        None => (None, None),
    };
    conn.execute(
        "INSERT INTO analysis_runs (
             layer, analyzer, analyzer_version, runner, isolation, image_digest,
             rules_digest, advisory_db_digest, advisory_db_published_at, command_policy,
             source_commit, source_tree, source_lockfile_blob, started_at, ended_at,
             exit_status, report_digest
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        params![
            run.layer,
            run.analyzer,
            run.analyzer_version,
            run.runner.as_str(),
            run.isolation.as_str(),
            run.image_digest,
            run.rules_digest,
            advisory_digest,
            advisory_published,
            policy,
            run.source.commit,
            run.source.tree,
            run.source.lockfile_blob,
            run.started_at,
            run.ended_at,
            run.exit_status,
            run.report_digest,
        ],
    )?;
    let run_id = conn.last_insert_rowid();

    for finding in findings {
        let span = finding.span.map(|s| (s.start, s.end));
        conn.execute(
            "INSERT INTO findings (
                 run_id, key, rule, severity, title, message, path, span_start, span_end, meta
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                run_id,
                finding.key.render(),
                finding.rule,
                finding.severity.as_str(),
                finding.title,
                finding.message,
                finding.path,
                span.map(|(start, _)| start),
                span.map(|(_, end)| end),
                serde_json::to_string(&finding.meta)?,
            ],
        )?;
    }

    Ok(FindingsApplied {
        layer: run.layer.clone(),
        findings: findings.len(),
        removed,
        replaced: previous.is_some(),
    })
}

/// Delete a layer — its run and every finding row it owns — returning how many
/// findings went with it, or `None` if no such layer was live.
pub(crate) fn delete_layer(conn: &Connection, layer: &str) -> Result<Option<usize>, StoreError> {
    let Some(id): Option<i64> = conn
        .query_row(
            "SELECT id FROM analysis_runs WHERE layer = ?1",
            [layer],
            |r| r.get(0),
        )
        .optional()?
    else {
        return Ok(None);
    };
    // Explicit owned-record cleanup, for the same reason as in `replace_layer`.
    let removed = conn.execute("DELETE FROM findings WHERE run_id = ?1", [id])?;
    conn.execute("DELETE FROM analysis_runs WHERE id = ?1", [id])?;
    Ok(Some(removed))
}

/// Every live layer, ordered by layer key; optionally narrowed to one analyzer.
pub(crate) fn layers(
    conn: &Connection,
    analyzer: Option<&str>,
) -> Result<Vec<FindingsLayer>, StoreError> {
    let (sql, bound): (String, Vec<&str>) = match analyzer {
        Some(a) => (
            format!(
                "SELECT {RUN_COLS} FROM analysis_runs r WHERE r.analyzer = ?1 ORDER BY r.layer"
            ),
            vec![a],
        ),
        None => (
            format!("SELECT {RUN_COLS} FROM analysis_runs r ORDER BY r.layer"),
            Vec::new(),
        ),
    };
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(rusqlite::params_from_iter(bound))?;
    let mut runs = Vec::new();
    while let Some(row) = rows.next()? {
        runs.push(run_from_row(row)?);
    }
    let mut out = Vec::with_capacity(runs.len());
    for (id, run) in runs {
        out.push(FindingsLayer {
            findings: findings_for_run(conn, id)?,
            run,
        });
    }
    Ok(out)
}

/// The findings owned by one run, ordered by key so output is deterministic.
fn findings_for_run(conn: &Connection, run_id: i64) -> Result<Vec<Finding>, StoreError> {
    let sql = format!("SELECT {FINDING_COLS} FROM findings f WHERE f.run_id = ?1 ORDER BY f.key");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query([run_id])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(finding_from_row(row)?);
    }
    Ok(out)
}

/// Total number of stored findings, across every layer.
pub(crate) fn count_findings(conn: &Connection) -> Result<u64, StoreError> {
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM findings", [], |r| r.get(0))?;
    Ok(u64::try_from(n).unwrap_or(0))
}

/// Total number of live analysis runs (one per layer).
pub(crate) fn count_runs(conn: &Connection) -> Result<u64, StoreError> {
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM analysis_runs", [], |r| r.get(0))?;
    Ok(u64::try_from(n).unwrap_or(0))
}

/// Findings whose owning run no longer exists. Always zero in a healthy store —
/// the layer-replacement tests assert exactly that, so an orphan can never pass
/// for a clean replacement.
pub(crate) fn count_orphan_findings(conn: &Connection) -> Result<u64, StoreError> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM findings f
         WHERE NOT EXISTS (SELECT 1 FROM analysis_runs r WHERE r.id = f.run_id)",
        [],
        |r| r.get(0),
    )?;
    Ok(u64::try_from(n).unwrap_or(0))
}

/// Decode an `analysis_runs` row into `(row id, run)`.
fn run_from_row(row: &rusqlite::Row<'_>) -> Result<(i64, AnalysisRun), StoreError> {
    let id: i64 = row.get(0)?;
    let runner_token: String = row.get(4)?;
    let runner = RunnerKind::from_token(&runner_token)
        .ok_or_else(|| StoreError::Corrupt(format!("unknown runner kind: {runner_token}")))?;
    let isolation_token: String = row.get(5)?;
    let isolation = Isolation::from_token(&isolation_token)
        .ok_or_else(|| StoreError::Corrupt(format!("unknown isolation: {isolation_token}")))?;
    let advisory_digest: Option<String> = row.get(8)?;
    let advisory_published: Option<String> = row.get(9)?;
    let policy_json: String = row.get(10)?;
    let run = AnalysisRun {
        layer: row.get(1)?,
        analyzer: row.get(2)?,
        analyzer_version: row.get(3)?,
        runner,
        isolation,
        image_digest: row.get(6)?,
        rules_digest: row.get(7)?,
        advisory_db: advisory_digest.map(|digest| AdvisoryDb {
            digest,
            published_at: advisory_published,
        }),
        command_policy: serde_json::from_str(&policy_json)?,
        source: SourceIdentity {
            commit: row.get(11)?,
            tree: row.get(12)?,
            lockfile_blob: row.get(13)?,
        },
        started_at: row.get(14)?,
        ended_at: row.get(15)?,
        exit_status: row.get(16)?,
        report_digest: row.get(17)?,
    };
    Ok((id, run))
}

/// Decode a `findings` row.
fn finding_from_row(row: &rusqlite::Row<'_>) -> Result<Finding, StoreError> {
    let key_text: String = row.get(0)?;
    let key = FindingKey::parse(&key_text)
        .map_err(|e| StoreError::Corrupt(format!("stored finding key: {e}")))?;
    let severity_token: String = row.get(2)?;
    let span_start: Option<u32> = row.get(6)?;
    let span_end: Option<u32> = row.get(7)?;
    let meta_json: String = row.get(8)?;
    Ok(Finding {
        key,
        rule: row.get(1)?,
        severity: Severity::from_token(&severity_token),
        title: row.get(3)?,
        message: row.get(4)?,
        path: row.get(5)?,
        span: match (span_start, span_end) {
            (Some(start), Some(end)) => Some(Span::new(start, end)),
            // The migration's CHECK makes a half-span impossible; treat one as
            // "no span" rather than failing a read on a database that cannot
            // produce it.
            _ => None,
        },
        meta: serde_json::from_str(&meta_json)?,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        AdvisoryDb, CommandPolicy, EnvironmentPolicy, FindingKey, FindingsError, Isolation,
        MAX_IDENTITY_PART, NetworkPolicy, RunnerKind, Severity, WorktreeAccess, WorktreeId,
        is_valid_analyzer_id, layer_key,
    };

    #[test]
    fn renders_the_documented_analyzer_keys() {
        let semgrep = FindingKey::new(
            "semgrep",
            &["rules.rust.unsafe", "src/lib.rs", "1024", "9f8e7d"],
        )
        .expect("key");
        assert_eq!(
            semgrep.render(),
            "finding:semgrep:rules.rust.unsafe:src/lib.rs:1024:9f8e7d"
        );

        let audit = FindingKey::new(
            "cargo-audit",
            &["RUSTSEC-2024-0001", "openssl", "0.10.5", "abc123"],
        )
        .expect("key");
        assert_eq!(
            audit.render(),
            "finding:cargo-audit:RUSTSEC-2024-0001:openssl:0.10.5:abc123"
        );
    }

    #[test]
    fn key_round_trips_including_components_containing_colons() {
        // A namespaced rule id and a drive-letter path both contain `:`; the key
        // must still parse back to exactly the components it was built from,
        // otherwise two different findings could collide on one identity.
        let key = FindingKey::new("semgrep", &["a:b", "C:\\src\\x.rs", "7", "deadbeef"])
            .expect("build key");
        let rendered = key.render();
        assert_eq!(FindingKey::parse(&rendered).expect("parse"), key);
        assert_eq!(key.analyzer(), "semgrep");
        assert_eq!(key.parts().len(), 4);

        // Distinct component lists that would collide under naive joining do not.
        let a = FindingKey::new("semgrep", &["x:y", "z"]).expect("a");
        let b = FindingKey::new("semgrep", &["x", "y:z"]).expect("b");
        assert_ne!(a.render(), b.render());
    }

    #[test]
    fn key_rejects_ill_formed_identities() {
        assert_eq!(
            FindingKey::new("Semgrep", &["x"]),
            Err(FindingsError::InvalidAnalyzerId("Semgrep".to_owned()))
        );
        let empty: [&str; 0] = [];
        assert_eq!(
            FindingKey::new("semgrep", &empty),
            Err(FindingsError::EmptyIdentity)
        );
        assert_eq!(
            FindingKey::new("semgrep", &[""]),
            Err(FindingsError::InvalidIdentityPart(String::new()))
        );
        let long = "x".repeat(MAX_IDENTITY_PART + 1);
        assert!(matches!(
            FindingKey::new("semgrep", &[long.as_str()]),
            Err(FindingsError::InvalidIdentityPart(_))
        ));
        assert!(matches!(
            FindingKey::new("semgrep", &["a\nb"]),
            Err(FindingsError::InvalidIdentityPart(_))
        ));
    }

    #[test]
    fn key_parse_rejects_malformed_strings() {
        for bad in [
            "notafinding:semgrep:x",
            "finding:semgrep",
            "finding",
            "finding:semgrep:x\\",
        ] {
            assert!(
                matches!(FindingKey::parse(bad), Err(FindingsError::MalformedKey(_))),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn key_serializes_as_its_rendered_string() {
        let key = FindingKey::new("semgrep", &["r", "p", "1", "h"]).expect("key");
        let json = serde_json::to_string(&key).expect("serialize");
        assert_eq!(json, "\"finding:semgrep:r:p:1:h\"");
        let back: FindingKey = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, key);
        assert!(serde_json::from_str::<FindingKey>("\"nope\"").is_err());
    }

    #[test]
    fn layer_keys_are_analyzer_and_worktree_scoped() {
        let wt = WorktreeId::new("ab12cd34").expect("worktree id");
        assert_eq!(
            layer_key("cargo-audit", &wt).expect("layer"),
            "security:cargo-audit:ab12cd34"
        );
        assert!(matches!(
            layer_key("Cargo Audit", &wt),
            Err(FindingsError::InvalidAnalyzerId(_))
        ));
    }

    #[test]
    fn worktree_ids_are_validated() {
        assert_eq!(WorktreeId::new("a1-b2").expect("ok").as_str(), "a1-b2");
        for bad in ["", "Upper", "has space", &"x".repeat(65)] {
            assert!(
                matches!(
                    WorktreeId::new(bad),
                    Err(FindingsError::InvalidWorktreeId(_))
                ),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn analyzer_ids_accept_the_real_tool_names_and_reject_separators() {
        assert!(is_valid_analyzer_id("cargo-audit"));
        assert!(is_valid_analyzer_id("semgrep"));
        assert!(is_valid_analyzer_id("trivy.fs"));
        assert!(!is_valid_analyzer_id(""));
        // A `:` in an analyzer id would make a layer key ambiguous.
        assert!(!is_valid_analyzer_id("a:b"));
    }

    #[test]
    fn stable_tokens_round_trip() {
        for r in [
            RunnerKind::Ingested,
            RunnerKind::Subprocess,
            RunnerKind::Sandboxed,
        ] {
            assert_eq!(RunnerKind::from_token(r.as_str()), Some(r));
        }
        assert_eq!(RunnerKind::from_token("nope"), None);

        for i in [Isolation::Ingested, Isolation::MicroVm, Isolation::None] {
            assert_eq!(Isolation::from_token(i.as_str()), Some(i));
        }
        assert_eq!(Isolation::from_token("nope"), None);

        for s in [
            Severity::Critical,
            Severity::High,
            Severity::Medium,
            Severity::Low,
            Severity::Info,
        ] {
            assert_eq!(Severity::from_token(s.as_str()), s);
        }
        // An unknown level is preserved, never coerced into a known one.
        assert_eq!(
            Severity::from_token("moderate"),
            Severity::Other("moderate".to_owned())
        );
    }

    #[test]
    fn the_default_command_policy_is_the_locked_down_one() {
        let policy = CommandPolicy::default();
        assert_eq!(policy.network, NetworkPolicy::Deny);
        assert_eq!(policy.worktree, WorktreeAccess::ReadOnly);
        assert_eq!(policy.environment, EnvironmentPolicy::Scrubbed);
        // It survives the JSON round-trip it is stored as.
        let json = serde_json::to_string(&policy).expect("serialize");
        assert_eq!(
            serde_json::from_str::<CommandPolicy>(&json).expect("deserialize"),
            policy
        );
    }

    #[test]
    fn advisory_db_publication_date_is_optional_but_preserved() {
        let db = AdvisoryDb {
            digest: "abc".to_owned(),
            published_at: Some("2026-08-01T00:00:00Z".to_owned()),
        };
        let json = serde_json::to_string(&db).expect("serialize");
        assert_eq!(
            serde_json::from_str::<AdvisoryDb>(&json).expect("deserialize"),
            db
        );
        let bare: AdvisoryDb = serde_json::from_str(r#"{"digest":"abc"}"#).expect("bare");
        assert_eq!(bare.published_at, None);
    }
}

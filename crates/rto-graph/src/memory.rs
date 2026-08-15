//! Episodic agent memory — a separate artifact store, never a graph fact.
//!
//! What a session *learned* — a lesson, an approach that was tried and failed, a
//! decision, a recurring failure pattern, a task outcome — has **no generating
//! function**. Re-run extraction over the same tree a thousand times and none of
//! it comes back, because it was never in the tree: it is the residue of work,
//! not a property of source. So it is not a `derived` fact. It was also not
//! deliberately written into a reviewed file, so it is not `authored` either
//! (ADR-0013, issue #288).
//!
//! It lives here instead: its own table, its own retrieval surface, and never in
//! `nodes`/`edges`. Three consequences are load-bearing, and all three are
//! asserted by tests rather than assumed:
//!
//! - [`crate::Store::export_factset`] — and therefore the published
//!   [`crate::GraphArtifact`] — stays a pure function of the tree **across every
//!   memory write**, because nothing in this module writes a node or an edge.
//! - No record acquires the `authored` relevance boost that [`crate::search`]
//!   applies. At this stage the guarantee is structural and total: memory does
//!   not enter [`crate::search`] **at all**, through any channel.
//! - Records survive [`crate::Store::rebuild`], following the `imports`
//!   precedent — `rebuild` deletes only `edges` and `nodes`, and what cannot be
//!   re-derived must not be destroyed by a re-derivation.
//!
//! Nothing here adds a [`crate::Provenance`] variant, and `EXTRACT_VERSION` does
//! not change: memory is not extraction output.
//!
//! # This is the episodic tier only
//!
//! ADR-0013 describes two tiers with **opposite rules**, because they have
//! opposite recovery costs: re-derivable knowledge is evictable, episodic
//! knowledge is not. This module is the episodic half — **unbounded, never
//! auto-evicted, removed only by an explicit [`crate::Store::forget_memory`]**.
//! Bounding it would be data loss, not cache management. The bounded cache tier
//! is a separate table with its own eviction policy and is not implemented here.
//!
//! # Anchoring: a node key and a blob, never a span
//!
//! A record may anchor to a point in the graph. The anchor is the pair
//! `(anchor_key, anchor_blob)`, captured when the record is written, and a span
//! is deliberately **not** part of it: a span is byte offsets and shifts on any
//! edit above it, so a record anchored by span would read as stale after an
//! unrelated import was added twenty lines up. A node key plus the blob hash the
//! node carried at capture time is stable under that edit and moves only when the
//! thing itself moves.
//!
//! On read — never on write, and never stored — the pair is checked against the
//! current graph, yielding an [`AnchorState`]:
//!
//! | Recorded | In the graph now | State |
//! |---|---|---|
//! | no anchor | — | [`AnchorState::Unanchored`] |
//! | a key | no such node | [`AnchorState::Vanished`] |
//! | a key + blob | node present, same blob | [`AnchorState::Valid`] |
//! | a key + blob | node present, different blob | [`AnchorState::Drifted`] |
//! | a key, no blob | node present | [`AnchorState::Unverifiable`] |
//!
//! **Drift marks; it never prunes.** The authored layer drops links to vanished
//! symbols; memory must not, because *a lesson about deleted code is often the
//! most valuable thing in the store* — "we removed this because the retry loop
//! double-counted" is worth more once the retry loop is gone, not less. This is a
//! deliberate departure from the house pruning rule, and it is the main reason
//! memory cannot live in the graph.
//!
//! # The anchor is the scope test
//!
//! The store is shared across branches, worktrees and clones, so the obvious
//! question is whether a lesson learned on a feature branch is valid on `main`.
//! The rule (ADR-0013 §*Scope*) is:
//!
//! > A lesson learned on a feature branch is valid on `main` **only if the
//! > relevant association is merged to `main` in the same format** — if not, then
//! > no.
//!
//! And that is not new machinery: it is [`AnchorState`], which this module
//! already computes. Validity is **not a property of the branch that wrote the
//! record**. It is whether the anchor resolves in the tree being looked at:
//!
//! - anchor resolves with a matching blob ⇒ the association is here *in the same
//!   format* ⇒ the record applies, whichever branch wrote it;
//! - drifted, vanished or unmeasurable ⇒ not merged, or merged in a different
//!   form ⇒ it does not apply *to this tree*. Kept and marked, never pruned.
//!
//! "Is this valid on `main`?" is answered by resolving the anchor against
//! `main`'s graph, and the identical mechanism answers it on any branch, worktree
//! or clone, with **no branch bookkeeping at all**. [`AnchorState::applies`] is
//! that predicate; note that it consults neither the scope, nor `created_at`, nor
//! the record's position in the sequence.
//!
//! **"In the same format" means the blob matches**, deliberately strictly: even a
//! pure reformat breaks the association. That fails toward *marked drifted*
//! rather than silently applying a lesson to code that has moved on, which is the
//! error worth avoiding.
//!
//! A record with **no anchor at all** is a general lesson about the repository
//! ("CI is Ubuntu-only") and is repo-wide: it applies everywhere, because it
//! never claimed to be about a particular piece of code. That is a different
//! thing from an anchor that failed to resolve, and the two are separate
//! [`AnchorState`] values with opposite answers so they can never be confused.
//!
//! ## What `scope` is, and is not
//!
//! `scope` is a **coarse namespace** — which repo or project a record belongs to,
//! in a multi-repo workspace. It is **not a branch label**, and nothing keys off
//! it beyond an exact-match filter: no isolation, no inheritance, no merging.
//! Branch applicability is the anchor's job, above, and giving `scope` a second
//! job would create two answers to one question.
//!
//! # Supersession, recorded and not guessed
//!
//! New knowledge overruling old is expressed **explicitly**, by pointing the old
//! record's [`MemoryRecord::superseded_by`] at the new one's id. A superseded
//! record drops out of live listing **immediately, regardless of age**, and the
//! chain stays auditable because nothing is deleted.
//!
//! This is the live analogue of [`crate::EdgeKind::Supersedes`], which exists in
//! the enum but is produced by nothing. Per the standing decision it stays
//! **inside the artifact store and never becomes a graph edge**.
//!
//! # Ordering is a generation, not a clock
//!
//! `id` is `INTEGER PRIMARY KEY AUTOINCREMENT` and is the ordering key.
//! [`MemoryRecord::created_at`] is written for humans and **never read**, exactly
//! as `imports.imported_at` behaves. The store is per-repo and shared across
//! worktrees and branches, so concurrent checkouts produce non-monotone
//! wall-clock, and `SQLite`'s `datetime('now')` is second-granular and ties on
//! intra-second writes. Ranking on either would make results non-deterministic
//! for a fixed repo state. [`MemoryRecord::superseded_at`] is the same kind of
//! value — display, never policy.
//!
//! # Privacy
//!
//! The store lives in `.git/roteiro/` beside `graph.db`: per-clone, never
//! committed, never pushed. That placement is not cosmetic. Extraction redacts
//! secret-looking config values *before* persistence because the graph is
//! exportable; memory has **no such chokepoint**, because it records prose an
//! agent wrote, which can contain pasted tokens, stack traces or customer names.
//! [`crate::Store::forget_memory`] is the reclamation path, and it is the only
//! one.
//!
//! @rto:0013

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::store::StoreError;

/// Stable schema tag on [`MemoryListing`], so a programmatic consumer can depend
/// on the shape.
pub const MEMORY_SCHEMA: &str = "roteiro.memory/v1";

/// The scope recorded when a caller names none.
///
/// `scope` is a **coarse namespace** — which repo or project a record belongs to
/// in a multi-repo workspace (ADR-0008) — and it is **explicitly not a branch
/// label**. Nothing keys off it beyond an exact-match filter in
/// [`MemoryFilter::scope`]: no isolation, no inheritance, no merging.
///
/// Whether a record applies to the tree in front of you is decided by its
/// **anchor**, not its scope — see [`AnchorState::applies`] and the module docs.
/// Giving `scope` that second job would create two answers to one question, and
/// the branch-shaped one would be wrong: a lesson does not become false because
/// the branch that learned it was deleted.
pub const DEFAULT_MEMORY_SCOPE: &str = "repo";

/// Longest permitted memory body, in bytes. Generous, because a body is prose —
/// a failure write-up with a stack trace in it is a legitimate memory. Anything
/// past this is a file being pasted into a database, not a lesson.
pub const MAX_MEMORY_BODY: usize = 64 * 1024;

/// Longest permitted scope, in bytes. A scope is a short label — a branch name,
/// a worktree id, a project — not a sentence.
pub const MAX_MEMORY_SCOPE: usize = 128;

/// Errors raised when writing or forgetting a memory record.
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    /// The underlying store failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// A scope was empty, over-long, or carried a control character or
    /// surrounding whitespace.
    #[error(
        "invalid scope {0:?} (expected 1 to {MAX_MEMORY_SCOPE} bytes, no control characters, no surrounding whitespace)"
    )]
    InvalidScope(String),
    /// A body was empty, whitespace-only, or longer than [`MAX_MEMORY_BODY`].
    #[error("invalid body: {0}")]
    InvalidBody(String),
    /// A confidence was offered that is not a probability.
    #[error("invalid confidence {0}: expected a finite number in [0.0, 1.0]")]
    InvalidConfidence(f64),
    /// A record was named that is not in the store.
    #[error("no memory record with id {0}")]
    NotFound(i64),
    /// A record was named as superseded that another record has already
    /// superseded. The chain stays a chain: re-pointing it would orphan the
    /// successor already recorded.
    #[error("memory record {id} is already superseded by {by}")]
    AlreadySuperseded {
        /// The record that was to be superseded.
        id: i64,
        /// The successor already on record.
        by: i64,
    },
    /// A stored row could not be interpreted (database corruption).
    #[error("corrupt memory record: {0}")]
    Corrupt(String),
}

impl From<rusqlite::Error> for MemoryError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Store(StoreError::Sqlite(err))
    }
}

/// What kind of knowledge a record holds.
///
/// A **closed** vocabulary, enforced by the schema as well as by this type,
/// following the same rule the `analysis_runs` runner and isolation tokens live
/// under: a value outside the known set is a corrupt write, not a new feature.
/// The five names are ADR-0013's own list of what episodic memory is for. Free
/// text was the alternative and was declined — `lesson`, `Lesson` and `lessons`
/// would be three different kinds, none of them findable by a filter, and a
/// vocabulary that cannot be filtered cannot later be ranked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryKind {
    /// Something established that a later session should not have to re-derive.
    Lesson,
    /// An approach that was tried and did not work, and why.
    Attempt,
    /// A choice made, so it is not silently remade.
    Decision,
    /// A failure mode seen more than once.
    Pattern,
    /// How a task actually ended.
    Outcome,
}

impl MemoryKind {
    /// Every kind, in declaration order — the vocabulary the CLI advertises.
    pub const ALL: [Self; 5] = [
        Self::Lesson,
        Self::Attempt,
        Self::Decision,
        Self::Pattern,
        Self::Outcome,
    ];

    /// Stable string token used in the `SQLite` store and in `--json` output.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lesson => "lesson",
            Self::Attempt => "attempt",
            Self::Decision => "decision",
            Self::Pattern => "pattern",
            Self::Outcome => "outcome",
        }
    }

    /// Parse a kind from its stable token; `None` for an unrecognised value (a
    /// corrupt row, or a typo on the command line).
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

impl std::fmt::Display for MemoryKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for MemoryKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_token(s).ok_or_else(|| {
            let known = Self::ALL.map(Self::as_str).join(", ");
            format!("unknown memory kind {s:?} (expected one of: {known})")
        })
    }
}

/// What a record's anchor is worth *right now*, computed on every read against
/// the current graph and **never stored**.
///
/// A stored verdict would have to be rewritten on every sync — and would be
/// wrong in between — which is the same reason ADR-0013 keeps decay out of the
/// table. None of these states deletes anything: see the module docs for why a
/// record about vanished code is kept and marked rather than pruned.
///
/// **This is also the scope test.** [`AnchorState::applies`] is what decides
/// whether a record applies to the tree in front of you — see the module docs.
/// The two "no useful anchor" situations are deliberately *separate* states with
/// opposite answers, because conflating them is the mistake that would make the
/// rule meaningless:
///
/// - [`AnchorState::Unanchored`] — **nothing was ever anchored**. A general
///   lesson about the repository, which applies everywhere.
/// - [`AnchorState::Vanished`] / [`AnchorState::Drifted`] /
///   [`AnchorState::Unverifiable`] — **an anchor was recorded and did not
///   resolve here**. The association is not present in this tree in the same
///   form, so the record does not apply to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnchorState {
    /// **No anchor was ever recorded** — a general lesson about the repository
    /// ("CI is Ubuntu-only"), tied to nothing in particular and therefore true
    /// wherever the repository is. Applies.
    ///
    /// Not to be confused with an anchor that failed to resolve: this record
    /// never claimed to be about a specific piece of code, so there is nothing
    /// for a tree to disagree with.
    Unanchored,
    /// The anchored node is present and carries the blob captured at write time:
    /// the association is in this tree **in the same format**. Applies.
    Valid,
    /// The anchored node is present but carries a **different** blob: the code
    /// changed underneath the record. It may still be right; it is no longer
    /// evidence about what is there now, and it does not apply to this tree.
    Drifted,
    /// The anchored node is **gone** from the graph. The most interesting state,
    /// and the one the authored layer would have pruned. Does not apply here —
    /// and is kept anyway, because a lesson about deleted code is often the most
    /// valuable one.
    Vanished,
    /// The anchored node is present, but no blob was captured (or the node
    /// carries none), so *the blob cannot be compared either way*. Reported
    /// honestly rather than folded into [`AnchorState::Valid`], which would claim
    /// a check that never happened.
    ///
    /// **Does not apply**, by the same strictness that makes [`AnchorState::
    /// Drifted`] not apply: the rule is that the association is present *in the
    /// same format*, and an unmeasurable blob cannot demonstrate that. Failing
    /// toward *marked* is the whole point — the alternative silently applies a
    /// lesson to code nobody checked.
    Unverifiable,
}

impl AnchorState {
    /// Stable string token used in `--json` output.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unanchored => "unanchored",
            Self::Valid => "valid",
            Self::Drifted => "drifted",
            Self::Vanished => "vanished",
            Self::Unverifiable => "unverifiable",
        }
    }

    /// **Whether this record applies to the tree it was just resolved against.**
    ///
    /// The whole scope rule, in one predicate: a record applies when it is
    /// anchored to nothing (a general lesson) or when its anchor resolves here
    /// with the same blob. Everything else — vanished, drifted, unmeasurable —
    /// means the association is not present in this tree in the same format, so
    /// the record does not apply *here*. It is still stored, still listed, and
    /// still applies wherever its anchor does resolve.
    ///
    /// Note what this predicate does **not** consult: the branch the record was
    /// written on, its `created_at`, its scope, or its position in the sequence.
    /// None of those is available to it, which is the point — applicability is a
    /// question about the tree, asked fresh every read.
    #[must_use]
    pub fn applies(self) -> bool {
        matches!(self, Self::Unanchored | Self::Valid)
    }

    /// Whether the anchored code has moved out from under this record — the
    /// evidence-first signal ADR-0013 depreciates on. **Never a delete
    /// condition**; a stale record is kept, marked, and (in a later stage) ranked
    /// lower.
    ///
    /// Narrower than the negation of [`AnchorState::applies`]: staleness means
    /// *the code moved*, which [`AnchorState::Unverifiable`] does not claim —
    /// nothing was measured there. A record can fail to apply without anything
    /// having gone stale.
    #[must_use]
    pub fn is_stale(self) -> bool {
        matches!(self, Self::Drifted | Self::Vanished)
    }
}

impl std::fmt::Display for AnchorState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where a record is anchored, as captured when it was written.
///
/// `blob` and `path` are the **evidence at capture time**, not a live view: they
/// are what the node carried then, which is precisely what makes a later
/// comparison meaningful.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryAnchor {
    /// The node key this record is about.
    pub key: String,
    /// The node's `blob_hash` when the record was written, if it had one. Half of
    /// the stable pair; without it drift cannot be detected.
    pub blob: Option<String>,
    /// The node's path when the record was written. Evidence for a human reader;
    /// never part of the drift check, because a path is not an identity.
    pub path: Option<String>,
}

/// One stored memory record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryRecord {
    /// The monotonic generation and identity. `AUTOINCREMENT`, so an id is never
    /// reused after a [`crate::Store::forget_memory`].
    pub id: i64,
    /// The recorded namespace — which repo or project this belongs to. **Not a
    /// branch label**; see [`DEFAULT_MEMORY_SCOPE`].
    pub scope: String,
    /// What kind of knowledge this is.
    pub kind: MemoryKind,
    /// Where it is anchored, if anywhere.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<MemoryAnchor>,
    /// What that anchor is worth against the **current** graph. Computed on read;
    /// no column holds it.
    pub anchor_state: AnchorState,
    /// **Whether this record applies to the tree it was just read against** —
    /// [`AnchorState::applies`] for the state above, surfaced as its own field so
    /// a programmatic consumer gets the scope rule without having to re-implement
    /// it from the state token.
    ///
    /// Like `anchor_state`, computed on every read and stored in no column. A
    /// `false` here is never a reason to delete anything: the record still applies
    /// wherever its anchor does resolve.
    pub applies: bool,
    /// The prose. Unredacted by construction — see the module docs on privacy.
    pub body: String,
    /// The writer's own confidence, when it offered one. **Not** the score an
    /// `inferred` edge carries, and never readable as one: no memory record is a
    /// graph fact.
    pub confidence: Option<f64>,
    /// The `sync_state` tree id when the record was written — the repo-state
    /// witness, so a reader can tell which state of the world the writer saw.
    pub tree: Option<String>,
    /// `SQLite`'s `datetime('now')` at write time. **Written for humans and never
    /// read**, exactly as `imports.imported_at` is; no ordering or policy depends
    /// on it.
    pub created_at: String,
    /// The record that overruled this one, if any. Live listing excludes any
    /// record with a successor, immediately and regardless of age.
    pub superseded_by: Option<i64>,
    /// When that happened. Display only, on the same terms as `created_at`.
    pub superseded_at: Option<String>,
}

impl MemoryRecord {
    /// Whether this record is live — nothing has superseded it.
    #[must_use]
    pub fn is_live(&self) -> bool {
        self.superseded_by.is_none()
    }
}

/// The values [`crate::Store::record_memory`] writes.
///
/// `anchor` is a **node key**; the blob and path stored alongside it are captured
/// by the store from the graph at write time, so there is exactly one place that
/// decides what an anchor's evidence is. `tree` is captured the same way.
#[derive(Debug, Clone, Copy)]
pub struct MemoryWrite<'a> {
    /// The scope to record.
    pub scope: &'a str,
    /// What kind of knowledge this is.
    pub kind: MemoryKind,
    /// The node key to anchor to, if any. A key naming no node is **accepted**,
    /// and reads back as [`AnchorState::Vanished`]: recording a lesson about code
    /// that is already gone is a legitimate — often the most valuable — thing to
    /// do, and refusing it would be the prune rule wearing a different hat.
    pub anchor: Option<&'a str>,
    /// The prose.
    pub body: &'a str,
    /// The writer's own confidence, if it has one.
    pub confidence: Option<f64>,
    /// The record this one overrules, if any. Supersession is recorded here,
    /// explicitly, at the moment the successor is written — never inferred later
    /// from age.
    pub supersedes: Option<i64>,
}

impl MemoryWrite<'_> {
    /// Validate a write, refusing a record that could not be stored or recalled.
    ///
    /// # Errors
    /// Returns [`MemoryError::InvalidScope`], [`MemoryError::InvalidBody`] or
    /// [`MemoryError::InvalidConfidence`], each naming what was actually wrong.
    pub fn validate(&self) -> Result<(), MemoryError> {
        if self.scope.is_empty()
            || self.scope.len() > MAX_MEMORY_SCOPE
            || self.scope.trim() != self.scope
            || self.scope.chars().any(char::is_control)
        {
            return Err(MemoryError::InvalidScope(self.scope.to_owned()));
        }
        if self.body.trim().is_empty() {
            return Err(MemoryError::InvalidBody(
                "it is empty or only whitespace".to_owned(),
            ));
        }
        if self.body.len() > MAX_MEMORY_BODY {
            return Err(MemoryError::InvalidBody(format!(
                "it is {} bytes, over the {MAX_MEMORY_BODY}-byte limit",
                self.body.len()
            )));
        }
        if let Some(confidence) = self.confidence
            && !(confidence.is_finite() && (0.0..=1.0).contains(&confidence))
        {
            return Err(MemoryError::InvalidConfidence(confidence));
        }
        Ok(())
    }
}

/// A narrowing filter for [`crate::Store::memory_records`].
///
/// [`MemoryFilter::default`] is **live records only, newest generation first, no
/// limit** — the listing an agent actually wants, with superseded knowledge
/// already gone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemoryFilter<'a> {
    /// Only records recorded in this scope, matched exactly.
    pub scope: Option<&'a str>,
    /// Only records of this kind.
    pub kind: Option<MemoryKind>,
    /// Only records anchored to this node key.
    pub anchor_key: Option<&'a str>,
    /// Also return records another record has superseded. Off by default: a
    /// superseded record drops out of live listing immediately, and the chain is
    /// kept for audit rather than for reading.
    pub include_superseded: bool,
    /// At most this many records (the newest generations). `None` for all of
    /// them.
    pub limit: Option<usize>,
}

/// A listing of memory records, with the counts that make it legible.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryListing {
    /// Stable schema tag ([`MEMORY_SCHEMA`]).
    pub schema: &'static str,
    /// The matching records, newest generation first.
    pub records: Vec<MemoryRecord>,
    /// Live records in the whole store, ignoring the filter — so a filtered
    /// listing that returns nothing is legible as *nothing matched* rather than
    /// *nothing is stored*.
    pub live: u64,
    /// Superseded records in the whole store. Never zero once knowledge has been
    /// overruled: nothing is deleted by supersession.
    pub superseded: u64,
}

/// What one [`crate::Store::forget_memory`] removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryForgotten {
    /// The record that was deleted.
    pub id: i64,
    /// Records that were superseded **by** the deleted one and are therefore live
    /// again.
    ///
    /// Forgetting a successor destroys the only recorded reason its predecessor
    /// was dropped from live listing. Leaving the predecessor superseded would
    /// make it invisible on the strength of a record that no longer exists —
    /// supersession by ghost, which is precisely the inferred-not-recorded
    /// failure the explicit pointer exists to prevent. So the pointer is cleared
    /// and the predecessor returns, reported here rather than silently.
    pub restored: Vec<i64>,
}

// --- Persistence. Free helpers over a `Connection` (a `Transaction` derefs to
// one), mirroring the findings and media stores. Every statement here touches
// `agent_memory` and reads `nodes` for anchor evidence; nothing in this module
// ever *writes* `nodes` or `edges`. ---

/// Columns of `agent_memory` plus the joined anchor evidence, in the order
/// [`record_from_row`] decodes them.
const RECORD_COLS: &str = "m.id, m.scope, m.kind, m.anchor_key, m.anchor_blob, m.anchor_path, \
     m.body, m.confidence, m.tree, m.created_at, m.superseded_by, m.superseded_at, \
     n.key, n.blob_hash";

/// The `LEFT JOIN` that resolves an anchor against the **current** graph. Left,
/// not inner: a record whose anchor vanished must still come back, marked.
const RECORD_FROM: &str = " FROM agent_memory m LEFT JOIN nodes n ON n.key = m.anchor_key";

/// Write one record, returning its new id (its generation).
///
/// Anchor evidence and the repo-state witness are captured here, from the graph,
/// so a caller cannot record an anchor blob that was never on the node.
pub(crate) fn record(conn: &Connection, write: &MemoryWrite<'_>) -> Result<i64, MemoryError> {
    write.validate()?;

    // Supersession is resolved *before* the insert, so a bad reference costs
    // nothing: the caller gets an error and the store is untouched.
    if let Some(target) = write.supersedes {
        let existing: Option<Option<i64>> = conn
            .query_row(
                "SELECT superseded_by FROM agent_memory WHERE id = ?1",
                [target],
                |r| r.get(0),
            )
            .optional()?;
        match existing {
            None => return Err(MemoryError::NotFound(target)),
            Some(Some(by)) => return Err(MemoryError::AlreadySuperseded { id: target, by }),
            Some(None) => {}
        }
    }

    // Anchor evidence, captured from the graph as it stands. A key that names no
    // node stores the key alone — the record is kept and reads back as
    // `Vanished`, never refused.
    let anchor: Option<(Option<String>, Option<String>)> = match write.anchor {
        Some(key) => Some(
            conn.query_row(
                "SELECT blob_hash, path FROM nodes WHERE key = ?1",
                [key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .unwrap_or((None, None)),
        ),
        None => None,
    };
    // The repo-state witness. `sync_state` is a single row that may not exist yet
    // in a store that has never synced, which is a legitimate state to record a
    // memory from.
    let tree: Option<String> = conn
        .query_row("SELECT tree FROM sync_state WHERE id = 0", [], |r| r.get(0))
        .optional()?;

    conn.execute(
        "INSERT INTO agent_memory (
             scope, kind, anchor_key, anchor_blob, anchor_path, body, confidence, tree
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            write.scope,
            write.kind.as_str(),
            write.anchor,
            anchor.as_ref().and_then(|(blob, _)| blob.as_deref()),
            anchor.as_ref().and_then(|(_, path)| path.as_deref()),
            write.body,
            write.confidence,
            tree,
        ],
    )?;
    let id = conn.last_insert_rowid();

    // The supersession itself: an explicit pointer from the overruled record to
    // this one, plus the moment for a human reader. The moment is never read —
    // `superseded_by` alone decides what is live.
    if let Some(target) = write.supersedes {
        conn.execute(
            "UPDATE agent_memory
                SET superseded_by = ?1, superseded_at = datetime('now')
              WHERE id = ?2",
            params![id, target],
        )?;
    }
    Ok(id)
}

/// Records matching `filter`, newest generation first.
pub(crate) fn records(
    conn: &Connection,
    filter: &MemoryFilter<'_>,
) -> Result<Vec<MemoryRecord>, StoreError> {
    let mut where_parts: Vec<&str> = Vec::new();
    let mut bound: Vec<String> = Vec::new();
    if let Some(scope) = filter.scope {
        where_parts.push("m.scope = ?");
        bound.push(scope.to_owned());
    }
    if let Some(kind) = filter.kind {
        where_parts.push("m.kind = ?");
        bound.push(kind.as_str().to_owned());
    }
    if let Some(key) = filter.anchor_key {
        where_parts.push("m.anchor_key = ?");
        bound.push(key.to_owned());
    }
    // The whole of "superseded records drop out of live listing immediately,
    // regardless of age": one clause on a recorded pointer, and no clock in it.
    if !filter.include_superseded {
        where_parts.push("m.superseded_by IS NULL");
    }
    let clause = if where_parts.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", where_parts.join(" AND "))
    };
    // `id DESC` is newest-generation-first. It is an `AUTOINCREMENT` integer, not
    // a timestamp, so this ordering is total and skew-proof across worktrees.
    let limit = match filter.limit {
        Some(n) => format!(" LIMIT {n}"),
        None => String::new(),
    };
    let sql = format!("SELECT {RECORD_COLS}{RECORD_FROM}{clause} ORDER BY m.id DESC{limit}");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(rusqlite::params_from_iter(bound))?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(record_from_row(row)?);
    }
    Ok(out)
}

/// One record by id, or `None` if it is not there.
pub(crate) fn get(conn: &Connection, id: i64) -> Result<Option<MemoryRecord>, StoreError> {
    let sql = format!("SELECT {RECORD_COLS}{RECORD_FROM} WHERE m.id = ?1");
    conn.query_row(&sql, [id], |row| Ok(record_from_row(row)))
        .optional()?
        .transpose()
}

/// Delete one record, restoring anything it had superseded. `None` if there was
/// no such record.
pub(crate) fn forget(conn: &Connection, id: i64) -> Result<Option<MemoryForgotten>, StoreError> {
    let present: Option<i64> = conn
        .query_row("SELECT id FROM agent_memory WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .optional()?;
    if present.is_none() {
        return Ok(None);
    }
    // Whatever this record superseded, read before the pointers are cleared.
    let restored: Vec<i64> = {
        let mut stmt =
            conn.prepare("SELECT id FROM agent_memory WHERE superseded_by = ?1 ORDER BY id")?;
        let mut rows = stmt.query([id])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(row.get::<_, i64>(0)?);
        }
        out
    };
    // Clear them first: `superseded_by` is a foreign key, so the delete below
    // would be refused while any row still points here. Clearing rather than
    // cascading is the deliberate part — see `MemoryForgotten::restored`.
    conn.execute(
        "UPDATE agent_memory SET superseded_by = NULL, superseded_at = NULL
          WHERE superseded_by = ?1",
        [id],
    )?;
    conn.execute("DELETE FROM agent_memory WHERE id = ?1", [id])?;
    Ok(Some(MemoryForgotten { id, restored }))
}

/// How many records are stored, split live / superseded.
pub(crate) fn counts(conn: &Connection) -> Result<(u64, u64), StoreError> {
    let (live, superseded): (i64, i64) = conn.query_row(
        "SELECT COALESCE(SUM(superseded_by IS NULL), 0), COALESCE(SUM(superseded_by IS NOT NULL), 0)
           FROM agent_memory",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok((
        u64::try_from(live).unwrap_or(0),
        u64::try_from(superseded).unwrap_or(0),
    ))
}

/// Decode an `agent_memory` row joined against `nodes`.
fn record_from_row(row: &rusqlite::Row<'_>) -> Result<MemoryRecord, StoreError> {
    let kind_token: String = row.get(2)?;
    let kind = MemoryKind::from_token(&kind_token)
        .ok_or_else(|| StoreError::Corrupt(format!("unknown memory kind: {kind_token}")))?;
    let anchor_key: Option<String> = row.get(3)?;
    let anchor_blob: Option<String> = row.get(4)?;
    // Whether the anchored node exists *now*, from the LEFT JOIN: `NULL` means no
    // such node, which is the vanished case rather than a missing column.
    let node_key: Option<String> = row.get(12)?;
    let node_blob: Option<String> = row.get(13)?;

    let anchor_state = match (&anchor_key, &node_key) {
        (None, _) => AnchorState::Unanchored,
        (Some(_), None) => AnchorState::Vanished,
        (Some(_), Some(_)) => match (&anchor_blob, &node_blob) {
            // Both halves of the stable pair are present, so drift is a real
            // comparison rather than an assumption.
            (Some(captured), Some(current)) if captured == current => AnchorState::Valid,
            (Some(_), Some(_)) => AnchorState::Drifted,
            // One side has no blob: the node is there, but nothing can be
            // concluded about the code under it. Said plainly rather than
            // rounded up to `Valid`.
            _ => AnchorState::Unverifiable,
        },
    };
    let anchor = anchor_key.map(|key| MemoryAnchor {
        key,
        blob: anchor_blob,
        path: row.get(5).unwrap_or(None),
    });
    Ok(MemoryRecord {
        id: row.get(0)?,
        scope: row.get(1)?,
        kind,
        anchor,
        anchor_state,
        // Derived from the state, in one place, rather than recomputed by every
        // consumer — the scope rule has exactly one implementation.
        applies: anchor_state.applies(),
        body: row.get(6)?,
        confidence: row.get(7)?,
        tree: row.get(8)?,
        created_at: row.get(9)?,
        superseded_by: row.get(10)?,
        superseded_at: row.get(11)?,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        AnchorState, DEFAULT_MEMORY_SCOPE, MAX_MEMORY_BODY, MAX_MEMORY_SCOPE, MemoryKind,
        MemoryWrite,
    };

    fn write(body: &str) -> MemoryWrite<'_> {
        MemoryWrite {
            scope: DEFAULT_MEMORY_SCOPE,
            kind: MemoryKind::Lesson,
            anchor: None,
            body,
            confidence: None,
            supersedes: None,
        }
    }

    /// **`EXTRACT_VERSION` does not change for agent memory.**
    ///
    /// It is folded into the content-addressed cache key, so bumping it discards
    /// every cached fact set in every clone and forces a full re-extraction of
    /// every repository. That cost is right when extraction *output* changes —
    /// and memory is not extraction output at all. It is not derived from
    /// `(path, blob id, bytes)`, no extractor emits it, and no cached fact set
    /// can contain it. This is a stage-scoped guard on a shared constant, and it
    /// lives here rather than in an integration test because the constant is
    /// crate-private.
    ///
    /// The expected base is the version *someone else* is entitled to move: this
    /// guard asserts that **memory** did not move it, not that it never moves.
    /// It must therefore be updated whenever a change to extraction output
    /// legitimately bumps the base, or adds a feature namespace — subtracting
    /// every namespace is what isolates the base from the feature matrix.
    ///
    /// Base 10 → 11 and the `audio-metadata` (+400) namespace both arrived with
    /// ADR-0016 (`audio_stream` nodes genuinely change extraction output, which
    /// is exactly what the constant is for). This guard shipped one minute later
    /// in a PR whose CI had never seen them, so for 57 seconds' worth of merge
    /// ordering it asserted against a constant that had already moved, and `main`
    /// went red under *every* feature combination — 11 ≠ 10 by default, 411 ≠ 10
    /// under `--all-features`.
    #[test]
    fn agent_memory_does_not_bump_the_extraction_version() {
        assert_eq!(
            crate::extract::EXTRACT_VERSION
                - if cfg!(feature = "pdf-text") { 100 } else { 0 }
                - if cfg!(feature = "image-ocr") { 200 } else { 0 }
                - if cfg!(feature = "audio-metadata") {
                    400
                } else {
                    0
                },
            11,
            "memory is not extraction output; nothing here may invalidate the fact cache",
        );
    }

    #[test]
    fn kind_tokens_round_trip_and_reject_the_unknown() {
        for kind in MemoryKind::ALL {
            assert_eq!(MemoryKind::from_token(kind.as_str()), Some(kind));
            assert_eq!(kind.to_string(), kind.as_str());
        }
        assert_eq!(MemoryKind::from_token("note"), None);
        assert_eq!(MemoryKind::from_token("Lesson"), None);
        let err = "note".parse::<MemoryKind>().expect_err("unknown kind");
        assert!(
            err.contains("lesson"),
            "the error lists the vocabulary: {err}"
        );
    }

    #[test]
    fn anchor_state_tokens_and_staleness() {
        assert!(AnchorState::Drifted.is_stale());
        assert!(AnchorState::Vanished.is_stale());
        // The three that are *not* stale, said explicitly: `Unverifiable` in
        // particular must not be treated as drift — nothing was measured.
        for state in [
            AnchorState::Unanchored,
            AnchorState::Valid,
            AnchorState::Unverifiable,
        ] {
            assert!(!state.is_stale(), "{state} must not read as stale");
        }
        assert_eq!(AnchorState::Vanished.to_string(), "vanished");
    }

    #[test]
    fn validation_names_what_was_actually_wrong() {
        write("a real lesson").validate().expect("the good case");

        let over_long = "x".repeat(MAX_MEMORY_BODY + 1);
        for (case, w) in [
            ("empty body", write("")),
            ("whitespace body", write("   \n\t ")),
            ("over-long body", write(&over_long)),
        ] {
            assert!(w.validate().is_err(), "{case} must be refused");
        }

        let long_scope = "s".repeat(MAX_MEMORY_SCOPE + 1);
        for scope in ["", " repo", "repo ", "re\npo", &long_scope] {
            let w = MemoryWrite {
                scope,
                ..write("body")
            };
            assert!(w.validate().is_err(), "scope {scope:?} must be refused");
        }

        for confidence in [Some(0.0), Some(1.0), Some(0.5), None] {
            let w = MemoryWrite {
                confidence,
                ..write("body")
            };
            w.validate().expect("a probability is fine");
        }
        for confidence in [-0.1, 1.1, f64::NAN, f64::INFINITY] {
            let w = MemoryWrite {
                confidence: Some(confidence),
                ..write("body")
            };
            assert!(
                w.validate().is_err(),
                "{confidence} is not a probability and must be refused"
            );
        }
    }
}

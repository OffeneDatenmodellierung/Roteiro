//! Provenance-tagged knowledge graph store.
//!
//! Every edge in a Roteiro graph carries a [`Provenance`] tag recording how it
//! was produced: deterministically derived from source ASTs, authored by a
//! human or agent in an ADR/blueprint, or inferred heuristically from docs and
//! other artifacts. See ADR-0001.
//!
//! The graph is a set of [`Node`]s addressed by a deterministic natural
//! [`Node::key`], connected by [`Edge`]s. Facts extracted from one source blob
//! are grouped into a [`FactSet`] and applied atomically to a [`Store`].
//!
//! @rto:0001

mod artifact;
// Audio metadata (ADR-0016): codec, rate, bit depth, channels, duration and tags,
// read from the container without decoding and without a model. Unlike the media
// module below, these *are* `derived` facts and do live in `nodes`/`edges` — the
// complement of ADR-0015 rather than an exception to it.
#[cfg(feature = "audio-metadata")]
pub mod audio;
mod cache;
mod codegraph;
mod config_keys;
mod context;
// The holder for the media extractors' process-wide native engines — and the
// deterministic release that keeps a Metal build from aborting at exit (#291) —
// now lives one level down, next to the llama.cpp backend that shares the same
// mechanism: `rto_llama::EngineSlot` (#296).
mod extract;
// Analyzer findings (ADR-0012): a *separate* artifact store, deliberately not a
// provenance class and deliberately not in `nodes`/`edges`.
mod findings;
mod git;
#[cfg(feature = "inference")]
mod infer;
mod links;
mod markers;
// Generated media content (ADR-0015): ASR transcripts and VLM descriptions. Like
// findings, a *separate* artifact store — generated text is not a deterministic
// function of the bytes, so it is not a `derived` fact and never enters
// `nodes`/`edges`.
pub mod media;
// Episodic agent memory (ADR-0013): what a session learned, which has no
// generating function at all — so it is neither `derived` nor `authored`, and it
// gets a *separate* artifact store on the same terms as findings and media.
mod memory;
mod migrations;
mod model;
// Which model serves which task, and **why** (Stage 33). Deliberately in *this*
// crate: `gix` is pinned here without transports, so a resolver that decides
// which model runs structurally cannot grow a "check for a newer one" call.
#[cfg(feature = "models")]
pub mod model_choice;
#[cfg(feature = "models")]
mod models;
mod provenance;
mod query;
// Stage 35 — the adjudicated review corpus, and the two pure decisions made over
// it. In *this* crate for the same reason `model_choice` is: `gix` is pinned here
// without transports, and both a historical record that must not be "refreshed
// from the GitHub API" and a suppression rule that must not "just ask CI" are
// precisely the code that would otherwise acquire such a call.
pub mod compile_claim;
pub mod review_corpus;
pub mod review_score;
// Stage 35b — the reviewer's judgement, which is likewise pure: prompt assembly,
// response parsing and the compile-claim site derivation are functions of bytes,
// so what the reviewer *decides* is testable with no model and no network. The
// loop that calls an engine is in the binary, where the engine already is.
pub mod reviewer;
mod store;
mod sync;
mod text;
// Whether a producer's identity is measured or asserted (ADR-0019 §5). In *this*
// crate rather than in `rto-remote` because `rto-remote` depends on this one, so
// `ModelSource::Remote` cannot name a type that lives there — and because the
// grade qualifies `Producer`, which is here. Two variants and a sentence: it
// brings no transport with it.
pub mod trust;
mod workspace;

pub use artifact::{ARTIFACT_SCHEMA, GraphArtifact};
#[cfg(feature = "audio-metadata")]
pub use audio::{AUDIO_STREAM_KIND, AudioDuration, AudioFacts, AudioTag, Exactness};
pub use cache::{CacheError, ObjectCache, ObjectSweep};
pub use codegraph::{ORACLE_SCHEMA, OracleError, OracleReport, compare as compare_codegraph};
pub use config_keys::{
    ConfigKey, canonicalize as canonicalize_config_key, flatten as flatten_config, is_config_path,
    is_secret_key, is_tooling_config_path, normalize as normalize_config_key,
};
pub use context::{
    BoundedEdges, ContextEdge, ContextNode, ContextRefresh, NodeContext, OmittedEdges,
    TOOL_CONTEXT_EDGE_CAP, ToolContext, build_context, context, dependents, refresh_contexts,
    tool_context,
};
pub use extract::{
    Extractor, FileNodeExtractor, IngestConfig, MediaEngineGuard, Registry, RustExtractor,
    release_media_engines,
};
pub use findings::{
    AdvisoryDb, AnalysisRun, CommandPolicy, EnvironmentPolicy, FINDING_KEY_PREFIX, Finding,
    FindingKey, FindingsApplied, FindingsError, FindingsLayer, Isolation, MAX_ANALYZER_ID,
    MAX_IDENTITY_PART, NetworkPolicy, RunnerKind, SECURITY_LAYER_PREFIX, Severity, SourceIdentity,
    WorktreeAccess, WorktreeId, analyzer_id_error, is_valid_analyzer_id, layer_key,
};
pub use git::{BlobRef, ChangeStatus, ChangedFile, GitError, GraphSource, Repo, Submodule};
#[cfg(feature = "inference")]
pub use infer::{
    DuplicateConfig, DuplicatePair, DuplicateReport, EMBED_REF, Embedder, HashEmbedder,
    InferenceConfig, duplicates, duplicates_with, embed, infer_edges, infer_edges_with, similarity,
};
pub use links::{
    EXTERNAL_REF_KIND, LINKS_REF, external_ref_key, external_ref_node, external_ref_target,
};
pub use media::{
    CandidateCount, GateReason, GateThresholds, GeneratedContent, MAX_MODEL_ID, MAX_PROMPT,
    MEDIA_PRODUCER_PREFIX, MEDIA_SCHEMA, MediaBlob, MediaBuildOptions, MediaBuildReport,
    MediaError, MediaFilter, MediaKind, MediaOutcome, MediaProducer, MediaRecord, MediaSkip,
    MediaStatus, MediaWrite, Producer, ProducerId, ProducerSummary, ProducerSummaryAvailable,
    SkipEntry, build_media, is_valid_model_id, media_blobs, status as media_status,
};
pub use memory::{
    AnchorState, CACHE_BUDGET_ENV, CACHE_SCHEMA, CacheEntry, CacheStats, CacheSweep, CacheWrite,
    DEFAULT_BASE_CONFIDENCE, DEFAULT_CACHE_BUDGET_BYTES, DEFAULT_DECAY_SPAN, DEFAULT_HALF_LIFE,
    DEFAULT_MEMORY_SCOPE, Decay, MAX_MEMORY_BODY, MAX_MEMORY_SCOPE, MEMORY_SCHEMA, MemoryAnchor,
    MemoryError, MemoryFilter, MemoryForgotten, MemoryKind, MemoryListing, MemoryRecord,
    MemoryWrite, RECALL_SCHEMA, Recall, RecallOptions, Recalled, anchor_penalty,
    cache_budget_bytes,
};
pub use model::{Direction, Edge, EdgeKind, FactSet, Node, NodeKind, Span};
#[cfg(feature = "models")]
pub use model_choice::{
    DEFAULT_GENERATIVE, DEFAULT_OCR, ModelChoice, ModelChoiceError, ModelPins, ModelSource,
    ModelTask, RemoteTier, TASKS as MODEL_TASKS, resolve as resolve_model,
    resolve_all_with as resolve_models, resolve_with as resolve_model_with,
    resolve_with_remote as resolve_model_with_remote, set_model_pins,
};
#[cfg(feature = "models")]
pub use models::{
    DownloadError, DownloadEvent, ModelFile, ModelKind, ModelRole, ModelSpec, ModelVariant,
    Platform, REGISTRY, RangeKind, RangeReply, Removal, ResourceTier, discard_partial,
    download_resumable, download_verified, ensure_model_dir, find as find_model, installed_size,
    interpret_range_response, is_installed, model_dir, partial_meta_path, partial_path,
    remove_model, set_model_store, sha256_hex, store_root, verify_sha256,
};
pub use provenance::Provenance;
pub use query::{
    ConfigSecretItem, ConfigSecretReport, CouplingItem, CouplingOrder, CouplingReport,
    DEFAULT_MIN_LINES, DebtDensityReport, DebtItem, DebtReport, DensityItem, DensityOrder, EdgeRef,
    Explanation, GeneratedHit, Listing, MemoryHit, NodeSummary, Path, PathHop, RedactionState,
    SCHEMA, SearchHit, SearchOptions, SearchResults, config_secrets, coupling, debt, debt_density,
    explain, list_kind, path, search, search_channels, window,
};
pub use store::{ImportApplied, SchemaAhead, Store, StoreError};
pub use sync::{
    DEFAULT_KEEP_GENERATIONS, ReclaimReport, SyncError, SyncReport, sweep_superseded, sync,
    sync_index, sync_tree, sync_worktree,
};
pub use text::{first_h1, heading_text, slugify};
pub use trust::ProducerTrust;
pub use workspace::{
    Follow, ResolvedWorkspace, Workspace, WorkspaceError, WorkspaceSet, discover_repos_under,
    parse_qualified,
};

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
#[cfg(feature = "models")]
mod models;
mod provenance;
mod query;
mod store;
mod sync;
mod workspace;

pub use artifact::{ARTIFACT_SCHEMA, GraphArtifact};
#[cfg(feature = "audio-metadata")]
pub use audio::{AUDIO_STREAM_KIND, AudioDuration, AudioFacts, AudioTag, Exactness};
pub use cache::{CacheError, ObjectCache};
pub use codegraph::{ORACLE_SCHEMA, OracleError, OracleReport, compare as compare_codegraph};
pub use config_keys::{
    ConfigKey, canonicalize as canonicalize_config_key, flatten as flatten_config, is_config_path,
    is_secret_key, is_tooling_config_path, normalize as normalize_config_key,
};
pub use context::{
    ContextRefresh, NodeContext, build_context, context, dependents, refresh_contexts,
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
pub use git::{BlobRef, ChangeStatus, ChangedFile, GitError, Repo, Submodule};
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
    AnchorState, DEFAULT_BASE_CONFIDENCE, DEFAULT_DECAY_SPAN, DEFAULT_HALF_LIFE,
    DEFAULT_MEMORY_SCOPE, Decay, MAX_MEMORY_BODY, MAX_MEMORY_SCOPE, MEMORY_SCHEMA, MemoryAnchor,
    MemoryError, MemoryFilter, MemoryForgotten, MemoryKind, MemoryListing, MemoryRecord,
    MemoryWrite, RECALL_SCHEMA, Recall, RecallOptions, Recalled, anchor_penalty,
};
pub use model::{Direction, Edge, EdgeKind, FactSet, Node, NodeKind, Span};
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
    DebtItem, DebtReport, EdgeRef, Explanation, GeneratedHit, Listing, NodeSummary, Path, PathHop,
    SCHEMA, SearchHit, SearchOptions, SearchResults, debt, explain, list_kind, path, search,
    search_channels,
};
pub use store::{ImportApplied, Store, StoreError};
pub use sync::{SyncError, SyncReport, sync, sync_index, sync_tree, sync_worktree};
pub use workspace::{
    Follow, ResolvedWorkspace, Workspace, WorkspaceError, WorkspaceSet, discover_repos_under,
    parse_qualified,
};

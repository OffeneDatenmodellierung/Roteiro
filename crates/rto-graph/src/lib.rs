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
mod cache;
mod codegraph;
mod config_keys;
mod context;
mod extract;
mod git;
#[cfg(feature = "inference")]
mod infer;
mod links;
mod markers;
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
pub use cache::{CacheError, ObjectCache};
pub use codegraph::{ORACLE_SCHEMA, OracleError, OracleReport, compare as compare_codegraph};
pub use config_keys::{
    ConfigKey, flatten as flatten_config, is_config_path, is_secret_key,
    normalize as normalize_config_key,
};
pub use context::{
    ContextRefresh, NodeContext, build_context, context, dependents, refresh_contexts,
};
pub use extract::{Extractor, FileNodeExtractor, IngestConfig, Registry, RustExtractor};
pub use git::{BlobRef, ChangeStatus, ChangedFile, GitError, Repo, Submodule};
#[cfg(feature = "inference")]
pub use infer::{
    DuplicateConfig, DuplicatePair, DuplicateReport, EMBED_REF, Embedder, HashEmbedder,
    InferenceConfig, duplicates, duplicates_with, embed, infer_edges, infer_edges_with, similarity,
};
pub use links::{
    EXTERNAL_REF_KIND, LINKS_REF, external_ref_key, external_ref_node, external_ref_target,
};
pub use model::{Direction, Edge, EdgeKind, FactSet, Node, NodeKind, Span};
#[cfg(feature = "models")]
pub use models::{
    DownloadError, ModelFile, ModelKind, ModelRole, ModelSpec, ModelVariant, Platform, REGISTRY,
    ResourceTier, download_verified, ensure_model_dir, find as find_model, is_installed, model_dir,
    set_model_store, sha256_hex, store_root, verify_sha256,
};
pub use provenance::Provenance;
pub use query::{
    DebtItem, DebtReport, EdgeRef, Explanation, Listing, NodeSummary, Path, PathHop, SCHEMA,
    SearchHit, debt, explain, list_kind, path, search,
};
pub use store::{ImportApplied, Store, StoreError};
pub use sync::{SyncError, SyncReport, sync, sync_index, sync_worktree};
pub use workspace::{Workspace, WorkspaceError, parse_qualified};

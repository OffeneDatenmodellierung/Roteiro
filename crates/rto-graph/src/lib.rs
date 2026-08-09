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
mod context;
mod extract;
mod git;
#[cfg(feature = "inference")]
mod infer;
#[cfg(feature = "inference-local-models")]
mod localmodel;
mod markers;
mod migrations;
mod model;
#[cfg(feature = "models")]
mod models;
mod provenance;
mod query;
mod store;
mod sync;

pub use artifact::{ARTIFACT_SCHEMA, GraphArtifact};
pub use cache::{CacheError, ObjectCache};
pub use codegraph::{ORACLE_SCHEMA, OracleError, OracleReport, compare as compare_codegraph};
pub use context::{
    ContextRefresh, NodeContext, build_context, context, dependents, refresh_contexts,
};
pub use extract::{Extractor, FileNodeExtractor, Registry, RustExtractor};
pub use git::{BlobRef, GitError, Repo};
#[cfg(feature = "inference")]
pub use infer::{
    DuplicateConfig, DuplicatePair, DuplicateReport, EMBED_REF, Embedder, HashEmbedder,
    InferenceConfig, duplicates, duplicates_with, embed, infer_edges, infer_edges_with, similarity,
};
#[cfg(feature = "inference-local-models")]
pub use localmodel::{GenConfig, LocalEmbedder, LocalGenerator, LocalModelError};
pub use model::{Direction, Edge, EdgeKind, FactSet, Node, NodeKind, Span};
#[cfg(feature = "models")]
pub use models::{
    ModelFile, ModelKind, ModelSpec, ModelVariant, Platform, REGISTRY, ResourceTier,
    ensure_model_dir, find as find_model, is_installed, model_dir, sha256_hex, store_root,
    verify_sha256,
};
pub use provenance::Provenance;
pub use query::{
    DebtItem, DebtReport, EdgeRef, Explanation, Listing, NodeSummary, Path, PathHop, SCHEMA,
    SearchHit, debt, explain, list_kind, path, search,
};
pub use store::{ImportApplied, Store, StoreError};
pub use sync::{SyncError, SyncReport, sync, sync_worktree};

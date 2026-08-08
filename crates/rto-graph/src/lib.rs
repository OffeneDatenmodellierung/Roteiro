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

mod cache;
mod extract;
mod git;
mod migrations;
mod model;
mod provenance;
mod store;
mod sync;

pub use cache::{CacheError, ObjectCache};
pub use extract::{Extractor, FileNodeExtractor};
pub use git::{BlobRef, GitError, Repo};
pub use model::{Direction, Edge, EdgeKind, FactSet, Node, NodeKind, Span};
pub use provenance::Provenance;
pub use store::{Store, StoreError};
pub use sync::{SyncError, SyncReport, sync};

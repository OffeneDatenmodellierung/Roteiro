//! Provenance-tagged knowledge graph store.
//!
//! Every edge in a Roteiro graph carries a [`Provenance`] tag recording how it
//! was produced: deterministically derived from source ASTs, authored by a
//! human or agent in an ADR/blueprint, or inferred heuristically from docs and
//! other artifacts. See ADR-0001.

mod provenance;
mod store;

pub use provenance::Provenance;
pub use store::{Store, StoreError};

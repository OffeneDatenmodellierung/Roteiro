//! SQLite-backed graph store.

use std::path::Path;

use rusqlite::Connection;

/// Errors raised by the store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Underlying SQLite failure.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// A Roteiro graph store backed by a single SQLite database.
pub struct Store {
    conn: Connection,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS nodes (
    id INTEGER PRIMARY KEY,
    kind TEXT NOT NULL,
    name TEXT NOT NULL,
    path TEXT,
    blob_hash TEXT
);
CREATE TABLE IF NOT EXISTS edges (
    id INTEGER PRIMARY KEY,
    src INTEGER NOT NULL REFERENCES nodes(id),
    dst INTEGER NOT NULL REFERENCES nodes(id),
    kind TEXT NOT NULL,
    provenance TEXT NOT NULL CHECK (provenance IN ('derived','authored','inferred')),
    confidence REAL
);
CREATE INDEX IF NOT EXISTS idx_edges_src ON edges(src);
CREATE INDEX IF NOT EXISTS idx_edges_dst ON edges(dst);
";

impl Store {
    /// Open (creating if absent) a store at `path` and apply the schema.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] if the database cannot be opened or the
    /// schema cannot be applied.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    /// Open an in-memory store (tests, previews).
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] if the schema cannot be applied.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    /// Number of nodes currently in the store.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on query failure.
    pub fn node_count(&self) -> Result<u64, StoreError> {
        let n: u64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::Store;

    #[test]
    fn open_in_memory_applies_schema() {
        let store = Store::open_in_memory().expect("open");
        assert_eq!(store.node_count().expect("count"), 0);
    }
}

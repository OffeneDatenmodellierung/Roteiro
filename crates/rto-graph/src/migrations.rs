//! Ordered, append-only schema migrations.
//!
//! Each [`Migration`] has a monotonically increasing `version` and a body of
//! SQL. On [`apply`], migrations with a version greater than the highest one
//! recorded in `schema_migrations` are run — each in its own transaction — and
//! then recorded. Applying twice is a no-op, so opening a store is idempotent.
//!
//! **Never edit the SQL of a shipped migration; always append a new one.**

use rusqlite::Connection;

/// A single schema migration.
pub(crate) struct Migration {
    /// Monotonic version; the first migration is `1`.
    pub version: u32,
    /// SQL executed as a batch when this migration is applied.
    pub sql: &'static str,
}

/// Migration 1: the initial provenance-tagged node/edge schema.
const M0001_INITIAL: &str = "
CREATE TABLE nodes (
    id         INTEGER PRIMARY KEY,
    key        TEXT NOT NULL UNIQUE,
    kind       TEXT NOT NULL,
    name       TEXT NOT NULL,
    path       TEXT,
    lang       TEXT,
    blob_hash  TEXT,
    span_start INTEGER,
    span_end   INTEGER,
    meta       TEXT NOT NULL DEFAULT 'null'
);
CREATE TABLE edges (
    id         INTEGER PRIMARY KEY,
    src        INTEGER NOT NULL REFERENCES nodes(id),
    dst        INTEGER NOT NULL REFERENCES nodes(id),
    kind       TEXT NOT NULL,
    provenance TEXT NOT NULL CHECK (provenance IN ('derived','authored','inferred')),
    confidence REAL,
    src_ref    TEXT,
    -- A confidence score is present exactly when the edge is inferred,
    -- and when present it lies in [0.0, 1.0]. (Rust `Edge::is_valid` is the
    -- primary guard and also rejects NaN/inf; this is defence in depth.)
    CHECK ((provenance = 'inferred') = (confidence IS NOT NULL)),
    CHECK (confidence IS NULL OR (confidence >= 0.0 AND confidence <= 1.0))
);
CREATE INDEX idx_nodes_kind ON nodes(kind);
CREATE INDEX idx_edges_src ON edges(src);
CREATE INDEX idx_edges_dst ON edges(dst);
CREATE INDEX idx_edges_provenance ON edges(provenance);
";

/// Migration 2: single-row table recording the last synced `HEAD` tree id, so
/// an unchanged tree can be detected as a no-op.
const M0002_SYNC_STATE: &str = "
CREATE TABLE sync_state (
    id   INTEGER PRIMARY KEY CHECK (id = 0),
    tree TEXT NOT NULL
);
";

/// Migration 3: make edges a set — unique by `(src, dst, kind, provenance)`.
/// The graph is a set of relationships, not a multiset, and this makes
/// re-applying a fact set (e.g. the authored layer on top of an unchanged
/// derived graph) idempotent instead of duplicating edges.
const M0003_EDGE_UNIQUE: &str = "
DELETE FROM edges WHERE id NOT IN (
    SELECT MIN(id) FROM edges GROUP BY src, dst, kind, provenance
);
CREATE UNIQUE INDEX idx_edges_unique ON edges(src, dst, kind, provenance);
";

/// Migration 4: durable import layers. `sync`'s full rebuild wipes `nodes`/
/// `edges`, so facts applied by an `import` (Graphify, lat.md, …) would be lost
/// on the next code-changing sync. Persist each import's `FactSet` here, keyed by
/// its `src_ref`, so `build_graph` can re-apply it after every rebuild. This
/// table is never touched by `rebuild`, so imported knowledge is durable.
const M0004_IMPORTS: &str = "
CREATE TABLE imports (
    src_ref     TEXT PRIMARY KEY,
    facts       TEXT NOT NULL,
    imported_at TEXT NOT NULL DEFAULT (datetime('now'))
);
";

/// Migration 5: the per-node context cache. Each row holds a node's assembled
/// context bundle (`json`) and a `fingerprint` derived from the node's own
/// content **and** its one-hop neighbourhood, so a change to the node or any
/// neighbour invalidates the entry (a cache miss on next read). Like `imports`,
/// this table is *not* touched by `rebuild`, so cached context survives a
/// code-changing sync and is invalidated only by fingerprint — a stale entry for
/// a deleted node is pruned on refresh.
const M0005_NODE_CONTEXT: &str = "
CREATE TABLE node_context (
    key         TEXT PRIMARY KEY,
    fingerprint TEXT NOT NULL,
    json        TEXT NOT NULL
);
";

/// Migration 6: tag each node with the layer that produced it, mirroring
/// `edges.provenance`. `sync` owns the *derived* layer; `check`/`import` own the
/// *authored*/*inferred* layers. Existing rows were all written by producers that
/// are derived (extraction) or re-applied every build (authored/import), so
/// back-filling `'derived'` is correct for the derived majority and harmless for
/// the rest, which are rewritten with their true provenance on the next
/// `check`/`reapply_imports`. A future incremental `sync` uses this to delete/
/// replace only derived nodes for changed paths, leaving other layers intact.
const M0006_NODE_PROVENANCE: &str = "
ALTER TABLE nodes ADD COLUMN provenance TEXT NOT NULL DEFAULT 'derived'
    CHECK (provenance IN ('derived','authored','inferred'));
CREATE INDEX idx_nodes_provenance ON nodes(provenance);
";

/// The ordered list of all migrations. Append only.
pub(crate) const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: M0001_INITIAL,
    },
    Migration {
        version: 2,
        sql: M0002_SYNC_STATE,
    },
    Migration {
        version: 3,
        sql: M0003_EDGE_UNIQUE,
    },
    Migration {
        version: 4,
        sql: M0004_IMPORTS,
    },
    Migration {
        version: 5,
        sql: M0005_NODE_CONTEXT,
    },
    Migration {
        version: 6,
        sql: M0006_NODE_PROVENANCE,
    },
];

/// The highest migration version known to this build.
#[cfg(test)]
pub(crate) fn latest_version() -> u32 {
    MIGRATIONS.iter().map(|m| m.version).max().unwrap_or(0)
}

/// Apply every migration newer than the recorded schema version. Idempotent.
pub(crate) fn apply(conn: &mut Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version    INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
    )?;
    let current: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |r| r.get(0),
    )?;
    for m in MIGRATIONS {
        if i64::from(m.version) > current {
            let tx = conn.transaction()?;
            tx.execute_batch(m.sql)?;
            tx.execute(
                "INSERT INTO schema_migrations (version) VALUES (?1)",
                [m.version],
            )?;
            tx.commit()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{apply, latest_version};
    use rusqlite::Connection;

    fn recorded_versions(conn: &Connection) -> Vec<u32> {
        let mut stmt = conn
            .prepare("SELECT version FROM schema_migrations ORDER BY version")
            .expect("prepare");
        stmt.query_map([], |r| r.get::<_, u32>(0))
            .expect("query")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect")
    }

    #[test]
    fn apply_is_idempotent() {
        let mut conn = Connection::open_in_memory().expect("open");
        apply(&mut conn).expect("first apply");
        let after_first = recorded_versions(&conn);
        apply(&mut conn).expect("second apply");
        let after_second = recorded_versions(&conn);

        assert_eq!(after_first, after_second, "re-applying must not add rows");
        assert_eq!(
            after_second.last().copied(),
            Some(latest_version()),
            "schema should be at the latest version"
        );
    }

    #[test]
    fn apply_creates_core_tables() {
        let mut conn = Connection::open_in_memory().expect("open");
        apply(&mut conn).expect("apply");
        for table in ["nodes", "edges", "imports", "schema_migrations"] {
            let count: i64 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                    |r| r.get(0),
                )
                .expect("query");
            assert_eq!(count, 1, "table {table} should exist");
        }
    }
}

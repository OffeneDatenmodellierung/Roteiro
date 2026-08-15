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
/// *authored*/*inferred* layers. Back-filling `'derived'` is correct for the
/// derived majority (extraction). Authored/import rows are re-tagged with their
/// true provenance on the next build: `check` re-applies ADR/blueprint facts
/// (now tagged authored), and `reapply_imports` re-upserts import nodes, which
/// the store repairs to non-derived on load (an import node is never derived), so
/// a legacy persisted import layer — whose serialized nodes predate this column —
/// is not left mislabelled. A future incremental `sync` uses the tag to delete/
/// replace only derived nodes for changed paths, leaving other layers intact.
const M0006_NODE_PROVENANCE: &str = "
ALTER TABLE nodes ADD COLUMN provenance TEXT NOT NULL DEFAULT 'derived'
    CHECK (provenance IN ('derived','authored','inferred'));
CREATE INDEX idx_nodes_provenance ON nodes(provenance);
";

/// Migration 7: record the extraction *identity* alongside the synced tree.
/// Extraction output depends on more than (path, bytes) — the extractor code
/// version (`EXTRACT_VERSION`) and its environment (installed image models,
/// ingestion toggles; see `Extractor::env_tag`), the same components the content
/// cache keys on. The incremental
/// committed `sync` reconstructs unchanged paths' facts from the store (extracted
/// under the *previous* env), so it is only sound when the env is unchanged;
/// otherwise it must fall back to a full re-extraction. Persisting the env lets
/// `sync` detect that. Nullable: a legacy row (or a non-committed sync) leaves it
/// `NULL`, which reads as \"unknown\" and forces the safe full path.
const M0007_SYNC_ENV: &str = "
ALTER TABLE sync_state ADD COLUMN env TEXT;
";

/// Migration 8: the analyzer-findings artifact store (ADR-0012).
///
/// Analyzer output (`cargo-audit`, `semgrep`, successors) is asserted by an
/// external tool at a point in time, against rules and an advisory database that
/// change independently of the source tree. That is a *fourth* production model,
/// not one of the graph's three provenance classes, so it gets its own tables and
/// never touches `nodes`/`edges` — which keeps `export_factset` (and the published
/// `GraphArtifact`) a pure function of the tree, and keeps findings off the
/// `authored` relevance boost in `search`.
///
/// An **analysis run** records the execution plus everything needed to reproduce
/// or distrust it. `layer` is `security:<analyzer>:<worktree-id>` and is UNIQUE:
/// exactly one run is live per layer, and a re-ingest replaces it wholesale, so a
/// finding that has been fixed disappears rather than lingering. The UNIQUE index
/// is also the entry point for "list live findings for this worktree/analyzer";
/// `idx_analysis_runs_analyzer` serves the same question across worktrees.
///
/// `ingested_at` is written for humans and **never read** — matching how
/// `imports.imported_at` already behaves; no ordering or policy depends on a clock.
///
/// A **finding** belongs to a run and carries a stable identity key so the same
/// issue is recognisable across runs. The analyzer id lives on the run, not
/// repeated on every finding row. `ON DELETE CASCADE` is defence in depth only —
/// `Store::replace_findings_layer` deletes the owned rows explicitly, because the
/// established import path deletes edges but *not* obsolete owned nodes and that
/// gap must not be inherited here.
const M0008_FINDINGS: &str = "
CREATE TABLE analysis_runs (
    id                       INTEGER PRIMARY KEY,
    layer                    TEXT NOT NULL UNIQUE,
    analyzer                 TEXT NOT NULL,
    analyzer_version         TEXT NOT NULL,
    runner                   TEXT NOT NULL CHECK (runner IN ('ingested','subprocess','sandboxed')),
    isolation                TEXT NOT NULL CHECK (isolation IN ('ingested','microvm','none')),
    image_digest             TEXT,
    rules_digest             TEXT,
    advisory_db_digest       TEXT,
    advisory_db_published_at TEXT,
    command_policy           TEXT NOT NULL,
    source_commit            TEXT,
    source_tree              TEXT,
    source_lockfile_blob     TEXT,
    started_at               TEXT NOT NULL,
    ended_at                 TEXT NOT NULL,
    exit_status              INTEGER NOT NULL,
    report_digest            TEXT NOT NULL,
    ingested_at              TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_analysis_runs_analyzer ON analysis_runs(analyzer);
CREATE TABLE findings (
    id         INTEGER PRIMARY KEY,
    run_id     INTEGER NOT NULL REFERENCES analysis_runs(id) ON DELETE CASCADE,
    key        TEXT NOT NULL,
    rule       TEXT NOT NULL,
    severity   TEXT NOT NULL,
    title      TEXT NOT NULL,
    message    TEXT NOT NULL,
    path       TEXT,
    span_start INTEGER,
    span_end   INTEGER,
    meta       TEXT NOT NULL DEFAULT 'null',
    -- A span is present as a pair or not at all, and never runs backwards.
    CHECK ((span_start IS NULL) = (span_end IS NULL)),
    CHECK (span_start IS NULL OR span_end >= span_start)
);
CREATE UNIQUE INDEX idx_findings_run_key ON findings(run_id, key);
CREATE INDEX idx_findings_run_severity ON findings(run_id, severity);
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
    Migration {
        version: 7,
        sql: M0007_SYNC_ENV,
    },
    Migration {
        version: 8,
        sql: M0008_FINDINGS,
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
    use super::{MIGRATIONS, apply, latest_version};
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

    /// An existing store must gain the findings tables without disturbing what it
    /// already holds. Migration discipline is append-only, so this is the shape
    /// every future migration has to satisfy too: apply the previously shipped
    /// set, put data in, apply the rest, and find the data untouched.
    #[test]
    fn a_later_migration_is_additive_on_a_populated_store() {
        let mut conn = Connection::open_in_memory().expect("open");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                version    INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL DEFAULT (datetime('now'))
            );",
        )
        .expect("bootstrap");
        for m in MIGRATIONS.iter().take_while(|m| m.version < 8) {
            conn.execute_batch(m.sql).expect("legacy migration");
            conn.execute(
                "INSERT INTO schema_migrations (version) VALUES (?1)",
                [m.version],
            )
            .expect("record");
        }
        conn.execute(
            "INSERT INTO nodes (key, kind, name) VALUES ('sym:rust:a.rs#main', 'fn', 'main')",
            [],
        )
        .expect("seed node");

        apply(&mut conn).expect("upgrade");

        assert_eq!(recorded_versions(&conn).last().copied(), Some(8));
        let nodes: i64 = conn
            .query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))
            .expect("count");
        assert_eq!(nodes, 1, "an upgrade must not disturb existing rows");
        let findings: i64 = conn
            .query_row("SELECT COUNT(*) FROM findings", [], |r| r.get(0))
            .expect("count");
        assert_eq!(findings, 0, "the new tables start empty");
    }

    /// The `analysis_runs` CHECK constraints are the last line of defence for the
    /// stored vocabulary: a runner kind or isolation label outside the known set
    /// is a corrupt write, not a new feature.
    #[test]
    fn findings_schema_rejects_unknown_runner_and_isolation_tokens() {
        let mut conn = Connection::open_in_memory().expect("open");
        apply(&mut conn).expect("apply");
        let insert = |runner: &str, isolation: &str| {
            conn.execute(
                "INSERT INTO analysis_runs (
                     layer, analyzer, analyzer_version, runner, isolation, command_policy,
                     started_at, ended_at, exit_status, report_digest
                 ) VALUES ('security:a:b', 'a', '1', ?1, ?2, '{}', 's', 'e', 0, 'd')",
                [runner, isolation],
            )
        };
        assert!(insert("ingested", "ingested").is_ok());
        assert!(insert("teleported", "ingested").is_err());
        assert!(insert("ingested", "airgapped").is_err());
    }

    #[test]
    fn apply_creates_core_tables() {
        let mut conn = Connection::open_in_memory().expect("open");
        apply(&mut conn).expect("apply");
        for table in [
            "nodes",
            "edges",
            "imports",
            "analysis_runs",
            "findings",
            "schema_migrations",
        ] {
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

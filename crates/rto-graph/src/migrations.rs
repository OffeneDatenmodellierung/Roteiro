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

/// Migration 9: the generated-media-content artifact store (ADR-0015).
///
/// An ASR transcript or a VLM description is **generated**, not decoded: asked to
/// transcribe digital silence a model returns fluent invented prose, and the same
/// blob under a different model, quantisation or sampling yields different
/// "facts". That is not a deterministic pure function of `(path, blob id, bytes)`,
/// so it is not a `derived` fact and must not be stored as one — it gets its own
/// table here and never touches `nodes`/`edges`, exactly as analyzer findings do
/// in migration 8. OCR stays on the derived path: it decodes text that is
/// actually present, so it has no row here and `kind` has no token for it.
///
/// **Keyed by `(blob_id, producer)`**, and that UNIQUE index is the whole design:
/// the producer column is a rendered identity over the model, its digest, its
/// quantisation, the projector digest, the prompt and the sampling parameters, so
/// re-describing a blob with a better model inserts a **new row beside the old
/// one** rather than overwriting it. You can compare the two, and you can drop
/// one producer's output wholesale without touching the graph.
///
/// The identity components are stored as columns rather than folded only into
/// `producer`, because a record must be legible — and distrustable — on its own:
/// `media status` reports which model said what, and a row whose evidence lived
/// only inside an opaque token could not answer that.
///
/// `produced_at` is written by `SQLite`, as `imports.imported_at` and
/// `analysis_runs.ingested_at` already are. Unlike those two it *is* read, but
/// only for display; no ordering or policy depends on a clock.
///
/// Records are **not** touched by `rebuild` (which deletes only `edges` and
/// `nodes`), following the `imports` precedent: they are expensive to reproduce —
/// a 715 MB projector load per blob, issue #301 — and are not derivable from
/// source alone.
const M0009_MEDIA_CONTENT: &str = "
CREATE TABLE media_content (
    id            INTEGER PRIMARY KEY,
    blob_id       TEXT NOT NULL,
    path          TEXT NOT NULL,
    kind          TEXT NOT NULL CHECK (kind IN ('audio','vision')),
    producer      TEXT NOT NULL,
    model         TEXT NOT NULL,
    model_digest  TEXT NOT NULL,
    quantisation  TEXT NOT NULL,
    mmproj_digest TEXT NOT NULL,
    prompt        TEXT NOT NULL,
    temperature   REAL NOT NULL,
    max_tokens    INTEGER NOT NULL,
    tool_version  TEXT NOT NULL,
    generation    INTEGER NOT NULL,
    produced_at   TEXT NOT NULL DEFAULT (datetime('now')),
    text          TEXT NOT NULL,
    confidence    REAL,
    -- A confidence signal, when a runtime exposes one, is a probability. It is
    -- **not** the score an `inferred` edge carries and must never be read as one.
    CHECK (confidence IS NULL OR (confidence >= 0.0 AND confidence <= 1.0))
);
CREATE UNIQUE INDEX idx_media_content_blob_producer ON media_content(blob_id, producer);
CREATE INDEX idx_media_content_producer ON media_content(producer);
CREATE INDEX idx_media_content_kind ON media_content(kind);
";

/// Migration 10: record the **pre-generation gate**'s refusals (ADR-0015).
///
/// A blob the gate refuses — digital silence, a flat-colour image — gets a
/// `media_content` row like any other, but one carrying the *measurement* that
/// refused it instead of generated text. Without it a skip would be an
/// indistinguishable hole, and an operator could not tell **not generated** from
/// **generated nothing**; recording an invisible skip would be its own small lie
/// in an ADR about not lying.
///
/// The table is rebuilt rather than `ALTER`ed because the point of the three new
/// columns is a constraint `ALTER TABLE` cannot add: **a skip carries a
/// measurement and no text, and a generated row is the exact converse**. That is
/// the invariant the whole change rests on — a gated blob must not put text
/// anywhere — and it belongs in the schema, not only in the Rust type that
/// happens to write it today. `text` stays `NOT NULL`, so a skip stores `''`,
/// which the `CHECK` requires.
///
/// The rebuild copies every existing row with all three columns `NULL` (every
/// pre-existing record was generated, by construction — the gate did not exist),
/// then drops the old table. Dropping it takes its indexes with it, so the three
/// are recreated verbatim below.
const M0010_MEDIA_GATE: &str = "
CREATE TABLE media_content_v2 (
    id             INTEGER PRIMARY KEY,
    blob_id        TEXT NOT NULL,
    path           TEXT NOT NULL,
    kind           TEXT NOT NULL CHECK (kind IN ('audio','vision')),
    producer       TEXT NOT NULL,
    model          TEXT NOT NULL,
    model_digest   TEXT NOT NULL,
    quantisation   TEXT NOT NULL,
    mmproj_digest  TEXT NOT NULL,
    prompt         TEXT NOT NULL,
    temperature    REAL NOT NULL,
    max_tokens     INTEGER NOT NULL,
    tool_version   TEXT NOT NULL,
    generation     INTEGER NOT NULL,
    produced_at    TEXT NOT NULL DEFAULT (datetime('now')),
    text           TEXT NOT NULL,
    confidence     REAL,
    skip_reason    TEXT CHECK (skip_reason IS NULL OR skip_reason IN ('silence','uniform')),
    skip_value     REAL,
    skip_threshold REAL,
    -- A confidence signal, when a runtime exposes one, is a probability. It is
    -- **not** the score an `inferred` edge carries and must never be read as one.
    CHECK (confidence IS NULL OR (confidence >= 0.0 AND confidence <= 1.0)),
    -- The two outcomes, and nothing in between. Either the model ran (no skip
    -- columns at all), or the gate refused the blob before it did — in which
    -- case the measurement is complete AND the row holds no generated text.
    --
    -- All three skip columns are named in BOTH branches, so they stand or fall
    -- together. `skip_reason` in particular must be named in the second branch:
    -- it is the discriminant the Rust decoder reads, so a row with a value and a
    -- threshold but no reason would come back as *generated content that happens
    -- to be empty* — the exact confusion this constraint exists to prevent.
    CHECK (
        (skip_reason IS NULL AND skip_value IS NULL AND skip_threshold IS NULL)
        OR (skip_reason IS NOT NULL AND skip_value IS NOT NULL
            AND skip_threshold IS NOT NULL AND text = '')
    )
);
INSERT INTO media_content_v2 (
    id, blob_id, path, kind, producer, model, model_digest, quantisation, mmproj_digest,
    prompt, temperature, max_tokens, tool_version, generation, produced_at, text, confidence
) SELECT
    id, blob_id, path, kind, producer, model, model_digest, quantisation, mmproj_digest,
    prompt, temperature, max_tokens, tool_version, generation, produced_at, text, confidence
FROM media_content;
DROP TABLE media_content;
ALTER TABLE media_content_v2 RENAME TO media_content;
CREATE UNIQUE INDEX idx_media_content_blob_producer ON media_content(blob_id, producer);
CREATE INDEX idx_media_content_producer ON media_content(producer);
CREATE INDEX idx_media_content_kind ON media_content(kind);
";

/// Migration 11: the episodic agent-memory artifact store (ADR-0013).
///
/// What a session *learned* — a lesson, an approach that failed, a decision, a
/// recurring failure pattern, a task outcome — has **no generating function**.
/// It cannot be re-derived from `(path, blob id, bytes)` at any price, so it is
/// not a `derived` fact; it was not written into a reviewed file, so it is not
/// `authored` either. It gets its own table here and never touches
/// `nodes`/`edges`, exactly as analyzer findings (migration 8) and generated
/// media content (migration 9) do. `EXTRACT_VERSION` is unchanged: memory is not
/// extraction output.
///
/// This is the **episodic** tier only — durable and never auto-evicted, modelling
/// what `imports` models. `rebuild` deletes only `edges` and `nodes`, so these
/// rows survive a code-changing sync by construction and are removed only by an
/// explicit `roteiro memory forget`. The bounded, evictable cache tier the ADR
/// pairs with it is a separate table in a later migration, deliberately split so
/// eviction policy can change without touching durable memory.
///
/// **`id` is the ordering key, and `AUTOINCREMENT` is load-bearing.** A plain
/// `INTEGER PRIMARY KEY` is the rowid, and `SQLite` reuses the largest deleted
/// rowid — so forgetting the newest record would hand its number to the next
/// write, making the "monotonic generation" non-monotone *and* silently
/// re-pointing any `superseded_by` that referenced it at an unrelated record.
/// `AUTOINCREMENT` never reuses an id, at the cost of one `sqlite_sequence` row.
///
/// **No column here is ranked on wall-clock.** `created_at` is written for humans
/// and never read, exactly as `imports.imported_at` behaves — the store is
/// per-repo and shared across worktrees and branches, so concurrent checkouts
/// produce non-monotone times, and `datetime('now')` ties on intra-second writes.
/// `superseded_at` is the same kind of value: display, never policy. The logical
/// fact of supersession is `superseded_by`, which is an id, which is a generation.
///
/// **The anchor is `(anchor_key, anchor_blob)`, never a span.** A span is byte
/// offsets and shifts on any edit above it; a node key plus the blob hash captured
/// when the record was written is stable, and is what lets a read say *the anchor
/// vanished* (no such node) apart from *the code changed underneath* (a different
/// blob). Anchor state is **computed on read and never stored**, and a record
/// whose anchor drifted is kept and marked — never pruned. That is a deliberate
/// departure from the authored layer's prune rule: a lesson about deleted code is
/// often the most valuable one there is.
///
/// Two constraints exist to make half-states unrepresentable, in the spirit of
/// migration 10's outcome CHECK:
///
/// * `superseded_by` and `superseded_at` stand or fall **together**. A row with a
///   timestamp and no successor would be a record dropped from live listing with
///   no auditable reason — supersession inferred rather than recorded, which is
///   the one thing ADR-0013 rules out.
/// * `anchor_blob`/`anchor_path` require an `anchor_key`. A blob hash with no node
///   key names nothing and can never be checked for drift.
const M0011_AGENT_MEMORY: &str = "
CREATE TABLE agent_memory (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    scope         TEXT NOT NULL,
    kind          TEXT NOT NULL
                  CHECK (kind IN ('lesson','attempt','decision','pattern','outcome')),
    anchor_key    TEXT,
    anchor_blob   TEXT,
    anchor_path   TEXT,
    body          TEXT NOT NULL,
    confidence    REAL,
    tree          TEXT,
    created_at    TEXT NOT NULL DEFAULT (datetime('now')),
    superseded_by INTEGER REFERENCES agent_memory(id),
    superseded_at TEXT,
    -- A scope and a body that are present but empty are corrupt writes, not
    -- minimal ones: an empty memory records nothing and can never be recalled.
    CHECK (scope <> ''),
    CHECK (body <> ''),
    -- A self-reported confidence, when a caller offers one, is a probability. It
    -- is **not** the score an `inferred` edge carries and must never be read as
    -- one: no memory record is a graph fact.
    CHECK (confidence IS NULL OR (confidence >= 0.0 AND confidence <= 1.0)),
    -- Anchor evidence without an anchor key names nothing.
    CHECK (anchor_blob IS NULL OR anchor_key IS NOT NULL),
    CHECK (anchor_path IS NULL OR anchor_key IS NOT NULL),
    -- Supersession is recorded, never inferred: the successor and the moment
    -- stand or fall together, and nothing supersedes itself.
    CHECK ((superseded_by IS NULL) = (superseded_at IS NULL)),
    CHECK (superseded_by IS NULL OR superseded_by <> id)
);
CREATE INDEX idx_mem_anchor ON agent_memory(anchor_key);
CREATE INDEX idx_mem_live ON agent_memory(scope, superseded_by, id DESC);
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
    Migration {
        version: 9,
        sql: M0009_MEDIA_CONTENT,
    },
    Migration {
        version: 10,
        sql: M0010_MEDIA_GATE,
    },
    Migration {
        version: 11,
        sql: M0011_AGENT_MEMORY,
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

    /// An existing store must gain the newest migration's tables without
    /// disturbing what it already holds. Migration discipline is append-only, so
    /// this is the shape every future migration has to satisfy: apply the
    /// previously shipped set, put data in, apply the rest, and find the data
    /// untouched.
    ///
    /// Written against [`latest_version`] rather than a hard-coded number, so the
    /// next migration is covered by this test the moment it is appended instead of
    /// leaving the newest one — the only untested one — unchecked.
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
        for m in MIGRATIONS
            .iter()
            .take_while(|m| m.version < latest_version())
        {
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

        assert_eq!(
            recorded_versions(&conn).last().copied(),
            Some(latest_version())
        );
        let nodes: i64 = conn
            .query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))
            .expect("count");
        assert_eq!(nodes, 1, "an upgrade must not disturb existing rows");
        for table in ["findings", "media_content", "agent_memory"] {
            let rows: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .expect("count");
            assert_eq!(rows, 0, "{table} starts empty");
        }
    }

    /// Insert one `media_content` row with the given outcome columns, returning
    /// whether `SQLite` accepted it. `blob` keys the row, so each attempt needs
    /// its own (the table is UNIQUE on `(blob_id, producer)`).
    fn insert_outcome(
        conn: &Connection,
        blob: &str,
        text: &str,
        skip_reason: Option<&str>,
        skip_value: Option<f64>,
        skip_threshold: Option<f64>,
    ) -> bool {
        conn.execute(
            "INSERT INTO media_content (
                 blob_id, path, kind, producer, model, model_digest, quantisation,
                 mmproj_digest, prompt, temperature, max_tokens, tool_version, generation,
                 text, skip_reason, skip_value, skip_threshold
             ) VALUES (
                 ?1, 'assets/clip.wav', 'audio', 'media:audio:m:0', 'm', 'd', 'Q4_K_M',
                 'p', 'prompt', 0.0, 512, '9.9.9', 1, ?2, ?3, ?4, ?5
             )",
            rusqlite::params![blob, text, skip_reason, skip_value, skip_threshold],
        )
        .is_ok()
    }

    /// **The outcome constraint has to bite.** A `media_content` row is either a
    /// generated description or a recorded gate refusal, and migration 10 exists
    /// to make anything between the two unrepresentable *in the schema* — not
    /// only in the Rust type that writes it today.
    ///
    /// The case that motivated tightening it is the first rejection below: a row
    /// with a measurement but **no reason**. `skip_reason` is the discriminant
    /// the decoder reads, so such a row comes back as generated content that
    /// happens to be empty — a silent lie of exactly the kind ADR-0015 exists to
    /// stop, arrived at through a `NULL` rather than through any code path.
    #[test]
    fn the_media_outcome_constraint_admits_only_the_two_real_outcomes() {
        /// `(name, text, reason, value, threshold)` for a shape that must be
        /// refused.
        type Rejected = (
            &'static str,
            &'static str,
            Option<&'static str>,
            Option<f64>,
            Option<f64>,
        );

        let mut conn = Connection::open_in_memory().expect("open");
        apply(&mut conn).expect("apply");

        // The two legitimate shapes insert.
        assert!(
            insert_outcome(&conn, "blob-generated", "a transcript", None, None, None),
            "a generated record carries text and no measurement",
        );
        assert!(
            insert_outcome(
                &conn,
                "blob-skipped",
                "",
                Some("silence"),
                Some(0.0),
                Some(1e-4)
            ),
            "a gated skip carries a complete measurement and no text",
        );

        let rejected: [Rejected; 6] = [
            // The hole this test was written for: a measurement with no reason
            // reads back as an empty *generated* record.
            (
                "value-and-threshold-but-no-reason",
                "",
                None,
                Some(0.0),
                Some(1e-4),
            ),
            (
                "reason-with-no-value",
                "",
                Some("silence"),
                None,
                Some(1e-4),
            ),
            (
                "reason-with-no-threshold",
                "",
                Some("silence"),
                Some(0.0),
                None,
            ),
            ("value-alone", "", None, Some(0.0), None),
            ("threshold-alone", "", None, None, Some(1e-4)),
            // A skip that also claims text: the other half of the exclusion.
            (
                "a-skip-that-also-has-text",
                "a transcript",
                Some("silence"),
                Some(0.0),
                Some(1e-4),
            ),
        ];
        for (name, text, reason, value, threshold) in rejected {
            assert!(
                !insert_outcome(&conn, name, text, reason, value, threshold),
                "`{name}` must be rejected by the outcome CHECK, not stored",
            );
        }

        // Only the two accepted rows are there.
        let stored: i64 = conn
            .query_row("SELECT COUNT(*) FROM media_content", [], |r| r.get(0))
            .expect("count");
        assert_eq!(stored, 2, "no refused shape may have slipped through");
    }

    /// ADR-0015 adds an artifact store, **not** a provenance class. The three
    /// tokens `nodes`/`edges` accept are a published contract, and migration 9
    /// must leave them exactly as migration 1 and 6 defined them — so the check
    /// is that the constraints still bite, on a store at the latest version.
    #[test]
    fn the_media_migration_leaves_the_provenance_vocabulary_alone() {
        let mut conn = Connection::open_in_memory().expect("open");
        apply(&mut conn).expect("apply");

        // The three legitimate node provenances still insert…
        for provenance in ["derived", "authored", "inferred"] {
            conn.execute(
                "INSERT INTO nodes (key, kind, name, provenance) VALUES (?1, 'fn', 'n', ?2)",
                [provenance, provenance],
            )
            .unwrap_or_else(|e| panic!("{provenance} must remain a valid node provenance: {e}"));
        }
        // …and nothing else does, on either table. A `generated` provenance is
        // precisely the thing this ADR declined to add.
        for rejected in ["generated", "media", ""] {
            assert!(
                conn.execute(
                    "INSERT INTO nodes (key, kind, name, provenance) VALUES (?1, 'fn', 'n', ?2)",
                    [&format!("bad-{rejected}"), rejected],
                )
                .is_err(),
                "{rejected:?} must not be an accepted node provenance"
            );
            assert!(
                conn.execute(
                    "INSERT INTO edges (src, dst, kind, provenance) VALUES (1, 1, 'calls', ?1)",
                    [rejected],
                )
                .is_err(),
                "{rejected:?} must not be an accepted edge provenance"
            );
        }

        // And the media table has no provenance column to borrow one from.
        let columns: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT name FROM pragma_table_info('media_content')")
                .expect("prepare");
            stmt.query_map([], |r| r.get::<_, String>(0))
                .expect("query")
                .collect::<Result<_, _>>()
                .expect("collect")
        };
        assert!(
            !columns.iter().any(|c| c == "provenance"),
            "generated content must not carry a provenance class: {columns:?}"
        );
    }

    /// `(blob, producer)` is the identity, and the schema — not just the Rust —
    /// enforces it: the same blob described by a *different* producer is a second
    /// row, while the same producer twice is refused outright.
    #[test]
    fn media_content_is_unique_per_blob_and_producer() {
        let mut conn = Connection::open_in_memory().expect("open");
        apply(&mut conn).expect("apply");
        let insert = |producer: &str, kind: &str| {
            conn.execute(
                "INSERT INTO media_content (
                     blob_id, path, kind, producer, model, model_digest, quantisation,
                     mmproj_digest, prompt, temperature, max_tokens, tool_version, generation, text
                 ) VALUES ('blob1', 'a.wav', ?2, ?1, 'm', 'd', 'Q4', 'p', 'say', 0.0, 8, '1.0', 1, 't')",
                [producer, kind],
            )
        };
        assert!(insert("media:audio:m:1", "audio").is_ok());
        assert!(
            insert("media:audio:m:1", "audio").is_err(),
            "the same producer must not describe one blob twice"
        );
        assert!(
            insert("media:audio:m:2", "audio").is_ok(),
            "a different producer is a new record, not a mutation"
        );
        assert!(
            insert("media:vision:m:3", "ocr").is_err(),
            "`ocr` is not a generative modality and has no token"
        );
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

    /// Insert one `agent_memory` row with the given columns, returning whether
    /// `SQLite` accepted it.
    fn insert_memory(conn: &Connection, scope: &str, kind: &str, body: &str) -> bool {
        conn.execute(
            "INSERT INTO agent_memory (scope, kind, body) VALUES (?1, ?2, ?3)",
            [scope, kind, body],
        )
        .is_ok()
    }

    /// **The stored vocabulary has to bite.** A memory `kind` outside the five
    /// ADR-0013 names is a corrupt write, not a new feature — the same rule the
    /// `analysis_runs` runner/isolation tokens live under. An empty scope or body
    /// is likewise refused: a memory that records nothing can never be recalled,
    /// and storing it would only make `memory list` lie about how much is there.
    #[test]
    fn agent_memory_constrains_its_vocabulary_scope_and_body() {
        let mut conn = Connection::open_in_memory().expect("open");
        apply(&mut conn).expect("apply");

        for kind in ["lesson", "attempt", "decision", "pattern", "outcome"] {
            assert!(
                insert_memory(&conn, "repo", kind, "a body"),
                "`{kind}` must remain a valid memory kind",
            );
        }
        for kind in ["note", "Lesson", "lessons", "derived", ""] {
            assert!(
                !insert_memory(&conn, "repo", kind, "a body"),
                "{kind:?} must not be an accepted memory kind",
            );
        }
        assert!(!insert_memory(&conn, "", "lesson", "a body"), "empty scope");
        assert!(!insert_memory(&conn, "repo", "lesson", ""), "empty body");

        // A self-reported confidence is a probability, and is not the score an
        // `inferred` edge carries.
        let with_confidence = |c: f64| {
            conn.execute(
                "INSERT INTO agent_memory (scope, kind, body, confidence)
                 VALUES ('repo', 'lesson', 'b', ?1)",
                [c],
            )
            .is_ok()
        };
        assert!(with_confidence(0.0) && with_confidence(1.0));
        assert!(!with_confidence(-0.1) && !with_confidence(1.1));
    }

    /// **Supersession is recorded, never inferred** — so the successor and the
    /// moment it happened stand or fall together, and nothing supersedes itself.
    /// A row with a `superseded_at` and no `superseded_by` would be a record
    /// dropped out of live listing with no auditable reason, which is exactly the
    /// state ADR-0013 exists to rule out.
    #[test]
    fn agent_memory_supersession_columns_stand_or_fall_together() {
        let mut conn = Connection::open_in_memory().expect("open");
        apply(&mut conn).expect("apply");
        assert!(insert_memory(&conn, "repo", "lesson", "the old finding"));
        assert!(insert_memory(&conn, "repo", "lesson", "the new finding"));

        let update = |by: Option<i64>, at: Option<&str>| {
            conn.execute(
                "UPDATE agent_memory SET superseded_by = ?1, superseded_at = ?2 WHERE id = 1",
                rusqlite::params![by, at],
            )
            .is_ok()
        };
        assert!(!update(Some(2), None), "a successor with no moment");
        assert!(
            !update(None, Some("2026-01-01")),
            "a moment with no successor"
        );
        assert!(!update(Some(1), Some("2026-01-01")), "self-supersession");
        assert!(
            update(Some(2), Some("2026-01-01")),
            "the one legitimate shape"
        );
        assert!(
            update(None, None),
            "and clearing it again is legitimate too"
        );
    }

    /// Anchor evidence with no anchor key names nothing and can never be checked
    /// for drift, so the schema refuses it outright.
    #[test]
    fn agent_memory_refuses_anchor_evidence_without_a_key() {
        let mut conn = Connection::open_in_memory().expect("open");
        apply(&mut conn).expect("apply");
        let insert = |key: Option<&str>, blob: Option<&str>, path: Option<&str>| {
            conn.execute(
                "INSERT INTO agent_memory (scope, kind, body, anchor_key, anchor_blob, anchor_path)
                 VALUES ('repo', 'lesson', 'b', ?1, ?2, ?3)",
                rusqlite::params![key, blob, path],
            )
            .is_ok()
        };
        assert!(
            insert(None, None, None),
            "an unanchored memory is legitimate"
        );
        assert!(insert(Some("sym:rust:a.rs#f"), Some("blob1"), Some("a.rs")));
        assert!(!insert(None, Some("blob1"), None), "a blob naming no node");
        assert!(!insert(None, None, Some("a.rs")), "a path naming no node");
    }

    /// **`AUTOINCREMENT` is not decoration.** `id` is the monotonic generation
    /// ADR-0013 ranks on instead of a clock, and a plain `INTEGER PRIMARY KEY`
    /// would reuse the largest deleted rowid — so forgetting the newest record
    /// would hand its number straight to the next write. That breaks monotonicity
    /// *and* silently re-points any surviving `superseded_by` at an unrelated
    /// record. This is the test that would fail if the keyword were ever dropped.
    #[test]
    fn agent_memory_ids_are_never_reused_after_a_delete() {
        let mut conn = Connection::open_in_memory().expect("open");
        apply(&mut conn).expect("apply");
        assert!(insert_memory(&conn, "repo", "lesson", "first"));
        assert!(insert_memory(&conn, "repo", "lesson", "second"));
        conn.execute("DELETE FROM agent_memory WHERE id = 2", [])
            .expect("delete the newest");
        assert!(insert_memory(&conn, "repo", "lesson", "third"));

        let id: i64 = conn
            .query_row(
                "SELECT id FROM agent_memory WHERE body = 'third'",
                [],
                |r| r.get(0),
            )
            .expect("query");
        assert_eq!(id, 3, "a forgotten id must never be handed out again");
    }

    /// ADR-0013 adds an artifact store, **not** a provenance class. Memory has no
    /// source blob and was not written into a reviewed file, so it is neither
    /// `derived` nor `authored` — and the way to keep that true is for the table
    /// to have no provenance column to borrow one from, while the three tokens
    /// `nodes`/`edges` accept stay exactly as migrations 1 and 6 defined them.
    #[test]
    fn the_memory_migration_leaves_the_provenance_vocabulary_alone() {
        let mut conn = Connection::open_in_memory().expect("open");
        apply(&mut conn).expect("apply");

        for provenance in ["derived", "authored", "inferred"] {
            conn.execute(
                "INSERT INTO nodes (key, kind, name, provenance) VALUES (?1, 'fn', 'n', ?2)",
                [provenance, provenance],
            )
            .unwrap_or_else(|e| panic!("{provenance} must remain a valid node provenance: {e}"));
        }
        for rejected in ["memory", "episodic", "remembered", ""] {
            assert!(
                conn.execute(
                    "INSERT INTO nodes (key, kind, name, provenance) VALUES (?1, 'fn', 'n', ?2)",
                    [&format!("bad-{rejected}"), rejected],
                )
                .is_err(),
                "{rejected:?} must not be an accepted node provenance"
            );
            assert!(
                conn.execute(
                    "INSERT INTO edges (src, dst, kind, provenance) VALUES (1, 1, 'calls', ?1)",
                    [rejected],
                )
                .is_err(),
                "{rejected:?} must not be an accepted edge provenance"
            );
        }

        let columns: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT name FROM pragma_table_info('agent_memory')")
                .expect("prepare");
            stmt.query_map([], |r| r.get::<_, String>(0))
                .expect("query")
                .collect::<Result<_, _>>()
                .expect("collect")
        };
        assert!(
            !columns.iter().any(|c| c == "provenance"),
            "a memory record must not carry a provenance class: {columns:?}"
        );
        assert!(
            !columns.iter().any(|c| c.starts_with("span")),
            "a span is byte offsets, not an anchor: {columns:?}"
        );
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
            "media_content",
            "agent_memory",
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

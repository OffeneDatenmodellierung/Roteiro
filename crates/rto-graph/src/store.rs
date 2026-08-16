//! SQLite-backed graph store.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use crate::findings::{self, AnalysisRun, Finding, FindingsApplied, FindingsLayer};
use crate::media::{self, MediaFilter, MediaKind, MediaRecord, MediaWrite, ProducerSummary};
use crate::memory::{
    self, CacheEntry, CacheStats, CacheSweep, CacheWrite, MemoryError, MemoryFilter,
    MemoryForgotten, MemoryListing, MemoryRecord, MemoryWrite, Recall, RecallOptions,
};
use crate::migrations;
use crate::model::{Direction, Edge, EdgeKind, FactSet, Node, NodeKind, Span};
use crate::provenance::Provenance;

/// Errors raised by the store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Underlying `SQLite` failure.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// A node's `meta` could not be (de)serialized as JSON.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    /// An edge referenced a node key that does not exist in the store.
    #[error("unknown node key: {0}")]
    UnknownNode(String),
    /// An edge violated the provenance/confidence invariant.
    #[error("invalid edge: {0}")]
    InvalidEdge(String),
    /// A stored value could not be interpreted (database corruption).
    #[error("corrupt store: {0}")]
    Corrupt(String),
}

/// A summary of applying/re-applying import layers (see
/// [`Store::apply_import_layer`] and [`Store::reapply_imports`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct ImportApplied {
    /// Number of import layers processed.
    pub layers: usize,
    /// Import nodes upserted (across all layers).
    pub nodes: usize,
    /// Import edges applied — both endpoints resolved. A duplicate of an
    /// already-present edge is a harmless no-op but still counted as applied.
    pub edges_applied: usize,
    /// Import edges **pruned**: an endpoint was absent (a cross-reference to code
    /// that no longer exists), so the edge was dropped from the persisted layer
    /// rather than kept as stale data.
    pub edges_pruned: usize,
}

/// Qualified node columns for `SELECT`s that alias the `nodes` table as `n`.
const NODE_COLS: &str = "n.key, n.kind, n.name, n.path, n.lang, n.blob_hash, n.span_start, n.span_end, n.provenance, n.meta";

/// `SELECT` prefix that yields an [`Edge`] row (endpoints resolved back to keys).
const EDGE_SELECT: &str = "SELECT ns.key AS src, nd.key AS dst, e.kind, e.provenance, \
     e.confidence, e.src_ref \
     FROM edges e JOIN nodes ns ON ns.id = e.src JOIN nodes nd ON nd.id = e.dst";

/// A Roteiro graph store backed by a single `SQLite` database.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open (creating if absent) a store at `path` and apply pending migrations.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] if the database cannot be opened or a
    /// migration fails.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        Self::from_conn(conn)
    }

    /// Open an in-memory store (tests, previews).
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] if a migration fails.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        Self::from_conn(conn)
    }

    fn from_conn(mut conn: Connection) -> Result<Self, StoreError> {
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        // Wait briefly for a concurrent writer instead of failing a read with
        // `database is locked`. Matters for workspace `serve` (ADR-0008), where a
        // long-lived server reads a project's graph while that repo's own
        // `roteiro sync` commits an update to the same file. Syncs are
        // sub-second, so this only ever costs a short wait, never a lost query.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        migrations::apply(&mut conn)?;
        Ok(Self { conn })
    }

    /// The schema version this store has been migrated to.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on query failure.
    pub fn schema_version(&self) -> Result<u32, StoreError> {
        let v: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |r| r.get(0),
        )?;
        Ok(u32::try_from(v).unwrap_or(0))
    }

    /// Number of nodes currently in the store.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on query failure.
    pub fn node_count(&self) -> Result<u64, StoreError> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    /// Number of edges currently in the store.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on query failure.
    pub fn edge_count(&self) -> Result<u64, StoreError> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    /// Insert or update a node, keyed by its natural [`Node::key`].
    ///
    /// # Errors
    /// Returns [`StoreError::Json`] if `meta` cannot be serialized, or
    /// [`StoreError::Sqlite`] on write failure.
    pub fn upsert_node(&self, node: &Node) -> Result<(), StoreError> {
        upsert_node(&self.conn, node)
    }

    /// Insert an edge. Both endpoints must already resolve to nodes.
    ///
    /// # Errors
    /// Returns [`StoreError::InvalidEdge`] if the provenance/confidence
    /// invariant is violated, [`StoreError::UnknownNode`] if an endpoint key is
    /// absent, or [`StoreError::Sqlite`] on write failure.
    pub fn insert_edge(&self, edge: &Edge) -> Result<(), StoreError> {
        insert_edge(&self.conn, edge)
    }

    /// Apply a fact set atomically: all nodes are upserted, then all edges are
    /// inserted, in a single transaction. On any error nothing is committed.
    ///
    /// # Errors
    /// Returns the first error encountered (see [`Store::upsert_node`] and
    /// [`Store::insert_edge`]); the transaction is rolled back.
    pub fn apply_factset(&mut self, facts: &FactSet) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        for node in &facts.nodes {
            upsert_node(&tx, node)?;
        }
        for edge in &facts.edges {
            insert_edge(&tx, edge)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The `HEAD` tree id recorded at the last successful [`Store::rebuild`], if
    /// any. Used by the sync engine to detect an unchanged tree.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on query failure.
    pub fn sync_state(&self) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT tree FROM sync_state WHERE id = 0", [], |r| r.get(0))
            .optional()?)
    }

    /// The extractor environment recorded with the last committed [`sync`],
    /// `None` if unset (a legacy row, or the last sync was a worktree/index
    /// preview). The incremental committed `sync` compares this to the current
    /// env and falls back to a full re-extraction when they differ.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on query failure.
    pub fn sync_env(&self) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT env FROM sync_state WHERE id = 0", [], |r| r.get(0))
            .optional()?
            .flatten())
    }

    /// Record the extractor environment for the current synced tree. Called by a
    /// committed `sync` right after it writes the tree, so a later sync can decide
    /// whether the incremental fast path is sound. A no-op if no tree is recorded.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on write failure.
    pub fn set_sync_env(&self, env: &str) -> Result<(), StoreError> {
        self.conn
            .execute("UPDATE sync_state SET env = ?1 WHERE id = 0", [env])?;
        Ok(())
    }

    /// Atomically replace the entire graph with `facts`, recording `tree` as the
    /// synced state (or clearing it when `tree` is `None`). All existing nodes
    /// and edges are deleted first, so the store reflects exactly the given fact
    /// set.
    ///
    /// Passing `None` records *no* synced tree — distinct from an empty string —
    /// so [`Store::sync_state`] returns `None` and a later `sync` will not
    /// spuriously short-circuit.
    ///
    /// # Errors
    /// Returns the first error encountered (see [`Store::apply_factset`]); on any
    /// error nothing is committed.
    pub fn rebuild(&mut self, facts: &FactSet, tree: Option<&str>) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM edges", [])?;
        tx.execute("DELETE FROM nodes", [])?;
        for node in &facts.nodes {
            upsert_node(&tx, node)?;
        }
        for edge in &facts.edges {
            insert_edge(&tx, edge)?;
        }
        write_sync_state(&tx, tree)?;
        tx.commit()?;
        Ok(())
    }

    /// Bring the store to exactly `facts` (as [`Store::rebuild`] does) but writing
    /// only what **differs** instead of wiping and reinserting the whole graph —
    /// the git-style "write only the delta". Unchanged node rows (which carry the
    /// heavy JSON `meta`) and unchanged edge rows are left untouched; only removed
    /// rows are deleted and new/changed rows written. The final state — nodes,
    /// edges, and `sync_state` — is identical to `rebuild(facts, tree)`.
    ///
    /// Leaving unchanged edges in place means their row ids do not match a cold
    /// rebuild's — which is safe *because* every edge query is content-ordered
    /// (`(src, dst, kind, provenance)`, see [`Store::all_edges`]), never by row
    /// id. So an incrementally reconciled store and a fresh rebuild return every
    /// query identically; the delta is invisible above the storage layer.
    ///
    /// # Errors
    /// Returns [`StoreError`] on a query failure; the transaction is rolled back.
    pub fn reconcile(&mut self, facts: &FactSet, tree: Option<&str>) -> Result<(), StoreError> {
        let current_nodes = self.all_nodes()?;
        let current_edges = self.all_edges()?;
        let cur_by_key: std::collections::HashMap<&str, &Node> =
            current_nodes.iter().map(|n| (n.key.as_str(), n)).collect();
        let new_keys: std::collections::HashSet<&str> =
            facts.nodes.iter().map(|n| n.key.as_str()).collect();

        // Edge identity is the full tuple, so a changed `confidence`/`src_ref`
        // counts as remove-old + add-new — keeping the result identical to a
        // wholesale rebuild, not the store's insert-time `DO NOTHING` semantics.
        let new_edge_ids: std::collections::HashSet<EdgeId> =
            facts.edges.iter().map(edge_identity).collect();
        let cur_edge_ids: std::collections::HashSet<EdgeId> =
            current_edges.iter().map(edge_identity).collect();

        let tx = self.conn.transaction()?;
        // 1. Delete removed edges first, so any node they reference can then be
        //    dropped (edges are FK-constrained on node ids, with no cascade). A
        //    removed node's edges are all removals, so they are gone before step 2.
        for edge in &current_edges {
            if !new_edge_ids.contains(&edge_identity(edge)) {
                delete_edge(&tx, edge)?;
            }
        }
        // 2. Drop nodes that no longer exist.
        for old in &current_nodes {
            if !new_keys.contains(old.key.as_str()) {
                tx.execute("DELETE FROM nodes WHERE key = ?1", [&old.key])?;
            }
        }
        // 3. Upsert only the nodes that are new or whose content changed (an upsert
        //    keeps the row id, so unchanged edges stay valid).
        for node in &facts.nodes {
            if cur_by_key
                .get(node.key.as_str())
                .is_none_or(|cur| *cur != node)
            {
                upsert_node(&tx, node)?;
            }
        }
        // 4. Insert only the added edges (their endpoints now all exist).
        for edge in &facts.edges {
            if !cur_edge_ids.contains(&edge_identity(edge)) {
                insert_edge(&tx, edge)?;
            }
        }
        write_sync_state(&tx, tree)?;
        tx.commit()?;
        Ok(())
    }

    /// Fetch a node by its natural key.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`], [`StoreError::Json`], or
    /// [`StoreError::Corrupt`] if a stored value cannot be decoded.
    pub fn get_node(&self, key: &str) -> Result<Option<Node>, StoreError> {
        let sql = format!("SELECT {NODE_COLS} FROM nodes n WHERE n.key = ?1");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query([key])?;
        match rows.next()? {
            Some(row) => Ok(Some(row_to_node(row)?)),
            None => Ok(None),
        }
    }

    /// Every node key in the store, ordered. Useful for whole-graph exports.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on query failure.
    pub fn all_keys(&self) -> Result<Vec<String>, StoreError> {
        let mut stmt = self.conn.prepare("SELECT key FROM nodes ORDER BY key")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Dump the entire graph as a single [`FactSet`], with nodes and edges in a
    /// deterministic order — suitable for a portable, content-stable artifact.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`], [`StoreError::Json`], or
    /// [`StoreError::Corrupt`] on decode failure.
    pub fn export_factset(&self) -> Result<FactSet, StoreError> {
        let node_sql = format!("SELECT {NODE_COLS} FROM nodes n ORDER BY n.key");
        let mut node_stmt = self.conn.prepare(&node_sql)?;
        let mut node_rows = node_stmt.query([])?;
        let nodes = collect_nodes(&mut node_rows)?;

        // Order edges by their resolved endpoint keys (not row id) so the dump is
        // stable regardless of insertion order.
        let edge_sql = format!("{EDGE_SELECT} ORDER BY ns.key, nd.key, e.kind, e.provenance");
        let mut edge_stmt = self.conn.prepare(&edge_sql)?;
        let mut edge_rows = edge_stmt.query([])?;
        let edges = collect_edges(&mut edge_rows)?;

        Ok(FactSet { nodes, edges })
    }

    /// All nodes of a given kind.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`], [`StoreError::Json`], or
    /// [`StoreError::Corrupt`] on decode failure.
    pub fn nodes_by_kind(&self, kind: &NodeKind) -> Result<Vec<Node>, StoreError> {
        let sql = format!("SELECT {NODE_COLS} FROM nodes n WHERE n.kind = ?1 ORDER BY n.key");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query([kind.as_str()])?;
        collect_nodes(&mut rows)
    }

    /// Nodes of a given kind whose `name` equals `name_lower` **case-insensitively**,
    /// ordered by key. Narrows a lookup at the SQL layer — using the `kind` index and
    /// filtering `name` in-query — so only matching rows are decoded, never every
    /// node of that kind. Used by the cross-repo follow bridge to fetch just the
    /// candidate struct(s) for a config section rather than scanning all structs.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`], [`StoreError::Json`], or
    /// [`StoreError::Corrupt`] on decode failure.
    pub fn nodes_by_kind_named(
        &self,
        kind: &NodeKind,
        name_lower: &str,
    ) -> Result<Vec<Node>, StoreError> {
        let sql = format!(
            "SELECT {NODE_COLS} FROM nodes n \
             WHERE n.kind = ?1 AND lower(n.name) = ?2 ORDER BY n.key"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query([kind.as_str(), name_lower])?;
        collect_nodes(&mut rows)
    }

    /// Every `config_key` node's flattened setting (ADR-0009), read back out of
    /// the graph as [`crate::ConfigKey`]s — the graph-native source the cross-repo
    /// link matcher (`roteiro links --infer`) consumes, so it never re-parses
    /// config files. Ordered by node key (deterministic). A node missing the
    /// `key`/`path` a well-formed `config_key` carries is skipped defensively.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`], [`StoreError::Json`], or
    /// [`StoreError::Corrupt`] on decode failure.
    pub fn config_keys(&self) -> Result<Vec<crate::ConfigKey>, StoreError> {
        let nodes = self.nodes_by_kind(&NodeKind::Other(crate::config_keys::KIND.to_owned()))?;
        let mut out = Vec::with_capacity(nodes.len());
        for n in &nodes {
            let key = n.meta.get("key").and_then(serde_json::Value::as_str);
            // A `config_key` node carries a `value` in `meta` when it has a real
            // setting (file-derived keys always do, even an empty string). A
            // struct-derived key (`meta.source = "struct"`) omits it — its value is
            // *unknown*, not empty — so record that absence explicitly rather than
            // defaulting it to `""`, which would false-match in value agreement.
            let value = n.meta.get("value").and_then(serde_json::Value::as_str);
            if let (Some(key), Some(file)) = (key, n.path.as_deref()) {
                out.push(crate::ConfigKey {
                    file: file.to_owned(),
                    key: key.to_owned(),
                    value: value.unwrap_or_default().to_owned(),
                    value_known: value.is_some(),
                });
            }
        }
        Ok(out)
    }

    /// Every node whose source `path` is `path`, ordered by key — the file node
    /// plus the symbols and markers defined in it. Used to scope a change to the
    /// graph (e.g. `roteiro review`).
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`], [`StoreError::Json`], or
    /// [`StoreError::Corrupt`] on decode failure.
    pub fn nodes_by_path(&self, path: &str) -> Result<Vec<Node>, StoreError> {
        let sql = format!("SELECT {NODE_COLS} FROM nodes n WHERE n.path = ?1 ORDER BY n.key");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query([path])?;
        collect_nodes(&mut rows)
    }

    /// Every node produced by a given layer, ordered by key. The incremental
    /// `sync` loads the `Derived` layer to reconstruct the extraction graph
    /// without re-reading every blob.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`], [`StoreError::Json`], or
    /// [`StoreError::Corrupt`] on decode failure.
    pub fn nodes_by_provenance(&self, provenance: Provenance) -> Result<Vec<Node>, StoreError> {
        let sql = format!("SELECT {NODE_COLS} FROM nodes n WHERE n.provenance = ?1 ORDER BY n.key");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query([provenance.as_str()])?;
        collect_nodes(&mut rows)
    }

    /// Every node in the store, ordered by key. Unlike [`Store::export_factset`]
    /// this decodes no edges, so it is cheap for node-only scans (e.g. search).
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`], [`StoreError::Json`], or
    /// [`StoreError::Corrupt`] on decode failure.
    pub fn all_nodes(&self) -> Result<Vec<Node>, StoreError> {
        let sql = format!("SELECT {NODE_COLS} FROM nodes n ORDER BY n.key");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query([])?;
        collect_nodes(&mut rows)
    }

    /// Every edge in the store, with endpoints resolved to their node keys. Used
    /// by [`Store::reconcile`] to diff the edge set.
    ///
    /// Ordered by the edge's **content** — `(src key, dst key, kind, provenance)`,
    /// the table's unique tuple — not by row id. This makes the order a function
    /// of the *graph*, not of insertion history, so an incrementally
    /// [`reconcile`](Store::reconcile)d store and a cold [`rebuild`](Store::rebuild)
    /// return edges identically. (The same reason node scans order by `key`.)
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] or [`StoreError::Corrupt`] on failure.
    pub fn all_edges(&self) -> Result<Vec<Edge>, StoreError> {
        let sql = format!("{EDGE_SELECT} ORDER BY ns.key, nd.key, e.kind, e.provenance");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query([])?;
        collect_edges(&mut rows)
    }

    /// Edges whose source is the node with the given key, in content order
    /// (`(dst key, kind, provenance)` — `src` is fixed). Content-ordered rather
    /// than by row id so the result is history-independent; see [`Store::all_edges`].
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] or [`StoreError::Corrupt`] on failure.
    pub fn edges_from(&self, key: &str) -> Result<Vec<Edge>, StoreError> {
        let sql = format!("{EDGE_SELECT} WHERE ns.key = ?1 ORDER BY nd.key, e.kind, e.provenance");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query([key])?;
        collect_edges(&mut rows)
    }

    /// Edges whose destination is the node with the given key, in content order
    /// (`(src key, kind, provenance)` — `dst` is fixed). See [`Store::all_edges`].
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] or [`StoreError::Corrupt`] on failure.
    pub fn edges_to(&self, key: &str) -> Result<Vec<Edge>, StoreError> {
        let sql = format!("{EDGE_SELECT} WHERE nd.key = ?1 ORDER BY ns.key, e.kind, e.provenance");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query([key])?;
        collect_edges(&mut rows)
    }

    /// All edges with the given provenance, in content order
    /// (`(src key, dst key, kind)` — `provenance` is fixed). See [`Store::all_edges`].
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] or [`StoreError::Corrupt`] on failure.
    pub fn edges_by_provenance(&self, provenance: Provenance) -> Result<Vec<Edge>, StoreError> {
        let sql = format!("{EDGE_SELECT} WHERE e.provenance = ?1 ORDER BY ns.key, nd.key, e.kind");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query([provenance.as_str()])?;
        collect_edges(&mut rows)
    }

    /// Delete all edges with the given provenance, returning how many were
    /// removed. Used to re-derive a whole provenance class authoritatively (e.g.
    /// `inferred` edges when re-running inference with different parameters).
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on write failure.
    pub fn delete_edges_by_provenance(&self, provenance: Provenance) -> Result<u64, StoreError> {
        let n = self.conn.execute(
            "DELETE FROM edges WHERE provenance = ?1",
            [provenance.as_str()],
        )?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    /// Delete all edges carrying the given `src_ref`, returning how many were
    /// removed. Lets one producer of `inferred` edges (e.g. the embedding layer,
    /// or a Graphify import) re-derive its own edges authoritatively without
    /// touching edges another producer contributed.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on write failure.
    pub fn delete_edges_by_src_ref(&self, src_ref: &str) -> Result<u64, StoreError> {
        let n = self
            .conn
            .execute("DELETE FROM edges WHERE src_ref = ?1", [src_ref])?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    /// Apply an import layer to the live graph **and** persist it durably under
    /// `src_ref`, validating as it goes: this ref's prior edges are cleared
    /// (an authoritative re-import), the layer's nodes are upserted, and each
    /// edge is applied only if both endpoints resolve. Dangling edges — cross-
    /// references to code that is not present — are dropped, and only the
    /// validated (trimmed) layer is persisted, so stale data is never stored.
    ///
    /// This is the "validate on import" half; [`Store::reapply_imports`] is the
    /// "validate on sync" half, re-checking layers against the rebuilt graph.
    ///
    /// # Errors
    /// Returns [`StoreError::Json`] if `facts` cannot be (de)serialized,
    /// [`StoreError::InvalidEdge`] on a malformed edge, or [`StoreError::Sqlite`]
    /// on write failure.
    pub fn apply_import_layer(
        &mut self,
        src_ref: &str,
        facts: &FactSet,
    ) -> Result<ImportApplied, StoreError> {
        let tx = self.conn.transaction()?;
        // Authoritative re-import: drop this ref's prior edges from the live graph.
        tx.execute("DELETE FROM edges WHERE src_ref = ?1", [src_ref])?;
        for node in &facts.nodes {
            upsert_node(&tx, node)?;
        }
        let (kept, applied) = apply_edges_pruning(&tx, &facts.edges)?;
        let trimmed = FactSet {
            nodes: facts.nodes.clone(),
            edges: kept,
        };
        put_import_row(&tx, src_ref, &trimmed)?;
        tx.commit()?;
        Ok(ImportApplied {
            layers: 1,
            nodes: facts.nodes.len(),
            ..applied
        })
    }

    /// Remove the persisted import layer for `src_ref`, returning whether one
    /// existed. Does not remove edges already in the live graph (use
    /// [`Store::delete_edges_by_src_ref`] for that).
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on write failure.
    pub fn delete_import(&self, src_ref: &str) -> Result<bool, StoreError> {
        let n = self
            .conn
            .execute("DELETE FROM imports WHERE src_ref = ?1", [src_ref])?;
        Ok(n > 0)
    }

    /// The `src_ref`s of all persisted import layers, ordered.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on query failure.
    pub fn import_refs(&self) -> Result<Vec<String>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT src_ref FROM imports ORDER BY src_ref")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Re-apply every persisted import layer on top of the current graph and
    /// **re-validate** it: all import nodes are upserted first (so cross-layer
    /// and self references resolve), then each edge is applied; an edge whose
    /// endpoint is now absent — a cross-reference to code a sync removed — is
    /// pruned from the persisted layer, not merely skipped. So the durable store
    /// keeps only still-correct data. Idempotent; safe to run after each rebuild.
    ///
    /// # Errors
    /// Returns [`StoreError::Json`] if a stored layer cannot be (de)serialized,
    /// or [`StoreError::Sqlite`] on write failure.
    pub fn reapply_imports(&mut self) -> Result<ImportApplied, StoreError> {
        let layers = self.load_import_layers()?;
        let tx = self.conn.transaction()?;
        // Pass 1: upsert every layer's nodes so intra-import edges resolve
        // regardless of which layer defines the endpoint.
        for (_, facts) in &layers {
            for node in &facts.nodes {
                upsert_node(&tx, node)?;
            }
        }
        // Pass 2: apply edges, pruning (and rewriting) any that dangle.
        let mut applied = ImportApplied {
            layers: layers.len(),
            ..ImportApplied::default()
        };
        for (src_ref, facts) in &layers {
            applied.nodes += facts.nodes.len();
            let (kept, counts) = apply_edges_pruning(&tx, &facts.edges)?;
            applied.edges_applied += counts.edges_applied;
            applied.edges_pruned += counts.edges_pruned;
            if kept.len() != facts.edges.len() {
                let trimmed = FactSet {
                    nodes: facts.nodes.clone(),
                    edges: kept,
                };
                put_import_row(&tx, src_ref, &trimmed)?;
            }
        }
        tx.commit()?;
        Ok(applied)
    }

    /// Load and decode every persisted import layer as `(src_ref, FactSet)`, in
    /// `src_ref` order.
    fn load_import_layers(&self) -> Result<Vec<(String, FactSet)>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT src_ref, facts FROM imports ORDER BY src_ref")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (src_ref, json) = row?;
            let mut facts: FactSet = serde_json::from_str(&json)?;
            // An import-layer node is *never* derived (derivation is `sync`'s job),
            // so a `Derived` tag here is always wrong. It arises two ways, both
            // repaired the same: a legacy layer persisted before nodes carried
            // provenance (the field is absent → serde defaults `Derived`), or —
            // anomalously — a layer that stored an explicit `"provenance":"derived"`
            // (a producer/data bug). We deliberately repair *both* rather than only
            // the absent case: leaving an explicit-`derived` import node in place
            // would let a layer-scoped `sync` treat it as derived and delete it —
            // the exact corruption this guards against — so repair is the safe
            // recovery, not silent masking. Idempotent, runs on every reapply
            // (old stores self-heal), and a no-op for correctly-tagged fresh imports.
            for node in &mut facts.nodes {
                if node.provenance == Provenance::Derived {
                    node.provenance = import_node_provenance(&node.key);
                }
            }
            out.push((src_ref, facts));
        }
        Ok(out)
    }

    /// Neighbouring nodes reachable from `key` in the given direction. Returns
    /// an empty vector if the node does not exist.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`], [`StoreError::Json`], or
    /// [`StoreError::Corrupt`] on failure.
    pub fn neighbors(&self, key: &str, dir: Direction) -> Result<Vec<Node>, StoreError> {
        let out = format!(
            "SELECT {NODE_COLS} FROM nodes n JOIN edges e ON n.id = e.dst \
             JOIN nodes s ON s.id = e.src WHERE s.key = ?1"
        );
        let inc = format!(
            "SELECT {NODE_COLS} FROM nodes n JOIN edges e ON n.id = e.src \
             JOIN nodes d ON d.id = e.dst WHERE d.key = ?1"
        );
        // Order by output column 1 (the node key) so results are deterministic
        // across SQLite versions/plans. Positional ordering avoids both the
        // ambiguity of a bare `key` (present in every joined table) and the fact
        // that a table-qualified name cannot be used after the `Both` UNION.
        let sql = match dir {
            Direction::Outgoing => format!("{out} ORDER BY 1"),
            Direction::Incoming => format!("{inc} ORDER BY 1"),
            Direction::Both => format!("{out} UNION {inc} ORDER BY 1"),
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query([key])?;
        collect_nodes(&mut rows)
    }

    /// Fetch the cached context bundle for `key` as `(fingerprint, json)`, if
    /// present. The caller compares the fingerprint to the node's current one to
    /// decide whether the entry is fresh (see [`crate::context`]).
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on query failure.
    pub fn context_cache_get(&self, key: &str) -> Result<Option<(String, String)>, StoreError> {
        let row = self
            .conn
            .query_row(
                "SELECT fingerprint, json FROM node_context WHERE key = ?1",
                [key],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?;
        Ok(row)
    }

    /// Fetch just the cached fingerprint for `key`, without reading the (larger)
    /// JSON payload — for a cheap freshness check.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on query failure.
    pub fn context_cache_fingerprint(&self, key: &str) -> Result<Option<String>, StoreError> {
        let fp = self
            .conn
            .query_row(
                "SELECT fingerprint FROM node_context WHERE key = ?1",
                [key],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        Ok(fp)
    }

    /// Store (or replace) the cached context bundle for `key`.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on write failure.
    pub fn context_cache_put(
        &self,
        key: &str,
        fingerprint: &str,
        json: &str,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO node_context (key, fingerprint, json) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET
                 fingerprint = excluded.fingerprint, json = excluded.json",
            [key, fingerprint, json],
        )?;
        Ok(())
    }

    /// Delete the cached context entry for `key`, returning whether one existed.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on write failure.
    pub fn context_cache_delete(&self, key: &str) -> Result<bool, StoreError> {
        let n = self
            .conn
            .execute("DELETE FROM node_context WHERE key = ?1", [key])?;
        Ok(n > 0)
    }

    /// Every key with a cached context entry, ordered. Used to prune entries for
    /// nodes that no longer exist.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on query failure.
    pub fn context_cache_keys(&self) -> Result<Vec<String>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT key FROM node_context ORDER BY key")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    // --- Analyzer findings (ADR-0012). A separate artifact store: these methods
    // touch `analysis_runs`/`findings` only, never `nodes`/`edges`, so
    // `export_factset` — and the published `GraphArtifact` — stays a pure
    // function of the tree no matter what an analyzer reports. ---

    /// Replace the findings layer `run.layer` **wholesale**, atomically: the
    /// previous run for that layer and every finding row it owned are deleted,
    /// then this run and its findings are written. A finding that has since been
    /// fixed therefore *disappears* instead of lingering, and re-ingesting an
    /// unchanged report is idempotent — the store ends up with the same rows and
    /// no growth.
    ///
    /// The owned-record cleanup is explicit rather than inherited. The import
    /// path ([`Store::apply_import_layer`]) deletes a layer's edges but leaves its
    /// obsolete *nodes* behind; copying that shape here would silently orphan
    /// findings, so the previous run's rows are deleted by hand and counted in
    /// [`FindingsApplied::removed`]. The schema's `ON DELETE CASCADE` is kept as
    /// defence in depth, not as the mechanism.
    ///
    /// `findings` must carry distinct [`FindingKey`](crate::FindingKey)s: a
    /// duplicate identity is a producer bug and is rejected by the unique index
    /// *inside* the transaction, so nothing is committed. Callers that parse
    /// untrusted reports should reject duplicates earlier, with a better message.
    ///
    /// # Errors
    /// Returns [`StoreError::Json`] if the run's command policy or a finding's
    /// `meta` cannot be serialized, or [`StoreError::Sqlite`] on write failure. On
    /// any error the transaction is rolled back and the previous layer survives
    /// intact.
    pub fn replace_findings_layer(
        &mut self,
        run: &AnalysisRun,
        findings: &[Finding],
    ) -> Result<FindingsApplied, StoreError> {
        let tx = self.conn.transaction()?;
        let applied = findings::replace_layer(&tx, run, findings)?;
        tx.commit()?;
        Ok(applied)
    }

    /// Delete a findings layer and every finding row it owns, returning how many
    /// findings went with it, or `None` if the layer was not live.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on write failure; nothing is committed on
    /// error.
    pub fn delete_findings_layer(&mut self, layer: &str) -> Result<Option<usize>, StoreError> {
        let tx = self.conn.transaction()?;
        let removed = findings::delete_layer(&tx, layer)?;
        tx.commit()?;
        Ok(removed)
    }

    /// Every live findings layer with its findings, ordered by layer key and, in
    /// each layer, by finding key. Pass `analyzer` to narrow to one analyzer.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on query failure, [`StoreError::Json`] if a
    /// stored policy or `meta` cannot be decoded, or [`StoreError::Corrupt`] on an
    /// unrecognised stored token.
    pub fn findings_layers(
        &self,
        analyzer: Option<&str>,
    ) -> Result<Vec<FindingsLayer>, StoreError> {
        findings::layers(&self.conn, analyzer)
    }

    /// Number of findings currently stored, across every layer.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on query failure.
    pub fn finding_count(&self) -> Result<u64, StoreError> {
        findings::count_findings(&self.conn)
    }

    /// Number of live analysis runs — one per findings layer.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on query failure.
    pub fn analysis_run_count(&self) -> Result<u64, StoreError> {
        findings::count_runs(&self.conn)
    }

    // --- Generated media content (ADR-0015). A separate artifact store, on the
    // same terms as findings above: these methods touch `media_content` only,
    // never `nodes`/`edges`, so `export_factset` stays a pure function of the
    // tree across a `media build`, and generated text can never reach the
    // `authored` relevance boost in `search`. ---

    /// Write one generated-content record, returning `true` if a row was written.
    ///
    /// Keyed by `(blob_id, producer)`. A record for that exact pair already
    /// present is **left alone** and `false` is returned — which is what makes
    /// `media build` incremental and a second run free. A *different* producer is
    /// a new row beside it, never an overwrite: that is the point of keying on the
    /// producer identity, so a better model's description can be compared with the
    /// one it replaces and a distrusted producer can be dropped wholesale.
    /// [`MediaWrite::replace`] (only `media build --force`) is the one path that
    /// overwrites, and only for the identical producer.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on a write failure; the transaction is
    /// rolled back.
    pub fn record_media_content(&mut self, write: &MediaWrite<'_>) -> Result<bool, StoreError> {
        let tx = self.conn.transaction()?;
        let written = media::record(&tx, write)?;
        tx.commit()?;
        Ok(written)
    }

    /// Whether a record already exists for exactly this `(blob, producer)`.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on query failure.
    pub fn has_media_record(&self, blob_id: &str, producer: &str) -> Result<bool, StoreError> {
        media::exists(&self.conn, blob_id, producer)
    }

    /// Stored records matching `filter`, ordered by `(producer, blob id)`.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on query failure, or
    /// [`StoreError::Corrupt`] if a row carries an unknown modality token.
    pub fn media_records(&self, filter: &MediaFilter<'_>) -> Result<Vec<MediaRecord>, StoreError> {
        media::records(&self.conn, filter)
    }

    /// Discard records — all of them, or only those written by `producer`.
    /// Returns how many rows went.
    ///
    /// Nothing in the graph is touched: dropping a model you no longer trust must
    /// not cost you a re-sync.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on a write failure.
    pub fn clear_media_content(&mut self, producer: Option<&str>) -> Result<usize, StoreError> {
        let tx = self.conn.transaction()?;
        let removed = media::delete(&tx, producer)?;
        tx.commit()?;
        Ok(removed)
    }

    /// Number of generated-content records currently stored.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on query failure.
    pub fn media_content_count(&self) -> Result<u64, StoreError> {
        media::count(&self.conn)
    }

    /// One summary per producer that owns records, ordered by producer id.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on query failure, or
    /// [`StoreError::Corrupt`] if a row carries an unknown modality token.
    pub fn media_producer_summaries(&self) -> Result<Vec<ProducerSummary>, StoreError> {
        media::producer_summaries(&self.conn)
    }

    /// The blob ids that have at least one record carrying **generated text**
    /// for `kind`. A blob the pre-generation gate refused is not described, so it
    /// is not here — see [`Store::gated_media_blobs`].
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on query failure.
    pub fn described_media_blobs(
        &self,
        kind: MediaKind,
    ) -> Result<std::collections::BTreeSet<String>, StoreError> {
        media::described_blobs(&self.conn, kind)
    }

    /// The blob ids the [pre-generation gate](crate::media::gate) refused for
    /// `kind` — silent clips, blank images — each of which has a record naming
    /// the value that refused it.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on query failure.
    pub fn gated_media_blobs(
        &self,
        kind: MediaKind,
    ) -> Result<std::collections::BTreeSet<String>, StoreError> {
        media::gated_blobs(&self.conn, kind)
    }

    /// Records whose blob is no longer anywhere in `present`. Exposed so a caller
    /// can see what a tree change has orphaned; nothing deletes them implicitly,
    /// because a record is expensive to reproduce and a blob can return.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on query failure.
    pub fn orphan_media_records(
        &self,
        present: &std::collections::BTreeSet<String>,
    ) -> Result<Vec<MediaRecord>, StoreError> {
        Ok(self
            .media_records(&MediaFilter::default())?
            .into_iter()
            .filter(|r| !present.contains(&r.blob_id))
            .collect())
    }

    // --- Episodic agent memory (ADR-0013). A separate artifact store, on the
    // same terms as findings and media above: these methods write `agent_memory`
    // and nothing else — they *read* `nodes` to capture and check an anchor, and
    // never write one. So `export_factset` stays a pure function of the tree
    // across every memory write, and nothing an agent remembers can reach the
    // `authored` relevance boost in `search`.
    //
    // These rows are not touched by `rebuild`, following the `imports` precedent:
    // what has no generating function must not be destroyed by a re-derivation.
    // ---

    /// Record one memory, returning its id — the monotonic generation it was
    /// written at.
    ///
    /// The anchor's blob and path, and the `sync_state` tree witness, are
    /// captured **here, from the graph**, so a caller cannot record evidence the
    /// node never carried. An anchor key naming no node is accepted and reads
    /// back as [`crate::AnchorState::Vanished`].
    ///
    /// [`MemoryWrite::supersedes`] records the supersession explicitly, in the
    /// same transaction as the successor: either both land or neither does.
    ///
    /// # Errors
    /// Returns [`MemoryError::InvalidScope`] / [`MemoryError::InvalidBody`] /
    /// [`MemoryError::InvalidConfidence`] for a record that could not be recalled,
    /// [`MemoryError::NotFound`] or [`MemoryError::AlreadySuperseded`] for a bad
    /// supersession target, or [`MemoryError::Store`] on a write failure — in
    /// every case with nothing committed.
    pub fn record_memory(&mut self, write: &MemoryWrite<'_>) -> Result<i64, MemoryError> {
        let tx = self.conn.transaction().map_err(StoreError::from)?;
        let id = memory::record(&tx, write)?;
        tx.commit().map_err(StoreError::from)?;
        Ok(id)
    }

    /// Memory records matching `filter`, newest generation first.
    ///
    /// Live records only unless [`MemoryFilter::include_superseded`] is set: a
    /// superseded record drops out immediately and regardless of age, because the
    /// test is a recorded pointer and not a clock.
    ///
    /// Each record's [`crate::AnchorState`] is computed against the **current**
    /// graph on every call and is never stored.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on query failure, or [`StoreError::Corrupt`]
    /// if a row carries an unknown kind token.
    pub fn memory_records(
        &self,
        filter: &MemoryFilter<'_>,
    ) -> Result<Vec<MemoryRecord>, StoreError> {
        memory::records(&self.conn, filter)
    }

    /// One memory record by id, or `None` if there is no such record.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on query failure, or [`StoreError::Corrupt`]
    /// if the row carries an unknown kind token.
    pub fn memory_record(&self, id: i64) -> Result<Option<MemoryRecord>, StoreError> {
        memory::get(&self.conn, id)
    }

    /// A [`MemoryListing`]: the records matching `filter`, plus the whole store's
    /// live and superseded counts, so an empty result is legible as *nothing
    /// matched* rather than *nothing is stored*.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on query failure, or [`StoreError::Corrupt`]
    /// if a row carries an unknown kind token.
    pub fn memory_listing(&self, filter: &MemoryFilter<'_>) -> Result<MemoryListing, StoreError> {
        let (live, superseded) = memory::counts(&self.conn)?;
        Ok(MemoryListing {
            schema: memory::MEMORY_SCHEMA,
            records: self.memory_records(filter)?,
            live,
            superseded,
        })
    }

    /// **The only way a memory record is ever removed.** Deletes it, and returns
    /// `None` if there was no such record.
    ///
    /// Episodic memory is unbounded and never auto-evicted — no sweep, no TTL, no
    /// capacity bound reaches this table — so an explicit call is the whole
    /// reclamation story. It is also the privacy story: memory has no redaction
    /// chokepoint, so a record that captured a token or a customer name is
    /// removed by asking.
    ///
    /// Anything the deleted record had superseded becomes **live again** and is
    /// named in [`MemoryForgotten::restored`]: leaving it superseded would hide it
    /// on the authority of a record that no longer exists.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on a write failure; the transaction is
    /// rolled back.
    pub fn forget_memory(&mut self, id: i64) -> Result<Option<MemoryForgotten>, StoreError> {
        let tx = self.conn.transaction()?;
        let forgotten = memory::forget(&tx, id)?;
        tx.commit()?;
        Ok(forgotten)
    }

    /// How many memory records are stored, as `(live, superseded)`.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on query failure.
    pub fn memory_counts(&self) -> Result<(u64, u64), StoreError> {
        memory::counts(&self.conn)
    }

    /// **Ranked recall**: the live records that match `opts`, scored
    /// `base_confidence × anchor_penalty × decay(age)` and ordered best first.
    ///
    /// Every term is computed **here, at retrieval time, and written to no
    /// column** (ADR-0013). A stored score that decayed would rewrite the store on
    /// every read and would be wrong in between, making recall depend on when you
    /// last looked. Three consequences follow, and each is a promise:
    ///
    /// - **This call mutates nothing.** Recall over an unchanged store and an
    ///   unchanged tree is idempotent, which is what makes
    ///   [`crate::Decay::None`] byte-identical across runs.
    /// - **A superseded record is never returned**, immediately and regardless of
    ///   age: the test is a recorded pointer, not a clock.
    /// - **A record whose anchor no longer resolves is still returned**, demoted
    ///   and labelled. Drift ranks it down; nothing deletes it.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on query failure, or [`StoreError::Corrupt`]
    /// if a row carries an unknown kind token.
    pub fn recall_memory(&self, opts: &RecallOptions<'_>) -> Result<Recall, StoreError> {
        let (live, superseded) = memory::counts(&self.conn)?;
        Ok(Recall {
            schema: memory::RECALL_SCHEMA,
            generation: memory::generation(&self.conn)?,
            decay: opts.decay,
            reproducible: opts.decay.is_reproducible(),
            results: memory::recall(&self.conn, opts)?,
            live,
            superseded,
        })
    }

    // --- The bounded cache tier (ADR-0013, Tier 2). The *opposite* rules to the
    // episodic tier above, because it holds the opposite kind of knowledge:
    // everything here is re-derivable, so eviction costs cycles and never
    // information. Nothing in this section can reach `agent_memory` — it has no
    // `bytes`, no `last_used` and no `hits` for a capacity policy to grip. ---

    /// Write (or replace) one cache entry.
    ///
    /// The payload size is computed here and the anchor's blob is captured from
    /// the graph here, so the sweep can order and total the tier without reading
    /// it, and a caller cannot record evidence a node never carried.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on write failure.
    pub fn agent_cache_put(&self, write: &CacheWrite<'_>) -> Result<(), StoreError> {
        memory::cache_put(&self.conn, write)
    }

    /// Read one cache entry back, **recording the access** — `hits` increments and
    /// `last_used` advances.
    ///
    /// This is the one read in the memory store that writes, and what it writes is
    /// the cache's own bookkeeping: those two columns exist to be moved by exactly
    /// this, and a hit counter nothing increments is a column that lies. It
    /// touches nothing outside `agent_cache`, so [`Store::recall_memory`] — the
    /// read whose reproducibility is promised — stays free of it.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on query failure.
    pub fn agent_cache_get(&self, key: &str) -> Result<Option<CacheEntry>, StoreError> {
        memory::cache_get(&self.conn, key)
    }

    /// Every cache entry, ordered by key, **without** recording an access:
    /// inspecting a cache is not using it.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on query failure.
    pub fn agent_cache_entries(&self) -> Result<Vec<CacheEntry>, StoreError> {
        memory::cache_entries(&self.conn)
    }

    /// Delete one cache entry, returning whether there was one.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on write failure.
    pub fn agent_cache_forget(&self, key: &str) -> Result<bool, StoreError> {
        memory::cache_forget(&self.conn, key)
    }

    /// What the cache tier holds, against `budget_bytes`.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on query failure.
    pub fn agent_cache_stats(&self, budget_bytes: u64) -> Result<CacheStats, StoreError> {
        memory::cache_stats(&self.conn, budget_bytes)
    }

    /// **Sweep the cache tier down to `budget_bytes`**, evicting oldest-first on
    /// `(anchor_valid ASC, last_used ASC)`, and advance the generation.
    ///
    /// Called at the maintenance seam (beside `refresh_contexts`) and **never on
    /// the read path**, so an ordinary query never mutates the store. Three things
    /// are never evicted: anything episodic — structurally, there is no column to
    /// grip it by; an entry written in the current generation whose anchor still
    /// applies, which is the session's own work; and the most-recently-used entry,
    /// always, even if it alone exceeds the budget.
    ///
    /// That last pair means a sweep can legitimately finish still over budget.
    /// [`CacheSweep::over_budget`] reports it rather than leaving a bound that
    /// silently failed to bind.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on failure; the transaction is rolled back.
    pub fn sweep_agent_cache(&mut self, budget_bytes: u64) -> Result<CacheSweep, StoreError> {
        let tx = self.conn.transaction()?;
        let swept = memory::cache_sweep(&tx, budget_bytes)?;
        tx.commit()?;
        Ok(swept)
    }

    /// Findings whose owning run no longer exists. Always `0` in a healthy store;
    /// exposed so layer replacement can be asserted to clean up its own records
    /// rather than orphaning them.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] on query failure.
    pub fn orphan_finding_count(&self) -> Result<u64, StoreError> {
        findings::count_orphan_findings(&self.conn)
    }
}

// --- Free helpers operating on a `Connection` (a `Transaction` derefs to one) ---

fn node_row_id(conn: &Connection, key: &str) -> rusqlite::Result<Option<i64>> {
    conn.query_row("SELECT id FROM nodes WHERE key = ?1", [key], |r| r.get(0))
        .optional()
}

/// Record (or clear) the last-synced `HEAD` tree id. Shared by `rebuild` and
/// `reconcile` so both leave identical `sync_state`.
fn write_sync_state(conn: &Connection, tree: Option<&str>) -> Result<(), StoreError> {
    match tree {
        // Clear `env` on every tree write: it is only valid for the tree a
        // committed `sync` set it against, and that sync re-records it (via
        // `set_sync_env`) immediately after. So a worktree/index sync, or any
        // path that does not re-set it, leaves `env` NULL — reading as "unknown"
        // and forcing the safe full re-extraction next time.
        Some(tree) => conn.execute(
            "INSERT INTO sync_state (id, tree) VALUES (0, ?1)
             ON CONFLICT(id) DO UPDATE SET tree = excluded.tree, env = NULL",
            [tree],
        )?,
        None => conn.execute("DELETE FROM sync_state WHERE id = 0", [])?,
    };
    Ok(())
}

fn upsert_node(conn: &Connection, node: &Node) -> Result<(), StoreError> {
    let meta = serde_json::to_string(&node.meta)?;
    let (span_start, span_end) = match node.span {
        Some(s) => (Some(i64::from(s.start)), Some(i64::from(s.end))),
        None => (None, None),
    };
    conn.execute(
        "INSERT INTO nodes (key, kind, name, path, lang, blob_hash, span_start, span_end, provenance, meta)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(key) DO UPDATE SET
             kind = excluded.kind, name = excluded.name, path = excluded.path,
             lang = excluded.lang, blob_hash = excluded.blob_hash,
             span_start = excluded.span_start, span_end = excluded.span_end,
             provenance = excluded.provenance, meta = excluded.meta",
        params![
            node.key,
            node.kind.as_str(),
            node.name,
            node.path,
            node.lang,
            node.blob_hash,
            span_start,
            span_end,
            node.provenance.as_str(),
            meta,
        ],
    )?;
    Ok(())
}

fn insert_edge(conn: &Connection, edge: &Edge) -> Result<(), StoreError> {
    validate_edge(edge)?;
    let src_id =
        node_row_id(conn, &edge.src)?.ok_or_else(|| StoreError::UnknownNode(edge.src.clone()))?;
    let dst_id =
        node_row_id(conn, &edge.dst)?.ok_or_else(|| StoreError::UnknownNode(edge.dst.clone()))?;
    insert_edge_row(conn, edge, src_id, dst_id)
}

/// Apply `edge` only if both endpoints already resolve to nodes, returning
/// whether it was **applied** (both endpoints resolved; a duplicate of an
/// existing edge is a harmless no-op via `ON CONFLICT DO NOTHING` but still
/// reports `true`). A missing endpoint returns `false` rather than erroring —
/// the caller prunes such dangling cross-references from the import layer.
fn insert_edge_if_present(conn: &Connection, edge: &Edge) -> Result<bool, StoreError> {
    validate_edge(edge)?;
    let (Some(src_id), Some(dst_id)) =
        (node_row_id(conn, &edge.src)?, node_row_id(conn, &edge.dst)?)
    else {
        return Ok(false);
    };
    insert_edge_row(conn, edge, src_id, dst_id)?;
    Ok(true)
}

/// Apply `edges`, keeping those whose endpoints resolve and pruning the rest.
/// Returns the kept edges plus the applied/pruned counts (in an [`ImportApplied`]
/// whose `layers`/`nodes` are left zero for the caller to fill).
fn apply_edges_pruning(
    conn: &Connection,
    edges: &[Edge],
) -> Result<(Vec<Edge>, ImportApplied), StoreError> {
    let mut kept = Vec::with_capacity(edges.len());
    let mut counts = ImportApplied::default();
    for edge in edges {
        if insert_edge_if_present(conn, edge)? {
            kept.push(edge.clone());
            counts.edges_applied += 1;
        } else {
            counts.edges_pruned += 1;
        }
    }
    Ok((kept, counts))
}

/// Upsert a persisted import layer row. Free helper so it can run inside the same
/// transaction as an apply/prune pass.
fn put_import_row(conn: &Connection, src_ref: &str, facts: &FactSet) -> Result<(), StoreError> {
    let json = serde_json::to_string(facts)?;
    conn.execute(
        "INSERT INTO imports (src_ref, facts) VALUES (?1, ?2)
         ON CONFLICT(src_ref) DO UPDATE SET facts = excluded.facts, imported_at = datetime('now')",
        params![src_ref, json],
    )?;
    Ok(())
}

/// The provenance/confidence invariant guard shared by the strict and tolerant
/// edge inserts.
fn validate_edge(edge: &Edge) -> Result<(), StoreError> {
    if edge.is_valid() {
        Ok(())
    } else {
        Err(StoreError::InvalidEdge(format!(
            "confidence must be present iff provenance is inferred (src={}, dst={})",
            edge.src, edge.dst
        )))
    }
}

/// Insert an edge row given already-resolved endpoint ids. Edges are a set: a
/// duplicate `(src, dst, kind, provenance)` is a no-op via `ON CONFLICT … DO
/// NOTHING`, so re-applying a fact set never accumulates duplicates. Other
/// constraint violations (guarded in Rust above) still surface.
fn insert_edge_row(
    conn: &Connection,
    edge: &Edge,
    src_id: i64,
    dst_id: i64,
) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO edges (src, dst, kind, provenance, confidence, src_ref)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(src, dst, kind, provenance) DO NOTHING",
        params![
            src_id,
            dst_id,
            edge.kind.as_str(),
            edge.provenance.as_str(),
            edge.confidence,
            edge.src_ref,
        ],
    )?;
    Ok(())
}

/// A hashable identity for an edge over **all** its fields — used by
/// [`Store::reconcile`] to diff the edge set. A tuple (not a delimiter-joined
/// string) so no field value can be confused with a separator: node keys embed
/// git paths, which may legally contain any byte (including control characters),
/// so a joined string could collapse distinct edges to one identity and drop an
/// edge. Confidence is compared by its exact bit pattern (`f64::to_bits`, wrapped
/// in `Option` so `None` and `Some(_)` stay distinct), the only non-`Eq` field.
fn edge_identity(edge: &Edge) -> EdgeId {
    (
        edge.src.clone(),
        edge.dst.clone(),
        edge.kind.as_str().to_owned(),
        edge.provenance.as_str().to_owned(),
        edge.confidence.map(f64::to_bits),
        edge.src_ref.clone(),
    )
}

/// The tuple form of an edge's full-field identity (see [`edge_identity`]):
/// `(src, dst, kind, provenance, confidence-bits, src_ref)`.
type EdgeId = (String, String, String, String, Option<u64>, Option<String>);

/// Delete the edge row identified by `(src, dst, kind, provenance)` — the table's
/// unique key — resolving the endpoint node keys to ids. A no-op if absent.
fn delete_edge(conn: &Connection, edge: &Edge) -> Result<(), StoreError> {
    conn.execute(
        "DELETE FROM edges
         WHERE src = (SELECT id FROM nodes WHERE key = ?1)
           AND dst = (SELECT id FROM nodes WHERE key = ?2)
           AND kind = ?3 AND provenance = ?4",
        params![
            edge.src,
            edge.dst,
            edge.kind.as_str(),
            edge.provenance.as_str()
        ],
    )?;
    Ok(())
}

fn collect_nodes(rows: &mut rusqlite::Rows) -> Result<Vec<Node>, StoreError> {
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(row_to_node(row)?);
    }
    Ok(out)
}

fn collect_edges(rows: &mut rusqlite::Rows) -> Result<Vec<Edge>, StoreError> {
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(row_to_edge(row)?);
    }
    Ok(out)
}

fn row_to_node(row: &rusqlite::Row) -> Result<Node, StoreError> {
    let kind: String = row.get("kind")?;
    let span_start: Option<i64> = row.get("span_start")?;
    let span_end: Option<i64> = row.get("span_end")?;
    let span = match (span_start, span_end) {
        (Some(s), Some(e)) => Some(Span::new(to_u32(s)?, to_u32(e)?)),
        _ => None,
    };
    let meta: String = row.get("meta")?;
    let provenance: String = row.get("provenance")?;
    let provenance = Provenance::from_token(&provenance)
        .ok_or_else(|| StoreError::Corrupt(format!("unknown node provenance: {provenance}")))?;
    Ok(Node {
        key: row.get("key")?,
        kind: NodeKind::from_token(&kind),
        name: row.get("name")?,
        path: row.get("path")?,
        lang: row.get("lang")?,
        blob_hash: row.get("blob_hash")?,
        span,
        provenance,
        meta: serde_json::from_str(&meta)?,
    })
}

fn row_to_edge(row: &rusqlite::Row) -> Result<Edge, StoreError> {
    let kind: String = row.get("kind")?;
    let provenance: String = row.get("provenance")?;
    let provenance = Provenance::from_token(&provenance)
        .ok_or_else(|| StoreError::Corrupt(format!("unknown provenance: {provenance}")))?;
    Ok(Edge {
        src: row.get("src")?,
        dst: row.get("dst")?,
        kind: EdgeKind::from_token(&kind),
        provenance,
        confidence: row.get("confidence")?,
        src_ref: row.get("src_ref")?,
    })
}

fn to_u32(v: i64) -> Result<u32, StoreError> {
    u32::try_from(v).map_err(|_| StoreError::Corrupt(format!("span offset out of range: {v}")))
}

/// The true provenance of an import-layer node, from its key namespace: Graphify
/// nodes (`graphify:`) are [`Provenance::Inferred`]; every other import node (lat,
/// …) is [`Provenance::Authored`]. Import-layer nodes are never derived, so this
/// is used to repair a legacy `Derived` tag on load (see `load_import_layers`).
fn import_node_provenance(key: &str) -> Provenance {
    if key.starts_with("graphify:") {
        Provenance::Inferred
    } else {
        Provenance::Authored
    }
}

#[cfg(test)]
mod tests {
    use super::Store;
    use crate::model::{Direction, Edge, EdgeKind, FactSet, Node, NodeKind, Span};
    use crate::provenance::Provenance;

    fn sample_node(key: &str) -> Node {
        Node {
            key: key.to_owned(),
            kind: NodeKind::Fn,
            name: "sample".to_owned(),
            path: Some("src/lib.rs".to_owned()),
            lang: Some("rust".to_owned()),
            blob_hash: Some("deadbeef".to_owned()),
            span: Some(Span::new(10, 42)),
            provenance: Provenance::Derived,
            meta: serde_json::json!({"vis": "pub"}),
        }
    }

    #[test]
    fn reconcile_matches_a_full_rebuild() {
        // reconcile must leave the store identical to a fresh rebuild, across an
        // add, a remove, a content change, and edge churn.
        let node = |k: &str, name: &str| {
            let mut n = sample_node(k);
            n.name = name.to_owned();
            n
        };
        let edge =
            |src: &str, dst: &str| Edge::derived(src.to_owned(), dst.to_owned(), EdgeKind::Calls);
        // An inferred edge carries confidence and a src_ref — exercise both so the
        // equivalence claim covers every edge field, not just derived calls.
        let inferred = |src: &str, dst: &str, conf: f64| {
            let mut e = Edge::inferred(src.to_owned(), dst.to_owned(), EdgeKind::Related, conf);
            e.src_ref = Some("import:demo".to_owned());
            e
        };

        let facts1 = FactSet {
            nodes: vec![node("a", "A"), node("b", "B"), node("c", "C")],
            edges: vec![edge("a", "b"), edge("b", "c"), inferred("a", "c", 0.7)],
        };
        // b changes (name), c is removed, d is added; edge b->c drops, a->d added,
        // and the inferred edge's confidence changes.
        let facts2 = FactSet {
            nodes: vec![node("a", "A"), node("b", "B2"), node("d", "D")],
            edges: vec![edge("a", "b"), edge("a", "d"), inferred("a", "d", 0.9)],
        };

        // Path 1: rebuild facts1, then reconcile to facts2.
        let mut reconciled = Store::open_in_memory().expect("open");
        reconciled.rebuild(&facts1, Some("t1")).expect("rebuild");
        reconciled
            .reconcile(&facts2, Some("t2"))
            .expect("reconcile");

        // Path 2: a fresh full rebuild of facts2.
        let mut rebuilt = Store::open_in_memory().expect("open");
        rebuilt.rebuild(&facts2, Some("t2")).expect("rebuild");

        let canon = |fs: FactSet| {
            let mut nodes = fs.nodes;
            nodes.sort_by(|a, b| a.key.cmp(&b.key));
            let mut edges: Vec<String> = fs
                .edges
                .iter()
                .map(|e| {
                    format!(
                        "{}\0{}\0{}\0{}\0{:?}\0{:?}",
                        e.kind.as_str(),
                        e.src,
                        e.dst,
                        e.provenance.as_str(),
                        e.confidence,
                        e.src_ref
                    )
                })
                .collect();
            edges.sort();
            (nodes, edges)
        };
        assert_eq!(
            canon(reconciled.export_factset().expect("export")),
            canon(rebuilt.export_factset().expect("export")),
            "reconcile must match a full rebuild",
        );
        assert_eq!(
            reconciled.sync_state().expect("state").as_deref(),
            Some("t2")
        );
    }

    #[test]
    fn reconcile_writes_only_the_edge_delta() {
        // An unchanged edge must keep its row (proving reconcile does not wipe and
        // reinsert the whole edge set); a removed edge's row goes; a new edge's row
        // appears. Row identity is the SQLite `rowid` — stable unless deleted.
        let n = |k: &str| sample_node(k);
        let e =
            |src: &str, dst: &str| Edge::derived(src.to_owned(), dst.to_owned(), EdgeKind::Calls);

        let mut store = Store::open_in_memory().expect("open");
        store
            .rebuild(
                &FactSet {
                    nodes: vec![n("a"), n("b"), n("c")],
                    edges: vec![e("a", "b"), e("b", "c")],
                },
                None,
            )
            .expect("rebuild");

        // Map (src_key, dst_key) → rowid via the private connection.
        let rowids = |store: &Store| -> std::collections::HashMap<(String, String), i64> {
            let mut stmt = store
                .conn
                .prepare(
                    "SELECT ns.key, nd.key, e.rowid FROM edges e \
                     JOIN nodes ns ON ns.id = e.src JOIN nodes nd ON nd.id = e.dst",
                )
                .expect("prepare");
            stmt.query_map([], |r| {
                Ok((
                    (r.get::<_, String>(0)?, r.get::<_, String>(1)?),
                    r.get::<_, i64>(2)?,
                ))
            })
            .expect("query")
            .map(Result::unwrap)
            .collect()
        };

        let before = rowids(&store);
        let ab_rowid = before[&("a".to_owned(), "b".to_owned())];

        // Keep a->b, drop b->c, add a->c.
        store
            .reconcile(
                &FactSet {
                    nodes: vec![n("a"), n("b"), n("c")],
                    edges: vec![e("a", "b"), e("a", "c")],
                },
                None,
            )
            .expect("reconcile");

        let after = rowids(&store);
        assert_eq!(
            after.get(&("a".to_owned(), "b".to_owned())),
            Some(&ab_rowid),
            "the unchanged edge keeps its row (not rewritten)"
        );
        assert!(
            !after.contains_key(&("b".to_owned(), "c".to_owned())),
            "the removed edge's row is gone"
        );
        assert!(
            after.contains_key(&("a".to_owned(), "c".to_owned())),
            "the added edge has a new row"
        );
    }

    #[test]
    fn reconcile_is_history_independent_for_edge_queries() {
        // The whole point of the edge delta: it must be invisible above storage.
        // A store reached by rebuild(f1)+reconcile(f2) has different edge row ids
        // than a cold rebuild at f2, yet every edge query must return byte-for-byte
        // the same result — order included — because the queries are content-ordered.
        let n = |k: &str| sample_node(k);
        let d =
            |src: &str, dst: &str| Edge::derived(src.to_owned(), dst.to_owned(), EdgeKind::Calls);
        let inf = |src: &str, dst: &str, c: f64| {
            Edge::inferred(src.to_owned(), dst.to_owned(), EdgeKind::Related, c)
        };
        let f1 = FactSet {
            nodes: vec![n("a"), n("b"), n("c")],
            edges: vec![d("a", "b"), d("b", "c"), inf("a", "c", 0.7)],
        };
        let f2 = FactSet {
            nodes: vec![n("a"), n("b"), n("d")],
            edges: vec![d("a", "b"), d("a", "d"), inf("a", "d", 0.9)],
        };

        let mut incremental = Store::open_in_memory().expect("open");
        incremental.rebuild(&f1, None).expect("rebuild");
        incremental.reconcile(&f2, None).expect("reconcile");
        let mut cold = Store::open_in_memory().expect("open");
        cold.rebuild(&f2, None).expect("rebuild");

        // Project to all fields so the comparison covers order *and* content.
        let proj = |es: Vec<Edge>| -> Vec<String> {
            es.into_iter()
                .map(|e| {
                    format!(
                        "{}|{}|{}|{}|{:?}|{:?}",
                        e.src,
                        e.dst,
                        e.kind.as_str(),
                        e.provenance.as_str(),
                        e.confidence,
                        e.src_ref
                    )
                })
                .collect()
        };

        for key in ["a", "b", "d"] {
            assert_eq!(
                proj(incremental.edges_from(key).expect("from")),
                proj(cold.edges_from(key).expect("from")),
                "edges_from({key}) must match a cold rebuild"
            );
            assert_eq!(
                proj(incremental.edges_to(key).expect("to")),
                proj(cold.edges_to(key).expect("to")),
                "edges_to({key}) must match a cold rebuild"
            );
        }
        assert_eq!(
            proj(incremental.all_edges().expect("all")),
            proj(cold.all_edges().expect("all")),
            "all_edges must match a cold rebuild"
        );
        for p in [Provenance::Derived, Provenance::Inferred] {
            assert_eq!(
                proj(incremental.edges_by_provenance(p).expect("prov")),
                proj(cold.edges_by_provenance(p).expect("prov")),
                "edges_by_provenance({}) must match a cold rebuild",
                p.as_str()
            );
        }
    }

    #[test]
    fn reconcile_updates_confidence_on_an_unchanged_tuple() {
        // A change to *only* an edge's confidence — same (src, dst, kind,
        // provenance) — must still be applied. Edge identity includes confidence,
        // so it is delete+add (matching a full rebuild), not the insert-time
        // `DO NOTHING` that would leave the stale confidence in place.
        let n = |k: &str| sample_node(k);
        let inf = |c: f64| {
            let mut e = Edge::inferred("a".to_owned(), "b".to_owned(), EdgeKind::Related, c);
            e.src_ref = Some("import:demo".to_owned());
            e
        };

        let mut store = Store::open_in_memory().expect("open");
        store
            .rebuild(
                &FactSet {
                    nodes: vec![n("a"), n("b")],
                    edges: vec![inf(0.5)],
                },
                None,
            )
            .expect("rebuild");
        store
            .reconcile(
                &FactSet {
                    nodes: vec![n("a"), n("b")],
                    edges: vec![inf(0.9)],
                },
                None,
            )
            .expect("reconcile");

        let edges = store.edges_from("a").expect("edges");
        assert_eq!(edges.len(), 1);
        assert_eq!(
            edges[0].confidence,
            Some(0.9),
            "confidence updated, not left stale"
        );
    }

    #[test]
    fn edge_identity_does_not_collide_across_field_boundaries() {
        // Node keys embed git paths, which may contain any byte — including the
        // unit separator (`\x1f`). A delimiter-joined identity would map these two
        // distinct edges to the same string (`a\x1fb\x1fc\x1f…`); the tuple identity
        // must keep them apart, or reconcile would drop one edge as a "duplicate".
        let e1 = super::edge_identity(&Edge::derived(
            "a\u{1f}b".to_owned(),
            "c".to_owned(),
            EdgeKind::Calls,
        ));
        let e2 = super::edge_identity(&Edge::derived(
            "a".to_owned(),
            "b\u{1f}c".to_owned(),
            EdgeKind::Calls,
        ));
        assert_ne!(
            e1, e2,
            "control chars in a key must not collapse identities"
        );

        // A confidence-only difference (same tuple otherwise) also stays distinct,
        // and `None` (derived) never equals `Some(0.0)`.
        let derived = super::edge_identity(&Edge::derived(
            "a".to_owned(),
            "b".to_owned(),
            EdgeKind::Related,
        ));
        let inferred0 = super::edge_identity(&Edge::inferred(
            "a".to_owned(),
            "b".to_owned(),
            EdgeKind::Related,
            0.0,
        ));
        assert_ne!(derived, inferred0, "None vs Some(0.0) confidence differ");
    }

    #[test]
    fn open_in_memory_applies_schema() {
        let store = Store::open_in_memory().expect("open");
        assert_eq!(store.node_count().expect("count"), 0);
        // Written against `latest_version()` rather than the literal that stood
        // here (8 for analyzer findings, then 9, 10, 11 as the media and
        // agent-memory tables landed). The literal was defended as making someone
        // confirm a new migration is meant to apply on open — but it could not do
        // that job: `apply` runs *every* migration newer than the recorded
        // version, so "applies on open" is not a per-migration choice there is
        // anything to confirm. What the literal actually asserted was the value of
        // a shared constant, which every future migration then has to come here
        // and edit, in a file it otherwise has no business in. This is the idiom
        // `migrations::tests::a_later_migration_is_additive_on_a_populated_store`
        // already uses, for the same reason.
        assert_eq!(
            store.schema_version().expect("version"),
            crate::migrations::latest_version(),
            "opening a store applies the whole migration set",
        );
    }

    #[test]
    fn upsert_and_get_round_trips_all_fields() {
        let store = Store::open_in_memory().expect("open");
        let node = sample_node("sym:rust:src/lib.rs#sample");
        store.upsert_node(&node).expect("upsert");
        let got = store.get_node(&node.key).expect("get").expect("present");
        assert_eq!(got, node);
    }

    #[test]
    fn upsert_updates_in_place() {
        let store = Store::open_in_memory().expect("open");
        let mut node = sample_node("k");
        store.upsert_node(&node).expect("insert");
        node.name = "renamed".to_owned();
        node.kind = NodeKind::Struct;
        store.upsert_node(&node).expect("update");
        assert_eq!(store.node_count().expect("count"), 1);
        let got = store.get_node("k").expect("get").expect("present");
        assert_eq!(got.name, "renamed");
        assert_eq!(got.kind, NodeKind::Struct);
    }

    #[test]
    fn edge_with_unknown_endpoint_is_rejected() {
        let store = Store::open_in_memory().expect("open");
        store
            .upsert_node(&Node::new("a", NodeKind::Fn, "a"))
            .expect("a");
        let edge = Edge::derived("a", "missing", EdgeKind::Calls);
        let err = store.insert_edge(&edge).expect_err("should reject");
        assert!(matches!(err, super::StoreError::UnknownNode(k) if k == "missing"));
    }

    #[test]
    fn inferred_edge_requires_confidence() {
        let store = Store::open_in_memory().expect("open");
        store
            .upsert_node(&Node::new("a", NodeKind::Fn, "a"))
            .expect("a");
        store
            .upsert_node(&Node::new("b", NodeKind::Fn, "b"))
            .expect("b");
        // Hand-build an inferred edge with no confidence to violate the invariant.
        let bad = Edge {
            src: "a".to_owned(),
            dst: "b".to_owned(),
            kind: EdgeKind::References,
            provenance: Provenance::Inferred,
            confidence: None,
            src_ref: None,
        };
        assert!(matches!(
            store.insert_edge(&bad).expect_err("reject"),
            super::StoreError::InvalidEdge(_)
        ));
    }

    #[test]
    fn apply_factset_is_atomic() {
        let mut store = Store::open_in_memory().expect("open");
        // Second edge references a missing node, so the whole set must roll back.
        let facts = FactSet::new()
            .with_node(Node::new("a", NodeKind::Fn, "a"))
            .with_node(Node::new("b", NodeKind::Fn, "b"))
            .with_edge(Edge::derived("a", "b", EdgeKind::Calls))
            .with_edge(Edge::derived("a", "ghost", EdgeKind::Calls));
        assert!(store.apply_factset(&facts).is_err());
        assert_eq!(store.node_count().expect("count"), 0, "rolled back");
        assert_eq!(store.edge_count().expect("count"), 0, "rolled back");
    }

    #[test]
    fn neighbors_and_provenance_queries() {
        let mut store = Store::open_in_memory().expect("open");
        let facts = FactSet::new()
            .with_node(Node::new("a", NodeKind::Fn, "a"))
            .with_node(Node::new("b", NodeKind::Fn, "b"))
            .with_node(Node::new("c", NodeKind::Fn, "c"))
            .with_edge(Edge::derived("a", "b", EdgeKind::Calls))
            .with_edge(Edge::inferred("a", "c", EdgeKind::References, 0.5));
        store.apply_factset(&facts).expect("apply");

        let out = store.neighbors("a", Direction::Outgoing).expect("out");
        let mut keys: Vec<_> = out.iter().map(|n| n.key.clone()).collect();
        keys.sort();
        assert_eq!(keys, ["b", "c"]);

        assert!(
            store
                .neighbors("b", Direction::Outgoing)
                .expect("b out")
                .is_empty()
        );
        assert_eq!(
            store
                .neighbors("b", Direction::Incoming)
                .expect("b in")
                .len(),
            1
        );

        let inferred = store
            .edges_by_provenance(Provenance::Inferred)
            .expect("inf");
        assert_eq!(inferred.len(), 1);
        assert_eq!(inferred[0].confidence, Some(0.5));
    }

    #[test]
    fn neighbors_of_absent_node_is_empty() {
        let store = Store::open_in_memory().expect("open");
        assert!(
            store
                .neighbors("nope", Direction::Both)
                .expect("q")
                .is_empty()
        );
    }

    #[test]
    fn get_missing_node_is_none() {
        let store = Store::open_in_memory().expect("open");
        assert!(store.get_node("absent").expect("get").is_none());
    }

    #[test]
    fn nodes_by_kind_and_edges_to() {
        let mut store = Store::open_in_memory().expect("open");
        let facts = FactSet::new()
            .with_node(Node::new("f1", NodeKind::Fn, "f1"))
            .with_node(Node::new("f2", NodeKind::Fn, "f2"))
            .with_node(Node::new("s1", NodeKind::Struct, "s1"))
            .with_edge(Edge::derived("f1", "s1", EdgeKind::References))
            .with_edge(Edge::derived("f2", "s1", EdgeKind::References));
        store.apply_factset(&facts).expect("apply");

        let fns = store.nodes_by_kind(&NodeKind::Fn).expect("fns");
        assert_eq!(
            fns.iter().map(|n| n.key.as_str()).collect::<Vec<_>>(),
            ["f1", "f2"]
        );
        assert!(
            store
                .nodes_by_kind(&NodeKind::Enum)
                .expect("enums")
                .is_empty()
        );

        let into_s1 = store.edges_to("s1").expect("edges_to");
        assert_eq!(into_s1.len(), 2);
        assert!(into_s1.iter().all(|e| e.dst == "s1"));
    }

    #[test]
    fn open_persists_across_reopen() {
        let path =
            std::env::temp_dir().join(format!("roteiro-open-test-{}.db", std::process::id()));
        std::fs::remove_file(&path).ok();
        {
            let store = Store::open(&path).expect("open");
            store
                .upsert_node(&sample_node("persisted"))
                .expect("upsert");
        }
        {
            let store = Store::open(&path).expect("reopen");
            assert_eq!(store.node_count().expect("count"), 1);
            // The subject here is that a *reopen* is at the same schema as a
            // fresh open — not what number that happens to be. See
            // `open_in_memory_applies_schema` on why the literal went.
            assert_eq!(
                store.schema_version().expect("version"),
                crate::migrations::latest_version(),
            );
            assert!(store.get_node("persisted").expect("get").is_some());
        }
        std::fs::remove_file(&path).expect("cleanup");
    }

    fn graphify_layer(dst: &str) -> FactSet {
        FactSet::new()
            .with_node(Node::new("graphify:doc1", NodeKind::Doc, "Doc 1"))
            .with_edge({
                let mut e = Edge::inferred("graphify:doc1", dst, EdgeKind::References, 0.9);
                e.src_ref = Some("import:graphify".to_owned());
                e
            })
    }

    /// A persisted import layer is re-applied after a `rebuild` wipes the graph,
    /// so imported facts survive a code-changing sync.
    #[test]
    fn imports_survive_rebuild() {
        let mut store = Store::open_in_memory().expect("open");
        let derived = FactSet::new().with_node(Node::new("file:a.rs", NodeKind::File, "a.rs"));
        store.rebuild(&derived, Some("tree1")).expect("rebuild");

        // apply_import_layer applies to the live graph and persists in one step.
        let applied = store
            .apply_import_layer("import:graphify", &graphify_layer("file:a.rs"))
            .expect("apply import");
        assert_eq!(applied.edges_applied, 1);
        assert_eq!(applied.edges_pruned, 0);
        assert_eq!(store.import_refs().expect("refs"), vec!["import:graphify"]);

        // Simulate a code-changing sync: the derived graph is rebuilt (still has
        // file:a.rs), which drops the imported doc + edge from the live graph.
        store.rebuild(&derived, Some("tree2")).expect("rebuild2");
        assert!(store.get_node("graphify:doc1").expect("get").is_none());

        // Re-applying imports restores them; nothing is pruned (target present).
        let applied = store.reapply_imports().expect("reapply");
        assert_eq!(applied.layers, 1);
        assert_eq!(applied.nodes, 1);
        assert_eq!(applied.edges_applied, 1);
        assert_eq!(applied.edges_pruned, 0);
        assert!(store.get_node("graphify:doc1").expect("get").is_some());
        assert_eq!(store.edges_from("graphify:doc1").expect("edges").len(), 1);
    }

    /// When a sync removes an edge's target (e.g. a deleted file), re-applying
    /// **prunes** that stale cross-reference from the persisted layer — it is not
    /// kept and retried forever. The import node itself is preserved.
    #[test]
    fn reapply_prunes_stale_cross_references() {
        let mut store = Store::open_in_memory().expect("open");
        let derived = FactSet::new().with_node(Node::new("file:gone.rs", NodeKind::File, "g"));
        store.rebuild(&derived, Some("t1")).expect("rebuild");
        store
            .apply_import_layer("import:graphify", &graphify_layer("file:gone.rs"))
            .expect("import");

        // Code-changing sync: file:gone.rs is deleted from the derived graph.
        store
            .rebuild(&FactSet::new(), Some("t2"))
            .expect("rebuild2");
        let applied = store.reapply_imports().expect("reapply");
        assert_eq!(applied.nodes, 1);
        assert_eq!(applied.edges_applied, 0);
        assert_eq!(
            applied.edges_pruned, 1,
            "the edge to the deleted file is pruned"
        );
        assert!(store.get_node("graphify:doc1").expect("get").is_some());

        // The prune is durable: a second reapply finds nothing left to prune,
        // proving the stale edge was removed from the persisted layer.
        let again = store.reapply_imports().expect("reapply2");
        assert_eq!(again.edges_applied, 0);
        assert_eq!(again.edges_pruned, 0, "already pruned; not retried");
    }

    /// `apply_import_layer` validates on import: a dangling edge in the incoming
    /// layer is dropped and never persisted.
    #[test]
    fn apply_import_layer_prunes_on_import() {
        let mut store = Store::open_in_memory().expect("open");
        let present = || FactSet::new().with_node(Node::new("file:a.rs", NodeKind::File, "a"));
        store.rebuild(&present(), Some("t")).expect("rebuild");

        let layer = graphify_layer("file:a.rs").with_edge({
            // Points at a file that does not exist → pruned on import.
            let mut e = Edge::inferred("graphify:doc1", "file:ghost.rs", EdgeKind::References, 0.9);
            e.src_ref = Some("import:graphify".to_owned());
            e
        });
        let applied = store
            .apply_import_layer("import:graphify", &layer)
            .expect("import");
        assert_eq!(applied.edges_applied, 1);
        assert_eq!(applied.edges_pruned, 1);

        // A rebuild + reapply confirms only the valid edge was persisted.
        store.rebuild(&present(), Some("t2")).expect("rebuild2");
        let re = store.reapply_imports().expect("reapply");
        assert_eq!(re.edges_applied, 1);
        assert_eq!(re.edges_pruned, 0, "ghost edge was not persisted");
    }

    #[test]
    fn legacy_import_layer_nodes_are_retagged_non_derived() {
        // A layer persisted before nodes carried provenance: its node objects have
        // no `provenance` field, so serde defaults them to Derived. On reapply the
        // store must repair them — a Graphify node to Inferred, a lat node to
        // Authored — so a later derived-only sync never mistakes them for derived.
        let mut store = Store::open_in_memory().expect("open");
        let legacy = r#"{"nodes":[
            {"key":"graphify:doc1","kind":"doc","name":"d","path":null,"lang":null,"blob_hash":null,"span":null,"meta":null},
            {"key":"lat:lat.md/a.md","kind":"doc","name":"a","path":null,"lang":null,"blob_hash":null,"span":null,"meta":null}
        ],"edges":[]}"#;
        store
            .conn
            .execute(
                "INSERT INTO imports (src_ref, facts) VALUES ('import:legacy', ?1)",
                [legacy],
            )
            .expect("seed legacy import row");

        store.reapply_imports().expect("reapply");

        let g = store
            .get_node("graphify:doc1")
            .expect("get")
            .expect("graphify node");
        assert_eq!(
            g.provenance,
            Provenance::Inferred,
            "graphify import node repaired to inferred"
        );
        let l = store
            .get_node("lat:lat.md/a.md")
            .expect("get")
            .expect("lat node");
        assert_eq!(
            l.provenance,
            Provenance::Authored,
            "lat import node repaired to authored"
        );
    }

    /// `apply_import_layer` replaces the layer for a ref; `delete_import` removes.
    #[test]
    fn apply_import_replaces_and_delete_removes() {
        let mut store = Store::open_in_memory().expect("open");
        let a = FactSet::new().with_node(Node::new("graphify:x", NodeKind::Doc, "x"));
        let b = FactSet::new().with_node(Node::new("graphify:y", NodeKind::Doc, "y"));
        store.apply_import_layer("import:graphify", &a).expect("a");
        store.apply_import_layer("import:graphify", &b).expect("b");
        assert_eq!(
            store.import_refs().expect("refs").len(),
            1,
            "same ref replaced"
        );
        assert!(store.delete_import("import:graphify").expect("del"));
        assert!(store.import_refs().expect("refs").is_empty());
        assert!(!store.delete_import("import:graphify").expect("del again"));
    }
}

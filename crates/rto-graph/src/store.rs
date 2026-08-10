//! SQLite-backed graph store.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

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
const NODE_COLS: &str =
    "n.key, n.kind, n.name, n.path, n.lang, n.blob_hash, n.span_start, n.span_end, n.meta";

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
    /// only the **changed node rows** instead of wiping and reinserting the whole
    /// graph — the git-style "write only what differs". Nodes carry the heavy JSON
    /// `meta`, so skipping unchanged nodes is the write saving on a large graph
    /// with a small change; edges (lean, and FK-constrained on node ids) are still
    /// replaced wholesale. The final state — nodes, edges, and `sync_state` — is
    /// identical to `rebuild(facts, tree)`.
    ///
    /// # Errors
    /// Returns [`StoreError`] on a query failure; the transaction is rolled back.
    pub fn reconcile(&mut self, facts: &FactSet, tree: Option<&str>) -> Result<(), StoreError> {
        let current = self.export_factset()?;
        let cur_by_key: std::collections::HashMap<&str, &Node> =
            current.nodes.iter().map(|n| (n.key.as_str(), n)).collect();
        let new_keys: std::collections::HashSet<&str> =
            facts.nodes.iter().map(|n| n.key.as_str()).collect();

        let tx = self.conn.transaction()?;
        // Edges reference node ids (FK, no cascade); clear them first so any node
        // can then be deleted safely, and reinsert the full set at the end.
        tx.execute("DELETE FROM edges", [])?;
        // Drop nodes that no longer exist.
        for old in &current.nodes {
            if !new_keys.contains(old.key.as_str()) {
                tx.execute("DELETE FROM nodes WHERE key = ?1", [&old.key])?;
            }
        }
        // Upsert only the nodes that are new or whose content changed.
        for node in &facts.nodes {
            if cur_by_key
                .get(node.key.as_str())
                .is_none_or(|cur| *cur != node)
            {
                upsert_node(&tx, node)?;
            }
        }
        for edge in &facts.edges {
            insert_edge(&tx, edge)?;
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

    /// Edges whose source is the node with the given key.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] or [`StoreError::Corrupt`] on failure.
    pub fn edges_from(&self, key: &str) -> Result<Vec<Edge>, StoreError> {
        let sql = format!("{EDGE_SELECT} WHERE ns.key = ?1 ORDER BY e.id");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query([key])?;
        collect_edges(&mut rows)
    }

    /// Edges whose destination is the node with the given key.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] or [`StoreError::Corrupt`] on failure.
    pub fn edges_to(&self, key: &str) -> Result<Vec<Edge>, StoreError> {
        let sql = format!("{EDGE_SELECT} WHERE nd.key = ?1 ORDER BY e.id");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query([key])?;
        collect_edges(&mut rows)
    }

    /// All edges with the given provenance.
    ///
    /// # Errors
    /// Returns [`StoreError::Sqlite`] or [`StoreError::Corrupt`] on failure.
    pub fn edges_by_provenance(&self, provenance: Provenance) -> Result<Vec<Edge>, StoreError> {
        let sql = format!("{EDGE_SELECT} WHERE e.provenance = ?1 ORDER BY e.id");
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
            out.push((src_ref, serde_json::from_str::<FactSet>(&json)?));
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
        Some(tree) => conn.execute(
            "INSERT INTO sync_state (id, tree) VALUES (0, ?1)
             ON CONFLICT(id) DO UPDATE SET tree = excluded.tree",
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
        "INSERT INTO nodes (key, kind, name, path, lang, blob_hash, span_start, span_end, meta)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(key) DO UPDATE SET
             kind = excluded.kind, name = excluded.name, path = excluded.path,
             lang = excluded.lang, blob_hash = excluded.blob_hash,
             span_start = excluded.span_start, span_end = excluded.span_end,
             meta = excluded.meta",
        params![
            node.key,
            node.kind.as_str(),
            node.name,
            node.path,
            node.lang,
            node.blob_hash,
            span_start,
            span_end,
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
    Ok(Node {
        key: row.get("key")?,
        kind: NodeKind::from_token(&kind),
        name: row.get("name")?,
        path: row.get("path")?,
        lang: row.get("lang")?,
        blob_hash: row.get("blob_hash")?,
        span,
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

        let facts1 = FactSet {
            nodes: vec![node("a", "A"), node("b", "B"), node("c", "C")],
            edges: vec![edge("a", "b"), edge("b", "c")],
        };
        // b changes (name), c is removed, d is added; edge b->c drops, a->d added.
        let facts2 = FactSet {
            nodes: vec![node("a", "A"), node("b", "B2"), node("d", "D")],
            edges: vec![edge("a", "b"), edge("a", "d")],
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
                        "{}\0{}\0{}\0{}",
                        e.kind.as_str(),
                        e.src,
                        e.dst,
                        e.provenance.as_str()
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
    fn open_in_memory_applies_schema() {
        let store = Store::open_in_memory().expect("open");
        assert_eq!(store.node_count().expect("count"), 0);
        assert_eq!(store.schema_version().expect("version"), 5);
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
            assert_eq!(store.schema_version().expect("version"), 5);
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

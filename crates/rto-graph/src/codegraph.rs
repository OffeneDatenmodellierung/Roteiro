//! codegraph **validation oracle** (not an importer).
//!
//! [code-graph-mcp](https://github.com/sdsrss/code-graph-mcp) builds an
//! independent tree-sitter AST graph in a portable `SQLite` snapshot. Per ADR-0001
//! its structural edges are **not** imported — Roteiro re-derives those — but the
//! snapshot is a useful *oracle*: [`compare`] checks Roteiro's derived Rust
//! symbols and `calls` edges against codegraph's and reports agreement and
//! divergence, so extraction gaps on either side surface. Read-only; nothing is
//! written to the store.
//!
//! Snapshot schema (v10): `files(id, path, …)`, `nodes(id, file_id, type,
//! qualified_name, …)`, `edges(source_id, target_id, relation, …)`,
//! `meta(key, value)`. A codegraph method's `qualified_name` uses `.` scoping
//! (`Store.open`); Roteiro uses `::`, so keys map as
//! `sym:rust:<path>#<qualified_name with '.'→'::'>`.

use std::collections::BTreeSet;
use std::path::Path;

use rusqlite::{Connection, OpenFlags};

use crate::store::{Store, StoreError};
use crate::{EdgeKind, NodeKind};

/// Stable schema tag for the oracle report.
pub const ORACLE_SCHEMA: &str = "roteiro.oracle/v1";

/// Errors raised while comparing against a codegraph snapshot.
#[derive(Debug, thiserror::Error)]
pub enum OracleError {
    /// Reading the codegraph snapshot failed.
    #[error("codegraph sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// Reading the Roteiro store failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The file is not a recognisable codegraph snapshot.
    #[error("not a codegraph snapshot: {0}")]
    NotCodegraph(String),
}

/// The result of comparing Roteiro's derived graph against a codegraph snapshot.
/// Counts cover **Rust** `function`/`struct`/`enum`/`trait` symbols (the overlap
/// where both tools operate) and function-to-function `calls`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OracleReport {
    /// Stable schema tag ([`ORACLE_SCHEMA`]).
    pub schema: &'static str,
    /// The commit codegraph indexed, from the snapshot `meta` (for context —
    /// a mismatch with the current `HEAD` explains divergence).
    pub source_commit: Option<String>,
    /// Comparable Rust symbols codegraph found.
    pub symbols_codegraph: usize,
    /// Comparable Rust symbols in Roteiro's derived graph.
    pub symbols_roteiro: usize,
    /// Symbols present in both with the *same* key (exact agreement).
    pub symbols_matched: usize,
    /// Symbols both tools found in the same file with the same leaf name but a
    /// **different scope** (e.g. codegraph `#foo` vs Roteiro `#tests::foo`) — the
    /// same symbol, keyed differently, not a real divergence.
    pub symbols_scope_diff: usize,
    /// Symbols codegraph found that Roteiro genuinely lacks (no same-file,
    /// same-leaf match) — a real extraction-coverage gap.
    pub codegraph_only: usize,
    /// Symbols Roteiro found that codegraph genuinely lacks.
    pub roteiro_only: usize,
    /// A capped, ordered sample of the genuine codegraph-only keys.
    pub codegraph_only_sample: Vec<String>,
    /// A capped, ordered sample of the genuine roteiro-only keys.
    pub roteiro_only_sample: Vec<String>,
    /// Constants codegraph extracted — a known Roteiro gap (it does not yet
    /// extract `const`/`static`), reported so the symbol counts stay honest.
    pub constants_codegraph: usize,
    /// Internal function→function `calls` edges codegraph found (both ends Rust).
    pub calls_codegraph: usize,
    /// Of those, how many Roteiro also has (agreement).
    pub calls_agree: usize,
    /// codegraph calls Roteiro lacks — expected, since Roteiro only links
    /// unambiguously-resolved calls while codegraph resolves by name too.
    pub calls_codegraph_only: usize,
}

/// Maximum number of divergent keys listed in a sample.
const SAMPLE_CAP: usize = 25;

/// Compare Roteiro's derived graph (`store`) against a codegraph snapshot at
/// `db_path`, returning an [`OracleReport`]. Read-only on both sides.
///
/// # Errors
/// Returns [`OracleError::NotCodegraph`] if the file lacks codegraph's tables,
/// [`OracleError::Sqlite`] on snapshot read failure, or [`OracleError::Store`]
/// on store read failure.
pub fn compare(db_path: &Path, store: &Store) -> Result<OracleReport, OracleError> {
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?;
    ensure_codegraph(&conn, db_path)?;

    let source_commit = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'snapshot_source_commit'",
            [],
            |r| r.get::<_, String>(0),
        )
        .ok();

    // codegraph side: comparable Rust symbols and internal calls.
    let cg_symbols = codegraph_symbols(&conn)?;
    let constants_codegraph = codegraph_constant_count(&conn)?;
    let cg_calls = codegraph_calls(&conn, &cg_symbols)?;

    // Roteiro side: derive from a single graph dump.
    let facts = store.export_factset()?;
    let ro_symbols: BTreeSet<String> = facts
        .nodes
        .iter()
        .filter(|n| is_comparable_kind(&n.kind))
        .map(|n| n.key.clone())
        .collect();
    let ro_calls: BTreeSet<(String, String)> = facts
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Calls)
        .map(|e| (e.src.clone(), e.dst.clone()))
        .collect();

    let matched = cg_symbols.intersection(&ro_symbols).count();

    // Of the non-exact remainder, a symbol both tools found in the same file
    // under the same leaf name (but a different scope, e.g. `#foo` vs
    // `#tests::foo`) is a scoping difference, not a real gap. Group the
    // remainder by (path, leaf) to separate those from genuine divergence.
    let cg_rest: Vec<&String> = cg_symbols.difference(&ro_symbols).collect();
    let ro_rest: Vec<&String> = ro_symbols.difference(&cg_symbols).collect();
    let cg_rest_leaves: BTreeSet<(String, String)> = cg_rest.iter().map(|k| path_leaf(k)).collect();
    let ro_rest_leaves: BTreeSet<(String, String)> = ro_rest.iter().map(|k| path_leaf(k)).collect();
    let scope_diff = cg_rest_leaves.intersection(&ro_rest_leaves).count();

    let cg_only: Vec<String> = cg_rest
        .into_iter()
        .filter(|k| !ro_rest_leaves.contains(&path_leaf(k)))
        .cloned()
        .collect();
    let ro_only: Vec<String> = ro_rest
        .into_iter()
        .filter(|k| !cg_rest_leaves.contains(&path_leaf(k)))
        .cloned()
        .collect();

    let calls_agree = cg_calls.iter().filter(|c| ro_calls.contains(*c)).count();

    Ok(OracleReport {
        schema: ORACLE_SCHEMA,
        source_commit,
        symbols_codegraph: cg_symbols.len(),
        symbols_roteiro: ro_symbols.len(),
        symbols_matched: matched,
        symbols_scope_diff: scope_diff,
        codegraph_only: cg_only.len(),
        roteiro_only: ro_only.len(),
        codegraph_only_sample: cg_only.into_iter().take(SAMPLE_CAP).collect(),
        roteiro_only_sample: ro_only.into_iter().take(SAMPLE_CAP).collect(),
        constants_codegraph,
        calls_codegraph: cg_calls.len(),
        calls_agree,
        calls_codegraph_only: cg_calls.len() - calls_agree,
    })
}

/// Whether a Roteiro node kind is one codegraph also extracts (the comparable
/// overlap): functions/methods, structs, enums, traits.
fn is_comparable_kind(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Fn | NodeKind::Struct | NodeKind::Enum | NodeKind::Trait
    )
}

/// Confirm the snapshot has codegraph's core tables.
fn ensure_codegraph(conn: &Connection, path: &Path) -> Result<(), OracleError> {
    for table in ["files", "nodes", "edges"] {
        let present: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
                [table],
                |_| Ok(true),
            )
            .unwrap_or(false);
        if !present {
            return Err(OracleError::NotCodegraph(format!(
                "{} has no `{table}` table",
                path.display()
            )));
        }
    }
    Ok(())
}

/// The Roteiro key for a codegraph symbol at `path` with codegraph `qualified`
/// (`.`-scoped): `sym:rust:<path>#<qualified with '.'→'::'>`.
fn symbol_key(path: &str, qualified: &str) -> String {
    format!("sym:rust:{path}#{}", qualified.replace('.', "::"))
}

/// The `(path, leaf-name)` of a `sym:rust:<path>#<qual>` key, where the leaf is
/// the final `::`-segment of the qualified name. Used to detect the same symbol
/// keyed under a different scope (`#foo` vs `#tests::foo`).
fn path_leaf(key: &str) -> (String, String) {
    let after = key.strip_prefix("sym:rust:").unwrap_or(key);
    match after.split_once('#') {
        Some((path, qual)) => (
            path.to_owned(),
            qual.rsplit("::").next().unwrap_or(qual).to_owned(),
        ),
        None => (after.to_owned(), String::new()),
    }
}

/// Comparable Rust symbol keys from the snapshot (functions/structs/enums/traits
/// in `.rs` files, matching Roteiro's extraction scope).
fn codegraph_symbols(conn: &Connection) -> Result<BTreeSet<String>, OracleError> {
    let mut stmt = conn.prepare(
        "SELECT f.path, COALESCE(n.qualified_name, n.name)
         FROM nodes n JOIN files f ON f.id = n.file_id
         WHERE n.type IN ('function','struct','enum','trait')
           AND f.path LIKE '%.rs'",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(symbol_key(&r.get::<_, String>(0)?, &r.get::<_, String>(1)?))
    })?;
    let mut set = BTreeSet::new();
    for row in rows {
        set.insert(row?);
    }
    Ok(set)
}

/// How many constants codegraph extracted from Rust files (a Roteiro gap).
fn codegraph_constant_count(conn: &Connection) -> Result<usize, OracleError> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM nodes n JOIN files f ON f.id = n.file_id
         WHERE n.type = 'constant' AND f.path LIKE '%.rs'",
        [],
        |r| r.get(0),
    )?;
    Ok(usize::try_from(n).unwrap_or(0))
}

/// Internal function→function `calls` from the snapshot, as Roteiro key pairs,
/// restricted to calls whose endpoints are both comparable Rust symbols.
fn codegraph_calls(
    conn: &Connection,
    symbols: &BTreeSet<String>,
) -> Result<BTreeSet<(String, String)>, OracleError> {
    let mut stmt = conn.prepare(
        "SELECT sf.path, COALESCE(s.qualified_name, s.name),
                tf.path, COALESCE(t.qualified_name, t.name)
         FROM edges e
         JOIN nodes s ON s.id = e.source_id JOIN files sf ON sf.id = s.file_id
         JOIN nodes t ON t.id = e.target_id JOIN files tf ON tf.id = t.file_id
         WHERE e.relation = 'calls'
           AND s.type = 'function' AND t.type = 'function'
           AND sf.path LIKE '%.rs' AND tf.path LIKE '%.rs'",
    )?;
    let rows = stmt.query_map([], |r| {
        let src = symbol_key(&r.get::<_, String>(0)?, &r.get::<_, String>(1)?);
        let dst = symbol_key(&r.get::<_, String>(2)?, &r.get::<_, String>(3)?);
        Ok((src, dst))
    })?;
    let mut set = BTreeSet::new();
    for row in rows {
        let (src, dst) = row?;
        // Only compare calls whose endpoints codegraph itself emitted as symbols.
        if symbols.contains(&src) && symbols.contains(&dst) {
            set.insert((src, dst));
        }
    }
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::{OracleError, compare};
    use crate::{Edge, EdgeKind, FactSet, Node, NodeKind, Store};
    use rusqlite::Connection;

    /// Build a minimal codegraph-schema snapshot at `path`.
    fn write_snapshot(path: &std::path::Path) {
        let conn = Connection::open(path).expect("open");
        conn.execute_batch(
            "CREATE TABLE files (id INTEGER PRIMARY KEY, path TEXT);
             CREATE TABLE nodes (id INTEGER PRIMARY KEY, file_id INTEGER, type TEXT,
                 name TEXT, qualified_name TEXT);
             CREATE TABLE edges (id INTEGER PRIMARY KEY, source_id INTEGER,
                 target_id INTEGER, relation TEXT);
             CREATE TABLE meta (key TEXT, value TEXT);
             INSERT INTO meta VALUES ('snapshot_source_commit', 'abc123');
             INSERT INTO files VALUES (1, 'src/lib.rs');
             -- Shared: Store (struct), Store.open (method), helper (fn).
             INSERT INTO nodes VALUES (1, 1, 'struct',   'Store',  'Store');
             INSERT INTO nodes VALUES (2, 1, 'function', 'open',   'Store.open');
             INSERT INTO nodes VALUES (3, 1, 'function', 'helper', 'helper');
             -- codegraph-only: a constant (Roteiro gap) and an extra fn.
             INSERT INTO nodes VALUES (4, 1, 'constant', 'MAX',    'MAX');
             INSERT INTO nodes VALUES (5, 1, 'function', 'only_cg','only_cg');
             -- scope difference: codegraph keys a test fn bare, Roteiro scopes it.
             INSERT INTO nodes VALUES (6, 1, 'function', 'foo',    'foo');
             -- calls: Store.open -> helper.
             INSERT INTO edges VALUES (1, 2, 3, 'calls');",
        )
        .expect("seed");
    }

    #[test]
    fn compares_symbols_and_calls() {
        let dir = std::env::temp_dir().join(format!("roteiro-oracle-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let db = dir.join("cg.db");
        write_snapshot(&db);

        // Roteiro side: shares Store, Store::open, helper; has its own `roteiro_only`
        // fn; lacks `only_cg` and the constant. The call open->helper matches.
        let mut store = Store::open_in_memory().expect("store");
        let facts = FactSet::new()
            .with_node(Node::new(
                "sym:rust:src/lib.rs#Store",
                NodeKind::Struct,
                "Store",
            ))
            .with_node(Node::new(
                "sym:rust:src/lib.rs#Store::open",
                NodeKind::Fn,
                "open",
            ))
            .with_node(Node::new(
                "sym:rust:src/lib.rs#helper",
                NodeKind::Fn,
                "helper",
            ))
            .with_node(Node::new(
                "sym:rust:src/lib.rs#roteiro_only",
                NodeKind::Fn,
                "roteiro_only",
            ))
            // Same `foo` codegraph found, but scoped under the test module.
            .with_node(Node::new(
                "sym:rust:src/lib.rs#tests::foo",
                NodeKind::Fn,
                "foo",
            ))
            .with_edge(Edge::derived(
                "sym:rust:src/lib.rs#Store::open",
                "sym:rust:src/lib.rs#helper",
                EdgeKind::Calls,
            ));
        store.apply_factset(&facts).expect("apply");

        let report = compare(&db, &store).expect("compare");
        assert_eq!(report.source_commit.as_deref(), Some("abc123"));
        // 5 comparable codegraph symbols (Store, Store::open, helper, only_cg, foo).
        assert_eq!(report.symbols_codegraph, 5);
        assert_eq!(report.symbols_roteiro, 5);
        assert_eq!(report.symbols_matched, 3, "Store, Store::open, helper");
        assert_eq!(report.symbols_scope_diff, 1, "foo vs tests::foo");
        assert_eq!(report.codegraph_only, 1, "only_cg (genuine)");
        assert_eq!(report.roteiro_only, 1, "roteiro_only (genuine)");
        assert_eq!(
            report.codegraph_only_sample,
            vec!["sym:rust:src/lib.rs#only_cg"]
        );
        assert_eq!(report.constants_codegraph, 1, "MAX is a Roteiro gap");
        // The one internal call is present in both.
        assert_eq!(report.calls_codegraph, 1);
        assert_eq!(report.calls_agree, 1);
        assert_eq!(report.calls_codegraph_only, 0);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_non_codegraph_db() {
        let dir = std::env::temp_dir().join(format!("roteiro-oracle-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let db = dir.join("plain.db");
        let conn = Connection::open(&db).expect("open");
        conn.execute_batch("CREATE TABLE whatever (x INTEGER);")
            .expect("seed");
        drop(conn);

        let store = Store::open_in_memory().expect("store");
        let err = compare(&db, &store).expect_err("should reject");
        assert!(matches!(err, OracleError::NotCodegraph(_)));

        std::fs::remove_dir_all(&dir).ok();
    }
}

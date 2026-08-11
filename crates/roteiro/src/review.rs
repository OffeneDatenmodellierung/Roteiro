//! `roteiro review`: graph-grounded review context for the current change
//! (Stage 17). The CLI-first surface for a context-aware review — a human or an
//! agent can see, for the working-tree change, *what the graph knows* about each
//! touched symbol (who calls it, what governs it, what it's related to), the
//! authored-layer drift the change introduces, the intent-debt it adds, and the
//! blast radius of dependents to check — rather than reviewing the diff in
//! isolation. The MCP `explain`/`path`/`debt` tools expose the same graph as a
//! bonus; this command needs no server.

use std::collections::BTreeSet;

use rto_graph::{NodeContext, NodeKind, Store, StoreError, build_context, dependents};
use serde::Serialize;

/// Schema tag for the `--json` review report.
pub const REVIEW_SCHEMA: &str = "roteiro.review/v1";

/// A graph-grounded review of the working-tree change.
#[derive(Debug, Serialize)]
pub struct ReviewReport {
    /// Stable schema tag.
    pub schema: &'static str,
    /// Number of changed tracked files reviewed.
    pub changed_files: usize,
    /// Per-file review context.
    pub files: Vec<FileReview>,
    /// Authored-layer violations the change touches (drift to resolve first).
    pub drift: Vec<DriftItem>,
    /// Keys of nodes *outside* the change whose context includes a changed
    /// symbol — the blast radius to check for ripple effects.
    pub impacted: Vec<Impacted>,
}

impl ReviewReport {
    /// Whether the change introduces authored-layer drift (review should resolve
    /// it before merging).
    #[must_use]
    pub fn has_drift(&self) -> bool {
        !self.drift.is_empty()
    }
}

/// The review context for one changed file.
#[derive(Debug, Serialize)]
pub struct FileReview {
    /// Repository-relative path.
    pub path: String,
    /// Change status: currently `"added"`, `"modified"`, or `"deleted"`. This is
    /// an **open set** within `roteiro.review/v1` — consumers must treat an
    /// unrecognised value as a generic change (see `docs/JSON_SCHEMA.md`).
    pub status: &'static str,
    /// Symbols defined in the file, each with its graph neighbourhood.
    pub symbols: Vec<SymbolReview>,
    /// Intent-debt markers present in the file.
    pub debt: Vec<String>,
}

/// One changed symbol and the graph's view of it.
#[derive(Debug, Serialize)]
pub struct SymbolReview {
    /// Node key (`sym:<lang>:<path>#<Name>`).
    pub key: String,
    /// Simple name.
    pub name: String,
    /// Node kind token (`fn`, `struct`, …).
    pub kind: String,
    /// Keys that call this symbol — break these and you break them.
    pub callers: Vec<String>,
    /// Keys this symbol calls.
    pub callees: Vec<String>,
    /// Authored nodes (ADRs / sections) that link to this symbol — the intent
    /// governing it, to keep the change consistent with.
    pub governed_by: Vec<String>,
    /// Inferred (similarity) neighbours, with confidence.
    pub related: Vec<Related>,
}

/// An inferred neighbour of a symbol.
#[derive(Debug, Serialize)]
pub struct Related {
    /// The related node's key.
    pub node: String,
    /// Similarity confidence.
    pub confidence: Option<f64>,
}

/// An authored-layer violation the change touches.
#[derive(Debug, Serialize)]
pub struct DriftItem {
    /// Violation category label.
    pub kind: String,
    /// Human-readable message.
    pub message: String,
}

/// A node outside the change whose context includes a changed symbol.
#[derive(Debug, Serialize)]
pub struct Impacted {
    /// The node's key.
    pub key: String,
    /// Simple name.
    pub name: String,
    /// Node kind token.
    pub kind: String,
}

/// Assemble the review report for `changed`, using the already-synced `store`
/// (built from the same working tree) and the authored-layer `violations` the
/// change produced.
///
/// # Errors
/// Returns [`StoreError`] on a store query failure.
pub fn build(
    store: &Store,
    changed: &[rto_graph::ChangedFile],
    violations: &[rto_spec::Violation],
) -> Result<ReviewReport, StoreError> {
    let changed_paths: BTreeSet<&str> = changed.iter().map(|c| c.path.as_str()).collect();
    let mut files = Vec::new();
    let mut changed_keys: Vec<String> = Vec::new();

    for cf in changed {
        if cf.status == rto_graph::ChangeStatus::Deleted {
            files.push(FileReview {
                path: cf.path.clone(),
                status: "deleted",
                symbols: Vec::new(),
                debt: Vec::new(),
            });
            continue;
        }
        let mut symbols = Vec::new();
        let mut debt = Vec::new();
        for node in store.nodes_by_path(&cf.path)? {
            match node.kind {
                // The file node itself carries no reviewable neighbourhood.
                NodeKind::File => continue,
                NodeKind::Marker => {
                    debt.push(node.name.clone());
                    continue;
                }
                _ => {}
            }
            changed_keys.push(node.key.clone());
            let ctx = build_context(store, &node.key)?;
            symbols.push(symbol_review(&node, ctx.as_ref()));
        }
        files.push(FileReview {
            path: cf.path.clone(),
            status: cf.status.as_str(),
            symbols,
            debt,
        });
    }

    // Drift the change touches. A violation belongs to the change when its
    // message names a changed path *or* its subject node lives in a changed file
    // — the latter catches a broken ADR link whose message leads with the ADR's
    // node key (e.g. `adr:0001#decision: …`), not the ADR file path.
    let mut drift = Vec::new();
    for v in violations {
        if violation_touches(store, v, &changed_paths)? {
            drift.push(DriftItem {
                kind: v.kind.label().to_owned(),
                message: v.message.clone(),
            });
        }
    }

    // Blast radius: one-hop dependents of the changed symbols, minus the changed
    // symbols themselves and anything defined in a changed file (already shown).
    let changed_set: BTreeSet<&str> = changed_keys.iter().map(String::as_str).collect();
    let mut impacted = Vec::new();
    for key in dependents(store, &changed_keys)? {
        if changed_set.contains(key.as_str()) {
            continue;
        }
        let Some(node) = store.get_node(&key)? else {
            continue;
        };
        if node
            .path
            .as_deref()
            .is_some_and(|p| changed_paths.contains(p))
        {
            continue;
        }
        impacted.push(Impacted {
            key: node.key,
            name: node.name,
            kind: node.kind.as_str().to_owned(),
        });
    }

    Ok(ReviewReport {
        schema: REVIEW_SCHEMA,
        changed_files: changed.len(),
        files,
        drift,
        impacted,
    })
}

/// Whether an authored-layer `violation` belongs to the change: its message
/// either names a changed path, or its subject node (the key before the first
/// `": "` — node keys carry no colon-space) resolves to a node in a changed file.
fn violation_touches(
    store: &Store,
    violation: &rto_spec::Violation,
    changed_paths: &BTreeSet<&str>,
) -> Result<bool, StoreError> {
    if changed_paths.iter().any(|p| violation.message.contains(p)) {
        return Ok(true);
    }
    if let Some((key, _)) = violation.message.split_once(": ")
        && let Some(node) = store.get_node(key)?
    {
        return Ok(node
            .path
            .as_deref()
            .is_some_and(|p| changed_paths.contains(p)));
    }
    Ok(false)
}

/// Classify a changed node's one-hop context into a reviewer-facing summary.
// `callers`/`callees` are the standard call-graph terms; keep them despite being
// one character apart.
#[allow(clippy::similar_names)]
fn symbol_review(node: &rto_graph::Node, ctx: Option<&NodeContext>) -> SymbolReview {
    let mut callers = Vec::new();
    let mut callees = Vec::new();
    let mut governed_by = Vec::new();
    let mut related = Vec::new();
    if let Some(ctx) = ctx {
        // `related` is specifically the similarity relation (`EdgeKind::Related`),
        // not every inferred edge — inferred `references` etc. would be noise.
        for e in &ctx.incoming {
            if e.kind == "calls" {
                callers.push(e.node.clone());
            }
            if e.provenance == "authored" {
                governed_by.push(e.node.clone());
            }
            if e.kind == "related" {
                related.push(Related {
                    node: e.node.clone(),
                    confidence: e.confidence,
                });
            }
        }
        for e in &ctx.outgoing {
            if e.kind == "calls" {
                callees.push(e.node.clone());
            }
            if e.kind == "related" {
                related.push(Related {
                    node: e.node.clone(),
                    confidence: e.confidence,
                });
            }
        }
    }
    SymbolReview {
        key: node.key.clone(),
        name: node.name.clone(),
        kind: node.kind.as_str().to_owned(),
        callers,
        callees,
        governed_by,
        related,
    }
}

#[cfg(test)]
mod tests {
    /// Freeze the `--json` schema tags (see `docs/JSON_SCHEMA.md`). These are the
    /// stable, versioned contracts; changing one is a breaking change that must
    /// bump the version deliberately — so a change here is caught in CI.
    #[test]
    fn json_schema_tags_are_frozen() {
        assert_eq!(super::REVIEW_SCHEMA, "roteiro.review/v1");
        assert_eq!(rto_graph::SCHEMA, "roteiro.query/v1");
        assert_eq!(rto_graph::ARTIFACT_SCHEMA, "roteiro.graph/v1");
        assert_eq!(rto_graph::ORACLE_SCHEMA, "roteiro.oracle/v1");
        assert_eq!(rto_spec::SPEC_SCHEMA, "roteiro.spec/v1");
    }
}

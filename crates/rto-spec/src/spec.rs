//! The authoring pillar (`roteiro spec`, ADR-0004), Tier 0: deterministic,
//! **graph-grounded** context assembly — no model, no network.
//!
//! [`context`] answers "what does the graph already know about <topic>?" by
//! searching the store for related symbols and docs and gathering each symbol's
//! neighbourhood (its container, callers/callees, and the ADRs that govern it).
//! It is the grounding an author or agent starts from before writing an ADR or
//! blueprint, so generated intent references *real* nodes rather than
//! hallucinated ones.

use std::collections::BTreeSet;

use rto_graph::{NodeSummary, Store, StoreError, explain, search};

/// Versioned schema tag for authoring outputs, so agents can depend on the shape.
pub const SPEC_SCHEMA: &str = "roteiro.spec/v1";

/// A code symbol related to the topic, with the slice of its graph neighbourhood
/// that grounds authoring: what defines it, what it calls / is called by, and the
/// authored ADRs/sections that govern it.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SymbolContext {
    /// The symbol node.
    pub node: NodeSummary,
    /// Key of the node that `contains`/`defines` it (its file or parent), if any.
    pub container: Option<String>,
    /// Keys this symbol `calls`.
    pub calls: Vec<String>,
    /// Keys that `call` this symbol.
    pub called_by: Vec<String>,
    /// Keys of ADR/section nodes with an `authored` edge to this symbol.
    pub authored_by: Vec<String>,
}

/// Graph-grounded context for a topic: the related symbols (with neighbourhood),
/// related docs/ADRs, and the set of ADRs that govern any matched symbol.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SpecContext {
    /// Stable schema tag ([`SPEC_SCHEMA`]).
    pub schema: &'static str,
    /// The topic that was searched.
    pub topic: String,
    /// Matched code symbols with their neighbourhood, most relevant first.
    pub symbols: Vec<SymbolContext>,
    /// Matched docs (ADRs, sections, blueprints, lat/imported docs).
    pub docs: Vec<NodeSummary>,
    /// Keys of ADR-layer nodes governing the matched symbols or matched directly.
    pub related_adrs: Vec<String>,
}

/// Node kinds treated as code symbols for authoring context.
const SYMBOL_KINDS: &[&str] = &["fn", "struct", "enum", "trait", "module"];
/// Node kinds treated as authored/imported documentation.
const DOC_KINDS: &[&str] = &["adr", "adr_section", "blueprint", "doc", "lat_section"];

/// Assemble graph-grounded [`SpecContext`] for `topic`, keeping up to `limit`
/// symbols and up to `limit` docs (most relevant first).
///
/// # Errors
/// Returns [`StoreError`] on query failure.
pub fn context(store: &Store, topic: &str, limit: usize) -> Result<SpecContext, StoreError> {
    // Over-fetch candidates so we can keep the top `limit` of each category.
    let hits = search(store, topic, limit.saturating_mul(3).max(30))?;

    let mut symbols = Vec::new();
    let mut docs = Vec::new();
    let mut related_adrs: BTreeSet<String> = BTreeSet::new();

    for hit in hits {
        let kind = hit.node.kind.as_str();
        if SYMBOL_KINDS.contains(&kind) {
            if symbols.len() >= limit {
                continue;
            }
            let Some(ex) = explain(store, &hit.node.key)? else {
                continue;
            };
            let container = ex
                .incoming
                .iter()
                .find(|e| e.kind == "contains" || e.kind == "defines")
                .map(|e| e.node.clone());
            let calls = edges_of(&ex.outgoing, "calls");
            let called_by = edges_of(&ex.incoming, "calls");
            let authored_by: Vec<String> = ex
                .incoming
                .iter()
                .filter(|e| e.provenance == "authored")
                .map(|e| e.node.clone())
                .collect();
            related_adrs.extend(authored_by.iter().cloned());
            symbols.push(SymbolContext {
                node: hit.node,
                container,
                calls,
                called_by,
                authored_by,
            });
        } else if DOC_KINDS.contains(&kind) {
            if kind == "adr" || kind == "adr_section" {
                related_adrs.insert(hit.node.key.clone());
            }
            if docs.len() < limit {
                docs.push(hit.node);
            }
        }
    }

    Ok(SpecContext {
        schema: SPEC_SCHEMA,
        topic: topic.to_owned(),
        symbols,
        docs,
        related_adrs: related_adrs.into_iter().collect(),
    })
}

/// The other-end keys of `edges` whose kind is `kind`.
fn edges_of(edges: &[rto_graph::EdgeRef], kind: &str) -> Vec<String> {
    edges
        .iter()
        .filter(|e| e.kind == kind)
        .map(|e| e.node.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{SPEC_SCHEMA, context};
    use rto_graph::{Edge, EdgeKind, FactSet, Node, NodeKind, Store};

    fn seeded() -> Store {
        let mut store = Store::open_in_memory().expect("store");
        let facts = FactSet::new()
            .with_node(Node::new("file:src/auth.rs", NodeKind::File, "auth.rs"))
            .with_node(Node::new(
                "sym:rust:src/auth.rs#validate_token",
                NodeKind::Fn,
                "validate_token",
            ))
            .with_node(Node::new(
                "sym:rust:src/auth.rs#login",
                NodeKind::Fn,
                "login",
            ))
            .with_node(Node::new(
                "adr:0007",
                NodeKind::Adr,
                "Authentication design",
            ))
            // Structure + calls + an authored ADR link into validate_token.
            .with_edge(Edge::derived(
                "file:src/auth.rs",
                "sym:rust:src/auth.rs#validate_token",
                EdgeKind::Defines,
            ))
            .with_edge(Edge::derived(
                "sym:rust:src/auth.rs#login",
                "sym:rust:src/auth.rs#validate_token",
                EdgeKind::Calls,
            ))
            .with_edge(Edge::authored(
                "adr:0007",
                "sym:rust:src/auth.rs#validate_token",
                EdgeKind::References,
            ));
        store.apply_factset(&facts).expect("apply");
        store
    }

    #[test]
    fn context_grounds_a_symbol_in_its_neighbourhood() {
        let store = seeded();
        let ctx = context(&store, "validate_token", 10).expect("context");
        assert_eq!(ctx.schema, SPEC_SCHEMA);

        let sym = ctx
            .symbols
            .iter()
            .find(|s| s.node.key == "sym:rust:src/auth.rs#validate_token")
            .expect("the symbol");
        assert_eq!(sym.container.as_deref(), Some("file:src/auth.rs"));
        assert_eq!(sym.called_by, vec!["sym:rust:src/auth.rs#login"]);
        assert_eq!(sym.authored_by, vec!["adr:0007"]);
        // The governing ADR is surfaced as related.
        assert!(ctx.related_adrs.contains(&"adr:0007".to_owned()));
    }

    #[test]
    fn context_finds_related_docs_by_topic() {
        let store = seeded();
        // "authentication" matches the ADR's name.
        let ctx = context(&store, "authentication", 10).expect("context");
        assert!(
            ctx.docs.iter().any(|d| d.key == "adr:0007"),
            "the ADR should be a related doc: {:?}",
            ctx.docs
        );
        assert!(ctx.related_adrs.contains(&"adr:0007".to_owned()));
    }

    #[test]
    fn empty_topic_yields_empty_context() {
        let store = seeded();
        let ctx = context(&store, "   ", 10).expect("context");
        assert!(ctx.symbols.is_empty() && ctx.docs.is_empty());
    }
}

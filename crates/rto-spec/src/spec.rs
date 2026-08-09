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
    if limit == 0 {
        return Ok(SpecContext {
            schema: SPEC_SCHEMA,
            topic: topic.to_owned(),
            symbols: Vec::new(),
            docs: Vec::new(),
            related_adrs: Vec::new(),
        });
    }
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

/// Generate a **house-style ADR skeleton** for `topic`, grounded in `ctx`:
/// correct frontmatter (id `adr_id`, `Draft`), the house section headings with
/// placeholders, a clarify **interview checklist**, and a build-plan outline. The
/// `[[…]]` links it emits — affected symbols and related ADRs — are drawn from
/// the graph, so they resolve and the scaffold is `roteiro check`-clean by
/// construction. `date` is `YYYY-MM-DD` (the caller supplies today's date).
#[must_use]
pub fn scaffold_adr(
    topic: &str,
    title: Option<&str>,
    adr_id: &str,
    date: &str,
    ctx: &SpecContext,
) -> String {
    use std::fmt::Write as _;

    let title = title.unwrap_or(topic);
    // Grounded links: affected symbols as `[[path#Symbol]]`, related ADRs as
    // `[[docs/adr/…md]]` — both resolve against real nodes.
    let symbol_links: Vec<String> = ctx
        .symbols
        .iter()
        .filter_map(|s| symbol_link_target(&s.node.key))
        .map(|t| format!("[[{t}]]"))
        .collect();
    let adr_links: Vec<String> = ctx
        .docs
        .iter()
        .filter(|d| d.kind == "adr")
        .filter_map(|d| d.path.clone())
        .map(|p| format!("[[{p}]]"))
        .collect();
    let files: Vec<String> = {
        let mut fs: Vec<String> = ctx
            .symbols
            .iter()
            .filter_map(|s| s.node.path.clone())
            .collect();
        fs.sort();
        fs.dedup();
        fs
    };

    let mut out = String::new();
    let _ = write!(
        out,
        "---\n\
         Title: {title}\n\
         Space: ARCH\n\
         Parent: ADRs\n\n\
         # ADR-specific metadata (unknown keys are ignored; used for indexing/search)\n\
         type: adr\n\
         adr-id: \"{adr_id}\"\n\
         status: Draft                       # Draft | For Review | Accepted | Rejected | Superseded\n\
         architectural-significance: MEDIUM  # SOFT | LOW | MEDIUM | HIGH | VERY HIGH\n\
         domain: Developer Tooling\n\
         decision-makers: [\"The Roteiro Project Team\"]\n\
         superseded-by:\n\
         version: \"0.1\"\n\
         last-modified: {date}\n\
         confluence-url:\n\
         ---\n\n\
         # ADR-{adr_id}: {title}\n\n\
         | | |\n|---|---|\n\
         | **State** | Draft |\n\
         | **Architectural Significance** | MEDIUM |\n\
         | **Domain** | Developer Tooling |\n\
         | **Document version** | 0.1 |\n\n\
         ## Reference\n\n\
         _Scaffolded by `roteiro spec` and grounded in the graph — the links below\n\
         already resolve against real nodes; fill in the prose._\n\n"
    );

    if !adr_links.is_empty() {
        let _ = writeln!(out, "Related decisions: {}.\n", adr_links.join(", "));
    }
    if !symbol_links.is_empty() {
        let _ = writeln!(out, "Affected code: {}.\n", symbol_links.join(", "));
    }

    out.push_str(
        "## Summary\n\n\
         _TODO: the decision in a sentence or two._\n\n\
         ## Context\n\n\
         _TODO: the forces at play and why a decision is needed now._\n\n\
         ## Interview — clarify before writing\n\n\
         - [ ] What problem does this solve, and who has it?\n\
         - [ ] Which existing ADRs does this relate to or supersede? (see Reference)\n\
         - [ ] Are the affected symbols above the right scope — anything missing?\n\
         - [ ] What options were considered, and why this one?\n\
         - [ ] What are the consequences, costs, and risks?\n\n\
         ## Decision makers\n\n\
         - The Roteiro Project Team\n\n\
         ## Recommended option\n\n_TODO._\n\n\
         ## Options considered + consequences\n\n_TODO._\n\n\
         ## Consequences\n\n_TODO._\n\n\
         ## Build-plan outline (grounded)\n\n",
    );

    if files.is_empty() && adr_links.is_empty() {
        out.push_str("_No related graph facts found for this topic yet._\n\n");
    } else {
        for f in &files {
            let _ = writeln!(out, "- Touches `{f}`");
        }
        if !adr_links.is_empty() {
            let _ = writeln!(out, "- Reconcile with: {}", adr_links.join(", "));
        }
        out.push('\n');
    }

    let _ = write!(
        out,
        "## Document version history\n\n\
         | Version | Date | Notes |\n\
         |---------|------|-------|\n\
         | 0.1 | {date} | Draft scaffold generated by `roteiro spec scaffold`. |\n"
    );
    out
}

/// The `path#Symbol` wiki-link target reconstructed from a `sym:<lang>:<path>#…`
/// key (dropping the `sym:<lang>:` prefix), or `None` if not a symbol key.
fn symbol_link_target(key: &str) -> Option<&str> {
    key.strip_prefix("sym:")
        .and_then(|rest| rest.split_once(':'))
        .map(|(_lang, target)| target)
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

    #[test]
    fn scaffold_is_grounded_and_check_clean() {
        use super::scaffold_adr;
        let mut store = seeded();
        let ctx = context(&store, "validate_token", 10).expect("context");
        let md = scaffold_adr(
            "validate_token",
            Some("Token validation"),
            "0099",
            "2026-08-09",
            &ctx,
        );

        // House frontmatter + title with the given id.
        assert!(md.contains("adr-id: \"0099\""), "{md}");
        assert!(md.contains("# ADR-0099: Token validation"));
        // Grounded affected-code link and the interview checklist.
        assert!(
            md.contains("[[src/auth.rs#validate_token]]"),
            "grounded link: {md}"
        );
        assert!(md.contains("- [ ] What problem does this solve"));

        // It parses as a house ADR and its links resolve to a real node — so it
        // is `check`-clean by construction.
        let doc = crate::parse_adr("docs/adr/0099-token-validation.md", &md).expect("parse");
        assert!(
            doc.links
                .iter()
                .any(|l| l.target_key == "sym:rust:src/auth.rs#validate_token"),
            "the scaffold's link must resolve to the real symbol: {:?}",
            doc.links,
        );
        let report = crate::run(&mut store, std::slice::from_ref(&doc), &[]).expect("check");
        assert_eq!(
            report.violations.len(),
            0,
            "scaffold must be check-clean: {:?}",
            report.violations
        );
    }

    #[test]
    fn scaffold_has_no_code_block_indentation() {
        use super::scaffold_adr;
        let store = seeded();
        let ctx = context(&store, "validate_token", 10).expect("context");
        let md = scaffold_adr("validate_token", None, "0099", "2026-08-09", &ctx);
        // The `\`-line-continuations in the template strip source indentation, so
        // no line begins with whitespace; a 4-space indent would (wrongly) render
        // the frontmatter/headings as a CommonMark code block.
        for (i, line) in md.lines().enumerate() {
            assert!(
                !line.starts_with(' ') && !line.starts_with('\t'),
                "line {} has leading whitespace: {line:?}",
                i + 1
            );
        }
    }
}

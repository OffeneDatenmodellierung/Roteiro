//! The agent- and human-facing query surface over the graph.
//!
//! Everything here is a read-only view built from the store's typed queries,
//! serialised under a **stable, versioned** JSON schema ([`SCHEMA`]) so agents
//! can depend on the shape. The primitives are [`explain`] (a node and its
//! provenance-labelled neighbourhood), [`list_kind`] (all nodes of a kind),
//! [`path`] (a shortest path between two nodes), [`debt`] (the intent-debt marker
//! inventory), and [`search`] (relevance-ranked node search). All return
//! mixed-provenance results — the "one query surface" from ADR-0001 — with every
//! edge carrying its `provenance`.

use std::collections::{BTreeMap, VecDeque};

use serde::Serialize;

use crate::store::{Store, StoreError};
use crate::{Edge, NodeKind, Provenance};

/// The versioned schema tag emitted on every query result. Bump the version on
/// any breaking change to the shape.
pub const SCHEMA: &str = "roteiro.query/v1";

/// A compact node summary (used in listings and as the subject of an
/// [`Explanation`]).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NodeSummary {
    /// Natural key.
    pub key: String,
    /// Kind token (e.g. `fn`, `adr`).
    pub kind: String,
    /// Human-facing name.
    pub name: String,
    /// Repository-relative path, if any.
    pub path: Option<String>,
    /// Language token, if any.
    pub lang: Option<String>,
}

impl NodeSummary {
    fn from_node(node: &crate::Node) -> Self {
        Self {
            key: node.key.clone(),
            kind: node.kind.as_str().to_owned(),
            name: node.name.clone(),
            path: node.path.clone(),
            lang: node.lang.clone(),
        }
    }
}

/// One end of an edge as seen from a subject node: the relationship, how it was
/// produced, and the node on the other end.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EdgeRef {
    /// Edge kind token (e.g. `calls`, `references`).
    pub kind: String,
    /// How the edge was produced (`derived` | `authored` | `inferred`).
    pub provenance: &'static str,
    /// Confidence score, present only for inferred edges.
    pub confidence: Option<f64>,
    /// The natural key of the node at the other end.
    pub node: String,
}

/// A node together with its provenance-labelled neighbourhood.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Explanation {
    /// Stable schema tag ([`SCHEMA`]).
    pub schema: &'static str,
    /// The subject node.
    pub node: NodeSummary,
    /// Structured metadata attached to the node.
    pub meta: serde_json::Value,
    /// Edges where the subject is the source.
    pub outgoing: Vec<EdgeRef>,
    /// Edges where the subject is the destination.
    pub incoming: Vec<EdgeRef>,
}

/// A listing of all nodes of one kind.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Listing {
    /// Stable schema tag ([`SCHEMA`]).
    pub schema: &'static str,
    /// The kind that was listed.
    pub kind: String,
    /// Matching nodes, ordered by key.
    pub nodes: Vec<NodeSummary>,
}

/// One intent-debt finding in a [`DebtReport`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DebtItem {
    /// Natural key of the marker node (`marker:<path>#<line>`).
    pub key: String,
    /// Category token (`todo` | `fixme` | `hack` | `stub` | `deferred`).
    pub category: String,
    /// The marker text (the trimmed source line).
    pub text: String,
    /// Repository-relative path of the source file, if any.
    pub path: Option<String>,
    /// 1-based line number, if recorded.
    pub line: Option<u32>,
}

/// The intent-debt inventory: every `marker` node, grouped and listed. A
/// deterministic, provenance-`derived` view of what is incomplete or postponed.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DebtReport {
    /// Stable schema tag ([`SCHEMA`]).
    pub schema: &'static str,
    /// Total markers in the report (after any category filter).
    pub total: usize,
    /// Count per category, ordered by category token.
    pub by_category: BTreeMap<String, usize>,
    /// The markers, ordered by `(path, line, key)`.
    pub items: Vec<DebtItem>,
}

/// Inventory intent-debt markers in the graph, optionally restricted to the
/// given `categories` (empty means all) and excluding markers whose file path
/// matches any `ignore` glob (config `[debt] ignore` — empty means keep all).
/// Ordered by `(path, line)` so output is stable and reads top-to-bottom per
/// file; `total` and `by_category` reflect the retained markers only.
///
/// # Errors
/// Returns [`StoreError`] on query failure.
pub fn debt(
    store: &Store,
    categories: &[String],
    ignore: &[String],
) -> Result<DebtReport, StoreError> {
    let filter: std::collections::BTreeSet<&str> = categories.iter().map(String::as_str).collect();
    let mut items = Vec::new();
    let mut by_category: BTreeMap<String, usize> = BTreeMap::new();
    for node in store.nodes_by_kind(&NodeKind::Marker)? {
        let category = node
            .meta
            .get("category")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("other")
            .to_owned();
        if !filter.is_empty() && !filter.contains(category.as_str()) {
            continue;
        }
        // Drop markers under an ignored path (e.g. `vendor/**`) before counting.
        if let Some(path) = node.path.as_deref()
            && ignore.iter().any(|glob| glob_match(glob, path))
        {
            continue;
        }
        let text = node
            .meta
            .get("text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(node.name.as_str())
            .to_owned();
        let line = node
            .meta
            .get("line")
            .and_then(serde_json::Value::as_u64)
            .and_then(|l| u32::try_from(l).ok());
        *by_category.entry(category.clone()).or_default() += 1;
        items.push(DebtItem {
            key: node.key.clone(),
            category,
            text,
            path: node.path.clone(),
            line,
        });
    }
    items.sort_by(|a, b| (&a.path, a.line, &a.key).cmp(&(&b.path, b.line, &b.key)));
    Ok(DebtReport {
        schema: SCHEMA,
        total: items.len(),
        by_category,
        items,
    })
}

/// Match a slash-separated `path` against a glob `pattern`, anchored end-to-end.
/// `?` matches one non-`/` character, `*` matches any run within a single path
/// segment, and `**` matches zero or more whole segments. Used for config
/// `[debt] ignore` patterns (e.g. `vendor/**`, `**/generated/*`).
#[must_use]
fn glob_match(pattern: &str, path: &str) -> bool {
    let pat: Vec<&str> = pattern.split('/').collect();
    let seg: Vec<&str> = path.split('/').collect();
    match_segments(&pat, &seg)
}

/// Anchored match of glob segments `pat` against path segments `seg`, with `**`
/// consuming zero or more segments.
fn match_segments(pat: &[&str], seg: &[&str]) -> bool {
    match pat.first() {
        None => seg.is_empty(),
        Some(&"**") => (0..=seg.len()).any(|i| match_segments(&pat[1..], &seg[i..])),
        Some(token) => {
            !seg.is_empty() && match_token(token, seg[0]) && match_segments(&pat[1..], &seg[1..])
        }
    }
}

/// Match a single path segment `s` against a `pattern` token containing `*`
/// (any run, no `/`) and `?` (one char, no `/`).
fn match_token(pattern: &str, s: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let chars: Vec<char> = s.chars().collect();
    match_token_chars(&pat, &chars)
}

/// Recursive char-slice matcher backing [`match_token`].
fn match_token_chars(pat: &[char], chars: &[char]) -> bool {
    match pat.first() {
        None => chars.is_empty(),
        Some('*') => (0..=chars.len()).any(|i| match_token_chars(&pat[1..], &chars[i..])),
        Some('?') => !chars.is_empty() && match_token_chars(&pat[1..], &chars[1..]),
        Some(&ch) => {
            !chars.is_empty() && chars[0] == ch && match_token_chars(&pat[1..], &chars[1..])
        }
    }
}

/// One step along a [`Path`]: the edge traversed and the node it leads to.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PathHop {
    /// Edge kind token (e.g. `calls`, `contains`).
    pub kind: String,
    /// How the edge was produced.
    pub provenance: &'static str,
    /// Confidence score, present only for inferred edges.
    pub confidence: Option<f64>,
    /// The direction the edge was traversed relative to the previous node
    /// (`outgoing` = along the edge, `incoming` = against it).
    pub direction: &'static str,
    /// The natural key of the node this hop arrives at.
    pub node: String,
}

/// A shortest path between two nodes. Edges are followed in either direction
/// (the graph is treated as undirected for reachability), and each hop records
/// the actual direction and provenance of the edge used.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Path {
    /// Stable schema tag ([`SCHEMA`]).
    pub schema: &'static str,
    /// Natural key of the start node.
    pub from: String,
    /// Natural key of the goal node.
    pub to: String,
    /// Whether a path (including the trivial empty one) was found.
    pub found: bool,
    /// Number of hops (edges) in the path; `0` when `from == to`.
    pub length: usize,
    /// The hops from `from` to `to`, in order.
    pub hops: Vec<PathHop>,
}

fn out_ref(edge: &Edge) -> EdgeRef {
    EdgeRef {
        kind: edge.kind.as_str().to_owned(),
        provenance: edge.provenance.as_str(),
        confidence: edge.confidence,
        node: edge.dst.clone(),
    }
}

fn in_ref(edge: &Edge) -> EdgeRef {
    EdgeRef {
        kind: edge.kind.as_str().to_owned(),
        provenance: edge.provenance.as_str(),
        confidence: edge.confidence,
        node: edge.src.clone(),
    }
}

fn sort_refs(refs: &mut [EdgeRef]) {
    // Include provenance so edges differing only in provenance have a total,
    // stable order; with the edge-uniqueness constraint this key is unique.
    refs.sort_by(|a, b| (&a.kind, &a.node, a.provenance).cmp(&(&b.kind, &b.node, b.provenance)));
}

/// Explain a node: its record plus every incoming and outgoing edge, each
/// labelled with provenance. Returns `None` if no node has that key.
///
/// # Errors
/// Returns [`StoreError`] on query failure.
pub fn explain(store: &Store, key: &str) -> Result<Option<Explanation>, StoreError> {
    let Some(node) = store.get_node(key)? else {
        return Ok(None);
    };
    let mut outgoing: Vec<EdgeRef> = store.edges_from(key)?.iter().map(out_ref).collect();
    let mut incoming: Vec<EdgeRef> = store.edges_to(key)?.iter().map(in_ref).collect();
    sort_refs(&mut outgoing);
    sort_refs(&mut incoming);
    Ok(Some(Explanation {
        schema: SCHEMA,
        node: NodeSummary::from_node(&node),
        meta: node.meta,
        outgoing,
        incoming,
    }))
}

/// List every node of the given `kind`, ordered by key.
///
/// # Errors
/// Returns [`StoreError`] on query failure.
pub fn list_kind(store: &Store, kind: &NodeKind) -> Result<Listing, StoreError> {
    let nodes = store
        .nodes_by_kind(kind)?
        .iter()
        .map(NodeSummary::from_node)
        .collect();
    Ok(Listing {
        schema: SCHEMA,
        kind: kind.as_str().to_owned(),
        nodes,
    })
}

/// A relevance-ranked search hit: a node summary plus its score.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchHit {
    /// Relevance score (higher is better); see [`search`] for how it is derived.
    pub score: u32,
    /// The matching node.
    #[serde(flatten)]
    pub node: NodeSummary,
    /// A short, whitespace-collapsed excerpt of the node's captured
    /// `meta.content` (see [`content_snippet`]), so a model that never calls
    /// [`explain`] still has real grounding text. `None` for pure symbol/config
    /// nodes with no content — the summary (name/kind/path) is the grounding then.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
}

/// Max **chars** of a search-hit content snippet, counting the trailing ellipsis
/// when truncated (so the total length never exceeds this). Bounded so many hits
/// cannot bloat the tool response or blow the served model's context window.
const SNIPPET_MAX: usize = 300;

/// Build a bounded, whitespace-collapsed snippet from a node's captured
/// `meta.content`, or `None` when the node has no textual content (pure symbol/
/// config nodes). Runs of whitespace collapse to single spaces, and the result is
/// at most [`SNIPPET_MAX`] chars *including* a trailing `…` when the content was
/// truncated, so a search hit carries grounding text even when the model never
/// calls [`explain`].
///
/// Processes `content` **lazily**: it collapses whitespace on the fly and stops
/// after ~`SNIPPET_MAX` chars, so a large content-bearing node never materialises
/// more than the bound regardless of how big its content is.
fn content_snippet(meta: &serde_json::Value) -> Option<String> {
    let content = meta.get("content").and_then(|v| v.as_str())?;

    // Collect at most SNIPPET_MAX + 1 collapsed chars: the one extra char only
    // tells us whether the content overflowed the bound (→ needs an ellipsis);
    // we never buffer more than that, however large `content` is.
    let mut collapsed: Vec<char> = Vec::with_capacity(SNIPPET_MAX + 1);
    let mut pending_space = false;
    for ch in content.chars() {
        if ch.is_whitespace() {
            // A run of whitespace becomes a single separator, but only once a
            // real char has been emitted (this also drops any leading whitespace).
            pending_space = !collapsed.is_empty();
            continue;
        }
        if pending_space {
            collapsed.push(' ');
            pending_space = false;
            if collapsed.len() > SNIPPET_MAX {
                break;
            }
        }
        collapsed.push(ch);
        if collapsed.len() > SNIPPET_MAX {
            break;
        }
    }

    if collapsed.is_empty() {
        return None;
    }
    // Overflowed the bound: truncate to SNIPPET_MAX - 1 chars and append the
    // ellipsis, so the total length (ellipsis included) is exactly SNIPPET_MAX.
    if collapsed.len() > SNIPPET_MAX {
        let snippet: String = collapsed[..SNIPPET_MAX - 1].iter().collect();
        Some(format!("{snippet}…"))
    } else {
        Some(collapsed.into_iter().collect())
    }
}

/// Deterministically search nodes for `query`, ranked by relevance, returning at
/// most `limit` hits. Case-insensitive; every whitespace/`::`-separated token must
/// appear somewhere in the node's **name, key, path, or captured `meta.content`**
/// (so a question's words find the *description*, e.g. a README/ADR, not only a
/// same-named symbol). Scoring favours an exact name match, then a name/content
/// substring, then per-token hits; it then **boosts curated intent** (`authored`
/// ADRs/blueprints) and READMEs/overviews and **penalises test scaffolding**, so
/// "what/why" questions land on the real answer rather than a same-named test
/// helper. Ties break by key so results are stable.
///
/// # Errors
/// Returns [`StoreError`] on query failure.
pub fn search(store: &Store, query: &str, limit: usize) -> Result<Vec<SearchHit>, StoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let q = query.trim().to_lowercase();
    // Tokens are separated by whitespace or the `::` path separator; a lone `:`
    // (as in a `sym:rust:…` key) does not split a token.
    let tokens: Vec<&str> = q.split("::").flat_map(str::split_whitespace).collect();
    if tokens.is_empty() {
        return Ok(Vec::new());
    }

    let mut hits: Vec<SearchHit> = Vec::new();
    for node in store.all_nodes()? {
        let name = node.name.to_lowercase();
        let key = node.key.to_lowercase();
        let path = node.path.as_deref().unwrap_or("").to_lowercase();
        // The captured knowledge base (doc comments, prose, ADR/README/blueprint
        // text) is searchable too, so a question's words find the *description*,
        // not just a same-named symbol. Only lowercase when a node actually has
        // content — most nodes (code symbols) don't, so skip the allocation.
        let content = node
            .meta
            .get("content")
            .and_then(|v| v.as_str())
            .map(str::to_lowercase);
        let content = content.as_deref().unwrap_or("");
        // Require every token to appear somewhere (including content), so a
        // multi-word query narrows.
        if !tokens
            .iter()
            .all(|t| name.contains(t) || key.contains(t) || path.contains(t) || content.contains(t))
        {
            continue;
        }
        let mut relevance: i32 = 0;
        if name == q {
            relevance += 100;
        } else if name.contains(&q) {
            relevance += 60;
        } else if content.contains(&q) {
            relevance += 25;
        }
        for t in &tokens {
            if name.contains(t) {
                relevance += 12;
            } else if key.contains(t) {
                relevance += 6;
            } else if content.contains(t) {
                relevance += 8;
            } else if path.contains(t) {
                relevance += 3;
            }
        }
        // Curated intent (ADRs/blueprints — `authored`) is the best answer to a
        // "what/why" question; a README/overview is the natural landing page; and
        // test scaffolding should not outrank the real thing when it shares a name.
        if node.provenance == Provenance::Authored {
            relevance += 40;
        }
        if is_overview_path(&path) {
            relevance += 30;
        }
        if is_test_path(&path) {
            relevance -= 60;
        }
        hits.push(SearchHit {
            score: u32::try_from(relevance.max(0)).unwrap_or(0),
            snippet: content_snippet(&node.meta),
            node: NodeSummary::from_node(&node),
        });
    }
    // Highest score first; ties by key for a stable, deterministic order.
    hits.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.node.key.cmp(&b.node.key))
    });
    hits.truncate(limit);
    Ok(hits)
}

/// A hit in the **generated** channel: text a model produced about a media blob,
/// never a graph fact.
///
/// It is deliberately *not* a [`SearchHit`]. A generated hit has no node, no
/// provenance and no key, and giving it a [`NodeSummary`] would be the first step
/// towards it being treated like one — the exact mistake ADR-0015 exists to
/// correct. Everything a consumer needs to label it is on the struct, including
/// the literal `generated: true`, so a caller that reads nothing else still
/// cannot mistake it for extracted text.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GeneratedHit {
    /// Relevance within the generated channel. Not comparable with a
    /// [`SearchHit::score`]: the two are ranked by different scorers, in
    /// different channels, on purpose.
    pub score: u32,
    /// Always `true`. A marker a consumer cannot miss or forget to check.
    pub generated: bool,
    /// The producer identity that wrote the text — which model, at which
    /// quantisation, under which prompt (see [`crate::Producer::id`]).
    pub producer: String,
    /// The model's registry name, repeated for legibility.
    pub model: String,
    /// The modality (`audio` | `vision`).
    pub kind: &'static str,
    /// Git blob id of the source media.
    pub blob: String,
    /// Repository path the blob was seen at.
    pub path: String,
    /// A bounded, whitespace-collapsed excerpt of the generated text, on the same
    /// terms as [`SearchHit::snippet`].
    pub snippet: Option<String>,
}

/// The two channels a search returns.
///
/// They are separate fields rather than one merged list because merging is
/// precisely what must not happen: generated text may be *retrievable*, but it
/// may never be *indistinguishable* from a derived or authored fact, and a single
/// ranked list would make the distinction a matter of reading each element
/// carefully.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchResults {
    /// Stable schema tag ([`SCHEMA`]).
    pub schema: &'static str,
    /// The graph channel: ranked nodes, exactly what [`search`] returns.
    pub hits: Vec<SearchHit>,
    /// The generated channel. **Empty unless
    /// [`SearchOptions::include_generated`] was set** — off by default, so a
    /// silent clip's confabulated prose cannot reach a default search.
    pub generated: Vec<GeneratedHit>,
}

/// How to search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchOptions {
    /// Maximum hits **per channel**. Each channel is ranked and truncated
    /// independently, so opting in to generated content never displaces a graph
    /// hit, and never silently returns fewer of them.
    pub limit: usize,
    /// Fold in the generated channel. Off by default (see
    /// [`SearchOptions::default`]).
    pub include_generated: bool,
}

impl Default for SearchOptions {
    /// Ten hits, graph channel only. The default is the safe answer: generated
    /// content is opt-in, always.
    fn default() -> Self {
        Self {
            limit: 10,
            include_generated: false,
        }
    }
}

/// Search both channels: the graph, and — only when
/// [`SearchOptions::include_generated`] is set — model-generated media content.
///
/// The graph channel is exactly [`search`]. The generated channel is ranked by
/// [a scorer of its own](generated_score) which has **no provenance term at
/// all**, so generated text cannot acquire the `authored` boost that curated
/// intent gets. It could not do so even by accident: a generated record is not a
/// node, so it never reaches the code that applies that boost.
///
/// # Errors
/// Returns [`StoreError`] on query failure.
pub fn search_channels(
    store: &Store,
    query: &str,
    opts: SearchOptions,
) -> Result<SearchResults, StoreError> {
    let hits = search(store, query, opts.limit)?;
    let generated = if opts.include_generated {
        search_generated(store, query, opts.limit)?
    } else {
        Vec::new()
    };
    Ok(SearchResults {
        schema: SCHEMA,
        hits,
        generated,
    })
}

/// Rank the generated channel alone. Ties break by `(producer, blob)` so results
/// are stable.
fn search_generated(
    store: &Store,
    query: &str,
    limit: usize,
) -> Result<Vec<GeneratedHit>, StoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let q = query.trim().to_lowercase();
    let tokens: Vec<&str> = q.split("::").flat_map(str::split_whitespace).collect();
    if tokens.is_empty() {
        return Ok(Vec::new());
    }
    let mut hits: Vec<GeneratedHit> = Vec::new();
    for record in store.media_records(&crate::MediaFilter::default())? {
        let text = record.content.text.to_lowercase();
        let path = record.path.to_lowercase();
        if !tokens.iter().all(|t| text.contains(t) || path.contains(t)) {
            continue;
        }
        hits.push(GeneratedHit {
            score: generated_score(&q, &tokens, &text, &path),
            generated: true,
            producer: record.producer_id.to_string(),
            model: record.producer.model.clone(),
            kind: record.producer.kind.as_str(),
            blob: record.blob_id.clone(),
            path: record.path.clone(),
            snippet: content_snippet(&serde_json::json!({ "content": record.content.text })),
        });
    }
    hits.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| (&a.producer, &a.blob).cmp(&(&b.producer, &b.blob)))
    });
    hits.truncate(limit);
    Ok(hits)
}

/// Relevance of one generated record: whole-query and per-token matches over its
/// text and path, and **nothing else**.
///
/// The omissions are the point, and each is deliberate:
///
/// - **no `authored` boost** — generated text is not curated intent, and the
///   graph's +40 for an ADR must never land on a transcript;
/// - **no overview boost** — a README's landing-page privilege is about authored
///   documentation;
/// - **no name or key term** — a generated record has neither.
///
/// Because this scorer shares no branch with the node scorer, "generated content
/// never acquires the authored boost" is a structural fact rather than a
/// condition to be maintained.
fn generated_score(q: &str, tokens: &[&str], text: &str, path: &str) -> u32 {
    let mut relevance: i32 = 0;
    if text.contains(q) {
        relevance += 25;
    }
    for t in tokens {
        if text.contains(t) {
            relevance += 8;
        } else if path.contains(t) {
            relevance += 3;
        }
    }
    u32::try_from(relevance.max(0)).unwrap_or(0)
}

/// Whether `path` (already lowercased) is a README/overview doc — the natural
/// landing for "what is this project" questions, so it is ranked up. Matches a
/// `readme*` or `overview*` basename (blueprints, the other overview docs, are
/// already boosted via their `authored` provenance).
fn is_overview_path(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .is_some_and(|base| base.starts_with("readme") || base.starts_with("overview"))
}

/// Whether `path` (already lowercased) is test scaffolding, which should not
/// outrank real content that happens to share a name.
fn is_test_path(path: &str) -> bool {
    path.contains("/tests/") || path.contains("/test/")
}

/// A candidate step out of a node during traversal: the edge used and the node
/// on the other end. Ordered so BFS expansion is deterministic.
struct Step {
    node: String,
    hop: PathHop,
}

/// All one-hop steps out of `key`, following edges in either direction, sorted
/// for deterministic traversal.
fn steps_from(store: &Store, key: &str) -> Result<Vec<Step>, StoreError> {
    let mut steps = Vec::new();
    for edge in store.edges_from(key)? {
        steps.push(Step {
            node: edge.dst.clone(),
            hop: hop(&edge, "outgoing", edge.dst.clone()),
        });
    }
    for edge in store.edges_to(key)? {
        steps.push(Step {
            node: edge.src.clone(),
            hop: hop(&edge, "incoming", edge.src.clone()),
        });
    }
    steps.sort_by(|a, b| {
        (&a.node, &a.hop.kind, a.hop.provenance, a.hop.direction).cmp(&(
            &b.node,
            &b.hop.kind,
            b.hop.provenance,
            b.hop.direction,
        ))
    });
    Ok(steps)
}

fn hop(edge: &Edge, direction: &'static str, node: String) -> PathHop {
    PathHop {
        kind: edge.kind.as_str().to_owned(),
        provenance: edge.provenance.as_str(),
        confidence: edge.confidence,
        direction,
        node,
    }
}

/// Find a shortest path from `from` to `to`, following edges in either
/// direction. Returns a [`Path`] with `found = false` (and no hops) if either
/// endpoint is absent or `to` is unreachable; `from == to` yields the trivial
/// zero-length path.
///
/// The search is breadth-first with deterministic neighbour ordering, so the
/// returned path is stable for a given graph.
///
/// # Errors
/// Returns [`StoreError`] on query failure.
pub fn path(store: &Store, from: &str, to: &str) -> Result<Path, StoreError> {
    let not_found = |found: bool, hops: Vec<PathHop>| Path {
        schema: SCHEMA,
        from: from.to_owned(),
        to: to.to_owned(),
        found,
        length: hops.len(),
        hops,
    };

    // Both endpoints must exist in the graph.
    if store.get_node(from)?.is_none() || store.get_node(to)?.is_none() {
        return Ok(not_found(false, Vec::new()));
    }
    if from == to {
        return Ok(not_found(true, Vec::new()));
    }

    // BFS, recording for each visited node the (predecessor, hop) that reached
    // it so the path can be reconstructed.
    let mut came_from: BTreeMap<String, (String, PathHop)> = BTreeMap::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    queue.push_back(from.to_owned());
    came_from.insert(from.to_owned(), (String::new(), placeholder_hop()));

    while let Some(current) = queue.pop_front() {
        if current == to {
            break;
        }
        for step in steps_from(store, &current)? {
            if came_from.contains_key(&step.node) {
                continue;
            }
            came_from.insert(step.node.clone(), (current.clone(), step.hop));
            queue.push_back(step.node);
        }
    }

    // Walk predecessors back from `to` to `from`, then reverse. Every node in
    // `came_from` other than `from` has a real predecessor, so this terminates
    // at `from`. If the chain is ever broken (an invariant violation), treat it
    // as no path rather than silently returning a partial one.
    let mut hops = Vec::new();
    let mut cursor = to.to_owned();
    while cursor != from {
        let Some((prev, hop)) = came_from.get(&cursor) else {
            return Ok(not_found(false, Vec::new()));
        };
        hops.push(hop.clone());
        cursor = prev.clone();
    }
    hops.reverse();
    Ok(not_found(true, hops))
}

/// A sentinel hop for the BFS start node (never emitted in a result).
fn placeholder_hop() -> PathHop {
    PathHop {
        kind: String::new(),
        provenance: "derived",
        confidence: None,
        direction: "outgoing",
        node: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::{SCHEMA, SNIPPET_MAX, explain, glob_match, list_kind, path, search};
    use crate::{Edge, EdgeKind, FactSet, Node, NodeKind, Store};

    fn seeded() -> Store {
        let mut store = Store::open_in_memory().expect("store");
        let facts = FactSet::new()
            .with_node(Node::new("sym:rust:a.rs#main", NodeKind::Fn, "main"))
            .with_node(Node::new("sym:rust:a.rs#helper", NodeKind::Fn, "helper"))
            .with_node(Node::new("adr:0001", NodeKind::Adr, "Build Roteiro"))
            .with_edge(Edge::derived(
                "sym:rust:a.rs#main",
                "sym:rust:a.rs#helper",
                EdgeKind::Calls,
            ))
            .with_edge(Edge::authored(
                "adr:0001",
                "sym:rust:a.rs#main",
                EdgeKind::References,
            ));
        store.apply_factset(&facts).expect("apply");
        store
    }

    #[test]
    fn search_ranks_by_relevance_and_is_bounded() {
        let store = seeded();
        // An exact name match outranks a substring match.
        let hits = search(&store, "helper", 10).expect("search");
        assert_eq!(hits[0].node.key, "sym:rust:a.rs#helper");
        assert!(hits[0].score >= 100, "exact name match scores high");

        // Every token must appear: "main roteiro" matches nothing (no node has both).
        assert!(
            search(&store, "main roteiro", 10)
                .expect("search")
                .is_empty()
        );

        // A lone `:` does not split a token: `sym:rust` is one token matching the
        // code-symbol keys but not `adr:0001`.
        let by_prefix = search(&store, "sym:rust", 10).expect("search");
        assert!(!by_prefix.is_empty());
        assert!(
            by_prefix
                .iter()
                .all(|h| h.node.key.starts_with("sym:rust:"))
        );

        // A blank query yields nothing; the limit is respected.
        assert!(search(&store, "   ", 10).expect("search").is_empty());
        assert!(search(&store, "a.rs", 1).expect("search").len() <= 1);
    }

    #[test]
    fn search_prefers_curated_content_over_same_named_test_symbols() {
        use crate::Provenance;
        let mut store = Store::open_in_memory().expect("store");
        // A same-named test helper (exact name, but test scaffolding)…
        let mut test_fn = Node::new(
            "sym:rust:crates/x/tests/cli.rs#roteiro",
            NodeKind::Fn,
            "roteiro",
        );
        test_fn.path = Some("crates/x/tests/cli.rs".into());
        // …the authored ADR that actually answers "what is roteiro"…
        let mut adr = Node::new("adr:0001", NodeKind::Adr, "Build Roteiro")
            .with_provenance(Provenance::Authored);
        adr.path = Some("docs/adr/0001.md".into());
        adr.meta = serde_json::json!({ "content": "Roteiro is a provenance-tagged codebase knowledge graph." });
        // …and a README whose *content* (not its name) describes the project.
        let mut readme = Node::new("file:README.md", NodeKind::File, "README.md");
        readme.path = Some("README.md".into());
        readme.meta =
            serde_json::json!({ "content": "Roteiro turns a repo into one knowledge graph." });
        store
            .apply_factset(
                &FactSet::new()
                    .with_node(test_fn)
                    .with_node(adr)
                    .with_node(readme),
            )
            .expect("apply");

        let hits = search(&store, "roteiro", 10).expect("search");
        let keys: Vec<&str> = hits.iter().map(|h| h.node.key.as_str()).collect();
        let idx = |k: &str| keys.iter().position(|x| *x == k).expect("present");
        // The authored ADR and the README (found *by content*) both outrank the
        // same-named test helper.
        assert!(
            idx("adr:0001") < idx("sym:rust:crates/x/tests/cli.rs#roteiro"),
            "authored ADR outranks the test symbol: {keys:?}"
        );
        assert!(
            idx("file:README.md") < idx("sym:rust:crates/x/tests/cli.rs#roteiro"),
            "README (matched via content) outranks the test symbol: {keys:?}"
        );

        // A content-only term finds the node even though no name/key/path has it.
        let by_content = search(&store, "provenance-tagged", 10).expect("search");
        assert_eq!(
            by_content.first().map(|h| h.node.key.as_str()),
            Some("adr:0001"),
            "content search matches the ADR by its captured text"
        );
    }

    #[test]
    fn search_hit_carries_a_bounded_content_snippet() {
        use crate::Provenance;
        let mut store = Store::open_in_memory().expect("store");
        // A content-bearing node whose content is longer than the cap and has
        // messy whitespace to collapse.
        let long = "word ".repeat(200);
        let mut adr =
            Node::new("adr:0001", NodeKind::Adr, "Overview").with_provenance(Provenance::Authored);
        adr.meta = serde_json::json!({ "content": format!("Roteiro   is\n\na graph. {long}") });
        // A pure symbol node with no captured content.
        let sym = Node::new("sym:rust:a.rs#main", NodeKind::Fn, "main");
        store
            .apply_factset(&FactSet::new().with_node(adr).with_node(sym))
            .expect("apply");

        let hits = search(&store, "roteiro", 10).expect("search");
        let adr_hit = hits
            .iter()
            .find(|h| h.node.key == "adr:0001")
            .expect("adr hit");
        let snippet = adr_hit
            .snippet
            .as_deref()
            .expect("a content-bearing node yields a snippet");
        // Whitespace is collapsed to single spaces (no runs, no newlines)…
        assert!(snippet.starts_with("Roteiro is a graph."), "got: {snippet}");
        assert!(!snippet.contains("  "));
        assert!(!snippet.contains('\n'));
        // …and the snippet is bounded to SNIPPET_MAX chars *including* the ellipsis.
        assert!(
            snippet.chars().count() <= SNIPPET_MAX,
            "snippet is bounded: {} chars",
            snippet.chars().count()
        );
        assert!(
            snippet.ends_with('…'),
            "over-long content is truncated with an ellipsis"
        );

        // A node without content falls back cleanly: no snippet, so the summary
        // (name/kind/path) is the grounding.
        let hits = search(&store, "main", 10).expect("search");
        let sym_hit = hits
            .iter()
            .find(|h| h.node.key == "sym:rust:a.rs#main")
            .expect("sym hit");
        assert!(
            sym_hit.snippet.is_none(),
            "a node with no content has no snippet"
        );
    }

    #[test]
    fn explain_reports_labelled_neighbourhood() {
        let store = seeded();
        let ex = explain(&store, "sym:rust:a.rs#main")
            .expect("query")
            .expect("present");
        assert_eq!(ex.schema, SCHEMA);
        assert_eq!(ex.node.kind, "fn");

        // Outgoing: derived call to helper.
        assert_eq!(ex.outgoing.len(), 1);
        assert_eq!(ex.outgoing[0].kind, "calls");
        assert_eq!(ex.outgoing[0].provenance, "derived");
        assert_eq!(ex.outgoing[0].node, "sym:rust:a.rs#helper");

        // Incoming: authored reference from the ADR.
        assert_eq!(ex.incoming.len(), 1);
        assert_eq!(ex.incoming[0].provenance, "authored");
        assert_eq!(ex.incoming[0].node, "adr:0001");
    }

    #[test]
    fn explain_missing_node_is_none() {
        let store = seeded();
        assert!(explain(&store, "sym:rust:a.rs#ghost").expect("q").is_none());
    }

    #[test]
    fn edges_differing_only_in_provenance_are_ordered() {
        // Two edges A->B with the same kind but different provenance must sort
        // into a stable, deterministic order (authored before derived).
        let mut store = Store::open_in_memory().expect("store");
        let facts = FactSet::new()
            .with_node(Node::new("a", NodeKind::Fn, "a"))
            .with_node(Node::new("b", NodeKind::Fn, "b"))
            .with_edge(Edge::derived("a", "b", EdgeKind::References))
            .with_edge(Edge::authored("a", "b", EdgeKind::References));
        store.apply_factset(&facts).expect("apply");

        let ex = explain(&store, "a").expect("q").expect("present");
        let provs: Vec<_> = ex.outgoing.iter().map(|e| e.provenance).collect();
        assert_eq!(provs, ["authored", "derived"]);
    }

    #[test]
    fn list_kind_is_ordered() {
        let store = seeded();
        let listing = list_kind(&store, &NodeKind::Fn).expect("list");
        let keys: Vec<_> = listing.nodes.iter().map(|n| n.key.as_str()).collect();
        assert_eq!(keys, ["sym:rust:a.rs#helper", "sym:rust:a.rs#main"]);
    }

    #[test]
    fn json_schema_is_stable() {
        let store = seeded();
        let ex = explain(&store, "adr:0001").expect("q").expect("present");
        let json = serde_json::to_value(&ex).expect("json");
        assert_eq!(json["schema"], SCHEMA);
        assert_eq!(json["node"]["key"], "adr:0001");
        assert_eq!(json["node"]["kind"], "adr");
        // Outgoing authored reference is present with its provenance label.
        assert_eq!(json["outgoing"][0]["kind"], "references");
        assert_eq!(json["outgoing"][0]["provenance"], "authored");
        assert_eq!(json["outgoing"][0]["node"], "sym:rust:a.rs#main");
        assert!(json["outgoing"][0]["confidence"].is_null());
    }

    #[test]
    fn path_crosses_provenance_and_direction() {
        // adr:0001 --authored/references--> main --derived/calls--> helper.
        // A path from the ADR to helper must traverse both, each hop labelled.
        let store = seeded();
        let p = path(&store, "adr:0001", "sym:rust:a.rs#helper").expect("path");
        assert!(p.found);
        assert_eq!(p.length, 2);
        assert_eq!(p.schema, SCHEMA);

        assert_eq!(p.hops[0].kind, "references");
        assert_eq!(p.hops[0].provenance, "authored");
        assert_eq!(p.hops[0].direction, "outgoing");
        assert_eq!(p.hops[0].node, "sym:rust:a.rs#main");

        assert_eq!(p.hops[1].kind, "calls");
        assert_eq!(p.hops[1].provenance, "derived");
        assert_eq!(p.hops[1].node, "sym:rust:a.rs#helper");
    }

    #[test]
    fn path_follows_edges_against_direction() {
        // From helper back to the ADR: both edges are traversed against their
        // stored direction, so each hop is `incoming`.
        let store = seeded();
        let p = path(&store, "sym:rust:a.rs#helper", "adr:0001").expect("path");
        assert!(p.found);
        assert_eq!(p.length, 2);
        assert!(p.hops.iter().all(|h| h.direction == "incoming"));
        assert_eq!(p.hops.last().unwrap().node, "adr:0001");
    }

    #[test]
    fn path_same_node_is_trivial() {
        let store = seeded();
        let p = path(&store, "adr:0001", "adr:0001").expect("path");
        assert!(p.found);
        assert_eq!(p.length, 0);
        assert!(p.hops.is_empty());
    }

    #[test]
    fn path_missing_endpoint_or_unreachable_is_not_found() {
        let mut store = Store::open_in_memory().expect("store");
        // Two disconnected components: a-b and an isolated island.
        let facts = FactSet::new()
            .with_node(Node::new("a", NodeKind::Fn, "a"))
            .with_node(Node::new("b", NodeKind::Fn, "b"))
            .with_node(Node::new("island", NodeKind::Fn, "island"))
            .with_edge(Edge::derived("a", "b", EdgeKind::Calls));
        store.apply_factset(&facts).expect("apply");

        // Absent endpoint.
        let missing = path(&store, "a", "ghost").expect("path");
        assert!(!missing.found);
        assert!(missing.hops.is_empty());

        // Present but unreachable.
        let unreachable = path(&store, "a", "island").expect("path");
        assert!(!unreachable.found);
        assert!(unreachable.hops.is_empty());
    }

    #[test]
    fn path_is_shortest() {
        // a-b-c-d chain plus a direct a-d edge: the path must take the shortcut.
        let mut store = Store::open_in_memory().expect("store");
        let facts = FactSet::new()
            .with_node(Node::new("a", NodeKind::Fn, "a"))
            .with_node(Node::new("b", NodeKind::Fn, "b"))
            .with_node(Node::new("c", NodeKind::Fn, "c"))
            .with_node(Node::new("d", NodeKind::Fn, "d"))
            .with_edge(Edge::derived("a", "b", EdgeKind::Calls))
            .with_edge(Edge::derived("b", "c", EdgeKind::Calls))
            .with_edge(Edge::derived("c", "d", EdgeKind::Calls))
            .with_edge(Edge::derived("a", "d", EdgeKind::Calls));
        store.apply_factset(&facts).expect("apply");

        let p = path(&store, "a", "d").expect("path");
        assert!(p.found);
        assert_eq!(p.length, 1, "the direct a->d edge is the shortest path");
        assert_eq!(p.hops[0].node, "d");
    }

    #[test]
    fn glob_matches_segments_and_wildcards() {
        // `**` spans segments (including zero) and anchors both ends.
        assert!(glob_match("vendor/**", "vendor/lib/a.rs"));
        assert!(glob_match("vendor/**", "vendor")); // zero trailing segments
        assert!(glob_match("**/generated/*", "src/gen/generated/x.rs"));
        assert!(glob_match("**/*.rs", "a/b/c.rs"));
        // `*` and `?` stay within one segment.
        assert!(glob_match("src/*.rs", "src/main.rs"));
        assert!(!glob_match("src/*.rs", "src/sub/main.rs"));
        assert!(glob_match("a?c.rs", "abc.rs"));
        assert!(!glob_match("a?c.rs", "ac.rs"));
        // Anchored: a bare name does not match a nested path.
        assert!(!glob_match("generated", "src/generated"));
        assert!(!glob_match("vendor/**", "third_party/vendor/a.rs"));
    }
}

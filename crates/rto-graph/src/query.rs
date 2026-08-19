//! The agent- and human-facing query surface over the graph.
//!
//! Everything here is a read-only view built from the store's typed queries,
//! serialised under a **stable, versioned** JSON schema ([`SCHEMA`]) so agents
//! can depend on the shape. The primitives are [`explain`] (a node and its
//! provenance-labelled neighbourhood), [`list_kind`] (all nodes of a kind),
//! [`path`] (a shortest path between two nodes), [`debt`] (the intent-debt marker
//! inventory), [`debt_density`] (that inventory per file, normalised by file
//! length), [`coupling`] (directed fan-in/fan-out over `Calls` edges),
//! [`config_secrets`] (secret-named config keys and their redaction state), and
//! [`search`] (relevance-ranked node search). All return
//! mixed-provenance results — the "one query surface" from ADR-0001 — with every
//! edge carrying its `provenance`.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::Serialize;

use crate::store::{Store, StoreError};
use crate::{Edge, EdgeKind, NodeKind, Provenance};

/// The versioned schema tag emitted on every query result. Bump the version on
/// any breaking change to the shape.
pub const SCHEMA: &str = "roteiro.query/v1";

/// Cut an already-ordered, already-materialised list down to the window a caller
/// asked for: skip `offset` items from the front, then keep at most `limit`.
///
/// **This is the one place that decides what `limit` and `offset` mean** for the
/// graph's list lenses — [`debt_density`], [`config_secrets`] and [`coupling`]
/// here, and the `/nodes` and `/hotspots` endpoints in the `roteiro` binary. It
/// exists because the parameter previously had two implementations: three lenses
/// truncated here and treated `0` as "no limit", while two HTTP handlers used
/// [`Iterator::take`] and so returned nothing for `0` — the same parameter name
/// with opposite meanings, and nothing that could make the disagreement visible
/// (issue #375). A sixth list lens should call this rather than write a third.
/// One did write a third — see *Episodic recall* below — and the warning is left
/// standing because the next one will too.
///
/// The contract:
///
/// - **`limit == 0` means unlimited** — every item that survives `offset` is
///   kept. This is the reading the CLI already documents (`roteiro
///   config-secrets --help`: *"0 shows every secret-named key"*), so no
///   published promise is withdrawn by making it universal; and it is the safer
///   of the two, because a caller who passes an unset variable then gets more
///   data than they meant to ask for rather than an empty page that reads like a
///   truthful "nothing found".
/// - **`offset` applies first, and `limit` to what remains.** So `offset = 20,
///   limit = 0` is "skip the first 20, then every remaining item", not "skip 20,
///   then nothing". An `offset` past the end yields an empty window rather than
///   panicking — a page beyond the last one is empty, not an error.
/// - A caller's reported `total` must be taken **before** this runs: every
///   surface reports the pre-windowing population, so a cut page still says what
///   it was cut from.
///
/// Ordering is the caller's job; this only removes, and only from the ends, so a
/// deterministically-ordered input yields a deterministic window.
///
/// # The search channels call this too, and the unit there is *per channel*
///
/// [`search`] and the generated/memory channels behind [`search_channels`] used
/// to keep a `limit == 0 => no hits` guard of their own — a third reading of one
/// parameter name, in the same crate as the two #375 reconciled (issue #393).
/// They now window here like every other lens, so `limit` has one definition and
/// not a second implementation of it, which is exactly how the first two drifted.
///
/// What differs is the **unit**, not the rule: a search `limit` bounds *each
/// channel* independently, so `0` is "every match, in every channel that was
/// asked for", not "every match overall". That is a bounded request rather than
/// "dump the graph": a channel's ranking only *orders* a set the query has
/// already filtered — every token must appear in a hit — and a query with no
/// tokens returns nothing at any limit, `0` included. Measured on this
/// repository at 6,685 nodes: an unbounded one-token search returned ~2.7k hits
/// in 0.24 s and a two-token one returned 3, against a full-population scan that
/// every limit pays anyway, so unlimited costs no more than the default does.
///
/// The **MCP tools are the one surface that cannot ask for it**, deliberately:
/// they clamp `limit` into `1..=25` and advertise `"minimum": 1`, because their
/// results are spent against a model's context window and `0` would be the one
/// value that escaped the ceiling those clamps exist to impose. That is a
/// surface declining to offer a value, not a second meaning for it — a model
/// that sends `0` anyway gets the smallest page, never the silent empty answer
/// this issue is about. The reasoning is restated where each clamp lives, in
/// `rto_render::mcp::GraphServer::search` and the served-chat `search` arm in
/// the `roteiro` binary; if this rule changes, those two must change with it.
///
/// # Episodic recall — the third implementation this doc predicted (issue #447)
///
/// [`crate::Store::recall_memory`] ranked, then called
/// [`Vec::truncate`] directly, so `limit = 0` emptied the result: `recall
/// --limit 0` returned nothing on a store where five other surfaces returned
/// everything, and the JSON said `"live": 8000` beside `"results": []` — not a
/// lie, and no help at all. It calls this now. Two details are worth keeping:
///
/// - **`None` and `Some(0)` had to collapse onto one meaning, not two.**
///   `RecallOptions::limit` is an `Option`, so "unlimited" was already sayable
///   twice; the fix maps `None` to `0` rather than adding a branch, because two
///   spellings of one request are how the first divergence started.
/// - **`memory list` cuts in SQL and so cannot call this.** `LIMIT 0` in SQL
///   means the *opposite* of what this function means, so `memory::records`
///   omits the clause entirely for `0`. That is the contract translated, and it
///   is the only place in the crate where the rule is re-expressed rather than
///   called — worth knowing if it ever has to change.
pub fn window<T>(items: &mut Vec<T>, offset: usize, limit: usize) {
    // `min(len)` rather than a bounds check: `drain(..offset)` panics past the
    // end of the vector, and "page 900 of 3" is an empty page, not a 500.
    items.drain(..offset.min(items.len()));
    if limit > 0 {
        items.truncate(limit);
    }
}

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

/// How a [`DebtDensityReport`]'s files are ranked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DensityOrder {
    /// By `per_kloc` — markers relative to file length. The lens's own question.
    #[default]
    Density,
    /// By `markers` — the raw count, which [`debt`] already reports per marker.
    /// Offered so the two rankings can be compared on one report rather than the
    /// reader being asked to trust that they differ.
    Markers,
    /// By `lines` — longest file first. Not a debt ranking; the control that
    /// shows *which* files the denominator is large for.
    Lines,
}

impl DensityOrder {
    /// The stable token for this order, as accepted by [`from_token`](Self::from_token).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Density => "density",
            Self::Markers => "markers",
            Self::Lines => "lines",
        }
    }

    /// Parse an order token. `None` for anything else — callers surface an error
    /// rather than silently ranking by something the caller did not ask for.
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        match s {
            "density" => Some(Self::Density),
            "markers" => Some(Self::Markers),
            "lines" => Some(Self::Lines),
            _ => None,
        }
    }

    /// The tokens [`from_token`](Self::from_token) accepts, for error messages
    /// and argument schemas — so the accepted set is stated in exactly one place.
    #[must_use]
    pub fn tokens() -> [&'static str; 3] {
        [
            Self::Density.as_str(),
            Self::Markers.as_str(),
            Self::Lines.as_str(),
        ]
    }
}

/// The default `min_lines` floor for [`debt_density`]: files shorter than this
/// are counted but not ranked (see [`DebtDensityReport::min_lines`] for why the
/// floor exists at all).
///
/// 50 because that is where one marker stops dominating: a single marker in a
/// 50-line file scores 20 per kloc, which is already near the top of this
/// repository's real ranking, so any shorter file with a marker is guaranteed a
/// high placement by its length alone. It is a default, not a rule — `0` ranks
/// every file.
pub const DEFAULT_MIN_LINES: u32 = 50;

/// One file's intent-debt density in a [`DebtDensityReport`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DensityItem {
    /// Repository-relative path of the file.
    pub path: String,
    /// Retained markers in this file (after category and `ignore` filtering).
    pub markers: u32,
    /// The file's length in lines — the denominator. See [`debt_density`] for
    /// exactly what this counts and what it does not.
    pub lines: u32,
    /// Markers per 1,000 lines, rounded to two decimals. Per *kilo*-line rather
    /// than per line because per-line densities are all leading zeroes: this
    /// repository's worst file is 0.06 markers per line, and `60.0` per kloc is
    /// a number a reader can hold.
    pub per_kloc: f64,
    /// Count per category within this file, ordered by category token — so a
    /// dense file can be read as "twelve `todo`" or "twelve `stub`", which are
    /// not the same finding.
    pub by_category: BTreeMap<String, usize>,
}

/// Intent-debt **density**: markers per file normalised by file length, ranked.
/// The counterpart to [`debt`], which reports markers and therefore ranks large
/// files first by construction.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DebtDensityReport {
    /// Stable schema tag ([`SCHEMA`]).
    pub schema: &'static str,
    /// The ranking that produced `items` ([`DensityOrder::as_str`]).
    pub order: &'static str,
    /// The requested cap on `items`; `0` means unlimited.
    pub limit: usize,
    /// The `min_lines` floor applied. Files shorter than this are excluded from
    /// the ranking and counted in `short_files`; `0` disables the floor.
    ///
    /// The floor exists because density is unstable in the denominator's tail: a
    /// 3-line stub file with one marker scores 333 per kloc, which is true and
    /// tells the reader nothing. Excluding those files is a ranking decision,
    /// not a suppression — they stay in `files_with_markers` and `total_markers`.
    pub min_lines: u32,
    /// Distinct files carrying at least one retained marker, before the
    /// `min_lines` floor. The population `items` is drawn from.
    pub files_with_markers: usize,
    /// Files that passed the `min_lines` floor and were therefore ranked.
    /// `ranked_files > items.len()` means `limit` truncated the list.
    pub ranked_files: usize,
    /// Files excluded from the ranking by the `min_lines` floor — reported, not
    /// silently dropped, so a short-file-heavy repository cannot read as a clean one.
    pub short_files: usize,
    /// Files whose marker count is known but whose length is **not**: no `file`
    /// node, or one carrying no `meta.lines`. Excluded from the ranking, because
    /// a density with no denominator is not a number — and reported, because
    /// silently omitting them would understate the inventory.
    pub unknown_length_files: usize,
    /// Retained markers across every file in `files_with_markers`, including
    /// those the floor excluded. Matches [`DebtReport::total`] for the same
    /// filters, minus any marker with no `path`.
    pub total_markers: usize,
    /// Summed `lines` of the ranked files. The denominator behind
    /// `overall_per_kloc`.
    pub total_lines: u64,
    /// Markers per 1,000 lines across the **ranked** files taken together — the
    /// baseline a single file's `per_kloc` should be read against. `0.0` when
    /// nothing was ranked.
    pub overall_per_kloc: f64,
    /// The ranked files: by `order` descending, ties broken by `path` ascending.
    pub items: Vec<DensityItem>,
}

/// Rank files by intent-debt **density** — retained markers per 1,000 lines —
/// most-dense first by `order`, capped at `limit` (`0` = unlimited). `categories`
/// and `ignore` filter markers exactly as [`debt`] does, so the two lenses always
/// agree about which markers exist.
///
/// # Why this is not [`debt`] with a division
///
/// A raw marker count ranks by file size: the biggest file wins because it has
/// the most lines to put a marker on. Density asks the different question — *how
/// concentrated is the debt* — and a 40-marker file of 4,000 lines and a
/// 40-marker file of 200 lines separate by a factor of twenty under it while
/// being indistinguishable under [`debt`].
///
/// # The denominator, and why it is this one
///
/// **`lines` is the `file` node's `meta.lines`**, recorded at extraction time as
/// the count of `\n` bytes in the blob. It is read straight from the graph, so
/// this lens adds **no extraction metadata and needs no `EXTRACT_VERSION` bump**.
///
/// It is deliberately *not* derived from [`crate::Span`]: a node's span is a pair
/// of **byte offsets**, not line numbers, and there is no line index in the store
/// to convert one to the other. Anything span-derived would be a byte density,
/// which is not the quantity anyone means by "debt density".
///
/// Three alternatives were rejected, each for the same reason:
///
/// - **Source lines of code** (blank and comment lines removed) is the denominator
///   a reader probably imagines. It does not exist in the graph and cannot be
///   computed from it: producing it means counting lines per language at
///   extraction, which is net-new derived metadata and would move this lens into
///   the batch that pays for an `EXTRACT_VERSION` bump.
/// - **Per symbol** rather than per file would be the finer-grained view — markers
///   already attach to their innermost enclosing symbol. But a symbol's length in
///   *lines* is exactly what `Span`'s byte offsets cannot give.
/// - **The highest marker line in the file** is available (`meta.line`), and is a
///   lower bound on the file's length rather than the length: a file whose only
///   marker is on line 3 would score 333 per kloc however long it is.
///
/// So `lines` is what the graph honestly has. What it counts, stated plainly
/// because the name invites over-reading:
///
/// - **Every line, including blanks, comments, imports and licence headers.** It
///   is *file length*, not "lines of code". Density figures here are therefore
///   systematically lower than an SLOC-based tool's, and by a different factor
///   per language and per file.
/// - **Newline bytes.** A file not ending in a newline is counted one line short,
///   and a file of a single unterminated line counts as `0` lines and is reported
///   under `unknown_length_files` rather than divided by zero.
/// - **The whole blob, vendored code included.** A minified bundle is one enormous
///   line and will look flawless. Use `ignore` (the shared `[debt] ignore` globs)
///   rather than a second exclusion vocabulary.
///
/// # Confidence, and why there is no CI gate
///
/// Density inherits every false positive of the marker scan beneath it — the
/// prose rules (`for now`, `deferred`, `tbd`) fire on ordinary writing, so a
/// design document rich in the word "deferred" ranks as dense debt. It then adds
/// one of its own: the denominator is file length, so a language or a file with
/// low information per line (verbose config, generated code, wide indentation) is
/// systematically flattered, and a dense language is systematically penalised.
/// Neither is a defect being reported; both move the number. A gate would fail
/// builds on prose and on formatting. So this lens **offers no CI gate**, and its
/// suppression story is the one that already exists: `[debt] ignore` globs and
/// the `roteiro:ignore` / `roteiro:ignore-file` source directives, both applied
/// before anything is counted here.
///
/// Ordering is total and deterministic: by the chosen metric descending, then by
/// `path` ascending, so identical input yields byte-identical output.
///
/// # Errors
/// Returns [`StoreError`] on query failure.
pub fn debt_density(
    store: &Store,
    categories: &[String],
    ignore: &[String],
    order: DensityOrder,
    limit: usize,
    min_lines: u32,
) -> Result<DebtDensityReport, StoreError> {
    // Reuse `debt` rather than re-walking the markers: the two lenses must never
    // disagree about which markers exist, and the only way to guarantee that is
    // for one to be built from the other's output.
    let inventory = debt(store, categories, ignore)?;
    let mut per_file: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let mut total_markers = 0usize;
    for item in &inventory.items {
        // A marker with no `path` cannot be attributed to a file, so it cannot
        // have a density. Extraction always records one; this is defence in
        // depth, and such a marker is left out of `total_markers` too so the
        // report's own arithmetic stays consistent.
        let Some(path) = item.path.as_deref() else {
            continue;
        };
        *per_file
            .entry(path.to_owned())
            .or_default()
            .entry(item.category.clone())
            .or_default() += 1;
        total_markers += 1;
    }
    let files_with_markers = per_file.len();

    // Only files that actually carry a marker are read back, so the denominator
    // costs one node lookup per such file rather than a whole-graph file scan —
    // which matters because `file` nodes carry captured `meta.content`.
    let mut ranked: Vec<(String, u32, u32, BTreeMap<String, usize>)> = Vec::new();
    let mut short_files = 0usize;
    let mut unknown_length_files = 0usize;
    for (path, by_category) in per_file {
        let markers = u32::try_from(by_category.values().sum::<usize>()).unwrap_or(u32::MAX);
        let Some(lines) = file_lines(store, &path)? else {
            unknown_length_files += 1;
            continue;
        };
        if lines < min_lines {
            short_files += 1;
            continue;
        }
        ranked.push((path, markers, lines, by_category));
    }
    let ranked_files = ranked.len();
    let total_lines: u64 = ranked
        .iter()
        .map(|(_, _, lines, _)| u64::from(*lines))
        .sum();
    let ranked_markers: u64 = ranked.iter().map(|(_, m, _, _)| u64::from(*m)).sum();

    ranked.sort_by(|a, b| {
        // Each order yields the metric as an exact `numerator / denominator`, so
        // the comparison can cross-multiply. Ranking on the rounded `per_kloc`
        // instead would make genuinely different densities tie and then break on
        // `path`, silently reordering the ranking the caller asked for; ranking
        // on `f64` at full precision would make the order depend on the last bit
        // of a division. `u128` so the cross-product cannot overflow whatever the
        // file lengths are, rather than relying on repositories staying small.
        let metric =
            |&(_, markers, lines, _): &(String, u32, u32, BTreeMap<String, usize>)| match order {
                DensityOrder::Density => (u128::from(markers) * 1000, u128::from(lines)),
                DensityOrder::Markers => (u128::from(markers), 1),
                DensityOrder::Lines => (u128::from(lines), 1),
            };
        let (an, ad) = metric(a);
        let (bn, bd) = metric(b);
        (bn * ad).cmp(&(an * bd)).then_with(|| a.0.cmp(&b.0))
    });
    window(&mut ranked, 0, limit);

    let items = ranked
        .into_iter()
        .map(|(path, markers, lines, by_category)| DensityItem {
            path,
            markers,
            lines,
            per_kloc: per_kloc(u64::from(markers), u64::from(lines)),
            by_category,
        })
        .collect();

    Ok(DebtDensityReport {
        schema: SCHEMA,
        order: order.as_str(),
        limit,
        min_lines,
        files_with_markers,
        ranked_files,
        short_files,
        unknown_length_files,
        total_markers,
        total_lines,
        overall_per_kloc: per_kloc(ranked_markers, total_lines),
        items,
    })
}

/// A file's length in lines from its `file` node's `meta.lines`, or `None` when
/// the node is absent, carries no `lines`, or reports **zero** lines.
///
/// Zero is folded into `None` deliberately: it is not a length that can be
/// divided by, and the two cases a reader would want distinguished — a genuinely
/// empty file and a single line with no terminating newline — are
/// indistinguishable in a newline count. Reporting both as "length unknown" is
/// the honest reading; reporting either as a density is not.
fn file_lines(store: &Store, path: &str) -> Result<Option<u32>, StoreError> {
    let Some(node) = store.get_node(&format!("file:{path}"))? else {
        return Ok(None);
    };
    Ok(node
        .meta
        .get("lines")
        .and_then(serde_json::Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .filter(|&n| n > 0))
}

/// Markers per 1,000 lines, rounded to two decimals; `0.0` for a zero
/// denominator, which callers have already excluded from any ranking.
fn per_kloc(markers: u64, lines: u64) -> f64 {
    if lines == 0 {
        return 0.0;
    }
    // `u64 as f64` is lossy above 2^53; a line count or marker count that large
    // is not reachable from a repository on disk, and the precision lost would be
    // below the two decimals this rounds to anyway.
    #[expect(clippy::cast_precision_loss, reason = "counts are far below 2^53")]
    let ratio = (markers as f64) * 1000.0 / (lines as f64);
    round2(ratio)
}

/// The redaction state of one config key in a [`ConfigSecretReport`]. Three
/// states, because collapsing them would misreport two of them: "declared in
/// code" is not a redaction, and "value present" is not a safe one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RedactionState {
    /// The value was read from a source file and **replaced** with the redaction
    /// placeholder before anything was persisted. The expected state for a
    /// secret-named key extracted from a config file.
    Redacted,
    /// The key carries **no value at all** — a struct-derived key
    /// (`meta.source = "struct"`), a config *field* declared in Rust with no
    /// literal in the code to redact.
    ///
    /// Not a redaction and not a leak: it records that a setting by this name
    /// exists, which is the inventory's job, and nothing about any value.
    Declared,
    /// The key carries a value that is **not** the redaction placeholder.
    ///
    /// Extraction redacts every secret-named key, so this state is unreachable
    /// from extraction alone. It is reachable through
    /// [`Store::apply_import_layer`](crate::Store::apply_import_layer), which
    /// upserts whatever nodes an imported factset carries — so an import produced
    /// by another tool, or by an older Roteiro, can put an unredacted value in the
    /// store. That is worth reporting loudly, and it is a finding about **this
    /// store**, not about the source repository.
    Present,
}

impl RedactionState {
    /// The stable token for this state, as serialised.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Redacted => "redacted",
            Self::Declared => "declared",
            Self::Present => "present",
        }
    }
}

/// One secret-named config key in a [`ConfigSecretReport`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConfigSecretItem {
    /// Natural key of the config-key node (`cfgkey:<path>#<dotted>`).
    pub key: String,
    /// Repository-relative path of the config file or Rust source it came from.
    pub path: Option<String>,
    /// The dotted key name (e.g. `serve.api_token`). **Names only** — no value is
    /// carried here, by construction as much as by choice: the value in the store
    /// is the redaction placeholder.
    pub name: String,
    /// Whether the value was redacted, absent, or present.
    pub state: RedactionState,
    /// `meta.source`, when the node records one — `struct` for a key synthesised
    /// from a `@rto:config` Rust struct. Absent for a file-derived key.
    pub source: Option<String>,
}

/// An inventory of **secret-named** config keys and their redaction state.
///
/// See [`config_secrets`] for what this is and — more importantly — what it is
/// not.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConfigSecretReport {
    /// Stable schema tag ([`SCHEMA`]).
    pub schema: &'static str,
    /// The requested cap on `items`; `0` means unlimited.
    pub limit: usize,
    /// Every `config_key` node in the graph, secret-named or not — the population
    /// the inventory was drawn from.
    pub config_keys: usize,
    /// Config keys whose **name** matched the secret-name heuristic. `items` is
    /// the first `limit` of these, so `secret_named > items.len()` means truncation.
    pub secret_named: usize,
    /// Of `secret_named`: how many carry the redaction placeholder.
    pub redacted: usize,
    /// Of `secret_named`: how many carry no value at all (struct-derived).
    pub declared: usize,
    /// Of `secret_named`: how many carry a value that is **not** the placeholder.
    ///
    /// **Expected to be zero.** A non-zero count is a finding about this store —
    /// see [`RedactionState::Present`] for the one path that reaches it.
    pub unredacted: usize,
    /// Config keys that are redacted but whose name is **not** secret-looking: a
    /// Kubernetes `Secret`'s `data`, redacted because of where it lives rather
    /// than what it is called.
    ///
    /// Reported so the redaction counts reconcile against the graph: without it a
    /// reader comparing `redacted` to the number of `<redacted>` values in the
    /// store would find an unexplained surplus.
    pub redacted_not_secret_named: usize,
    /// Distinct files carrying at least one secret-named key.
    pub files: usize,
    /// The secret-named keys, ordered by `(path, name, key)`.
    pub items: Vec<ConfigSecretItem>,
}

/// Inventory the **secret-named** config keys in the graph — where they are, what
/// they are called, and whether their values were redacted before persistence —
/// capped at `limit` (`0` = unlimited).
///
/// # What this reports
///
/// Config extraction (ADR-0009) flattens TOML/JSON/YAML/`.env` into `config_key`
/// nodes, and **redacts the value of any secret-named key before it reaches the
/// store** (see [`crate::config_keys::REDACTED`] and the redaction sites it
/// names). This lens reads that back: *secret-named config keys are present, here
/// are their paths and names, and here is their redaction state*. It is an
/// **inventory with an invariant check**, and it is useful for exactly two
/// questions: which of my config surfaces deal in credentials, and did anything
/// unredacted get into this graph.
///
/// # What this CANNOT do — read this before extending it
///
/// **It is not a secret scanner and this architecture cannot make it one.** The
/// lens is named for the inventory it can be, not the scanner the shortlist's
/// original title promised.
///
/// - **It cannot detect a hardcoded credential in source code.** It reads
///   `config_key` nodes, which come only from config *files* and from
///   `@rto:config` struct declarations. An AWS key pasted into a `.rs` string
///   literal produces no `config_key` node and is invisible here. Nothing about
///   the node kinds this reads can change that.
/// - **It cannot judge validity.** It never sees a value: by the time anything is
///   in the store, a secret-named value has already been replaced. There is no
///   entropy test, no format check, no liveness probe, and there cannot be one
///   without persisting the very thing extraction exists to redact.
/// - **It cannot tell a real secret from a placeholder.** `API_TOKEN=changeme` in
///   a committed `.env.example` and a genuine token in an uncommitted `.env` are
///   the same row here: same key name, same redacted value, same state.
/// - **It cannot say a repository has no secrets.** An empty report means "no
///   secret-*named* config key", which is a statement about naming. A credential
///   under an innocuous key (`endpoint`, `dsn`, `url`) is not secret-named, is not
///   redacted, and does not appear.
///
/// If you find yourself wanting to widen this toward detecting real credentials,
/// **that instinct is what the rename exists to prevent**: the widening cannot be
/// built on these inputs, and a tool that half-does it while being named for the
/// whole job is worse than one that does the inventory honestly. Every surface
/// carries this limitation in its own words, so a model calling the tool passes it
/// on rather than reporting a security guarantee that was never offered.
///
/// # The heuristic, stated
///
/// "Secret-named" is [`crate::config_keys::is_secret_key`]: the key's
/// ASCII-alphanumerics, lowercased, containing any of `secret`, `password`,
/// `passwd`, `passphrase`, `token`, `apikey`, `credential`, `privatekey`,
/// `accesskey`, `pwd`. So it matches `API_TOKEN`, `db.passwordFile` and
/// `serve.apiKey`, and misses `dsn`, `connection_string` and `auth` — and it
/// false-positives on `token_bucket_size` and `csrf_token_header`, which are
/// settings, not secrets. Both directions of error are inherent to matching on
/// names; neither is reported as a finding.
///
/// # Ordering
///
/// By `(path, name, key)` ascending — an inventory, like [`debt`], not a ranking.
/// There is deliberately no ordering knob: nothing here is a magnitude worth
/// sorting by, and offering one would suggest some keys are more secret than
/// others. Identical input yields byte-identical output.
///
/// # Errors
/// Returns [`StoreError`] on query failure.
pub fn config_secrets(store: &Store, limit: usize) -> Result<ConfigSecretReport, StoreError> {
    let nodes = store.nodes_by_kind(&NodeKind::Other(crate::config_keys::KIND.to_owned()))?;
    let config_keys = nodes.len();
    let mut items = Vec::new();
    let mut redacted = 0usize;
    let mut declared = 0usize;
    let mut unredacted = 0usize;
    let mut redacted_not_secret_named = 0usize;
    let mut files: BTreeSet<String> = BTreeSet::new();
    for node in nodes {
        // Prefer `meta.key` — the dotted key as the source spelled it — and fall
        // back to the node's name, which extraction sets to the same string.
        let name = node
            .meta
            .get("key")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(node.name.as_str())
            .to_owned();
        let value = node.meta.get("value").and_then(serde_json::Value::as_str);
        if !crate::config_keys::is_secret_key(&name) {
            // A redacted value under a non-secret name is a k8s `Secret`'s data:
            // counted so the report's redaction figures reconcile with the graph,
            // but not listed — this lens's subject is secret-*named* keys.
            if value == Some(crate::config_keys::REDACTED) {
                redacted_not_secret_named += 1;
            }
            continue;
        }
        let state = match value {
            Some(v) if v == crate::config_keys::REDACTED => {
                redacted += 1;
                RedactionState::Redacted
            }
            // A struct-derived key omits `meta.value` entirely: there is no
            // literal in the code to redact, so "absent" is the honest state
            // rather than folding it in with a successful redaction.
            None => {
                declared += 1;
                RedactionState::Declared
            }
            Some(_) => {
                unredacted += 1;
                RedactionState::Present
            }
        };
        if let Some(path) = node.path.as_deref() {
            files.insert(path.to_owned());
        }
        items.push(ConfigSecretItem {
            key: node.key,
            path: node.path,
            name,
            state,
            source: node
                .meta
                .get("source")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
        });
    }
    let secret_named = items.len();
    items.sort_by(|a, b| (&a.path, &a.name, &a.key).cmp(&(&b.path, &b.name, &b.key)));
    window(&mut items, 0, limit);

    Ok(ConfigSecretReport {
        schema: SCHEMA,
        limit,
        config_keys,
        secret_named,
        redacted,
        declared,
        unredacted,
        redacted_not_secret_named,
        files: files.len(),
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

/// How a [`CouplingReport`]'s items are ranked. The three orders answer three
/// different questions, which a single undirected degree cannot tell apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CouplingOrder {
    /// By `fan_in + fan_out` — overall call coupling.
    #[default]
    Total,
    /// By `fan_in` — the most depended-on symbols ("what calls this?").
    FanIn,
    /// By `fan_out` — the symbols that reach furthest ("what does this call?").
    FanOut,
}

impl CouplingOrder {
    /// The stable token for this order, as accepted by [`from_token`](Self::from_token).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Total => "total",
            Self::FanIn => "fan_in",
            Self::FanOut => "fan_out",
        }
    }

    /// Parse an order token. `None` for anything else — callers surface an error
    /// rather than silently ranking by something the caller did not ask for.
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        match s {
            "total" => Some(Self::Total),
            "fan_in" => Some(Self::FanIn),
            "fan_out" => Some(Self::FanOut),
            _ => None,
        }
    }

    /// The tokens [`from_token`](Self::from_token) accepts, for error messages
    /// and argument schemas — so the accepted set is stated in exactly one place.
    #[must_use]
    pub fn tokens() -> [&'static str; 3] {
        [
            Self::Total.as_str(),
            Self::FanIn.as_str(),
            Self::FanOut.as_str(),
        ]
    }
}

/// One node's **directed** call coupling in a [`CouplingReport`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CouplingItem {
    /// Natural key of the node.
    pub key: String,
    /// Kind token (e.g. `fn`).
    pub kind: String,
    /// Human-facing name.
    pub name: String,
    /// Repository-relative path, if any.
    pub path: Option<String>,
    /// How many **distinct** other nodes call this one.
    pub fan_in: u32,
    /// How many **distinct** other nodes this one calls.
    pub fan_out: u32,
    /// `fan_in + fan_out` — the directed equivalent of the undirected degree.
    pub total: u32,
    /// Martin's instability, `fan_out / (fan_in + fan_out)`, rounded to two
    /// decimals. `0.0` = purely depended-on (stable); `1.0` = purely depending
    /// (unstable). The denominator is never zero: an item exists only when it
    /// has at least one non-self call edge.
    pub instability: f64,
}

/// Directed call coupling: per-node fan-in and fan-out over `Calls` edges,
/// ranked. The counterpart to an undirected degree ranking, which cannot tell
/// "everything calls this" from "this calls everything".
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CouplingReport {
    /// Stable schema tag ([`SCHEMA`]).
    pub schema: &'static str,
    /// The edge kind measured. Always `calls` — the only edge kind whose
    /// direction carries a caller/callee meaning.
    pub edge_kind: &'static str,
    /// The ranking that produced `items` ([`CouplingOrder::as_str`]).
    pub order: &'static str,
    /// The requested cap on `items`; `0` means unlimited.
    pub limit: usize,
    /// Total `Calls` edges scanned, including duplicates and self-calls.
    pub call_edges: usize,
    /// Self-referential `Calls` edges (recursion), counted in `call_edges` but
    /// excluded from every fan — see [`coupling`].
    pub self_calls: usize,
    /// `Calls` edges whose endpoints are in two different languages: name
    /// collisions from simple-name call resolution, not calls. Counted in
    /// `call_edges` but excluded from every fan — see [`coupling`].
    pub cross_language_calls: usize,
    /// Distinct nodes with at least one non-self `Calls` edge. `items` is the
    /// top `limit` of these, so `coupled_nodes > items.len()` means truncation.
    pub coupled_nodes: usize,
    /// The ranked nodes: by `order` descending, ties broken by `key` ascending.
    pub items: Vec<CouplingItem>,
}

/// Rank nodes by **directed** call coupling — fan-in (distinct callers) and
/// fan-out (distinct callees) over `Calls` edges — most-coupled first by
/// `order`, capped at `limit` (`0` = unlimited).
///
/// Three deliberate counting rules, all of which change the numbers:
///
/// - **Distinct counterparts, not edges.** Edges are a set per `(src, dst, kind,
///   provenance)`, which still admits *parallel* `Calls` edges between one pair
///   at different provenances — a `derived` extraction and an `inferred`
///   suggestion of the same call. Counting distinct counterpart keys makes
///   `fan_in` mean "how many things depend on this", which is the coupling
///   question, rather than "how many layers asserted the dependency".
/// - **Self-calls are excluded from both fans.** Recursion is a real edge but
///   couples a node to nothing outside itself, and counting it would inflate
///   `fan_in` *and* `fan_out` for the same node. It is reported separately as
///   `self_calls` rather than silently dropped.
/// - **Cross-language call edges are excluded.** Roteiro extracts no FFI, so a
///   `Calls` edge between two languages is never a call — see
///   [`same_language`]. Reported as `cross_language_calls`.
///
/// Ordering is total and deterministic: by the chosen metric descending, then by
/// `key` ascending, so identical input yields byte-identical output.
///
/// # Precision
///
/// `fan_in` is exactly as precise as the `Calls` edges beneath it, and those are
/// resolved by **simple name**: a callee that is unique by bare name anywhere in
/// the repository binds to that definition, wherever it lives. So a single
/// same-language helper with a very common name absorbs every call to that name,
/// and its `fan_in` reads high for a reason that has nothing to do with design.
/// Excluding cross-language edges removes the worst of this, but not all of it.
/// Treat a large `fan_in` on a short, generically-named function as a question,
/// not a finding — which is also why this lens offers no CI gate.
///
/// # Errors
/// Returns [`StoreError`] on query failure.
pub fn coupling(
    store: &Store,
    order: CouplingOrder,
    limit: usize,
) -> Result<CouplingReport, StoreError> {
    // `inbound`: dst key -> distinct src keys. `outbound`: src key -> distinct dst
    // keys. Named for the direction rather than caller/callee, which read alike.
    let mut inbound: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut outbound: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut call_edges = 0usize;
    let mut self_calls = 0usize;
    let mut cross_language_calls = 0usize;
    for edge in store.all_edges()? {
        if edge.kind != EdgeKind::Calls {
            continue;
        }
        call_edges += 1;
        if edge.src == edge.dst {
            self_calls += 1;
            continue;
        }
        if !same_language(&edge.src, &edge.dst) {
            cross_language_calls += 1;
            continue;
        }
        inbound
            .entry(edge.dst.clone())
            .or_default()
            .insert(edge.src.clone());
        outbound.entry(edge.src).or_default().insert(edge.dst);
    }

    // Rank on the counts alone, so only the nodes that survive the cap are read
    // back from the store — a whole-graph node scan is not needed to answer a
    // top-N question.
    let keys: BTreeSet<&String> = inbound.keys().chain(outbound.keys()).collect();
    let coupled_nodes = keys.len();
    let mut ranked: Vec<(u32, u32, &String)> = keys
        .into_iter()
        .map(|key| {
            let fan_in = count_of(&inbound, key);
            let fan_out = count_of(&outbound, key);
            (fan_in, fan_out, key)
        })
        .collect();
    ranked.sort_by(|a, b| {
        let metric = |&(fan_in, fan_out, _): &(u32, u32, &String)| match order {
            CouplingOrder::Total => fan_in + fan_out,
            CouplingOrder::FanIn => fan_in,
            CouplingOrder::FanOut => fan_out,
        };
        metric(b).cmp(&metric(a)).then_with(|| a.2.cmp(b.2))
    });
    window(&mut ranked, 0, limit);

    let mut items = Vec::with_capacity(ranked.len());
    for (fan_in, fan_out, key) in ranked {
        // `edges.src`/`edges.dst` are foreign keys into `nodes`, so a node behind
        // a call edge always exists; the guard is defence in depth, not a case.
        let Some(node) = store.get_node(key)? else {
            continue;
        };
        let total = fan_in + fan_out;
        items.push(CouplingItem {
            key: node.key,
            kind: node.kind.as_str().to_owned(),
            name: node.name,
            path: node.path,
            fan_in,
            fan_out,
            total,
            instability: round2(f64::from(fan_out) / f64::from(total)),
        });
    }

    Ok(CouplingReport {
        schema: SCHEMA,
        edge_kind: EdgeKind::Calls.as_str(),
        order: order.as_str(),
        limit,
        call_edges,
        self_calls,
        cross_language_calls,
        coupled_nodes,
        items,
    })
}

/// The language token of a symbol key (`sym:<lang>:<path>#<name>` → `<lang>`),
/// or `None` for any other key shape.
fn sym_lang(key: &str) -> Option<&str> {
    let rest = key.strip_prefix("sym:")?;
    let (lang, _) = rest.split_once(':')?;
    (!lang.is_empty()).then_some(lang)
}

/// Whether a call edge's two endpoints are in the same language — `true` unless
/// both keys carry a language token and the tokens differ.
///
/// Cross-file call resolution binds a callee by **simple name** across every
/// `Fn` node in the repository, language included. Roteiro extracts no FFI, so
/// nothing in the graph can legitimately record a JavaScript function calling a
/// Rust one; such an edge is a name collision — a lone Rust `join` helper
/// absorbing every JavaScript `.join(…)` in the tree. Excluding them keeps a
/// language's coupling figures about that language.
///
/// Unknown-shaped keys (anything that is not `sym:<lang>:…`) are **kept**: this
/// filter removes edges it can prove span two languages, and never guesses.
fn same_language(src: &str, dst: &str) -> bool {
    match (sym_lang(src), sym_lang(dst)) {
        (Some(a), Some(b)) => a == b,
        _ => true,
    }
}

/// The size of `key`'s counterpart set, as a `u32` (a node cannot have more
/// distinct counterparts than there are nodes, so the cast cannot realistically
/// saturate; saturating beats wrapping if it ever did).
fn count_of(map: &BTreeMap<String, BTreeSet<String>>, key: &str) -> u32 {
    map.get(key)
        .map_or(0, |set| u32::try_from(set.len()).unwrap_or(u32::MAX))
}

/// Round to two decimals, so the serialised ratio is short and stable rather
/// than carrying the full binary expansion of a division.
fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
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
/// most `limit` hits — or **every match when `limit == 0`**, which is [`window`]'s
/// rule and the one every list lens follows (issue #393). An empty result
/// therefore always means "nothing matched", never "you asked for nothing".
///
/// Case-insensitive; every whitespace/`::`-separated token must appear
/// somewhere in the node's **name, key, path, or captured `meta.content`**
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
    // `window`, not `truncate`: `0` is unlimited here as it is everywhere else.
    // The scan above is full-population at every limit, so an unbounded search
    // costs the same as a bounded one — only the printing differs.
    window(&mut hits, 0, limit);
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

/// A hit in the **memory** channel: something a session learned, never a graph
/// fact and never a re-derivable one.
///
/// Deliberately *not* a [`SearchHit`], for the reason [`GeneratedHit`] is not: a
/// memory record has no node, no provenance and no key, and giving it a
/// [`NodeSummary`] would be the first step towards its being treated like one.
/// Unlike either of the other channels, it also carries **what the tree thinks of
/// it** — [`MemoryHit::applies`] and [`MemoryHit::anchor_state`] — because a
/// lesson about code that has since moved is worth reading and worth labelling,
/// and returning it unlabelled would be the worse of the two mistakes.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MemoryHit {
    /// Relevance within the memory channel. Not comparable with a
    /// [`SearchHit::score`] or a [`GeneratedHit::score`]: three channels, three
    /// scorers, on purpose.
    pub score: u32,
    /// Always `true`. A marker a consumer cannot miss or forget to check.
    pub memory: bool,
    /// The record's id — its generation, and what `roteiro memory forget` takes.
    pub id: i64,
    /// What kind of knowledge it is (`lesson` | `attempt` | …).
    pub kind: &'static str,
    /// The namespace it was recorded in. **Not a branch label.**
    pub scope: String,
    /// The node key it is anchored to, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    /// What that anchor is worth against the current tree (`valid` | `drifted` |
    /// `vanished` | `unverifiable` | `unanchored`).
    pub anchor_state: &'static str,
    /// **Whether this record applies to the tree being searched.** A `false` here
    /// is a label, never a reason to have withheld the hit.
    pub applies: bool,
    /// The evidence multiplier the record's own ranking gave it
    /// (`base_confidence × anchor_penalty`), reported so the channel's score can
    /// be taken apart.
    pub evidence: f64,
    /// A bounded, whitespace-collapsed excerpt of the body, on the same terms as
    /// [`SearchHit::snippet`].
    pub snippet: Option<String>,
}

/// The three channels a search returns.
///
/// They are separate fields rather than one merged list because merging is
/// precisely what must not happen: generated text and remembered prose may both
/// be *retrievable*, but neither may ever be *indistinguishable* from a derived or
/// authored fact, and a single ranked list would make the distinction a matter of
/// reading each element carefully.
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
    /// The memory channel. **Empty unless [`SearchOptions::include_memory`] was
    /// set** — off by default, so unreviewed accumulated prose cannot reach a
    /// default search either.
    pub memory: Vec<MemoryHit>,
}

/// How to search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchOptions {
    /// Maximum hits **per channel**, where `0` is unlimited ([`window`]'s rule,
    /// applied per channel). Each channel is ranked and windowed independently,
    /// so opting in to another one never displaces a graph hit, and never
    /// silently returns fewer of them — and `0` is "all of each channel asked
    /// for", not "all of them merged and then cut".
    pub limit: usize,
    /// Fold in the generated channel. Off by default (see
    /// [`SearchOptions::default`]).
    pub include_generated: bool,
    /// Fold in the memory channel. Off by default, for the same reason: what an
    /// agent remembers is unreviewed, unredacted and accumulated, so it is
    /// something a caller asks for rather than something that arrives.
    pub include_memory: bool,
}

impl Default for SearchOptions {
    /// Ten hits, graph channel only. The default is the safe answer: everything
    /// that is not an extracted or authored fact is opt-in, always.
    fn default() -> Self {
        Self {
            limit: 10,
            include_generated: false,
            include_memory: false,
        }
    }
}

/// Search every channel: the graph, and — each only when asked for —
/// model-generated media content and episodic agent memory.
///
/// The graph channel is exactly [`search`]. The other two are ranked by scorers of
/// their own ([`generated_score`], [`memory_score`]) which have **no provenance
/// term at all**, so neither can acquire the `authored` boost that curated intent
/// gets. Neither could do so even by accident: neither record is a node, so
/// neither ever reaches the code that applies that boost.
///
/// The memory channel is scored with **no decay** regardless of what a caller
/// might prefer elsewhere, so a search is reproducible for a fixed store and a
/// fixed tree.
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
    let memory = if opts.include_memory {
        search_memory(store, query, opts.limit)?
    } else {
        Vec::new()
    };
    Ok(SearchResults {
        schema: SCHEMA,
        hits,
        generated,
        memory,
    })
}

/// Rank the memory channel alone.
///
/// Built on [`Store::recall_memory`] rather than on a query of its own, so the
/// channel inherits every promise recall makes without restating any of them: a
/// superseded record is already gone, an unanchored one is already labelled, and
/// nothing here writes anything. Decay is fixed at [`crate::Decay::None`] so a
/// search over an unchanged store and tree is reproducible.
///
/// Ties break by newest generation, so the order is total. `limit` follows
/// [`window`]: `0` is every matching record, not none of them.
fn search_memory(store: &Store, query: &str, limit: usize) -> Result<Vec<MemoryHit>, StoreError> {
    let q = query.trim().to_lowercase();
    let tokens: Vec<&str> = q.split("::").flat_map(str::split_whitespace).collect();
    if tokens.is_empty() {
        return Ok(Vec::new());
    }
    // Recall does the filtering, the anchor resolution and the evidence
    // weighting; this function only adds the lexical relevance a search wants.
    let recalled = store.recall_memory(&crate::RecallOptions {
        query: Some(query),
        decay: crate::Decay::None,
        ..crate::RecallOptions::default()
    })?;

    let mut hits: Vec<MemoryHit> = recalled
        .results
        .into_iter()
        .map(|r| {
            let body = r.record.body.to_lowercase();
            let anchor = r
                .record
                .anchor
                .as_ref()
                .map(|a| a.key.to_lowercase())
                .unwrap_or_default();
            MemoryHit {
                score: memory_score(&q, &tokens, &body, &anchor, r.score),
                memory: true,
                id: r.record.id,
                kind: r.record.kind.as_str(),
                scope: r.record.scope.clone(),
                anchor: r.record.anchor.as_ref().map(|a| a.key.clone()),
                anchor_state: r.record.anchor_state.as_str(),
                applies: r.record.applies,
                evidence: r.score,
                snippet: content_snippet(&serde_json::json!({ "content": r.record.body })),
            }
        })
        .collect();
    hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| b.id.cmp(&a.id)));
    window(&mut hits, 0, limit);
    Ok(hits)
}

/// Relevance of one memory record: **lexical match, weighted by the record's own
/// evidence**, and nothing else.
///
/// The `evidence` factor is `base_confidence × anchor_penalty` from
/// [`crate::Store::recall_memory`] — so a lesson whose anchor still resolves in
/// this tree outranks an equally-worded one whose code has moved on, which is the
/// whole depreciation model showing up in search.
///
/// # The weight is in `[0, 1]`, and zero is reachable — deliberately
///
/// An earlier version of this comment said `(0, 1]`. That was wrong, and the
/// half-open interval hid a decision rather than describing one. The two factors
/// are not alike and the difference is the point:
///
/// - **[`crate::anchor_penalty`] can never be zero.** Its floor is `0.25`
///   ([`crate::AnchorState::Drifted`]), and
///   `memory::tests::anchor_penalty_demotes_without_ever_silencing` pins that
///   every state is `> 0`. So **drift can never drive evidence to zero** — which
///   is ADR-0013's "demote, never delete" rule holding *structurally*, not by
///   convention. Roteiro's own inference about a record is never allowed to
///   reduce it to nothing.
/// - **`base_confidence` can be exactly `0.0`**, because the writer can say so.
///   `roteiro memory add --confidence 0` is an operator stating "I am recording
///   this and I give it no credence." Flooring that would silently overrule an
///   explicit statement — and the value is a probability, where `0.0` is
///   legitimate rather than a boundary error.
///
/// So the asymmetry is exactly the right way round: **what Roteiro infers never
/// silences a record; what the operator explicitly states is honoured.**
///
/// # Zero relevance is not zero visibility
///
/// A zero score does **not** remove a hit. Nothing in this module or in
/// [`crate::Store::recall_memory`] filters on the score — it orders, and the
/// record comes back, is printed, and is labelled exactly as any other.
/// `a_zero_confidence_memory_is_ranked_last_and_still_returned` enforces that in
/// both surfaces, so the claim is a tested property rather than something this
/// comment asserts and nothing checks. (A limit can still truncate a
/// bottom-ranked hit — that is what a limit means, and it applies to every hit
/// regardless of score.)
///
/// The omissions are the point, and each is deliberate:
///
/// - **no `authored` boost** — this is the whole reason the channel exists. That
///   +40 is for intent a human deliberately wrote into a reviewed file;
///   accumulated, unreviewed, unredacted prose riding it would be trust-model
///   contamination by construction.
/// - **no overview boost** — a README's landing-page privilege is about authored
///   documentation.
/// - **no name or key term** — a memory record has neither.
///
/// Because this scorer shares no branch with the node scorer, "memory never
/// acquires the authored boost" is a structural fact rather than a condition to be
/// maintained.
fn memory_score(q: &str, tokens: &[&str], body: &str, anchor: &str, evidence: f64) -> u32 {
    let mut relevance: i32 = 0;
    if body.contains(q) {
        relevance += 25;
    }
    for t in tokens {
        if body.contains(t) {
            relevance += 8;
        } else if anchor.contains(t) {
            relevance += 3;
        }
    }
    // `[0.0, 1.0]`, closed at both ends: zero is reachable, and only ever because
    // a writer stated it. See the header — `anchor_penalty` cannot contribute a
    // zero, so drift can never land here.
    let weighted = f64::from(relevance.max(0)) * evidence.clamp(0.0, 1.0);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the product of a small non-negative relevance and a weight in [0, 1]"
    )]
    let score = weighted.round() as u32;
    score
}

/// Rank the generated channel alone. Ties break by `(producer, blob)` so results
/// are stable. `limit` follows [`window`]: `0` is every matching record, not none
/// of them.
fn search_generated(
    store: &Store,
    query: &str,
    limit: usize,
) -> Result<Vec<GeneratedHit>, StoreError> {
    let q = query.trim().to_lowercase();
    let tokens: Vec<&str> = q.split("::").flat_map(str::split_whitespace).collect();
    if tokens.is_empty() {
        return Ok(Vec::new());
    }
    let mut hits: Vec<GeneratedHit> = Vec::new();
    for record in store.media_records(&crate::MediaFilter::default())? {
        // A record the pre-generation gate refused holds a measurement, not text.
        // It is deliberately unsearchable: it has nothing to match on, and the
        // path *would* match — which would put a silent clip back into search
        // results as a hit with an empty snippet, which is the shape of the very
        // bug ADR-0015 exists to correct.
        let Some(generated_text) = record.outcome.text() else {
            continue;
        };
        let text = generated_text.to_lowercase();
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
            snippet: content_snippet(&serde_json::json!({ "content": generated_text })),
        });
    }
    hits.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| (&a.producer, &a.blob).cmp(&(&b.producer, &b.blob)))
    });
    window(&mut hits, 0, limit);
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
    use super::{
        ConfigSecretReport, CouplingItem, CouplingOrder, CouplingReport, DebtDensityReport,
        DensityItem, DensityOrder, RedactionState, SCHEMA, SNIPPET_MAX, SearchOptions,
        SearchResults, config_secrets, coupling, debt_density, explain, glob_match, list_kind,
        memory_score, path, search, search_channels, window,
    };
    use crate::{AnchorState, Edge, EdgeKind, FactSet, Node, NodeKind, Store};

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

    /// [`window`] is the single definition of `limit`/`offset` for every list
    /// lens, so its contract is pinned here rather than only through the lenses
    /// that call it.
    #[test]
    fn window_reads_zero_as_unlimited_and_offsets_before_limiting() {
        let ten = || (0..10).collect::<Vec<u8>>();

        // `0` is unlimited, not empty — the whole point of #375.
        let mut all = ten();
        window(&mut all, 0, 0);
        assert_eq!(all, ten(), "limit 0 keeps everything");

        // A non-zero limit cuts from the end, keeping the caller's order.
        let mut top = ten();
        window(&mut top, 0, 3);
        assert_eq!(top, vec![0, 1, 2]);

        // A limit at or beyond the population is a no-op, so the boundary
        // between "bounded" and "unbounded" has no step in it.
        let mut exact = ten();
        window(&mut exact, 0, 10);
        assert_eq!(exact, ten());
        let mut over = ten();
        window(&mut over, 0, 99);
        assert_eq!(over, ten());

        // `offset` applies first and `limit` to what remains.
        let mut paged = ten();
        window(&mut paged, 4, 3);
        assert_eq!(paged, vec![4, 5, 6]);

        // The decision this fix had to make: offset with an unlimited limit is
        // "skip N, then every remaining item" — not "skip N, then nothing".
        let mut rest = ten();
        window(&mut rest, 7, 0);
        assert_eq!(rest, vec![7, 8, 9], "offset then unlimited");

        // An offset at or past the end is an empty page, not a panic and not a
        // wrapped-around one.
        let mut at_end = ten();
        window(&mut at_end, 10, 0);
        assert!(at_end.is_empty());
        let mut past_end = ten();
        window(&mut past_end, 500, 0);
        assert!(past_end.is_empty());
        let mut past_end_limited = ten();
        window(&mut past_end_limited, 500, 5);
        assert!(past_end_limited.is_empty());

        // An empty input stays empty under every combination.
        let mut empty: Vec<u8> = Vec::new();
        window(&mut empty, 0, 0);
        window(&mut empty, 3, 0);
        window(&mut empty, 0, 3);
        assert!(empty.is_empty());
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

    /// The population every issue-#393 test below works over: 12 in each of the
    /// three channels, each matching a term only its own channel carries.
    ///
    /// 12 is deliberately above the default of 10, so a `limit` of `0` that had
    /// quietly fallen back to the default could not pass for "unlimited".
    fn three_channels(population: usize) -> Store {
        use crate::{
            GeneratedContent, MediaKind, MediaOutcome, MediaWrite, MemoryKind, MemoryWrite,
            Producer,
        };

        let mut store = Store::open_in_memory().expect("store");
        let mut facts = FactSet::new();
        for i in 0..population {
            facts = facts.with_node(Node::new(
                format!("sym:rust:a.rs#quokka{i}"),
                NodeKind::Fn,
                format!("quokka{i}"),
            ));
        }
        store.apply_factset(&facts).expect("apply");

        let producer = Producer {
            kind: MediaKind::Audio,
            model: "voxtral-mini-3b".to_owned(),
            model_digest: "4705be8e".to_owned(),
            quantisation: "Q4_K_M".to_owned(),
            mmproj_digest: "4f24c4ef".to_owned(),
            prompt: "Transcribe this audio recording.".to_owned(),
            temperature: 0.0,
            max_tokens: 512,
        };
        for i in 0..population {
            store
                .record_memory(&MemoryWrite {
                    scope: crate::DEFAULT_MEMORY_SCOPE,
                    kind: MemoryKind::Lesson,
                    anchor: None,
                    body: &format!("wombat lesson number {i}"),
                    confidence: None,
                    supersedes: None,
                })
                .expect("memory write");
            assert!(
                store
                    .record_media_content(&MediaWrite {
                        blob_id: &format!("blob-{i}"),
                        path: &format!("assets/clip{i}.wav"),
                        producer: &producer,
                        tool_version: "0.0.0",
                        outcome: &MediaOutcome::Generated(GeneratedContent {
                            text: format!("narwhal transcript number {i}"),
                            confidence: None,
                        }),
                        replace: false,
                    })
                    .expect("media write"),
                "each clip is a fresh record",
            );
        }
        store
    }

    /// Every channel asked for, at `limit`.
    fn all_channels(store: &Store, query: &str, limit: usize) -> SearchResults {
        search_channels(
            store,
            query,
            SearchOptions {
                limit,
                include_generated: true,
                include_memory: true,
            },
        )
        .expect("search")
    }

    /// Issue #393: `limit == 0` reads as **unlimited on the graph channel**, and
    /// it is [`window`] that says so rather than a rule of `search`'s own — the
    /// third reading of one parameter name is gone, not relocated.
    #[test]
    fn search_reads_zero_as_unlimited_and_only_removes_the_cut() {
        const POPULATION: usize = 12;
        let store = three_channels(POPULATION);

        let bounded = search(&store, "quokka", 10).expect("search");
        assert_eq!(bounded.len(), 10, "a positive limit still cuts");

        let unlimited = search(&store, "quokka", 0).expect("search");
        assert_eq!(unlimited.len(), POPULATION, "0 is every match");

        // An unlimited search is the same ranking uncut, not a different one:
        // the bounded page is the prefix of the unbounded one.
        assert_eq!(
            unlimited[..10]
                .iter()
                .map(|h| h.node.key.as_str())
                .collect::<Vec<_>>(),
            bounded
                .iter()
                .map(|h| h.node.key.as_str())
                .collect::<Vec<_>>(),
            "unlimited only removes the cut",
        );
    }

    /// The unit is **per channel**: `0` is "all of each channel that was asked
    /// for", not "all of them merged and then cut". Each channel here matches a
    /// term the other two do not, so the three populations stay separable.
    #[test]
    fn each_search_channel_reads_zero_as_unlimited_over_its_own_population() {
        const POPULATION: usize = 12;
        let store = three_channels(POPULATION);

        // Each channel matches a term the other two do not, so a bounded and an
        // unbounded read of one says nothing about the others.
        assert_eq!(all_channels(&store, "quokka", 10).hits.len(), 10);
        assert_eq!(
            all_channels(&store, "quokka", 0).hits.len(),
            POPULATION,
            "graph channel: 0 is unlimited",
        );
        assert_eq!(all_channels(&store, "wombat", 10).memory.len(), 10);
        assert_eq!(
            all_channels(&store, "wombat", 0).memory.len(),
            POPULATION,
            "memory channel: 0 is unlimited",
        );
        assert_eq!(all_channels(&store, "narwhal", 10).generated.len(), 10);
        assert_eq!(
            all_channels(&store, "narwhal", 0).generated.len(),
            POPULATION,
            "generated channel: 0 is unlimited",
        );

        // And the unit really is per channel: an unbounded search of one term
        // leaves the channels it does not match empty rather than filling them.
        let graph_only = all_channels(&store, "quokka", 0);
        assert!(
            graph_only.memory.is_empty() && graph_only.generated.is_empty(),
            "unlimited is per channel, not a merged population",
        );
    }

    /// What keeps "unlimited" from meaning "the whole store": a query with no
    /// tokens matches nothing, at `0` exactly as at any other limit. `--limit 0`
    /// is bounded by what was asked for, not by the population.
    #[test]
    fn a_tokenless_query_is_nothing_in_every_channel_at_every_limit() {
        let store = three_channels(12);
        for blank in ["", "   ", "\t\n"] {
            for limit in [0, 10] {
                let nothing = all_channels(&store, blank, limit);
                assert!(
                    nothing.hits.is_empty()
                        && nothing.generated.is_empty()
                        && nothing.memory.is_empty(),
                    "a query with no tokens is nothing, not everything ({blank:?}, limit {limit})",
                );
            }
        }
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

    /// **The evidence weight is closed at both ends**, and the two ends mean
    /// different things.
    ///
    /// The boundary the doc comment used to get wrong: it claimed `(0, 1]`, which
    /// would have made a zero unreachable. It is reachable, from a writer stating
    /// `--confidence 0` and from nowhere else — the lowest weight Roteiro can
    /// *infer* is `anchor_penalty(Drifted)`, and that still leaves a score
    /// standing, which is checked here against the real constant rather than a
    /// number copied from it.
    #[test]
    fn the_evidence_weight_is_closed_at_both_ends() {
        let score = |evidence| memory_score("batch", &["batch"], "a batch cursor", "", evidence);
        let full = score(1.0);
        assert!(full > 0, "a fully-evidenced hit scores");
        assert_eq!(score(0.0), 0, "and a zero weight takes it to zero");
        assert!(
            score(0.5) < full && score(0.5) > 0,
            "in between, in between"
        );

        // The worst Roteiro can infer about a record still leaves it scoring —
        // "demote, never delete", holding as arithmetic.
        let worst_inferable = [
            AnchorState::Valid,
            AnchorState::Unanchored,
            AnchorState::Unverifiable,
            AnchorState::Vanished,
            AnchorState::Drifted,
        ]
        .into_iter()
        .map(crate::anchor_penalty)
        .fold(f64::INFINITY, f64::min);
        assert!(
            score(worst_inferable) > 0,
            "the most demoted anchor state ({worst_inferable}) must not silence a hit",
        );

        // Out-of-range input is clamped rather than trusted, so a corrupt stored
        // confidence cannot manufacture a score above the honest ceiling.
        assert_eq!(score(2.0), full, "clamped at the top");
        assert_eq!(score(-1.0), 0, "and at the bottom");
    }

    // -- coupling (Q3) -----------------------------------------------------

    /// A graph whose two most-coupled nodes have the **same undirected degree**
    /// but opposite direction: `hub` is called by two callers and calls nothing;
    /// `spread` calls two callees and is called by nothing. An undirected degree
    /// ranking cannot tell them apart, which is the whole point of this lens.
    fn coupled() -> Store {
        let mut store = Store::open_in_memory().expect("store");
        let mut facts = FactSet::new();
        for name in ["hub", "spread", "a", "b", "x", "y"] {
            facts = facts.with_node(Node::new(
                format!("sym:rust:a.rs#{name}"),
                NodeKind::Fn,
                name,
            ));
        }
        for (src, dst) in [("a", "hub"), ("b", "hub"), ("spread", "x"), ("spread", "y")] {
            facts = facts.with_edge(Edge::derived(
                format!("sym:rust:a.rs#{src}"),
                format!("sym:rust:a.rs#{dst}"),
                EdgeKind::Calls,
            ));
        }
        store.apply_factset(&facts).expect("apply");
        store
    }

    /// Find an item by symbol name, so assertions read by name not by index.
    fn item<'a>(report: &'a CouplingReport, name: &str) -> &'a CouplingItem {
        report
            .items
            .iter()
            .find(|i| i.name == name)
            .unwrap_or_else(|| panic!("`{name}` missing from {:?}", report.items))
    }

    #[test]
    fn coupling_keeps_the_direction_an_undirected_degree_discards() {
        let report = coupling(&coupled(), CouplingOrder::Total, 0).expect("coupling");
        let hub = item(&report, "hub");
        let spread = item(&report, "spread");

        // Identical undirected degree — what a both-ends-incremented ranking sees.
        assert_eq!(hub.total, spread.total, "same total coupling");

        // …and opposite direction, which is what this lens exists to report.
        assert_eq!((hub.fan_in, hub.fan_out), (2, 0), "hub is depended upon");
        assert_eq!(
            (spread.fan_in, spread.fan_out),
            (0, 2),
            "spread depends on others"
        );
        assert!(
            (hub.instability - 0.0).abs() < f64::EPSILON,
            "a purely called node is maximally stable: {}",
            hub.instability
        );
        assert!(
            (spread.instability - 1.0).abs() < f64::EPSILON,
            "a purely calling node is maximally unstable: {}",
            spread.instability
        );

        assert_eq!(report.edge_kind, "calls");
        assert_eq!(report.coupled_nodes, 6);
        assert_eq!(report.call_edges, 4);
        assert_eq!(report.self_calls, 0);
        assert_eq!(report.cross_language_calls, 0);
    }

    #[test]
    fn coupling_excludes_cross_language_name_collisions() {
        // Cross-file call resolution binds a callee by simple name across every
        // `Fn` node regardless of language, and Roteiro extracts no FFI — so a
        // JavaScript function "calling" a Rust one is a name collision. On this
        // repository that single rule is the difference between a Rust helper
        // reading as the most depended-on symbol in the tree and not appearing
        // at all.
        let mut store = coupled();
        let mut facts = FactSet::new().with_node(Node::new(
            "sym:javascript:app.js#render",
            NodeKind::Fn,
            "render",
        ));
        facts = facts.with_edge(Edge::derived(
            "sym:javascript:app.js#render",
            "sym:rust:a.rs#hub",
            EdgeKind::Calls,
        ));
        store.apply_factset(&facts).expect("apply");

        let report = coupling(&store, CouplingOrder::Total, 0).expect("coupling");
        assert_eq!(
            item(&report, "hub").fan_in,
            2,
            "a JavaScript caller is not a dependant of a Rust function"
        );
        assert_eq!(
            report.cross_language_calls, 1,
            "the excluded edge is reported, not silently dropped"
        );
        assert_eq!(report.call_edges, 5, "and still counted as scanned");
    }

    #[test]
    fn same_language_never_guesses_about_unknown_key_shapes() {
        assert!(super::same_language("sym:rust:a.rs#f", "sym:rust:b.rs#g"));
        assert!(!super::same_language(
            "sym:javascript:a.js#f",
            "sym:rust:b.rs#g"
        ));
        // A key that is not `sym:<lang>:…` carries no language to compare, so the
        // edge is kept: this filter drops only what it can prove spans languages.
        assert!(super::same_language("file:a.md", "sym:rust:b.rs#g"));
        assert!(super::same_language("sym:", "sym:rust:b.rs#g"));
        assert_eq!(super::sym_lang("sym:rust:a.rs#f"), Some("rust"));
        assert_eq!(
            super::sym_lang("sym::a.rs#f"),
            None,
            "empty lang is no lang"
        );
        assert_eq!(super::sym_lang("marker:a.rs#7"), None);
    }

    #[test]
    fn coupling_counts_distinct_callers_not_parallel_edges() {
        // Migration 3 makes edges a set per `(src, dst, kind, provenance)` — so
        // the way one caller contributes two `Calls` rows is by **provenance**:
        // an extractor's `derived` call and an inference layer's `inferred` one.
        // Two rows, one dependant.
        let mut store = coupled();
        let inferred = Edge::inferred("sym:rust:a.rs#a", "sym:rust:a.rs#hub", EdgeKind::Calls, 0.9);
        store
            .apply_factset(&FactSet::new().with_edge(inferred))
            .expect("apply");

        let report = coupling(&store, CouplingOrder::Total, 0).expect("coupling");
        assert_eq!(
            item(&report, "hub").fan_in,
            2,
            "the same caller at two provenances is one dependant, not two"
        );
        // The raw edge is still counted, so the parallel edge stays visible
        // rather than being silently normalised away.
        assert_eq!(
            report.call_edges, 5,
            "the extra edge is reported as scanned"
        );
    }

    #[test]
    fn coupling_excludes_self_calls_from_both_fans() {
        // Recursion is a real edge that couples a node to nothing outside itself;
        // counting it would inflate `fan_in` AND `fan_out` for the same node.
        let mut store = coupled();
        let recursive = Edge::derived("sym:rust:a.rs#hub", "sym:rust:a.rs#hub", EdgeKind::Calls);
        store
            .apply_factset(&FactSet::new().with_edge(recursive))
            .expect("apply");

        let report = coupling(&store, CouplingOrder::Total, 0).expect("coupling");
        let hub = item(&report, "hub");
        assert_eq!(
            (hub.fan_in, hub.fan_out),
            (2, 0),
            "recursion changes neither fan"
        );
        assert_eq!(report.self_calls, 1, "but it is reported, not dropped");
    }

    #[test]
    fn coupling_order_picks_the_question_being_asked() {
        let store = coupled();
        let top = |order| {
            coupling(&store, order, 1).expect("coupling").items[0]
                .name
                .clone()
        };
        assert_eq!(top(CouplingOrder::FanIn), "hub", "most depended-on");
        assert_eq!(top(CouplingOrder::FanOut), "spread", "reaches furthest");

        // `total` cannot separate the two, so the tie must break on `key` —
        // a stable order rather than whatever the map iteration yields.
        let by_total = coupling(&store, CouplingOrder::Total, 2).expect("coupling");
        assert_eq!(
            by_total.items.iter().map(|i| &i.name).collect::<Vec<_>>(),
            ["hub", "spread"],
            "ties break by key ascending"
        );
    }

    #[test]
    fn coupling_reports_truncation_and_is_deterministic() {
        let store = coupled();
        let capped = coupling(&store, CouplingOrder::Total, 2).expect("coupling");
        assert_eq!(capped.items.len(), 2);
        assert_eq!(
            capped.coupled_nodes, 6,
            "the population is reported, so a capped list cannot read as the whole graph"
        );
        assert_eq!(capped.limit, 2);

        // Identical input → byte-identical output, including the ratio's rendering.
        let a = serde_json::to_string(&capped).expect("json");
        let b =
            serde_json::to_string(&coupling(&store, CouplingOrder::Total, 2).expect("coupling"))
                .expect("json");
        assert_eq!(a, b, "deterministic serialisation");
    }

    #[test]
    fn coupling_ignores_edge_kinds_whose_direction_is_not_a_call() {
        // `references` is directed too, but an ADR referencing a symbol is not a
        // caller. Only `Calls` may move these numbers.
        let mut store = coupled();
        let mut facts = FactSet::new().with_node(Node::new("adr:0001", NodeKind::Adr, "A"));
        facts = facts.with_edge(Edge::authored(
            "adr:0001",
            "sym:rust:a.rs#hub",
            EdgeKind::References,
        ));
        store.apply_factset(&facts).expect("apply");

        let report = coupling(&store, CouplingOrder::Total, 0).expect("coupling");
        assert_eq!(item(&report, "hub").fan_in, 2, "a reference is not a call");
        assert!(
            !report.items.iter().any(|i| i.key == "adr:0001"),
            "a node with no call edges is not in the population: {:?}",
            report.items
        );
        assert_eq!(report.call_edges, 4);
    }

    #[test]
    fn coupling_order_tokens_round_trip() {
        for token in CouplingOrder::tokens() {
            let order = CouplingOrder::from_token(token)
                .unwrap_or_else(|| panic!("`{token}` is advertised but not accepted"));
            assert_eq!(order.as_str(), token);
        }
        assert!(
            CouplingOrder::from_token("degree").is_none(),
            "an unknown order is rejected, not silently defaulted"
        );
    }

    // -- debt density (Q1) -------------------------------------------------

    /// A `file` node carrying the `meta.lines` this lens divides by — the shape
    /// `extract::file_node` emits for every blob.
    fn file_of(path: &str, lines: u64) -> Node {
        let mut node = Node::new(format!("file:{path}"), NodeKind::File, path);
        node.path = Some(path.to_owned());
        node.meta = serde_json::json!({ "bytes": lines * 30, "lines": lines });
        node
    }

    /// A marker node as `markers::augment` emits it.
    fn marker_of(path: &str, line: u32, category: &str) -> Node {
        let mut node = Node::new(
            format!("marker:{path}#{line}"),
            NodeKind::Marker,
            format!("TODO {line}"), // roteiro:ignore
        );
        node.path = Some(path.to_owned());
        node.meta = serde_json::json!({
            "category": category,
            "text": format!("TODO {line}"), // roteiro:ignore
            "line": line,
        });
        node
    }

    /// Two files with the **same marker count** and very different lengths —
    /// indistinguishable under `debt`, twenty-fold apart under density. Plus a
    /// third, short file whose single marker would top the ranking on arithmetic
    /// alone.
    fn marked() -> Store {
        let mut store = Store::open_in_memory().expect("store");
        let mut facts = FactSet::new()
            .with_node(file_of("big.rs", 4000))
            .with_node(file_of("small.rs", 200))
            .with_node(file_of("tiny.rs", 10));
        for line in 1..=40 {
            facts = facts.with_node(marker_of("big.rs", line, "todo")); // roteiro:ignore
            facts = facts.with_node(marker_of("small.rs", line, "todo")); // roteiro:ignore
        }
        facts = facts.with_node(marker_of("tiny.rs", 3, "stub"));
        store.apply_factset(&facts).expect("apply");
        store
    }

    /// Every default: no category filter, no ignore globs, unlimited, floored at
    /// [`super::DEFAULT_MIN_LINES`].
    fn density(store: &Store, order: DensityOrder) -> DebtDensityReport {
        debt_density(store, &[], &[], order, 0, super::DEFAULT_MIN_LINES).expect("density")
    }

    /// Find an item by path, so assertions read by file not by index.
    fn at<'a>(report: &'a DebtDensityReport, path: &str) -> &'a DensityItem {
        report
            .items
            .iter()
            .find(|i| i.path == path)
            .unwrap_or_else(|| panic!("`{path}` missing from {:?}", report.items))
    }

    #[test]
    fn density_separates_files_a_raw_marker_count_cannot() {
        let report = density(&marked(), DensityOrder::Density);
        let big = at(&report, "big.rs");
        let small = at(&report, "small.rs");

        // Identical under `debt` — the same forty markers each.
        assert_eq!(big.markers, small.markers, "same raw count");

        // …and twenty-fold apart under density, which is the whole lens.
        assert!(
            (big.per_kloc - 10.0).abs() < f64::EPSILON,
            "40 markers in 4000 lines is 10 per kloc, was {}",
            big.per_kloc
        );
        assert!(
            (small.per_kloc - 200.0).abs() < f64::EPSILON,
            "40 markers in 200 lines is 200 per kloc, was {}",
            small.per_kloc
        );
        assert_eq!(
            report.items.first().map(|i| i.path.as_str()),
            Some("small.rs"),
            "the dense file ranks first: {:?}",
            report.items
        );

        // The per-file category split, so "forty todo" and "forty stub" stay
        // distinguishable in a report that otherwise shows one number per file.
        assert_eq!(small.by_category.get("todo"), Some(&40)); // roteiro:ignore
        assert_eq!(report.schema, SCHEMA);
    }

    #[test]
    fn markers_order_ranks_the_way_debt_already_does() {
        // The control: on `markers` the two forty-marker files tie and break on
        // path, so density is demonstrably the thing that separated them — not
        // some other difference in the fixture.
        let report = density(&marked(), DensityOrder::Markers);
        assert_eq!(
            report.items.iter().map(|i| &i.path).collect::<Vec<_>>(),
            ["big.rs", "small.rs"],
            "equal counts tie and break on path ascending"
        );
    }

    #[test]
    fn the_short_file_floor_excludes_without_hiding() {
        // `tiny.rs` is 1 marker in 10 lines = 100 per kloc, which would place it
        // second on arithmetic alone. The floor keeps it out of the *ranking*
        // while leaving it in the population and the totals.
        let report = density(&marked(), DensityOrder::Density);
        assert!(
            !report.items.iter().any(|i| i.path == "tiny.rs"),
            "a 10-line file is not ranked: {:?}",
            report.items
        );
        assert_eq!(report.short_files, 1, "and its exclusion is reported");
        assert_eq!(
            report.files_with_markers, 3,
            "the population still counts it"
        );
        assert_eq!(report.ranked_files, 2);
        assert_eq!(
            report.total_markers, 81,
            "and so do the totals: 40 + 40 + 1"
        );

        // `min_lines = 0` disables the floor rather than merely lowering it.
        let unfloored =
            debt_density(&marked(), &[], &[], DensityOrder::Density, 0, 0).expect("density");
        assert_eq!(unfloored.short_files, 0);
        assert_eq!(unfloored.ranked_files, 3);
        let tiny = at(&unfloored, "tiny.rs").per_kloc;
        assert!(
            (tiny - 100.0).abs() < f64::EPSILON,
            "the arithmetic the floor exists to keep out of the ranking, was {tiny}"
        );
    }

    #[test]
    fn a_file_with_no_recorded_length_is_reported_not_divided_by() {
        // Three ways a denominator goes missing, all of which must land in
        // `unknown_length_files` rather than in the ranking with a fabricated
        // density: no `file` node at all, a `file` node with no `meta.lines`, and
        // a `lines` of zero (an empty file, or one unterminated line — a newline
        // count cannot tell those apart, so neither does this).
        let mut store = Store::open_in_memory().expect("store");
        let mut no_lines = Node::new("file:b.rs", NodeKind::File, "b.rs");
        no_lines.path = Some("b.rs".into());
        no_lines.meta = serde_json::json!({ "bytes": 90 });
        let facts = FactSet::new()
            .with_node(marker_of("orphan.rs", 1, "todo")) // roteiro:ignore
            .with_node(no_lines)
            .with_node(marker_of("b.rs", 1, "todo")) // roteiro:ignore
            .with_node(file_of("empty.rs", 0))
            .with_node(marker_of("empty.rs", 1, "todo")); // roteiro:ignore
        store.apply_factset(&facts).expect("apply");

        let report = density(&store, DensityOrder::Density);
        assert!(report.items.is_empty(), "nothing rankable: {report:?}");
        assert_eq!(report.unknown_length_files, 3);
        assert_eq!(
            report.total_markers, 3,
            "the markers are still inventoried, so the file cannot vanish silently"
        );
        assert!(
            (report.overall_per_kloc - 0.0).abs() < f64::EPSILON,
            "and no density is invented from a zero denominator"
        );
    }

    #[test]
    fn density_shares_debt_s_filters_rather_than_adding_a_second_vocabulary() {
        let store = marked();
        // The `[debt] ignore` globs `debt` already honours.
        let ignored = debt_density(
            &store,
            &[],
            &["small.rs".into()],
            DensityOrder::Density,
            0,
            super::DEFAULT_MIN_LINES,
        )
        .expect("density");
        assert!(
            !ignored.items.iter().any(|i| i.path == "small.rs"),
            "an ignored path leaves the report entirely: {:?}",
            ignored.items
        );
        assert_eq!(
            ignored.files_with_markers, 2,
            "not merely unranked — it is not in the population either"
        );

        // And the same category filter, so a `--kind stub` density is the density
        // of stubs and not of everything.
        let stubs = debt_density(&store, &["stub".into()], &[], DensityOrder::Density, 0, 0)
            .expect("density");
        assert_eq!(stubs.total_markers, 1);
        assert_eq!(
            stubs.items.iter().map(|i| &i.path).collect::<Vec<_>>(),
            ["tiny.rs"]
        );
    }

    #[test]
    fn density_ranks_on_the_exact_ratio_not_the_rounded_one() {
        // Two files whose densities differ in the fourth decimal: 1/3000 is
        // 0.3333 per kloc and 1/3001 is 0.3332. Both round to 0.33, so a ranking
        // built on `per_kloc` would tie them and break on path — putting the
        // *less* dense file first, since `a.rs` sorts before `b.rs`.
        let mut store = Store::open_in_memory().expect("store");
        let facts = FactSet::new()
            .with_node(file_of("a.rs", 3001))
            .with_node(marker_of("a.rs", 1, "todo")) // roteiro:ignore
            .with_node(file_of("b.rs", 3000))
            .with_node(marker_of("b.rs", 1, "todo")); // roteiro:ignore
        store.apply_factset(&facts).expect("apply");

        let report = density(&store, DensityOrder::Density);
        assert_eq!(
            report.items.iter().map(|i| &i.path).collect::<Vec<_>>(),
            ["b.rs", "a.rs"],
            "the shorter file is denser, however the figures round"
        );
        assert_eq!(
            (report.items[0].per_kloc, report.items[1].per_kloc),
            (0.33, 0.33),
            "and the rendered figures really are equal, so the order came from elsewhere"
        );
    }

    #[test]
    fn density_reports_truncation_and_is_deterministic() {
        let store = marked();
        let capped = debt_density(&store, &[], &[], DensityOrder::Density, 1, 0).expect("density");
        assert_eq!(capped.items.len(), 1);
        assert_eq!(capped.limit, 1);
        assert_eq!(
            capped.ranked_files, 3,
            "the population is reported, so a capped list cannot read as the whole repository"
        );
        // `overall_per_kloc` is the baseline across every ranked file, not across
        // the ones that survived the cap — otherwise the top file's own density
        // would be its own baseline.
        assert_eq!(capped.total_lines, 4210);
        assert!(
            (capped.overall_per_kloc - 19.24).abs() < f64::EPSILON,
            "81 markers over 4210 lines, was {}",
            capped.overall_per_kloc
        );

        let a = serde_json::to_string(&capped).expect("json");
        let b = serde_json::to_string(
            &debt_density(&store, &[], &[], DensityOrder::Density, 1, 0).expect("density"),
        )
        .expect("json");
        assert_eq!(a, b, "deterministic serialisation");
    }

    #[test]
    fn density_order_tokens_round_trip() {
        for token in DensityOrder::tokens() {
            let order = DensityOrder::from_token(token)
                .unwrap_or_else(|| panic!("`{token}` is advertised but not accepted"));
            assert_eq!(order.as_str(), token);
        }
        assert!(
            DensityOrder::from_token("count").is_none(),
            "an unknown order is rejected, not silently defaulted"
        );
    }

    // -- config-secret inventory (S1) --------------------------------------

    /// A `config_key` node as `extract::config_facts` emits it: `meta.value`
    /// present (already redacted, if the key name called for it).
    fn cfgkey(path: &str, dotted: &str, value: &str) -> Node {
        let mut node = Node::new(
            format!("cfgkey:{path}#{dotted}"),
            NodeKind::Other("config_key".to_owned()),
            dotted,
        );
        node.path = Some(path.to_owned());
        node.meta = serde_json::json!({ "key": dotted, "value": value });
        node
    }

    /// A **struct-derived** `config_key` node as `synthesize_config_keys` emits
    /// it: `meta.value` OMITTED, because a Rust field declares no literal value.
    fn struct_cfgkey(path: &str, dotted: &str) -> Node {
        let mut node = Node::new(
            format!("cfgkey:{path}#{dotted}"),
            NodeKind::Other("config_key".to_owned()),
            dotted,
        );
        node.path = Some(path.to_owned());
        node.meta = serde_json::json!({
            "key": dotted,
            "source": "struct",
            "struct": "AppConfig",
        });
        node
    }

    /// One of each state extraction can produce, plus a non-secret key and a
    /// k8s-`Secret`-style redaction under an innocuous name.
    fn configured() -> Store {
        let mut store = Store::open_in_memory().expect("store");
        let facts = FactSet::new()
            // Secret-named, redacted by extraction — the expected state.
            .with_node(cfgkey(".env", "API_TOKEN", "<redacted>"))
            .with_node(cfgkey("config.toml", "db.password", "<redacted>"))
            // Secret-named, struct-derived — no value to redact.
            .with_node(struct_cfgkey("src/config.rs", "serve.api_key"))
            // Not secret-named — not this lens's subject at all.
            .with_node(cfgkey("config.toml", "serve.addr", "127.0.0.1:8017"))
            // A k8s `Secret`'s data: redacted for where it lives, not what it is
            // called, so it is counted but not listed.
            .with_node(cfgkey("k8s/secret.yaml", "database-url", "<redacted>"));
        store.apply_factset(&facts).expect("apply");
        store
    }

    /// Find an item by dotted name.
    fn secret<'a>(report: &'a ConfigSecretReport, name: &str) -> &'a super::ConfigSecretItem {
        report
            .items
            .iter()
            .find(|i| i.name == name)
            .unwrap_or_else(|| panic!("`{name}` missing from {:?}", report.items))
    }

    #[test]
    fn the_inventory_reports_presence_and_redaction_not_values() {
        let report = config_secrets(&configured(), 0).expect("config_secrets");

        assert_eq!(report.config_keys, 5, "the population it drew from");
        assert_eq!(report.secret_named, 3, "{:?}", report.items);
        assert_eq!(report.files, 3);
        assert_eq!(report.schema, SCHEMA);

        // Paths, key names and state, which is what the lens is for.
        assert_eq!(secret(&report, "API_TOKEN").path.as_deref(), Some(".env"));
        assert_eq!(
            secret(&report, "db.password").key,
            "cfgkey:config.toml#db.password"
        );
        // The state comes from comparing the stored value against the redactor's
        // own constant, so asserting it is what keeps reader and writer from
        // drifting apart on a spelling.
        assert_eq!(
            secret(&report, "API_TOKEN").state,
            RedactionState::Redacted,
            "the placeholder extraction wrote is recognised as a redaction"
        );
        assert_eq!(report.redacted, 2, "{report:?}");

        // No value is carried on any item — there is no field for one. The
        // serialised shape is the contract, so assert against that, not the type.
        let json = serde_json::to_value(&report).expect("json");
        let text = serde_json::to_string(&report).expect("json");
        assert!(
            json["items"][0].get("value").is_none(),
            "an item carries no value field: {text}"
        );
        assert!(
            !text.contains("<redacted>"),
            "not even the placeholder is echoed back: {text}"
        );

        // Ordering is `(path, name, key)` — an inventory, not a ranking.
        assert_eq!(
            report.items.iter().map(|i| &i.name).collect::<Vec<_>>(),
            ["API_TOKEN", "db.password", "serve.api_key"]
        );
    }

    #[test]
    fn a_struct_declared_key_is_neither_redacted_nor_a_leak() {
        // A `@rto:config` struct field has no literal value in code, so extraction
        // omits `meta.value` entirely. Folding that in with a successful redaction
        // would claim a redaction that never happened; calling it unredacted would
        // report a leak that does not exist.
        let report = config_secrets(&configured(), 0).expect("config_secrets");
        let declared = secret(&report, "serve.api_key");
        assert_eq!(declared.state, RedactionState::Declared);
        assert_eq!(declared.source.as_deref(), Some("struct"));

        assert_eq!(report.redacted, 2, "the two file-derived keys");
        assert_eq!(report.declared, 1);
        assert_eq!(
            report.unredacted, 0,
            "the invariant extraction maintains: {report:?}"
        );
    }

    #[test]
    fn an_unredacted_secret_named_value_is_reported_as_a_finding() {
        // Extraction redacts every secret-named key, so this state is unreachable
        // from extraction — but `apply_import_layer` upserts whatever nodes an
        // imported factset carries, so another tool's import can put an unredacted
        // value in the store. That is the one path worth reporting, and it is a
        // finding about THIS STORE, not about the source repository.
        let mut store = configured();
        store
            .apply_import_layer(
                "other-tool",
                &FactSet::new().with_node(cfgkey("imported.env", "AWS_SECRET", "AKIAnot-redacted")),
            )
            .expect("import");

        let report = config_secrets(&store, 0).expect("config_secrets");
        assert_eq!(report.unredacted, 1, "{report:?}");
        assert_eq!(secret(&report, "AWS_SECRET").state, RedactionState::Present);
        // And still no value in the report: the lens says *that* something is
        // unredacted, and never repeats it.
        let text = serde_json::to_string(&report).expect("json");
        assert!(
            !text.contains("AKIA"),
            "the value is not echoed back: {text}"
        );
    }

    #[test]
    fn a_redaction_under_an_innocuous_name_is_counted_but_not_listed() {
        // A k8s `Secret`'s `data` is redacted because of where it lives, whatever
        // the key is called. It is not secret-*named*, so it is not this lens's
        // subject — but it is counted, so a reader comparing `redacted` against the
        // number of `<redacted>` values in the graph does not find a surplus they
        // cannot explain.
        let report = config_secrets(&configured(), 0).expect("config_secrets");
        assert_eq!(report.redacted_not_secret_named, 1);
        assert!(
            !report.items.iter().any(|i| i.name == "database-url"),
            "not listed: {:?}",
            report.items
        );
        assert_eq!(
            report.redacted + report.redacted_not_secret_named,
            3,
            "and the two figures together account for every redacted value"
        );
    }

    #[test]
    fn the_inventory_cannot_see_a_credential_that_is_not_a_config_key() {
        // The load-bearing limitation, asserted rather than only documented: a
        // credential in a Rust string literal produces no `config_key` node, so it
        // is invisible here. No extension of this lens can change that — which is
        // why it is named for the inventory it is, not the scanner it is not.
        let mut store = configured();
        let mut hardcoded = Node::new("sym:rust:src/main.rs#connect", NodeKind::Fn, "connect");
        hardcoded.path = Some("src/main.rs".into());
        // Split at the prefix for the same reason as `FAKE_TOKEN` in
        // `roteiro/tests/config_secrets_cli.rs`: assembled, this is AWS's own
        // documentation placeholder, but it matches the canonical access-key-id
        // rule exactly and a regex-rule scanner cannot know the difference. The
        // assembled value is unchanged; no assertion here matches on its text.
        hardcoded.meta = serde_json::json!({
            "content": concat!("let token = \"AKIA", "IOSFODNN7EXAMPLE\";"),
        });
        store
            .apply_factset(&FactSet::new().with_node(hardcoded))
            .expect("apply");

        let report = config_secrets(&store, 0).expect("config_secrets");
        assert_eq!(
            report.secret_named, 3,
            "a hardcoded credential does not appear: {:?}",
            report.items
        );
        assert_eq!(report.config_keys, 5, "and is not a config key at all");
    }

    #[test]
    fn the_inventory_reports_truncation_and_is_deterministic() {
        let store = configured();
        let capped = config_secrets(&store, 1).expect("config_secrets");
        assert_eq!(capped.items.len(), 1);
        assert_eq!(capped.limit, 1);
        assert_eq!(
            capped.secret_named, 3,
            "the population is reported, so a capped list cannot read as a clean repository"
        );
        // The state counts are over the whole population too, not the shown rows —
        // otherwise a cap could hide an `unredacted` finding.
        assert_eq!((capped.redacted, capped.declared), (2, 1));

        let a = serde_json::to_string(&capped).expect("json");
        let b = serde_json::to_string(&config_secrets(&store, 1).expect("config_secrets"))
            .expect("json");
        assert_eq!(a, b, "deterministic serialisation");
    }

    #[test]
    fn an_empty_report_means_no_secret_named_key_not_no_secret() {
        // The distinction the lens must never blur: a credential under an
        // innocuous key name (`dsn`) is not secret-named, is not redacted, and does
        // not appear. So "nothing found" is a statement about naming.
        let mut store = Store::open_in_memory().expect("store");
        store
            .apply_factset(&FactSet::new().with_node(cfgkey(
                ".env",
                "DSN",
                "postgres://u:pw@host/db",
            )))
            .expect("apply");

        let report = config_secrets(&store, 0).expect("config_secrets");
        assert_eq!(report.secret_named, 0, "nothing is secret-*named*");
        assert_eq!(report.redacted_not_secret_named, 0);
        assert_eq!(
            report.config_keys, 1,
            "while the graph does hold a config key with a credential in it"
        );
    }

    #[test]
    fn redaction_state_tokens_match_their_serialisation() {
        // The token and the wire form are the same string, so a caller matching on
        // the JSON and a caller matching on `as_str` cannot disagree.
        for state in [
            RedactionState::Redacted,
            RedactionState::Declared,
            RedactionState::Present,
        ] {
            let json = serde_json::to_string(&state).expect("json");
            assert_eq!(json, format!("\"{}\"", state.as_str()));
        }
    }
}

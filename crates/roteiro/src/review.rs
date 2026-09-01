//! `roteiro review`: graph-grounded review context for the current change
//! (Stage 17). The CLI-first surface for a context-aware review — a human or an
//! agent can see, for the working-tree change, *what the graph knows* about each
//! touched symbol (who calls it, what governs it, what it's related to), the
//! authored-layer drift the change introduces, the intent-debt it adds, and the
//! blast radius of dependents to check — rather than reviewing the diff in
//! isolation. The MCP `explain`/`path`/`debt` tools expose the same graph as a
//! bonus; this command needs no server.

use std::collections::BTreeSet;
use std::path::Path;

use rto_graph::{NodeContext, NodeKind, Store, StoreError, build_context, debt, dependents};
use serde::Serialize;

/// Schema tag for the `--json` review report.
pub const REVIEW_SCHEMA: &str = "roteiro.review/v1";

/// A graph-grounded review of the working-tree change.
#[derive(Debug, Serialize)]
pub struct ReviewReport {
    /// Stable schema tag.
    pub schema: &'static str,
    /// **What `--base` actually compared against** (issue #649), or `None` for a
    /// working-tree review, which compares against `HEAD` and has no spec to
    /// resolve.
    ///
    /// Without this there was no field a reader could consult to tell a review
    /// measured against a seventeen-commit-stale `main` from a correct one: the
    /// two rendered identically, both with `drift: []` and exit 0. A report that
    /// cannot say what it compared against is insufficient in exactly the way a
    /// report without the diff was — the reader cannot reconstruct the question
    /// from the answer.
    ///
    /// Additive within `roteiro.review/v1` — see `docs/JSON_SCHEMA.md`, which
    /// permits new fields within a major version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<rto_graph::BaseResolution>,
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
    /// The change itself, as a unified diff (issue #649).
    ///
    /// `None` when no diff was requested or git would not produce one; `Some("")`
    /// when git ran and emitted nothing for this path. The two are not the same
    /// fact and are not rendered the same way.
    ///
    /// Mode changes, renames and binary files are **not** the empty case, though
    /// they read like it should be: `git diff -U3` emits headers for all three
    /// (`old mode`/`new mode`, `similarity index`/`rename from`, `Binary files …
    /// differ`), so they arrive as ordinary non-empty diffs. Measured rather than
    /// assumed — 57, 79 and 97 bytes respectively on a one-file fixture.
    ///
    /// What is left for `Some("")` is a genuinely empty new file, or a path whose
    /// content already matches the range's base.
    ///
    /// Additive within `roteiro.review/v1` — see `docs/JSON_SCHEMA.md`, which
    /// permits new fields within a major version. A consumer that has never
    /// heard of this one is unaffected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
}

/// Where [`build`] should read diff text from.
///
/// Passing `None` for this is what "review without the diff" means; the graph
/// arm behaves exactly as it did before issue #649.
#[derive(Debug, Clone, Copy)]
pub struct DiffSource<'a> {
    /// The repository to run git in.
    pub repo: &'a Path,
    /// The range's base, or `None` for a working-tree review.
    pub base: Option<&'a str>,
}

impl DiffSource<'_> {
    /// The unified diff for one changed file, or `None` if git would not answer.
    ///
    /// An **added** file needs care. In a working-tree review it may be
    /// untracked, and plain `git diff` cannot see an untracked path at all — it
    /// reports success and no text, which is indistinguishable from an
    /// unchanged file. So an empty answer for an addition is retried against
    /// `/dev/null`, which is the only way a brand-new file's contents reach the
    /// reviewer. Getting this wrong is quiet: the file is listed, its symbols
    /// are listed, and the one thing new about it is missing.
    fn for_file(self, cf: &rto_graph::ChangedFile) -> Option<String> {
        let range: Vec<&str> = match self.base {
            Some(b) => vec![b, "HEAD"],
            None => vec!["HEAD"],
        };
        let text = crate::diff::unified(self.repo, &range, &cf.path);
        if cf.status == rto_graph::ChangeStatus::Added
            && self.base.is_none()
            && text.as_deref().is_none_or(str::is_empty)
        {
            return crate::diff::unified_untracked(self.repo, &cf.path);
        }
        text
    }
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
/// change produced. `ignore` is the repository's `[debt] ignore` list (ADR-0007),
/// threaded from `main` exactly as `debt`, `check`, `render` and the graph API
/// thread it.
///
/// # Why the marker set comes from [`debt`] rather than from this walk
///
/// A review's per-file `debt` is the *same concept* `roteiro debt` reports, so it
/// must be the same marker set — issue #409 was this file walking
/// [`Store::nodes_by_path`] and keeping every `Marker` node it found, which made
/// `review` report debt in files every other surface excludes. Threading the
/// `ignore` list in and re-applying the globs here would fix that instance and
/// leave the next one available: two copies of "which markers count" that have to
/// be kept in step by whoever edits either.
///
/// So the decision is not re-made. [`debt`] is asked which markers exist under
/// this repository's exclusions, and this walk keeps only those — the same reason
/// [`rto_graph::debt_density`] is built from [`debt`]'s output instead of
/// re-walking the markers itself. Adding an exclusion rule to [`debt`] reaches
/// this surface with no edit here; there is no second filter to forget.
///
/// The `String`s pushed are still the marker nodes' names, not [`debt`]'s item
/// text: [`debt`] decides *which* markers a surface may report, not how each one
/// is rendered.
///
/// # Errors
/// Returns [`StoreError`] on a store query failure.
pub fn build(
    store: &Store,
    changed: &[rto_graph::ChangedFile],
    violations: &[rto_spec::Violation],
    ignore: &[String],
    diff: Option<DiffSource<'_>>,
    base: Option<rto_graph::BaseResolution>,
) -> Result<ReviewReport, StoreError> {
    let changed_paths: BTreeSet<&str> = changed.iter().map(|c| c.path.as_str()).collect();
    // Every marker `debt` retains under this repository's `[debt] ignore`, keyed
    // for lookup. Whole-graph rather than per-file because `debt` is the surface
    // that owns the question; a changed file's markers are a subset of it.
    let inventory = debt(store, &[], ignore)?;
    let retained: BTreeSet<&str> = inventory.items.iter().map(|i| i.key.as_str()).collect();
    let mut files = Vec::new();
    let mut changed_keys: Vec<String> = Vec::new();

    for cf in changed {
        if cf.status == rto_graph::ChangeStatus::Deleted {
            files.push(FileReview {
                path: cf.path.clone(),
                status: "deleted",
                symbols: Vec::new(),
                debt: Vec::new(),
                // A deletion has a diff worth reading — it is the removed code,
                // and it is the only place a reviewer can see what was lost. The
                // graph cannot show it: the nodes are gone.
                diff: diff.and_then(|d| d.for_file(cf)),
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
                    // Not `debt.push(...)` unconditionally: a marker `debt`
                    // dropped is one this repository has excluded, and a review
                    // that reported it would be the second, unreconcilable debt
                    // figure ADR-0007 v1.1 exists to prevent.
                    if retained.contains(node.key.as_str()) {
                        debt.push(node.name.clone());
                    }
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
            diff: diff.and_then(|d| d.for_file(cf)),
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
        base,
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

/// Score a candidate reviewer's run against the adjudicated corpus and print the
/// result (Stage 35).
///
/// Needs no graph, no model and no network: the corpus is embedded and the scoring
/// is pure. That is what makes it usable as a regression gate on a reviewer — the
/// numbers can be recomputed on any machine, including CI.
///
/// # Errors
/// If the run document cannot be read or scored, or the corpus override cannot be
/// read or parsed.
pub fn run_score(run_path: &str, corpus_path: Option<&str>, json: bool) -> anyhow::Result<()> {
    use rto_graph::review_corpus::Corpus;
    use rto_graph::review_score::{CandidateRun, score};

    let corpus = match corpus_path {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .map_err(|e| anyhow::anyhow!("reading corpus {path}: {e}"))?;
            Corpus::parse(&text).map_err(|e| anyhow::anyhow!("{path}: {e}"))?
        }
        None => rto_graph::review_corpus::builtin()?,
    };
    let text = std::fs::read_to_string(run_path)
        .map_err(|e| anyhow::anyhow!("reading run {run_path}: {e}"))?;
    let run = CandidateRun::parse(&text).map_err(|e| anyhow::anyhow!("{run_path}: {e}"))?;
    let scored = score(&corpus, &run)?;

    if json {
        crate::emit_json(&scored)?;
    } else {
        print_score(&scored);
    }
    Ok(())
}

/// Render a score as a per-class table.
///
/// The table is the report. A single headline number is deliberately absent: the
/// question an implementer is asking is *which* defect classes a reviewer can see,
/// and a mean over the classes — most of which hold a single row — answers a
/// question nobody asked while hiding the one they did. The exact denominators are
/// printed beside each rate rather than stated here, so this comment cannot go
/// stale as rows are added.
fn print_score(score: &rto_graph::review_score::Score) {
    println!(
        "scored {} of {} corpus commit(s)",
        score.attempted_shas, score.corpus_shas
    );
    println!("\nrecall by defect class (found/real):");
    for class in &score.per_class {
        if class.real == 0 {
            continue;
        }
        let rate = match class.recall() {
            Some(r) => format!("{:>4.0}%", r * 100.0),
            None => "   —".to_owned(),
        };
        // `n=1` is printed beside the rate, not left to the caveats, because a
        // reader scanning a column of percentages will otherwise compare 0% of one
        // row against 40% of five as though they weighed the same.
        let weight = if class.real == 1 { "  (n=1)" } else { "" };
        println!(
            "  {rate}  {:>2}/{:<2}  {}{weight}",
            class.found,
            class.real,
            class.class.as_str()
        );
        if class.misclassified > 0 {
            println!(
                "          {} found but labelled as another class",
                class.misclassified
            );
        }
        // The misses are the actionable half of a recall figure — "0/1
        // `cleanup-gap`" tells nobody what to look at. Printed with the anchor and
        // the permalink so the next step is opening a file, not grepping a fixture.
        for miss in &class.missed {
            println!(
                "          miss {}:{} — {}",
                miss.path, miss.line, miss.description
            );
            println!("               {}", miss.comment_url);
        }
    }
    println!(
        "\n{}/{} real defect(s) found; {}/{} known-false claim(s) reproduced",
        score.found, score.real_in_scope, score.known_false_reproduced, score.known_false_in_scope
    );
    // Printed on the same line of sight as the recall it qualifies, never below
    // the caveats: a chance baseline a reader has to scroll to is one they will
    // quote the number without.
    if let Some(expected) = score.expected_by_position {
        println!(
            "  chance baseline: ~{expected:.1} would match by position alone              (see the caveat below)"
        );
    }
    match score.corpus_precision() {
        Some(p) => println!(
            "precision over adjudicated findings only: {:.0}% \
             ({} unadjudicated finding(s) excluded — see below)",
            p * 100.0,
            score.unadjudicated
        ),
        None => println!(
            "precision: not computable — no finding matched an adjudicated row \
             ({} unadjudicated)",
            score.unadjudicated
        ),
    }
    // The whole-change verdicts (#649, part 2), below the findings because they
    // are a different question and must not be read as part of the recall figure.
    // Printed even at zero when the run carried any, so a reader can tell "no
    // verdict was contradicted" from "this run had no verdicts to check".
    if score.verdicts > 0 {
        println!(
            "\n{} whole-change verdict(s) — a model's opinion, gating nothing:",
            score.verdicts
        );
        println!(
            "  {} declared a change CLEAN that the corpus knows carries a real defect",
            score.verdicts_contradicted
        );
        println!(
            "  {} the corpus cannot judge (every `concerns` verdict, and `clean` on \
             a commit with no real row)",
            score.verdicts_unadjudicated
        );
    }
    if score.suppressed_real + score.suppressed_known_false + score.suppressed_unadjudicated > 0 {
        println!(
            "suppression filter withheld: {} real, {} known-false, {} unadjudicated",
            score.suppressed_real, score.suppressed_known_false, score.suppressed_unadjudicated
        );
    }
    let caveats = score.caveats();
    if !caveats.is_empty() {
        println!("\nread these numbers with:");
        for caveat in &caveats {
            println!("  - {caveat}");
        }
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
        assert_eq!(
            rto_graph::review_score::SCORE_SCHEMA,
            "roteiro.review-score/v1"
        );
        assert_eq!(rto_graph::review_score::RUN_SCHEMA, "roteiro.review-run/v1");
        assert_eq!(rto_graph::SCHEMA, "roteiro.query/v1");
        assert_eq!(rto_graph::ARTIFACT_SCHEMA, "roteiro.graph/v1");
        assert_eq!(rto_graph::ORACLE_SCHEMA, "roteiro.oracle/v1");
        assert_eq!(rto_spec::SPEC_SCHEMA, "roteiro.spec/v1");
        assert_eq!(rto_spec::TOOL_CHECK_SCHEMA, "roteiro.check/v1");
    }
}

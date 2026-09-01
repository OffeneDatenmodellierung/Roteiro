//! The LLM reviewer's driver: the loop that calls a model, and the replay that
//! measures one against the adjudicated corpus (Stage 35b).
//!
//! The reviewer's *judgement* — what to ask, how to read the answer, what a
//! compile claim requires — is [`rto_graph::reviewer`], where it is pure and
//! tested with no model. What is here is everything that touches the world: an
//! engine, git, and a file to write.
//!
//! # Two surfaces, and only one of them is an experiment
//!
//! [`run_llm`] is the shipped surface: review the change in front of you.
//! [`run_replay`] is the harness that makes the reviewer a number — it
//! reconstructs each commit the corpus adjudicated, reviews every file that
//! commit touched, and writes a `roteiro.review-run/v1` document for
//! `roteiro review --score`. They share one per-file path, so the thing measured
//! is the thing that ships.
//!
//! # Reconstructing the reviewed tree
//!
//! The corpus keys on each comment's `reviewed_sha`, and getting the base wrong
//! yields a silent zero from either direction: the merged PR head contains the
//! *fix* commits, and the obvious `merge-base main <sha>` yields an **empty
//! diff** for 13 of the 15 review commits, because a merged branch is an ancestor
//! of `main`. [`fork_point`] implements the corrected recipe — find the merge that
//! brought the branch in, and diff from its first parent's merge base — and
//! `every_corpus_commit_reconstructs_a_diff_touching_its_anchor` holds it to every
//! row. The fixture README states the same rule in prose; the two agree because
//! this test and `rto-graph`'s assert the same property against the same data.
//!
//! # Local only, and not by omission
//!
//! [`rto_graph::ModelTask::Review`] reports `goes_remote() == true`: it is a
//! command-level generative surface, which is what ADR-0019 §3 asks. But a remote
//! review would have to send the diff, and ADR-0019 §4's payload allow-list
//! carries node identities and prose — never source. That needs a new
//! allow-listed field, which is an ADR amendment, not a flag. So there is no
//! `--allow-remote` here, and a test says so rather than leaving its absence to
//! read as an oversight.

use std::path::Path;
use std::process::Command;

// The one definition of "run git here" (issue #649): this module used to carry
// its own copy, and the graph arm could not reach it behind this module's
// feature gate.
use crate::diff::git;

use rto_graph::reviewer::FileUnderReview;

/// The half of this module that needs a generation backend. Everything outside
/// it — the diff reconstruction and the parent-module lookup — is useful and
/// tested in a build with no model, which is the build CI runs.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
use {
    rto_graph::compile_claim::{CheckRun, suppression},
    rto_graph::review_score::{CandidateFinding, CandidateRun},
    rto_graph::reviewer::{SINGLE_CALL_BUDGET_TOKENS, build_prompt, claim_site, parse_findings},
};

use rto_graph::reviewer::GraphContext;
use std::collections::BTreeSet;

/// Tokens a review of one file may generate.
///
/// Generous relative to `spec draft`'s 800, and **raised from 1,200 by
/// measurement**. A file with several findings needs a line each, and a reply cut
/// off mid-list would be scored as the reviewer having found fewer defects than it
/// did — measuring the cap rather than the model.
///
/// 1,200 was not merely tight; it was silently wrong for a whole class of model,
/// and finding that out is the most useful thing this stage has produced.
/// `qwen3.8-27b` is a reasoning GGUF: on the held-out commit it spent the entire
/// budget inside `<think>` on **4 files of 4**, so the run reported *"0 finding(s)
/// over 4 file(s)"* — a clean-looking result in which **no review had happened at
/// all**. Scored, that would have read as zero recall and been reported as an
/// honest negative about local reviewers.
///
/// [`rto_graph::reviewer::Parsed::reasoning_truncated`] is how that is now caught
/// rather than believed, and a truncated file is reported as unreviewed rather
/// than counted as clean.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
const REVIEW_MAX_TOKENS: u32 = 4_096;

/// The context window this reviewer asks llama.cpp for.
///
/// **Passed explicitly, and the first version of this code did not.**
/// [`rto_llama::llama::LlamaEngine::new`] takes `n_ctx` and reads `0` as its
/// default of **4,096** — which `spec draft` passes, because a drafted section is
/// small. A reviewer handed a whole file's diff is not: the first replay run died
/// on file two with *"prompt is 4111 tokens, over the 4096-token limit"*. The
/// model itself has a very large window; 4,096 was Roteiro's number, not Qwen's.
///
/// So the budget analysis and the engine have to agree by construction rather
/// than by coincidence, and this is sized from the other two constants with slack
/// for the estimate being an estimate:
/// [`rto_graph::reviewer::estimate_tokens`] is `len / 4`, deliberately not a
/// tokeniser's count, and code tokenises **denser** than the prose that ratio
/// comes from — so the true count of a prompt this module believes is 30k can be
/// materially higher. The slack absorbs that; `the_context_window_holds_the_whole_budget`
/// holds the arithmetic.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
const REVIEW_N_CTX: u32 = 49_152;

/// The estimated-token budget for one file's prompt.
///
/// 35a's measured single-call figure. Kept as the budget rather than lowered to
/// hide the estimate's imprecision: the slack belongs in [`REVIEW_N_CTX`], where
/// it is visible, not in a quietly smaller budget that would truncate files the
/// measurement says fit.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
const REVIEW_PROMPT_BUDGET: usize = SINGLE_CALL_BUDGET_TOKENS;

/// The window must hold the prompt *and* what is generated into it, with room for
/// `len / 4` to have understated the prompt. A build that broke this relationship
/// would fail per file at run time, on whichever file happened to be largest.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
const _: () = assert!(
    REVIEW_N_CTX as usize >= (REVIEW_PROMPT_BUDGET * 13 / 10) + REVIEW_MAX_TOKENS as usize,
    "REVIEW_N_CTX must hold a 30%-underestimated prompt plus the generation"
);

/// Which context the reviewer is given — **the one variable Stage 35b PR 2
/// varies**.
///
/// Both arms share every other input: the same model, the same corpus, the same
/// reconstruction, the same prompt scaffolding, the same commit of this binary.
/// That is the whole design; a comparison in which anything else moved would
/// measure the something else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewArm {
    /// No context at all — [`GraphContext::none`]. The baseline PR 1 shipped no
    /// figure for, and the thing the graph arm has to beat.
    DiffOnly,
    /// Governing ADRs and the file's own out-of-diff doc surface, assembled by
    /// [`graph_context_for`] from the graph **at the reviewed commit**.
    Graph,
}

impl ReviewArm {
    /// The tag written into [`rto_graph::review_score::RunArm::context`].
    #[must_use]
    pub fn tag(self) -> &'static str {
        match self {
            Self::DiffOnly => "diff-only",
            Self::Graph => "graph",
        }
    }
}

/// One file's review, before scoring.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
pub struct FileOutcome {
    /// Findings the reviewer stands behind.
    pub findings: Vec<CandidateFinding>,
    /// Compile claims withheld under [`rto_graph::compile_claim`], with the job
    /// that refuted each.
    pub suppressed: Vec<(CandidateFinding, String)>,
    /// Lines that looked like findings but carried no usable anchor.
    pub unparsed: usize,
    /// Whether the model declared the file clean in the required form.
    pub declared_clean: bool,
    /// The generation stopped inside a reasoning block, so this file was never
    /// actually reviewed — reported, never counted as a clean pass.
    pub reasoning_truncated: bool,
    /// Diff tokens dropped to fit the budget.
    pub dropped_tokens: usize,
}

/// Review one file with `engine`, applying the compile-claim filter against
/// `checks`.
///
/// `checks` is evidence the caller supplies; with none, nothing is suppressed —
/// [`rto_graph::compile_claim`] is opt-in on evidence, so a caller that cannot
/// reach CI loses the filter rather than gaining a blanket suppression.
///
/// # Errors
/// If the engine fails to generate.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
pub fn review_file(
    engine: &rto_llama::llama::LlamaEngine,
    model: &str,
    file: &FileUnderReview,
    context: &GraphContext,
    checks: &[CheckRun],
    sources: &dyn Fn(&str) -> Option<String>,
) -> anyhow::Result<FileOutcome> {
    use rto_llama::Engine as _;

    let prompt = build_prompt(file, context, REVIEW_PROMPT_BUDGET);
    let completion = engine
        .chat(&rto_llama::ChatRequest {
            tools: None,
            model: model.to_owned(),
            messages: vec![rto_llama::Message {
                role: "user".to_owned(),
                content: prompt.text,
            }],
            images: vec![],
            audio: vec![],
            temperature: 0.0,
            max_tokens: REVIEW_MAX_TOKENS,
        })
        .map_err(|e| anyhow::anyhow!("reviewing {}: {e}", file.path))?;
    let read = rto_llama::thinking::answer(&completion.content, completion.finish_reason);
    // The one way to tell a reviewer that found nothing from a reviewer that was
    // asked the wrong question. Prompt work is otherwise done by staring at a
    // score, which moves for both reasons at once. Shows the raw generation when
    // there was no answer in it, because the deliberation is the only evidence
    // there is about why.
    if std::env::var_os("ROTEIRO_REVIEW_DEBUG").is_some() {
        eprintln!(
            "--- {} ({} prompt tokens est.)\n{}\n---",
            file.path,
            prompt.tokens,
            read.unwrap_or(&completion.content)
        );
    }
    // **A file this model never got to is reported, never counted.** An
    // unterminated `<think>` block means the generation stopped mid-deliberation,
    // so this file was not reviewed — and the old stripper handed the raw block
    // to `parse_findings`, which is the one thing `strip_thinking`'s own doc
    // comment said must not happen: *"a reviewer that parsed a model's `<think>`
    // block would read its scratch reasoning as findings"* (#583).
    //
    // It returns `Ok` rather than an error because a truncated review is an
    // outcome about one file, not a failed run: `reasoning_truncated` exists so a
    // run that cannot tell "found nothing" from "never answered" stops reporting
    // a recall figure it did not measure. `parse_findings` keeps its own
    // `contains("<think>")` check, which now only fires on a block opened
    // mid-reply — belt and braces over the same fact, arriving by a route this
    // one deliberately does not claim.
    let Ok(reply) = read else {
        return Ok(FileOutcome {
            findings: Vec::new(),
            suppressed: Vec::new(),
            unparsed: 0,
            declared_clean: false,
            reasoning_truncated: true,
            dropped_tokens: prompt.dropped_tokens,
        });
    };
    let parsed = parse_findings(&file.reviewed_sha, &file.path, reply);

    let mut findings = Vec::new();
    let mut withheld = Vec::new();
    for finding in parsed.findings {
        if !finding.claims_compile_failure || checks.is_empty() {
            findings.push(finding);
            continue;
        }
        // The site is derived from the reviewed tree, not from the diff: whether
        // the code is macOS-gated, feature-gated or test code is a property of
        // the file, and the diff shows only what changed in it.
        let source = sources(&file.path).unwrap_or_default();
        let parent = parent_module_source(&file.path, sources);
        let site = claim_site(
            &file.reviewed_sha,
            &file.path,
            finding.line,
            &source,
            parent.as_deref(),
        );
        let verdict = suppression(&site, checks);
        if verdict.is_refuted() {
            withheld.push((finding, verdict.reason().to_owned()));
        } else {
            findings.push(finding);
        }
    }

    Ok(FileOutcome {
        findings,
        suppressed: withheld,
        unparsed: parsed.unparsed.len(),
        declared_clean: parsed.declared_clean,
        reasoning_truncated: parsed.reasoning_truncated,
        dropped_tokens: prompt.dropped_tokens,
    })
}

/// The graph as it was at `sha`, for the graph arm — or `None` for the diff-only
/// arm, which is the absence of a store rather than an empty one.
///
/// In memory, and one per commit: the graph arm needs the repository as it was
/// when the code was written, and writing that to the developer's own store would
/// leave their graph rebuilt at a historical commit after the run.
///
/// # Errors
/// If the graph at `sha` cannot be assembled.
#[cfg(any(feature = "serve", feature = "inference-local-models", test))]
fn graph_at(
    repo: &rto_graph::Repo,
    cache: &rto_graph::ObjectCache,
    ingest: rto_graph::IngestConfig,
    arm: ReviewArm,
    sha: &str,
) -> anyhow::Result<Option<rto_graph::Store>> {
    if arm == ReviewArm::DiffOnly {
        return Ok(None);
    }
    let mut store = rto_graph::Store::open_in_memory()?;
    crate::build_graph_at_rev(repo, &mut store, cache, ingest, sha)?;
    Ok(Some(store))
}

/// The context for one file, from an optional graph — the single place both
/// surfaces turn an arm into a [`GraphContext`].
///
/// [`ReviewArm::DiffOnly`] is `None` and yields [`GraphContext::none`], so the
/// baseline is the absence of a store rather than a store that happened to answer
/// nothing. The two are indistinguishable in the prompt and very distinguishable
/// in what they mean about a run.
#[cfg(any(feature = "serve", feature = "inference-local-models", test))]
fn context_for(
    graph: Option<&rto_graph::Store>,
    file: &FileUnderReview,
    sources: &dyn Fn(&str) -> Option<String>,
) -> anyhow::Result<GraphContext> {
    match graph {
        None => Ok(GraphContext::none()),
        Some(store) => {
            let annotated = rto_graph::reviewer::annotate_diff(&file.diff);
            graph_context_for(store, file, &annotated, sources)
        }
    }
}

/// The graph of the **working tree** — `HEAD` plus uncommitted edits — for the
/// live `review --llm` surface.
///
/// The same assembly `review` and `check` already build, and the same one
/// [`crate::build_graph_at_rev`] performs at a historical commit: derived layer,
/// then the authored layer over the identical file set. A live surface reviewing
/// against a different graph than the measured one would make the replay's number
/// a claim about something users do not run.
///
/// # Errors
/// If the repository or the graph cannot be assembled.
#[cfg(any(feature = "serve", feature = "inference-local-models", test))]
fn worktree_graph(
    repo: &Path,
    ingest: rto_graph::IngestConfig,
) -> anyhow::Result<rto_graph::Store> {
    let graph_repo = rto_graph::Repo::discover(repo)?;
    let cache =
        rto_graph::ObjectCache::open(graph_repo.common_dir().join("roteiro").join("objects"))?;
    let mut store = rto_graph::Store::open_in_memory()?;
    let registry = rto_graph::Registry::new(ingest);
    rto_graph::sync_worktree(&mut store, &graph_repo, &cache, &registry)?;
    crate::apply_authored_layer(&mut store, graph_repo.walk_blobs()?, &|blob| {
        Ok(graph_repo
            .workdir()
            .and_then(|w| std::fs::read(w.join(&blob.path)).ok()))
    })?;
    Ok(store)
}

/// Assemble the graph arm's context for one file (Stage 35b PR 2).
///
/// `store` must hold the graph **at the reviewed commit** — see
/// [`crate::build_graph_at_rev`] for why reviewing a commit against `HEAD`'s ADRs
/// would be a silent wrong answer of the same family as scoring against a PR head.
/// `markdown_at` reads a repository file's text at that same commit.
///
/// # What is selected, in priority order
///
/// 1. **Governing ADR and blueprint sections** (`authored`). The one thing a
///    per-file reviewer structurally cannot obtain, and `contract-drift`'s
///    defining shape.
/// 2. **Doc comments from elsewhere in this file** (`derived`), and only those the
///    diff does not already show — see [`doc_already_shown`].
///
/// Callers, callees and blast radius are excluded by decision, not omission; the
/// reasoning is on [`GraphContext`].
///
/// The order matters because [`GraphContext::fit`] drops from the tail: under
/// pressure a file keeps its governing decision and loses a doc comment, which is
/// the way round that preserves what the arm is testing.
///
/// # Two measured limits on what this can ever supply
///
/// Both were found by running it over the corpus, and both bound the arm's power
/// independently of any model:
///
/// * **An ADR under review gets nothing.** The graph stores an `adr_section` node
///   per heading with **no body**, and no authored edge points *into* one — so a
///   file whose own nodes are `adr_section`s has neither a governing decision to
///   fetch nor a doc comment to quote. That is precisely the shape of the
///   corpus's clearest ADR-drift row (frontmatter bumped to 1.3 while the summary
///   table below still says 1.2): both halves are in the ADR, one is outside the
///   `-U3` window, and the graph cannot reach either.
/// * **A newly added file gets nothing, correctly.** Its whole text is already in
///   the diff, so [`doc_already_shown`] filters every doc comment, and nothing
///   governs a file that did not exist at the fork point. Three of the corpus's
///   five `contract-drift` rows sit in files like this, which means the arm's
///   prompt on them is byte-identical to the diff-only arm's and no run can
///   separate the two.
///
/// Neither is a defect in this function. They are the honest ceiling on the
/// experiment, and they are why the measurement is reported as a bound rather
/// than as a difference between two scores.
///
/// # Errors
/// If the store cannot be queried.
#[cfg(any(feature = "serve", feature = "inference-local-models", test))]
pub fn graph_context_for(
    store: &rto_graph::Store,
    file: &FileUnderReview,
    annotated_diff: &str,
    markdown_at: &dyn Fn(&str) -> Option<String>,
) -> anyhow::Result<GraphContext> {
    use rto_graph::reviewer::{ContextItem, doc_already_shown, section_body};
    use rto_graph::{NodeKind, Provenance};

    let symbols: Vec<rto_graph::Node> = store
        .nodes_by_path(&file.path)?
        .into_iter()
        // The file node carries no contract, and a marker is intent debt rather
        // than a promise about behaviour.
        .filter(|n| !matches!(n.kind, NodeKind::File | NodeKind::Marker))
        .collect();

    // Governing sections, and which symbols each governs. A `BTreeMap` because two
    // symbols in a file commonly share one ADR, and because the run must be
    // reproducible: an arm whose context order varied between runs would make the
    // repeat-run variance check measure the assembler instead of the model.
    let mut governing: std::collections::BTreeMap<String, BTreeSet<String>> =
        std::collections::BTreeMap::new();
    for sym in &symbols {
        for edge in store.edges_to(&sym.key)? {
            if edge.provenance == Provenance::Authored {
                governing
                    .entry(edge.src.clone())
                    .or_default()
                    .insert(sym.name.clone());
            }
        }
    }

    let mut items = Vec::new();
    for (section_key, governed) in governing {
        let Some(node) = store.get_node(&section_key)? else {
            continue;
        };
        let Some(path) = node.path.as_deref() else {
            continue;
        };
        let Some(markdown) = markdown_at(path) else {
            continue;
        };
        // No body means the heading the graph recorded is not in the file at this
        // commit. Skipped rather than emitted empty: an item that says an ADR
        // governs this code and then quotes nothing is worse than its absence.
        let Some(body) = section_body(&markdown, &node.name) else {
            continue;
        };
        if body.is_empty() {
            continue;
        }
        let governed: Vec<&str> = governed.iter().map(String::as_str).collect();
        items.push(ContextItem {
            label: format!(
                "{} \u{a7}{} ({}) \u{2014} governs {}",
                node.key.split('#').next().unwrap_or(&node.key),
                node.name,
                path,
                governed.join(", ")
            ),
            provenance: "authored".to_owned(),
            body,
        });
    }

    for sym in &symbols {
        let Some(doc) = sym.meta.get("content").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if doc_already_shown(doc, annotated_diff) {
            continue;
        }
        items.push(ContextItem {
            label: format!(
                "doc comment of {} `{}` \u{2014} elsewhere in this file, not in the diff",
                sym.kind.as_str(),
                sym.name
            ),
            provenance: "derived".to_owned(),
            body: doc.to_owned(),
        });
    }

    Ok(GraphContext::fit(
        items,
        rto_graph::reviewer::estimate_tokens(annotated_diff),
    ))
}

/// The source of the module that declares `path`, where a file's feature gate is
/// written — `src/foo.rs`'s parent is `src/lib.rs` (or `src/main.rs`), and
/// `src/a/b.rs`'s is `src/a/mod.rs` or `src/a.rs`.
///
/// # `mod.rs` is declared one directory up
///
/// `src/a/b/mod.rs` is declared by `mod b;` in `src/a`, not in `src/a/b` — so for
/// a `mod.rs` the search starts from the grandparent directory. Searching its own
/// directory finds nothing (`src/a/b/b.rs` cannot exist alongside it, and
/// `src/a/b/lib.rs` is not a thing), which returns `None`; and `None` on the
/// features axis reads as *unconditional* in [`claim_site`]. Getting this wrong
/// is therefore permissive, not merely lossy, which is why it is handled here
/// rather than left to the candidate list to stumble onto.
fn parent_module_source(path: &str, sources: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let dir = match path.rsplit_once('/')? {
        (d, "mod.rs") => d.rsplit_once('/').map_or(d, |(up, _)| up),
        (d, _) => d,
    };
    for candidate in [
        format!("{dir}/mod.rs"),
        format!("{dir}/lib.rs"),
        format!("{dir}/main.rs"),
        format!("{dir}.rs"),
    ] {
        if candidate == path {
            continue;
        }
        if let Some(text) = sources(&candidate) {
            return Some(text);
        }
    }
    None
}

/// The commit a review commit's branch forked from — **the corrected
/// reconstruction recipe**.
///
/// `merge-base <main> <sha>` is wrong for a merged branch: the review commit is
/// an ancestor of `main`, so the merge base is the review commit itself and the
/// diff is empty. The base wanted is where the branch forked, found by locating
/// the merge `M` that brought it in (`sha` is an ancestor of `M^2` and not of
/// `M^1`) and taking `merge-base M^1 <sha>`. A branch that was rebased or
/// squashed away is no longer an ancestor, and there the plain merge base is
/// right after all.
///
/// # Errors
/// If git cannot resolve the commit.
pub fn fork_point(repo: &Path, sha: &str, main: &str) -> anyhow::Result<String> {
    let is_ancestor = |a: &str, b: &str| {
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["merge-base", "--is-ancestor", a, b])
            .status()
            .is_ok_and(|s| s.success())
    };
    let merges = git(
        repo,
        &[
            "rev-list",
            "--merges",
            "--ancestry-path",
            &format!("{sha}..{main}"),
        ],
    )
    .unwrap_or_default();
    // Oldest first: the merge that brought this branch in is the *earliest* on
    // the ancestry path, and `rev-list` prints newest-first.
    let found = merges.lines().rev().find_map(|m| {
        let parents = git(repo, &["rev-list", "--parents", "-n1", m])?;
        let mut it = parents.split_whitespace().skip(1);
        let (p1, p2) = (it.next()?, it.next()?);
        (is_ancestor(sha, p2) && !is_ancestor(sha, p1))
            .then(|| git(repo, &["merge-base", p1, sha]))
            .flatten()
    });
    match found {
        Some(base) => Ok(base),
        None => git(repo, &["merge-base", main, sha])
            .ok_or_else(|| anyhow::anyhow!("git cannot resolve a merge base for {sha}")),
    }
}

/// The reviewable files a commit touched, and what was set aside.
#[derive(Debug, Default)]
pub struct ReviewSet {
    /// Files with a readable diff.
    pub files: Vec<FileUnderReview>,
    /// Paths git reported as changed but produced no hunk for — binary blobs,
    /// and pure mode or rename records.
    ///
    /// **Counted, never quietly dropped.** Six of the corpus's 190 changed paths
    /// are binary audio fixtures whose whole diff is `Binary files … differ`.
    /// Sending that to a model buys a call's latency and returns noise that lands
    /// in the unadjudicated count, so they are set aside — but a run that reduced
    /// its own denominator without saying so would be reporting coverage it did
    /// not have, which is the failure mode this whole stage is arranged against.
    pub skipped: Vec<String>,
}

impl ReviewSet {
    /// Partition changed paths into files that can be reviewed and paths that
    /// cannot, by the one rule both callers use.
    ///
    /// # Why this is a shared constructor and not a shared predicate
    ///
    /// The replay path filtered unreviewable diffs and the live `--llm` path did
    /// not, so binary blobs and mode- or rename-only records were sent to the
    /// model on one surface and not the other. The output contract is
    /// *unachievable* for those: the reviewer must cite `line=<n>`, and
    /// `annotate_diff` never numbered a diff with no hunk, so the best possible
    /// reply is unparsed noise landing in the unadjudicated count.
    ///
    /// Exporting a `fn reviewable(diff) -> bool` for both sides to remember to
    /// call would leave the divergence possible and merely currently-absent. This
    /// is the same shape as the `[debt]` ignore honoured on three surfaces and not
    /// a fourth, and as `limit=0` meaning two things across five endpoints: each
    /// was closed by removing the room for the two answers to differ, not by
    /// correcting the instance. So collecting the set *is* applying the rule —
    /// there is no way to obtain a `ReviewSet` that skipped the check.
    ///
    /// `diff_of` returning `None` is treated as an unreadable diff rather than as
    /// an absent file: it lands in `skipped`, where it is reported, instead of
    /// being dropped on the floor.
    fn collect(reviewed_sha: &str, names: &str, diff_of: &dyn Fn(&str) -> Option<String>) -> Self {
        let mut set = Self::default();
        for path in names.lines().filter(|p| !p.is_empty()) {
            let diff = diff_of(path).unwrap_or_default();
            // No hunk header means there is no text to review: git emits
            // `Binary files a/… and b/… differ` for a blob, and a bare header for
            // a mode or rename change.
            if !diff.contains("@@") {
                set.skipped.push(path.to_owned());
                continue;
            }
            set.files.push(FileUnderReview {
                reviewed_sha: reviewed_sha.to_owned(),
                path: path.to_owned(),
                diff,
            });
        }
        set
    }
}

/// Every file a review commit touched, with its own diff.
///
/// # Errors
/// If the diff cannot be reconstructed.
pub fn files_at(repo: &Path, sha: &str, main: &str) -> anyhow::Result<ReviewSet> {
    let fork = fork_point(repo, sha, main)?;
    anyhow::ensure!(
        fork != sha,
        "the reconstruction base for {sha} is the review commit itself, so the diff \
         would be empty and every finding would score zero"
    );
    let names = git(repo, &["diff", "--name-only", &fork, sha])
        .ok_or_else(|| anyhow::anyhow!("git diff --name-only {fork}..{sha} failed"))?;
    Ok(ReviewSet::collect(sha, &names, &|path| {
        crate::diff::unified(repo, &[&fork, sha], path)
    }))
}

#[cfg(any(feature = "serve", feature = "inference-local-models"))]
/// A file's contents at a commit.
fn blob_at(repo: &Path, sha: &str, path: &str) -> Option<String> {
    git(repo, &["show", &format!("{sha}:{path}")])
}

/// Which reference stands for the trunk here.
fn main_ref(repo: &Path) -> anyhow::Result<String> {
    ["origin/main", "main"]
        .into_iter()
        .find(|r| git(repo, &["rev-parse", "--verify", "--quiet", r]).is_some())
        .map(str::to_owned)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "neither `origin/main` nor `main` resolves in {}",
                repo.display()
            )
        })
}

#[cfg(any(feature = "serve", feature = "inference-local-models"))]
/// What a replay produced, beyond the run document itself.
#[derive(Debug, Default)]
pub struct ReplayReport {
    /// Files reviewed.
    pub files: usize,
    /// Commits attempted.
    pub commits: usize,
    /// Findings emitted.
    pub findings: usize,
    /// Compile claims withheld by the filter.
    pub suppressed: usize,
    /// Replies that declared the file clean in the required form.
    pub clean: usize,
    /// Lines that looked like findings but could not be anchored.
    pub unparsed: usize,
    /// Files whose diff had to be truncated to fit the budget.
    pub truncated: usize,
    /// Files whose *reply* stopped inside a reasoning block — never reviewed, and
    /// never to be read as clean or scored as a zero.
    pub reasoning_truncated: usize,
    /// Files carrying at least one adjudicated corpus row.
    pub anchored_files: usize,
    /// Changed paths with no reviewable diff — binary blobs, mode and rename
    /// records. Reported so a reduced denominator is visible rather than assumed.
    pub skipped: usize,
    /// Context items actually sent, summed over files — the graph arm's dose.
    ///
    /// Reported because "the graph arm found more" is not a result if the arm
    /// turned out to be sending nothing. A run whose context was empty on every
    /// file is the diff-only arm under another name, and the number that says so
    /// belongs beside the recall figure rather than in a reader's assumption.
    pub context_items: usize,
    /// Context items dropped to stay inside the cap.
    pub context_dropped: usize,
    /// Estimated tokens of context sent, summed over files.
    pub context_tokens: usize,
    /// Files that carried at least one context item.
    pub files_with_context: usize,
    /// Files the engine refused (over its context window, or a decode failure).
    /// Named rather than counted: which files a budget cannot review is the
    /// actionable half, and a run that swallowed them would report coverage it
    /// did not have.
    pub refused: Vec<String>,
}

/// Replay the reviewer over every commit the corpus adjudicated and write a
/// `roteiro.review-run/v1` document.
///
/// # Errors
/// If the repository, the model or the output file cannot be used.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
pub fn run_replay(
    repo: &Path,
    out: &str,
    checks_path: Option<&str>,
    limit: Option<usize>,
    arm: ReviewArm,
    ingest: rto_graph::IngestConfig,
) -> anyhow::Result<()> {
    let corpus = rto_graph::review_corpus::builtin()?;
    let main = main_ref(repo)?;
    let checks = match checks_path {
        Some(p) => read_checks(p)?,
        None => Vec::new(),
    };
    if checks.is_empty() {
        eprintln!(
            "note: no --checks evidence supplied, so no compile claim can be refuted \
             and none will be withheld. That is the conservative default, not a \
             disabled filter: `compile_claim` is opt-in on evidence."
        );
    }

    let choice = rto_graph::resolve_model(rto_graph::ModelTask::Review)?;
    let model = choice.require_installed()?;
    eprintln!("reviewing with {model} — {}", choice.why());
    let engine = rto_llama::llama::LlamaEngine::new(
        vec![rto_llama::llama::Served {
            name: model.to_owned(),
            path: rto_graph::model_dir(model).join("model.gguf"),
            mmproj: None,
        }],
        REVIEW_N_CTX,
    )
    .map_err(|e| anyhow::anyhow!("starting llama.cpp: {e}"))?;

    // Anchored (sha, path) pairs, so the report can say how much of what it
    // reviewed the corpus can judge at all.
    let anchors: BTreeSet<(&str, &str)> = corpus
        .rows()
        .iter()
        .map(|r| (r.reviewed_sha.as_str(), r.path.as_str()))
        .collect();

    let shas: Vec<&str> = corpus.reviewed_shas().into_iter().collect();
    let shas = match limit {
        Some(n) => &shas[..n.min(shas.len())],
        None => &shas[..],
    };

    let mut run = CandidateRun {
        arm: Some(rto_graph::review_score::RunArm {
            context: arm.tag().to_owned(),
            model: model.to_owned(),
        }),
        ..CandidateRun::default()
    };
    let mut report = ReplayReport::default();
    // The object cache is shared across worktrees and content-addressed, so the
    // per-commit graph builds below re-extract only the blobs that actually differ
    // from an already-synced tree. Opened once rather than per commit.
    let graph_repo = rto_graph::Repo::discover(repo)?;
    let object_cache =
        rto_graph::ObjectCache::open(graph_repo.common_dir().join("roteiro").join("objects"))?;
    for (idx, sha) in shas.iter().enumerate() {
        let set = files_at(repo, sha, &main)?;
        let graph = graph_at(&graph_repo, &object_cache, ingest, arm, sha)?;
        run.attempted_shas.insert((*sha).to_owned());
        report.commits += 1;
        report.skipped += set.skipped.len();
        eprintln!(
            "[{}/{}] {} — {} file(s){}",
            idx + 1,
            shas.len(),
            &sha[..8],
            set.files.len(),
            if set.skipped.is_empty() {
                String::new()
            } else {
                format!(", {} with no reviewable diff", set.skipped.len())
            }
        );
        for file in &set.files {
            let sources = |p: &str| blob_at(repo, sha, p);
            let context = context_for(graph.as_ref(), file, &sources)?;
            report.context_items += context.items.len();
            report.context_dropped += context.dropped_items;
            report.context_tokens += context.tokens();
            report.files_with_context += usize::from(!context.is_empty());
            // A file the engine refuses is recorded and stepped over, never fatal.
            // A three-hour pass that dies on file 140 has measured nothing, and the
            // refusals are themselves a result: they say which files this budget
            // cannot actually review.
            let outcome = match review_file(&engine, model, file, &context, &checks, &sources) {
                Ok(outcome) => outcome,
                Err(e) => {
                    eprintln!("      refused {}: {e}", file.path);
                    report.refused.push(file.path.clone());
                    continue;
                }
            };
            report.files += 1;
            report.findings += outcome.findings.len();
            report.suppressed += outcome.suppressed.len();
            report.unparsed += outcome.unparsed;
            report.clean += usize::from(outcome.declared_clean);
            report.truncated += usize::from(outcome.dropped_tokens > 0);
            report.reasoning_truncated += usize::from(outcome.reasoning_truncated);
            report.anchored_files += usize::from(anchors.contains(&(*sha, file.path.as_str())));
            run.findings.extend(outcome.findings);
            run.suppressed
                .extend(outcome.suppressed.into_iter().map(|(f, _)| f));
        }
    }

    let json = serde_json::to_string_pretty(&run)?;
    std::fs::write(out, format!("{json}\n")).map_err(|e| anyhow::anyhow!("writing {out}: {e}"))?;
    print_replay(&report, out);
    Ok(())
}

#[cfg(any(feature = "serve", feature = "inference-local-models"))]
/// Print what a replay covered — **unadjudicated volume first**.
///
/// The order is the argument. Recall is what a score reports, but 22 adjudicated
/// rows sit across 190 reviewed files, so the great majority of what a reviewer
/// says here is something the corpus cannot judge and a human would have to. A
/// reviewer with excellent recall that also emits a finding on every file is not
/// one anybody runs, and a report that leads with recall hides that.
fn print_replay(report: &ReplayReport, out: &str) {
    println!(
        "\nreviewed {} file(s) over {} commit(s)",
        report.files, report.commits
    );
    println!(
        "  {} finding(s) emitted, of which the corpus can judge at most those on \
         the {} file(s) carrying an adjudicated row",
        report.findings, report.anchored_files
    );
    if report.files > 0 {
        #[expect(
            clippy::cast_precision_loss,
            reason = "file and finding counts here are in the hundreds"
        )]
        let per_file = report.findings as f64 / report.files as f64;
        println!("  {per_file:.2} finding(s) per file — the human-cost rate");
    }
    if report.files_with_context > 0 || report.context_dropped > 0 {
        println!(
            "  graph context: {} item(s), ~{} token(s), over {} of {} file(s){}",
            report.context_items,
            report.context_tokens,
            report.files_with_context,
            report.files,
            if report.context_dropped > 0 {
                format!("; {} item(s) dropped by the cap", report.context_dropped)
            } else {
                String::new()
            }
        );
    }
    if report.skipped > 0 {
        println!(
            "  {} changed path(s) had no reviewable diff (binary, mode or rename) \
             and were not sent to the model",
            report.skipped
        );
    }
    println!(
        "  {} file(s) declared clean in the required form",
        report.clean
    );
    println!(
        "  {} compile claim(s) withheld by the filter",
        report.suppressed
    );
    if report.unparsed > 0 {
        println!(
            "  {} line(s) looked like findings but carried no usable anchor — \
             a prompt problem, not a recall one",
            report.unparsed
        );
    }
    if report.truncated > 0 {
        println!(
            "  {} file(s) had their diff truncated to fit the budget, so those \
             reviews are of PART of the file",
            report.truncated
        );
    }
    if !report.refused.is_empty() {
        println!(
            "  {} file(s) the engine refused, so they were not reviewed at all:",
            report.refused.len()
        );
        for path in &report.refused {
            println!("      {path}");
        }
    }
    println!("\nwrote {out} — score it with: roteiro review --score {out}");
}

#[cfg(any(feature = "serve", feature = "inference-local-models"))]
/// Read a `CheckRun` array.
fn read_checks(path: &str) -> anyhow::Result<Vec<CheckRun>> {
    let text =
        std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("reading checks {path}: {e}"))?;
    serde_json::from_str(&text).map_err(|e| anyhow::anyhow!("{path}: {e}"))
}

/// The working-tree change (or a `base..HEAD` range) as one entry per file.
///
/// Split out of [`run_llm`] because collecting a diff and reviewing one are
/// separate jobs, and only the second needs a model — which is what lets the
/// interesting half of `run_llm` stay short enough to read.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
fn changed_files(repo: &Path, base: Option<&str>) -> ReviewSet {
    let head = git(repo, &["rev-parse", "HEAD"]).unwrap_or_else(|| "HEAD".to_owned());
    let range: Vec<String> = match base {
        Some(b) => vec![b.to_owned(), "HEAD".to_owned()],
        None => vec!["HEAD".to_owned()],
    };
    let mut args: Vec<&str> = vec!["diff", "--name-only"];
    args.extend(range.iter().map(String::as_str));
    let names = git(repo, &args).unwrap_or_default();

    // Returns a `ReviewSet` rather than a bare `Vec` so this path cannot differ
    // from the replay path about what is reviewable: the rule lives in
    // `ReviewSet::collect` and there is no way to build one around it. This used
    // to filter on `!diff.is_empty()` alone, which sent binary blobs and
    // mode-only records to the model under a contract they cannot satisfy.
    ReviewSet::collect(&head, &names, &|path| {
        let r: Vec<&str> = range.iter().map(String::as_str).collect();
        crate::diff::unified(repo, &r, path)
    })
}

/// Say which changed paths were never put to the model, and why.
///
/// **Reported, not silently dropped — and reported before the findings**, so it
/// cannot read as a footnote to a clean result. "Nothing to say about this file"
/// and "this file was never reviewed" are different facts; a run that renders
/// them identically is the vacuous zero this stage exists to make impossible,
/// and is the same distinction `reasoning_truncated` carries for a reply that
/// stopped early. A change that is *entirely* unreviewable would otherwise print
/// `0 finding(s)` and look like a clean review.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
fn announce_unreviewable(skipped: &[String]) {
    if skipped.is_empty() {
        return;
    }
    println!(
        "{} changed path(s) NOT REVIEWED — no hunk to anchor a finding to (binary \
         blob, or a mode/rename-only change), so the model is not asked for a \
         `line=` it could not cite:",
        skipped.len()
    );
    for path in skipped {
        println!("  {path}");
    }
}

/// Review the working-tree change (or a `base..HEAD` range) with the model.
///
/// # Errors
/// If the repository, the model or git cannot be used.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
pub fn run_llm(
    repo: &Path,
    base: Option<&str>,
    checks_path: Option<&str>,
    arm: ReviewArm,
    ingest: rto_graph::IngestConfig,
) -> anyhow::Result<()> {
    let checks = match checks_path {
        Some(p) => read_checks(p)?,
        None => Vec::new(),
    };
    let ReviewSet { files, skipped } = changed_files(repo, base);

    if files.is_empty() && skipped.is_empty() {
        println!("no changes to review");
        return Ok(());
    }
    announce_unreviewable(&skipped);
    if files.is_empty() {
        println!("\nnothing reviewable in the change");
        return Ok(());
    }

    let choice = rto_graph::resolve_model(rto_graph::ModelTask::Review)?;
    let model = choice.require_installed()?;
    eprintln!(
        "reviewing {} file(s) with {model} — {}",
        files.len(),
        choice.why()
    );
    if cfg!(debug_assertions) {
        eprintln!(
            "note: unoptimized build — local generation is very slow; use a \
             release build (`cargo build --release`) for usable speed."
        );
    }
    let engine = rto_llama::llama::LlamaEngine::new(
        vec![rto_llama::llama::Served {
            name: model.to_owned(),
            path: rto_graph::model_dir(model).join("model.gguf"),
            mmproj: None,
        }],
        REVIEW_N_CTX,
    )
    .map_err(|e| anyhow::anyhow!("starting llama.cpp: {e}"))?;

    // The live surface reviews the working tree, so its graph is the working
    // tree's — `HEAD` plus uncommitted edits, which is what `review` and `check`
    // already build. The replay's historical-rev build is the same assembly at a
    // different tree, not a different rule.
    let graph = match arm {
        ReviewArm::DiffOnly => None,
        ReviewArm::Graph => Some(worktree_graph(repo, ingest)?),
    };

    let mut total = 0usize;
    let mut withheld = 0usize;
    let mut never_reviewed: Vec<&str> = Vec::new();
    for file in &files {
        let sources = |p: &str| std::fs::read_to_string(repo.join(p)).ok();
        let context = context_for(graph.as_ref(), file, &sources)?;
        let outcome = review_file(&engine, model, file, &context, &checks, &sources)?;
        if outcome.reasoning_truncated {
            never_reviewed.push(file.path.as_str());
        }
        if outcome.findings.is_empty() && outcome.suppressed.is_empty() {
            continue;
        }
        println!("\n{}", file.path);
        for f in &outcome.findings {
            let class = f.defect_class.map_or("unclassified", |c| c.as_str());
            println!("  {}:{}  [{class}]  {}", file.path, f.line, f.description);
            total += 1;
        }
        for (f, reason) in &outcome.suppressed {
            println!("  {}:{}  [withheld]  {}", file.path, f.line, f.description);
            println!("      {reason}");
            withheld += 1;
        }
    }
    println!(
        "\n{total} finding(s) over {} file(s); {withheld} compile claim(s) withheld",
        files.len()
    );
    // Printed before the caveat below and never folded into the count, because
    // this is the line whose absence made a 0-of-4 reasoning-model run read as a
    // clean review rather than as no review at all.
    if !never_reviewed.is_empty() {
        println!(
            "\n{} of those file(s) were NOT REVIEWED — the reply stopped inside a \
             reasoning block before reaching an answer, so a low finding count here \
             says nothing about the code:",
            never_reviewed.len()
        );
        for path in &never_reviewed {
            println!("  {path}");
        }
        println!(
            "  Use a non-reasoning model, or raise the generation cap \
             (currently {REVIEW_MAX_TOKENS} tokens)."
        );
    }
    println!(
        "These are one model's opinions, unadjudicated. `docs/REVIEW_CHECKLIST.md` \
         has the triage rule; the corpus in `crates/rto-graph/tests/fixtures/review/` \
         is what any of it is measured against."
    );
    Ok(())
}

/// The shas the corpus adjudicated, for a test that needs them without a model.
#[cfg(test)]
#[must_use]
pub fn corpus_shas() -> Vec<String> {
    rto_graph::review_corpus::builtin()
        .map(|c| c.reviewed_shas().into_iter().map(str::to_owned).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{
        FileUnderReview, ReviewArm, ReviewSet, context_for, corpus_shas, files_at, fork_point,
        graph_at, main_ref, parent_module_source,
    };
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    /// This repository's root, from the crate that is being tested.
    fn repo() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    /// Whether the git history this test needs is present, printing the reason if
    /// not — the pattern `dependency_axis.rs` uses for the OSV database. A shallow
    /// clone is a property of the checkout, never of the code.
    fn history_available(repo: &Path) -> bool {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["rev-parse", "--is-inside-work-tree"])
            .output()
            .is_ok_and(|o| o.status.success());
        if !ok {
            eprintln!("SKIP: not a git work tree, cannot reconstruct reviewed diffs");
            return false;
        }
        let shallow = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["rev-parse", "--is-shallow-repository"])
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "true");
        if shallow {
            eprintln!("SKIP: shallow clone — run `git fetch --unshallow` to reconstruct");
            return false;
        }
        if main_ref(repo).is_err() {
            eprintln!("SKIP: neither origin/main nor main resolves here");
            return false;
        }
        true
    }

    /// Open this repository and the shared, content-addressed object cache the
    /// per-commit graph builds hit.
    fn graph_inputs() -> Option<(rto_graph::Repo, rto_graph::ObjectCache)> {
        let repo = rto_graph::Repo::discover(&repo()).ok()?;
        let cache =
            rto_graph::ObjectCache::open(repo.common_dir().join("roteiro").join("objects")).ok()?;
        Some((repo, cache))
    }

    /// **The graph arm must be built at the reviewed commit, not at `HEAD`.**
    ///
    /// This is the same silent zero as scoring against a PR head, arriving from a
    /// fourth direction: a run assembled against today's ADRs would review 2026's
    /// code against decisions written after it, produce a perfectly clean-looking
    /// set of numbers, and describe a repository that never existed. Nothing about
    /// the output would look wrong.
    ///
    /// Held by a count rather than by inspection: this repository has gained ADRs
    /// since every commit the corpus covers, so a graph built at `HEAD` by mistake
    /// carries strictly more `adr` nodes than the commit had files.
    #[test]
    fn the_graph_arm_is_built_at_the_reviewed_commit_not_at_head() {
        let repo_path = repo();
        if !history_available(&repo_path) {
            return;
        }
        let Some((repo, cache)) = graph_inputs() else {
            eprintln!("SKIP: cannot open the repository's object cache");
            return;
        };
        for sha in &corpus_shas() {
            let on_disk = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo_path)
                .args(["ls-tree", "-r", "--name-only", sha, "--", "docs/adr/"])
                .output()
                .expect("git ls-tree runs");
            let expected = String::from_utf8_lossy(&on_disk.stdout)
                .lines()
                .filter(|p| {
                    std::path::Path::new(p)
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("md"))
                        && !p.ends_with("README.md")
                })
                .count();

            let store = graph_at(
                &repo,
                &cache,
                rto_graph::IngestConfig::default(),
                ReviewArm::Graph,
                sha,
            )
            .expect("the graph at a corpus commit assembles")
            .expect("the graph arm yields a store");
            let adrs = store
                .nodes_by_kind(&rto_graph::NodeKind::Adr)
                .expect("the store answers");
            assert_eq!(
                adrs.len(),
                expected,
                "{sha} carries {expected} ADR file(s) but the graph holds {} — \
                 built at the wrong tree",
                adrs.len()
            );
        }
    }

    /// **The arm tags are a written contract, not a display string.**
    ///
    /// They are recorded into every run document
    /// ([`rto_graph::review_score::RunArm::context`]) and are how a reader tells
    /// the two arms of this experiment apart six months from now. Renaming one
    /// would not break a build; it would silently make old artifacts and new ones
    /// incomparable, which is the failure this whole stage is arranged against.
    #[test]
    fn the_arm_tags_are_stable_and_distinct() {
        assert_eq!(ReviewArm::DiffOnly.tag(), "diff-only");
        assert_eq!(ReviewArm::Graph.tag(), "graph");
        assert_ne!(ReviewArm::DiffOnly.tag(), ReviewArm::Graph.tag());
    }

    /// **The live surface and the measured surface build the same graph.**
    ///
    /// `review --llm --graph-context` assembles the working tree's graph and the
    /// replay assembles a commit's, through the same derived-then-authored
    /// sequence. If they diverged, the replay's number would be a measurement of
    /// something users cannot run — which is the failure `ReviewSet::collect` was
    /// introduced to close on the other half of this module.
    #[test]
    fn the_live_surface_builds_the_same_graph_as_the_replay() {
        let repo_path = repo();
        if !history_available(&repo_path) {
            return;
        }
        let Ok(store) = super::worktree_graph(&repo_path, rto_graph::IngestConfig::default())
        else {
            eprintln!("SKIP: the working-tree graph could not be assembled here");
            return;
        };
        let on_disk = std::fs::read_dir(repo_path.join("docs/adr"))
            .expect("this repository has an ADR directory")
            .filter_map(Result::ok)
            .filter(|e| {
                let name = e.file_name();
                let name = name.to_string_lossy();
                std::path::Path::new(name.as_ref())
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
                    && name != "README.md"
            })
            .count();
        let adrs = store
            .nodes_by_kind(&rto_graph::NodeKind::Adr)
            .expect("the store answers");
        assert_eq!(
            adrs.len(),
            on_disk,
            "the live graph holds {} ADR node(s) against {on_disk} on disk — the \
             authored layer did not reach it",
            adrs.len()
        );
    }

    /// **The diff-only arm is the absence of a store, and must send nothing.**
    ///
    /// The baseline's whole meaning is that the model saw the diff and nothing
    /// else. An arm that quietly acquired one item would make the comparison a
    /// comparison of two graph arms.
    #[test]
    fn the_diff_only_arm_sends_no_context_at_all() {
        let file = FileUnderReview {
            reviewed_sha: "0".repeat(40),
            path: "src/lib.rs".to_owned(),
            diff: "@@ -1 +1 @@\n+x\n".to_owned(),
        };
        let context = context_for(None, &file, &|_| None).expect("no store, no work");
        assert!(context.is_empty());
        assert_eq!(context.dropped_items, 0);
        assert_eq!(context.tokens(), 0);
    }

    /// **The graph arm must actually send something, or it is the diff-only arm
    /// wearing another name.**
    ///
    /// A comparison whose treatment turned out to be empty reports the model's
    /// run-to-run variance as a finding about the graph. So this asserts a
    /// non-empty, provenance-tagged context on a real corpus file — and that
    /// `authored` items are present, since a governing decision is the specific
    /// thing the arm exists to supply.
    #[test]
    fn the_graph_arm_supplies_provenance_tagged_context_on_the_corpus() {
        let repo_path = repo();
        if !history_available(&repo_path) {
            return;
        }
        let Some((repo, cache)) = graph_inputs() else {
            eprintln!("SKIP: cannot open the repository's object cache");
            return;
        };
        let main = main_ref(&repo_path).expect("checked above");
        let mut files_with_context = 0usize;
        let mut authored = 0usize;
        let mut derived = 0usize;
        let mut items = 0usize;
        let mut tokens = 0usize;
        let mut dropped = 0usize;
        for sha in &corpus_shas() {
            let store = graph_at(
                &repo,
                &cache,
                rto_graph::IngestConfig::default(),
                ReviewArm::Graph,
                sha,
            )
            .expect("the graph at a corpus commit assembles")
            .expect("the graph arm yields a store");
            let set = files_at(&repo_path, sha, &main).expect("the diff reconstructs");
            for file in &set.files {
                let sources = |p: &str| {
                    std::process::Command::new("git")
                        .arg("-C")
                        .arg(&repo_path)
                        .args(["show", &format!("{sha}:{p}")])
                        .output()
                        .ok()
                        .filter(|o| o.status.success())
                        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                };
                let context =
                    context_for(Some(&store), file, &sources).expect("the context assembles");
                assert!(
                    context.tokens() <= rto_graph::reviewer::CONTEXT_CAP_TOKENS,
                    "{}: {} context tokens exceeds the cap — the dose is not bounded \
                     by the policy that was pre-registered for it",
                    file.path,
                    context.tokens()
                );
                if !context.is_empty() {
                    files_with_context += 1;
                    items += context.items.len();
                    tokens += context.tokens();
                    dropped += context.dropped_items;
                }
                for item in &context.items {
                    assert!(
                        !item.body.trim().is_empty(),
                        "{}: an item claiming context quoted nothing",
                        item.label
                    );
                    match item.provenance.as_str() {
                        "authored" => authored += 1,
                        "derived" => derived += 1,
                        other => panic!("unknown provenance layer {other:?} on {}", item.label),
                    }
                }
            }
        }
        println!(
            "graph-arm dose over the corpus: {files_with_context} file(s) carried \
             context, {items} item(s) ({authored} authored, {derived} derived), \
             ~{tokens} token(s), {dropped} item(s) dropped by the cap"
        );
        assert!(
            files_with_context > 0,
            "the graph arm produced no context on any corpus file — it is the \
             diff-only arm under another name"
        );
        assert!(
            authored > 0,
            "no governing ADR reached any file: the arm's central item is missing \
             ({derived} derived item(s) were sent)"
        );
    }

    /// **The recipe, held to the data by the shipped code rather than by a
    /// transcript of it.**
    ///
    /// `rto-graph`'s `every_row_reconstructs_a_non_empty_reviewed_diff` asserts
    /// the same property to guard the corpus fixture; this asserts it against the
    /// function a replay actually calls. A recipe that is correct in a test and
    /// wrong in the harness produces a score that looks like a measurement.
    #[test]
    fn every_corpus_commit_reconstructs_a_diff_touching_its_anchor() {
        let repo = repo();
        if !history_available(&repo) {
            return;
        }
        let main = main_ref(&repo).expect("checked above");
        let corpus = rto_graph::review_corpus::builtin().expect("the shipped corpus parses");

        let mut by_sha: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for row in corpus.rows() {
            by_sha
                .entry(row.reviewed_sha.as_str())
                .or_default()
                .push(row.path.as_str());
        }

        for (sha, anchors) in by_sha {
            let fork = fork_point(&repo, sha, &main).expect("a fork point resolves");
            assert_ne!(
                fork,
                sha,
                "{}: the base is the review commit itself, so the diff is empty — \
                 the silent zero this recipe exists to avoid",
                &sha[..8]
            );
            let set = files_at(&repo, sha, &main).expect("the diff reconstructs");
            assert!(!set.files.is_empty(), "{}: empty diff", &sha[..8]);
            let paths: Vec<&str> = set.files.iter().map(|f| f.path.as_str()).collect();
            for anchor in anchors {
                assert!(
                    paths.contains(&anchor),
                    "{}: the reconstructed diff does not touch {anchor}, the file a \
                     comment is anchored to. Touched: {}",
                    &sha[..8],
                    paths.join(", ")
                );
            }
            // Every file handed to a reviewer carries a diff it could read, and
            // everything set aside genuinely had none.
            assert!(
                set.files.iter().all(|f| f.diff.contains("@@")),
                "{}: a file with no hunk reached the reviewable set",
                &sha[..8]
            );
        }
    }

    /// **No adjudicated row is anchored to a file the harness sets aside.** Six of
    /// the corpus's 190 changed paths are binary audio fixtures with no reviewable
    /// diff. Skipping them is free only while that stays true — a skip rule that
    /// quietly excluded a file carrying a real defect would raise recall by
    /// shrinking its own denominator, which is the most flattering mistake
    /// available here.
    #[test]
    fn nothing_the_harness_skips_carries_an_adjudicated_row() {
        let repo = repo();
        if !history_available(&repo) {
            return;
        }
        let main = main_ref(&repo).expect("checked above");
        let corpus = rto_graph::review_corpus::builtin().expect("parses");
        let mut skipped_total = 0;
        for sha in corpus_shas() {
            let set = files_at(&repo, &sha, &main).expect("reconstructs");
            skipped_total += set.skipped.len();
            for path in &set.skipped {
                assert!(
                    !corpus
                        .rows()
                        .iter()
                        .any(|r| r.reviewed_sha == sha && &r.path == path),
                    "{}: {path} is skipped as unreviewable but carries a corpus row",
                    &sha[..8]
                );
            }
        }
        assert_eq!(
            skipped_total, 6,
            "the measured count of unreviewable paths in this corpus"
        );
    }

    /// The measured scale of a replay, asserted so a change to the recipe that
    /// quietly reviews half the corpus is caught. The budget analysis was computed
    /// over 190 changed paths on 15 commits, of which **184 are reviewable** —
    /// both halves are asserted, because a drop in either is a different bug.
    #[test]
    fn a_replay_covers_the_measured_number_of_files() {
        let repo = repo();
        if !history_available(&repo) {
            return;
        }
        let main = main_ref(&repo).expect("checked above");
        let (mut reviewable, mut changed) = (0usize, 0usize);
        for sha in corpus_shas() {
            let set = files_at(&repo, &sha, &main).expect("reconstructs");
            reviewable += set.files.len();
            changed += set.files.len() + set.skipped.len();
        }
        assert_eq!(
            (corpus_shas().len(), changed, reviewable),
            (15, 190, 184),
            "15 commits, 190 changed paths, 184 with a reviewable diff"
        );
    }

    /// **The engine's window must hold the budget the analysis was done against.**
    /// The first replay run died on its second file because `LlamaEngine::new`
    /// reads `n_ctx: 0` as 4,096 — Roteiro's default, not the model's limit — and
    /// `spec draft` passes `0` because a drafted section is small. The `const`
    /// assertion beside the constants catches the arithmetic at compile time; this
    /// records what it is for, and that the slack is for `len / 4` understating a
    /// prompt of code rather than a margin someone liked the look of.
    #[cfg(any(feature = "serve", feature = "inference-local-models"))]
    #[test]
    fn the_context_window_holds_the_whole_budget() {
        use super::{REVIEW_MAX_TOKENS, REVIEW_N_CTX, REVIEW_PROMPT_BUDGET};
        // `LlamaEngine`'s own default, which `n_ctx: 0` selects and which killed
        // the first replay run on its second file. Compared at compile time —
        // these are all constants, so a runtime assertion over them is one that
        // can never fail at test time, which clippy rightly objects to.
        const ENGINE_DEFAULT_N_CTX: u32 = 4_096;
        const _: () = assert!(
            REVIEW_N_CTX > ENGINE_DEFAULT_N_CTX,
            "the engine default is what broke this"
        );
        let worst_case = REVIEW_PROMPT_BUDGET * 13 / 10 + REVIEW_MAX_TOKENS as usize;
        assert!(
            REVIEW_N_CTX as usize >= worst_case,
            "a prompt `len / 4` understated by 30% plus its generation is \
             {worst_case} tokens, over the {REVIEW_N_CTX}-token window"
        );
    }

    /// **One reviewability rule, and both paths reach it by construction.**
    ///
    /// The replay path filtered on `contains("@@")` and the live `--llm` path on
    /// `!is_empty()`, so a binary blob or a mode-only record was sent to the model
    /// on one surface and not the other — under a contract it cannot satisfy,
    /// since `annotate_diff` numbers no line in a diff with no hunk. Both now
    /// obtain their set only from `ReviewSet::collect`, so the rule cannot be
    /// applied on one side and forgotten on the other; this pins what the rule
    /// says, and the type pins that it is asked.
    #[test]
    fn the_reviewable_rule_is_one_rule_and_skips_are_kept() {
        let diffs: BTreeMap<&str, &str> = [
            ("src/real.rs", "@@ -1,2 +1,3 @@\n context\n+added\n"),
            (
                "assets/beep.wav",
                "Binary files a/assets/beep.wav and b/assets/beep.wav differ\n",
            ),
            (
                "scripts/run.sh",
                "diff --git a/scripts/run.sh b/scripts/run.sh\nold mode 100644\nnew mode 100755\n",
            ),
        ]
        .into_iter()
        .collect();
        // `gone.rs` resolves to no diff at all, which is unreadable rather than
        // absent: it is reported, not dropped on the floor.
        let names = "src/real.rs\nassets/beep.wav\nscripts/run.sh\ngone.rs\n";

        let set = ReviewSet::collect("deadbeef", names, &|p| {
            diffs.get(p).map(|d| (*d).to_owned())
        });

        let reviewed: Vec<&str> = set.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            reviewed,
            vec!["src/real.rs"],
            "only a diff with a hunk carries a citable line number"
        );
        assert_eq!(set.files[0].reviewed_sha, "deadbeef");
        assert_eq!(
            set.skipped,
            vec![
                "assets/beep.wav".to_owned(),
                "scripts/run.sh".to_owned(),
                "gone.rs".to_owned(),
            ],
            "a binary blob, a mode-only record and an unreadable diff are all \
             counted rather than silently reducing the denominator"
        );
    }

    /// A file's feature gate lives in its parent module, so the lookup has to find
    /// the parent and must not find the file itself.
    #[test]
    fn the_parent_module_is_found_and_is_never_the_file_itself() {
        let files: BTreeMap<&str, &str> = [
            ("crates/rto-exec/src/lib.rs", "pub mod boxlite;"),
            ("crates/rto-exec/src/boxlite.rs", "fn run() {}"),
        ]
        .into_iter()
        .collect();
        let sources = |p: &str| files.get(p).map(|s| (*s).to_owned());

        let parent = parent_module_source("crates/rto-exec/src/boxlite.rs", &sources);
        assert_eq!(parent.as_deref(), Some("pub mod boxlite;"));

        // `lib.rs`'s own parent is not `lib.rs`: the candidate equal to the path
        // is skipped, or a gate on the file would be read as a gate on itself.
        let own = parent_module_source("crates/rto-exec/src/lib.rs", &sources);
        assert_ne!(own.as_deref(), Some("pub mod boxlite;"));

        // A file at the repository root has no parent directory to look in.
        assert!(parent_module_source("main.rs", &sources).is_none());
    }

    /// **A `mod.rs`'s parent lives one directory up**, so the search must start
    /// from the grandparent — searching its own directory finds nothing and
    /// returns `None`, which `claim_site` reads as *unconditional*.
    ///
    /// Latent here (this repository has no `src/**/mod.rs`), fixed because the
    /// failure direction is permissive: it would suppress a feature-gated file's
    /// compile claims on the strength of a job that never built it.
    #[test]
    fn a_mod_rss_parent_is_searched_one_directory_up() {
        let files: BTreeMap<&str, &str> = [
            (
                "crates/rto-exec/src/lib.rs",
                "#[cfg(feature = \"exec-boxlite\")]\npub mod boxlite;",
            ),
            ("crates/rto-exec/src/boxlite/mod.rs", "fn run() {}"),
        ]
        .into_iter()
        .collect();
        let sources = |p: &str| files.get(p).map(|s| (*s).to_owned());

        let parent = parent_module_source("crates/rto-exec/src/boxlite/mod.rs", &sources);
        assert_eq!(
            parent.as_deref(),
            Some("#[cfg(feature = \"exec-boxlite\")]\npub mod boxlite;"),
            "`boxlite/mod.rs` is declared in `src`, not in `src/boxlite`"
        );
    }

    /// **`review --llm` is local-only, and that is enforced rather than
    /// remembered.** `ModelTask::Review` reports `goes_remote() == true` — it is a
    /// command-level generative surface — but ADR-0019 §4's payload allow-list
    /// carries node identities and prose and says *"You are not given source
    /// code"*. A remote review sends the diff, so it needs a new allow-listed
    /// field: an ADR amendment, not a flag. Until then the flag must not exist,
    /// because a `--allow-remote` that worked here would have gone around the
    /// allow-list rather than through it.
    #[test]
    fn review_llm_has_no_allow_remote_flag_until_the_allow_list_carries_source() {
        use clap::CommandFactory;

        let cli = <crate::Cli as CommandFactory>::command();
        let review = cli
            .get_subcommands()
            .find(|c| c.get_name() == "review")
            .expect("`review` is a subcommand");
        let flags: Vec<&str> = review
            .get_arguments()
            .map(clap::Arg::get_id)
            .map(clap::Id::as_str)
            .collect();
        assert!(flags.contains(&"llm"), "the surface exists: {flags:?}");
        assert!(flags.contains(&"replay"), "the harness exists: {flags:?}");
        assert!(
            !flags.contains(&"allow_remote"),
            "review must not offer the remote tier while the payload allow-list \
             cannot carry source: {flags:?}"
        );
        // And the task itself still qualifies, so this is a missing payload rather
        // than a task ruled out — the two are different facts and the fix differs.
        //
        // Gated because `ModelTask` is re-exported behind `rto-graph/models`,
        // which only this crate's `models` feature turns on — so in a
        // `--no-default-features --features execution` build the type does not
        // exist and this line was a compile error (issue #445). The gate is on
        // the assertion alone rather than on the test, because the flag
        // assertions above are the property under protection and they hold in
        // *every* build, including the one with no remote tier to offer.
        #[cfg(feature = "models")]
        assert!(rto_graph::ModelTask::Review.goes_remote());
    }
}

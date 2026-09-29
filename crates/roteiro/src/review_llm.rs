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
//! diff** for every one of the 15 review commits, because each is an ancestor of
//! `main`. It used to be all but two, the exceptions being three rows on the two
//! commits a force-push had removed from the repository; #822 re-pinned them, and
//! the historical denominator is deliberately not quoted here, because the window
//! has been 15, then 16, then 15 again and a number in prose beside a moving set is
//! the drift this module documents elsewhere.
//! [`fork_point`] implements the corrected recipe — find the merge that
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

/// Where a finding's defect class comes from (issue #897).
///
/// # Why this is a choice and not simply the better way
///
/// [`Self::TypedRead`] strictly removes a failure: `parse_findings` reads
/// `class=<token>` through `DefectClass::from_token`, which answers `None` for
/// anything outside the fourteen — so an invented, abbreviated or translated
/// class is silently discarded and the finding is recorded as carrying no class
/// at all. A typed read cannot produce a non-member, because the answer is an
/// index into the class list rather than a string.
///
/// It is nonetheless **off by default**, on two grounds:
///
/// * **It costs one prefill per finding, not per file.** The recorded baseline
///   emits 10.9 findings per file, so this is an order of magnitude more
///   inference calls than the review itself — cheap ones (the question is a few
///   hundred tokens, not a whole diff) but ten times as many. A default that
///   multiplies a three-hour pass is a decision for the person paying for it.
/// * **It changes what the reviewer emits.** The recorded 1,995-finding run is
///   the baseline every comparison is against, and its classes came out of the
///   reply's text. Switching the default would make the next run incomparable
///   with it while looking like the same command.
///
/// [`Self::ReplyText`] is therefore what `review --llm` does unless asked, and a
/// run document written under it is byte-identical to one written before any of
/// this existed.
///
/// `#[non_exhaustive]`: these two are the mechanisms that exist, not the
/// mechanisms there can be. A *fitted* read — the same distribution through a
/// calibration the module docs say must come before the number may be called a
/// confidence — is a third source and not a variation on either of these, and it
/// is the obvious next one.
///
/// Behind the same gate as [`FileOutcome`], and for the same reason: the only
/// things that read it are the per-file review and the classification pass, both
/// of which need a generation backend. In a build without one there is no
/// mechanism to choose between.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ClassSource {
    /// From the `class=<token>` field of the reply's own text — the default, and
    /// what every recorded run used.
    #[default]
    ReplyText,
    /// From a typed question over the closed class set, read off the model's
    /// label-token distribution (`rto_llama::typed`). Carries a sharpness, an
    /// option mass and a margin; see [`classify_findings`].
    TypedRead,
}

#[cfg(any(feature = "serve", feature = "inference-local-models"))]
impl ClassSource {
    /// The tag written into [`rto_graph::review_score::RunArm::class_source`].
    ///
    /// Beside [`ReviewArm::tag`] and in the same shape, because this is the
    /// **second** experimental variable `review --llm` has: a run document that
    /// recorded one and not the other could not be told from a run that varied
    /// the unrecorded one, which is the whole reason `RunArm` exists.
    #[must_use]
    pub fn tag(self) -> &'static str {
        match self {
            Self::ReplyText => "reply-text",
            Self::TypedRead => "typed-read",
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
/// Ask the model for **one judgement over the whole change** (issue #649, part 2).
///
/// # It is reported and never gated on
///
/// `roteiro review` exits non-zero on authored-layer drift and on nothing else,
/// and this does not change that: the graph arm computes the exit status, this is
/// the `--llm` arm, and no value returned here reaches a gate. A deterministic
/// gate that sometimes depends on a generation is not a gate, and its failure
/// mode is the worst kind — it passes when it should not, occasionally, for
/// reasons nobody can reproduce.
///
/// # A missing verdict is reported as missing
///
/// `Ok(None)` covers both a reply that carried no `VERDICT` line and a generation
/// that stopped inside a reasoning block. Neither is defaulted to `clean`: a
/// verdict this function invented would be indistinguishable, to a reader and to
/// the corpus, from one a model actually formed — which is the same silent zero
/// [`FileOutcome::reasoning_truncated`] exists to prevent, one level up.
///
/// # Errors
/// If the engine fails to generate.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
pub fn verdict_on(
    engine: &rto_llama::llama::LlamaEngine,
    model: &str,
    reviewed_sha: &str,
    files: &[FileUnderReview],
    findings: &[String],
) -> anyhow::Result<Option<rto_graph::review_score::CandidateVerdict>> {
    use rto_llama::Engine as _;

    let lines: Vec<&str> = findings.iter().map(String::as_str).collect();
    let prompt = rto_graph::reviewer::build_verdict_prompt(files, &lines, REVIEW_PROMPT_BUDGET);
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
        .map_err(|e| anyhow::anyhow!("summarising the change: {e}"))?;
    let Ok(reply) = rto_llama::thinking::answer(&completion.content, completion.finish_reason)
    else {
        return Ok(None);
    };
    Ok(rto_graph::reviewer::parse_verdict(reviewed_sha, reply))
}

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
    class_source: ClassSource,
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

    if class_source == ClassSource::TypedRead {
        // Both lists, and for the same reason the suppression loop keeps them
        // both: a withheld finding is still recorded in the run document, and a
        // score over it would otherwise compare a typed class against a parsed
        // one depending on whether CI happened to refute it.
        classify_findings(engine, model, &mut findings)?;
        for (finding, _) in &mut withheld {
            classify_findings(engine, model, std::slice::from_mut(finding))?;
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

/// Ask the model, as a **typed question**, which of the fourteen classes each
/// finding belongs to, and overwrite its class and all **three** shape numbers —
/// sharpness, option mass and margin — with what the distribution says
/// (issue #897).
///
/// # What the model is shown, and what it is not
///
/// The state is the finding's **own text** — its path, its line and its
/// description — and not the diff it came from. That is deliberate and it is the
/// reason this is cheap: the question is *which class does this claim belong to*,
/// which is a property of the claim, and the review prompt already requires each
/// finding to be self-contained (*"Quote the specific words that conflict, so a
/// reader can check you without opening the file"*). Re-sending the diff would
/// cost a second full prefill per finding to re-derive something already stated.
///
/// It is a real limitation and not only an economy: a description too vague to
/// classify is classified anyway, from too little — and **nothing here reports
/// that it happened.**
///
/// An earlier version of this paragraph claimed such a case produces a flat
/// distribution, so the limitation would be visible in the sharpness. The
/// measurement in `typed_class_calibration` disproves it: of the five corpus rows
/// this classifier got wrong, four came back at a sharpness of *exactly*
/// `1_000_000` ppm. A model can be sharply wrong on an under-specified
/// description, and on this model it usually is. The limitation is therefore
/// **not** self-reporting, and a caller must not read a high sharpness as
/// evidence that the description carried enough to classify.
///
/// # No fallback to the parsed class
///
/// A finding this read touches has its class replaced outright, including when
/// the text had already parsed to a class. Keeping the parsed one "when it looks
/// right" would make the recorded class a function of two mechanisms and of which
/// agreed, and nothing downstream could then say which instrument it was scoring.
///
/// # Three numbers, and none of them ranks what this function produces
///
/// The reading records its sharpness, its option mass and its **margin**, and the
/// measured scope of each is on
/// `CandidateFinding::class_margin_micronats` and
/// `CandidateFinding::class_sharpness_ppm` in full. In short, on
/// `qwen3-coder-30b-a3b`:
///
/// * the **sharpness** is saturated — exactly `1_000_000` on 26 of the 27
///   adjudicated corpus rows, four of the five wrong answers included — so there
///   is nothing in it to order two findings by;
/// * the **margin** ordered a correct class above a wrong one on those same
///   human-written corpus descriptions at 0.81, and ordered **the findings this
///   function classifies** no better than chance (0.52 against a null of 0.4999,
///   `P = 0.4411`, over 446 of them).
///
/// So nothing recorded here is a triage signal for the reviewer's own output.
/// **Two** things separate those two margin figures rather than one — who wrote
/// the prose, and whether what is being separated is a correct class from a wrong
/// one or a real finding from noise — so neither alone accounts for the drop, and
/// neither is offered here as the explanation.
///
/// The **0.98** recorded against this same corpus belongs to `REALITY_QUESTION`, a
/// two-option yes/no that this function never asks and that nothing ships. It is
/// not a reading of the margin recorded here, and quoting it for this number would
/// credit a binary instrument's result to a fourteen-way one.
///
/// All three numbers are kept because a different model may behave differently,
/// and a run that records only one cannot show it.
///
/// # Errors
/// If the engine fails on a question. One failure aborts the file rather than
/// leaving a mixture: a `FileOutcome` whose findings came from two different
/// class mechanisms is not a measurement of either.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
pub fn classify_findings(
    engine: &rto_llama::llama::LlamaEngine,
    model: &str,
    findings: &mut [CandidateFinding],
) -> anyhow::Result<()> {
    use rto_graph::review_score::{fraction_ppm, margin_micronats};

    if findings.is_empty() {
        return Ok(());
    }
    // One question, asked of every finding: built once because it is a pure
    // function of the class set, and because `Choice::new`'s validation of that
    // set should be paid once per file rather than once per finding.
    let question = rto_llama::typed::Choice::new(rto_graph::reviewer::class_options())
        .map_err(|e| anyhow::anyhow!("building the class question: {e}"))?;

    for finding in findings {
        let state = format!(
            "A code reviewer reported this finding about `{}` at line {}:\n\n{}",
            finding.path, finding.line, finding.description,
        );
        let answer = engine
            .ask_choice(
                model,
                &state,
                rto_graph::reviewer::CLASS_QUESTION,
                &question,
            )
            .map_err(|e| anyhow::anyhow!("classifying {}:{}: {e}", finding.path, finding.line))?;
        // `value()` is a `DefectClass` the question was built from, so there is no
        // token to validate and no `None` arm to take — which is the whole of what
        // this replaces.
        finding.defect_class = Some(*answer.value());
        finding.class_sharpness_ppm = Some(fraction_ppm(answer.sharpness()));
        finding.class_option_mass_ppm = Some(fraction_ppm(answer.option_mass()));
        // Nats, so a **different** quantiser: `fraction_ppm` clamps at one and
        // every margin measured on this corpus was between 8 and 33, so reusing it
        // would record them all as the same number. `review_score` holds the two
        // apart with a test.
        finding.class_margin_micronats = Some(margin_micronats(answer.margin()));
    }
    Ok(())
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
    // `walk_blobs` rather than `authored_blobs` — this caller wants the whole
    // tree from disk — so the `[paths]` policy is supplied explicitly here. That
    // is the case `authored_docs_from` re-checks for: a blob list built outside
    // `rto-spec` cannot have been filtered by it.
    crate::apply_authored_layer(
        &mut store,
        graph_repo.walk_blobs()?,
        &|blob| {
            Ok(graph_repo
                .workdir()
                .and_then(|w| std::fs::read(w.join(&blob.path)).ok()))
        },
        ingest.paths,
    )?;
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
///    diff does not already show — see `doc_already_shown`.
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
///   the diff, so `doc_already_shown` filters every doc comment, and nothing
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
    /// **Counted, never quietly dropped.** Six of the corpus's 182 changed paths
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
    fn collect(
        reviewed_sha: &str,
        names: &str,
        paths: &rto_graph::PathPolicy,
        diff_of: &dyn Fn(&str) -> Option<String>,
    ) -> Self {
        let mut set = Self::default();
        for path in names.lines().filter(|p| !p.is_empty()) {
            // **The egress gate, and it is here because this is the one place
            // either route turns a name into bytes.** `review --llm` differs in
            // kind from every other reader of `[paths]`: it does not *store* what
            // it reads, it **sends** it, so a declaration is an egress control and
            // not merely a graph filter. A user who excludes a corpus of
            // third-party documents will take that to mean the bytes do not leave
            // the machine, and a filter holding on one route into the model and
            // not another reads as a guarantee while not being one.
            //
            // Asked **before** `diff_of`, which shells out to `git diff` and reads
            // the file: that is what makes `exclude`'s "the bytes are never read"
            // true here rather than approximately true.
            //
            // Gating the two callers instead was the first attempt and it left the
            // replay path open — `files_at` builds a diff for every changed name
            // and hands it straight to `review_file`. One rule, at the chokepoint,
            // is what a third caller inherits without being told.
            if !paths.classify(path).mines() {
                continue;
            }
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
pub fn files_at(
    repo: &Path,
    sha: &str,
    main: &str,
    paths: &rto_graph::PathPolicy,
) -> anyhow::Result<ReviewSet> {
    let fork = fork_point(repo, sha, main)?;
    anyhow::ensure!(
        fork != sha,
        "the reconstruction base for {sha} is the review commit itself, so the diff \
         would be empty and every finding would score zero"
    );
    let names = git(repo, &["diff", "--name-only", &fork, sha])
        .ok_or_else(|| anyhow::anyhow!("git diff --name-only {fork}..{sha} failed"))?;
    Ok(ReviewSet::collect(sha, &names, paths, &|path| {
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
    /// Whole-change verdicts the model actually formed (issue #649, part 2).
    pub verdicts: usize,
    /// Of those, how many said `clean`. Reported beside the total rather than as
    /// a rate, because a replay over three commits makes "67% clean" a number with
    /// no denominator worth quoting.
    pub verdicts_clean: usize,
    /// Commits whose reply carried **no** verdict in the required form, or whose
    /// verdict call failed.
    ///
    /// Counted and printed rather than absorbed, on exactly the argument
    /// [`ReplayReport::reasoning_truncated`] is counted on: "the model had nothing
    /// to push back on" and "the model never answered" are opposite facts about a
    /// reviewer, and a run that rendered them identically would report a clean
    /// summary rate it did not measure.
    pub verdicts_absent: usize,
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
    class_source: ClassSource,
    ingest: rto_graph::IngestConfig,
) -> anyhow::Result<()> {
    let corpus = rto_graph::review_corpus::builtin()?;
    let main = main_ref(repo)?;
    let checks = checks_with_notice(checks_path)?;

    let choice = rto_graph::resolve_model(rto_graph::ModelTask::Review)?;
    let model = choice.require_installed()?;
    eprintln!("reviewing with {model} — {}", choice.why());
    let engine = start_engine(model)?;

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
        arm: Some(run_arm(arm, model, class_source)),
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
        let set = files_at(repo, sha, &main, ingest.paths)?;
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
        // What this commit's per-file pass reported, handed back to the
        // whole-change pass below so it synthesises rather than repeats. Per
        // commit, not per run: a verdict is about one change.
        let mut reported: Vec<String> = Vec::new();
        for file in &set.files {
            // Every byte that reaches the model passes through this closure, so
            // the policy is asked **here** rather than only over the change set.
            // `context_for` follows a file's parent modules, which a filtered
            // change set does not cover: a narrow `exclude` naming one module
            // file would otherwise be read and sent as an admitted file's parent.
            // This is the last gate before egress, which is the one worth being
            // total.
            let sources = |p: &str| ingest.class(p).mines().then(|| blob_at(repo, sha, p))?;
            let context = context_for(graph.as_ref(), file, &sources)?;
            report.context_items += context.items.len();
            report.context_dropped += context.dropped_items;
            report.context_tokens += context.tokens();
            report.files_with_context += usize::from(!context.is_empty());
            // A file the engine refuses is recorded and stepped over, never fatal.
            // A three-hour pass that dies on file 140 has measured nothing, and the
            // refusals are themselves a result: they say which files this budget
            // cannot actually review.
            let outcome = match review_file(
                &engine,
                model,
                file,
                &context,
                &checks,
                &sources,
                class_source,
            ) {
                Ok(outcome) => outcome,
                Err(e) => {
                    eprintln!("      refused {}: {e}", file.path);
                    report.refused.push(file.path.clone());
                    continue;
                }
            };
            record_outcome(
                outcome,
                &file.path,
                anchors.contains(&(*sha, file.path.as_str())),
                &mut run,
                &mut report,
                &mut reported,
            );
        }

        record_verdict(
            &engine,
            model,
            sha,
            &set.files,
            &reported,
            &mut run,
            &mut report,
        );
    }

    let json = serde_json::to_string_pretty(&run)?;
    std::fs::write(out, format!("{json}\n")).map_err(|e| anyhow::anyhow!("writing {out}: {e}"))?;
    print_replay(&report, out);
    Ok(())
}

/// Read the `--checks` evidence, saying plainly on stderr when there is none.
///
/// The read and the notice are one function because the notice is *about* the
/// absence the read produced: `compile_claim` is opt-in on evidence, so an empty
/// set is the conservative default rather than a disabled filter, and a run whose
/// log does not say so reads as one where the filter was consulted.
///
/// # Errors
/// If `checks_path` names a file that cannot be read or parsed.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
fn checks_with_notice(checks_path: Option<&str>) -> anyhow::Result<Vec<CheckRun>> {
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
    Ok(checks)
}

/// The arm a run document records: the context, the model, and the class source
/// **only when it is not the default**.
///
/// A function rather than a literal at the one call site, because the "only when
/// not default" rule is the whole of the byte-identical guarantee on
/// `RunArm::class_source` and it needs somewhere a test can reach. It was written
/// as a literal first, recorded `Some("reply-text")` unconditionally, and broke
/// that guarantee on every default replay — while the serialisation test in
/// `rto-graph` constructed `None` by hand and so never touched the path that was
/// wrong. `the_default_arm_records_no_class_source` is the guard; the `rto-graph`
/// tests document the wire shape.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
fn run_arm(
    arm: ReviewArm,
    model: &str,
    class_source: ClassSource,
) -> rto_graph::review_score::RunArm {
    rto_graph::review_score::RunArm {
        context: arm.tag().to_owned(),
        model: model.to_owned(),
        // Absence means `reply-text`, which is also what a run predating the field
        // means — the same fact either way, so the collapse costs a reader nothing.
        class_source: (class_source != ClassSource::ReplyText)
            .then(|| class_source.tag().to_owned()),
    }
}

/// Start llama.cpp on `model` with this reviewer's context window.
///
/// One definition rather than the two identical copies [`run_llm`] and
/// [`run_replay`] carried: [`REVIEW_N_CTX`] is load-bearing — the first replay
/// died on file two because the engine defaulted to 4,096 — and a second copy is
/// a second place for it to be passed wrongly.
///
/// # Errors
/// If the engine cannot be started.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
fn start_engine(model: &str) -> anyhow::Result<rto_llama::llama::LlamaEngine> {
    rto_llama::llama::LlamaEngine::new(
        vec![rto_llama::llama::Served {
            name: model.to_owned(),
            path: rto_graph::model_dir(model).join("model.gguf"),
            mmproj: None,
        }],
        REVIEW_N_CTX,
    )
    .map_err(|e| anyhow::anyhow!("starting llama.cpp: {e}"))
}

/// Fold one file's outcome into the run document and the replay's counters, and
/// append its findings to `reported` in the form the whole-change pass is handed.
///
/// Suppressed findings are recorded in the run but **not** in `reported`: they
/// were withheld from the reader under [`rto_graph::compile_claim`], and a
/// verdict asked to synthesise a claim nobody was shown would be summarising
/// evidence the reader cannot check.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
fn record_outcome(
    outcome: FileOutcome,
    path: &str,
    anchored: bool,
    run: &mut CandidateRun,
    report: &mut ReplayReport,
    reported: &mut Vec<String>,
) {
    report.files += 1;
    report.findings += outcome.findings.len();
    report.suppressed += outcome.suppressed.len();
    report.unparsed += outcome.unparsed;
    report.clean += usize::from(outcome.declared_clean);
    report.truncated += usize::from(outcome.dropped_tokens > 0);
    report.reasoning_truncated += usize::from(outcome.reasoning_truncated);
    report.anchored_files += usize::from(anchored);
    for f in &outcome.findings {
        let class = f.defect_class.map_or("unclassified", |c| c.as_str());
        reported.push(format!("{path}:{} [{class}] {}", f.line, f.description));
    }
    run.findings.extend(outcome.findings);
    run.suppressed
        .extend(outcome.suppressed.into_iter().map(|(f, _)| f));
}

/// Print the whole-change verdict, labelled as an opinion in the same words the
/// model was told (issue #649, part 2).
///
/// **`None` is printed, not skipped.** A missing verdict and a `clean` one are
/// opposite facts, and silence would render as the first looking like the second
/// — the same failure `announce_unreviewable` and
/// [`FileOutcome::reasoning_truncated`] exist to prevent for files. A summary this
/// code invented would be indistinguishable from one a model formed.
///
/// `never_reviewed` is the count of files whose review never happened, said
/// *beside* the verdict rather than only above it: a judgement formed over an
/// incomplete pass is worth less than one formed over a complete one, and the
/// reader needs that where they read the judgement.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
fn print_verdict(
    verdict: Option<&rto_graph::review_score::CandidateVerdict>,
    never_reviewed: usize,
) {
    let Some(verdict) = verdict else {
        // Deliberately does **not** name a cause. `None` arrives from a reply that
        // carried no verdict, from one that stopped inside a reasoning block, and
        // from an engine that refused — and naming a cause a reader can check and
        // find false is worse than naming none. The engine's own error, when there
        // was one, has already gone to stderr where it belongs.
        println!(
            "\nno whole-change verdict: none was produced in the required form, so \
             there is no summary here rather than an assumed clean one."
        );
        return;
    };
    println!(
        "\nverdict on the whole change [{}] — ONE MODEL'S OPINION, not a gate: it \
         changes no exit status and is not a finding.",
        verdict.stance.as_str()
    );
    println!("  {}", verdict.summary);
    if never_reviewed > 0 {
        println!(
            "  Read it against the {never_reviewed} file(s) above that were never \
             reviewed: the verdict saw their diffs, but not a review of them."
        );
    }
}

/// Ask for one commit's whole-change verdict and record it in the run document
/// (issue #649, part 2).
///
/// One verdict per commit, so `--score` can adjudicate the summaries the same way
/// it adjudicates the findings. A commit with nothing reviewable gets none: a
/// verdict over no review would be an opinion about nothing, and the corpus would
/// score it as though it were about the change.
///
/// Never fatal, and never defaulted. A verdict the model did not form — because
/// the reply carried none, or because the engine refused — is counted as
/// **absent**, exactly as a file whose review was refused is recorded and stepped
/// over. Writing an invented `clean` into the run document would put a claim no
/// model made in front of the scorer.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
fn record_verdict(
    engine: &rto_llama::llama::LlamaEngine,
    model: &str,
    sha: &str,
    files: &[FileUnderReview],
    reported: &[String],
    run: &mut CandidateRun,
    report: &mut ReplayReport,
) {
    if files.is_empty() {
        return;
    }
    match verdict_on(engine, model, sha, files, reported) {
        Ok(Some(verdict)) => {
            report.verdicts += 1;
            report.verdicts_clean +=
                usize::from(verdict.stance == rto_graph::review_score::VerdictStance::Clean);
            run.verdicts.push(verdict);
        }
        Ok(None) => report.verdicts_absent += 1,
        Err(e) => {
            eprintln!("      no verdict for {}: {e}", &sha[..8.min(sha.len())]);
            report.verdicts_absent += 1;
        }
    }
}

#[cfg(any(feature = "serve", feature = "inference-local-models"))]
/// Print what a replay covered — **unadjudicated volume first**.
///
/// The order is the argument. Recall is what a score reports, but 22 adjudicated
/// rows sit across 176 reviewable files, so the great majority of what a reviewer
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
    // The absent count is on the same line as the total, never below it: a
    // whole-change verdict that never arrived and one that said `clean` are
    // opposite facts, and only the first is silence.
    println!(
        "  {} whole-change verdict(s), {} of them `clean`; {} commit(s) produced \
         none in the required form and are recorded as ABSENT, never as clean",
        report.verdicts, report.verdicts_clean, report.verdicts_absent
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
/// Resolve `--base` for the LLM arm the way the graph arm resolves it, and
/// return the **commit** everything downstream should key off (issue #649).
///
/// The graph arm learned this in part 4 and the `--llm` arm had the identical
/// blind spot one layer down: `changed_files` shells out to `git diff
/// --name-only <base> HEAD`, so a bare `main` binds to the **local** branch, and
/// a stale one reviews a superset of the change — silently, because a superset
/// still contains everything of yours. Rebasing does not help; rebasing the
/// branch does not move the local ref.
///
/// Returning the resolved commit rather than the spec means the file set, the
/// diffs sent to the model and the trailer range are all one answer to one
/// question, and [`crate::warn_about_stale_base`] says so on stderr in the same
/// words `review` already uses.
///
/// # Errors
/// If the repository cannot be opened or the spec names no single commit.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
fn resolve_llm_base(repo: &Path, base: Option<&str>) -> anyhow::Result<Option<String>> {
    let Some(spec) = base else {
        return Ok(None);
    };
    let resolved = rto_graph::Repo::discover(repo)?.resolve_base(spec)?;
    crate::warn_about_stale_base(&resolved);
    Ok(Some(resolved.commit))
}

/// Say on **stderr** when the model about to review the change is also the model
/// named in its `Co-Authored-By` trailers (issue #649, part 3).
///
/// # Warn, never refuse
///
/// A model reviewing its own output shares the blind spot that produced the
/// defect, so the warning is worth printing — but it still finds things, and
/// refusing would trade a weakened review for **no** review on exactly the
/// machine most likely to have one model installed. The failure mode of refusing
/// is silent: people stop running it. So this returns nothing and gates nothing.
///
/// # stderr, and not a finding
///
/// Two separate constraints, both deliberate. **stderr**, so it cannot corrupt a
/// `--json` document on stdout. And it never enters [`FileOutcome::findings`],
/// because a run document carrying it would be scored against the corpus as
/// though it were a defect the reviewer *detected* — a warning about provenance
/// counted as recall.
///
/// # A working-tree review has no trailers, and that is not a special case
///
/// Uncommitted work carries no commit message, so there is nothing to read and
/// nothing is printed. That is the same silence a human-authored commit
/// produces, arrived at by the same route: no trailer, no match, no warning.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
fn warn_if_reviewing_own_work(repo: &Path, base: Option<&str>, model: &str) {
    let Some(base) = base else {
        return;
    };
    // `%B` is the raw subject and body, NUL-separated so a message containing
    // blank lines cannot be split into two.
    let range = format!("{base}..HEAD");
    let Some(log) = git(repo, &["log", "--format=%B%x00", &range]) else {
        return;
    };
    // Empties dropped, not merely trimmed: `git log` writes a newline after each
    // record, so the split yields a trailing "" and every message after the first
    // arrives with a leading one. Left in, they would inflate the "of N commits"
    // denominator by one on every range — a wrong number in the warning's own
    // sentence.
    let messages: Vec<String> = log
        .split('\0')
        .map(|m| m.trim().to_owned())
        .filter(|m| !m.is_empty())
        .collect();
    let own = rto_graph::authorship::reviewers_own_work(&messages, model);
    if own.is_empty() {
        return;
    }
    eprintln!(
        "warning: {model} is reviewing its own work — {} of {} commit(s) in {range} \
         name it as co-author (as {}). A model reviewing code it wrote shares the \
         blind spot that produced the defect, so read a clean result here as weaker \
         evidence than usual. This is a warning, not a gate: a weakened review \
         beats no review, and nothing about it changes the exit status or enters \
         the findings.",
        own.commits,
        messages.len(),
        own.names.join(", "),
    );
}

/// Read a working-tree file's text, **or refuse it** because the repository
/// declared its path out of the scan (ADR-0007 `[paths]`).
///
/// A named function rather than an inline closure because it is the **last gate
/// before egress**: everything `context_for` and `review_file` put in front of a
/// model passes through here, and a rule that decides what leaves the machine
/// should be findable by name. Filtering the change set is not sufficient on its
/// own — `context_for` follows a file's *parent modules*, so a narrow `exclude`
/// naming one module file would otherwise be read and sent as an admitted file's
/// parent, which a filtered file set does not cover.
///
/// It refuses `opaque` as well as `excluded`: an opaque path's bytes may be
/// measured, never mined, and handing them to a model is mining by any reading.
#[cfg(feature = "inference-local-models")]
fn worktree_sources<'a>(
    repo: &'a Path,
    paths: &'a rto_graph::PathPolicy,
) -> impl Fn(&str) -> Option<String> + 'a {
    move |p: &str| {
        paths
            .classify(p)
            .mines()
            .then(|| std::fs::read_to_string(repo.join(p)).ok())?
    }
}

#[cfg(any(feature = "serve", feature = "inference-local-models"))]
fn changed_files(repo: &Path, base: Option<&str>, paths: &rto_graph::PathPolicy) -> ReviewSet {
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
    // The policy is applied inside `collect`, not here: both this path and the
    // replay path build their diffs there, and one rule at the shared chokepoint
    // is what stops a second route being opened without one.
    ReviewSet::collect(&head, &names, paths, &|path| {
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

/// Print one file's findings and its withheld compile claims, appending each
/// *printed* finding to `reported` in the shape the whole-change pass is handed.
///
/// Returns `(findings printed, claims withheld)`.
///
/// # The shape numbers are shown when there are any
///
/// Under `--typed-class` a finding carries a sharpness, an option mass and a
/// margin, and the replay path records all three into the run document. The live
/// path writes no document, so printing them is the **only** way a person who
/// asked for a typed read can see what it produced — without it the flag is
/// indistinguishable from the default except by reading the source. They are
/// printed only when present, so the default output is byte-for-byte what it was.
///
/// The margin is given in nats and the other two as fractions, because that is
/// what they are; and the mass is worth reading first, since a sharpness over a
/// question the model never engaged with is a number about nothing.
///
/// A withheld claim is printed and counted but **not** appended: it was withheld
/// from the reader under `rto_graph::compile_claim`, and a verdict asked to
/// synthesise a claim nobody was shown would be summarising evidence the reader
/// cannot check. That is the same rule [`record_outcome`] follows for the replay,
/// and it is stated in both places because the two paths build `reported`
/// independently.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
fn print_file_findings(
    path: &str,
    outcome: &FileOutcome,
    reported: &mut Vec<String>,
) -> (usize, usize) {
    println!("\n{path}");
    for f in &outcome.findings {
        let class = f.defect_class.map_or("unclassified", |c| c.as_str());
        println!("  {path}:{}  [{class}]  {}", f.line, f.description);
        if let Some(margin) = f.class_margin_micronats {
            let scale = f64::from(rto_graph::review_score::PPM_SCALE);
            let frac = |v: Option<u32>| v.map_or(f64::NAN, |x| f64::from(x) / scale);
            println!(
                // The scope, not a hedge: "may be unreliable" is what lets
                // somebody use it anyway. The margin was measured at P = 0.44
                // against a random ordering *on findings like these*, so the
                // actionable sentence is that it does not rank them.
                "      typed class: margin {:.2} nats, sharpness {:.4}, \
                 option mass {:.4} — none is a confidence, and the margin does \
                 NOT rank self-authored findings (measured P = 0.44 vs random)",
                f64::from(margin) / scale,
                frac(f.class_sharpness_ppm),
                frac(f.class_option_mass_ppm),
            );
        }
        reported.push(format!("{path}:{} [{class}] {}", f.line, f.description));
    }
    for (f, reason) in &outcome.suppressed {
        println!("  {path}:{}  [withheld]  {}", f.line, f.description);
        println!("      {reason}");
    }
    (outcome.findings.len(), outcome.suppressed.len())
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
    class_source: ClassSource,
    ingest: rto_graph::IngestConfig,
) -> anyhow::Result<()> {
    let checks = match checks_path {
        Some(p) => read_checks(p)?,
        None => Vec::new(),
    };
    // Resolved once, before anything reads it, so the file set, the diffs and the
    // trailer range below all name the same commit.
    let base = resolve_llm_base(repo, base)?;
    let base = base.as_deref();
    let ReviewSet { files, skipped } = changed_files(repo, base, ingest.paths);

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
    // Before the review rather than after it: the reader needs to know how to
    // weigh the result while they are reading it, not once they have already
    // formed a view of a clean one.
    warn_if_reviewing_own_work(repo, base, model);
    if cfg!(debug_assertions) {
        eprintln!(
            "note: unoptimized build — local generation is very slow; use a \
             release build (`cargo build --release`) for usable speed."
        );
    }
    let engine = start_engine(model)?;

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
    // What the per-file pass said, in the shape the whole-change pass is handed
    // back so it can synthesise rather than repeat. Collected in the loop rather
    // than reconstructed afterwards, so the two can never disagree about what was
    // reported.
    let mut reported: Vec<String> = Vec::new();
    for file in &files {
        let sources = worktree_sources(repo, ingest.paths);
        let context = context_for(graph.as_ref(), file, &sources)?;
        let outcome = review_file(
            &engine,
            model,
            file,
            &context,
            &checks,
            &sources,
            class_source,
        )?;
        if outcome.reasoning_truncated {
            never_reviewed.push(file.path.as_str());
        }
        if outcome.findings.is_empty() && outcome.suppressed.is_empty() {
            continue;
        }
        let (printed, suppressed) = print_file_findings(&file.path, &outcome, &mut reported);
        total += printed;
        withheld += suppressed;
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

    // The whole-change verdict (#649, part 2), after the per-file findings
    // because it is a synthesis of them and the diffs.
    let sha = files
        .first()
        .map_or_else(|| "HEAD".to_owned(), |f| f.reviewed_sha.clone());
    // **Not `?`.** An engine refusal here would otherwise throw away a whole
    // completed review at its last step — the same argument `run_replay` makes
    // for stepping over a refused file, and worse in this direction, because
    // every per-file finding above has already been earned and printed. The
    // failure is reported on stderr and the verdict is recorded as absent.
    let verdict = verdict_on(&engine, model, &sha, &files, &reported).unwrap_or_else(|e| {
        eprintln!("warning: the whole-change verdict could not be generated: {e}");
        None
    });
    print_verdict(verdict.as_ref(), never_reviewed.len());

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

    /// **A default run's arm records no class source, so its document is
    /// byte-identical to one written before the field existed.**
    ///
    /// Built through [`super::run_arm`], which is what `run_replay` uses — not by
    /// hand. The first version of this guarantee was tested by constructing
    /// `class_source: None` directly in `rto-graph`, which asserted the wire shape
    /// and could not see that the production path set `Some("reply-text")` on
    /// every default replay. A test that does not contain the difference cannot
    /// find it.
    ///
    /// Gated like the items it exercises: `run_arm` and `ClassSource` are both
    /// behind the generation-backend feature, so in a build without one there is
    /// no production path for this to guard. Left ungated it compiled at
    /// `--all-features` and at `--features serve`, and broke **every other CI
    /// cell** — neither of those is the default build.
    #[cfg(any(feature = "serve", feature = "inference-local-models"))]
    #[test]
    fn the_default_arm_records_no_class_source() {
        let arm = super::run_arm(ReviewArm::DiffOnly, "m", super::ClassSource::ReplyText);
        assert_eq!(
            arm.class_source, None,
            "the default arm records a class source"
        );
        let json = serde_json::to_string(&arm).expect("serialize");
        assert!(
            !json.contains("class_source"),
            "a default replay's document is no longer byte-identical: {json}"
        );
        let typed = super::run_arm(ReviewArm::DiffOnly, "m", super::ClassSource::TypedRead);
        assert_eq!(
            typed.class_source.as_deref(),
            Some("typed-read"),
            "a typed run must record what varied, or the artifact cannot be audited"
        );
    }

    /// **`[paths] exclude` is an egress control on this command, not only a graph
    /// filter — and the replay path must honour it too.**
    ///
    /// `review --llm` is different in kind from every other reader of the policy:
    /// it does not *store* what it reads, it **sends** it. A user who writes
    /// `exclude = ["raw/**"]` over a corpus of third-party documents will
    /// reasonably take that to mean those bytes do not leave the machine, and a
    /// filter that holds on one route into the model and not another reads as a
    /// guarantee while not being one — which is worse than no filter at all.
    ///
    /// The live path filters its **names** before any diff is built. The replay
    /// path reached the model by a different route: [`files_at`] reconstructs a
    /// unified diff for every changed name and hands `FileUnderReview::diff`
    /// straight to `review_file`, which embeds it in the prompt
    /// (`rto_graph::reviewer::build_prompt` → `annotate_diff(&file.diff)`).
    /// Gating the `sources` closure does not touch that: the diff is already in
    /// hand before any source is read.
    ///
    /// Asserted on the **diff text**, not merely on the path list, because the
    /// path list is not what egresses.
    #[test]
    fn the_replay_path_builds_no_diff_for_a_declared_path() {
        let dir =
            std::env::temp_dir().join(format!("roteiro-replay-egress-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("mkdir");
        let git_at = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .args([
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.com",
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "init.defaultBranch=main",
                ])
                .args(args)
                .current_dir(&dir)
                .status()
                .expect("run git");
            assert!(ok.success(), "git {args:?}");
        };
        let write = |rel: &str, body: &str| {
            let path = dir.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir");
            std::fs::write(path, body).expect("write");
        };

        git_at(&["init", "-q"]);
        write("src/lib.rs", "pub struct Thing;\n");
        git_at(&["add", "."]);
        git_at(&["commit", "-q", "-m", "base"]);
        git_at(&["checkout", "-q", "-b", "work"]);
        // One admitted file and one the policy will name. The excluded file's
        // body is the secret: if it appears in any diff, it would have been sent.
        write("src/lib.rs", "pub struct Thing;\npub struct Two;\n");
        write("raw/paper.md", "CONFIDENTIAL-CORPUS-BODY\n");
        git_at(&["add", "."]);
        git_at(&["commit", "-q", "-m", "work"]);

        let sha = String::from_utf8(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(["rev-parse", "HEAD"])
                .output()
                .expect("rev-parse")
                .stdout,
        )
        .expect("utf8");
        let sha = sha.trim();

        let policy = rto_graph::PathPolicy::new(vec!["raw/**".to_owned()], Vec::new());

        // Without a declaration the corpus body is in the reviewable set — or
        // this test is measuring nothing.
        let open =
            files_at(&dir, sha, "main", rto_graph::PathPolicy::empty()).expect("reconstructs");
        assert!(
            open.files
                .iter()
                .any(|f| f.diff.contains("CONFIDENTIAL-CORPUS-BODY")),
            "the corpus body is reviewable without a declaration"
        );

        let guarded = files_at(&dir, sha, "main", &policy).expect("reconstructs");
        assert!(
            !guarded
                .files
                .iter()
                .any(|f| f.diff.contains("CONFIDENTIAL-CORPUS-BODY")),
            "an excluded path's bytes must not reach a reviewable diff: {:?}",
            guarded.files.iter().map(|f| &f.path).collect::<Vec<_>>()
        );
        assert!(
            !guarded.files.iter().any(|f| f.path.starts_with("raw/")),
            "nor its name"
        );
        // The negative: the admitted file is still reviewed, so the gate is a
        // filter rather than an off switch.
        assert!(
            guarded.files.iter().any(|f| f.path == "src/lib.rs"),
            "an admitted file is still reviewed: {:?}",
            guarded.files.iter().map(|f| &f.path).collect::<Vec<_>>()
        );
        // And it is not silently dropped into `skipped`, which announces files as
        // unreviewable-but-present; an excluded path is not in the change at all.
        assert!(
            !guarded.skipped.iter().any(|p| p.starts_with("raw/")),
            "an excluded path is absent, not announced as skipped: {:?}",
            guarded.skipped
        );

        std::fs::remove_dir_all(&dir).ok();
    }
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    /// This repository's root, from the crate that is being tested.
    fn repo() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    /// Why a corpus test cannot run here, and what to do about it.
    ///
    /// The remedy travels with the reason because one shared remedy was wrong for
    /// most of them: the old skip told every reader to run `git fetch --unshallow`,
    /// which does nothing for a checkout that is not a work tree and does not create
    /// a missing `origin/main`. The three history reasons are mirrored in
    /// `rto-graph`'s `tests/review_corpus.rs`, which gates the same corpus and must
    /// be changed with this; the last two are this module's own, because only these
    /// tests build a graph.
    #[derive(Debug, Clone, Copy)]
    enum CannotRun<'a> {
        NotAWorkTree,
        Shallow,
        NoMainRef,
        /// Carries the error, because the remedy below cites it.
        NoObjectCache(&'a str),
        /// Likewise.
        NoWorktreeGraph(&'a str),
    }

    impl CannotRun<'_> {
        fn reason(self) -> String {
            match self {
                Self::NotAWorkTree => {
                    "not a git work tree, or git could not be run here".to_owned()
                }
                Self::Shallow => "shallow clone".to_owned(),
                Self::NoMainRef => "neither origin/main nor main resolves here".to_owned(),
                // `graph_inputs` can fail at `Repo::discover` as well as at
                // `ObjectCache::open`, and its message says which — so this names
                // the pair rather than mislabelling a discovery failure as a cache
                // one.
                Self::NoObjectCache(e) => {
                    format!("the repository or its object cache could not be opened ({e})")
                }
                Self::NoWorktreeGraph(e) => {
                    format!("the working-tree graph could not be assembled ({e})")
                }
            }
        }

        fn remedy(self) -> &'static str {
            match self {
                Self::NotAWorkTree => "Run these from a checkout of the repository.",
                Self::Shallow => "`git fetch --unshallow` runs them.",
                Self::NoMainRef => {
                    "Fetch the default branch (`git fetch origin \
                     main:refs/remotes/origin/main`) — unshallowing alone does not create \
                     that ref."
                }
                Self::NoObjectCache(_) | Self::NoWorktreeGraph(_) => {
                    "This is a failure rather than a missing precondition, and the \
                     reason is quoted above: a checkout that cannot build its own graph \
                     cannot measure a reviewer either."
                }
            }
        }
    }

    /// This repository's path, as it appears in a clone's remote URL — the one
    /// signal a vendored copy cannot present, because a consumer's remote is their
    /// own. Used by the two guards below to decide whether they are looking at
    /// *this* repository.
    const REPOSITORY_PATH: &str = "OffeneDatenmodellierung/Roteiro";

    /// Whether the checkout at `repo` was cloned from this repository.
    ///
    /// Independent of everything the guards check: not the manifest (a vendored
    /// crate sits under a consumer's), not the layout (the corpus fixture ships
    /// inside the package, so `consumer/crates/rto-graph/tests/fixtures/…` exists
    /// too), and not the marker itself, which is the thing being held. A fork
    /// reports its own path and so declines to assert, which is the right way for a
    /// guard to fail. Mirrors `rto-graph`'s copy in `tests/review_corpus.rs`.
    fn cloned_from_this_repository(repo: &Path) -> bool {
        std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["remote", "get-url", "origin"])
            .output()
            .is_ok_and(|o| {
                o.status.success() && String::from_utf8_lossy(&o.stdout).contains(REPOSITORY_PATH)
            })
    }

    /// This workspace manifest's **own** `repository =` line. Matched as a whole
    /// line rather than searched for: a consumer that depends on Roteiro by git URL
    /// has that URL in its manifest too, and `contains` would call their project
    /// ours — see [`is_repository_checkout`].
    const REPOSITORY_FIELD: &str =
        "repository = \"https://github.com/OffeneDatenmodellierung/Roteiro\"";

    /// The manifest rule, as a pure function of the text — the half that can be
    /// tested against manifests this checkout does not contain.
    ///
    /// **Both conditions are whole-line matches, and the second is the interesting
    /// one.** A consumer that depends on Roteiro by git URL has our URL in its
    /// manifest, so a `contains` search would call their project ours and run our
    /// corpus gates on their CI. Matching the manifest's own `repository =` line
    /// tells "this is Roteiro" apart from "this uses Roteiro". Kept in step with
    /// `rto-graph`'s copy in `tests/review_corpus.rs`, which has the same guard.
    fn manifest_is_ours(text: &str) -> bool {
        text.lines().any(|line| line.trim() == "[workspace]")
            && text.lines().any(|line| line.trim() == REPOSITORY_FIELD)
    }

    /// Whether this is **this repository's** checkout rather than a packaged crate.
    ///
    /// **The reason the CI rule below is not simply "never skip".** `roteiro` is
    /// published, and these tests ship inside the package — a downstream `cargo test`
    /// on the unpacked crate runs them against a directory that is not this
    /// repository, usually with `CI=true` set. Failing there would be our defect
    /// reaching somebody who did nothing wrong, so a package skips even on a runner;
    /// a real checkout that is missing history does not.
    ///
    /// **Two signals, because one is not enough.** The shape follows
    /// `crates/roteiro/tests/common/mod.rs`, the canonical copy, which reads the
    /// workspace manifest two levels up — but a crate vendored into *another*
    /// project sits two levels under *that* project's root, whose manifest may well
    /// say `[workspace]` too, and this guard would then treat a stranger's
    /// repository as ours and fail their CI. So the manifest must also name this
    /// repository. It cannot be imported here — that module serves the integration
    /// tests, and this is a unit-test module inside the binary — so this is a
    /// deliberate transcription, kept in step with `rto-graph`'s copy in
    /// `tests/review_corpus.rs`, whose
    /// `the_package_exemption_cannot_claim_a_real_checkout` has a twin below.
    /// **Only `NotFound` means "packaged"**: collapsing every IO error into that
    /// would turn "cannot read the repository" into "this is not a repository",
    /// which is the same vacuity one level up.
    fn is_repository_checkout(repo: &Path) -> bool {
        let manifest = repo.join("Cargo.toml");
        match std::fs::read_to_string(&manifest) {
            Ok(text) => manifest_is_ours(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => panic!(
                "cannot read {} ({:?}: {e}). Without it a guard cannot tell a packaged \
                 crate from a repository checkout, and guessing would make it skip in \
                 silence — which is the failure these guards exist to rule out.",
                manifest.display(),
                e.kind(),
            ),
        }
    }

    /// What a skip withheld, for the tests that walk every corpus commit. Named
    /// rather than written out at each call site, because the message is a claim
    /// about what did not happen and two copies of a claim drift.
    const GRAPH_AT_EVERY_CORPUS_COMMIT: &str = "The graph at every corpus commit";
    /// The same, for the one test that compares the live surface with the replay and
    /// reconstructs no corpus commit at all — the message used to tell it that
    /// corpus commits went unreconstructed, which was simply not what it does.
    const LIVE_VS_REPLAY_COMPARISON: &str = "The live-surface-versus-replay graph comparison";

    /// A skip that can be found in a log, written to **real** stderr — and **never
    /// taken on a runner**, where every one of these reasons is an environment
    /// defect rather than a property of the checkout.
    ///
    /// `eprintln!` goes through libtest's capture, which discards the output of a
    /// test that *passes* — so the `SKIP:` lines this used to print were unreadable
    /// without `--nocapture`, and on CI nobody ever read them. `std::io::stderr()`
    /// writes to the file descriptor, which the capture does not intercept. What
    /// went unchecked is part of the message on purpose: a skip that does not say
    /// how much it withheld reads exactly like a pass, which is how all six tests
    /// gated on this reported green on every CI run while never reconstructing a
    /// single commit (#822).
    ///
    /// The CI half covers every reason, not only shallowness: a deep checkout with
    /// no `origin/main`, a `git` that cannot be executed, or a graph that will not
    /// assemble would each skip its way to green by the identical mechanism.
    ///
    /// **A packaged crate is the one exemption**, and it is not a loophole in the
    /// rule but the boundary of what the rule is about: a published tarball cannot
    /// contain this repository's history, so failing there would be our defect
    /// landing on somebody who did nothing wrong. See [`is_repository_checkout`].
    fn loud_skip_unless_on_ci(test: &str, why: CannotRun<'_>, unchecked: &str) {
        use std::io::Write;

        let on_ci =
            std::env::var_os("CI").is_some() || std::env::var_os("GITHUB_ACTIONS").is_some();
        assert!(
            !on_ci || !is_repository_checkout(&repo()),
            "review_llm::tests::{test} cannot run on CI: {}. This is half of the \
             corpus's gate and must not be skipped in a checkout — \
             `.github/workflows/ci.yml` checks out with `fetch-depth: 0` in every job \
             that runs tests, so fix the environment rather than widening this skip, \
             which is how the corpus replay went unrun on every CI run until #822. {}",
            why.reason(),
            why.remedy()
        );
        let mut err = std::io::stderr().lock();
        let _ = writeln!(
            err,
            "SKIP: review_llm::tests::{test} — {}. {unchecked} went unchecked. {} In a \
             repository checkout on CI this is a failure rather than a skip \
             (`.github/workflows/ci.yml` checks out with `fetch-depth: 0` in every \
             job that runs tests), so on a runner this line means the crate is \
             packaged.",
            why.reason(),
            why.remedy()
        );
        let _ = err.flush();
    }

    /// Whether the git history this test needs is present.
    ///
    /// The skip was modelled on `dependency_axis.rs`'s OSV gate, and for a missing
    /// database that shape is right: a shallow clone is a property of the checkout,
    /// never of the code. What made it wrong here is that it was *always* taken on
    /// CI — every `actions/checkout` in `ci.yml` used the action's default
    /// `fetch-depth: 1` — so the six tests gated on it were invoked, returned
    /// immediately, and reported green for their whole existence, including while
    /// two of the corpus's `reviewed_sha` values no longer existed anywhere (#822).
    ///
    /// Returns `false` off a runner, and on one **only for a packaged crate** — in a
    /// repository checkout a missing precondition panics instead. See
    /// [`loud_skip_unless_on_ci`], which is where that split lives.
    fn history_available(repo: &Path, test: &str, unchecked: &str) -> bool {
        // The **output**, not the exit status: `--is-inside-work-tree` exits 0 in a
        // bare repository and prints `false`, so a status-only check calls a bare
        // repo a work tree and then fails later for a reason it cannot explain.
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["rev-parse", "--is-inside-work-tree"])
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "true");
        if !ok {
            loud_skip_unless_on_ci(test, CannotRun::NotAWorkTree, unchecked);
            return false;
        }
        let shallow = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["rev-parse", "--is-shallow-repository"])
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "true");
        if shallow {
            loud_skip_unless_on_ci(test, CannotRun::Shallow, unchecked);
            return false;
        }
        if main_ref(repo).is_err() {
            loud_skip_unless_on_ci(test, CannotRun::NoMainRef, unchecked);
            return false;
        }
        true
    }

    /// Open this repository and the shared, content-addressed object cache the
    /// per-commit graph builds hit.
    /// **Carries its error rather than dropping it.** This used to end in `.ok()?`,
    /// so a failure to open the repository or its cache arrived at the skip as a
    /// bare `None` — and the skip then told the reader to "read the error above it",
    /// which was not there. A diagnostic that promises evidence it discarded is the
    /// same defect as a skip nobody can see.
    fn graph_inputs() -> Result<(rto_graph::Repo, rto_graph::ObjectCache), String> {
        let repo = rto_graph::Repo::discover(&repo())
            .map_err(|e| format!("cannot discover the repository: {e}"))?;
        let cache = rto_graph::ObjectCache::open(repo.common_dir().join("roteiro").join("objects"))
            .map_err(|e| format!("cannot open the object cache: {e}"))?;
        Ok((repo, cache))
    }

    /// **The manifest rule, held against manifests that are not ours** — the twin of
    /// `rto-graph`'s `the_manifest_rule_accepts_only_this_repository`.
    ///
    /// The real-checkout assertion below cannot catch a rule that is too *loose*:
    /// this repository satisfies a substring search just as well as a whole-line
    /// match, so reverting to `contains` would leave the suite green and put these
    /// gates back on the CI of anyone who depends on Roteiro by git URL.
    #[test]
    fn the_manifest_rule_accepts_only_this_repository() {
        // The live manifest is only checked where the remote says this checkout is
        // ours. A packaged crate has no manifest there; a crate vendored under a
        // consumer has *their* manifest there, and asserting on it would fail their
        // `cargo test` for being correctly packaged. The three synthetic cases below
        // are the rule and run everywhere.
        if cloned_from_this_repository(&repo()) {
            let ours = std::fs::read_to_string(repo().join("Cargo.toml"))
                .expect("a checkout of this repository has a workspace manifest");
            assert!(
                manifest_is_ours(&ours),
                "our own workspace manifest no longer satisfies the rule — every CI \
                 run would now take the packaged exemption and skip these gates in \
                 silence, so this is the marker's problem, not this assertion's"
            );
        }

        let url = REPOSITORY_FIELD
            .trim_start_matches("repository = ")
            .trim_matches('"');
        let consumer_depending_on_us = format!(
            "[workspace]\nmembers = [\"app\"]\n\n[dependencies]\n\
             roteiro = {{ git = \"{url}\" }}\n"
        );
        assert!(
            !manifest_is_ours(&consumer_depending_on_us),
            "a workspace that DEPENDS on Roteiro is not Roteiro; a substring search \
             cannot tell those apart, which is why the rule matches whole lines"
        );

        assert!(
            !manifest_is_ours("[workspace]\nmembers = [\"app\"]\n"),
            "a plain consumer workspace is not this repository either"
        );

        // The fourth case lives in the IO half rather than in the rule: no manifest
        // at all is what an unpacked crate looks like, and it must read as
        // "packaged" rather than as an error.
        let empty =
            std::env::temp_dir().join(format!("roteiro-no-manifest-llm-{}", std::process::id()));
        std::fs::create_dir_all(&empty).expect("create an empty directory");
        assert!(
            !is_repository_checkout(&empty),
            "a directory with no manifest is a packaged crate, not this repository"
        );
        std::fs::remove_dir_all(&empty).ok();
    }

    /// **The exemption may not switch the rule off** — the twin of
    /// `rto-graph`'s `the_package_exemption_cannot_claim_a_real_checkout`, because
    /// this crate carries its own copy of the marker and a copy that drifts would
    /// let all six gates below skip on CI while the other crate's stayed green.
    ///
    /// Only the dangerous direction is asserted, and only where two signals
    /// independent of the marker agree that this is our checkout: git reports a work
    /// tree **and** the corpus fixture sits at its own path. Either alone is not
    /// enough — a vendored copy inside somebody else's repository has a work tree.
    /// In a packaged crate it is vacuous by construction, which is correct there and
    /// is said rather than hidden.
    #[test]
    fn the_package_exemption_cannot_claim_a_real_checkout() {
        let repo = repo();
        let work_tree = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["rev-parse", "--is-inside-work-tree"])
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "true");
        // Two signals, for the reason `rto-graph`'s twin gives: a vendored copy inside
        // somebody's repository has a work tree too, and the marker rightly says
        // "packaged" there. The layout cannot be the second signal — the corpus
        // fixture ships in the package — so the remote is.
        if work_tree && cloned_from_this_repository(&repo) {
            assert!(
                is_repository_checkout(&repo),
                "git reports a work tree at {} but the package marker says this is a \
                 packaged crate. That combination would let every CI run skip these \
                 gates silently — which is exactly #822 — so the marker, not this \
                 assertion, is what needs fixing",
                repo.display()
            );
        }
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
        if !history_available(
            &repo_path,
            "the_graph_arm_is_built_at_the_reviewed_commit_not_at_head",
            GRAPH_AT_EVERY_CORPUS_COMMIT,
        ) {
            return;
        }
        let (repo, cache) = match graph_inputs() {
            Ok(inputs) => inputs,
            Err(why) => {
                loud_skip_unless_on_ci(
                    "the_graph_arm_is_built_at_the_reviewed_commit_not_at_head",
                    CannotRun::NoObjectCache(&why),
                    GRAPH_AT_EVERY_CORPUS_COMMIT,
                );
                return;
            }
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
        if !history_available(
            &repo_path,
            "the_live_surface_builds_the_same_graph_as_the_replay",
            LIVE_VS_REPLAY_COMPARISON,
        ) {
            return;
        }
        let store = match super::worktree_graph(&repo_path, rto_graph::IngestConfig::default()) {
            Ok(store) => store,
            Err(e) => {
                loud_skip_unless_on_ci(
                    "the_live_surface_builds_the_same_graph_as_the_replay",
                    CannotRun::NoWorktreeGraph(&e.to_string()),
                    LIVE_VS_REPLAY_COMPARISON,
                );
                return;
            }
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
        if !history_available(
            &repo_path,
            "the_graph_arm_supplies_provenance_tagged_context_on_the_corpus",
            GRAPH_AT_EVERY_CORPUS_COMMIT,
        ) {
            return;
        }
        let (repo, cache) = match graph_inputs() {
            Ok(inputs) => inputs,
            Err(why) => {
                loud_skip_unless_on_ci(
                    "the_graph_arm_supplies_provenance_tagged_context_on_the_corpus",
                    CannotRun::NoObjectCache(&why),
                    GRAPH_AT_EVERY_CORPUS_COMMIT,
                );
                return;
            }
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
            let set = files_at(&repo_path, sha, &main, rto_graph::PathPolicy::empty())
                .expect("the diff reconstructs");
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
        if !history_available(
            &repo,
            "every_corpus_commit_reconstructs_a_diff_touching_its_anchor",
            "The diff reconstruction at every corpus commit",
        ) {
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
            let set = files_at(&repo, sha, &main, rto_graph::PathPolicy::empty())
                .expect("the diff reconstructs");
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
    /// the corpus's 182 changed paths are binary audio fixtures with no reviewable
    /// diff. Skipping them is free only while that stays true — a skip rule that
    /// quietly excluded a file carrying a real defect would raise recall by
    /// shrinking its own denominator, which is the most flattering mistake
    /// available here.
    #[test]
    fn nothing_the_harness_skips_carries_an_adjudicated_row() {
        let repo = repo();
        if !history_available(
            &repo,
            "nothing_the_harness_skips_carries_an_adjudicated_row",
            "The unreviewable-path check over every corpus commit",
        ) {
            return;
        }
        let main = main_ref(&repo).expect("checked above");
        let corpus = rto_graph::review_corpus::builtin().expect("parses");
        let mut skipped_total = 0;
        for sha in corpus_shas() {
            let set =
                files_at(&repo, &sha, &main, rto_graph::PathPolicy::empty()).expect("reconstructs");
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
    /// quietly reviews half the corpus is caught. The replay window is **182
    /// changed paths on 15 commits, of which 176 are reviewable** — both halves
    /// are asserted, because a drop in either is a different bug.
    ///
    /// These numbers move when the **corpus** gains a commit, which is the guard
    /// working rather than failing: the replay window is the corpus's distinct
    /// `reviewed_sha` values, so adjudicating a comment on a new commit widens
    /// it. #736 took it from 15 commits and 190 paths to 16 and 197.
    ///
    /// #822 took it back to 15 and 182, and that is the one direction worth
    /// explaining, because a *shrinking* window is what this assertion is for.
    /// Two of those 16 commits were PR #293's pre-force-push shas, which no
    /// longer exist in the repository or the remote; they are re-pinned to the
    /// surviving rebased commits `ab3b1bc` and `fec606e`, and `fec606e` was
    /// already a `reviewed_sha` here — so two review commits collapsed into one
    /// the window already held, and one commit's worth of changed paths left it.
    /// The 197/191 pair was honest when #736 measured it and became
    /// unverifiable, not wrong, when those objects were lost: this assertion has
    /// never run on CI (every checkout was shallow, so `history_available`
    /// returned early), so nothing outside a developer's own clone re-checked it.
    #[test]
    fn a_replay_covers_the_measured_number_of_files() {
        let repo = repo();
        if !history_available(
            &repo,
            "a_replay_covers_the_measured_number_of_files",
            "The measured replay window (commits, changed paths, reviewable paths)",
        ) {
            return;
        }
        let main = main_ref(&repo).expect("checked above");
        let (mut reviewable, mut changed) = (0usize, 0usize);
        for sha in corpus_shas() {
            let set =
                files_at(&repo, &sha, &main, rto_graph::PathPolicy::empty()).expect("reconstructs");
            reviewable += set.files.len();
            changed += set.files.len() + set.skipped.len();
        }
        assert_eq!(
            (corpus_shas().len(), changed, reviewable),
            (15, 182, 176),
            "15 commits, 182 changed paths, 176 with a reviewable diff"
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

        let set = ReviewSet::collect("deadbeef", names, rto_graph::PathPolicy::empty(), &|p| {
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

/// **Does the typed class read's `sharpness` know when it is wrong?** (Issue
/// #897.)
///
/// The cheap half of the #897 measurement, and the one that has to come first.
/// Ranking a reviewer's 1,995 findings by sharpness costs a full replay pass; a
/// ranking is only worth that if the number carries information at all, and there
/// is exactly one labelled set on which to ask — the adjudicated corpus, whose
/// every row carries a human-assigned `defect_class`. So this asks the shipped
/// question, in the shipped prompt shape, about each row's own description, and
/// prints how the answers land against the labels.
///
/// # It prints and does not threshold
///
/// Written to the shape of `rto_llama`'s `tests/batch_numerics.rs`: the answer is
/// a property of a model, a prompt and a corpus, and a tolerance asserted here
/// would be a claim about all three that this module is not in a position to
/// make. What it *does* assert is that the pass happened — every row classified,
/// every row carrying both shape numbers — so that a silent no-op cannot be read
/// as a result. That is the same rule `reasoning_truncated` exists for one level
/// up: a number whose null is not stated is not yet a result, and a run that did
/// not happen is not a zero.
///
/// # What it reports, and why each number is there
///
/// * **Accuracy** against the labels, with three nulls beside it. The
///   label-permutation expectation is the honest one: hold the classifier's own
///   predicted distribution fixed and assign its predictions to rows at random,
///   which is `Σ_c n_pred(c)·n_true(c) / N` exactly and needs no trials. It is
///   the null that catches the failure this reviewer has already been measured
///   making — 71% of 1,995 findings under one label of fourteen — because a
///   classifier that answers one thing scores its base rate and no more.
/// * **Separation**, as the probability that a correct answer's sharpness
///   exceeds a wrong answer's over every correct/incorrect pair, ties at a half.
///   `0.5` is no separation. A single number, no threshold, and it is the whole
///   question: if sharpness cannot order right from wrong on 27 labelled rows, it
///   will not order real from noise on 1,995 unlabelled ones, and the expensive
///   half of the experiment should not be run.
/// * **Option mass**, min and mean. Sharpness over fourteen options is a
///   distribution whatever the model was thinking about, so this is what says
///   whether it engaged with the question. A near-zero mass makes every other
///   number here a statement about renormalised noise, and reading them without
///   it is the mistake `CandidateFinding::class_option_mass_ppm` exists to
///   prevent.
///
/// ```text
/// cargo test -p roteiro --features serve --bin roteiro \
///     review_llm::typed_class_calibration -- --ignored --nocapture
/// ```
#[cfg(all(test, any(feature = "serve", feature = "inference-local-models")))]
mod typed_class_calibration {
    // Every cast below turns a count into a rate. The counts are a corpus row
    // count (27) and a class count (14); `f64` represents every integer to 2^53
    // exactly, so nothing here can lose a digit. Stated once at the module rather
    // than at fifteen call sites, because it is one fact about all of them.
    #![expect(
        clippy::cast_precision_loss,
        reason = "every cast is a small count (<= 27) becoming a rate; f64 is exact there"
    )]

    use std::io::Write;

    use rto_graph::review_corpus::{CLASSES, CorpusRow, DefectClass};
    use rto_graph::review_score::{CandidateFinding, PPM_SCALE};

    /// What one row's reading came to, in the two numbers and the one comparison
    /// the summary is built from.
    struct Reading {
        /// Sharpness values on the rows the classifier got right.
        correct: Vec<u32>,
        /// Sharpness values on the rows it got wrong.
        wrong: Vec<u32>,
        /// Option mass on every row, right or wrong.
        masses: Vec<u32>,
    }

    /// A row's own text, in exactly the shape `classify_findings` builds for a
    /// real finding — so this measures the shipped instrument and not a variant
    /// of it.
    fn as_finding(row: &CorpusRow) -> CandidateFinding {
        CandidateFinding {
            reviewed_sha: row.reviewed_sha.clone(),
            path: row.path.clone(),
            line: row.line,
            description: row.description.clone(),
            claims_compile_failure: false,
            defect_class: None,
            class_sharpness_ppm: None,
            class_option_mass_ppm: None,
            class_margin_micronats: None,
        }
    }

    /// A ppm integer back as the `0.0..=1.0` fraction it quantised.
    fn fraction(ppm: u32) -> f64 {
        f64::from(ppm) / f64::from(PPM_SCALE)
    }

    /// The mean of some ppm values as a fraction, or `NaN` for none — which
    /// prints as `NaN` and so cannot be mistaken for a measured zero.
    fn mean(values: &[u32]) -> f64 {
        if values.is_empty() {
            return f64::NAN;
        }
        values.iter().map(|v| fraction(*v)).sum::<f64>() / values.len() as f64
    }

    /// A margin in nats onto a `u32` at 1e-6-nat resolution, so it can reach
    /// [`separation`]'s integer comparison.
    ///
    /// Integers for the reason `p_yes` uses them: `separation` counts ties, and a
    /// tie between two `f32`s off two different softmaxes is a statement about the
    /// last bits rather than about the model. A margin of tens of nats scaled by
    /// 1e6 stays far inside `u32`; a pathological one saturates rather than
    /// wrapping.
    fn margin_ppm(nats: f32) -> u32 {
        let scaled = (f64::from(nats) * 1e6).clamp(0.0, f64::from(u32::MAX));
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to 0..=u32::MAX on the line above"
        )]
        let q = scaled as u32;
        q
    }

    /// `P(sharpness of a correct answer > sharpness of a wrong one)` over every
    /// correct/incorrect pair, ties counted as a half.
    ///
    /// The Mann-Whitney statistic, normalised. `0.5` is no separation and `1.0` is
    /// perfect ordering. Chosen over a threshold-and-count because a threshold is
    /// the thing this measurement must not quietly pick: any cut would be fitted
    /// to 27 rows and would then be the result.
    ///
    /// `None` when one of the two groups is empty — perfect or zero accuracy has
    /// no pairs to order, and reporting `0.5` for it would read as "measured, no
    /// separation" rather than "not measurable".
    pub(super) fn separation(correct: &[u32], wrong: &[u32]) -> Option<f64> {
        if correct.is_empty() || wrong.is_empty() {
            return None;
        }
        let mut wins = 0.0f64;
        for c in correct {
            for w in wrong {
                wins += match c.cmp(w) {
                    std::cmp::Ordering::Greater => 1.0,
                    std::cmp::Ordering::Equal => 0.5,
                    std::cmp::Ordering::Less => 0.0,
                };
            }
        }
        Some(wins / (correct.len() as f64 * wrong.len() as f64))
    }

    /// Expected correct answers if the classifier's **own** predicted
    /// distribution were assigned to rows at random: `Σ_c n_pred(c)·n_true(c)/N`.
    ///
    /// Exact rather than sampled, so it costs nothing and cannot be quietly
    /// under-trialled. This is the null that a one-answer classifier fails: it
    /// scores its base rate, and so does the null.
    fn permutation_expectation(predicted: &[DefectClass], truth: &[DefectClass]) -> f64 {
        let n = truth.len();
        if n == 0 {
            return 0.0;
        }
        let mut expected = 0.0f64;
        for class in CLASSES {
            let p = predicted.iter().filter(|c| **c == class).count() as f64;
            let t = truth.iter().filter(|c| **c == class).count() as f64;
            expected += p * t / n as f64;
        }
        expected
    }

    /// How many distinct classes the classifier actually used.
    ///
    /// The one number that catches the failure already on record: 71% of 1,995
    /// findings under one label of fourteen. A classifier using two of fourteen
    /// can still look accurate on a corpus whose labels are skewed the same way,
    /// and the permutation null beside it is what prices that.
    fn distinct_used(predicted: &[DefectClass]) -> usize {
        CLASSES
            .into_iter()
            .filter(|c| predicted.contains(c))
            .count()
    }

    /// Print one line per row and collect the three vectors the summary needs.
    fn print_rows(
        out: &mut impl Write,
        findings: &[CandidateFinding],
        truth: &[DefectClass],
        predicted: &[DefectClass],
    ) -> Reading {
        let mut reading = Reading {
            correct: Vec::new(),
            wrong: Vec::new(),
            masses: Vec::new(),
        };
        for ((f, want), got) in findings.iter().zip(truth).zip(predicted) {
            let sharp = f.class_sharpness_ppm.expect("asserted present");
            let mass = f.class_option_mass_ppm.expect("asserted present");
            reading.masses.push(mass);
            if got == want {
                reading.correct.push(sharp);
            } else {
                reading.wrong.push(sharp);
            }
            let _ = writeln!(
                out,
                // **Printed as the ppm integers, not as rounded fractions.**
                // `separation` below consumes these exact integers, and at four
                // decimal places every sharpness on this model prints as `1.0000`
                // while differing in the last few ppm — so a rounded table cannot
                // reproduce the statistic computed from it, and a reader checking
                // one against the other would find a number with no visible
                // support. The scale is `PPM_SCALE`; `1000000` is all the mass on
                // one option.
                "  {:<34} {:>5}  want {:<21} got {:<21} sharp {sharp:>7}  mass {mass:>7}{}",
                f.path.rsplit('/').next().unwrap_or(&f.path),
                f.line,
                want.as_str(),
                got.as_str(),
                if got == want { "" } else { "  <- wrong" },
            );
        }
        reading
    }

    /// The same separation statistic over the **margins** the same readings
    /// already recorded — no second pass, because `classify_findings` persists
    /// them.
    ///
    /// Printed beside the sharpness separation rather than instead of it: the two
    /// lines are the same statistic over the same rows by two different numbers,
    /// and the comparison is the result. A margin separation near `0.5` as well
    /// would say the reading carries no orderable signal at all and that no
    /// rescaling of it would help.
    fn print_margin_summary(
        out: &mut impl Write,
        findings: &[CandidateFinding],
        truth: &[DefectClass],
        predicted: &[DefectClass],
    ) {
        let mut correct: Vec<u32> = Vec::new();
        let mut wrong: Vec<u32> = Vec::new();
        let mut raw: Vec<u32> = Vec::new();
        for ((f, want), got) in findings.iter().zip(truth).zip(predicted) {
            let m = f.class_margin_micronats.expect("asserted present");
            raw.push(m);
            if got == want {
                correct.push(m);
            } else {
                wrong.push(m);
            }
        }
        let nats = |m: u32| f64::from(m) / f64::from(PPM_SCALE);
        let _ = writeln!(
            out,
            "  --- the same readings, ordered by MARGIN instead ---\
             \n  margin (nats) min / max     {:.3} / {:.3}\
             \n  separation P(correct>wrong) {}\
             \n\n  Compare with the sharpness separation above. If this one is \
             also ~0.5 the reading carries no orderable signal at all, and no \
             rescaling of it would.\n",
            raw.iter().copied().min().map_or(f64::NAN, nats),
            raw.iter().copied().max().map_or(f64::NAN, nats),
            separation(&correct, &wrong)
                .map_or_else(|| "n/a (one group empty)".to_owned(), |s| format!("{s:.4}")),
        );
    }

    /// Print the summary: accuracy with three nulls beside it, the separation
    /// statistic, and the option mass that says whether to believe any of them.
    fn print_summary(
        out: &mut impl Write,
        reading: &Reading,
        truth: &[DefectClass],
        predicted: &[DefectClass],
    ) {
        let n = truth.len();
        let hits = reading.correct.len();
        let null = permutation_expectation(predicted, truth);
        let majority = CLASSES
            .into_iter()
            .map(|c| truth.iter().filter(|t| **t == c).count())
            .max()
            .unwrap_or(0);
        let classes = CLASSES.len();
        let _ = writeln!(
            out,
            "\n  accuracy                    {hits}/{n} = {:.3}\
             \n  permutation null (exact)    {null:.2}/{n} = {:.3}\
             \n  majority-class baseline     {majority}/{n} = {:.3}\
             \n  uniform baseline            1/{classes} = {:.3}\
             \n  distinct classes used       {} of {classes}\
             \n  mean sharpness (correct)    {:.4}\
             \n  mean sharpness (wrong)      {:.4}\
             \n  separation P(correct>wrong) {}\
             \n  option mass min / mean      {:.4} / {:.4}",
            hits as f64 / n as f64,
            null / n as f64,
            majority as f64 / n as f64,
            1.0 / classes as f64,
            distinct_used(predicted),
            mean(&reading.correct),
            mean(&reading.wrong),
            separation(&reading.correct, &reading.wrong)
                .map_or_else(|| "n/a (one group empty)".to_owned(), |s| format!("{s:.4}")),
            reading
                .masses
                .iter()
                .copied()
                .min()
                .map_or(f64::NAN, fraction),
            mean(&reading.masses),
        );
        let _ = writeln!(
            out,
            "\n  No threshold is asserted above. `separation` at 0.5 is no ordering, \
             and `option mass` decides whether the rest is about anything.\n"
        );
    }

    /// Resolve the pinned review model and start llama.cpp on it, or print why
    /// this measurement is being skipped.
    ///
    /// Written to `stderr` directly rather than through `eprintln!` because libtest
    /// discards a *passing* test's captured output, and a skip that printed to
    /// nobody would read as a run that found nothing.
    ///
    /// The name comes back owned rather than borrowed: it is read through the
    /// model choice, which does not outlive this function, and a measurement that
    /// does not say which model produced it is not comparable with the next one —
    /// so it is carried rather than dropped.
    pub(super) fn engine_or_skip(
        out: &mut impl Write,
    ) -> Option<(rto_llama::llama::LlamaEngine, String)> {
        // **`resolve_model` reads a process-global slot that only `main` fills.**
        //
        // A test binary is not `main`, so without this the pins are all unset and
        // every task resolves to its *default* — `qwen3-0.6b` for `Review`, which
        // is not installed here, so the measurement self-skipped while reporting a
        // pass. The skip printed its reason, which is the only thing that made it
        // visible; a silent one would have read as a measurement that found
        // nothing.
        //
        // So it is done here exactly as `main` does it, from the same two layers,
        // and the model name is printed beside every number: this repository pins
        // `[models] generative` to the model the Stage 35b baseline was recorded
        // on, and a measurement against a different one is not comparable with it.
        #[cfg(feature = "models")]
        match crate::config::load(&std::env::current_dir().unwrap_or_default()) {
            Ok(cfg) => rto_graph::set_model_pins(cfg.effective.models.resolve()),
            Err(e) => {
                let _ = writeln!(out, "SKIP: config would not load ({e})");
                return None;
            }
        }
        let choice = match rto_graph::resolve_model(rto_graph::ModelTask::Review) {
            Ok(c) => c,
            Err(e) => {
                let _ = writeln!(out, "SKIP: no review model resolved ({e})");
                return None;
            }
        };
        let model = match choice.require_installed() {
            Ok(m) => m.to_owned(),
            Err(e) => {
                let _ = writeln!(out, "SKIP: review model not installed ({e})");
                return None;
            }
        };
        match super::start_engine(&model) {
            Ok(engine) => Some((engine, model)),
            Err(e) => {
                let _ = writeln!(out, "SKIP: llama.cpp would not start ({e})");
                None
            }
        }
    }

    #[test]
    #[ignore = "needs the pinned generative model under ~/.roteiro/models; prints a measurement"]
    fn sharpness_against_the_adjudicated_class_set() {
        let mut out = std::io::stderr();
        let corpus = rto_graph::review_corpus::builtin().expect("the builtin corpus parses");
        let Some((engine, model)) = engine_or_skip(&mut out) else {
            return;
        };

        let truth: Vec<DefectClass> = corpus.rows().iter().map(|r| r.defect_class).collect();
        let mut findings: Vec<CandidateFinding> = corpus.rows().iter().map(as_finding).collect();
        let n = findings.len();
        let _ = writeln!(
            out,
            "\n=== typed class read vs the adjudicated corpus — {model}, {n} rows, \
             {} classes ===",
            CLASSES.len()
        );

        super::classify_findings(&engine, &model, &mut findings)
            .expect("the typed class read completes over every row");

        // **The pass happened**, asserted before a single number is read off it, so
        // that a no-op cannot be reported as a measurement.
        assert_eq!(findings.len(), n, "a row was lost");
        for f in &findings {
            assert!(
                f.defect_class.is_some(),
                "{}:{} came back with no class",
                f.path,
                f.line
            );
            assert!(
                f.class_sharpness_ppm.is_some()
                    && f.class_option_mass_ppm.is_some()
                    && f.class_margin_micronats.is_some(),
                "{}:{} came back with no shape numbers",
                f.path,
                f.line
            );
        }

        let predicted: Vec<DefectClass> = findings
            .iter()
            .map(|f| f.defect_class.expect("just asserted present"))
            .collect();
        let reading = print_rows(&mut out, &findings, &truth, &predicted);
        print_summary(&mut out, &reading, &truth, &predicted);
        print_margin_summary(&mut out, &findings, &truth, &predicted);
    }
    /// The yes/no question this measurement asks, and the reason it lives here
    /// rather than beside `rto_graph::reviewer::CLASS_QUESTION`.
    ///
    /// `CLASS_QUESTION` is part of the shipped instrument: `--typed-class` asks
    /// it, so it belongs with the option list it is asked over. This one is not
    /// shipped — nothing in `review --llm` asks whether a finding is real, and
    /// #897's own note says a model judging its own findings is a separate claim
    /// from a model classifying them. So it is a question the *measurement* asks,
    /// and it is written where the measurement is.
    const REALITY_QUESTION: &str = "Does that finding describe a real defect — something a maintainer would \
         actually fix — rather than a false alarm?";

    /// **Can `P(yes)` tell an adjudicated real row from a known-false one?**
    ///
    /// The half of #897's Phase 3 that the corpus can answer without a replay
    /// pass. The expensive question — does ranking a reviewer's own 1,995 findings
    /// by sharpness beat the permutation null — needs the reviewer re-run; this
    /// asks the *same* instrument about 27 rows a human already adjudicated, and
    /// costs 27 prefills.
    ///
    /// It is a weaker question than Phase 3's in one specific way that must not be
    /// glossed: these descriptions are a **human reviewer's** prose, not the
    /// model's own findings, so a separation here does not establish that the
    /// number would order the model's output. It is nonetheless the strictly
    /// easier task — the rows are well-written and half of them were written by a
    /// tool that got them right — so **a failure here is decisive** and a success
    /// is only encouraging.
    ///
    /// # The base rate is the whole difficulty
    ///
    /// 22 of 27 rows are `real`, so answering "yes" to everything scores 0.815 and
    /// accuracy is close to useless. The number that is not fooled by that is the
    /// separation over every real/false pair, which is invariant to the base rate:
    /// `0.5` is no ordering whatever the mix. Accuracy is printed beside it only so
    /// the base rate is visible rather than implied.
    ///
    /// Uses `ask_choice` over `Noul::as_choice` rather than `ask_noul`, because
    /// `ask_noul` returns the probability alone and this needs the option mass —
    /// which is exactly the choice that method's own docs tell a caller to make.
    ///
    /// ```text
    /// cargo test -p roteiro --features serve --bin roteiro \
    ///     review_llm::typed_class_calibration::the_noul -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs the pinned generative model under ~/.roteiro/models; prints a measurement"]
    fn the_noul_separates_real_rows_from_known_false_ones() {
        use rto_graph::review_corpus::Verdict;
        use rto_graph::review_score::fraction_ppm;

        let mut out = std::io::stderr();
        let corpus = rto_graph::review_corpus::builtin().expect("the builtin corpus parses");
        let Some((engine, model)) = engine_or_skip(&mut out) else {
            return;
        };
        let noul = rto_llama::typed::Noul::new();

        let _ = writeln!(
            out,
            "\n=== noul: real vs known-false — {model}, {} rows ===",
            corpus.rows().len()
        );

        let mut real: Vec<u32> = Vec::new();
        let mut known_false: Vec<u32> = Vec::new();
        let mut masses: Vec<u32> = Vec::new();
        let mut margins: Vec<f32> = Vec::new();
        let mut real_margins: Vec<u32> = Vec::new();
        let mut false_margins: Vec<u32> = Vec::new();
        for row in corpus.rows() {
            let state = format!(
                "A code reviewer reported this finding about `{}` at line {}:\n\n{}",
                row.path, row.line, row.description,
            );
            let answer = engine
                .ask_choice(&model, &state, REALITY_QUESTION, noul.as_choice())
                .expect("the noul completes over every row");
            // Through the ppm quantiser and not kept as an `f32`: `separation`
            // counts ties, and a tie between two floats off two different
            // softmaxes is a statement about the last bits rather than about the
            // model. Integers make "the same reading" mean something.
            let p_yes = fraction_ppm(rto_llama::typed::Noul::p_yes(&answer));
            let mass = fraction_ppm(answer.option_mass());
            masses.push(mass);
            // Margins go into their own two groups as well as the printed list:
            // eyeballing which rows sit low is not a statistic, and the whole
            // point of `separation` is that it is one.
            let margin_q = margin_ppm(answer.margin());
            match row.verdict {
                Verdict::Real => real_margins.push(margin_q),
                Verdict::False => false_margins.push(margin_q),
            }
            margins.push(answer.margin());
            match row.verdict {
                Verdict::Real => real.push(p_yes),
                Verdict::False => known_false.push(p_yes),
            }
            let _ = writeln!(
                out,
                // ppm integers, for the reason `print_rows` states: these are
                // what `separation` reads, and four decimal places round every
                // one of them to `1.0000`.
                "  {:<34} {:>5}  {:<5}  P(yes) {p_yes:>7}  mass {mass:>7}  margin {:>7.3}",
                row.path.rsplit('/').next().unwrap_or(&row.path),
                row.line,
                match row.verdict {
                    Verdict::Real => "real",
                    Verdict::False => "false",
                },
                answer.margin(),
            );
        }

        // **The pass happened.** Both groups non-empty, because the separation
        // statistic below is `None` for an empty one and a printed `n/a` must mean
        // "the corpus has no such rows", never "the loop did nothing".
        assert_eq!(real.len() + known_false.len(), corpus.rows().len());
        assert!(
            !real.is_empty() && !known_false.is_empty(),
            "one group is empty"
        );

        let said_yes = real.iter().filter(|p| **p > PPM_SCALE / 2).count()
            + known_false.iter().filter(|p| **p > PPM_SCALE / 2).count();
        let _ = writeln!(
            out,
            "\n  rows                        {} real, {} known-false\
             \n  base rate (answer yes)      {:.3}\
             \n  said yes at P > 0.5         {said_yes}/{}\
             \n  mean P(yes) on real         {:.4}\
             \n  mean P(yes) on known-false  {:.4}\
             \n  separation P(real>false)    {}\
             \n  option mass min / mean      {:.4} / {:.4}\
             \n  margin (nats) min / max     {:.3} / {:.3}\
             \n  separation by MARGIN        {}\
             \n\n  A separation of 0.5 is no ordering, and it is the only number \
             here the 22:5 base rate cannot flatter.\n",
            real.len(),
            known_false.len(),
            real.len() as f64 / (real.len() + known_false.len()) as f64,
            real.len() + known_false.len(),
            mean(&real),
            mean(&known_false),
            separation(&real, &known_false)
                .map_or_else(|| "n/a (one group empty)".to_owned(), |s| format!("{s:.4}")),
            masses.iter().copied().min().map_or(f64::NAN, fraction),
            mean(&masses),
            margins.iter().copied().fold(f32::INFINITY, f32::min),
            margins.iter().copied().fold(f32::NEG_INFINITY, f32::max),
            separation(&real_margins, &false_margins)
                .map_or_else(|| "n/a (one group empty)".to_owned(), |s| format!("{s:.4}")),
        );
    }
}

/// **Does ordering the reviewer's own findings by logit margin put the real ones
/// near the top?** (Issue #897, the expensive half.)
///
/// # This measures RANKING, and ranking is not recall
///
/// The recorded failure is a **recall** failure: observed 4 real rows matched
/// against a permutation null mean of 4.19, `P(≥observed) = 0.72`, and the
/// project's own verdict that *"the reviewer scored below chance"*
/// (`docs/history/BUILD_PLAN_V2.md`). A per-finding number cannot fix that. It
/// cannot make the reviewer see a defect it did not see, and nothing in this
/// module claims otherwise.
///
/// What it can fix is **ranking**: given the findings the reviewer does emit, does
/// ordering by margin put the real ones first? Those are different questions and
/// the second is the useful one. A reviewer emitting 10.9 findings per file of
/// which two are real is unusable as a list and usable as a *ranked* list, if the
/// ranking works. So the headline here is the ranking statistic against a
/// random-ordering null, and recall is reported beside it as the thing that did
/// not change.
///
/// # On the margin, not on the sharpness
///
/// `sharpness` was measured saturated — exactly `1_000_000` ppm on 26 of the 27
/// corpus rows (`typed_class_calibration`). Ranking by a number that is constant
/// cannot separate from chance, and that is arithmetic rather than a measurement.
/// So this ranks by `CandidateFinding::class_margin_micronats`.
///
/// # The population is 23 files, and that is not the recorded baseline's
///
/// `review_score`'s matcher credits a finding to a row on
/// `(commit, path, line ±LINE_WINDOW)`, and the permutation null relocates a row
/// **inside its own reconstructed diff** — so a row never leaves its own
/// `(sha, path)`. Only findings on a corpus anchor can therefore match, in either
/// arm. There are **23 distinct anchors** over 15 commits, so 23 file reviews
/// answer the same statistic as 183 would, at an eighth of the inference.
///
/// It also means this run's finding population is **not** the recorded run's
/// 1,995 over 183 files. Density differs, so the null differs, so
/// `4 vs 4.19` is **not** a like-for-like comparison and this module does not make
/// one. It computes its own observed and its own null, from one command over one
/// file set at one commit, and says so.
///
/// # Checkpointed, and it proves it ran
///
/// Each file's findings are appended to a JSONL checkpoint as they are produced,
/// so a fault costs one file rather than the run; a re-run skips what is already
/// there. Because a resumed run performs no inference, the "it really ran" proof
/// is reported per-file and asserted only over the files reviewed *this* time —
/// the session that wrote this module had a calibration run report `ok` having
/// done no inference at all, and the only thing that made it visible was a
/// printed reason.
///
/// ```text
/// ROTEIRO_RANK_MAX_FILES=1 cargo test -p roteiro --features serve --bin roteiro \
///     review_llm::margin_ranking -- --ignored --nocapture      # time one file
/// cargo test -p roteiro --features serve --bin roteiro \
///     review_llm::margin_ranking -- --ignored --nocapture      # the whole run
/// ```
#[cfg(all(test, any(feature = "serve", feature = "inference-local-models")))]
mod margin_ranking {
    #![expect(
        clippy::cast_precision_loss,
        reason = "every cast is a small count (corpus rows, findings, trials) \
                  becoming a rate; f64 is exact well past any of them"
    )]

    use std::collections::{BTreeMap, BTreeSet};
    use std::io::Write;
    use std::path::PathBuf;
    use std::time::Instant;

    use rto_graph::review_corpus::{Corpus, CorpusRow, Verdict};
    use rto_graph::review_score::{CandidateFinding, CandidateRun, LINE_WINDOW};
    use rto_graph::reviewer::FileUnderReview;

    use super::typed_class_calibration::{engine_or_skip, separation};
    use super::{ClassSource, ReviewArm};

    /// Permutation trials for both nulls.
    ///
    /// The recorded baseline used 2,000 for the recall null and this matches it so
    /// the two are the same experiment; the ranking null gets more because it is
    /// cheaper (a label shuffle, no diff parsing) and its statistic is finer.
    /// Neither is sampled adaptively and neither is re-run: the count is fixed
    /// here, before any number is seen.
    const RECALL_TRIALS: usize = 2_000;
    /// See [`RECALL_TRIALS`].
    const RANKING_TRIALS: usize = 100_000;

    /// The seed both nulls draw from.
    ///
    /// Fixed and stated so the p-values reproduce. A seed chosen after seeing a
    /// result is the tuning this measurement must not do, so it is a constant in
    /// the source rather than an input.
    const SEED: u64 = 0x5197_8974_2026_0929;

    /// `k` values `precision@k` is reported at.
    const KS: [usize; 5] = [1, 3, 5, 10, 20];

    /// A deterministic `xorshift64*` — enough for a label shuffle, and it avoids
    /// taking a dependency for one.
    struct Rng(u64);

    impl Rng {
        fn next_u64(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        /// A uniform value in `0..n`, by rejection so that the modulo bias which
        /// would quietly skew a null is not present.
        fn below(&mut self, n: usize) -> usize {
            assert!(n > 0, "no range to draw from");
            let n64 = n as u64;
            let limit = u64::MAX - (u64::MAX % n64);
            loop {
                let v = self.next_u64();
                if v < limit {
                    // `n` came in as a `usize`, so `v % n` is inside `usize` by
                    // construction on every target.
                    return usize::try_from(v % n64).expect("v % n < n <= usize::MAX");
                }
            }
        }

        fn shuffle<T>(&mut self, items: &mut [T]) {
            for i in (1..items.len()).rev() {
                items.swap(i, self.below(i + 1));
            }
        }
    }

    /// One reviewed file, as the checkpoint records it.
    struct Reviewed {
        sha: String,
        path: String,
        findings: Vec<CandidateFinding>,
        seconds: f64,
        /// Whether the model said `NO FINDINGS` in the required form.
        declared_clean: bool,
        /// Lines that looked like findings but carried no usable anchor.
        unparsed: usize,
        /// The generation stopped inside a reasoning block, so the file was never
        /// actually reviewed.
        reasoning_truncated: bool,
        /// What produced these findings — the model plus this harness's own
        /// revision. A resumed run refuses a record from a different one.
        instrument: String,
    }

    /// Bumped whenever a change would make old findings incomparable with new
    /// ones — the prompt, the class question, the arm, or what
    /// `classify_findings` records.
    ///
    /// It is deliberately **not** derived from anything: a hash of the source
    /// would invalidate the checkpoint on a comment change, and a version that
    /// moves for free gets ignored. This is a judgement, made when the change is
    /// made.
    const INSTRUMENT: &str = "diff-only/typed-read/v1";

    /// Where the checkpoint lives. Outside the repository, and overridable so two
    /// runs need not share one.
    fn checkpoint_path() -> PathBuf {
        std::env::var_os("ROTEIRO_RANK_CHECKPOINT").map_or_else(
            || std::env::temp_dir().join("roteiro-margin-ranking.jsonl"),
            PathBuf::from,
        )
    }

    /// Read whatever the checkpoint already holds, keyed by `(sha, path)`.
    ///
    /// A malformed line is **fatal**, not skipped: a half-written record from a
    /// killed process would otherwise silently reduce the population and move
    /// every number here.
    fn read_checkpoint(path: &std::path::Path) -> BTreeMap<(String, String), Reviewed> {
        let mut out = BTreeMap::new();
        let Ok(text) = std::fs::read_to_string(path) else {
            return out;
        };
        for (n, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let v: serde_json::Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("checkpoint line {} is not JSON: {e}", n + 1));
            let sha = v["sha"].as_str().expect("sha").to_owned();
            let p = v["path"].as_str().expect("path").to_owned();
            let findings: Vec<CandidateFinding> =
                serde_json::from_value(v["findings"].clone()).expect("findings");
            let seconds = v["seconds"].as_f64().unwrap_or(0.0);
            let instrument = v["instrument"].as_str().unwrap_or("unrecorded").to_owned();
            out.insert(
                (sha.clone(), p.clone()),
                Reviewed {
                    sha,
                    path: p,
                    findings,
                    seconds,
                    // Absent in a checkpoint written before these were recorded.
                    // `false`/`0` is the reading that claims the least: it says
                    // "not recorded as clean" rather than "recorded as clean",
                    // and the summary counts them separately so an older
                    // checkpoint cannot be read as having answered the question.
                    declared_clean: v["declared_clean"].as_bool().unwrap_or(false),
                    unparsed: usize::try_from(v["unparsed"].as_u64().unwrap_or(0)).unwrap_or(0),
                    reasoning_truncated: v["reasoning_truncated"].as_bool().unwrap_or(false),
                    instrument,
                },
            );
        }
        out
    }

    /// Append one file's outcome to the checkpoint, flushed before returning.
    fn append_checkpoint(path: &std::path::Path, r: &Reviewed) {
        let record = serde_json::json!({
            "sha": r.sha,
            "path": r.path,
            // **What produced these findings, so a resumed run cannot mix
            // instruments.** Keyed only by `(sha, path)`, the checkpoint silently
            // reused findings from a different model, a different prompt or a
            // different classifier — the resumed run would then report a
            // measurement over two instruments and say nothing about either.
            "instrument": r.instrument,
            "seconds": r.seconds,
            // The three fields that tell "found nothing" from "never answered".
            // A file with zero findings is uninterpretable without them, and 11 of
            // this run's 23 files had zero.
            "declared_clean": r.declared_clean,
            "unparsed": r.unparsed,
            "reasoning_truncated": r.reasoning_truncated,
            "findings": serde_json::to_value(&r.findings).expect("findings serialise"),
        });
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("checkpoint opens");
        writeln!(f, "{record}").expect("checkpoint writes");
        f.flush().expect("checkpoint flushes");
    }

    /// The new-side line numbers a unified diff shows, in order.
    ///
    /// The permutation null relocates each corpus row to a line **its own diff
    /// actually shows**, which is the recorded baseline's recipe; this is that set.
    /// Context lines count as well as added ones, because a reviewer can anchor a
    /// finding to either and the tighter added-only variant is reported separately
    /// by the baseline.
    fn diff_new_lines(diff: &str) -> Vec<u32> {
        let mut out = Vec::new();
        let mut next = 0u32;
        for line in diff.lines() {
            if let Some(rest) = line.strip_prefix("@@") {
                // `@@ -a,b +c,d @@` — take `c`.
                if let Some(plus) = rest.split('+').nth(1) {
                    let digits: String = plus.chars().take_while(char::is_ascii_digit).collect();
                    if let Ok(start) = digits.parse::<u32>() {
                        next = start;
                    }
                }
                continue;
            }
            match line.as_bytes().first() {
                Some(b'+') => {
                    out.push(next);
                    next += 1;
                }
                Some(b'-') => {}
                // A context line, and the `\ No newline` marker which is neither.
                _ if line.starts_with('\\') => {}
                _ => {
                    out.push(next);
                    next += 1;
                }
            }
        }
        out
    }

    /// Which findings the scorer's rule credits to a row, as indices into
    /// `findings`, keyed by row id.
    ///
    /// **A reimplementation of `review_score`'s private `match_findings`, and it is
    /// cross-checked rather than trusted.** The shipped matcher returns finding
    /// *references* keyed by row and the ranking needs indices, so this repeats
    /// the rule: greedy one-to-one over `(sha, path, |Δline| ≤ LINE_WINDOW)`,
    /// nearest first, ties broken by row id then by line so the result does not
    /// depend on finding order. The test then asserts its real-row count equals
    /// the shipped `score()`'s `found`, which is what licenses using it — the same
    /// step the recorded baseline took before believing its own null.
    fn credited(rows: &[&CorpusRow], findings: &[CandidateFinding]) -> BTreeMap<u64, usize> {
        let mut pairs: Vec<(u32, u64, u32, usize)> = Vec::new();
        for (idx, f) in findings.iter().enumerate() {
            for row in rows {
                if row.reviewed_sha != f.reviewed_sha || row.path != f.path {
                    continue;
                }
                let distance = row.line.abs_diff(f.line);
                if distance <= LINE_WINDOW {
                    pairs.push((distance, row.id, f.line, idx));
                }
            }
        }
        pairs.sort_unstable();
        let mut by_row: BTreeMap<u64, usize> = BTreeMap::new();
        let mut used: BTreeSet<usize> = BTreeSet::new();
        for (_, row_id, _, idx) in pairs {
            if by_row.contains_key(&row_id) || used.contains(&idx) {
                continue;
            }
            by_row.insert(row_id, idx);
            used.insert(idx);
        }
        by_row
    }

    /// Refuse a checkpoint record that a **different instrument** produced.
    ///
    /// The checkpoint is keyed by `(sha, path)`, which says nothing about what
    /// made the findings. Resuming after a `[models] generative` change, a prompt
    /// change or a classifier change would silently mix two instruments, and a
    /// measurement over two instruments measures neither — the sort of quiet
    /// population change this module's own comparability block exists to refuse.
    ///
    /// # Errors
    /// If the record names a different model or a different [`INSTRUMENT`].
    fn same_instrument(prev: &Reviewed, model: &str, ckpt: &std::path::Path) -> anyhow::Result<()> {
        let want = format!("{model}|{INSTRUMENT}");
        anyhow::ensure!(
            prev.instrument == want,
            "checkpoint record for {}:{} was produced by `{}`, but this run is `{want}`. \
             Delete {} or point ROTEIRO_RANK_CHECKPOINT elsewhere; mixing them would \
             measure neither instrument.",
            prev.sha,
            prev.path,
            prev.instrument,
            ckpt.display(),
        );
        Ok(())
    }

    /// Review every corpus anchor file with the typed class read on, timing each
    /// and checkpointing as it goes.
    ///
    /// Returns `(reviewed files, files reviewed *this run*, seconds spent this
    /// run, stopped early)`. The middle two are what the "it really ran"
    /// assertions are made over, because a fully resumed run performs no inference
    /// and must not be able to report one; the last says the population is a
    /// deliberate fragment and must not be measured.
    fn review_anchors(
        out: &mut impl Write,
        corpus: &Corpus,
        repo: &std::path::Path,
    ) -> anyhow::Result<(Vec<Reviewed>, usize, f64, bool)> {
        let anchors: BTreeSet<(&str, &str)> = corpus
            .rows()
            .iter()
            .map(|r| (r.reviewed_sha.as_str(), r.path.as_str()))
            .collect();
        let cap = std::env::var("ROTEIRO_RANK_MAX_FILES")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(usize::MAX);

        let ckpt = checkpoint_path();
        let mut done = read_checkpoint(&ckpt);
        let _ = writeln!(
            out,
            "checkpoint {} — {} file(s) already recorded",
            ckpt.display(),
            done.len()
        );

        let Some((engine, model)) = engine_or_skip(out) else {
            anyhow::bail!("no engine");
        };
        let main = super::main_ref(repo)?;
        let shas: Vec<&str> = corpus.reviewed_shas().into_iter().collect();

        let mut reviewed: Vec<Reviewed> = Vec::new();
        let mut fresh = 0usize;
        let mut spent = 0.0f64;
        // The full anchor count, deliberately **not** capped: this number prices
        // the whole run, and a projection that shrank with `ROTEIRO_RANK_MAX_FILES`
        // would make a one-file timing run report the cost of a one-file run.
        let total_anchors = anchors.len();

        for sha in &shas {
            let set = super::files_at(repo, sha, &main, rto_graph::PathPolicy::empty())?;
            for file in set.files {
                if !anchors.contains(&(*sha, file.path.as_str())) {
                    continue;
                }
                let key = ((*sha).to_owned(), file.path.clone());
                if let Some(prev) = done.remove(&key) {
                    same_instrument(&prev, &model, &ckpt)?;
                    reviewed.push(prev);
                    continue;
                }
                if reviewed.len() >= cap {
                    let _ = writeln!(
                        out,
                        "stopping at ROTEIRO_RANK_MAX_FILES={cap} — this is a timing \
                         run, not a measurement"
                    );
                    return Ok((reviewed, fresh, spent, true));
                }
                let started = Instant::now();
                let outcome = super::review_file(
                    &engine,
                    &model,
                    &file,
                    &rto_graph::reviewer::GraphContext::none(),
                    &[],
                    &|_p: &str| None,
                    ClassSource::TypedRead,
                )?;
                let seconds = started.elapsed().as_secs_f64();
                spent += seconds;
                fresh += 1;
                let n = outcome.findings.len();
                let why = if outcome.reasoning_truncated {
                    " NEVER REVIEWED (reasoning truncated)"
                } else if n == 0 && outcome.declared_clean {
                    " declared clean"
                } else if n == 0 {
                    " zero findings and NO clean declaration"
                } else {
                    ""
                };
                // Progress with a live projection, so the cost of the whole run is
                // visible from its first file rather than extrapolated from prose.
                let per_file = spent / fresh as f64;
                let _ = writeln!(
                    out,
                    "  [{}/{total_anchors}] {} — {n} finding(s){why} in {seconds:.1}s \
                     (mean {per_file:.1}s, projected total {:.1} min)",
                    reviewed.len() + 1,
                    file.path,
                    per_file * total_anchors as f64 / 60.0,
                );
                let r = Reviewed {
                    sha: (*sha).to_owned(),
                    path: file.path.clone(),
                    declared_clean: outcome.declared_clean,
                    unparsed: outcome.unparsed,
                    reasoning_truncated: outcome.reasoning_truncated,
                    findings: outcome.findings,
                    seconds,
                    instrument: format!("{model}|{INSTRUMENT}"),
                };
                append_checkpoint(&ckpt, &r);
                reviewed.push(r);
            }
        }
        assert!(
            done.is_empty(),
            "the checkpoint holds {} record(s) for files this run did not visit, so it \
             is from a different corpus or a different path policy: {:?}",
            done.len(),
            done.keys().collect::<Vec<_>>()
        );
        Ok((reviewed, fresh, spent, false))
    }

    /// The observed real-row count and the recall permutation null.
    ///
    /// Relocate every corpus row to a uniformly random line its own reconstructed
    /// diff shows, leave the findings byte-for-byte as emitted, and rescore. The
    /// recorded baseline's recipe exactly, so the *method* is comparable even
    /// though the population is not.
    fn recall_null(
        out: &mut impl Write,
        corpus: &Corpus,
        findings: &[CandidateFinding],
        lines_by_anchor: &BTreeMap<(String, String), Vec<u32>>,
        observed: usize,
    ) {
        let real: Vec<&CorpusRow> = corpus
            .rows()
            .iter()
            .filter(|r| r.verdict == Verdict::Real)
            .collect();
        // Two supports, because two rows anchor outside their own diff. The
        // baseline's recipe relocates every row into the diff, which for those two
        // *raises* their matchability above the position they were observed at and
        // so biases the null upward — conservative, but not like-for-like. The
        // second arm keeps only rows whose own line is inside, so relocation stays
        // within the support the observation came from. Both are reported; neither
        // is chosen after the fact.
        let inside_only: Vec<&CorpusRow> = real
            .iter()
            .copied()
            .filter(|r| {
                lines_by_anchor
                    .get(&(r.reviewed_sha.clone(), r.path.clone()))
                    .is_some_and(|l| l.contains(&r.line))
            })
            .collect();
        let observed_inside = credited(&inside_only, findings).len();

        let trial = |rows: &[&CorpusRow], target: usize| -> (f64, f64) {
            let mut rng = Rng(SEED);
            let mut at_or_above = 0usize;
            let mut total = 0usize;
            for _ in 0..RECALL_TRIALS {
                let moved: Vec<CorpusRow> = rows
                    .iter()
                    .map(|r| {
                        let mut c = (*r).clone();
                        let key = (r.reviewed_sha.clone(), r.path.clone());
                        if let Some(lines) = lines_by_anchor.get(&key)
                            && !lines.is_empty()
                        {
                            c.line = lines[rng.below(lines.len())];
                        }
                        c
                    })
                    .collect();
                let refs: Vec<&CorpusRow> = moved.iter().collect();
                let hits = credited(&refs, findings).len();
                total += hits;
                if hits >= target {
                    at_or_above += 1;
                }
            }
            (
                total as f64 / RECALL_TRIALS as f64,
                at_or_above as f64 / RECALL_TRIALS as f64,
            )
        };
        let (mean_all, p_all) = trial(&real, observed);
        let (mean_in, p_in) = trial(&inside_only, observed_inside);
        let _ = writeln!(
            out,
            "\n  == RECALL (not what a per-finding number can change) ==\
             \n  {RECALL_TRIALS} trials, seed fixed, rows relocated inside their own diff\
             \n  all real rows ({:>2})          observed {observed}, null mean {mean_all:.2}, \
             P(null>=obs) {p_all:.3}\
             \n  rows inside their diff ({:>2}) observed {observed_inside}, null mean \
             {mean_in:.2}, P(null>=obs) {p_in:.3}",
            real.len(),
            inside_only.len(),
        );
    }

    /// The ranking statistic and its random-ordering null — **the headline**.
    fn ranking_null(
        out: &mut impl Write,
        findings: &[CandidateFinding],
        hit_indices: &BTreeSet<usize>,
    ) {
        // The `precision@k` line is assembled into a `String`, which needs the
        // `fmt` trait beside this module's `io` one.
        use std::fmt::Write as _;

        let n = findings.len();
        let margins: Vec<u32> = findings
            .iter()
            .map(|f| {
                f.class_margin_micronats
                    .expect("typed read recorded a margin")
            })
            .collect();
        let hits: Vec<u32> = (0..n)
            .filter(|i| hit_indices.contains(i))
            .map(|i| margins[i])
            .collect();
        let misses: Vec<u32> = (0..n)
            .filter(|i| !hit_indices.contains(i))
            .map(|i| margins[i])
            .collect();

        let Some(observed) = separation(&hits, &misses) else {
            let _ = writeln!(
                out,
                "\n  == RANKING ==\n  not measurable: {} credited of {n} finding(s), so one \
                 group is empty. That is a statement about this run's recall, not about \
                 the margin.",
                hits.len()
            );
            return;
        };

        // Order by margin, highest first; ties by index so the order is total and
        // does not depend on sort stability.
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by_key(|&i| (std::cmp::Reverse(margins[i]), i));
        let ranks: Vec<usize> = order
            .iter()
            .enumerate()
            .filter(|(_, i)| hit_indices.contains(i))
            .map(|(rank, _)| rank + 1)
            .collect();
        let mean_rank = ranks.iter().sum::<usize>() as f64 / ranks.len() as f64;

        // The random-ordering null for `separation` is exactly 0.5, so the p-value
        // is what is worth computing: shuffle which findings are credited, keeping
        // the count, and recompute.
        let mut rng = Rng(SEED);
        let mut labels: Vec<bool> = (0..n).map(|i| hit_indices.contains(&i)).collect();
        let mut at_or_above = 0usize;
        let mut null_total = 0.0f64;
        for _ in 0..RANKING_TRIALS {
            rng.shuffle(&mut labels);
            let mut h: Vec<u32> = Vec::with_capacity(hits.len());
            let mut m: Vec<u32> = Vec::with_capacity(n - hits.len());
            for (&g, &is_hit) in margins.iter().zip(&labels) {
                if is_hit {
                    h.push(g);
                } else {
                    m.push(g);
                }
            }
            let s = separation(&h, &m).unwrap_or(0.5);
            null_total += s;
            if s >= observed {
                at_or_above += 1;
            }
        }
        let p = at_or_above as f64 / RANKING_TRIALS as f64;

        let _ = writeln!(
            out,
            "\n  == RANKING BY MARGIN — the question this run exists to answer ==\
             \n  findings ranked             {n}\
             \n  credited to a real row      {}\
             \n  separation P(hit>miss)      {observed:.4}   (0.5 = no ordering)\
             \n  null mean over {RANKING_TRIALS} shuffles  {:.4}\
             \n  P(null >= observed)         {p:.4}\
             \n  ranks of credited findings  {ranks:?} of {n}\
             \n  mean rank                   {mean_rank:.1}  (random: {:.1})",
            hits.len(),
            null_total / RANKING_TRIALS as f64,
            (n + 1) as f64 / 2.0,
        );

        let base = hits.len() as f64 / n as f64;
        let mut line = String::from("  precision@k                 ");
        for k in KS {
            if k > n {
                continue;
            }
            let got = order[..k]
                .iter()
                .filter(|i| hit_indices.contains(i))
                .count();
            let _ = write!(line, "k={k}: {got}/{k}  ");
        }
        let _ = writeln!(out, "{line}\n  random expectation at any k {base:.4} of k");
    }

    /// **The harness is real, asserted before a single number is read off it.**
    ///
    /// A run that performed no inference must not be able to report a measurement,
    /// and in this session one already has: the first `typed_class_calibration`
    /// run reported `ok` having resolved a model nobody configured and called it
    /// zero times. So four things that a do-nothing run cannot produce are checked
    /// here — wall-clock over the files reviewed *this* time, a non-empty finding
    /// set, a margin on every finding, and **more than one distinct margin** — and
    /// the model id and per-file seconds are printed beside them.
    ///
    /// A fully resumed run legitimately performs no inference. That is reported
    /// loudly rather than asserted against, because the checkpoint is the whole
    /// point of being resumable; what must not happen is a *fresh* run passing
    /// these checks without having worked.
    fn prove_it_ran(
        out: &mut impl Write,
        reviewed: &[Reviewed],
        fresh: usize,
        spent: f64,
    ) -> Vec<CandidateFinding> {
        if fresh == 0 {
            let _ = writeln!(
                out,
                "\nNOTE: every file came from the checkpoint, so THIS run performed no \
                 inference. The numbers below are the recorded run's."
            );
        } else {
            assert!(
                spent > 1.0,
                "{fresh} file(s) reportedly reviewed in {spent:.3}s — a 30B model cannot \
                 do that, so no inference happened"
            );
            let _ = writeln!(
                out,
                "\n  inference: {fresh} file(s) reviewed in {spent:.1}s ({:.1}s/file)",
                spent / fresh as f64,
            );
        }

        let findings: Vec<CandidateFinding> =
            reviewed.iter().flat_map(|r| r.findings.clone()).collect();
        assert!(
            !findings.is_empty(),
            "no findings at all over {} file(s) — nothing to rank",
            reviewed.len()
        );
        for f in &findings {
            assert!(
                f.class_margin_micronats.is_some(),
                "{}:{} carries no margin, so --typed-class did not run",
                f.path,
                f.line
            );
        }
        let distinct: BTreeSet<u32> = findings
            .iter()
            .map(|f| f.class_margin_micronats.expect("just asserted"))
            .collect();
        assert!(
            distinct.len() > 1,
            "all {} findings share one margin ({distinct:?}) — there is nothing to rank, \
             and that is the read being degenerate rather than the ranking failing",
            findings.len(),
        );
        // **A zero-finding file is uninterpretable on its own.** The repository's
        // own record names three ways a clean-looking zero means nothing — an
        // empty diff, a PR head, a truncated reasoning reply — so the breakdown is
        // printed rather than left for a reader to assume "clean".
        let empty: Vec<&Reviewed> = reviewed.iter().filter(|r| r.findings.is_empty()).collect();
        let truncated = empty.iter().filter(|r| r.reasoning_truncated).count();
        let declared = empty.iter().filter(|r| r.declared_clean).count();
        let unexplained = empty.len() - truncated - declared;
        let _ = writeln!(
            out,
            "\n=== margin ranking — {} file(s), {} finding(s), {} distinct margin(s) ===\
             \n  files with zero findings    {} of {} — {declared} declared clean, \
             {truncated} never reviewed, {unexplained} neither recorded\
             \n  unparsed finding-like lines {}",
            reviewed.len(),
            findings.len(),
            distinct.len(),
            empty.len(),
            reviewed.len(),
            reviewed.iter().map(|r| r.unparsed).sum::<usize>(),
        );
        findings
    }

    /// The new-side line set per anchor, and how many corpus rows actually sit
    /// inside their own.
    ///
    /// # The check, and what measuring it found
    ///
    /// If [`diff_new_lines`] were wrong the relocation set would be wrong and so
    /// would the null, silently and in an unknown direction. The corpus's own
    /// anchors are the only known-good coordinates to check it against — so this
    /// counts how many rows land on a line their own diff shows.
    ///
    /// It was first written to **assert** that all of them do, and that assertion
    /// fired: `25 of 27`. Two rows anchor outside their own reconstructed diff —
    /// `add397f2 main.rs:2070`, whose nearest shown line is 2064, and
    /// `413f73cc config.rs:634`, whose nearest is 575. The parser is right and the
    /// assumption was wrong; the repository's own guarantee is that a row's diff
    /// *touches its anchor file*, not its anchor line. So the count is reported
    /// and the assertion is now the weaker one a broken parser would still fail:
    /// a non-empty line set per anchor, and a clear majority of rows inside.
    ///
    /// The second of those two rows matters on its own: at 59 lines from the
    /// nearest line its diff shows, **no finding can ever match it** within
    /// `LINE_WINDOW`. It is a real row outside the reachable set, and it caps
    /// recall independently of anything a reviewer does.
    fn relocation_lines(
        corpus: &Corpus,
        repo: &std::path::Path,
        out: &mut impl Write,
    ) -> BTreeMap<(String, String), Vec<u32>> {
        let main = super::main_ref(repo).expect("a main ref");
        let mut lines: BTreeMap<(String, String), Vec<u32>> = BTreeMap::new();
        for sha in corpus.reviewed_shas() {
            let set = super::files_at(repo, sha, &main, rto_graph::PathPolicy::empty())
                .expect("reconstructs");
            for FileUnderReview { path, diff, .. } in set.files {
                lines.insert((sha.to_owned(), path), diff_new_lines(&diff));
            }
        }
        let mut inside = 0usize;
        for row in corpus.rows() {
            let key = (row.reviewed_sha.clone(), row.path.clone());
            let shown = lines
                .get(&key)
                .unwrap_or_else(|| panic!("no reconstructed diff for {key:?}"));
            assert!(
                !shown.is_empty(),
                "the diff for {key:?} parsed to no new-side lines at all, so the parser \
                 is broken rather than the corpus being unusual"
            );
            if shown.contains(&row.line) {
                inside += 1;
            } else {
                let nearest = shown
                    .iter()
                    .min_by_key(|l| l.abs_diff(row.line))
                    .copied()
                    .unwrap_or(0);
                let _ = writeln!(
                    out,
                    "  row {} at {}:{} is OUTSIDE its own diff (nearest shown line {nearest}, \
                     {} away{})",
                    row.id,
                    row.path,
                    row.line,
                    nearest.abs_diff(row.line),
                    if nearest.abs_diff(row.line) > LINE_WINDOW {
                        "; unreachable, so it caps recall"
                    } else {
                        ""
                    },
                );
            }
        }
        let _ = writeln!(
            out,
            "  rows inside their own diff  {inside}/{}",
            corpus.rows().len()
        );
        assert!(
            inside * 3 >= corpus.rows().len() * 2,
            "only {inside} of {} rows land on a line their own diff shows — a working \
             parser puts nearly all of them there, so this is the parser",
            corpus.rows().len()
        );
        lines
    }

    #[test]
    #[ignore = "reviews 23 files with a 30B model; prints a measurement"]
    fn margin_ranks_the_reviewers_own_findings() {
        let mut out = std::io::stderr();
        let corpus = rto_graph::review_corpus::builtin().expect("the builtin corpus parses");
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .expect("the workspace root is two levels above this crate")
            .to_path_buf();

        let (reviewed, fresh, spent, capped) = match review_anchors(&mut out, &corpus, &repo) {
            Ok(v) => v,
            Err(e) => {
                let _ = writeln!(out, "SKIP: {e}");
                return;
            }
        };
        if capped {
            // A capped run priced the inference and stopped. It has no population
            // to measure, and running the assertions over a deliberate fragment
            // would fail for the one reason that is not interesting.
            let _ = writeln!(
                out,
                "\n  TIMING ONLY — {fresh} file(s) in {spent:.1}s ({:.1}s/file). \
                 Unset ROTEIRO_RANK_MAX_FILES for the measurement.\n",
                spent / fresh.max(1) as f64,
            );
            return;
        }

        let findings = prove_it_ran(&mut out, &reviewed, fresh, spent);

        // The observed real-row count, from the **shipped** scorer, and the
        // reimplemented matcher cross-checked against it.
        let run = CandidateRun {
            schema: rto_graph::review_score::RUN_SCHEMA.to_owned(),
            attempted_shas: reviewed.iter().map(|r| r.sha.clone()).collect(),
            findings: findings.clone(),
            verdicts: Vec::new(),
            suppressed: Vec::new(),
            arm: Some(super::run_arm(
                ReviewArm::DiffOnly,
                "qwen3-coder-30b-a3b",
                ClassSource::TypedRead,
            )),
        };
        let scored = rto_graph::review_score::score(&corpus, &run).expect("the run scores");

        let real: Vec<&CorpusRow> = corpus
            .rows()
            .iter()
            .filter(|r| r.verdict == Verdict::Real)
            .collect();
        let by_row = credited(&real, &findings);
        assert_eq!(
            by_row.len(),
            scored.found,
            "the reimplemented matcher credits {} real row(s) and the shipped scorer {} — \
             they must agree before either is believed",
            by_row.len(),
            scored.found
        );
        let _ = writeln!(
            out,
            "  matcher cross-check         {} real row(s), same as the shipped scorer",
            by_row.len()
        );

        let hit_indices: BTreeSet<usize> = by_row.values().copied().collect();
        ranking_null(&mut out, &findings, &hit_indices);

        let lines_by_anchor = relocation_lines(&corpus, &repo, &mut out);
        recall_null(&mut out, &corpus, &findings, &lines_by_anchor, scored.found);

        let _ = writeln!(
            out,
            "\n  == COMPARABILITY ==\
             \n  This is {} file(s) / {} commit(s) / {} findings, one command, one commit.\
             \n  The recorded baseline is 183 files / 15 commits / 1,995 findings, so its\
             \n  4-vs-4.19 is a DIFFERENT POPULATION and nothing above is an improvement\
             \n  on it. Only the numbers in this block's own run are compared with each\
             \n  other.\
             \n  Sample-size limit: the corpus holds 22 real and 5 known-false rows. Five\
             \n  is five, whatever any statistic over it reads.\n",
            reviewed.len(),
            corpus.reviewed_shas().len(),
            findings.len(),
        );
    }
}

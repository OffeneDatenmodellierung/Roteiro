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

use rto_graph::reviewer::FileUnderReview;

/// The half of this module that needs a generation backend. Everything outside
/// it — the diff reconstruction and the parent-module lookup — is useful and
/// tested in a build with no model, which is the build CI runs.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
use {
    rto_graph::compile_claim::{CheckRun, suppression},
    rto_graph::review_score::{CandidateFinding, CandidateRun},
    rto_graph::reviewer::{
        GraphContext, SINGLE_CALL_BUDGET_TOKENS, build_prompt, claim_site, parse_findings,
    },
    std::collections::BTreeSet,
};

/// Tokens a review of one file may generate.
///
/// Generous relative to `spec draft`'s 800: a file with several findings needs a
/// line each, and a reply cut off mid-list would be scored as the reviewer having
/// found fewer defects than it did — measuring the cap rather than the model.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
const REVIEW_MAX_TOKENS: u32 = 1_200;

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
const REVIEW_N_CTX: u32 = 40_960;

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
    let reply = crate::strip_thinking_public(&completion.content);
    // The one way to tell a reviewer that found nothing from a reviewer that was
    // asked the wrong question. Prompt work is otherwise done by staring at a
    // score, which moves for both reasons at once.
    if std::env::var_os("ROTEIRO_REVIEW_DEBUG").is_some() {
        eprintln!(
            "--- {} ({} prompt tokens est.)\n{reply}\n---",
            file.path, prompt.tokens
        );
    }
    let parsed = parse_findings(&file.reviewed_sha, &file.path, &reply);

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
        dropped_tokens: prompt.dropped_tokens,
    })
}

/// The source of the module that declares `path`, where a file's feature gate is
/// written — `src/foo.rs`'s parent is `src/lib.rs` (or `src/main.rs`), and
/// `src/a/b.rs`'s is `src/a/mod.rs` or `src/a.rs`.
fn parent_module_source(path: &str, sources: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let dir = path.rsplit_once('/').map(|(d, _)| d)?;
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

/// Run `git` in `repo` and return trimmed stdout, or `None` if it failed.
fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim_end().to_owned())
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
    let mut set = ReviewSet::default();
    for path in names.lines().filter(|p| !p.is_empty()) {
        let diff = git(repo, &["diff", "-U3", &fork, sha, "--", path]).unwrap_or_default();
        // No hunk header means there is no text to review: git emits
        // `Binary files a/… and b/… differ` for a blob, and a bare header for a
        // mode or rename change.
        if !diff.contains("@@") {
            set.skipped.push(path.to_owned());
            continue;
        }
        set.files.push(FileUnderReview {
            reviewed_sha: sha.to_owned(),
            path: path.to_owned(),
            diff,
        });
    }
    Ok(set)
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
    /// Files carrying at least one adjudicated corpus row.
    pub anchored_files: usize,
    /// Changed paths with no reviewable diff — binary blobs, mode and rename
    /// records. Reported so a reduced denominator is visible rather than assumed.
    pub skipped: usize,
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

    let mut run = CandidateRun::default();
    let mut report = ReplayReport::default();
    for (idx, sha) in shas.iter().enumerate() {
        let set = files_at(repo, sha, &main)?;
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
            // A file the engine refuses is recorded and stepped over, never fatal.
            // A three-hour pass that dies on file 140 has measured nothing, and the
            // refusals are themselves a result: they say which files this budget
            // cannot actually review.
            let outcome = match review_file(
                &engine,
                model,
                file,
                &GraphContext::none(),
                &checks,
                &sources,
            ) {
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

/// Review the working-tree change (or a `base..HEAD` range) with the model.
///
/// # Errors
/// If the repository, the model or git cannot be used.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
pub fn run_llm(repo: &Path, base: Option<&str>, checks_path: Option<&str>) -> anyhow::Result<()> {
    let checks = match checks_path {
        Some(p) => read_checks(p)?,
        None => Vec::new(),
    };
    let head = git(repo, &["rev-parse", "HEAD"]).unwrap_or_else(|| "HEAD".to_owned());
    let range: Vec<String> = match base {
        Some(b) => vec![b.to_owned(), "HEAD".to_owned()],
        None => vec!["HEAD".to_owned()],
    };
    let mut args: Vec<&str> = vec!["diff", "--name-only"];
    args.extend(range.iter().map(String::as_str));
    let names = git(repo, &args).unwrap_or_default();

    let files: Vec<FileUnderReview> = names
        .lines()
        .filter(|p| !p.is_empty())
        .filter_map(|path| {
            let mut d: Vec<&str> = vec!["diff", "-U3"];
            d.extend(range.iter().map(String::as_str));
            d.extend(["--", path]);
            let diff = git(repo, &d)?;
            (!diff.is_empty()).then(|| FileUnderReview {
                reviewed_sha: head.clone(),
                path: path.to_owned(),
                diff,
            })
        })
        .collect();

    if files.is_empty() {
        println!("no changes to review");
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

    let mut total = 0usize;
    let mut withheld = 0usize;
    for file in &files {
        let sources = |p: &str| std::fs::read_to_string(repo.join(p)).ok();
        let outcome = review_file(
            &engine,
            model,
            file,
            &GraphContext::none(),
            &checks,
            &sources,
        )?;
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
    use super::{corpus_shas, files_at, fork_point, main_ref, parent_module_source};
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
        assert!(
            REVIEW_N_CTX > 4_096,
            "the engine default is what broke this"
        );
        let worst_case = REVIEW_PROMPT_BUDGET * 13 / 10 + REVIEW_MAX_TOKENS as usize;
        assert!(
            REVIEW_N_CTX as usize >= worst_case,
            "a prompt `len / 4` understated by 30% plus its generation is \
             {worst_case} tokens, over the {REVIEW_N_CTX}-token window"
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
        assert!(rto_graph::ModelTask::Review.goes_remote());
    }
}

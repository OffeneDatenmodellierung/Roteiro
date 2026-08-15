//! MTP speculative decoding against a real model (issue #320).
//!
//! The mechanism — how a round accepts proposals, and what the sampler is asked
//! to do while it does — is pinned without a model or a GPU by `rto_llama`'s
//! `speculative` unit tests, which Ubuntu CI runs. This file is for the two
//! claims that **cannot** be made without weights:
//!
//! 1. **The output does not change.** Speculative decoding is only sound if it
//!    is invisible: same seed, same sampler, same tokens. The code is written so
//!    that this follows from how the sampler is driven, but the last link is
//!    llama.cpp's own arithmetic — a batch of four tokens and four batches of one
//!    are not *guaranteed* to give bit-identical logits on a GPU, because the
//!    kernels differ. So it is checked here, against the real thing, over the
//!    three kinds of output whose acceptance rates differ most.
//! 2. **A model with no draft head falls back**, silently and successfully,
//!    rather than failing the request.
//!
//! The third assertion is the **exit status of this binary**, as in issues #292,
//! #298 and #301: the draft context is native state that must be gone before
//! ggml-metal's global destructors run at `exit()`. It is a stack local inside a
//! generation, so it cannot outlive the engine — and the `release_shared_backend`
//! assertions at the end of each test are what would notice if that ever stopped
//! being true.
//!
//! # Running it
//!
//! Every test **self-skips** with a printed reason when its model is not
//! present, so CI compiles this file under `--all-features` and prints skip lines
//! rather than failing. Point `ROTEIRO_MTP_TEST_GGUF` at any GGUF that ships an
//! MTP head, or install one of the registry models listed in [`MTP_MODELS`]:
//!
//! ```text
//! export ROTEIRO_MTP_TEST_GGUF=/path/to/Qwen3.5-9B-Q4_K_M.gguf
//! cargo test -p rto-llama --features llama --test speculative -- --nocapture
//! # and the before/after measurement, which is slow and so is `#[ignore]`d:
//! cargo test -p rto-llama --features llama --test speculative -- --ignored --nocapture
//! ```
#![cfg(feature = "llama")]

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use rto_llama::backend::release_shared_backend;
use rto_llama::llama::{LlamaEngine, Served};
use rto_llama::{ChatRequest, Engine, Message};

/// Registry names (`~/.roteiro/models/<name>/model.gguf`) that are known to ship
/// an MTP draft head, tried in order when `ROTEIRO_MTP_TEST_GGUF` is unset.
const MTP_MODELS: &[&str] = &["qwen3.8-27b", "qwen3.5-9b", "qwen3.5-4b"];

/// A registry model known **not** to ship a draft head, for the fallback test.
/// Small, already a fixture of this crate's other tests, and generative (so the
/// chat path does not reject it the way it rejects an encoder-only embedder).
const NO_MTP_MODEL: &str = "smolvlm-500m-gguf";

/// The environment variable pointing at a GGUF with an MTP head.
const GGUF_ENV: &str = "ROTEIRO_MTP_TEST_GGUF";

/// The name this file serves its model under. Nothing depends on it matching a
/// registry entry — [`Served`] is just a name-to-path pair — which is what lets
/// the tests run against a GGUF anywhere on disk.
const MODEL: &str = "mtp-under-test";

/// The three kinds of completion whose draft-acceptance rates differ most, and
/// the reason a single averaged number would be a bad way to report this.
///
/// Roteiro's served workload is the first two: `<tool_call>` emission for the
/// graph tools, and code and ADR prose from `spec draft`. Prose is here as the
/// pessimistic case — the one where speculative decoding is expected to buy
/// least — so that a headline figure cannot be quoted without it.
const PROMPTS: &[(&str, &str)] = &[
    (
        "code",
        "Write a Rust function `fn lru_evict_count(sizes: &[u64], budget: u64) -> usize` that \
         returns how many of the oldest entries to drop so the rest fit the budget, always \
         keeping at least the last one. Output only the code.",
    ),
    (
        "tool-call",
        "You have one tool: search(query: string). Emit a call to it, and nothing else, in \
         exactly this form:\n<tool_call>\n{\"name\": \"search\", \"arguments\": {\"query\": \
         \"...\"}}\n</tool_call>\nThe user asked: where is the model residency cache?",
    ),
    (
        "prose",
        "Explain, in two short paragraphs of plain prose with no code and no lists, why \
         generating text on a laptop GPU is limited by memory bandwidth rather than by \
         arithmetic.",
    ),
];

/// Tokens per completion. Long enough for the acceptance rate to settle and for a
/// divergence to have somewhere to show up; short enough that the identity test
/// runs six completions in a reasonable time.
const MAX_TOKENS: u32 = 192;

/// The llama.cpp backend is a process-global and this binary builds and releases
/// it, so the harness's default parallelism would let one test's release land
/// inside another's engine lifetime. Every test takes this first.
static SERIAL: Mutex<()> = Mutex::new(());

/// Enter the exclusive section from a known-clean state. A poisoned lock only
/// means an earlier test panicked; recover rather than cascade.
fn exclusive() -> MutexGuard<'static, ()> {
    let guard = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    let _released = release_shared_backend();
    guard
}

/// A file in the default model store (`~/.roteiro/models/<name>/<file>`), or
/// `None` when it is not installed.
fn model_file(name: &str, file: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    let path = PathBuf::from(home)
        .join(".roteiro")
        .join("models")
        .join(name)
        .join(file);
    path.exists().then_some(path)
}

/// A GGUF that ships an MTP draft head, or `None` (with a skip line) when there
/// is none to be found.
fn mtp_gguf() -> Option<PathBuf> {
    if let Some(raw) = std::env::var_os(GGUF_ENV) {
        let path = PathBuf::from(raw);
        if path.exists() {
            return Some(path);
        }
        eprintln!(
            "SKIP: {GGUF_ENV} is set but {} does not exist",
            path.display()
        );
        return None;
    }
    if let Some(path) = MTP_MODELS
        .iter()
        .find_map(|name| model_file(name, "model.gguf"))
    {
        return Some(path);
    }
    eprintln!("SKIP: no MTP-capable GGUF found — set {GGUF_ENV}, or install one of {MTP_MODELS:?}");
    None
}

/// An engine over one GGUF, with speculation forced on or off.
fn engine(path: &Path, speculative: bool) -> LlamaEngine {
    LlamaEngine::new(
        vec![Served {
            name: MODEL.to_owned(),
            path: path.to_path_buf(),
            mmproj: None,
        }],
        0,
    )
    .expect("engine builds")
    .with_speculative(speculative)
}

/// One completion, at temperature 0 so the sampler is greedy and the run is
/// reproducible — which is the setting the identity claim is made under.
///
/// Returns the text and the engine's own token count, so the measurement below
/// divides by tokens the decoder actually emitted rather than by a word count.
fn complete(engine: &LlamaEngine, prompt: &str) -> (String, u32) {
    let completion = engine
        .chat(&ChatRequest {
            model: MODEL.to_owned(),
            messages: vec![Message {
                role: "user".to_owned(),
                content: prompt.to_owned(),
            }],
            images: Vec::new(),
            audio: Vec::new(),
            temperature: 0.0,
            max_tokens: MAX_TOKENS,
        })
        .expect("the request completes");
    (completion.content, completion.completion_tokens)
}

/// The first index at which two strings differ, for an error message that says
/// *where* speculation diverged rather than dumping two paragraphs.
fn first_difference(a: &str, b: &str) -> Option<usize> {
    a.char_indices()
        .zip(b.char_indices())
        .find_map(|((i, x), (_, y))| (x != y).then_some(i))
        .or_else(|| (a.len() != b.len()).then_some(a.len().min(b.len())))
}

/// **The claim the whole feature rests on**: with a fixed seed and sampler,
/// speculative decoding produces the *same tokens* as plain decoding.
///
/// Not "the same distribution" and not "similar text" — identical strings. A
/// speculative decoder that changes the output is not a faster decoder, it is a
/// different model, and the failure mode it produces in the field is "the model
/// got worse" with no cause anyone can point at. So this is asserted directly,
/// over all three prompt kinds, rather than assumed from the library's contract.
///
/// Two engines rather than one, so the runs share nothing but the GGUF on disk.
#[test]
fn speculation_does_not_change_the_output() {
    let _serial = exclusive();
    let Some(path) = mtp_gguf() else {
        return;
    };

    let plain = engine(&path, false);
    let spec = engine(&path, true);

    for (kind, prompt) in PROMPTS {
        let (without, _) = complete(&plain, prompt);
        let (with, _) = complete(&spec, prompt);
        assert!(
            !with.trim().is_empty(),
            "{kind}: speculative decoding produced nothing"
        );
        if let Some(at) = first_difference(&without, &with) {
            panic!(
                "{kind}: speculative decoding changed the output at char {at}\n\
                 plain:       {:?}\n\
                 speculative: {:?}",
                &without[at.saturating_sub(40)..without.len().min(at + 40)],
                &with[at.saturating_sub(40)..with.len().min(at + 40)],
            );
        }
    }

    let stats = spec.speculative_stats();
    eprintln!(
        "speculative stats: {stats:?} acceptance={:?}",
        stats.acceptance_rate()
    );
    assert_eq!(
        stats.activations,
        PROMPTS.len() as u64,
        "every text generation on a model with a draft head must have used it — \
         identical output is only evidence of anything if speculation actually ran"
    );
    assert!(
        stats.accepted > 0,
        "the draft head proposed {} tokens and none were accepted: the outputs match \
         because nothing was ever drafted, which is not what this test is checking",
        stats.drafted
    );
    assert_eq!(
        plain.speculative_stats().activations,
        0,
        "the control engine must have taken the plain path"
    );

    drop(plain);
    drop(spec);
    assert!(
        release_shared_backend(),
        "with both engines gone the backend is releasable — the draft contexts went with \
         their generations, so nothing native outlives the engine that owned it"
    );
}

/// A model with no draft head must decode exactly as it always did: no error, no
/// warning path, no speculation. This is the case that has to keep working for
/// every model Roteiro served before Qwen3.5.
#[test]
fn a_model_without_a_draft_head_falls_back_cleanly() {
    let _serial = exclusive();
    let Some(path) = model_file(NO_MTP_MODEL, "model.gguf") else {
        eprintln!("SKIP: `{NO_MTP_MODEL}` not installed (run `roteiro model pull {NO_MTP_MODEL}`)");
        return;
    };

    // Speculation is switched *on*, so the fallback is doing the work here, not
    // the configuration.
    let engine = engine(&path, true);
    let (text, _) = complete(&engine, "Name three primary colours.");
    assert!(
        !text.trim().is_empty(),
        "a model with no draft head must still generate"
    );
    assert_eq!(
        engine.speculative_stats().activations,
        0,
        "no draft head means the plain path, not a failed request and not a silent \
         half-speculative one"
    );

    drop(engine);
    assert!(release_shared_backend(), "nothing outlives the engine");
}

/// One timed completion, in tokens per second.
fn rate(engine: &LlamaEngine, prompt: &str) -> f64 {
    let start = Instant::now();
    let (text, tokens) = complete(engine, prompt);
    let elapsed = start.elapsed().as_secs_f64();
    assert!(!text.trim().is_empty());
    f64::from(tokens) / elapsed
}

/// The median of a small sample.
fn median(mut xs: Vec<f64>) -> f64 {
    xs.sort_by(f64::total_cmp);
    xs[xs.len() / 2]
}

/// How many timed pairs to run per prompt kind. Each pair is one plain and one
/// speculative completion, back to back.
const REPS: usize = 5;

/// The before/after measurement (issue #320's deliverable), printed rather than
/// asserted.
///
/// Deliberately **not** an assertion: tok/s on a shared laptop GPU is not a
/// contract, and a threshold here would either be so loose as to prove nothing or
/// flaky enough to be disabled within a month. What is worth pinning — how the
/// two paths relate — is pinned above.
///
/// **The protocol matters more than the numbers.** A developer laptop is a
/// contended machine: a background build can halve the absolute tok/s of both
/// arms while the run is in progress, and a single before-then-after measurement
/// silently attributes that drift to the change. So this:
///
/// * holds **both** engines resident for the whole run, so neither arm pays a
///   model load the other does not;
/// * **interleaves** them — plain, speculative, plain, speculative — so load
///   drift lands on both arms alike;
/// * reports the **median of the per-pair ratios** rather than the ratio of the
///   medians, because a ratio measured inside one pair is what cancels the drift;
/// * prints the spread of the absolute rates, so a reader can see how contended
///   the machine was rather than having to trust that it was not.
///
/// `#[ignore]` because it runs `3 × 2 × REPS` completions on a large model.
#[test]
#[ignore = "measurement: needs a large MTP model and takes minutes"]
fn measure_speculative_speedup() {
    let _serial = exclusive();
    let Some(path) = mtp_gguf() else {
        return;
    };
    eprintln!("model: {}", path.display());

    let plain = engine(&path, false);
    let spec = engine(&path, true);
    // Warm both models into residency and the file into the page cache, so the
    // timed runs measure decoding rather than loading.
    let _warm = complete(&plain, "Say OK.");
    let _warm = complete(&spec, "Say OK.");

    for (kind, prompt) in PROMPTS {
        let mut plain_rates = Vec::with_capacity(REPS);
        let mut spec_rates = Vec::with_capacity(REPS);
        let mut ratios = Vec::with_capacity(REPS);
        // Acceptance is cumulative on the engine, so take the difference across
        // this kind's completions: acceptance *per prompt kind* is the number
        // that explains why the speedups differ.
        let before = spec.speculative_stats();
        for _ in 0..REPS {
            let p = rate(&plain, prompt);
            let s = rate(&spec, prompt);
            ratios.push(s / p);
            plain_rates.push(p);
            spec_rates.push(s);
        }
        let after = spec.speculative_stats();
        let drafted = after.drafted - before.drafted;
        let acceptance = if drafted == 0 {
            "n/a".to_owned()
        } else {
            #[allow(clippy::cast_precision_loss)]
            let rate = (after.accepted - before.accepted) as f64 / drafted as f64;
            format!("{:.0}%", rate * 100.0)
        };
        eprintln!(
            "{kind:<10} plain {:6.2} tok/s [{:.1}–{:.1}]  speculative {:6.2} tok/s [{:.1}–{:.1}]  \
             median ratio {:.2}x  (acceptance {acceptance})",
            median(plain_rates.clone()),
            plain_rates.iter().copied().fold(f64::MAX, f64::min),
            plain_rates.iter().copied().fold(f64::MIN, f64::max),
            median(spec_rates.clone()),
            spec_rates.iter().copied().fold(f64::MAX, f64::min),
            spec_rates.iter().copied().fold(f64::MIN, f64::max),
            median(ratios),
        );
    }

    drop(plain);
    drop(spec);
    assert!(release_shared_backend(), "nothing outlives the engines");
}

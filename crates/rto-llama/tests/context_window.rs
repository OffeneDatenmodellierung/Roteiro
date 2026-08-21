//! What a context window actually costs, and why one number cannot serve every
//! model (issue #486).
//!
//! `DEFAULT_N_CTX` was 4096 for every served model, and unreachable from
//! configuration. Raising it is not free the way #349's `n_batch` change was
//! free, and conflating the two is the mistake this file exists to prevent:
//!
//! * **`n_batch`** (#349) costs a few token/pos/seq arrays. Raising it moved
//!   median RSS by −32 KiB — noise — because `n_ubatch` sizes the compute graph
//!   and stays at 512.
//! * **`n_ctx`** costs **KV cache, linearly**, and llama.cpp allocates it
//!   *eagerly* in the `llama_kv_cache` constructor
//!   (`ggml_backend_alloc_ctx_tensors_from_buft`, "real buffer"). Since
//!   `LlamaEngine::new_context` builds a context **per generation**, a large
//!   fixed window would pay that allocation on every request, including a
//!   fifty-token one.
//!
//! That is the whole argument for sizing a context to the request rather than to
//! a fixed maximum, so it is measured here rather than asserted.
//!
//! **What this instrument does not see** (issue #578). The cost reported here is
//! a `ps` RSS delta, and RSS accounts for the KV and recurrent buffers but *not*
//! for ggml's compute buffers. On `qwen3.8-27b` at `n_ctx = 4096` llama.cpp
//! reports allocating 256 MiB KV + 149.62 MiB recurrent + **509.02 MiB Metal
//! compute + 24.02 MiB CPU compute**, against the 429 MiB this file prints —
//! and a real 2,001-token decode adds only 35 MiB more, so the compute buffers
//! are not merely waiting to be faulted in. "Metal is invisible to `ps`" is not
//! the explanation either: the KV and recurrent buffers are `MTL0` allocations
//! too, and they are counted. Why the compute buffers differ is unresolved, and
//! `ps` cannot answer it — `phys_footprint` or `vmmap` would be the instrument.
//!
//! Read these numbers, then, as **KV + recurrent**, which is what the
//! per-request-sizing argument turns on and which they measure faithfully. The
//! trap is tuning `n_ubatch` from them: `n_ubatch` scales precisely the buffer
//! this cannot see, so a sweep to 2048 moves RSS by 44 MiB while moving the real
//! allocation by 1,527 MiB, and reads as nearly free when it is not. See
//! `speculative::base_params` for that measurement.
//!
//! These need the `llama` feature **and** a GGUF on disk, so they are
//! `#[ignore]`d and self-skip with a printed reason when the model is absent —
//! CI compiles them under `--all-features` without running them, exactly as
//! `batch_capacity.rs` does.
//!
//! ```text
//! cargo test -p rto-llama --features llama --test context_window -- --ignored --nocapture
//! ```
#![cfg(feature = "llama")]

use std::num::NonZeroU32;
use std::path::PathBuf;

use llama_cpp_2::context::params::{KvCacheType, LlamaContextParams};
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::model::LlamaModel;
use llama_cpp_2::model::params::LlamaModelParams;

/// The primary generative model, and the one whose trained window (262,144) is
/// 64× the old default. The expensive arm.
const BIG_MODEL: &str = "qwen3.8-27b";

/// A small generative model, for the arm that can run anywhere. Its trained
/// window is 8,192 — already twice the old default.
const SMALL_MODEL: &str = "smolvlm-500m-gguf";

/// The BERT embedding model whose trained window is **512** — an eighth of the
/// old default, and the reason a single engine-wide `n_ctx` cannot be right.
const EMBED_MODEL: &str = "bge-large-en-v1.5";

/// The measured served-tool overhead this issue is named for: the prompt the
/// client has not contributed to yet.
const TOOL_SURFACE_TOKENS: u32 = 3146;

/// The default model store (`~/.roteiro/models/<name>/model.gguf`).
fn model_gguf(name: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    let path = PathBuf::from(home)
        .join(".roteiro")
        .join("models")
        .join(name)
        .join("model.gguf");
    path.exists().then_some(path)
}

/// Resident-set size of this process, in KiB, via `ps` — the workspace forbids
/// `unsafe_code`, which rules out asking the kernel directly. The same helper
/// `batch_capacity.rs` measures with.
fn rss_kib() -> u64 {
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

/// Build one context at `n_ctx` and report the resident memory that appeared
/// while it existed, in KiB, alongside what llama.cpp settled on for the two
/// batch widths.
///
/// `n_batch` follows `n_ctx` exactly as `speculative::base_params` sets it, so
/// what this measures is the shape the engine really builds — not a context
/// configured specially for the test.
fn arm(backend: &LlamaBackend, model: &LlamaModel, n_ctx: u32, quantised: bool) -> (i64, u32, u32) {
    let base = LlamaContextParams::default()
        .with_n_ctx(Some(NonZeroU32::new(n_ctx).expect("nonzero")))
        .with_n_batch(n_ctx);
    let params = if quantised {
        base.with_type_k(KvCacheType::Q8_0)
            .with_type_v(KvCacheType::Q8_0)
    } else {
        base
    };
    let before = rss_kib();
    let ctx = model
        .new_context(backend, params)
        .expect("context builds at the requested window");
    let after = rss_kib();
    let widths = (ctx.n_batch(), ctx.n_ubatch());
    drop(ctx);
    (
        i64::try_from(after).unwrap_or(i64::MAX) - i64::try_from(before).unwrap_or(0),
        widths.0,
        widths.1,
    )
}

/// The headline measurement: what a context costs at a series of window sizes on
/// the primary model, and what the served-tool overhead becomes as a share of
/// each.
///
/// This is the evidence for per-request sizing. If the cost were flat, a large
/// fixed `DEFAULT_N_CTX` would be the whole fix and nothing else would be
/// needed; it is not flat, and the shape of the curve is the argument.
#[test]
#[ignore = "needs `qwen3.8-27b` under ~/.roteiro/models; prints a measurement"]
fn measure_what_a_window_costs_on_the_primary_model() {
    let Some(path) = model_gguf(BIG_MODEL) else {
        eprintln!("SKIP: need `{BIG_MODEL}` under ~/.roteiro/models");
        return;
    };
    let backend = LlamaBackend::init().expect("backend");
    let model = LlamaModel::load_from_file(&backend, &path, &LlamaModelParams::default())
        .expect("model loads");
    let trained = model.n_ctx_train();

    eprintln!("{BIG_MODEL}: n_ctx_train={trained}");
    eprintln!(
        "{:>10}  {:>12}  {:>10}  {:>8}  {:>8}",
        "n_ctx", "RSS delta", "tool %", "n_batch", "n_ubatch"
    );
    for n_ctx in [4096_u32, 32768, 131_072, 262_144] {
        if n_ctx > trained {
            continue;
        }
        let (cost, logical, physical) = arm(&backend, &model, n_ctx, false);
        let pct = f64::from(TOOL_SURFACE_TOKENS) * 100.0 / f64::from(n_ctx);
        eprintln!(
            "{n_ctx:>10}  {:>9} MiB  {pct:>9.1}%  {logical:>8}  {physical:>8}",
            cost / 1024
        );
    }

    // The claim under test is that the cost is *not* flat — that is what makes a
    // large fixed window expensive and per-request sizing worthwhile.
    let (small, _, _) = arm(&backend, &model, 4096, false);
    let (large, _, _) = arm(&backend, &model, 131_072, false);
    assert!(
        large > small * 4,
        "a 32x window should cost far more than the 4k one \
         (4096: {small} KiB, 131072: {large} KiB) — if it does not, \
         the eager-allocation premise of issue #486 is wrong"
    );
}

/// Whether `n_batch` following `n_ctx` is still close to free at a *large*
/// window — re-measured, not extrapolated.
///
/// #349 established that `.with_n_batch(n_ctx)` costs nothing meaningful,
/// because `n_ubatch` sizes the compute graph and stays at 512. But it measured
/// that at `n_ctx = 4096`, and the batch's own token/pos/seq arrays scale with
/// `n_batch` — so at 262,144 the same claim is a 64× extrapolation. This holds
/// `n_ctx` fixed and moves only `n_batch`, which is the only way to attribute
/// the cost to one of the two.
#[test]
#[ignore = "needs `qwen3.8-27b` under ~/.roteiro/models; prints a measurement"]
fn measure_whether_a_wide_batch_is_still_cheap_at_a_large_window() {
    /// Large enough that a 64x extrapolation from #349's 4096 would be a guess,
    /// small enough to build twice without straining a 64 GB machine.
    const N_CTX: u32 = 131_072;

    let Some(path) = model_gguf(BIG_MODEL) else {
        eprintln!("SKIP: need `{BIG_MODEL}` under ~/.roteiro/models");
        return;
    };
    let backend = LlamaBackend::init().expect("backend");
    let model = LlamaModel::load_from_file(&backend, &path, &LlamaModelParams::default())
        .expect("model loads");

    // Same window both times; only the logical batch moves. Whatever the KV
    // cache costs is therefore common to both arms and cancels.
    let ctx_at = |n_batch: u32| -> (i64, u32, u32) {
        let params = LlamaContextParams::default()
            .with_n_ctx(Some(NonZeroU32::new(N_CTX).expect("nonzero")))
            .with_n_batch(n_batch);
        let before = rss_kib();
        let ctx = model.new_context(&backend, params).expect("context builds");
        let after = rss_kib();
        let widths = (ctx.n_batch(), ctx.n_ubatch());
        drop(ctx);
        (
            i64::try_from(after).unwrap_or(i64::MAX) - i64::try_from(before).unwrap_or(0),
            widths.0,
            widths.1,
        )
    };

    let (narrow, narrow_b, narrow_u) = ctx_at(512);
    let (wide, wide_b, wide_u) = ctx_at(N_CTX);
    eprintln!(
        "at n_ctx={N_CTX}: n_batch={narrow_b} (ubatch {narrow_u}) {} MiB \
         vs n_batch={wide_b} (ubatch {wide_u}) {} MiB — delta {:+} MiB",
        narrow / 1024,
        wide / 1024,
        (wide - narrow) / 1024,
    );

    // The reason #349's conclusion survives the 64x extrapolation: the physical
    // batch, which sizes the compute graph, is untouched by either arm.
    assert_eq!(
        narrow_u, wide_u,
        "n_ubatch must stay at llama.cpp's 512 in both arms"
    );
}

/// Whether KV quantisation is reachable from this binding, and what it buys.
///
/// `llama-cpp-2` exposes `with_type_k` / `with_type_v` taking a `KvCacheType`
/// (`context/params/get_set.rs:523,552`), so the answer is yes; this measures
/// the size of the prize rather than restating the API.
#[test]
#[ignore = "needs `qwen3.8-27b` under ~/.roteiro/models; prints a measurement"]
fn measure_what_kv_quantisation_buys() {
    /// The window the two KV types are compared at — the same one the batch
    /// arm uses, so the two measurements are read against one baseline.
    const N_CTX: u32 = 131_072;

    let Some(path) = model_gguf(BIG_MODEL) else {
        eprintln!("SKIP: need `{BIG_MODEL}` under ~/.roteiro/models");
        return;
    };
    let backend = LlamaBackend::init().expect("backend");
    let model = LlamaModel::load_from_file(&backend, &path, &LlamaModelParams::default())
        .expect("model loads");

    let (f16, _, _) = arm(&backend, &model, N_CTX, false);
    let (q8, _, _) = arm(&backend, &model, N_CTX, true);
    eprintln!(
        "{BIG_MODEL} at n_ctx={N_CTX}: f16 KV {} MiB, q8_0 KV {} MiB ({:.2}x)",
        f16 / 1024,
        q8 / 1024,
        f64::from(i32::try_from(f16).unwrap_or(i32::MAX))
            / f64::from(i32::try_from(q8.max(1)).unwrap_or(1)),
    );
}

/// What the change actually buys, end to end: the window a *real* served
/// request is given, and what it costs, before and after.
///
/// The two arms are the two policies, not two configurations of one policy:
///
/// * **before** — one fixed window for every request, whatever it asked for.
/// * **after** — [`window_for_request`]'s answer for that same request.
///
/// The point is that "after" is *both* larger where it matters and smaller where
/// it does not, which no single fixed number can be. A fixed window large enough
/// for the third row would pay the third row's memory on the first.
#[test]
#[ignore = "needs `qwen3.8-27b` under ~/.roteiro/models; prints a measurement"]
fn measure_before_and_after_on_real_request_shapes() {
    /// The window every request used to get, from `DEFAULT_N_CTX`.
    const BEFORE: u32 = 4_096;
    /// `DEFAULT_MAX_TOKENS` in `rto-serve` — the generation budget a served
    /// request reserves when the client names none.
    const MAX_TOKENS: u32 = 512;
    /// The headroom `window_for_request` adds. Restated rather than imported:
    /// these tests drive the public engine, not the crate's internals.
    const HEADROOM: u32 = 64;
    /// The large shape below, as a constant so the claim made about it is a
    /// compile-time fact rather than a runtime assertion on a literal.
    const LARGE_PROMPT: u32 = 120_000;
    // The large request is the one the old fixed window could not serve at all:
    // 120,000 tokens against a 4,096-token context is a 400, not a slow answer.
    // A `const` claim because both sides are constants — it pins the shape of
    // the table below rather than measuring anything.
    const _: () = assert!(
        LARGE_PROMPT + MAX_TOKENS > BEFORE,
        "the large shape must be one the old window refused"
    );

    let Some(path) = model_gguf(BIG_MODEL) else {
        eprintln!("SKIP: need `{BIG_MODEL}` under ~/.roteiro/models");
        return;
    };
    let backend = LlamaBackend::init().expect("backend");
    let model = LlamaModel::load_from_file(&backend, &path, &LlamaModelParams::default())
        .expect("model loads");
    let trained = model.n_ctx_train();

    // The three shapes issue #486 is actually about.
    let shapes: [(&str, u32); 3] = [
        ("a fifty-token question", 50),
        ("the served tool surface alone", TOOL_SURFACE_TOKENS),
        ("a large tool result in context", LARGE_PROMPT),
    ];

    eprintln!("{BIG_MODEL}: n_ctx_train={trained}, before = fixed {BEFORE} for every request");
    for (what, prompt) in shapes {
        let after = (prompt + MAX_TOKENS + HEADROOM).max(BEFORE).min(trained);
        let (before_cost, _, _) = arm(&backend, &model, BEFORE, false);
        let (after_cost, _, _) = arm(&backend, &model, after, false);
        let fits_before = prompt + MAX_TOKENS <= BEFORE;
        eprintln!(
            "  {what:<32} prompt={prompt:>7}  before: {BEFORE:>7} tok / {:>6} MiB {}  \
             after: {after:>7} tok / {:>6} MiB",
            before_cost / 1024,
            if fits_before { "(fits)" } else { "(REFUSED)" },
            after_cost / 1024,
        );
    }

    // The number issue #486 is titled after: what share of the window the served
    // tool surface consumes before the client has sent anything. Under a fixed
    // window that share is a constant and the conversation gets what is left;
    // under per-request sizing the window grows with the conversation, so the
    // share falls away instead of competing with it.
    let before_share = f64::from(TOOL_SURFACE_TOKENS) * 100.0 / f64::from(BEFORE);
    let before_left = BEFORE.saturating_sub(TOOL_SURFACE_TOKENS + MAX_TOKENS);
    let at_trained = f64::from(TOOL_SURFACE_TOKENS) * 100.0 / f64::from(trained);
    let at_trained_left = trained.saturating_sub(TOOL_SURFACE_TOKENS + MAX_TOKENS);
    eprintln!(
        "\n  tool surface ({TOOL_SURFACE_TOKENS} tok) as a share of the window:\n\
         \x20   before: {before_share:.1}% of {BEFORE} — {before_left} tokens left for the \
         conversation\n\
         \x20   after:  {at_trained:.1}% of {trained} at this model's ceiling — \
         {at_trained_left} tokens left"
    );

    // And it is servable now, because the model was always trained for it —
    // this one does depend on the model, so it stays a runtime assertion.
    assert!(
        LARGE_PROMPT + MAX_TOKENS + HEADROOM <= trained,
        "the large shape must fit the trained window ({trained})"
    );
}

/// The 512× spread that rules out a single engine-wide window, read from the
/// GGUFs themselves rather than from model cards.
#[test]
#[ignore = "needs GGUFs under ~/.roteiro/models; prints a measurement"]
fn measure_the_spread_in_trained_windows() {
    let backend = LlamaBackend::init().expect("backend");
    let mut seen = 0;
    for name in [BIG_MODEL, SMALL_MODEL, EMBED_MODEL] {
        let Some(path) = model_gguf(name) else {
            eprintln!("SKIP {name}: not installed");
            continue;
        };
        let model = LlamaModel::load_from_file(&backend, &path, &LlamaModelParams::default())
            .expect("model loads");
        let trained = model.n_ctx_train();
        let arch = model
            .meta_val_str("general.architecture")
            .unwrap_or_else(|_| "?".to_owned());
        eprintln!("{name:<24} arch={arch:<10} n_ctx_train={trained}");
        seen += 1;
    }
    assert!(seen > 0, "no model was available to measure");
}

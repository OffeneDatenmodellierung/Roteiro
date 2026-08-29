//! The llama.cpp-backed [`Engine`] (ADR-0006), behind the `llama` feature.
//!
//! Loads a plain GGUF (embedded tokenizer + chat template come for free). Serves
//! only the models it was handed; never downloads.
//!
//! **Model residency.** Loaded models are held in a small memory-bounded LRU
//! ([`ModelCache`]): each request loads its model on demand and keeps it warm,
//! and when the resident set exceeds the byte budget the least-recently-used
//! models are unloaded — so a process serving several models (or alternating
//! between an embedding and a generative model) swaps them in and out in real
//! time instead of thrashing a single slot, while a memory-limited host caps how
//! many stay resident. The default budget keeps a single model, matching the
//! previous one-slot behaviour.
//!
//! **Concurrency.** The cache mutex is held only long enough to resolve, load,
//! and hand out an [`Arc<LlamaModel>`] — it is released *before* generation, so
//! two requests to *different* resident models decode concurrently (the common
//! embedding + generative mixed workload no longer head-of-line-blocks). Requests
//! to the *same* model instance are serialised through that model's own
//! `gen_lock`, side-stepping any question of concurrent decode sharing one
//! model's llama.cpp state; the [`Arc`] also keeps a model alive for an in-flight
//! request even if it is evicted from the cache meanwhile. (Cross-model
//! concurrency on the Metal backend is exercised by the `--ignored` stress test
//! in `tests/concurrency.rs`.)
//!
//! **Teardown.** An [`LlamaEngine`] owns native state that a GPU backend expects
//! to be handed back: on Metal, a loaded model's buffers stay registered in the
//! device's residency set until the model is freed, and ggml-metal's own
//! teardown — which libc runs from `exit()`, after `main` — asserts that set is
//! empty. An engine must therefore be **dropped** before the process exits.
//! Parking one in a `static` (which Rust never drops) turns a successful run into
//! a SIGABRT at exit; Roteiro issue #291 is exactly that, and `rto-graph`'s
//! `release_media_engines` is how its cached engines are given a deterministic
//! end of life.
//!
//! The backend an engine holds is the process's **shared** one ([`crate::backend`],
//! issue #296) — llama.cpp permits exactly one — so the "models before the
//! backend" ordering now spans engines as well as struct fields: the backend is
//! freed only once *no* engine borrows it, which is a fact about `Arc` ownership
//! rather than a rule callers have to remember.
//!
//! **Projector residency (issue #301).** The multimodal projector an `mmproj`
//! GGUF holds used to be loaded per media blob: a sync over twenty audio files
//! re-read the 688 MB Voxtral projector twenty times, building and freeing a clip
//! context and its GPU buffers each time. It is now built once per
//! `(loaded model, mmproj path)` and reused — see [`Projector`] and
//! [`LlamaEngine::projector`].
//!
//! What that is worth depends on the host, and less than the issue expected on
//! the one it was measured on: an `mmproj` is `mmap`ed, so a repeat load of a
//! page-cache-warm file costs system CPU rather than wall-clock. Over a six-clip
//! sync on an M5 Pro the five avoided loads moved wall time by less than half a
//! percent while cutting kernel CPU by about a third; the ~5 s per clip reported
//! in #299 is the clip's own encode-and-generate cost, which this does not touch.
//! The reload was still pure waste, and on a host that cannot keep 688 MB
//! resident it is I/O rather than bookkeeping.
//!
//! Caching it adds a third native object to the teardown chain, and it is placed
//! *inside* the existing one rather than beside it: a projector is owned by the
//! [`ModelCache`] entry of the very model it was initialised over, so it is freed
//! when that model is evicted or when the engine is dropped, and always before the
//! backend. Nothing new has to be released, and nothing new can be forgotten.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use llama_cpp_2::ChatTemplateError;
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaChatMessage, LlamaChatTemplate, LlamaModel};
use llama_cpp_2::mtmd::{
    MtmdBitmap, MtmdContext, MtmdContextParams, MtmdInputText, mtmd_default_marker,
};
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_2::{LogOptions, send_logs_to_tracing};

use crate::engine::{
    ChatRequest, CompletionStats, Engine, EngineError, FinishReason, Message, ModelInfo,
};
use crate::slot::KeyedSlot;
use crate::speculative::{Mtp, SpecCounters, SpeculativeStats, draft_head_layers};

/// The smallest window a generative request is ever given — the fixed window
/// every request used to get, now a floor rather than a ceiling (issue #486).
///
/// Keeping the old constant as the floor is what makes the move to per-request
/// sizing monotone: **no request receives a smaller window than it did before**,
/// so nothing that fit yesterday can stop fitting today. It is only a floor for
/// models whose own trained window is larger; a model trained at less than this
/// (`bge-large-en-v1.5` at 512) is clamped down to what it was actually trained
/// for, which is a correction rather than a regression — the tokens above its
/// trained window were never usable.
const MIN_N_CTX: u32 = 4096;

/// Slack added on top of `prompt + max_tokens` when sizing a context.
///
/// The prompt count is exact — the context is built *after* tokenisation, from
/// the real token vector — so this is not a fudge factor for a bad estimate. It
/// covers the two things that legitimately need positions past the arithmetic:
/// MTP speculative decoding verifies [`crate::speculative`]'s draft window ahead
/// of the accepted position, and llama.cpp pads the KV cache to a granularity of
/// its own. Small enough to be irrelevant to the memory figures, large enough
/// that neither can run off the end.
const WINDOW_HEADROOM: u32 = 64;

/// The window a single generation gets: what the request actually needs, bounded
/// by what the operator allows and by what the model was trained for.
///
/// This is the whole of issue #486's answer, and it is a *pure* function so that
/// unit tests needing no model — the ones that therefore run in CI — can pin it.
///
/// **Why size per request rather than raise a fixed default.** llama.cpp
/// allocates the KV cache eagerly in the `llama_kv_cache` constructor
/// (`ggml_backend_alloc_ctx_tensors_from_buft`, whose own comment reads "real
/// buffer"), and [`LlamaEngine::new_context`] builds a context **per
/// generation**. A fixed window is therefore paid in full on every request,
/// whatever that request asked for. Measured on `qwen3.8-27b`
/// (`tests/context_window.rs`), a context's KV and recurrent state cost 429 MiB
/// at 4,096 and **16,466 MiB at its trained 262,144** — so a fixed maximum would
/// spend 16 GiB to answer "hello". (KV and recurrent state are what that
/// instrument measures; it does not see ggml's compute buffers, which is
/// immaterial to this argument but not to every argument — see the note on
/// `tests/context_window.rs`.) Sizing to the request gives the *whole* trained window to a
/// request that needs it, and the floor to one that does not.
///
/// `ceiling` is the operator's cap ([`LlamaEngine::new`]'s `n_ctx`), where `0`
/// means "whatever the model was trained for". `trained` is the model's own
/// `n_ctx_train`, read from its GGUF; it always wins, because a window past it
/// is not a larger context but a wrong one.
fn window_for_request(prompt_tokens: u32, max_tokens: u32, ceiling: u32, trained: u32) -> u32 {
    // A model declaring nothing usable still has to get a context; fall back to
    // the floor rather than asking llama.cpp for zero tokens.
    let trained = if trained == 0 { MIN_N_CTX } else { trained };
    let cap = if ceiling == 0 {
        trained
    } else {
        ceiling.min(trained)
    };
    let needed = prompt_tokens
        .saturating_add(max_tokens)
        .saturating_add(WINDOW_HEADROOM);
    // The floor must not push the window past the cap: on `bge` the cap (512) is
    // below the floor (4,096), and the cap is the answer.
    needed.max(MIN_N_CTX.min(cap)).min(cap)
}

/// How wide a batch `mtmd_helper` is asked to chunk a multimodal prompt into.
/// llama.cpp's own default for the same call, and the value this path has always
/// used; [`LlamaEngine::chat_media`] clamps it to the context's capacity so it
/// cannot become an over-wide batch on a small context.
const MEDIA_CHUNK_TOKENS: u32 = 512;

/// Ensures the native-log → `tracing` bridge is installed exactly once.
static NATIVE_LOG_BRIDGE: std::sync::Once = std::sync::Once::new();

/// Route llama.cpp + ggml's native C logging through `tracing` instead of letting
/// it write straight to stdout/stderr (ADR-0011). This is what tames the wall of
/// `llama_model_loader:` / `create_tensor:` / `print_info:` / `ggml_*` lines a
/// model load emits: `send_logs_to_tracing` installs both the `llama_log_set` and
/// `ggml_log_set` callbacks, so each native line becomes a `tracing` event on the
/// `llama.cpp` / `ggml` target at its mapped level (ggml DEBUG/INFO/WARN/ERROR →
/// tracing DEBUG/INFO/WARN/ERROR). It then obeys whatever subscriber Roteiro
/// installed: quiet on stdout by default (that layer filters at `warn`), captured
/// in the rotating file when file logging is on (that layer filters at `info`),
/// and fully surfaced with `ROTEIRO_LOG=debug`.
///
/// Called at engine construction, after the subscriber is already installed (in
/// `roteiro`'s `main`), so no native line escapes ahead of the bridge. Idempotent
/// via [`std::sync::Once`]; the underlying setter is a documented no-op after the
/// first call, but the `Once` avoids re-running the FFI setters per engine. Uses
/// only the safe `llama-cpp-2` wrapper — a hand-rolled callback would need
/// `unsafe`, which is `forbid`den workspace-wide.
fn install_native_log_bridge() {
    NATIVE_LOG_BRIDGE.call_once(|| {
        // `LogOptions::default()` forwards logs to tracing (rather than suppressing
        // them); the level filtering is the subscriber's job, not the bridge's.
        send_logs_to_tracing(LogOptions::default());
    });
}

/// One installed model this engine may serve: its public name and GGUF path,
/// plus the multimodal projector for vision models.
#[derive(Debug, Clone)]
pub struct Served {
    /// Public model id (registry name).
    pub name: String,
    /// Path to the GGUF file on disk.
    pub path: PathBuf,
    /// Path to the multimodal-projector GGUF (`mmproj`), for multimodal models —
    /// enables image inputs on `/v1/chat/completions` (ADR-0006) for a vision
    /// projector, or audio transcription for an audio projector. `None` for
    /// text-only models.
    pub mmproj: Option<PathBuf>,
}

/// A llama.cpp inference engine over a fixed set of installed GGUF models.
///
/// **Field order is load-bearing.** Rust drops fields in declaration order, and
/// llama.cpp's contract is that every model is freed *before* the backend is
/// (`llama_backend_free` is the last call of a process): freeing a model is what
/// releases its ggml buffers — and, on Metal, deregisters them from the device's
/// residency set. So `cache` (the resident models) is declared first and
/// `backend` last. The cached multimodal projectors (issue #301) own ggml buffers
/// of their own, and they sit *inside* `cache` — each in the [`Loaded`] entry of
/// the model it is bound to — so dropping this struct frees projectors, then
/// models, then the backend handle, in that order and by construction. See also
/// [`crate::llama`]'s note on teardown: an engine that
/// is never dropped at all leaves that residency set non-empty and aborts the
/// process in ggml-metal's exit-time teardown (Roteiro issue #291), which is why
/// callers must not park an engine in a `static`.
///
/// **The backend is shared, not owned** (issue #296). llama.cpp's backend is a
/// process-global that may be initialised only once, so this is an [`Arc`] handle
/// on the one [`crate::backend`] holds rather than a private backend of its own —
/// which is what lets a second engine (a second modality, or a second model in
/// `roteiro serve`) exist at all. Holding that handle is also what carries the
/// ordering guarantee out of this struct without losing it: the backend cannot be
/// freed while any engine is alive, because
/// [`crate::backend::release_shared_backend`] declines while a handle is
/// outstanding.
pub struct LlamaEngine {
    cache: Mutex<ModelCache>,
    served: Vec<Served>,
    /// The **ceiling** a per-request context may grow to, not the size every
    /// context is built at (issue #486). `0` — the default — means "the window
    /// each model was trained for", so the engine offers as much as the model
    /// supports and spends only what a request actually uses. An operator lowers
    /// it to bound the KV cache on a smaller machine; it can only ever reduce
    /// the window, never raise one past `n_ctx_train`.
    n_ctx_ceiling: u32,
    /// Trained windows already warned about, so the "ceiling above
    /// `n_ctx_train`" message is logged once per distinct trained window (per
    /// engine instance) rather than once per request. Not native state —
    /// bookkeeping, so its position among the fields carries no teardown meaning.
    warned_windows: Mutex<std::collections::BTreeSet<u32>>,
    /// How many multimodal projectors this engine has loaded since it was built;
    /// see [`LlamaEngine::projector_inits`]. Not native state — a counter, so its
    /// position among the fields carries no teardown meaning.
    projector_inits: AtomicUsize,
    /// Whether this engine may pair a model with its MTP draft head (issue
    /// #320). Resolved once from `ROTEIRO_SPECULATIVE` at construction — reading
    /// it per request would let a long-lived `roteiro serve` change decode
    /// strategy underneath itself — and overridable with
    /// [`LlamaEngine::with_speculative`].
    speculative_enabled: bool,
    /// What MTP speculative decoding has done on this engine (issue #320): also
    /// plain counters, and also carrying no teardown meaning. The draft head's
    /// own native state is *not* here — it is created and dropped inside a single
    /// generation, which is the whole of how it fits the ordering above.
    speculative: SpecCounters,
    backend: Arc<LlamaBackend>,
}

/// One loaded model held in the residency cache.
///
/// **Field order is load-bearing here too**: `projectors` is declared before
/// `model`, so evicting an entry frees its multimodal projectors before the model
/// they were initialised over. (Each [`Projector`] also carries its own handle on
/// that model, so the ordering holds for a projector handed out to an in-flight
/// request as well — this declaration order is the same statement made where a
/// reader of the cache will look for it.)
struct Loaded {
    name: String,
    /// On-disk GGUF size, used as a proxy for the model's memory footprint when
    /// deciding what to evict (the real RSS is not cheaply available). Includes
    /// `draft`'s file when there is one, so a split draft head is **charged to
    /// the residency budget** rather than being memory the cache cannot see —
    /// which is how issue #320's cost is surfaced instead of hidden.
    bytes: u64,
    /// The **split** MTP draft head for this model (issue #320), when one is
    /// installed beside its GGUF: `ggml-org`'s `mtp-*.gguf`, a whole model file
    /// carrying only the head's tensors. `None` both for a model with no head at
    /// all and for one that *bundles* its head in the main GGUF, where the head
    /// is already resident and no second load exists.
    ///
    /// Resident rather than per-request, because every completion needs it and
    /// the alternative is re-reading 1.7 GB per request — the same reasoning that
    /// put [`Projector`] here.
    ///
    /// **Field order carries no meaning for this one**, unusually for this
    /// struct, and that is worth saying rather than leaving to be inferred: a
    /// draft model is an independent `LlamaModel`, not something built *over*
    /// `model` the way a projector is, so nothing requires it to be freed before
    /// or after its target. What it does share is the rule that binds every model
    /// here — gone before the backend — and living in this entry is what gives it
    /// that by construction, with nothing new for a caller to release.
    draft: Option<Arc<LlamaModel>>,
    /// The multimodal projectors built over *this* loaded model, one per `mmproj`
    /// path (issue #301). Living in the model's own cache entry is what keys the
    /// cache by the model as well as by the projector: a different model — or the
    /// same model reloaded after eviction — gets a different entry and so builds
    /// its own projector, which is required, because `mtmd_init_from_file` records
    /// the `llama_model *` it was given and every later `tokenize`/`eval_chunks`
    /// call dereferences it.
    ///
    /// Shared as an [`Arc`] so a caller can hold the slot (and so keep a projector
    /// resident) after the cache lock is released, exactly as it does the model.
    projectors: Arc<KeyedSlot<PathBuf, Projector>>,
    /// The loaded model, shared so a request can keep decoding on it after the
    /// cache lock is dropped (and even after eviction) — the [`Arc`] outlives the
    /// cache entry for the duration of any in-flight generation.
    model: Arc<LlamaModel>,
    /// Serialises generation on *this* model instance: requests to the same model
    /// take this lock one at a time, while requests to different models hold
    /// different locks and decode concurrently. Cloned out under the cache lock
    /// and held across the whole decode.
    gen_lock: Arc<Mutex<()>>,
}

/// A multimodal projector, bound to the model it was initialised over.
///
/// `mtmd_init_from_file(mmproj, text_model, …)` does not copy the model: the
/// returned `mtmd_context` keeps the `llama_model *` and dereferences it on every
/// `mtmd_tokenize` and `mtmd_eval_chunks`. A projector is therefore **not**
/// reusable across models, and reusing one whose model has been freed would be a
/// use-after-free — the sharp edge of caching something llama.cpp used to rebuild
/// per call.
///
/// So the binding is carried by the value rather than by a rule: this struct owns
/// a handle on that model, and `mtmd` is declared **before** `model` so Rust's
/// field order frees the projector first and releases the model handle second. A
/// [`Projector`] is thus safe to use for as long as it exists, wherever it exists,
/// which is what lets one be handed to a caller and outlive the cache entry it
/// came from.
///
/// **Sharing one costs no new serialisation.** An `mtmd_context` is no more
/// thread-safe than the rest of llama.cpp, and a cached one is reachable by every
/// request for its model where a per-call one was not — but those requests already
/// take that model's `gen_lock` for the whole of `chat_media`, and a projector is
/// only ever reachable through the model it belongs to. Same projector implies
/// same model implies same lock, so two threads cannot be inside one projector at
/// once. Requests to a *different* model still run concurrently, exactly as
/// before.
struct Projector {
    /// The loaded projector. Dropped before `model`.
    mtmd: MtmdContext,
    /// The model `mtmd` holds a raw pointer to, kept alive for its lifetime.
    model: Arc<LlamaModel>,
}

impl Projector {
    /// The loaded projector.
    fn mtmd(&self) -> &MtmdContext {
        &self.mtmd
    }

    /// The model this projector is bound to — the only model it may be used with,
    /// so the media path reads it from here rather than being passed one
    /// separately that would have to be *checked* to be the same.
    fn model(&self) -> &LlamaModel {
        &self.model
    }
}

/// What [`LlamaEngine::resolve`] hands back: shared handles onto one resident
/// model, held by the caller for the duration of a request so that neither
/// eviction nor a concurrent release can pull them away mid-generation.
struct Resolved {
    model: Arc<LlamaModel>,
    /// The split MTP draft head, if this model has one; see [`Loaded::draft`].
    draft: Option<Arc<LlamaModel>>,
    gen_lock: Arc<Mutex<()>>,
    projectors: Arc<KeyedSlot<PathBuf, Projector>>,
}

/// A memory-bounded LRU of loaded models. Entries are ordered least- to
/// most-recently-used; the sum of resident `bytes` is kept at or below
/// `budget_bytes`, except that the most-recently-used model is always retained
/// even if it alone exceeds the budget (a request for a model must be servable).
struct ModelCache {
    budget_bytes: u64,
    loaded: Vec<Loaded>,
}

/// Given resident model sizes ordered LRU→MRU and a byte budget, the number of
/// oldest entries to evict so the remainder fits the budget — always keeping at
/// least the most-recently-used entry (the one a request just needs). A budget of
/// `0` therefore keeps exactly one model resident.
fn lru_evict_count(sizes_lru_to_mru: &[u64], budget_bytes: u64) -> usize {
    let mut total: u64 = sizes_lru_to_mru.iter().sum();
    let mut evict = 0;
    while sizes_lru_to_mru.len() - evict > 1 && total > budget_bytes {
        total -= sizes_lru_to_mru[evict];
        evict += 1;
    }
    evict
}

impl LlamaEngine {
    /// Build an engine serving `served`, with `n_ctx` as the **ceiling** a
    /// request's context may grow to — `0` (the recommended value) meaning
    /// "whatever each model was trained for".
    ///
    /// This is not the window every context is built at (issue #486): contexts
    /// are per-generation and disposable, so each is sized to the request that
    /// needs it — see [`window_for_request`]. Passing a non-zero `n_ctx` bounds
    /// that sizing for every model this engine serves; it cannot raise a window
    /// past a model's own `n_ctx_train`, which is clamped with a warning.
    ///
    /// Keeps a single model resident; use [`LlamaEngine::new_with_budget`] to
    /// hold several. Attaches to the process's shared llama.cpp backend,
    /// starting it if this is the first engine.
    ///
    /// # Errors
    /// Returns an error if the llama.cpp backend fails to start.
    pub fn new(served: Vec<Served>, n_ctx: u32) -> anyhow::Result<Self> {
        Self::new_with_budget(served, n_ctx, 0)
    }

    /// Build an engine that keeps as many models resident as fit within
    /// `budget_bytes` of (GGUF-size-proxied) memory, unloading the
    /// least-recently-used past that cap. `0` keeps a single model (the default).
    ///
    /// # Errors
    /// Returns an error if the llama.cpp backend fails to start.
    pub fn new_with_budget(
        served: Vec<Served>,
        n_ctx: u32,
        budget_bytes: u64,
    ) -> anyhow::Result<Self> {
        // Redirect llama.cpp + ggml's native logs through `tracing` *before* the
        // backend starts — its device probe (e.g. ggml-metal's
        // `ggml_metal_device_init` block) logs during init, so installing the
        // callback afterwards would let that first batch escape to stderr. The log
        // setters are global C functions that need no initialised backend, so
        // setting them first is safe and captures everything the model loads emit.
        install_native_log_bridge();
        // Not `LlamaBackend::init()`: llama.cpp's backend is a process-global, so
        // a second engine initialising its own would be refused with
        // `BackendAlreadyInitialized` and — since callers swallow that with
        // `.ok()` — go silently inert (issue #296). Attach to the one the process
        // already has, starting it only if there is none.
        let backend = crate::backend::shared_backend()?;
        Ok(Self {
            backend,
            served,
            // Carried through as given: `0` stays `0`, meaning "each model's own
            // trained window", and is resolved per model at request time rather
            // than collapsed to one number here — which is precisely what the
            // old `DEFAULT_N_CTX` substitution did, and what made one 4,096
            // serve a model trained at 262,144 and another trained at 512.
            n_ctx_ceiling: n_ctx,
            warned_windows: Mutex::new(std::collections::BTreeSet::new()),
            projector_inits: AtomicUsize::new(0),
            speculative_enabled: crate::speculative::speculative_enabled(),
            speculative: SpecCounters::default(),
            cache: Mutex::new(ModelCache {
                budget_bytes,
                loaded: Vec::new(),
            }),
        })
    }

    /// Turn MTP speculative decoding (issue #320) on or off for this engine,
    /// overriding the `ROTEIRO_SPECULATIVE` default.
    ///
    /// There is no quality trade-off to make here — the output is identical
    /// either way — so this exists for the two cases where the *path* matters
    /// rather than the result: measuring one against the other, and pinning a
    /// process to the plain loop while a suspected llama.cpp bug is investigated.
    /// Turning it **on** cannot force speculation onto a model with no draft
    /// head; that still falls back.
    #[must_use]
    pub fn with_speculative(mut self, enabled: bool) -> Self {
        self.speculative_enabled = enabled;
        self
    }

    /// How many multimodal projectors this engine has loaded since it was built.
    ///
    /// The number issue #301 is about, and the only externally visible difference
    /// between a cached projector and a rebuilt one: a media request served from
    /// the cache does not increment it, so a sync over N blobs of one modality
    /// leaves this at `1` where it used to leave it at `N`. Two modalities in one
    /// engine leave it at `2`. Exposed so a test can assert on the count rather
    /// than on wall-clock, which would be flaky.
    #[must_use]
    pub fn projector_inits(&self) -> usize {
        self.projector_inits.load(Ordering::Relaxed)
    }

    /// What MTP speculative decoding has done on this engine since it was built
    /// (issue #320): how many generations used a draft head, how many tokens it
    /// proposed, and how many of those the target model agreed with.
    ///
    /// Exposed because the speedup is not a single number — acceptance is high
    /// on code and `<tool_call>` emission and low on prose — and because
    /// `activations == 0` is how a caller confirms a model fell back to plain
    /// decoding rather than silently doing something else.
    #[must_use]
    pub fn speculative_stats(&self) -> SpeculativeStats {
        self.speculative.snapshot()
    }

    /// Ensure the model named `name` (GGUF at `path`) is resident in `cache`,
    /// loading it and evicting least-recently-used models past the budget if
    /// needed, then promoting it to most-recently-used. Returns its index (always
    /// the last, most-recently-used slot).
    fn ensure_loaded(
        &self,
        cache: &mut ModelCache,
        name: &str,
        path: &Path,
    ) -> Result<usize, EngineError> {
        if let Some(i) = cache.loaded.iter().position(|l| l.name == name) {
            // Already resident — promote to most-recently-used.
            let entry = cache.loaded.remove(i);
            cache.loaded.push(entry);
        } else {
            // The GGUF size is the residency budget's footprint proxy, so a
            // failure to read it must surface — a silent `0` would let a model
            // count as free and break the eviction invariant. (The file is about
            // to be loaded from this same path, so this rarely fails.)
            let bytes = std::fs::metadata(path)
                .map_err(|e| EngineError::Inference(format!("stat model `{name}`: {e}")))?
                .len();
            let params = LlamaModelParams::default();
            let model = LlamaModel::load_from_file(&self.backend, path, &params)
                .map_err(|e| EngineError::Inference(format!("load `{name}`: {e}")))?;
            // A split MTP draft head installed beside the GGUF (issue #320). A
            // head that will not load is not a failed model load: the request
            // still has a perfectly good target model and decodes without one.
            let draft = self.load_draft_head(path);
            let draft_bytes = draft.as_ref().map_or(0, |(_, draft_bytes)| *draft_bytes);
            cache.loaded.push(Loaded {
                name: name.to_owned(),
                // The head is charged to the residency budget with the target it
                // belongs to: it is loaded and freed with that entry, so the two
                // are one footprint as far as eviction is concerned.
                bytes: bytes.saturating_add(draft_bytes),
                draft: draft.map(|(model, _)| Arc::new(model)),
                // A freshly loaded model has no projectors yet: the first media
                // request builds one, and it is bound to *this* model instance.
                projectors: Arc::new(KeyedSlot::new()),
                model: Arc::new(model),
                gen_lock: Arc::new(Mutex::new(())),
            });
            // Evict oldest models now that the newcomer (MRU) is resident.
            let sizes: Vec<u64> = cache.loaded.iter().map(|l| l.bytes).collect();
            let evict = lru_evict_count(&sizes, cache.budget_bytes);
            cache.loaded.drain(0..evict);
        }
        Ok(cache.loaded.len() - 1)
    }

    /// Ensure `name` (GGUF at `path`) is resident and hand back cloned shared
    /// handles — the model, its per-instance generation lock, and its projector
    /// slot — releasing the cache lock before returning. Holding the returned
    /// `Arc`s lets the caller generate without pinning the cache: other models
    /// stay servable, and this model (with its projectors) survives eviction until
    /// the last handle drops.
    fn resolve(&self, name: &str, path: &Path) -> Result<Resolved, EngineError> {
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| EngineError::Inference("engine mutex poisoned".to_owned()))?;
        let idx = self.ensure_loaded(&mut cache, name, path)?;
        let l = &cache.loaded[idx];
        Ok(Resolved {
            model: Arc::clone(&l.model),
            draft: l.draft.clone(),
            gen_lock: Arc::clone(&l.gen_lock),
            projectors: Arc::clone(&l.projectors),
        })
    }

    /// Load the split MTP draft head installed beside `path`, with its file size,
    /// or `None` when there is none to load.
    ///
    /// Every way this can go wrong returns `None`: no `mtp.gguf` beside the
    /// model, a file that will not load, or one that loads but turns out not to
    /// carry a draft head after all. None of those is a reason to fail the
    /// model load — the target model is fine and decoding without a head is
    /// exactly what Roteiro did before issue #320.
    ///
    /// Runs under the cache lock, like the target model's own load.
    fn load_draft_head(&self, path: &Path) -> Option<(LlamaModel, u64)> {
        if !self.speculative_enabled {
            return None;
        }
        let draft_path = crate::speculative::draft_gguf_beside(path)?;
        let bytes = std::fs::metadata(&draft_path).ok()?.len();
        let model =
            LlamaModel::load_from_file(&self.backend, &draft_path, &LlamaModelParams::default())
                .ok()?;
        // Confirm rather than assume: the file is named by convention, so it has
        // to say for itself that it is a draft head before it is treated as one.
        // A `0` here means `mtp.gguf` is some other model, and keeping it
        // resident would be pure waste.
        if draft_head_layers(&model) == 0 {
            return None;
        }
        Some((model, bytes))
    }

    /// The projector `mmproj` describes, over the model `resolved` names —
    /// **loaded once and reused** for every later media blob (issue #301).
    ///
    /// The cache key is the pair `(loaded model, mmproj path)`, and each half is
    /// there for its own reason:
    ///
    /// * the **mmproj path** because two projectors legitimately coexist in one
    ///   process (issue #298) and they are not interchangeable — an audio request
    ///   handed the vision projector fails `support_audio`. It is the path rather
    ///   than a digest of the file because that is what identifies a projector to
    ///   every other part of Roteiro (`Served::mmproj`, the model store's
    ///   `mmproj.gguf`); a hash would cost a re-read of the 715 MB file to detect
    ///   a change that would mean the model store was edited under a running
    ///   process, which is not a case this cache is trying to survive.
    /// * the **model** because an `mtmd_context` holds the `llama_model *` it was
    ///   built with (see [`Projector`]), so a projector is only ever sound for
    ///   that one model *instance*. That half of the key is structural rather than
    ///   compared: the slot lives in the model's own [`Loaded`] entry, so a
    ///   different model — or the same model reloaded after eviction — cannot
    ///   reach this one's projectors.
    ///
    /// The [`MtmdContextParams`] are not part of the key because they are constant
    /// (`default()`); if a request ever chose them, they would have to be.
    ///
    /// Runs under the caller's per-model generation lock, like every other
    /// `LlamaModel` FFI call on this path.
    fn projector(&self, resolved: &Resolved, mmproj: &Path) -> Result<Arc<Projector>, EngineError> {
        let mmproj_path = mmproj
            .to_str()
            .ok_or_else(|| EngineError::Inference("non-UTF-8 mmproj path".to_owned()))?;
        resolved
            .projectors
            .get_or_try_init(mmproj.to_path_buf(), || {
                let mtmd = MtmdContext::init_from_file(
                    mmproj_path,
                    &resolved.model,
                    &MtmdContextParams::default(),
                )
                .map_err(|e| EngineError::Inference(format!("init projector: {e}")))?;
                self.projector_inits.fetch_add(1, Ordering::Relaxed);
                Ok(Projector {
                    mtmd,
                    // The handle that makes the projector safe to use wherever it
                    // travels, rather than only while this cache entry lives.
                    model: Arc::clone(&resolved.model),
                })
            })
    }

    /// Resolve a served model name to its GGUF path.
    fn path_for(&self, name: &str) -> Option<PathBuf> {
        self.served
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.path.clone())
    }

    /// A fresh context sized to `n_ctx`, borrowing `model`.
    ///
    /// The parameters come from [`crate::speculative::base_params`] — the one
    /// place a generative context's shape is written down — so a plain context
    /// and the two a speculative generation builds cannot disagree about the
    /// window or the batch width they accept.
    ///
    /// The window is passed in rather than read from the engine because it is a
    /// property of the *request* now, not of the engine (issue #486); callers
    /// get it from [`LlamaEngine::request_window`].
    fn new_context<'m>(
        &self,
        model: &'m LlamaModel,
        n_ctx: u32,
    ) -> Result<LlamaContext<'m>, EngineError> {
        model
            .new_context(&self.backend, crate::speculative::base_params(n_ctx))
            .map_err(|e| EngineError::Inference(format!("context: {e}")))
    }

    /// The window this engine will build for a request of `prompt_tokens` that
    /// may generate `max_tokens` more, against `model`'s own trained window.
    ///
    /// Warns — once per process per model, so a busy server does not repeat
    /// itself per request — when the operator's ceiling is above what the model
    /// was trained for. That case is **clamped, not refused**: the ceiling is a
    /// budget an operator sets across every served model at once, and the models
    /// differ by 512× (`qwen3.8-27b` 262,144, `bge-large-en-v1.5` 512), so one
    /// number over one model's trained window is ordinary rather than a mistake
    /// worth failing a request over. What is *not* clamped is the request: a
    /// prompt longer than the resulting window is refused by
    /// [`check_batch_capacity`] as an [`EngineError::InvalidRequest`], because
    /// silently truncating a user's prompt would answer a question they did not
    /// ask.
    ///
    /// # The ceiling is load-bearing, not merely a memory guard
    ///
    /// **Read this before raising it.** When per-request sizing landed, the only
    /// thing that could move `prompt_tokens` was Roteiro's own prompt — a served
    /// tool surface this repository writes and can measure. That is no longer
    /// the whole story: once a client may supply its own `tools` array, the
    /// prompt is **caller-influenced**, and `prompt_tokens` is therefore an input
    /// an outside party has a hand in. Since the window follows the prompt, so
    /// does the allocation: at 64 KiB/token on `qwen3.8-27b`, a prompt driven up
    /// to that model's trained 262,144 is a 16 GiB allocation.
    ///
    /// The bound on *that* belongs at the edge — the serving layer refuses an
    /// oversized tool surface with a 400 before it ever reaches here — and this
    /// function deliberately does **not** add a second clamp of its own, because
    /// two independent bounds on one quantity drift apart and then neither can be
    /// trusted. What this ceiling does is put a floor under that arrangement: it
    /// is the backstop that decides the worst case a single request can reach
    /// whatever the edge does. Raising it raises that worst case, so it is a
    /// decision about exposure and not only about memory.
    fn request_window(&self, model: &LlamaModel, prompt_tokens: u32, max_tokens: u32) -> u32 {
        let trained = model.n_ctx_train();
        if self.n_ctx_ceiling > trained && trained > 0 {
            self.warn_ceiling_clamped(trained);
        }
        window_for_request(prompt_tokens, max_tokens, self.n_ctx_ceiling, trained)
    }

    /// Emit the "ceiling above `n_ctx_train`" warning at most once per distinct
    /// trained window, so `roteiro serve` does not log it on every request.
    fn warn_ceiling_clamped(&self, trained: u32) {
        let mut warned = match self.warned_windows.lock() {
            Ok(warned) => warned,
            // A poisoned lock here means another thread panicked mid-warning.
            // That is not a reason to fail a generation, and the worst case of
            // carrying on is a warning logged twice.
            Err(poisoned) => poisoned.into_inner(),
        };
        if warned.insert(trained) {
            tracing::warn!(
                configured = self.n_ctx_ceiling,
                n_ctx_train = trained,
                "configured context-window ceiling is above what this model was \
                 trained for; clamping to the trained window"
            );
        }
    }

    /// A context paired with `model`'s own MTP draft head (issue #320), or
    /// `None` when this generation should decode the plain way.
    ///
    /// **`None` is never a failure**, which is the point: absence of a draft head
    /// is the common case (every model Roteiro served before Qwen3.5), and the
    /// three ways speculation can be unavailable — switched off, no head in the
    /// GGUF, llama.cpp declining to build the context — all land here and all
    /// mean "decode as before". Nothing about the request changes; the output is
    /// identical either way (see [`crate::speculative`]), so there is nothing for
    /// a caller to be told and nothing to configure.
    ///
    /// The metadata probe runs before the context is built so that a model
    /// without a head does not make llama.cpp log its "context type MTP
    /// requested but model doesn't contain MTP layers" warning once per request.
    fn speculative_decoder<'m>(
        &self,
        model: &'m LlamaModel,
        draft: Option<&'m LlamaModel>,
        n_ctx: u32,
    ) -> Option<Mtp<'m>> {
        // The head is in whichever file has one: the target GGUF for a bundled
        // model, the sibling `mtp.gguf` for a split one. A model with neither
        // takes the plain path.
        let head = draft.unwrap_or(model);
        if !self.speculative_enabled || draft_head_layers(head) == 0 {
            return None;
        }
        Mtp::try_new(&self.backend, model, draft, n_ctx)
    }

    /// Text-only chat: apply the chat template, prime the prompt, and generate.
    ///
    /// Takes the whole [`Resolved`] rather than just its model, because this is
    /// the path that may pair the model with a draft head (issue #320) and the
    /// head is resolved from the cache alongside it.
    fn chat_text(
        &self,
        resolved: &Resolved,
        req: &ChatRequest,
        on_token: &mut dyn FnMut(&str),
    ) -> Result<CompletionStats, EngineError> {
        let model = &resolved.model;
        let prompt = render_prompt(model, &req.model, &req.messages, req.tools.as_ref())?;

        let tokens = model
            .str_to_token(&prompt, AddBos::Always)
            .map_err(|e| EngineError::Inference(format!("tokenize: {e}")))?;
        let prompt_tokens = u32::try_from(tokens.len()).unwrap_or(u32::MAX);

        // The prompt is already tokenised, so the context can be sized to this
        // exact request rather than to a fixed maximum (issue #486) — that
        // ordering is what makes per-request sizing possible at all, and it was
        // already the ordering here. `max_tokens` is included because those
        // positions are as real as the prompt's.
        let n_ctx = self.request_window(model, prompt_tokens, req.max_tokens);

        // The context comes first, and the prompt is measured against it before a
        // single token is batched (issue #346). This used to run the other way
        // round — batch the whole prompt, then "let the decode call enforce it" —
        // but the decode call does not *enforce* anything a caller can catch: an
        // over-long batch trips a `GGML_ASSERT` inside llama.cpp and aborts the
        // process. The bound is a property of the context, so the context (plain
        // or speculative) is built first and asked what it will accept.
        let mut decoder = match self.speculative_decoder(model, resolved.draft.as_deref(), n_ctx) {
            Some(mtp) => Decoder::Speculative(Box::new(mtp)),
            None => Decoder::Plain(self.new_context(model, n_ctx)?),
        };
        // Encoder-only models never reach here — `chat_stream` rejects them
        // earlier — so this is the causal bound, `n_batch`.
        check_batch_capacity(
            tokens.len(),
            batch_capacity(decoder.context(), false),
            "prompt",
        )?;

        // Prime the batch with the prompt; only the last token needs logits.
        let mut batch = LlamaBatch::new(tokens.len().max(1), 1);
        let last = tokens.len().saturating_sub(1);
        for (i, token) in tokens.iter().enumerate() {
            let pos = i32::try_from(i).unwrap_or(i32::MAX);
            batch
                .add(*token, pos, &[0], i == last)
                .map_err(|e| EngineError::Inference(format!("prompt batch: {e}")))?;
        }

        let start = i32::try_from(tokens.len()).unwrap_or(i32::MAX);
        let mut sampler = new_sampler(req.temperature);

        // Text generation is the path MTP speculative decoding applies to
        // (issue #320), and the one Roteiro's served workload lives on: graph
        // `<tool_call>` emission and `spec draft`. A model with no draft head —
        // or a `ROTEIRO_SPECULATIVE=0` — simply gets the loop it always had, with
        // the same sampler and the same seed.
        let (completion_tokens, finish_reason) = match &mut decoder {
            Decoder::Speculative(mtp) => {
                mtp.prime(&mut batch, &tokens)?;
                self.speculative.activate();
                crate::speculative::run_generation(
                    model,
                    mtp,
                    start,
                    &mut sampler,
                    req.max_tokens,
                    &self.speculative,
                    on_token,
                )?
            }
            Decoder::Plain(ctx) => {
                ctx.decode(&mut batch)
                    .map_err(|e| EngineError::Inference(format!("prompt decode: {e}")))?;
                run_generation(model, ctx, start, &mut sampler, req.max_tokens, on_token)?
            }
        };
        Ok(CompletionStats {
            prompt_tokens,
            completion_tokens,
            finish_reason,
        })
    }

    /// Multimodal chat (ADR-0006): project the request's `modality` media
    /// (images or audio) through `projector` and generate. The media are placed at
    /// media markers inside the last user turn; the use case is just a prompt
    /// ("transcribe the text in this image" / "transcribe this audio"). Both
    /// modalities share this path — the projector decodes the raw file bytes
    /// (images via `stb_image`, audio via miniaudio) and only the support check
    /// and which byte vectors are read differ.
    ///
    /// The projector arrives already loaded, from [`LlamaEngine::projector`]'s
    /// per-model cache (issue #301) — this used to re-read the `mmproj` GGUF on
    /// every call. The model is read from the projector rather than passed
    /// alongside it, because the only model this projector may be used with is the
    /// one it is bound to.
    fn chat_media(
        &self,
        projector: &Projector,
        req: &ChatRequest,
        modality: Modality,
        on_token: &mut dyn FnMut(&str),
    ) -> Result<CompletionStats, EngineError> {
        let model = projector.model();
        let mtmd = projector.mtmd();
        if !modality.supported(mtmd) {
            return Err(EngineError::Inference(format!(
                "this projector does not support {}",
                modality.noun(),
            )));
        }

        // `from_buffer` decodes both images and audio from their raw file bytes
        // (audio is auto-detected by magic bytes and resampled by miniaudio).
        let bitmaps = modality
            .media(req)
            .iter()
            .map(|blob| MtmdBitmap::from_buffer(mtmd, blob, false))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| EngineError::Inference(format!("decode {}: {e}", modality.noun())))?;
        let bitmap_refs: Vec<&MtmdBitmap> = bitmaps.iter().collect();

        let prompt = media_prompt(model, req, bitmaps.len())?;
        let chunks = mtmd
            .tokenize(
                MtmdInputText {
                    text: prompt,
                    add_special: true,
                    parse_special: true,
                },
                &bitmap_refs,
            )
            .map_err(|e| EngineError::Inference(format!("mtmd tokenize: {e}")))?;
        let prompt_tokens = u32::try_from(chunks.total_tokens()).unwrap_or(u32::MAX);

        // Same sizing as the text path (issue #486), and available for the same
        // reason: `mtmd` has already counted the prompt — text plus projected
        // media embeddings — before any context exists. A projected image is
        // worth hundreds of positions, so this path benefits from a window that
        // follows the request rather than a fixed one it might not fit in.
        let n_ctx = self.request_window(model, prompt_tokens, req.max_tokens);
        let mut ctx = self.new_context(model, n_ctx)?;
        // `eval_chunks` decodes text + projected media (image/audio) embeddings
        // into the context and returns the new position to continue generating.
        //
        // Unlike the text path this one needs no length guard: `mtmd_helper` does
        // its own chunking, splitting both text and projected embeddings into
        // batches of the width passed here — so an arbitrarily long media prompt
        // arrives as a series of decodes that each fit. That width used to be the
        // literal `512`, which is only *safe* while it happens to be under the
        // context's batch capacity; an engine built with a context smaller than
        // 512 would have handed llama.cpp an over-wide batch and aborted, which is
        // the same defect as issue #346 waiting for a different caller. Taking the
        // minimum keeps the chunk width at exactly 512 for every context in the
        // tree today — so nothing about how a media prompt is batched changes —
        // and shrinks it to fit when a caller asks for a smaller window.
        let chunk_width = ctx.n_batch().min(MEDIA_CHUNK_TOKENS);
        let n_past = chunks
            .eval_chunks(
                mtmd,
                &ctx,
                0,
                0,
                i32::try_from(chunk_width).unwrap_or(1),
                true,
            )
            .map_err(|e| EngineError::Inference(format!("mtmd eval: {e}")))?;

        // No speculation here: `mtmd_eval_chunks` decodes the prompt itself, so
        // the draft head never sees those batches — and llama.cpp's MTP drafter
        // ignores embedding batches in any case. Media requests keep the plain
        // loop, and say so rather than appearing to have been left out.
        let mut sampler = new_sampler(req.temperature);
        let (completion_tokens, finish_reason) = run_generation(
            model,
            &mut ctx,
            n_past,
            &mut sampler,
            req.max_tokens,
            on_token,
        )?;
        Ok(CompletionStats {
            prompt_tokens,
            completion_tokens,
            finish_reason,
        })
    }
}

/// How one text generation will be decoded: the plain context, or the
/// target+draft pair a speculative generation runs on (issue #320).
///
/// This exists so that the choice — which is made per request, from the model's
/// own metadata — happens *before* the prompt is batched. Both variants own a
/// context whose batch capacity has to be honoured, and asking each for that
/// capacity through one accessor is what lets [`LlamaEngine::chat_text`] check
/// the prompt once instead of once per branch, and stops a future branch from
/// quietly being added without a check (issue #346).
enum Decoder<'m> {
    /// A single context: the plain decode loop.
    Plain(LlamaContext<'m>),
    /// An MTP target/draft pair. Boxed because [`Mtp`] is much the larger of the
    /// two and an unboxed enum would round every plain generation up to its size.
    Speculative(Box<Mtp<'m>>),
}

impl<'m> Decoder<'m> {
    /// The context the prompt batch is submitted to — the target context in the
    /// speculative case, which is the one that decodes the prompt in
    /// [`Mtp::prime`] and therefore the one whose bound applies.
    fn context(&self) -> &LlamaContext<'m> {
        match self {
            Self::Plain(ctx) => ctx,
            Self::Speculative(mtp) => mtp.target(),
        }
    }
}

/// Which media modality a multimodal request carries. Both flow through
/// [`LlamaEngine::chat_media`]; only the projector support check and which byte
/// vectors are read differ.
#[derive(Debug, Clone, Copy)]
enum Modality {
    /// Images (PNG/JPEG bytes), projected through a vision `mmproj`.
    Vision,
    /// Audio clips (WAV/MP3/FLAC bytes), projected through an audio `mmproj`.
    Audio,
}

impl Modality {
    /// The request's media byte-vectors for this modality.
    fn media(self, req: &ChatRequest) -> &[Vec<u8>] {
        match self {
            Self::Vision => &req.images,
            Self::Audio => &req.audio,
        }
    }

    /// Whether `mtmd`'s loaded projector supports this modality.
    fn supported(self, mtmd: &MtmdContext) -> bool {
        match self {
            Self::Vision => mtmd.support_vision(),
            Self::Audio => mtmd.support_audio(),
        }
    }

    /// Plural noun for error messages (`images` / `audio`).
    fn noun(self) -> &'static str {
        match self {
            Self::Vision => "images",
            Self::Audio => "audio",
        }
    }
}

/// Build the templated prompt for a multimodal request: prepend `n_media` media
/// markers to the last user turn (where `mtmd` will splice the projected image or
/// audio embeddings), then apply the model's chat template.
fn media_prompt(
    model: &LlamaModel,
    req: &ChatRequest,
    n_media: usize,
) -> Result<String, EngineError> {
    let marker = mtmd_default_marker();
    let target = req
        .messages
        .iter()
        .rposition(|m| m.role == "user")
        .unwrap_or(req.messages.len().saturating_sub(1));
    let mut markers = String::new();
    for _ in 0..n_media {
        markers.push_str(marker);
        markers.push('\n');
    }

    // The markers go into the message *before* templating, so the media sits
    // where the model expects it inside its own turn structure rather than being
    // appended to a finished prompt.
    let messages: Vec<Message> = req
        .messages
        .iter()
        .enumerate()
        .map(|(i, m)| Message {
            role: m.role.clone(),
            content: if i == target {
                format!("{markers}{}", m.content)
            } else {
                m.content.clone()
            },
        })
        .collect();

    render_prompt(model, &req.model, &messages, req.tools.as_ref())
}

/// Render the conversation into a prompt, using the model's **own** template.
///
/// Prefers rendering the embedded Jinja in Rust (issue #492). `apply_chat_template`
/// remains the path for a *builtin name* — the fallback a model with no embedded
/// template resolves to — because a name is a key into llama.cpp's table, not
/// something to render.
///
/// Why not `apply_chat_template` for the Jinja too: it runs none. `llama.h` says
/// so ("does not use a jinja parser"), and it substring-matches for `<|im_start|>`
/// then emits eight lines of C++ instead. qwen3-32b's 4,100-byte template comes
/// back as 153 bytes.
///
/// `tools` reach the template's own `tools` slot, which is the shape a model
/// trained on tool use expects — and the thing `apply_chat_template` could not do
/// at any setting, because its signature carries no tools argument. A builtin
/// name has no such slot, so tools are simply absent on that path; the caller's
/// system turn still advertises them.
///
/// A template that fails to render is **not** silently replaced by the C++ path.
/// That would trade a loud failure for a wrong prompt, which is the whole defect
/// being fixed here: the caller gets the error and can see which template broke.
fn render_prompt(
    model: &LlamaModel,
    name: &str,
    messages: &[Message],
    tools: Option<&serde_json::Value>,
) -> Result<String, EngineError> {
    let template = resolve_chat_template(model)?;
    let raw = template
        .to_str()
        .map_err(|e| EngineError::Inference(format!("chat template is not UTF-8: {e}")))?;

    if crate::chat_template::is_jinja(raw) {
        let msgs: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| serde_json::json!({"role": m.role, "content": m.content}))
            .collect();
        // Deliberately does **not** fall back to `apply_chat_template`. That path
        // runs no Jinja and would answer with a prompt built from eight lines of
        // C++ — a silently wrong prompt in place of a loud failure, which is the
        // defect this whole module exists to remove.
        //
        // `render_advertising` rather than `render`, so that a template which
        // never references `tools` still gets them: that is what lets rto-serve
        // stop listing the tools a second time in its own system turn.
        return crate::chat_template::render_advertising(raw, &msgs, tools, true).map_err(|e| {
            // Actionable, because this is the one failure an unknown model can
            // bring: its template uses a Jinja feature this renderer does not
            // have. Naming the model and the gap is what turns "it broke" into a
            // fix — and the registry has already produced two such gaps
            // (`tojson`, `startswith`), each closed by adding the feature rather
            // than by giving up on Jinja.
            EngineError::Inference(format!(
                "model `{name}`: its embedded chat template could not be rendered \
                 ({e}). The template is the model's own, so this is a Jinja feature \
                 Roteiro does not yet support rather than a fault in the model. \
                 Roteiro renders the template itself because llama.cpp's does not \
                 run Jinja at all and would otherwise return a prompt this model \
                 was not trained on."
            ))
        });
    }

    // A builtin template *name* has no `tools` slot — llama.cpp renders it from
    // C++. So if the caller offered tools, they are advertised here instead, and
    // the guarantee this function makes holds either way: **a caller that passes
    // tools gets them into the prompt**, without having to know which path ran.
    //
    // Not a second rendering competing with the template's: exactly one of these
    // branches executes. The fallback is deliberately plain, because a model that
    // embeds no chat template was not trained on a tool-use format either.
    let mut messages = messages.to_vec();
    if let Some(t) = tools {
        messages.insert(
            0,
            Message {
                role: "system".to_owned(),
                content: crate::chat_template::tool_advertisement(t),
            },
        );
    }
    let llama_messages = messages
        .iter()
        .map(|m| LlamaChatMessage::new(m.role.clone(), m.content.clone()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| EngineError::Inference(format!("chat message: {e}")))?;
    model
        .apply_chat_template(&template, &llama_messages, true)
        .map_err(|e| EngineError::Inference(format!("apply chat template: {e}")))
}

/// Resolve a usable chat template for `model`, preferring the one embedded in
/// the GGUF (`tokenizer.chat_template`) and falling back to a sensible default
/// when the model embeds none.
///
/// Some served generative GGUFs (e.g. `moondream2`) carry no embedded chat
/// template. llama.cpp's [`LlamaModel::chat_template`] surfaces that as
/// [`ChatTemplateError::MissingTemplate`] (a null pointer from the C API);
/// applying the chat path directly to that would fail the whole request with
/// `no chat template`. Instead we synthesise a template from a built-in
/// llama.cpp identifier — the model's family template when its architecture is
/// recognised, otherwise `ChatML`, a widely compatible default — so the request
/// still produces a valid formatted prompt. Behaviour is unchanged for models
/// that *do* embed a template: theirs is returned verbatim. Only a genuinely
/// unusable lookup (a non-missing error, or a fallback name that cannot be
/// turned into a C string) surfaces as a typed [`EngineError`].
fn resolve_chat_template(model: &LlamaModel) -> Result<LlamaChatTemplate, EngineError> {
    // The architecture read is deferred into a closure so the happy path (an
    // embedded template) never touches model metadata — it is only consulted on
    // the missing-template fallback.
    resolve_chat_template_from(model.chat_template(None), || {
        model.meta_val_str("general.architecture").ok()
    })
}

/// The pure decision behind [`resolve_chat_template`], split out from the
/// [`LlamaModel`] lookup so it can be exercised without loading a GGUF: given
/// the embedded-template lookup result and a lazy architecture provider, pick
/// the template to use. An embedded template is returned unchanged — `arch` is
/// never invoked in that case; a missing one falls back to a built-in identifier
/// (see [`fallback_template_name`], the only path that reads the architecture);
/// any other lookup failure becomes a typed [`EngineError`].
fn resolve_chat_template_from(
    embedded: Result<LlamaChatTemplate, ChatTemplateError>,
    arch: impl FnOnce() -> Option<String>,
) -> Result<LlamaChatTemplate, EngineError> {
    match embedded {
        Ok(template) => Ok(template),
        Err(ChatTemplateError::MissingTemplate) => {
            let arch = arch();
            let name = fallback_template_name(arch.as_deref());
            LlamaChatTemplate::new(name).map_err(|e| {
                EngineError::Inference(format!("build fallback chat template `{name}`: {e}"))
            })
        }
        // Not an absent template (that is handled by the fallback above) but a
        // genuine failure to read the embedded one — surface it as such.
        Err(e) => Err(EngineError::Inference(format!(
            "chat template lookup failed: {e}"
        ))),
    }
}

/// Built-in llama.cpp chat-template identifier to use for a model that embeds
/// none, chosen from its GGUF architecture (`general.architecture`). Recognised
/// families map to their known format; everything else (including an unknown or
/// absent architecture) uses `ChatML`, a widely compatible default. Each returned
/// name is a llama.cpp built-in template identifier that
/// [`LlamaModel::apply_chat_template`] expands into a real prompt.
fn fallback_template_name(arch: Option<&str>) -> &'static str {
    match arch {
        Some("gemma" | "gemma2" | "gemma3") => "gemma",
        Some("phi3") => "phi3",
        // Everything else falls back to ChatML — the neutral default, and the
        // actual format used by the Qwen family (`qwen2`/`qwen3`), which makes up
        // most of this engine's install set. Also covers unknown or absent
        // architectures (e.g. `moondream2`, which records none).
        _ => "chatml",
    }
}

/// Refuse `n_tokens` when a single llama.cpp batch cannot hold that many, naming
/// both the count and the limit (issue #346).
///
/// **This is the guard that has to exist whatever else is done about batch
/// width.** llama.cpp checks the batch against the context's capacity with a
/// `GGML_ASSERT`, which calls `ggml_abort` — it terminates the *process*, so no
/// Rust error handling anywhere upstream can turn it back into a failed request.
/// A `roteiro serve` that reaches it takes the graph API and the web UI down with
/// it, and an ordinary over-long chat message is enough to do it. So the count is
/// checked here, before any token reaches llama.cpp, and refused as an
/// [`EngineError::InvalidRequest`] — which `rto-serve` already maps to a 400.
/// Raising the capacity moves the bound; it cannot remove it, because some input
/// will always exceed whatever the bound is.
///
/// `limit` is read back from the built [`LlamaContext`] rather than recomputed
/// here, because llama.cpp derives it from the requested parameters by rules of
/// its own (it clamps `n_batch` to `n_ctx` for a causal model, and does not for a
/// non-causal one). Asking the context what it will accept cannot drift from what
/// it then asserts; a constant restated on this side could.
///
/// # Errors
/// [`EngineError::InvalidRequest`] when `n_tokens` exceeds `limit`.
fn check_batch_capacity(n_tokens: usize, limit: u32, what: &str) -> Result<(), EngineError> {
    let n_tokens = u64::try_from(n_tokens).unwrap_or(u64::MAX);
    if n_tokens <= u64::from(limit) {
        return Ok(());
    }
    Err(EngineError::InvalidRequest(format!(
        "{what} is {n_tokens} tokens, over the {limit}-token limit this model \
         accepts in one request — send less text (shorten or split it)"
    )))
}

/// The largest single batch `ctx` will accept, in tokens.
///
/// Two different llama.cpp asserts are in play and they do not agree, which is
/// why `encoder_only` has to be passed in — the distinction is a property of the
/// *model*, and llama.cpp does not expose the resulting `causal_attn` through the
/// Rust binding:
///
/// * a **causal** (generative) model decodes through `llama_context::decode`,
///   which splits the logical batch into `n_ubatch`-sized physical pieces itself.
///   Its bound is the *logical* batch, `n_batch`
///   (`GGML_ASSERT(n_tokens_all <= cparams.n_batch)`).
/// * an **encoder-only** model is routed to `llama_context::encode`, where
///   llama.cpp's own comment reads "micro-batching is not possible for non-causal
///   encoding, so we process the batch in a single shot". Its bound is therefore
///   the *physical* batch, `n_ubatch`
///   (`GGML_ASSERT(cparams.n_ubatch >= n_tokens)`) — and that is 512 by default,
///   four times tighter than the generative one. That difference is why one
///   number would have been wrong for both paths.
fn batch_capacity(ctx: &LlamaContext, encoder_only: bool) -> u32 {
    if encoder_only {
        ctx.n_ubatch().min(ctx.n_batch())
    } else {
        ctx.n_batch()
    }
}

/// Whether a GGUF `general.architecture` names an **encoder-only** model — the
/// BERT embedding family (`bert`, `nomic-bert`, `nomic-bert-moe`, `jina-bert-v2`,
/// `distilbert`, `roberta`, …). Such models have no decoder: routing them through
/// the chat/generation path makes llama.cpp take its encoder route, which aborts
/// the process with `GGML_ASSERT(n_ubatch >= n_tokens)`. The chat path rejects
/// them up front instead (see [`Engine::chat_stream`]). Match is
/// case-insensitive and substring-based on `bert`, which covers every BERT
/// derivative naming while never matching a generative family (`qwen2`/`qwen3`,
/// `llama`, `gemma`, `phi3`, …).
fn is_encoder_only_arch(arch: &str) -> bool {
    arch.to_ascii_lowercase().contains("bert")
}

/// The sampler for a request: greedy when temperature is 0 (deterministic);
/// otherwise temp + dist on a fixed seed.
///
/// Built by the caller rather than inside the decode loop so that the plain and
/// the speculative loops provably share one — which is what makes "same seed,
/// same output with and without speculation" a claim about *one* sampler rather
/// than about two that happen to be configured alike.
fn new_sampler(temperature: f32) -> LlamaSampler {
    if temperature <= 0.0 {
        LlamaSampler::greedy()
    } else {
        LlamaSampler::chain_simple([LlamaSampler::temp(temperature), LlamaSampler::dist(1234)])
    }
}

/// The shared sampling loop: from `start_pos`, sample → emit → decode until an
/// end-of-generation token or `max_tokens`. Returns `(completion_tokens, reason)`.
///
/// The speculative counterpart is [`crate::speculative::run_generation`], which
/// is written to the same shape on purpose: it drives `sampler` with exactly the
/// call sequence this loop does, so the two produce the same tokens.
fn run_generation(
    model: &LlamaModel,
    ctx: &mut LlamaContext,
    start_pos: i32,
    sampler: &mut LlamaSampler,
    max_tokens: u32,
    on_token: &mut dyn FnMut(&str),
) -> Result<(u32, FinishReason), EngineError> {
    let mut completion_tokens = 0u32;
    let mut finish_reason = FinishReason::Length;
    let mut n_cur = start_pos;
    // One decoder for the whole run: a multi-byte character whose UTF-8 bytes span
    // two tokens is stitched across `token_to_piece` calls.
    let mut decoder = encoding_rs::UTF_8.new_decoder();
    let mut batch = LlamaBatch::new(1, 1);

    while completion_tokens < max_tokens {
        // `-1` samples from the last decoded position's logits.
        let token = sampler.sample(ctx, -1);
        sampler.accept(token);
        if model.is_eog_token(token) {
            finish_reason = FinishReason::Stop;
            break;
        }
        // Emit the exact detokenized piece: no trimming, so the streamed text and
        // `completion_tokens` stay consistent and match OpenAI behaviour.
        let piece = model
            .token_to_piece(token, &mut decoder, false, None)
            .map_err(|e| EngineError::Inference(format!("detokenize: {e}")))?;
        on_token(&piece);

        batch.clear();
        batch
            .add(token, n_cur, &[0], true)
            .map_err(|e| EngineError::Inference(format!("decode batch: {e}")))?;
        n_cur += 1;
        completion_tokens += 1;
        ctx.decode(&mut batch)
            .map_err(|e| EngineError::Inference(format!("decode: {e}")))?;
    }

    Ok((completion_tokens, finish_reason))
}

impl Engine for LlamaEngine {
    /// Always. `render_prompt` guarantees it: the model's own chat template
    /// renders the tools where it references them, and `render_advertising`
    /// supplies a plain advertisement where it does not.
    fn carries_tools(&self, model: &str) -> bool {
        let _ = model;
        true
    }

    fn models(&self) -> Vec<ModelInfo> {
        self.served
            .iter()
            .map(|s| ModelInfo { id: s.name.clone() })
            .collect()
    }

    fn chat_stream(
        &self,
        req: &ChatRequest,
        on_token: &mut dyn FnMut(&str),
    ) -> Result<CompletionStats, EngineError> {
        let served = self
            .served
            .iter()
            .find(|s| s.name == req.model)
            .ok_or_else(|| EngineError::UnknownModel(req.model.clone()))?;
        let path = served.path.clone();
        let mmproj = served.mmproj.clone();

        // Pick the media modality from the request. A request carries at most one
        // modality; both set at once is a client error (the projector splices one
        // media stream), rejected up front so no model is loaded for it.
        let modality = match (!req.images.is_empty(), !req.audio.is_empty()) {
            (false, false) => None,
            (true, false) => Some(Modality::Vision),
            (false, true) => Some(Modality::Audio),
            (true, true) => {
                return Err(EngineError::InvalidRequest(
                    "a request may carry images or audio, not both".to_owned(),
                ));
            }
        };

        // Resolve + load under the cache lock, clone the shared handles, then
        // release the lock so generation on a *different* model can run
        // concurrently (see the module-level "Concurrency" note).
        let resolved = self.resolve(&req.model, &path)?;
        let model = &resolved.model;

        // Serialise *all* interactions with this model instance (llama.cpp is not
        // assumed thread-safe): acquire the per-model generation lock BEFORE any
        // `LlamaModel` FFI call — including the metadata read in the guard below —
        // so a concurrent request on the same model can never touch the handle
        // unserialised. The lock is per-model, so a *different* model still decodes
        // concurrently; nothing below re-locks it (`chat_text`/`chat_media` borrow
        // the already-locked model).
        let _gen = resolved
            .gen_lock
            .lock()
            .map_err(|_| EngineError::Inference("model generation lock poisoned".to_owned()))?;

        // Defensive guard (ADR-0006), run under `_gen`: an *encoder-only* embedding
        // model (a BERT-arch `bge-*`, say) cannot generate. Driving one through the
        // decode path makes llama.cpp take its encoder route, which aborts the
        // **whole process** with `GGML_ASSERT(n_ubatch >= n_tokens)` — a single
        // mis-addressed chat request would kill the server for everyone. Detect it
        // from the GGUF architecture and reject it as a typed client error *before*
        // any decode. The serving layer already keeps such models out of the Ask
        // model pool; this backstops a direct `POST /v1/chat/completions`.
        if let Ok(arch) = model.meta_val_str("general.architecture")
            && is_encoder_only_arch(&arch)
        {
            return Err(EngineError::InvalidRequest(format!(
                "model `{}` is an embedding model (encoder-only architecture `{arch}`) \
                 and cannot generate chat completions — use `/v1/embeddings` instead",
                req.model,
            )));
        }

        match (modality, mmproj.as_deref()) {
            (None, _) => self.chat_text(&resolved, req, on_token),
            (Some(m), Some(mmproj)) => {
                // Built on the first media blob for this (model, mmproj) pair and
                // reused by every one after it (issue #301).
                let projector = self.projector(&resolved, mmproj)?;
                self.chat_media(&projector, req, m, on_token)
            }
            (Some(m), None) => Err(EngineError::InvalidRequest(format!(
                "model `{}` is text-only and cannot accept {}",
                req.model,
                m.noun(),
            ))),
        }
    }

    fn embed(&self, model: &str, inputs: &[String]) -> Result<Vec<Vec<f32>>, EngineError> {
        let path = self
            .path_for(model)
            .ok_or_else(|| EngineError::UnknownModel(model.to_owned()))?;
        // Resolve + load under the cache lock, then release it before running the
        // (potentially long) embedding pass; serialise on this model instance.
        let resolved = self.resolve(model, &path)?;
        let model_ref = &resolved.model;
        let _gen = resolved
            .gen_lock
            .lock()
            .map_err(|_| EngineError::Inference("model generation lock poisoned".to_owned()))?;

        // One embeddings-enabled context, reused across all inputs — creating a
        // fresh context per input (re-allocating the KV cache each time) dominated
        // the cost of embedding a whole repo. The KV cache is cleared between
        // inputs so each is pooled independently. Pooling defaults to the model's
        // own type (CLS for BGE), giving one sentence vector per input.
        // Sized to the model's own trained window rather than to a request
        // (issue #486), because this is the one context that is *not* per
        // request: it is built once and reused across every input, so there is
        // no single request to size it to. Passing `0` as the prompt count asks
        // [`window_for_request`] for the largest window this model and this
        // operator allow.
        //
        // For the BERT family this serves, that is a **reduction**: `bge-large-
        // en-v1.5` declares `n_ctx_train = 512`, and it was being given a 4,096
        // window it could not use a quarter of. The bound that actually refuses
        // an over-long input is `n_ubatch` (512, unchanged — see
        // [`batch_capacity`]), so nothing an operator could embed before stops
        // embedding now; the context simply stops reserving what the model was
        // never trained to address.
        let n_ctx = self.request_window(model_ref, 0, 0);
        let ctx_params = crate::speculative::base_params(n_ctx).with_embeddings(true);
        let mut ctx = model_ref
            .new_context(&self.backend, ctx_params)
            .map_err(|e| EngineError::Inference(format!("embedding context: {e}")))?;

        // The embedding models this serves are the BERT family, and llama.cpp
        // routes those through `llama_context::encode`, whose bound is the
        // *physical* batch and is four times tighter than the generative one — so
        // which bound applies has to be settled from the architecture, per model
        // (issue #346). A generative model asked for embeddings decodes normally
        // and gets the wider bound.
        let encoder_only = model_ref
            .meta_val_str("general.architecture")
            .is_ok_and(|arch| is_encoder_only_arch(&arch));
        let capacity = batch_capacity(&ctx, encoder_only);

        let mut out = Vec::with_capacity(inputs.len());
        for input in inputs {
            ctx.clear_kv_cache();

            let tokens = model_ref
                .str_to_token(input, AddBos::Always)
                .map_err(|e| EngineError::Inference(format!("tokenize: {e}")))?;
            if tokens.is_empty() {
                return Err(EngineError::Inference(
                    "empty input tokenizes to nothing".to_owned(),
                ));
            }
            // Per input, not per request: one over-long string in a batch of
            // otherwise fine ones is still the one that would abort the process.
            check_batch_capacity(tokens.len(), capacity, "embedding input")?;

            let mut batch = LlamaBatch::new(tokens.len(), 1);
            for (i, token) in tokens.iter().enumerate() {
                let pos = i32::try_from(i).unwrap_or(i32::MAX);
                // Enable output for every token so pooling sees the whole sequence.
                batch
                    .add(*token, pos, &[0], true)
                    .map_err(|e| EngineError::Inference(format!("embedding batch: {e}")))?;
            }
            ctx.decode(&mut batch)
                .map_err(|e| EngineError::Inference(format!("embedding decode: {e}")))?;

            let vector = ctx
                .embeddings_seq_ith(0)
                .map_err(|e| EngineError::Inference(format!("read embedding: {e}")))?;
            out.push(l2_normalize(vector));
        }
        Ok(out)
    }
}

/// L2-normalise an embedding (unit length), so cosine similarity is a dot
/// product — the convention OpenAI clients expect. A zero vector is returned
/// unchanged.
fn l2_normalize(v: &[f32]) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        v.iter().map(|x| x / norm).collect()
    } else {
        v.to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ChatTemplateError, EngineError, LlamaChatTemplate, MIN_N_CTX, WINDOW_HEADROOM,
        check_batch_capacity, fallback_template_name, install_native_log_bridge,
        is_encoder_only_arch, lru_evict_count, resolve_chat_template_from, window_for_request,
    };

    /// The trained windows of the models actually served, read from their GGUFs
    /// (`tests/context_window.rs` prints them). They span **512×**, which is the
    /// whole reason one engine-wide `n_ctx` could not be right.
    const QWEN_TRAINED: u32 = 262_144;
    const SMOL_TRAINED: u32 = 8_192;
    const BGE_TRAINED: u32 = 512;

    /// A small request does not get a large context, even on a model that could
    /// support one — which is the point of sizing per request. Measured, a
    /// 262,144-token context on `qwen3.8-27b` costs 16,466 MiB; paying that to
    /// answer a fifty-token question is what this prevents.
    #[test]
    fn a_small_request_gets_the_floor_not_the_trained_maximum() {
        assert_eq!(
            window_for_request(50, 512, 0, QWEN_TRAINED),
            MIN_N_CTX,
            "a request that fits the floor must not allocate the trained maximum"
        );
    }

    /// A large request gets what it needs — "as large as possible" *when it is
    /// needed*, which a fixed default cannot express in the same engine.
    #[test]
    fn a_large_request_grows_to_what_it_needs() {
        let window = window_for_request(200_000, 4_096, 0, QWEN_TRAINED);
        assert_eq!(window, 200_000 + 4_096 + WINDOW_HEADROOM);
        assert!(
            window <= QWEN_TRAINED,
            "growth stops at the trained window: {window}"
        );
    }

    /// `max_tokens` is part of the window, not an afterthought: those positions
    /// are written to the same KV cache the prompt is. A window sized to the
    /// prompt alone would run out exactly when the model started answering.
    #[test]
    fn the_generation_budget_is_counted_in_the_window() {
        let big_answer = window_for_request(8_000, 32_000, 0, QWEN_TRAINED);
        let small_answer = window_for_request(8_000, 16, 0, QWEN_TRAINED);
        assert!(
            big_answer > small_answer,
            "a larger `max_tokens` must widen the window: {big_answer} vs {small_answer}"
        );
    }

    /// The model's own trained window always wins — the case the issue's
    /// "refused or clamped, never silently wrong" rule is about. Clamped here,
    /// and the request that then does not fit is refused by
    /// [`check_batch_capacity`] rather than quietly truncated.
    #[test]
    fn the_trained_window_caps_both_the_request_and_the_operator() {
        // A request asking past the trained window is capped at it, not beyond.
        assert_eq!(
            window_for_request(1_000_000, 4_096, 0, SMOL_TRAINED),
            SMOL_TRAINED
        );
        // And an operator ceiling above the trained window cannot raise it.
        assert_eq!(
            window_for_request(1_000_000, 4_096, u32::MAX, SMOL_TRAINED),
            SMOL_TRAINED
        );
    }

    /// The 512-token model is the one a single engine-wide number gets wrong in
    /// the *other* direction, and the floor must not override its real ceiling —
    /// `bge-large-en-v1.5` was being handed a 4,096-token context for a model
    /// trained at 512.
    #[test]
    fn a_model_trained_below_the_floor_is_clamped_down_to_its_own_window() {
        // Pinned at compile time rather than asserted at run time: both sides are
        // constants, so this states a precondition of the test above rather than
        // testing anything, and `const` is where such a claim belongs.
        const _: () = assert!(
            BGE_TRAINED < MIN_N_CTX,
            "the test below is only meaningful while the floor is above bge's window"
        );

        assert_eq!(window_for_request(0, 0, 0, BGE_TRAINED), BGE_TRAINED);
        assert_eq!(window_for_request(64, 512, 0, BGE_TRAINED), BGE_TRAINED);
    }

    /// An operator's ceiling lowers the window and never raises it — the
    /// property that makes the config key a *value* under ADR-0007 v1.4 rather
    /// than a capability: it can only ever spend less of the machine.
    #[test]
    fn the_operator_ceiling_can_only_lower_the_window() {
        for ceiling in [0_u32, 512, 4_096, 32_768, 262_144, u32::MAX] {
            let bounded = window_for_request(100_000, 1_024, ceiling, QWEN_TRAINED);
            let unbounded = window_for_request(100_000, 1_024, 0, QWEN_TRAINED);
            assert!(
                bounded <= unbounded.max(MIN_N_CTX.min(ceiling.max(1))),
                "ceiling {ceiling} produced {bounded}, above the unbounded {unbounded}"
            );
            assert!(
                bounded > 0,
                "ceiling {ceiling} produced a zero-token window"
            );
        }
    }

    /// Saturating arithmetic, not wrapping: a caller asking for `u32::MAX`
    /// generated tokens must get the trained window, not a tiny one produced by
    /// an overflow.
    #[test]
    fn an_absurd_request_saturates_rather_than_wrapping() {
        assert_eq!(
            window_for_request(u32::MAX, u32::MAX, 0, QWEN_TRAINED),
            QWEN_TRAINED
        );
    }

    /// A model whose GGUF declares no trained window still gets a usable
    /// context. llama.cpp refuses a zero-token one, so "unknown" has to mean the
    /// floor rather than nothing.
    #[test]
    fn a_model_declaring_no_trained_window_still_gets_the_floor() {
        assert_eq!(window_for_request(0, 0, 0, 0), MIN_N_CTX);
    }

    /// Nothing shrinks. Every request that fit the old fixed 4,096 window still
    /// gets at least 4,096 on any model trained for it — the invariant that lets
    /// this land without a behaviour-change note for existing deployments.
    #[test]
    fn no_request_gets_less_than_the_window_it_used_to_get() {
        for prompt in [0_u32, 1, 512, 2_048, 3_146, 4_000] {
            for max_tokens in [0_u32, 16, 512] {
                let window = window_for_request(prompt, max_tokens, 0, QWEN_TRAINED);
                assert!(
                    window >= MIN_N_CTX,
                    "prompt {prompt} + {max_tokens} got {window}, below the old default"
                );
            }
        }
    }

    /// A prompt the size of the limit is the largest one llama.cpp accepts —
    /// `GGML_ASSERT(n_tokens_all <= cparams.n_batch)` is `<=`, not `<`. Pinning
    /// the boundary from both sides is what makes an off-by-one here (a `>=`
    /// where the code says `>`) a test failure rather than a silent 400 on
    /// perfectly valid input.
    #[test]
    fn a_prompt_exactly_at_the_limit_is_accepted() {
        assert!(check_batch_capacity(2048, 2048, "prompt").is_ok());
        assert!(check_batch_capacity(2047, 2048, "prompt").is_ok());
        assert!(check_batch_capacity(0, 2048, "prompt").is_ok());
        // A limit of zero can only accept an empty batch, and llama.cpp rejects
        // those separately — but it must not be read as "no limit".
        assert!(check_batch_capacity(1, 0, "prompt").is_err());
    }

    /// One token over is refused, as a *client* error — the variant `rto-serve`
    /// maps to 400. If this ever came back as `Inference` it would be reported as
    /// a 500, telling the caller the server broke when in fact their input was
    /// too long.
    #[test]
    fn a_prompt_over_the_limit_is_an_invalid_request_naming_both_numbers() {
        let err = check_batch_capacity(2049, 2048, "prompt")
            .expect_err("2049 tokens must not be accepted by a 2048-token batch");
        assert!(
            matches!(err, EngineError::InvalidRequest(_)),
            "over-long input is a client error, not an inference failure: {err:?}"
        );
        // The message has to carry both numbers: a caller who cannot see the
        // server's configuration has no other way to learn how much to cut.
        let msg = err.to_string();
        assert!(msg.contains("2049"), "message must name the size: {msg}");
        assert!(msg.contains("2048"), "message must name the limit: {msg}");
        assert!(
            msg.contains("prompt"),
            "message must name what was too long: {msg}"
        );
    }

    /// The embedding path passes a different noun and a different (tighter)
    /// limit, and the message follows both — one shared helper, two call sites,
    /// no second wording to keep in step.
    #[test]
    fn the_embedding_bound_reports_its_own_noun_and_limit() {
        assert!(check_batch_capacity(512, 512, "embedding input").is_ok());
        let msg = check_batch_capacity(513, 512, "embedding input")
            .expect_err("513 tokens must not be accepted by a 512-token batch")
            .to_string();
        assert!(msg.contains("embedding input"), "{msg}");
        assert!(msg.contains("513") && msg.contains("512"), "{msg}");
    }

    #[test]
    fn encoder_only_arch_is_detected() {
        // BERT-family embedding architectures (what `bge-*` GGUFs report) are
        // encoder-only and must be flagged so the chat path rejects them before the
        // decode call that would abort the process with a GGML_ASSERT.
        for arch in [
            "bert",
            "BERT",
            "nomic-bert",
            "nomic-bert-moe",
            "jina-bert-v2",
            "distilbert",
            "roberta",
        ] {
            assert!(is_encoder_only_arch(arch), "{arch} should be encoder-only");
        }
        // Generative families this engine actually serves for chat are not flagged.
        for arch in ["qwen2", "qwen3", "llama", "gemma3", "phi3", "moondream2"] {
            assert!(
                !is_encoder_only_arch(arch),
                "{arch} is generative, not encoder-only"
            );
        }
    }

    /// Installing the native-log → tracing bridge is safe to call repeatedly (the
    /// `Once` guard makes every call after the first a no-op) and never panics —
    /// so constructing several engines in one process can't double-install or
    /// crash. No model load required; this only exercises the FFI log setter.
    #[test]
    fn native_log_bridge_install_is_idempotent() {
        install_native_log_bridge();
        install_native_log_bridge();
        assert!(
            super::NATIVE_LOG_BRIDGE.is_completed(),
            "the bridge is installed exactly once"
        );
    }

    #[test]
    fn budget_zero_keeps_a_single_model() {
        // The default (budget 0) unloads everything but the most-recently-used.
        assert_eq!(lru_evict_count(&[100, 100, 100], 0), 2);
        assert_eq!(lru_evict_count(&[100], 0), 0, "never evict the only model");
        assert_eq!(lru_evict_count(&[], 0), 0);
    }

    #[test]
    fn budget_evicts_oldest_until_it_fits() {
        // 300 bytes over a 250 budget → drop the oldest (100), leaving 200.
        assert_eq!(lru_evict_count(&[100, 100, 100], 250), 1);
        // Comfortably under budget → keep everything.
        assert_eq!(lru_evict_count(&[100, 100, 100], 1000), 0);
        // Exactly at budget → no eviction.
        assert_eq!(lru_evict_count(&[100, 100, 100], 300), 0);
    }

    #[test]
    fn mru_is_retained_even_if_it_alone_exceeds_budget() {
        // A single resident model larger than the budget is still kept — a
        // request for it must be servable.
        assert_eq!(lru_evict_count(&[500], 10), 0);
        // With an older small model present, only the old one is evicted.
        assert_eq!(lru_evict_count(&[100, 500], 10), 1);
    }

    #[test]
    fn embedded_template_is_used_unchanged() {
        // When the GGUF embeds a chat template, resolution returns it verbatim —
        // no fallback, byte-for-byte identical (behaviour for qwen*/deepseek/etc.
        // is unaffected). The architecture provider must not be consulted on this
        // happy path, so wire it to panic if it ever is.
        let embedded = "<|im_start|>{{ messages }}<|im_end|>";
        let resolved =
            resolve_chat_template_from(Ok(LlamaChatTemplate::new(embedded).unwrap()), || {
                panic!("architecture must not be read when a template is embedded")
            })
            .expect("embedded template resolves");
        assert_eq!(resolved.to_str().unwrap(), embedded);
    }

    #[test]
    fn missing_template_falls_back_to_a_valid_builtin() {
        // A model with no embedded template (e.g. moondream2, no architecture)
        // must still yield a usable template instead of the old null-pointer
        // error. The fallback is a llama.cpp built-in identifier, not an empty
        // or null string, so apply_chat_template can expand it into a prompt.
        let resolved = resolve_chat_template_from(Err(ChatTemplateError::MissingTemplate), || None)
            .expect("missing template falls back");
        assert_eq!(resolved.to_str().unwrap(), "chatml");

        // A recognised family maps to its own known format.
        let gemma = resolve_chat_template_from(Err(ChatTemplateError::MissingTemplate), || {
            Some("gemma2".to_owned())
        })
        .expect("family fallback resolves");
        assert_eq!(gemma.to_str().unwrap(), "gemma");
    }

    #[test]
    fn family_detection_maps_arch_to_known_or_chatml() {
        // Qwen chat models use ChatML; unknown/absent architectures also default
        // to ChatML — a widely compatible neutral format.
        assert_eq!(fallback_template_name(Some("qwen3")), "chatml");
        assert_eq!(fallback_template_name(Some("qwen2")), "chatml");
        assert_eq!(fallback_template_name(Some("gemma3")), "gemma");
        assert_eq!(fallback_template_name(Some("phi3")), "phi3");
        assert_eq!(fallback_template_name(Some("some-unknown-arch")), "chatml");
        assert_eq!(fallback_template_name(None), "chatml");
    }

    #[test]
    fn non_missing_lookup_error_surfaces_as_typed_error() {
        // A genuine lookup failure (not "missing") is not silently masked by the
        // fallback — it becomes a typed inference error, never a crash.
        let nul = std::ffi::CString::new(vec![b'a', 0, b'b']).unwrap_err();
        let err = resolve_chat_template_from(Err(ChatTemplateError::NullError(nul)), || None);
        assert!(matches!(err, Err(EngineError::Inference(_))));
    }
}

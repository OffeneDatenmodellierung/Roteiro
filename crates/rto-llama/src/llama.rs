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

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use llama_cpp_2::ChatTemplateError;
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaChatMessage, LlamaChatTemplate, LlamaModel};
use llama_cpp_2::mtmd::{
    MtmdBitmap, MtmdContext, MtmdContextParams, MtmdInputText, mtmd_default_marker,
};
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_2::{LogOptions, send_logs_to_tracing};

use crate::engine::{ChatRequest, CompletionStats, Engine, EngineError, FinishReason, ModelInfo};

/// Default context window when the caller does not set one.
const DEFAULT_N_CTX: u32 = 4096;

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
pub struct LlamaEngine {
    backend: LlamaBackend,
    served: Vec<Served>,
    n_ctx: u32,
    cache: Mutex<ModelCache>,
}

/// One loaded model held in the residency cache.
struct Loaded {
    name: String,
    /// On-disk GGUF size, used as a proxy for the model's memory footprint when
    /// deciding what to evict (the real RSS is not cheaply available).
    bytes: u64,
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
    /// Build an engine serving `served`, with an `n_ctx` context window
    /// (`0` selects the default). Keeps a single model resident; use
    /// [`LlamaEngine::new_with_budget`] to hold several. Initialises the
    /// llama.cpp backend once.
    ///
    /// # Errors
    /// Returns an error if the llama.cpp backend fails to initialise.
    pub fn new(served: Vec<Served>, n_ctx: u32) -> anyhow::Result<Self> {
        Self::new_with_budget(served, n_ctx, 0)
    }

    /// Build an engine that keeps as many models resident as fit within
    /// `budget_bytes` of (GGUF-size-proxied) memory, unloading the
    /// least-recently-used past that cap. `0` keeps a single model (the default).
    ///
    /// # Errors
    /// Returns an error if the llama.cpp backend fails to initialise.
    pub fn new_with_budget(
        served: Vec<Served>,
        n_ctx: u32,
        budget_bytes: u64,
    ) -> anyhow::Result<Self> {
        // Redirect llama.cpp + ggml's native logs through `tracing` *before*
        // `LlamaBackend::init()` — the backend's device probe (e.g. ggml-metal's
        // `ggml_metal_device_init` block) logs during init, so installing the
        // callback afterwards would let that first batch escape to stderr. The log
        // setters are global C functions that need no initialised backend, so
        // setting them first is safe and captures everything the model loads emit.
        install_native_log_bridge();
        let backend = LlamaBackend::init()?;
        Ok(Self {
            backend,
            served,
            n_ctx: if n_ctx == 0 { DEFAULT_N_CTX } else { n_ctx },
            cache: Mutex::new(ModelCache {
                budget_bytes,
                loaded: Vec::new(),
            }),
        })
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
            cache.loaded.push(Loaded {
                name: name.to_owned(),
                bytes,
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
    /// handles — the model and its per-instance generation lock — releasing the
    /// cache lock before returning. Holding the returned `Arc`s lets the caller
    /// generate without pinning the cache: other models stay servable, and this
    /// model survives eviction until the last handle drops.
    fn resolve(
        &self,
        name: &str,
        path: &Path,
    ) -> Result<(Arc<LlamaModel>, Arc<Mutex<()>>), EngineError> {
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| EngineError::Inference("engine mutex poisoned".to_owned()))?;
        let idx = self.ensure_loaded(&mut cache, name, path)?;
        let l = &cache.loaded[idx];
        Ok((Arc::clone(&l.model), Arc::clone(&l.gen_lock)))
    }

    /// Resolve a served model name to its GGUF path.
    fn path_for(&self, name: &str) -> Option<PathBuf> {
        self.served
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.path.clone())
    }

    /// A fresh context sized to `self.n_ctx`, borrowing `model`.
    fn new_context<'m>(&self, model: &'m LlamaModel) -> Result<LlamaContext<'m>, EngineError> {
        let n_ctx = NonZeroU32::new(self.n_ctx).unwrap_or(NonZeroU32::MIN);
        let params = LlamaContextParams::default().with_n_ctx(Some(n_ctx));
        model
            .new_context(&self.backend, params)
            .map_err(|e| EngineError::Inference(format!("context: {e}")))
    }

    /// Text-only chat: apply the chat template, prime the prompt, and generate.
    fn chat_text(
        &self,
        model: &LlamaModel,
        req: &ChatRequest,
        on_token: &mut dyn FnMut(&str),
    ) -> Result<CompletionStats, EngineError> {
        let messages = req
            .messages
            .iter()
            .map(|m| LlamaChatMessage::new(m.role.clone(), m.content.clone()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| EngineError::Inference(format!("chat message: {e}")))?;
        let template = resolve_chat_template(model)?;
        let prompt = model
            .apply_chat_template(&template, &messages, true)
            .map_err(|e| EngineError::Inference(format!("apply chat template: {e}")))?;

        let tokens = model
            .str_to_token(&prompt, AddBos::Always)
            .map_err(|e| EngineError::Inference(format!("tokenize: {e}")))?;
        let prompt_tokens = u32::try_from(tokens.len()).unwrap_or(u32::MAX);

        let mut ctx = self.new_context(model)?;

        // Prime the batch with the prompt; only the last token needs logits. Size
        // the batch to the prompt; whether it fits the context is enforced by the
        // decode call.
        let mut batch = LlamaBatch::new(tokens.len().max(1), 1);
        let last = tokens.len().saturating_sub(1);
        for (i, token) in tokens.iter().enumerate() {
            let pos = i32::try_from(i).unwrap_or(i32::MAX);
            batch
                .add(*token, pos, &[0], i == last)
                .map_err(|e| EngineError::Inference(format!("prompt batch: {e}")))?;
        }
        ctx.decode(&mut batch)
            .map_err(|e| EngineError::Inference(format!("prompt decode: {e}")))?;

        let start = i32::try_from(tokens.len()).unwrap_or(i32::MAX);
        let (completion_tokens, finish_reason) = run_generation(
            model,
            &mut ctx,
            start,
            req.temperature,
            req.max_tokens,
            on_token,
        )?;
        Ok(CompletionStats {
            prompt_tokens,
            completion_tokens,
            finish_reason,
        })
    }

    /// Multimodal chat (ADR-0006): project the request's `modality` media
    /// (images or audio) through `mmproj` and generate. The media are placed at
    /// media markers inside the last user turn; the use case is just a prompt
    /// ("transcribe the text in this image" / "transcribe this audio"). Both
    /// modalities share this path — the projector decodes the raw file bytes
    /// (images via `stb_image`, audio via miniaudio) and only the support check
    /// and which byte vectors are read differ.
    fn chat_media(
        &self,
        model: &LlamaModel,
        mmproj: &Path,
        req: &ChatRequest,
        modality: Modality,
        on_token: &mut dyn FnMut(&str),
    ) -> Result<CompletionStats, EngineError> {
        let mmproj = mmproj
            .to_str()
            .ok_or_else(|| EngineError::Inference("non-UTF-8 mmproj path".to_owned()))?;
        let mtmd = MtmdContext::init_from_file(mmproj, model, &MtmdContextParams::default())
            .map_err(|e| EngineError::Inference(format!("init projector: {e}")))?;
        if !modality.supported(&mtmd) {
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
            .map(|blob| MtmdBitmap::from_buffer(&mtmd, blob, false))
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

        let mut ctx = self.new_context(model)?;
        // `eval_chunks` decodes text + projected media (image/audio) embeddings
        // into the context and returns the new position to continue generating.
        let n_past = chunks
            .eval_chunks(&mtmd, &ctx, 0, 0, 512, true)
            .map_err(|e| EngineError::Inference(format!("mtmd eval: {e}")))?;

        let (completion_tokens, finish_reason) = run_generation(
            model,
            &mut ctx,
            n_past,
            req.temperature,
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

    let messages = req
        .messages
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let content = if i == target {
                format!("{markers}{}", m.content)
            } else {
                m.content.clone()
            };
            LlamaChatMessage::new(m.role.clone(), content)
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| EngineError::Inference(format!("chat message: {e}")))?;

    let template = resolve_chat_template(model)?;
    model
        .apply_chat_template(&template, &messages, true)
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

/// The shared sampling loop: from `start_pos`, sample → emit → decode until an
/// end-of-generation token or `max_tokens`. Returns `(completion_tokens, reason)`.
fn run_generation(
    model: &LlamaModel,
    ctx: &mut LlamaContext,
    start_pos: i32,
    temperature: f32,
    max_tokens: u32,
    on_token: &mut dyn FnMut(&str),
) -> Result<(u32, FinishReason), EngineError> {
    // Greedy when temperature is 0 (deterministic); otherwise temp + dist.
    let mut sampler = if temperature <= 0.0 {
        LlamaSampler::greedy()
    } else {
        LlamaSampler::chain_simple([LlamaSampler::temp(temperature), LlamaSampler::dist(1234)])
    };

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
        let (model, gen_lock) = self.resolve(&req.model, &path)?;
        // Serialise decode on this model instance.
        let _gen = gen_lock
            .lock()
            .map_err(|_| EngineError::Inference("model generation lock poisoned".to_owned()))?;

        match (modality, mmproj.as_deref()) {
            (None, _) => self.chat_text(&model, req, on_token),
            (Some(m), Some(mmproj)) => self.chat_media(&model, mmproj, req, m, on_token),
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
        let (model_ref, gen_lock) = self.resolve(model, &path)?;
        let _gen = gen_lock
            .lock()
            .map_err(|_| EngineError::Inference("model generation lock poisoned".to_owned()))?;

        let n_ctx = NonZeroU32::new(self.n_ctx).unwrap_or(NonZeroU32::MIN);
        // One embeddings-enabled context, reused across all inputs — creating a
        // fresh context per input (re-allocating the KV cache each time) dominated
        // the cost of embedding a whole repo. The KV cache is cleared between
        // inputs so each is pooled independently. Pooling defaults to the model's
        // own type (CLS for BGE), giving one sentence vector per input.
        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(Some(n_ctx))
            .with_embeddings(true);
        let mut ctx = model_ref
            .new_context(&self.backend, ctx_params)
            .map_err(|e| EngineError::Inference(format!("embedding context: {e}")))?;

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
        ChatTemplateError, EngineError, LlamaChatTemplate, fallback_template_name,
        install_native_log_bridge, lru_evict_count, resolve_chat_template_from,
    };

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

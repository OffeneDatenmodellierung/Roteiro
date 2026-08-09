//! The llama.cpp-backed [`Engine`] (ADR-0006), behind the `llama` feature.
//!
//! Loads a plain GGUF (embedded tokenizer + chat template come for free), keeps
//! one model warm, and serialises requests through a mutex — llama.cpp batching
//! is a later enhancement. Serves only the models it was handed; never downloads.

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Mutex;

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaChatMessage, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;

use crate::engine::{ChatRequest, CompletionStats, Engine, EngineError, FinishReason, ModelInfo};

/// Default context window when the caller does not set one.
const DEFAULT_N_CTX: u32 = 4096;

/// One installed model this engine may serve: its public name and GGUF path.
#[derive(Debug, Clone)]
pub struct Served {
    /// Public model id (registry name).
    pub name: String,
    /// Path to the GGUF file on disk.
    pub path: PathBuf,
}

/// A llama.cpp inference engine over a fixed set of installed GGUF models.
pub struct LlamaEngine {
    backend: LlamaBackend,
    served: Vec<Served>,
    n_ctx: u32,
    warm: Mutex<Option<Warm>>,
}

/// The single model kept loaded between requests.
struct Warm {
    name: String,
    model: LlamaModel,
}

impl LlamaEngine {
    /// Build an engine serving `served`, with an `n_ctx` context window
    /// (`0` selects the default). Initialises the llama.cpp backend once.
    ///
    /// # Errors
    /// Returns an error if the llama.cpp backend fails to initialise.
    pub fn new(served: Vec<Served>, n_ctx: u32) -> anyhow::Result<Self> {
        let backend = LlamaBackend::init()?;
        Ok(Self {
            backend,
            served,
            n_ctx: if n_ctx == 0 { DEFAULT_N_CTX } else { n_ctx },
            warm: Mutex::new(None),
        })
    }

    /// Resolve a served model name to its GGUF path.
    fn path_for(&self, name: &str) -> Option<PathBuf> {
        self.served
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.path.clone())
    }
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
        let path = self
            .path_for(&req.model)
            .ok_or_else(|| EngineError::UnknownModel(req.model.clone()))?;

        let mut warm = self
            .warm
            .lock()
            .map_err(|_| EngineError::Inference("engine mutex poisoned".to_owned()))?;

        // Load lazily and keep warm; a different model evicts the previous one.
        if warm.as_ref().map(|w| w.name.as_str()) != Some(req.model.as_str()) {
            let params = LlamaModelParams::default();
            let model = LlamaModel::load_from_file(&self.backend, &path, &params)
                .map_err(|e| EngineError::Inference(format!("load `{}`: {e}", req.model)))?;
            *warm = Some(Warm {
                name: req.model.clone(),
                model,
            });
        }
        let model = &warm.as_ref().expect("just loaded").model;

        // Apply the GGUF's embedded chat template to render the prompt.
        let messages = req
            .messages
            .iter()
            .map(|m| LlamaChatMessage::new(m.role.clone(), m.content.clone()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| EngineError::Inference(format!("chat message: {e}")))?;
        let template = model
            .chat_template(None)
            .map_err(|e| EngineError::Inference(format!("no chat template: {e}")))?;
        let prompt = model
            .apply_chat_template(&template, &messages, true)
            .map_err(|e| EngineError::Inference(format!("apply chat template: {e}")))?;

        let tokens = model
            .str_to_token(&prompt, AddBos::Always)
            .map_err(|e| EngineError::Inference(format!("tokenize: {e}")))?;
        let prompt_tokens = u32::try_from(tokens.len()).unwrap_or(u32::MAX);

        let n_ctx = NonZeroU32::new(self.n_ctx).unwrap_or(NonZeroU32::MIN);
        let ctx_params = LlamaContextParams::default().with_n_ctx(Some(n_ctx));
        let mut ctx = model
            .new_context(&self.backend, ctx_params)
            .map_err(|e| EngineError::Inference(format!("context: {e}")))?;

        // Prime the batch with the prompt; only the last token needs logits. The
        // batch must hold the whole prompt at once, so size it to the prompt (the
        // single-token decode steps below reuse the same batch). Whether the
        // prompt fits the context window is enforced by the decode call.
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

        // Greedy when temperature is 0 (deterministic); otherwise temp + dist.
        let mut sampler = if req.temperature <= 0.0 {
            LlamaSampler::greedy()
        } else {
            LlamaSampler::chain_simple([
                LlamaSampler::temp(req.temperature),
                LlamaSampler::dist(1234),
            ])
        };

        let mut completion_tokens = 0u32;
        let mut finish_reason = FinishReason::Length;
        let mut n_cur = i32::try_from(tokens.len()).unwrap_or(i32::MAX);
        // One decoder for the whole run: a multi-byte character whose UTF-8 bytes
        // span two tokens is stitched across `token_to_piece` calls.
        let mut decoder = encoding_rs::UTF_8.new_decoder();

        while completion_tokens < req.max_tokens {
            let token = sampler.sample(&ctx, batch.n_tokens() - 1);
            sampler.accept(token);
            if model.is_eog_token(token) {
                finish_reason = FinishReason::Stop;
                break;
            }
            // Emit the exact detokenized piece: no trimming, so the streamed text
            // and `completion_tokens` stay consistent and match OpenAI behaviour.
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

        Ok(CompletionStats {
            prompt_tokens,
            completion_tokens,
            finish_reason,
        })
    }

    fn embed(&self, model: &str, inputs: &[String]) -> Result<Vec<Vec<f32>>, EngineError> {
        let path = self
            .path_for(model)
            .ok_or_else(|| EngineError::UnknownModel(model.to_owned()))?;
        let mut warm = self
            .warm
            .lock()
            .map_err(|_| EngineError::Inference("engine mutex poisoned".to_owned()))?;
        if warm.as_ref().map(|w| w.name.as_str()) != Some(model) {
            let params = LlamaModelParams::default();
            let loaded = LlamaModel::load_from_file(&self.backend, &path, &params)
                .map_err(|e| EngineError::Inference(format!("load `{model}`: {e}")))?;
            *warm = Some(Warm {
                name: model.to_owned(),
                model: loaded,
            });
        }
        let model_ref = &warm.as_ref().expect("just loaded").model;

        let n_ctx = NonZeroU32::new(self.n_ctx).unwrap_or(NonZeroU32::MIN);
        let mut out = Vec::with_capacity(inputs.len());
        for input in inputs {
            // A fresh embeddings-enabled context per input; pooling defaults to the
            // model's own type (CLS for BGE), giving one sentence vector.
            let ctx_params = LlamaContextParams::default()
                .with_n_ctx(Some(n_ctx))
                .with_embeddings(true);
            let mut ctx = model_ref
                .new_context(&self.backend, ctx_params)
                .map_err(|e| EngineError::Inference(format!("embedding context: {e}")))?;

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

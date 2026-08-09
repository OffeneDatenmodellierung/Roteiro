//! Offline text generation with a small quantized (GGUF) instruct model —
//! ADR-0004 Tier 1, extending ADR-0003's `inference-local-models` tier from
//! embedding to *generation*. CPU-only, pure-Rust (candle), no network at run
//! time. The model is a Qwen2- or Qwen3-family GGUF (`ChatML`), dispatched on the
//! GGUF's `general.architecture`, loaded once and reused.
//!
//! Only built with `--features inference-local-models`.

use std::io::Seek;
use std::path::Path;

use candle_core::quantized::gguf_file;
use candle_core::{Device, Tensor};
use candle_transformers::generation::{LogitsProcessor, Sampling};
use candle_transformers::models::{quantized_qwen2, quantized_qwen3};
use tokenizers::Tokenizer;

use super::LocalModelError;

/// A loaded quantized instruct model — Qwen2 or Qwen3, selected by the GGUF's
/// `general.architecture`. Both candle model types share the same
/// `from_gguf`/`forward`/`clear_kv_cache` shape, so this enum is a thin dispatch.
enum Weights {
    Qwen2(quantized_qwen2::ModelWeights),
    Qwen3(quantized_qwen3::ModelWeights),
}

impl Weights {
    fn forward(&mut self, x: &Tensor, offset: usize) -> candle_core::Result<Tensor> {
        match self {
            Self::Qwen2(m) => m.forward(x, offset),
            Self::Qwen3(m) => m.forward(x, offset),
        }
    }

    fn clear_kv_cache(&mut self) {
        match self {
            Self::Qwen2(m) => m.clear_kv_cache(),
            Self::Qwen3(m) => m.clear_kv_cache(),
        }
    }
}

/// Local filenames the generator expects under a model directory.
const GGUF_FILE: &str = "model.gguf";
const TOKENIZER_FILE: &str = "tokenizer.json";

/// Sampling knobs for [`LocalGenerator::generate`]. Defaults to greedy (argmax),
/// which is deterministic for a given model — the right default for drafting.
#[derive(Debug, Clone, Copy)]
pub struct GenConfig {
    /// Hard cap on newly generated tokens (excludes the prompt).
    pub max_new_tokens: usize,
    /// `None` ⇒ greedy/argmax; `Some(t)` ⇒ temperature sampling.
    pub temperature: Option<f64>,
    /// Nucleus-sampling cutoff, used only when `temperature` is `Some`.
    pub top_p: Option<f64>,
    /// RNG seed, so temperature sampling is reproducible.
    pub seed: u64,
}

impl Default for GenConfig {
    fn default() -> Self {
        Self {
            max_new_tokens: 320,
            temperature: None,
            top_p: None,
            seed: 299_792_458,
        }
    }
}

/// A loaded quantized instruct model, reusable across many `generate` calls.
pub struct LocalGenerator {
    model: Weights,
    tokenizer: Tokenizer,
    device: Device,
    /// End-of-turn / EOS token ids resolved from the tokenizer.
    eos: Vec<u32>,
    /// Whether to suppress Qwen3's `<think>` reasoning block (Qwen3 only): for
    /// drafting we want the answer, not the chain-of-thought.
    suppress_think: bool,
}

impl LocalGenerator {
    /// Load a Qwen2- or Qwen3-family GGUF model and its tokenizer from
    /// `model_dir` (expects `model.gguf` + `tokenizer.json`). The architecture is
    /// read from the GGUF `general.architecture` metadata and dispatched.
    ///
    /// # Errors
    /// Returns [`LocalModelError`] if a file is missing/unreadable, the GGUF
    /// architecture is unsupported, or the tokenizer fails to load.
    pub fn load(model_dir: &Path) -> Result<Self, LocalModelError> {
        let device = Device::Cpu;
        let tokenizer = Tokenizer::from_file(model_dir.join(TOKENIZER_FILE))
            .map_err(|e| LocalModelError::Tokenizer(e.to_string()))?;

        let mut file = std::fs::File::open(model_dir.join(GGUF_FILE))?;
        let content = gguf_file::Content::read(&mut file)?;
        // Read the architecture before `content` is moved into `from_gguf`;
        // default to qwen2 for older GGUFs that omit the key.
        let arch = content
            .metadata
            .get("general.architecture")
            .and_then(|v| v.to_string().ok())
            .map_or_else(|| "qwen2".to_owned(), Clone::clone);
        // `Content::read` left the cursor past the header; `from_gguf` seeks to
        // absolute tensor offsets, so rewind to the file start.
        file.rewind()?;
        let model = match arch.as_str() {
            "qwen2" => Weights::Qwen2(quantized_qwen2::ModelWeights::from_gguf(
                content, &mut file, &device,
            )?),
            "qwen3" => Weights::Qwen3(quantized_qwen3::ModelWeights::from_gguf(
                content, &mut file, &device,
            )?),
            other => return Err(LocalModelError::UnsupportedArch(other.to_owned())),
        };
        let suppress_think = arch == "qwen3";

        // ChatML end-of-turn `<|im_end|>` and the base `<|endoftext|>`; resolved
        // from the tokenizer so a differing vocab still stops correctly. A model
        // with neither would only ever stop on `max_new_tokens`, so treat that as
        // a load error rather than generating run-on text.
        let eos: Vec<u32> = ["<|im_end|>", "<|endoftext|>"]
            .iter()
            .filter_map(|t| tokenizer.token_to_id(t))
            .collect();
        if eos.is_empty() {
            return Err(LocalModelError::Tokenizer(
                "tokenizer has no end-of-turn token (`<|im_end|>`/`<|endoftext|>`)".to_owned(),
            ));
        }

        Ok(Self {
            model,
            tokenizer,
            device,
            eos,
            suppress_think,
        })
    }

    /// Generate a completion for `user` (optionally with a `system` message),
    /// wrapping it in the Qwen `ChatML` template and decoding until an EOS token
    /// or `cfg.max_new_tokens`.
    ///
    /// # Errors
    /// Returns [`LocalModelError`] if tokenization, a forward pass, or decoding
    /// fails.
    pub fn generate(
        &mut self,
        system: Option<&str>,
        user: &str,
        cfg: &GenConfig,
    ) -> Result<String, LocalModelError> {
        // Clear the KV cache up front so the instance is reliably reusable even
        // if a previous `generate` call errored out mid-decode.
        self.model.clear_kv_cache();

        let prompt = chatml(system, user, self.suppress_think);
        let encoding = self
            .tokenizer
            .encode(prompt, true)
            .map_err(|e| LocalModelError::Tokenizer(e.to_string()))?;
        let prompt_tokens = encoding.get_ids();
        if prompt_tokens.is_empty() {
            return Ok(String::new());
        }

        let sampling = match cfg.temperature {
            None => Sampling::ArgMax,
            Some(t) => match cfg.top_p {
                None => Sampling::All { temperature: t },
                Some(p) => Sampling::TopP { p, temperature: t },
            },
        };
        let mut sampler = LogitsProcessor::from_sampling(cfg.seed, sampling);

        // Prefill the whole prompt at position 0; `forward` returns `[1, vocab]`.
        let input = Tensor::new(prompt_tokens, &self.device)?.unsqueeze(0)?;
        let mut next = sampler.sample(&self.model.forward(&input, 0)?.squeeze(0)?)?;

        let mut generated: Vec<u32> = Vec::new();
        // Decode one token at a time; `index_pos` is the KV-cache offset, starting
        // just past the prompt.
        for index_pos in (prompt_tokens.len()..).take(cfg.max_new_tokens) {
            if self.eos.contains(&next) {
                break;
            }
            generated.push(next);
            let input = Tensor::new(&[next], &self.device)?.unsqueeze(0)?;
            let logits = self.model.forward(&input, index_pos)?.squeeze(0)?;
            next = sampler.sample(&logits)?;
        }

        self.tokenizer
            .decode(&generated, true)
            .map(|s| s.trim().to_owned())
            .map_err(|e| LocalModelError::Tokenizer(e.to_string()))
    }
}

/// Wrap a user message (and optional system message) in the Qwen `ChatML`
/// template. When `suppress_think` is set (Qwen3), an empty `<think></think>`
/// block is opened and closed after the assistant turn so the model skips its
/// reasoning trace and emits the answer directly — the right default for
/// drafting. Qwen2 ignores it (it has no thinking mode), so it is only added for
/// Qwen3.
fn chatml(system: Option<&str>, user: &str, suppress_think: bool) -> String {
    let system = system.unwrap_or("You are a precise technical writer.");
    let think = if suppress_think {
        "<think>\n\n</think>\n\n"
    } else {
        ""
    };
    format!(
        "<|im_start|>system\n{system}<|im_end|>\n\
         <|im_start|>user\n{user}<|im_end|>\n\
         <|im_start|>assistant\n{think}"
    )
}

#[cfg(test)]
mod tests {
    use super::chatml;

    #[test]
    fn chatml_wraps_system_and_user() {
        let p = chatml(Some("Be terse."), "Hello", false);
        assert!(p.starts_with("<|im_start|>system\nBe terse.<|im_end|>"));
        assert!(p.contains("<|im_start|>user\nHello<|im_end|>"));
        assert!(p.ends_with("<|im_start|>assistant\n"));
    }

    #[test]
    fn chatml_suppresses_qwen3_thinking() {
        // Qwen3: an empty <think></think> block follows the assistant turn so the
        // model answers directly instead of emitting a reasoning trace.
        let p = chatml(None, "Hi", true);
        assert!(p.ends_with("<|im_start|>assistant\n<think>\n\n</think>\n\n"));
        // Qwen2: no thinking block.
        let q = chatml(None, "Hi", false);
        assert!(q.ends_with("<|im_start|>assistant\n"));
    }
}

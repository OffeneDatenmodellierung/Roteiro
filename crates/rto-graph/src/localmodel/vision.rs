//! Offline image *understanding* with a small quantized vision-language model —
//! ADR-0005 Tier B. Moondream2 via candle's `quantized_moondream`, CPU-only,
//! pure-Rust, no network at run time. Turns an image into a short description
//! embedded into `meta.content` for images that OCR alone can't capture
//! (diagrams, charts, photos).
//!
//! Only built with `--features image-vision`.

use std::path::Path;

use candle_core::{DType, Device, Tensor};
use candle_transformers::generation::LogitsProcessor;
use candle_transformers::models::{moondream, quantized_moondream};
use candle_transformers::quantized_var_builder::VarBuilder;
use tokenizers::Tokenizer;

use super::LocalModelError;

/// Local filenames the loader expects under a model directory.
const GGUF_FILE: &str = "model.gguf";
const TOKENIZER_FILE: &str = "tokenizer.json";
/// Moondream's fixed vision input is a 378×378 RGB image.
const IMAGE_SIZE: u32 = 378;
/// Cap on generated description tokens — keep `meta.content` short.
const MAX_NEW_TOKENS: usize = 128;
/// The prompt driving the description.
const PROMPT: &str = "Describe this image concisely.";

/// A loaded quantized vision-language model. Not reused across images (its KV
/// cache is per-generation), so callers load one per image.
pub struct LocalVlm {
    model: quantized_moondream::Model,
    tokenizer: Tokenizer,
    device: Device,
    /// The `<|endoftext|>` token, used as BOS and as a stop token.
    eos: u32,
}

impl LocalVlm {
    /// Load the Moondream GGUF model and its tokenizer from `model_dir` (expects
    /// `model.gguf` + `tokenizer.json`).
    ///
    /// # Errors
    /// Returns [`LocalModelError`] if a file is missing/unreadable, the model
    /// fails to build, or the tokenizer lacks `<|endoftext|>`.
    pub fn load(model_dir: &Path) -> Result<Self, LocalModelError> {
        let device = Device::Cpu;
        let tokenizer = Tokenizer::from_file(model_dir.join(TOKENIZER_FILE))
            .map_err(|e| LocalModelError::Tokenizer(e.to_string()))?;
        let config = moondream::Config::v2();
        let vb = VarBuilder::from_gguf(model_dir.join(GGUF_FILE), &device)?;
        let model = quantized_moondream::Model::new(&config, vb)?;
        let eos = tokenizer.token_to_id("<|endoftext|>").ok_or_else(|| {
            LocalModelError::Tokenizer("moondream tokenizer has no `<|endoftext|>`".to_owned())
        })?;
        Ok(Self {
            model,
            tokenizer,
            device,
            eos,
        })
    }

    /// Produce a short natural-language description of the image in `bytes`.
    ///
    /// # Errors
    /// Returns [`LocalModelError`] if the image cannot be decoded, or a tensor
    /// op / generation / decode step fails.
    pub fn describe(&mut self, bytes: &[u8]) -> Result<String, LocalModelError> {
        let image = self.preprocess(bytes)?;
        let image_embeds = image.unsqueeze(0)?.apply(self.model.vision_encoder())?;

        let prompt = format!("\n\nQuestion: {PROMPT}\n\nAnswer:");
        let encoding = self
            .tokenizer
            .encode(prompt, true)
            .map_err(|e| LocalModelError::Tokenizer(e.to_string()))?;
        let mut tokens = encoding.get_ids().to_vec();
        if tokens.is_empty() {
            return Ok(String::new());
        }

        let mut sampler = LogitsProcessor::new(0, None, None); // greedy
        let mut generated: Vec<u32> = Vec::new();
        for index in 0..MAX_NEW_TOKENS {
            // The first step consumes the whole prompt alongside the image; later
            // steps feed the single previous token (KV cache is internal).
            let context = if index > 0 { 1 } else { tokens.len() };
            let ctxt = &tokens[tokens.len() - context..];
            let input = Tensor::new(ctxt, &self.device)?.unsqueeze(0)?;
            let logits = if index > 0 {
                self.model.text_model.forward(&input)?
            } else {
                let bos = Tensor::new(&[self.eos], &self.device)?.unsqueeze(0)?;
                self.model
                    .text_model
                    .forward_with_img(&bos, &input, &image_embeds)?
            };
            let logits = logits.squeeze(0)?.to_dtype(DType::F32)?;
            let next = sampler.sample(&logits)?;
            tokens.push(next);
            // Stop on EOS or Moondream's `<END>` marker (`[27, 10619, 29]`).
            if next == self.eos || tokens.ends_with(&[27, 10619, 29]) {
                break;
            }
            generated.push(next);
        }

        self.tokenizer
            .decode(&generated, true)
            .map(|s| s.trim().to_owned())
            .map_err(|e| LocalModelError::Tokenizer(e.to_string()))
    }

    /// Decode `bytes` and preprocess to the `(3, 378, 378)` tensor Moondream
    /// expects: RGB, scaled to `[0,1]`, normalised with per-channel mean/std 0.5.
    fn preprocess(&self, bytes: &[u8]) -> Result<Tensor, LocalModelError> {
        let img = image::load_from_memory(bytes)
            .map_err(candle_core::Error::wrap)?
            .resize_to_fill(
                IMAGE_SIZE,
                IMAGE_SIZE,
                image::imageops::FilterType::Triangle,
            )
            .to_rgb8();
        let side = IMAGE_SIZE as usize; // widening cast, always exact
        let data =
            Tensor::from_vec(img.into_raw(), (side, side, 3), &self.device)?.permute((2, 0, 1))?;
        let mean = Tensor::new(&[0.5f32, 0.5, 0.5], &self.device)?.reshape((3, 1, 1))?;
        let std = Tensor::new(&[0.5f32, 0.5, 0.5], &self.device)?.reshape((3, 1, 1))?;
        let out = (data.to_dtype(DType::F32)? / 255.)?
            .broadcast_sub(&mean)?
            .broadcast_div(&std)?;
        Ok(out)
    }
}

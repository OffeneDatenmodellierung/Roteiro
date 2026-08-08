//! A candle-backed BERT sentence embedder for the `inference-local-models` tier.
//!
//! Loads a local sentence-transformer directory (`config.json`,
//! `tokenizer.json`, `model.safetensors`) once, then embeds many texts by
//! running the BERT forward pass, masked mean-pooling the last hidden states,
//! and L2-normalising — producing unit vectors comparable by dot product, just
//! like the hashing embedder. CPU only.

use std::path::Path;

use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config};
use tokenizers::Tokenizer;

/// Errors from loading or running a local model.
#[derive(Debug, thiserror::Error)]
pub enum LocalModelError {
    /// A required model file could not be read.
    #[error("model io error: {0}")]
    Io(#[from] std::io::Error),
    /// `config.json` could not be parsed.
    #[error("model config error: {0}")]
    Config(#[from] serde_json::Error),
    /// The tokenizer could not be loaded or applied.
    #[error("tokenizer error: {0}")]
    Tokenizer(String),
    /// A candle tensor/model operation failed.
    #[error("candle error: {0}")]
    Candle(#[from] candle_core::Error),
}

/// A loaded local embedding model. Reusable across many `embed` calls.
pub struct LocalEmbedder {
    model: BertModel,
    tokenizer: Tokenizer,
    device: Device,
    dim: usize,
}

impl LocalEmbedder {
    /// Load a sentence-transformer model from `model_dir`.
    ///
    /// # Errors
    /// Returns [`LocalModelError`] if any of `config.json`, `tokenizer.json`, or
    /// `model.safetensors` is missing/invalid, or the model fails to build.
    pub fn load(model_dir: &Path) -> Result<Self, LocalModelError> {
        let device = Device::Cpu;

        let config_bytes = std::fs::read(model_dir.join("config.json"))?;
        let config: Config = serde_json::from_slice(&config_bytes)?;
        let dim = config.hidden_size;

        let tokenizer = Tokenizer::from_file(model_dir.join("tokenizer.json"))
            .map_err(|e| LocalModelError::Tokenizer(e.to_string()))?;

        // Read the weights into an owned buffer and use the *safe* loader (the
        // mmap variant is `unsafe`, which the crate forbids). A sentence
        // embedder's weights are ~tens of MB, so buffering is fine.
        let weights = std::fs::read(model_dir.join("model.safetensors"))?;
        let vb = VarBuilder::from_buffered_safetensors(weights, DType::F32, &device)?;
        let model = BertModel::load(vb, &config)?;

        Ok(Self {
            model,
            tokenizer,
            device,
            dim,
        })
    }

    /// The embedding dimensionality of this model.
    #[must_use]
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Embed `text` into a unit vector (masked mean-pooled, L2-normalised).
    ///
    /// # Errors
    /// Returns [`LocalModelError`] if tokenization or the forward pass fails.
    pub fn embed(&self, text: &str) -> Result<Vec<f32>, LocalModelError> {
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(|e| LocalModelError::Tokenizer(e.to_string()))?;
        let ids: Vec<u32> = encoding.get_ids().to_vec();
        let mask: Vec<u32> = encoding.get_attention_mask().to_vec();

        let input_ids = Tensor::new(ids.as_slice(), &self.device)?.unsqueeze(0)?;
        let token_type_ids = input_ids.zeros_like()?;
        let attention_mask = Tensor::new(mask.as_slice(), &self.device)?.unsqueeze(0)?;

        // Last hidden states: [1, seq_len, hidden].
        let hidden = self
            .model
            .forward(&input_ids, &token_type_ids, Some(&attention_mask))?;

        // Masked mean-pool over the sequence dimension.
        let mask_f = attention_mask.to_dtype(DType::F32)?.unsqueeze(2)?; // [1, seq, 1]
        let summed = hidden.broadcast_mul(&mask_f)?.sum(1)?; // [1, hidden]
        let counts = mask_f.sum(1)?; // [1, 1]
        let mean = summed.broadcast_div(&counts)?;

        // L2-normalise so the result is comparable by dot product.
        let norm = mean.sqr()?.sum_keepdim(1)?.sqrt()?;
        let normalized = mean.broadcast_div(&norm)?;

        Ok(normalized.squeeze(0)?.to_vec1::<f32>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::{LocalEmbedder, LocalModelError};

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("roteiro-embed-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        dir
    }

    #[test]
    fn load_missing_dir_is_io_error() {
        // No config.json to read → an IO error, not a panic.
        let dir = tmp("missing");
        assert!(matches!(
            LocalEmbedder::load(&dir),
            Err(LocalModelError::Io(_))
        ));
    }

    #[test]
    fn load_invalid_config_is_config_error() {
        let dir = tmp("badcfg");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("config.json"), b"this is not json").expect("write");
        assert!(matches!(
            LocalEmbedder::load(&dir),
            Err(LocalModelError::Config(_))
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_missing_tokenizer_after_valid_config_errors() {
        // A syntactically valid, minimal BERT config, but no tokenizer.json →
        // the load fails at the tokenizer step rather than succeeding.
        let dir = tmp("notokenizer");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(
            dir.join("config.json"),
            br#"{"hidden_size":16,"num_hidden_layers":1,"num_attention_heads":1,"intermediate_size":16,"vocab_size":8,"max_position_embeddings":8,"type_vocab_size":2}"#,
        )
        .expect("write");
        assert!(LocalEmbedder::load(&dir).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}

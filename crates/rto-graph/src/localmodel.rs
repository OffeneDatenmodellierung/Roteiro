//! Pluggable local embedding/generative models — the candle-backed loaders of
//! ADR-0003's `inference-local-models` tier.
//!
//! The candle-free parts (the model **registry**, per-platform variants, the
//! on-disk store layout, and SHA-256 verification) live in [`crate::models`], so
//! they can be shared by non-candle model tiers (e.g. OCR). This module holds
//! only the candle loaders: the sentence-transformer [`LocalEmbedder`] and the
//! GGUF instruct [`LocalGenerator`].
//!
//! Only built with `--features inference-local-models`.

mod embedder;
mod generator;
pub use embedder::{LocalEmbedder, LocalModelError};
pub use generator::{GenConfig, LocalGenerator};

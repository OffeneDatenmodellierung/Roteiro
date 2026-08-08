//! Pluggable local embedding models — ADR-0003's `inference-local-models` tier.
//!
//! This module holds the parts that do **not** touch the network: a small
//! in-binary **registry** of downloadable embedding models with **per-platform
//! variants**, host-aware variant selection, the on-disk **model store** layout
//! (`~/.roteiro/models/<name>/`), and SHA-256 verification of downloaded files.
//! The actual consent-gated download lives in the `roteiro` binary (it needs a
//! TTY and a network client); the candle-backed embedder is [`LocalEmbedder`].
//!
//! Only built with `--features inference-local-models`.

use std::path::PathBuf;

mod embedder;
pub use embedder::{LocalEmbedder, LocalModelError};

/// A host platform that a model may have a tuned variant for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// Apple Silicon (macOS / aarch64) — prefers Metal/MLX-oriented builds.
    MacosArm64,
    /// Everything else — the standard build (CPU by default).
    Standard,
}

impl Platform {
    /// The platform Roteiro is running on.
    #[must_use]
    pub fn host() -> Self {
        if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            Self::MacosArm64
        } else {
            Self::Standard
        }
    }

    /// Stable token for this platform.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MacosArm64 => "macos-arm64",
            Self::Standard => "standard",
        }
    }
}

/// One file that makes up a model variant, with its verification hash.
#[derive(Debug, Clone, Copy)]
pub struct ModelFile {
    /// Filename stored under the model directory (e.g. `model.safetensors`).
    pub name: &'static str,
    /// URL to fetch it from.
    pub url: &'static str,
    /// Lowercase hex SHA-256 the downloaded bytes must match.
    pub sha256: &'static str,
}

/// A platform-specific set of files for a model.
#[derive(Debug, Clone, Copy)]
pub struct ModelVariant {
    /// Which platform this variant targets.
    pub platform: Platform,
    /// The files to fetch (config, tokenizer, weights, …).
    pub files: &'static [ModelFile],
}

/// A model the user can pull and use for inference.
#[derive(Debug, Clone, Copy)]
pub struct ModelSpec {
    /// Unique registry name (e.g. `all-minilm-l6-v2`).
    pub name: &'static str,
    /// Embedding dimensionality.
    pub dim: usize,
    /// SPDX licence of the model weights.
    pub licence: &'static str,
    /// One-line description.
    pub description: &'static str,
    /// Approximate download size, in mebibytes, for the consent prompt.
    pub size_mib: u32,
    /// Available per-platform variants (at least one `Standard`).
    pub variants: &'static [ModelVariant],
}

impl ModelSpec {
    /// The variant best matching the host: an exact platform match if present,
    /// otherwise the `Standard` variant.
    #[must_use]
    pub fn variant_for(&self, platform: Platform) -> Option<&ModelVariant> {
        self.variants
            .iter()
            .find(|v| v.platform == platform)
            .or_else(|| {
                self.variants
                    .iter()
                    .find(|v| v.platform == Platform::Standard)
            })
    }
}

/// The built-in registry of known embedding models.
///
/// Kept intentionally small; entries are curated so `pull` can suggest the right
/// per-platform artifact and verify its checksum. Larger/re-encoded variants
/// (e.g. Apple MLX builds) are added here as `MacosArm64` variants when they
/// exist — until then the host resolves to the `Standard` variant.
pub const REGISTRY: &[ModelSpec] = &[ModelSpec {
    name: "all-minilm-l6-v2",
    dim: 384,
    licence: "Apache-2.0",
    description: "sentence-transformers/all-MiniLM-L6-v2 — small, fast general-purpose embeddings",
    size_mib: 90,
    variants: &[ModelVariant {
        platform: Platform::Standard,
        files: &[
            ModelFile {
                name: "config.json",
                url: "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/config.json",
                sha256: "",
            },
            ModelFile {
                name: "tokenizer.json",
                url: "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/tokenizer.json",
                sha256: "",
            },
            ModelFile {
                name: "model.safetensors",
                url: "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/model.safetensors",
                sha256: "",
            },
        ],
    }],
}];

/// Look up a model spec by name.
#[must_use]
pub fn find(name: &str) -> Option<&'static ModelSpec> {
    REGISTRY.iter().find(|m| m.name == name)
}

/// Pure resolution of the model-store root from a `ROTEIRO_HOME` override and a
/// home directory. Factored out so it is testable without mutating the process
/// environment.
fn store_root_from(roteiro_home: Option<PathBuf>, home: Option<PathBuf>) -> PathBuf {
    if let Some(dir) = roteiro_home {
        return dir.join("models");
    }
    home.unwrap_or_else(|| PathBuf::from("."))
        .join(".roteiro")
        .join("models")
}

/// Root of the user-level model store (`~/.roteiro/models`), honouring
/// `ROTEIRO_HOME` if set (for tests and non-standard layouts).
#[must_use]
pub fn store_root() -> PathBuf {
    store_root_from(
        std::env::var_os("ROTEIRO_HOME").map(PathBuf::from),
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from),
    )
}

/// Directory a given model is (or would be) stored in.
#[must_use]
pub fn model_dir(name: &str) -> PathBuf {
    store_root().join(name)
}

/// Whether every file of `variant` is already present in the model directory.
#[must_use]
pub fn is_installed(name: &str, variant: &ModelVariant) -> bool {
    let dir = model_dir(name);
    variant.files.iter().all(|f| dir.join(f.name).exists())
}

/// Lowercase hex SHA-256 of `bytes`.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Verify `bytes` against an expected lowercase-hex SHA-256. An empty
/// `expected` means "no hash pinned" and always passes (registry entries whose
/// checksum has not yet been recorded).
#[must_use]
pub fn verify_sha256(bytes: &[u8], expected: &str) -> bool {
    expected.is_empty() || sha256_hex(bytes).eq_ignore_ascii_case(expected)
}

/// The directory to hand [`LocalEmbedder::load`] for `name`, if the model looks
/// installed (its directory exists).
#[must_use]
pub fn installed_dir(name: &str) -> Option<PathBuf> {
    let dir = model_dir(name);
    dir.is_dir().then_some(dir)
}

/// Path helper: ensure the model directory exists, returning it.
///
/// # Errors
/// Returns [`std::io::Error`] if the directory cannot be created.
pub fn ensure_model_dir(name: &str) -> std::io::Result<PathBuf> {
    let dir = model_dir(name);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::{Platform, REGISTRY, find, sha256_hex, store_root, verify_sha256};
    use std::path::Path;

    #[test]
    fn registry_entries_are_well_formed() {
        assert!(!REGISTRY.is_empty());
        for spec in REGISTRY {
            assert!(!spec.name.is_empty());
            assert!(spec.dim > 0);
            assert!(!spec.variants.is_empty());
            // Every model must have a Standard variant so any host resolves.
            assert!(
                spec.variants
                    .iter()
                    .any(|v| v.platform == Platform::Standard),
                "{} needs a Standard variant",
                spec.name,
            );
            let v = spec.variant_for(Platform::host()).expect("host variant");
            assert!(!v.files.is_empty());
        }
    }

    #[test]
    fn variant_selection_falls_back_to_standard() {
        let spec = find("all-minilm-l6-v2").expect("registered");
        // MiniLM only ships a Standard variant, so both hosts resolve to it.
        let mac = spec.variant_for(Platform::MacosArm64).expect("mac");
        let std = spec.variant_for(Platform::Standard).expect("std");
        assert_eq!(mac.platform, Platform::Standard);
        assert_eq!(std.platform, Platform::Standard);
    }

    #[test]
    fn platform_host_is_stable() {
        let p = Platform::host();
        assert!(matches!(p, Platform::MacosArm64 | Platform::Standard));
        assert!(!p.as_str().is_empty());
    }

    #[test]
    fn store_root_resolution() {
        use super::store_root_from;
        use std::path::PathBuf;
        // ROTEIRO_HOME override wins.
        assert_eq!(
            store_root_from(
                Some(PathBuf::from("/opt/rt")),
                Some(PathBuf::from("/home/u"))
            ),
            Path::new("/opt/rt/models"),
        );
        // Else falls back to <home>/.roteiro/models.
        assert_eq!(
            store_root_from(None, Some(PathBuf::from("/home/u"))),
            Path::new("/home/u/.roteiro/models"),
        );
        // The live resolver returns a `models`-suffixed path.
        assert!(store_root().ends_with("models"));
    }

    #[test]
    fn sha256_and_verify() {
        // Known vector: SHA-256("abc").
        let want = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(sha256_hex(b"abc"), want);
        assert!(verify_sha256(b"abc", want));
        assert!(verify_sha256(b"abc", &want.to_uppercase()));
        assert!(!verify_sha256(b"abc", "00"));
        // Empty expected = unpinned, always passes.
        assert!(verify_sha256(b"anything", ""));
    }
}

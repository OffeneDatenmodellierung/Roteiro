//! Local model **registry** and on-disk store — the candle-free parts of the
//! pluggable-models machinery (ADR-0003), shared by every model tier.
//!
//! Holds a small in-binary registry of downloadable models with **per-platform
//! variants**, host-aware variant selection, the model-store layout
//! (`~/.roteiro/models/<name>/`), and SHA-256 verification. It touches neither
//! candle nor the network: the candle-backed loaders live in
//! [`crate::localmodel`] (feature `inference-local-models`), and the
//! consent-gated download lives in the `roteiro` binary. This module is compiled
//! whenever any model tier is enabled (feature `models`), so an OCR build
//! (feature `image-ocr`) can reuse the registry and `roteiro model pull` without
//! pulling candle.

use std::path::PathBuf;

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

/// What a registry model is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelKind {
    /// A text-embedding model (the inference layer, ADR-0003).
    Embedding,
    /// A generative instruct model (spec/blueprint drafting, ADR-0004 Tier 1).
    Generative,
    /// An OCR model set for image text extraction (ADR-0005 Tier A).
    Ocr,
}

impl ModelKind {
    /// Stable token naming the model's *section* in the registry.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Embedding => "embedding",
            Self::Generative => "generative",
            Self::Ocr => "ocr",
        }
    }
}

/// The rough hardware a model is aimed at — an opinionated curation so `roteiro
/// model list` can recommend a pick per section for a machine's resources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceTier {
    /// Runs comfortably on any laptop (low RAM/CPU).
    Low,
    /// Wants a moderate machine (~16 GB).
    Mid,
    /// Aimed at a workstation (e.g. a 64 GB Apple-silicon machine).
    High,
}

impl ResourceTier {
    /// Stable token for this tier.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Mid => "mid",
            Self::High => "high",
        }
    }
}

/// A model the user can pull and use.
#[derive(Debug, Clone, Copy)]
pub struct ModelSpec {
    /// Unique registry name (e.g. `all-minilm-l6-v2`).
    pub name: &'static str,
    /// What the model is for.
    pub kind: ModelKind,
    /// The hardware tier this pick is curated for within its section.
    pub tier: ResourceTier,
    /// Embedding dimensionality (0 for generative models).
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
pub const REGISTRY: &[ModelSpec] = &[
    ModelSpec {
        name: "all-minilm-l6-v2",
        kind: ModelKind::Embedding,
        tier: ResourceTier::Low,
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
    },
    // Mid embedding tier: a stronger general-purpose BERT sentence-transformer,
    // loadable by the same `LocalEmbedder` as MiniLM (standard BERT arch).
    ModelSpec {
        name: "bge-base-en-v1.5",
        kind: ModelKind::Embedding,
        tier: ResourceTier::Mid,
        dim: 768,
        licence: "MIT",
        description: "BAAI/bge-base-en-v1.5 — stronger English embeddings (768-d)",
        size_mib: 420,
        variants: &[ModelVariant {
            platform: Platform::Standard,
            files: &[
                ModelFile {
                    name: "config.json",
                    url: "https://huggingface.co/BAAI/bge-base-en-v1.5/resolve/main/config.json",
                    sha256: "bc00af31a4a31b74040d73370aa83b62da34c90b75eb77bfa7db039d90abd591",
                },
                ModelFile {
                    name: "tokenizer.json",
                    url: "https://huggingface.co/BAAI/bge-base-en-v1.5/resolve/main/tokenizer.json",
                    sha256: "d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66",
                },
                ModelFile {
                    name: "model.safetensors",
                    url: "https://huggingface.co/BAAI/bge-base-en-v1.5/resolve/main/model.safetensors",
                    sha256: "c7c1988aae201f80cf91a5dbbd5866409503b89dcaba877ca6dba7dd0a5167d7",
                },
            ],
        }],
    },
    // High embedding tier: the strongest BERT sentence-transformer our loader
    // handles (bigger/better embeddings than this are decoder-based — e.g.
    // gte-Qwen2-7B — and need a future decoder-embedding loader).
    ModelSpec {
        name: "bge-large-en-v1.5",
        kind: ModelKind::Embedding,
        tier: ResourceTier::High,
        dim: 1024,
        licence: "MIT",
        description: "BAAI/bge-large-en-v1.5 — strongest BERT embeddings we load (1024-d)",
        size_mib: 1340,
        variants: &[ModelVariant {
            platform: Platform::Standard,
            files: &[
                ModelFile {
                    name: "config.json",
                    url: "https://huggingface.co/BAAI/bge-large-en-v1.5/resolve/main/config.json",
                    sha256: "446712fac367857b4b1302762fe1cd7bfa8b3c4b77b4dc5d77c4025407660896",
                },
                ModelFile {
                    name: "tokenizer.json",
                    url: "https://huggingface.co/BAAI/bge-large-en-v1.5/resolve/main/tokenizer.json",
                    sha256: "d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66",
                },
                ModelFile {
                    name: "model.safetensors",
                    url: "https://huggingface.co/BAAI/bge-large-en-v1.5/resolve/main/model.safetensors",
                    sha256: "45e1954914e29bd74080e6c1510165274ff5279421c89f76c418878732f64ae7",
                },
            ],
        }],
    },
    // ADR-0004 Tier 1: Apache-2.0 Qwen3 instruct GGUFs for offline spec/blueprint
    // drafting, curated low/mid/high. Stored as `model.gguf` + its `tokenizer.json`
    // (which lives in the base instruct repo, not the GGUF repo — all Qwen3 sizes
    // share one tokenizer). Loaded via the GGUF-arch-dispatching `LocalGenerator`
    // (Qwen2 GGUFs still load too). The low pick is the `spec draft` default.
    ModelSpec {
        name: "qwen3-0.6b",
        kind: ModelKind::Generative,
        tier: ResourceTier::Low,
        dim: 0,
        licence: "Apache-2.0",
        description: "Qwen3-0.6B (Q4_K_M GGUF) — tiny offline instruct model, the `spec draft` default",
        size_mib: 380,
        variants: &[ModelVariant {
            platform: Platform::Standard,
            files: &[
                ModelFile {
                    name: "model.gguf",
                    url: "https://huggingface.co/unsloth/Qwen3-0.6B-GGUF/resolve/main/Qwen3-0.6B-Q4_K_M.gguf",
                    sha256: "ac2d97712095a558e31573f62f466a3f9d93990898b0ec79d7c974c1780d524a",
                },
                ModelFile {
                    name: "tokenizer.json",
                    url: "https://huggingface.co/Qwen/Qwen3-0.6B/resolve/main/tokenizer.json",
                    sha256: "aeb13307a71acd8fe81861d94ad54ab689df773318809eed3cbe794b4492dae4",
                },
            ],
        }],
    },
    ModelSpec {
        name: "qwen3-8b",
        kind: ModelKind::Generative,
        tier: ResourceTier::Mid,
        dim: 0,
        licence: "Apache-2.0",
        description: "Qwen3-8B (Q4_K_M GGUF) — stronger offline drafting on a ~16 GB machine",
        size_mib: 4795,
        variants: &[ModelVariant {
            platform: Platform::Standard,
            files: &[
                ModelFile {
                    name: "model.gguf",
                    url: "https://huggingface.co/Qwen/Qwen3-8B-GGUF/resolve/main/Qwen3-8B-Q4_K_M.gguf",
                    sha256: "d98cdcbd03e17ce47681435b5150e34c1417f50b5c0019dd560e4882c5745785",
                },
                ModelFile {
                    name: "tokenizer.json",
                    url: "https://huggingface.co/Qwen/Qwen3-8B/resolve/main/tokenizer.json",
                    sha256: "aeb13307a71acd8fe81861d94ad54ab689df773318809eed3cbe794b4492dae4",
                },
            ],
        }],
    },
    ModelSpec {
        name: "qwen3-32b",
        kind: ModelKind::Generative,
        tier: ResourceTier::High,
        dim: 0,
        licence: "Apache-2.0",
        description: "Qwen3-32B (Q4_K_M GGUF) — best offline drafting on a workstation (~20 GB)",
        size_mib: 18845,
        variants: &[ModelVariant {
            platform: Platform::Standard,
            files: &[
                ModelFile {
                    name: "model.gguf",
                    url: "https://huggingface.co/Qwen/Qwen3-32B-GGUF/resolve/main/Qwen3-32B-Q4_K_M.gguf",
                    sha256: "efd971561896866f0e910cce52761ca77b1b138090c7f15fe284676d57d1f689",
                },
                ModelFile {
                    name: "tokenizer.json",
                    url: "https://huggingface.co/Qwen/Qwen3-32B/resolve/main/tokenizer.json",
                    sha256: "aeb13307a71acd8fe81861d94ad54ab689df773318809eed3cbe794b4492dae4",
                },
            ],
        }],
    },
    // ADR-0005 Tier A: the `ocrs` pure-Rust OCR model set (detection +
    // recognition, `.rten` format). Weights trace to open datasets (HierText,
    // CC-BY-SA-4.0); the `ocrs` engine crate is MIT/Apache-2.0. Checksums are
    // pinned so a model change invalidates cached image facts (see extract.rs).
    ModelSpec {
        name: "ocrs-text",
        kind: ModelKind::Ocr,
        tier: ResourceTier::Low,
        dim: 0,
        licence: "CC-BY-SA-4.0",
        description: "ocrs text detection + recognition (pure-Rust OCR for `image-ocr`)",
        size_mib: 12,
        variants: &[ModelVariant {
            platform: Platform::Standard,
            files: &[
                ModelFile {
                    name: "text-detection.rten",
                    url: "https://ocrs-models.s3-accelerate.amazonaws.com/text-detection.rten",
                    sha256: "f15cfb56bd02c4bf478a20343986504a1f01e1665c2b3a0ad66340f054b1b5ca",
                },
                ModelFile {
                    name: "text-recognition.rten",
                    url: "https://ocrs-models.s3-accelerate.amazonaws.com/text-recognition.rten",
                    sha256: "e484866d4cce403175bd8d00b128feb08ab42e208de30e42cd9889d8f1735a6e",
                },
            ],
        }],
    },
];

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
    use super::{
        ModelKind, Platform, REGISTRY, ResourceTier, find, sha256_hex, store_root, verify_sha256,
    };
    use std::path::Path;

    #[test]
    fn registry_entries_are_well_formed() {
        assert!(!REGISTRY.is_empty());
        for spec in REGISTRY {
            assert!(!spec.name.is_empty());
            // Embedding models carry a dimension; generative models do not.
            assert_eq!(
                spec.dim > 0,
                spec.kind == ModelKind::Embedding,
                "{}",
                spec.name
            );
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
            assert!(!spec.tier.as_str().is_empty());
        }
    }

    #[test]
    fn every_section_has_a_low_tier_floor() {
        // The curated matrix must offer a runs-anywhere pick for each section, so
        // `roteiro model list` always has a low-resource recommendation.
        for kind in [ModelKind::Embedding, ModelKind::Generative, ModelKind::Ocr] {
            assert!(
                REGISTRY
                    .iter()
                    .any(|s| s.kind == kind && s.tier == ResourceTier::Low),
                "section {} needs a Low-tier entry",
                kind.as_str(),
            );
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

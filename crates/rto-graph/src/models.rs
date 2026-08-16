//! Local model **registry** and on-disk store (ADR-0003), shared by every model
//! tier.
//!
//! Holds a small in-binary registry of downloadable models with **per-platform
//! variants**, host-aware variant selection, the model-store layout
//! (`~/.roteiro/models/<name>/`), and SHA-256 verification. It only lists,
//! resolves, and verifies models — it touches neither an inference engine nor the
//! network. The actual loaders are feature-specific: the GGUF tiers (embedding /
//! generative / vision / audio) load via the llama.cpp core (`rto-llama`, feature
//! `inference-local-models`), while the OCR `.rten` set loads via `ocrs`/`rten`
//! (feature `image-ocr`); the consent-gated download lives in the `roteiro`
//! binary. This module is compiled whenever any model tier is enabled (feature
//! `models`), so even an OCR-only build can reuse the registry and `roteiro model
//! pull` without pulling the llama.cpp engine.

use std::path::{Path, PathBuf};

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
    /// A vision-language model for image *understanding* (ADR-0005 Tier B).
    Vision,
    /// An audio-capable multimodal model for speech transcription (Stage 18;
    /// served through the same llama.cpp `mtmd` path as [`Self::Vision`], with an
    /// audio projector instead of a vision one).
    Audio,
}

impl ModelKind {
    /// Stable token naming the model's *section* in the registry.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Embedding => "embedding",
            Self::Generative => "generative",
            Self::Ocr => "ocr",
            Self::Vision => "vision",
            Self::Audio => "audio",
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

/// The specialisation of a generative model — a sub-label within the generative
/// section so `model list` can distinguish general drafting from coding and
/// reasoning models (Stage 20). Non-generative models are [`ModelRole::None`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelRole {
    /// Not a generative model (embedding / OCR / vision).
    None,
    /// General instruction-following (chat, spec/blueprint drafting). Qwen3's
    /// thinking mode makes these reasoning-capable out of the box.
    Instruct,
    /// Code-specialised (completion, refactoring, code Q&A).
    Coding,
    /// Reasoning-specialised (long chain-of-thought before answering).
    Reasoning,
}

impl ModelRole {
    /// Stable token for this role (`instruct` | `coding` | `reasoning`), or `None`
    /// for a non-generative model.
    #[must_use]
    pub fn as_str(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Instruct => Some("instruct"),
            Self::Coding => Some("coding"),
            Self::Reasoning => Some("reasoning"),
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
    /// For generative models, the specialisation (instruct/coding/reasoning);
    /// [`ModelRole::None`] for non-generative models.
    pub role: ModelRole,
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
    // Embedding models are **GGUF** (llama.cpp via rto-llama): they serve
    // `/v1/embeddings` and back `roteiro infer --model` through the shared engine
    // — no candle. The GGUF embeds its own tokenizer, so only `model.gguf` is
    // needed. `bge-small` is the low-tier default (below); bge-base/large are the
    // mid/high picks.
    ModelSpec {
        name: "bge-base-en-v1.5",
        kind: ModelKind::Embedding,
        role: ModelRole::None,
        tier: ResourceTier::Mid,
        dim: 768,
        licence: "MIT",
        description: "BAAI/bge-base-en-v1.5 (F16 GGUF) — stronger English embeddings (768-d)",
        size_mib: 209,
        variants: &[ModelVariant {
            platform: Platform::Standard,
            files: &[ModelFile {
                name: "model.gguf",
                url: "https://huggingface.co/CompendiumLabs/bge-base-en-v1.5-gguf/resolve/main/bge-base-en-v1.5-f16.gguf",
                sha256: "88360fdf8521af0ac08d43818bd272da679ab97c685d9b273c48efd01a4187c2",
            }],
        }],
    },
    ModelSpec {
        name: "bge-large-en-v1.5",
        kind: ModelKind::Embedding,
        role: ModelRole::None,
        tier: ResourceTier::High,
        dim: 1024,
        licence: "MIT",
        description: "BAAI/bge-large-en-v1.5 (F16 GGUF) — strongest English embeddings (1024-d)",
        size_mib: 639,
        variants: &[ModelVariant {
            platform: Platform::Standard,
            files: &[ModelFile {
                name: "model.gguf",
                url: "https://huggingface.co/CompendiumLabs/bge-large-en-v1.5-gguf/resolve/main/bge-large-en-v1.5-f16.gguf",
                sha256: "3379a0e9cea28fc6d7136df8ea7a88ef99ccce5963b9a6f7af9609997be762e3",
            }],
        }],
    },
    // The low-tier embedding default.
    ModelSpec {
        name: "bge-small-en-v1.5-gguf",
        kind: ModelKind::Embedding,
        role: ModelRole::None,
        tier: ResourceTier::Low,
        dim: 384,
        licence: "MIT",
        description: "BAAI/bge-small-en-v1.5 (F16 GGUF) — small English embeddings (384-d), served via llama.cpp",
        size_mib: 65,
        variants: &[ModelVariant {
            platform: Platform::Standard,
            files: &[ModelFile {
                name: "model.gguf",
                url: "https://huggingface.co/CompendiumLabs/bge-small-en-v1.5-gguf/resolve/main/bge-small-en-v1.5-f16.gguf",
                sha256: "f0b2fef971e8366438bfd2d9aefea1b0115919389448806d290237f638bae999",
            }],
        }],
    },
    // ADR-0004 Tier 1: Apache-2.0 Qwen3 instruct GGUFs for offline spec/blueprint
    // drafting, curated low/mid/high. GGUF-only — the embedded tokenizer serves
    // llama.cpp, so no separate `tokenizer.json` is needed. The low pick is the
    // `spec draft` default.
    ModelSpec {
        name: "qwen3-0.6b",
        kind: ModelKind::Generative,
        role: ModelRole::Instruct,
        tier: ResourceTier::Low,
        dim: 0,
        licence: "Apache-2.0",
        description: "Qwen3-0.6B (Q4_K_M GGUF) — tiny offline instruct model, the `spec draft` default",
        size_mib: 380,
        variants: &[ModelVariant {
            platform: Platform::Standard,
            files: &[ModelFile {
                name: "model.gguf",
                url: "https://huggingface.co/unsloth/Qwen3-0.6B-GGUF/resolve/main/Qwen3-0.6B-Q4_K_M.gguf",
                sha256: "ac2d97712095a558e31573f62f466a3f9d93990898b0ec79d7c974c1780d524a",
            }],
        }],
    },
    ModelSpec {
        name: "qwen3-8b",
        kind: ModelKind::Generative,
        role: ModelRole::Instruct,
        tier: ResourceTier::Mid,
        dim: 0,
        licence: "Apache-2.0",
        description: "Qwen3-8B (Q4_K_M GGUF) — stronger offline drafting on a ~16 GB machine",
        size_mib: 4795,
        variants: &[ModelVariant {
            platform: Platform::Standard,
            files: &[ModelFile {
                name: "model.gguf",
                url: "https://huggingface.co/Qwen/Qwen3-8B-GGUF/resolve/main/Qwen3-8B-Q4_K_M.gguf",
                sha256: "d98cdcbd03e17ce47681435b5150e34c1417f50b5c0019dd560e4882c5745785",
            }],
        }],
    },
    ModelSpec {
        name: "qwen3-32b",
        kind: ModelKind::Generative,
        role: ModelRole::Instruct,
        tier: ResourceTier::High,
        dim: 0,
        licence: "Apache-2.0",
        description: "Qwen3-32B (Q4_K_M GGUF) — best offline drafting, for a workstation",
        size_mib: 18845,
        variants: &[ModelVariant {
            platform: Platform::Standard,
            files: &[ModelFile {
                name: "model.gguf",
                url: "https://huggingface.co/Qwen/Qwen3-32B-GGUF/resolve/main/Qwen3-32B-Q4_K_M.gguf",
                sha256: "efd971561896866f0e910cce52761ca77b1b138090c7f15fe284676d57d1f689",
            }],
        }],
    },
    // The current strongest offline instruct pick for a workstation. Qwen3.8-27B
    // is a dense 27B declaring llama.cpp arch `qwen35`, which the pinned
    // `llama-cpp-2` 0.1.154 registers (`LLM_ARCH_QWEN35` in its vendored
    // `llama-arch.cpp`) — verified by loading this exact file with this repo's
    // own build and getting a `<tool_call>` for a graph tool, not merely a
    // successful load. A model Roteiro serves that cannot call
    // `search`/`explain`/`path`/`debt` is much less useful to it.
    //
    // **`ggml-org` rather than `unsloth` deliberately.** Unsloth bundles the
    // multi-token-prediction tensors inside the main GGUF, and on this build they
    // are allocated whether or not the head is used — roughly 0.3–0.5 GB resident
    // for something Roteiro never runs. `ggml-org` ships MTP as separate
    // `mtp-*.gguf` files, so *not* listing them here is the whole saving.
    //
    // **Q4_K_M rather than Q5_K_M deliberately.** Generation on Metal is
    // bandwidth-bound, so Q5 costs ~13–15% per token for the life of the model,
    // against a quality difference not observable at 27B.
    //
    // The repo also publishes `mmproj-*.gguf` (it is a vision-language model
    // upstream). Those are deliberately not listed: this entry is the *text*
    // generative tier, and a Vision-tier entry could add the projector later.
    ModelSpec {
        name: "qwen3.8-27b",
        kind: ModelKind::Generative,
        role: ModelRole::Instruct,
        tier: ResourceTier::High,
        dim: 0,
        licence: "Apache-2.0",
        description: "Qwen3.8-27B (Q4_K_M GGUF) — strongest offline instruct pick, tool-calling, for a workstation",
        size_mib: 18095,
        variants: &[ModelVariant {
            platform: Platform::Standard,
            files: &[ModelFile {
                name: "model.gguf",
                url: "https://huggingface.co/ggml-org/Qwen3.8-27B-GGUF/resolve/main/Qwen3.8-27B-Q4_K_M.gguf",
                // Measured over the downloaded file: Hugging Face publishes no
                // SHA-256 for it, so this can come from nowhere else.
                sha256: "31629f53165ab6a7dad8c9847dcfd1fdf55829dac1e6e748f4a68581b0033d34",
            }],
        }],
    },
    // Stage 20: opt-in coding + reasoning generative models for local use and
    // serving (ADR-0006). GGUF-only (the embedded tokenizer serves llama.cpp — no
    // separate `tokenizer.json`); `role` distinguishes them from the general
    // Qwen3 instruct picks in `model list`. Off by default.
    ModelSpec {
        name: "qwen2.5-coder-3b",
        kind: ModelKind::Generative,
        role: ModelRole::Coding,
        tier: ResourceTier::Mid,
        dim: 0,
        licence: "Apache-2.0",
        description: "Qwen2.5-Coder-3B-Instruct (Q4_K_M GGUF) — code completion/Q&A, served via llama.cpp",
        size_mib: 1841,
        variants: &[ModelVariant {
            platform: Platform::Standard,
            files: &[ModelFile {
                name: "model.gguf",
                url: "https://huggingface.co/bartowski/Qwen2.5-Coder-3B-Instruct-GGUF/resolve/main/Qwen2.5-Coder-3B-Instruct-Q4_K_M.gguf",
                sha256: "3da3afe6cf5c674ac195803ea0dd6fee7e1c228c2105c1ce8c66890d1d4ab460",
            }],
        }],
    },
    ModelSpec {
        name: "qwen3-coder-30b-a3b",
        kind: ModelKind::Generative,
        role: ModelRole::Coding,
        tier: ResourceTier::High,
        dim: 0,
        licence: "Apache-2.0",
        description: "Qwen3-Coder-30B-A3B-Instruct (Q4_K_M GGUF) — 30B MoE coder (3B active), for a workstation",
        size_mib: 17697,
        variants: &[ModelVariant {
            platform: Platform::Standard,
            files: &[ModelFile {
                name: "model.gguf",
                url: "https://huggingface.co/unsloth/Qwen3-Coder-30B-A3B-Instruct-GGUF/resolve/main/Qwen3-Coder-30B-A3B-Instruct-Q4_K_M.gguf",
                sha256: "fadc3e5f8d42bf7e894a785b05082e47daee4df26680389817e2093056f088ad",
            }],
        }],
    },
    ModelSpec {
        name: "deepseek-r1-distill-qwen-1.5b",
        kind: ModelKind::Generative,
        role: ModelRole::Reasoning,
        tier: ResourceTier::Low,
        dim: 0,
        licence: "MIT",
        description: "DeepSeek-R1-Distill-Qwen-1.5B (Q4_K_M GGUF) — small reasoning model, served via llama.cpp",
        size_mib: 1066,
        variants: &[ModelVariant {
            platform: Platform::Standard,
            files: &[ModelFile {
                name: "model.gguf",
                url: "https://huggingface.co/bartowski/DeepSeek-R1-Distill-Qwen-1.5B-GGUF/resolve/main/DeepSeek-R1-Distill-Qwen-1.5B-Q4_K_M.gguf",
                sha256: "1741e5b2d062b07acf048bf0d2c514dadf2a48f94e2b4aa0cfe069af3838ee2f",
            }],
        }],
    },
    // ADR-0005 Tier A: the `ocrs` pure-Rust OCR model set (detection +
    // recognition, `.rten` format). Weights trace to open datasets (HierText,
    // CC-BY-SA-4.0); the `ocrs` engine crate is MIT/Apache-2.0. Checksums are
    // pinned so a model change invalidates cached image facts (see extract.rs).
    ModelSpec {
        name: "ocrs-text",
        kind: ModelKind::Ocr,
        role: ModelRole::None,
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
    // Vision-language GGUF for the llama.cpp serving path (ADR-0006 multimodal
    // `/v1/chat/completions`). Ships a base `model.gguf` plus its multimodal
    // projector `mmproj.gguf`; served via llama.cpp `mtmd`. SmolVLM-500M is
    // llama.cpp's small reference multimodal model — image description *and*
    // reading text in an image (the OCR use case is just a prompt).
    ModelSpec {
        name: "smolvlm-500m-gguf",
        kind: ModelKind::Vision,
        role: ModelRole::None,
        tier: ResourceTier::Low,
        dim: 0,
        licence: "Apache-2.0",
        description: "SmolVLM-500M-Instruct (Q8_0 GGUF + mmproj) — small vision-language model served via llama.cpp",
        size_mib: 520,
        variants: &[ModelVariant {
            platform: Platform::Standard,
            files: &[
                ModelFile {
                    name: "model.gguf",
                    url: "https://huggingface.co/ggml-org/SmolVLM-500M-Instruct-GGUF/resolve/main/SmolVLM-500M-Instruct-Q8_0.gguf",
                    sha256: "9d4612de6a42214499e301494a3ecc2be0abdd9de44e663bda63f1152fad1bf4",
                },
                ModelFile {
                    name: "mmproj.gguf",
                    url: "https://huggingface.co/ggml-org/SmolVLM-500M-Instruct-GGUF/resolve/main/mmproj-SmolVLM-500M-Instruct-Q8_0.gguf",
                    sha256: "d1eb8b6b23979205fdf63703ed10f788131a3f812c7b1f72e0119d5d81295150",
                },
            ],
        }],
    },
    // Stage 18: audio transcription via an audio-capable llama.cpp `mtmd` model —
    // the same multimodal path as vision, with an *audio* projector (a Whisper-
    // style encoder, `mmproj.gguf`) instead of a vision one. So `roteiro sync` can
    // transcribe spoken-word audio (wav/mp3/flac) into `meta.content`. Off by
    // default (feature `audio-transcribe`).
    //
    // Model choice: Voxtral-Mini-3B (Mistral, Apache-2.0) is a *transcription-
    // specialised* audio model, verified transcribing a speech clip verbatim over
    // this mtmd path (`rto-llama/tests/audio.rs`). It is the only curated audio
    // pick, and a mid-tier one — so the audio section has no low-tier floor
    // (see `every_section_has_a_low_tier_floor`). A smaller low-tier option (e.g.
    // Ultravox 1B) could be added later if it verifies at acceptable quality.
    ModelSpec {
        name: "voxtral-mini-3b",
        kind: ModelKind::Audio,
        role: ModelRole::None,
        tier: ResourceTier::Mid,
        dim: 0,
        licence: "Apache-2.0",
        description: "Voxtral-Mini-3B (Mistral, Q4_K_M GGUF + Q8_0 audio mmproj) — speech transcription via llama.cpp mtmd",
        size_mib: 3041,
        variants: &[ModelVariant {
            platform: Platform::Standard,
            files: &[
                ModelFile {
                    name: "model.gguf",
                    url: "https://huggingface.co/ggml-org/Voxtral-Mini-3B-2507-GGUF/resolve/main/Voxtral-Mini-3B-2507-Q4_K_M.gguf",
                    sha256: "4705be8ec22ca23d12632f4b4a3691faa95917d90a06d3cf3c3ec0e91958f1a8",
                },
                ModelFile {
                    name: "mmproj.gguf",
                    url: "https://huggingface.co/ggml-org/Voxtral-Mini-3B-2507-GGUF/resolve/main/mmproj-Voxtral-Mini-3B-2507-Q8_0.gguf",
                    sha256: "4f24c4ef3ce929d02ed9d1cfb050ae9a7365f057c0ddec0d489580982ebe0d02",
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

/// Pure resolution of the model-store root, in precedence order: an explicit
/// `model_store` directory (config `[paths] model_store` or `ROTEIRO_MODEL_STORE`,
/// used verbatim), then `roteiro_home`'s `models` subdir (`ROTEIRO_HOME`), then
/// `~/.roteiro/models` under the given home. Factored out so it is testable
/// without mutating the process environment.
fn store_root_from(
    model_store: Option<PathBuf>,
    roteiro_home: Option<PathBuf>,
    home: Option<PathBuf>,
) -> PathBuf {
    // An explicit model-store dir (config `[paths] model_store`) wins verbatim.
    if let Some(dir) = model_store {
        return dir;
    }
    if let Some(dir) = roteiro_home {
        return dir.join("models");
    }
    home.unwrap_or_else(|| PathBuf::from("."))
        .join(".roteiro")
        .join("models")
}

/// A process-wide model-store override, set once from config `[paths]
/// model_store` (the env is `unsafe` to mutate under edition 2024, so a
/// `OnceLock` carries the config value instead).
static MODEL_STORE_OVERRIDE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Set the model-store directory for this process (config `[paths] model_store`).
/// First call wins; later calls are ignored. Call once at startup, before any
/// model operation.
pub fn set_model_store(dir: PathBuf) {
    let _ = MODEL_STORE_OVERRIDE.set(dir);
}

/// Root of the user-level model store (`~/.roteiro/models`). Honours, in order,
/// the config override ([`set_model_store`]), `ROTEIRO_MODEL_STORE` (an explicit
/// store dir), and `ROTEIRO_HOME` (its `models` subdir).
#[must_use]
pub fn store_root() -> PathBuf {
    if let Some(dir) = MODEL_STORE_OVERRIDE.get() {
        return dir.clone();
    }
    store_root_from(
        std::env::var_os("ROTEIRO_MODEL_STORE").map(PathBuf::from),
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

/// Total bytes a model currently occupies in the store — every file under its
/// directory, including any orphaned `.partial` from an abandoned pull.
///
/// 0 for a model that is not installed. This is what `roteiro model rm` would
/// reclaim and what `roteiro model list` reports beside an installed entry.
#[must_use]
pub fn installed_size(name: &str) -> u64 {
    dir_size(&model_dir(name))
}

/// Recursive byte total of `dir`, ignoring anything unreadable (a size report
/// must never be the thing that fails a command).
fn dir_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(_) => e.metadata().map_or(0, |m| m.len()),
            Err(_) => 0,
        })
        .sum()
}

/// What [`remove_model`] deleted.
#[derive(Debug, Clone)]
pub struct Removal {
    /// The directory that was removed.
    pub dir: PathBuf,
    /// Names of the files removed, sorted — including any `.partial` and its
    /// sidecar, so an abandoned pull is cleaned up with the model.
    pub files: Vec<String>,
    /// Bytes reclaimed.
    pub bytes: u64,
}

/// Delete a model's directory from the store, reporting what was freed.
///
/// The whole directory goes, not just the files the *current* registry entry
/// lists: a model's file set can change between releases, and leaving behind
/// bytes that `model list` no longer mentions is exactly the accumulation this
/// command exists to stop.
///
/// # Errors
/// Returns [`std::io::Error`] if the directory exists but cannot be removed.
/// Removing a model that is not installed is not an error here — callers decide
/// what an empty [`Removal`] means (`roteiro model rm` refuses).
pub fn remove_model(name: &str) -> std::io::Result<Removal> {
    let dir = model_dir(name);
    let mut files = Vec::new();
    let mut bytes = 0;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            bytes += match entry.file_type() {
                Ok(t) if t.is_dir() => dir_size(&entry.path()),
                _ => entry.metadata().map_or(0, |m| m.len()),
            };
            files.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    files.sort();
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    Ok(Removal { dir, files, bytes })
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

/// Errors from [`download_verified`] and [`download_resumable`].
#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    /// A read/write failure while streaming the download. For
    /// [`download_resumable`] the bytes already on disk are **kept** so the next
    /// attempt resumes from them.
    #[error("download io error: {0}")]
    Io(#[from] std::io::Error),
    /// The streamed bytes did not match the pinned checksum. The partial file is
    /// always discarded in this case: its contents are known-wrong, so resuming
    /// from it could only ever reproduce the same bad digest.
    #[error("checksum mismatch: expected {expected}, got {got} (partial discarded)")]
    Checksum {
        /// The pinned SHA-256.
        expected: String,
        /// The SHA-256 actually computed over the downloaded bytes.
        got: String,
    },
    /// The caller's range-opening callback failed to reach the server.
    #[error("transport error: {0}")]
    Transport(Box<dyn std::error::Error + Send + Sync>),
    /// The server's answer to a `Range` request could not be used: an
    /// unexpected status, or a `Content-Range` that is unparseable or starts
    /// somewhere other than where resumption asked it to.
    #[error("range request failed: {0}")]
    Range(String),
}

/// What a server did with a `Range` request, as handed back to
/// [`download_resumable`] by the caller's transport.
///
/// Distinguishing these two is the whole point: appending a `200` body (the
/// **entire** file) onto an existing prefix silently corrupts the result, and
/// the corruption only surfaces as a checksum failure after the *whole* file has
/// been transferred a second time.
#[derive(Debug)]
pub enum RangeReply<R> {
    /// The server honoured the request (`206 Partial Content`): `reader` yields
    /// the bytes **from the requested offset onwards**.
    Partial {
        /// Reader over the remaining bytes.
        reader: R,
        /// Total size of the complete resource, from `Content-Range`, if the
        /// server stated it (`*` ⇒ `None`).
        total: Option<u64>,
    },
    /// The server ignored the request, or none was made (`200 OK`): `reader`
    /// yields the resource **from byte zero**.
    Full {
        /// Reader over the whole resource.
        reader: R,
        /// Total size, from `Content-Length`, if the server stated it.
        total: Option<u64>,
        /// Why this is a whole-file body (status + `Accept-Ranges`), for the
        /// message shown when a resume has to be abandoned. See
        /// [`interpret_range_response`], which produces it.
        detail: String,
    },
}

/// Notable things that happen during a resumable download, reported to the
/// caller so it can tell the user. The library never prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadEvent {
    /// An existing partial could not be trusted and was deleted before starting
    /// over. `reason` says which check rejected it.
    DiscardedPartial {
        /// Bytes thrown away.
        bytes: u64,
        /// Why the partial was rejected.
        reason: String,
    },
    /// Picking up an existing partial: the transfer asks for `offset..`.
    Resuming {
        /// Byte offset the request resumes from.
        offset: u64,
        /// Total size, if known from the sidecar.
        total: Option<u64>,
    },
    /// The partial was already complete, so nothing was transferred — it only
    /// needed verifying and installing.
    AlreadyComplete {
        /// Size of the complete partial.
        bytes: u64,
    },
    /// The server would not honour the `Range` request, so the transfer
    /// restarted from zero rather than appending a whole-file body onto the
    /// existing prefix.
    RangeUnsupported {
        /// Bytes discarded by restarting.
        discarded: u64,
        /// What the server said (status / `Accept-Ranges`).
        detail: String,
    },
    /// The transfer failed part-way. The bytes are on disk and the next attempt
    /// will resume from them.
    KeptPartial {
        /// Bytes kept for the next attempt.
        bytes: u64,
    },
    /// The completed transfer failed verification, so the partial was discarded.
    PoisonedPartial {
        /// Bytes discarded.
        bytes: u64,
    },
}

/// Sidecar recorded next to a `.partial`, so a later run can tell whether the
/// bytes on disk are still a valid prefix of what it is now being asked to
/// fetch. Without it a partial is just anonymous bytes and must be discarded.
#[derive(serde::Serialize, serde::Deserialize)]
struct PartialMeta {
    /// Sidecar format version; an unrecognised value discards the partial.
    version: u32,
    /// The URL the partial was started against.
    url: String,
    /// The pinned SHA-256 it was started against (empty ⇒ unpinned).
    sha256: String,
    /// Total size of the complete resource, if the server stated it.
    total: Option<u64>,
}

/// Current [`PartialMeta::version`]. Bump when the sidecar's meaning changes so
/// older partials are discarded rather than misread.
const PARTIAL_META_VERSION: u32 = 1;

/// Path of the in-progress download for `dest` (`model.gguf` → `model.partial`).
#[must_use]
pub fn partial_path(dest: &Path) -> PathBuf {
    dest.with_extension("partial")
}

/// Path of the sidecar describing [`partial_path`]'s provenance.
#[must_use]
pub fn partial_meta_path(dest: &Path) -> PathBuf {
    dest.with_extension("partial.json")
}

/// Delete `dest`'s partial and its sidecar, returning the bytes reclaimed.
///
/// Used both internally and by `roteiro model rm`, which cleans up partials
/// orphaned by an abandoned pull.
///
/// # Errors
/// Returns [`std::io::Error`] if a file exists but cannot be removed.
pub fn discard_partial(dest: &Path) -> std::io::Result<u64> {
    let tmp = partial_path(dest);
    let freed = std::fs::metadata(&tmp).map_or(0, |m| m.len());
    remove_if_present(&tmp)?;
    remove_if_present(&partial_meta_path(dest))?;
    Ok(freed)
}

/// `remove_file`, treating "already gone" as success.
fn remove_if_present(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Interpret a server's answer to a range request into a [`RangeReply`] shape.
///
/// Pure, so the (fiddly) status/`Content-Range` reasoning is unit-testable
/// without a socket. `206` is authoritative for "the range was honoured";
/// `accept_ranges` is only advisory and is used to explain a `200`.
///
/// Returns the kind of body to expect and the total size if the server stated
/// it.
///
/// # Errors
/// Returns [`DownloadError::Range`] for a status other than `200`/`206`, a
/// `Content-Range` that cannot be parsed, or a `206` whose range starts
/// somewhere other than `requested_from`.
pub fn interpret_range_response(
    status: u16,
    accept_ranges: Option<&str>,
    content_range: Option<&str>,
    content_length: Option<u64>,
    requested_from: u64,
) -> Result<(RangeKind, Option<u64>), DownloadError> {
    match status {
        206 => {
            let raw = content_range
                .ok_or_else(|| DownloadError::Range("206 response without Content-Range".into()))?;
            let (start, total) = parse_content_range(raw)?;
            if start != requested_from {
                return Err(DownloadError::Range(format!(
                    "server resumed at byte {start} but {requested_from} was requested \
                     (Content-Range: {raw})"
                )));
            }
            Ok((RangeKind::Partial, total))
        }
        200 => {
            let detail = match accept_ranges {
                Some(v) => format!("200 OK, Accept-Ranges: {v}"),
                None => "200 OK, no Accept-Ranges header".to_owned(),
            };
            Ok((RangeKind::Full { detail }, content_length))
        }
        other => Err(DownloadError::Range(format!(
            "unexpected status {other} (expected 200 or 206)"
        ))),
    }
}

/// The two shapes [`interpret_range_response`] can conclude.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeKind {
    /// A `206`: the body starts at the requested offset.
    Partial,
    /// A `200`: the body is the whole resource, whatever was requested.
    Full {
        /// Human-readable note on why this is a whole-file body.
        detail: String,
    },
}

/// Parse `bytes <start>-<end>/<total>` into `(start, total)`; `total` is `None`
/// for the `*` (unknown-length) form.
fn parse_content_range(raw: &str) -> Result<(u64, Option<u64>), DownloadError> {
    let bad = || DownloadError::Range(format!("unparseable Content-Range: {raw}"));
    let rest = raw.trim().strip_prefix("bytes ").ok_or_else(bad)?;
    let (range, total) = rest.split_once('/').ok_or_else(bad)?;
    let (start, _end) = range.split_once('-').ok_or_else(bad)?;
    let start: u64 = start.trim().parse().map_err(|_| bad())?;
    let total = match total.trim() {
        "*" => None,
        n => Some(n.parse().map_err(|_| bad())?),
    };
    Ok((start, total))
}

/// Stream `reader` to `dest`, hashing the bytes **as they are written** (constant
/// memory — the file is never buffered whole), verify the result against
/// `expected_sha256` (empty ⇒ unpinned, verification skipped), and install
/// atomically (temp file + rename). This lets multi-gigabyte models download
/// without holding the whole file in memory.
///
/// On a checksum mismatch the partial file is removed and
/// [`DownloadError::Checksum`] is returned.
///
/// # Errors
/// Returns [`DownloadError::Io`] on a read/write failure, or
/// [`DownloadError::Checksum`] if the pinned hash does not match.
pub fn download_verified(
    mut reader: impl std::io::Read,
    dest: &Path,
    expected_sha256: &str,
) -> Result<(), DownloadError> {
    use sha2::{Digest, Sha256};

    /// Removes the partial file on drop unless disarmed — best-effort cleanup so
    /// *any* early return (network drop, disk full, fsync/rename failure, checksum
    /// mismatch) never leaves a stray `.partial` behind.
    struct PartialGuard<'a> {
        path: &'a Path,
        armed: bool,
    }
    impl Drop for PartialGuard<'_> {
        fn drop(&mut self) {
            if self.armed {
                std::fs::remove_file(self.path).ok();
            }
        }
    }

    let tmp = dest.with_extension("partial");
    let mut guard = PartialGuard {
        path: &tmp,
        armed: true,
    };

    let mut writer = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16]; // 64 KiB chunks
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        std::io::Write::write_all(&mut writer, &buf[..n])?;
    }
    // `into_inner` flushes the buffer; fsync so the bytes are durable before the
    // rename makes them the installed file.
    writer
        .into_inner()
        .map_err(std::io::IntoInnerError::into_error)?
        .sync_all()?;

    if !expected_sha256.is_empty() {
        let mut got = String::with_capacity(64);
        for byte in hasher.finalize() {
            use std::fmt::Write as _;
            let _ = write!(got, "{byte:02x}");
        }
        if !got.eq_ignore_ascii_case(expected_sha256) {
            // `guard` removes the partial file on return.
            return Err(DownloadError::Checksum {
                expected: expected_sha256.to_owned(),
                got,
            });
        }
    }
    // Atomic install: remove any existing file first (Windows `rename` fails if
    // the destination exists), then rename the verified temp into place. If
    // either fails, `guard` cleans up the partial.
    if dest.exists() {
        std::fs::remove_file(dest)?;
    }
    std::fs::rename(&tmp, dest)?;
    guard.armed = false; // installed successfully — nothing to clean up
    Ok(())
}

/// A `Write` that forwards to `inner` and folds everything actually written into
/// `hasher` — so the digest is always over exactly the bytes that reached the
/// file, in constant memory.
struct HashingWriter<'a, W> {
    inner: W,
    hasher: &'a mut sha2::Sha256,
}

impl<W: std::io::Write> std::io::Write for HashingWriter<'_, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        use sha2::Digest as _;
        // Hash only what the inner writer accepted, so a short write cannot
        // desynchronise the digest from the file.
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Length of `path`, or 0 if it does not exist.
fn existing_len(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |m| m.len())
}

/// Fold the first `len` bytes of `path` into `hasher`.
///
/// A resume has to reconstruct the digest of the bytes already on disk, and
/// `sha2` cannot serialise a mid-stream hasher state — so the prefix is re-read.
/// That is a local disk read, which is orders of magnitude cheaper than
/// re-fetching the same prefix over the network, and it has the useful property
/// of hashing *what is actually there* rather than what a previous run believed
/// it wrote.
fn hash_prefix(path: &Path, len: u64, hasher: &mut sha2::Sha256) -> std::io::Result<()> {
    let mut src = std::io::Read::take(std::fs::File::open(path)?, len);
    let mut sink = HashingWriter {
        inner: std::io::sink(),
        hasher,
    };
    let read = std::io::copy(&mut src, &mut sink)?;
    if read != len {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            format!("partial download shrank while being re-hashed ({read} of {len} bytes)"),
        ));
    }
    Ok(())
}

/// Read the sidecar next to a partial, or `None` if it is absent or unreadable.
fn load_partial_meta(path: &Path) -> Option<PartialMeta> {
    let raw = std::fs::read(path).ok()?;
    serde_json::from_slice(&raw).ok()
}

/// Write the sidecar recording what the partial is being fetched against.
fn write_partial_meta(path: &Path, meta: &PartialMeta) -> std::io::Result<()> {
    let json = serde_json::to_vec(meta).map_err(std::io::Error::other)?;
    std::fs::write(path, json)
}

/// Verify the streamed digest and install the partial atomically.
///
/// On a mismatch the partial is **discarded**: unlike a dropped connection, bad
/// bytes are not a usable prefix of anything, so keeping them would only let a
/// later run resume into the same failure.
fn install_verified(
    dest: &Path,
    expected_sha256: &str,
    hasher: sha2::Sha256,
    on_event: &mut impl FnMut(DownloadEvent),
) -> Result<(), DownloadError> {
    use sha2::Digest as _;

    let tmp = partial_path(dest);
    if !expected_sha256.is_empty() {
        let mut got = String::with_capacity(64);
        for byte in hasher.finalize() {
            use std::fmt::Write as _;
            let _ = write!(got, "{byte:02x}");
        }
        if !got.eq_ignore_ascii_case(expected_sha256) {
            let bytes = existing_len(&tmp);
            discard_partial(dest)?;
            on_event(DownloadEvent::PoisonedPartial { bytes });
            return Err(DownloadError::Checksum {
                expected: expected_sha256.to_owned(),
                got,
            });
        }
    }
    // Atomic install: remove any existing file first (Windows `rename` fails if
    // the destination exists), then rename the verified temp into place.
    if dest.exists() {
        std::fs::remove_file(dest)?;
    }
    std::fs::rename(&tmp, dest)?;
    // The sidecar only describes an in-progress download; the installed file is
    // described by the registry.
    remove_if_present(&partial_meta_path(dest))?;
    Ok(())
}

/// Decide where a download should start: the length of a partial that every
/// check confirms is still a prefix of `url`'s current contents, or 0.
///
/// Anything that cannot be *positively* confirmed is discarded, because a wrong
/// prefix is far more expensive than a re-download — it is only detected after
/// the whole file has been transferred again, and then only as an unexplained
/// checksum failure. Returns the resume offset and the total size the partial
/// was started against, if it was recorded.
fn plan_resume(
    dest: &Path,
    url: &str,
    expected_sha256: &str,
    on_event: &mut impl FnMut(DownloadEvent),
) -> Result<(u64, Option<u64>), DownloadError> {
    let on_disk = existing_len(&partial_path(dest));
    if on_disk == 0 {
        return Ok((0, None));
    }

    let mut known_total = None;
    let reject = match load_partial_meta(&partial_meta_path(dest)) {
        None => Some("no sidecar recording what it was started against".to_owned()),
        Some(m) if m.version != PARTIAL_META_VERSION => Some(format!(
            "its sidecar is format v{}, not v{PARTIAL_META_VERSION}",
            m.version
        )),
        Some(m) if m.url != url => Some("it was started against a different URL".to_owned()),
        Some(m) if m.sha256 != expected_sha256 => {
            Some("the pinned checksum changed since it was started".to_owned())
        }
        Some(m) => match m.total {
            Some(t) if on_disk > t => Some(format!(
                "it is larger ({on_disk} bytes) than the recorded total ({t} bytes)"
            )),
            total => {
                known_total = total;
                None
            }
        },
    };
    if let Some(reason) = reject {
        on_event(DownloadEvent::DiscardedPartial {
            bytes: on_disk,
            reason,
        });
        discard_partial(dest)?;
        return Ok((0, None));
    }
    Ok((on_disk, known_total))
}

/// Ask the transport for the bytes still needed and settle where they go.
///
/// `resume_from`, `known_total` and `hasher` are in/out: on return they describe
/// the transfer that is about to happen, so a rejected resume (`Range` ignored,
/// or a remote that changed size) leaves the caller with a consistent
/// start-from-zero state rather than a half-updated one. The `bool` says whether
/// the body appends to an existing prefix or replaces the file.
///
/// At most one retry: only the *server* can reveal that the resource is now a
/// different size from the one the partial was started against, so that check
/// costs a second request — but only ever one.
fn start_transfer<R, F, E>(
    dest: &Path,
    open: &mut F,
    on_event: &mut E,
    hasher: &mut sha2::Sha256,
    resume_from: &mut u64,
    known_total: &mut Option<u64>,
) -> Result<(R, bool), DownloadError>
where
    R: std::io::Read,
    F: FnMut(u64) -> Result<RangeReply<R>, DownloadError>,
    E: FnMut(DownloadEvent),
{
    use sha2::Digest as _;

    let tmp = partial_path(dest);
    loop {
        if *resume_from > 0 {
            hash_prefix(&tmp, *resume_from, hasher)?;
        }
        match open(*resume_from)? {
            RangeReply::Partial { reader, total } => {
                if let (Some(remote), Some(recorded)) = (total, *known_total)
                    && remote != recorded
                {
                    on_event(DownloadEvent::DiscardedPartial {
                        bytes: *resume_from,
                        reason: format!(
                            "the remote is now {remote} bytes but it was started against {recorded}"
                        ),
                    });
                    discard_partial(dest)?;
                    *resume_from = 0;
                    *known_total = None;
                    *hasher = sha2::Sha256::new();
                    continue;
                }
                *known_total = total.or(*known_total);
                on_event(DownloadEvent::Resuming {
                    offset: *resume_from,
                    total: *known_total,
                });
                return Ok((reader, true));
            }
            RangeReply::Full {
                reader,
                total,
                detail,
            } => {
                if *resume_from > 0 {
                    // Never append a whole-file body onto an existing prefix: it
                    // would corrupt the result and only surface as a checksum
                    // failure after the whole file had transferred again.
                    on_event(DownloadEvent::RangeUnsupported {
                        discarded: *resume_from,
                        detail,
                    });
                    *resume_from = 0;
                    *hasher = sha2::Sha256::new();
                }
                *known_total = total;
                return Ok((reader, false));
            }
        }
    }
}

/// Download `url` to `dest`, **resuming** an interrupted earlier attempt when it
/// is safe to do so, verifying the pinned SHA-256, and installing atomically.
///
/// `open` is the caller's transport: given a byte offset it issues the request
/// (a `Range: bytes=<offset>-` when the offset is non-zero) and reports what the
/// server did as a [`RangeReply`]. Keeping the transport out of this crate is
/// deliberate — `rto-graph` never touches the network — and it makes every
/// branch below testable against a local socket or an in-process stub.
///
/// `on_event` receives the [`DownloadEvent`]s worth telling a user about. The
/// library itself never prints.
///
/// # Failure modes
/// - **Transport failure** (connection dropped, disk full mid-write): the bytes
///   on disk are **kept** along with their sidecar, and
///   [`DownloadEvent::KeptPartial`] is emitted. The next call resumes from them.
///   This is the whole point: a 19 GiB pull that dies at 90% costs the last 10%,
///   not the whole thing.
/// - **Checksum failure**: the partial is **discarded**
///   ([`DownloadEvent::PoisonedPartial`]) and [`DownloadError::Checksum`]
///   returned. Those bytes are known-wrong; resuming from them is meaningless.
/// - **Server without `Range` support** (a `200` where a `206` was asked for):
///   the existing prefix is dropped and the transfer restarts from zero, with
///   [`DownloadEvent::RangeUnsupported`] saying so. Appending a whole-file body
///   onto a prefix would corrupt the result and only surface as a checksum
///   failure after another full transfer.
/// - **Stale or mismatched partial** (different URL, different pinned digest,
///   different remote size, missing or unrecognised sidecar): discarded before
///   anything is transferred, with [`DownloadEvent::DiscardedPartial`] naming
///   the check that rejected it.
///
/// # Errors
/// Returns [`DownloadError::Io`] on a read/write failure,
/// [`DownloadError::Transport`] if `open` fails, [`DownloadError::Range`] if the
/// server's answer to a range request is unusable, or
/// [`DownloadError::Checksum`] if the pinned hash does not match.
pub fn download_resumable<R, F, E>(
    dest: &Path,
    url: &str,
    expected_sha256: &str,
    mut open: F,
    mut on_event: E,
) -> Result<(), DownloadError>
where
    R: std::io::Read,
    F: FnMut(u64) -> Result<RangeReply<R>, DownloadError>,
    E: FnMut(DownloadEvent),
{
    use sha2::Digest as _;

    let tmp = partial_path(dest);
    let meta_path = partial_meta_path(dest);

    // 1. Decide whether the bytes already on disk are a usable prefix of what we
    //    are about to fetch.
    let (mut resume_from, mut known_total) =
        plan_resume(dest, url, expected_sha256, &mut on_event)?;

    // 2. A partial that already covers the whole resource needs no transfer at
    //    all — just verification. (A previous run died between the last write
    //    and the rename.)
    if resume_from > 0 && known_total == Some(resume_from) {
        on_event(DownloadEvent::AlreadyComplete { bytes: resume_from });
        let mut hasher = sha2::Sha256::new();
        hash_prefix(&tmp, resume_from, &mut hasher)?;
        return install_verified(dest, expected_sha256, hasher, &mut on_event);
    }

    // 3. Open the transfer, folding any prefix already on disk into the hash.
    let mut hasher = sha2::Sha256::new();
    let (mut reader, append) = start_transfer(
        dest,
        &mut open,
        &mut on_event,
        &mut hasher,
        &mut resume_from,
        &mut known_total,
    )?;

    // 4. Record what this partial is being fetched against *before* writing any
    //    bytes, so even a hard kill leaves a resumable pair.
    write_partial_meta(
        &meta_path,
        &PartialMeta {
            version: PARTIAL_META_VERSION,
            url: url.to_owned(),
            sha256: expected_sha256.to_owned(),
            total: known_total,
        },
    )?;

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&tmp)?;
    if append {
        std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(resume_from))?;
    } else {
        file.set_len(0)?;
    }

    // 5. Stream, hashing as it writes — a multi-gigabyte model never buffers in
    //    memory. A failure here keeps everything on disk for the next attempt.
    let mut sink = HashingWriter {
        inner: std::io::BufWriter::with_capacity(1 << 20, file),
        hasher: &mut hasher,
    };
    let streamed = std::io::copy(&mut reader, &mut sink).map(|_| ());
    // Flush and fsync unconditionally, so the file length on disk is exactly the
    // prefix a later resume will re-hash — even on the failure path.
    let durable = std::io::Write::flush(&mut sink).and_then(|()| sink.inner.get_ref().sync_all());
    drop(sink);

    if let Err(e) = streamed.and(durable) {
        on_event(DownloadEvent::KeptPartial {
            bytes: existing_len(&tmp),
        });
        return Err(DownloadError::Io(e));
    }

    // 6. A connection that dies mid-body closes *cleanly* from the reader's point
    //    of view, so `copy` returns `Ok` having transferred less than the whole
    //    file. Without this length check that would be diagnosed as a checksum
    //    failure — which discards the partial, and so would defeat resumption for
    //    the single most common failure mode there is.
    let on_disk = existing_len(&tmp);
    if let Some(total) = known_total {
        if on_disk < total {
            on_event(DownloadEvent::KeptPartial { bytes: on_disk });
            return Err(DownloadError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!("connection closed after {on_disk} of {total} bytes"),
            )));
        }
        if on_disk > total {
            // More bytes than the resource has: the response did not describe
            // what it sent, so nothing on disk can be trusted as a prefix.
            discard_partial(dest)?;
            on_event(DownloadEvent::PoisonedPartial { bytes: on_disk });
            return Err(DownloadError::Range(format!(
                "server sent {on_disk} bytes for a {total}-byte resource"
            )));
        }
    }

    // 7. Verify and install atomically.
    install_verified(dest, expected_sha256, hasher, &mut on_event)
}

/// A local HTTP/1.1 server for the download tests — enough of the protocol to
/// answer `GET` with and without `Range`, and to cut a response short so the
/// resume path is exercised over a real socket instead of a stub.
///
/// Lives outside `mod tests` only because it is shared by the unit tests here
/// and kept beside the code it exercises; it is compiled only under `cfg(test)`.
#[cfg(test)]
mod testserver {
    use std::io::{Read as _, Write as _};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    /// What the server should do with the next request.
    pub struct Behaviour {
        /// The resource being served.
        pub body: Vec<u8>,
        /// Whether to honour `Range` (a `206`) or ignore it (a `200`).
        pub ranges: bool,
        /// Write at most this many body bytes, then close — a dropped
        /// connection.
        pub limit: Option<usize>,
    }

    /// One served request: the offset it started at and how many bytes went out.
    pub type Hit = (u64, usize);

    pub struct TestServer {
        pub addr: SocketAddr,
        pub behaviour: Arc<Mutex<Behaviour>>,
        pub hits: Arc<Mutex<Vec<Hit>>>,
        stop: Arc<AtomicBool>,
    }

    impl TestServer {
        pub fn start(behaviour: Behaviour) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let addr = listener.local_addr().expect("addr");
            let behaviour = Arc::new(Mutex::new(behaviour));
            let hits = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(AtomicBool::new(false));
            {
                let (behaviour, hits, stop) = (behaviour.clone(), hits.clone(), stop.clone());
                std::thread::spawn(move || {
                    for sock in listener.incoming() {
                        if stop.load(Ordering::SeqCst) {
                            break;
                        }
                        if let Ok(sock) = sock {
                            handle(sock, &behaviour, &hits);
                        }
                    }
                });
            }
            Self {
                addr,
                behaviour,
                hits,
                stop,
            }
        }

        /// Requests served so far, oldest first.
        pub fn hits(&self) -> Vec<Hit> {
            self.hits.lock().expect("hits").clone()
        }

        /// Change what the next request gets.
        pub fn set_limit(&self, limit: Option<usize>) {
            self.behaviour.lock().expect("behaviour").limit = limit;
        }
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            // Unblock the accept loop so the thread exits with the test.
            self.stop.store(true, Ordering::SeqCst);
            let _ = TcpStream::connect(self.addr);
        }
    }

    /// Read the request head, then write the (possibly partial) response.
    fn handle(mut sock: TcpStream, behaviour: &Arc<Mutex<Behaviour>>, hits: &Arc<Mutex<Vec<Hit>>>) {
        let Some(head) = read_head(&mut sock) else {
            return;
        };
        // `Range: bytes=<from>-`
        let requested = head
            .lines()
            .find_map(|l| {
                l.to_ascii_lowercase()
                    .strip_prefix("range:")
                    .map(str::trim)
                    .map(str::to_owned)
            })
            .and_then(|v| v.strip_prefix("bytes=").map(str::to_owned))
            .and_then(|v| v.split('-').next().and_then(|n| n.parse::<u64>().ok()));

        let b = behaviour.lock().expect("behaviour");
        let total = b.body.len();
        let start = match requested {
            Some(from) if b.ranges => usize::try_from(from).expect("offset fits"),
            _ => 0,
        };
        let partial = b.ranges && requested.is_some();
        let remainder = &b.body[start.min(total)..];
        let serve = b.limit.unwrap_or(remainder.len()).min(remainder.len());

        let status_line = if partial {
            format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{}/{total}\r\n",
                total.saturating_sub(1)
            )
        } else {
            "HTTP/1.1 200 OK\r\n".to_owned()
        };
        let accept = if b.ranges {
            "Accept-Ranges: bytes\r\n"
        } else {
            "Accept-Ranges: none\r\n"
        };
        // Content-Length always states the *full* remainder: when `limit` cuts the
        // body short the client sees exactly what a dropped connection looks like.
        let resp = format!(
            "{status_line}{accept}Content-Length: {}\r\nConnection: close\r\n\r\n",
            remainder.len()
        );
        let _ = sock.write_all(resp.as_bytes());
        let _ = sock.write_all(&remainder[..serve]);
        let _ = sock.flush();
        hits.lock()
            .expect("hits")
            .push((u64::try_from(start).unwrap_or(0), serve));
    }

    /// Read bytes until the end of the header block, and no further.
    fn read_head(sock: &mut TcpStream) -> Option<String> {
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        while !buf.ends_with(b"\r\n\r\n") {
            match sock.read(&mut byte) {
                Ok(0) | Err(_) => return None,
                Ok(_) => buf.push(byte[0]),
            }
        }
        Some(String::from_utf8_lossy(&buf).into_owned())
    }
}

/// The test-side HTTP client: issues the request and interprets the answer with
/// the real [`interpret_range_response`], so the tests cover that reasoning too.
#[cfg(test)]
fn test_get_range(
    addr: std::net::SocketAddr,
    from: u64,
) -> Result<RangeReply<std::net::TcpStream>, DownloadError> {
    use std::io::{Read as _, Write as _};

    let transport = |e: std::io::Error| DownloadError::Transport(Box::new(e));
    let mut sock = std::net::TcpStream::connect(addr).map_err(transport)?;
    let range = if from > 0 {
        format!("Range: bytes={from}-\r\n")
    } else {
        String::new()
    };
    let req =
        format!("GET /model.bin HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n{range}\r\n");
    sock.write_all(req.as_bytes()).map_err(transport)?;

    // Byte-at-a-time so the socket is left positioned exactly at the body.
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match sock.read(&mut byte) {
            Ok(0) => return Err(DownloadError::Range("connection closed in headers".into())),
            Ok(_) => head.push(byte[0]),
            Err(e) => return Err(transport(e)),
        }
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    let mut lines = head.lines();
    let status: u16 = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1).and_then(|s| s.parse().ok()))
        .ok_or_else(|| DownloadError::Range("no status line".into()))?;
    let (mut accept_ranges, mut content_range, mut content_length) = (None, None, None);
    for line in lines {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let v = v.trim().to_owned();
        match k.trim().to_ascii_lowercase().as_str() {
            "accept-ranges" => accept_ranges = Some(v),
            "content-range" => content_range = Some(v),
            "content-length" => content_length = v.parse().ok(),
            _ => {}
        }
    }
    let (kind, total) = interpret_range_response(
        status,
        accept_ranges.as_deref(),
        content_range.as_deref(),
        content_length,
        from,
    )?;
    Ok(match kind {
        RangeKind::Partial => RangeReply::Partial {
            reader: sock,
            total,
        },
        RangeKind::Full { detail } => RangeReply::Full {
            reader: sock,
            total,
            detail,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::testserver::{Behaviour, TestServer};
    use super::{
        DownloadError, DownloadEvent, ModelKind, Platform, REGISTRY, RangeKind, ResourceTier,
        download_resumable, download_verified, find, installed_size, interpret_range_response,
        partial_meta_path, partial_path, sha256_hex, store_root, test_get_range, verify_sha256,
    };
    use std::path::{Path, PathBuf};

    /// A fresh scratch directory per test.
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("roteiro-dl-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    /// Widen a length for comparison against a byte count.
    fn u(n: usize) -> u64 {
        u64::try_from(n).expect("length fits u64")
    }

    /// A deterministic, incompressible-enough payload big enough to be cut in
    /// half meaningfully.
    fn payload(len: usize) -> Vec<u8> {
        (0..len)
            .map(|i| u8::try_from(i % 251).unwrap_or(0))
            .collect()
    }

    /// Run one `download_resumable` attempt against `server`, collecting events.
    fn attempt(
        server: &TestServer,
        dest: &Path,
        sha: &str,
    ) -> (Result<(), DownloadError>, Vec<DownloadEvent>) {
        let addr = server.addr;
        let mut events = Vec::new();
        let url = format!("http://{addr}/model.bin");
        let result = download_resumable(
            dest,
            &url,
            sha,
            |from| test_get_range(addr, from),
            |e| events.push(e),
        );
        (result, events)
    }

    #[test]
    fn resumable_clean_download() {
        let dir = scratch("clean");
        let dest = dir.join("model.bin");
        let body = payload(50_000);
        let sha = sha256_hex(&body);
        let server = TestServer::start(Behaviour {
            body: body.clone(),
            ranges: true,
            limit: None,
        });

        let (result, events) = attempt(&server, &dest, &sha);
        result.expect("clean download");
        assert_eq!(std::fs::read(&dest).expect("installed"), body);
        // Nothing left behind.
        assert!(!partial_path(&dest).exists());
        assert!(!partial_meta_path(&dest).exists());
        // One request, whole file, from byte zero.
        assert_eq!(server.hits(), vec![(0, 50_000)]);
        assert!(events.is_empty(), "unexpected events: {events:?}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resumable_interrupted_then_resumed_transfers_only_the_remainder() {
        const CUT: usize = 20_000;
        let dir = scratch("resume");
        let dest = dir.join("model.bin");
        let body = payload(50_000);
        let sha = sha256_hex(&body);
        let server = TestServer::start(Behaviour {
            body: body.clone(),
            ranges: true,
            limit: Some(CUT),
        });

        // First attempt: the connection dies after CUT bytes.
        let (result, events) = attempt(&server, &dest, &sha);
        let err = result.expect_err("interrupted");
        assert!(
            matches!(err, DownloadError::Io(_)),
            "a dropped connection must be an I/O failure, not a checksum one: {err:?}"
        );
        assert_eq!(events, vec![DownloadEvent::KeptPartial { bytes: u(CUT) }]);
        // The partial and its sidecar survive, holding exactly the prefix.
        assert_eq!(
            std::fs::metadata(partial_path(&dest))
                .expect("partial kept")
                .len(),
            u(CUT)
        );
        assert!(partial_meta_path(&dest).exists());
        assert!(!dest.exists());

        // Second attempt against a healthy server.
        server.set_limit(None);
        let (result, events) = attempt(&server, &dest, &sha);
        result.expect("resumed");
        assert_eq!(std::fs::read(&dest).expect("installed"), body);
        assert_eq!(
            events,
            vec![DownloadEvent::Resuming {
                offset: u(CUT),
                total: Some(50_000),
            }]
        );

        // The point of the exercise: the second request asked for, and received,
        // only the remaining bytes.
        assert_eq!(
            server.hits(),
            vec![(0, CUT), (u(CUT), 50_000 - CUT)],
            "the resumed attempt must transfer only the remainder"
        );
        assert!(!partial_path(&dest).exists());
        assert!(!partial_meta_path(&dest).exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resumable_server_without_range_support_restarts_and_says_so() {
        const CUT: usize = 15_000;
        let dir = scratch("norange");
        let dest = dir.join("model.bin");
        let body = payload(40_000);
        let sha = sha256_hex(&body);
        // `ranges: false` → every response is a 200 with `Accept-Ranges: none`.
        let server = TestServer::start(Behaviour {
            body: body.clone(),
            ranges: false,
            limit: Some(CUT),
        });

        let (result, _) = attempt(&server, &dest, &sha);
        assert!(matches!(result, Err(DownloadError::Io(_))));
        assert_eq!(
            std::fs::metadata(partial_path(&dest)).expect("kept").len(),
            u(CUT)
        );

        // Second attempt: the server ignores Range, so the prefix must be thrown
        // away and the transfer restarted — never appended to.
        server.set_limit(None);
        let (result, events) = attempt(&server, &dest, &sha);
        result.expect("restarted");
        assert_eq!(
            std::fs::read(&dest).expect("installed"),
            body,
            "restarting must not append a whole-file body onto the stale prefix"
        );
        match events.as_slice() {
            [DownloadEvent::RangeUnsupported { discarded, detail }] => {
                assert_eq!(*discarded, u(CUT));
                assert!(
                    detail.contains("200") && detail.to_ascii_lowercase().contains("accept-ranges"),
                    "the message must explain why: {detail}"
                );
            }
            other => panic!("expected a single RangeUnsupported event, got {other:?}"),
        }
        // Second request started at zero and carried the whole file.
        assert_eq!(server.hits(), vec![(0, CUT), (0, 40_000)]);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resumable_stale_partial_is_discarded() {
        let dir = scratch("stale");
        let body = payload(30_000);
        let sha = sha256_hex(&body);
        let server = TestServer::start(Behaviour {
            body: body.clone(),
            ranges: true,
            limit: None,
        });

        // (a) A partial with no sidecar at all — anonymous bytes.
        let dest = dir.join("nometa.bin");
        std::fs::write(partial_path(&dest), b"anonymous bytes").expect("write");
        let (result, events) = attempt(&server, &dest, &sha);
        result.expect("re-downloaded");
        assert_eq!(std::fs::read(&dest).expect("installed"), body);
        assert!(
            matches!(
                events.first(),
                Some(DownloadEvent::DiscardedPartial { bytes: 15, reason }) if reason.contains("sidecar")
            ),
            "{events:?}"
        );

        // (b) A partial whose sidecar records a different pinned checksum.
        let dest = dir.join("othersha.bin");
        std::fs::write(partial_path(&dest), &body[..1000]).expect("write");
        std::fs::write(
            partial_meta_path(&dest),
            format!(
                r#"{{"version":1,"url":"http://{}/model.bin","sha256":"{}","total":30000}}"#,
                server.addr,
                "0".repeat(64)
            ),
        )
        .expect("write meta");
        let (result, events) = attempt(&server, &dest, &sha);
        result.expect("re-downloaded");
        assert_eq!(std::fs::read(&dest).expect("installed"), body);
        assert!(
            matches!(
                events.first(),
                Some(DownloadEvent::DiscardedPartial { reason, .. }) if reason.contains("checksum")
            ),
            "{events:?}"
        );

        // (c) A partial whose sidecar records a different URL.
        let dest = dir.join("otherurl.bin");
        std::fs::write(partial_path(&dest), &body[..2000]).expect("write");
        std::fs::write(
            partial_meta_path(&dest),
            format!(
                r#"{{"version":1,"url":"http://elsewhere.invalid/model.bin","sha256":"{sha}","total":30000}}"#
            ),
        )
        .expect("write meta");
        let (result, events) = attempt(&server, &dest, &sha);
        result.expect("re-downloaded");
        assert!(
            matches!(
                events.first(),
                Some(DownloadEvent::DiscardedPartial { reason, .. }) if reason.contains("URL")
            ),
            "{events:?}"
        );

        // (d) A partial started against a *different remote size* — only the
        //     server can reveal this, so it is caught on the response.
        let dest = dir.join("othersize.bin");
        std::fs::write(partial_path(&dest), &body[..3000]).expect("write");
        std::fs::write(
            partial_meta_path(&dest),
            format!(
                r#"{{"version":1,"url":"http://{}/model.bin","sha256":"{sha}","total":999999}}"#,
                server.addr
            ),
        )
        .expect("write meta");
        let (result, events) = attempt(&server, &dest, &sha);
        result.expect("re-downloaded");
        assert_eq!(std::fs::read(&dest).expect("installed"), body);
        assert!(
            matches!(
                events.first(),
                Some(DownloadEvent::DiscardedPartial { reason, .. }) if reason.contains("30000")
            ),
            "{events:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resumable_checksum_failure_discards_the_partial() {
        let dir = scratch("poison");
        let dest = dir.join("model.bin");
        let body = payload(25_000);
        let server = TestServer::start(Behaviour {
            body,
            ranges: true,
            limit: None,
        });

        // Pin a hash the body cannot match: the transfer completes, then fails.
        let (result, events) = attempt(&server, &dest, &"a".repeat(64));
        let err = result.expect_err("checksum mismatch");
        assert!(matches!(err, DownloadError::Checksum { .. }), "{err:?}");
        assert_eq!(
            events,
            vec![DownloadEvent::PoisonedPartial { bytes: 25_000 }]
        );
        // Unlike a dropped connection, poisoned bytes are *not* kept: resuming
        // from them could only reproduce the same bad digest.
        assert!(!partial_path(&dest).exists());
        assert!(!partial_meta_path(&dest).exists());
        assert!(!dest.exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resumable_complete_partial_only_needs_verifying() {
        let dir = scratch("complete");
        let dest = dir.join("model.bin");
        let body = payload(12_345);
        let sha = sha256_hex(&body);
        let server = TestServer::start(Behaviour {
            body: body.clone(),
            ranges: true,
            limit: None,
        });

        // A previous run wrote every byte but died before the rename.
        std::fs::write(partial_path(&dest), &body).expect("write");
        std::fs::write(
            partial_meta_path(&dest),
            format!(
                r#"{{"version":1,"url":"http://{}/model.bin","sha256":"{sha}","total":12345}}"#,
                server.addr
            ),
        )
        .expect("write meta");

        let (result, events) = attempt(&server, &dest, &sha);
        result.expect("installed from the complete partial");
        assert_eq!(std::fs::read(&dest).expect("installed"), body);
        assert_eq!(
            events,
            vec![DownloadEvent::AlreadyComplete { bytes: 12_345 }]
        );
        // Nothing was fetched at all.
        assert!(server.hits().is_empty(), "no request should have been made");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn range_response_interpretation() {
        // A 206 that starts where we asked.
        let (kind, total) =
            interpret_range_response(206, Some("bytes"), Some("bytes 100-499/500"), None, 100)
                .expect("206");
        assert_eq!(kind, RangeKind::Partial);
        assert_eq!(total, Some(500));

        // A 206 with an unknown total (`*`).
        let (_, total) =
            interpret_range_response(206, None, Some("bytes 10-19/*"), None, 10).expect("206 *");
        assert_eq!(total, None);

        // A 206 that resumes somewhere else is a hard error — appending it would
        // silently corrupt the file.
        let err = interpret_range_response(206, None, Some("bytes 0-499/500"), None, 100)
            .expect_err("wrong offset");
        assert!(matches!(err, DownloadError::Range(_)), "{err:?}");

        // A 206 without a Content-Range at all.
        assert!(matches!(
            interpret_range_response(206, None, None, None, 5),
            Err(DownloadError::Range(_))
        ));

        // Unparseable Content-Range.
        assert!(matches!(
            interpret_range_response(206, None, Some("chunks 1-2/3"), None, 1),
            Err(DownloadError::Range(_))
        ));

        // A 200 is a whole-file body, and the detail explains why.
        let (kind, total) =
            interpret_range_response(200, Some("none"), None, Some(500), 100).expect("200");
        match kind {
            RangeKind::Full { detail } => assert!(detail.contains("none"), "{detail}"),
            RangeKind::Partial => panic!("expected Full, got Partial"),
        }
        assert_eq!(total, Some(500));

        // A 200 with no Accept-Ranges header says so.
        let (kind, _) = interpret_range_response(200, None, None, None, 0).expect("200");
        match kind {
            RangeKind::Full { detail } => assert!(detail.contains("no Accept-Ranges"), "{detail}"),
            RangeKind::Partial => panic!("expected Full, got Partial"),
        }

        // Anything else is refused rather than guessed at.
        assert!(matches!(
            interpret_range_response(416, None, None, None, 10),
            Err(DownloadError::Range(_))
        ));
    }

    #[test]
    fn installed_size_and_removal() {
        use super::{model_dir, remove_model, set_model_store};

        // Point the store at a scratch dir for this process. `set_model_store` is
        // first-call-wins, so tolerate another test having set it already.
        // Must still end in `models`: `store_root_resolution` asserts that, and
        // the override is process-wide across the parallel test run.
        let dir = scratch("store");
        set_model_store(dir.join("models"));
        let root = super::store_root();

        let name = "size-probe";
        assert_eq!(installed_size(name), 0, "absent model occupies nothing");

        let mdir = root.join(name);
        std::fs::create_dir_all(&mdir).expect("mkdir");
        std::fs::write(mdir.join("model.gguf"), vec![7u8; 4096]).expect("write");
        std::fs::write(mdir.join("model.partial"), vec![7u8; 1024]).expect("write");
        std::fs::write(mdir.join("model.partial.json"), b"{}").expect("write");
        assert_eq!(model_dir(name), mdir);
        assert_eq!(installed_size(name), 4096 + 1024 + 2);

        let removed = remove_model(name).expect("removed");
        assert_eq!(removed.bytes, 4096 + 1024 + 2);
        assert_eq!(
            removed.files,
            vec!["model.gguf", "model.partial", "model.partial.json"],
            "an orphaned partial is cleaned up with the model"
        );
        assert!(!mdir.exists());

        // Removing again is not an error; it simply frees nothing.
        let again = remove_model(name).expect("idempotent");
        assert_eq!(again.bytes, 0);
        assert!(again.files.is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn download_verified_streams_and_checks() {
        // A reader that yields `.0` bytes then errors — to exercise the mid-stream
        // I/O-failure cleanup path.
        struct FailReader(usize);
        impl std::io::Read for FailReader {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.0 == 0 {
                    return Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "boom"));
                }
                let n = buf.len().min(self.0);
                buf[..n].fill(b'x');
                self.0 -= n;
                Ok(n)
            }
        }

        let dir = std::env::temp_dir().join(format!("roteiro-dl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let payload = b"the streamed model bytes";
        let sha = sha256_hex(payload);

        // Correct hash → file installed with exactly the streamed bytes.
        let good = dir.join("good.bin");
        download_verified(&payload[..], &good, &sha).expect("verified");
        assert_eq!(std::fs::read(&good).expect("read"), payload);

        // Wrong hash → error, and no partial file left behind.
        let bad = dir.join("bad.bin");
        let err = download_verified(&payload[..], &bad, &"0".repeat(64)).unwrap_err();
        assert!(matches!(err, DownloadError::Checksum { .. }));
        assert!(!bad.exists());
        assert!(!bad.with_extension("partial").exists());

        // A read error mid-stream → error, and the partial file is cleaned up.
        let dropped = dir.join("dropped.bin");
        let err = download_verified(FailReader(100), &dropped, "").unwrap_err();
        assert!(matches!(err, DownloadError::Io(_)));
        assert!(!dropped.exists());
        assert!(!dropped.with_extension("partial").exists());

        // Empty (unpinned) hash → installed without verification.
        let unpinned = dir.join("unpinned.bin");
        download_verified(&payload[..], &unpinned, "").expect("unpinned");
        assert!(unpinned.exists());

        std::fs::remove_dir_all(&dir).ok();
    }

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
        // `roteiro model list` always has a low-resource recommendation — except
        // Audio: the only curated audio model (Voxtral, transcription-quality) is
        // mid-tier and no low-tier audio pick is offered yet, so the audio section
        // deliberately has no low-tier floor. See the audio registry entry.
        for kind in [
            ModelKind::Embedding,
            ModelKind::Generative,
            ModelKind::Ocr,
            ModelKind::Vision,
        ] {
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
        let spec = find("bge-base-en-v1.5").expect("registered");
        // It only ships a Standard variant, so both hosts resolve to it.
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
        // An explicit model-store dir wins verbatim (config `[paths] model_store`).
        assert_eq!(
            store_root_from(
                Some(PathBuf::from("/data/models")),
                Some(PathBuf::from("/opt/rt")),
                Some(PathBuf::from("/home/u"))
            ),
            Path::new("/data/models"),
        );
        // Else ROTEIRO_HOME's `models` subdir.
        assert_eq!(
            store_root_from(
                None,
                Some(PathBuf::from("/opt/rt")),
                Some(PathBuf::from("/home/u"))
            ),
            Path::new("/opt/rt/models"),
        );
        // Else falls back to <home>/.roteiro/models.
        assert_eq!(
            store_root_from(None, None, Some(PathBuf::from("/home/u"))),
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

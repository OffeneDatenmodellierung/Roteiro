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

/// Errors from [`download_verified`].
#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    /// A read/write failure while streaming the download.
    #[error("download io error: {0}")]
    Io(#[from] std::io::Error),
    /// The streamed bytes did not match the pinned checksum.
    #[error("checksum mismatch: expected {expected}, got {got}")]
    Checksum {
        /// The pinned SHA-256.
        expected: String,
        /// The SHA-256 actually computed over the downloaded bytes.
        got: String,
    },
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

#[cfg(test)]
mod tests {
    use super::{
        DownloadError, ModelKind, Platform, REGISTRY, ResourceTier, download_verified, find,
        sha256_hex, store_root, verify_sha256,
    };
    use std::path::Path;

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

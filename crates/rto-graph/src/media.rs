//! Generated media content — a separate artifact store, never a graph fact.
//!
//! An ASR transcript (Voxtral) and a VLM description (`SmolVLM`) are **generated**,
//! not decoded. Asked to transcribe digital silence a model does not return
//! nothing; it returns fluent, confident prose. That is not a deterministic pure
//! function of `(path, blob id, bytes)` — change the model, the quantisation or
//! the sampling parameters and the same blob yields different "facts" — so it is
//! not a `derived` fact and must not be stored as one (ADR-0015, issue #300).
//!
//! It lives here instead: its own table, its own retrieval surface, and never in
//! `nodes`/`edges`. Two consequences are load-bearing, and both are asserted by
//! tests rather than assumed:
//!
//! - [`crate::Store::export_factset`] — and therefore the published
//!   [`crate::GraphArtifact`] — stays a pure function of the tree **across a
//!   `media build`**, because nothing in this module writes a node or an edge.
//! - No record acquires the `authored` relevance boost that [`crate::search`]
//!   applies, because generated content is ranked in [a separate
//!   channel](crate::search_channels) by a scorer that has no provenance term at
//!   all.
//!
//! Nothing here adds a [`crate::Provenance`] variant.
//!
//! # The boundary is generation, not models
//!
//! | Content | Nature | Verdict |
//! |---|---|---|
//! | Prose, PDF text | deterministic parse | stays `derived` |
//! | **OCR** (`ocrs-text`) | discriminative; decodes text that is *actually present*; its errors are misreadings, correctable against the image | **stays `derived`** |
//! | **ASR transcript** (Voxtral) | generative | **lives here** |
//! | **VLM description** (`SmolVLM`) | generative | **lives here** |
//!
//! OCR has ground truth in the artefact. A transcript of silence has no ground
//! truth to be wrong *against*, and no amount of model improvement changes its
//! kind.
//!
//! # Keying: source blob + producer identity
//!
//! A record is keyed by `(blob_id, producer)`, where the producer is the whole
//! identity of what produced the text — model id and file digest, quantisation,
//! mmproj digest, prompt, and sampling parameters (see [`Producer`]). So
//! re-describing the same blob with a better model writes a **new record, not a
//! mutation**: you can compare the two, and you can discard one producer's output
//! wholesale when you stop trusting it.
//!
//! Records survive [`crate::Store::rebuild`], following the `imports` precedent —
//! they are expensive to reproduce (a 715 MB projector load per blob, issue #301)
//! and are not derivable from source alone.
//!
//! @rto:0015

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::store::StoreError;

pub mod producers;

/// The prefix of every rendered [`ProducerId`].
pub const MEDIA_PRODUCER_PREFIX: &str = "media";

/// Stable schema tag on [`MediaBuildReport`] and [`MediaStatus`], so a
/// programmatic consumer can depend on the shape.
pub const MEDIA_SCHEMA: &str = "roteiro.media/v1";

/// Audio files larger than this (compressed bytes) are not transcribed — decode
/// plus inference time scales with duration, so cap the work one clip imposes.
///
/// Unconditional (not behind `audio-transcribe`) because [`build_media`] applies
/// the cap while *enumerating* candidates, which every build can do.
pub const MAX_AUDIO_BYTES: usize = 50 * 1024 * 1024;

/// Images larger than this (compressed bytes) are not described. Shared with the
/// OCR path in [`crate::extract`], which applies the same cap.
pub const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;

/// Longest permitted prompt, in bytes. A prompt is part of the producer identity
/// and is stored on every row; anything longer is a configuration mistake, not a
/// prompt.
pub const MAX_PROMPT: usize = 4096;

/// Longest permitted model id, in characters — the same bound, and the same
/// character set, the analyzer ids in [`crate::findings`] use, because a model id
/// is likewise a component of a stored, indexed and printed identity.
pub const MAX_MODEL_ID: usize = 64;

/// Errors raised when constructing or building generated media content.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MediaError {
    /// A model id was empty, over-long, or contained a character outside
    /// lowercase `[a-z0-9._-]`.
    #[error(
        "invalid model id {0:?} (expected 1 to {MAX_MODEL_ID} characters of lowercase [a-z0-9._-])"
    )]
    InvalidModelId(String),
    /// A producer field that must be present was empty, or a prompt was longer
    /// than [`MAX_PROMPT`]. The message names the field.
    #[error("invalid producer {field}: {reason}")]
    InvalidProducer {
        /// The offending field.
        field: &'static str,
        /// Why it was refused.
        reason: String,
    },
    /// `media build` was asked for a modality this **binary** cannot produce.
    /// Names the feature that would provide it, because the fix is a rebuild.
    #[error(
        "this build cannot generate {kind} content: rebuild with `--features {feature}` \
         (generated media content is opt-in, so the default build has no producer)"
    )]
    NoProducer {
        /// The modality asked for.
        kind: &'static str,
        /// The cargo feature that provides it.
        feature: &'static str,
    },
    /// The feature is compiled in but the model is not on disk. Names the exact
    /// command that installs it, rather than degrading to silence.
    #[error("model `{model}` is not installed: run `roteiro model pull {model}`")]
    ModelMissing {
        /// Registry name of the missing model.
        model: String,
    },
    /// A stored row could not be interpreted (database corruption).
    #[error("corrupt media record: {0}")]
    Corrupt(String),
}

/// Which generative modality produced a record.
///
/// Only generative modalities appear here: OCR is discriminative and stays on the
/// `derived` extraction path, so it has no variant and cannot acquire one by
/// accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    /// Speech transcription of an audio blob.
    Audio,
    /// A vision-language model's description of an image blob.
    Vision,
}

impl MediaKind {
    /// Stable string token used in the `SQLite` store and in `--json` output.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Audio => "audio",
            Self::Vision => "vision",
        }
    }

    /// Parse a modality from its stable token; `None` for an unrecognised value
    /// (a corrupt row).
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        match s {
            "audio" => Some(Self::Audio),
            "vision" => Some(Self::Vision),
            _ => None,
        }
    }

    /// Whether `path` names a blob this modality can read.
    #[must_use]
    pub fn accepts_path(self, path: &str) -> bool {
        match self {
            Self::Audio => is_audio(path),
            Self::Vision => is_image(path),
        }
    }

    /// The byte cap this modality applies to a candidate blob.
    #[must_use]
    pub fn max_bytes(self) -> usize {
        match self {
            Self::Audio => MAX_AUDIO_BYTES,
            Self::Vision => MAX_IMAGE_BYTES,
        }
    }
}

impl std::fmt::Display for MediaKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether `path` is an audio file the projector's miniaudio decoder can read
/// (WAV/MP3/FLAC — the formats llama.cpp bundles support for).
#[must_use]
pub fn is_audio(path: &str) -> bool {
    matches!(
        crate::extract::extension(path).as_deref(),
        Some("wav" | "mp3" | "flac")
    )
}

/// Whether `path` is an image the OCR and vision paths can read.
#[must_use]
pub fn is_image(path: &str) -> bool {
    matches!(
        crate::extract::extension(path).as_deref(),
        Some("png" | "jpg" | "jpeg")
    )
}

/// Whether a character may appear in a model id.
fn is_model_id_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')
}

/// Whether `id` is a well-formed model id: 1..=[`MAX_MODEL_ID`] characters of
/// lowercase `[a-z0-9._-]`. A `:` is excluded because a model id is a component
/// of a rendered [`ProducerId`].
#[must_use]
pub fn is_valid_model_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_MODEL_ID && id.chars().all(is_model_id_char)
}

/// Everything about *what produced* a piece of generated text — the evidence
/// chain graph provenance was never designed to hold.
///
/// This whole struct is the identity a record is keyed by, via
/// [`Producer::id`]: change the model, its digest, the quantisation, the
/// projector, the prompt or a sampling parameter and you have a different
/// producer, so the next `media build` writes a **new record** rather than
/// overwriting the old one.
///
/// The **tool version** is deliberately *not* part of the identity — it is
/// recorded on the row ([`MediaRecord::tool_version`]) for forensics, but folding
/// it in would invalidate every record on every release without the output having
/// changed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Producer {
    /// Which generative modality this producer covers.
    pub kind: MediaKind,
    /// Registry name of the model (`voxtral-mini-3b`, `smolvlm-500m-gguf`).
    pub model: String,
    /// Digest of the model file as pinned in the registry — the tie between a
    /// record and the exact weights that produced it.
    pub model_digest: String,
    /// Quantisation of those weights (`Q4_K_M`, `Q8_0`, …), read from the pinned
    /// file name; `unknown` when it cannot be determined.
    pub quantisation: String,
    /// Digest of the multimodal projector (`mmproj.gguf`) the model was run with.
    pub mmproj_digest: String,
    /// The prompt the model was given.
    pub prompt: String,
    /// Sampling temperature.
    pub temperature: f64,
    /// Token budget for the generation.
    pub max_tokens: u32,
}

impl Producer {
    /// Validate a producer, refusing an identity that could not be stored or
    /// rendered unambiguously.
    ///
    /// # Errors
    /// Returns [`MediaError::InvalidModelId`] for a malformed model id, or
    /// [`MediaError::InvalidProducer`] naming the field for an empty digest, an
    /// empty or over-long prompt, or a non-finite temperature.
    pub fn validate(&self) -> Result<(), MediaError> {
        if !is_valid_model_id(&self.model) {
            return Err(MediaError::InvalidModelId(self.model.clone()));
        }
        let non_empty = |field: &'static str, value: &str| {
            if value.is_empty() {
                Err(MediaError::InvalidProducer {
                    field,
                    reason: "it is empty".to_owned(),
                })
            } else {
                Ok(())
            }
        };
        non_empty("model_digest", &self.model_digest)?;
        non_empty("quantisation", &self.quantisation)?;
        non_empty("mmproj_digest", &self.mmproj_digest)?;
        non_empty("prompt", &self.prompt)?;
        if self.prompt.len() > MAX_PROMPT {
            return Err(MediaError::InvalidProducer {
                field: "prompt",
                reason: format!(
                    "it is {} bytes, over the {MAX_PROMPT}-byte limit",
                    self.prompt.len()
                ),
            });
        }
        if !self.temperature.is_finite() {
            return Err(MediaError::InvalidProducer {
                field: "temperature",
                reason: format!("{} is not a finite number", self.temperature),
            });
        }
        Ok(())
    }

    /// The identity token this producer's records are keyed by:
    /// `media:<kind>:<model>:<fingerprint>`.
    ///
    /// The fingerprint is a 64-bit FNV-1a fold of the canonical rendering of
    /// *every* identity field, in a fixed order. It is a **handle, not a
    /// digest**: it makes the identity short enough to type at
    /// `media clear --producer <id>`, while the row itself carries all the fields
    /// verbatim, so nothing depends on the fold being collision-free. And because
    /// the kind and the model name are in the token literally, a collision would
    /// additionally require the *same* model, differing only in digest, prompt or
    /// sampling parameters.
    ///
    /// No hash crate is involved deliberately: this is not a security boundary,
    /// and the workspace does not take a dependency for one.
    #[must_use]
    pub fn id(&self) -> ProducerId {
        use std::fmt::Write as _;

        // A length-prefixed, ordered rendering, so two producers cannot fold to
        // the same bytes by moving a `:` from one field into the next.
        let mut canonical = String::new();
        for part in [
            self.kind.as_str(),
            self.model.as_str(),
            self.model_digest.as_str(),
            self.quantisation.as_str(),
            self.mmproj_digest.as_str(),
            self.prompt.as_str(),
        ] {
            // Writing to a `String` is infallible.
            let _ = write!(canonical, "{}:{part}", part.len());
        }
        // `{:?}` on an f64 round-trips exactly, so two distinct temperatures can
        // never render identically.
        let _ = write!(canonical, "t{:?}m{}", self.temperature, self.max_tokens);
        ProducerId(format!(
            "{MEDIA_PRODUCER_PREFIX}:{}:{}:{:016x}",
            self.kind.as_str(),
            self.model,
            fnv1a(canonical.as_bytes())
        ))
    }
}

/// 64-bit FNV-1a. Deterministic, dependency-free, and used only for the
/// [`ProducerId`] handle — see [`Producer::id`] for why that is sufficient.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// A rendered [`Producer`] identity — the token `media clear --producer` takes
/// and `media status` prints.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ProducerId(String);

impl ProducerId {
    /// The token.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ProducerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a [`MediaProducer`] returns for one blob.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GeneratedContent {
    /// The generated text.
    pub text: String,
    /// A confidence signal, when the runtime exposes one. `None` is the honest
    /// answer for both ASR and VLM today — neither emits a calibrated score — and
    /// this is *not* the confidence an `inferred` edge carries; it must never be
    /// read as one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
}

/// One stored record: a blob, the producer that described it, and what it said.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaRecord {
    /// Git blob id of the source media.
    pub blob_id: String,
    /// Repository path the blob was seen at. Evidence, not identity — the same
    /// blob at two paths is one record.
    pub path: String,
    /// The rendered producer identity this record is keyed by.
    pub producer_id: ProducerId,
    /// The full producer identity, verbatim.
    pub producer: Producer,
    /// Version of the tool that wrote the record. Recorded, never part of the
    /// identity — see [`Producer`].
    pub tool_version: String,
    /// How many records this blob had (across all producers) when this one was
    /// written, counting from 1. Lets `media status` show that a blob has been
    /// re-described rather than merely described.
    pub generation: u32,
    /// When the record was written, as `SQLite`'s `datetime('now')`. Written for
    /// humans and for `media status`; no ordering or policy depends on it.
    pub produced_at: String,
    /// The generated text and its confidence signal.
    #[serde(flatten)]
    pub content: GeneratedContent,
}

/// A narrowing filter for [`crate::Store::media_records`]. All-`None` means
/// "every record".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MediaFilter<'a> {
    /// Only records written by this producer id.
    pub producer: Option<&'a str>,
    /// Only records of this modality.
    pub kind: Option<MediaKind>,
    /// Only records for this source blob.
    pub blob_id: Option<&'a str>,
}

/// The values [`crate::Store::record_media_content`] writes.
#[derive(Debug, Clone, Copy)]
pub struct MediaWrite<'a> {
    /// Git blob id of the source media.
    pub blob_id: &'a str,
    /// Repository path the blob was seen at.
    pub path: &'a str,
    /// Who produced the text.
    pub producer: &'a Producer,
    /// Version of the tool doing the writing.
    pub tool_version: &'a str,
    /// The text and its confidence signal.
    pub content: &'a GeneratedContent,
    /// Replace an existing record for this exact `(blob, producer)` instead of
    /// leaving it alone. Only `media build --force` sets this: a *different*
    /// producer never mutates, it writes a new record.
    pub replace: bool,
}

/// What one producer has in the store, for `media status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProducerSummary {
    /// The producer identity.
    pub producer_id: ProducerId,
    /// Its modality.
    pub kind: MediaKind,
    /// The model it ran.
    pub model: String,
    /// Its quantisation.
    pub quantisation: String,
    /// How many records it owns.
    pub records: u64,
    /// The most recent `produced_at` among them.
    pub latest: String,
}

/// The `media status` report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaStatus {
    /// Stable schema tag ([`MEDIA_SCHEMA`]).
    pub schema: &'static str,
    /// Total stored records.
    pub records: u64,
    /// One entry per producer, ordered by producer id.
    pub producers: Vec<ProducerSummary>,
    /// Media blobs in the current tree, by modality — the denominator a rebuild
    /// would work against.
    pub candidates: Vec<CandidateCount>,
    /// Producers this **binary** could run right now, ordered by id. Empty in a
    /// build with no media features, or with no model installed; that is what
    /// makes "0 records" legible as *cannot generate* rather than *nothing to
    /// generate*.
    pub available_producers: Vec<ProducerSummaryAvailable>,
}

/// How many blobs of one modality the current tree holds, and how many of them
/// already have a record for *some* producer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateCount {
    /// The modality.
    pub kind: MediaKind,
    /// Distinct media blobs in the tree (within the size cap).
    pub blobs: u64,
    /// How many of those have at least one record.
    pub described: u64,
}

/// A producer this binary could run, as reported by `media status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProducerSummaryAvailable {
    /// The producer identity it would write under.
    pub producer_id: ProducerId,
    /// Its modality.
    pub kind: MediaKind,
    /// The model it would run.
    pub model: String,
    /// Whether the store already holds records for exactly this identity — so a
    /// caller can see at a glance that a rebuild would produce *new* records
    /// because the installed model moved.
    pub current: bool,
}

// --- Building --------------------------------------------------------------

/// One modality's generator. The seam exists so the orchestration in
/// [`build_media`] — incrementality, idempotence, `--force`, per-blob dedup — is
/// testable without a 3 GB model and without a GPU, which is what CI actually
/// runs.
pub trait MediaProducer {
    /// The identity every record this producer writes is keyed by.
    fn producer(&self) -> &Producer;

    /// Generate content for one blob, or `None` when the model declines to
    /// produce anything usable. `path` is passed for logging and for producers
    /// that need the extension; the identity never includes it.
    fn generate(&self, path: &str, bytes: &[u8]) -> Option<GeneratedContent>;
}

/// Which modalities a `media build` should run, and whether to redo work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaBuildOptions {
    /// Generate audio transcripts.
    pub audio: bool,
    /// Generate image descriptions.
    pub vision: bool,
    /// Regenerate even where a record already exists for the current producer,
    /// replacing it in place. Without this, `build` is incremental: a second run
    /// with the same producer does no work.
    pub force: bool,
}

impl Default for MediaBuildOptions {
    /// Both modalities, incremental.
    fn default() -> Self {
        Self {
            audio: true,
            vision: true,
            force: false,
        }
    }
}

impl MediaBuildOptions {
    /// Whether `kind` is requested.
    #[must_use]
    pub fn wants(self, kind: MediaKind) -> bool {
        match kind {
            MediaKind::Audio => self.audio,
            MediaKind::Vision => self.vision,
        }
    }
}

/// What one `media build` did.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MediaBuildReport {
    /// Stable schema tag ([`MEDIA_SCHEMA`]).
    #[serde(default = "media_schema")]
    pub schema: &'static str,
    /// Distinct `(blob, producer)` pairs considered.
    pub candidates: usize,
    /// Records written (new, or replaced under `--force`).
    pub generated: usize,
    /// Pairs skipped because a record for that exact producer already existed —
    /// the number that makes incrementality visible.
    pub skipped_existing: usize,
    /// Pairs where the model was invoked and returned nothing usable.
    pub empty: usize,
    /// Producer ids that ran, ordered.
    pub producers: Vec<ProducerId>,
}

/// Default for [`MediaBuildReport::schema`] when deserialising.
fn media_schema() -> &'static str {
    MEDIA_SCHEMA
}

/// One candidate blob: a media file in the tree, within its modality's cap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaBlob {
    /// Git blob id.
    pub blob_id: String,
    /// Repository path.
    pub path: String,
    /// Which modality can read it.
    pub kind: MediaKind,
}

/// Every media blob in `repo`'s `HEAD` tree that some modality can read and that
/// is within that modality's byte cap, de-duplicated by `(blob id, kind)` and
/// ordered by `(kind, blob id)` so a build is deterministic.
///
/// The same blob committed at two paths is **one** candidate: the record is keyed
/// by blob, so describing it twice would be work for one row. The lexically
/// first path is the one recorded.
///
/// # Errors
/// Returns [`crate::GitError`] if the tree cannot be walked or a blob cannot be
/// read.
pub fn media_blobs(repo: &crate::Repo) -> Result<Vec<MediaBlob>, crate::GitError> {
    let mut blobs = repo.walk_blobs()?;
    // Sort by path so "the lexically first path wins" is a fact, not an accident
    // of the walk order.
    blobs.sort_by(|a, b| a.path.cmp(&b.path));
    let mut seen: std::collections::BTreeSet<(MediaKind, String)> =
        std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for blob in blobs {
        for kind in [MediaKind::Audio, MediaKind::Vision] {
            if !kind.accepts_path(&blob.path) {
                continue;
            }
            if seen.contains(&(kind, blob.oid.clone())) {
                continue;
            }
            // The size cap is applied here, before any model is loaded: an
            // oversized clip is refused rather than partially transcribed.
            let bytes = repo.read_blob(&blob.oid)?;
            if bytes.len() > kind.max_bytes() {
                continue;
            }
            seen.insert((kind, blob.oid.clone()));
            out.push(MediaBlob {
                blob_id: blob.oid.clone(),
                path: blob.path.clone(),
                kind,
            });
        }
    }
    out.sort_by(|a, b| (a.kind, &a.blob_id).cmp(&(b.kind, &b.blob_id)));
    Ok(out)
}

/// Generate content for every candidate blob that has no record for the current
/// producer, writing one record per `(blob, producer)`.
///
/// **Incremental by default and idempotent**: a second run with the same
/// producers does no work at all — every pair lands in
/// [`MediaBuildReport::skipped_existing`] and no model is invoked. A producer
/// whose identity changed (a new model, a different quantisation, an edited
/// prompt) has a different [`Producer::id`], so its pairs are *not* skipped and
/// its output is a **new record beside the old one**, never an overwrite. Only
/// [`MediaBuildOptions::force`] replaces, and only for the identical producer.
///
/// `read` supplies a blob's bytes — [`crate::Repo::read_blob`] in production, a
/// closure in tests.
///
/// Nothing here writes a node or an edge.
///
/// # Errors
/// Returns [`StoreError`] on a store failure; a producer that fails to generate
/// contributes to [`MediaBuildReport::empty`] rather than aborting the build, so
/// one bad blob cannot lose the whole run's work.
pub fn build_media<F>(
    store: &mut crate::Store,
    blobs: &[MediaBlob],
    producers: &[&dyn MediaProducer],
    opts: MediaBuildOptions,
    mut read: F,
) -> Result<MediaBuildReport, StoreError>
where
    F: FnMut(&MediaBlob) -> Option<Vec<u8>>,
{
    let tool_version = env!("CARGO_PKG_VERSION");
    let mut report = MediaBuildReport {
        schema: MEDIA_SCHEMA,
        ..MediaBuildReport::default()
    };
    let mut ids: Vec<ProducerId> = producers.iter().map(|p| p.producer().id()).collect();
    ids.sort();
    ids.dedup();
    report.producers = ids;

    for producer in producers {
        let identity = producer.producer();
        let id = identity.id();
        for blob in blobs {
            if blob.kind != identity.kind || !opts.wants(blob.kind) {
                continue;
            }
            report.candidates += 1;
            // The incrementality decision, made *before* the bytes are read and
            // long before a model is loaded.
            if !opts.force && store.has_media_record(&blob.blob_id, id.as_str())? {
                report.skipped_existing += 1;
                continue;
            }
            let Some(bytes) = read(blob) else {
                report.empty += 1;
                continue;
            };
            let Some(content) = producer.generate(&blob.path, &bytes) else {
                report.empty += 1;
                continue;
            };
            if content.text.trim().is_empty() {
                report.empty += 1;
                continue;
            }
            let written = store.record_media_content(&MediaWrite {
                blob_id: &blob.blob_id,
                path: &blob.path,
                producer: identity,
                tool_version,
                content: &content,
                replace: opts.force,
            })?;
            if written {
                report.generated += 1;
            } else {
                report.skipped_existing += 1;
            }
        }
    }
    Ok(report)
}

/// Assemble the `media status` report: what is stored, by which producer, and
/// how it compares with the media blobs actually in the tree.
///
/// `blobs` is the current candidate set (see [`media_blobs`]); pass an empty
/// slice to report on the store alone.
///
/// # Errors
/// Returns [`StoreError`] on a store failure.
pub fn status(store: &crate::Store, blobs: &[MediaBlob]) -> Result<MediaStatus, StoreError> {
    let mut candidates = Vec::new();
    for kind in [MediaKind::Audio, MediaKind::Vision] {
        let described_ids = store.described_media_blobs(kind)?;
        let in_tree: std::collections::BTreeSet<&str> = blobs
            .iter()
            .filter(|b| b.kind == kind)
            .map(|b| b.blob_id.as_str())
            .collect();
        let described = in_tree
            .iter()
            .filter(|id| described_ids.contains(**id))
            .count();
        candidates.push(CandidateCount {
            kind,
            blobs: u64::try_from(in_tree.len()).unwrap_or(u64::MAX),
            described: u64::try_from(described).unwrap_or(u64::MAX),
        });
    }
    let stored = store.media_producer_summaries()?;
    let mut available_producers: Vec<ProducerSummaryAvailable> = producers::available()
        .into_iter()
        .map(|p| {
            let producer_id = p.id();
            ProducerSummaryAvailable {
                current: stored.iter().any(|s| s.producer_id == producer_id),
                producer_id,
                kind: p.kind,
                model: p.model,
            }
        })
        .collect();
    available_producers.sort_by(|a, b| a.producer_id.cmp(&b.producer_id));
    Ok(MediaStatus {
        schema: MEDIA_SCHEMA,
        records: store.media_content_count()?,
        producers: stored,
        candidates,
        available_producers,
    })
}

// --- Persistence. Free helpers over a `Connection` (a `Transaction` derefs to
// one), mirroring the findings store. Every statement here touches
// `media_content` and nothing else: nothing in this module reads or writes
// `nodes` or `edges`. ---

/// Columns of `media_content`, in the order [`record_from_row`] decodes them.
const RECORD_COLS: &str = "m.blob_id, m.path, m.kind, m.producer, m.model, m.model_digest, \
     m.quantisation, m.mmproj_digest, m.prompt, m.temperature, m.max_tokens, \
     m.tool_version, m.generation, m.produced_at, m.text, m.confidence";

/// Write one record, returning whether a row was written. See [`MediaWrite`].
pub(crate) fn record(conn: &Connection, write: &MediaWrite<'_>) -> Result<bool, StoreError> {
    let id = write.producer.id();
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM media_content WHERE blob_id = ?1 AND producer = ?2",
            params![write.blob_id, id.as_str()],
            |r| r.get(0),
        )
        .optional()?;
    match (existing, write.replace) {
        // Already described by this exact producer, and not forced: leave it.
        // This is what makes a second `media build` free.
        (Some(_), false) => return Ok(false),
        (Some(row), true) => {
            conn.execute("DELETE FROM media_content WHERE id = ?1", [row])?;
        }
        (None, _) => {}
    }
    // The generation counter is per *blob*, not per producer: it answers "has
    // this blob been described before, by anyone?".
    let prior: i64 = conn.query_row(
        "SELECT COUNT(*) FROM media_content WHERE blob_id = ?1",
        [write.blob_id],
        |r| r.get(0),
    )?;
    let generation = u32::try_from(prior + 1).unwrap_or(u32::MAX);
    conn.execute(
        "INSERT INTO media_content (
             blob_id, path, kind, producer, model, model_digest, quantisation, mmproj_digest,
             prompt, temperature, max_tokens, tool_version, generation, text, confidence
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            write.blob_id,
            write.path,
            write.producer.kind.as_str(),
            id.as_str(),
            write.producer.model,
            write.producer.model_digest,
            write.producer.quantisation,
            write.producer.mmproj_digest,
            write.producer.prompt,
            write.producer.temperature,
            write.producer.max_tokens,
            write.tool_version,
            generation,
            write.content.text,
            write.content.confidence,
        ],
    )?;
    Ok(true)
}

/// Whether a record exists for exactly this `(blob, producer)`.
pub(crate) fn exists(conn: &Connection, blob_id: &str, producer: &str) -> Result<bool, StoreError> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM media_content WHERE blob_id = ?1 AND producer = ?2",
        params![blob_id, producer],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Records matching `filter`, ordered by `(producer, blob_id)` so output is
/// deterministic.
pub(crate) fn records(
    conn: &Connection,
    filter: &MediaFilter<'_>,
) -> Result<Vec<MediaRecord>, StoreError> {
    let mut where_parts: Vec<&str> = Vec::new();
    let mut bound: Vec<String> = Vec::new();
    if let Some(producer) = filter.producer {
        where_parts.push("m.producer = ?");
        bound.push(producer.to_owned());
    }
    if let Some(kind) = filter.kind {
        where_parts.push("m.kind = ?");
        bound.push(kind.as_str().to_owned());
    }
    if let Some(blob) = filter.blob_id {
        where_parts.push("m.blob_id = ?");
        bound.push(blob.to_owned());
    }
    let clause = if where_parts.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", where_parts.join(" AND "))
    };
    let sql =
        format!("SELECT {RECORD_COLS} FROM media_content m{clause} ORDER BY m.producer, m.blob_id");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(rusqlite::params_from_iter(bound))?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(record_from_row(row)?);
    }
    Ok(out)
}

/// Delete every record, or only one producer's. Returns how many rows went.
pub(crate) fn delete(conn: &Connection, producer: Option<&str>) -> Result<usize, StoreError> {
    let removed = match producer {
        Some(id) => conn.execute("DELETE FROM media_content WHERE producer = ?1", [id])?,
        None => conn.execute("DELETE FROM media_content", [])?,
    };
    Ok(removed)
}

/// Total number of stored records.
pub(crate) fn count(conn: &Connection) -> Result<u64, StoreError> {
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM media_content", [], |r| r.get(0))?;
    Ok(u64::try_from(n).unwrap_or(0))
}

/// One summary row per producer, ordered by producer id.
pub(crate) fn producer_summaries(conn: &Connection) -> Result<Vec<ProducerSummary>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT producer, kind, model, quantisation, COUNT(*), MAX(produced_at)
         FROM media_content GROUP BY producer, kind, model, quantisation ORDER BY producer",
    )?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let kind_token: String = row.get(1)?;
        let kind = MediaKind::from_token(&kind_token)
            .ok_or_else(|| StoreError::Corrupt(format!("unknown media kind: {kind_token}")))?;
        let records: i64 = row.get(4)?;
        out.push(ProducerSummary {
            producer_id: ProducerId(row.get(0)?),
            kind,
            model: row.get(2)?,
            quantisation: row.get(3)?,
            records: u64::try_from(records).unwrap_or(0),
            latest: row.get(5)?,
        });
    }
    Ok(out)
}

/// The set of blob ids that have at least one record, for a modality.
pub(crate) fn described_blobs(
    conn: &Connection,
    kind: MediaKind,
) -> Result<std::collections::BTreeSet<String>, StoreError> {
    let mut stmt = conn
        .prepare("SELECT DISTINCT blob_id FROM media_content WHERE kind = ?1 ORDER BY blob_id")?;
    let mut rows = stmt.query([kind.as_str()])?;
    let mut out = std::collections::BTreeSet::new();
    while let Some(row) = rows.next()? {
        out.insert(row.get::<_, String>(0)?);
    }
    Ok(out)
}

/// Decode a `media_content` row.
fn record_from_row(row: &rusqlite::Row<'_>) -> Result<MediaRecord, StoreError> {
    let kind_token: String = row.get(2)?;
    let kind = MediaKind::from_token(&kind_token)
        .ok_or_else(|| StoreError::Corrupt(format!("unknown media kind: {kind_token}")))?;
    let generation: i64 = row.get(12)?;
    Ok(MediaRecord {
        blob_id: row.get(0)?,
        path: row.get(1)?,
        producer_id: ProducerId(row.get(3)?),
        producer: Producer {
            kind,
            model: row.get(4)?,
            model_digest: row.get(5)?,
            quantisation: row.get(6)?,
            mmproj_digest: row.get(7)?,
            prompt: row.get(8)?,
            temperature: row.get(9)?,
            max_tokens: row.get(10)?,
        },
        tool_version: row.get(11)?,
        generation: u32::try_from(generation).unwrap_or(u32::MAX),
        produced_at: row.get(13)?,
        content: GeneratedContent {
            text: row.get(14)?,
            confidence: row.get(15)?,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::{
        GeneratedContent, MAX_MODEL_ID, MAX_PROMPT, MediaError, MediaKind, Producer,
        is_valid_model_id,
    };

    fn producer() -> Producer {
        Producer {
            kind: MediaKind::Audio,
            model: "voxtral-mini-3b".to_owned(),
            model_digest: "4705be8e".to_owned(),
            quantisation: "Q4_K_M".to_owned(),
            mmproj_digest: "4f24c4ef".to_owned(),
            prompt: "Transcribe this audio recording.".to_owned(),
            temperature: 0.0,
            max_tokens: 512,
        }
    }

    #[test]
    fn a_producer_id_names_its_modality_and_model() {
        let id = producer().id();
        assert!(
            id.as_str().starts_with("media:audio:voxtral-mini-3b:"),
            "got {id}"
        );
        // Stable across calls — the identity is a pure function of the fields.
        assert_eq!(producer().id(), producer().id());
    }

    /// Every identity field must move the id. This is the property the whole
    /// store rests on: if changing the quantisation left the id alone, a
    /// re-describe would silently *skip* instead of writing a new record.
    #[test]
    fn every_identity_field_changes_the_producer_id() {
        /// A named single-field edit to a [`Producer`].
        type Mutation = (&'static str, fn(&mut Producer));

        let base = producer().id();
        let mutate: [Mutation; 7] = [
            ("kind", |p| p.kind = MediaKind::Vision),
            ("model", |p| p.model = "smolvlm-500m-gguf".to_owned()),
            ("model_digest", |p| p.model_digest = "deadbeef".to_owned()),
            ("quantisation", |p| p.quantisation = "Q8_0".to_owned()),
            ("mmproj_digest", |p| p.mmproj_digest = "cafebabe".to_owned()),
            ("prompt", |p| p.prompt = "Describe this.".to_owned()),
            ("temperature", |p| p.temperature = 0.2),
        ];
        for (field, apply) in mutate {
            let mut p = producer();
            apply(&mut p);
            assert_ne!(p.id(), base, "changing {field} must change the producer id");
        }
        // …including `max_tokens`, which the table above cannot express because it
        // is not a `String` field.
        let mut p = producer();
        p.max_tokens = 256;
        assert_ne!(
            p.id(),
            base,
            "changing max_tokens must change the producer id"
        );
    }

    /// The canonical rendering is length-prefixed, so no field can borrow a
    /// character from its neighbour to impersonate a different identity.
    #[test]
    fn adjacent_fields_cannot_be_confused() {
        let mut a = producer();
        a.model_digest = "ab".to_owned();
        a.quantisation = "cd".to_owned();
        let mut b = producer();
        b.model_digest = "abc".to_owned();
        b.quantisation = "d".to_owned();
        assert_ne!(a.id(), b.id());
    }

    #[test]
    fn model_ids_accept_the_registry_names_and_reject_separators() {
        assert!(is_valid_model_id("voxtral-mini-3b"));
        assert!(is_valid_model_id("smolvlm-500m-gguf"));
        assert!(!is_valid_model_id(""));
        // A `:` would make a producer id ambiguous.
        assert!(!is_valid_model_id("a:b"));
        assert!(!is_valid_model_id("Voxtral"));
        assert!(is_valid_model_id(&"a".repeat(MAX_MODEL_ID)));
        assert!(!is_valid_model_id(&"a".repeat(MAX_MODEL_ID + 1)));
    }

    #[test]
    fn validation_names_the_field_it_refused() {
        assert!(producer().validate().is_ok());

        let mut bad = producer();
        bad.model = "Voxtral".to_owned();
        assert_eq!(
            bad.validate(),
            Err(MediaError::InvalidModelId("Voxtral".to_owned()))
        );

        for (field, apply) in [
            (
                "model_digest",
                (|p: &mut Producer| p.model_digest.clear()) as fn(&mut Producer),
            ),
            ("quantisation", |p: &mut Producer| p.quantisation.clear()),
            ("mmproj_digest", |p: &mut Producer| p.mmproj_digest.clear()),
            ("prompt", |p: &mut Producer| p.prompt.clear()),
        ] {
            let mut p = producer();
            apply(&mut p);
            let err = p.validate().expect_err("empty field must be refused");
            assert!(
                err.to_string().contains(field),
                "the rejection must name {field}: {err}"
            );
        }

        let mut long = producer();
        long.prompt = "x".repeat(MAX_PROMPT + 1);
        assert!(
            long.validate()
                .expect_err("over-long prompt")
                .to_string()
                .contains("over the")
        );

        let mut nan = producer();
        nan.temperature = f64::NAN;
        assert!(
            nan.validate()
                .expect_err("NaN temperature")
                .to_string()
                .contains("finite")
        );
    }

    #[test]
    fn media_kind_tokens_round_trip() {
        for kind in [MediaKind::Audio, MediaKind::Vision] {
            assert_eq!(MediaKind::from_token(kind.as_str()), Some(kind));
        }
        // OCR is not a generative modality, so it has no token here — the
        // boundary of ADR-0015 expressed as a type.
        assert_eq!(MediaKind::from_token("ocr"), None);
        assert_eq!(MediaKind::from_token("nope"), None);
    }

    #[test]
    fn modalities_accept_only_their_own_extensions() {
        assert!(MediaKind::Audio.accepts_path("a/clip.wav"));
        assert!(MediaKind::Audio.accepts_path("a/clip.MP3"));
        assert!(MediaKind::Audio.accepts_path("a/clip.flac"));
        assert!(!MediaKind::Audio.accepts_path("a/clip.ogg"));
        assert!(!MediaKind::Audio.accepts_path("a/clip.wav.bak"));
        assert!(MediaKind::Vision.accepts_path("a/x.png"));
        assert!(MediaKind::Vision.accepts_path("a/x.jpeg"));
        assert!(!MediaKind::Vision.accepts_path("a/x.gif"));
        // No modality claims a document.
        assert!(!MediaKind::Audio.accepts_path("a/x.md"));
        assert!(!MediaKind::Vision.accepts_path("a/x.md"));
    }

    #[test]
    fn generated_content_omits_an_absent_confidence() {
        let bare = GeneratedContent {
            text: "hello".to_owned(),
            confidence: None,
        };
        assert_eq!(
            serde_json::to_string(&bare).expect("serialize"),
            r#"{"text":"hello"}"#
        );
    }
}

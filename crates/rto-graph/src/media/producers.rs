//! The concrete generators behind [`MediaProducer`]: a vision-language model and
//! an audio-capable model, both reached through the provider engine (ADR-0033).
//!
//! Everything here is feature-gated: the default build has no generator at all,
//! and [`installed`] then returns a **named, actionable** error rather than
//! quietly producing nothing (ADR-0015). That matters more than it sounds:
//! before ADR-0015 an absent model and a blob with nothing to say were
//! indistinguishable, because both produced no content.
//!
//! # The engine is injected
//!
//! The provider engine lives in the `roteiro` crate, which depends on this one,
//! so it cannot be built here. `roteiro` hands one over with `provide` before
//! `media status` or `media build`, together with the model each modality uses
//! (`[models] audio` / `[models] vision`). Without that, no producer is offered,
//! and `media build` says what to configure.

#[cfg(any(feature = "audio-transcribe", feature = "image-vision"))]
use super::GeneratedContent;
use super::{MediaBuildOptions, MediaError, MediaKind, MediaProducer, Producer};

#[cfg(any(feature = "audio-transcribe", feature = "image-vision"))]
/// What `roteiro` supplies so a modality can run: the engine, the host it talks
/// to (recorded in the producer identity) and the provider model per modality.
pub struct MediaEngine {
    /// The provider engine every generation call goes through.
    pub engine: std::sync::Arc<dyn rto_llama::Engine>,
    /// The provider's host, e.g. `localhost:8080` — part of the identity, so the
    /// same model name on two servers writes two sets of records.
    pub host: String,
    /// The provider model id for transcription, from `[models] audio`.
    pub audio_model: Option<String>,
    /// The provider model id for image description, from `[models] vision`.
    pub vision_model: Option<String>,
}

#[cfg(any(feature = "audio-transcribe", feature = "image-vision"))]
/// The engine `roteiro` provided for this process, if any.
static ENGINE: std::sync::OnceLock<MediaEngine> = std::sync::OnceLock::new();

#[cfg(any(feature = "audio-transcribe", feature = "image-vision"))]
/// Supply the engine and models for this process. The first call wins; later
/// calls are ignored, since one process serves one configuration.
pub fn provide(engine: MediaEngine) {
    let _ = ENGINE.set(engine);
}

/// The prompt the ASR model is given. Part of the producer identity, so editing
/// it makes the next `media build` write new records beside the old ones rather
/// than silently changing what an existing record claims.
#[cfg(feature = "audio-transcribe")]
const ASR_PROMPT: &str = "Transcribe this audio recording. Output only the spoken words, verbatim.";

/// The prompt the vision model is given. Part of the producer identity.
#[cfg(feature = "image-vision")]
const VLM_PROMPT: &str = "Describe this image in one or two sentences.";

/// Sampling temperature for both generators: greedy, so a re-run with the same
/// producer is as reproducible as the provider allows.
#[cfg(any(feature = "audio-transcribe", feature = "image-vision"))]
const TEMPERATURE: f64 = 0.0;

/// Token budget for a transcript.
#[cfg(feature = "audio-transcribe")]
const ASR_MAX_TOKENS: u32 = 512;

/// Token budget for a description.
#[cfg(feature = "image-vision")]
const VLM_MAX_TOKENS: u32 = 128;

/// The producers this binary can run right now: one per modality that is both
/// compiled in and configured, ordered `audio` then `vision`.
///
/// Never errors — an empty result is the honest answer for a default build, and
/// `media status` reports it as such so "0 records" reads as *cannot generate*
/// rather than *nothing to generate*. [`installed`] is the erroring variant, for
/// `media build`, which was asked to do something specific.
#[must_use]
pub fn available() -> Vec<Producer> {
    let mut out = Vec::new();
    for kind in [MediaKind::Audio, MediaKind::Vision] {
        if kind.compiled_in()
            && let Some(p) = producer_for(kind)
        {
            out.push(p);
        }
    }
    out
}

/// The producers needed for `opts`, or the first reason one is unavailable.
///
/// # Errors
/// Returns [`MediaError::NoProducer`] when the modality was not compiled in
/// (naming the cargo feature that provides it), or [`MediaError::NoModel`] when
/// it was but no provider model is configured for it (naming the key to set).
pub fn installed(opts: MediaBuildOptions) -> Result<Vec<Box<dyn MediaProducer>>, MediaError> {
    let mut out: Vec<Box<dyn MediaProducer>> = Vec::new();
    if opts.audio {
        out.push(producer_box(MediaKind::Audio, "audio-transcribe")?);
    }
    if opts.vision {
        out.push(producer_box(MediaKind::Vision, "image-vision")?);
    }
    Ok(out)
}

/// The generator for `kind`, or why there isn't one.
fn producer_box(
    kind: MediaKind,
    feature: &'static str,
) -> Result<Box<dyn MediaProducer>, MediaError> {
    if !kind.compiled_in() {
        return Err(MediaError::NoProducer {
            kind: kind.as_str(),
            feature,
        });
    }
    let producer = producer_for(kind).ok_or(MediaError::NoModel {
        kind: kind.as_str(),
    })?;
    Ok(engine_producer(producer))
}

#[cfg(any(feature = "audio-transcribe", feature = "image-vision"))]
fn engine_producer(producer: Producer) -> Box<dyn MediaProducer> {
    Box::new(EngineProducer { producer })
}

#[cfg(not(any(feature = "audio-transcribe", feature = "image-vision")))]
fn engine_producer(_producer: Producer) -> Box<dyn MediaProducer> {
    unreachable!("no modality is compiled in, so producer_box refused first")
}

/// The identity `kind` would write under, or `None` when no engine was provided
/// or no model is configured for it.
///
/// A provider exposes no weights digest, quantisation or projector, so those
/// three fields carry the sentinel `provider:<host>/<model>` (owner decision,
/// ADR-0033): the exact provider model and the server it ran on are what
/// identify the output. The record's `model` is the provider id folded into the
/// `[a-z0-9._-]` alphabet a [`super::ProducerId`] component allows; the digest
/// keeps it verbatim, so folding cannot merge two models.
#[cfg(any(feature = "audio-transcribe", feature = "image-vision"))]
fn producer_for(kind: MediaKind) -> Option<Producer> {
    let provided = ENGINE.get()?;
    let model = match kind {
        MediaKind::Audio => provided.audio_model.as_deref(),
        MediaKind::Vision => provided.vision_model.as_deref(),
    }?;
    let (prompt, max_tokens) = prompt_for(kind)?;
    let sentinel = format!("provider:{}/{model}", provided.host);
    Some(Producer {
        kind,
        model: fold_model_id(model),
        model_digest: sentinel.clone(),
        quantisation: "provider".to_owned(),
        mmproj_digest: sentinel,
        prompt: prompt.to_owned(),
        temperature: prompt_temperature(),
        max_tokens,
    })
}

#[cfg(any(feature = "audio-transcribe", feature = "image-vision"))]
const fn prompt_temperature() -> f64 {
    TEMPERATURE
}

#[cfg(any(feature = "audio-transcribe", feature = "image-vision"))]
/// The prompt and token budget for `kind`, when its feature is compiled in.
fn prompt_for(kind: MediaKind) -> Option<(&'static str, u32)> {
    match kind {
        #[cfg(feature = "audio-transcribe")]
        MediaKind::Audio => Some((ASR_PROMPT, ASR_MAX_TOKENS)),
        #[cfg(feature = "image-vision")]
        MediaKind::Vision => Some((VLM_PROMPT, VLM_MAX_TOKENS)),
        #[allow(
            unreachable_patterns,
            reason = "reachable exactly when a modality's feature is off"
        )]
        _ => None,
    }
}

/// No modality is compiled in, so nothing can be offered.
#[cfg(not(any(feature = "audio-transcribe", feature = "image-vision")))]
fn producer_for(_kind: MediaKind) -> Option<Producer> {
    None
}

/// A provider model id folded into the [`super::MAX_MODEL_ID`]-bounded
/// `[a-z0-9._-]` alphabet: lowercased, every other character replaced by `-`.
#[cfg(any(feature = "audio-transcribe", feature = "image-vision"))]
fn fold_model_id(model: &str) -> String {
    let folded: String = model
        .chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .take(super::MAX_MODEL_ID)
        .collect();
    if folded.is_empty() {
        "provider".to_owned()
    } else {
        folded
    }
}

/// A generator that sends the blob to the provider as an image or an
/// `input_audio` chat part (ADR-0033) and records the reply.
#[cfg(any(feature = "audio-transcribe", feature = "image-vision"))]
struct EngineProducer {
    producer: Producer,
}

#[cfg(any(feature = "audio-transcribe", feature = "image-vision"))]
impl MediaProducer for EngineProducer {
    fn producer(&self) -> &Producer {
        &self.producer
    }

    fn generate(&self, _path: &str, bytes: &[u8]) -> Option<GeneratedContent> {
        // An image whose header does not parse, or whose pixel count is over the
        // cap, is refused before it is sent — the same guard the OCR path
        // applies, for the same decompression-bomb reason.
        #[cfg(feature = "image-vision")]
        if self.producer.kind == MediaKind::Vision && !crate::extract::image_dimensions_ok(bytes) {
            return None;
        }
        let provided = ENGINE.get()?;
        let model = match self.producer.kind {
            MediaKind::Audio => provided.audio_model.clone(),
            MediaKind::Vision => provided.vision_model.clone(),
        }?;
        let (images, audio) = match self.producer.kind {
            MediaKind::Vision => (vec![bytes.to_vec()], Vec::new()),
            MediaKind::Audio => (Vec::new(), vec![bytes.to_vec()]),
        };
        let completion = provided
            .engine
            .chat(&rto_llama::ChatRequest {
                tools: None,
                model,
                messages: vec![rto_llama::Message {
                    role: "user".to_owned(),
                    content: self.producer.prompt.clone(),
                }],
                images,
                audio,
                // The identity stores the temperature as `f64` (a `REAL` column
                // and a JSON number); the engine takes `f32`. The stored value
                // stays the wide one, so `Producer::id` never depends on this.
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "engine API is f32; the stored identity keeps the f64"
                )]
                temperature: self.producer.temperature as f32,
                max_tokens: self.producer.max_tokens,
            })
            .ok()?;
        let text = completion.content.trim();
        (!text.is_empty()).then(|| GeneratedContent {
            text: text.to_owned(),
            // A provider exposes no calibrated confidence; recording `None` is
            // the honest answer (ADR-0015).
            confidence: None,
        })
    }
}

#[cfg(test)]
mod tests {
    #[cfg(not(all(feature = "audio-transcribe", feature = "image-vision")))]
    use super::MediaError;
    use super::{MediaBuildOptions, available, installed};

    /// The refusal, as a value. `Box<dyn MediaProducer>` is not `Debug`, so
    /// `expect_err` is not available.
    #[cfg(not(all(feature = "audio-transcribe", feature = "image-vision")))]
    fn refusal(opts: MediaBuildOptions) -> MediaError {
        match installed(opts) {
            Err(e) => e,
            Ok(ok) => panic!("expected a refusal, got {} producer(s)", ok.len()),
        }
    }

    #[test]
    #[cfg(not(feature = "audio-transcribe"))]
    fn an_audio_build_without_the_feature_is_refused_by_name() {
        let err = refusal(MediaBuildOptions {
            audio: true,
            vision: false,
            ..MediaBuildOptions::default()
        });
        assert_eq!(
            err,
            MediaError::NoProducer {
                kind: "audio",
                feature: "audio-transcribe",
            }
        );
        assert!(
            err.to_string().contains("--features audio-transcribe"),
            "the message must name the rebuild: {err}"
        );
    }

    #[test]
    #[cfg(not(feature = "image-vision"))]
    fn a_vision_build_without_the_feature_is_refused_by_name() {
        let err = refusal(MediaBuildOptions {
            audio: false,
            vision: true,
            ..MediaBuildOptions::default()
        });
        assert!(
            err.to_string().contains("--features image-vision"),
            "the message must name the rebuild: {err}"
        );
    }

    /// Asking for nothing is not an error in any build.
    #[test]
    fn requesting_no_modality_needs_no_producer() {
        let none = installed(MediaBuildOptions {
            audio: false,
            vision: false,
            ..MediaBuildOptions::default()
        })
        .expect("asking for nothing cannot fail");
        assert!(none.is_empty());
    }

    /// With no engine provided, nothing is offered, in any build.
    #[test]
    fn no_engine_offers_no_producer() {
        assert!(available().is_empty());
    }

    /// A provider id with characters a producer id cannot carry is folded, never
    /// refused: `qwen3:8b` and `hf.co/x/Y` still name a valid record.
    #[test]
    #[cfg(any(feature = "audio-transcribe", feature = "image-vision"))]
    fn provider_ids_fold_into_the_model_id_alphabet() {
        use super::fold_model_id;
        assert_eq!(fold_model_id("qwen3:8b"), "qwen3-8b");
        assert_eq!(fold_model_id("hf.co/x/Voxtral"), "hf.co-x-voxtral");
        assert!(crate::media::is_valid_model_id(&fold_model_id(
            &"x".repeat(200)
        )));
    }
}

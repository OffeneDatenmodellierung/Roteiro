//! The concrete generators behind [`MediaProducer`](super::MediaProducer), and
//! the process-wide llama.cpp engines they load.
//!
//! Everything here is feature-gated: the default build has no generator at all,
//! and [`installed`] then returns a **named, actionable** error rather than
//! quietly producing nothing (ADR-0015; `BUILD_PLAN_V2` principle 10 —
//! "offline-capable, not offline"). That matters more than it sounds: before
//! ADR-0015 an absent model and a blob with nothing to say were indistinguishable,
//! because both produced no content.
//!
//! The engines moved here from [`crate::extract`] with the generative content
//! itself: after ADR-0015 nothing on the extraction path loads a GGUF model, so
//! `sync` never touches llama.cpp. Their **release** is still driven from
//! `extract` ([`crate::release_media_engines`]), which is the public entry point
//! `roteiro`'s `main` holds for the whole process (issues #291, #296).

#[cfg(any(feature = "audio-transcribe", feature = "image-vision"))]
use super::GeneratedContent;
use super::{MediaBuildOptions, MediaError, MediaKind, MediaProducer, Producer};

/// The prompt the ASR model is given. Part of the producer identity, so editing
/// it makes the next `media build` write new records beside the old ones rather
/// than silently changing what an existing record claims.
#[cfg(feature = "audio-transcribe")]
const ASR_PROMPT: &str = "Transcribe this audio recording. Output only the spoken words, verbatim.";

/// The prompt the vision model is given. Part of the producer identity.
#[cfg(feature = "image-vision")]
const VLM_PROMPT: &str = "Describe this image in one or two sentences.";

/// Sampling temperature for both generators: greedy, so a re-run with the same
/// producer is as reproducible as llama.cpp allows.
#[cfg(any(feature = "audio-transcribe", feature = "image-vision"))]
const TEMPERATURE: f64 = 0.0;

/// Token budget for a transcript.
#[cfg(feature = "audio-transcribe")]
const ASR_MAX_TOKENS: u32 = 512;

/// Token budget for a description.
#[cfg(feature = "image-vision")]
const VLM_MAX_TOKENS: u32 = 128;

/// The GGUF audio model backing `audio-transcribe`.
#[cfg(feature = "audio-transcribe")]
pub(crate) const ASR_MODEL: &str = "voxtral-mini-3b";

/// The GGUF vision-language model backing `image-vision`.
#[cfg(feature = "image-vision")]
pub(crate) const VLM_MODEL: &str = "smolvlm-500m-gguf";

/// The producers this binary can run right now: one per modality that is both
/// compiled in and installed, ordered `audio` then `vision`.
///
/// Never errors — an empty result is the honest answer for a default build, and
/// `media status` reports it as such so "0 records" reads as *cannot generate*
/// rather than *nothing to generate*. [`installed`] is the erroring variant, for
/// `media build`, which was asked to do something specific.
#[must_use]
pub fn available() -> Vec<Producer> {
    // `mut` only where a modality is compiled in; a default build has nothing to
    // push and would otherwise warn.
    #[cfg_attr(
        not(any(feature = "audio-transcribe", feature = "image-vision")),
        allow(unused_mut)
    )]
    let mut out = Vec::new();
    #[cfg(feature = "audio-transcribe")]
    if let Some(p) = asr_producer() {
        out.push(p);
    }
    #[cfg(feature = "image-vision")]
    if let Some(p) = vlm_producer() {
        out.push(p);
    }
    out
}

/// The producers needed for `opts`, or the first reason one is unavailable.
///
/// # Errors
/// Returns [`MediaError::NoProducer`] when the modality was not compiled in
/// (naming the cargo feature that provides it), or [`MediaError::ModelMissing`]
/// when it was but the model is not on disk (naming the `roteiro model pull`
/// command that installs it). Both are actionable; neither degrades to silence.
pub fn installed(opts: MediaBuildOptions) -> Result<Vec<Box<dyn MediaProducer>>, MediaError> {
    let mut out: Vec<Box<dyn MediaProducer>> = Vec::new();
    if opts.audio {
        out.push(audio_producer()?);
    }
    if opts.vision {
        out.push(vision_producer()?);
    }
    Ok(out)
}

/// The audio generator, or why there isn't one.
#[cfg(feature = "audio-transcribe")]
fn audio_producer() -> Result<Box<dyn MediaProducer>, MediaError> {
    let producer = asr_producer().ok_or_else(|| MediaError::ModelMissing {
        model: ASR_MODEL.to_owned(),
    })?;
    let engine = asr_engine().ok_or_else(|| MediaError::ModelMissing {
        model: ASR_MODEL.to_owned(),
    })?;
    Ok(Box::new(LlamaProducer { producer, engine }))
}

#[cfg(not(feature = "audio-transcribe"))]
fn audio_producer() -> Result<Box<dyn MediaProducer>, MediaError> {
    Err(MediaError::NoProducer {
        kind: MediaKind::Audio.as_str(),
        feature: "audio-transcribe",
    })
}

/// The vision generator, or why there isn't one.
#[cfg(feature = "image-vision")]
fn vision_producer() -> Result<Box<dyn MediaProducer>, MediaError> {
    let producer = vlm_producer().ok_or_else(|| MediaError::ModelMissing {
        model: VLM_MODEL.to_owned(),
    })?;
    let engine = vlm_engine().ok_or_else(|| MediaError::ModelMissing {
        model: VLM_MODEL.to_owned(),
    })?;
    Ok(Box::new(LlamaProducer { producer, engine }))
}

#[cfg(not(feature = "image-vision"))]
fn vision_producer() -> Result<Box<dyn MediaProducer>, MediaError> {
    Err(MediaError::NoProducer {
        kind: MediaKind::Vision.as_str(),
        feature: "image-vision",
    })
}

/// The identity the installed ASR model would write under, or `None` when it is
/// not installed.
#[cfg(feature = "audio-transcribe")]
fn asr_producer() -> Option<Producer> {
    registry_producer(MediaKind::Audio, ASR_MODEL, ASR_PROMPT, ASR_MAX_TOKENS)
}

/// The identity the installed vision model would write under, or `None`.
#[cfg(feature = "image-vision")]
fn vlm_producer() -> Option<Producer> {
    registry_producer(MediaKind::Vision, VLM_MODEL, VLM_PROMPT, VLM_MAX_TOKENS)
}

/// Build a [`Producer`] from the **registry's pinned digests** for `model`,
/// provided every one of its files is on disk.
///
/// The digests come from the registry rather than from hashing the installed
/// files: the registry pin *is* the identity of what was installed (a file whose
/// bytes differ from the pin never gets written — see
/// [`crate::models::download_verified`]), and re-hashing three gigabytes on every
/// `media status` would make the command unusable.
#[cfg(any(feature = "audio-transcribe", feature = "image-vision"))]
fn registry_producer(
    kind: MediaKind,
    model: &str,
    prompt: &str,
    max_tokens: u32,
) -> Option<Producer> {
    let spec = crate::models::find(model)?;
    let variant = spec.variant_for(crate::models::Platform::host())?;
    let dir = crate::models::model_dir(model);
    if !variant.files.iter().all(|f| dir.join(f.name).exists()) {
        return None;
    }
    let file = |name: &str| variant.files.iter().find(|f| f.name == name);
    let weights = file("model.gguf")?;
    let mmproj = file("mmproj.gguf")?;
    Some(Producer {
        kind,
        model: model.to_owned(),
        model_digest: weights.sha256.to_owned(),
        quantisation: quantisation_of(weights.url),
        mmproj_digest: mmproj.sha256.to_owned(),
        prompt: prompt.to_owned(),
        temperature: TEMPERATURE,
        max_tokens,
    })
}

/// The GGUF quantisation named in a pinned model URL's file name
/// (`…-Q4_K_M.gguf` → `Q4_K_M`), or `unknown` when the name carries no
/// recognisable token.
///
/// Reading it from the pin keeps the registry the single source of truth: change
/// the pinned file and the recorded quantisation follows, which moves the
/// producer id, which makes the next build write new records. A hand-maintained
/// second copy of the same fact could drift from it silently.
#[cfg(any(feature = "audio-transcribe", feature = "image-vision"))]
fn quantisation_of(url: &str) -> String {
    let name = url.rsplit('/').next().unwrap_or(url);
    let stem = name.strip_suffix(".gguf").unwrap_or(name);
    // GGUF quantisation tokens are whole `-`-separated segments. Scan from the
    // end, because the model name itself may contain digits and dashes.
    for segment in stem.rsplit('-') {
        let upper = segment.to_ascii_uppercase();
        let quantised = (upper.starts_with('Q') || upper.starts_with("IQ"))
            && upper.chars().any(|c| c.is_ascii_digit())
            || matches!(upper.as_str(), "F16" | "F32" | "BF16");
        if quantised {
            return upper;
        }
    }
    "unknown".to_owned()
}

/// A generator backed by the shared llama.cpp engine — the same `mtmd`
/// multimodal path for both modalities, differing only in whether the blob is
/// handed over as audio or as an image.
#[cfg(any(feature = "audio-transcribe", feature = "image-vision"))]
struct LlamaProducer {
    producer: Producer,
    engine: std::sync::Arc<rto_llama::llama::LlamaEngine>,
}

#[cfg(any(feature = "audio-transcribe", feature = "image-vision"))]
impl MediaProducer for LlamaProducer {
    fn producer(&self) -> &Producer {
        &self.producer
    }

    fn generate(&self, _path: &str, bytes: &[u8]) -> Option<GeneratedContent> {
        use rto_llama::Engine as _;

        // An image whose header does not parse, or whose pixel count is over the
        // cap, is refused before the projector is loaded — the same guard the OCR
        // path applies, for the same decompression-bomb reason.
        #[cfg(feature = "image-vision")]
        if self.producer.kind == MediaKind::Vision && !crate::extract::image_dimensions_ok(bytes) {
            return None;
        }
        let (images, audio) = match self.producer.kind {
            MediaKind::Vision => (vec![bytes.to_vec()], Vec::new()),
            MediaKind::Audio => (Vec::new(), vec![bytes.to_vec()]),
        };
        let completion = self
            .engine
            .chat(&rto_llama::ChatRequest {
                model: self.producer.model.clone(),
                messages: vec![rto_llama::Message {
                    role: "user".to_owned(),
                    content: self.producer.prompt.clone(),
                }],
                images,
                audio,
                // The identity stores the temperature as `f64` (it is a `REAL`
                // column and a JSON number); the engine takes `f32`. The narrowing
                // is deliberate and cannot affect identity: the *stored* value
                // stays the wide one, so `Producer::id` never depends on this
                // conversion, and every temperature a producer actually uses is a
                // short decimal literal that survives it exactly.
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
            // Neither runtime exposes a calibrated confidence. Recording `None`
            // is the honest answer; inventing one would be exactly the kind of
            // fabricated certainty ADR-0015 exists to stop.
            confidence: None,
        })
    }
}

/// The process-wide audio engine's slot; see [`asr_engine`].
#[cfg(feature = "audio-transcribe")]
static ASR_ENGINE: rto_llama::EngineSlot<rto_llama::llama::LlamaEngine> =
    rto_llama::EngineSlot::new();

/// The process-wide vision engine's slot; see [`vlm_engine`].
#[cfg(feature = "image-vision")]
static VLM_ENGINE: rto_llama::EngineSlot<rto_llama::llama::LlamaEngine> =
    rto_llama::EngineSlot::new();

/// The process-wide audio engine, built lazily from the installed [`ASR_MODEL`]
/// (`model.gguf` + audio `mmproj.gguf`). `None` when the model is not installed.
///
/// Lives in an [`EngineSlot`](rto_llama::EngineSlot) rather than a
/// `static OnceLock` so [`crate::release_media_engines`] can destroy it *before*
/// the process exits — a `static` engine is never dropped, which aborts the
/// process on Metal (issue #291).
///
/// It shares the process's one llama.cpp backend with [`vlm_engine`]
/// (`rto_llama::backend`), so which of the two modalities a run happens to touch
/// first no longer decides whether the other works at all (issue #296).
#[cfg(feature = "audio-transcribe")]
pub(crate) fn asr_engine() -> Option<std::sync::Arc<rto_llama::llama::LlamaEngine>> {
    ASR_ENGINE.get_or_init(|| build_engine(ASR_MODEL))
}

/// Transcribe one clip through the real engine, returning just the text.
///
/// Test-only, and a thin wrapper over the production path: `media build` goes
/// through [`MediaProducer::generate`]. It exists so the engine-teardown tests in
/// [`crate::extract`] — which are about the process-global engines and the
/// backend they share (issues #291, #296, #301), not about the artifact store —
/// keep driving a real load without each of them assembling a producer.
#[cfg(all(test, feature = "audio-transcribe"))]
pub(crate) fn asr_content(bytes: &[u8]) -> Option<String> {
    let producer = asr_producer()?;
    let engine = asr_engine()?;
    LlamaProducer { producer, engine }
        .generate("fixture.wav", bytes)
        .map(|c| c.text)
}

/// The process-wide vision engine, built lazily from the installed [`VLM_MODEL`].
/// Same slot mechanism and the same shared backend as [`asr_engine`].
#[cfg(feature = "image-vision")]
pub(crate) fn vlm_engine() -> Option<std::sync::Arc<rto_llama::llama::LlamaEngine>> {
    VLM_ENGINE.get_or_init(|| build_engine(VLM_MODEL))
}

/// Describe one image through the real engine, returning just the text. Test-only
/// sibling of [`asr_content`], and for the same reason.
#[cfg(all(test, feature = "image-vision"))]
pub(crate) fn vlm_content(bytes: &[u8]) -> Option<String> {
    let producer = vlm_producer()?;
    let engine = vlm_engine()?;
    LlamaProducer { producer, engine }
        .generate("fixture.png", bytes)
        .map(|c| c.text)
}

/// Build a `mtmd` engine over an installed model's `model.gguf` + `mmproj.gguf`.
#[cfg(any(feature = "audio-transcribe", feature = "image-vision"))]
fn build_engine(model: &str) -> Option<rto_llama::llama::LlamaEngine> {
    let dir = crate::models::model_dir(model);
    let (gguf, mmproj) = (dir.join("model.gguf"), dir.join("mmproj.gguf"));
    if !gguf.exists() || !mmproj.exists() {
        return None;
    }
    rto_llama::llama::LlamaEngine::new(
        vec![rto_llama::llama::Served {
            name: model.to_owned(),
            path: gguf,
            mmproj: Some(mmproj),
        }],
        0,
    )
    .ok()
}

/// Release the vision engine, or nothing in a build without `image-vision`.
#[cfg(feature = "image-vision")]
pub(crate) fn release_vlm_engine() -> bool {
    VLM_ENGINE.release()
}

#[cfg(not(feature = "image-vision"))]
pub(crate) fn release_vlm_engine() -> bool {
    false
}

/// Release the ASR engine, or nothing in a build without `audio-transcribe`.
#[cfg(feature = "audio-transcribe")]
pub(crate) fn release_asr_engine() -> bool {
    ASR_ENGINE.release()
}

#[cfg(not(feature = "audio-transcribe"))]
pub(crate) fn release_asr_engine() -> bool {
    false
}

#[cfg(test)]
mod tests {
    #[cfg(not(all(feature = "audio-transcribe", feature = "image-vision")))]
    use super::MediaError;
    use super::{MediaBuildOptions, available, installed};

    /// The refusal, as a value. `Box<dyn MediaProducer>` is not `Debug`, so
    /// `expect_err` is not available; this says the same thing and reports an
    /// unexpected success just as clearly.
    ///
    /// Only the "this build cannot do that" tests need it, and those exist only
    /// where a modality is compiled *out* — so in an `--all-features` build there
    /// is nothing to refuse and the helper would be dead code.
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
            force: false,
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
            force: false,
        });
        assert!(
            err.to_string().contains("--features image-vision"),
            "the message must name the rebuild: {err}"
        );
    }

    /// Asking for nothing is not an error in any build — `media build --audio` in
    /// a vision-only binary must not be refused for the modality it did not ask
    /// for.
    #[test]
    fn requesting_no_modality_needs_no_producer() {
        let none = installed(MediaBuildOptions {
            audio: false,
            vision: false,
            force: false,
        })
        .expect("asking for nothing cannot fail");
        assert!(none.is_empty());
    }

    /// `available` never errors: a default build simply has nothing to offer.
    #[test]
    #[cfg(not(any(feature = "audio-transcribe", feature = "image-vision")))]
    fn a_default_build_offers_no_producer() {
        assert!(available().is_empty());
    }

    #[test]
    #[cfg(any(feature = "audio-transcribe", feature = "image-vision"))]
    fn quantisation_is_read_from_the_pinned_file_name() {
        use super::quantisation_of;
        assert_eq!(
            quantisation_of(
                "https://huggingface.co/x/resolve/main/Voxtral-Mini-3B-2507-Q4_K_M.gguf"
            ),
            "Q4_K_M"
        );
        assert_eq!(
            quantisation_of(
                "https://huggingface.co/x/resolve/main/mmproj-SmolVLM-500M-Instruct-Q8_0.gguf"
            ),
            "Q8_0"
        );
        assert_eq!(quantisation_of("https://x/model-F16.gguf"), "F16");
        // A name with no quantisation token is reported as unknown rather than
        // guessed at — and `3B` must not be mistaken for one.
        assert_eq!(quantisation_of("https://x/SmolVLM-500M-3B.gguf"), "unknown");
    }

    /// The producers this binary can offer must be exactly the modalities it was
    /// compiled with (when installed). Anything else means a producer leaked
    /// across a feature gate.
    #[test]
    fn available_producers_match_the_compiled_modalities() {
        use super::MediaKind;
        for producer in available() {
            let compiled_in = match producer.kind {
                MediaKind::Audio => cfg!(feature = "audio-transcribe"),
                MediaKind::Vision => cfg!(feature = "image-vision"),
            };
            assert!(
                compiled_in,
                "a {} producer was offered by a build without its feature",
                producer.kind
            );
            // Whatever this build offers must be a well-formed identity.
            producer.validate().expect("an offered producer is valid");
        }
    }
}

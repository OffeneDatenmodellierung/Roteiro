//! The **pre-generation gate**: a cheap, deterministic refusal of media blobs
//! that obviously contain nothing to read, evaluated *before* a model is loaded
//! (ADR-0015).
//!
//! Two measurements, one per modality:
//!
//! - **Audio** — root-mean-square amplitude over the whole clip, normalised so
//!   that full scale is `1.0`. Digital silence measures exactly `0.0`.
//! - **Images** — the variance of the luma plane, likewise normalised (a pixel is
//!   `0.0`…`1.0`). A flat-colour image measures exactly `0.0`.
//!
//! Three properties make it worth having, and each is asserted by a test rather
//! than assumed:
//!
//! 1. **It runs before the model loads.** [`super::build_media`] evaluates the
//!    gate and, on a refusal, never calls [`MediaProducer::generate`](super::MediaProducer::generate)
//!    at all — and the llama.cpp engines are built lazily *inside* `generate`
//!    (see [`super::producers`]), so a repository of silent or blank assets
//!    performs no 715 MB projector load whatsoever (issue #301).
//! 2. **The refusal is recorded, not silent.** A gated blob still gets a
//!    [`MediaRecord`](super::MediaRecord) — one carrying a [`MediaSkip`] instead
//!    of text — so `media status` can print *"skipped: below silence threshold
//!    (rms=0.0)"* rather than leaving an indistinguishable hole. An operator can
//!    tell **not generated** from **generated nothing**; an invisible skip would
//!    be its own small lie, which would be an odd thing to add to an ADR about
//!    not lying.
//! 3. **It is deterministic** — a pure function of the bytes and the thresholds —
//!    so it costs nothing in reproducibility.
//!
//! # What it does not do
//!
//! The gate raises the floor. It does not fix the label, and it does not stop
//! confabulation:
//!
//! - **Quiet speech, room tone, tape hiss and hum all pass**, by design: the
//!   defaults sit near the digital noise floor, far below any real recording.
//!   A model handed room tone will still return confident invented prose.
//! - **Subtly-textured images pass** — a faint gradient, a watermark, a scan of a
//!   blank page. A VLM will still describe them.
//! - **Only WAV is measurable.** MP3 and FLAC need a decoder this workspace does
//!   not depend on, so the gate **abstains** for them (see [`audio_stats`]) and
//!   they go to the model as before. Abstention is a pass, never a skip.
//! - **Images are measurable only in a build with an image codec** — that is,
//!   with `image-ocr` or `image-vision` — which is exactly the build that can
//!   generate a description at all.
//!
//! Only the artifact store fixes the label. This is adopted because it is cheap
//! and helps, not because it addresses the defect in issue #300.
//!
//! @rto:0015

use serde::{Deserialize, Serialize};

use super::MediaKind;

/// Why the gate refused a blob.
///
/// One variant per modality, because one measurement per modality is what the
/// gate makes. The token is stored in `media_content.skip_reason` and is part of
/// a `CHECK` constraint, so adding a variant is a schema change, not a rename.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GateReason {
    /// An audio clip whose RMS amplitude was at or below the silence threshold.
    Silence,
    /// An image whose luma variance was at or below the uniformity threshold.
    Uniform,
}

impl GateReason {
    /// Stable token used in the `SQLite` store and in `--json` output.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Silence => "silence",
            Self::Uniform => "uniform",
        }
    }

    /// Parse a reason from its stable token; `None` for an unrecognised value (a
    /// corrupt row).
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        match s {
            "silence" => Some(Self::Silence),
            "uniform" => Some(Self::Uniform),
            _ => None,
        }
    }

    /// The name of the quantity that was measured, as printed beside the value:
    /// `rms` for silence, `variance` for uniformity.
    #[must_use]
    pub fn metric(self) -> &'static str {
        match self {
            Self::Silence => "rms",
            Self::Uniform => "variance",
        }
    }

    /// The threshold's name in prose, for the one-line explanation.
    #[must_use]
    pub fn threshold_name(self) -> &'static str {
        match self {
            Self::Silence => "silence",
            Self::Uniform => "uniformity",
        }
    }
}

impl std::fmt::Display for GateReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A recorded refusal: why, what was measured, and what it was measured against.
///
/// The **measured value** is the load-bearing field. "Skipped" alone would tell
/// an operator nothing about whether the gate was right; `rms=0.0` against a
/// threshold of `0.0001` says the clip was digitally silent, and `rms=0.00009`
/// says it was very nearly so and the threshold is worth a look.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MediaSkip {
    /// Which measurement refused the blob.
    pub reason: GateReason,
    /// The value measured on this blob, in the metric's own units.
    pub value: f64,
    /// The threshold it was compared against, recorded so a later change of
    /// defaults cannot silently reinterpret an old skip.
    pub threshold: f64,
}

impl std::fmt::Display for MediaSkip {
    /// The one-line explanation `media status` and the explorer print, e.g.
    /// `below silence threshold (rms=0, threshold 0.0001)`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "below {} threshold ({}={}, threshold {})",
            self.reason.threshold_name(),
            self.reason.metric(),
            self.value,
            self.threshold,
        )
    }
}

/// The gate's two tunable thresholds.
///
/// **Conservative by default.** A false skip is a silently missing description,
/// which is worse than a false pass now that generated output is clearly
/// labelled — so both defaults sit at the digital noise floor, where "there is
/// nothing here" is not a judgement call. Raising them trades that safety for
/// fewer pointless model loads; that is an operator's decision, taken in
/// `roteiro.toml` under `[media]`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GateThresholds {
    /// RMS amplitude (full scale `1.0`) at or below which a clip is silent.
    pub silence_rms: f64,
    /// Luma variance (a pixel is `0.0`…`1.0`) at or below which an image is
    /// uniform.
    pub image_variance: f64,
}

/// RMS at or below which a clip counts as silent: `1e-4`, about **-80 dBFS**.
///
/// A 16-bit sample is `1/32768` ≈ `3e-5` of full scale, so this admits a couple
/// of least-significant bits of dither and nothing else. Ordinary quiet speech
/// sits around -40 dBFS (`1e-2`) and even a hissy room tone around -60 dBFS
/// (`1e-3`) — both two to three orders of magnitude clear of the gate.
pub const DEFAULT_SILENCE_RMS: f64 = 1e-4;

/// Luma variance at or below which an image counts as uniform: `1e-5`, a standard
/// deviation of about **0.8 levels out of 255**.
///
/// A flat colour is exactly `0.0`; a flat colour that has been through JPEG
/// picks up a little ringing and stays well under. Anything with visible
/// structure — a faint gradient, a watermark, a line of text — is orders of
/// magnitude above, and passes.
pub const DEFAULT_IMAGE_VARIANCE: f64 = 1e-5;

impl Default for GateThresholds {
    fn default() -> Self {
        Self {
            silence_rms: DEFAULT_SILENCE_RMS,
            image_variance: DEFAULT_IMAGE_VARIANCE,
        }
    }
}

impl GateThresholds {
    /// Thresholds that refuse nothing — the gate turned off, as
    /// `[media] gate = false` produces.
    ///
    /// Expressed as *negative* thresholds rather than as a separate `enabled`
    /// flag: no measurement can be below zero, so "off" is the same code path as
    /// "on", and there is no second way for a blob to reach the model.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            silence_rms: -1.0,
            image_variance: -1.0,
        }
    }
}

/// Evaluate the gate for one blob: `Some(skip)` to refuse it, `None` to let it
/// through.
///
/// `None` covers both "there is something here" and "this build cannot measure
/// this input" — abstention is always a pass, never a skip, because a false skip
/// is the expensive mistake.
#[must_use]
pub fn evaluate(kind: MediaKind, bytes: &[u8], thresholds: GateThresholds) -> Option<MediaSkip> {
    match kind {
        MediaKind::Audio => {
            let stats = audio_stats(bytes)?;
            (stats.rms <= thresholds.silence_rms).then_some(MediaSkip {
                reason: GateReason::Silence,
                value: stats.rms,
                threshold: thresholds.silence_rms,
            })
        }
        MediaKind::Vision => {
            let stats = image_stats(bytes)?;
            (stats.variance <= thresholds.image_variance).then_some(MediaSkip {
                reason: GateReason::Uniform,
                value: stats.variance,
                threshold: thresholds.image_variance,
            })
        }
    }
}

/// What the gate measured on an audio clip. Amplitudes are normalised so that
/// full scale is `1.0`, whatever the source sample format.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioStats {
    /// Largest absolute sample amplitude.
    pub peak: f64,
    /// Root-mean-square amplitude over the whole clip — the value the gate
    /// compares, and the one a skip records.
    pub rms: f64,
}

/// What the gate measured on an image, over its luma plane with each pixel
/// normalised to `0.0`…`1.0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImageStats {
    /// Mean luma. Recorded for legibility; the gate does not compare it — a
    /// black image and a white image are equally empty.
    pub mean: f64,
    /// Variance of the luma plane — the value the gate compares.
    pub variance: f64,
}

/// Measure an audio blob, or `None` when this build cannot read it.
///
/// Only **WAV** is measurable: it is a container over raw PCM, so peak and RMS
/// fall out of a header parse and a pass over the samples, with no decoder and
/// therefore no new dependency. MP3 and FLAC are entropy-coded; measuring them
/// means decoding them, and the audio decoder in this process lives inside
/// llama.cpp's bundled miniaudio — behind the very model load the gate exists to
/// avoid. So the gate abstains for those formats and they reach the model
/// unchanged, which is the conservative failure.
#[must_use]
pub fn audio_stats(bytes: &[u8]) -> Option<AudioStats> {
    let pcm = wav_pcm(bytes)?;
    Some(pcm_stats(&pcm))
}

/// Peak and RMS over already-normalised samples. Separated from the container
/// parse so the statistics are exercised in every build, on samples a test can
/// write by hand.
///
/// An empty clip measures `0.0` for both, so a zero-length `data` chunk is
/// silence — which it is.
#[must_use]
pub fn pcm_stats(samples: &[f64]) -> AudioStats {
    let mut peak = 0.0_f64;
    let mut sum_squares = 0.0_f64;
    for s in samples {
        peak = peak.max(s.abs());
        sum_squares += s * s;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "sample counts are far below 2^53; the mean only needs to be accurate \
                  to many more digits than a threshold comparison uses"
    )]
    let rms = if samples.is_empty() {
        0.0
    } else {
        (sum_squares / samples.len() as f64).sqrt()
    };
    AudioStats { peak, rms }
}

/// Mean and variance over an already-decoded 8-bit luma plane, normalised to
/// `0.0`…`1.0`. Separated from the codec for the same reason as [`pcm_stats`]:
/// the statistics are then testable in a build with no image codec at all.
///
/// An empty plane measures `0.0` for both.
#[must_use]
pub fn luma_stats(luma: &[u8]) -> ImageStats {
    if luma.is_empty() {
        return ImageStats {
            mean: 0.0,
            variance: 0.0,
        };
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "pixel counts are far below 2^53 (the pixel cap is 40 megapixels)"
    )]
    let n = luma.len() as f64;
    let mut sum = 0.0_f64;
    let mut sum_squares = 0.0_f64;
    for &b in luma {
        let v = f64::from(b) / 255.0;
        sum += v;
        sum_squares += v * v;
    }
    let mean = sum / n;
    // The population variance, computed as E[x²] - E[x]². Clamped at zero
    // because floating-point cancellation can push a genuinely flat plane a
    // hair below it, and a negative variance would be nonsense to print.
    ImageStats {
        mean,
        variance: (sum_squares / n - mean * mean).max(0.0),
    }
}

/// Decode a WAV blob's samples to `[-1.0, 1.0]`, or `None` when it is not a WAV
/// this parser understands.
///
/// Handles the sample formats a recorder actually emits: unsigned 8-bit, signed
/// 16/24/32-bit PCM, and 32/64-bit IEEE float, in both plain (`0x0001`/`0x0003`)
/// and `WAVE_FORMAT_EXTENSIBLE` (`0xFFFE`) flavours. Channels are not separated —
/// the gate asks "is there anything here at all", which is a question about the
/// whole clip.
fn wav_pcm(bytes: &[u8]) -> Option<Vec<f64>> {
    /// Uncompressed integer PCM.
    const FORMAT_PCM: u16 = 0x0001;
    /// IEEE 754 float samples.
    const FORMAT_FLOAT: u16 = 0x0003;
    /// `WAVE_FORMAT_EXTENSIBLE`; the real format is the first two bytes of the
    /// sub-format GUID in the extension, which we read below.
    const FORMAT_EXTENSIBLE: u16 = 0xFFFE;
    /// Largest number of samples the gate will measure. A clip is capped at
    /// [`MAX_AUDIO_BYTES`](super::MAX_AUDIO_BYTES) already, but that is a
    /// *compressed* cap and says nothing about a hand-written header, so bound
    /// the allocation independently.
    const MAX_SAMPLES: usize = 64 * 1024 * 1024;

    let u16_at = |at: usize| -> Option<u16> {
        Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
    };
    let u32_at = |at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
    };

    if bytes.get(..4)? != b"RIFF" || bytes.get(8..12)? != b"WAVE" {
        return None;
    }
    // Walk the chunk list rather than assuming `fmt ` then `data`: real files
    // interleave `LIST`, `fact` and padding chunks, and a parser that assumed
    // the layout would abstain on perfectly ordinary recordings.
    let (mut format, mut bits) = (None, None);
    let mut cursor = 12;
    while cursor + 8 <= bytes.len() {
        let id = bytes.get(cursor..cursor + 4)?;
        let size = u32_at(cursor + 4)? as usize;
        let body = cursor + 8;
        match id {
            b"fmt " if size >= 16 => {
                let tag = u16_at(body)?;
                let declared_bits = u16_at(body + 14)?;
                // `EXTENSIBLE` defers the real format to the sub-format GUID,
                // whose first two bytes are the plain tag it stands in for.
                let tag = if tag == FORMAT_EXTENSIBLE && size >= 26 {
                    u16_at(body + 24)?
                } else {
                    tag
                };
                format = Some(tag);
                bits = Some(declared_bits);
            }
            b"data" => {
                let (format, bits) = (format?, bits?);
                let data = bytes.get(body..body.saturating_add(size))?;
                return decode_samples(data, format, bits, FORMAT_PCM, FORMAT_FLOAT, MAX_SAMPLES);
            }
            _ => {}
        }
        // Chunk bodies are word-aligned: an odd size is followed by a pad byte.
        cursor = body.checked_add(size)?.checked_add(size & 1)?;
    }
    None
}

/// Turn a `data` chunk into normalised samples for one of the formats
/// [`wav_pcm`] accepts, or `None` for anything else.
fn decode_samples(
    data: &[u8],
    format: u16,
    bits: u16,
    format_pcm: u16,
    format_float: u16,
    max_samples: usize,
) -> Option<Vec<f64>> {
    let width = usize::from(bits).div_ceil(8);
    if width == 0 || data.len() / width > max_samples {
        return None;
    }
    // A `data` chunk that is not a whole number of samples is a corrupt or
    // truncated file. **Abstain rather than measure the aligned prefix**: the
    // decoders below use `chunks_exact`, which silently drops the remainder, so
    // measuring anyway would report an RMS for part of a clip and could refuse
    // a blob on the strength of it. That is a false skip — a silently missing
    // description — and avoiding it is what the whole gate is calibrated
    // around. Handing an unreadable clip to the model costs one model load;
    // refusing a readable one costs the operator something they cannot see.
    if !data.len().is_multiple_of(width) {
        return None;
    }
    let mut out = Vec::with_capacity(data.len() / width);
    match (format, bits) {
        // Unsigned, mid-scale at 128 — the one PCM width that is not signed.
        (f, 8) if f == format_pcm => {
            out.extend(data.iter().map(|&b| (f64::from(b) - 128.0) / 128.0));
        }
        (f, 16) if f == format_pcm => {
            out.extend(
                data.chunks_exact(2)
                    .map(|c| f64::from(i16::from_le_bytes([c[0], c[1]])) / f64::from(1_i32 << 15)),
            );
        }
        // 24-bit is stored packed, three bytes little-endian, so sign-extend it
        // into an i32 by placing it in the top three bytes and shifting back.
        (f, 24) if f == format_pcm => {
            out.extend(data.chunks_exact(3).map(|c| {
                let v = i32::from_le_bytes([0, c[0], c[1], c[2]]) >> 8;
                f64::from(v) / f64::from(1_i32 << 23)
            }));
        }
        (f, 32) if f == format_pcm => {
            out.extend(data.chunks_exact(4).map(|c| {
                f64::from(i32::from_le_bytes([c[0], c[1], c[2], c[3]])) / 2_147_483_648.0
            }));
        }
        (f, 32) if f == format_float => {
            out.extend(
                data.chunks_exact(4)
                    .map(|c| f64::from(f32::from_le_bytes([c[0], c[1], c[2], c[3]]))),
            );
        }
        (f, 64) if f == format_float => {
            out.extend(
                data.chunks_exact(8)
                    .map(|c| f64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]])),
            );
        }
        _ => return None,
    }
    // A non-finite float sample is a corrupt file, not a loud one; refusing to
    // measure it abstains rather than reporting a NaN RMS that compares false
    // against every threshold in a way nobody could debug.
    out.iter().all(|s| s.is_finite()).then_some(out)
}

/// Measure an image blob, or `None` when this build has no image codec.
///
/// Gated on the same features that make a description possible in the first
/// place, so the abstention is never reachable from a build that could have
/// generated something.
#[cfg(any(feature = "image-ocr", feature = "image-vision"))]
#[must_use]
pub fn image_stats(bytes: &[u8]) -> Option<ImageStats> {
    // The decompression-bomb guard the OCR and vision paths already apply: read
    // the dimensions from the header and refuse an over-large image before any
    // pixel is decoded. Abstaining here is right — an image too large to measure
    // is not an image the gate should claim is empty.
    if !crate::extract::image_dimensions_ok(bytes) {
        return None;
    }
    let luma = image::load_from_memory(bytes).ok()?.into_luma8();
    Some(luma_stats(luma.as_raw()))
}

/// Without an image codec there is nothing to decode with, so the gate abstains
/// and every image passes. Such a build cannot generate a description either, so
/// nothing is lost.
#[cfg(not(any(feature = "image-ocr", feature = "image-vision")))]
#[must_use]
pub fn image_stats(_bytes: &[u8]) -> Option<ImageStats> {
    None
}

#[cfg(test)]
mod tests {
    use super::{
        AudioStats, DEFAULT_IMAGE_VARIANCE, DEFAULT_SILENCE_RMS, GateReason, GateThresholds,
        ImageStats, MediaKind, MediaSkip, audio_stats, evaluate, luma_stats, pcm_stats,
    };

    /// A minimal 16-bit mono WAV around `samples`.
    fn wav16(samples: &[i16]) -> Vec<u8> {
        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&u32::try_from(36 + data.len()).expect("fits").to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16_u32.to_le_bytes()); // chunk size
        out.extend_from_slice(&1_u16.to_le_bytes()); // PCM
        out.extend_from_slice(&1_u16.to_le_bytes()); // mono
        out.extend_from_slice(&8000_u32.to_le_bytes()); // sample rate
        out.extend_from_slice(&16_000_u32.to_le_bytes()); // byte rate
        out.extend_from_slice(&2_u16.to_le_bytes()); // block align
        out.extend_from_slice(&16_u16.to_le_bytes()); // bits
        out.extend_from_slice(b"data");
        out.extend_from_slice(&u32::try_from(data.len()).expect("fits").to_le_bytes());
        out.extend_from_slice(&data);
        out
    }

    /// A WAV declaring `bits` per sample around a `data` chunk of exactly
    /// `data` — which the caller may deliberately leave unaligned to the sample
    /// width, unlike [`wav16`], whose `data` is always whole samples.
    fn wav_with_data(bits: u16, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&u32::try_from(36 + data.len()).expect("fits").to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16_u32.to_le_bytes()); // chunk size
        out.extend_from_slice(&1_u16.to_le_bytes()); // PCM
        out.extend_from_slice(&1_u16.to_le_bytes()); // mono
        out.extend_from_slice(&8000_u32.to_le_bytes()); // sample rate
        out.extend_from_slice(&16_000_u32.to_le_bytes()); // byte rate
        out.extend_from_slice(&2_u16.to_le_bytes()); // block align
        out.extend_from_slice(&bits.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&u32::try_from(data.len()).expect("fits").to_le_bytes());
        out.extend_from_slice(data);
        out
    }

    /// **A `data` chunk that is not a whole number of samples must abstain, not
    /// be measured on its aligned prefix.**
    ///
    /// The decoders use `chunks_exact`, which silently drops the remainder — so
    /// without the length check a truncated file would be measured on whatever
    /// happened to align. Every case here is all-zero bytes, which is the
    /// dangerous shape: the aligned prefix measures `rms = 0`, so the gate would
    /// **refuse** a clip it could not actually read. A false skip is a silently
    /// missing description, and avoiding it is the rule the whole gate is
    /// calibrated around.
    #[test]
    fn an_unaligned_data_chunk_abstains_rather_than_being_measured() {
        // `(bits, data length)`, each length short of a whole sample. Only the
        // integer-PCM widths this builder declares (`format = 1`), so every case
        // reaches a real decoder arm and is refused for its length rather than
        // for an unsupported format.
        for (bits, len) in [(16_u16, 65_usize), (24, 64), (32, 66)] {
            let wav = wav_with_data(bits, &vec![0_u8; len]);
            assert!(
                audio_stats(&wav).is_none(),
                "{bits}-bit with a {len}-byte data chunk must not be measured",
            );
            assert!(
                evaluate(MediaKind::Audio, &wav, GateThresholds::default()).is_none(),
                "{bits}-bit with a {len}-byte data chunk must PASS — measuring the \
                 aligned prefix of a corrupt clip would be a false skip",
            );
        }

        // The same builder with an aligned chunk still measures, so the guard is
        // rejecting misalignment and not simply everything it is handed.
        assert_eq!(
            audio_stats(&wav_with_data(16, &[0; 64])).expect("64 bytes is 32 whole samples"),
            AudioStats {
                peak: 0.0,
                rms: 0.0
            },
        );
    }

    #[test]
    fn digital_silence_measures_exactly_zero() {
        let stats = audio_stats(&wav16(&[0; 800])).expect("a WAV is measurable");
        assert_eq!(
            stats,
            AudioStats {
                peak: 0.0,
                rms: 0.0
            }
        );
    }

    #[test]
    fn a_tone_is_far_above_the_silence_threshold() {
        // A half-scale square wave: RMS is 0.5, four thousand times the default.
        let samples: Vec<i16> = (0..800)
            .map(|i| if i % 2 == 0 { 16_384 } else { -16_384 })
            .collect();
        let stats = audio_stats(&wav16(&samples)).expect("measurable");
        assert!(
            stats.rms > DEFAULT_SILENCE_RMS * 1000.0,
            "a tone must clear the gate by orders of magnitude: {stats:?}"
        );
        assert!(
            evaluate(
                MediaKind::Audio,
                &wav16(&samples),
                GateThresholds::default()
            )
            .is_none()
        );
    }

    /// The headline: silence is refused, and the refusal carries its measurement.
    #[test]
    fn silence_is_refused_with_its_measured_value() {
        let skip = evaluate(
            MediaKind::Audio,
            &wav16(&[0; 800]),
            GateThresholds::default(),
        )
        .expect("digital silence must be refused");
        assert_eq!(
            skip,
            MediaSkip {
                reason: GateReason::Silence,
                value: 0.0,
                threshold: DEFAULT_SILENCE_RMS,
            }
        );
        assert_eq!(
            skip.to_string(),
            "below silence threshold (rms=0, threshold 0.0001)"
        );
    }

    /// The defaults sit at the digital noise floor, not near real audio. This is
    /// the "conservative" claim, made checkable: a signal three least-significant
    /// bits wide is still refused, and one at -60 dBFS — a hissy room tone, far
    /// quieter than speech — is not.
    #[test]
    fn the_default_threshold_admits_dither_and_nothing_louder() {
        let dither: Vec<i16> = (0..800).map(|i| if i % 2 == 0 { 2 } else { -2 }).collect();
        assert!(
            evaluate(MediaKind::Audio, &wav16(&dither), GateThresholds::default()).is_some(),
            "a couple of least-significant bits is still silence"
        );

        // -60 dBFS ≈ 0.001 of full scale ≈ 32 counts at 16-bit.
        let room_tone: Vec<i16> = (0..800)
            .map(|i| if i % 2 == 0 { 33 } else { -33 })
            .collect();
        assert!(
            evaluate(
                MediaKind::Audio,
                &wav16(&room_tone),
                GateThresholds::default()
            )
            .is_none(),
            "room tone must pass — the gate does not claim to stop confabulation"
        );
    }

    /// Abstention is a pass. A format the gate cannot decode, a truncated file
    /// and a blob that is not audio at all must all reach the model unchanged,
    /// because a false skip is the expensive mistake.
    #[test]
    fn an_unmeasurable_blob_passes_rather_than_being_refused() {
        for bytes in [
            b"ID3\x04\x00\x00\x00\x00\x00\x00".as_slice(), // an MP3 tag header
            b"fLaC\x00\x00\x00\x22".as_slice(),            // a FLAC stream marker
            b"RIFF".as_slice(),                            // truncated before WAVE
            b"".as_slice(),
            &wav16(&[0; 4])[..20], // a WAV truncated inside its header
        ] {
            assert!(audio_stats(bytes).is_none(), "must not measure {bytes:?}");
            assert!(
                evaluate(MediaKind::Audio, bytes, GateThresholds::default()).is_none(),
                "abstention must pass, not skip: {bytes:?}"
            );
        }
    }

    /// Every sample format a recorder emits must be measurable, or the gate
    /// abstains on ordinary files and quietly does nothing.
    #[test]
    fn the_common_wav_sample_formats_are_all_measured_as_silent() {
        /// `(format tag, bits, one silent sample's bytes)`.
        const CASES: [(u16, u16, &[u8]); 6] = [
            (1, 8, &[128]),                     // unsigned 8-bit: mid-scale is 128
            (1, 16, &[0, 0]),                   // signed 16-bit
            (1, 24, &[0, 0, 0]),                // packed signed 24-bit
            (1, 32, &[0, 0, 0, 0]),             // signed 32-bit
            (3, 32, &[0, 0, 0, 0]),             // f32
            (3, 64, &[0, 0, 0, 0, 0, 0, 0, 0]), // f64
        ];
        for (format, bits, sample) in CASES {
            let data: Vec<u8> = sample.repeat(64);
            let mut out = Vec::new();
            out.extend_from_slice(b"RIFF");
            out.extend_from_slice(&u32::try_from(36 + data.len()).expect("fits").to_le_bytes());
            out.extend_from_slice(b"WAVEfmt ");
            out.extend_from_slice(&16_u32.to_le_bytes());
            out.extend_from_slice(&format.to_le_bytes());
            out.extend_from_slice(&1_u16.to_le_bytes());
            out.extend_from_slice(&8000_u32.to_le_bytes());
            out.extend_from_slice(&16_000_u32.to_le_bytes());
            out.extend_from_slice(&2_u16.to_le_bytes());
            out.extend_from_slice(&bits.to_le_bytes());
            out.extend_from_slice(b"data");
            out.extend_from_slice(&u32::try_from(data.len()).expect("fits").to_le_bytes());
            out.extend_from_slice(&data);
            let stats = audio_stats(&out).unwrap_or_else(|| panic!("{format}/{bits} must decode"));
            assert_eq!(
                stats,
                AudioStats {
                    peak: 0.0,
                    rms: 0.0
                },
                "{format}/{bits} silence must measure zero",
            );
        }
    }

    /// A `fmt ` chunk followed by anything other than `data` — real files carry
    /// `LIST`/`fact` chunks, and an odd-sized chunk is padded to a word boundary.
    #[test]
    fn intervening_chunks_and_their_padding_are_walked_over() {
        let data: Vec<u8> = vec![0; 128];
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFFxxxxWAVEfmt ");
        out.extend_from_slice(&16_u32.to_le_bytes());
        out.extend_from_slice(&1_u16.to_le_bytes());
        out.extend_from_slice(&1_u16.to_le_bytes());
        out.extend_from_slice(&8000_u32.to_le_bytes());
        out.extend_from_slice(&16_000_u32.to_le_bytes());
        out.extend_from_slice(&2_u16.to_le_bytes());
        out.extend_from_slice(&16_u16.to_le_bytes());
        // An odd-sized `LIST` chunk, plus its pad byte.
        out.extend_from_slice(b"LIST");
        out.extend_from_slice(&3_u32.to_le_bytes());
        out.extend_from_slice(b"abc\0");
        out.extend_from_slice(b"data");
        out.extend_from_slice(&u32::try_from(data.len()).expect("fits").to_le_bytes());
        out.extend_from_slice(&data);
        assert_eq!(
            audio_stats(&out).expect("measurable"),
            AudioStats {
                peak: 0.0,
                rms: 0.0
            }
        );
    }

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "a flat plane's variance is exactly zero — E[x^2] and E[x]^2 are the \
                  same sum, so the subtraction cancels bit for bit; a tolerance here \
                  would weaken the claim being made"
    )]
    fn a_flat_luma_plane_has_no_variance_whatever_its_colour() {
        for level in [0_u8, 128, 255] {
            let stats = luma_stats(&[level; 4096]);
            assert_eq!(stats.variance, 0.0, "level {level} must be uniform");
            // The mean is a running sum over 4096 terms, so it lands within a
            // few ULPs of the exact level rather than on it.
            assert!(
                (stats.mean - f64::from(level) / 255.0).abs() < 1e-12,
                "level {level} mean was {}",
                stats.mean,
            );
        }
        // The gate does not compare the mean: black and white are equally empty.
        assert!(luma_stats(&[0; 16]).variance <= DEFAULT_IMAGE_VARIANCE);
        assert!(luma_stats(&[255; 16]).variance <= DEFAULT_IMAGE_VARIANCE);
    }

    #[test]
    fn a_textured_luma_plane_clears_the_uniformity_threshold() {
        // A checkerboard of adjacent grey levels — about as subtle as structure
        // gets — is still three orders of magnitude above the threshold.
        let plane: Vec<u8> = (0..4096)
            .map(|i| if i % 2 == 0 { 120 } else { 136 })
            .collect();
        let variance = luma_stats(&plane).variance;
        assert!(
            variance > DEFAULT_IMAGE_VARIANCE * 50.0,
            "a checkerboard of adjacent greys must clear the threshold: got {variance}",
        );
    }

    #[test]
    fn statistics_of_nothing_are_zero_rather_than_nan() {
        assert_eq!(
            pcm_stats(&[]),
            AudioStats {
                peak: 0.0,
                rms: 0.0
            }
        );
        assert_eq!(
            luma_stats(&[]),
            ImageStats {
                mean: 0.0,
                variance: 0.0
            }
        );
    }

    /// The disabled thresholds are negative, and no measurement can be negative,
    /// so nothing is ever refused. This is what `[media] gate = false` becomes.
    #[test]
    fn disabled_thresholds_refuse_nothing() {
        assert!(
            evaluate(
                MediaKind::Audio,
                &wav16(&[0; 800]),
                GateThresholds::disabled()
            )
            .is_none()
        );
    }

    #[test]
    fn gate_reason_tokens_round_trip() {
        for reason in [GateReason::Silence, GateReason::Uniform] {
            assert_eq!(GateReason::from_token(reason.as_str()), Some(reason));
        }
        assert_eq!(GateReason::from_token("blank"), None);
    }
}

//! Audio metadata as `derived` facts — a **format read**, never a decode.
//!
//! A `.wav` file genuinely *is* 16 kHz, *is* mono, *is* 256 ms long. Those
//! statements are exactly as deterministic as "this Rust file declares
//! `fn extract`": same bytes, same answer, no clock, no sampling, no model. They
//! satisfy the `derived` contract in full, and until ADR-0016 the graph recorded
//! none of them.
//!
//! This module reads them with [`symphonia`]'s format layer. **No decoder is ever
//! instantiated** — [`read`] probes the container, inspects the track's declared
//! parameters, drains the metadata log, and drops the reader. Nothing here
//! allocates a sample buffer, and nothing here loads a model.
//!
//! # The complement of ADR-0015, not an exception to it
//!
//! ADR-0015 moved ASR transcripts and VLM descriptions *out* of `derived`,
//! because a model asked to transcribe silence returns confident prose rather
//! than nothing — generated text, not decoded text. This module moves extracted
//! facts *in*, for the opposite reason: a `fmt ` chunk saying `16000` is present
//! in the bytes and cannot be anything else. The two ADRs draw the same line from
//! its two sides, which is what makes the rule applicable rather than merely
//! memorable.
//!
//! Consequently nothing here touches [`crate::media`]'s store, and nothing there
//! touches these facts.
//!
//! # Exact, estimated, absent
//!
//! Duration is the one place where a deterministic fact can still be *inexact*,
//! and it is handled as a three-state answer rather than a nullable number:
//!
//! | Case | Container | Recorded as |
//! |---|---|---|
//! | **Exact** | WAV, FLAC — the container states the sample count | [`Exactness::Exact`] |
//! | **Estimated** | MP3 — a Xing/VBRI claim, or inferred from bitrate | [`Exactness::Estimated`] |
//! | **Absent** | the reader returned no frame count at all | no `duration` at all |
//!
//! [`AudioDuration`] pairs the number with its marker in one struct, so a
//! consumer cannot reach the milliseconds without passing the exactness — the
//! same "sum type, not a nullable column" discipline
//! [`crate::media::MediaOutcome`] uses, and for the same reason. Asserting an
//! approximate number under the graph's strongest provenance label would repeat,
//! in miniature, the mistake ADR-0015 exists to correct.
//!
//! # Determinism
//!
//! [`read`] is a pure function of the bytes:
//!
//! * duration is computed in **integer milliseconds** from the frame count and
//!   the sample rate — symphonia's own `Time` carries a nanosecond remainder we
//!   would have to round, and no floating point appears anywhere on this path;
//! * tags are **sorted**, never emitted in container order, because a container
//!   may legitimately store them in any order;
//! * every metadata revision is merged (an MP3 can carry `ID3v2` at the head *and*
//!   `ID3v1` at the tail), so the answer does not depend on which one a reader
//!   happens to surface first — and tags are then **de-duplicated on
//!   `(name, value)`**, so a fact two of those blocks both state is recorded once,
//!   with a survivor chosen by byte order rather than by drain order
//!   ([`read_tags`]).
//!
//! @rto:0016

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use symphonia::core::audio::Channels;
use symphonia::core::audio::sample::SampleFormat;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::{AudioCodecId, AudioCodecParameters, well_known as codec_ids};
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::{MetadataOptions, RawValue, StandardTag, Tag};

/// The `NodeKind::Other` token for the node these facts live on — one per audio
/// blob, keyed `audio:<path>`, hung off the `file` node by a `contains` edge.
///
/// A node of its own rather than extra keys on the `file` node's `meta`, for
/// three reasons that are all about not being mistaken for something else:
///
/// * **`meta.content` on an audio `file` node is the slot ADR-0015 emptied.** A
///   transcript used to live there. Putting extracted text back into that exact
///   slot would make "does this audio file node carry content?" ambiguous again,
///   which is the question ADR-0015 exists to keep answerable.
/// * **Searchability comes free.** [`crate::search`] matches on a node's name,
///   key, path and `meta.content`; a node with a rendered summary is found by the
///   ordinary scorer, with no new branch and therefore no new ranking rule. It
///   takes no `authored` boost, because it is `derived`.
/// * **It follows the house pattern.** `config_key` (ADR-0009) and `image_ref`
///   do the same thing: a derived sub-fact of a file becomes its own node under a
///   `contains`/`references` edge, rather than swelling the file node's `meta`.
pub const AUDIO_STREAM_KIND: &str = "audio_stream";

/// Longest tag value kept, in bytes. A tag is a label, not a document; a comment
/// field can nonetheless hold an entire liner note, and the graph should not grow
/// one per blob. Truncation is marked with a trailing `…` so a reader can see it
/// happened.
const MAX_TAG_VALUE: usize = 512;

/// Most tags kept per blob, after sorting and de-duplication. Bounds a
/// pathological file (thousands of `COMMENT` frames) without a heuristic about
/// which tags matter.
const MAX_TAGS: usize = 64;

/// How exactly a duration is known. There is no third variant for "unknown":
/// unknown is recorded by the **absence** of an [`AudioDuration`], not by a value
/// standing in for one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Exactness {
    /// The container states the sample count outright (WAV's `data` chunk length,
    /// FLAC's `STREAMINFO` total-samples field), so the duration follows from it
    /// by arithmetic.
    Exact,
    /// The number is a claim or an inference, not a measurement: an MP3's
    /// Xing/VBRI header states a frame count the encoder asserted, and with
    /// neither header present symphonia infers one from the bitrate and the
    /// stream length. **Every MPEG-audio duration is marked this way**, including
    /// one backed by a Xing header — see [`exactness_of_container`].
    Estimated,
}

impl Exactness {
    /// The stable token used in `meta` and in `--json` output.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Estimated => "estimated",
        }
    }
}

impl std::fmt::Display for Exactness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A duration and how exactly it is known, as one indivisible value.
///
/// The pairing is the point. A bare `duration_ms: Option<u64>` would let any
/// consumer — a CLI, a report, a future exporter — read the number and forget the
/// qualifier, which is precisely the failure mode ADR-0016 forbids. Here there is
/// no way to reach [`AudioDuration::ms`] without having the
/// [`AudioDuration::exactness`] in hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioDuration {
    /// Length in whole milliseconds, truncated. Integer arithmetic over the frame
    /// count and sample rate: no float, so no platform can round it differently.
    pub ms: u64,
    /// Whether [`AudioDuration::ms`] is stated by the container or inferred.
    pub exactness: Exactness,
}

impl std::fmt::Display for AudioDuration {
    /// `"256 ms (exact)"` / `"26122 ms (estimated)"`. The marker travels with the
    /// number in every rendering, which is how the "never show an estimate as
    /// exact" rule survives contact with a new display surface.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ms ({})", self.ms, self.exactness)
    }
}

/// One tag read out of the container.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AudioTag {
    /// The **normalised** name, `snake_case` — `track_title`, `album_artist`.
    ///
    /// Taken from symphonia's [`StandardTag`], which already maps `ID3v2`'s `TPE1`,
    /// a Vorbis comment's `ARTIST` and RIFF INFO's `IART` onto one vocabulary; we
    /// deliberately keep no tag-name table of our own (ADR-0016). When a tag has
    /// no standard mapping this falls back to the container's own key, lowercased.
    pub name: String,
    /// The value as it appears in the file.
    pub value: String,
    /// The container's own key for this tag (`TPE1`, `ARTIST`, `IART`), kept so
    /// the normalisation is auditable rather than lossy.
    ///
    /// **Not part of a tag's identity.** Two rows with the same `name` and `value`
    /// are one fact however many container keys stated it, so they are merged, and
    /// the lowest `source_key` in byte order is the one recorded — see
    /// [`read_tags`] for why that rule and not another.
    pub source_key: String,
}

/// Everything a format read yields for one audio blob.
///
/// Every field but [`AudioFacts::container`] and [`AudioFacts::codec`] is
/// optional, and an absent field means **the container did not say** — never a
/// default standing in for one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioFacts {
    /// The container format symphonia matched: `wave`, `flac`, `mp3`.
    pub container: String,
    /// The codec of the audio track: `pcm_s16le`, `flac`, `mp3`. Rendered as a
    /// hexadecimal codec id (`codec_0x1234`) when it is one this build has no name
    /// for — an honest placeholder rather than a guess.
    pub codec: String,
    /// Sampling rate in Hz.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate_hz: Option<u32>,
    /// Bits per sample, as coded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<u32>,
    /// Number of channels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channels: Option<u16>,
    /// The channel layout, when the container names positions:
    /// `front_left+front_right`, `discrete`, `ambisonic_order_1`, `custom`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_layout: Option<String>,
    /// The decoded sample format the codec parameters declare (`s16`, `f32`, …),
    /// where the container states one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_format: Option<String>,
    /// Playable audio frames, excluding encoder delay and padding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames: Option<u64>,
    /// Length, with its exactness. **Absent when the reader gave no frame count**
    /// — the degenerate-MP3 case, where recording a guess would be worse than
    /// recording nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<AudioDuration>,
    /// Tags, sorted and de-duplicated on `(name, value)`, so both the emission
    /// order and the set itself are a function of the content rather than of the
    /// container's layout — one row per thing the file says, however many of its
    /// tag blocks say it. See [`read_tags`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<AudioTag>,
}

impl AudioFacts {
    /// A one-line human rendering: `flac, 16000 Hz, mono, 16-bit, 4096 frames,
    /// 256 ms (exact)`.
    ///
    /// This is what lands in the node's `meta.content`, which is what
    /// [`crate::search`] matches on — so `roteiro search flac`, `roteiro search
    /// 16000` and `roteiro search stereo` all find the blob through the ordinary
    /// scorer. The duration is rendered through [`AudioDuration`]'s `Display`, so
    /// an estimate is shown *as* an estimate here too.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut parts = vec![self.container.clone()];
        if self.codec != self.container {
            parts.push(self.codec.clone());
        }
        if let Some(rate) = self.sample_rate_hz {
            parts.push(format!("{rate} Hz"));
        }
        if let Some(channels) = self.channels {
            parts.push(channel_word(channels));
        }
        if let Some(layout) = &self.channel_layout {
            parts.push(layout.clone());
        }
        if let Some(depth) = self.bit_depth {
            parts.push(format!("{depth}-bit"));
        }
        if let Some(format) = &self.sample_format {
            parts.push(format.clone());
        }
        if let Some(frames) = self.frames {
            parts.push(format!("{frames} frames"));
        }
        if let Some(duration) = self.duration {
            parts.push(duration.to_string());
        } else {
            // Say so out loud. A summary that simply omitted the duration would
            // read as "short clip" rather than "the container did not say".
            parts.push("duration unknown".to_owned());
        }
        let mut out = parts.join(", ");
        for tag in &self.tags {
            // One tag per line, so a search snippet of the summary reads as a
            // list rather than as a run-on sentence.
            let _ = write!(out, "\n{}: {}", tag.name, tag.value);
        }
        out
    }
}

/// Read one audio blob's metadata from its bytes, or `None` when the container
/// yields nothing usable at all.
///
/// # `None` and an absent duration are different answers
///
/// Both are forms of "we do not know", and ADR-0016 turns on keeping them apart,
/// so this is the contract:
///
/// * **`None`** — the bytes are not a readable audio container: the probe matched
///   no format, or the container carries no audio track. Nothing is known, so
///   [`crate::extract`] emits **no `audio_stream` node** for the blob (the `file`
///   node is unaffected). Pinned by `a_blob_the_reader_rejects_yields_no_facts`
///   and `an_unreadable_audio_blob_emits_no_stream_node`.
/// * **`Some` with [`AudioFacts::duration`] `None`** — the container *is* readable
///   and its facts are recorded; it simply states no frame count, so no duration
///   can be derived. This is the expected outcome for the repository's degenerate
///   1 040-byte MP3 fixture, which still yields codec, sample rate and channel
///   count from its frame headers. Pinned by
///   `a_container_that_states_no_length_yields_no_duration_at_all`.
///
/// Conflating the two would undo the point: ADR-0016 exists so that an unknown
/// duration is *recorded as unknown* rather than guessed, and a partially-known
/// blob keeps the parts that are known instead of being discarded whole. Every
/// optional field on [`AudioFacts`] behaves the same way — absent means the
/// container did not say, never a default standing in for one.
///
/// `extension` seeds symphonia's probe [`Hint`]; it is an optimisation, and a
/// wrong or missing hint changes nothing about the answer, only the search the
/// probe does to reach it.
///
/// # Panics guarded
/// The call is wrapped in [`std::panic::catch_unwind`], following the `pdf-text`
/// and `image-ocr` precedent in [`crate::extract`]: symphonia forbids `unsafe`
/// and is fuzz-tested, but a malformed blob must degrade to "no facts" rather
/// than abort an entire `sync`.
#[must_use]
pub fn read(bytes: &[u8], extension: Option<&str>) -> Option<AudioFacts> {
    // `&[u8]`/`&str` are unwind-safe, so the borrow needs no `AssertUnwindSafe`
    // and the (up to 50 MiB) blob is not cloned.
    std::panic::catch_unwind(|| read_inner(bytes, extension))
        .ok()
        .flatten()
}

/// The body of [`read`], outside the panic guard.
fn read_inner(bytes: &[u8], extension: Option<&str>) -> Option<AudioFacts> {
    // A `Cursor` over the blob is seekable and knows its length, which is what
    // lets symphonia's MP3 reader estimate a duration at all — an unseekable
    // stream gets no frame count, and would turn every MP3 into the absent case.
    let source = std::io::Cursor::new(bytes);
    let stream = MediaSourceStream::new(Box::new(source), MediaSourceStreamOptions::default());
    let mut hint = Hint::new();
    if let Some(ext) = extension {
        hint.with_extension(ext);
    }
    let mut reader = symphonia::default::get_probe()
        .probe(
            &hint,
            stream,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .ok()?;

    let container = reader.format_info().short_name.to_owned();
    // The *default* audio track, not merely the first: a container may carry more
    // than one, and `default_track` applies the container's own choice rather than
    // ours. A file with no audio track at all yields no facts.
    let track = reader.default_track(TrackType::Audio)?;
    let params = match track.codec_params.as_ref() {
        Some(CodecParameters::Audio(params)) => Some(params),
        // A track whose codec the container did not describe still has a frame
        // count and a timebase worth recording; only the stream shape is missing.
        _ => None,
    };
    let sample_rate_hz = params.and_then(|p| p.sample_rate);
    let frames = track.num_frames;
    let duration = duration_of(frames, sample_rate_hz, &container);

    let facts = AudioFacts {
        codec: params.map_or_else(|| "unknown".to_owned(), |p| codec_name(p.codec)),
        sample_rate_hz,
        bit_depth: params.and_then(bit_depth_of),
        channels: params
            .and_then(|p| p.channels.as_ref())
            .and_then(channel_count),
        channel_layout: params
            .and_then(|p| p.channels.as_ref())
            .and_then(channel_layout),
        sample_format: params
            .and_then(|p| p.sample_format)
            .map(|f| sample_format_name(f).to_owned()),
        frames,
        duration,
        tags: read_tags(&mut reader),
        container,
    };
    Some(facts)
}

/// Merge, normalise, sort and de-duplicate every tag in the reader's metadata
/// log.
///
/// **All** revisions are drained, not just the current one: an MP3 can carry an
/// `ID3v2` tag at the head and an `ID3v1` tag at the tail, and each arrives as its own
/// revision. Sorting afterwards means the result does not depend on which
/// revision a reader surfaces first, nor on the order tags appear within one.
///
/// # De-duplication is on `(name, value)`, not on the whole row
///
/// That same two-source MP3 is exactly why. Its artist arrives twice — once as
/// `ID3v2`'s `TPE1`, once as `ID3v1`'s `ARTIST` — and both normalise to the same
/// [`AudioTag::name`] and the same [`AudioTag::value`]. They are **one fact stated
/// twice**, so they must collapse to one row; de-duplicating on the derived `Eq`
/// would compare [`AudioTag::source_key`] too, keep both, and make
/// [`AudioFacts::summary`] print `artist: …` on two consecutive lines.
///
/// Which `source_key` survives is decided by the sort, not by drain order: rows
/// are ordered by the full `(name, value, source_key)` triple, and `dedup_by`
/// keeps the **first** of each `(name, value)` run — so the lowest `source_key` in
/// byte order wins. Two properties follow, and both are load-bearing:
///
/// * it is **total and deterministic**, because the ordering is over bytes that
///   are themselves a function of the file. A rule like "keep whichever revision
///   was drained first" would look identical on any single read while quietly
///   making the exported factset depend on symphonia's revision ordering — which
///   an upstream release could change without a byte of the file moving, breaking
///   ADR-0016's byte-identical-facts requirement;
/// * it deliberately **does not rank tag formats**. Preferring `ID3v2` over
///   `ID3v1` as the richer source is a defensible instinct, but it cannot be
///   implemented from what we have: nothing on a [`Tag`] says which reader
///   produced it, so the preference would have to be *guessed* from the key's
///   shape — a per-format table, which is the thing ADR-0016 adopted symphonia's
///   normalisation to avoid.
///
/// The concrete consequence is worth stating outright rather than leaving to be
/// discovered: `ARTIST` sorts before `TPE1`, so it is the `ID3v1` key that
/// survives on such a file. Nothing about the *fact* changes with it — the name
/// and value are identical by construction, and the discarded key is a second
/// label for the same statement, not a second statement.
fn read_tags(reader: &mut Box<dyn symphonia::core::formats::FormatReader + '_>) -> Vec<AudioTag> {
    let mut out: Vec<AudioTag> = Vec::new();
    {
        let mut log = reader.metadata();
        loop {
            if let Some(revision) = log.current() {
                out.extend(revision.media.tags.iter().filter_map(normalize_tag));
            }
            // `pop` discards the front revision and returns it, leaving the next
            // one current; it returns `None` (without emptying) once one is left,
            // so every revision is visited exactly once and the loop terminates.
            if log.pop().is_none() {
                break;
            }
        }
    }
    // Sort on the full triple, so the run of rows sharing a `(name, value)` is
    // contiguous *and* internally ordered — that second part is what makes the
    // survivor below a function of the bytes rather than of the drain order.
    out.sort();
    // `dedup_by` passes the later element first and drops it when the closure
    // returns true, so the earliest of each run — the lowest `source_key` — is the
    // one kept.
    out.dedup_by(|later, earlier| later.name == earlier.name && later.value == earlier.value);
    out.truncate(MAX_TAGS);
    out
}

/// Turn one symphonia [`Tag`] into an [`AudioTag`], or drop it.
///
/// Dropped: tags whose value is binary (an embedded image or fingerprint blob is
/// not text, and hex-dumping it would put kilobytes of noise in the graph), and
/// tags that render to nothing.
fn normalize_tag(tag: &Tag) -> Option<AudioTag> {
    if matches!(tag.raw.value, RawValue::Binary(_)) {
        return None;
    }
    let value = cap_tag_value(&tag.raw.value.to_string());
    if value.is_empty() {
        return None;
    }
    let source_key = tag.raw.key.clone();
    let name = tag
        .std
        .as_ref()
        .and_then(standard_tag_name)
        .unwrap_or_else(|| source_key.to_lowercase());
    Some(AudioTag {
        name,
        value,
        source_key,
    })
}

/// The `snake_case` name of a [`StandardTag`] variant — `Album` → `album`,
/// `AlbumArtist` → `album_artist`.
///
/// **Derived from the variant's `Debug` rendering, deliberately.** `StandardTag`
/// has 211 single-payload variants, is `#[non_exhaustive]`, and exposes no name
/// accessor; a `match` over it would be 211 lines of boilerplate that must be
/// re-checked on every upstream release, for a mapping the derive already
/// encodes. So this reads `"Album(\"x\")"` and takes the identifier in front of
/// the `(`.
///
/// The two things that make that safe rather than clever: the dependency is
/// **pinned to an exact version**, and the derivation is **validated at the point
/// of use** — anything that is not a plain `UpperCamelCase` identifier returns
/// `None`, and the caller falls back to the container's own key. So the worst
/// outcome of an upstream change is a tag named `tpe1` instead of `artist`, never
/// a malformed or non-deterministic name. [`standard_tag_names_are_snake_cased`]
/// pins the behaviour.
fn standard_tag_name(std: &StandardTag) -> Option<String> {
    let debug = format!("{std:?}");
    let variant = debug.split('(').next().unwrap_or_default();
    let mut chars = variant.chars();
    // Must look like `UpperCamelCase`: a leading uppercase letter, then letters
    // and digits only. A `Debug` shape we do not recognise is not guessed at.
    if !chars.next().is_some_and(|c| c.is_ascii_uppercase()) || !chars.all(char::is_alphanumeric) {
        return None;
    }
    let mut out = String::with_capacity(variant.len() + 4);
    for (i, c) in variant.char_indices() {
        if c.is_ascii_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
    }
    Some(out)
}

/// Truncate a tag value to [`MAX_TAG_VALUE`] bytes on a character boundary,
/// marking the cut with `…`. Also collapses newlines, so one tag stays one line
/// in the rendered summary.
fn cap_tag_value(value: &str) -> String {
    let flat: String = value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let flat = flat.trim();
    if flat.len() <= MAX_TAG_VALUE {
        return flat.to_owned();
    }
    let mut end = MAX_TAG_VALUE;
    while end > 0 && !flat.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &flat[..end])
}

/// Duration in whole milliseconds from a frame count and a sample rate, marked
/// with the exactness the container earns.
///
/// `None` — recorded as absence — whenever either input is missing or zero. That
/// is the degenerate-MP3 case: no frame count, so no duration, so no field.
fn duration_of(
    frames: Option<u64>,
    sample_rate_hz: Option<u32>,
    container: &str,
) -> Option<AudioDuration> {
    let frames = frames?;
    let rate = u64::from(sample_rate_hz?);
    if rate == 0 {
        return None;
    }
    // Integer arithmetic in u128 so a long stream at a high rate cannot overflow
    // the intermediate product; the result is milliseconds, which always fits.
    let ms = u64::try_from(u128::from(frames) * 1000 / u128::from(rate)).unwrap_or(u64::MAX);
    Some(AudioDuration {
        ms,
        exactness: exactness_of_container(container),
    })
}

/// How exactly a container's frame count can be trusted.
///
/// **Every MPEG-audio stream is [`Exactness::Estimated`]**, including one whose
/// frame count came from a Xing or VBRI header. Two reasons, and the first alone
/// decides it:
///
/// * symphonia's `MpaReader` reaches a frame count by three different routes —
///   Xing, VBRI, or an inference from bitrate and stream length — and **exposes
///   no flag saying which**. Distinguishing them would mean parsing the first
///   MPEG frame ourselves for a Xing/Info/VBRI header, i.e. hand-rolling exactly
///   the MP3 header parsing ADR-0016 declined to hand-roll.
/// * A Xing frame count is in any case the *encoder's claim* about the stream, not
///   a property the container measures the way a WAV `data` chunk length does.
///
/// So this errs in the safe direction: the only way to be wrong here is to call
/// an estimate exact, and this cannot. Unknown containers are estimated for the
/// same reason.
fn exactness_of_container(container: &str) -> Exactness {
    match container {
        // The `data` chunk length and `STREAMINFO`'s total-samples field are
        // statements of fact by the container, not claims about it.
        "wave" | "flac" => Exactness::Exact,
        _ => Exactness::Estimated,
    }
}

/// Bits per sample: the coded width where the container states one, else the
/// decoded width.
fn bit_depth_of(params: &AudioCodecParameters) -> Option<u32> {
    params.bits_per_coded_sample.or(params.bits_per_sample)
}

/// Number of channels, or `None` for a container that declared none.
fn channel_count(channels: &Channels) -> Option<u16> {
    u16::try_from(channels.count()).ok().filter(|n| *n > 0)
}

/// A stable rendering of the channel layout. Positioned layouts list their
/// speaker positions in bit order (`front_left+front_right`), which is fixed by
/// the flag definitions and therefore deterministic.
fn channel_layout(channels: &Channels) -> Option<String> {
    match channels {
        Channels::Positioned(positions) => {
            let names: Vec<String> = positions
                .iter_names()
                .map(|(name, _)| name.to_lowercase())
                .collect();
            (!names.is_empty()).then(|| names.join("+"))
        }
        Channels::Discrete(_) => Some("discrete".to_owned()),
        Channels::Ambisonic(order) => Some(format!("ambisonic_order_{order}")),
        Channels::Custom(_) => Some("custom".to_owned()),
        Channels::None => None,
        // `Channels` is `#[non_exhaustive]`: an upstream addition renders as a
        // name we do not claim to know, rather than failing to build.
        _ => Some("other".to_owned()),
    }
}

/// `"mono"` / `"stereo"` / `"3 channels"` — the word a human reads, and the word
/// a search query is likely to use.
fn channel_word(channels: u16) -> String {
    match channels {
        1 => "mono".to_owned(),
        2 => "stereo".to_owned(),
        n => format!("{n} channels"),
    }
}

/// The stable token for a decoded sample format.
///
/// Exhaustive on purpose: `SampleFormat` is not `#[non_exhaustive]`, so an
/// upstream addition is a compile error here rather than a silent `"unknown"` —
/// which is what we want from a version-pinned dependency.
fn sample_format_name(format: SampleFormat) -> &'static str {
    match format {
        SampleFormat::U8 => "u8",
        SampleFormat::U16 => "u16",
        SampleFormat::U24 => "u24",
        SampleFormat::U32 => "u32",
        SampleFormat::S8 => "s8",
        SampleFormat::S16 => "s16",
        SampleFormat::S24 => "s24",
        SampleFormat::S32 => "s32",
        SampleFormat::F32 => "f32",
        SampleFormat::F64 => "f64",
    }
}

/// The stable token for an audio codec id.
///
/// Covers exactly what the three enabled readers can emit — the RIFF reader's PCM
/// and ADPCM variants, the MPEG reader's three layers, and FLAC — because
/// symphonia only names a codec through a *registered decoder*, and this build
/// registers none (it never decodes). Anything else renders as its hexadecimal id
/// rather than as a guess or an empty string.
fn codec_name(codec: AudioCodecId) -> String {
    let known = [
        (codec_ids::CODEC_ID_PCM_S8, "pcm_s8"),
        (codec_ids::CODEC_ID_PCM_U8, "pcm_u8"),
        (codec_ids::CODEC_ID_PCM_S16LE, "pcm_s16le"),
        (codec_ids::CODEC_ID_PCM_S16BE, "pcm_s16be"),
        (codec_ids::CODEC_ID_PCM_S24LE, "pcm_s24le"),
        (codec_ids::CODEC_ID_PCM_S24BE, "pcm_s24be"),
        (codec_ids::CODEC_ID_PCM_S32LE, "pcm_s32le"),
        (codec_ids::CODEC_ID_PCM_S32BE, "pcm_s32be"),
        (codec_ids::CODEC_ID_PCM_F32LE, "pcm_f32le"),
        (codec_ids::CODEC_ID_PCM_F32BE, "pcm_f32be"),
        (codec_ids::CODEC_ID_PCM_F64LE, "pcm_f64le"),
        (codec_ids::CODEC_ID_PCM_F64BE, "pcm_f64be"),
        (codec_ids::CODEC_ID_PCM_ALAW, "pcm_alaw"),
        (codec_ids::CODEC_ID_PCM_MULAW, "pcm_mulaw"),
        (codec_ids::CODEC_ID_ADPCM_MS, "adpcm_ms"),
        (codec_ids::CODEC_ID_ADPCM_IMA_WAV, "adpcm_ima_wav"),
        (codec_ids::CODEC_ID_MP1, "mp1"),
        (codec_ids::CODEC_ID_MP2, "mp2"),
        (codec_ids::CODEC_ID_MP3, "mp3"),
        (codec_ids::CODEC_ID_FLAC, "flac"),
    ];
    known
        .iter()
        .find(|(id, _)| *id == codec)
        .map_or_else(|| format!("codec_{codec}"), |(_, name)| (*name).to_owned())
}

#[cfg(test)]
mod tests {
    use super::{
        AudioDuration, AudioFacts, AudioTag, Exactness, cap_tag_value, channel_word, codec_name,
        duration_of, exactness_of_container, standard_tag_name,
    };
    use symphonia::core::codecs::audio::well_known as codec_ids;
    use symphonia::core::meta::StandardTag;

    fn facts() -> AudioFacts {
        AudioFacts {
            container: "flac".to_owned(),
            codec: "flac".to_owned(),
            sample_rate_hz: Some(16_000),
            bit_depth: Some(16),
            channels: Some(1),
            channel_layout: Some("front_center".to_owned()),
            sample_format: None,
            frames: Some(4096),
            duration: Some(AudioDuration {
                ms: 256,
                exactness: Exactness::Exact,
            }),
            tags: Vec::new(),
        }
    }

    #[test]
    fn duration_is_integer_milliseconds_from_frames_and_rate() {
        let d = duration_of(Some(4096), Some(16_000), "flac").expect("duration");
        assert_eq!(d.ms, 256);
        assert_eq!(d.exactness, Exactness::Exact);
        // Truncation, not rounding — and no float anywhere on the path.
        assert_eq!(
            duration_of(Some(11_520), Some(44_100), "mp3")
                .expect("duration")
                .ms,
            261
        );
    }

    /// The three-state contract: a missing input records **absence**, never a
    /// zero or a guess.
    #[test]
    fn a_missing_frame_count_or_rate_yields_no_duration_at_all() {
        assert_eq!(duration_of(None, Some(16_000), "flac"), None);
        assert_eq!(duration_of(Some(4096), None, "flac"), None);
        assert_eq!(duration_of(Some(4096), Some(0), "flac"), None);
    }

    #[test]
    fn mpeg_audio_is_always_estimated_and_lossless_containers_are_exact() {
        assert_eq!(exactness_of_container("wave"), Exactness::Exact);
        assert_eq!(exactness_of_container("flac"), Exactness::Exact);
        // Including a stream whose frame count came from a Xing header: symphonia
        // does not say which route it took, so we never claim the stronger label.
        assert_eq!(exactness_of_container("mp3"), Exactness::Estimated);
        assert_eq!(exactness_of_container("mp2"), Exactness::Estimated);
        // An unknown container errs the safe way.
        assert_eq!(exactness_of_container("ogg"), Exactness::Estimated);
    }

    /// Every rendering of a duration carries its marker. This is the property
    /// that makes "no surface shows an estimate as exact" a fact about the type
    /// rather than an audit of the call sites.
    #[test]
    fn every_duration_rendering_names_its_exactness() {
        assert_eq!(
            AudioDuration {
                ms: 256,
                exactness: Exactness::Exact
            }
            .to_string(),
            "256 ms (exact)"
        );
        assert_eq!(
            AudioDuration {
                ms: 26_122,
                exactness: Exactness::Estimated
            }
            .to_string(),
            "26122 ms (estimated)"
        );
    }

    #[test]
    fn a_summary_reads_as_a_sentence_and_carries_the_tags() {
        let mut f = facts();
        f.tags = vec![AudioTag {
            name: "artist".to_owned(),
            value: "Someone".to_owned(),
            source_key: "ARTIST".to_owned(),
        }];
        let summary = f.summary();
        assert!(
            summary.starts_with(
                "flac, 16000 Hz, mono, front_center, 16-bit, 4096 frames, 256 ms (exact)"
            ),
            "got {summary}"
        );
        assert!(summary.contains("\nartist: Someone"), "got {summary}");
    }

    /// An absent duration is said out loud, so a reader cannot mistake silence
    /// for brevity.
    #[test]
    fn a_summary_says_when_the_duration_is_unknown() {
        let mut f = facts();
        f.duration = None;
        assert!(f.summary().contains("duration unknown"), "{}", f.summary());
    }

    #[test]
    fn standard_tag_names_are_snake_cased() {
        use std::sync::Arc;

        let name = |t: StandardTag| standard_tag_name(&t);
        assert_eq!(
            name(StandardTag::Album(Arc::new("x".to_owned()))),
            Some("album".to_owned())
        );
        assert_eq!(
            name(StandardTag::AlbumArtist(Arc::new("x".to_owned()))),
            Some("album_artist".to_owned())
        );
        assert_eq!(
            name(StandardTag::TrackTitle(Arc::new("x".to_owned()))),
            Some("track_title".to_owned())
        );
        // A non-string payload takes the same path.
        assert_eq!(
            name(StandardTag::TrackNumber(3)),
            Some("track_number".to_owned())
        );
    }

    #[test]
    fn a_tag_value_is_flattened_and_bounded() {
        assert_eq!(cap_tag_value("  a\nb  "), "a b");
        let long = cap_tag_value(&"x".repeat(super::MAX_TAG_VALUE + 10));
        assert!(long.ends_with('…'));
        assert!(long.len() <= super::MAX_TAG_VALUE + '…'.len_utf8());
        // A multi-byte character straddling the cut is not split.
        let wide = cap_tag_value(&"é".repeat(super::MAX_TAG_VALUE));
        assert!(wide.ends_with('…'));
    }

    #[test]
    fn codec_ids_render_as_names_or_as_their_id() {
        assert_eq!(codec_name(codec_ids::CODEC_ID_FLAC), "flac");
        assert_eq!(codec_name(codec_ids::CODEC_ID_MP3), "mp3");
        assert_eq!(codec_name(codec_ids::CODEC_ID_PCM_S16LE), "pcm_s16le");
        // A codec this build has no name for is reported as its id, not guessed.
        assert!(codec_name(codec_ids::CODEC_ID_OPUS).starts_with("codec_"));
    }

    #[test]
    fn channel_counts_read_as_words() {
        assert_eq!(channel_word(1), "mono");
        assert_eq!(channel_word(2), "stereo");
        assert_eq!(channel_word(6), "6 channels");
    }

    /// An absent optional field must not serialise as `null`: `meta` records what
    /// the container said, and a `null` reads as a value.
    #[test]
    fn absent_fields_are_omitted_from_json_entirely() {
        let bare = AudioFacts {
            container: "mp3".to_owned(),
            codec: "mp3".to_owned(),
            sample_rate_hz: None,
            bit_depth: None,
            channels: None,
            channel_layout: None,
            sample_format: None,
            frames: None,
            duration: None,
            tags: Vec::new(),
        };
        assert_eq!(
            serde_json::to_string(&bare).expect("serialize"),
            r#"{"container":"mp3","codec":"mp3"}"#
        );
    }

    /// The duration is one indivisible value in JSON too: there is no bare
    /// `duration_ms` key a consumer could read without its marker.
    #[test]
    fn duration_serialises_with_its_exactness_attached() {
        let json = serde_json::to_value(facts()).expect("serialize");
        assert_eq!(json["duration"]["ms"], 256);
        assert_eq!(json["duration"]["exactness"], "exact");
        assert!(json.get("duration_ms").is_none());
    }
}

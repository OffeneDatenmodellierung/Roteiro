//! Generates — and gates the determinism of — the committed audio fixtures in
//! `tests/fixtures/audio/`.
//!
//! The repository carries no third-party audio: every fixture is *synthesised
//! here*, by the encoders below, from integer-only signal generators. That buys
//! three things the project cares about:
//!
//! * **Licence cleanliness.** Nothing was downloaded, so nothing carries an
//!   unknown provenance. The fixtures are original works of this repository,
//!   under the same `MIT OR Apache-2.0` terms as the code that emits them.
//! * **Determinism.** No clock, no encoder-version string, no floating point
//!   (the sine table is a hard-coded Q15 constant, not `f64::sin`), so the same
//!   source produces the same bytes on every platform — the property
//!   [`fixtures_are_byte_reproducible`] actually asserts.
//! * **No new dependency.** No audio-encoding crate enters the tree, not even
//!   as a dev-dependency. WAV, FLAC and MP3 are written by hand; see each
//!   module for the format notes.
//!
//! # Regenerating
//!
//! ```text
//! ROTEIRO_WRITE_AUDIO_FIXTURES=1 cargo test -p rto-graph --test audio_fixtures
//! ```
//!
//! That rewrites every file under `tests/fixtures/audio/` and then re-runs the
//! comparison, so a passing run means the committed bytes are exactly what this
//! source produces. Without the variable the test only *checks*, and fails with
//! the offending path when a fixture has drifted.
//!
//! # Consumers outside this file
//!
//! `tests/audio_ingest.rs` reads the whole set at run time. Less obviously,
//! `syllables-16khz-mono-512ms.wav` is included via `include_bytes!` by the
//! two-modality teardown test *inside the library* (`src/extract.rs`).
//!
//! That test cannot call [`wav::encode`] because this file is compiled as its
//! own crate, which links the library rather than the other way round — nothing
//! here is nameable from `src/`. The reverse is blocked too, but for a
//! different reason: the library is rebuilt *without* `--cfg test` when an
//! integration-test crate links it, so a `#[cfg(test)]` helper in `src/` would
//! not exist here either. Sharing the committed artefact rather than the
//! encoder's source is therefore what keeps this the workspace's only WAV
//! writer (#302), and renaming that one file is a compile error in `src/`, not
//! just a failure here.

use std::path::PathBuf;

/// Sample rate of the WAV/FLAC fixtures. 16 kHz is what speech models want, and
/// it is one of FLAC's directly-encodable rates (no rate escape needed).
const RATE_16K: u32 = 16_000;

/// Fixture length for the WAV/FLAC clips: one whole FLAC block (4096 samples =
/// 256 ms at 16 kHz), so every FLAC is exactly one frame.
const BLOCK: usize = 4096;

/// Environment variable that switches this file from *checking* the fixtures to
/// *writing* them.
const WRITE_VAR: &str = "ROTEIRO_WRITE_AUDIO_FIXTURES";

/// `tests/fixtures/audio/`, resolved from the crate manifest so the test works
/// from any working directory.
fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("audio")
}

/// Every committed fixture: file name paired with the function that produces its
/// exact bytes. Adding a row and re-running with [`WRITE_VAR`] set is the whole
/// process for adding a fixture.
fn fixtures() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        (
            "silence-16khz-mono-256ms.wav",
            wav::encode(RATE_16K, &signal::silence(BLOCK)),
        ),
        (
            "tone-500hz-16khz-mono-256ms.wav",
            wav::encode(RATE_16K, &signal::tone(BLOCK)),
        ),
        (
            "syllables-16khz-mono-512ms.wav",
            wav::encode(RATE_16K, &signal::syllables()),
        ),
        (
            "silence-16khz-mono-256ms.flac",
            flac::encode(&signal::silence(BLOCK)),
        ),
        (
            "tone-500hz-16khz-mono-256ms.flac",
            flac::encode(&signal::tone(BLOCK)),
        ),
        ("silence-44khz-mono-261ms.mp3", mp3::silence(10)),
    ]
}

/// How many fixtures [`fixtures`] must yield. Both tests here loop over that
/// list, so an accidentally-empty list would make them pass while checking
/// nothing at all; pinning the count is what stops a vacuous green.
const FIXTURE_COUNT: usize = 6;

#[test]
fn fixtures_are_byte_reproducible() {
    let dir = fixture_dir();
    let write = std::env::var_os(WRITE_VAR).is_some();
    if write {
        std::fs::create_dir_all(&dir).expect("create fixture dir");
    }

    assert_eq!(
        fixtures().len(),
        FIXTURE_COUNT,
        "the fixture list changed size"
    );
    for (name, bytes) in fixtures() {
        let path = dir.join(name);
        if write {
            std::fs::write(&path, &bytes).expect("write fixture");
            eprintln!("wrote {} ({} bytes)", path.display(), bytes.len());
        }
        let on_disk = std::fs::read(&path).unwrap_or_else(|e| {
            panic!(
                "read {}: {e} — regenerate with `{WRITE_VAR}=1`",
                path.display()
            )
        });
        assert_eq!(
            on_disk, bytes,
            "{name} has drifted from its generator; regenerate with `{WRITE_VAR}=1 cargo test \
             -p rto-graph --test audio_fixtures`",
        );
    }
}

/// The fixtures must satisfy the *sniffer* llama.cpp applies before it hands a
/// buffer to miniaudio: `mtmd-helper.cpp`'s `audio_helpers::is_audio_file`
/// requires at least 12 bytes and one of three magic patterns. A fixture that
/// fails this is silently treated as an image and rejected, so pin the contract
/// here rather than discovering it inside a three-gigabyte model run.
#[test]
fn fixtures_carry_the_magic_bytes_the_projector_sniffs_for() {
    assert_eq!(
        fixtures().len(),
        FIXTURE_COUNT,
        "the fixture list changed size"
    );
    for (name, bytes) in fixtures() {
        assert!(
            bytes.len() >= 12,
            "{name}: mtmd rejects buffers shorter than 12 bytes",
        );
        let sniffed = match name.rsplit('.').next() {
            // RIFF....WAVE
            Some("wav") => &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE",
            // An MPEG sync word (or an ID3 tag, which these fixtures do not use).
            Some("mp3") => bytes[0] == 0xFF && bytes[1] & 0xE0 == 0xE0,
            Some("flac") => &bytes[..4] == b"fLaC",
            other => panic!("{name}: unexpected extension {other:?}"),
        };
        assert!(
            sniffed,
            "{name}: magic bytes would not be recognised as audio"
        );
    }
}

/// Integer-only signal generators. No floating point anywhere, so the emitted
/// samples are bit-identical on every target — the property the fixtures' byte
/// reproducibility rests on.
mod signal {
    /// One quarter-period of a sine in Q15, i.e. `sin(2πk/32) * 32767` rounded to
    /// the nearest integer for `k` in `0..=8`. Hard-coded rather than computed so
    /// no platform `libm` can round the last bit differently:
    ///
    /// ```text
    /// python3 -c "import math; print([round(math.sin(2*math.pi*k/32)*32767) for k in range(9)])"
    /// ```
    const QUARTER_Q15: [i32; 9] = [0, 6393, 12539, 18204, 23170, 27245, 30273, 32137, 32767];

    /// `sin(2πk/32)` in Q15, for any `k`, by reflecting [`QUARTER_Q15`] through
    /// the sine's quarter-wave symmetry. A 32-sample period at 16 kHz is exactly
    /// 500 Hz, which is why the fixtures use 500 Hz and not the conventional
    /// 440 Hz: it lands on whole samples, so no interpolation (and no float) is
    /// needed.
    fn sin_q15(k: usize) -> i32 {
        match k % 32 {
            n @ 0..=8 => QUARTER_Q15[n],
            n @ 9..=16 => QUARTER_Q15[16 - n],
            n @ 17..=24 => -QUARTER_Q15[n - 16],
            n => -QUARTER_Q15[32 - n],
        }
    }

    /// Scale a Q15 value by `amplitude` and saturate into `i16`.
    fn scale(q15: i32, amplitude: i32) -> i16 {
        let v = (q15 * amplitude) >> 15;
        i16::try_from(v.clamp(i32::from(i16::MIN), i32::from(i16::MAX))).expect("clamped to i16")
    }

    /// `n` samples of digital silence. The determinism fixture: a transcript of
    /// silence should be stable and empty-ish, so a model that "hears" words in
    /// it is hallucinating, not transcribing.
    pub fn silence(n: usize) -> Vec<i16> {
        vec![0; n]
    }

    /// `n` samples of a 500 Hz sine at roughly -9 dBFS.
    pub fn tone(n: usize) -> Vec<i16> {
        (0..n).map(|i| scale(sin_q15(i), 11_500)).collect()
    }

    /// Length of one voiced burst, in samples (100 ms at 16 kHz).
    const BURST: usize = 1600;
    /// Silence between bursts, in samples (28 ms at 16 kHz).
    const GAP: usize = 448;
    /// Samples over which a burst fades in and out, so it starts and stops like
    /// a syllable rather than a click.
    const RAMP: i32 = 200;

    /// A spoken-word-*shaped* signal: four voiced bursts separated by gaps, each
    /// a 500/1000/2000 Hz harmonic stack under a trapezoidal envelope. It is not
    /// speech and does not claim to be — it is a signal with a syllabic envelope
    /// and vowel-like harmonic structure, so an ASR model is given something with
    /// the coarse shape of an utterance instead of a steady tone.
    ///
    /// The harmonics are the 1st, 2nd and 4th of a 32-sample fundamental, so
    /// every one lands on whole samples and the whole clip stays integer-exact.
    pub fn syllables() -> Vec<i16> {
        let period = BURST + GAP;
        (0..4 * period)
            .map(|i| {
                let at = i % period;
                if at >= BURST {
                    return 0;
                }
                let rise = i32::try_from(at).expect("burst index fits i32");
                let fall = i32::try_from(BURST - at).expect("burst index fits i32");
                // Trapezoid: linear in over RAMP samples, flat, linear out.
                let envelope = rise.min(fall).min(RAMP);
                let stack = sin_q15(i) * 4 + sin_q15(2 * i) * 2 + sin_q15(4 * i);
                scale(stack / 7, 11_500 * envelope / RAMP)
            })
            .collect()
    }
}

/// A canonical 44-byte-header RIFF/WAVE writer for 16-bit mono PCM.
///
/// Only the two mandatory chunks are emitted (`fmt ` then `data`) and no `LIST`
/// or `INFO` metadata, which is what keeps the output free of encoder names and
/// timestamps.
mod wav {
    /// Encode `samples` as 16-bit mono PCM at `rate` Hz.
    pub fn encode(rate: u32, samples: &[i16]) -> Vec<u8> {
        let data_len = u32::try_from(samples.len() * 2).expect("fixture fits a u32 data chunk");
        let mut out = Vec::with_capacity(44 + samples.len() * 2);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len).to_le_bytes());
        out.extend_from_slice(b"WAVE");

        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes()); // PCM `fmt ` chunk size
        out.extend_from_slice(&1u16.to_le_bytes()); // WAVE_FORMAT_PCM
        out.extend_from_slice(&1u16.to_le_bytes()); // mono
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 2).to_le_bytes()); // byte rate
        out.extend_from_slice(&2u16.to_le_bytes()); // block align
        out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample

        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        for s in samples {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out
    }
}

/// A minimal FLAC writer: `STREAMINFO` plus one frame of VERBATIM subframes.
///
/// FLAC's compression comes from LPC/fixed predictors, which a fixture does not
/// need; the format also defines a VERBATIM subframe that stores raw samples.
/// Using it makes the encoder about eighty lines instead of two thousand, and
/// the result is a fully spec-conformant FLAC stream — just an incompressible
/// one, which for a 4096-sample clip costs 8 KB.
///
/// Every parameter is chosen so the bitstream is byte-aligned end to end (16-bit
/// samples, one channel, a block size and sample rate that both have direct
/// header codes), so no bit-level writer is needed.
mod flac {
    /// Samples per frame; also the only block size the header code below encodes.
    const BLOCK_SIZE: usize = 4096;
    /// Bits per sample.
    const BPS: u64 = 16;
    /// Sample rate, in Hz. Must stay 16 kHz to match the `0b0101` header code.
    const RATE: u64 = 16_000;

    /// Encode exactly [`BLOCK_SIZE`] samples as a single-frame 16-bit mono FLAC.
    pub fn encode(samples: &[i16]) -> Vec<u8> {
        assert_eq!(
            samples.len(),
            BLOCK_SIZE,
            "this writer emits one whole FLAC block; pad or trim the signal",
        );
        let mut out = Vec::with_capacity(42 + samples.len() * 2 + 16);
        out.extend_from_slice(b"fLaC");

        // METADATA_BLOCK_HEADER: last-block=1, type=0 (STREAMINFO), length=34.
        out.push(0x80);
        out.extend_from_slice(&34u32.to_be_bytes()[1..]);

        let block = u16::try_from(BLOCK_SIZE).expect("block size fits u16");
        out.extend_from_slice(&block.to_be_bytes()); // min block size
        out.extend_from_slice(&block.to_be_bytes()); // max block size
        out.extend_from_slice(&[0; 3]); // min frame size: 0 = unknown
        out.extend_from_slice(&[0; 3]); // max frame size: 0 = unknown
        // 20 bits rate | 3 bits (channels - 1) | 5 bits (bps - 1) | 36 bits total samples.
        let total = u64::try_from(BLOCK_SIZE).expect("block size fits u64");
        out.extend_from_slice(&((RATE << 44) | ((BPS - 1) << 36) | total).to_be_bytes());
        // The MD5 of the unencoded audio. All-zero is the spec's "not computed"
        // value; writing a real digest would mean pulling in an MD5 crate for no
        // gain, since decoders treat it as advisory.
        out.extend_from_slice(&[0; 16]);

        let frame_start = out.len();
        // Frame header. 14-bit sync `11111111111110`, reserved 0, fixed-blocksize 0.
        out.extend_from_slice(&[0xFF, 0xF8]);
        // Block size code `1100` (4096) | sample rate code `0101` (16 kHz).
        out.push(0xC5);
        // Channel assignment `0000` (1 independent channel) | sample size `100`
        // (16 bit) | reserved 0.
        out.push(0x08);
        // Frame number 0, in FLAC's UTF-8-like coding: values under 128 are one byte.
        out.push(0x00);
        out.push(crc8(&out[frame_start..]));

        // SUBFRAME_HEADER: zero bit, type `000001` (VERBATIM), no wasted bits.
        out.push(0x02);
        for s in samples {
            out.extend_from_slice(&s.to_be_bytes());
        }
        // The bitstream is already byte-aligned here, so no padding bits.
        out.extend_from_slice(&crc16(&out[frame_start..]).to_be_bytes());
        out
    }

    /// FLAC's frame-header CRC: CRC-8 with polynomial `x^8 + x^2 + x + 1`.
    fn crc8(bytes: &[u8]) -> u8 {
        bytes.iter().fold(0u8, |crc, &b| {
            (0..8).fold(crc ^ b, |c, _| {
                if c & 0x80 == 0 {
                    c << 1
                } else {
                    (c << 1) ^ 0x07
                }
            })
        })
    }

    /// FLAC's frame CRC: CRC-16 with polynomial `x^16 + x^15 + x^2 + 1`.
    fn crc16(bytes: &[u8]) -> u16 {
        bytes.iter().fold(0u16, |crc, &b| {
            (0..8).fold(crc ^ (u16::from(b) << 8), |c, _| {
                if c & 0x8000 == 0 {
                    c << 1
                } else {
                    (c << 1) ^ 0x8005
                }
            })
        })
    }
}

/// A writer for *silent* MPEG-1 Layer III streams.
///
/// A real MP3 encoder is a psychoacoustic model plus Huffman coding, and there
/// is no allow-list-clean pure-Rust one to lean on — but silence is a special
/// case that needs neither. A Layer III granule whose `part2_3_length` is zero
/// carries no main data at all, so every spectral coefficient is zero and the
/// granule decodes to silence. Zeroing the side info and the frame body
/// therefore yields a stream that is valid, silent, and trivially deterministic.
///
/// That is the honest limit of this module: it can emit silence and nothing
/// else. A tone or a spoken clip in MP3 would need a real encoder, and the
/// fixture set says so rather than pretending otherwise.
mod mp3 {
    /// Frame length in bytes for MPEG-1 Layer III at 32 kbit/s and 44.1 kHz:
    /// `floor(144 * 32000 / 44100)`, with no padding slot.
    const FRAME_LEN: usize = 104;

    /// `frames` frames of silence — 1152 samples each, so 26.12 ms apiece.
    pub fn silence(frames: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(frames * FRAME_LEN);
        for _ in 0..frames {
            // 0xFF 0xFB: 11-bit sync, MPEG-1 (`11`), Layer III (`01`), no CRC (`1`).
            // 0x10: bitrate index `0001` (32 kbit/s), sample rate `00` (44.1 kHz),
            //       no padding, not private.
            // 0xC0: single channel (`11`), no mode extension, not copyrighted,
            //       not original, no emphasis.
            out.extend_from_slice(&[0xFF, 0xFB, 0x10, 0xC0]);
            // 17 bytes of mono side info followed by the main-data slot, all zero:
            // `part2_3_length == 0` in both granules, so there is nothing to read.
            out.extend_from_slice(&[0; FRAME_LEN - 4]);
        }
        out
    }
}

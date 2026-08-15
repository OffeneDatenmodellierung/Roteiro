# Audio fixtures

Tiny, deterministic audio clips for the ASR ingestion path
(`rto-graph`'s `is_audio` / `audio_content` / `asr_content`, and `rto-llama`'s
`chat_media` with `Modality::Audio`).

## Provenance and licence

**Every file here is synthesised by this repository.** Nothing was downloaded,
sampled or recorded; there is no third-party audio, no unknown provenance and no
external licence to honour. The clips are original works of the Roteiro project
and carry the repository's own terms, `MIT OR Apache-2.0`.

The generator is [`../../audio_fixtures.rs`](../../audio_fixtures.rs). It writes
WAV, FLAC and MP3 by hand — no audio-encoding crate enters the dependency tree,
not even as a dev-dependency — from integer-only signal generators. There is no
floating point anywhere in the chain (the sine is a hard-coded Q15 table, not
`f64::sin`), no clock and no encoder-version string, so the bytes are identical
on every platform and every run. `fixtures_are_byte_reproducible` asserts exactly
that on each test run.

## Regenerating

```sh
ROTEIRO_WRITE_AUDIO_FIXTURES=1 cargo test -p rto-graph --test audio_fixtures
```

The same command with the variable unset only *checks* the files, and names the
one that drifted. Add a fixture by adding a row to `fixtures()` in the generator
and re-running with the variable set.

## The files

| File | Format | Rate | Duration | Bytes | Signal |
| --- | --- | --- | --- | --- | --- |
| `silence-16khz-mono-256ms.wav` | RIFF/WAVE, 16-bit PCM | 16 kHz | 256 ms | 8 236 | digital silence |
| `tone-500hz-16khz-mono-256ms.wav` | RIFF/WAVE, 16-bit PCM | 16 kHz | 256 ms | 8 236 | 500 Hz sine, ≈ −9 dBFS |
| `syllables-16khz-mono-512ms.wav` | RIFF/WAVE, 16-bit PCM | 16 kHz | 512 ms | 16 428 | four voiced bursts (500/1000/2000 Hz stack, trapezoidal envelope) |
| `silence-16khz-mono-256ms.flac` | FLAC, VERBATIM subframes | 16 kHz | 256 ms | 8 243 | digital silence |
| `tone-500hz-16khz-mono-256ms.flac` | FLAC, VERBATIM subframes | 16 kHz | 256 ms | 8 243 | 500 Hz sine, ≈ −9 dBFS |
| `silence-44khz-mono-261ms.mp3` | MPEG-1 Layer III, 32 kbit/s mono | 44.1 kHz | 261 ms | 1 040 | digital silence |

Total: **50 426 bytes** across six files.

Some notes on the choices:

- **500 Hz, not 440 Hz.** At 16 kHz a 500 Hz period is exactly 32 samples, so the
  sine lands on whole samples and a 32-entry integer table generates it exactly —
  no interpolation and no floating point, which is what makes the output
  bit-identical across targets.
- **4096 samples.** One whole FLAC block, so each FLAC is exactly one frame and
  the encoder never has to emit a short final frame.
- **Silence earns its place.** A transcript of silence should be stable and
  empty-ish; a model that "hears" words in it is hallucinating rather than
  transcribing, and the silent clips make that visible.
- **`syllables` is speech-*shaped*, not speech.** It has a syllabic envelope and
  vowel-like harmonic structure so an ASR model is handed something with the
  coarse shape of an utterance. It is not spoken words and does not claim to be —
  synthesising real speech would mean either a TTS binary (not reproducible
  across machines) or a recording (third-party provenance).

## Why MP3 is silence only

WAV is a header plus samples, and FLAC has a VERBATIM subframe that stores raw
samples, so both can carry any signal in about eighty lines of hand-written
encoder. MP3 cannot: a real MPEG-1 Layer III encoder is a psychoacoustic model
plus Huffman coding, and no allow-listed pure-Rust one exists to lean on (the
usual crates wrap LAME, which is LGPL and so off `deny.toml`'s allow-list, and
this machine has no `lame`, `ffmpeg`, `sox` or `flac` binary either).

Silence is the one case that needs none of that: a Layer III granule whose
`part2_3_length` is zero carries no main data, so every spectral coefficient is
zero and the granule decodes to silence. Zeroing the side info and frame body
yields a valid, silent, trivially deterministic stream. A tone or a spoken clip
in MP3 would need a real encoder, so the set does not have one.

## Verified against the decoder that actually runs

llama.cpp's `mtmd` decodes audio with its bundled **miniaudio**
(`tools/mtmd/mtmd-helper.cpp`), gated by `audio_helpers::is_audio_file`, which
requires ≥ 12 bytes and a `RIFF`…`WAVE`, `fLaC`, `ID3` or MPEG-sync-word prefix.
Every fixture was decoded with that exact code — miniaudio compiled from
`llama-cpp-sys-2`'s vendored tree with mtmd's own build flags, driven through
`ma_decoder_init_memory` at the projector's 16 kHz — and all six pass:

```text
silence-16khz-mono-256ms.flac     frames=4096  0.256s  peak=0.0000  rms=0.0000
silence-16khz-mono-256ms.wav      frames=4096  0.256s  peak=0.0000  rms=0.0000
silence-44khz-mono-261ms.mp3      frames=4179  0.261s  peak=0.0000  rms=0.0000
syllables-16khz-mono-512ms.wav    frames=8192  0.512s  peak=0.2421  rms=0.1311
tone-500hz-16khz-mono-256ms.flac  frames=4096  0.256s  peak=0.3510  rms=0.2482
tone-500hz-16khz-mono-256ms.wav   frames=4096  0.256s  peak=0.3510  rms=0.2482
```

The FLAC and WAV pairs decode to identical peak and RMS, which is the check that
the hand-written FLAC encoder is correct and not merely well-formed.

The magic-byte half of that contract is pinned in Rust by
`fixtures_carry_the_magic_bytes_the_projector_sniffs_for`, so a fixture that
would be silently rejected as "not audio" fails in CI rather than inside a
three-gigabyte model run.

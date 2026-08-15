---
Title: Audio metadata as derived facts — symphonia for format reads, and MPL-2.0 in the licence allowlist
Space: ARCH
Parent: ADRs

# ADR-specific metadata (unknown keys are ignored; used for indexing/search)
type: adr
adr-id: "0016"
status: For Review                  # Draft | For Review | Accepted | Rejected | Superseded
architectural-significance: MEDIUM  # SOFT | LOW | MEDIUM | HIGH | VERY HIGH
domain: Knowledge Graph
decision-makers: ["The Roteiro Project Team"]
superseded-by:
version: "1.0"
last-modified: 2026-08-15
confluence-url:
---

# ADR-0016: Audio metadata as derived facts — symphonia for format reads, and MPL-2.0 in the licence allowlist

| | |
|---|---|
| **State** | For Review |
| **Architectural Significance** | MEDIUM |
| **Domain** | Knowledge Graph |
| **Document version** | 1.0 |

## Reference

Adds **deterministic audio metadata** — codec, sample rate, bit depth, channel
layout, duration, tags — to the graph as ordinary `derived` facts, read from the
container without decoding audio and without any model. Adopts
[`symphonia`](https://github.com/pdeljanov/Symphonia) for the format read, and
therefore admits **MPL-2.0** to the `deny.toml` licence allowlist.

This is the deliberate complement to
[[docs/adr/0015-generated-media-content-artifact-store.md]]: 0015 removed
*generated* content (transcripts, descriptions) from `derived` because it is
invented; this ADR adds *extracted* content because it is present in the bytes.
Together they draw the line in both directions rather than only one.

Governed by [[docs/adr/0001-build-roteiro-unified-codebase-knowledge-graph.md]];
extends the ingestion story of
[[docs/adr/0005-image-ocr-vision-ingestion.md]].

## Summary

- Audio blobs gain **real, queryable facts**: codec, sample rate, bit depth,
  channels, duration, frame count, and tags (ID3v1/v2, APE, Vorbis comments, RIFF
  INFO, MP4 atoms).
- These are `derived` — a pure function of `(path, blob id, bytes)` — so they live
  in the graph, unlike the generated content of ADR-0015.
- **No decoding.** A format read instantiates no decoder: measured **1–100 µs** on
  this repo's own fixtures (8 µs FLAC, 17 µs WAV, 103 µs for a 2 MB MP3).
- **`symphonia` with `default-features = false, features = ["flac", "mp3", "wav"]`** —
  17 packages locked, 9 external, no FFI, no C, ~2 s cold build, MSRV 1.85, 100 %
  safe Rust.
- **MPL-2.0 is added to the allowlist** with a recorded rationale. It is the only
  new licence; every transitive dependency already qualifies.
- **An estimated value may never be presented as an exact one** — MP3 duration is
  sometimes inferred, and must be marked.
- **`EXTRACT_VERSION` bumps once**: extraction output genuinely changes.

## Context

Today an audio blob is close to opaque in the graph: a `file` node with a path and
a size. Everything Roteiro knew about its *content* came from an ASR model — and
ADR-0015 correctly moved that out of `derived`, because a transcript is generated
rather than extracted.

That left an asymmetry worth fixing. A `.wav` file genuinely contains facts:
it *is* 16 kHz, it *is* mono, it *is* 256 ms long, and those statements are exactly
as deterministic as "this Rust file declares `fn extract`". They satisfy the
`derived` contract in full — same bytes, same answer, no clock, no sampling, no
model — and the graph currently records none of them.

Reading them needs a container parser. Three routes were assessed:

1. **Reuse llama.cpp's bundled miniaudio** — *impossible.* It is compiled
   `#define MA_API static`, so every symbol has internal linkage; `nm` over the
   built `libmtmd.a` finds no `ma_decoder` symbols at all, `llama-cpp-sys-2`'s
   bindgen allowlist covers only `ggml_*`/`gguf_*`/`llama_*`/`mtmd_*`, and the one
   public entry point requires a live projector context — the very multi-gigabyte
   load we are trying to avoid.
2. **Hand-roll the parsers** — viable for *silence detection* (see below), but for
   metadata it means MP3 frame-header walking plus Xing/VBRI, FLAC STREAMINFO,
   RIFF chunk walking, and then ID3v2 and Vorbis comment parsing. Tag parsers in
   particular are where hand-written code quietly goes wrong.
3. **Take a decoding library for its format layer** — `symphonia` is the only
   maintained candidate; it was previously excluded not on merit but because
   MPL-2.0 is absent from the allowlist.

## Decision makers

The Roteiro Project Team.

## Recommended option

### What becomes a `derived` fact

Per audio blob, from a format read only:

- **Stream shape**: codec id, sample rate, bit depth, channel count and layout,
  sample format, frame count.
- **Duration**, with its exactness marked (below).
- **Tags**, where present: ID3v1/ID3v2/APE, Vorbis comments (FLAC, OGG), RIFF INFO,
  MP4 atoms. `symphonia` normalises these into a standard enum, which spares us a
  tag-name mapping table of our own.

These are ordinary graph facts: cached by blob id like every other extraction,
sorted deterministically, and carrying no clock or environment input beyond the
media env tag already folded into the cache key.

### Exact vs estimated — the one subtlety

**MP3 duration is not always exact.** It comes from a Xing/VBRI header when one is
present, and is otherwise inferred from bitrate — and only when the source is
seekable. On a degenerate 1 040-byte fixture the reader returns nothing at all.

That is still *extraction* (deterministic given the bytes, no generation), so it
stays `derived`. But a `derived` fact that is deterministic yet **inexact** is new
here, and must not be laundered into a precise-looking number:

- record duration with an explicit exactness marker (`exact` | `estimated`);
- when the reader gives nothing, record **nothing** — absence, not a guess;
- every surface that displays a duration must show the estimate as an estimate.

Asserting an approximate number under the graph's strongest provenance label
would repeat, in miniature, the mistake ADR-0015 exists to correct.

### Licence: MPL-2.0 admitted, deliberately

All seven `symphonia-*` crates are MPL-2.0; every transitive dependency
(`lazy_static`, `log`, `bitflags`, `bytemuck`, `smallvec`, `num-complex`,
`num-traits`, `regex-lite`, `extended`) already falls inside the existing
allowlist. So MPL-2.0 is the sole addition.

MPL-2.0 is **file-level weak copyleft**: it obliges publication of modifications to
*symphonia's own files*, plus notice and source availability when binaries are
shipped. It does not reach Roteiro's own source, and leaves the project's MIT/
Apache-2.0 dual licence untouched. Using the crate unmodified — the expected case —
reduces the obligation to keeping the notice.

The allowlist entry must carry a comment saying this, so a future reader sees a
decision rather than an oversight.

### Scope — deliberately narrow

**In:** the format read and the facts above.

**Out, with reasons:**

- **Decoding audio for ASR.** `symphonia` has no resampler and no channel mixer
  (`dsp/` is FFT and MDCT only). Voxtral wants 16 kHz mono, so replacing llama.cpp's
  decode path would additionally need `rubato` plus a downmix. Possible later; not
  bought here.
- **Widening `is_audio` beyond wav/mp3/flac.** `is_audio` is currently pinned to
  what llama.cpp can decode; widening it without changing the decode path yields
  files that can be indexed but not transcribed. Revisit only if segmented decode
  lands. Note that **`symphonia` does not support Opus** — the README advertises a
  feature flag, but 0.6.1 ships none.
- **Cross-container duplicate detection.** `duplicates` matches `file` nodes on
  identical git blob hash (plus semantics on nodes with `meta.content`), so a `.wav`
  and a `.flac` of the same signal would never pair without a decoded-PCM
  fingerprint — a new comparison axis for a case that does not occur in code
  repositories.
- **Silence detection.** Metadata alone cannot tell you a file is digitally silent.
  Gating MP3/FLAC remains the structural-parsing option assessed separately; this
  ADR neither delivers nor blocks it.

## Options considered + consequences

| Option | Verdict |
|---|---|
| Reuse llama.cpp's miniaudio | **Impossible** — static linkage, no exported symbols, public path needs a projector context. |
| Hand-roll metadata parsers | **Rejected** — tag parsing is disproportionate risk for the value. |
| `claxon` + `puremp3` | **Rejected** — clear the licence gate but are abandoned (2020 and a 2019 `0.1.0`). |
| `symphonia`, decode features off | **Chosen** — small, modular, safe-Rust, and the only maintained option. |
| Do nothing | Leaves the graph blind to facts it could hold for microseconds of work. |

## Consequences

**Positive**

- Audio blobs become queryable without a model: duration, codec, rate, channels,
  tags are all `search`-able and available offline.
- `media status` can estimate work honestly ("42 minutes of audio to process")
  because duration is now known before any model runs.
- ADR-0015's boundary is now drawn in both directions — generated content out,
  extracted content in — which makes the rule easier to apply than to memorise.
- A future segmented-decode path for long audio becomes possible without a further
  dependency decision.

**Negative / costs**

- One new licence class (MPL-2.0) in the allowlist, and nine new crates in the tree
  under the audio feature.
- **Upstream bus factor is 1**: 81 of the last 100 commits are from one maintainer,
  36 PRs are open, and the project was dormant for 20 months before its 2026
  revival. Pin the version and track advisories deliberately.
- The `exact` vs `estimated` distinction is new vocabulary that every consumer of
  duration must respect.
- One `EXTRACT_VERSION` bump: every user re-extracts once.

## Status

For Review. Implemented in the same PR that carries this ADR, per the decision to
land the rationale and the code together.

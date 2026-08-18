---
Title: Audio metadata as derived facts — symphonia for format reads, and MPL-2.0 in the licence allowlist
Space: ARCH
Parent: ADRs

# ADR-specific metadata (unknown keys are ignored; used for indexing/search)
type: adr
adr-id: "0016"
status: Accepted                    # Draft | For Review | Accepted | Rejected | Superseded
architectural-significance: MEDIUM  # SOFT | LOW | MEDIUM | HIGH | VERY HIGH
domain: Knowledge Graph
decision-makers: ["The Roteiro Project Team"]
superseded-by:
version: "1.2"
last-modified: 2026-08-15
confluence-url:
---

# ADR-0016: Audio metadata as derived facts — symphonia for format reads, and MPL-2.0 in the licence allowlist

| | |
|---|---|
| **State** | Accepted |
| **Architectural Significance** | MEDIUM |
| **Domain** | Knowledge Graph |
| **Document version** | 1.2 |

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
  channels, duration, frame count, and tags (ID3v1/v2, APE, Vorbis comments —
  see *Tag formats actually reached* for the two that are not).
- These are `derived` — a pure function of `(path, blob id, bytes)` — so they live
  in the graph, unlike the generated content of ADR-0015.
- **No decoding.** A format read instantiates no decoder: measured **1–100 µs** on
  this repo's own fixtures (8 µs FLAC, 17 µs WAV, 103 µs for a 2 MB MP3).
- **`symphonia` with `default-features = false, features = ["flac", "mp3", "wav",
  "id3v1", "id3v2", "ape"]`** — 16 packages locked (7 `symphonia-*` plus 9
  external), no FFI, no C, ~2 s cold build, MSRV 1.85, 100 % safe Rust. The three
  tag flags were added at implementation time and add **no package**; see *Tag
  formats actually reached*.
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
- **Tags**, where present. `symphonia` normalises these into a standard enum,
  which spares us a tag-name mapping table of our own — see *Tag formats actually
  reached* for which formats arrive.

These are ordinary graph facts: cached by blob id like every other extraction,
sorted deterministically, and carrying no clock or environment input beyond the
media env tag already folded into the cache key.

### Tag formats actually reached (v1.1)

Two corrections from implementing this, both recorded rather than quietly
absorbed. Version 1.0 listed the tag formats aspirationally; this is what the
chosen dependency actually delivers.

| Format | Reached? | Why |
|---|---|---|
| ID3v1 / ID3v2 (MP3) | **yes**, with `id3v1`/`id3v2` | not implied by `mp3` |
| APE (MP3) | **yes**, with `ape` | not implied by `mp3` |
| Vorbis comments (FLAC) | yes | embedded in the container; arrives with `flac` |
| RIFF INFO (WAV) | **no** — upstream bug | parsed, then discarded (below) |
| Vorbis comments (OGG) | no | `ogg` is not enabled; `.ogg` is not `is_audio` |
| MP4 atoms | no | `isomp4` is not enabled; `.m4a` is not `is_audio` |

**ID3v1/ID3v2/APE need their own feature flags.** They are *standalone* metadata
readers in `symphonia-metadata`, registered on the probe separately from any
container. With `features = ["flac", "mp3", "wav"]` alone,
`register_enabled_formats` registers **no metadata reader at all**, and every tag
on an MP3 — the format most likely to carry one — is silently invisible. The three
flags are therefore added. They pull **no additional package**: all three gate
code inside `symphonia-metadata`, which `flac` and `wav` already require.

**WAV RIFF INFO is lost inside symphonia 0.6.1.** `WavReader::try_new` parses a
`LIST`/`INFO` chunk into a local `metadata` binding, then constructs itself from
`opts.external_data.metadata.unwrap_or_default()` instead — so the parsed revision
is dropped. (`symphonia-bundle-flac` does the corresponding thing correctly, which
is why FLAC's Vorbis comments do arrive.) Recovering them would mean re-parsing the
RIFF chunk list ourselves, i.e. the hand-rolling this ADR declined; so the
limitation is accepted, and pinned by a test that **fails when a future symphonia
fixes it** — at which point this table should be revised. WAV *stream* facts are
unaffected: rate, depth, channels and an exact duration all read correctly.

The two `no`s at the bottom of the table are consequences of the deliberately
narrow scope below, not new decisions: neither `.ogg` nor `.m4a` is an extension
`is_audio` admits, so no blob in scope can carry either.

### Exact vs estimated — the one subtlety

**MP3 duration is not always exact.** It comes from a Xing/VBRI header when one is
present, and is otherwise inferred from bitrate — and only when the source is
seekable. On the degenerate 1 040-byte fixture the reader states no frame count at
all, so there is no duration to record.

That is still *extraction* (deterministic given the bytes, no generation), so it
stays `derived`. But a `derived` fact that is deterministic yet **inexact** is new
here, and must not be laundered into a precise-looking number:

- record duration with an explicit exactness marker (`exact` | `estimated`);
- when the reader gives nothing, record **nothing** — absence, not a guess;
- every surface that displays a duration must show the estimate as an estimate.

Asserting an approximate number under the graph's strongest provenance label
would repeat, in miniature, the mistake ADR-0015 exists to correct.

**Every MPEG-audio duration is marked `estimated` (v1.1)** — including one whose
frame count came from a Xing header. `MpaReader` reaches a frame count by three
routes (Xing, VBRI, or an inference from bitrate and stream length) and **exposes
no flag saying which**; distinguishing them would mean parsing the first MPEG frame
for a Xing/Info/VBRI header ourselves, which is the hand-rolling this ADR rejected.
A Xing count is in any case the *encoder's claim* about the stream rather than a
property of it, unlike a WAV `data` chunk length or FLAC's `STREAMINFO`
total-samples field, which are the two cases marked `exact`. The only error
possible here is calling an estimate exact, and this construction cannot make it.

**The absence is per fact, not per blob (v1.1).** The degenerate MP3 still yields
codec, sample rate and channel count — what its frame headers state outright — and
simply carries no `duration` key. A blob the reader rejects outright yields no node
at all. Both are tested.

### Where the facts live (v1.1)

Each audio blob's facts become **their own node** — kind `audio_stream`, key
`audio:<path>`, hung off the `file` node by a `derived` `contains` edge — rather
than extra keys on the `file` node's `meta`. This is the shape `config_key`
(ADR-0009) and `image_ref` already use, and it is chosen for three reasons:

- **It leaves ADR-0015's empty slot empty.** `meta.content` on an audio *file* node
  is where a transcript used to live. Reoccupying it, even with extracted text,
  would make "does this audio file node carry content?" ambiguous again — the very
  question ADR-0015 exists to keep answerable. The file node is untouched.
- **Searchability comes free, with no new ranking rule.** `search` matches on a
  node's name, key, path and `meta.content`; a node whose `meta.content` is a
  rendered summary (`flac, 16000 Hz, mono, 16-bit, 4096 frames, 256 ms (exact)`,
  then one line per tag) is found by the *ordinary* scorer, through no new branch.
  Being `derived`, it takes none of the `authored` boost curated intent gets.
- **One node, one thing the container said.** A fact set stays legible, and a blob
  the reader declines contributes no node rather than a node full of nulls.

`AudioDuration` is a struct (`{ms, exactness}`), not a bare number beside a
qualifier — the same "sum type, not a nullable column" discipline `MediaOutcome`
uses. No consumer can reach the milliseconds without the marker, which turns "every
surface shows an estimate as an estimate" into a property of the type rather than a
standing review obligation.

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

- One new licence class (MPL-2.0) in the allowlist, and fourteen new crates in the
  tree under the audio feature (7 `symphonia-*` plus 7 externals not already
  present; `bitflags` and `smallvec` were already there via `gix`/tree-sitter).
- **Upstream bus factor is 1**: 81 of the last 100 commits are from one maintainer,
  36 PRs are open, and the project was dormant for 20 months before its 2026
  revival. Pin the version and track advisories deliberately.
- The `exact` vs `estimated` distinction is new vocabulary that every consumer of
  duration must respect.
- One `EXTRACT_VERSION` bump: every user re-extracts once. (10 → 11, plus a `+400`
  namespace so a feature build and a default build never serve each other stale
  facts from a shared cache. Because the feature is off by default, a default build
  re-extracts to exactly the facts it already had — the bump is honesty about the
  version, not a change in its output.)
- WAV tags are unavailable until symphonia fixes its RIFF INFO handling — see *Tag
  formats actually reached*.

## Status

**Accepted** (2026-08-17), and implemented — Stage 29 (#316), released in **v1.11.0**. Implemented in the same PR that carries this ADR, per the decision to
land the rationale and the code together.

## Document version history

| Version | Date | Notes |
|---------|------|-------|
| 1.0 | 2026-08-15 | Audio metadata as `derived` facts via a `symphonia` format read; MPL-2.0 admitted to the `deny.toml` allow-list with a recorded rationale; duration carries an `exact`/`estimated` marker and absence is recorded as absence; scope narrowed to exclude ASR decoding, widening `is_audio`, cross-container duplicates and silence detection. |
| 1.1 | 2026-08-15 | Implementation corrections, no decision changed. **Feature list**: `id3v1`/`id3v2`/`ape` added — they are standalone metadata readers not implied by `mp3`, so without them the probe registers no metadata reader and every MP3 tag is invisible; they add no package. **RIFF INFO**: recorded as unreachable in symphonia 0.6.1 (the WAV reader parses the `LIST`/`INFO` chunk and then discards it), pinned by a test that fails when upstream fixes it. **Exactness**: every MPEG-audio duration is `estimated`, Xing-backed ones included, because the reader exposes no flag saying which route it took. **Absence**: clarified as per-fact — the degenerate MP3 still yields codec, rate and channels. **Placement**: facts live on an `audio_stream` node keyed `audio:<path>` under a `contains` edge, not on the `file` node's `meta`, so ADR-0015's emptied `meta.content` slot stays empty. Package count corrected (16 locked, not 17). |
| 1.2 | 2026-08-17 | **Accepted.** No content changed. Status corrected: this ADR described shipped, released behaviour while still reading *For Review*. |

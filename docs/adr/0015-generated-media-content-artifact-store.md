---
Title: Generated media content — its own artifact store, rebuildable on demand
Space: ARCH
Parent: ADRs

# ADR-specific metadata (unknown keys are ignored; used for indexing/search)
type: adr
adr-id: "0015"
status: Accepted                    # Draft | For Review | Accepted | Rejected | Superseded
architectural-significance: HIGH    # SOFT | LOW | MEDIUM | HIGH | VERY HIGH
domain: Knowledge Graph
decision-makers: ["The Roteiro Project Team"]
superseded-by:
version: "1.2"
last-modified: 2026-08-15
confluence-url:
---

# ADR-0015: Generated media content — its own artifact store, rebuildable on demand

| | |
|---|---|
| **State** | Accepted |
| **Architectural Significance** | HIGH |
| **Domain** | Knowledge Graph |
| **Document version** | 1.2 |

## Reference

Decides where **generatively produced media content** lives: audio transcripts
(Voxtral) and image descriptions (SmolVLM). Such content is **not** a derived fact
and must not be stored as one. It gets its own artifact store, exactly as analyzer
findings did in [[docs/adr/0012-analyzer-findings-artifact-model.md]] and agent
memory did in [[docs/adr/0013-agent-memory-artifact-store.md]] — and, because the
content is genuinely useful, it comes with **first-class CLI and UI paths to build
it, rebuild it and search it**.

Amends [[docs/adr/0005-image-ocr-vision-ingestion.md]], whose intent this enforces:
*"OCR text is `derived`-ish content fed to inference; a VLM description is clearly a
**suggestion**. Neither should masquerade as authored fact."* That was stated and
never enforced. Governed by
[[docs/adr/0001-build-roteiro-unified-codebase-knowledge-graph.md]].

Resolves issue #300.

## Summary

- **Generative output stops being `derived`.** ASR transcripts and VLM descriptions
  move out of `nodes.meta.content` into a separate `media_content` artifact store.
- **OCR and PDF text stay `derived`** — see the boundary below. This ADR is about
  *generation*, not about *models*.
- **Every record carries its production evidence**: model id, quantisation, mmproj
  digest, prompt, sampling parameters, and the source blob id.
- **Utility is preserved deliberately, not accidentally.** `roteiro media build`
  produces it, `media status` reports it, and search/context can include it behind
  an explicit opt-in that always labels it as generated.
- **A deterministic pre-generation gate** refuses obviously-empty inputs (silence,
  blank images) *before* a model is invoked — adopted as a complement, not as the
  fix. Its refusals are **recorded with the value they measured**, so a skip is
  legible rather than an indistinguishable hole.
- The graph keeps rebuilding identically from source; `export_factset` stays a pure
  function of the tree.
- **`EXTRACT_VERSION` bumps once** — this genuinely changes extraction output.

## Context

Issue #300 was found concretely, not theoretically: a fixture of **digital silence**
was transcribed by Voxtral into ~2 KB of fluent prose — a lecture on world
government — persisted as the `meta.content` of a node whose provenance is
`derived`, and subsequently ranked in `roteiro search`. Worse, the test written to
prevent exactly that was **green while it happened** (fixed in #299), so both the
failure and its guard were broken at once.

`AGENTS.md` is unambiguous: `derived` extraction is *a deterministic pure function
of `(path, git blob id, bytes)`* and *"don't fabricate authored or derived facts"*.
Generative output satisfies neither clause. Re-run with a different model,
quantisation or sampling seed and the same blob yields different "facts" — which
also silently violates the assumption behind the content-addressed cache and
`EXTRACT_VERSION`.

### The boundary: generation, not models

The line is **not** "produced by a model". It is whether the artifact being read
*exists in the bytes*:

| Content | Nature | Verdict |
|---|---|---|
| Prose, PDF text | deterministic parse | stays `derived` |
| **OCR** (`ocrs-text`: detection + recognition) | discriminative; decodes text that is *actually present*; errors are **misreadings**, correctable against the image | **stays `derived`** |
| **ASR transcript** (Voxtral) | generative; invents fluent text when no speech is present | **moves out** |
| **VLM description** (SmolVLM) | generative; invents a description of a blank or decorative image | **moves out** |

OCR has ground truth in the artefact; a human can point at the pixels and say the
model got it wrong. A transcript of silence has no ground truth to be wrong
*against* — it is confabulation, and no amount of model improvement changes its
kind. That is the distinction worth encoding, and it keeps OCR's real utility on the
cheap path.

*(OCR's determinism does depend on pinned model files and a pinned runtime; the
media env tag already participates in the extraction cache key. If OCR ever gains
sampling, it crosses the line and moves too.)*

### No migration is required

Generated media content is **not yet relied on by any consumer** — the capability
exists but nothing depends on the transcripts or descriptions currently sitting in
graphs. So this is a clean cutover, not a data migration:

- `EXTRACT_VERSION` bumps once; every user re-extracts, and the generated text
  simply stops being written into `nodes.meta.content`;
- nothing is copied into the new store as part of the upgrade — records are
  produced on demand by `roteiro media build`;
- no back-compat shim, no dual-read period, no deprecation window.

Had this shipped a year later, that would not have been true. Doing it now is
substantially cheaper than doing it after something depends on it.

## Decision makers

The Roteiro Project Team.

## Recommended option

### Its own store

A `media_content` artifact store, sibling to the analyzer-findings and
agent-memory stores, keyed by **source blob id + producer identity**. Each record
carries the evidence needed to reproduce or distrust it:

- source blob id and repository path;
- model id and file digest, quantisation, mmproj digest;
- prompt and sampling parameters (temperature, seed where available);
- produced-at timestamp, generation counter, and the tool version;
- the generated text itself, plus a confidence signal where the runtime exposes one.

Keying on `(blob, producer)` means the same audio re-described by a better model is
a **new record, not a mutation** — you can compare, and you can discard a producer's
output wholesale when you stop trusting it.

Records survive `rebuild`, following the `imports` precedent, because they are
expensive to reproduce (a 715 MB projector load per blob — see #301) and are not
derivable from source alone.

### Building and rebuilding — the part that must be easy

Moving content out of the graph must not make it hard to get back. This is a
deliberate requirement of the decision, not an afterthought:

- **`roteiro media build [--audio] [--vision] [--blob <id>] [--force]`** — generate
  content for media blobs that lack a record for the current producer. Incremental
  by default: only blobs with no record, or whose producer identity changed, are
  processed. `--blob` narrows to one source blob, which is the per-blob rebuild the
  explorer's action hands over.
- **`roteiro media status [--json]`** — what exists, produced by which model, when,
  how much is stale relative to the currently-installed models, and what a rebuild
  would cost.
- **`roteiro media clear [--producer <id>]`** — discard records, wholly or per
  producer, so a distrusted model's output can be dropped without touching the graph.
- **UI**: the explorer surfaces generated content on a media node, always visibly
  labelled with its producer, with a rebuild action for that blob. The action
  hands over the exact per-blob command (`roteiro media build --blob <id>
  --force`) rather than running it: the explorer is a read-only view over graphs
  a server already holds, and a rebuild means a multi-gigabyte model load, which
  does not belong behind an HTTP handler in a build that is deliberately
  llama-free ([[docs/adr/0010-explorer-web-app-vendored-js.md]]).
- **Search/context**: `roteiro search --include-generated` (and the equivalent
  config toggle) folds generated text into results. It is **off by default**, and
  when on, every hit is visibly marked as generated and ranked in its own channel —
  never with the `authored` boost, never indistinguishable from extracted text.

The existing `[ingest] audio` / `vision` toggles keep their meaning as *"may this
run generate content at all"*, so an operator can disable generation outright.

### The pre-generation gate (adopted)

Before any model is invoked, a **cheap, deterministic** check refuses inputs that
obviously contain nothing to read:

- **Audio** — peak/RMS amplitude below a threshold across the clip, i.e. digital or
  near-digital silence.
- **Images** — near-uniform content (pixel variance/entropy below a threshold), i.e.
  blank or flat-colour images.

Three properties make it worth having:

1. **It runs before the model loads**, so a repo full of silent or blank assets
   avoids the projector load entirely (see #301) — a correctness improvement that is
   also the cheapest performance win available here.
2. **The refusal is recorded, not silent.** A skipped blob gets a `media_content`
   record stating *why* it was skipped and the measured value, so `media status` can
   show "skipped: below silence threshold (rms=0.0)" rather than leaving an
   indistinguishable hole. An operator can tell "not generated" from "generated
   nothing".
3. **It is deterministic**, so it belongs on the extraction side of the line and
   costs nothing in reproducibility.

Thresholds are configurable with **conservative defaults**: the gate exists to catch
the unambiguous cases, and a false skip is worse than a false pass now that
generated output is clearly labelled anyway. `--force` overrides it.

**What it explicitly does not do:** a model asked to transcribe *quiet speech*, a
noisy room tone, or a subtly-textured image will still confabulate. The gate raises
the floor; only the artifact store fixes the label. It is adopted because it is
cheap and helps, not because it addresses the defect in #300.

#### As built

| | Audio | Images |
|---|---|---|
| Measured | RMS amplitude over the whole clip, full scale `1.0` | variance of the luma plane, a pixel being `0.0`…`1.0` |
| Default threshold | `silence_rms = 0.0001` (≈ **-80 dBFS**) | `image_variance = 0.00001` (σ ≈ **0.8 levels of 255**) |
| Config key | `[media] silence_rms` | `[media] image_variance` |
| Readable formats | **WAV only** | PNG and JPEG |

`[media] gate = false` turns it off wholesale; `roteiro media build --force`
overrides it for one run. Both defaults sit at the digital noise floor: 16-bit
dither is `≈0.00003` of full scale and is still refused, while a hissy room tone
at -60 dBFS (`0.001`) passes with an order of magnitude to spare. That asymmetry
is deliberate — a false skip is a silently missing description, which is worse
than a false pass now that generated output is labelled.

Three limits are worth stating plainly, because none of them is visible from the
threshold table:

1. **MP3 and FLAC are not measured.** They are entropy-coded, and decoding them
   means the audio decoder bundled inside llama.cpp — behind the very model load
   the gate exists to avoid. The gate therefore **abstains** on those formats and
   they reach the model exactly as before. Abstention is always a pass, never a
   skip. Adding a decoder would be a new dependency for a check whose whole
   justification is that it is nearly free.
2. **Images are measured only in a build with an image codec** (`image-ocr` or
   `image-vision`) — which is exactly the build that could generate a description
   in the first place, so nothing is lost.
3. **Everything above the floor still confabulates.** Quiet speech, room tone,
   tape hiss, a faint gradient, a watermark, a scan of a blank page: all pass, and
   a model handed any of them still returns confident invented prose. The gate
   removes the unambiguous cases from the model's path. It does not make what the
   model says about the rest any more true, and nothing here should be read as
   claiming it does.

The saving is real and is the reason the gate sits where it does: the llama.cpp
engines are built lazily *inside* the producer's `generate`, and `build_media`
evaluates the gate **before** calling it — so a repository of silent or blank
assets completes a `media build` without loading a model at all.

### What this fixes for the operator

Today's failure is silent: fabricated prose enters the graph, ranks in search, and
nothing marks it. After this change the same content is still available and still
searchable — but it is opt-in, attributed to a named model, and cannot be mistaken
for something extracted from the source.

## Options considered + consequences

| Option | Verdict |
|---|---|
| Leave as `derived` | **Rejected** — the label is factually false and breaks the project's headline promise. |
| Move to `inferred` with a confidence | **Rejected, narrowly** — `inferred` today means *similarity edges between existing nodes*, not *content attached to a node*; widening it dilutes a second vocabulary, and ASR/VLM do not naturally emit a calibrated confidence. |
| Silence/blank gate only | **Rejected as *the* fix, adopted as a complement (see below)** — it reduces garbage in, but a model confabulating on *quiet speech* still fabricates; it narrows the failure without correcting the label. |
| Mark and demote in place | **Rejected** — keeps the false label and relies on every consumer honouring the mark. |
| **Separate artifact store + explicit rebuild/search paths (chosen)** | Keeps the graph a pure function of source, keeps the content available, and makes its provenance legible. |

## Consequences

**Positive**

- `derived` regains its precise meaning; the graph rebuilds identically and
  `export_factset` stays byte-identical for a tree.
- Generated content becomes **attributable** — you can see which model said it, and
  drop a producer you no longer trust.
- Re-describing with a better model is a comparison, not a silent overwrite.
- Non-speech audio and blank images can no longer inject confident prose into search
  results as fact.

**Negative / costs**

- **A capability regresses unless the new paths are built**: searching audio content
  currently "just works". This ADR only holds if `media build` / `--include-generated`
  ship *with* the move, not after it.
- One `EXTRACT_VERSION` bump: extraction output genuinely changes, so every user
  re-extracts once. Batch it with any other extraction-affecting change.
- The gate adds two tunable thresholds — a small ongoing calibration surface, and a
  false skip is a silently missing description (mitigated by recording the skip and
  its measured value).
- A third artifact store to maintain, and a third retrieval surface to keep coherent
  with the other two.

## Status

**Accepted** (2026-08-17), and implemented — Stage 28a/28b (#310, #312), released from **v1.10.x**. Sequenced in [BUILD_PLAN_V2](../BUILD_PLAN_V2.md) as Stage 28, and
implemented across two changes: the artifact store, its CLI and the search
channel (Stage 28a), then the pre-generation gate and the explorer surfacing
(Stage 28b). The projector cache (#301) is complementary and tracked separately;
the gate compounds with it, since a refused blob loads no projector to cache.

## Version history

| Version | Date | Notes |
|---|---|---|
| 1.2 | 2026-08-17 | **Accepted.** No content changed. Status corrected: this ADR described shipped, released behaviour while still reading *For Review*. |

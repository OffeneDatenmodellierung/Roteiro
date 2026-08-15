---
Title: Image ingestion — tiered OCR (pure-Rust) + optional vision understanding
Space: ARCH
Parent: ADRs

# ADR-specific metadata (unknown keys are ignored; used for indexing/search)
type: adr
adr-id: "0005"
status: Accepted                    # Draft | For Review | Accepted | Rejected | Superseded
architectural-significance: MEDIUM  # SOFT | LOW | MEDIUM | HIGH | VERY HIGH
domain: Developer Tooling
decision-makers: ["The Roteiro Project Team"]
superseded-by:
version: "1.2"
last-modified: 2026-08-15
confluence-url:
---

# ADR-0005: Image ingestion — tiered OCR (pure-Rust) + optional vision understanding

| | |
|---|---|
| **State** | Accepted |
| **Architectural Significance** | MEDIUM |
| **Domain** | Developer Tooling |
| **Document version** | 1.2 |

## Reference

Governs the **image ingestion** item of Stage 12 (see `docs/BUILD_PLAN.md`) — the last remaining piece of "make `inferred` edges meaningful by embedding *real content*". It extends the content-ingestion pattern already shipped for prose and PDF text (their bodies land in `meta.content` and are embedded), and reuses the tiered, offline-first, consent-gated model machinery decided in [[docs/adr/0003-pluggable-embedding-models.md]]. It answers the item ADR-0001 and the Stage 12 plan flagged as "the one genuinely uncertain item in the backlog." See [[docs/adr/0001-build-roteiro-unified-codebase-knowledge-graph.md]].

## Summary

Ingest text and meaning from **images** (screenshots, scanned docs, diagrams) into `meta.content`, so inference relates an image to code by *content* — exactly as prose and PDF already do. Structure it in **two tiers by purpose**, not one spliced pipeline:

- **Tier A — OCR text extraction (default image tier): pure-Rust `ocrs` + `rten`.** A detection→recognition OCR engine running on `rten`, a **pure-Rust** model runtime (no ONNX Runtime, no tesseract, no C/C++ FFI). Extracts literal text from an image into `meta.content`. This is the common case and keeps the pure-Rust stance (Principle 3, ADR-0001).
- **Tier B — image *understanding* (opt-in): a small document VLM via `candle`.** For images where literal OCR is insufficient (architecture diagrams, charts), a vision-language model — run on the **`candle`** stack we already vendor — produces a semantic description. Weights flow through ADR-0003's registry / consent-gated `pull`, exactly like the embedding and generative model tiers.

Both are **feature-gated and opt-in**; the default and `inference` builds pull neither. This mirrors the tiering Roteiro already uses for inference (hashing → local GGUF → larger → agent) and authoring (ADR-0004).

We **reject all three surveyed OCR crates** (`rusto-rs`, `yingkitw/ocr`, `oar-ocr`) and we **explicitly reject splicing** Tier B's model into Tier A's pipeline (using `candle`-TrOCR as the recognizer behind `rten` detection).

## Context

Stage 12 already embeds *real content* for prose and PDF (`meta.content` → `node_text` → embedding). Images are the last modality. The Stage 12 plan called this out as needing "its own decision (like candle)": pure-Rust OCR is historically weak, while accurate OCR usually means a **C/C++ inference engine** (tesseract, ONNX Runtime, MNN) — against the pure-Rust stance — or a heavy vision model with large weights.

Forces to reconcile (from ADR-0001 and the project's stance):

1. **Pure-Rust, no C/C++ FFI (strong preference).** `unsafe_code = "forbid"` in our crates; the build must stay `cargo`-only with no system libraries to install. A bundled ONNX Runtime / tesseract binary is the thing to avoid.
2. **Offline-by-default & lean binary.** Any model is opt-in and local, pulled through the ADR-0003 consent gate; the default `roteiro` binary must not grow, and must never make a network call implicitly.
3. **`cargo deny` licence gate.** Every transitive dependency must satisfy the allow-list (MIT, Apache-2.0, BSD-2/3, Unicode-3.0, Zlib, ISC).
4. **MSRV 1.94.** Any new dependency tree must build on 1.94.
5. **Correctness over fluency.** OCR text is `derived`-ish content fed to inference; a VLM description is clearly a *suggestion*. Neither should masquerade as authored fact.

Three community Rust OCR projects were surveyed as candidates (the user's shortlist):

- **`rusto-rs`** — uses the **MNN** C++ inference engine + PaddleOCR models converted to MNN. Accurate, but a native C++ engine and an offline model-conversion step. Immature (v0.1.2).
- **`yingkitw/ocr`** — the only pure-Rust one (`ndarray`), Apache-2.0, bundled models. But very immature (1★) and its accurate CRNN path is explicitly **unvalidated** ("blocked on hardware"); the default is pattern-matching for clean printed text only. Pulls `tokio`.
- **`oar-ocr`** — the most mature (142★, crates.io), but built on **ONNX Runtime** (`ort`, a bundled C++ binary) for its classic pipeline, with a `candle` VLM path alongside. The ONNX Runtime native dependency is the disqualifier.

## Decision makers

- The Roteiro Project Team

## Recommended option

**Option 4 — two-tier, pure-Rust-default image ingestion (recommended).**

- **Tier A — `ocrs` + `rten`** (feature `image-ocr`, off by default): detect + recognise text, cap/whitespace-collapse into `meta.content`, embedded like prose/PDF. Panic-guarded and size-capped like the PDF path (ADR-adjacent to the `pdf-text` feature). Models are `.rten` files pulled through the ADR-0003 registry/consent gate into the model store, **not** ocrs's default `~/.cache` auto-fetch — so the offline + consent invariants hold.
- **Tier B — `candle` document VLM** (feature `image-vision`, off by default, implies `inference-local-models`): for richer understanding, produce a description into `meta.content`. Reuses ADR-0003's registry / platform-variant / consent `pull` / `candle` backend — adding a *vision* model kind alongside the embedding and generative kinds.
- **`image` dependency is pinned to minimal codecs** — `default-features = false, features = ["png", "jpeg"]` (± `webp`) — see Consequences for why this is load-bearing.

### Go/no-go de-risk (completed before this ADR was accepted)

A throwaway spike gated Tier A against the invariants (`ocrs 0.12.2`, `rten 0.25.0`, `image 0.25.10`):

| Gate | Result |
|---|---|
| **MSRV 1.94 build** | ✅ compiles clean |
| **Pure-Rust (no FFI)** | ✅ no ONNX Runtime / tesseract / `*-sys` in the tree |
| **`cargo deny` advisories** | ✅ |
| **`cargo deny` licences** | ✅ **with minimal `image` codecs** (see below) |
| **Tree size** | **73 crates** (≈ the `pdf-text` feature's 71), opt-in |

## Options considered + consequences

### Option 1: `rusto-rs` (MNN engine)
- Pros: accurate (PaddleOCR); multi-platform.
- Cons: **native C++ engine (MNN)** + an offline ONNX→MNN model-conversion step; immature (v0.1.2, 11★). Violates the pure-Rust/no-FFI stance. **Rejected.**

### Option 2: `yingkitw/ocr` (pure-Rust)
- Pros: pure-Rust, Apache-2.0, bundled models — no native deps.
- Cons: the "pure-Rust OCR is weak" caveat made real — 1★, the accurate CRNN path **unvalidated**, printed-clean-text only, and it pulls `tokio`. Too immature to depend on. **Rejected** (it is the immature end of the very category Tier A occupies more maturely).

### Option 3: `oar-ocr` (ONNX Runtime + candle VLM)
- Pros: most mature (142★, crates.io); good PaddleOCR accuracy; has a candle VLM path.
- Cons: the classic pipeline is built on **ONNX Runtime** (`ort` + a bundled/downloaded C++ binary) — a native dependency and an implicit binary download, against the pure-Rust and offline stances. Adopting it drags `ort` into the tree even if only the VLM path is used. **Rejected.**

### Option 3b: Splice — `rten` detection + `candle`-TrOCR recognition (one pipeline)
- Pros: reuses the `candle` stack we already vendor for the recognizer.
- Cons: **combines the costs.** Two tensor runtimes (`rten` *and* `candle`) → double the `deny`/MSRV surface, larger binary, two model formats; `ocrs` already ships a recognizer tuned to its own detector (bridging the seam risks accuracy); and TrOCR is autoregressive-per-line and CPU-only here, far slower than `rten`'s CTC recognizer. **Explicitly rejected** — `candle` earns its place as a *separate understanding tier*, not as the recognizer inside the OCR pipeline.

### Option 4: Two-tier, pure-Rust-default (recommended)
- Pros: Tier A is **pure-Rust** (no FFI), builds on **MSRV 1.94**, passes **`cargo deny`**, and is a modest opt-in tree (~73 crates) — validated by the spike. Tier B adds genuine image *understanding* by reusing ADR-0003 machinery, and only when opted in. Matches Roteiro's established tiering; the default build is unchanged. `ocrs`/`rten` are the most mature pure-Rust OCR available (1.9k★, actively maintained), far ahead of Option 2.
- Cons: `ocrs` is Latin-script/early-preview (mitigated: it is opt-in and degrades to "no content", never blocks sync; non-Latin/handwriting is out of scope for v1); routing model downloads through our consent gate rather than ocrs's default cache is a small integration cost; Tier B weights are large (mitigated: separate opt-in feature + consent gate, and Tier A needs none).

## Consequences

- **The `image` dependency must be pinned to minimal codecs** (`default-features = false, features = ["png", "jpeg"]`). With default features, `image` pulls the AVIF encoder chain `ravif → rav1e → libfuzzer-sys`, which carries the **NCSA** licence — *not* on our `cargo deny` allow-list. Pinning to the codecs OCR actually needs drops that chain entirely: the spike went from a licence rejection at 144 crates to a clean `advisories ok, bans ok, licenses ok, sources ok` at **73**. No `deny` policy change is required. This constraint is recorded here because it is easy to reintroduce by adding a bare `image` dependency.
- **Two new opt-in features**: `image-ocr` (Tier A, `ocrs`/`rten`) and `image-vision` (Tier B, `candle` VLM, implies `inference-local-models`). The default, `inference`, and `pdf-text` builds pull neither. Each is subject to the same `deny` gate and MSRV as every dependency.
- **Extraction stays deterministic and panic-safe.** Image OCR is wired into `extract`'s content path beside prose and PDF: a decode/OCR failure degrades an image to a plain `file` node (no `meta.content`), never aborting sync. `EXTRACT_VERSION` gains a distinct namespace when `image-ocr` is enabled, so an OCR build and a default build never serve each other stale image facts from the shared cache (as `pdf-text` already does).
- **Models via the consent gate.** Tier A's `.rten` models and Tier B's VLM weights are registered in the ADR-0003 registry and pulled with consent into the model store — no implicit network fetch, offline-first preserved.
- **Ingested image text/description is content, embedded like any other** — it produces `inferred` similarity edges (clearly labelled, confidence-scored) and participates in semantic dedup and the context cache. It is never authored fact.
- **Tier B's shared engine must be *released* before the process exits.** The vision (and audio) engine is loaded once per process and reused across blobs, and it owns native llama.cpp/ggml state: on the Metal backend a loaded model's buffers stay registered in the device's residency set until the engine is **dropped**. Rust never drops `static`s, so holding the engine in a `static OnceLock` left that set non-empty when libc's C++ finalizers tore ggml-metal down at `exit()`; `ggml_metal_rsets_free` asserted and aborted a *successful* run with SIGABRT — exit 134 for any subcommand that described at least one image (issue #291). Extraction therefore keeps each engine in a releasable slot that [[crates/rto-graph/src/extract.rs#release_media_engines]] empties, and the CLI owns that teardown for the length of a run via [[crates/rto-graph/src/extract.rs#MediaEngineGuard]]. Recorded here, like the codec pin above, because parking the engine in a `static` is the obvious-looking way to write this and silently reintroduces the abort.
- **One llama.cpp backend per process, shared by both modalities.** llama.cpp's backend is a process-global: `LlamaBackend::init()` refuses a second call while a first backend is alive. Each engine used to initialise its own, so in a build with both media features the *second* engine a run needed failed to construct — and because the extractors resolve an engine with `.ok()`, the second modality was **silently inert** rather than reported (issue #296). The backend is therefore started once and shared: engines hold an `Arc` handle from [[crates/rto-llama/src/backend.rs#shared_backend]] rather than a backend of their own. That also carries the release ordering above out of the engine struct without losing it — llama.cpp frees models before the backend, and [[crates/rto-llama/src/backend.rs#release_shared_backend]] simply **declines** while any engine still holds a handle, so "engines first, backend last" is a property of ownership rather than of call order. Both live in the same build-once/release-deterministically holder, [[crates/rto-llama/src/slot.rs#EngineSlot]]. Recorded here beside the release consequence because per-engine backend initialisation is the obvious-looking way to write this, and its failure mode is a missing capability rather than an error.
- **Scope for v1:** Tier A (`image-ocr`) is the committed deliverable and ships first, since it is the default/common case and the gate passed. Tier B (`image-vision`) is designed here but sequenced after, as an opt-in enrichment.

## Advice Received

Project direction incorporated above: keep the **pure-Rust / no-C++-FFI** stance (reject the ONNX-Runtime and MNN engines despite their maturity); prefer reusing the existing **candle + ADR-0003** machinery for anything model-based; and — from the review of combining `ocrs`/`rten` with `candle` — treat `candle` as a **separate understanding tier**, not as a recognizer spliced into the OCR pipeline, because splicing combines the runtimes' costs rather than their benefits.

## Document version history

| Version | Date | Notes |
|---------|------|-------|
| 1.0 | 2026-08-09 | Accepted. Two-tier image ingestion: Tier A pure-Rust OCR (`ocrs`/`rten`, feature `image-ocr`) as the default text tier; Tier B optional `candle` document-VLM understanding (feature `image-vision`) reusing ADR-0003. Rejects `rusto-rs` (MNN C++), `yingkitw/ocr` (immature/unvalidated), `oar-ocr` (ONNX Runtime C++), and the spliced `rten`+`candle`-TrOCR pipeline. Go/no-go spike passed: MSRV 1.94 build, no FFI, `cargo deny` clean at ~73 crates — provided `image` is pinned to minimal codecs (default features pull an AVIF→`libfuzzer-sys` NCSA chain). |
| 1.1 | 2026-08-15 | Consequence added: the shared vision/audio engine must be released before process exit — a `static`-cached engine is never dropped and aborts a Metal build in ggml-metal's exit-time teardown (issue #291). No decision changed. |
| 1.2 | 2026-08-15 | Consequence added: the llama.cpp backend is a process-global, so it is initialised once and shared by every engine — a second engine used to fail to construct and go silently inert (issue #296). Release ordering (engines, then backend) is now enforced by `Arc` ownership rather than by call order. No decision changed. |

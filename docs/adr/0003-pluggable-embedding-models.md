---
Title: Pluggable embedding models — tiny static default, opt-in local models
Space: ARCH
Parent: ADRs

# ADR-specific metadata (unknown keys are ignored; used for indexing/search)
type: adr
adr-id: "0003"
status: Accepted                    # Draft | For Review | Accepted | Rejected | Superseded
architectural-significance: HIGH    # SOFT | LOW | MEDIUM | HIGH | VERY HIGH
domain: Developer Tooling
decision-makers: ["The Roteiro Project Team"]
superseded-by:
version: "1.4"
last-modified: 2026-08-17
confluence-url:
---

# ADR-0003: Pluggable embedding models — tiny static default, opt-in local models

| | |
|---|---|
| **State** | Accepted |
| **Architectural Significance** | HIGH |
| **Domain** | Developer Tooling |
| **Document version** | 1.4 |

## Reference

Governs the embedding model used by the inference layer (Stage 8) of [[docs/adr/0001-build-roteiro-unified-codebase-knowledge-graph.md]]. Answers open question Q7 (the offline embedding model + binary-size budget) from `docs/BUILD_PLAN.md`.

## Summary

Ship a **tiny static (int8) `model2vec`-style embedding compiled into the binary as the default**, so inference works offline with no download and adds only single-digit MB. Beyond that, models are **pluggable at runtime**: a small in-binary **registry** describes downloadable models (per-platform variants, url, sha256, licence, dimension), Roteiro **selects the right variant for the host** (Metal/Apple-oriented on macOS/Apple Silicon, standard GGUF elsewhere), and fetching is **consent-gated** (`[y/N]` prompt on a TTY; never in automation). The heavy inference backend (`candle`, to run GGUF/local models) sits behind a **second feature flag** so even the inference-enabled default build stays small. Chosen model file format for pluggables: **GGUF**.

## Context

Stage 8 emits `inferred` edges (doc/PDF/symbol similarity) with confidence scores. It needs a vector embedding of text. ADR-0001 mandates **offline by default**: the default build must work with no network and no separate model download. That collides with model size — a transformer embedding is tens-to-hundreds of MB to bundle; an API call breaks offline entirely.

Three forces to reconcile:

1. **Offline-by-default & lean binary** — `cargo install roteiro` must stay small and never require a download to function.
2. **Quality ceiling** — some users want better embeddings than a tiny static model gives, and want to use hardware acceleration (Apple GPU / CUDA) with larger local models.
3. **No silent network** — fetching a model is a network action a human must authorise, per ADR-0001.

A single bundled model can satisfy at most two of these. Decoupling *the default* from *what's possible* resolves the tension.

## Decision makers

- The Roteiro Project Team

## Recommended option

**Option 3 — tiny static default + pluggable local models (recommended).**

- **Default embedding:** a distilled token→vector static table, int8-quantised (`model2vec`-style), **compiled into the binary**. Pure-Rust, no runtime, fully offline, single-digit MB. Sufficient for the similarity that `inferred` edges need.
- **Model registry (in-binary):** `{ name, dim, licence, variants: { "<platform>": { url, sha256, format } } }`. Ships as data so `pull` can suggest/verify and `load` can checksum.
- **Platform-aware variant selection** at runtime against `target_os`/`target_arch`:
  - **macOS / Apple Silicon** → prefer a Metal/MLX-oriented variant. Two layers: the **backend** (`candle` with its `metal` feature runs ordinary weights on the Apple GPU) and the **model variant** (where a re-encoded build such as an `mlx-community` quantisation exists, the registry lists it as the macOS variant and it is preferred; otherwise standard GGUF + Metal backend).
  - **AMD / everything else** → the **standard** GGUF variant, CPU by default (candle's ROCm story is thin; CUDA/Vulkan can be opt-in later).
- **Consent-gated fetch:** `roteiro model pull <name>` prints size + source + licence, then on a TTY prompts **`Download <name> (~N MB, <licence>) from <url>? [y/N]`**; on `y` it fetches to `~/.roteiro/models/` and verifies the sha256. In a **non-TTY / CI** context it defaults to **No** and prints the exact manual command instead — so nothing ever touches the network without an explicit human "yes".
- **`roteiro infer --model <name|path>`** loads a local model, falling back to the bundled static default if the named model is absent.
- **Feature tiers** (nobody pays for weight they didn't ask for):
  - *default build* — no inference, no model, smallest.
  - `inference` — the tiny static model; offline; lean.
  - `inference-local-models` — pulls in `candle` (+ `metal` on macOS), GGUF loading, the registry/fetch flow.

## Options considered + consequences

### Option 1: Single bundled transformer model

- Pros: best out-of-box quality; one code path.
- Cons: adds 100–400 MB to every `roteiro` binary; heavy even when feature-gated; still can't use the user's GPU or a different model. Rejected — violates the lean-binary force.

### Option 2: No bundled model; always fetch

- Pros: smallest binary; newest models.
- Cons: inference does not work offline out of the box; every first run needs a network fetch. Rejected — violates offline-by-default (ADR-0001).

### Option 3: Tiny static default + pluggable local models (recommended)

- Pros: works offline with zero download; lean default; users can opt into bigger/GPU-accelerated local models; platform-aware acceleration; network only on explicit consent; heavy backend isolated behind a second feature so the inference default stays small.
- Cons: more moving parts (a registry, variant resolution, a fetch/consent flow, a second heavy dep); two embedding code paths (static vs. candle) to keep behind one interface; per-platform variant availability must be curated in the registry.

## Consequences

- Q7 is answered: **static/int8 default**, **GGUF** pluggable format, size budget = single-digit MB for the `inference` build.
- New deps arrive only under `inference-local-models` (`candle`, GGUF, an HTTP client for `pull`), each subject to the `cargo deny` licence gate; PDF/image extraction crates (Stage 8 ingestion) are checked likewise. The default and `inference` builds pull none of them. *(Update, v1.3: the HTTP client half no longer holds. The registry/`pull` machinery was split out into its own `models` feature, and `models` is now **on by default** — see v1.3 below. The heavy inference backend is still opt-in, which is what this bullet was protecting.)*
- `~/.roteiro/models/` becomes a user-level cache; model licences are surfaced at `pull` time and recorded in the registry.
- Inference remains a **separate, optional** pipeline that never gates offline local rebuilds (per ADR-0001) — derived+authored builds are unaffected whether or not any model is present.
- **One resolver decides which model serves a task, and can say why (Stage 33, v1.4).** This ADR made models *pluggable*; it never said who picks one. In practice seven surfaces each answered that separately — `spec draft` searched the registry, `infer --model` validated by hand, `serve` filtered the served set, `serve`/Ask ranked it, and the audio, vision and OCR paths held **compiled-in string constants**. The last of those is the part that reached users: `[models]` had keys for `embedding` and `generative` only, so **a project could not pin its ASR model at all**, and no configuration could change which model transcribed its audio. Now:
  - **`[models]` grows to five keys** — `embedding`, `generative`, `vision`, `audio`, `ocr` — one per model *kind*, not per command. `generative` governs both `spec draft` and Ask, because they want the same kind of model. Every key stays optional, and **unset resolves to exactly the model that surface used before** (`qwen3-0.6b`, `voxtral-mini-3b`, `smolvlm-500m-gguf`, `ocrs-text`, and the compiled-in hashing embedder for `infer`).
  - **One function, `rto_graph::model_choice::resolve`,** takes the task and the pins and returns the model **plus the rule that chose it**. The rule is not decoration: `roteiro config` has to answer *why did it use that model?* per surface, and it cannot do that from a bare string.
  - **It lives in `rto-graph` deliberately.** That crate pins `gix` with `default-features = false` to exclude the network transports, so the code that decides which model runs structurally cannot grow a "check the hub for a newer one" call. The registry, the store and now the choice all sit behind the same wall.
  - **Deterministic rules over categorical signals, not a classifier.** Every signal is low-cardinality — six tasks, five model kinds, installed or not — and a table over categoricals gives the property a learned selector cannot: the same inputs pick the same model on every machine, forever, which is what ADR-0015's producer identity depends on. The seed was the existing `chat_capable_model_ids`, which filters models that *cannot do the job* (an embedding model routed through `/v1/chat/completions` aborts llama.cpp with a `GGML_ASSERT`); the table generalises **capability**, and still ranks nothing.
  - **A pin that cannot be honoured is refused by name, never replaced by the default.** Silently falling back would leave the configuration *appearing* honoured while another model ran — and on the llama.cpp `mtmd` path a model of the wrong architecture does not mis-answer, it aborts the process. `roteiro config` is the single exception: it reports the error rather than refusing to run, because it is the command an operator reaches for when a pin is not doing what they expected.
- **Inference-core unification on llama.cpp (Stage 20, amends this ADR).** [[docs/adr/0006-local-model-serving.md]] adopted **llama.cpp** (`llama-cpp-2`) as the serving engine — fastest, `cargo deny`-clean, GGUF tokenizer/template for free — and named it the target for the *whole* inference core. The migration is staged: **generation is moved** — `spec draft` now generates through llama.cpp (the `serve` feature's engine) over the same GGUFs, with the candle `LocalGenerator` kept only as a transitional fallback on an `inference-local-models`-without-`serve` build. **Embeddings** (`infer`) and **image vision** (`sync`) are **scheduled next** — embeddings move to GGUF embedding models (already served via llama.cpp, e.g. `bge-small-en-v1.5-gguf`), vision to `mmproj` (already served, e.g. `smolvlm-500m-gguf`); until those internal call-sites cut over, candle (`inference-local-models` / `image-vision`) remains their backend and the two coexist transitionally. End state: one llama.cpp inference core shared by serving and internal uses; candle is removed once embeddings + vision are migrated.

## Advice Received

Decision refined with the project team: (a) prefer models built/re-encoded for the host GPU architecture — Apple-oriented on macOS, standard elsewhere; (b) when the model source is known, offer an interactive Y/N fetch rather than only printing a command. Both are incorporated above.

## Document version history

| Version | Date | Notes |
|---------|------|-------|
| 1.0 | 2026-08-08 | Accepted. Tiny static int8 default compiled in; GGUF pluggable local models via an in-binary registry; platform-aware (Metal/Apple vs standard) variant selection; consent-gated fetch; candle behind `inference-local-models`. Answers ADR-0001 Q7. |
| 1.1 | 2026-08-09 | Amended (Stage 20) for the **inference-core unification on llama.cpp** (direction set in ADR-0006). **Generation moved**: `spec draft` generates via llama.cpp (the `serve` engine) over the same GGUFs; candle `LocalGenerator` is a transitional fallback only. **Embeddings → GGUF** and **vision → `mmproj`** scheduled next (both already served via llama.cpp); candle stays their backend until they cut over, then is removed. Adds a `role` label (instruct/coding/reasoning) and opt-in coding (`qwen2.5-coder-3b`) + reasoning (`deepseek-r1-distill-qwen-1.5b`) registry entries. |
| 1.2 | 2026-08-09 | **Unification complete — candle removed.** The engine was extracted into a shared `rto-llama` crate (no HTTP/async deps), and all three internal uses cut over to it: `infer --model` embeds via GGUF embedding models (`bge-*` re-listed as F16 GGUF; the safetensors `all-MiniLM` entry dropped), `sync` image understanding uses `smolvlm-500m-gguf` + `mmproj` (candle moondream removed), and `spec draft` generates via the shared engine directly. candle-core/nn/transformers + tokenizers are gone from the tree; `inference-local-models`/`image-vision` now mean "local **llama.cpp** models". One inference core, shared by serving and internal uses. |
| 1.3 | 2026-08-16 | **`models` becomes a default feature.** The registry and consent-gated `pull` ship in a stock `cargo install roteiro`; only *running* local models (`inference-local-models`, `image-vision`, `audio-transcribe`, `serve`) stays opt-in, so this ADR's title — "tiny static default, **opt-in local models**" — is unchanged in substance. The reason is that `roteiro model pull` is the prerequisite for the offline story ADR-0001 mandates, and gating it made that story unreachable from the shipped default: the clap variant is `#[cfg(feature = "models")]`, so a stock install answered `unrecognized subcommand` rather than degrading. The consent gate is untouched — presence is not activity; nothing is fetched without an explicit `[y/N]` (or `--yes`). Cost, measured: ~2.3 MB of binary and 20 crates (`ureq` + `rustls`), pure Rust, no new host-toolchain class. `serve` was considered and deliberately **not** flipped (llama.cpp from source, a hard `cmake`/`libclang` build-script failure, and 13 unmonitored vendored advisories — see `crates/roteiro/Cargo.toml`). Licence disclosure in ADR-0017 v1.2. |
| 1.4 | 2026-08-17 | Amended (Stage 33) — **local model resolution**. `[models]` grows from two keys to five (`vision`, `audio`, `ocr` join `embedding` and `generative`), closing the gap that a project could **not pin its ASR model at all**: audio, vision and OCR were compiled-in string constants. One resolver in `rto-graph` — the crate whose `gix` pin excludes network transports — takes a task and the pins and returns the model **plus the rule that chose it**, and seven scattered call sites now ask it instead of deciding for themselves. Deterministic table over categoricals, not a classifier; it generalises `chat_capable_model_ids`'s **capability** filter and ranks nothing. A pin that names an unknown model or the wrong modality is a named error quoting the key, never a silent fallback (llama.cpp aborts rather than errors on the wrong architecture). Unset is byte-identical to the previous behaviour, proven rather than asserted. No new dependency, no network, `EXTRACT_VERSION` unchanged at 11. Also corrects the frontmatter and header table, which both read 1.2 while the history below already carried a 1.3 row. |

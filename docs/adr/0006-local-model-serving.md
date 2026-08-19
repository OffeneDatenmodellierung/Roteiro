---
Title: Local model serving — a llama.cpp-backed, code-aware OpenAI-compatible endpoint
Space: ARCH
Parent: ADRs

# ADR-specific metadata (unknown keys are ignored; used for indexing/search)
type: adr
adr-id: "0006"
status: Accepted                    # Draft | For Review | Accepted | Rejected | Superseded
architectural-significance: HIGH    # SOFT | LOW | MEDIUM | HIGH | VERY HIGH
domain: Developer Tooling
decision-makers: ["The Roteiro Project Team"]
superseded-by:
version: "1.5"
last-modified: 2026-08-19
confluence-url:
---

# ADR-0006: Local model serving — a llama.cpp-backed, code-aware OpenAI-compatible endpoint

| | |
|---|---|
| **State** | Accepted |
| **Architectural Significance** | HIGH |
| **Domain** | Developer Tooling |
| **Document version** | 1.5 |

## Reference

Adds an **opt-in local model server** so tools other than Roteiro (e.g. an Omnigent agent, an editor) can call the models a user has already pulled — offline, with no second download. Reuses the model registry and consent-gated store from [[docs/adr/0003-pluggable-embedding-models.md]], and wires in the graph query tools from [[docs/adr/0002-adopt-rmcp-for-networked-mcp-serving.md]] so the served model is **code-aware**. Configured through [[docs/adr/0007-configuration-file.md]]'s `[serve]` table. Rests on the offline-first principle of [[docs/adr/0001-build-roteiro-unified-codebase-knowledge-graph.md]]. Introduces **llama.cpp** as the high-performance inference backend, evaluated against candle and mistral.rs below.

## Summary

Serve local models over an **opt-in, loopback, OpenAI-compatible HTTP endpoint** (`roteiro serve --models`), backed by **llama.cpp** (via the Rust binding `llama-cpp-2`) for performance, with **Roteiro's own graph tools auto-registered** so the served model can query the codebase.

Three decisions:

1. **Engine: llama.cpp (`llama-cpp-2`), opt-in.** After a head-to-head de-risk (candle vs mistral.rs vs llama.cpp on this project's MSRV 1.94 + strict `cargo deny`), llama.cpp is the choice: it is the **fastest** (Metal ~129 tok/s on a 0.6B, ~2.75× CPU), the **only** candidate that passes our `cargo deny` **unchanged** (46 crates, all allow-listed), and it reads a plain GGUF's **embedded tokenizer + chat template for free** (no `tokenizer.json`, no quant plumbing). The price is a **C/C++ toolchain** (it compiles vendored llama.cpp via cmake) — accepted, deliberately, because performance is the priority (models run often in the background and developers should not wait) and the crate tree is ~10× smaller than the alternatives.
2. **Serving layer: our own thin `/v1`**, not the stock `llama-server`. `llama-cpp-2` exposes inference primitives only; we hand-roll a small axum `/v1` (`/v1/chat/completions`, `/v1/embeddings`, `/v1/models`) over `load → apply_chat_template → tokenize → decode → sample`. We own the loop **so we can wire Roteiro's tools into it** (see #3). (A passthrough to the standalone `llama-server` was considered and deferred — it is a separate process and makes the tool integration external.)
3. **Code-aware by default: auto-register Roteiro's MCP graph tools.** On `serve`, the model is handed Roteiro's [[docs/adr/0002-adopt-rmcp-for-networked-mcp-serving.md]] tools (`explain` / `debt` / `path` / `search`) via OpenAI function-calling — so a locally-served model can query *this codebase's* graph out of the box. This is Roteiro's differentiator: not just a model server, a **graph-grounded** one.

Scope remains **reuse + performance**, not a general model server. Loopback-bound by default; serves only *installed* models; never downloads. candle stays the backend for the internal uses (`infer`, `spec draft`, image vision) for now; **unifying the inference core on llama.cpp is the stated direction** (§Consequences) — a follow-up amendment to ADR-0003, not a big-bang.

## Context

Roteiro already downloads, verifies, and stores real GGUF/safetensors models for its own use. A separate local tool that only sometimes needs a model would otherwise re-download its own copy and ship its own runtime. Serving reuses the local store so nothing new is fetched and nothing leaves the machine. **Scoped to serving (v1.3).** That sentence is about `roteiro serve` and remains true of it. It is not a project-wide guarantee: [[docs/adr/0019-remote-model-tier.md]] adds an optional, default-off remote model tier that does send content off the machine, under a consent gate described there. Nothing in this ADR changes — `serve` still exposes only installed models and still never downloads — but a reader quoting this line as a general promise would now be wrong. Two things sharpened the design after the first draft:

1. **Performance is a first-class requirement.** These models run frequently in the background; slow inference is a real developer-experience cost. That tilts the engine choice toward the fastest viable option, and makes candle's modest quantized-Metal decode a genuine liability rather than a footnote.
2. **A local server is only interesting if it's *ours*.** Anyone can run Ollama or `llama-server`. Roteiro's reason to serve is to hand the model **the codebase graph** — the one query surface from ADR-0001/0002 — so the served model is code-aware. That argues for owning the request loop (to inject tools), not delegating to a black-box server.

Forces to reconcile: offline-first & self-contained (ADR-0001); don't become a general model server; honest about the C++ trade (we have held a pure-Rust preference — this opt-in feature is where we consciously relax it for performance, while the default build stays pure-Rust); and a universal interface (OpenAI API, which every calling tool already speaks).

## Decision makers

- The Roteiro Project Team

## Recommended option

**llama.cpp engine + our own `/v1` layer + auto-registered graph tools (recommended).**

- **CLI:** `roteiro serve --models [--addr 127.0.0.1:PORT]` behind an opt-in `serve` feature (pulls `llama-cpp-2`). Off by default; loopback bind; warns on a non-loopback address (no auth — a localhost dev tool; TLS/authn terminate at a reverse proxy, as ADR-0002 frames for MCP). *(Update, 2026-08-14: this is now simply `roteiro serve [--addr …]` — the model endpoint is the default for `serve`, and `--models` is a redundant deprecated flag. A `serve` build with no model installed, or a build without the `serve` feature, degrades to the llama-free `/v1/graph` API + web UI (ADR-0010) instead of erroring.)*
- **Endpoints (grow across PRs):** `/v1/chat/completions` (generation, over the same installed GGUFs), `/v1/models` (installed only), then `/v1/embeddings`. *(Embeddings note: llama.cpp embeds via GGUF embedding models; our current embedders are BERT safetensors, so embedding-serving either adds GGUF embedding entries to the registry or is served via the existing candle `LocalEmbedder` — resolved at implementation.)*
- **Execution:** models loaded lazily on first request and kept warm in a memory-bounded LRU — the resident set is capped by a byte budget (`[serve] memory_budget_mb`, GGUF size as the footprint proxy) and the least-recently-used model is unloaded past it, so several models can stay warm and swap in real time on a machine that can afford them, while a memory-limited host keeps one. **The context window is sized per request and bounded per model (v1.5, issue #486):** llama.cpp allocates the KV cache eagerly when a context is created and a context is created per generation, so a window is paid for in full on every request whether or not it is used — measured on `qwen3.8-27b`, 429 MiB at 4,096 tokens and 16,466 MiB at its trained 262,144. The prompt is already tokenised before the context exists, so each context is instead sized to that request's own `prompt + max_tokens`, floored at the 4,096 every request used to get and capped at the model's GGUF `n_ctx_train`. `[serve] max_context_tokens` lowers that cap across every served model; unset, each model offers the whole window it was trained for. Requests are serialised through the single engine mutex (so all requests are mutually exclusive, across models as well as within one) — llama.cpp batching / per-model concurrency is a later enhancement. Serves only installed models; never downloads.
- **Graph tools:** Roteiro's MCP tools auto-registered into the served model's function-calling; the model calls a tool → Roteiro executes it against the graph → result is fed back.
- **HTTP/1.1 only — HTTP/2 is a non-goal.** `axum` is taken with `http1` and
  without `http2`, so `serve` speaks no HTTP/2 and this is a decision rather than
  an omission. Three reasons, and the first is the one that would otherwise be
  rediscovered the expensive way. **HTTP/2 is a recurring source of
  denial-of-service advisories** — Rapid Reset, CONTINUATION flood, and
  RUSTSEC-2026-0258 (`h2` unbounded empty DATA frames, adopted here on an
  approved cooldown bypass). Roteiro's exposure to that last one was *nil*
  precisely because `http2` is off; enabling it converts that class of advisory
  from theoretical to reachable, permanently. **Second, over loopback it buys
  nothing** — multiplexing, HPACK and connection reuse pay off over
  high-latency links, and the bottleneck here is model inference, not transport;
  SSE streaming works over HTTP/1.1 and is universally supported. **Third, where
  it would matter it is already delegated**: HTTP/2 in practice requires TLS
  (browsers do not speak h2c), and this ADR already terminates TLS at a reverse
  proxy — so HTTP/2 belongs exactly where TLS already belongs, in software whose
  job is surviving hostile traffic.

  What would overturn this: **a concrete client that fails over HTTP/1.1.** No
  such client is known, and this repository records no client-compatibility
  matrix, so the absence is unproven rather than established. If one appears,
  that is a requirement to design against — not a reason to enable a protocol
  speculatively.

- **Acceleration:** llama.cpp's Metal backend (enabled by default on macOS in the vendored build) — this *is* the acceleration story for served models, and moots candle's quantized-Metal weakness on the serving path.

## Options considered + consequences

### Engine — candle vs mistral.rs vs llama.cpp (de-risked on MSRV 1.94 + strict `cargo deny`)

| | **llama.cpp** (`llama-cpp-2`) — chosen | **mistral.rs** | **candle** (hand-roll) |
|---|---|---|---|
| Metal tok/s (0.6B) | **~129** (2.75× CPU) | ~125 | slower (quant-decode ties CPU) |
| `cargo deny` | ✅ **passes unchanged** | ❌ fails (MPL-2.0/CDLA/0BSD core deps) | ✅ |
| Crates | **46** (mostly build-only) | 465 | large candle tree |
| OpenAI server | our `/v1` | embedded | our `/v1` |
| GGUF tokenizer/template | **free (embedded)** | free | we hand-code it |
| Cost | C++ (cmake/clang/libclang) | C/C++ + licence waivers | pure-Rust; slower; more of our code |

- **mistral.rs — rejected.** It embeds an OpenAI server (attractive) and builds on 1.94, but its **core** dependencies pull **MPL-2.0** (`option-ext` via hf-hub), **CDLA-Permissive-2.0** (`webpki-roots`), and **0BSD** (`interprocess`) — unavoidable, so it fails our `cargo deny` allow-list without waivers, and it drags 465 crates + candle 0.10 (a version split from our 0.11). Disqualified under current policy.
- **candle hand-roll — viable, not chosen for serving.** Keeps everything pure-Rust and reuses our loaders, but it is the slowest, and we'd hand-write the tokenizer/quant/Metal work llama.cpp gives for free. Given performance is the priority, it loses here — though it remains the backend for internal uses (for now).
- **llama.cpp — chosen.** Fastest, `deny`-clean unchanged, minimal Rust surface, GGUF tokenizer/template for free. Trade accepted: a C/C++ build for the opt-in `serve` feature.

### Serving layer — our `/v1` vs the stock `llama-server`
- **`llama-server` (stock) — deferred.** `llama-cpp-2` does not build it; using it means a separate process, and it makes wiring **our** graph tools external. A future optional passthrough is possible, but it is not where the Roteiro value (code-awareness) lives.
- **Our thin `/v1` — chosen.** ~5 primitives; we own the loop, so tool-registration is natural.

### Surface — OpenAI REST vs MCP-only (from v1.0 of this ADR)
- OpenAI REST is the front door (every tool speaks it); MCP (ADR-0002) is reused *inside* it as the tool layer, not as the primary API.

## Consequences

- A new opt-in `serve` feature pulls **`llama-cpp-2`** — a **C/C++ toolchain** (cmake, clang, libclang) is required to build *that feature*; the default and other opt-in builds stay as they are. **No `cargo deny` change** is needed — `llama-cpp-2`'s 46-crate tree is fully allow-listed with no advisories. This is the point at which the project consciously accepts C++ FFI for an **opt-in** performance path, while holding pure-Rust for the default build.
- Serving **only ever exposes installed models** and **never downloads** — the ADR-0003 consent gate is preserved.
- The served model is **code-aware**: Roteiro's graph tools are auto-registered, so a local model can `explain`/`search`/`path`/`debt` over this repo — dogfooding the one query surface (ADR-0001) for external agents.
- **Direction — inference-core unify.** Performance matters for the internal uses too (`spec draft`, `infer`), so llama.cpp — now proven fast and `deny`-clean — is the stated target for the *whole* inference core: a **staged migration off candle** (generation first; embeddings move to GGUF models; vision to `mmproj`), recorded as a follow-up amendment to ADR-0003, not a big-bang. Until then candle remains the internal backend and the two coexist only transitionally (serving reads the same GGUFs candle does, so generation stays consistent).
- **`serve` shares the process's one llama.cpp backend, so a second engine is possible at all.** llama.cpp's backend is a process-global and refuses a second initialisation, so while each engine initialised its own, the long-lived `serve` process was limited to whichever engine it built first: a second modality arriving alongside chat could not be served without a restart, and — because callers resolve an engine with `.ok()` — the failure surfaced as a **quietly missing capability** rather than an error, which is far worse in a server than in a one-shot CLI run (issue #296). The backend is now started once and handed out as an `Arc` by [[crates/rto-llama/src/backend.rs#shared_backend]]; the server's engine and the extractors' engines are peers on it, and it is freed only once none of them holds a handle ([[crates/rto-llama/src/backend.rs#release_shared_backend]]). This is a property of the shared engine core, so it applies identically to `serve`, `infer --model` and `spec draft`.
- Composes with ADR-0007: `[serve]` sets defaults (enable, addr, which models, tool-registration on/off), overridable by CLI flags.

## Advice Received

Project direction incorporated: **prioritise performance** (background use; developers shouldn't wait) — so use the fastest viable engine even at the cost of C++; since we're allowing C++ bindings, **llama.cpp** is the pick (and it passes `deny` cleanly, unlike mistral.rs); use **our own internal serving layer** (not the stock server) so we can **auto-register Roteiro's MCP/agent tools** and serve a code-aware model; keep it opt-in and offline; and treat unifying the inference core on llama.cpp as the direction.

## Document version history

| Version | Date | Notes |
|---------|------|-------|
| 1.0 | 2026-08-09 | Accepted. Opt-in loopback OpenAI-compatible endpoint reusing installed models, warm + serialised over the ADR-0002 stack; scoped to reuse; candle-implied engine; rejected Ollama-replacement and MCP-only as the front door. |
| 1.1 | 2026-08-09 | Revised after a head-to-head engine de-risk. **Engine → llama.cpp (`llama-cpp-2`)** — fastest, and the only candidate passing `cargo deny` unchanged (mistral.rs fails on MPL-2.0/CDLA/0BSD; candle is slower). **Serving via our own thin `/v1`** (not stock `llama-server`) so **Roteiro's graph tools auto-register** into the model (code-aware serving). Accepts a C/C++ toolchain for the opt-in `serve` feature; no `deny` change needed. States the candle→llama.cpp inference-core unify as the direction (follow-up ADR-0003 amendment). |
| 1.2 | 2026-08-15 | Consequence added: llama.cpp's backend is a process-global, initialised once and shared by every engine, so a long-lived `serve` process can hold more than one engine instead of silently losing every engine after the first (issue #296). No decision changed. |
| 1.3 | 2026-08-17 | Scoped, not changed. "Nothing leaves the machine" is stated of **serving**, which is what it always described; ADR-0019 adds an optional default-off remote model tier elsewhere in the product, so the sentence needed a boundary before it was read as project-wide. No decision in this ADR changed. |
| 1.4 | 2026-08-18 | HTTP/2 recorded as a **non-goal** rather than left as an absence: `axum` is taken with `http1` only, HTTP/2 is a recurring DoS-advisory surface (Rapid Reset, CONTINUATION flood, RUSTSEC-2026-0258 — to which this build's exposure was nil *because* `http2` is off), it buys nothing over a loopback bind, and where it matters it is already delegated to the reverse proxy this ADR terminates TLS at. What would overturn it is stated: a concrete client that fails over HTTP/1.1. Also corrected two defects found while editing — an inline note cited *(Update, v1.5)*, a version this document has never had (the change it describes landed 2026-08-14 while the document was at 1.1, and was never given a history row), now labelled by its date; and the history table listed 1.3 above 1.2, now ascending. |
| 1.5 | 2026-08-19 | Amended (issue #486). **The context window becomes a per-request, per-model quantity** rather than one hardcoded 4,096 that no configuration key could reach. Three measurements decided the shape. (a) llama.cpp allocates the KV cache *eagerly* in the `llama_kv_cache` constructor, and `LlamaEngine` builds a context *per generation* — so a large fixed window is spent on every request, including a fifty-token one: 16,466 MiB on `qwen3.8-27b` at its trained 262,144. (b) The served models' trained windows span **512×** (262,144 for `qwen3.8-27b`, 512 for `bge-large-en-v1.5`), so no single number is correct for the set. (c) Sizing is possible because tokenisation already precedes context creation on both the text and media paths, so the count is exact rather than estimated. A request therefore gets `prompt + max_tokens + headroom`, floored at the old 4,096 so nothing shrinks, and capped at the model's own `n_ctx_train`. New `[serve] max_context_tokens` lowers that cap; it is a **value** under [[docs/adr/0007-configuration-file.md]] v1.4 by that ADR's default rule — the default already grants each model's full window, so the key can only spend *less* of the machine, and clause 4 is never reached. A ceiling above `n_ctx_train` is **clamped with a warning** (one number spans models differing 512×); a *request* that does not fit is **refused** as a 400, never truncated. `n_ubatch` is unchanged at 512, and #349's finding that `n_batch` may follow `n_ctx` for free was **re-measured at the larger window** rather than extrapolated: +2 MiB against 8,366 MiB at `n_ctx = 131,072`. KV-cache quantisation is reachable (`with_type_k`/`with_type_v`) and measured at 1.85× on this model, but is not adopted — it changes generated output, and per-request sizing removes the memory pressure that would have justified it. |

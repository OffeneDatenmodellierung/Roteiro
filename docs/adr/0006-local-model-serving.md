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
version: "1.2"
last-modified: 2026-08-15
confluence-url:
---

# ADR-0006: Local model serving — a llama.cpp-backed, code-aware OpenAI-compatible endpoint

| | |
|---|---|
| **State** | Accepted |
| **Architectural Significance** | HIGH |
| **Domain** | Developer Tooling |
| **Document version** | 1.2 |

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

Roteiro already downloads, verifies, and stores real GGUF/safetensors models for its own use. A separate local tool that only sometimes needs a model would otherwise re-download its own copy and ship its own runtime. Serving reuses the local store so nothing new is fetched and nothing leaves the machine. Two things sharpened the design after the first draft:

1. **Performance is a first-class requirement.** These models run frequently in the background; slow inference is a real developer-experience cost. That tilts the engine choice toward the fastest viable option, and makes candle's modest quantized-Metal decode a genuine liability rather than a footnote.
2. **A local server is only interesting if it's *ours*.** Anyone can run Ollama or `llama-server`. Roteiro's reason to serve is to hand the model **the codebase graph** — the one query surface from ADR-0001/0002 — so the served model is code-aware. That argues for owning the request loop (to inject tools), not delegating to a black-box server.

Forces to reconcile: offline-first & self-contained (ADR-0001); don't become a general model server; honest about the C++ trade (we have held a pure-Rust preference — this opt-in feature is where we consciously relax it for performance, while the default build stays pure-Rust); and a universal interface (OpenAI API, which every calling tool already speaks).

## Decision makers

- The Roteiro Project Team

## Recommended option

**llama.cpp engine + our own `/v1` layer + auto-registered graph tools (recommended).**

- **CLI:** `roteiro serve --models [--addr 127.0.0.1:PORT]` behind an opt-in `serve` feature (pulls `llama-cpp-2`). Off by default; loopback bind; warns on a non-loopback address (no auth — a localhost dev tool; TLS/authn terminate at a reverse proxy, as ADR-0002 frames for MCP). *(Update, v1.5: this is now simply `roteiro serve [--addr …]` — the model endpoint is the default for `serve`, and `--models` is a redundant deprecated flag. A `serve` build with no model installed, or a build without the `serve` feature, degrades to the llama-free `/v1/graph` API + web UI (ADR-0010) instead of erroring.)*
- **Endpoints (grow across PRs):** `/v1/chat/completions` (generation, over the same installed GGUFs), `/v1/models` (installed only), then `/v1/embeddings`. *(Embeddings note: llama.cpp embeds via GGUF embedding models; our current embedders are BERT safetensors, so embedding-serving either adds GGUF embedding entries to the registry or is served via the existing candle `LocalEmbedder` — resolved at implementation.)*
- **Execution:** models loaded lazily on first request and kept warm in a memory-bounded LRU — the resident set is capped by a byte budget (`[serve] memory_budget_mb`, GGUF size as the footprint proxy) and the least-recently-used model is unloaded past it, so several models can stay warm and swap in real time on a machine that can afford them, while a memory-limited host keeps one. Requests are serialised through the single engine mutex (so all requests are mutually exclusive, across models as well as within one) — llama.cpp batching / per-model concurrency is a later enhancement. Serves only installed models; never downloads.
- **Graph tools:** Roteiro's MCP tools auto-registered into the served model's function-calling; the model calls a tool → Roteiro executes it against the graph → result is fed back.
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

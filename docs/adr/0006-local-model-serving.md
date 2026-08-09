---
Title: Local model serving — reuse pulled models over an OpenAI-compatible endpoint
Space: ARCH
Parent: ADRs

# ADR-specific metadata (unknown keys are ignored; used for indexing/search)
type: adr
adr-id: "0006"
status: Accepted                    # Draft | For Review | Accepted | Rejected | Superseded
architectural-significance: MEDIUM  # SOFT | LOW | MEDIUM | HIGH | VERY HIGH
domain: Developer Tooling
decision-makers: ["The Roteiro Project Team"]
superseded-by:
version: "1.0"
last-modified: 2026-08-09
confluence-url:
---

# ADR-0006: Local model serving — reuse pulled models over an OpenAI-compatible endpoint

| | |
|---|---|
| **State** | Accepted |
| **Architectural Significance** | MEDIUM |
| **Domain** | Developer Tooling |
| **Document version** | 1.0 |

## Reference

Adds an **opt-in local model server** so tools other than Roteiro (e.g. an Omnigent agent) can call the models a user has already pulled — offline, with no second download or second runtime. Reuses the model registry and consent-gated store from [[docs/adr/0003-pluggable-embedding-models.md]] and the candle loaders that grew across it and [[docs/adr/0004-spec-blueprint-authoring-pillar.md]] / [[docs/adr/0005-image-ocr-vision-ingestion.md]], and the networked-serving stack already chosen in [[docs/adr/0002-adopt-rmcp-for-networked-mcp-serving.md]]. Configured through [[docs/adr/0007-configuration-file.md]]'s `[serve]` table. See [[docs/adr/0001-build-roteiro-unified-codebase-knowledge-graph.md]] for the offline-first principle this rests on.

## Summary

Expose the **installed registry models** — embeddings (`LocalEmbedder`), generative (`LocalGenerator`), and image description (`LocalVlm`) — over a **local, opt-in, OpenAI-compatible HTTP endpoint**, so any tool that speaks the OpenAI API can use them **offline** without pulling its own copies or bundling its own inference runtime.

Deliberately scoped to *reuse*, **not to compete with Ollama / llama.cpp**:

- **Opt-in & local.** A feature-gated `roteiro serve --models` (bound to `127.0.0.1` by default), off by default; the default binary and the graph query surface are unchanged.
- **OpenAI-compatible surface.** `POST /v1/embeddings` and `POST /v1/chat/completions` first (the two endpoints every tool already speaks), image-description next. Models are addressed by their **registry name** (`qwen3-8b`, `bge-large-en-v1.5`, …); only *installed* models are served.
- **Warm & serialised.** The model is loaded once and kept resident; requests are **serialised through it** (candle generation is stateful — a per-model KV cache), a mutex/queue rather than true concurrency. Correctness over throughput.
- **Reuses what exists.** The HTTP stack from ADR-0002 (rmcp / streamable HTTP) and the consent-gated model store from ADR-0003 — so this is a thin surface, not a new subsystem.

**Honest positioning:** this is "reuse the models you already pulled," not a high-throughput inference server. candle CPU generation is slower than llama.cpp; on Apple Silicon the acceleration path (candle's `metal` backend, target-gated on macOS) makes it genuinely usable, especially for the larger tiers — but we do not promise Ollama-class performance.

## Context

Roteiro already downloads, verifies, and stores real models for its own use (`infer --model`, `spec draft`, image OCR/vision). A user running Roteiro on a plane already has, say, `qwen3-8b` and `bge-large` on disk. A *separate* local tool (an Omnigent agent that only sometimes needs a foundation model) would otherwise **re-download its own copy and ship its own runtime** to do the same thing offline. That is wasteful and, for a fully-offline workflow, a real friction.

Forces to reconcile:

1. **Offline-first & self-contained (ADR-0001).** The value is precisely that nothing leaves the machine and nothing new is fetched — reuse the local store. The default build must not grow; serving is opt-in.
2. **Don't become a worse Ollama.** A general model server is a large, ongoing surface (streaming, batching, concurrency, model management, format compatibility). We must scope tightly to *reuse* or this balloons past the project's mission.
3. **candle models are stateful and CPU-slow.** `LocalGenerator`'s KV cache means requests cannot run concurrently on one model instance; and CPU decode is modest. The design must serialise and set expectations honestly (with the Mac `metal` path as the performance answer).
4. **A universal interface.** The calling tools (Omnigent, editors, scripts) overwhelmingly speak the **OpenAI API**. That, not a bespoke protocol, is the interoperable choice. (MCP from ADR-0002 is the graph surface; it can expose these as tools later, but REST is the front door.)

## Decision makers

- The Roteiro Project Team

## Recommended option

**Option 3 — opt-in OpenAI-compatible local endpoint over the existing stack (recommended).**

- **CLI:** `roteiro serve --models [--addr 127.0.0.1:PORT]` (feature `serve-models`), or the equivalent `[serve]` config (ADR-0007). Off by default; loopback bind by default; a warning if bound to a non-loopback address.
- **Endpoints (grow across PRs):**
  - `POST /v1/embeddings` — `LocalEmbedder`; the fast, high-value, low-risk one (stateless, cheap). Ships first.
  - `POST /v1/chat/completions` — `LocalGenerator` (Qwen2/Qwen3), ChatML mapped from the OpenAI messages; non-streaming first, then SSE streaming.
  - `GET /v1/models` — lists the *installed* registry models.
  - Image description (`LocalVlm`) — exposed as a vision `chat/completions` (OpenAI image-input shape) once the text endpoints are solid.
- **Execution model:** each model is loaded lazily on first request and kept warm; a per-model mutex serialises requests (KV-cache safety); the offline/consent invariants are unchanged (it only serves already-pulled models — it never downloads on demand).
- **Acceleration:** honours the ADR-0006-adjacent acceleration decision — candle's `metal` backend, target-gated on macOS — so a Mac serves the big tiers at usable speed. No MLX (candle+Metal was de-risk-verified to run our Q4_K_M models correctly; a second FFI engine is not justified).

## Options considered + consequences

### Option 1: Don't build it — tell users to run Ollama/llama.cpp
- Pros: zero work; those tools are faster and battle-tested.
- Cons: defeats the point — the user then maintains a *second* model store and runtime, re-downloads GGUFs, and loses the "one self-contained offline tool" property. Rejected as the whole answer (but we explicitly *don't* try to match their performance).

### Option 2: Expose models only through MCP (ADR-0002) tools
- Pros: reuses the exact server we already run; no new protocol.
- Cons: most external tools speak the OpenAI API, not MCP tool-calls, so adoption friction is high. Rejected as the *primary* surface — MCP can expose serving as tools later, but REST is the front door.

### Option 3: Opt-in OpenAI-compatible local endpoint (recommended)
- Pros: universally callable; reuses the registry, store, and HTTP stack; strictly offline (serves only local models); scoped to reuse, not a general server; default build unchanged.
- Cons: candle CPU throughput is modest and generation is serialised (mitigated: warm model, honest positioning, Mac `metal` acceleration for the big tiers); maintaining even a small OpenAI-compat surface is ongoing work (mitigated: start with embeddings + non-streaming chat, grow deliberately).

## Consequences

- A new opt-in `serve-models` feature and `roteiro serve --models`; the default binary, `infer`, `spec`, and the graph surface are untouched. Loopback-by-default; a non-loopback bind warns (there is no auth — it is a localhost dev tool, TLS/authn terminate at a reverse proxy, as ADR-0002 already frames for MCP).
- Serving **only ever exposes installed models** and **never downloads** — the consent gate (ADR-0003) is preserved: you serve what you chose to pull.
- Requests are serialised per model; a busy generative request blocks another. Documented; acceptable for a single-user local endpoint.
- Performance is honestly "reuse-grade," not "Ollama-grade"; the Mac `metal` backend is the answer for the larger tiers, and the coding/reasoning models the registry may add are natural things to serve.
- Composes with ADR-0007: `[serve]` in `roteiro.toml` sets defaults (enable, addr, which models to expose), overridable by CLI flags.

## Advice Received

Project direction incorporated: build it, but keep it a **reuse** endpoint, not a general model server — scope to the OpenAI endpoints tools actually call, reuse the existing registry/store/HTTP stack, stay offline (serve only local models, never fetch), and be honest that performance is candle-grade with the Mac `metal` path as the accelerator rather than promising llama.cpp throughput.

## Document version history

| Version | Date | Notes |
|---------|------|-------|
| 1.0 | 2026-08-09 | Accepted. Opt-in, loopback OpenAI-compatible local endpoint (`roteiro serve --models`) reusing the installed registry models (embeddings → generative → vision), warm + serialised, over the ADR-0002 HTTP stack; scoped to *reuse* not a general server; honest candle-grade performance with the macOS `metal` acceleration path; configured via ADR-0007's `[serve]`. Rejects "just use Ollama" and MCP-only as the primary surface. |

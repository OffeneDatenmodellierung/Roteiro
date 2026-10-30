---
Title: External model provider — models run in a local server the user already has, not inside Roteiro
Space: ARCH
Parent: ADRs

# ADR-specific metadata (unknown keys are ignored; used for indexing/search)
type: adr
adr-id: "0027"
status: For Review                  # Draft | For Review | Accepted | Rejected | Superseded
architectural-significance: VERY HIGH  # SOFT | LOW | MEDIUM | HIGH | VERY HIGH
domain: Inference
decision-makers: ["The Roteiro Project Team"]
superseded-by:
version: "0.1"
last-modified: 2026-10-30
confluence-url:
---

# ADR-0027: External model provider — models run in a local server the user already has, not inside Roteiro

|  |  |
|---|---|
| **State** | For Review |
| **Architectural Significance** | VERY HIGH |
| **Domain** | Inference |
| **Document version** | 0.1 |

## Reference

Supersedes the in-process engine of [[docs/adr/0006-local-model-serving.md]]
and the GGUF half of the model store in
[[docs/adr/0003-pluggable-embedding-models.md]]. Amends
[[docs/adr/0005-image-ocr-vision-ingestion.md]] (vision moves to the provider,
OCR does not), [[docs/adr/0015-generated-media-content-artifact-store.md]]
(producer identity of a model Roteiro did not pin), and
[[docs/adr/0019-remote-model-tier.md]] (which endpoint is egress). Configured
through [[docs/adr/0007-configuration-file.md]].

## Summary

Roteiro stops **running** models and stops **storing** the GGUF models it ran.
Every generative, embedding, vision and audio call goes to an
**OpenAI-compatible server the user already runs on this machine** — Ollama,
`llama-server`, LM Studio — named once as `[provider] base_url`.

Three things are kept, and they are the substance of the decision as much as the
removals are:

1. **`roteiro serve`'s `/v1` endpoint stays.** It becomes a graph-tools layer in
   front of the provider rather than a model host. The value it adds — graph
   tools in the conversation, the Ask panel — was never llama.cpp's.
2. **OCR stays in-process.** `ocrs` is pure Rust and is not a language model; no
   provider serves it. `roteiro model pull` survives, narrowed to OCR weights.
3. **Audio transcription stays**, served by a provider that accepts audio input.
   Ollama does not; `llama-server` does, so the setup guide names it for that
   one task.

## Context

llama.cpp is the largest third-party surface Roteiro carries. It needs cmake and
a loadable libclang at build time, it panics in a build script without them, and
`docs/VENDORED_DEPENDENCIES.md` records published upstream advisories for it that
`cargo audit` cannot see. Every hour spent on its batch sizing, prefix cache,
speculative decoding and context budgets is an hour spent keeping pace with
projects whose whole job that is.

Users who want local models already run one of those projects. Asking them to
pull a second copy of the same weights into `~/.roteiro/models` — in a format
and at a quantisation Roteiro chose — is a cost with no matching benefit.

Every consumer already talks to a model through one seam:
[[crates/rto-llama/src/engine.rs#Engine]]. An HTTP implementation of that trait
replaces the in-process one without changing the consumers' shape.

## Decision makers

The Roteiro Project Team. D1–D3 below were decided by the project owner on
2026-10-30.

## Recommended option

### 1. One provider, named by URL, loopback by default

`[provider] base_url` names the server, e.g. `http://127.0.0.1:11434/v1`. A
credential, if the server wants one, is an environment variable and never a
config key, for ADR-0019 §3's reason: `roteiro.toml` is committed.

**A non-loopback `base_url` is refused.** Sending repository content to another
machine is egress, and ADR-0019 already decides what egress requires. Until a
non-loopback provider is routed through that gate, the refusal is the gate. With
that rule the key is a *value* under ADR-0007 — a committed project file cannot
use it to send anything off the machine.

There is no reachability probe at startup. Whether the server answers is
discovered by the first call, which names the URL when it fails.

### 2. `[models]` values become provider model ids

`[models] generative = "qwen3:8b"` names a model **the provider** serves. Roteiro
no longer validates the name against a registry, because it no longer owns the
list. `[models] ocr` is the exception and still names a registry entry.

### 3. The tool protocol does not change

The provider engine reports that it does **not** carry tools, so the existing
loop keeps advertising tools in a system turn and parsing the model's
`<tool_call>` text. That keeps the protocol identical across providers, rather
than depending on whether a given server version parses a given model's
tool-call dialect.

### 4. Model identity is what the provider can attest

A registry model was pinned by digest. A provider tag such as `qwen3:8b` is a
mutable pointer. Media producer identity (ADR-0015) records the provider URL and
model id, plus the provider's digest where it exposes one, and a producer with
no digest is graded accordingly rather than presented as pinned.

### 5. Typed answers read `logprobs`

Typed questions read the label-token distribution. Over HTTP that is
`top_logprobs`, capped at the top 20 tokens, so `option_mass` and the margin
become approximations of what the in-process engine measured, and the figures
recorded before this ADR are not comparable to those after it. A provider that
returns no `logprobs` is refused by name rather than answered with a guess.

## Options considered + consequences

| Option | Verdict |
|---|---|
| **External provider, keep `/v1`, OCR and audio** | **Recommended.** Removes the C++ surface; keeps every user-visible capability. |
| External provider, drop `/v1` | Rejected (D1). The graph tools and Ask panel are Roteiro's value and are pure Rust. |
| External provider, drop audio | Rejected (D3). Costs a capability to avoid one line of setup. |
| Keep llama.cpp for audio only | Rejected. Keeps the whole toolchain and advisory surface for one task. |
| Keep the status quo | Viable, at the costs in Context. |

## Consequences

- The default and `--all-features` builds need no C++ toolchain.
- `roteiro model pull` no longer offers GGUF models. That and the change in what
  `[models]` values mean are breaking for `roteiro` users and ship as a major.
- Performance features — speculative decoding, prefix cache, residency budgets —
  become the provider's. `[serve] prefix_cache_mb` and `memory_budget_mb` stop
  meaning anything and are removed with the engine.
- "Offline" now depends on the provider being local, which §1 enforces.
- `docs/MODEL_SETUP.md` becomes the place that says which models each task needs.

## Status

**For Review.** Implementation lands in steps: the provider engine and `/v1`
first, then each consumer, then the removal of the in-process engine and the
GGUF registry. Until the removal step, a build with `serve` still carries
llama.cpp and uses it when `[provider] base_url` is unset.

## Version history

| Version | Date | Change |
|---|---|---|
| 0.1 | 2026-10-30 | Initial. Records D1 (keep `/v1`), D2 (keep OCR weights) and D3 (keep transcription via an audio-capable provider). |

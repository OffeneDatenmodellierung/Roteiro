---
Title: Retire the in-process llama.cpp engine and the GGUF model store — every model call goes through the provider
Space: ARCH
Parent: ADRs

# ADR-specific metadata (unknown keys are ignored; used for indexing/search)
type: adr
adr-id: "0033"
status: Accepted                    # Draft | For Review | Accepted | Rejected | Superseded
architectural-significance: VERY HIGH    # SOFT | LOW | MEDIUM | HIGH | VERY HIGH
domain: Inference
decision-makers: ["The Roteiro Project Team (owner decisions, 2026-10-30 and 2026-11-14)"]
superseded-by:
version: "1.0"
last-modified: 2026-11-14
confluence-url:
---

# ADR-0033: Retire the in-process llama.cpp engine and the GGUF model store — every model call goes through the provider

|  |  |
|---|---|
| **State** | Accepted |
| **Architectural Significance** | VERY HIGH |
| **Domain** | Inference |
| **Document version** | 1.0 |

## Reference

- Supersedes [[docs/adr/0006-local-model-serving.md]] (local model serving over
  pulled GGUF models).
- Builds on [[docs/adr/0028-one-engine-local-or-hosted.md]], which made the
  provider engine the one way to reach a local or hosted server.
- The engine: [[crates/roteiro/src/provider.rs#ProviderEngine]]; the trait it
  implements: [[crates/rto-llama/src/engine.rs#Engine]].
- The media producers it is injected into:
  [[crates/rto-graph/src/media/producers.rs#provide]].
- Issue #969.

## Summary

Roteiro no longer runs a model in its own process. The `llama` feature of
`rto-llama`, the `llama-cpp-2` / `llama-cpp-sys-2` crates and every GGUF entry
in the model registry are gone. Ask, `spec draft`, `review --llm`,
`infer --model`, image description and audio transcription all go through the
provider engine — Ollama, `llama-server`, LM Studio, or a hosted API — named by
`[provider] base_url` and `[models]`. `roteiro model pull` installs the OCR
weights only. `roteiro serve` keeps its `/v1` as the graph-tools layer in front
of the provider, and joins the default build.

## Context

The in-process engine was the reason Roteiro needed a C/C++ toolchain
(`cmake`, `libclang`, minutes of build on small machines) and the reason
`docs/VENDORED_DEPENDENCIES.md` tracked 13 llama.cpp advisories by hand, outside
`cargo audit`'s reach. ADR-0028 had already made the provider engine the single
path for everything the provider could serve, so the in-process half was a
second engine maintained for one configuration.

## Decision makers

The Roteiro Project Team. The owner decided the plan on 2026-10-30 (D1–D3) and
the open points on 2026-11-14.

## Recommended option

### Owner decisions

|  | Decision |
|---|---|
| D1 | `roteiro serve`'s `/v1` stays, as the graph-tools layer in front of the provider. |
| D2 | OCR stays in-process (pure Rust, `ocrs`); `model pull` is narrowed to its weights. |
| D3 | Transcription stays, through a provider that accepts audio (`llama-server`; Ollama has none). |
| Transport | Audio is sent as an OpenAI `input_audio` chat part, the same path images take — no `/v1/audio/transcriptions` client. |
| Media identity | A provider exposes no weights digest, quantisation or projector, so those producer fields carry `provider:<host>/<model>`. A record names the server and model that wrote it; the first `media build` after upgrading writes new records. |
| `--typed-class` | Removed. It read the label-token distribution, which only the in-process engine exposed. Recorded runs that used it still parse. |
| `[serve]` budget keys | `memory_budget_mb`, `max_context_tokens` and `prefix_cache_mb` are accepted with a warning that they do nothing; the provider owns context and memory. |
| Defaults | `serve` joins the default features: without llama.cpp it is axum, tokio and rustls-on-*ring*. |

### How each surface reaches a model

| Surface | Before | Now |
|---|---|---|
| Ask (`serve`) | provider, else installed GGUF models | provider; without `base_url`, the browse-only UI |
| `spec draft`, `review --llm` | provider, else llama.cpp | provider, else a refusal naming what to set |
| `infer --model` | a GGUF embedding model | `Engine::embed` on the provider, probed once so a wrong id is a named error |
| `media build` (vision, audio) | llama.cpp `mtmd` with a projector | `Engine::chat` with an image or `input_audio` part |
| OCR (`sync`) | `ocrs` in-process | unchanged |

The provider engine lives in `roteiro`, which depends on `rto-graph`, so the
media producers cannot build it. `roteiro` hands it over with
`rto_graph::media::producers::provide` before `media status` / `media build`.

### The resolver

`[models] ocr` still resolves against the registry. Every other key is a
provider model id, passed through as `ModelSource::Provider`: the provider owns
its list and refuses an unknown id itself.

## Options considered + consequences

| Option | Why not |
|---|---|
| Keep llama.cpp behind a non-default feature | A second engine kept for one configuration, with the toolchain and the unmonitored advisories it brings for whoever enables it. |
| `/v1/audio/transcriptions` for audio | New multipart code, drops the transcription prompt, and `llama-server` serving that route was not confirmed; `input_audio` already works. |
| Make the media digest fields optional | A schema change for a fact the sentinel states exactly. |

## Consequences

- **Breaking, so 8.0.0**: `model pull` of any non-OCR model is an unknown model;
  `review --typed-class` is gone; the `inference-local-models` feature and the
  `llama` features of `rto-llama` and `rto-serve` are gone.
- No C/C++ toolchain for any feature; `VENDORED_DEPENDENCIES.md` loses its
  llama.cpp rows.
- Media records written by the in-process engine stay readable; new ones are
  written beside them under the provider identity.
- A model call now always leaves the process. With a loopback `base_url` it stays
  on the machine; a hosted one is announced on every run (ADR-0028).

## Status

Accepted and implemented in the PR that closes #969.

## Version history

| Version | Date | Change |
|---|---|---|
| 1.0 | 2026-11-14 | Initial. Retires the in-process llama.cpp engine and the GGUF model store; every model call goes through the provider. Records D1–D3 and the 2026-11-14 decisions: `input_audio` for transcription, the `provider:<host>/<model>` media identity, `--typed-class` removed, the `[serve]` budget keys warned about, `serve` in the default build. Supersedes ADR-0006. |

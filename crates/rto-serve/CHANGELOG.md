# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.27.3](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-serve-v1.27.2...rto-serve-v1.27.3) - 2026-08-20

### Fixed

- *(serve)* raise the two budgets #550's refusals made visible

### Other

- Potential fix for pull request finding
- Merge remote-tracking branch 'origin/main' into fix/serve-tool-budgets

## [1.27.2](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-serve-v1.27.1...rto-serve-v1.27.2) - 2026-08-20

### Fixed

- *(serve)* a tool call is never the user's answer

### Other

- *(serve)* state the #489 guarantee with its condition, everywhere

## [1.26.2](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-serve-v1.26.1...rto-serve-v1.26.2) - 2026-08-19

### Other

- Merge remote-tracking branch 'origin/main' into fix/489-xml-tool-dialect

## [1.26.1](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-serve-v1.26.0...rto-serve-v1.26.1) - 2026-08-19

### Other

- Merge pull request #507 from OffeneDatenmodellierung/docs/448-500-provenance-closed-and-crate-posture
- two standing decisions, so they stop arriving as escalations

## [1.26.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-serve-v1.25.0...rto-serve-v1.26.0) - 2026-08-19

### Added

- *(serve)* honour a client's `tools` on `/v1/chat/completions`

### Fixed

- *(serve)* close the graph-tool suppression gap; bound the client tool surface

## [1.13.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-serve-v1.12.0...rto-serve-v1.13.0) - 2026-08-16

### Other

- *(llama,serve)* pin the batch guard, and measure what the wider batch costs

## [1.10.1](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-serve-v1.10.0...rto-serve-v1.10.1) - 2026-08-15

### Fixed

- *(llama)* share one backend per process

## [1.9.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-serve-v1.8.0...rto-serve-v1.9.0) - 2026-08-14

### Added

- *(ask)* ground answers with search snippets + stricter grounding prompt
- *(serve)* scope the workspace Ask to the selected workspace

### Fixed

- *(ask)* address PR #285 Copilot review comments
- *(serve)* clippy --all-targets clean in the workspace-routing test
- *(serve)* prove per-workspace routing + correct app.js header comment

## [1.7.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-serve-v1.6.0...rto-serve-v1.7.0) - 2026-08-14

### Fixed

- *(serve)* never run chat on an embedding model (server hard-crash)

## [1.1.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-serve-v1.0.0...rto-serve-v1.1.0) - 2026-08-13

### Other

- *(deps)* upgrade dependencies to latest MSRV-1.94-compatible

## [1.0.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-serve-v0.0.18...rto-serve-v1.0.0) - 2026-08-11

First stable release — the public API is now covered by SemVer; breaking changes will bump the major version.

### Added
- **Workspace-aware `/v1` (ADR-0008):** `/v1/{project}/…` path routing (pre-binds a
  project), `GET /v1/projects` (client-side discovery), and `serve_blocking_router[_tls]`
  to serve a caller-composed router — used to mount `/v1` and `/mcp` on one port.
- `ToolRegistry::projects()` for enumerating hosted projects.


## [0.0.18](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-serve-v0.0.17...rto-serve-v0.0.18) - 2026-08-11

### Other

- *(debt)* precision pass — intent-debt now reflects real debt (97 → 6)

## [0.0.16](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-serve-v0.0.15...rto-serve-v0.0.16) - 2026-08-11

### Added

- *(audio)* transcribe audio into the graph via llama.cpp mtmd ([#18](https://github.com/OffeneDatenmodellierung/Roteiro/pull/18))
- *(serve)* in-app TLS for the model endpoint (ADR-0002 follow-up)

### Other

- address PR #155 review on TLS wiring

## [0.0.14](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-serve-v0.0.13...rto-serve-v0.0.14) - 2026-08-10

### Other

- fix crate README wording (PR #117 review follow-up)
- *(crates)* per-crate crates.io READMEs + refresh root README (Stage 14)
- *(llama)* extract rto-llama inference core from rto-serve

## [0.0.13](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-serve-v0.0.12...rto-serve-v0.0.13) - 2026-08-09

### Added

- *(serve)* vision serving — multimodal /v1/chat/completions via llama.cpp mtmd (ADR-0006)
- *(serve)* /v1/embeddings from GGUF via llama.cpp (ADR-0006, completes Stage 19)
- *(serve)* auto-register graph tools — code-aware serving (ADR-0006, Stage 19b)
- *(serve)* SSE streaming for /v1/chat/completions (ADR-0006, Stage 19b)

### Fixed

- *(serve)* cap images per request (PR #91 follow-up)
- *(serve)* address PR #91 review — image data-URI hardening, 400s
- *(serve)* address PR #89 review — 501 for unsupported, empty-input test
- *(serve)* address PR #86 review — tool loop robustness
- *(serve)* address PR #85 review — streaming 404, finish_reason doc

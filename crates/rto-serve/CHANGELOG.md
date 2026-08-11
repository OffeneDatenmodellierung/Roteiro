# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.20](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-serve-v0.0.19...rto-serve-v0.0.20) - 2026-08-11

### Added

- *(serve)* GET /v1/projects — enumerate hosted projects (ADR-0008)
- *(serve)* /v1/{project}/… path routing pre-binds a workspace project (ADR-0008)

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

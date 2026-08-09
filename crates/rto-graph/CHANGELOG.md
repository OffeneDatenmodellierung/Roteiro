# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.14](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v0.0.13...rto-graph-v0.0.14) - 2026-08-09

### Added

- *(models)* coding/reasoning generative models + role label (Stage 20)

### Fixed

- address PR #99 review — v1.2 refs, dedup vlm doc, reject non-embedding model
- *(models)* address PR #93 review — tokenizer.json + deterministic default

### Other

- remove candle — unify the whole inference core on llama.cpp (ADR-0003 v1.2)
- Merge branch 'main' into feat/stage20-models

## [0.0.13](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v0.0.12...rto-graph-v0.0.13) - 2026-08-09

### Added

- *(serve)* vision serving — multimodal /v1/chat/completions via llama.cpp mtmd (ADR-0006)
- *(serve)* /v1/embeddings from GGUF via llama.cpp (ADR-0006, completes Stage 19)
- *(config)* wire [ingest] content toggles into sync (ADR-0007)
- *(generator)* Qwen3 support + curated generative tiers (low/mid/high)
- *(models)* opinionated model matrix — resource tiers per section
- *(extract)* Tier A image OCR via ocrs/rten (Stage 12, ADR-0005)
- *(context)* dependency-aware per-node context cache (Stage 12)
- *(infer)* semantic + structural duplicate detection (Stage 12)
- *(extract)* ingest PDF text into meta.content (Stage 12, feature-gated)
- *(infer)* embed real content, not just names (Stage 12 content ingestion)
- *(spec)* `roteiro spec draft` — Tier 1 offline local-model drafting (completes Stage 13)
- *(spec)* `roteiro spec context` — graph-grounded authoring context (Stage 13, Tier 0)

### Fixed

- *(models)* PR #75 review — clean up the partial file on any download error
- PR #72 review — clearer arch match, generic error doc, tier-consistent desc
- *(models)* PR #71 review — pin SHA-256 for the bge embedding files
- *(extract)* PR #69 review — host-variant env tag, borrow in OCR guard, docs
- *(context)* PR #64 review — exact confidence in fingerprint; lighter refresh; doc link
- *(extract)* PR #62 review — case-insensitive extension dispatch
- *(extract)* PR #61 review — O(n) content cap; robust `/**/` doc comment
- *(spec)* PR #60 review — require an EOS token; clear KV cache defensively
- *(spec)* use a range loop for the decode counter (CI clippy 1.97)
- *(spec)* PR #53 review — short-circuit limit==0; lock in clean scaffold output
- *(query)* PR #52 review — `::`-only tokenizer, node-only search scan

### Other

- address PR #82 review — precise env_tag + missing-feature wording
- Merge branch 'main' into feat/streaming-downloads
- *(models)* stream model downloads to disk instead of buffering in memory
- *(models)* decouple the model registry/pull from the candle feature
- *(infer)* PR #63 review — bound dedup memory to O(limit); skip needless sims

## [0.0.12](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v0.0.11...rto-graph-v0.0.12) - 2026-08-09

### Added

- *(oracle)* codegraph validation oracle — `import --from codegraph` (Stage 11, 3/3)
- *(import)* durable import layers surviving code-changing syncs (Stage 11)
- *(debt)* inline `roteiro:ignore` / `roteiro:ignore-file` opt-out directives
- *(debt)* intent-debt tracking — marker nodes + `roteiro debt` (Stage 15)
- *(rto-spec)* Graphify importer — `roteiro import --from graphify` (Stage 9)
- inference-local-models tier — candle embedder + model registry (Stage 8)

### Fixed

- *(oracle)* PR #46 review — propagate DB errors, count-based scope diff, both samples
- *(import)* validate import layers on import and on sync; PR review
- *(debt)* PR review — anchor to first non-ws byte, mixed-case tags, stronger test
- address PR #33 review — model validation, checksum warning, tests

## [0.0.11](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v0.0.10...rto-graph-v0.0.11) - 2026-08-08

### Added

- *(rto-graph)* offline inference layer — `roteiro infer` (Stage 8 core)

### Fixed

- *(infer)* address PR #31 review — authoritative re-infer, stem, perf, docs

## [0.0.10](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v0.0.9...rto-graph-v0.0.10) - 2026-08-08

### Added

- *(rto-graph)* portable graph artifacts — `export`/`load` (Stage 10 part 1)

### Fixed

- *(rto-graph)* address PR #27 review — tree-less artifacts + determinism test

## [0.0.9](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v0.0.8...rto-graph-v0.0.9) - 2026-08-08

### Added

- *(query)* `roteiro path` + MCP `path` tool (Stage 5/7 follow-up)

### Fixed

- address PR #25 review — path invariant, stderr, tool description

## [0.0.7](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v0.0.6...rto-graph-v0.0.7) - 2026-08-08

### Added

- *(rto-render)* real docs-site + Obsidian renderers (Stage 6)

## [0.0.6](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v0.0.5...rto-graph-v0.0.6) - 2026-08-08

### Added

- query surface with stable --json schema (Stage 5, part 1)
- *(rto-spec)* authored ADR layer and `roteiro check` (Stage 4)

### Fixed

- address PR #17 review — code-span runs and deterministic edge order

### Other

- Merge pull request #15 from OffeneDatenmodellierung/release-plz-2026-08-08T09-17-25Z
- Merge remote-tracking branch 'origin/main' into feat/authored-layer

## [0.0.5](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v0.0.4...rto-graph-v0.0.5) - 2026-08-08

### Added

- *(rto-graph)* uncommitted working-tree dirty overlay (Stage 3)

## [0.0.4](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v0.0.3...rto-graph-v0.0.4) - 2026-08-08

### Added

- *(rto-graph)* derived tree-sitter Rust extraction (Stage 3)

## [0.0.3](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v0.0.2...rto-graph-v0.0.3) - 2026-08-08

### Added

- *(rto-graph)* content-addressed cache and `roteiro sync` (Stage 2)

### Fixed

- *(rto-graph)* fix read_blob build + cache path-collision (PR #9 review)
- add missing closing braces to read_blob and impl Repo block

### Other

- Potential fix for pull request finding
- Potential fix for pull request finding
- Potential fix for pull request finding

## [0.0.2](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v0.0.1...rto-graph-v0.0.2) - 2026-08-07

### Added

- *(rto-graph)* implement graph core (Stage 1)

### Fixed

- *(rto-graph)* address PR review — confidence range + deterministic neighbors

## [0.0.1](https://github.com/OffeneDatenmodellierung/Roteiro/releases/tag/rto-graph-v0.0.1) - 2026-08-07

### Added

- Initial commit

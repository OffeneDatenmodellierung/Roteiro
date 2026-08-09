# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.12](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v0.0.11...roteiro-v0.0.12) - 2026-08-09

### Added

- *(import)* durable import layers surviving code-changing syncs (Stage 11)
- *(debt)* intent-debt tracking — marker nodes + `roteiro debt` (Stage 15)
- *(rto-spec)* Graphify importer — `roteiro import --from graphify` (Stage 9)
- inference-local-models tier — candle embedder + model registry (Stage 8)

### Fixed

- *(import)* validate import layers on import and on sync; PR review
- *(render)* resolve [[wiki-links]] and publish the Build Plan page
- *(clippy)* backtick BUILD_PLAN path in import doc comment
- address PR #33 review — model validation, checksum warning, tests

### Other

- make Graphify import durability explicit (PR #35 review)

## [0.0.11](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v0.0.10...roteiro-v0.0.11) - 2026-08-08

### Added

- *(rto-graph)* offline inference layer — `roteiro infer` (Stage 8 core)

### Fixed

- *(infer)* address PR #31 review — authoritative re-infer, stem, perf, docs

## [0.0.10](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v0.0.9...roteiro-v0.0.10) - 2026-08-08

### Added

- *(rto-graph)* portable graph artifacts — `export`/`load` (Stage 10 part 1)

## [0.0.9](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v0.0.8...roteiro-v0.0.9) - 2026-08-08

### Added

- *(query)* `roteiro path` + MCP `path` tool (Stage 5/7 follow-up)

### Fixed

- address PR #25 review — path invariant, stderr, tool description

## [0.0.8](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v0.0.7...roteiro-v0.0.8) - 2026-08-08

### Added

- *(mcp)* adopt rmcp for stdio + networked HTTP serving (ADR-0002)
- *(rto-render)* MCP server over stdio, feature-gated (Stage 7)

## [0.0.7](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v0.0.6...roteiro-v0.0.7) - 2026-08-08

### Added

- *(rto-render)* real docs-site + Obsidian renderers (Stage 6)

## [0.0.6](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v0.0.5...roteiro-v0.0.6) - 2026-08-08

### Added

- *(roteiro)* `init` + git hooks + AGENTS.md (Stage 5, part 2)
- query surface with stable --json schema (Stage 5, part 1)
- *(rto-spec)* authored ADR layer and `roteiro check` (Stage 4)

### Fixed

- *(rto-spec)* address PR #14 review — fail on malformed ADRs, fix stale doc

### Other

- Merge pull request #15 from OffeneDatenmodellierung/release-plz-2026-08-08T09-17-25Z
- Merge remote-tracking branch 'origin/main' into feat/authored-layer

## [0.0.5](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v0.0.4...roteiro-v0.0.5) - 2026-08-08

### Added

- *(rto-graph)* uncommitted working-tree dirty overlay (Stage 3)

## [0.0.4](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v0.0.3...roteiro-v0.0.4) - 2026-08-08

### Added

- *(rto-graph)* derived tree-sitter Rust extraction (Stage 3)

## [0.0.3](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v0.0.2...roteiro-v0.0.3) - 2026-08-08

### Added

- *(rto-graph)* content-addressed cache and `roteiro sync` (Stage 2)

## [0.0.2](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v0.0.1...roteiro-v0.0.2) - 2026-08-07

### Added

- *(rto-graph)* implement graph core (Stage 1)

## [0.0.1](https://github.com/OffeneDatenmodellierung/Roteiro/releases/tag/roteiro-v0.0.1) - 2026-08-07

### Added

- Initial commit

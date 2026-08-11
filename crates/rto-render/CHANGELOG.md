# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.20](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-render-v0.0.19...rto-render-v0.0.20) - 2026-08-11

### Added

- *(serve)* multi-repo workspace serve — one instance, many graphs (ADR-0008)

### Other

- Merge pull request #189 from OffeneDatenmodellierung/feat/workspace-serve

## [0.0.19](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-render-v0.0.18...rto-render-v0.0.19) - 2026-08-11

### Added

- *(query)* search captured content + rank curated/overview above test symbols

## [0.0.18](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-render-v0.0.17...rto-render-v0.0.18) - 2026-08-11

### Other

- *(debt)* precision pass — intent-debt now reflects real debt (97 → 6)

## [0.0.14](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-render-v0.0.13...rto-render-v0.0.14) - 2026-08-10

### Added

- *(config)* wire [debt] ignore-paths and [paths] model_store
- *(render)* useful Obsidian vault — home overview, content, status, tags (Stage 14)

### Fixed

- address PR #119 review — propagate store errors + accurate tag docs

### Other

- Merge branch 'main' into feat/obsidian-vault
- remove duplicated helpers (Stage 14 health check)

## [0.0.13](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-render-v0.0.12...rto-render-v0.0.13) - 2026-08-09

### Fixed

- *(docs)* link .ico + apple-touch-icon favicons in rendered pages
- *(render)* rewrite `[…](*.md)` links to their rendered `.html` targets

### Other

- Add favicon assertions to root-level render_doc test
- Potential fix for pull request finding

## [0.0.12](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-render-v0.0.11...rto-render-v0.0.12) - 2026-08-09

### Added

- *(debt)* intent-debt tracking — marker nodes + `roteiro debt` (Stage 15)

### Fixed

- *(render)* honour multi-backtick code spans in wiki-link rewrite
- *(render)* resolve [[wiki-links]] and publish the Build Plan page

## [0.0.9](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-render-v0.0.8...rto-render-v0.0.9) - 2026-08-08

### Added

- *(query)* `roteiro path` + MCP `path` tool (Stage 5/7 follow-up)

### Fixed

- address PR #25 review — path invariant, stderr, tool description

## [0.0.8](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-render-v0.0.7...rto-render-v0.0.8) - 2026-08-08

### Added

- *(mcp)* adopt rmcp for stdio + networked HTTP serving (ADR-0002)
- *(rto-render)* MCP server over stdio, feature-gated (Stage 7)

## [0.0.7](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-render-v0.0.6...rto-render-v0.0.7) - 2026-08-08

### Added

- *(rto-render)* real docs-site + Obsidian renderers (Stage 6)

## [0.0.1](https://github.com/OffeneDatenmodellierung/Roteiro/releases/tag/rto-render-v0.0.1) - 2026-08-07

### Added

- Initial commit

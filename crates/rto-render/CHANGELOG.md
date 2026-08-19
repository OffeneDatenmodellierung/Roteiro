# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.26.4](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-render-v1.26.3...rto-render-v1.26.4) - 2026-08-19

### Fixed

- *(render)* resolve every local link the docs site serves (#456, #457, #508)

## [1.26.1](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-render-v1.26.0...rto-render-v1.26.1) - 2026-08-19

### Other

- two standing decisions, so they stop arriving as escalations

## [1.25.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-render-v1.24.0...rto-render-v1.25.0) - 2026-08-19

### Added

- *(mcp)* expose the two read-only `security` subcommands, scoped and bounded

### Fixed

- *(security)* `ready` must mean ready, not "its assets are provisioned"

### Other

- Merge pull request #468 from OffeneDatenmodellierung/feat/mcp-security-list-status

## [1.24.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-render-v1.23.0...rto-render-v1.24.0) - 2026-08-19

### Added

- *(website)* the landing page's content becomes rendered pages
- *(check)* the website becomes a document class the gate can see

## [1.23.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-render-v1.22.0...rto-render-v1.23.0) - 2026-08-18

### Fixed

- *(review)* apply `[debt] ignore` by taking the marker set from `debt`

## [1.21.1](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-render-v1.21.0...rto-render-v1.21.1) - 2026-08-18

### Fixed

- *(query)* search reads `limit = 0` as unlimited, per channel, via `window`

### Other

- *(cli,mcp)* say what `limit` means on every surface that describes it

## [1.16.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-render-v1.15.0...rto-render-v1.16.0) - 2026-08-17

### Added

- *(cli)* `roteiro debt-density`, and the five other surfaces

### Fixed

- *(render)* `_Home` scopes intent debt by `[debt] ignore`, both tables

## [1.13.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-render-v1.12.0...rto-render-v1.13.0) - 2026-08-16

### Added

- *(render)* coupling in the Obsidian `_Home` overview
- *(surfaces)* coupling on the graph API and both tool registries

## [1.1.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-render-v1.0.0...rto-render-v1.1.0) - 2026-08-13

### Added

- *(serve)* follow cross-repo links in the served tools (ADR-0009)

### Other

- *(serve)* address PR #210 review

## [1.0.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-render-v0.0.19...rto-render-v1.0.0) - 2026-08-11

First stable release — the public API is now covered by SemVer; breaking changes will bump the major version.

### Added
- The MCP server is **`Workspace`-backed** (ADR-0008): every tool takes an optional
  `project` selector and a `list_projects` tool enumerates hosted projects. `mcp_router`
  exposes the `/mcp` axum router for mounting alongside `/v1` on one port.


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

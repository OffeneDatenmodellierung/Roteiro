# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [4.1.1](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-render-v4.1.0...rto-render-v4.1.1) - 2026-09-01

### Fixed

- *(render)* the space guard's own diagnostic could panic on a multi-byte character
- *(mcp)* give the MCP surface the read-the-content rule the served turn has

### Other

- *(render)* pin that no advertised description carries a run of spaces
- *(render)* cut 1,709 bytes of advertised tool prose ([#675](https://github.com/OffeneDatenmodellierung/Roteiro/pull/675))

## [4.1.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-render-v4.0.0...rto-render-v4.1.0) - 2026-09-01

### Fixed

- *(spec)* two headings claiming one id, and one of them lost

## [4.0.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-render-v3.3.0...rto-render-v4.0.0) - 2026-08-29

### Added

- *(okf)* [**breaking**] delete the Obsidian vault renderer
- *(okf)* nest a workspace bundle by member
- *(okf)* render the bundle, and cap a slug the filesystem would refuse
- *(okf)* assemble a whole bundle, and settle slug collisions once
- *(okf)* the concept, index and log emitters
- *(mcp)* a session should not pay for tools it will never call

### Fixed

- *(okf)* a cross-repo link landed on the stub standing in for its target
- *(okf)* a title could write its own `verified` block
- *(okf)* a link resolved by guesswork, and a review nobody did
- *(mcp)* the class report explained two of the five states it emits
- *(mcp)* a tool this build never had was not withheld from anyone

### Other

- *(okf)* a member directory carries no index, and three pages said it did
- *(okf)* say why `Actor` is deliberately exhaustive
- *(serve)* resolve the tool selection once, not once per predicate call

## [3.2.1](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-render-v3.2.0...rto-render-v3.2.1) - 2026-08-27

### Fixed

- *(render)* one path through `for_tool`, so no placeholder can escape

### Other

- *(render)* one definition per tool description, not two
- *(render)* give each accessor back its own doc, and fix the usage line
- *(serve)* one authority for a shared tool description, and a measure of it
- link the community Discord from the site and the README

## [3.2.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-render-v3.1.0...rto-render-v3.2.0) - 2026-08-26

### Added

- *(render)* the manifest says what each member deploys, and against which hub

### Fixed

- *(render)* the caption agrees with its count, and the doc with its code

## [3.1.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-render-v3.0.0...rto-render-v3.1.0) - 2026-08-26

### Other

- Merge pull request #627 from OffeneDatenmodellierung/dependabot/cargo/yaml-rust2-0.12.0
- Merge branch 'main' into fix/524-heading-id-is-one-rule

## [3.0.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-render-v2.3.0...rto-render-v3.0.0) - 2026-08-22

### Added

- *(render)* the workspace vault carries findings, settings and a manifest

### Fixed

- *(spec)* [**breaking**] a heading's id is one rule, honoured by the graph and the renderer
- *(render)* the manifest is a table too, and a bare URL still linkifies
- *(render)* there are three rendering contexts here, not one
- *(render)* quote every analyzer field, and stop the manifest overpromising

### Other

- *(render)* the vault stopped arguing against its own new section

## [2.3.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-render-v2.2.0...rto-render-v2.3.0) - 2026-08-22

### Added

- *(links)* persist authored [[links]], making the `authored → gold` path reachable

### Fixed

- *(render)* saturate the inferred count, and stop showing literal asterisks
- *(render)* count declared links before the cap, and unbreak default-features clippy
- *(links)* refuse the hub flags too, count pruned edges, and test the vault

## [2.2.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-render-v2.1.1...rto-render-v2.2.0) - 2026-08-22

### Fixed

- *(cli)* address review — name the flag, not the token, and stop pinning prose
- *(cli)* read the working tree, and stop a read rewriting the store

## [2.1.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-render-v2.0.2...rto-render-v2.1.0) - 2026-08-22

### Added

- *(mcp)* let the operator restrict the advertised tool surface (--tools)

## [2.0.1](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-render-v2.0.0...rto-render-v2.0.1) - 2026-08-21

### Fixed

- *(render)* name a key the vault holds, and fail the guard on what it cannot read

### Other

- *(render)* describe the note-name rule, not the pre-#574 spelling

## [2.0.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-render-v1.30.0...rto-render-v2.0.0) - 2026-08-21

### Fixed

- *(render)* [**breaking**] make note_name injective under filename case folding

### Other

- Merge remote-tracking branch 'origin/main' into fix/574-lossless-note-names

## [1.30.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-render-v1.29.0...rto-render-v1.30.0) - 2026-08-21

### Added

- *(render)* render an Obsidian vault for a whole workspace (#442 part 1)

### Fixed

- *(render)* one YAML escaping rule for every frontmatter field

### Other

- *(render)* the qualified key and the note name are two strings

## [1.29.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-render-v1.28.0...rto-render-v1.29.0) - 2026-08-20

### Fixed

- *(lint)* unbreak `main` under the clippy that ships with Rust 1.98

## [1.27.1](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-render-v1.27.0...rto-render-v1.27.1) - 2026-08-20

### Fixed

- *(render)* a prose note in the vault is the document, not 6% of it

## [1.26.6](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-render-v1.26.5...rto-render-v1.26.6) - 2026-08-20

### Other

- drop the possessive from the two landing-page bar test names
- Merge pull request #531 from OffeneDatenmodellierung/chore/441-possessive-test-names
- drop the apostropheless possessives from twelve test names

## [1.26.5](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-render-v1.26.4...rto-render-v1.26.5) - 2026-08-19

### Other

- *(render)* parse with the shared Markdown dialect, and pin that it stays shared

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

# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [5.2.1](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-spec-v5.2.0...rto-spec-v5.2.1) - 2026-09-02

### Fixed

- *(docs)* four review findings on the archived-plans move

### Other

- four more review findings — three stale claims and a tense slip
- serve the archived build plans from /history/, matching the repo
- archive the build plans to docs/history/ (WIP — one open question)

## [4.1.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-spec-v4.0.0...rto-spec-v4.1.0) - 2026-09-01

### Fixed

- *(spec)* a blueprint is rendered, and the comment said otherwise
- *(spec)* two headings claiming one id, and one of them lost

## [4.0.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-spec-v3.3.0...rto-spec-v4.0.0) - 2026-08-29

### Added

- *(check)* [**breaking**] a lossy conversion feeding a hash is drift, and fix the one we had

### Fixed

- *(init)* [**breaking**] the hook this PR installs invoked a command this PR deleted
- *(test)* prove the safecrlf fixture reproduces, and correct a stale doc
- *(check)* staging a file no longer hides its drift

### Other

- *(check)* the counts justifying this rule were wrong, and now measured
- *(check)* the module claimed nothing enforces what it enforces
- *(check)* the rule's own doc named a conversion it does not scan
- *(spec)* describe the old union without tripping the debt detector

## [3.2.1](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-spec-v3.2.0...rto-spec-v3.2.1) - 2026-08-27

### Fixed

- *(spec)* a link written inside a heading belongs to that heading
- *(spec)* a heading is not a line beginning with `## `

## [3.1.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-spec-v3.0.0...rto-spec-v3.1.0) - 2026-08-26

### Fixed

- *(spec)* a section's title is its text, not the line that declared it
- *(spec)* extend the shared heading rule to ADRs and blueprints

### Other

- Merge branch 'main' into fix/524-heading-id-is-one-rule
- *(spec)* record what the no-id fallback changed, and measure it
- Merge branch 'main' into fix/524-heading-id-is-one-rule

## [3.0.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-spec-v2.3.0...rto-spec-v3.0.0) - 2026-08-22

### Added

- *(spec)* [**breaking**] check that an `#[allow(…)]` carries a justification

### Other

- Merge pull request #622 from OffeneDatenmodellierung/feat/438-justify-your-allow

## [2.2.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-spec-v2.1.1...rto-spec-v2.2.0) - 2026-08-22

### Added

- *(spec)* ADR version rules 4 and 5 — frontmatter vs history, and last-modified

### Fixed

- *(spec)* make DocDate::parse exactly YYYY-MM-DD, as its doc already claimed

## [1.27.1](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-spec-v1.27.0...rto-spec-v1.27.1) - 2026-08-20

### Fixed

- *(spec)* a span keeps its indentation — trim blank lines, not whitespace
- *(spec)* an ADR note in the vault carries the decision it names

### Other

- *(spec,render)* pin the split — a section note is its own section

## [1.26.6](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-spec-v1.26.5...rto-spec-v1.26.6) - 2026-08-20

### Other

- drop the apostropheless possessives from twelve test names

## [1.26.4](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-spec-v1.26.3...rto-spec-v1.26.4) - 2026-08-19

### Fixed

- *(spec)* read a heading with the parser, not a line scan ([#469](https://github.com/OffeneDatenmodellierung/Roteiro/pull/469))

## [1.26.1](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-spec-v1.26.0...rto-spec-v1.26.1) - 2026-08-19

### Other

- two standing decisions, so they stop arriving as escalations

## [1.24.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-spec-v1.23.0...rto-spec-v1.24.0) - 2026-08-19

### Added

- *(check)* the website becomes a document class the gate can see

## [1.23.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-spec-v1.22.0...rto-spec-v1.23.0) - 2026-08-18

### Added

- *(check)* gate ADR version metadata against itself

### Other

- *(check)* record what the fourth and fifth rules will cost

## [1.12.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-spec-v1.11.0...rto-spec-v1.12.0) - 2026-08-16

### Fixed

- *(check)* detect two ADRs sharing an adr-id ([#324](https://github.com/OffeneDatenmodellierung/Roteiro/pull/324))

## [1.0.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-spec-v0.0.19...rto-spec-v1.0.0) - 2026-08-11

First stable release — the public API is now covered by SemVer; breaking changes will bump the major version.

### Changed
- First stable release; no functional changes since 0.0.19.


## [0.0.18](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-spec-v0.0.17...rto-spec-v0.0.18) - 2026-08-11

### Other

- *(debt)* precision pass — intent-debt now reflects real debt (97 → 6)

## [0.0.15](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-spec-v0.0.14...rto-spec-v0.0.15) - 2026-08-10

### Added

- *(store)* tag nodes with a provenance layer

## [0.0.14](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-spec-v0.0.13...rto-spec-v0.0.14) - 2026-08-10

### Added

- *(lat)* import @lat: source backlinks as authored edges

### Fixed

- address PR #108 review — lang_for case-normalization + cap doc

### Other

- fix crate README wording (PR #117 review follow-up)
- *(crates)* per-crate crates.io READMEs + refresh root README (Stage 14)
- remove duplicated helpers (Stage 14 health check)

## [0.0.13](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-spec-v0.0.12...rto-spec-v0.0.13) - 2026-08-09

### Added

- *(spec)* `roteiro spec draft` — Tier 1 offline local-model drafting (completes Stage 13)
- *(spec)* blueprint scaffold kind — `spec scaffold --kind blueprint`
- *(spec)* `roteiro spec scaffold` — grounded, check-clean ADR skeletons (Stage 13, Tier 0)
- *(spec)* `roteiro spec context` — graph-grounded authoring context (Stage 13, Tier 0)

### Fixed

- *(spec)* PR #53 review — short-circuit limit==0; lock in clean scaffold output

## [0.0.12](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-spec-v0.0.11...rto-spec-v0.0.12) - 2026-08-09

### Added

- *(import)* lat.md importer — authored layer over the code graph (Stage 11, 2/3)
- *(rto-spec)* Graphify importer — `roteiro import --from graphify` (Stage 9)

### Fixed

- *(import)* PR #44 review round 2 — symlink-safe walk, docs, sort
- *(import)* stamp lat edges with LAT_REF; reject lat files outside the repo
- *(rto-spec)* namespace Graphify hyperedge groups (PR #35 review)
- *(rto-spec)* address PR #35 review — import doc + node-kind token

## [0.0.6](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-spec-v0.0.5...rto-spec-v0.0.6) - 2026-08-08

### Added

- query surface with stable --json schema (Stage 5, part 1)
- *(rto-spec)* authored ADR layer and `roteiro check` (Stage 4)

### Fixed

- address PR #17 review — code-span runs and deterministic edge order
- *(rto-spec)* address PR #14 review — fail on malformed ADRs, fix stale doc

## [0.0.1](https://github.com/OffeneDatenmodellierung/Roteiro/releases/tag/rto-spec-v0.0.1) - 2026-08-07

### Added

- Initial commit

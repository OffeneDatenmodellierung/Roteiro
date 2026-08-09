# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.13](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-spec-v0.0.12...rto-spec-v0.0.13) - 2026-08-09

### Added

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

# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [4.0.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-exec-v3.3.0...rto-exec-v4.0.0) - 2026-08-29

### Added

- *(check)* [**breaking**] a lossy conversion feeding a hash is drift, and fix the one we had

## [1.29.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-exec-v1.28.0...rto-exec-v1.29.0) - 2026-08-20

### Fixed

- *(lint)* unbreak `main` under the clippy that ships with Rust 1.98

## [1.27.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-exec-v1.26.6...rto-exec-v1.27.0) - 2026-08-20

### Added

- *(sandbox)* user-supplied analyzer images, digest-pinned

### Fixed

- *(sandbox)* a pin refusal says what is wrong with the reference it got

## [1.26.6](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-exec-v1.26.5...rto-exec-v1.26.6) - 2026-08-20

### Other

- drop the apostropheless possessives from twelve test names

## [1.26.3](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-exec-v1.26.2...rto-exec-v1.26.3) - 2026-08-19

### Fixed

- one convention for a hint's upstream page, and a doc claim narrowed to what holds
- a missing analyzer refusal says how to install it

## [1.26.1](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-exec-v1.26.0...rto-exec-v1.26.1) - 2026-08-19

### Other

- two standing decisions, so they stop arriving as escalations

## [1.25.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-exec-v1.24.0...rto-exec-v1.25.0) - 2026-08-19

### Added

- *(mcp)* expose the two read-only `security` subcommands, scoped and bounded

### Fixed

- *(security)* the cross-reference guard the model-facing document claimed to have
- *(security)* `ready` must mean ready, not "its assets are provisioned"

### Other

- Merge pull request #468 from OffeneDatenmodellierung/feat/mcp-security-list-status

## [1.24.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-exec-v1.23.0...rto-exec-v1.24.0) - 2026-08-19

### Added

- *(lint)* the sandboxed builder, so sandbox-by-default has something to select
- *(lint)* sandboxed by default; host execution is opt-in and layered
- *(lint)* report clippy at a point in time, and store none of it

### Fixed

- *(lint)* build refusals from lines, and test the default feature set
- *(lint)* refuse a relative scratch root; snapshot contents, not names
- *(lint)* set the scratch target dir rather than inheriting it

## [1.15.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-exec-v1.14.0...rto-exec-v1.15.0) - 2026-08-17

### Added

- *(exec)* verify the runtime boxlite extracted, not just the archive

### Fixed

- *(exec)* percent-encode the file:// URLs this prints, since curl decodes them
- *(exec)* format, and correct the feature-gating claim the message relies on
- *(exec)* look in the asset cache before demanding BOXLITE_RUNTIME_URL

### Other

- *(exec)* record the runtime-verification trade, and fix the stale selection rule

## [1.14.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-exec-v1.13.0...rto-exec-v1.14.0) - 2026-08-17

### Fixed

- *(tests)* make the default feature set compile clean under -D warnings

### Other

- Merge pull request #364 from OffeneDatenmodellierung/fix/default-feature-set-gate
- Merge pull request #359 from OffeneDatenmodellierung/feat/models-default-feature
- *(features)* UNREVIEWED checkpoint - exec-subprocess default + prefetch/status move

## [1.13.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-exec-v1.12.0...rto-exec-v1.13.0) - 2026-08-16

### Added

- *(exec)* make the sandboxed backend actually run, and prove parity
- *(exec)* govern the boxlite runtime fetch and add the sandboxed backend

### Fixed

- *(exec)* review fixes — fixture race, download timeout, honest gate docs

### Other

- *(stage24)* record what shipped, and what the stage turned out to be about
- *(exec)* pin the archive contract and audit dependency build scripts

## [1.12.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-exec-v1.11.0...rto-exec-v1.12.0) - 2026-08-16

### Added

- *(rto-exec)* cross-reference dependency findings across analyzers
- *(rto-exec)* osv-scanner adapter and a download-by-URL asset source

### Fixed

- *(security)* refuse an asset body whose completeness cannot be established

### Other

- *(crossref)* make the two-versions test actually guard the version bucket
- cover the two guarded behaviours that had no test
- *(rto-exec)* the dependency axis, over real output from both tools

## [1.11.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-exec-v1.10.1...rto-exec-v1.11.0) - 2026-08-16

### Added

- *(cli)* security run / prefetch / status, and native-output ingest
- *(exec)* asset provisioning and the subprocess backend
- *(exec)* vendored semgrep rules, polyglot fixtures, and a snippet source
- *(exec)* the adapter seam, with semgrep and cargo-audit

### Other

- renumber the coverage ADR to 0018, and record what Stage 22 shipped
- *(exec)* bring the crate README up to what the crate now does
- *(exec)* annotate the adapters with the coverage ADR
- *(adr)* ADR-0016 — the analyzer coverage matrix, with its evidence
- *(exec)* the equivalence, coverage and offline contracts

## [1.10.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-exec-v1.9.0...rto-exec-v1.10.0) - 2026-08-15

### Added

- *(exec)* add AnalyzerRunner contract and security ingest

### Fixed

- *(findings)* state the full analyzer-id contract in rejection errors

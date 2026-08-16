# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [2.0.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-exec-v1.11.0...rto-exec-v2.0.0) - 2026-08-16

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

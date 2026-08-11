# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.18](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-llama-v0.0.17...rto-llama-v0.0.18) - 2026-08-11

### Other

- *(debt)* precision pass — intent-debt now reflects real debt (97 → 6)

## [0.0.16](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-llama-v0.0.15...rto-llama-v0.0.16) - 2026-08-11

### Added

- *(audio)* transcribe audio into the graph via llama.cpp mtmd ([#18](https://github.com/OffeneDatenmodellierung/Roteiro/pull/18))
- *(llama)* per-model concurrency — release cache lock before decode ([#18](https://github.com/OffeneDatenmodellierung/Roteiro/pull/18))

### Fixed

- *(audio)* address PR #161 review — reject dual media; fix stale Ultravox docs

### Other

- Merge pull request #161 from OffeneDatenmodellierung/feat/audio-ingestion

## [0.0.14](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-llama-v0.0.13...rto-llama-v0.0.14) - 2026-08-10

### Fixed

- address PR #107 review — surface GGUF stat failure + doc wording

### Other

- *(crates)* per-crate crates.io READMEs + refresh root README (Stage 14)
- *(llama)* reuse embedding context + memory-bounded model residency

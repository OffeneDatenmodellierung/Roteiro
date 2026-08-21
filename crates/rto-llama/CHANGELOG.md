# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [2.0.1](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-llama-v2.0.0...rto-llama-v2.0.1) - 2026-08-21

### Other

- *(llama)* fix subject-verb agreement in the measurement caveat
- *(llama)* scope the context-cost measurement, and settle n_ubatch

## [1.26.6](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-llama-v1.26.5...rto-llama-v1.26.6) - 2026-08-20

### Other

- drop the apostropheless possessives from twelve test names

## [1.26.1](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-llama-v1.26.0...rto-llama-v1.26.1) - 2026-08-19

### Other

- Merge pull request #510 from OffeneDatenmodellierung/fix/483-llama-dev-dep
- stop building llama.cpp at every feature set

## [1.26.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-llama-v1.25.0...rto-llama-v1.26.0) - 2026-08-19

### Added

- *(llama)* size the context window per request, per model

### Fixed

- *(config)* render `max_context_tokens`'s `0` as its meaning, not as `0`

### Other

- Potential fix for pull request finding

## [1.13.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-llama-v1.12.0...rto-llama-v1.13.0) - 2026-08-16

### Fixed

- *(llama)* derive the media chunk width from the context, not a literal
- *(llama)* refuse over-long prompts instead of aborting the process

### Other

- *(llama,serve)* pin the batch guard, and measure what the wider batch costs

## [1.11.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-llama-v1.10.1...rto-llama-v1.11.0) - 2026-08-16

### Added

- *(llama)* use a split MTP draft head installed beside the model
- *(llama)* MTP speculative decoding over a model's own draft head

### Other

- Potential fix for pull request finding
- *(speculative)* attribute the argmax flips to the test that actually saw them
- *(speculative)* stop asserting an identity the feature does not have
- *(speculative)* checkpoint the draft-head skip guard — NOT reviewed, amend freely
- *(speculative)* the module header claimed an invariant the measurements refute
- *(llama)* cargo fmt --all
- *(speculative)* checkpoint after transport drop — NOT reviewed, amend freely
- *(llama)* measure llama.cpp's batch-width numerics, and probe MTP end to end
- *(llama)* cache the mtmd projector per model, not per blob

## [1.10.1](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-llama-v1.10.0...rto-llama-v1.10.1) - 2026-08-15

### Fixed

- *(llama)* share one backend per process

## [1.10.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-llama-v1.9.0...rto-llama-v1.10.0) - 2026-08-15

### Fixed

- *(extract)* drop cached vision/ASR engines before exit

## [1.7.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-llama-v1.6.0...rto-llama-v1.7.0) - 2026-08-14

### Fixed

- *(serve)* acquire per-model gen_lock before the encoder-only guard
- *(serve)* never run chat on an embedding model (server hard-crash)

## [1.6.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-llama-v1.5.0...rto-llama-v1.6.0) - 2026-08-14

### Added

- *(telemetry)* route native llama.cpp + ggml logs through tracing

### Other

- Merge remote-tracking branch 'origin/main' into feat/file-logging-rotation

## [1.0.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-llama-v0.0.19...rto-llama-v1.0.0) - 2026-08-11

First stable release — the public API is now covered by SemVer; breaking changes will bump the major version.

### Changed
- First stable release; no functional changes since 0.0.19.


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

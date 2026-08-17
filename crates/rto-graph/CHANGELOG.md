# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.17.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v1.16.0...rto-graph-v1.17.0) - 2026-08-17

### Added

- *(models)* one resolver decides which model serves a task, and says why

## [1.16.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v1.15.0...rto-graph-v1.16.0) - 2026-08-17

### Added

- *(graph)* intent-debt density query (Q1)

## [1.14.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v1.13.0...rto-graph-v1.14.0) - 2026-08-17

### Fixed

- *(tests)* make the default feature set compile clean under -D warnings
- *(review)* name the row in every validation failure, and stop the counts drifting

### Other

- *(review)* an adjudicated corpus so a reviewer can be measured, not guessed at

## [1.13.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v1.12.0...rto-graph-v1.13.0) - 2026-08-16

### Added

- *(render)* coupling in the Obsidian `_Home` overview
- *(cli)* `roteiro coupling`, and exclude cross-language call edges
- *(graph)* directed call coupling query (Q3)

### Other

- Merge pull request #348 from OffeneDatenmodellierung/fix/store-newer-than-binary
- Potential fix for pull request finding

## [1.12.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v1.11.0...rto-graph-v1.12.0) - 2026-08-16

### Fixed

- *(migrations)* select by set membership, not > MAX(version)
- *(check)* see new ADRs on disk; stamp the tree a graph holds ([#330](https://github.com/OffeneDatenmodellierung/Roteiro/pull/330))
- *(debt)* apply each repo's own exclusions in the graph API ([#321](https://github.com/OffeneDatenmodellierung/Roteiro/pull/321))

### Other

- Merge origin/main (migrations 13 + osv-scanner) into fix/guardrails-319-321-324-330

## [1.11.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v1.10.1...rto-graph-v1.11.0) - 2026-08-16

### Added

- *(audio)* read stream shape, duration and tags as `derived` facts
- *(media)* add the pre-generation gate, recorded not silent
- *(media)* move generated content to its own artifact store

### Fixed

- *(test)* make the cache snapshot fail fast instead of silently empty
- *(memory)* unbreak main — EXTRACT_VERSION guard missed audio-metadata
- *(audio)* de-duplicate tags on `(name, value)`, not on the whole row
- *(sync)* honour the extraction identity at an unchanged tree
- *(gate)* abstain on a WAV data chunk that is not whole samples
- *(media)* close the outcome CHECK's measurement-without-reason hole
- *(media)* correct the generation counter and the status advice
- *(test)* one WAV encoder in the workspace ([#302](https://github.com/OffeneDatenmodellierung/Roteiro/pull/302))

### Other

- Merge pull request #329 from OffeneDatenmodellierung/fix/extract-version-guard
- Merge remote-tracking branch 'origin/main' into fix/extract-version-guard
- Merge origin/main into feat/stage22-analyzers
- Merge pull request #317 from OffeneDatenmodellierung/feat/stage23-agent-memory
- *(audio)* `read`'s `None` is unreadable, not "no duration"
- *(adr)* ADR-0016 v1.1 — what the implementation found
- *(audio)* the three duration cases, determinism, and placement
- *(deps)* symphonia behind `audio-metadata`, and MPL-2.0 in the allow-list
- *(media)* satisfy clippy on the two review fixes
- *(media)* assert each pre-generation gate invariant
- *(llama)* cache the mtmd projector per model, not per blob
- *(tests)* name the right constraint per direction

## [1.10.1](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v1.10.0...rto-graph-v1.10.1) - 2026-08-15

### Fixed

- *(llama)* share one backend per process

### Other

- *(extract)* justify the cast_possible_truncation allow

## [1.10.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v1.9.0...rto-graph-v1.10.0) - 2026-08-15

### Added

- *(exec)* add AnalyzerRunner contract and security ingest

### Fixed

- *(findings)* state the full analyzer-id contract in rejection errors
- *(extract)* drop cached vision/ASR engines before exit

### Other

- *(engine-slot)* describe the init lock precisely

## [1.9.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v1.8.0...rto-graph-v1.9.0) - 2026-08-14

### Added

- *(ask)* ground answers with search snippets + stricter grounding prompt
- *(serve)* scope the workspace Ask to the selected workspace

### Fixed

- *(ask)* address PR #285 Copilot review comments

## [1.4.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v1.3.0...rto-graph-v1.4.0) - 2026-08-14

### Added

- *(config)* derive config keys from typed Rust config structs
- *(links)* match config keys across camelCase/snake_case/kebab conventions

### Fixed

- *(config)* address PR #266 review — explicit value-absence + doc fixes

### Other

- Merge remote-tracking branch 'origin/main' into feat/app-config-only-filter
- Merge pull request #262 from OffeneDatenmodellierung/feat/links-canonical-key-match
- *(links)* accurately describe normalize/canonicalize splitting

## [1.2.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v1.1.0...rto-graph-v1.2.0) - 2026-08-13

### Added

- *(explorer)* follow-the-link hop — config_key→struct bridge + cross-repo jump [PR 7]
- *(config)* multi-workspace + standalone config and WorkspaceSet (links selector)
- *(explorer)* read-only /v1/graph JSON API (PR 1/5 — data foundation)
- *(models)* readable model list + Qwen3-Coder-30B-A3B registry entry

### Fixed

- *(explorer)* address PR #249 Copilot review — no-alloc kind check, narrowed struct lookup, non-null workspace, drift wording
- *(explorer)* update serve-path graph_api call site to the new signature
- *(config)* address PR #239 Copilot review (linked-name collisions, standalone invariant, deferred set build)
- *(explorer)* address PR #237 review comments

## [1.1.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v1.0.0...rto-graph-v1.1.0) - 2026-08-13

### Added

- version-pin resolution — resolve cross-repo drift at the deployed hub version (ADR-0009 step 8a)
- *(graph)* git submodule pins (ADR-0009 derived deploy-artifact extraction, part 2)
- *(graph)* derived deploy-artifact extraction — YAML/k8s config + Dockerfile pins (ADR-0009)
- *(links)* persist inferred cross-repo edges, read config keys from graph (ADR-0009 2b)
- *(graph)* config-key nodes are graph-native (ADR-0009)
- *(links)* cross-repo authored links + `roteiro links` (ADR-0009)

### Fixed

- *(links)* address PR #222 review — match hub dir name, cheaper pin resolution
- *(graph)* address PR #221 review — resolve any revspec, test path fixes
- *(graph)* address PR #218 review — index-aware submodule pins
- *(graph)* address PR #217 review — dockerfile stage alias, YAML non-scalars
- *(links)* clear stale inferred edges + CI clippy, address PR #213 review
- *(graph)* address PR #211 review — redact secrets, dedupe keys

### Other

- *(deps)* upgrade dependencies to latest MSRV-1.94-compatible
- *(deps)* bump toml 0.9 → 1.1
- *(links)* address PR #206 review

## [1.0.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v0.0.19...rto-graph-v1.0.0) - 2026-08-11

First stable release — the public API is now covered by SemVer; breaking changes will bump the major version.

### Added
- **`Workspace`** (ADR-0008): a per-repo graph registry that opens each repo's store on
  demand and caches it, with in-place reload (`reload_from`, source-validated eviction)
  and an optional first-open hook (`with_on_open`).

### Changed
- Store connections set a `busy_timeout`, so a read that lands during a concurrent
  `sync` waits briefly rather than failing with `database is locked`.


## [0.0.19](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v0.0.18...rto-graph-v0.0.19) - 2026-08-11

### Added

- *(query)* search captured content + rank curated/overview above test symbols

### Other

- *(query)* address PR #178 review — avoid content alloc, widen overview match

## [0.0.18](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v0.0.17...rto-graph-v0.0.18) - 2026-08-11

### Other

- Merge pull request #176 from OffeneDatenmodellierung/chore/debt-precision
- *(debt)* precision pass — intent-debt now reflects real debt (97 → 6)

## [0.0.17](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v0.0.16...rto-graph-v0.0.17) - 2026-08-11

### Added

- *(review)* distinguish "added" files from "modified"

### Fixed

- *(review)* address PR #169 review — open-set doc, comment, added test

### Other

- Merge pull request #169 from OffeneDatenmodellierung/feat/review-added-status

## [0.0.16](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v0.0.15...rto-graph-v0.0.16) - 2026-08-11

### Added

- *(audio)* transcribe audio into the graph via llama.cpp mtmd ([#18](https://github.com/OffeneDatenmodellierung/Roteiro/pull/18))

### Fixed

- *(audio)* address PR #161 review — reject dual media; fix stale Ultravox docs

### Other

- Merge pull request #161 from OffeneDatenmodellierung/feat/audio-ingestion

## [0.0.15](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v0.0.14...rto-graph-v0.0.15) - 2026-08-10

### Added

- *(store)* tag nodes with a provenance layer
- *(store)* edge-level delta persistence, determinism-safe

### Fixed

- *(store)* repair legacy import-node provenance on load (PR #146 review)

### Other

- Merge pull request #149 from OffeneDatenmodellierung/fix/legacy-import-provenance
- *(store)* clarify import-node provenance repair covers both cases (PR #149 review)
- Merge pull request #147 from OffeneDatenmodellierung/fix/edge-identity-collision
- Merge pull request #144 from OffeneDatenmodellierung/feat/edge-delta

## [0.0.14](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v0.0.13...rto-graph-v0.0.14) - 2026-08-10

### Added

- *(markers)* restrict prose deferral phrases to comments in code
- *(init)* honour core.hooksPath when installing managed hooks
- *(review)* CLI-first graph-grounded review of the current change (Stage 17)
- *(extract)* broad multi-language symbol extraction via tree-sitter tags
- *(models)* coding/reasoning generative models + role label (Stage 20)

### Fixed

- address PR #130 review — empty core.hooksPath = unset; hermetic test
- address PR #125 review — reconcile reads only nodes; test inferred edges
- address PR #112 review — ADR-edit drift, deterministic order, related-kind
- address PR #108 review — lang_for case-normalization + cap doc
- *(extract)* per-grammar config cache key + add SQL
- address PR #99 review — v1.2 refs, dedup vlm doc, reject non-embedding model
- *(models)* address PR #93 review — tokenizer.json + deterministic default

### Other

- Merge pull request #136 from OffeneDatenmodellierung/feat/markers-in-comments
- address PR #136 review on marker comment-gating
- Merge branch 'main' into feat/sync-index
- Merge pull request #131 from OffeneDatenmodellierung/feat/range-review
- Merge branch 'main' into feat/store-reconcile
- *(sync)* delta-persist via Store::reconcile instead of full wipe+reinsert (Stage 14)
- remove duplicated helpers (Stage 14 health check)
- remove candle — unify the whole inference core on llama.cpp (ADR-0003 v1.2)
- Merge branch 'main' into feat/stage20-models

## [0.0.13](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v0.0.12...rto-graph-v0.0.13) - 2026-08-09

### Added

- *(serve)* vision serving — multimodal /v1/chat/completions via llama.cpp mtmd (ADR-0006)
- *(serve)* /v1/embeddings from GGUF via llama.cpp (ADR-0006, completes Stage 19)
- *(config)* wire [ingest] content toggles into sync (ADR-0007)
- *(generator)* Qwen3 support + curated generative tiers (low/mid/high)
- *(models)* opinionated model matrix — resource tiers per section
- *(extract)* Tier A image OCR via ocrs/rten (Stage 12, ADR-0005)
- *(context)* dependency-aware per-node context cache (Stage 12)
- *(infer)* semantic + structural duplicate detection (Stage 12)
- *(extract)* ingest PDF text into meta.content (Stage 12, feature-gated)
- *(infer)* embed real content, not just names (Stage 12 content ingestion)
- *(spec)* `roteiro spec draft` — Tier 1 offline local-model drafting (completes Stage 13)
- *(spec)* `roteiro spec context` — graph-grounded authoring context (Stage 13, Tier 0)

### Fixed

- *(models)* PR #75 review — clean up the partial file on any download error
- PR #72 review — clearer arch match, generic error doc, tier-consistent desc
- *(models)* PR #71 review — pin SHA-256 for the bge embedding files
- *(extract)* PR #69 review — host-variant env tag, borrow in OCR guard, docs
- *(context)* PR #64 review — exact confidence in fingerprint; lighter refresh; doc link
- *(extract)* PR #62 review — case-insensitive extension dispatch
- *(extract)* PR #61 review — O(n) content cap; robust `/**/` doc comment
- *(spec)* PR #60 review — require an EOS token; clear KV cache defensively
- *(spec)* use a range loop for the decode counter (CI clippy 1.97)
- *(spec)* PR #53 review — short-circuit limit==0; lock in clean scaffold output
- *(query)* PR #52 review — `::`-only tokenizer, node-only search scan

### Other

- address PR #82 review — precise env_tag + missing-feature wording
- Merge branch 'main' into feat/streaming-downloads
- *(models)* stream model downloads to disk instead of buffering in memory
- *(models)* decouple the model registry/pull from the candle feature
- *(infer)* PR #63 review — bound dedup memory to O(limit); skip needless sims

## [0.0.12](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v0.0.11...rto-graph-v0.0.12) - 2026-08-09

### Added

- *(oracle)* codegraph validation oracle — `import --from codegraph` (Stage 11, 3/3)
- *(import)* durable import layers surviving code-changing syncs (Stage 11)
- *(debt)* inline `roteiro:ignore` / `roteiro:ignore-file` opt-out directives
- *(debt)* intent-debt tracking — marker nodes + `roteiro debt` (Stage 15)
- *(rto-spec)* Graphify importer — `roteiro import --from graphify` (Stage 9)
- inference-local-models tier — candle embedder + model registry (Stage 8)

### Fixed

- *(oracle)* PR #46 review — propagate DB errors, count-based scope diff, both samples
- *(import)* validate import layers on import and on sync; PR review
- *(debt)* PR review — anchor to first non-ws byte, mixed-case tags, stronger test
- address PR #33 review — model validation, checksum warning, tests

## [0.0.11](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v0.0.10...rto-graph-v0.0.11) - 2026-08-08

### Added

- *(rto-graph)* offline inference layer — `roteiro infer` (Stage 8 core)

### Fixed

- *(infer)* address PR #31 review — authoritative re-infer, stem, perf, docs

## [0.0.10](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v0.0.9...rto-graph-v0.0.10) - 2026-08-08

### Added

- *(rto-graph)* portable graph artifacts — `export`/`load` (Stage 10 part 1)

### Fixed

- *(rto-graph)* address PR #27 review — tree-less artifacts + determinism test

## [0.0.9](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v0.0.8...rto-graph-v0.0.9) - 2026-08-08

### Added

- *(query)* `roteiro path` + MCP `path` tool (Stage 5/7 follow-up)

### Fixed

- address PR #25 review — path invariant, stderr, tool description

## [0.0.7](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v0.0.6...rto-graph-v0.0.7) - 2026-08-08

### Added

- *(rto-render)* real docs-site + Obsidian renderers (Stage 6)

## [0.0.6](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v0.0.5...rto-graph-v0.0.6) - 2026-08-08

### Added

- query surface with stable --json schema (Stage 5, part 1)
- *(rto-spec)* authored ADR layer and `roteiro check` (Stage 4)

### Fixed

- address PR #17 review — code-span runs and deterministic edge order

### Other

- Merge pull request #15 from OffeneDatenmodellierung/release-plz-2026-08-08T09-17-25Z
- Merge remote-tracking branch 'origin/main' into feat/authored-layer

## [0.0.5](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v0.0.4...rto-graph-v0.0.5) - 2026-08-08

### Added

- *(rto-graph)* uncommitted working-tree dirty overlay (Stage 3)

## [0.0.4](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v0.0.3...rto-graph-v0.0.4) - 2026-08-08

### Added

- *(rto-graph)* derived tree-sitter Rust extraction (Stage 3)

## [0.0.3](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v0.0.2...rto-graph-v0.0.3) - 2026-08-08

### Added

- *(rto-graph)* content-addressed cache and `roteiro sync` (Stage 2)

### Fixed

- *(rto-graph)* fix read_blob build + cache path-collision (PR #9 review)
- add missing closing braces to read_blob and impl Repo block

### Other

- Potential fix for pull request finding
- Potential fix for pull request finding
- Potential fix for pull request finding

## [0.0.2](https://github.com/OffeneDatenmodellierung/Roteiro/compare/rto-graph-v0.0.1...rto-graph-v0.0.2) - 2026-08-07

### Added

- *(rto-graph)* implement graph core (Stage 1)

### Fixed

- *(rto-graph)* address PR review — confidence range + deterministic neighbors

## [0.0.1](https://github.com/OffeneDatenmodellierung/Roteiro/releases/tag/rto-graph-v0.0.1) - 2026-08-07

### Added

- Initial commit

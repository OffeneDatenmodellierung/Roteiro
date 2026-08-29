# Roteiro — Technical Implementation Plan

_The overall project blueprint: a graph-grounded build plan for the whole system,
the sibling of the ADRs in the authoring pillar ([[docs/adr/0004-spec-blueprint-authoring-pillar.md]]).
Where the Build Plan (`docs/BUILD_PLAN.md`) tracks **stages over time**, this
blueprint describes **how the system fits together right now** — each section is
wired into the code it governs by `[[…]]` links that `roteiro check` validates
against the derived graph, so this document cannot silently drift from the
implementation._

Grounded in: [[docs/adr/0001-build-roteiro-unified-codebase-knowledge-graph.md]],
[[docs/adr/0002-adopt-rmcp-for-networked-mcp-serving.md]],
[[docs/adr/0003-pluggable-embedding-models.md]],
[[docs/adr/0004-spec-blueprint-authoring-pillar.md]],
[[docs/adr/0005-image-ocr-vision-ingestion.md]],
[[docs/adr/0006-local-model-serving.md]],
[[docs/adr/0007-configuration-file.md]].

> **Status.** Built — this reflects the shipped architecture (v0.0.x, hardening
> toward v1.0). It is a living document: it is re-validated on every `roteiro
> check`, so a link here to a symbol that no longer exists fails CI.

---

## 0. What this plan covers

Roteiro turns a git repository into one **provenance-tagged knowledge graph** and
serves it over a single query surface (CLI `--json`, MCP, and renderers). The
operator-facing surface is the `roteiro` binary
([[crates/roteiro/src/main.rs]]): `sync`, `check`, `review`, `query`, `context`,
`debt`, `path`, `infer`, `spec`, `import`, `render`, `serve`, and `model`. Every
fact carries one of three provenance classes — **derived** (a pure function of the
code), **authored** (ADRs/blueprints/`@rto:` intent), and **inferred** (embeddings,
content) — so consumers can trust each fact exactly as much as its source
(ADR-0001).

This blueprint is the map from those decisions to the crates that implement them.

## 1. Crate placement

The workspace is a small set of single-responsibility crates so the C/C++ and
model dependencies stay opt-in behind feature flags:

- [[crates/rto-graph/src/lib.rs]] — the graph core: the git reader, extraction,
  the content-addressed sync engine, the store, inference, and the registry. No
  HTTP, no inference engine; pure-Rust by default.
- [[crates/rto-spec/src/lib.rs]] — the authored layer: ADR + blueprint parsing and
  the `check` drift gate.
- [[crates/rto-llama/src/lib.rs]] — the single home of llama.cpp: the `Engine`
  trait and a llama.cpp-backed engine (generation, embeddings, multimodal),
  behind the `llama` feature (ADR-0003/0006).
- [[crates/rto-render/src/lib.rs]] — the renderers over the graph (docs site,
  OKF bundle, MCP surface).
- [[crates/rto-serve/src/lib.rs]] — the opt-in, loopback, OpenAI-compatible `/v1`
  endpoint (ADR-0006).
- [[crates/roteiro/src/main.rs]] — the CLI that wires them together.

## 2. The graph core — one provenance-tagged store

The store [[crates/rto-graph/src/store.rs#Store]] is the single SQLite-backed
graph. Every node/edge carries a `Provenance`; a sync reconciles the **derived**
layer while the authored and inferred layers are re-applied on top, so a rebuild
never loses curated intent. Imports persist as a re-appliable layer
([[crates/rto-graph/src/store.rs]]). The git facts the graph needs come through a
thin `gix` wrapper [[crates/rto-graph/src/git.rs#Repo]], kept small so all git
coupling lives in one place.

## 3. Sync — incremental, content-addressed, git-native

[[crates/rto-graph/src/sync.rs#sync]] brings the store into agreement with the
`HEAD` tree. Extraction is content-addressed by blob id via the object cache
[[crates/rto-graph/src/cache.rs]], so only changed blobs are re-extracted; an
unchanged tree is a no-op, and the incremental fast path diffs the last-synced
tree oid against `HEAD` to touch only changed paths — the git-native, content-hash
(not mtime) version of "skip unchanged subtrees." The working-tree overlay
[[crates/rto-graph/src/sync.rs#sync_worktree]] previews uncommitted edits so
`check`/`review` see work before it is committed.

## 4. Extraction & language breadth

Dispatch by extension [[crates/rto-graph/src/extract.rs#Registry]] sends Rust
through its dedicated AST walker and 15 further languages + SQL through a generic
tree-sitter *tags* extractor, emitting `defines`/`contains`/`calls`/`imports`
facts. Intent-debt markers (TODOs, stubs, deferred work) are detected during sync
[[crates/rto-graph/src/markers.rs]] and surfaced as first-class `marker` nodes.

## 5. The authored layer — ADRs, blueprints, and the drift gate

ADRs [[crates/rto-spec/src/adr.rs]] and blueprints
[[crates/rto-spec/src/blueprint.rs]] (this document is one) link their intent into
the code with `[[path#Symbol]]` wiki-links and `// @rto:` source annotations
[[crates/rto-spec/src/annotate.rs]]. [[crates/rto-spec/src/check.rs#run]] validates
every authored link against the derived graph and fails CI on drift — a link to a
missing symbol, or code annotated to a superseded ADR. This is what keeps the
design and the implementation honest (ADR-0004).

## 6. Inference, content ingestion & local models

The default embedder is a dependency-free hashing embedding
[[crates/rto-graph/src/infer.rs]]; the `inference-local-models` tier swaps in a
llama.cpp-backed GGUF embedder. Content ingestion enriches `meta.content` for
embedding: prose and PDF text, OCR/vision for images (ADR-0005), and speech
transcription for audio — all through the shared engine
[[crates/rto-llama/src/llama.rs#LlamaEngine]], which implements the
[[crates/rto-llama/src/engine.rs#Engine]] trait. The curated model matrix and the
consent-gated, checksum-verified downloader live in the registry
[[crates/rto-graph/src/models.rs]] (candle has been retired; llama.cpp is the sole
engine).

## 7. Serving & the one query surface

The graph is queried one way and rendered many ways. `query`/`context`/`path`
read the store [[crates/rto-graph/src/query.rs]]; the docs-site and OKF
renderers [[crates/rto-render/src/docs.rs]] / [[crates/rto-render/src/okf.rs]]
are build-outputs of the same graph; and the local model server
[[crates/rto-serve/src/server.rs]] exposes an OpenAI-compatible `/v1` endpoint over
pulled models, optionally registering the graph tools so a served model can query
the codebase (ADR-0002/0006).

## 8. CI, artifacts & configuration

A merge publishes the content-addressed graph artifact
[[crates/rto-graph/src/artifact.rs]]; `post-checkout`/`post-merge` hooks installed
by [[crates/roteiro/src/init.rs]] fetch and `load` it (offline fallback: rebuild),
so a fresh clone is `check`-green without a local rebuild. All behaviour is driven
by one layered project TOML [[crates/roteiro/src/config.rs]] (ADR-0007). The
codegraph oracle [[crates/rto-graph/src/codegraph.rs]] cross-checks Roteiro's
derived symbols against an external tool as a validation-only comparison.

## 9. Risks & invariants

- **Provenance is never conflated.** Derived facts are a pure function of code;
  authored and inferred facts layer on top and are re-applied after every sync
  (§2). A rebuild must never promote an inferred guess to a derived fact.
- **The graph is content-addressed, not mtime-based** (§3), so it is correct
  across branches, worktrees, and CI caches.
- **Authored intent is CI-gated** (§5): this blueprint and the ADRs cannot drift
  from the code without failing `check`.
- **Heavy dependencies stay opt-in** (§1): llama.cpp, the model *execution*
  tiers and the sandbox runtime are behind feature flags. The default build needs
  no toolchain class beyond the C compiler Rust already requires — it compiles C
  for bundled SQLite, 18 tree-sitter grammars, and (since `models` became a
  default feature) `ring`'s crypto core — but never C++, cmake or libclang.
- **Capability in the default build is not activity** (§1). `models` and
  `exec-subprocess` ship by default so a stock install can *prepare* to work
  offline, but each retains its own runtime consent: `model pull` needs a `[y/N]`
  yes, `security prefetch --allow-download` needs the flag, and `security run`
  needs `--allow-unsandboxed` on every invocation. Removing a build-time gate is
  only safe while the runtime one stays.

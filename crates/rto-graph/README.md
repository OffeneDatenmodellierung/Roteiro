# rto-graph

The graph core of [**Roteiro**](https://roteiro.dev) — a provenance-tagged
knowledge graph for your codebase.

This crate provides the **SQLite store**, the **provenance model** (`derived` |
`authored` | `inferred` edges, the last carrying a confidence), the
**content-addressed cache** (keyed by git blob/tree hashes so extraction is
deterministic and shareable), the **tree-sitter extraction** (symbols, calls,
imports across 15+ languages), the incremental **sync** engine (with a
working-tree dirty overlay), the **query** surface (`explain`, `path`, `debt`,
`search`), the dependency-aware **context** cache, similarity **inference** and
**duplicate** detection, and the pluggable local-model **registry**.

It is the foundation the other Roteiro crates build on and is usable as a
library. Heavy capabilities (`inference`, `models`, `pdf-text`, `image-ocr`,
`image-vision`) are behind feature flags; the default build is small and offline.

## Stability

This crate is **an implementation detail of the `roteiro` CLI**. It is published
only because crates.io requires a published package's dependencies to be registry
packages, so `roteiro` cannot ship unless it does.

Its public API carries **no stability guarantee** — breaking changes ship as minor
version bumps. If you depend on it directly, pin an exact version.

- **Docs:** <https://roteiro.dev>
- **Source & issues:** <https://github.com/OffeneDatenmodellierung/Roteiro>

Dual-licensed under MIT or Apache-2.0.

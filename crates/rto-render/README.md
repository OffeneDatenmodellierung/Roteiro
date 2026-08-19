# rto-render

The **renderers** of [**Roteiro**](https://roteiro.dev) — build outputs of the
one provenance-tagged graph.

This crate turns the graph into a **docs website** and an **Obsidian vault** (one
linked note per node, edges as provenance-labelled `[[wikilinks]]`), and — behind
the optional `mcp` feature — an **MCP server** exposing the graph to AI agents
(`explain`/`search`/`context`/`check`/`path`/`debt`/`list_kind`, all read-only). Because every renderer regenerates from the
same store, what humans review is exactly what agents query.

## Stability

This crate is **an implementation detail of the `roteiro` CLI**. It is published
only because crates.io requires a published package's dependencies to be registry
packages, so `roteiro` cannot ship unless it does.

Its public API carries **no stability guarantee** — breaking changes ship as minor
version bumps. If you depend on it directly, pin an exact version.

- **Docs:** <https://roteiro.dev>
- **Source & issues:** <https://github.com/OffeneDatenmodellierung/Roteiro>

Dual-licensed under MIT or Apache-2.0.

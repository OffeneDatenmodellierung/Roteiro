# rto-render

The **renderers** of [**Roteiro**](https://roteiro.dev) — build outputs of the
one provenance-tagged graph.

This crate turns the graph into a **docs website** and an **Obsidian vault** (one
linked note per node, edges as provenance-labelled `[[wikilinks]]`), and — behind
the optional `mcp` feature — an **MCP server** exposing the graph to AI agents
(`explain`/`path`/`debt`/`list_kind`). Because every renderer regenerates from the
same store, what humans review is exactly what agents query.

- **Docs:** <https://roteiro.dev>
- **Source & issues:** <https://github.com/OffeneDatenmodellierung/Roteiro>

Dual-licensed under MIT or Apache-2.0.

# Roteiro

**A provenance-tagged knowledge graph for your codebase.**

The Portuguese *roteiros* were the guarded pilot books of the Age of Discovery —
accumulated route knowledge that made navigation repeatable. Roteiro does the
same for a codebase: structure, intent, and context in one queryable store,
for humans and AI agents alike.

Every edge in the graph records **how it was produced**:

| Provenance | Source | Nature |
|---|---|---|
| `derived` | tree-sitter AST extraction | Deterministic — symbols, calls, imports |
| `authored` | ADRs, blueprints, `// @rto:` annotations | Curated intent, drift-checked in CI |
| `inferred` | Docs, PDFs, embeddings | Fuzzy suggestions with confidence scores |

One SQLite store. One query surface. Three renderers: a docs website, an
Obsidian vault, and an optional MCP server (`--features mcp`) — all build
outputs of the same graph, so what humans review is what agents query.

## Status

**v0.0.1 — scaffold.** The workspace, provenance schema, and CLI surface exist;
subcommands fail loudly until implemented. See
[ADR-0001](docs/adr/0001-build-roteiro-unified-codebase-knowledge-graph.md)
for the full design and roadmap.

## Workspace

- `crates/rto-graph` — SQLite store, provenance model, content-addressed cache
- `crates/rto-spec` — house-style ADR/blueprint parsing, intent interview, `check`
- `crates/rto-render` — docs site, Obsidian vault, MCP (feature-gated)
- `crates/roteiro` — umbrella CLI

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your
option. Contributions are accepted under the same terms.

# Roteiro

**A provenance-tagged knowledge graph for your codebase.**

The Portuguese *roteiros* were the guarded pilot books of the Age of Discovery —
accumulated route knowledge that made navigation repeatable. Roteiro does the
same for a codebase: structure, intent, and context in one queryable store,
for humans and AI agents alike.

Every edge in the graph records **how it was produced**:

| Provenance | Source | Nature |
|---|---|---|
| `derived` | tree-sitter extraction (full symbol graph for Rust today; more languages rolling out via tree-sitter `tags`) | Deterministic — symbols, calls, imports |
| `authored` | ADRs, blueprints, `// @rto:` annotations | Curated intent, drift-checked in CI |
| `inferred` | Docs, PDFs, images, embeddings | Fuzzy suggestions with confidence scores |

One SQLite store. One query surface. Three renderers: a docs website, an
Obsidian vault, and an optional MCP server (`--features mcp`) — all build
outputs of the same graph, so what humans review is what agents query. Offline
by default; git-native and content-addressed, so the graph is shareable across a
team.

## Getting started

```sh
cargo install roteiro          # lean, fully-offline default build
cd your-repo
roteiro init                   # store + git hooks + AGENTS.md
roteiro sync                   # build the graph
roteiro review                 # graph-grounded review of your change
```

See <https://roteiro.dev> for the full guide (modes, local models, languages,
config), and [ADR-0001](docs/adr/0001-build-roteiro-unified-codebase-knowledge-graph.md)
plus [`docs/BUILD_PLAN.md`](docs/BUILD_PLAN.md) for the design and roadmap.
Contribution + review standards live in [`AGENTS.md`](AGENTS.md).

## Workspace

- `crates/rto-graph` — SQLite store, provenance model, content-addressed cache, extraction, sync, query, inference
- `crates/rto-spec` — house-style ADR/blueprint parsing, `check` (drift gate), importers, spec authoring
- `crates/rto-render` — docs site, Obsidian vault, MCP server (feature-gated)
- `crates/rto-serve` — local OpenAI-compatible model server (llama.cpp; feature-gated)
- `crates/rto-llama` — llama.cpp inference core (generation, embeddings, vision), shared by serving and internal uses
- `crates/roteiro` — umbrella CLI

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your
option. Contributions are accepted under the same terms.

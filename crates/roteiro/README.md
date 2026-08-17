# roteiro

**A provenance-tagged knowledge graph for your codebase** — structure, intent,
and context in one queryable store, for humans and AI agents alike.

Roteiro reads a git repository and assembles a single SQLite knowledge graph of
the code (symbols, calls, imports across 15+ languages), the documents and
decisions that govern it (ADRs, blueprints, `// @rto:` annotations), and the
fuzzy relationships between them (embeddings). Every node and edge records **how
it was produced** — `derived`, `authored`, or `inferred` — so you always know
whether a fact is a verified truth, a human decision, or a machine's suggestion.
Offline by default; git-native and content-addressed, so the graph is shareable
across a team.

This crate is the umbrella **CLI**. Common commands:

```sh
roteiro init      # scaffold the store + git hooks + AGENTS.md in your repo
roteiro sync      # build / incrementally update the graph
roteiro query …   # explain a node and its provenance-labelled edges
roteiro review    # graph-grounded review of your current change
roteiro check     # verify authored intent against the code (a drift gate)
roteiro render …  # emit a docs site or an Obsidian vault
```

Feature flags gate the heavier capabilities (`inference`,
`inference-local-models`, `pdf-text`, `image-ocr`, `image-vision`, `serve`,
`mcp`, `exec-boxlite`); the default build is small, needs no C++/cmake/libclang,
and makes no network call on its own. It carries `models` and `exec-subprocess`,
so the whole of "prepare once, then work offline" — `roteiro model pull` and
`roteiro security prefetch|status|run` — exists without a rebuild. Presence is
not activity: `pull` fetches nothing until you consent, and `security run`
refuses without `--allow-unsandboxed` every time. See
`cargo install roteiro --all-features` for everything, and
`--no-default-features --features execution` for a build that provisions and
ingests but cannot execute an analyzer.

- **Docs & guide:** <https://roteiro.dev>
- **Source & issues:** <https://github.com/OffeneDatenmodellierung/Roteiro>

Dual-licensed under MIT or Apache-2.0.

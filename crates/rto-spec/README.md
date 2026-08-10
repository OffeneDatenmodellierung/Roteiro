# rto-spec

The **authored layer** of [**Roteiro**](https://roteiro.dev) — house-style
ADR/blueprint parsing, the intent interview, drift checking, importers, and
graph-grounded spec authoring.

This crate parses house-style ADRs (frontmatter, sections, `[[path#Symbol]]`
wiki-links) and `// @rto:` annotations into `authored` facts, and validates them
against the derived code graph (`roteiro check` — the drift gate). It also hosts
the one-shot **importers** (lat.md → authored, Graphify → inferred, codegraph →
validation oracle) and the tiered, graph-grounded **spec authoring**
(`roteiro spec context`/`scaffold`/`draft`, ADR-0004).

Dependency-free by design (no YAML crate — frontmatter is hand-parsed).

- **Docs:** <https://roteiro.dev>
- **Source & issues:** <https://github.com/OffeneDatenmodellierung/Roteiro>

Dual-licensed under MIT or Apache-2.0.

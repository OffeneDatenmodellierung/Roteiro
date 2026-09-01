---
Title: Build Roteiro — a unified, provenance-tagged codebase knowledge graph (spec-store v2)
Space: ARCH
Parent: ADRs

# ADR-specific metadata (mark ignores unknown keys; we use them for indexing/search)
type: adr
adr-id: "0001"
status: Accepted                    # Draft | For Review | Accepted | Rejected | Superseded
architectural-significance: HIGH    # SOFT | LOW | MEDIUM | HIGH | VERY HIGH
domain: Developer Tooling
decision-makers: ["The Roteiro Project Team"]
superseded-by:
version: "1.5"
last-modified: 2026-09-01
confluence-url:
---

# ADR-0001: Build Roteiro — a unified, provenance-tagged codebase knowledge graph (spec-store v2)

| | |
|---|---|
| **State** | Accepted |
| **Architectural Significance** | HIGH |
| **Domain** | Developer Tooling |
| **Document version** | 1.5 |

## Reference

Successor to the spec-store CLI (April 2026). Standalone project; Thalweg will be its first consumer but Roteiro carries no `twg-` dependency and must serve any codebase.

Repository: `github.com/OffeneDatenmodellierung/Roteiro` · Docs site: `roteiro.dev` · crates.io: `roteiro`, `rto-graph`, `rto-spec`, `rto-render` (all confirmed available 2026-08-07). Open source, dual-licensed **MIT OR Apache-2.0** (matching Thalweg and Rust ecosystem convention).

## Summary

Build **Roteiro**: a standalone Rust tool (1.96, 2024 edition) that replaces the current three-tool stack (lat.md, codegraph, Graphify) with a single knowledge graph in which every edge carries **provenance** — `derived` (deterministic tree-sitter AST extraction), `authored` (house-style ADRs/blueprints linked into code symbols), or `inferred` (fuzzy doc/image/embedding extraction with confidence scores). One SQLite store, one query surface, three renderers (docs site, Obsidian vault, optional MCP), with a content-addressed cache keyed by git tree hash so branches and worktrees share extraction work and CI-published artifacts remain the single source of truth. Roteiro inherits spec-store's intent-confirmation interview as the front door for authoring new specs/ADRs, and ships one-shot importers for existing lat.md, Graphify, and codegraph content.

## Context

We currently run three overlapping tools to give agents and humans context about a codebase: **lat.md** (authored markdown knowledge graph capturing intent, with wiki links and drift checking), **codegraph** (deterministic tree-sitter AST symbol/call graph in SQLite, exposed over MCP), and **Graphify** (broad tree-sitter + doc/PDF/image ingestion into a fuzzy relationship graph). Each answers a different question — *why is it shaped this way*, *what is the structure*, *what exists across all artifacts* — but running three tools per change is operationally heavy, and agents cannot reliably choose which tool answers a given question, degrading answer quality.

Separately, our house ADR/blueprint discipline already contains most of the *intent* content lat.md would hold (57 ADRs on Thalweg alone), but lacks symbol-level linkage into code and automated drift checking. The spec-store CLI proved two things worth carrying forward: the pre-coding interview flow that confirms user intent before specs are written, and registry-backed duplication prevention for AI agents.

The decision: consolidate into one purpose-built tool, or continue composing existing tools.

## Decision makers

- The Roteiro Project Team — primary decision maker

## Recommended option

**Option 4 — build Roteiro.** The three tools answer different questions with incompatible production models (deterministic derivation, human authoring, heuristic inference); no existing tool unifies them, and a naive union would be a worse version of each. Modelling the difference as **edge provenance within one graph** eliminates the agent's tool-selection problem (one query surface returns mixed, labelled results), lets precise matching win wherever it exists with fuzzy links clearly marked as suggestions, and makes the docs website, Obsidian vault, and agent context all build outputs of the same graph — humans review the same data agents query. Build effort is bounded because the structural layer reuses proven tree-sitter patterns (validated externally at Linux-kernel scale) rather than reinventing parsing; our effort concentrates on the parts nobody ships: the ADR/blueprint parser, provenance schema, drift `check`, interview flow, and git-native caching.

## Options considered + consequences

Evaluation dimensions: answer quality for agents, human review surface, ops burden per change, offline capability, build/maintenance effort, migration cost.

### Option 1: Keep the three-tool stack (lat.md + codegraph + Graphify)

**Description:** Continue running all three, each maintained upstream, integrated via their own MCP servers/CLIs.

**Consequences:**
- Pros: zero build effort; upstream maintenance; each tool best-in-class at its layer.
- Cons: three indexes rebuilt per change; agents mis-route questions across three query surfaces; no shared provenance so fuzzy and precise answers are indistinguishable; three schemas, three configs, three failure modes; intent layer (lat.md) duplicates our ADR content rather than building on it; no unified docs/Obsidian output.

### Option 2: Adopt lat.md + codegraph only, drop Graphify

**Description:** Use lat.md for intent and codegraph for structure, unmodified; accept loss of doc/PDF/image ingestion.

**Consequences:**
- Pros: two tools instead of three; both local-first and offline-capable; minimal build effort.
- Cons: still two query surfaces and the routing problem; loses multi-artifact ingestion we actively use; lat.md's markdown conventions conflict with house ADR/blueprint style, forcing either duplication or abandoning the house style; no CI-canonical artifact story; no interview flow.

### Option 3: Extend spec-store in place (v1.x)

**Description:** Bolt AST extraction, ADR parsing, and renderers onto the existing spec-store codebase (Qdrant/SQLite hybrid).

**Consequences:**
- Pros: reuses working interview flow, git hooks, quality gates; single repo continuity.
- Cons: spec-store's Qdrant dependency undermines the offline/zero-service requirement; its schema was designed for spec registration, not a provenance-tagged multi-source graph — retrofit cost approaches rebuild cost; risks ending with two half-overlapping registries (spec-store and the graph), recreating the routing confusion this decision exists to eliminate.

### Option 4: Build Roteiro (recommended)

**Description:** New standalone Rust 1.96 / 2024-edition workspace: three crates plus one umbrella CLI —

- **`rto-graph`** — SQLite store; tree-sitter extraction across all supported languages (vendor proven grammars/patterns, do not reinvent); provenance-tagged edges (`derived` | `authored` | `inferred` + confidence); content-addressed cache in `.git/roteiro/objects/` keyed by blob/tree hash, shared across worktrees, per-worktree dirty-file overlay.
- **`rto-spec`** — house-style ADR/blueprint parser (sections become graph nodes; `[[path#Symbol]]` links and `// @rto:` source annotations become `authored` edges); spec-store's intent-confirmation interview (`roteiro spec new`); `roteiro check` fails CI on drift (ADR → missing symbol, code → superseded ADR); duplication check querying both semantic similarity and structural facts.
- **`rto-render`** — docs website (ADRs, blueprints, overview, per-subsystem AI context pages), Obsidian vault export, and MCP server as a feature-gated `serve` subcommand (thin wrapper over the same query API; CLI-first with `--json` is the primary agent interface, plus an installed agent skill/AGENTS.md snippet).
- **`roteiro`** — umbrella CLI.

Operational model: git hooks (`post-checkout`/`post-merge`) compare local store against HEAD tree hash → no-op, fetch CI-published content-addressed artifact, or incremental rebuild proportional to the diff. CI on PR merge runs extract → check → assemble → publish artifact → render docs + vault, making the merged graph the central source of truth; local runs are deterministic previews. Fully offline by default (compiled-in grammars, bundled local embedding model); network only for optional artifact fetch, with rebuild as fallback. Inference (fuzzy layer) is a separate, optional CI stage so it never gates offline local rebuilds. One-shot importers (`roteiro import --from lat|graphify|codegraph`) map lat.md → `authored`, Graphify doc/media nodes → `inferred` (code-structure edges dropped in favour of re-derivation), codegraph → bootstrap/validation oracle only; each import emits a migration report listing imported edges per provenance class, validation failures, and promotion candidates.

**Consequences:**
- Pros: one query surface ends agent tool-routing ambiguity; precise-where-known, fuzzy-where-suggested with explicit labelling; house ADR style becomes the intent layer rather than competing with it; docs site, vault, and agent context are guaranteed-consistent build outputs; branch/worktree correctness via content addressing; offline; interview flow preserved; clean migration path from all three incumbents.
- Cons: build and ongoing maintenance effort is ours; tree-sitter grammar breadth must be tracked over time; supersedes spec-store (migration for its existing registry required); fuzzy-extraction quality is our problem once Graphify is dropped; dual MIT/Apache-2.0 licensing requires vendored code and grammars to be licence-compatible (`cargo deny` licence gate enforces this).
- Quality bar: clippy/fmt/audit/deny gates from day one, dogfooded on its own repository; this ADR is authored and checked by the tool once bootstrapped. An 85% per-file coverage ratchet was intended as part of this bar — it is an **aspiration that was never wired into CI**, and coverage is now measured non-blocking instead (issue #319). Stated here as intent, not as an enforced gate.

Cost is engineering time only in all options (all tools open source, local-first); no vendor rack rates apply.

## Implementation

This ADR's decisions are linked into the code they govern, so `roteiro check`
validates the design against the implementation (the ADR is dog-fooded like any
other authored intent):

- **The one provenance-tagged store** — [[crates/rto-graph/src/store.rs#Store]],
  with the three-class model in [[crates/rto-graph/src/provenance.rs#Provenance]].
- **Deterministic, content-addressed derived extraction** —
  [[crates/rto-graph/src/extract.rs#Registry]] over the cache
  [[crates/rto-graph/src/cache.rs#ObjectCache]], assembled incrementally by
  [[crates/rto-graph/src/sync.rs#sync]] (with the working-tree overlay
  [[crates/rto-graph/src/sync.rs#sync_worktree]]).
- **The authored layer & drift gate** —
  [[crates/rto-spec/src/check.rs#run]].
- **Renderers as build-outputs of the one graph** (docs site, Obsidian vault,
  MCP) — [[crates/rto-render/src/lib.rs#Target]].

### The three classes are closed, permanently

`derived | authored | inferred` is not a list awaiting a fourth entry. It is the
decision this ADR makes, and the evidence that it is closed is that the project
has since declined to extend it three times running:

- **[[docs/adr/0012-analyzer-findings-artifact-model.md]]** — an analyzer's
  finding is not `derived` (it is not a pure function of the tree), not
  `authored` (nobody wrote it), and not `inferred` (its severity is a tool's
  output, not a confidence). It got its own artifact store.
- **[[docs/adr/0013-agent-memory-artifact-store.md]]** — an agent's memory is
  episodic and unreproducible, and re-running nothing regenerates it. Its own
  store.
- **[[docs/adr/0015-generated-media-content-artifact-store.md]]** — a model's description of an
  image, or its transcript of audio, is *generated* rather than extracted; a
  silent clip once produced a fluent lecture that was stored as a `derived` fact.
  Its own store.

Each was a candidate for a fourth variant, and each time a separate store was the
better answer — because what did not fit was never *a fourth way of producing a
graph fact*. It was *not a graph fact*.

So `Provenance` is deliberately exhaustive in Rust and closed on the wire, and
that is a decision rather than an oversight. Marking it `#[non_exhaustive]` would
be **weaker documentation than the current silence**: it would tell a reader that
a fourth variant is anticipated, when three consecutive ADRs establish that it is
not. The precedent for stating this at the definition is
[[crates/rto-remote/src/escalation.rs#Trigger]], whose doc comment records the
same reasoning — exhaustiveness is the right default where a set is closed by a
*decision* rather than by today's implementation.

The cost is stated rather than hidden. `Provenance` rides every edge of every
`roteiro.query/v1` document, so a fourth class would be simultaneously a Rust
break and a wire break on the most-consumed document the project emits. **That is
the point.** The break is the signal, and anything that would require one should
first be asked whether it is a graph fact at all — which, three times out of
three, it was not.

## Advice Received

Accepted by the project team without external advisory review — single-team open-source project; future significant changes will be superseding ADRs. Naming secured: `roteiro.dev` registered, crates.io names available, repo created at `OffeneDatenmodellierung/Roteiro`. Planned: circulate v0.x for review before implementation beyond crate-name reservation.

| Date | Advisor | Decision version | Advice |
|------|---------|------------------|--------|

## Document version history

| Version | Date | Notes |
|---------|------|-------|
| 0.1 | 2026-08-07 | Initial draft: consolidation rationale, four options, Roteiro architecture (provenance model, three crates + CLI, content-addressed cache, CI-canonical artifacts, importers, MCP-optional). |
| 1.0 | 2026-08-07 | Accepted. Naming secured (roteiro.dev, crates.io, OffeneDatenmodellierung/Roteiro); MIT OR Apache-2.0; attribution to The Roteiro Project Team. Stage 1 bootstrap started. |
| 1.1 | 2026-08-10 | Added an **Implementation** section linking the ADR's decisions into the code (`[[path#Symbol]]`), so `roteiro check` validates this ADR against the implementation (Stage 14 self-check). |
| 1.2 | 2026-08-16 | Corrected the quality bar (issue #319): the 85% per-file coverage ratchet named here was an **aspiration that was never wired into CI** — `.github/workflows/ci.yml` contained no coverage tooling at all, so every stage DoD citing it was unverifiable. Coverage is now measured non-blocking; the decision to enforce a floor is deferred until the real numbers are in hand. No architectural decision in this ADR changes. (Frontmatter `version` also brought up to date — it still read 1.0 after the 1.1 amendment.) |
| 1.3 | 2026-08-19 | **Answers a question the code had left to silence** (issue #448): is `derived \| authored \| inferred` closed on purpose, permanently? Yes, and the evidence is that the project has since declined to extend it three times running — ADR-0012 gave analyzer findings their own artifact store, ADR-0013 gave agent memory one, ADR-0015 gave generated media one. Each was a candidate for a fourth variant; each time what did not fit was not a fourth way of *producing a graph fact* but something that was **not a graph fact**. Records the consequence for anyone reaching for `#[non_exhaustive]` on this enum: it would be **weaker documentation than the current silence**, because it would imply a fourth variant is anticipated when three consecutive ADRs establish that it is not. The precedent for saying so at the definition is `rto_remote::escalation::Trigger`. The cost is stated rather than hidden — `Provenance` rides every edge of every `roteiro.query/v1` document, so a fourth class is simultaneously a Rust break and a wire break on the most-consumed document the project emits, and that break is the signal rather than the obstacle. No architectural decision changes; a standing one is written down. |
| 1.4 | 2026-09-01 | **MSRV raised 1.94 → 1.96.** The driver is the OKF conformance stack (ADR-0021): `okf-validator`'s tree pulls twelve `oxc_*` crates that declare `rust-version = 1.96`, and the project accepted that tree into the **default** build rather than gating it — so the MSRV, not a feature flag, is what has to give. This is the project's first MSRV move, and it is made under BUILD_PLAN's standing rule that a bump is ADR-worthy and happens only when a dependency forces one, which is what happened. A second consequence is recorded rather than left implicit: the raise **retires the `rusqlite = 0.39` pin's stated reason**. That pin exists because `libsqlite3-sys` 0.38+ uses `cfg_select!`, stable since 1.95, and 1.95 was newer than the old MSRV; at 1.96 that blocker is gone. It is **not** thereby unblocked, and the manifest now says so: `boxlite` 0.10.0 requires `rusqlite ^0.39` and `libsqlite3-sys` declares `links = "sqlite3"`, so the graph admits one version and `--all-features` cannot move to 0.40 until boxlite does. Lifting the pin is a dependency-upgrade task gated upstream, not an MSRV one — recorded so the next MSRV bump does not inherit it as unfinished business. On CI only three of the ten pinned `toolchain:` values move — `ci.yml`'s `msrv` job and both `website.yml` jobs; the other seven already ran 1.98 and are untouched. No architectural decision in this ADR changes. |
| 1.5 | 2026-09-01 | **The anticipated break happened** (issue #706; recorded as a decision in ADR-0021 v1.1). v1.3 above closed `derived \| authored \| inferred` on purpose and named the price of ever opening it: a fourth class would be *"simultaneously a Rust break and a wire break on the most-consumed document the project emits, and that break is the signal rather than the obstacle"*. Reading a peer's OKF bundle is that case, and it resolved the way v1.3 said it should. The enum was **not** given a fourth way of *producing* a graph fact; the three that exist were qualified by **who** produced them — `external-derived` / `external-authored` / `external-inferred`, six tokens, with externality flattened to exactly one level and the import layer's `src_ref` naming the source. v1.3's reasoning is why a flat `External` was rejected rather than an oversight: collapsing a peer's tier would force one arm of `okf::origin_for` and then re-emit the flattened tier outward, laundering by round-trip. So v1.3 should be read as having priced this correctly, not as having forbidden it, and `#[non_exhaustive]` is still declined for the reason it gives. **The consequence v1.3 attached is still owed.** `Provenance` is a `pub` enum without `#[non_exhaustive]` in a published crate that shipped three variants at `rto-graph-v5.0.0`, so six is a breaking change for any downstream exhaustive match — precisely the case `AGENTS.md` reserves `!` for. The commit that made it (`7a98938`) carried neither a `!` nor a `BREAKING CHANGE:` footer, **and it has now shipped**: release-plz cut `5.1.0` on 2026-09-01, and the source published to crates.io as `rto-graph 5.1.0` carries all six variants. So the break did not merely risk going unsignalled — it went out as a **minor**, and a dependent on `^5.0` resolves forward into it and fails to compile. This row records the fact rather than the remedy, because the remedy (yanking the 5.1.0 set, and cutting the major that v1.3 says is the signal) is a registry action across all nine crates and belongs to the owner. |

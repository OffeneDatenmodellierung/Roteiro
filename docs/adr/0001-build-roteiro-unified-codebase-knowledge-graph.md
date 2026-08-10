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
version: "1.0"
last-modified: 2026-08-07
confluence-url:
---

# ADR-0001: Build Roteiro — a unified, provenance-tagged codebase knowledge graph (spec-store v2)

| | |
|---|---|
| **State** | Accepted |
| **Architectural Significance** | HIGH |
| **Domain** | Developer Tooling |
| **Document version** | 1.0 |

## Reference

Successor to the spec-store CLI (April 2026). Standalone project; Thalweg will be its first consumer but Roteiro carries no `twg-` dependency and must serve any codebase.

Repository: `github.com/OffeneDatenmodellierung/Roteiro` · Docs site: `roteiro.dev` · crates.io: `roteiro`, `rto-graph`, `rto-spec`, `rto-render` (all confirmed available 2026-08-07). Open source, dual-licensed **MIT OR Apache-2.0** (matching Thalweg and Rust ecosystem convention).

## Summary

Build **Roteiro**: a standalone Rust tool (1.94, 2024 edition) that replaces the current three-tool stack (lat.md, codegraph, Graphify) with a single knowledge graph in which every edge carries **provenance** — `derived` (deterministic tree-sitter AST extraction), `authored` (house-style ADRs/blueprints linked into code symbols), or `inferred` (fuzzy doc/image/embedding extraction with confidence scores). One SQLite store, one query surface, three renderers (docs site, Obsidian vault, optional MCP), with a content-addressed cache keyed by git tree hash so branches and worktrees share extraction work and CI-published artifacts remain the single source of truth. Roteiro inherits spec-store's intent-confirmation interview as the front door for authoring new specs/ADRs, and ships one-shot importers for existing lat.md, Graphify, and codegraph content.

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

**Description:** New standalone Rust 1.94 / 2024-edition workspace: three crates plus one umbrella CLI —

- **`rto-graph`** — SQLite store; tree-sitter extraction across all supported languages (vendor proven grammars/patterns, do not reinvent); provenance-tagged edges (`derived` | `authored` | `inferred` + confidence); content-addressed cache in `.git/roteiro/objects/` keyed by blob/tree hash, shared across worktrees, per-worktree dirty-file overlay.
- **`rto-spec`** — house-style ADR/blueprint parser (sections become graph nodes; `[[path#Symbol]]` links and `// @rto:` source annotations become `authored` edges); spec-store's intent-confirmation interview (`roteiro spec new`); `roteiro check` fails CI on drift (ADR → missing symbol, code → superseded ADR); duplication check querying both semantic similarity and structural facts.
- **`rto-render`** — docs website (ADRs, blueprints, overview, per-subsystem AI context pages), Obsidian vault export, and MCP server as a feature-gated `serve` subcommand (thin wrapper over the same query API; CLI-first with `--json` is the primary agent interface, plus an installed agent skill/AGENTS.md snippet).
- **`roteiro`** — umbrella CLI.

Operational model: git hooks (`post-checkout`/`post-merge`) compare local store against HEAD tree hash → no-op, fetch CI-published content-addressed artifact, or incremental rebuild proportional to the diff. CI on PR merge runs extract → check → assemble → publish artifact → render docs + vault, making the merged graph the central source of truth; local runs are deterministic previews. Fully offline by default (compiled-in grammars, bundled local embedding model); network only for optional artifact fetch, with rebuild as fallback. Inference (fuzzy layer) is a separate, optional CI stage so it never gates offline local rebuilds. One-shot importers (`roteiro import --from lat|graphify|codegraph`) map lat.md → `authored`, Graphify doc/media nodes → `inferred` (code-structure edges dropped in favour of re-derivation), codegraph → bootstrap/validation oracle only; each import emits a migration report listing imported edges per provenance class, validation failures, and promotion candidates.

**Consequences:**
- Pros: one query surface ends agent tool-routing ambiguity; precise-where-known, fuzzy-where-suggested with explicit labelling; house ADR style becomes the intent layer rather than competing with it; docs site, vault, and agent context are guaranteed-consistent build outputs; branch/worktree correctness via content addressing; offline; interview flow preserved; clean migration path from all three incumbents.
- Cons: build and ongoing maintenance effort is ours; tree-sitter grammar breadth must be tracked over time; supersedes spec-store (migration for its existing registry required); fuzzy-extraction quality is our problem once Graphify is dropped; dual MIT/Apache-2.0 licensing requires vendored code and grammars to be licence-compatible (`cargo deny` licence gate enforces this).
- Quality bar: 85% per-file coverage ratchet, clippy/fmt/audit/deny gates from day one, dogfooded on its own repository; this ADR is authored and checked by the tool once bootstrapped.

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

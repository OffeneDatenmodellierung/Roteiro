---
Title: Spec/Blueprint authoring pillar — tiered, graph-grounded, check-gated
Space: ARCH
Parent: ADRs

# ADR-specific metadata (unknown keys are ignored; used for indexing/search)
type: adr
adr-id: "0004"
status: Accepted                    # Draft | For Review | Accepted | Rejected | Superseded
architectural-significance: HIGH    # SOFT | LOW | MEDIUM | HIGH | VERY HIGH
domain: Developer Tooling
decision-makers: ["The Roteiro Project Team"]
superseded-by:
version: "1.0"
last-modified: 2026-08-09
confluence-url:
---

# ADR-0004: Spec/Blueprint authoring pillar — tiered, graph-grounded, check-gated

| | |
|---|---|
| **State** | Accepted |
| **Architectural Significance** | HIGH |
| **Domain** | Developer Tooling |
| **Document version** | 1.0 |

## Reference

Governs the **authoring pillar** (`roteiro spec`, Stage 13) of [[docs/adr/0001-build-roteiro-unified-codebase-knowledge-graph.md]] — the "spec-store v2" intent-confirmation front door ADR-0001 always envisioned but left as a stub. Extends the tiered, offline-first model machinery decided in [[docs/adr/0003-pluggable-embedding-models.md]] from *embedding* to *generative* models. See `docs/history/BUILD_PLAN.md` Stage 13.

## Summary

Make `roteiro spec` the front door for **authoring intent** — house-style ADRs/blueprints and a **graph-grounded build/deploy plan** — structured by GitHub **spec-kit**'s phase discipline (constitution → specify → clarify → plan → tasks) but rendered in Roteiro's *house* ADR/blueprint style, not spec-kit's own format.

Two invariants make it trustworthy rather than a hallucination engine:

1. **Graph-grounded.** Every generated artifact references *real* nodes from the store — existing symbols, ADRs, dependencies, and prior intent — assembled deterministically by the tool. Generated plans cannot cite code that does not exist.
2. **Check-gated.** Output is only "done" once `roteiro check` validates its `[[links]]` / `@rto:` against the graph. Authoring produces **`authored`** facts; the drift gate keeps them honest.

Generation is **tiered, mirroring inference** (ADR-0003), so it degrades gracefully with no model and no network:

- **Tier 0 — offline, no model (the floor):** deterministic **scaffolding** — a house-style ADR/blueprint skeleton, a structured **interview checklist**, and a build-plan **outline** built from real graph facts. No prose generation, so planning works with nothing installed / on a plane.
- **Tier 1 — light, default generative:** a **small local GGUF *instruct* model** (candle) drafts and expands sections offline on low-power hardware, extending ADR-0003's registry / consent-gated `pull` / candle backend from embedding to *text generation*.
- **Tier 2 — larger local:** a bigger pulled GGUF model for higher draft quality.
- **Tier 3 — foundation / agent:** the coding agent over MCP for best quality, or to **review** a Tier-1 draft. Driven by a bundled spec-kit-style **skill**.

**Agent-vs-tool boundary:** the *tool* owns everything deterministic and verifiable — graph-grounded context assembly, house-style skeletons, build-plan outlines from real facts, and the `check` gate. *Prose* is delegated — to a local model (Tiers 1–2) or the agent (Tier 3) — and always flows back through the tool's check gate before it counts. The tool never emits unlabelled or ungrounded intent.

## Context

ADR-0001 chose Roteiro over the three-tool stack partly on the strength of spec-store's **pre-coding interview** — the flow that confirms user intent *before* specs are written — and our house **ADR/blueprint discipline**. Both were named as the parts "nobody ships." Yet `roteiro spec` remains a `bail!` stub: the graph, the authored layer, `check`, and the inference tiers all exist, but the front door that produces new intent does not.

Four forces to reconcile:

1. **Offline-by-default & lean binary (ADR-0001).** Authoring must work with no model and no network; any generative model is opt-in and local, never an API call. The default `roteiro` binary must not grow.
2. **Correctness over fluency.** A generated plan that references a symbol or ADR that does not exist is *worse* than none — it launders hallucination as intent. Grounding in the graph and gating on `check` is what separates this from a chatbot. (ADR-0001's "precise-where-known" principle.)
3. **Low-power local generation (project directive).** Light-mode drafting must run on modest local hardware, exactly as the inference default does — not require a foundation model. This is a generative sibling of ADR-0003's embedding tiers.
4. **House style, not spec-kit's format.** spec-kit's *phase discipline* is worth adopting, but its markdown conventions conflict with the house ADR/blueprint style — the same conflict that led ADR-0001 to reject adopting lat.md's format wholesale. We take spec-kit's phases; we render in the house style.

**spec-kit phase → Roteiro artifact mapping:**

| spec-kit phase | Roteiro artifact | Grounding |
|---|---|---|
| constitution | house **principles/invariants** (ADR-0001 §1) | already exists; new work is checked against it |
| specify | house **ADR / blueprint** ("what & why") | `authored` nodes; symbol/ADR `[[links]]` |
| clarify | the **intent interview** (spec-store's front door) | graph-aware — surfaces related ADRs/symbols to prevent duplication |
| plan | graph-grounded **build/deploy plan** | references real symbols/deps; `check`-gated like `BUILD_PLAN.md` |
| tasks | a **task outline** derived from the plan | optionally linked to intent-debt markers (Stage 15) |

## Decision makers

- The Roteiro Project Team

## Recommended option

**Option 4 — tiered, graph-grounded, house-style, check-gated authoring (recommended).**

- **CLI surface** (grows across the tier PRs):
  - `roteiro spec context <topic>` — assemble **grounded context** for a topic from the graph (relevant symbols, ADRs, dependencies, and existing intent), as human text or `--json` for an agent. Tier 0; no model.
  - `roteiro spec scaffold [--kind adr|blueprint]` — emit a **house-style skeleton** plus a build-plan **outline** populated with real graph facts, and a structured **interview checklist**. Tier 0; no model. The result is `check`-clean by construction (its links point at real nodes).
  - `roteiro spec draft` *(Tier 1+, behind `inference-local-models`)* — fill prose into a scaffold using the local GGUF instruct model; falls back to Tier 0 (leave the checklist) when no model is installed.
  - A bundled **skill** lets the agent (Tier 3) drive the phased flow and, crucially, **review** a Tier-1 draft.
- **Generative model machinery** reuses ADR-0003's registry / platform-aware variant selection / consent-gated `pull` / `candle` backend, adding an **instruct-model** registry entry and a text-generation path (`candle-transformers`). Feature-gated under `inference-local-models` so the default and `inference` builds pull none of it.
- **Everything re-enters `check`.** Whatever tier produced the prose, the artifact is validated against the graph before it is considered authored intent.

## Options considered + consequences

### Option 1: Agent-only — the agent writes ADRs freehand

- Pros: no tool work; best prose when a foundation model is present.
- Cons: no grounding (cites symbols that may not exist), no offline path, no determinism, no `check` gate unless bolted on afterwards. Recreates the hallucinated-planning problem this ADR exists to prevent. Rejected as the *whole* answer — kept as **Tier 3**, but always behind the tool's grounding + gate.

### Option 2: Tool-only — deterministic templates, no prose ever

- Pros: fully offline, deterministic, always `check`-clean.
- Cons: too rigid; the interview and prose drafting are where the value is. Rejected as the *whole* answer — kept as **Tier 0**, the guaranteed floor.

### Option 3: Adopt spec-kit as-is (its CLI and markdown format)

- Pros: least design work; a known, documented flow.
- Cons: its markdown conventions conflict with the house ADR/blueprint style (the lat.md-format conflict from ADR-0001), and it is not graph-grounded or `check`-gated. Rejected — we take spec-kit's **phases**, not its **format**.

### Option 4: Tiered, graph-grounded, house-style, check-gated (recommended)

- Pros: works offline at Tier 0; generation scales with available hardware/agent; **every** artifact is grounded and verifiable; reuses existing machinery (ADR-0003 registry, `check`, the graph); the agent-vs-tool boundary keeps deterministic work in the tool and prose clearly delegated.
- Cons: a generative GGUF model is larger and slower than the embedding one (mitigated: same opt-in feature + consent gate, and Tier 0 needs none); the spec-kit→house-style phase mapping must be maintained; small-local-model prose is a *draft*, not final (mitigated: the Tier-3 review path).

## Consequences

- `roteiro spec` stops being a stub: Tier 0 (`spec context`, `spec scaffold`) ships first with **no new dependencies**, then Tier 1 (`spec draft`) extends `inference-local-models` with a generative entry — subject to the `cargo deny` licence gate like every model dep, and behind the existing feature flag so the default build is unchanged.
- Generated ADRs/blueprints are **`authored`** provenance and are **`check`-gated** — Roteiro's own dogfood `check` in CI validates them, so a scaffold that links a non-existent symbol fails the build.
- The authoring pillar composes with the rest: `context` reads the same graph agents query (one query surface, ADR-0001); the interview is duplication-aware via the graph; `tasks` can reference Stage 15 intent-debt markers.
- A future **capstone "overall Blueprint"** artifact (an end-of-implementation, graph-grounded summary of the whole system) is a natural product of this pillar and is tracked here.

## Advice Received

Project direction incorporated above: light-mode generation must run **offline on low-power local hardware** (a generative sibling of the ADR-0003 inference default), not require a foundation model; and the tool must keep **deterministic, verifiable** work (grounding, scaffolding, `check`) separate from delegated prose.

## Document version history

| Version | Date | Notes |
|---------|------|-------|
| 0.1 | 2026-08-09 | For Review. Tiered (0: offline scaffold → 1: local GGUF instruct → 2: larger local → 3: agent), graph-grounded, house-style, `check`-gated authoring pillar; spec-kit phases mapped to house artifacts; generative tier extends ADR-0003's registry/consent/candle machinery; agent-vs-tool boundary defined. |
| 1.0 | 2026-08-09 | Accepted. Tier 0 implementation began with `roteiro spec context` (graph-grounded context assembly, no model). |

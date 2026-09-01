---
name: code-review
description: Review a change to Roteiro against its provenance model, CI gates, and house standards, grounded in the knowledge graph.
---

# Roteiro code review

Review the change using this repository's standards. The authoritative sources
are [`AGENTS.md`](../../../AGENTS.md) and
[`docs/REVIEW_CHECKLIST.md`](../../../docs/REVIEW_CHECKLIST.md) — read them; this
skill is a pointer, not a duplicate.

## How to review

1. **Ground it in the graph.** If the tooling is available, run
   `roteiro review [--json]` on the change: it reports each touched symbol's
   callers/callees, the ADRs governing it, related docs, the authored **drift**
   and **intent-debt** the change introduces, and the **blast radius** of
   dependents to re-check. Review against that, not just the diff.
2. **Work through the checklist** in `docs/REVIEW_CHECKLIST.md`.

## What to flag (most important first)

- **Provenance violations** — an unlabelled edge, an `inferred` edge with no
  confidence, or non-deterministic `derived` extraction. These break the core
  model.
- **Broken gates** — clippy pedantic, `cargo fmt`, tests, `roteiro check`,
  `cargo deny`/`audit`, MSRV 1.96, or `unsafe_code`.
- **Offline-by-default regressions** — a heavy dependency not feature-gated, an
  un-consented network call, or a dependency licence not on the allow-list.
- **Drift** — a change that dangles an ADR `[[link]]`/`@rto:` annotation, or an
  architectural change without an ADR update.
- **Scope** — more than one concern in the PR.

Report findings concisely with file:line references and a concrete fix.

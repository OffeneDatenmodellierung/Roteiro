---
Title: Analyzer findings — a separate artifact model, never a provenance class
Space: ARCH
Parent: ADRs

# ADR-specific metadata (unknown keys are ignored; used for indexing/search)
type: adr
adr-id: "0012"
status: For Review                  # Draft | For Review | Accepted | Rejected | Superseded
architectural-significance: HIGH    # SOFT | LOW | MEDIUM | HIGH | VERY HIGH
domain: Knowledge Graph
decision-makers: ["The Roteiro Project Team"]
superseded-by:
version: "1.0"
last-modified: 2026-08-15
confluence-url:
---

# ADR-0012: Analyzer findings — a separate artifact model, never a provenance class

| | |
|---|---|
| **State** | For Review |
| **Architectural Significance** | HIGH |
| **Domain** | Knowledge Graph |
| **Document version** | 1.0 |

## Reference

Establishes how output from **external security/quality analyzers** (`cargo-audit`,
`semgrep`, and successors) is persisted. Such output is **not** a fact extracted
from source, and it does **not** get a provenance class: it is stored as its own
**analysis-run / finding artifact model**, leaving `derived | authored | inferred`
(`crates/rto-graph/src/provenance.rs`) untouched. Governed by the determinism and
offline-first principles of
[[docs/adr/0001-build-roteiro-unified-codebase-knowledge-graph.md]] and the
optional-capability posture of
[[docs/adr/0003-pluggable-embedding-models.md]] and
[[docs/adr/0006-local-model-serving.md]].

This ADR is the **first of a pair**. Its sibling,
[[docs/adr/0013-agent-memory-artifact-store.md]], applies the same principle to
durable agent-learned knowledge. Together they state a structural rule:
*knowledge that is not a derived/authored/inferred graph fact gets its own
artifact store, and never borrows the graph's trust.*

## Summary

- Analyzer output is persisted as an **analysis run** (the execution and its
  evidence) plus the **findings** it produced — a model distinct from
  `nodes`/`edges`.
- **No fourth `Provenance` variant.** `derived` keeps meaning *deterministic
  extraction, a pure function of (path, blob id, bytes)*; `inferred` keeps
  meaning *fuzzy, confidence-bearing*. Analyzer severity is neither.
- **Nothing enters `nodes`/`edges`**, so `export_factset` remains a
  byte-identical function of the tree and the published `GraphArtifact` is
  unaffected.
- **Two runners, one schema.** A finding is the same artifact whether it was
  produced locally in a sandbox or ingested from a CI report
  (`roteiro security ingest`). Ingest is a first-class mode, not a fallback.
- **Evidence is part of the record**: image digest, tool + version, rules digest,
  advisory-DB digest and age, command policy, source tree/blob identity, exit
  status, timestamps.
- **`EXTRACT_VERSION` does not change.** Analyzer results are not extraction
  output; bumping it would needlessly invalidate every cached blob.

## Context

Roteiro's headline promise is provenance-tagged, deterministic, reproducible
knowledge: the graph rebuilds identically from source, and
`AGENTS.md` states derived extraction is a pure function of
`(path, blob id, bytes)`. Analyzer findings do not fit that contract:

1. They are produced by an **external tool at a point in time**, against a
   pinned rule set and advisory database that both change independently of the
   source tree.
2. They carry **severity**, which is a tool judgement, not the confidence score
   that `inferred` requires (`migrations.rs` — "a fuzzy suggestion without a
   score is a bug").
3. They are **re-derivable but not source-pure**: re-running the same analyzer
   at the same commit with a *newer* advisory DB legitimately yields a different
   answer. That is a feature of security scanning and a violation of derivation.

ADR-0001 records that provenance exists precisely because the source tools had
**incompatible production models**. Analyzer output is a fourth production model:
*asserted by a tool run, valid as of that run's inputs*. The consistent response
is a new artifact model, not a new label on an existing one.

Three tempting shortcuts were considered and rejected — see below. The most
dangerous is the one that **works mechanically**: `NodeKind::Other("security_finding")`
would compile and pass, while inheriting `nodes.provenance NOT NULL DEFAULT 'derived'`
and being swept into `export_factset`, silently publishing tool output into an
artifact that is supposed to be a pure function of the tree.

## Decision makers

The Roteiro Project Team.

## Recommended option

**A separate persisted analysis-run/finding artifact model, with its own tables,
its own retrieval surface, and no contact with graph provenance.**

### What is stored

An **analysis run** records the execution and everything needed to reproduce or
distrust it: analyzer id and version, runner kind (`sandboxed` | `subprocess` |
`ingested`), isolation label, image digest where applicable, rules digest,
advisory-DB digest and publication date, command policy (network denied,
read-only worktree, scrubbed environment), the source identity it ran against
(commit / tree / lockfile blob), start and end timestamps, exit status, and a
digest of the raw report.

A **finding** belongs to a run and carries a stable identity key so the same
issue is recognisable across runs:

- `finding:semgrep:<rule>:<path>:<start-byte>:<snippet-hash>`
- `finding:cargo-audit:<advisory>:<pkg>:<version>:<lockfile-blob>`

Findings are a **replaceable layer** keyed by `security:<analyzer>:<worktree-id>`:
a successful re-run replaces the previous layer wholesale, so a fixed issue
disappears rather than lingering. Note the known gap — existing import code
deletes edges but not obsolete owned nodes, so owned-record cleanup is net-new
work and must be implemented rather than assumed.

### What is deliberately not done

- **No fourth `Provenance` variant.** It would require widening two CHECK
  constraints, touching every exhaustive match on a `Copy` enum, and — worse —
  it would redefine the vocabulary the README leads with.
- **No `NodeKind` for findings in the graph tables.** See the trap above.
- **No `authored`-with-metadata.** `authored` means *a human or agent
  deliberately wrote this in a reviewed file* and carries a **+40 relevance
  boost** in `search` (`crates/rto-graph/src/query.rs`). Unreviewed tool output
  riding that boost is trust-model contamination by construction.

### Offline and degradation contract

Analyzers and their inputs are **pre-provisioned, then pinned**, mirroring the
existing model-pull UX (`roteiro model list/pull`): `roteiro security prefetch`
fetches and verifies pinned assets by digest; `roteiro security status` reports
each digest, fetch time, and advisory-DB age.

- Cold cache with **no network**: fail with a distinct `assets-unavailable-offline`
  error naming the missing digests and the exact prefetch command. Never silently
  fall back to host tools; never fetch implicitly.
- Cached but **stale** advisory DB: still run, but stamp every result with
  `advisory_db_published_at`, `fetched_at`, and age, and label it *possibly
  stale* — never *current*.

This satisfies "mostly offline": pre-download is expected, degradation is
explicit and informative, and no code path becomes quietly network-dependent.

## Options considered + consequences

| Option | Verdict |
|---|---|
| New `Provenance::Analyzed` variant | **Rejected** — broad semver/storage churn, and it dilutes a three-word vocabulary that is the project's headline promise. |
| Reuse `Derived` + metadata | **Rejected** — redefines `derived` from *pure function of source* to *produced by some configured producer*; a permanent conceptual smear. |
| `NodeKind::Other("security_finding")` in `nodes`/`edges` | **Rejected** — works mechanically, which is the trap; inherits `provenance DEFAULT 'derived'` and leaks into `export_factset`. |
| **Separate artifact model (chosen)** | Keeps the graph a pure function of source; gives findings the evidence chain they actually need. |

## Consequences

**Positive**

- The graph still rebuilds identically from source; `GraphArtifact` stays a
  byte-identical function of the tree.
- `derived | authored | inferred` keep their published meanings.
- Findings gain an evidence chain (digests, policy, run identity) that graph
  provenance was never designed to hold.
- Local sandboxed execution and CI ingest become the **same code path**, so the
  two approaches stop competing.

**Negative / costs**

- A second retrieval surface: findings are not reachable through existing graph
  queries and need their own commands/serving.
- Owned-record cleanup on layer replacement is net-new work.
- One append-only SQLite migration.
- Cross-surfacing findings alongside graph facts later will need a deliberate
  join design rather than falling out for free.

## Status

For Review. Execution is planned in [BUILD_PLAN_V2](../BUILD_PLAN_V2.md) Stage 21,
which lands this contract-first: the runner trait, the normalized finding schema,
and `roteiro security ingest` before any sandboxed backend (the backend itself is
[[docs/adr/0014-sandboxed-analyzer-execution.md]]).

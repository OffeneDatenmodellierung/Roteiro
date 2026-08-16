---
Title: Agent memory — a two-tier artifact store, decaying by evidence not by clock
Space: ARCH
Parent: ADRs

# ADR-specific metadata (unknown keys are ignored; used for indexing/search)
type: adr
adr-id: "0013"
status: For Review                  # Draft | For Review | Accepted | Rejected | Superseded
architectural-significance: HIGH    # SOFT | LOW | MEDIUM | HIGH | VERY HIGH
domain: Knowledge Graph
decision-makers: ["The Roteiro Project Team"]
superseded-by:
version: "1.2"
last-modified: 2026-08-16
confluence-url:
---

# ADR-0013: Agent memory — a two-tier artifact store, decaying by evidence not by clock

| | |
|---|---|
| **State** | For Review |
| **Architectural Significance** | HIGH |
| **Domain** | Knowledge Graph |
| **Document version** | 1.2 |

## Reference

Establishes how **durable agent-learned knowledge** — lessons, attempted-and-failed
approaches, decisions, recurring failure patterns, task outcomes — is persisted so
that sessions stop re-learning what earlier sessions already established. Such
knowledge is **not** a fact extracted from source and does **not** get a provenance
class: it lives in its own artifact store, leaving `derived | authored | inferred`
(`crates/rto-graph/src/provenance.rs`) untouched.

This is the **sibling** of
[[docs/adr/0012-analyzer-findings-artifact-model.md]], and applies the same
structural rule: *knowledge that is not a derived/authored/inferred graph fact
gets its own artifact store, and never borrows the graph's trust.* Governed by the
determinism principles of
[[docs/adr/0001-build-roteiro-unified-codebase-knowledge-graph.md]].

## Summary

- **Two tiers with opposite rules**, because they have opposite recovery costs:
  - **Episodic memory** — not re-derivable (*"this refactor failed because X"*).
    **Unbounded, never auto-evicted.** Follows the `imports` precedent exactly.
  - **Transient cache** — re-derivable by definition. **Bounded and freely
    evictable**, because the worst case is recomputing it.
  - The rule: **re-derivable ⇒ evictable; episodic ⇒ never silently evicted.**
- **Deprecation is by evidence first, clock second.** A record whose anchor blob
  no longer matches is stale *by evidence*; a record explicitly superseded is
  stale *by fact*. Age is only a tiebreak between records that are otherwise
  equally valid.
- **Decay is computed at retrieval time, never stored.** A stored score that
  ticks down would rewrite the store on every read.
- **Supersession is recorded explicitly**, not inferred from age.
- **Nothing enters `nodes`/`edges`.** The graph stays a pure function of source
  and `GraphArtifact` stays byte-identical.
- Memory lives in **`.git/roteiro/`** — per-clone, never committed, never pushed.
- **`EXTRACT_VERSION` does not change**; two append-only migrations are required.

## Context

Roteiro currently has **no durable record of an agent's past sessions or
outcomes** — searching `session|conversation|history` across the workspace finds
only a test name, a fixture string, and rmcp's HTTP session manager. `roteiro
review` computes and prints, persisting nothing. Every session therefore starts
cold and re-derives knowledge that previous sessions already paid for.

Two constraints shape the answer.

**1. The determinism contract.** The graph rebuilds identically from source;
`AGENTS.md` states derived extraction is a pure function of
`(path, blob id, bytes)`. Memory is accumulated, session-dependent, and
unreproducible. It can only coexist with that contract by being *outside* it.

**2. No *persisted* store is bounded — but an eviction idiom does exist, and we
follow it.** Nothing in SQLite is capacity-managed: `node_context`
(`migrations.rs:94-100`) is `(key, fingerprint, json)` with no timestamp, no hit
counter, no TTL and no LRU; `ObjectCache` (`cache.rs`) has no delete method at all
and *orphans* stale entries by folding `EXTRACT_VERSION` into the key rather than
deleting them; telemetry rotates by time only, with size-based rotation explicitly
deferred (`telemetry.rs:81-82`). For *persistent* staleness the house idiom is
**content-addressed key invalidation, not expiry**.

However, Roteiro already has a working eviction policy in memory:
`rto-llama`'s `ModelCache { budget_bytes, loaded }` with `lru_evict_count`
(`crates/rto-llama/src/llama.rs:120-137`) evicts oldest-first until the resident
set fits a **byte budget**, always keeping at least the most-recently-used entry
(a budget of `0` therefore keeps exactly one). Its behaviour is pinned by
`tests::budget_evicts_oldest_until_it_fits`.

This ADR therefore does **not** invent a policy: it **ports an existing one to
disk**. The consequences are concrete — the cache tier is bounded by *bytes*
rather than row count, and inherits the always-keep-the-MRU rule, so a session's
just-written entry can never be evicted by its own sweep.

**3. There is no clock to rank by.** Only two timestamps exist — `imports.imported_at`
and `schema_migrations.applied_at` — and **neither is read by any query**; no
`ORDER BY` touches a timestamp anywhere. There is no run id, generation counter,
or logical clock. Worse, the store is per-repo and shared across worktrees and
branches, so concurrent checkouts produce non-monotone wall-clock, and SQLite's
second-granular `datetime('now')` ties on intra-second writes. Any wall-clock
ranking would make results non-deterministic for a fixed repo state.

## Decision makers

The Roteiro Project Team.

## Recommended option

### Tier 1 — episodic memory (durable, unbounded)

Models exactly what `imports` models: knowledge with **no generating function**.
`imports` is persisted precisely because `sync`'s rebuild would otherwise destroy
it (`migrations.rs:76-84`), and is re-applied after every rebuild. Agent memory
has the same property and takes the same treatment: it survives `rebuild`, and is
deleted **only by explicit user command**.

Bounding this tier would be data loss, not cache management.

### Tier 2 — transient cache (bounded, evictable)

Re-derivable knowledge only: context bundles, embeddings, analyzer findings over
current code, query results. Safety comes free — `build_context` is proven to
reconstruct identically (`context.rs:445-453` asserts `built == cached`), so
eviction costs cycles, never information.

Bound by a **byte budget**, following `ModelCache` (`llama.rs:120-137`) rather
than inventing a row-count cap: entries vary hugely in size, and bytes are what
actually constrain `.git/roteiro/`. Evict oldest-first on
`(anchor_valid ASC, last_used ASC)` until the tier fits, **always keeping at least
the most-recently-used entry**. No *persisted* access tracking exists today, so
`last_used`, `hits` and `bytes` are introduced with the table. Sweep at the
existing maintenance seam where `refresh_contexts` is already called
(`main.rs:2508`) — **not on the read path**, so reads stay non-mutating.

**Never evict:** anything in Tier 1, and any Tier 2 row whose anchor is still
valid *and* was written in the current generation.

### Depreciation: evidence first, clock last

Ranking is computed at query time:

```
score = base_confidence × anchor_penalty × decay(current_generation − row.generation)
```

- **`anchor_penalty` dominates.** Each record stores `(anchor_key, anchor_blob)`.
  On read, `get_node(anchor_key)` absent ⇒ the anchor vanished; a differing
  `blob_hash` ⇒ the code changed underneath. This mirrors the fingerprint check
  at `context.rs:110-122`. Note **`span` is byte offsets** (`extract.rs:2051-2052`)
  and shifts on any edit above it — `blob_hash + key` is the stable anchor, span
  is not.
- **Anchor drift demotes, it never deletes.** The authored layer prunes links to
  vanished symbols; memory must not. *A lesson about a deleted function is often
  the most valuable thing you have* — so an unanchored record is marked
  **unanchored**, kept, and ranked lower. This is a deliberate departure from the
  house pruning rule and the main reason memory cannot live in the graph.
- **`decay(·)` is a pure ranking function** over a monotonic generation counter,
  offered as `linear | exponential | none`, with **`none` guaranteeing
  reproducible recall**.

**Ordering key.** Not wall-clock. `INTEGER PRIMARY KEY AUTOINCREMENT` supplies a
free, skew-proof monotonic generation, and `sync_state.tree` (`migrations.rs:56-61`)
is recorded as the repo-state witness. `created_at` is retained for human display
only — written but not read, exactly as `imported_at` is today.

### Scope: a namespace, with the anchor as the validity test

*Added in v1.1, settling the question v1.0 left to the reviewer.*

The store is per-repo and shared across branches, worktrees and clones, so: **is a
lesson learned on a feature branch valid on `main`?** The rule is:

> A lesson learned on a feature branch is valid on `main` **only if the relevant
> association is merged to `main` in the same format** — if not, then no.

**This needs no new machinery: the anchor already is that test.** Validity is not
a property of the branch that wrote a record. It is whether the record's anchor
resolves in the tree being looked at, which is exactly what `anchor_penalty`
above computes:

| Anchor resolves against this tree | Meaning | Applies here |
|---|---|---|
| present, blob matches | merged, in the same format | **yes** |
| present, blob differs (`drifted`) | merged in a *different* form | no |
| absent (`vanished`) | not merged | no |
| present, blob unmeasurable (`unverifiable`) | cannot be shown | no |
| **no anchor recorded at all** | a general lesson about the repo | **yes, everywhere** |

"Is this valid on `main`?" is answered by resolving the anchor against `main`'s
graph — and the identical mechanism answers it on any branch, worktree or clone,
with **no branch bookkeeping anywhere**. Nothing is stored to make this work, and
the verdict is recomputed on every read, so it is always about the tree in front
of you rather than the tree that existed when the record was written.

Three consequences worth stating, because each is a place the rule could be
weakened by accident:

- **"In the same format" means the blob matches**, deliberately strictly: even a
  pure reformat breaks the association. This fails toward *marked drifted* rather
  than toward silently applying a lesson to code that has moved on, and that
  asymmetry is the point — a false "does not apply" costs a re-read, a false
  "applies" costs a wrong decision.
- **`unverifiable` does not apply.** Where the blob cannot be compared, the
  association cannot be shown to be present in the same format, so the strict
  reading holds. It is nonetheless kept distinct from `drifted`, which asserts
  something stronger (the code *did* move).

  The implementation flagged this state as its own reading rather than something
  the rule stated, since a node carrying no blob hash was measured neither way.
  **The strict reading was put to the decision-makers and ratified**: a memory
  that cannot be *shown* to apply is kept, marked, and reported as not applying
  here, rather than quietly asserted against code nobody verified. Recorded so it
  is not re-opened as an oversight; the lenient reading remains a one-line change
  to `AnchorState::applies` if the trade is ever revisited.
- **A record with no anchor is repo-wide and always applies** — "CI is
  Ubuntu-only" is true wherever the repository is. This must never be conflated
  with an anchor that failed to resolve: they both lack a usable anchor and they
  have *opposite* answers, so they are separate states.

**Therefore `scope` is a coarse namespace** — which repo or project a record
belongs to in a multi-repo workspace ([[docs/adr/0008-multi-repo-workspace-serve.md]])
— and is **not a branch label**. Nothing keys off it but an exact-match filter: no
isolation, no inheritance, no merging. Giving it a second, branch-shaped job would
produce two answers to one question, and the branch-shaped answer would be the
wrong one: a lesson does not become false because the branch that learned it was
deleted, nor true because it was merged.

None of this changes the depreciation model above; it names what that model was
already deciding.

### Supersession, recorded not guessed

New research overruling old is expressed **explicitly** via a nullable
`superseded_by` self-reference plus `superseded_at`, so a superseded record drops
out of live recall immediately regardless of age, and the chain stays auditable.

This finally gives `EdgeKind::Supersedes` a live analogue — today it exists in the
enum (`model.rs:106-107`) but is **produced by nothing**, referenced only by a
round-trip test. Per the standing decision it stays **inside the artifact store
and never becomes a graph edge**.

### Proposed shapes

```sql
-- Migration 8 — episodic, durable, never auto-evicted
CREATE TABLE agent_memory (
  id INTEGER PRIMARY KEY AUTOINCREMENT,   -- monotonic generation/seq
  scope TEXT NOT NULL, kind TEXT NOT NULL,
  anchor_key TEXT, anchor_blob TEXT, anchor_path TEXT,
  body TEXT NOT NULL, confidence REAL,
  tree TEXT, created_at TEXT NOT NULL DEFAULT (datetime('now')),
  superseded_by INTEGER REFERENCES agent_memory(id), superseded_at TEXT);
CREATE INDEX idx_mem_anchor ON agent_memory(anchor_key);
CREATE INDEX idx_mem_live ON agent_memory(scope, superseded_by, id DESC);

-- Migration 9 — transient, bounded, evictable
CREATE TABLE agent_cache (
  key TEXT PRIMARY KEY, fingerprint TEXT NOT NULL, json TEXT NOT NULL,
  bytes INTEGER NOT NULL, generation INTEGER NOT NULL,
  last_used INTEGER NOT NULL, hits INTEGER NOT NULL DEFAULT 0);
CREATE INDEX idx_cache_evict ON agent_cache(last_used);
```

**Two migrations, not one** — different lifetimes and different guarantees, and
since `migrations.rs:8` mandates append-only SQL, splitting them means the
eviction tier can later be altered without touching durable memory.

### Trust, placement, privacy

- **Never boosted as `authored`.** `authored` means *a human or agent deliberately
  wrote this in a reviewed file* and carries **+40 relevance** in `search`
  (`query.rs`). Unreviewed accumulated memory riding that boost is trust-model
  contamination by construction. If memory surfaces in `search` at all it needs a
  visually distinct channel and its own score.
- **Per-clone, uncommitted.** Lives in `.git/roteiro/` beside `graph.db`
  (`main.rs:2872-2875`), consistent with the fact that **nothing Roteiro generates
  is committed today**.
- **Privacy forces that placement.** Extraction redacts secret-looking config
  values *before* persistence because the store is exportable
  (`extract.rs:304-320`). Memory has **no such chokepoint** — it records prose an
  agent wrote, which can contain pasted tokens, stack traces, or customer names.

## Options considered + consequences

| Option | Verdict |
|---|---|
| New `Provenance` variant for memory | **Rejected** — memory has no source blob; it would break the pure-function-of-source promise outright. |
| `NodeKind::Other("memory")` in `nodes`/`edges` | **Rejected** — works mechanically, which is the trap: inherits `provenance DEFAULT 'derived'` and leaks into `export_factset`. |
| Authored-with-metadata | **Rejected** — inherits the +40 `authored` boost for unreviewed content. |
| Single unified table with TTL | **Rejected** — one policy cannot serve both recovery costs; TTL on episodic data is data loss. |
| **Two-tier artifact store (chosen)** | Matches recovery cost to eviction policy; keeps the graph pure. |

## Consequences

**Positive**

- Sessions stop re-deriving established knowledge; the cache tier pays for itself
  in cycles with no information risk.
- The graph still rebuilds identically; `GraphArtifact` stays a pure function of
  the tree.
- Depreciation tracks *evidence* (anchor drift, supersession), so stable truths
  stay ranked and superseded ones drop out immediately.
- Reads never mutate the store; `decay = none` gives fully reproducible recall.

**Negative / costs**

- Roteiro's first **persisted** capacity-based eviction policy. The policy itself
  is not new (`ModelCache` already does byte-budget LRU in memory), but durable
  eviction adds the `last_used`/`hits`/`bytes` tracking that no table currently
  carries.
- Two migrations and a second retrieval surface.
- Anchor-drift marking (rather than pruning) deliberately departs from the
  authored layer's rule and must be explained wherever memory is surfaced.
- Unbounded episodic growth is accepted by design; only explicit user commands
  reclaim it.

## Status

For Review. This ADR is itself the smallest useful step, since it forecloses the
cheap `NodeKind::Other("memory")` shortcut that someone will otherwise take.

**Both halves are now delivered.** Stage 23 shipped the episodic tier — migration
11, the `agent_memory` store, `roteiro memory add|list|forget`. Stage 25 shipped
the rest: migration **12** (`agent_cache`, plus the single-row `agent_cache_clock`
its counters are drawn from), retrieval-time ranking, the byte-budget sweep at the
`refresh_contexts` maintenance seam, and a **third `search` channel** for memory
that is off by default and takes no `authored` boost.

**Settled since v1.0:** the `scope` question (v1.1) — scope is a namespace, and
applicability is decided by anchor resolution. **The cache byte budget** is
answered: **256 MB by default, raisable** via `ROTEIRO_CACHE_BUDGET_MB` or
`--budget-mb` (build plan §9.1, decided by the owner). Nothing is left open.

**Where the implementation went beyond this ADR** — none of it reversing a
decision here, all of it recorded in the build plan's Stage 25 entry:

- **The logical clock.** The proposed `agent_cache` names `generation` and
  `last_used` without saying where the values come from, and §3 above rules out
  wall-clock. They are drawn from `agent_cache_clock`: `ticks` advances per
  access, `generation` once per sweep. The split is what makes "written in the
  current generation" a *window* rather than a single row, and what makes the
  never-evict pin lapse instead of becoming permanent.
- **`decay = none` is the default**, not merely offered.
- **`base_confidence` defaults to `0.5`** when a writer states none — the midpoint
  of the range a writer can state, so claiming high evidence promotes and claiming
  low evidence demotes, both relative to silence.
- **`anchor_penalty` ranks `drifted` below `vanished`.** Drift is the state that
  can mislead about code still under the same key; ranking vanished lowest would
  have punished exactly the records this ADR says are worth keeping most.
- **A sweep can finish over budget** and reports it. The always-keep-the-MRU rule
  and the current-generation pin can between them leave nothing evictable, which
  is this ADR's own rule; what it does not say is that a bound which fails to bind
  must say so, and `CacheSweep::over_budget` does.
- **The cache tier has no producer yet.** `node_context` remains the context
  cache; moving it onto the bounded tier is a data migration rather than a policy
  change, and was deliberately not bundled with the policy.

## Document version history

| Version | Date | Notes |
|---------|------|-------|
| 1.0 | 2026-08-15 | For Review. Two-tier artifact store for durable agent-learned knowledge: episodic (unbounded, never auto-evicted, following the `imports` precedent) and a byte-budget-bounded transient cache (porting `rto-llama`'s `ModelCache` LRU to disk). Depreciation by evidence — anchor drift and explicit supersession — not by clock; decay computed at retrieval, never stored. Nothing enters `nodes`/`edges`; no `Provenance` variant; `EXTRACT_VERSION` unchanged. Rejects `NodeKind::Other("memory")`, authored-with-metadata, and a single TTL'd table. Left two questions to the reviewer: the cache byte budget, and what `scope` means. |
| 1.2 | 2026-08-16 | **Both tiers delivered; the last open question answered.** The cache byte budget is **256 MB by default, raisable** (`ROTEIRO_CACHE_BUDGET_MB`, `--budget-mb`), so nothing is left open. Records where the Stage 25 implementation went beyond this ADR without reversing it: a single-row `agent_cache_clock` supplies the `generation`/`last_used` counters the proposed table named but did not source (§3 rules out wall-clock — `ticks` advances per access, `generation` once per sweep, which is what makes the never-evict pin a lapsing window rather than a permanent one); `decay = none` is the *default* rather than merely offered; `base_confidence` defaults to the midpoint `0.5` when a writer states none, so stating one is worth the trouble in both directions; `anchor_penalty` ranks `drifted` **below** `vanished`, because drift is the state that can mislead about code still under the same key and ranking vanished lowest would punish the records this ADR most wants kept; a sweep that finishes over budget (everything left pinned — this ADR's own rule) reports it rather than leaving a bound that silently failed to bind; and the tier ships with its policy and seam but **no producer**, since moving `node_context` onto it is a data migration rather than a policy change. No decision from 1.0 or 1.1 changed. |
| 1.1 | 2026-08-15 | **Scope settled** (new §*Scope*): a lesson is valid in a tree only if the relevant association is present there *in the same format*, so **the anchor is the scope test** and `scope` is a coarse per-repo/project **namespace, never a branch label**. Records what "same format" means (the blob matches — strictly, so a reformat breaks it and the failure is toward *marked*, not toward silently applying), that `unverifiable` therefore does not apply, and that a record with **no anchor** is repo-wide and must never be conflated with an anchor that failed to resolve. No new machinery: this names what the existing anchor check was already deciding, and no decision from 1.0 changed. Also records the episodic tier as delivered. |

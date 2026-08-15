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
version: "1.0"
last-modified: 2026-08-15
confluence-url:
---

# ADR-0013: Agent memory — a two-tier artifact store, decaying by evidence not by clock

| | |
|---|---|
| **State** | For Review |
| **Architectural Significance** | HIGH |
| **Domain** | Knowledge Graph |
| **Document version** | 1.0 |

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
  superseded_by INTEGER REFERENCES agent_memory(id), superseded_at INTEGER);
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
Execution is planned in [BUILD_PLAN_V2](../BUILD_PLAN_V2.md) Stage 23; the first
code PR is the episodic migration plus `roteiro memory add|list` — write path
only, no retrieval, no graph integration.

**Open for the reviewer to settle** (deliberately not decided here): the cache
tier's byte budget *value* (the unit follows `ModelCache`; the number is a
judgement about tolerable `.git/roteiro/` growth), and whether `scope` isolates
memory per branch/worktree or shares it repo-wide.

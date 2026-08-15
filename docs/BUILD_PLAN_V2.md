# Roteiro — Build Plan V2

Status: Active · Owner: The Roteiro Project Team · Last-modified: 2026-08-15
Governing decisions: [ADR-0001](adr/0001-build-roteiro-unified-codebase-knowledge-graph.md),
[ADR-0012](adr/0012-analyzer-findings-artifact-model.md),
[ADR-0013](adr/0013-agent-memory-artifact-store.md),
[ADR-0014](adr/0014-sandboxed-analyzer-execution.md)

This plan succeeds [BUILD_PLAN.md](BUILD_PLAN.md), which took Roteiro from the
v0.0.1 scaffold through Stage 20 to the released v1.9.0. V2 covers the next arc:
**Roteiro learns things that are not in the source tree — and stores them without
compromising the promise that the graph is a pure function of the source tree.**

Stage numbering continues from the v1 plan (which ended at Stage 20). As there,
stage numbers are **labels, not execution order**, and the `v1.x`/`v2.0` headings
are *nominal targets* — release-plz cuts the real tags from conventional commits,
so a stage nominally marked `v1.16.0` may actually ship in a `v1.10.x`. Each
delivered stage records **the version it really shipped in**.

> **Keep this document current.** A stage is not finished when its code merges —
> it is finished when its entry here says what shipped, in which release, and what
> that settles for later stages. The stage entry is where the next person looks
> first; a plan that lags the tree is worse than no plan, because it is trusted.
> Update it in the same PR as the work wherever possible.

---

## 1. Thesis of V2

V1 built one thing well: a provenance-tagged graph, deterministically derived from
git blobs, that humans and agents query through one surface. V2 adds three kinds of
knowledge that **do not fit that model** and would corrupt it if forced in:

1. **Analyzer findings** — asserted by an external tool at a point in time, against
   rules and advisory databases that change independently of the source.
2. **Agent memory** — accumulated across sessions, episodic, unreproducible, and
   often the record of something that *failed*.
2b. **Generated media content** — ASR transcripts and VLM descriptions, invented
   fluently when the source contains nothing to read (ADR-0015, Stage 28).
3. **Deeper analysis lenses** — genuinely derived facts, which stay in the graph,
   but whose true cost was previously understated by an order of magnitude.

The organising rule for (1) and (2) is one sentence, and it is what makes V2
coherent rather than a list of features:

> **Knowledge that is not a derived/authored/inferred graph fact gets its own
> artifact store, and never borrows the graph's trust.**

`imports` already works this way — it exists precisely because `sync`'s rebuild
would destroy it, and is re-applied afterwards. V2 generalises that precedent
instead of inventing something new.

---

## 2. Principles

All seven principles of [BUILD_PLAN.md §1](BUILD_PLAN.md) remain binding. V2 adds
three invariants that constrain every stage below:

8. **The graph stays a pure function of source.** Nothing in V2 writes to
   `nodes`/`edges` unless it is deterministically derived from `(path, blob id,
   bytes)`. `export_factset` must remain byte-identical for a given tree.
9. **Artifact stores never borrow graph trust.** No V2 record acquires the
   `authored` relevance boost, and none is exported in the `GraphArtifact`.
10. **Offline-capable, not "offline".** Optional capabilities may require
    pre-provisioned assets; they must be digest-pinned, explicitly prefetched, and
    must fail with a named, actionable error rather than fetching implicitly or
    silently degrading.

---

## 3. Baseline (start of V2)

Verified against `main` at the time of writing:

| Fact | Value | Consequence for V2 |
|---|---|---|
| Released | **v1.10.1** on crates.io (baseline at V2's start: v1.9.0) | V2 work is post-1.0 — semver is now real. |
| MSRV | `rust-version = "1.94"` | New deps must respect it. |
| Lints | `unsafe_code = "forbid"`, clippy pedantic `-D warnings` | Native/FFI deps must be isolated behind a feature. |
| Coverage | 85% per-file ratchet | Every stage below carries test cost, not just code cost. |
| CI | Ubuntu-only, `--all-features` | `/dev/kvm` may be absent; Apple Silicon untested. |
| Schema | **migrations 1–11 applied** (1–7 at V2's start) | V2 appends only; see §5. |
| `EXTRACT_VERSION` | **`10`** (`crates/rto-graph/src/extract.rs`) — bumped once by Stage 28 | Bumping it forces full re-extraction for every user. |
| Provenance | `Derived | Authored | Inferred`, CHECK-constrained | Unchanged by V2, by decision. |
| Eviction idiom | in-memory byte-budget LRU (`rto-llama` `ModelCache`); **nothing persisted is bounded** | Stage 25 ports the existing policy to disk rather than inventing one. |

---

## 4. Crate & feature map

| Crate | Change | Notes |
|---|---|---|
| `rto-exec` | **new** | `AnalyzerRunner` trait + three backends (ADR-0014). Feature `execution`, subfeatures `exec-boxlite`, `exec-subprocess`. |
| `rto-graph` | extended | Artifact-store tables + accessors; new query fns for lenses. Graph model untouched. |
| `roteiro` (CLI) | extended | `security {prefetch,status,run,ingest}`, `memory {add,list,recall,forget}`, new lens subcommands. |
| `rto-serve` | extended | New lenses surfaced to served-chat tools; memory recall exposed only behind explicit opt-in. |
| `rto-render` | extended | Findings and lens renderers. |

Default install gains **no new dependency**. Everything in Stages 22/24 is
feature-gated and off by default.

---

## 5. Schema plan — migration discipline

`migrations.rs` mandates append-only SQL: never edit a shipped migration. V2 adds
**three** tables across three migrations, deliberately not merged:

| Migration | Table | Lifetime | Evictable |
|---|---|---|---|
| **8** ✅ | analysis runs + findings (ADR-0012) | replaceable layer per `(analyzer, worktree)` | replaced wholesale, not aged out |
| **11** ✅ | `agent_memory` (ADR-0013 episodic) | durable, survives `rebuild` | **never** |
| N | `agent_cache` (ADR-0013 transient) | bounded | yes, by capacity |

**Numbers are assigned in landing order, not reserved in advance.** Stage 21 landed
first and took **8**; Stage 28 took **9** and **10**; episodic memory took **11**,
and the cache tier takes the next free number whenever it merges. Splitting memory
across two migrations is intentional: different lifetimes and guarantees, so the
eviction tier can later be altered without touching durable memory.

**Stages 22 and 24 need no migration** — `RunnerKind` shipped in migration 8 already
naming all three backends, with the schema CHECK accepting them.

**`EXTRACT_VERSION` does not change in Stages 21–25.** None of that work is
extraction output (Stage 21 shipped without touching it, as required). It *does*
change in Stage 26, once — see the note there.

---

## 6. Staged roadmap

Dependency shape — four tracks, only one hard chain:

```
Track A (findings):  21 ✅ ──► 22 ──► 24
Track B (memory):    23 ✅ ──────────► 25
Track C (lenses):    26      (independent of A and B throughout)
Track D (media):     28 ✅ ──► 29
                                       └──► 27 (v2.0 hardening)
```

Stages 21, 23 and 26 can start in parallel. Nothing in Track C touches the artifact
stores; nothing in Track B blocks Track A.

---

### Stage 21 — Analyzer contract & ingest ([ADR-0012](adr/0012-analyzer-findings-artifact-model.md), [ADR-0014](adr/0014-sandboxed-analyzer-execution.md)) → **v1.10.0** · effort **S** ✅ *delivered*

**Goal:** land the whole value of the findings design with **no analyzer and no
sandbox** — the seam, the schema, and a working ingest path. This is the stage that
makes CI ingestion and local execution the same code path.

- **Rust surface:** new `rto-exec` crate; `AnalyzerRunner` trait (request: analyzer
  id, read-only worktree, `network: Deny`, explicit consent → response: normalized
  findings + evidence); `IngestRunner` as the first implementation; normalized
  `Finding` + `AnalysisRun` types in `rto-graph`.
- **Schema:** migration N — analysis runs + findings, with stable finding identity
  keys (`finding:semgrep:<rule>:<path>:<start-byte>:<snippet-hash>`,
  `finding:cargo-audit:<advisory>:<pkg>:<version>:<lockfile-blob>`) and layer
  replacement keyed `security:<analyzer>:<worktree-id>`.
- **CLI:** `roteiro security ingest <normalized-json>`, `roteiro security list
  [--json]`.
- **Deps:** none beyond serde. No feature flag needed for ingest.
- **Known gap to implement, not assume:** existing import code deletes edges but
  **not obsolete owned nodes**; owned-record cleanup on layer replacement is
  net-new work and is part of this stage's DoD.
- **DoD:** ingesting a report twice is idempotent; a finding fixed between runs
  *disappears* on replacement; `export_factset` output is byte-identical before and
  after ingest (regression test); no new `nodes`/`edges` rows exist; findings never
  appear in `search` results ranked as `authored`.

**Delivered in v1.10.0** (PR #293). What shipped, and what it changes for later
stages:

- `rto-exec` with `AnalyzerRunner` + `IngestRunner`. The preflight (`check_request`)
  deliberately sits **outside** the trait, so a later backend cannot quietly skip
  the consent/worktree checks.
- **Migration 8** (`analysis_runs` + `findings`), appended; migrations 1–7 untouched.
- `FindingKey` = `finding:<analyzer>:<analyzer's own ordered identity components>`
  with escaping. The two analyzers named above are therefore **examples, not
  schema** — a new analyzer needs no migration.
- **`RunnerKind` already names all three backends** and the schema CHECK accepts
  them, so **Stages 22 and 24 need no further migration.**
- `execution` ships as a **default feature**, reconciling this plan's "behind a
  feature" with ADR-0014's "ingest is always available": a named seam for later
  stages that is nonetheless present in a stock install. `--no-default-features`
  builds with no analyzer surface.
- Owned-record cleanup was implemented, not inherited: `replace_findings_layer`
  deletes the previous run's finding rows explicitly (cascade is defence in depth),
  and `Store::orphan_finding_count()` exists so tests assert zero orphans directly.
- Every DoD item above is pinned by a test; the artifact invariant was additionally
  verified end-to-end through the CLI (ingest → list → re-ingest → removal), with
  `nodes`/`edges` counts and the exported artifact digest unchanged throughout.

### Stage 22 — First analyzers: `cargo-audit`, then `semgrep` → **v1.11.0** · effort **M + M**

**Goal:** two real analyzers behind the Stage 21 contract, via the subprocess
runner, honestly labelled.

- **Rust surface:** `SubprocessRunner` (feature `exec-subprocess`); per-analyzer
  adapters normalising native output into `Finding`.
- **CLI:** `roteiro security run <analyzer> [--allow-unsandboxed]`. The flag is
  **required** for subprocess execution; evidence records `isolation=none`.
- **Provisioning:** `roteiro security prefetch` / `status` land here — digest-pinned
  advisory DB and rule sets, with `assets-unavailable-offline` as the cold-cache
  failure (never an implicit fetch, never a silent host-tool fallback).
- **Staleness honesty:** a cached-but-old advisory DB still runs, but results carry
  `advisory_db_published_at`, `fetched_at`, age, and a *possibly stale* label.
- **DoD:** both analyzers produce identical normalized findings from the same
  inputs whether run locally or ingested from CI; offline with a warm cache
  succeeds; offline with a cold cache fails with the named error and the exact
  prefetch command; `cargo deny` clean.

### Stage 23 — Agent memory, episodic tier ([ADR-0013](adr/0013-agent-memory-artifact-store.md)) → **v1.10.x** ✅ *delivered*

**Goal:** stop losing what sessions learn. Write path only — no retrieval ranking,
no graph integration.

- **Rust surface:** `agent_memory` accessors in `rto-graph`; anchor capture as
  `(anchor_key, anchor_blob, anchor_path)`; explicit `superseded_by` /
  `superseded_at`. **`span` is not an anchor** — it is byte offsets and shifts on
  any edit above it; `blob_hash + node_key` is the stable pair.
- **Ordering:** `INTEGER PRIMARY KEY AUTOINCREMENT` supplies the monotonic
  generation. `created_at` is written for humans and **never read** — matching how
  `imported_at` already behaves. No wall-clock ranking, because the store is shared
  across worktrees and branches and `datetime('now')` is second-granular.
- **Storage location:** `.git/roteiro/` beside `graph.db` — per-clone, never
  committed, never pushed. Privacy forces this: extraction redacts secret-looking
  config values before persistence, and memory has **no such chokepoint**.
- **CLI:** `roteiro memory add|list|forget`.
- **DoD:** memory survives `roteiro sync`/`rebuild` (the `imports` property);
  `export_factset` unchanged; nothing enters `nodes`/`edges`; supersession recorded
  explicitly and superseded rows excluded from live listing.

**Delivered in #317.** Migration 11 (`agent_memory`), the `rto_graph::memory`
store, and `roteiro memory add|list|forget`. Every DoD item above has a test;
`EXTRACT_VERSION` is unchanged and asserted so, because memory is not extraction
output.

**Four deviations from ADR-0013's proposed SQL**, each deliberate:

1. **`kind` is a closed `CHECK … IN`** over the ADR's own five names
   (`lesson|attempt|decision|pattern|outcome`), not the free `TEXT` proposed. Free
   text makes `lesson`/`Lesson`/`lessons` three kinds, none findable by a filter,
   and a vocabulary that cannot be filtered cannot later be ranked — which Stage 25
   needs. Follows the `analysis_runs.runner`/`isolation` and `media_content.kind`
   precedent. *Cost:* a sixth kind is an append-only migration, not a string.
2. **`superseded_at` is `TEXT`, not `INTEGER`.** An integer here would hold the
   generation of supersession — which *is* `superseded_by`, since the successor's
   id is the generation — so it would duplicate the column beside it. As `TEXT` it
   is a human timestamp on `created_at`'s terms: written, displayed, never read.
3. **Four extra `CHECK`s** making half-states unrepresentable, per migration 10's
   precedent: `superseded_by`/`superseded_at` stand or fall together (a moment with
   no successor is supersession *inferred*, the one thing the ADR rules out);
   nothing supersedes itself; anchor evidence requires an anchor key; empty
   scope/body refused.
4. **`AUTOINCREMENT` kept, and it is load-bearing** — not decoration. A plain
   `INTEGER PRIMARY KEY` is the rowid, and SQLite reuses the largest deleted one,
   so forgetting the newest record would hand its number to the next write:
   `ORDER BY id DESC` stops being newest-first *and* a surviving `superseded_by`
   silently re-points at an unrelated record.

**Scope is settled, so Stage 25 inherits it rather than re-litigating it**
(ADR-0013 v1.1 §*Scope*). The owner's rule: *a lesson learned on a feature branch
is valid on `main` only if the relevant association is merged to `main` in the
same format — if not, then no.* That needs no new machinery, because **the anchor
is the scope test**: a record applies to a tree when its anchor resolves there
with the same blob, or when it has no anchor at all (a general lesson, repo-wide).
Drifted, vanished or unverifiable ⇒ does not apply *here*, kept and marked.
"Same format" means the blob matches, strictly — a reformat breaks it, failing
toward *marked* rather than toward silently applying a lesson to code that moved.
Consequently **`scope` is a coarse per-repo/project namespace and never a branch
label**; no branch bookkeeping exists anywhere in the schema. Recall in Stage 25
should rank on this predicate (`AnchorState::applies`), not invent a second one.

**Out of scope, still:** the bounded cache tier, recall ranking, decay, and any
`search` integration — all Stage 25. Memory currently reaches `search` through no
channel at all, which is asserted rather than assumed.

### Stage 24 — boxlite sandboxed backend ([ADR-0014](adr/0014-sandboxed-analyzer-execution.md)) → **v1.13.0** · effort **L**

**Goal:** the reproducible, offline-capable local run — one command, pinned inputs,
digest-level evidence.

- **Deps:** `boxlite` (Apache-2.0), **pinned exactly**, behind `exec-boxlite`.
  Publication on crates.io was verified directly (17 versions, default 0.9.7, not
  yanked), so this is a dependency addition, not a packaging problem.
- **Rust surface:** `BoxliteRunner`; digest-pinned OCI image; read-only worktree
  mount, scrubbed environment, no ambient credentials, egress denied by default.
- **CI:** `--all-features` must not fail on a runner without `/dev/kvm` — gate
  sandbox tests on a runtime capability probe and skip with a visible message.
  Apple Silicon microVM execution stays **untested in CI**, documented as an
  accepted gap.
- **Standing duties (from the ADR):** exact pin, deliberate advisory tracking,
  `cargo deny` over the full resolved native/FFI closure.
- **DoD:** the same analyzer produces the same findings via subprocess and via
  boxlite, differing only in the isolation label and image digest; a machine with
  no network but a warm cache produces a full run; `cargo deny` clean on the
  resolved tree.

### Stage 25 — Memory recall: cache tier, decay, supersession → **v1.14.0** · effort **L**

**Goal:** make memory *useful* — recall that ranks by evidence, plus the bounded
cache that stops sessions re-deriving what they already know.

- **The two-tier split is the whole design.** Re-derivable ⇒ evictable; episodic ⇒
  never silently evicted. `build_context` is *proven* to reconstruct identically
  (`context.rs` asserts `built == cached`), which is what makes cache eviction cost
  cycles rather than information.
- **Schema:** migration N (the next free number after **11**) — `agent_cache` with
  `bytes`, `generation`, `last_used`, `hits`. No **persisted** access tracking
  exists today, so the signal must be introduced with the table (the in-memory
  `ModelCache` tracks recency by list order, which does not survive a process).
- **Inherited from Stage 23, do not re-derive:** applicability is already decided —
  `AnchorState::applies` (ADR-0013 v1.1 §*Scope*) is the whole rule, and it is what
  `anchor_penalty` below should be built on. Do **not** add a branch or scope term
  to recall: `scope` is a namespace, the anchor is the validity test, and a second
  rule would give two answers to one question.
- **Eviction:** **byte budget**, following the existing `ModelCache`
  (`crates/rto-llama/src/llama.rs:120-137`) rather than a new row-count cap —
  evict oldest-first on `(anchor_valid ASC, last_used ASC)` until the tier fits,
  **always keeping at least the most-recently-used entry**. Swept at the existing
  maintenance seam where `refresh_contexts` is already called — **not on the read
  path**, so reads never mutate. Never evict: anything episodic, or a
  valid-anchored row written in the current generation.
- **Ranking (retrieval-time, never stored):**
  `score = base_confidence × anchor_penalty × decay(current_generation − row.generation)`
  with `decay ∈ {linear, exponential, none}` and **`none` guaranteeing reproducible
  recall**. A stored decaying score would rewrite the store on every read.
- **Anchor drift demotes, never deletes.** The authored layer prunes links to
  vanished symbols; memory must not — *a lesson about a deleted function is often
  the most valuable thing you have*. Unanchored records are marked, kept, ranked
  lower.
- **Surfacing:** if memory appears in `search` at all it needs a visually distinct
  channel and its own score. It never takes the `authored` +40 boost.
- **DoD:** `decay=none` gives byte-identical recall for a fixed repo state across
  runs; eviction never removes an episodic row; a superseded memory drops out of
  recall immediately regardless of age; an unanchored memory is still retrievable
  and clearly labelled.

### Stage 26 — Analysis lenses (A1) → **v1.15.0** · effort **S–M per lens** *(independent track)*

**Goal:** deepen the graph itself — the on-brand work — with **honest costs**.

**Cost correction, which this stage exists to respect:** a fully surfaced lens is
**~195–500 LOC across 6–8 files**, not the ~20-line mirror previously assumed. That
figure describes only the internal query fn. There are **seven** surfacing stages,
not four: extraction (`scan_markers` + `augment`), the query fn, the query result
types, **MCP** (`GraphServer`) and **served-chat** (`GraphToolRegistry`) as
*separate* registries, Obsidian render, and CLI-side aggregation — plus tests and
docs.

Shortlist, in order:

1. **Q3 — directed coupling** *(the standout)*. `Calls` edges already retain
   direction, and today's hotspot view **throws that away by incrementing both
   ends**. Highest value per line in the set.
2. **Q1 — debt density.** Builds directly on delivered intent-debt tracking.
3. **S1 — config-secret inventory** *(renamed, deliberately)*. Values are redacted
   before persistence, so this lens can report *"secret-named config keys present
   and safely redacted"* with paths and key names. It **cannot** detect hardcoded
   credentials in source, judge validity, or distinguish a real secret from a
   placeholder. The old title promised a scanner that this architecture cannot
   build.

Explicitly **deferred out of this stage**, with reasons:

- **Q2 (LOC hotspots)** is not a pure query — `Node.span` is *byte offsets*, so it
  needs net-new extraction metadata.
- **Q10 (dependency pins)** is mis-scoped — existing pins are Docker `image_ref`
  and submodules; package-manifest pins are extraction work. Split S / M.
- **Q7 (doc coverage)** needs a language and a denominator; docs live mostly in
  symbol `meta.content`, not `Doc` nodes.

**`EXTRACT_VERSION` bump:** required **once**, if and only if a lens adds derived
extraction metadata (Q2 and Q10 do; Q1/Q3/S1 as scoped do not). Bumping invalidates
every cached blob for every user and forces full re-extraction — so batch all
extraction-touching lenses behind a **single** bump rather than paying it twice.

**Also in scope (documentation debt):** normalise the security taxonomy — the prose
defines GDS/NNX/EXT/LLM while rows S1–S6 use undefined GPB/CVE/SAST labels — and
mark the "SmolVLM is too small to emit `<tool_call>`" claim as a **hypothesis**, as
it currently rests on no code or benchmark evidence.

- **DoD per lens:** deterministic output; `roteiro check` green; surfaced on all
  applicable surfaces or explicitly documented as CLI-only; scale-benchmarked on
  this repo (whole-graph lenses matter — `search` already scans all nodes); false
  positives have a suppression story, a confidence signal and a baseline before any
  CI-gating is offered.

### Stage 28 — Generated media content moves out of `derived` ([ADR-0015](adr/0015-generated-media-content-artifact-store.md)) → **v1.10.x** ✅ *delivered* *(independent track)*

**Goal:** stop generative model output masquerading as deterministic extraction —
without losing the ability to search it. Resolves #300.

- **The boundary is generation, not models.** OCR (`ocrs-text`) and PDF text stay
  `derived`: they decode content that *exists in the bytes*, and their errors are
  misreadings correctable against the source. ASR transcripts and VLM descriptions
  move out: they invent fluent text when there is nothing to read.
- **Schema:** a `media_content` store keyed by **source blob id + producer identity**
  (model id + digest, quantisation, mmproj digest, prompt, sampling parameters).
  Re-describing with a better model is a **new record, not a mutation**. Records
  survive `rebuild`, following the `imports` precedent — they are expensive to
  reproduce (a 715 MB projector load per blob, see #301) and not derivable from
  source alone.
- **CLI (ships WITH the move, not after it):** `roteiro media build [--audio]
  [--vision] [--force]` (incremental — only blobs lacking a record for the current
  producer), `media status [--json]`, `media clear [--producer <id>]`.
- **Retrieval:** `roteiro search --include-generated`, **off by default**; when on,
  every hit is visibly marked as generated, ranked in its own channel, and never
  given the `authored` boost. The explorer UI surfaces generated content on a media
  node with its producer and a per-blob rebuild action.
- **Pre-generation gate (in scope):** a cheap, deterministic refusal of inputs with
  nothing to read — peak/RMS below threshold for audio, near-uniform pixel
  variance/entropy for images — evaluated **before the model loads**, so a repo of
  silent or blank assets skips the projector load entirely (a free win against
  #301). The skip is **recorded, not silent**: a `media_content` record states the
  reason and the measured value, so `media status` distinguishes *not generated*
  from *generated nothing*. Conservative, configurable thresholds; `--force`
  overrides. It raises the floor — quiet speech and subtly-textured images still
  confabulate — so it complements the store rather than substituting for it.
- **`EXTRACT_VERSION` bumps here** — extraction output genuinely changes. This is the
  one bump referenced in §5; batch it with Stage 26's extraction-touching lenses if
  they land together, so users re-extract once rather than twice.
- **No migration.** Generated media content is not yet relied on by any consumer, so
  this is a clean cutover: the bump stops the text being written into
  `nodes.meta.content`, nothing is copied into the new store, and records are
  produced on demand by `media build`. No shim, no dual-read, no deprecation window
  — which is only true because it is being done now.
- **Complementary, tracked separately:** the projector cache (#301).
- **DoD:** a silent clip cannot put prose into default `search` results; a silent
  clip is refused *before* the model loads and the refusal is visible in `media
  status` with its measured value; generated text is attributable to a named
  producer everywhere it surfaces; `media build` restores full searchability in one
  command; `export_factset` is byte-identical across a `media build`; dropping a
  producer's records leaves the graph untouched.

**Delivered across two PRs, both merged:**

- **28a** (#310) — the `media_content` store (migration 9), keyed by source blob +
  producer identity; generated text stopped being written into `nodes.meta.content`;
  `EXTRACT_VERSION` 9→10; `media build|status|clear`; `search --include-generated`
  (off by default, always labelled, never the `authored` boost). A later fix
  corrected the `generation` counter to read `MAX(generation)` **before** a `--force`
  delete — a count is wrong under deletion, a max is not.
- **28b** (#312) — the pre-generation gate (migration 10), evaluated **before the
  model loads** and proved so by a test producer that panics if reached; the refusal
  is **recorded with its measured value**, so `media status` distinguishes *not
  generated* from *generated nothing*. Plus the explorer surfacing (attribution +
  a copyable per-blob rebuild command; deliberately not a mutating endpoint, since
  the explorer is llama-free per ADR-0010) and the deferred `media` CLI arg-shape
  tests.

**Closes #300.** Measured on a real repo: `media build --audio` refuses a silent
clip in **0.013 s with no model load**, versus 12.9 s and ~2 KB of confabulated
prose under `--force`.

**Known limit, recorded rather than papered over:** MP3 and FLAC are **not gated**.
They are entropy-coded, so measuring amplitude means decoding — behind the very load
the gate avoids. The gate **abstains**, and abstention is a pass, so those formats
still reach the model. Whether to close that gap with structural parsing (no
dependency) is tracked separately.

### Stage 29 — Audio metadata as `derived` facts ([ADR-0016](adr/0016-audio-metadata-extraction.md)) → *in progress* · effort **M** *(independent track)*

**Goal:** the complement of Stage 28. That stage took *generated* content out of
`derived` because it is invented; this one puts *extracted* content in because it is
present in the bytes — codec, sample rate, bit depth, channels, duration, frame
count and tags, from a **format read with no decoding and no model** (measured
1–100 µs on this repo's own fixtures).

- **Dependency:** `symphonia`, `default-features = false`, codec/container features
  plus the `id3v1`/`id3v2`/`ape` metadata readers — which are separate feature flags
  and are **not** implied by `flac`/`mp3`/`wav`; without them every MP3 tag is
  invisible. Adds **MPL-2.0** to `deny.toml` (file-level copyleft; does not reach
  Roteiro's own source), recorded with its rationale.
- **The new subtlety:** MP3 duration is sometimes *estimated* (Xing/VBRI when
  present, else inferred from bitrate, and only when seekable). A
  deterministic-but-inexact `derived` fact is new here, so duration carries an
  `exact | estimated` marker and **absence is recorded as absence, never a guess**.
- **Out of scope, deliberately:** decoding for ASR (symphonia has no resampler or
  channel mixer), widening `is_audio` (symphonia does not support Opus at all), and
  cross-container duplicate detection (`duplicates` matches on git blob hash, so it
  could never pair).
- **DoD:** identical bytes yield byte-identical facts; one `EXTRACT_VERSION` bump;
  `export_factset` unchanged in shape; tests need **no model**, so they run on CI
  rather than self-skipping.

### Stage 27 — v2.0 hardening & release → **v2.0.0** · effort **M**

- Semver review: query output is explicitly versioned, so new query shapes carry
  semver weight.
- Scale benchmarks for every whole-graph lens and for memory recall.
- Docs: blueprint updated, `docs/JSON_SCHEMA.md` extended for findings + memory,
  every "offline" claim re-audited to say **offline-capable once provisioned**
  where that is the truth.
- Coverage ratchet held at 85% across all new crates; `cargo deny` clean with
  `--all-features` on the resolved native closure.

---

## 7. Milestones → releases

| Release | Contains | Gate |
|---|---|---|
| v1.10.0 ✅ | Stage 21 — analyzer contract + ingest | Artifact byte-identical; ingest idempotent — **met** |
| v1.11.0 | Stage 22 — cargo-audit + semgrep | Offline warm-cache run; named cold-cache failure |
| v1.10.x ✅ | Stage 23 — episodic memory | Survives rebuild; graph untouched — **met**; scope settled (the anchor is the scope test) |
| v1.13.0 | Stage 24 — boxlite backend | Parity with subprocess; `cargo deny` clean |
| v1.14.0 | Stage 25 — recall + bounded cache | `decay=none` reproducible; no episodic eviction |
| v1.15.0 | Stage 26 — lenses Q3/Q1/S1 | `check` green; benchmarked |
| v1.10.x ✅ | Stage 28 — generated media content moves out of `derived` | Silent clip cannot reach default search; `media build` restores searchability — **met** |
| — | Stage 29 — audio metadata as `derived` facts | *in progress* |
| **v2.0.0** | Stage 27 — hardening | Full gates; semver review complete |

---

## 8. Risk register

| Risk | Severity | Mitigation |
|---|---|---|
| A V2 record leaks into `nodes`/`edges` and breaks artifact purity | **High** | `NodeKind::Other("…")` is *mechanically* possible — that is the trap. CI regression test asserting `export_factset` is byte-identical across ingest/memory writes. |
| Unreviewed memory acquires `authored` relevance | High | Separate store, separate ranking channel; assert in tests that memory never scores through the authored path. |
| Memory captures secrets (tokens, stack traces, customer names) | High | Uncommitted `.git/roteiro/` placement; explicit `forget`; documented that memory has no redaction chokepoint. |
| boxlite advisory lands and is missed | Medium | Exact pin + deliberate advisory tracking as a standing duty (ADR-0014). |
| `--all-features` CI fails without `/dev/kvm` | Medium | Runtime capability probe; sandbox tests skip visibly. |
| Unbounded episodic growth | Medium | Accepted by design; explicit user reclamation only. |
| A single-vendor factual claim drives a design | Medium | This plan already survived one: a "boxlite is unpublished, therefore unmergeable" blocker was refuted by direct crates.io checking. Verify checkable externals independently. |
| `EXTRACT_VERSION` bumped twice, forcing two full re-extractions | Low | Batch all extraction-touching lenses behind one bump (Stage 26). |

---

## 9. Open questions (decide before the stage that needs them)

1. **Cache bound value** (Stage 25): the *unit* is settled — a byte budget,
   following `ModelCache`. The *number* is a judgement about tolerable
   `.git/roteiro/` growth and still needs deciding.
2. ~~**Memory scope** (Stage 23)~~ — **answered**, ADR-0013 v1.1 §*Scope*. A lesson
   is valid in a tree only if the relevant association is present there **in the
   same format**, so the **anchor is the scope test** and `scope` is a coarse
   per-repo namespace, never a branch label. Shipped in #317; Stage 25's recall
   ranks on `AnchorState::applies` rather than inventing a second rule.
3. **Semantic recall** (post-Stage 25): memory recall is lexical + anchor + decay in
   this plan. A vector index would need migration, model/dimension versioning,
   retention, rebuild and storage-size policy — materially more than "persist
   embeddings", and deferred deliberately.
4. **`code_interpreter`** remains rejected (ADR-0014). The sharper question behind
   it — *is local code execution something Roteiro wants to be?* — is a product
   decision, not a backend one. If it ever becomes "yes", boxlite is the vehicle and
   Track A rides along; until then the answer stays "no".
5. **Findings ↔ graph cross-surfacing**: joining findings to graph facts is
   deliberately not free in this design. When it is wanted, it needs a designed
   join, not an implicit one.

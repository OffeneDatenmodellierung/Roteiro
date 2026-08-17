# Roteiro — Build Plan V2

Status: Active · Owner: The Roteiro Project Team · Last-modified: 2026-08-15
Governing decisions: [ADR-0001](adr/0001-build-roteiro-unified-codebase-knowledge-graph.md),
[ADR-0012](adr/0012-analyzer-findings-artifact-model.md),
[ADR-0013](adr/0013-agent-memory-artifact-store.md),
[ADR-0014](adr/0014-sandboxed-analyzer-execution.md),
[ADR-0018](adr/0018-analyzer-coverage-matrix.md)

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
| Released | **v1.15.0** on crates.io, all seven crates | V2 work is post-1.0 — semver is now real. |
| MSRV | `rust-version = "1.94"` | New deps must respect it. |
| Lints | `unsafe_code = "forbid"`, clippy pedantic `-D warnings` | Native/FFI deps must be isolated behind a feature. |
| Coverage | **measured in CI, not gated** — `cargo llvm-cov` runs non-blocking; the 85% per-file floor is an aspiration (ADR-0001), never an enforced check (issue #319) | Every stage below still carries test cost, but a DoD may not cite "85% coverage" as if something verified it. |
| CI | Ubuntu-only; `--all-features` **and** the default set (the `default-features` job, added by #364 after the default set was found not to compile — issue #360) | `/dev/kvm` may be absent; Apple Silicon untested. Turning features *on* cannot find defects caused by code being cfg'd *out*. |
| Schema | **migrations 1–13 applied** (1–7 at V2's start) | V2 appends only; see §5. |
| `EXTRACT_VERSION` | **`11`** (`crates/rto-graph/src/extract.rs`) — the Stage 28 bump landed in #316 | Bumping it forces full re-extraction for every user. No test pins the value. |
| Provenance | `Derived | Authored | Inferred`, CHECK-constrained | Unchanged by V2, by decision. |
| Eviction idiom | in-memory byte-budget LRU (`rto-llama` `ModelCache`); **nothing persisted is bounded** | Stage 25 ports the existing policy to disk rather than inventing one — **done**, and tested against `lru_evict_count`'s own numbers. |

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
| **13** ✅ | `agent_cache` + `agent_cache_clock` (ADR-0013 transient) | bounded | yes, by capacity |

**Numbers are assigned in landing order, not reserved in advance.** Stage 21 landed
first and took **8**; Stage 28 took **9** and **10**; episodic memory took **11**;
**12** is `sync_worktree`, from the guardrails branch; and the cache tier took
**13**. Splitting memory across two migrations is intentional: different lifetimes
and guarantees, so the eviction tier can later be altered without touching durable
memory.

**Check the other worktrees, not just the refs.** Stage 25 was dispatched with
"12 is free" after `git grep` over every local *and* remote branch found nothing
claiming it — which was true of the refs and false of the tree, because the
guardrails work was sitting in an unpushed worktree. That is the third numbering
collision of the day and all three had the same cause, so the check that actually
works is:

```sh
git worktree list | awk '{print $1}' | xargs -I{} grep -ho 'version: [0-9]*' {}/crates/rto-graph/src/migrations.rs | sort -u
```

Two constants both declaring the same `version` **merge cleanly in git** and break
at runtime on whichever store applies them second, so this is not a conflict the
tooling will catch for you. `migration_versions_are_unique_and_ascending` now
fails the build if a merge ever produces one.

**Stages 22 and 24 need no migration** — `RunnerKind` shipped in migration 8 already
naming all three backends, with the schema CHECK accepting them. Stage 22 confirmed
this: two analyzers landed with no schema change at all, because `FindingKey`
takes each analyzer's own ordered identity components. **Stage 22b confirmed it
again**: `osv-scanner` landed with a five-component identity and no schema change,
and a test asserts exactly that.

**`EXTRACT_VERSION` does not change in Stages 21–25.** None of that work is
extraction output (Stage 21 shipped without touching it, as required). It *does*
change in Stage 26, once — see the note there.

---

## 6. Staged roadmap

Dependency shape — four tracks, only one hard chain:

```
Track A (findings):  21 ✅ ──► 22 ✅ ──► 22b ✅ ──► 24
Track B (memory):    23 ✅ ──────────────► 25
Track C (lenses):    26         (independent of A and B throughout)
Track D (media):     28 ✅ ──► 29
                                          └──► 27 (v2.0 hardening)
```

Stages 21, 23 and 26 were the parallel-startable set; 21, 22, 22b, 23 and 28 have
now landed, leaving 24, 25, 26 and 29 open. Nothing in Track C touches the artifact
stores; nothing in Track B blocks Track A. **22b was sequenced after 22 and did not
block 24**: it added one adapter behind a seam 24 does not touch, and needed no
migration.

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

### Stage 22 — First analyzers: `semgrep` + `cargo-audit` → **v1.11.0** · effort **M + M** ✅ *delivered*

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

**Delivered in v1.11.0** (PR #322). What shipped, and what it changes for later
stages:

**Coverage is a matrix, not an analyzer list**
([ADR-0018](adr/0018-analyzer-coverage-matrix.md)). The requirement is findings
for Rust, Python, SQL, Java and Node. The two analyzers named in Stage 22 deliver
that **on the SAST axis only**; Stage 22b closed the dependency axis:

| Language | SAST | Dependency vulnerabilities |
|---|---|---|
| Rust | semgrep (GA) | cargo-audit (RustSec) **+ `osv-scanner`** — both kept, cross-referenced (22b) |
| Python | semgrep (GA) | `osv-scanner` (OSV `PyPI`) ✅ 22b |
| Java | semgrep (GA) | `osv-scanner` (OSV `Maven`) ✅ 22b |
| Node (JS/TS) | semgrep (GA) | `osv-scanner` (OSV `npm`) ✅ 22b |
| SQL | semgrep `generic` — token matching, no parser | n/a — no dependency ecosystem |

- Semgrep's published language list has **no SQL entry at any maturity level** —
  not GA, not beta, not experimental. SQL is therefore matched by semgrep's
  `generic` (token) engine: no AST, no dataflow, no types. It can say *this
  statement grants ALL PRIVILEGES*; it cannot say *this value reaches a query
  unsanitised*. The qualification is carried in the adapter's declared language
  list (`sql (generic mode)`), in every SQL rule's `engine-note` metadata, and in
  a test that asserts the note is present on every SQL finding.
- Semgrep's dependency scanning (Supply Chain) is a **hosted product feature**,
  not something `semgrep scan` does against a lockfile, so it contributes nothing
  to the second column.

**The adapter is the seam, so ingest and execution agree by construction.** One
conversion (`rto_exec::normalize_native`) turns an analyzer's *native* output
into the normalized report Stage 21 already validates, and both paths call it: a
subprocess run hands it the analyzer's stdout, `roteiro security ingest
--analyzer <name>` hands it a report file CI produced. Equality of `Finding`
values is a property of the code, not something a test establishes afterwards.
**A new analyzer is a file in `adapter/` and a row in `ADAPTERS` — no migration**,
because `FindingKey` takes each analyzer's own ordered identity components.

**Three tool behaviours found by running the tools, not by reading their docs.**
Each is a trap the next analyzer author would otherwise re-discover:

1. **Semgrep path-prefixes rule ids.** With `--config <file>` it renames every
   rule to `<config.path.components>.<id>`. The rule id is the first component of
   a `FindingKey`, so this puts the local asset-cache directory into every stored
   key — user-identifying data in a persisted record, and keys that differ
   between two machines running the identical scan. **`--no-rewrite-rule-ids` is
   mandatory**, and a test asserts no key contains a local path.
2. **Semgrep redacts the matched source.** In the open-source CLI `extra.lines`
   and `extra.fingerprint` are the literal string `"requires login"` unless the
   caller is authenticated to Semgrep's hosted platform. ADR-0012's identity
   recipe ends in a snippet hash, so hashing that field makes the component a
   constant *and* changes every stored key the day someone logs in. **Snippets
   are read from the worktree instead** (`rto_exec::snippet`), which is a
   function of the source rather than of the analyzer's auth state.
3. **`cargo audit` reports no advisory-database identity when you pin one.**
   Given `--db <path>` it returns `last-commit: null` and `last-updated: null` —
   at a shallow clone and at its own managed checkout alike (0.22.2). Only the
   unpinned, self-resolving configuration populates them, so the reproducible
   offline configuration was the one losing its staleness evidence. **Provisioning
   records the publication date from the database's `HEAD` commit time** and the
   adapter falls back to it; the tool's own account still wins where it has one.

**Provisioning: writing and running are separate.** `prefetch` is the only thing
that writes to the asset cache; a run never provisions, so "did this machine have
the pinned rules?" always has an answer. As shipped, `prefetch` **verifies and
pins but fetches nothing** — the rule set is vendored into the binary and the
`RustSec` advisory database is a git checkout with no digest-stable URL, so it is
refused with the exact clone command rather than obtained by shelling out to
`git` (which would be the host-tool fallback ADR-0014 forbids). ADR-0014 v1.1
records that clarification. An asset whose bytes change after provisioning is
**refused, not warned about**: a run would otherwise stamp a digest that does not
describe what it read.

**Rules are ours, vendored and pinned.** `semgrep --config p/default` resolves
against a network service, which makes an "offline" analyzer network-dependent
and its results irreproducible. Every shipped rule was written for this
repository under its own licence; **no Semgrep Registry rule is vendored**,
because those carry the *Semgrep Rules License v1.0*, which is not on
`deny.toml`'s allow-list — and `cargo deny` governs crates, so it would never
have caught a rule file. The position is stated in the rule header, the adapter
docs and ADR-0018 rather than assumed.

**Accepted gaps, recorded so they are decisions rather than oversights:**

- Semgrep's `errors` array is **not** converted to findings. A scan that failed
  to parse some files reports fewer findings; only a fatal exit (≥2) is caught.
- **No CVSS base score is computed** from RustSec's vector. The advisory kind is
  mapped to a severity and the raw vector, aliases, `related` CVEs and categories
  are preserved verbatim in `meta`; a number disagreeing with `cargo audit`'s own
  would be worse than carrying the vector unchanged.
- **Windows is untested.** Paths and `PATH` splitting are written for it; nothing
  has run on it.
- Semgrep's default ignore list skips `tests/`, `fixtures/` and similar, so a
  user's repository is scanned with those excluded. That is semgrep's normal
  behaviour, and it is why the live test copies the fixture tree somewhere
  neutral first.

**No new dependencies** — `Cargo.lock` is untouched; `exec-subprocess` is
`std::process` over the crates already present, and is **off by default**.

### Stage 22b — `osv-scanner`: the dependency axis for Python, Java and Node → effort **M** ✅ *delivered*

Split out of Stage 22 because it is a different axis, not more of the same one,
and because the SAST half is independently useful and independently reviewable.

- **Rust surface:** one more adapter behind the existing seam. The subprocess
  runner, the asset cache, `prefetch`/`status` and the finding schema are all
  reused unchanged — which is the seam doing its job.
- **Provisioning:** OSV's per-ecosystem databases
  (`https://osv-vulnerabilities.storage.googleapis.com/<ECOSYSTEM>/all.zip`) are
  single files at stable URLs, so they are the first asset that genuinely wants a
  download-by-URL source. `AssetSource` is `#[non_exhaustive]` for that.
- **The Rust overlap is now DECIDED — implement it, do not re-open it.**
  `osv-scanner` also reads `Cargo.lock` and OSV ingests RustSec, so the same Rust
  advisory arrives twice under two finding keys. [ADR-0018](adr/0018-analyzer-coverage-matrix.md)
  **v1.1** resolves it: **keep both findings and cross-reference them at the
  reporting layer.** Neither layer is filtered or made conditional on the other.
  The join key needs no invention — OSV keys a RustSec-derived record by the
  RUSTSEC id itself and carries `aliases` (`RUSTSEC-2020-0071` →
  `CVE-2020-26235`, `GHSA-wcg3-cvx6-7396`), and `cargo-audit`'s adapter already
  stores `aliases` verbatim in `meta` (`crates/rto-exec/src/adapter/cargo_audit.rs:225`).
  Join on the RUSTSEC id, fall back to alias-set intersection. Render a duplicate
  pair as **one advisory confirmed by two analyzers**, with both finding keys
  still addressable — never a merged super-finding, and never a count that
  silently halves.
- **Two things to MEASURE here, not assume.** (1) OSV.dev the *database* does
  carry RustSec informational advisories — `RUSTSEC-2024-0388`,
  `RUSTSEC-2021-0139` and `RUSTSEC-2026-0192` all resolve, each with
  `aliases: null` — but whether `osv-scanner` the *tool* surfaces them by default
  is unestablished, and ADR-0018 v1.0 conflated the two. Measure it and correct
  the ADR. (2) RustSec→OSV ingestion lag: if it can trail by days the analyzers
  will legitimately disagree for a window, and "present in one, absent in the
  other" must render as a real state rather than a defect.
- **DoD:** a Python, a Java and a Node lockfile each produce findings; offline
  with a pinned database succeeds; the Rust overlap with `cargo-audit` is
  explicitly resolved and recorded, rather than left to chance.

**Delivered.** What shipped, and what it changes for later stages:

- **The `osv-scanner` adapter**, behind the Stage 21 seam. The subprocess runner,
  asset cache, `prefetch`/`status` and finding schema were reused **unchanged**,
  and — as Stage 21 predicted — it needed **no migration**: `FindingKey` already
  takes each analyzer's own ordered identity components (`advisory, ecosystem,
  package, version, manifest`), and `RunnerKind` already names the backends. A
  test asserts the new analyzer's keys are valid with no schema change.
- **`AssetSource::Download`**, the first asset fetched by URL — OSV's
  per-ecosystem `all.zip` databases. `rto-exec` gained **no network dependency**:
  it takes the fetcher as a function argument, so the code that can open a socket
  lives in the CLI and is reachable only from `prefetch`. That flag is new and
  required: `roteiro security prefetch --analyzer osv-scanner --allow-download`,
  because the four databases are roughly **260 MB** (`npm` alone is ~210 MB) and
  that is not a reasonable surprise. The stale comment in `assets.rs` claiming an
  unused fetch path is a security surface with no user was updated, not left to
  contradict the code.
- **The Rust overlap is implemented as ADR-0018 v1.1 decided**: both analyzers'
  findings are kept, and `rto_exec::cross_reference` joins them at the reporting
  layer on identifiers both upstreams publish. `security list` renders a
  duplicate pair as one advisory confirmed by two analyzers with both finding
  keys still addressable, and prints the finding total **unchanged** above it.
  Real fixture data added a constraint the decision did not state: `chrono`'s
  `cargo-audit` advisory lists `time`'s CVE under `related`, so the join also
  requires the same package at the same version, and names a correspondence only
  by an id an analyzer actually fired.
- **Both open items were measured, and ADR-0018 is at v1.2 in this PR.**
  (1) `osv-scanner` 2.5.0 **does** report `RustSec`'s `unmaintained` and
  `unsound` by default — v1.0's options table conflated the database with the
  tool. Only `yanked` is unavailable, and structurally: it is not an advisory,
  `cargo audit` reads it from the crates.io index. (2) RustSec→OSV ingestion lag
  is **~2.5 minutes**, not days; what actually makes the two analyzers differ is
  **pin age**, since each database is provisioned separately. "Present in one" is
  rendered as a normal single-source row with its cause named.
- **Three more undocumented tool behaviours**, each of which would have been a
  silent defect and all three now recorded in ADR-0018: `--offline-vulnerabilities`
  alone consults no database and reports a clean scan (`--offline` is what loads
  it); reported paths are absolute even when the target is `.`, which would have
  put the scanning machine's home directory into a persisted finding key; and the
  same advisory is listed twice under its RUSTSEC and GHSA ids, with `groups`
  already saying so.
- Fixtures are **real captured output** from osv-scanner 2.5.0 over a committed
  four-ecosystem lockfile tree, taken fully offline against pinned databases. The
  tool-dependent test self-skips visibly when no `osv-scanner` is on `PATH`.

### Stage 23 — Agent memory, episodic tier ([ADR-0013](adr/0013-agent-memory-artifact-store.md)) → **v1.11.0** · effort **M** ✅ *delivered*

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
memory cannot invalidate the fact cache, and is asserted so as a property of
memory writes (`tests/sync.rs::memory_writes_do_not_invalidate_the_fact_cache`),
because memory is not extraction output.

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

### Stage 24 — boxlite sandboxed backend ([ADR-0014](adr/0014-sandboxed-analyzer-execution.md)) → **v1.13.0** · effort **L** ✅ *delivered*

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

**Delivered.** `BoxliteRunner` behind `exec-boxlite`, `boxlite` pinned `=0.9.7`,
the DoD executed rather than argued: real `semgrep` 1.173.0 over one tree, once
as a host child process and once in a digest-pinned microVM, **4 identical
findings** — `PARITY OK … subprocess isolation=none image=none, boxlite
isolation=microvm image=sha256:67319956…`. 22 fault injections, 21 red on the
right message; the one that was not is recorded below, because it did **not** go
red and saying so is the point.

**What the stage actually turned out to be about.** The plan said publication was
verified "so this is a dependency addition, not a packaging problem". Publication
was real; the inference was not. **`boxlite` from crates.io ships no hypervisor**:
its three `-sys` crates each detect a published package (`.cargo_vcs_info.json`)
and disable themselves, and `libkrun-sys` excludes the sources they would build —
enabling its `krun` feature compiles and then fails to link with 26 undefined
symbols. What executes is a prebuilt runtime archive that `boxlite`'s own build
script fetches with a bare `curl -fsSL`, `include_bytes!`s into the rlib, and
extracts and execs at run time. That fetch has **no expected digest** (searched
four ways, NOT FOUND) and an env-overridable URL, so two builds of the same
pinned version could embed different bytes undetectably.

So the stage's real work was **governing that fetch**, not adding a dependency:

1. **`AssetSource::PinnedArchive`** — the first asset source with a *compile-time*
   digest, in `runtime_pins.rs` and shared with `build.rs` by `include!` so the
   two cannot drift. It closes the gap `Fetcher`'s contract has to leave open for
   `Download` assets: a fetcher that reports success over a truncated body cannot
   defeat a pin checked in-crate before install. Tested with a deliberately lying
   fetcher.
2. **`build.rs` refuses to build** unless `BOXLITE_RUNTIME_URL` names a local file
   matching the pin. Unset, remote, or wrong bytes are hard failures with a
   runnable recipe. `boxlite`'s `curl` then never reaches the network, because
   what it is asked to fetch is already on disk.
3. **The licence acceptance is recorded in `deny.toml`** and discharged by
   `crates/rto-exec/NOTICE-boxlite-runtime.md`, which is `include_str!`d and
   printed by `prefetch` before installing. The archive embeds GPL-2.0 (`mke2fs`,
   `debugfs`, `libkrunfw`) and LGPL-2.0-or-later (`bwrap`) binaries; they are
   exec'd as separate processes, so this is aggregation and Roteiro stays
   `MIT OR Apache-2.0`, but distributing a binary built this way carries GPL-2.0
   §3 source-offer and LGPL relinking duties.

**The gate that could not see any of this is now closed.**
`crates/rto-exec/tests/build_script_fetch_audit.rs` reads the build script of
every package in the `--all-features` graph and **fails** on anything that looks
like it fetches without a recorded pin. Measured: 613 packages, 89 of which have
a build script (96 script files), **2 flagged (both governed), 0 false
positives**, in **0.3s** — the audit prints those numbers on every run, so "it
flagged nothing" stays checkable rather than trusted. Review asked whether the
matcher was narrower than its docs claimed; it was, and the docs were the wrong
half: widening to a bare `http(s)://` was measured at 29 flagged with **27 false
positives** (`serde`, `quote`, `anyhow`, `winapi` … all citing a docs URL in a
comment), so the claim was narrowed to what the code does and a test now pins the
two together. Its module docs state what it does *not* cover (helper crates,
`include!`d build modules, obfuscation, run-time fetches, already-vendored
bytes); read them before trusting it. This hole was general, not boxlite's:
`cargo deny --all-features check` reported `licenses ok` while 25 MB of GPL
binaries were being embedded, and was not wrong to — it governs crates.

**Deviations and costs, stated rather than buried:**

- **Workspace `rusqlite` moved `=0.40.2` → `0.39`.** Not a preference: `boxlite`
  requires `rusqlite ^0.39`, `libsqlite3-sys` declares `links = "sqlite3"`, and
  cargo forbids two versions of a `links` crate in one graph. It is a hard
  resolution failure otherwise. It also restores what the pin's own comment
  already described. **Coordinate with #342** (`store.rs`/`migrations.rs`); the
  workspace compiles unchanged on 0.39, and MSRV 1.94 still builds `--all-features`.
- **`protoc >= 3.12` is now a build requirement** for `exec-boxlite`
  (`boxlite-shared`'s build script, no stub path around it). Documented in the
  README before anyone meets it as a build failure; added to all three CI jobs.
- **CI grew a provisioning step** (`scripts/provision-sandbox-runtime.py`, which
  reads the digests out of `runtime_pins.rs` so it cannot drift). All three
  `--all-features` jobs now also compile ~400 extra crates — a real cost on every
  PR, in keeping with the llama.cpp build they already carry.
- **Four `unmaintained` advisory ignores** (`term_size`, `bincode`, `adler`,
  `atty`), all arriving through `boxlite`, none a vulnerability, each with the
  four-part rationale `deny.toml` demands. They are global — cargo-deny cannot
  scope an ignore to a feature — and say so.
- **`roteiro/exec-boxlite` implies `exec-subprocess`**, because `security
  prefetch|status|run` are all gated on that feature today. CLI plumbing, not
  policy: unsandboxed *runs* still need `--allow-unsandboxed` per invocation.
- **Only `semgrep` has a pinned image.** `cargo-audit` has no official one, and
  publishing a security tool's container is not a job this project is taking on.
- **Guest sizing is explicit** (2 vCPU / 4096 MiB) and the image entrypoint is
  replaced with a waiting shell. Both were found the hard way: a box lives exactly
  as long as its init process, and an analyzer image's entrypoint *is* the
  analyzer, so it printed usage, exited, and SIGKILLed the in-flight scan — which
  reads exactly like an OOM kill and is not one. `SandboxError::Killed` exists so
  nobody diagnoses that twice.
- **One claim is untested and labelled as such.** Guest-path relativisation is
  never exercised, because `semgrep` reports relative paths; fault injection
  confirmed setting it to `None` leaves the parity test green. It is there for
  `osv-scanner`, the next image candidate.

**What this settles for later stages.** Analyzer execution now has one
provisioning contract with a real digest pin, so a future backend or analyzer
image inherits verification rather than re-inventing it; and the build-script
audit means the *next* dependency that fetches something is a failing test rather
than a discovery. **Apple Silicon microVM execution remains untested in CI** — the
parity proof above ran on Apple Silicon locally, and CI runners have no
`/dev/kvm`, so the sandbox tests skip there with a visible message and the ingest
and subprocess paths carry the functional coverage. That gap is unchanged and
still accepted.

### Stage 25 — Memory recall: cache tier, decay, supersession → shipped in **v1.12.0** · effort **L** ✅ *delivered*

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

**Delivered.** Migration **13** (`agent_cache` + `agent_cache_clock`),
retrieval-time ranking in `rto_graph::memory`, the byte-budget sweep, and the
`search` memory channel. All four DoD items have a test, and each was
**fault-injected**: the guarded behaviour was broken, the guard watched go red,
and the source reverted byte-identically (15 injections, all red — two of them
only after the tests they exposed as weak were strengthened).

**What shipped, and where it deviated:**

1. **`Decay` is `none | linear[:span] | exponential[:half-life]`, and `none` is
   the default.** ADR-0013 offered the three modes without saying which one leads;
   the reproducible answer is the default here, on the same terms as
   `SearchOptions` defaulting generated content off. Age is counted in
   **generations** — one per record written — so ranking never touches a clock.
2. **`base_confidence` defaults to `0.5`** when a writer states none. Not in the
   ADR. `1.0` would let every record that claimed nothing outrank one that
   honestly claimed `0.9`, pricing honesty; `0.0` would make the common case (the
   CLI states no confidence unless asked) unrecallable. The midpoint is the only
   value that makes stating one worth the trouble in both directions.
3. **`anchor_penalty` ranks `drifted` *below* `vanished`** (`valid` 1.0,
   `unanchored` 0.9, `unverifiable` 0.5, `vanished` 0.35, `drifted` 0.25). Drift
   is the one state that can actively mislead about code still sitting under the
   same key; a vanished anchor can mislead nobody, and ranking it lowest would
   punish exactly the records the ADR says are worth keeping most. Two properties
   are asserted rather than assumed: nothing is ever zero, and every state that
   `AnchorState::applies` outranks every state that does not.
4. **The eviction counters needed a logical clock**, so migration 13 adds a
   single-row `agent_cache_clock` beside the table (on `sync_state`'s precedent).
   ADR-0013 §3 rules out wall-clock and the ADR's proposed `agent_cache` names
   `generation`/`last_used` without saying where the values come from. `ticks`
   advances per access — `ModelCache`'s list position, made durable, with no ties
   for the sweep to break arbitrarily — and `generation` advances once per sweep,
   which is what makes "written in the current generation" a window rather than a
   single row, and what makes the pin lapse instead of becoming permanent.
5. **`evict_count` is a pure function tested against `lru_evict_count`'s own
   numbers.** Porting an existing policy is worth nothing if the port quietly
   behaves differently. The always-keep-the-MRU rule moved from a
   `len - evict > 1` guard into the caller's pinned set, because this tier pins
   other rows too and one rule beats two. A sweep can therefore finish **still
   over budget** when everything left is pinned; `CacheSweep::over_budget` says so
   rather than leaving a bound that silently failed to bind.
6. **Memory reaches `search` through a third channel, `--include-memory`, off by
   default** — beside the graph and generated channels, never merged with either.
   Its scorer shares no branch with the node scorer, so "memory never takes the
   `authored` +40" is structural. Stage 23's *total absence* assertion is restated
   rather than relaxed, and is now checked more sharply than absence could check
   it: a memory hit's score must not exceed the ceiling its own lexical terms can
   produce, which is what a leaked +40 would breach.
7. **CLI:** `roteiro memory recall [query] [--decay …] [--applicable-only] …`,
   which prints every term of the ranking and not just the product, and
   `roteiro memory cache [--sweep] [--budget-mb]`. The sweep also runs at the
   maintenance seam in `roteiro context --refresh`, never on a read path.
   `context --refresh --json` keeps its long-standing shape: wrapping it would
   break callers to pay for maintenance they did not ask about, so the sweep is
   reported on stderr there and has its own `--json` under `memory cache`.
8. **Budget:** `--budget-mb`, else `ROTEIRO_CACHE_BUDGET_MB`, else the 256 MB of
   §9.1. An unreadable value is an **error, not a fallback** — running the default
   under a name that says otherwise is how an operator ends up believing in a
   bound that was never applied. The config-file layer was deliberately not
   touched.

**Two absolute assertions on a shared constant disarmed**, at `store.rs:1709` and
`:1880` — `assert_eq!(store.schema_version()?, 11)`, which migration 13 breaks.
Both now read `migrations::latest_version()`, the idiom
`a_later_migration_is_additive_on_a_populated_store` already uses. The literal was
defended in a comment as making someone confirm a new migration is meant to apply
on open, but `apply` runs *every* migration newer than the recorded version, so
there was never a per-migration choice there to confirm — the literal asserted the
value of a shared constant and nothing else. Fault injection confirmed the
rewritten assertions still catch the thing they are for: an `apply` that stops one
migration short of `latest_version()` fails both.

**`EXTRACT_VERSION` is unchanged**, and
`tests/sync.rs::memory_writes_do_not_invalidate_the_fact_cache` still passes —
also fault-injected, by making a memory write clear `sync_env`.

**Not in this stage, deliberately:** the cache tier ships with its policy, its
seam and its API, but **no producer** — `node_context` is still the context cache.
Moving a live cache onto the bounded tier is a data migration, not a policy
change, and bundling it would have put a schema move and an eviction policy in one
reviewable unit.

### Stage 26 — Analysis lenses (A1) → **v1.15.0** · effort **S–M per lens** *(independent track)* ✅ *delivered*

**Goal:** deepen the graph itself — the on-brand work — with **honest costs**.

**Cost correction, which this stage exists to respect:** a fully surfaced lens is
**~195–500 LOC across 6–8 files**, not the ~20-line mirror previously assumed. That
figure describes only the internal query fn. There are **seven** surfacing stages,
not four: extraction (`scan_markers` + `augment`), the query fn, the query result
types, **MCP** (`GraphServer`) and **served-chat** (`GraphToolRegistry`) as
*separate* registries, Obsidian render, and CLI-side aggregation — plus tests and
docs.

Shortlist, in order:

All three shipped, as three separate PRs — a reviewable diff beats a complete one
nobody can check.

1. **Q3 — directed coupling** ✅ *(the standout)*. `Calls` edges already retain
   direction, and today's hotspot view **throws that away by incrementing both
   ends**. Highest value per line in the set.
2. **Q1 — debt density** ✅. Builds directly on delivered intent-debt tracking.
3. **S1 — config-secret inventory** ✅ *(renamed, deliberately)*. Values are redacted
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

#### Q3 — directed coupling ✅ *delivered*

`rto_graph::coupling` reports, per node, **fan-in** (distinct callers) and
**fan-out** (distinct callees) over `Calls` edges, plus Martin's instability
`fan_out / (fan_in + fan_out)`. Ranked by `total` | `fan_in` | `fan_out`, ties
broken by key, so identical input gives byte-identical output.

**Surfaced on six of the seven stages; the seventh is a deliberate no-op.**

| Stage | Q3 |
| --- | --- |
| Extraction (`scan_markers` + `augment`) | **Untouched by design** — Q3 adds no derived metadata, so `EXTRACT_VERSION` stays **11**. Confirmed live: `sync` after the change reported *237 of 239 blobs cached*. |
| Query fn | `rto_graph::coupling` |
| Query result types | `CouplingReport` / `CouplingItem` / `CouplingOrder` |
| MCP (`GraphServer`) | `coupling` tool (`rto-render/src/mcp.rs`) |
| Served-chat (`GraphToolRegistry`) | `coupling` tool — a **separate** registry, with its own test |
| Obsidian render | `_Home` → "Most depended-on (call fan-in)" |
| CLI-side aggregation | `roteiro coupling [--order] [--limit] [--json]`, `GET /v1/graph/{project}/coupling` |

**Not surfaced:** the explorer web app has no coupling panel. The `/coupling`
endpoint serves the data; wiring `assets/app.js` is follow-up, tracked here rather
than left to be discovered.

**`/hotspots` is deliberately unchanged.** Undirected degree over *every* edge kind
is a different, still-useful question; the explorer depends on its shape. Its doc
comment now says so and points at `/coupling`, rather than the discarded direction
being an unremarked accident.

**Two counting rules that change the numbers**, both reported rather than silent:

- **Distinct counterparts, not edges.** Edges are a set per
  `(src, dst, kind, provenance)`, which still admits *parallel* `Calls` edges at
  two provenances. `fan_in` counts dependants, not layers that asserted a
  dependency.
- **Self-calls and cross-language edges excluded.** Recursion couples a node to
  nothing outside itself. Cross-language edges are never calls: cross-file
  resolution (`sync.rs`) binds a callee by **simple name** across every `Fn` node
  regardless of language, and Roteiro extracts no FFI. On this repo that is
  **615 of 6553** call edges (9.4%) — enough that excluding them removes a
  JavaScript `clone` from second place in the ranking.

**Confidence signal, and why there is no CI gate.** `fan_in` is exactly as precise
as the edges beneath it, and the residual same-language case is not fixable here: a
lone Rust `join` helper still absorbs every `.join(…)` in the workspace, reading as
**232 callers**. That is a limit of call resolution — fixing it is extraction work,
and extraction work means bumping `EXTRACT_VERSION`. So the lens **offers no CI
gate**, says so, and carries the caveat on every surface including both tool
descriptions, so a model reporting a high `fan_in` passes it on. No suppression
mechanism was added: coupling is a *measurement*, not a finding, so there is
nothing to suppress — and a second exclusion vocabulary beside `[debt] ignore`
would not have helped anyway (this repo's `[debt] ignore` does not cover the
vendored `cytoscape.min.js` that dominates the ranking).

**Scale benchmark**, this repo (5501 nodes, 12036 edges, 6553 `calls` edges;
2887 coupled nodes), release build, warm, whole-process wall clock, best of 5:

| Command | Time |
| --- | --- |
| `roteiro coupling --limit 20` | **0.05 s** |
| `roteiro coupling --limit 0` (all 2887) | **0.07 s** |
| `roteiro debt` (existing baseline) | 0.04 s |
| `roteiro search store` (scans all nodes) | 0.05 s |

Ranking runs on the counts alone and only the nodes that survive the cap are read
back, so a top-N question costs one edge scan plus N node lookups — not the
whole-graph node scan `/hotspots` performs.

**Documentation debt (both items), fixed in [#288](https://github.com/OffeneDatenmodellierung/Roteiro/issues/288), which is where they live** —
not in any repo file:

- **Security taxonomy normalised.** Rows S1–S6 carried `GPB`/`CVE`/`SAST`, which
  the issue's own class key never defines. They now use the defined
  **GDS**/**NNX**/**EXT**/**LLM** vocabulary, assigned to match the issue's own
  A1–A4 tiering (S1, S4 → `GDS`; S2, S3 → `NNX`; S5, S6 → `EXT`), and S1 is
  retitled to the inventory it can actually be.
- **The SmolVLM claim is now marked a hypothesis.** *"too small to emit
  `<tool_call>`"* rests on no code and no benchmark — **NOT FOUND** across four
  queries: `grep -rni smolvlm --include='*.rs' crates/` (10 hits, all registry /
  media-producer / speculative-decoding plumbing), `grep -rni tool_call` filtered
  to vision/VLM/mmproj/image terms (zero), and `roteiro search` for
  *"smolvlm tool_call"* and *"vision tool protocol"* (no matches). Stage 31's DoD
  required Qwen3 to be **shown** to emit a `<tool_call>`; no equivalent run exists
  for SmolVLM. The describe-then-query recommendation built on it is relabelled as
  the safe default, with the one-run experiment that would settle it.

The A1 cost line in #288 (*"~20-line mirror of `debt`"*) is struck through and
replaced with the ~195–500 LOC / 6–8 file figure this stage is built on.

#### Q1 — debt density ✅ *delivered*

`rto_graph::debt_density` ranks **files** by retained intent-debt markers per 1,000
lines. A raw marker count ranks by file size — the biggest file has the most lines
to put a marker on — so a 40-marker file of 4,000 lines and a 40-marker file of 200
lines are indistinguishable under `debt` and twenty-fold apart under this. Built on
`debt`'s own output rather than re-walking markers, so the two lenses cannot
disagree about which markers exist, and `[debt] ignore` applies unchanged.

**The denominator was the design decision, and it did *not* need extraction
metadata.** `lines` is the `file` node's `meta.lines` — the count of `\n` bytes in
the blob, recorded at extraction since Stage 2 and therefore already in every
`EXTRACT_VERSION` 11 blob. Three alternatives were rejected: **SLOC** does not
exist in the graph and computing it is net-new derived metadata (which would have
moved Q1 into the Q2/Q10 batch below); **per-symbol** is what
`Node.span`'s **byte offsets** cannot give, since a span is not a line range; and
**the highest marker line** is a lower bound on length, not the length. What
`lines` counts is stated on every surface: every line, blanks and comments
included — *file length*, not lines of code.

Two rules keep the arithmetic honest. A `min_lines` floor (default **50**) keeps
the denominator's tail out of the ranking — one marker in a 3-line file is 333 per
kloc, true and useless — while leaving those files in `files_with_markers` and
`total_markers`, reported as `short_files`. And ranking cross-multiplies the exact
ratio in `u128` rather than the rounded `per_kloc`, so densities differing in the
fourth decimal do not tie and silently reorder on path.

**Surfaced on six of the seven stages; the seventh is a deliberate no-op.**

| Stage | Q1 |
| --- | --- |
| Extraction (`scan_markers` + `augment`) | **Untouched by design** — the denominator was already extracted, so `EXTRACT_VERSION` stays **11**. Confirmed live: a rebuild from an empty store reported *257 of 258 blobs cached, 0 extracted*. |
| Query fn | `rto_graph::debt_density` |
| Query result types | `DebtDensityReport` / `DensityItem` / `DensityOrder` / `DEFAULT_MIN_LINES` |
| MCP (`GraphServer`) | `debt_density` tool (`rto-render/src/mcp.rs`) |
| Served-chat (`GraphToolRegistry`) | `debt_density` tool — a **separate** registry, with its own test |
| Obsidian render | `_Home` → "Densest files (markers per 1,000 lines)", under the existing intent-debt section |
| CLI-side aggregation | `roteiro debt-density [--kind] [--order] [--limit] [--min-lines] [--json]`, `GET /v1/graph/{project}/debt/density` |

**Not surfaced:** the explorer web app has no density panel — the same position as
Q3's coupling panel. `roteiro debt` is deliberately unchanged: a marker inventory is
a different question, and `check` depends on its shape.

**No CI gate, and the caveat travels on every surface** (both tool descriptions, the
`_Home` table, the CLI summary, the endpoint doc) — Q3's precedent, not one buried
mention. Density inherits the marker scan's prose false positives (`for now`,
`placeholder`, `tbd` fire on ordinary writing, so a design document ranks as dense
debt) and adds one of its own: the denominator is file length, so verbose,
generated or widely-indented files are systematically flattered and dense
languages penalised. A gate would fail builds on prose and on formatting. The
suppression story is the existing one — `[debt] ignore` globs and the
`roteiro:ignore` / `roteiro:ignore-file` directives, applied before anything is
counted.

**One follow-up, found in review and fixed on the branch** (`fix(render): _Home
scopes intent debt by [debt] ignore, both tables`): the Obsidian `_Home` overview
computed debt with an **empty** ignore list, so the vault reported a different debt
for the same repository than `debt`, `debt-density`, `check` and the graph API —
the disagreement ADR-0007 v1.1 was amended to end. Both `_Home` calls were fixed,
not only the one Q1 added: leaving `debt`'s wrong would have had the category
totals and the density table on one page disagreeing about which files exist,
which is worse than being consistently wrong. This was the **third** surface with
that defect (#321 records the first two) and it was missed for the same reason each
time — the earlier fix went to the surfaces that had been *reported* rather than to
this stage's own list of seven, which names the Obsidian render. The enumeration
that closes it: `debt` and `debt_density` are the only functions in the workspace
taking an `ignore: &[String]`, and every call site of either now passes the target
project's list wherever one can be reached.

#### S1 — config-secret inventory ✅ *delivered*

`rto_graph::config_secrets` reports **secret-named** config keys — paths, key
names, and whether each value was redacted before persistence — in three states,
because collapsing any two would misreport them:

| State | Meaning |
| --- | --- |
| `redacted` | The value was read from a config file and replaced with the placeholder before anything was stored. The expected state. |
| `declared` | The key carries **no value at all** — a `@rto:config` struct field, declared in Rust with no literal to redact. Neither a redaction nor a leak. |
| `present` | A value that is **not** the placeholder. Extraction cannot produce this; `Store::apply_import_layer` can, so a non-zero count is a finding about **this store**, pointing at the importing tool rather than the source repository. |

`redacted_not_secret_named` counts values redacted for *where they live* (a
Kubernetes `Secret`'s data) rather than what they are called, so the redaction
figures reconcile against the graph instead of leaving an unexplained surplus.
`config_keys::REDACTED` is now a single constant shared by the two redaction sites
and this one reader, so the lens cannot drift from the redactor by a spelling.

**`[debt] ignore` deliberately does not apply here**, which is worth stating given
the follow-up above. That list is defined as a debt-marker exclusion (ADR-0007), and
`config_secrets` takes no `ignore` parameter at all — so there is no call site that
could be passing the wrong thing. Giving it one would mean inventing a second
exclusion vocabulary for a different question, which is the mistake Q3 declined to
make for coupling.

**The rename is the whole point, and it is enforced rather than merely documented.**
The lens **cannot** detect a hardcoded credential in source (that produces no
`config_key` node), **cannot** judge validity (it never sees a value — no
`ConfigSecretItem` has a value field), and **cannot** tell a real secret from a
placeholder. An empty report means *no secret-**named** config key* — a statement
about naming, not a clean bill of health: a credential under `dsn` or `endpoint`
never appears. That limitation is carried in **both** tool descriptions in the
imperative ("state the limits when you report it"; "if asked to scan for secrets,
say plainly that this tool cannot do it"), in the CLI summary **unconditionally**
including on an empty report, in the `_Home` section, and in the endpoint doc — and
each of those is fault-injected. `tests/config_secrets_cli.rs` puts the *same*
token in a `.env` (where it becomes a redacted config key) and in a Rust function
body (where it becomes nothing this lens reads), so the boundary is a test.

**Surfaced on six of the seven stages; the seventh is a deliberate no-op.**

| Stage | S1 |
| --- | --- |
| Extraction (`scan_markers` + `augment`) | **Untouched by design** — the redaction already happens there, so `EXTRACT_VERSION` stays **11**. Confirmed live: *259 of 260 blobs cached, 0 extracted*. |
| Query fn | `rto_graph::config_secrets` |
| Query result types | `ConfigSecretReport` / `ConfigSecretItem` / `RedactionState` |
| MCP (`GraphServer`) | `config_secrets` tool (`rto-render/src/mcp.rs`) |
| Served-chat (`GraphToolRegistry`) | `config_secrets` tool — a **separate** registry, with its own test |
| Obsidian render | `_Home` → "Config keys named like secrets" (counts and files, **not** key names — a vault note is browsed out of context) |
| CLI-side aggregation | `roteiro config-secrets [--limit] [--json]`, `GET /v1/graph/{project}/config-secrets` |

**Not surfaced:** the explorer web app has no config-secret panel. **No ordering
knob**, deliberately: this is an inventory ordered by `(path, name, key)`, and an
order would imply some keys are more secret than others. **No CI gate and no
non-zero exit**, even on an `unredacted` finding — that finding is about Roteiro's
own import layer, so failing a user's build over it would be the wrong response.

**Scale benchmark**, this repo (5,860 nodes, 13,048 edges; 372 config keys, 48
markers over 20 files, 32,444 ranked lines), release build, warm, whole-process
wall clock, best of 5:

| Command | Time |
| --- | --- |
| `roteiro debt-density --limit 20` | **0.04 s** |
| `roteiro debt-density --limit 0` (all 20 ranked) | **0.04 s** |
| `roteiro debt-density --limit 0 --min-lines 0` (floor off) | **0.04 s** |
| `roteiro config-secrets` (default 50) | **0.04 s** |
| `roteiro config-secrets --limit 0` (all 372 keys scanned) | **0.04 s** |
| `roteiro debt` (existing baseline) | 0.04 s |
| `roteiro coupling --limit 0` (Q3 reference) | 0.08 s |
| `roteiro search store` (scans all nodes) | 0.05 s |

Both new lenses are indistinguishable from the `debt` baseline. Q1 reads back only
the files that actually carry a marker — one node lookup each, not a whole-graph
`file`-node scan, which matters because `file` nodes carry captured `meta.content`.

On this repository S1 finds **2 secret-named keys among 372 config keys**, both
redacted, none unredacted — which is also a fair illustration of its reach.

**The cost estimate, corrected a second time.** Actuals per lens, and the
figure each was measured against:

| Lens | Files | Insertions |
| --- | --- | --- |
| Q3 (#346) | 8 | 1,121 |
| Q1 ([#372](https://github.com/OffeneDatenmodellierung/Roteiro/pull/372)) | 8 | 1,509 |
| S1 | 10 | 1,523 |
| **Stage total** | **11** | **3,016** |

The RFC's original *"~20-line mirror of `debt`"* was out by ~75×. This stage's own
corrected **195–500 LOC / 6–8 files** is out by ~3–8× on lines and right on files:
a fully surfaced lens is **~1,100–1,500 insertions across 8–10 files**, and that
is the figure a future lens should be planned against. The line count is dominated
by tests and by doc comments defending the design decisions — Q1's denominator,
S1's three states and its limits — not by the query, which is under 120 lines in
both cases. S1 costs the same as Q1 despite a simpler query because the rename
obliged the limitation to be restated, and fault-injected, on every surface.

**41 fault injections** across the two lenses (20 for Q1, 21 for S1), one per new
behaviour on every surface, each caught by a named test with every file
byte-identical after revert. Two were retained after they exposed real test gaps
rather than being retargeted: Q1's ranking on the rounded ratio, and S1's CLI
warning path, which extraction cannot reach and so needed a unit test on
`config_secrets_summary` rather than a repository fixture.

**`EXTRACT_VERSION` is still 11** and no lens in this stage touched extraction
output. The batched bump (§8b: Q2, Q10, cross-language edge resolution) remains
unpaid and unblocked.

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

### Stage 29 — Audio metadata as `derived` facts ([ADR-0016](adr/0016-audio-metadata-extraction.md)) → **v1.11.0** · effort **M** ✅ *delivered* *(independent track)*

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

### Stage 30 — MTP speculative decoding (issue #320) → **v1.11.0**, opt-in only · effort **M** ✅ *delivered* *(independent track)*

**Goal:** spend the draft head a Qwen3.5+ GGUF already carries. Generation on Metal
is memory-bandwidth bound, so a decode that *confirms* three proposed tokens costs
about what a decode producing one costs. The drafter is the model's own `nextn`
block — no second model to install or keep in step — and on the vendored llama.cpp
(b10200, pre-upstream #26296) its tensors are loaded whether or not anything uses
them, so the memory is already paid for.

- **Measured** (split-head Qwen3.8-27B-Q4, 5 interleaved pairs, median of per-pair
  ratios): draft width 1 → code **1.45×**, tool-call **1.22×**, prose **1.28×**;
  width 3 → code **1.50×**, tool-call **1.35×**, prose **1.26×**. `DRAFT_MAX = 3`.
  On Qwen3.5-9B the same sweep could not separate the widths from noise (0.86–1.14×,
  both directions), so the win is size-dependent and the small-model case is honestly
  *no win*.
- **The finding that decided the default:** speculative decoding **changes some
  completions**. Not in principle — measured. The sampler is driven identically
  (one call per emitted token, in position order, `accept` between), so the
  *distribution* is untouched; but llama.cpp's logits differ between batch width 1
  and width 4, and that is enough. `tests/batch_numerics.rs` measures the gap at
  max |Δlogit| **0.115–0.282** on hybrid Qwen3.5 against **0.002–0.003** on a dense
  llama model, once exceeding the top-two margin outright; `tests/speculative.rs`
  found the text differing on every run it ever made ("about the approach" vs "about
  the algorithm", and so on). Fixing the seed neither reveals nor prevents this.
- **Therefore opt-in**, `ROTEIRO_SPECULATIVE=1`, and *only* an explicit recognised
  "on" — an unrecognised value is not consent. A completion that changes because the
  decoder got faster must be asked for, not inherited by upgrading.
- **Split heads:** `ggml-org/Qwen3.8-27B-GGUF` — the shape the registry installs —
  ships its head as a separate file and records no `nextn_predict_layers` in its
  main GGUF. Found by convention (`mtp.gguf` beside `model.gguf`, where `mmproj.gguf`
  already sits), confirmed rather than trusted, and charged to the residency budget
  alongside its target.
- **Out of scope, deliberately:** multimodal requests (`mtmd_eval_chunks` decodes the
  prompt itself, so the drafter never sees those batches), and shared-KV MTP
  architectures (`new_context_with_ctx_other` would alias the target's memory across
  a teardown order this deliberately declines).
- **DoD:** no new link in the teardown chain — the draft context is a stack local
  borrowing the model, so "drafts before engines before backend" is the borrow
  checker's problem, not a discipline; a headless model falls back silently; and the
  identity claim is **not** asserted, because it is false. `tests/speculative.rs`
  asserts what holds (speculation activated, proposals accepted, control arm plain)
  and *reports* the divergence.
- **Not shippable as a default** until either llama.cpp's cross-batch-width numerics
  tighten or the divergence is judged acceptable as a product decision. That is an
  open question, not a task — see §9.
### Stage 31 — Model lifecycle: resumable pulls, removal, high tier ([ADR-0003](adr/0003-pluggable-embedding-models.md)) → **v1.11.0** · effort **M** ✅ *delivered* *(independent track)*

**Goal:** make a multi-gigabyte model store survivable. Nothing here touches the
graph, the schema or `EXTRACT_VERSION` — it is the store and its CLI only, which
is why it rides an independent track.

- **Resumable downloads.** `download_verified` deleted its temp file on *any*
  early return, so a transport failure at 90% of an 18 GiB pull threw away every
  byte. On an imperfect connection that is not "slow", it is *never finishes*:
  each attempt must win the whole race from zero. `download_resumable` keeps the
  partial and continues it with an HTTP `Range` request; the transport stays in
  the `roteiro` binary, so `rto-graph` still never touches the network.
- **The failure modes are deliberately not alike.** Transport failure keeps the
  bytes; a **checksum** failure discards them loudly (known-wrong bytes are not a
  prefix of anything); a server answering `200` where `206` was asked restarts
  rather than appending a whole-file body onto a prefix — which would corrupt the
  file and surface only as a checksum failure after another full transfer. A
  `.partial.json` sidecar records the URL, pinned digest and total size the bytes
  were started against, because anonymous bytes cannot be trusted as a prefix.
- **The non-obvious bug this class invites:** a dropped connection closes
  *cleanly*, so `io::copy` returns `Ok` having moved less than the whole file.
  Diagnosed naively that is a checksum failure — which discards the partial, and
  so defeats resumption for the commonest failure there is. The transferred
  length is checked explicitly.
- **`roteiro model rm`.** There was no supported way to remove a model; files
  accumulated with nothing but `rm -rf`. Removal reports what it freed, takes the
  whole directory (a model's file set changes between releases, and bytes
  `model list` no longer mentions are exactly the accumulation this stops), and
  clears orphaned partials. `model list` gained measured on-disk size, so "what
  would this reclaim?" is answerable before removing.
- **No in-use detection, stated rather than implied.** Roteiro keeps no lock or
  pid file over the model store, so a running `serve` holding a model is not
  something `rm` can discover. The help says so instead of implying a check.
- **Registry addition, not a replacement:** `ggml-org/Qwen3.8-27B-GGUF` Q4_K_M as
  the high generative tier. `ggml-org` over `unsloth` because the latter bundles
  MTP tensors into the main GGUF (allocated whether or not the head runs);
  Q4_K_M over Q5_K_M because generation on Metal is bandwidth-bound. The tiered
  matrix is not a leaderboard — `qwen2.5-coder-3b` and the rest stay for slower
  hardware.
- **DoD:** resume proved over a local socket (interrupted transfer re-requests
  only the remainder) *and* against the live host; the pinned SHA-256 measured
  from the downloaded file rather than quoted, since Hugging Face publishes none;
  the model shown to load on the pinned engine **and** to emit a `<tool_call>`,
  because a served model that cannot reach the graph tools is much less useful.

### Stage 32 — Guardrails: four ways a wrong answer looked like a right one → **v1.12.0** · effort **M** ✅ *delivered* *(independent track)*

**Goal:** close four defects that share one shape — each let Roteiro, or its own
documentation, state something false *confidently*. Every one of them was found
during this project's own development, and every one passed CI. The fix in each
case is to make the failure **loud**, not merely to make the output correct.

- **Duplicate `adr-id` (#324).** ADR nodes are keyed `adr:NNNN`, so two ADR files
  declaring one id collapse into a single node: `query adr:0016` answers for one
  decision while the other is invisible, `@rto:0016` binds to whichever won, and
  the published artifact carries only the survivor. The two files merge cleanly
  in git and `check` reported **0 violations**. This happened here — two parallel
  branches both authored ADR-0016. `check` now reports a `duplicate-adr-id`
  violation naming **both** paths and the id. The collision class does not exist
  for blueprints, `lat.md`, files or symbols: their ids *are* their paths, and a
  tree cannot hold two files at one path.
- **Graph API applied no debt exclusions (#321).** `debt(s, &[], &[])` — the
  ignore lists passed empty — so the explorer UI counted markers the CLI
  excludes and browser and terminal disagreed about the same repository. Fixed on
  three axes: pass the exclusions; resolve them **per repo** from the target's
  own root (ADR-0009's rule extended to scanning — *a repository's own config
  governs how it is scanned, whoever is asking*); and make `[debt] ignore`
  **merge** across config layers instead of the project layer silently discarding
  the user layer, with an explicit `ignore_reset` as the way to inherit nothing.
  ADR-0007 amended to v1.1. The MCP `debt` tool had the same defect.
- **The 85% coverage ratchet was never wired into CI (#319).** The docs asserted
  `cargo-llvm-cov` ran with a per-file floor; the workflow contained no coverage
  tooling at all, making every DoD citing "85% coverage" unverifiable. Coverage
  is now **measured, non-blocking**, and every document saying otherwise was
  corrected. Measured baseline: **87.51% lines** workspace-wide, **7 of 64 files**
  below 85%. The workspace already clears 85%; a *per-file* gate would fail seven
  files whose coverage is low for reasons about what the code does (CLI wiring,
  and paths needing a loaded model, a GPU or a sandboxed subprocess). Choosing
  the threshold is deliberately a separate change, now informed by real numbers.
- **Worktrees and the graph store (#330).** Investigated, and the reported root
  cause did not hold: `graph.db` already lives under each worktree's **own** git
  dir, not the shared common dir — verified with real linked worktrees. The
  *observed* symptoms ("`check` said 17 ADRs while 18 files sat on disk";
  "`sync` said up to date while the store lacked three ADRs") were reproduced,
  and their real cause is unrelated to worktrees: `sync_worktree` deliberately
  overlays **untracked** files into the derived layer, while the authored layer
  read only the `HEAD` tree — so a brand-new ADR had its symbols extracted but
  was never parsed as an ADR, in a single tree, silently. Fixed by making the two
  layers agree. The per-worktree layout is now pinned by a test, and a
  worktree stamp (migration 12) makes a store that *does* come to hold another
  tree rebuild loudly rather than answer "up to date".
- **A migration could be skipped permanently.** `apply` ran migrations with
  `version > MAX(recorded)`, so a store stamped by a build that knew migration
  **13** but not **12** never got 12 — `12 > 13` is false, forever. The store
  opened cleanly, reported a schema it did not have, and failed at run time on
  the missing column. Reproduced against a copy of this repository's real
  `graph.db`: `sync` died on `no such column: worktree`, i.e. **the #330 tree
  stamp added above was itself the thing silently absent**. Selection is now by
  **set membership**, so an unrecorded migration is repaired on the next open
  wherever it sits; ordering comes from the migration list, which a `const`
  assertion holds strictly ascending at **compile time**. `schema_version()` now
  reports the highest *gap-free* version rather than the maximum, so it cannot
  name a schema the store lacks. No gate could have caught this: CI always starts
  from a fresh store, where both rules agree. It is the `EXTRACT_VERSION`
  incident's shape exactly — two independently-correct branches, a failure that
  exists only in the combination. **Consequence:** merging guardrails before
  stage25 was load-bearing and a mistake would have been permanent for any store
  that met stage25 first; it is now a preference.

**Known gap, not fixed here (separate work).** The *other* direction is still
silent: a store at migration 13 opened by a binary that knows only 1..12 opens
without complaint. Reads stay sound — migrations are additive in effect, so the
columns an older binary reads still exist — but `sync` would re-extract under an
older `EXTRACT_VERSION` and **rewrite the graph with worse content**: a silent
downgrade, not a crash. A hard error in `apply` was considered and rejected as
the wrong granularity, since it would also block the reads that are provably
safe. The fix belongs on the *write* paths (`sync`/`reconcile`/`rebuild`),
refusing to rewrite a graph whose store is newer than the binary, and it needs a
`StoreError` variant — a semver-visible addition on a 1.x crate. Filed rather
than folded in.

**Deliberately NOT done:** per-worktree databases. `findings`, `media_content`,
`agent_memory` and `imports` all live inside `graph.db`, and [ADR-0013](adr/0013-agent-memory-artifact-store.md)
v1.1 depends on that store being **shared** — its scope rule (a memory applies
wherever its anchor resolves, with no branch bookkeeping) was demonstrated with
one row in one store giving opposite verdicts on two branches. Splitting the
database per worktree would silently reintroduce the branch-scoping that ADR
rejected, and would need the ADR **amended**, not extended. Note the distinction
the codebase already draws and this preserves: `ObjectCache` is content-addressed
by blob id, so sharing it across worktrees is *correct and valuable*; the
assembled graph is not, so sharing it would mean last-writer-wins.

- **DoD:** every guard proved by fault injection — the guarded behaviour broken,
  the test watched to go red, the file restored and verified byte-identical —
  because every real defect found in this project today passed CI, and an
  untested-for-failure test is an assumption in costume. No absolute assertions
  on shared constants (`EXTRACT_VERSION`, `schema_version`, migration counts);
  migration 12 is covered by the existing additive-migration property test (#329)
  rather than a pinned version number.

### Stage 33 — Local model resolution → **v1.16.0** · effort **S–M** ✅ *delivered (independent track)*

**Goal:** one place decides which model serves a task, and can say why.

**The user-facing gap:** `[models]` has keys for `embedding` and `generative`
only. Vision, audio and OCR are **hard-coded string constants**
(`voxtral-mini-3b`, `smolvlm-500m-gguf`, `ocrs-text`), so **a project cannot pin
its ASR model today**. Seven surfaces each pick a model by their own rule —
`spec draft`, `infer --model`, `serve` load, `serve`/Ask answer, media
generation, OCR during `sync` — and none knows the others exist.

**Shape:** one function in `rto-graph` — the crate that structurally cannot reach
the network — taking `(task_kind, modality, config, host_platform)` and returning
the model **plus the rule that chose it**, folding in the scattered call sites.
Deterministic rules over categorical signals, not a classifier: every reliably
observable signal here is low-cardinality (installed, modality, build feature,
task kind), and **a table over categoricals *is* the correct model**.

The seed already exists and is the right one: `chat_capable_model_ids` filters
models that *cannot do the job*, and exists because routing a BERT encoder
through `/v1/chat/completions` aborts llama.cpp with a `GGML_ASSERT`. Generalise
that, rather than starting from "which model is best".

**No network, no new dependency, no ADR** — it implements what ADR-0003 and
ADR-0007 already document, so it takes amendments with version-history rows, not
a new decision.

**DoD:** vision, audio and OCR models are configurable and pinned per project;
`roteiro config` answers *why did it use that model?* for every surface;
resolution is deterministic and unit-tested without loading a model.

#### What shipped

`rto_graph::model_choice` — `resolve_with(task, pins) -> Result<ModelChoice,
ModelChoiceError>`, plus a process-wide pin slot published once at startup beside
the existing `[paths] model_store` one. `ModelChoice` carries the model, the rule
(`pinned` / `built-in default`), and whether the weights are on disk; the error
type names the offending key.

`[models]` grew to five keys — `embedding`, `generative`, `vision`, `audio`,
`ocr` — one per model **kind**, not per command, so `generative` governs both
`spec draft` and Ask.

**Signature, corrected.** The plan said `(task_kind, modality, config,
host_platform)`. `modality` turned out to be the same axis as `task_kind` — a
`transcribe` task *is* the audio modality — so a separate parameter would have
admitted the meaningless pair `(Ocr, Audio)`. Host platform is read inside, from
the registry's existing `Platform::host()`, rather than passed: making it an
argument would have let a caller ask about a machine that is not the one the model
must load on. The signature is `(task, pins)`.

**Nine call sites, not seven.** The plan's seven are all real and all folded in:
`spec draft` (generative), `infer` (embedding — the config half only; a `--model`
flag still wins and is validated by the embedder), `serve` load (`served_models`'
kind filter), `serve`/Ask (`chat_capable_model_ids`, plus a startup check so a bad
pin fails before the listener opens rather than per request), media generation
**audio**, media generation **vision**, and OCR during `sync`. Two the plan did
not name turned up while enumerating:

- **`media status`**, which told an operator to `roteiro model pull` the built-in
  default even when the project had pinned another model — advice that would have
  them download the wrong weights and still be unable to build.
- **The extraction cache key** (`media_env_tag`), which folded `ocrs-text` *by
  name*. Left alone it would have made `[models] ocr` the one pin that changes
  what is extracted without invalidating what was extracted before it, so a
  repository would keep serving text read by a model it no longer uses.

**Two behaviours changed for a set config, deliberately.** `spec draft` used to
*filter out* a `[models] generative` that was not a generative model and fall
through to the default — a silent fallback, and exactly the failure this stage
exists to remove; it now refuses, naming the key. And a pinned model that is not
installed is now a hard error there, matching what `roteiro infer` has always done
with a configured embedding model. Unset behaviour is unchanged on every surface.

**The one exception to failing loudly** is `roteiro config` itself, which reports
a bad key rather than refusing — it is the command an operator runs *because* a
pin is misbehaving, so it must not be the command the pin breaks.

**Cost, measured:** **+1,667 / −106 lines across 12 files** (1,635 of the
insertions are Rust across 9 files; the rest is the two ADR amendments, this
entry, and the website's config sample). Against an estimate of **S–M**. The bulk
is the resolver itself (769 lines, of which roughly half is the module's own
documentation and its 12 unit tests) and the new 355-line CLI test. As with Stage
26, the surfaced-everywhere work is what costs: 366 lines of `main.rs` are the
call sites, the `roteiro config` resolution table, and its `--json` twin.

**Gates:** `fmt` clean; `clippy --all-targets` and `clippy --all-targets
--all-features` clean at `-D warnings`; `cargo test --workspace --no-fail-fast`
853 passed / 0 failed; `--all-features --no-fail-fast` 1,073 passed / 0 failed.
`EXTRACT_VERSION` unchanged at 11, no migration, no new dependency, no network.
Every new test was fault-injected — 12 unit tests and 6 CLI tests, each shown to
fail under a mutation of the behaviour it claims to check, with the tree
byte-identical afterwards.

---

### Stage 34 — Remote model tier ([ADR-0019](adr/0019-remote-model-tier.md)) → **v1.17.0** · effort **L** *(independent track)* 🔶 *part 1 of 2 delivered*

**Unblocked.** [ADR-0019](adr/0019-remote-model-tier.md) is **Accepted** (2026-08-17), so this stage has a settled contract to build against. It remains the largest posture change in the project: the first capability that sends repository content off the machine.

**Cut in two, guard first.** Part 1 — the consent gate, the payload allow-list, the dry-run and the egress record — has landed, in a build that compiles **no backend and therefore cannot send anything**. Part 2 is the transport and the promise amendments it makes necessary. See *What shipped* below for the seam and for what part 2 still owes.

**Goal:** an optional, explicitly-consented remote model backend for work local
models cannot do.

**Why an ADR is a prerequisite rather than paperwork.** This is the first
capability that sends repository content off the machine, and three written
promises currently forbid it:

- **ADR-0006** says *"nothing leaves the machine"* — twice. This contradicts it;
  it does not extend it.
- **ADR-0007**'s precedence must **invert for one key**: `roteiro.toml` is
  committed and shared by design, so a project file may **deny but never grant**.
  Grant lives at the user layer plus the invocation — both required, neither
  sufficient. A teammate must not inherit egress from a merged line.
- **Principle 10** (*offline-capable, not offline*) works because optional assets
  are digest-pinned and prefetched. A remote call is fetching by definition and
  can be neither pinned nor prefetched — so it must be **exempted**, explicitly.

**The framing that decides the design:** mis-routing among local models wastes
tokens; mis-routing *outward* sends source off the machine for a reason nobody
can inspect. **The local→remote edge is not a routing decision — it is a gate the
user opened.** So no learned router, at any model quality.

**And the disclosure gap must be stated in the ADR, not deferred:** extraction
redacts secret-*named* config keys before persistence, but that is name-matching
over ten needles and **there is no redaction chokepoint on a prompt**. Prompts
carry symbol names and prose; `DATABASE_URL=postgres://user:pw@host` matches none
of those needles.

**Also unresolved by design:** ADR-0015's `Producer` identity folds a
`model_digest`. A hosted model has no digest — a vendor model string is a
**mutable pointer**, and the weights behind it can change while the name does
not. If remote output is ever stored, it needs `ProducerTrust::{PinnedDigest,
VendorAsserted}` so a record states on its face that its identity is a *claim*.

**Sequencing note:** Stage 27 re-audits *every "offline" claim*. Landing this
before it converts a documentation task into a re-litigation of the product's
identity.

#### What shipped — part 1 of 2: **the guard, before the capability**

The stage is **cut in two**, at the seam ADR-0019's own structure suggests. Part
1 is everything that decides, shows and records; part 2 is the thing that sends.
The reason is not PR size, though the size is real: an egress path whose guard
lands in the same change as its transport is a guard nobody reviewed on its own,
and ADR-0019 §4 names that failure explicitly — *"deferring this is how an egress
path ships before its guard"*. Landing the guard **first** inverts that, and
leaves a build that cannot send anything at all to review it in.

**`rto-remote`** — a new crate holding the policy and **no HTTP client**.
`call_with` takes the transport as a caller-supplied closure, exactly as
`rto-exec` takes its `Fetcher`, so the code that decides whether bytes may leave
is not the code that can make them leave. The guarantee is checkable from a
`Cargo.toml` rather than promised in prose, and every test exercises the whole
path with no network — a test cannot accidentally become the first thing that
sends data. `rto-graph` gains nothing: its `gix` is still pinned
`default-features = false`, and `rto-remote` depends *on* it.

Five modules, one per clause:

- **`consent`** — ADR-0019 §3's inversion. `ConfigGrant::from_layers` is the
  workspace's single implementation of "a project may deny but never grant", and
  the binary's config layering calls it, so the value `roteiro config` echoes and
  the value the gate consults cannot drift apart. Six named `Reason`s, each with
  a remedy — except `ProjectDenied`, whose honest remedy is *"no flag overrides
  this; take it up with the repository"*. A discarded project grant is **reported
  rather than swallowed**: a committed setting that silently does nothing is
  worse than one refused out loud.
- **`payload`** — the allow-list as a *type*. `ContextItem::from_node` reads five
  named fields off a node — key, kind, name, path, and up to 1,500 characters of
  `meta.content`; every other key in its free-form `meta` is unreachable, and the
  test that proves it plants a credential in a sibling key. `disclosure()` says
  what leaves *and refuses to stop at the reassuring half*, naming the
  `DATABASE_URL` case that matches none of `is_secret_key`'s ten needles.
- **`record`** — the egress ledger at `$ROTEIRO_HOME/remote/egress.jsonl`
  (owner-only on Unix). Endpoint, model, `ProducerTrust`, timestamp and a copy of
  the body, written **before** the transport runs, so a call that hung is still a
  call you know about. **An unwritable ledger refuses the call** rather than
  sending unrecorded — the one ordering decision in `call_with` that is a policy
  rather than a convenience.
- **`escalation`** — the deterministic post-hoc check. `LocalAttempt` carries
  nothing but measurements of a finished run, so it **cannot be constructed
  before the local attempt happened** — a stronger guarantee than a comment
  saying so. A trigger is an input to the gate, never a substitute for it.
- **`trust`** — `ProducerTrust::{PinnedDigest, VendorAsserted}`, with the caveat
  a vendor-asserted record is displayed with.

**Config.** `[remote]` grows three keys, of which exactly one inverts. `enabled`
goes through `ConfigGrant`; `endpoint` and `model` are ordinary keys and layer
ordinarily, so a project may choose *where* its gateway is without being able to
turn the tier on. `roteiro config` prints the section as **layers rather than one
merged value**, because a reader applying the general precedence here would be
wrong about the one key where being wrong means believing egress is off when it
is on. Without the feature the section still prints, saying the build has no
tier — an omitted section reads as "no such setting".

**CLI.** `roteiro remote status | dry-run | log`, behind an **off-by-default**
`remote` feature that adds no third-party dependency. `status` reports the gate
layer by layer then the decision; `dry-run` prints the exact bytes and sends
nothing; `log` reads the ledger and says *"nothing has left this machine"* rather
than leaving that to be inferred from silence.

**What is deliberately absent, and is part 2.** The `ureq` backend and the
`Transport` implementation over it; the TTY-prompt form of the invocation grant
(`status` and `dry-run` must never prompt, so the prompt belongs with the call);
wiring the tier into `spec draft` / Ask via `model_choice`; and the README and
website amendments. **Those docs are not owed yet**: with no backend compiled,
*"nothing leaves the machine"* is still literally true of every build this
produces, and amending it now would describe a capability that does not exist.
Part 2 owes them on the same commit that makes them false.

**One thing the resolver could not express, and was not routed around.**
`rto_graph::model_choice` resolves a *registry* model — a name with a variant, a
platform and a digest on disk. A remote model is none of those, and
`ModelChoice::installed` has no meaning for one. Part 1 needs no resolution (it
never picks a model to run; the endpoint names its own), so nothing bespoke was
added and nothing was worked around. Part 2 does need it, and the proposal is a
`ModelSource::Remote { trust }` variant plus a `ModelChoice::installed` that is
`None` rather than `false` for it — an amendment to Stage 33's resolver, not a
second selection rule beside it.

**Gates:** `fmt` clean; `clippy --all-targets` and `--all-targets --all-features`
clean at `-D warnings`; `cargo test --workspace --no-fail-fast` and
`--all-features --no-fail-fast` green. `EXTRACT_VERSION` unchanged at 11, no
migration, **no new dependency**, and no network — in either the code or the
tests. Every new test was fault-injected.

---

### Stage 35 — `roteiro review` LLM mode → **v1.18.0** · effort **M–L** *(independent track)*

**Depends on Stage 33** (a reviewer must resolve a model without a fourth
bespoke rule). Independent of Stage 34 — it can run wholly local.

> **Split into two PRs at the measurement seam.** 35a — the scoring harness and
> the suppression filter — is **delivered**: `roteiro review --score`, the corpus
> as a typed shipped asset, per-class recall, and `rto_graph::compile_claim`. It
> lands before any reviewer because it is what makes a reviewer's value a number
> instead of an impression, and because a "do not build" verdict needs the same
> harness a "build it" verdict does.
>
> 35b — the local reviewer arm — is **not built, pending 35a's numbers**. Two
> measurements from 35a constrain it, and both were taken on this repository
> rather than assumed:
>
> - **Whole-diff review is not the shape.** Reconstructing the 15 review diffs at
>   `-U3` costs ~513k tokens total, ~34k mean, 103k worst (PR #339). Against the
>   measured ~30k single-call budget, **9 of 15 do not fit in one call** before
>   any graph context is added. The ~79k per-file budget is the one to design to,
>   which puts `contract-drift` — the largest class, 5 of 22 — squarely on the
>   graph: per-file review cannot see a doc in another file contradicting the code
>   under review unless something hands it that doc. That is the claim to test,
>   and it is now testable.
> - **The corpus can falsify a reviewer but not finely rank two.** 22 real rows
>   over 13 classes, 8 of them holding a single row. A 0-or-2-of-22 result is
>   decisive; a 9-vs-11 difference is not. 35b's DoD should be a *floor* to clear,
>   not a percentage to maximise.
>
> 35b also needs a **resolver addition, not a workaround**: `ModelTask::Review`
> in `rto_graph::model_choice`, sharing the `generative` key with `Draft`/`Chat`.
> Adding a seventh task is the Stage 33-sanctioned move; a bespoke selection rule
> in the reviewer would be the fourth one Stage 33 exists to have removed.

**Goal:** give the adjudicated review corpus a consumer, and put the graph to
work on the one thing a diff-only reviewer structurally cannot see.

**The asset that already exists:** `crates/rto-graph/tests/fixtures/review/` holds
**26 adjudicated review comments** with verdicts, defect classes, and the
`reviewed_sha` each was left on. That makes a reviewer **measurable rather than
guessed at** — which most projects cannot do. It currently has **no consumer**,
and that is how a fixture rots.

**What the graph adds, stated honestly:** not access — Copilot has been agentic
since March 2026 and reads repository context via tool calls, verified against
this corpus (it cited a file outside a PR's changed set, correctly). What
`roteiro review` has is **pre-assembled, provenance-tagged** context: governing
ADRs, authored drift, blast radius, intent debt. A weaker claim than a moat, and
the one to test.

**Two constraints inherited from the investigation:**

- **A free precision filter.** On this corpus, *every* false positive was a
  compile-error claim and *every* compile-error claim was a false positive (4/4).
  CI's `msrv` job already refutes them ~60 s before a human reads the comment. So
  withhold any finding claiming the code will not compile while the relevant
  check is green **at that commit and configuration** — see
  `docs/REVIEW_CHECKLIST.md`, which records why "green build" alone is too coarse
  (ubuntu-only, `--all-features`; the macOS teardown abort of #291 was invisible
  to it).
- **Context budget measured, not guessed:** ~30k tokens single-call, ~79k
  per-file on this repository.

**DoD:** scored against the corpus with per-defect-class recall — **not an
average**, which hides the only thing an implementer needs — at the
`reviewed_sha` of each comment, never the PR head (merged heads contain the fix
commits, so scoring against them measures recall on already-fixed code and
silently reports zero).

**Delivered in 35a**, so 35b inherits rather than rebuilds it: `review --score`,
per-class recall with the denominators printed beside the rates, an outright
refusal to score a commit the corpus does not know (the PR-head guard), and
`compile_claim`'s coverage model. One thing 35a had to fix on the way: the
corpus README's own reconstruction recipe — `merge-base <base> <reviewed_sha>` —
**produced an empty diff for 13 of the 15 review commits**, because a merged PR
branch is an ancestor of `main` and the merge base is then the review commit
itself. A reviewer handed an empty diff also scores zero, silently, from the
opposite direction. The recipe is corrected and now has an executable form
(`every_row_reconstructs_a_non_empty_reviewed_diff`), which also checks that each
reconstructed diff touches the file its comment is anchored to.

**"Do not build this" remains an acceptable outcome.** The corpus keeps its value
either way: it is how any future reviewer, hosted or local, gets measured.

---

### Stage 27 — v2.0 hardening & release → **v2.0.0** · effort **M** ⏸️ *deferred by decision*

> **Deferred deliberately, not merely unstarted.** The owner's call, recorded so
> the two are distinguishable: a stage nobody has got to and a stage somebody
> decided to leave look identical six months later, and the second should not be
> picked up by whoever next has a free afternoon.
>
> Nothing *blocks* it — Stages 21–25 and 28–32 are delivered and v1.15.0 is out.
> It is held because the hardening it describes is worth more once the work ahead
> of it has landed and been measured: the remaining A1 lenses (Stage 26) and
> Stages 33–35. And because v2.0.0 is a number worth spending once, deliberately.
>
> Stage 34 in particular should land *before* this, not after: Stage 27 re-audits
> every "offline" claim, and adding a remote tier afterwards would reopen an audit
> that had just been closed.
>
> One consequence to carry: the scope below grew during v1.10–v1.15. The offline
> claim it re-audits is now a real surface — `docs/OFFLINE_SETUP.md`, the
> `security prefetch`/`status` contract, digest-pinned assets, and per-file
> verification of the extracted boxlite runtime — so this is an audit against
> something concrete rather than a prose sweep.

- Semver review: query output is explicitly versioned, so new query shapes carry
  semver weight.
- Scale benchmarks for every whole-graph lens and for memory recall.
- Docs: blueprint updated, `docs/JSON_SCHEMA.md` extended for findings + memory,
  every "offline" claim re-audited to say **offline-capable once provisioned**
  where that is the truth.
- Coverage **measured** across all new crates and the numbers reviewed before
  release (there is no ratchet enforcing 85% — issue #319); `cargo deny` clean
  with `--all-features` on the resolved native closure.

---

## 7. Milestones → releases

| Release | Contains | Gate |
|---|---|---|
| v1.10.0 ✅ | Stage 21 — analyzer contract + ingest | Artifact byte-identical; ingest idempotent — **met** |
| v1.11.0 ✅ | Stage 22 — semgrep + cargo-audit (SAST axis, five languages) | Offline warm-cache run; named cold-cache failure — **met** |
| v1.11.x ✅ | Stage 22b — `osv-scanner` (dependency axis: Python/Java/Node) | Lockfile findings per ecosystem; Rust overlap resolved — **met** |
| v1.11.0 ✅ | Stage 23 — episodic memory | Survives rebuild; graph untouched — **met** (#317) |
| v1.13.0 ✅ | Stage 24 — boxlite backend | Parity with subprocess; `cargo deny` clean — **met** (#352): identical finding keys via both backends, differing only in isolation label and image digest |
| v1.12.0 ✅ | Stage 25 — recall + bounded cache | `decay=none` reproducible; no episodic eviction — **met** (#340). Shipped two releases ahead of its nominal target |
| v1.13.0 ✅ | Stage 26 — lenses Q3/Q1/S1 | **All three**, one PR each — Q3 (#346), Q1 (#372), S1. `coupling --limit 0` over 2,887 nodes in 0.07 s; `debt-density --limit 0` and `config-secrets --limit 0` (372 keys) both 0.04 s, level with the `debt` baseline. **No lens offers a CI gate**, each saying why: Q3's cross-language call edges are name collisions (615/6,553 = 9.4%); Q1 inherits the marker scan's prose false positives and adds a file-length denominator; S1 is an inventory, and its one real finding indicts Roteiro's import layer, not the user's repo. `EXTRACT_VERSION` stayed **11** throughout. Cost: **11 files, 3,016 insertions** — the corrected 195–500 LOC estimate is itself out by ~3–8× |
| v1.10.x ✅ | Stage 28 — generated media content moves out of `derived` | Silent clip cannot reach default search; `media build` restores searchability — **met** |
| v1.11.0 ✅ | Stage 29 — audio metadata as `derived` facts | Format read costs 1–100 µs and instantiates no decoder; duration exact/estimated/absent never guessed — **met** |
| v1.11.0 ✅ | Stage 30 — MTP speculative decoding | Opt-in only; 1.22–1.50× on 27B — **but output is not identical**, so default-on is blocked on §9.6 |
| v1.11.0 ✅ | Stage 31 — model lifecycle: resumable pulls, `model rm`, high tier | Interrupted pull transfers only the remainder; checksum failure discards; pinned digest measured, not quoted |
| v1.12.0 ✅ | Stage 32 — guardrails: four confident wrong answers (#324, #321, #319, #330) | Two ADRs on one id fail `check` naming both files; API and CLI debt agree, per repo; coverage measured (87.51% lines, 7/64 files under 85%) with no document claiming a gate that does not run; a new ADR on disk is never silently uncounted — **met** |
| v1.16.0 | Stage 33 — local model resolution | Vision/audio/OCR pinnable per project; `roteiro config` answers *why that model* for every surface |
| v1.17.0 | Stage 34 — remote model tier | **Gated on ADR-0019.** Project file may deny, never grant; no learned router on the local→remote edge |
| v1.18.0 | Stage 35 — `roteiro review` LLM mode | Scored against the in-tree corpus at each comment's `reviewed_sha`, per defect class |
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
| Speculative decoding silently changes a completion | **High** | Measured, not hypothetical (Stage 30). Off unless `ROTEIRO_SPECULATIVE` explicitly says on; unrecognised values are not consent. The risk is *acceptance by default*, so the mitigation is that there is no default. |

---

## 8b. Beyond v2.0 — deferred work, and where it is written down

Stage 27 is the last *scheduled* stage. It is not the last work, and the
difference has been invisible: everything below was decided somewhere in this
document and then scattered across §9 and Stage 26's deferral list, where a
reader planning the next quarter would not find it. This section is a map, not a
new commitment — each item keeps its original reasoning at the reference given.

### Already decided, deliberately not scheduled

| Item | Where | Why it is not in a stage |
|---|---|---|
| **Semantic recall** — vector index over memory | §9.3 | Needs migration, model/dimension versioning, retention, rebuild and storage-size policy. Materially more than "persist embeddings". |
| **Findings ↔ graph cross-surfacing** | §9.7 | Joining findings to graph facts is deliberately not free in this design. When wanted, it needs a *designed* join, not an implicit one. |
| **`code_interpreter`** | §9.4, ADR-0014 | Rejected. The real question is *is local code execution something Roteiro wants to be?* — a product decision, not a backend one. |
| **Q2 — LOC hotspots** | Stage 26 | Not a pure query: `Node.span` is *byte offsets*, so it needs net-new extraction metadata. |
| **Q10 — dependency pins** | Stage 26 | Mis-scoped as written; existing pins are Docker `image_ref` and submodules, so package-manifest pins are extraction work. |
| **Q7 — doc coverage** | Stage 26 | Needs a language and a denominator; docs live mostly in symbol `meta.content`, not `Doc` nodes. |
| **S2–S6** — the rest of the security lens series | Stage 26 | Taxonomy normalised (S1, S4 → `GDS`; S2, S3 → `NNX`; S5, S6 → `EXT`), but none is scoped. |

### The batching constraint that shapes all of it

**Q2, Q10 and cross-language call-edge resolution each need extraction metadata,
so each forces an `EXTRACT_VERSION` bump — and every bump is a full
re-extraction for every user.** The risk register already says to batch
extraction-touching lenses behind a single bump. That makes these a *cluster*
rather than three independent tickets: doing them one at a time is the expensive
way to do the same work.

Cross-language call-edge resolution belongs in that cluster and is not yet
recorded anywhere else. Stage 26's Q3 measured **615 of 6,553 call edges (9.4%)
on this repository as cross-language name collisions** — cross-file resolution
binds a callee by simple name across every `Fn` node regardless of language, and
no FFI is extracted. That is why Q3 offers no CI gate. Fixing it is extraction
work, so it batches with Q2 and Q10 or it is paid for twice.

#### The one exception, and why it was made

**The `placeholder` marker-needle correction (#384) spent a bump on its own**
(`EXTRACT_VERSION` 11 → 12), by the owner's decision. It is not a fourth member of
the cluster above: it shipped alone, and Q2, Q10 and cross-language edge
resolution remain a cluster **with each other**, still unpaid and still
unscheduled — this exception does not release them.

The trade the batching rule is meant to prevent is *paying twice for the same
work*. That is not what this was. The bare word `placeholder` was a `stub` needle
scoring **0% precision — 36 of 36 findings on this repository, none a stub**: the
external-ref placeholder node (ADR-0009), the redaction placeholder (ADR-0015),
S1's own sentence about not being able to tell a secret from a placeholder, a
`{tag}` ref template, CSS `::placeholder`. The lens was reporting the codebase's
vocabulary as its debt, and `roteiro check` printed the inflated figure at a
glance. Replacing the word with the two phrases that predicate incompleteness of
an implementation — `placeholder implementation`, `returns a placeholder` — takes
`stub` from 36 to **0**, the true count, and reclassifies nothing else.

Holding that behind Q2, Q10 and cross-language edge resolution would have meant
shipping a knowingly false count for the whole of an unscheduled, post-v2.0
horizon — indefinitely, since none of the three has a date. Stage 26's standard
is that a lens which over-reports is worse than none; a 100%-noise category is
the case that standard was written for. Correctness of a number users read now
outweighed the cost of one re-extraction, so the batching rule was set aside
deliberately rather than forgotten. It still governs the three items above: they
each add *new* extraction metadata, they are genuinely one body of work, and
nothing about this exception makes them cheaper to do separately.

**What the bump cost, measured** on a store extracted at version 11 and then
opened by the version-12 binary: **all 275 cached fact sets re-extracted** (every
tracked blob — the base version is unconditional, so nothing survives the key
change), 3.2 s cold against 0.17 s warm on a debug build. The object cache is
write-and-keep with no eviction (`crates/rto-graph/src/cache.rs`), so the
superseded version-11 entries stay on disk: `.git/roteiro/objects` went
**4.8 MiB → 9.7 MiB** and does not shrink again. That is per repository, per
user, and it is the whole price —
`.git/roteiro` is derived, so deleting it is always safe if a user would rather
reclaim the space than keep the old entries.

### Now scheduled as Stages 33–35

Formerly scoped-but-unrecorded; added to the roadmap by decision. Summarised here
because §8b is where a reader looks for what outlives the current stage — the
stages themselves carry the detail:

- **Stage 33 — local model resolution.** Closes a user-facing gap on its own
  merits: a project cannot pin its ASR model today. No network, no new
  dependency, no ADR.
- **Stage 34 — remote model tier.** **Blocked on ADR-0019**, which must amend
  ADR-0006, invert ADR-0007's precedence for one key, and exempt principle 10.
  Not startable until that ADR is accepted.
- **Stage 35 — `roteiro review` LLM mode.** Depends on Stage 33. Gives the
  26-comment adjudicated corpus a consumer before it rots.

---

## 9. Open questions (decide before the stage that needs them)

1. ~~**Cache bound value** (Stage 25)~~ — **answered: 256 MB by default,
   configurable** (decided by the owner). The unit was already settled as a byte
   budget following `ModelCache`; this fixes the number and makes it raisable for
   larger repositories.

   The scale that justifies it, measured on this repository: `.git/roteiro` is
   **49 MB** (44 MB object cache over 2,395 entries, 4.6 MB `graph.db`) against a
   **91 MB** `.git`. Roteiro's sidecar is already ~54% of the repository it
   describes, so a cache tier is not a new cost category — it is a bound on one
   that is currently unbounded in every direction. 256 MB is small against `.git`,
   trivial against an 18 GB model store, and large enough that an ordinary session
   never evicts.

   Erring small is deliberate and cheap: `build_context` is *proven* to reconstruct
   identically (`context.rs` asserts `built == cached`), so **eviction costs cycles,
   never information**. Erring large only costs disk. Neither error is expensive,
   which is precisely why this did not warrant more analysis than a measurement.
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
6. ~~**Is a faster decoder worth a different completion?**~~ **Answered: yes**
   (Stage 30, decided by the owner). Speculative decoding is measurably 1.22–1.50×
   on a 27B model and measurably does **not** reproduce plain decoding's text. Both
   halves are settled measurements; the judgement was whether Roteiro accepts the
   second to get the first, and it does.

   What that does **not** license is flipping the default. Generation was never a
   reproducible surface — sampling, quantisation and the served model all move the
   text already — so this changes how *fast* an already-variable answer arrives,
   not whether Roteiro keeps a promise it was making. The graph is where
   reproducibility is promised, and nothing in Stage 30 touches it. But the
   remaining honest reasons to keep `ROTEIRO_SPECULATIVE` opt-in stand on their
   own: the win is **size-dependent** (0.86–1.14× on a 9B — noise), it needs a
   draft head that most installed models do not ship, and the identity claim has
   never been observed to hold on any model. A default that helps one model class
   and silently changes output on the rest is a worse default than none. Revisit
   when a draft head is present on the common tier, not before.
7. **Findings ↔ graph cross-surfacing**: joining findings to graph facts is
   deliberately not free in this design. When it is wanted, it needs a designed
   join, not an implicit one.

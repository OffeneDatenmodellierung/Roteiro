# Roteiro — Build Plan

Status: Draft · Owner: The Roteiro Project Team · Last-modified: 2026-08-07
Governing decision: [ADR-0001](adr/0001-build-roteiro-unified-codebase-knowledge-graph.md)

This plan takes Roteiro from the current v0.0.1 scaffold to a dogfooded v1.0. It
is organised as sequenced **stages**, each ending in a shippable release cut by
release-plz. Every stage names its deliverables, the concrete Rust surface it
adds, new dependencies (with licence notes for the `cargo deny` gate), the CLI
it wires up, and an explicit **Definition of Done (DoD)**.

---

## 1. Principles (invariants that constrain every stage)

These come from ADR-0001 and must hold at every release, not just at v1.0:

1. **Provenance is first-class.** Every edge is `derived` | `authored` |
   `inferred`, and `inferred` edges carry a confidence score. No code path may
   produce an unlabelled edge.
2. **One query surface.** Humans (docs/vault) and agents (CLI `--json`, MCP)
   read the *same* store. Renderers are pure build-outputs of the graph.
3. **Offline by default.** No network needed to build or query. Grammars are
   compiled in; the only optional network call is fetching a CI-published
   artifact, with local rebuild as the fallback.
4. **Git-native & deterministic.** Extraction is a pure function of a git blob;
   the same blob always yields the same facts. The cache is content-addressed
   by git object id so branches/worktrees share work.
5. **Precise-where-known, fuzzy-where-suggested.** Deterministic derivation and
   human authoring win; inference is clearly marked as suggestion.
6. **Dogfooded.** Roteiro runs on its own repo in CI; `roteiro check` gates the
   build from the first stage it exists.
7. **Quality gates from day one.** fmt, clippy (`-D warnings`, pedantic), audit,
   deny, and an 85% per-file coverage ratchet. `unsafe_code = "forbid"`.

---

## 2. Current state (v0.0.1 — done)

| Crate | What exists today | What's missing |
|---|---|---|
| `rto-graph` | `Store` (SQLite, `nodes`/`edges` schema, `open`/`open_in_memory`/`node_count`), `Provenance` enum | Insert/query API, node identity, migrations, extraction, cache |
| `rto-spec` | `AdrStatus`, `AdrMeta`, status `FromStr` | Full ADR/blueprint parser, wiki-link/annotation edges, `check`, dedup |
| `rto-render` | `Target` enum (`docs`/`obsidian`) | Any actual rendering (site is a shell `build.sh` + `md2html.awk` stopgap) |
| `roteiro` | CLI with `init/sync/check/import/render/spec/serve` stubs that `bail!` | All behaviour |

Infra already in place: workspace (edition 2024, MSRV 1.94, `rusqlite =0.39`
pin), CI (`checks` + `msrv`), release-plz, Cloudflare Pages site, all actions
SHA-pinned. This plan assumes that baseline.

---

## 3. Crate responsibilities & dependency graph

```
        roteiro (CLI, arg parsing, wiring, hooks, init)
        /        |          \
  rto-spec   rto-render    (rto-graph re-exported)
        \        |          /
              rto-graph  (store, model, cache, extraction, query)
```

- `rto-graph` is the foundation and depends on nothing in-workspace.
- `rto-spec` and `rto-render` depend only on `rto-graph`.
- `roteiro` depends on all three; owns process concerns (args, TTY, git hooks,
  filesystem, exit codes) so the libraries stay side-effect-light and testable.

**Rule:** libraries return typed errors and data; the `roteiro` binary owns
`anyhow`, stdout/stderr, and exit codes.

---

## 4. Data model & schema plan

The v0.0.1 schema is a starting point. Target model:

- **Node**: `id` (autoinc), `key` (unique natural id), `kind`
  (`fn|struct|enum|trait|module|file|adr|adr_section|blueprint|doc|…`), `name`,
  `path`, `lang`, `blob_hash`, `span` (byte range), `meta` (JSON).
  - Natural key scheme (deterministic, upsertable):
    `sym:<lang>:<path>#<name>` for code symbols, `adr:<id>` /
    `adr:<id>#<section-slug>` for ADR nodes, `file:<path>` for files.
- **Edge**: `id`, `src`, `dst`, `kind` (`calls|imports|defines|contains|
  references|supersedes|authored_by|inferred_from|…`), `provenance`,
  `confidence` (NULL unless `inferred`), `src_ref` (where the fact came from:
  blob hash + span, or ADR id).
- **Schema versioning**: a `schema_migrations` table + an ordered list of
  migration SQL applied idempotently on `open`. Never mutate a shipped
  migration; always append.
- **Invariant enforced in-DB** (already partly present): `CHECK` on provenance;
  add `CHECK (provenance <> 'inferred' OR confidence IS NOT NULL)`.

Extraction outputs a **fact set per blob** (nodes+edges scoped to one file),
which is what gets content-addressed and cached. Assembly merges fact sets for
all blobs in a tree, then resolves cross-file references (e.g. a `calls` edge's
`dst` symbol) into node ids.

---

## 5. Staged roadmap

Each stage is independently shippable and leaves `main` green + dogfoodable.

### Stage 1 — Graph core & query primitives  → **v0.1.0** ✅ *delivered*
**Goal:** a real, typed, transactional store the rest of the system builds on.
- `rto-graph`: `Node`, `Edge`, `NodeKind`, `EdgeKind` types; `FactSet` (nodes +
  edges for one blob). Builder/insert API: `upsert_node`, `insert_edge`,
  `apply_factset` (transactional), `get_node`, `neighbors`, `query(kind, prov)`.
- Schema migrations framework + `schema_migrations` table.
- Deterministic natural-key derivation and upsert semantics.
- New deps: none beyond current (rusqlite, serde). Add `serde_json` for `meta`.
- CLI: none yet (library milestone), but add `roteiro --version`/help polish.
- **DoD:** round-trip property test (insert arbitrary FactSet → query → equal);
  migration idempotency test; 85% coverage; docs on every public item.
- **Status: delivered.** `rto-graph` now has `NodeKind`/`EdgeKind` (open sets),
  `Node`/`Edge`/`Span`/`FactSet`, an append-only migrations framework, and the
  full insert/query API (`upsert_node`, `insert_edge`, `apply_factset`,
  `get_node`, `nodes_by_kind`, `edges_from`/`edges_to`, `edges_by_provenance`,
  `neighbors`). The provenance/confidence invariant is enforced in Rust and by a
  DB `CHECK`. An exhaustive dependency-free round-trip test replaces `proptest`
  (keeping the stage's "only `serde_json`" dep budget). Coverage: ~94% regions /
  ~98% lines; clippy (pedantic, `-D warnings`) and fmt clean on the 1.94 MSRV.

### Stage 2 — Content-addressed cache & `roteiro sync`  → **v0.2.0** ✅ *delivered (core)*
**Goal:** git-native incremental graph updates.
- `rto-graph::cache`: compute git blob/tree ids, store per-blob FactSets under
  `.git/roteiro/objects/<blob-id>`, key the assembled snapshot by tree id.
- Worktree-aware: per-worktree dirty-file overlay (hash working-copy bytes;
  extract in-memory, do not persist dirty facts to shared objects).
- Incremental algorithm: diff HEAD tree vs last-synced tree → re-extract only
  changed blobs; unchanged blobs load from cache.
- New deps: **`gix`** (pure-Rust git; MIT OR Apache-2.0 — offline-friendly,
  matches licence policy). **Decided** over `git2`/libgit2: pure-Rust is
  preferred where workable. FactSet on-disk codec: **JSON** (see Q2).
- CLI: `roteiro sync` (full + incremental), `--json` summary of work done.
- **DoD:** fixture-repo integration test proving (a) cold sync populates store,
  (b) no-op sync after unchanged tree touches zero blobs, (c) single-file change
  re-extracts exactly one blob; cache is branch/worktree-correct.
- **Status: delivered (core).** `gix` (pure-Rust, `sha1`/`status`/`dirwalk`/
  `revision`, no default features → no network transports) added and passing
  `cargo deny`. New modules in `rto-graph`: `cache` (git-style sharded, atomic
  JSON object store under the *common* git dir → shared across worktrees),
  `extract` (`Extractor` trait + `FileNodeExtractor` placeholder until Stage 3),
  `git` (thin gix wrapper), `sync` (tree-id short-circuit + per-blob
  content-addressed extract/cache + transactional `Store::rebuild`). Migration 2
  adds `sync_state`. `roteiro sync [--json]` wired and dogfooded on this repo.
  All four DoD criteria pass via a fixture-repo integration test (cold /
  no-op / single-file / cache-reuse-across-stores). Coverage ~93% region /
  ~97% line; clippy pedantic + deny + msrv clean.
  - ~~**Deferred to a Stage 2 follow-up:**~~ the uncommitted working-tree dirty
    overlay — **delivered in Stage 3** (see below).

### Stage 3 — Derived extraction (tree-sitter)  → **v0.3.0** ✅ *delivered (Rust + dirty overlay)*
**Goal:** the `derived` provenance class for real code.
- `rto-graph::extract`: language registry; per-language tree-sitter grammar +
  `.scm` query patterns → symbols (`defines`), calls (`calls`), imports
  (`imports`), containment (`contains`).
- **Language order:** Rust first (dogfood target), then TypeScript/JavaScript,
  then Python — pick the smallest set that exercises the abstractions before
  breadth.
- Deterministic ordering of emitted facts (stable cache keys).
- New deps: `tree-sitter`, `tree-sitter-rust` (+ later grammar crates). Licence:
  tree-sitter MIT; **each grammar's licence must be verified** and added to the
  `cargo deny` allowlist — this is a recurring gate as languages are added.
- CLI: `roteiro sync` now produces derived edges; `--json` node/edge counts.
- **DoD:** extract Roteiro's own crates; assert known symbols/edges exist
  (e.g. `Store::open` `defines` node, `main` `calls` `Cli::parse`); snapshot
  test on a fixture file; extraction is idempotent and cache-stable.
- **Status: delivered (Rust extractor).** `RustExtractor` (tree-sitter) emits a
  `file` node, symbol nodes (`fn`/`struct`/`enum`/`trait`/`mod` + `type`/`macro`)
  with lexical `defines`/`contains` edges and path-scoped keys
  (`sym:rust:<path>#<qualified>`), `imports` edges for `use` declarations, and
  records each function's callee names in `meta.calls`. A `Registry` dispatches
  by extension (`.rs` → Rust, else the `FileNodeExtractor` fallback). Emitted
  facts are sorted for a byte-stable cache. **Cross-file `calls`** are resolved
  at assembly time in `sync` (unambiguous simple-name match only — ambiguous and
  external names are left unlinked rather than guessed). Deps `tree-sitter` +
  `tree-sitter-rust` (both MIT) pass `cargo deny`. Dogfooded on this repo:
  248 nodes / 356 edges (150 `calls`, 128 `defines`, 48 `imports`, 30
  `contains`), with `main → run_sync` resolved. Fixture integration test proves
  cross-file call resolution + cache-stable re-sync; ~92% region / ~97% line.
  - **A note on the DoD example:** `main calls Cli::parse` is an *external*
    (clap) call, which the unambiguous-resolver deliberately leaves unlinked; the
    test instead asserts an intra-tree cross-file call (`main → helper`), which
    is the honest, resolvable analogue.
  - **Dirty overlay — delivered.** `sync_worktree` overlays uncommitted edits to
    *tracked* files on top of the committed graph: a file is dirty when its
    working copy hashes (via `gix::objs::compute_hash`, no status enum) to a
    different blob id than `HEAD`; dirty files are re-extracted in memory (never
    cached), deleted files are dropped. The sync state encodes the dirty set so
    repeated previews no-op while a committed `sync` supersedes the overlay.
    `roteiro sync` uses it by default; `--committed` selects committed-only
    (for hooks/CI). New *untracked* files are not yet included (needs a
    gitignore-aware dirwalk) — a small follow-up. Dogfooded (+N uncommitted) and
    covered by a fixture test (edit → preview, delete → drop, committed
    supersede).
  - **Follow-up:** cross-file `calls` resolve by unambiguous simple name today; a
    scope-aware resolver (import paths + `Self` types) and untracked-file overlay
    are later refinements. TypeScript/JS and Python extractors follow.

### Stage 4 — Authored layer & `roteiro check` (rto-spec)  → **v0.4.0**
**Goal:** house ADR/blueprint intent linked into code; drift gating.
- `rto-spec`: full ADR/blueprint parser — YAML frontmatter → `AdrMeta`, sections
  → `adr_section` nodes; `[[path#Symbol]]` wiki links and `// @rto:<key>` source
  annotations → `authored` edges into code symbols.
- `roteiro check`: fail (non-zero) on drift — ADR references a missing symbol;
  code annotation references a superseded/absent ADR; broken `[[…]]` target.
- Structural duplication check (semantic dedup deferred to Stage 8).
- New deps: a maintained YAML reader (decision below — likely `saphyr`/
  `yaml-rust2`; **avoid unmaintained `serde_yaml`** which the audit/advisory
  gate will flag). `pulldown-cmark` for section/heading segmentation if the
  hand-parser proves fragile.
- CLI: `roteiro check` wired and added to Roteiro's own CI (dogfood gate).
- **DoD:** ADR-0001 parses; a deliberately-broken `[[path#Symbol]]` makes
  `check` exit non-zero; `check` passes on the real repo and runs in CI.

### Stage 5 — Query surface, `--json`, `init` & git hooks  → **v0.5.0**
**Goal:** the agent-facing interface and zero-touch freshness.
- `rto-graph::query`: mixed-provenance query API returning labelled results
  (path, explain, neighbours, by-symbol, by-ADR). Stable `--json` schema
  (versioned) — the primary agent interface.
- `roteiro init`: scaffold store, install `post-checkout`/`post-merge` git hooks
  (compare local store vs HEAD tree → no-op / fetch artifact / incremental
  rebuild), and drop an agent skill / `AGENTS.md` snippet.
- New deps: `serde_json` (in use); `clap` completions optional.
- CLI: `roteiro query …` (or subcommands), `roteiro init`.
- **DoD:** `--json` output snapshot-tested and documented; `init` on a clean
  clone installs working hooks; a checkout that changes files auto-updates the
  graph via the hook.

### Stage 6 — Renderers replace the shell stopgap (rto-render)  → **v0.6.0**
**Goal:** docs site + Obsidian vault as true graph build-outputs.
- `rto-render`: `roteiro render docs` produces the site (ADRs, blueprints,
  overview, per-subsystem AI-context pages) from the graph; `render obsidian`
  emits a vault. Retire `website/build.sh` + `md2html.awk` once parity is met.
- Deterministic output (byte-stable) so CI diffs are meaningful.
- New deps: a Markdown renderer (`pulldown-cmark`) + minimal templating (hand
  or `askama`/`minijinja` — decision below). No JS runtime.
- CLI: `roteiro render <docs|obsidian> [--out DIR]`.
- **DoD:** rendered site reaches parity with the current stopgap (headings,
  tables, links, theme, back-links — the bugs we already hit), snapshot-tested;
  Cloudflare build command switches to `roteiro render docs`.

### Stage 7 — MCP server (feature-gated `serve`)  → **v0.7.0**
**Goal:** agent access over MCP as a thin wrapper on the query API.
- `rto-render` (or a dedicated module) behind `--features mcp`: expose query,
  path, explain as MCP tools over stdio. No new query logic — wrapper only.
- New deps (feature-gated only): `rmcp` (official Rust MCP SDK) or a minimal
  JSON-RPC/stdio impl; must not leak into the default build.
- CLI: `roteiro serve` (already stubbed under `#[cfg(feature = "mcp")]`).
- **DoD:** `serve` answers a real MCP `tools/call` for a query against the
  dogfood graph; default build unchanged (no MCP deps).

### Stage 8 — Inference layer (`inferred`)  → **v0.8.0**
**Goal:** fuzzy doc/PDF/image → suggestions with confidence.
- Separate, **optional** pipeline (never gates offline local rebuilds): ingest
  docs/PDFs/images, embed, emit `inferred` edges with confidence + `inferred_
  from` provenance ref. Runs as its own CI stage.
- Semantic duplication check joins the structural one from Stage 4.
- New deps (biggest risk): a **bundled, offline** embedding model. Options to
  evaluate — `candle` + a small static/int8 model, `model2vec`-style static
  embeddings, vs. binary-size/licence trade-offs. Decision required before
  starting (see Open Questions). PDF/image extraction crates each need a licence
  check.
- CLI: `roteiro infer` (or `sync --with-inference`); confidence surfaced in
  `--json` and renderers (clearly marked as suggestions).
- **DoD:** an inferred edge appears labelled with confidence; disabling the
  layer leaves derived+authored builds fully offline and unchanged.

### Stage 9 — Importers  → **v0.9.0**
**Goal:** migration path off the three incumbents.
- `roteiro import --from lat|graphify|codegraph`: lat.md → `authored`, Graphify
  doc/media nodes → `inferred` (drop its code-structure edges in favour of
  re-derivation), codegraph → bootstrap/validation oracle only.
- Each import emits a **migration report**: edges imported per provenance class,
  validation failures, promotion candidates.
- CLI: `roteiro import --from <src>` (stub already exists).
- **DoD:** importing a sample of each source produces a graph + report;
  re-derivation supersedes imported structural edges without duplication.

### Stage 10 — CI-canonical artifacts & v1.0 hardening  → **v1.0.0**
**Goal:** the merged graph is the source of truth; ship stable.
- CI on PR merge: extract → `check` → assemble → publish content-addressed
  artifact → render docs + vault. Hooks fetch the artifact (offline fallback:
  rebuild). Local runs are deterministic previews.
- Multi-language breadth expanded; performance targets met; `--json` schema
  frozen and versioned; full docs; `roteiro check` authored-and-checks ADR-0001
  itself.
- **DoD:** clean clone → `init` → hook fetches CI artifact → `check` green,
  offline; docs/vault reproducible byte-for-byte in CI.

---

## 6. Cross-cutting concerns

**Testing strategy**
- Unit tests per module (already the pattern).
- Integration tests over **fixture git repos** (tiny committed trees) for
  sync/cache/extraction — the only way to test git-native behaviour honestly.
- Snapshot tests (`insta`) for `--json`, rendered docs, migration reports.
- Property tests (`proptest`) for FactSet round-trips, cache idempotency, parser
  invariants.
- **Dogfood test:** CI runs `roteiro sync && roteiro check` on this repo from
  Stage 4 onward; a failure blocks merge.

**Coverage ratchet:** `cargo-llvm-cov` in CI, 85% per-file floor (ADR bar).
Ratchet up, never down; new files land with tests.

**Dependency & licence policy:** every new crate (esp. tree-sitter grammars and
inference/PDF crates) must pass `cargo deny` (licence MIT/Apache-compatible,
no duplicate/banned crates) and `cargo audit`. Prefer pure-Rust, offline-capable
crates (`gix` over libgit2). Keep MCP/inference deps behind features so the
default build stays lean and the MSRV surface stays small.

**MSRV discipline:** stay on 1.94 until a dependency forces a move; the
`rusqlite =0.39` pin exists for exactly this reason (documented in
`Cargo.toml`). Any MSRV bump is an ADR-worthy decision.

**Error handling:** libraries expose `thiserror` enums; the binary uses
`anyhow` + process exit codes. `check`/`sync` return structured results so the
CLI can render both human and `--json` forms.

**Performance targets (validate at Stage 10):** cold full extract of a
mid-size repo in seconds; incremental sync proportional to the diff; cache-hit
sync effectively instant; `--json` queries sub-100ms on the dogfood graph.

---

## 7. Milestones → releases

| Release | Stage | Headline capability |
|---|---|---|
| v0.1.0 ✅ | 1 | Typed transactional graph store + migrations |
| v0.2.0 ✅ | 2 | Content-addressed cache; `roteiro sync` (incremental) |
| v0.3.0 | 3 | Derived tree-sitter extraction (Rust first) |
| v0.4.0 | 4 | Authored ADR/blueprint layer; `roteiro check` gates CI |
| v0.5.0 | 5 | Query API + `--json`; `init` + git hooks |
| v0.6.0 | 6 | Real docs-site + Obsidian renderers (retire shell stopgap) |
| v0.7.0 | 7 | MCP `serve` (feature-gated) |
| v0.8.0 | 8 | Inference layer (`inferred` + confidence) |
| v0.9.0 | 9 | Importers (lat.md / Graphify / codegraph) + reports |
| v1.0.0 | 10 | CI-canonical artifacts, multi-language, frozen `--json`, stable |

---

## 8. Risk register

| Risk | Impact | Mitigation |
|---|---|---|
| Tree-sitter grammar licence incompatibility | Blocks a language | Verify licence before vendoring; `cargo deny` gate; drop/replace grammar if needed |
| Offline embedding model size/licence (Stage 8) | Binary bloat or licence conflict | Decide model early; keep inference feature-gated & optional; static/int8 models |
| Unmaintained YAML crate flagged by audit | `check` gate can't ship | Choose a maintained reader (`saphyr`/`yaml-rust2`) or hand-parse simple frontmatter |
| Non-deterministic extraction → cache churn | Cache/CI-diff noise | Sort all emitted facts; snapshot + idempotency tests from Stage 3 |
| MSRV drift from a new dep | CI `msrv` job breaks | Pin (as with rusqlite); gate new deps on 1.94; ADR any bump |
| Scope creep in `check`/dedup | Slips v0.4 | Structural checks first; semantic dedup deferred to Stage 8 |

---

## 9. Open questions (decide before the relevant stage)

1. ~~**Node identity scheme**~~ — **decided:** opaque natural keys with the
   convention `sym:<lang>:<path>#<name>` / `adr:<id>#<section>` / `file:<path>`.
   The store treats keys as opaque strings, so the scheme can evolve. (Stage 1)
2. ~~**On-disk FactSet codec**~~ — **decided: JSON** (debuggable; revisit a
   compact binary only if size/speed demands it). (Stage 2)
3. **First language breadth** — Rust-only for v0.3, or Rust+TS together? (Stage 3)
4. **YAML/Markdown parsing** — maintained crate vs. hand-parser for house-style
   frontmatter/sections. (Stage 4)
5. **Templating** — hand-rolled vs. `askama`/`minijinja` for the docs site. (Stage 6)
6. **MCP SDK** — `rmcp` vs. minimal hand-rolled JSON-RPC. (Stage 7)
7. **Embedding model** — which offline model, and the binary-size budget. (Stage 8)

---

*This is a living document. Each stage should land with any decisions above
resolved in its PR description, and — once `roteiro check` exists — this plan and
ADR-0001 are themselves checked by the tool.*

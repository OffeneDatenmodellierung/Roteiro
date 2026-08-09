# Roteiro — Build Plan

Status: Active · Owner: The Roteiro Project Team · Last-modified: 2026-08-08
Governing decision: [ADR-0001](adr/0001-build-roteiro-unified-codebase-knowledge-graph.md)

This plan takes Roteiro from the initial v0.0.1 scaffold to a dogfooded v1.0. It
is organised as sequenced **stages**, each ending in a shippable release cut by
release-plz. Every stage names its deliverables, the concrete Rust surface it
adds, new dependencies (with licence notes for the `cargo deny` gate), the CLI
it wires up, and an explicit **Definition of Done (DoD)**.

**Current position (2026-08-08):** Stages 1–8 are delivered (Stage 8 = offline
inference core + candle local-models), Stage 10's portable graph-artifact
format shipped, and **Stage 9's Graphify importer** is done. What each stage
*deferred* is now tracked honestly as first-class **Stages 11–14** in §5b — so
nothing hides in a footnote. Agreed order: **Stage 11** (lat.md + codegraph
importers) → **Stage 12** (inference content/PDF/image ingestion + semantic
dedup) → **Stage 13** (spec/blueprint authoring pillar) → **Stage 16**
(commit-time correctness gate) → **Stage 14** (v1.0 hardening). **A note on stage
numbers:** they are labels, not execution order — Stage 15 (intent-debt) shipped
early and independently, and Stage 16 (commit-time gate) is sequenced last before
the Stage 14 freeze because it touches sync, check, and hooks together. **A note
on version labels:** the per-stage `v0.x` headings are
*nominal targets*; because the workspace is pre-1.0, release-plz bumps `feat`
commits as patches, so real tags are `0.0.n` (Stage 1 → v0.0.2 … artifacts →
v0.0.10, Graphify import → next). §7 maps them.

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

### Stage 4 — Authored layer & `roteiro check` (rto-spec)  → **v0.4.0** ✅ *delivered*
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
- **Status: delivered.** `rto-spec` now parses ADRs (`parse_adr`): frontmatter →
  `AdrMeta`, `## ` sections → `adr_section` nodes with `contains` edges, and
  `[[path#Symbol]]`/`[[path]]` wiki-links resolved to graph keys. `scan_annotations`
  finds `@rto:<id>` on comment lines. `check::run` applies ADR nodes, validates
  wiki-links against the derived graph and `@rto:` targets against ADR state,
  weaving valid links in as `authored` edges and reporting `BrokenLink` /
  `UnknownAdr` / `InactiveAdr` drift. `roteiro check [--json]` self-syncs the
  derived graph and reads authored inputs from the HEAD tree; it exits non-zero
  on drift and is wired into CI as a dogfood gate. **No new dependencies** —
  frontmatter is hand-parsed (Q4 decided: no `serde_yaml`), and the scanner
  respects code spans/fences and comment lines to avoid false positives from
  documented examples. Robustness note: it handles the real ADR-0001's inline
  `#` frontmatter comments and its `` `[[path#Symbol]]` `` example.
  - **Scope/follow-ups:** `check` validates the *committed* HEAD tree (ideal for
    a CI gate); making it working-tree-aware pairs with `sync_worktree`.
    Blueprint parsing and the structural duplication check are deferred within
    Stage 4 / to Stage 8 as planned.

### Stage 5 — Query surface, `--json`, `init` & git hooks  → **v0.5.0** ✅ *delivered*
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
- **Status: query surface delivered.** `rto-graph::query` exposes `explain`
  (a node + its provenance-labelled incoming/outgoing edges) and `list_kind`,
  serialised under the versioned schema tag **`roteiro.query/v1`**. `roteiro
  query <key>` explains a node; `roteiro query --kind <k>` lists — both with
  `--json`. `check` and `query` share a `build_graph` helper that assembles the
  full derived + authored graph. **Fixed** a latent bug surfaced here: edges are
  now a *set* (migration 3: unique `(src, dst, kind, provenance)` + `ON CONFLICT
  DO NOTHING`), so re-applying the authored layer over an unchanged derived
  graph is idempotent instead of duplicating edges. Query JSON schema asserted
  in a test; ~93% region / ~96% line.
  - **`init` + hooks — delivered.** `roteiro init` builds the initial graph and
    installs *managed* `post-checkout`/`post-merge` hooks (marker-tagged, so
    re-runs refresh in place and a foreign hook is never clobbered) plus a
    managed `AGENTS.md` section pointing agents at `roteiro query`. Hooks are
    self-guarding (`command -v roteiro … || true`) so they never break git on a
    machine without the tool, and live under the common git dir (shared across
    worktrees). An end-to-end test drives the real binary: `init` installs
    working hooks, and a `git checkout` rebuilds the graph via the hook.
  - **`roteiro path <from> <to>` — delivered** (v0.0.8 follow-up). Shortest path
    between two nodes via deterministic BFS, following edges in either direction;
    each hop is provenance- and direction-labelled, under the same
    `roteiro.query/v1` schema. Exits non-zero when the nodes are unconnected, so
    it doubles as a reachability assertion. Exposed as a third MCP tool (`path`).
  - **Follow-ups:** a working-tree query mode; respecting `core.hooksPath`; hooks
    fetching a CI artifact (Stage 10) instead of always rebuilding.

### Stage 6 — Renderers replace the shell stopgap (rto-render)  → **v0.6.0** ✅ *delivered*
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
- **Status: delivered.** `rto-render` now renders with **`pulldown-cmark`**
  (MIT, MSRV-clean, deny-clean), retiring `md2html.awk` entirely — which also
  fixes the whole class of hand-rolled-parser bugs (backtick runs, tables,
  headings) for free. `render docs` produces the themed ADR pages + index +
  copied assets (byte-for-byte parity with the stopgap, verified: 1 h1, 8 h2,
  3 tables, no frontmatter/code-span leaks); `render obsidian` emits a linked
  vault (one note per node, edges as provenance-labelled `[[wikilinks]]` — 415
  notes on this repo). Page chrome is hand-rolled (no templating dep). CLI:
  `roteiro render <docs|obsidian> [--out DIR]`. `website/build.sh` now calls
  `roteiro render docs` and the `Website` CI job builds it with a Rust
  toolchain. Unit tests snapshot the HTML/markdown; an end-to-end test drives
  the binary. Coverage ~98% on `rto-render`.
  - **Deploy note:** the Cloudflare Pages build now needs the Rust toolchain
    (Pages provides it) since `build.sh` runs `cargo run … render docs`. If the
    Pages image can't build Rust, the fallback is to render in CI and serve the
    artifact (Stage 10). Blueprints / overview / per-subsystem AI-context pages
    are future render targets.

### Stage 7 — MCP server (feature-gated `serve`)  → **v0.7.0** ✅ *delivered*
**Goal:** agent access over MCP as a thin wrapper on the query API.
- `rto-render` (or a dedicated module) behind `--features mcp`: expose query,
  path, explain as MCP tools over stdio. No new query logic — wrapper only.
- New deps (feature-gated only): `rmcp` (official Rust MCP SDK) or a minimal
  JSON-RPC/stdio impl; must not leak into the default build.
- CLI: `roteiro serve` (already stubbed under `#[cfg(feature = "mcp")]`).
- **DoD:** `serve` answers a real MCP `tools/call` for a query against the
  dogfood graph; default build unchanged (no MCP deps).
- **Status: delivered (on `rmcp`).** `rto-render::mcp` (behind `--features mcp`)
  exposes `explain` and `list_kind` as MCP tools — thin wrappers over the query
  surface, no new query logic. **Q6 decided ([[docs/adr/0002-adopt-rmcp-for-networked-mcp-serving.md]]):**
  the first cut was a lean hand-rolled JSON-RPC/stdio server, but once
  **networked serving** became a near-term goal we adopted the official **`rmcp`
  SDK** — hand-rolling HTTP/SSE/sessions is the wrong bet. `roteiro serve` now
  serves over **stdio** (default, local agents) or **streamable-HTTP**
  (`--http <addr>`, networked/multi-client; TLS terminated at a reverse proxy).
  The `Store` is `!Sync`, so it's shared behind `Arc<Mutex<…>>`; the tokio
  runtime lives inside `rto-render` so `roteiro` stays runtime-free. Everything
  is strictly feature-gated: the **default build pulls none of rmcp/tokio/axum**
  (verified). rmcp's tree builds on the 1.94 MSRV and passes `cargo deny`.
  Dogfooded both transports (stdio session returns the graph JSON; HTTP `/mcp`
  answers `initialize` with 200). Tool methods unit-tested + `get_info`; an
  end-to-end test drives the real binary over a stdio MCP session.
  - **`path` tool — delivered** (v0.0.8 follow-up): the MCP surface now exposes
    `explain`, `list_kind`, **and `path`**.
  - **Follow-ups:** in-app TLS (rustls) as an alternative to proxy termination;
    richer capabilities (resources/prompts) if agents want them.

### Stage 8 — Inference layer (`inferred`)  → **v0.8.0** ✅ *delivered (offline default + local models)*
**Goal:** fuzzy doc/PDF/image → suggestions with confidence.
- Separate, **optional** pipeline (never gates offline local rebuilds): ingest
  docs/PDFs/images, embed, emit `inferred` edges with confidence + `inferred_
  from` provenance ref. Runs as its own CI stage.
- Semantic duplication check joins the structural one from Stage 4.
- **Model decision made — [[docs/adr/0003-pluggable-embedding-models.md]]:** a
  tiny static int8 (`model2vec`-style) embedding **compiled in** as the offline
  default (single-digit MB), plus **GGUF** pluggable local models via an
  in-binary registry (`roteiro model list|pull`), platform-aware variant
  selection (Metal/Apple vs standard), and consent-gated fetch. The `candle`
  backend and GGUF loading sit behind a second `inference-local-models` feature,
  so the default and `inference` builds pull none of them. PDF/image extraction
  crates each need a `cargo deny` licence check.
- CLI: `roteiro infer` (or `sync --with-inference`); confidence surfaced in
  `--json` and renderers (clearly marked as suggestions).
- **DoD:** an inferred edge appears labelled with confidence; disabling the
  layer leaves derived+authored builds fully offline and unchanged.
- **Status: core delivered (the offline default).** `rto-graph::infer` (feature
  `inference`, **zero new deps**) implements ADR-0003's lean tier: a pure-Rust
  hashing embedding (`embed`/`similarity`), and `infer_edges` which suggests
  `EdgeKind::Related` edges tagged `provenance = inferred` with `confidence =`
  cosine similarity and an `embedding:hash/v1` `src_ref`. `infer_edges` skips any
  pair already joined by an existing edge (either direction) and is
  deterministic. `roteiro infer [--min-confidence --top-k --json]` builds the
  derived+authored graph, **clears prior inferred edges** (so re-running with
  different flags is authoritative and never accumulates stale suggestions), then
  applies the new suggestions; they surface in `query`/`explain` with their
  confidence. Dogfooded on this repo (e.g. `run_sync`
  ↔ its sibling `run_*` handlers ≈ 0.7). ~97% coverage on `infer.rs`; both the
  default (no inference) and `--all-features` builds are clippy/deny/msrv clean,
  and the default build pulls none of it — **DoD met**.
  - **`inference-local-models` tier — delivered.** Behind that feature,
    `rto-graph::localmodel` adds a **candle**-backed BERT sentence embedder
    (`LocalEmbedder`, CPU, safe `from_buffered_safetensors` — no `unsafe`), an
    in-binary **model registry** with per-platform variants + host-aware
    selection (`Platform::host` → Metal/Apple vs standard), the model store
    (`~/.roteiro/models`), and SHA-256 verification. `roteiro model list` shows
    the registry; `roteiro model pull <name>` is **consent-gated** — it prints
    source/licence/size and asks `[y/N]` on a TTY, and in a **non-interactive**
    session refuses and prints the manual command (offline-by-default is never
    broken). `roteiro infer --model <name>` uses the pulled model, falling back
    to the hashing embedder if absent. candle enters the tree **only** under this
    feature; the default and `inference` builds pull none of it (verified). One
    scoped `cargo deny` exception was added: `RUSTSEC-2024-0436` (`paste`, an
    unmaintained build-time proc-macro reached via candle→gemm; no CVE).
    - **Verification note:** everything except *running a real downloaded model*
      is unit/integration-tested and gate-clean (registry, platform selection,
      checksum, consent-decline, feature errors; the candle embedder compiles
      against the real API via a de-risked reference). Live model inference is
      verified locally by the operator — CI has no network to fetch weights.
  - **Remaining → [Stage 12](#stage-12--inference-ingestion-content-pdf-image--semantic-dedup--v012x-completes-stage-8):**
    doc/PDF/image ingestion (embed real content, not just names) + semantic
    dedup. Also outstanding: real per-model SHA-256 pins + Apple MLX registry
    variants.

### Stage 9 — Importers  → **v0.9.0** 🚧 *Graphify delivered; lat.md / codegraph pending samples*
**Goal:** migration path off the three incumbents.
- `roteiro import --from lat|graphify|codegraph`: lat.md → `authored`, Graphify
  doc/media nodes → `inferred` (drop its code-structure edges in favour of
  re-derivation), codegraph → bootstrap/validation oracle only.
- Each import emits a **migration report**: edges imported per provenance class,
  validation failures, promotion candidates.
- CLI: `roteiro import --from <src>` (stub already exists).
- **DoD:** importing a sample of each source produces a graph + report;
  re-derivation supersedes imported structural edges without duplication.
- **Status: Graphify importer delivered.** `rto-spec::import_graphify` parses
  Graphify's `NetworkX` node-link `graph.json`, importing doc/concept/rationale/
  image nodes (keyed `graphify:<id>`) and its **semantic/`INFERRED`** links as
  `inferred` edges (stamped `src_ref = import:graphify`), while **dropping** the
  code/AST nodes and edges (Roteiro re-derives those, more precisely). Hyperedges
  become grouping nodes with `related` edges to imported members. `roteiro import
  --from graphify <dir|graph.json> [--json]` applies the facts, then **grounds**
  each imported doc to a real `file:<path>` node where present, and prints a
  migration report (imported / dropped-code / dropped-ast / dangling / hyperedge
  / docs-linked). The two `inferred` producers (this import + the embedding
  layer) **coexist**: each clears only its own `src_ref`, so `roteiro infer`
  never wipes imported edges (and vice versa) — added `Store::delete_edges_by_src_ref`
  and switched `infer`'s clear to be `src_ref`-scoped. Dogfooded on the real
  Thalweg export: 572 knowledge nodes + 346 inferred edges + 25 groups imported,
  2121 code nodes dropped, 17 docs linked to files. Unit tests (mapping rules,
  hyperedge membership, bad JSON) + an end-to-end CLI test.
  - **Remaining → [Stage 11](#stage-11--importers-latmd--codegraph--v011x-next):**
    lat.md → `authored` and codegraph → validation-oracle importers. Samples will
    be generated by running those tools against this repo.

### Stage 10 — CI-canonical artifacts  → **v0.10.x** 🚧 *artifact format delivered*
**Goal:** the merged graph is the source of truth; ship stable. *(The v1.0
hardening that once lived here is now tracked explicitly as [Stage 14](#stage-14--v10-hardening--v100).)*
- CI on PR merge: extract → `check` → assemble → publish content-addressed
  artifact → render docs + vault. Hooks fetch the artifact (offline fallback:
  rebuild). Local runs are deterministic previews.
- Multi-language breadth expanded; performance targets met; `--json` schema
  frozen and versioned; full docs; `roteiro check` authored-and-checks ADR-0001
  itself.
- **DoD:** clean clone → `init` → hook fetches CI artifact → `check` green,
  offline; docs/vault reproducible byte-for-byte in CI.
- **Status: artifact format delivered (part 1).** `GraphArtifact` (`rto-graph`)
  is a portable, **versioned** (`roteiro.graph/v1`) JSON snapshot of the whole
  graph plus its `HEAD` tree id. `Store::export_factset` dumps nodes/edges in a
  deterministic order, so the same graph always serialises byte-identically
  (verified). `roteiro export [--out FILE|-]` writes it; `roteiro load <FILE|->`
  rebuilds a store from it, **skipping extraction** and recording the tree id so
  a `sync` at the matching commit short-circuits — the clone fast-path. An
  unknown schema tag is rejected. Unit tests (round-trip / determinism /
  schema-reject) + an end-to-end test that exports from one repo and loads into a
  *different* one, asserting (via a direct store read) the loaded graph is the
  artifact's, not re-extracted. ~97% coverage on `artifact.rs`.
  - **Remaining → [Stage 14](#stage-14--v10-hardening--v100).**

---

## 5b. Remaining-work stages (the honest backlog to v1.0)

Several stages above shipped their *core* and deferred the rest. Rather than let
that deferred work hide inside per-stage footnotes, it is promoted here to
first-class, sequenced stages. Order reflects the agreed priority:
**complete Stage 9 → complete Stage 8 → the spec/blueprint authoring pillar →
Stage 10 overflow (v1.0 hardening).**

### Stage 11 — Importers: lat.md + codegraph  → **v0.11.x** ⛔ *next*
**Goal:** finish the migration path off the remaining two incumbents (completes
the [Stage 9](#stage-9--importers--v090--graphify-delivered-latmd--codegraph-pending-samples) deferral).
- `roteiro import --from lat` — lat.md authored markdown → **`authored`** nodes/
  edges (wiki-links → `authored` references, like ADRs); a migration report.
- `roteiro import --from codegraph` — codegraph is a bootstrap/**validation
  oracle** only: compare its symbols/edges against Roteiro's derived graph and
  report agreement/divergence; do **not** import its structural edges (Roteiro
  re-derives them).
- **Sample data:** download and run lat.md and codegraph **against this repo**
  and use the outputs as import fixtures/dogfood — no external samples needed.
- **Durable imports (fix carried from the Graphify importer):** today an import
  is a re-appliable layer — `sync`'s full rebuild drops imported facts on the
  next code-changing sync (same as `infer`, but with no auto-regeneration since
  the source is external). Make imports **durable**: persist the imported
  `FactSet` in the store (a new `imports` table) and re-apply it in `build_graph`
  after sync+authored, tolerating endpoints whose derived nodes vanished (e.g. a
  deleted file). This makes `import` a first-class, persistent layer.
- **DoD:** importing each produces a graph + report; lat.md content lands as
  `authored`; codegraph is oracle-only (no duplicate structural edges); imported
  facts **survive a subsequent code-changing sync**; an end-to-end CLI test per
  source.

### Stage 12 — Inference ingestion: content, PDF, image + semantic dedup  → **v0.12.x** *(completes Stage 8)*
**Goal:** make `inferred` edges meaningful by embedding **real content**, not
just node names, and extend ingestion to docs/PDFs/images.
- **Text-content ingestion (first, zero new deps):** during extraction, capture
  markdown bodies and Rust doc-comments/comments and feed them to the embedder,
  so inference relates docs ↔ code by *meaning* (today it embeds only names +
  file stem). This alone lifts inferred-edge quality and lights up the
  Graphify-imported doc nodes against real code.
- **PDF text:** a pure-Rust extractor (e.g. `pdf-extract`/`lopdf`, licence-gated)
  → `doc` nodes with text → embedded. Feature-gated.
- **Image scanning:** OCR/vision over images. **Needs its own decision (like
  candle):** pure-Rust OCR is weak; real OCR is C++ FFI (tesseract — against the
  pure-Rust stance) or a candle vision model (heavy, weights required). De-risk
  (MSRV + `deny`) and record an ADR before committing; keep behind its own
  feature. **This is the one genuinely uncertain item in the backlog.**
- **Semantic duplication check:** join the structural dedup from Stage 4 using
  the embedding similarity already built.
- **Dependency-aware invalidation (from the codegraph comparison):** once
  per-node embeddings/AI-context are *cached*, a changed symbol must invalidate
  the cached context of its **dependents** (callers, referencing docs) — the
  graph edges we already have make this codegraph-style "dirty propagation"
  natural. The derived graph itself needs no propagation (it re-resolves at
  assembly); this applies only to cached inferred context.
- **DoD:** a doc's *content* produces an inferred edge to semantically-related
  code; PDF/image ingestion each add labelled `inferred` facts behind their
  features; the default/`inference` builds stay unchanged.

### Stage 13 — Spec/Blueprint authoring pillar  → **v0.13.x** *(the "spec-store" bits from ADR-0001)*
**Goal:** the intent interview + house-style ADR/blueprint + **graph-grounded,
correct build/deploy plan** generation — the front door ADR-0001 always
envisioned (`roteiro spec`), sharpened by GitHub **spec-kit**'s phases
(constitution → specify → clarify → plan → tasks). Grounded in Roteiro's graph
so generated plans reference *real* symbols/ADRs/deps and are `check`-gated.
- **Records an ADR first (ADR-0004):** the tiered, Roteiro-grounded authoring
  design and the agent-vs-tool boundary.
- **Tiers (mirroring inference), all graph-grounded + house-style + check-gated:**
  - **Tier 0 (offline, no model):** deterministic **scaffolding** — a house-style
    skeleton + a structured interview checklist + a build-plan outline built from
    real graph facts. No prose generation, so planning works with nothing
    installed / on a plane.
  - **Tier 1 (light, default):** a **small local GGUF instruct model** (candle)
    drafts/expands sections offline on low-power hardware — extends the Stage 8
    `inference-local-models` registry/pull/consent machinery from embedding to
    *generative* models.
  - **Tier 2 (larger local):** a bigger pulled GGUF model for better quality.
  - **Tier 3 (foundation/agent):** the agent over MCP for best quality, or to
    **review** a light-mode draft later.
- CLI: `roteiro spec context <topic>` (graph-grounded context), `roteiro spec
  scaffold` (house-style ADR/blueprint skeleton + build-plan outline), plus a
  bundled spec-kit-style **skill** the agent drives.
- **DoD:** tier-0 produces a valid, `check`-passing house-style skeleton grounded
  in real graph facts with **no** model; tier-1 drafts prose from a small local
  model offline; both artifacts are `check`-gated.

### Stage 16 — Commit-time correctness gate  → **v0.x** *(execution order: after Stage 13, immediately before Stage 14)*
**Goal:** guarantee the knowledge base is not just *fresh* (what `sync` gives)
but *correct* (no authored-vs-code drift) **at the point of a commit** — and
checkable mid-work during a large change — instead of relying on a manual
`check` or only Roteiro's own CI. Today the managed hooks run `sync --committed`
only (freshness on checkout/merge); nothing runs `check`, and `check` validates
the committed `HEAD` tree, so as a pre-commit hook it would inspect the *parent*
commit, not the staged change. This stage closes that gap. Resolves the
follow-ups noted under Stages 2/4 ("making `check` working-tree-aware pairs with
`sync_worktree`"; "a working-tree query mode").
- **Working-tree-aware `check`:** a mode that validates the *staged/uncommitted*
  state about to be committed, built on the delivered `sync_worktree` overlay
  (hash working-copy bytes over the committed tree). The committed-`HEAD` form
  stays for the CI merge gate.
- **`pre-commit` hook (managed by `roteiro init`):** runs the worktree-aware
  `check` and blocks a commit that introduces drift (ADR `[[link]]` to a missing
  symbol, `@rto:` to an unknown/superseded ADR, malformed ADR). Guarded and
  skippable like the other managed hooks; git-native `--no-verify` is the escape
  hatch. Added to `MANAGED_HOOKS`.
- **`post-commit` freshness (optional):** a `post-commit` hook running `roteiro
  sync --committed` so a same-branch commit refreshes the graph — today only
  `post-checkout`/`post-merge` do.
- **Mid-work usage:** the same worktree-aware `check` is what an agent or human
  runs before finishing a large change (mirrors lat.md's `lat check`; the
  `AGENTS.md` snippet already points agents at `roteiro check`).
- **Why here:** it touches `sync` (worktree overlay), `check` (validation), and
  `init` (hooks) at once, so it lands as the last correctness guarantee before
  the Stage 14 freeze.
- **DoD:** worktree-aware `check` validates uncommitted state; a managed
  `pre-commit` hook blocks a drift-introducing commit and passes a clean one;
  `post-commit` refresh works; dogfooded on Roteiro; `--no-verify` documented.

### Stage 14 — v1.0 hardening  → **v1.0.0**
**Goal:** the merged graph is the canonical source; ship stable (completes the
[Stage 10](#stage-10--ci-canonical-artifacts--v010x-artifact-format-delivered) deferral).
- **CI artifact publish/fetch:** CI publishes the content-addressed graph
  artifact on merge; `post-checkout`/`post-merge` hooks fetch it (offline
  fallback: rebuild). Local runs stay deterministic previews.
- **Language breadth:** TypeScript/JavaScript and Python tree-sitter extractors
  (Rust shipped in Stage 3).
- **Deploy:** render docs in CI and publish the static output to Cloudflare
  (Direct Upload + `CLOUDFLARE_API_TOKEN`/`ACCOUNT_ID` secrets), removing the
  Rust-in-Pages build cost; or keep the Git-integration build.
- **Freeze & polish:** `--json` schema frozen and versioned; performance targets
  met; full docs; `roteiro check` authored-and-checks ADR-0001 itself.
- **Per-crate crates.io READMEs:** every published crate (`rto-graph`,
  `rto-spec`, `rto-render`, `roteiro`) ships a `README.md` wired via
  `readme = "README.md"` in its `Cargo.toml`, stating the crate's role in the
  workspace and linking back to <https://roteiro.dev> and the repo. So the
  crates.io landing page is not empty and points to the canonical docs.
- **Perf — subtree pruning (from the codegraph comparison):** `sync` walks the
  whole `HEAD` tree today; instead **diff the last-synced tree oid against HEAD
  and prune subtrees whose oid is unchanged** — the git-native, content-hash
  (not mtime) version of codegraph's "skip unchanged subtrees," for large-repo
  sync latency.
- **DoD:** clean clone → `init` → hook fetches the CI artifact → `check` green,
  offline; docs/vault reproducible byte-for-byte in CI; `--json` schema declared
  stable.

### Stage 15 — Intent-debt tracking (TODOs, stubs, deferred work)  → **delivered** *(independent; low-risk)*
**Goal:** deterministically detect, log, and track **intent debt** — the markers
in code and docs that signal *missed intent* or *intent left for the future* —
so end users and AI can find what's incomplete instead of it hiding in comments
and footnotes.
- **Detect during sync (derived provenance, no new deps):**
  - comment markers — `TODO`, `FIXME`, `HACK`, `XXX`, `BUG`;
  - not-implemented / stub — `todo!()`, `unimplemented!()`, `bail!("not
    implemented")`, "stub", "placeholder", "not yet implemented";
  - deferred / future intent — "for now", "later", "deferred", "follow-up",
    "TBD", and unchecked `- [ ]` items in docs/ADRs.
- **Schema (graph-native — on the one query surface):** a **`marker`** node per
  finding (key `marker:<path>#<line>`) with a **category** (`todo` | `fixme` |
  `hack` | `stub` | `deferred`), the text, and location; a `contains` edge from
  the enclosing file/symbol → marker. `derived` provenance (pure function of the
  source). Queryable via CLI `--json`, MCP, and renderers.
- **Interface:** `roteiro debt [--json] [--kind …]` lists/groups findings; a
  **summary line in `roteiro check`** (report, not a gate by default — optional
  threshold later). Ties to authored intent: a deferred item may `[[link]]` the
  ADR that owns it; a `@rto:` annotation can mark intentional debt.
- **DoD:** markers extracted deterministically; `roteiro debt` lists them with
  location + category; surfaces in `query`/`explain` + MCP; dogfooded on Roteiro
  itself (finds the lat/codegraph import stubs, the `spec` stub, and the
  "deferred/remaining" notes).
- **Delivered:** `crates/rto-graph/src/markers.rs` scans every blob during
  extraction (cached alongside the language facts) and emits `marker` nodes
  (`NodeKind::Marker`) with a `contains` edge from the innermost enclosing
  symbol (else the file), resolved by byte span. `rto_graph::debt` is a new
  query primitive under the versioned schema; `roteiro debt [--json] [--kind …]`
  groups by category, `roteiro check` prints a debt summary line, and MCP gains
  a `debt` tool. Markers are also reachable via the existing `query --kind
  marker` / `explain` surface and flow into the Obsidian vault as nodes. Tags
  match mixed case (`todo`/`fixme`/`tbd` anywhere; `BUG`/`HACK`/`XXX` uppercase
  anywhere or in annotation form `Bug:` / `hack(…)`).
- **Opt-out directives:** an inline `ignore` directive skips one line and an
  `ignore-file` directive (both prefixed `roteiro:`, spelled out in
  `markers.rs`) skips a whole blob — a git-tracked escape hatch for false
  positives. Applied to `markers.rs` itself, which only enumerates the detection
  vocabulary. (This page deliberately avoids the literal directive tokens so it
  is not self-silenced.)
- **Scoped out (precision):** the noisy bare words `later` and `stub` from the
  original phrase list are omitted — they flood documentation prose with false
  positives without comment-awareness. Per-language *comment vs code* scoping
  (so soft deferral phrases only fire inside comments) is a natural follow-up
  once more language extractors land in Stage 14; tracked here. The debt
  feature's own API docs still self-report a handful of markers (they name the
  categories); the ignore directives are the intended remedy where it matters.

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

**Performance targets (validate at Stage 14):** cold full extract of a
mid-size repo in seconds; incremental sync proportional to the diff; cache-hit
sync effectively instant; `--json` queries sub-100ms on the dogfood graph.

---

## 7. Milestones → releases

| Nominal | Stage | Headline capability | Actual tag / status |
|---|---|---|---|
| v0.1.0 | 1 | Typed transactional graph store + migrations | ✅ v0.0.2 |
| v0.2.0 | 2 | Content-addressed cache; `roteiro sync` (incremental) | ✅ v0.0.3 |
| v0.3.0 | 3 | Derived tree-sitter extraction (Rust) + dirty overlay | ✅ v0.0.4 |
| v0.4.0 | 4 | Authored ADR/blueprint layer; `roteiro check` gates CI | ✅ v0.0.5 |
| v0.5.0 | 5 | Query API + `--json`; `init` + git hooks | ✅ v0.0.6 |
| v0.6.0 | 6 | Real docs-site + Obsidian renderers (retire shell stopgap) | ✅ v0.0.7 |
| v0.7.0 | 7 | MCP `serve` (rmcp; stdio + HTTP, [ADR-0002](adr/0002-adopt-rmcp-for-networked-mcp-serving.md)) | ✅ v0.0.8 |
| — | 7+ | `roteiro path` + MCP `path` tool (follow-up) | ✅ v0.0.9 |
| v0.8.0 | 8 | Inference layer (`inferred` + confidence) | ✅ offline core + candle local-models (`roteiro infer`/`model`); **ingestion → Stage 12** |
| v0.9.0 | 9 | Importers (lat.md / Graphify / codegraph) + reports | 🚧 Graphify shipped; **lat.md/codegraph → Stage 11** |
| v0.10.x | 10 | CI-canonical artifacts | 🚧 artifact `export`/`load` shipped (v0.0.10); **CI publish/fetch etc. → Stage 14** |
| v0.11.x | 11 | Importers: lat.md + codegraph (completes 9) | ⛔ **next** — generate samples by running the tools on this repo |
| v0.12.x | 12 | Inference ingestion: content/PDF/image + semantic dedup (completes 8) | ⛔ content-first (0 deps); image OCR/vision needs a decision |
| v0.13.x | 13 | Spec/Blueprint authoring pillar (ADR-0004; tiered, graph-grounded) | ⛔ the "spec-store" front door from ADR-0001 |
| v0.x | 16 | Commit-time correctness gate: worktree-aware `check` + `pre-commit`/`post-commit` hooks | ⛔ runs just before Stage 14 (touches sync+check+init) |
| v1.0.0 | 14 | v1.0 hardening (completes 10): CI artifacts, TS/JS+Python, deploy, `--json` freeze | ⛔ ships v1.0 |
| v0.x | 15 | Intent-debt tracking: TODO/stub/deferred markers as `derived` facts + `roteiro debt` | ✅ `marker` nodes + `debt` query/CLI/MCP; `check` summary line |

---

## 8. Risk register

| Risk | Impact | Mitigation |
|---|---|---|
| Tree-sitter grammar licence incompatibility | Blocks a language | Verify licence before vendoring; `cargo deny` gate; drop/replace grammar if needed |
| Offline embedding model size/licence (Stage 8) | Binary bloat or licence conflict | **Resolved by [ADR-0003](adr/0003-pluggable-embedding-models.md):** tiny static int8 default compiled in; GGUF local models opt-in behind `inference-local-models`; consent-gated fetch |
| Unmaintained YAML crate flagged by audit | `check` gate can't ship | **Resolved:** hand-parsed frontmatter in `rto-spec` (no `serde_yaml`) |
| Non-deterministic extraction → cache churn | Cache/CI-diff noise | Sort all emitted facts; snapshot + idempotency tests from Stage 3 |
| MSRV drift from a new dep | CI `msrv` job breaks | Pin (as with rusqlite); gate new deps on 1.94; ADR any bump |
| Scope creep in `check`/dedup | Slips v0.4 | **Handled:** structural `check` shipped (v0.0.5); semantic dedup → Stage 12 |
| Image OCR/vision has no good pure-Rust path (Stage 12) | Blocks image ingestion or forces a C++ FFI / heavy model dep | De-risk (MSRV + `deny`) and ADR before committing; keep behind its own feature; ship text + PDF ingestion first (both low-risk) so image is isolated |
| Generative local model for authoring is bigger than the embedding default (Stage 13) | "Light mode" still needs a pulled model | Tier-0 (offline, no model) guarantees planning always works; light-tier reuses the Stage 8 candle/GGUF registry; foundation/agent tier for quality |

---

## 9. Open questions (decide before the relevant stage)

1. ~~**Node identity scheme**~~ — **decided:** opaque natural keys with the
   convention `sym:<lang>:<path>#<name>` / `adr:<id>#<section>` / `file:<path>`.
   The store treats keys as opaque strings, so the scheme can evolve. (Stage 1)
2. ~~**On-disk FactSet codec**~~ — **decided: JSON** (debuggable; revisit a
   compact binary only if size/speed demands it). (Stage 2)
3. ~~**First language breadth**~~ — **decided: Rust-only** for the first
   extractor; TS/JS and Python are tracked Stage 3 follow-ups. (Stage 3)
4. ~~**YAML/Markdown parsing**~~ — **decided: hand-parser** for house-style
   frontmatter/sections (no `serde_yaml`); `pulldown-cmark` is used only for
   rendering, not parsing intent. (Stage 4)
5. ~~**Templating**~~ — **decided: hand-rolled** page chrome for the docs site
   (no `askama`/`minijinja` — no templating dependency). (Stage 6)
6. ~~**MCP SDK**~~ — **decided: `rmcp`** (the official SDK), for stdio +
   networked HTTP serving — see [[docs/adr/0002-adopt-rmcp-for-networked-mcp-serving.md]]. (Stage 7)
7. ~~**Embedding model**~~ — **decided ([[docs/adr/0003-pluggable-embedding-models.md]]):**
   a tiny static int8 (`model2vec`-style) embedding compiled in as the offline
   default (single-digit MB budget), plus **GGUF** pluggable local models via an
   in-binary registry with platform-aware (Metal/Apple vs standard) variant
   selection and consent-gated fetch; the `candle` backend sits behind a second
   `inference-local-models` feature. (Stage 8)
8. **Image OCR/vision backend** — pure-Rust OCR (weak) vs. tesseract C++ FFI
   (breaks the pure-Rust stance) vs. a candle vision model (heavy, weights).
   Decide + ADR before building. (Stage 12)
9. **Authoring model driver** — confirmed **Roteiro-grounded with tiers**
   (offline scaffolding → small local GGUF instruct model → foundation/agent);
   the small-model choice + size budget is the open sub-decision. (Stage 13)

---

*This is a living document. Each stage should land with any decisions above
resolved in its PR description, and — once `roteiro check` exists — this plan and
ADR-0001 are themselves checked by the tool.*

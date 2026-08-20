---
Title: Multi-repo workspace serve — one instance, many project graphs, one model
Space: ARCH
Parent: ADRs

# ADR-specific metadata (unknown keys are ignored; used for indexing/search)
type: adr
adr-id: "0008"
status: Accepted                    # Draft | For Review | Accepted | Rejected | Superseded
architectural-significance: HIGH    # SOFT | LOW | MEDIUM | HIGH | VERY HIGH
domain: Developer Tooling
decision-makers: ["The Roteiro Project Team"]
superseded-by:
version: "1.3"
last-modified: 2026-08-20
confluence-url:
---

# ADR-0008: Multi-repo workspace serve — one instance, many project graphs, one model

| | |
|---|---|
| **State** | Accepted |
| **Architectural Significance** | HIGH |
| **Domain** | Developer Tooling |
| **Document version** | 1.3 |

## Reference

Extends the local model server of [[docs/adr/0006-local-model-serving.md]] and the
networked MCP surface of [[docs/adr/0002-adopt-rmcp-for-networked-mcp-serving.md]]
from **one repository per process** to **one process serving many repositories**.
Every graph command today resolves its store through
[[crates/roteiro/src/main.rs#open_graph]], which discovers a single repo from the
current directory via [[crates/rto-graph/src/git.rs#Repo]] and opens that repo's
[[crates/rto-graph/src/store.rs#Store]]; both `serve` entry points —
[[crates/roteiro/src/main.rs#serve_mcp]] and
[[crates/roteiro/src/main.rs#serve_models_endpoint]] — inherit that single-repo
binding. This ADR adds an **opt-in workspace mode** on top, configured through a
new `[workspace]` table in [[docs/adr/0007-configuration-file.md]]'s
[[crates/roteiro/src/config.rs#Config]]. It rests on the offline-first, one-store
principle of [[docs/adr/0001-build-roteiro-unified-codebase-knowledge-graph.md]] —
graphs stay **per-repo and isolated**; only the *serving process* is shared.

## Summary

Let a single long-running `roteiro serve` hold the model(s) in RAM **once** and
answer questions about **any** of a user's registered repositories, opening each
repo's `.git/roteiro/graph.db` on demand and routing every request to the right
project via an explicit selector. Single-repo serve (the current cwd-scoped
behaviour) stays the default; workspace mode is opt-in.

## Context

A knowledge graph is **per-repo by construction**: each store lives at
`<repo>/.git/roteiro/graph.db`, discovered by walking up to the nearest `.git`
(see [[crates/roteiro/src/main.rs#open_graph]]). That isolation is a feature —
identical relative paths (`src/main.rs`) across repos never collide, provenance
and `check` stay scoped to one project, and a graph travels with its checkout.

But `serve` binds to exactly one repo — the one it was launched in. A developer
with 10–20 repositories who wants a model to answer grounded questions about all
of them must run 10–20 servers. The costs are asymmetric:

- **The graph is cheap.** Each store is a small SQLite file, opened in
  milliseconds.
- **The model is expensive.** A generative GGUF is gigabytes resident in RAM.
  N single-repo servers = N copies of the same weights, plus N ports an agent
  must map to the right repo.

So the natural resource split is **one model, many graphs**: keep the expensive
thing loaded once, and treat the cheap per-repo stores as on-demand attachments.
This is what a user asking *"can I run one instance and probe the correct code?"*
is describing, and the current topology cannot express it. The per-model
concurrency work already landed (ADR-0006 follow-up) means a single loaded model
can safely serve concurrent requests, so the engine side is ready.

## Interview — clarify before writing

- [x] **What problem does this solve, and who has it?** A developer (or an agent
  acting for them) with many local repos who wants grounded Q&A across all of
  them without N model copies or N ports.
- [x] **Which existing ADRs does this relate to or supersede?** Extends
  [[docs/adr/0006-local-model-serving.md]] and
  [[docs/adr/0002-adopt-rmcp-for-networked-mcp-serving.md]]; configured via
  [[docs/adr/0007-configuration-file.md]]. Supersedes nothing — additive.
- [x] **Are the affected symbols the right scope?** Yes: the single-repo binding
  is entirely inside [[crates/roteiro/src/main.rs#open_graph]] and the two serve
  entry points; the graph tools and the store are already repo-agnostic given a
  path.
- [x] **What options were considered, and why this one?** See below — the
  alternatives (N servers, a proxy, a merged mega-store) each fail on RAM,
  routing, or isolation.
- [x] **What are the consequences, costs, and risks?** A new registry + selector
  surface, an on-demand store cache, and an ambiguity risk when no project is
  named — all bounded and behind an opt-in flag.

## Decision makers

- The Roteiro Project Team

## Recommended option

**Add a workspace mode to `serve`: one process, model loaded once, per-repo
graphs opened on demand, and an explicit `project` selector on every surface.**

1. **Workspace registry (user-level).** A `[workspace]` table in
   `~/.roteiro/config.toml` (machine-specific, so user-layer — not the per-repo
   `roteiro.toml`) lists repo roots, or `roteiro serve --workspace <root>`
   auto-discovers every git repo under a root. Project **names** derive from the
   repo directory (deduplicated), e.g. `roteiro`, `omnigent`.
2. **On-demand, cached store resolution.** A [[crates/rto-graph/src/workspace.rs#Workspace]]
   type resolves a project name to its store, opening
   `<repo>/.git/roteiro/graph.db` on first use and caching the open
   [[crates/rto-graph/src/store.rs#Store]] handle by name. The cache is bounded by
   the registry (a handful of repos), so no eviction is needed; a repo with no
   graph yet reports "run `roteiro sync`" rather than opening an empty store.
3. **One model, shared.** The llama.cpp engine loads the configured model(s)
   once; all projects share them. This is the entire point — the graphs attach to
   a single warm model.
4. **Explicit project routing.**
   - **MCP** ([[crates/roteiro/src/main.rs#GraphToolRegistry]],
     [[crates/rto-render/src/mcp.rs#serve_http]]): the graph tools
     (`search`/`explain`/`path`/`debt`/`list_kind`) gain an optional `project`
     argument, plus a new `list_projects` tool. A missing `project` errors with
     the available names (or uses a configured default) — never a silent guess.
   - **OpenAI `/v1`** ([[crates/roteiro/src/main.rs#serve_models_endpoint]],
     [[crates/rto-serve/src/server.rs#serve_blocking_with_tools]]): **two** ways to
     select a project. (a) The served model's graph tools carry the **same
     `project` argument** and a `list_projects` tool, so *"what does the auth
     module in beta do?"* resolves via `list_projects` → `search(project: "beta",
     …)` — uniform with MCP, no bespoke routing. (b) A **`/v1/{project}/…` path
     prefix** pre-binds a project: a client points its `base_url` at
     `…/v1/<project>` and every tool call is scoped to it without the model
     naming it (an explicit `project` still overrides, allowing a cross-project
     query). `GET /v1/projects` lets a client (e.g. an agent router) enumerate the
     hosted projects without a model round-trip. And `serve --models --mcp` **merges `/v1` and `/mcp` onto one port** (both are axum path prefixes), so a single process — one loaded model, one Workspace — serves both surfaces.
5. **Default unchanged.** With no `[workspace]`/`--workspace`, `serve` behaves
   exactly as today (cwd-scoped, no project selector). Loopback-only default is
   retained ([[crates/roteiro/src/config.rs#ServeConfig]]); a public bind must
   still front a proxy with authz — more relevant here, since one port now
   exposes several repos' graphs.

## Options considered + consequences

1. **Status quo — one `serve` per repo.** Simple and fully isolated, and stays
   the default for the single-repo case. But it does not scale: N repos ⇒ N model
   copies in RAM and N ports an agent must map correctly. Rejected as the *only*
   option, kept as the default one.
2. **A router/proxy in front of N single-repo servers.** Solves port sprawl but
   not the RAM cost — still N resident model copies. Adds a moving part.
   Rejected.
3. **Merge all repos into one mega-store.** A single graph spanning every project
   would collapse routing into ordinary queries, but it **breaks isolation**:
   identical relative paths collide, cross-project edges become possible,
   provenance and `check` lose their per-repo meaning. Isolation is a core
   property (ADR-0001); rejected.
4. **Chosen — one process, model loaded once, per-repo graphs opened on demand,
   explicit selector.** Pays a modest new surface (registry + selector + store
   cache) to get the one-model-many-graphs topology the cheap/expensive split
   calls for, while preserving per-repo isolation and the single-repo default.

## Consequences

- **New surface:** a `[workspace]` config table, a `--workspace` flag, and — on
  both the MCP and `/v1` tool surfaces — an optional `project` argument plus a
  `list_projects` tool. These are **always present but optional** (uniform across
  both surfaces — the MCP schema is macro-generated and can't hide them); a
  single-project server resolves the sole default for a bare call.
- **Refactor, not rewrite:** [[crates/roteiro/src/main.rs#open_graph]] splits into
  a cwd convenience wrapper over a `(repo_path) -> (Repo, Store, cache)` resolver;
  the resolver is reused by both serve modes. All other callers keep the cwd
  behaviour untouched.
- **Backwards compatible:** absent workspace config, every existing command and
  the default `serve` are unchanged; no migration.
- **Freshness is the hooks' job — and it is live.** Workspace serve opens
  whatever graph each repo already has (kept fresh by that repo's managed hooks).
  A project's own `roteiro sync` rewrites its `graph.db` **in place**, and the
  server's cached connection reads the latest *committed* state on the next
  query — so a project's updates appear on the next question with **no reload**.
  A `busy_timeout` on every store connection makes a read that lands during a
  concurrent sync-commit wait briefly rather than fail with `database is locked`.
  `serve --sync-on-access` opts into the opposite trade: (re)build a project's
  graph on first touch (a first-open hook on the workspace), so a stale or
  never-synced repo is prepared before it is served — slower first query, never
  a stale answer.
- **The registry reloads on `SIGHUP`.** The *set* of hosted projects is read at
  `serve` start; sending the running server `SIGHUP` re-scans the roots and swaps
  the registry in place — added repos become available, removed ones are dropped
  (their cached store evicted), and still-present ones keep their warm connection
  — with no restart and no dropped requests. (Windows has no `SIGHUP`; there the
  set is fixed at start.) A registered-but-unsynced repo is picked up once its
  first sync lands, since a missing graph is not cached.
- **Security:** one port now exposes multiple graphs — acceptable on the
  loopback default, but the "front a public bind with a proxy" guidance from
  ADR-0006 becomes load-bearing. No new network exposure by default.
- **Risk — ambiguous project.** A tool call or request without a project is
  ambiguous; mitigated by requiring the selector (or a configured default) and
  shipping `list_projects` so an agent can discover names first.

## Nested workspaces (v1.3)

A `[[workspaces]]` entry may name other named workspaces whose members fold into
it:

```toml
[[workspaces]]
name = "backend"
repos = ["~/git/api", "~/git/worker"]

[[workspaces]]
name  = "platform"
includes = ["backend", "frontend"]     # members of both, plus anything below
repos    = ["~/git/shared-infra"]
```

**Nesting adds no expressiveness. It adds non-duplication.** Everything it can
express is already expressible by listing the same paths under two names — a
repo may belong to any number of workspaces, and nothing checks otherwise. What
it buys is that the two lists cannot drift: a repo added to `backend` is in
`platform` on the next resolution, rather than on the next time somebody
remembers. That is the whole of the case for it, and it is the same argument
this project has applied a dozen times to two copies of one fact.

### Resolution flattens

`includes` is resolved transitively into the member set, and a composed
workspace **is a workspace with more members** — the same flat
`ResolvedWorkspace { name, roots, repos, linked }` every surface already
consumes. No surface learns a new concept, and none needs to change: `links`,
`serve`, the explorer and the vault see a longer list of repos and nothing else.

This is deliberately the cheap half. Whether any surface should *display* the
nesting — grouped sections in a rendered vault, a tree in the explorer — is **not
decided here and is not foreclosed.** Flattening at resolution discards nothing,
because the declaration remains in the config: a later hierarchy-aware surface
can re-derive the tree from `includes` without a format change. The config shape
is the commitment; the rendering is not.

### What is refused, and why

- **Cycles** — `a` includes `b` includes `a` — are a config error naming the
  path, never a silent flatten and never a hang. A cycle is a mistake in a file
  a person wrote, and the honest response is to say which one.
- **An unknown name** in `includes` is an error listing the workspaces that do
  exist, matching how `--workspace-name` already refuses (`no workspace named
  'nope' (known: one, two, gamma)`).
- **`[standalone]` cannot be included.** It is a single unnamed table, so there
  is no name to reference — and that removes an incoherence by construction
  rather than by rule: a `linked` workspace absorbing repos declared to have *no*
  cross-repo links would be asking for links among repos whose whole point is
  that they have none.

Diamonds need no special handling: `a` including `b` and `c` which both include
`d` yields `d` once, because member paths are already de-duplicated within a
workspace.

## Build-plan outline (grounded)

1. Extract a `(repo_path) -> (Repo, Store, cache)` resolver from
   [[crates/roteiro/src/main.rs#open_graph]] (today cwd-only via
   [[crates/rto-graph/src/git.rs#Repo]] + [[crates/rto-graph/src/store.rs#Store]]).
2. Add `[workspace]` to [[crates/roteiro/src/config.rs#Config]] and a
   `serve --workspace <root>` that auto-discovers repos; names from repo dirs.
3. Add an on-demand, LRU-cached store registry keyed by repo path.
4. Add a `project` argument to the graph tools and a `list_projects` tool on
   [[crates/roteiro/src/main.rs#GraphToolRegistry]] and
   [[crates/rto-render/src/mcp.rs#serve_http]].
5. Route the `/v1` endpoint by `/v1/<project>/…` in
   [[crates/roteiro/src/main.rs#serve_models_endpoint]] /
   [[crates/rto-serve/src/server.rs#serve_blocking_with_tools]].
6. Docs: the website "Ask your code" section and the agent `SKILL.md` — how to
   point one instance at many repos and select a project.

## Document version history

| Version | Date | Notes |
|---------|------|-------|
| 0.1 | 2026-08-11 | Draft for review. Proposes an opt-in workspace `serve`: one process, one resident model, per-repo graphs opened on demand behind a store cache, explicit `project` selection on the tools (+ `list_projects`). Keeps single-repo cwd serve as the default; rejects N-servers, a proxy, and a merged mega-store. Grounded in `open_graph`, the two serve entry points, and the `[workspace]` config layer. |
| 1.0 | 2026-08-11 | Accepted and implemented. Added `rto_graph::Workspace` (name→store registry, opened on demand, cached), a `[workspace]` config table (`roots`/`repos`) and `serve --workspace <root>` (shallow repo discovery). Both tool surfaces are workspace-backed: the MCP tools and the `/v1` graph tools gained an optional `project` argument and a `list_projects` tool, exposed only when several projects are hosted. Single-repo serve is unchanged (the cwd repo is the sole default project). **Realised `/v1` selection as a uniform `project` tool argument, not a `/v1/<project>/…` path prefix** — one mechanism across MCP and `/v1`, no bespoke routing. |
| 1.1 | 2026-08-11 | Added `busy_timeout` on every store connection (a workspace read that lands during a project's concurrent `sync`-commit waits briefly instead of failing with `database is locked`), and **live registry reload on `SIGHUP`** — a dedicated thread re-scans the workspace roots and swaps the registry in place (added/removed repos, no restart). Content freshness already needed no reload (in-place `graph.db` writes are read on the next query). |
| 1.2 | 2026-08-11 | Implemented the two optional extras this ADR left open. **`/v1/{project}/…` path routing** (`rto-serve`): a client uses `…/v1/<project>` as its base URL and tool calls are pre-bound to that project (a `ScopedTools` wrapper fills `project` when the model omits it); `models`/`embeddings` accept and ignore the prefix. **`serve --sync-on-access`**: a first-open hook on the `Workspace` (re)builds a project's graph on first touch, so a stale/never-synced repo is prepared before serving. Also added **`GET /v1/projects`** so a client-side router can enumerate hosted projects without a model round-trip. And `serve --models --mcp` **merges `/v1` and `/mcp` onto one port** (both are axum path prefixes), so a single process — one loaded model, one Workspace — serves both surfaces. |
| 1.3 | 2026-08-20 | Added **nested workspaces**: a `[[workspaces]]` entry may `includes` other named workspaces, whose members fold in transitively. Resolution **flattens** — a composed workspace is a flat `ResolvedWorkspace` like any other, so no surface learns a new concept and none changes. Records that nesting adds no expressiveness, only non-duplication (the lists cannot drift), and that whether a surface should *display* hierarchy is undecided and unforeclosed, since the declaration survives in config for a later tree-aware surface to re-derive. Cycles and unknown names are named errors; `[standalone]` is unnameable and therefore uncomposable, which removes the `linked`-across-a-boundary incoherence by construction. |

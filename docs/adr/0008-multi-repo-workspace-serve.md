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
version: "1.7"
last-modified: 2026-09-15
confluence-url:
---

# ADR-0008: Multi-repo workspace serve — one instance, many project graphs, one model

|  |  |
|---|---|
| **State** | Accepted |
| **Architectural Significance** | HIGH |
| **Domain** | Developer Tooling |
| **Document version** | 1.7 |

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

**Since v1.6, that sentence is true of the invocation and not only of the file.**
Until then "opt-in" meant opting in *once*, in
`~/.roteiro/config.toml`: a config that named any `[[workspaces]]` turned
workspace mode on for every `roteiro serve` and `roteiro explorer` thereafter,
including the ones run inside a single repository to ask about that repository.
`--scope` makes each mode requestable — `here` (the default), `all`,
`workspace <NAME>`, `bundle <PATH>` — and `[serve] scope` declares one for a
server that cannot be passed a flag. Workspace mode is now `--scope all`.

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
   repo directory (deduplicated), e.g. `roteiro`, `omnigent`. Since v1.7 a scan
   walks past **linked git worktrees** — a second checkout is not a discovery —
   unless the workspace sets `include_worktrees`; an explicit `repos` entry naming
   one is honoured regardless.
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
  concurrent sync-commit wait briefly rather than fail with `database is locked`
  — and since v1.5 the store runs in WAL, so such a read does not wait at all.
  Two *writers* need more than that timeout; see v1.5.
  `serve --sync-on-access` opts into the opposite trade: (re)build a project's
  graph on first touch (a first-open hook on the workspace), so a stale or
  never-synced repo is prepared before it is served — slower first query, never
  a stale answer.
- **`SIGHUP` is handled by every server, and reloads all of one.** Two rules,
  because they failed independently.

  *Handled.* `SIGHUP`'s default disposition **terminates the process**, so a
  server that does not handle it dies on the signal (exit 129). The handler is
  therefore installed in `main`, before dispatch, for every long-lived server —
  `serve`, `mcp` and `explorer` alike — rather than by whichever code path
  happens to build a reloadable workspace. What the signal *does* is registered
  separately by that path; a server whose hosted set cannot change (single-repo
  `serve`/`mcp`, `explorer`'s cwd fallback) registers *why*, and says so on the
  signal instead of exiting. (Windows has no `SIGHUP`; there the set is fixed at
  start.)

  *All of one.* A `serve`/`mcp` process holds **two** views — the
  [[crates/rto-graph/src/workspace.rs#WorkspaceSet]] behind `/v1/graph/*` and the
  served UI, and the flattened [[crates/rto-graph/src/workspace.rs#Workspace]]
  behind the model tools, `/v1/{project}/…` and the MCP router. A reload swaps
  both from **one** `resolved_workspaces()` and **one walk of the roots** — the
  flat view is planned from the very paths the set's own discovery found, not
  from a second scan, because two scans are two filesystem views and a repo
  created between them would land in one surface and not the other. Both discover
  first and swap second (a `plan_reload` that does all the git discovery, then an
  `apply_reload` that only takes a lock), so the two swaps are adjacent lock
  acquisitions with no I/O between them; a request reads one view or the other,
  never both, so no single response can straddle a reload.

  *What a reload sees.* The **roots are re-scanned** — added repos become
  available, removed ones are dropped (their cached store evicted), and
  still-present ones keep their warm connection, with no restart and no dropped
  requests. A registered-but-unsynced repo is picked up once its first sync
  lands, since a missing graph is not cached. The **configuration is not
  re-read**: `SIGHUP` re-scans what the running process was configured with, and
  a workspace or root added to the config file needs a restart. Re-reading would
  apply the handful of keys that can still take effect while silently ignoring
  the many consumed at startup (`[serve] addr`, the TLS pair, the MCP surface,
  `[models]` pins), and would let a file edit change a running server's remote
  grant, which ADR-0019 v1.2 decides once per invocation on purpose.
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
`ResolvedWorkspace { name, roots, repos, linked, include_worktrees }` every
surface already consumes. No surface learns a new concept, and none needs to change: `links`,
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
`d` yields `d` once. Not because workspaces de-duplicate their members — they do
not, and a repo a user lists twice in one entry still resolves twice — but
because the fold carries a dedupe of its own, in two parts: a workspace is
expanded at most once by name, and every root and repo an include contributes is
checked against a seen-set spanning the whole expansion. That set matches on the
*expanded* path, so `~/git/api` and the spelling it expands to are one member
rather than two, and it is **seeded from what the including workspace already
declares** — so an included member duplicating a declared one is dropped, while a
duplicate the user wrote by hand is left exactly as it was.

The distinction matters because the fold's dedupe is the only thing holding this
property. Read as a pre-existing invariant it looks redundant, and the next
person to tidy it away would reintroduce diamonds silently.

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
|---|---|---|
| 0.1 | 2026-08-11 | Draft for review. Proposes an opt-in workspace `serve`: one process, one resident model, per-repo graphs opened on demand behind a store cache, explicit `project` selection on the tools (+ `list_projects`). Keeps single-repo cwd serve as the default; rejects N-servers, a proxy, and a merged mega-store. Grounded in `open_graph`, the two serve entry points, and the `[workspace]` config layer. |
| 1.0 | 2026-08-11 | Accepted and implemented. Added `rto_graph::Workspace` (name→store registry, opened on demand, cached), a `[workspace]` config table (`roots`/`repos`) and `serve --workspace <root>` (shallow repo discovery). Both tool surfaces are workspace-backed: the MCP tools and the `/v1` graph tools gained an optional `project` argument and a `list_projects` tool, exposed only when several projects are hosted. Single-repo serve is unchanged (the cwd repo is the sole default project). **Realised `/v1` selection as a uniform `project` tool argument, not a `/v1/<project>/…` path prefix** — one mechanism across MCP and `/v1`, no bespoke routing. |
| 1.1 | 2026-08-11 | Added `busy_timeout` on every store connection (a workspace read that lands during a project's concurrent `sync`-commit waits briefly instead of failing with `database is locked`), and **live registry reload on `SIGHUP`** — a dedicated thread re-scans the workspace roots and swaps the registry in place (added/removed repos, no restart). Content freshness already needed no reload (in-place `graph.db` writes are read on the next query). |
| 1.2 | 2026-08-11 | Implemented the two optional extras this ADR left open. **`/v1/{project}/…` path routing** (`rto-serve`): a client uses `…/v1/<project>` as its base URL and tool calls are pre-bound to that project (a `ScopedTools` wrapper fills `project` when the model omits it); `models`/`embeddings` accept and ignore the prefix. **`serve --sync-on-access`**: a first-open hook on the `Workspace` (re)builds a project's graph on first touch, so a stale/never-synced repo is prepared before serving. Also added **`GET /v1/projects`** so a client-side router can enumerate hosted projects without a model round-trip. And `serve --models --mcp` **merges `/v1` and `/mcp` onto one port** (both are axum path prefixes), so a single process — one loaded model, one Workspace — serves both surfaces. |
| 1.3 | 2026-08-20 | Added **nested workspaces**: a `[[workspaces]]` entry may `includes` other named workspaces, whose members fold in transitively. Resolution **flattens** — a composed workspace is a flat `ResolvedWorkspace` like any other, so no surface learns a new concept and none changes. Records that nesting adds no expressiveness, only non-duplication (the lists cannot drift), and that whether a surface should *display* hierarchy is undecided and unforeclosed, since the declaration survives in config for a later tree-aware surface to re-derive. Cycles and unknown names are named errors; `[standalone]` is unnameable and therefore uncomposable, which removes the `linked`-across-a-boundary incoherence by construction. |
| 1.4 | 2026-09-01 | Split the `SIGHUP` guarantee in two, after both halves were found broken. **Installing** the handler moved to `main`, before dispatch, driven by an exhaustive `is_long_lived_server` match over the CLI: it had lived at the tail of `build_serve_workspaces`, which `explorer` never calls and which the single-repo fallback returns before reaching — so those two servers had no handler and the signal's fatal default killed them (exit 129). **Reloading** now covers the whole server: `WorkspaceSet` gained a reload (it had none), and a `SIGHUP` swaps it beside the flattened `Workspace` from one snapshot, so the server can no longer log a fresh project list and serve a stale one — both are planned from one walk of the roots and then swapped back to back. Records that a reload re-scans the roots but does **not** re-read config, and why. |
| 1.5 | 2026-09-08 | Amended (no issue; the finding was recorded on #579, which is closed **not planned**, and outlives it). **v1.1's `busy_timeout` does not do what that row claims for a second *writer*, and this corrects it.** The row says a concurrent access "waits briefly instead of failing with `database is locked`". True of a reader. Not true of a writer: every upsert in the store reads before it writes inside one transaction, and `Connection::transaction()` defaults to `DEFERRED`, so two such writers each hold a shared lock and then each ask to upgrade. SQLite refuses that **immediately** and deliberately does not invoke the busy handler, because waiting could only deadlock — so the five second timeout was never reached in the case it appears to cover, and one of the two writers died at once. Two Roteiro processes over one repository is ordinary rather than exotic: an editor's MCP client and a terminal, a long-lived `serve` and that repo's own `sync`, or several clients each spawning their own stdio server. Two settings now, in the one place a connection is configured, and **neither substitutes for the other** — which an injection shows rather than an argument. (a) Write transactions are `IMMEDIATE`, taking the lock when the transaction opens rather than upgrading into it, which turns the refusal into a queue the timeout can wait on. (b) The store runs in **WAL**, so a reader is no longer shut out for the whole of a writer's transaction — under the default rollback journal a `sync` blocked every query in a running `serve`, which is what v1.1's timeout was papering over. **WAL alone would not have fixed the reported failure**: with WAL on and transactions left `DEFERRED` the second writer still dies with `SQLITE_BUSY` / "database is locked", and the regression test says exactly that under injection — so the obvious half of this fix is the half that does not address the symptom. WAL is *tolerated rather than required*: it needs shared memory and is refused on most network filesystems, where `PRAGMA journal_mode` reports the mode actually in force. Such a store keeps precisely the behaviour it had before, so there is no new failure to report and the fallback is deliberately silent. |
| 1.6 | 2026-09-13 | Amended (issue #810). **`--scope` makes the served set a request, and restores this ADR's own default.** The Summary has said since v0.1 that "single-repo serve stays the default; workspace mode is opt-in", and the implementation had drifted off it in a way nobody had to decide: workspace mode was selected by the *presence of a config file*, so a `[[workspaces]]` entry written once turned it on for every later invocation, including one run inside a single repository to ask about that repository. The served set was decided by absences throughout — no config meant the cwd repo, no repository *and* no config meant a bundle — and two flags already meant different things about it (`--workspace <ROOT>` narrows the set; `-w` only names a default within it). `roteiro serve` and `roteiro explorer` now take `--scope here` \| `all` \| `workspace <NAME>` \| `bundle <PATH>`, **defaulting to `here`**, with `[serve] scope` as the config key a service manager needs — a unit file starts in `/` or `$HOME`, where `here` resolves to nothing, so without that key the new default would break precisely the deployments that cannot pass a flag. Three consequences are part of the decision rather than of the implementation. *`here` outside a repository is an **error** naming `--scope all`*, never a silent fall-back: falling back would put the served set back under the control of whether a `.git` directory happened to sit above the working directory, which is the implicitness being removed. *A one-line notice* when config defines workspaces and the scope resolved to `here` from the default — and only from the default, since telling somebody who typed `--scope here` about `--scope all` is how a notice becomes noise. *`--scope` is additive*: `--workspace <ROOT>` and `-w` keep their meanings and imply `all` when no scope is named, and a contradictory pair is a startup error rather than a silently chosen winner. **A reload is scoped too** — v1.4 established that a `SIGHUP` must reproduce the start rather than a different start, and re-reading the whole configured list would have widened a narrowed server on the next signal with nothing red to show for it. `--scope project` is **deferred and refused by name**: confinement here is per-*workspace* (`workspace_handles` → one tool registry per workspace → `/v1/workspaces/{ws}/…`) and there is no project-level equivalent to surface, so project scope is new machinery rather than a mode that already exists unnamed — and "unknown scope" would report a decision as a misspelling. `roteiro mcp` is deliberately **unchanged** and keeps `all`: its invocation is argv in a client's configuration file, where "the current directory" is whatever that client was started in, so inverting its default is a separate decision. The bundle scope's own surface is recorded in [[docs/adr/0022-dynamic-okf-viewer.md]] v1.4. Issue #824 — `serve -w NAME` with no workspace config could never start, for any NAME, because the single-repo fallback was guarded by `workspace_name.is_none()` — is fixed by the same refactor: the fallback is chosen by the scope, so `-w` is validated *against* it and either selects it or names what is wrong, as `explorer` has always done. |
| 1.7 | 2026-09-15 | Amended (issue #837). **A `roots` scan no longer hosts linked git worktrees, and says how many it walked past.** v1.0 defined root discovery as "every git repo under a root", and a worktree holds a `.git` entry, so a directory of worktrees resolved to one repository present N times, at N revisions, under N names, inside one workspace. That is not only noisy: coupling, hotspot and debt figures counted the same symbols once per checkout; cross-repo drift and link analysis treated two branches of one repo as two repositories; and a workspace-scoped retrieval could return one file at three revisions as three independent sources, which is a harder failure than a missing file because nothing marks the three as one. Three parts of the decision are recorded here rather than left to the implementation. *Detection is **structural***: `gix`'s repository classification (`Kind::WorkTree { linked_git_dir: Some(_) }`), equivalently `--git-common-dir` differing from `--git-dir`, never the directory's name — the `<repo>-wt-<task>` convention that made the issue visible is one machine's habit and a name test is wrong in both directions. A **submodule** is explicitly not a worktree and stays hosted, because it is a different repository rather than a second checkout of this one. *`roots` skips; `repos` is honoured*: a root is a discovery mechanism and discovering a second checkout is not a discovery, while naming one in `repos` is a deliberate operator act and is never *discovered* at all, so it is the escape hatch for the one-off case and needs no opt-in. *The skip is **announced***, in the same startup sentence that already reported subdirectories holding no `.git` — the behaviour being replaced was silent, and a silent skip would move the silence rather than remove it. The opt-in is `include_worktrees`, a scalar beside `roots`/`repos` in `[workspace]`, each `[[workspaces]]` entry and `[standalone]`, and it is a property of the **group**, which is what lets it compose with v1.6's `--scope` rather than compete with it: `--scope` chooses which groups are served and a chosen group carries its own discovery rule, so there is no interaction to reconcile. **This is a behaviour change to a shipped default, not a bug fix** — an unchanged config hosts fewer projects (on the installation that prompted the issue, 23 → 21). One consequence is accepted deliberately: a worktree under a scanned root whose **main** checkout lies outside every root is skipped too, and is therefore no longer graphed at all. A scan cannot distinguish "its main checkout is elsewhere in your config" from "its main checkout is on another disk", so making the answer depend on the rest of the configuration would make one directory's fate turn on an unrelated entry; the startup note makes the omission visible and either escape hatch restores it. **Amended the same day, and the amendment sharpens rather than reverses it: skipping applies to DISCOVERY, not to SELECTION.** `--scope here` while standing *inside* a worktree serves that worktree, scoped to it alone — being there is as explicit an act as naming it in `repos`, it composes with v1.6's `here` default so that `cd <worktree> && roteiro serve` simply works, and it is directly useful to agent workflows that create a worktree per task. The risk there is not that nothing is served but that the **wrong tree** is: a worktree's `HEAD` lives in `<main>/.git/worktrees/<name>/HEAD`, not in `<main>/.git/HEAD`, and both ways of getting it wrong — resolving through the common dir and serving the main repository's refs, or walking the worktree's files while reading `HEAD` blobs from the common dir and serving a mixture — produce a running server with a plausible project name. Which directory each path uses was therefore established by reading and is recorded here: the graph **store** is per-worktree (`open_graph_at` takes `repo.git_dir().join("roteiro")`), the object **cache** is shared (`repo.common_dir()`), and the committed tree comes from `Repo::walk_blobs` → `head_tree()` on the repository actually discovered. The start **announces** it, naming the repository the worktree belongs to and the revision it is at — its branch, or *a detached HEAD* when it has no branch, which `git worktree add --detach` and adding at a tag both produce and which is an ordinary state rather than an error — because presenting a worktree as though it were the repository is this issue's own misrepresentation approached from the other side, and naming a branch it does not have would be the same misrepresentation one level down. The open case the amendment left to the implementation is answered and is **no collision**: the two checkouts have separate stores (keyed by git dir) and share one object cache keyed on `(blob oid, path, extractor version, env)`, where a collision would require two different extraction results at one `(path, oid)` — impossible, because the oid is the content. Sharing is the benefit rather than the hazard, and is asserted as a cache *hit* beside the assertion that neither graph holds the other branch's files. `here` also does **not** pick up sibling repositories under the worktree's parent: `here` means this checkout, which the single-repo path upholds structurally by never consulting the resolved list. **Review of the implementation found the announcement requirement unmet on three further paths, each recorded here because each is the same defect the decision names — a skip nobody is told about.** (a) `[standalone] roots` are resolved to concrete `repos` before any `ResolvedWorkspace` exists, so their scan is unreachable from the resolved list and that whole table skipped worktrees in total silence; the `[standalone]` table is now carried to the diagnostics separately, and **only under `--scope all`**, the one scope under which the whole table is what is being served. (b) A root holding *only* worktrees — the layout an orchestrator that creates a checkout per task produces, and the layout the issue was reported from — resolves to nothing, so the start bails *before* the startup note is reached and the existing hint probes only subdirectories holding no `.git`, which a worktree is not; the empty-set error now names the count, the directories and both escape hatches, because a changed default that cannot explain itself at the moment it bites is the silence being removed. (c) `roteiro explorer` never emitted the scanned-roots note at all: it builds its set in `run_explorer` and never reaches the path that printed it, so **the surface that actually shows a person the repository list** was the one saying least — v1.6's depth diagnostic was invisible there too, and both are now printed. A fourth finding was a silently discarded opt-in rather than a silent skip: `WorkspaceConfig::is_empty` asks whether a table names any *members*, so a `[workspace]` declaring only `include_worktrees` read as absent and the group `--workspace <ROOT>` creates took the built-in `false`; the CLI-created group now inherits that table's rule, since an opt-in that reads as available while doing nothing is worse than none. **`roteiro mcp` is deliberately not given `--scope here`**, for the reason v1.6 records: the amendment is about that flag and `mcp` has none, so granting one would take the decision v1.6 explicitly left open. Measured rather than assumed, `mcp` is not silent either way — with no configured workspace it still reaches the single-repo branch and serves the worktree it stands in, and with configured roots it serves the configured set and reports in its startup note how many worktrees each root walked past. Finally, the announcement says "at its own revision" rather than "at this branch's revision": `git worktree add --detach`, or adding at a tag, produces an ordinary branchless worktree, and the sentence would otherwise have asserted a branch in exactly the case it had just reported as having none. |

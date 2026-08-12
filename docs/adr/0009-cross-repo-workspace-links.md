---
Title: Cross-repo workspace links — interlink a hub app with its deployment repos
Space: ARCH
Parent: ADRs

# ADR-specific metadata (unknown keys are ignored; used for indexing/search)
type: adr
adr-id: "0009"
status: Accepted                    # Draft | For Review | Accepted | Rejected | Superseded
architectural-significance: HIGH    # SOFT | LOW | MEDIUM | HIGH | VERY HIGH
domain: Developer Tooling
decision-makers: ["The Roteiro Project Team"]
superseded-by:
version: "1.5"
last-modified: 2026-08-12
confluence-url:
---

# ADR-0009: Cross-repo workspace links — interlink a hub app with its deployment repos

| | |
|---|---|
| **State** | Accepted |
| **Architectural Significance** | HIGH |
| **Domain** | Developer Tooling |
| **Document version** | 1.5 |

## Reference

Extends the multi-repo workspace of [[docs/adr/0008-multi-repo-workspace-serve.md]]
from *many isolated graphs served together* to *many graphs that can reference each
other*. It rests on the per-repo isolation of
[[docs/adr/0001-build-roteiro-unified-codebase-knowledge-graph.md]] — which
ADR-0008 deliberately preserved — and adds a **thin cross-repo link layer** on top,
resolved by [[crates/rto-graph/src/workspace.rs#Workspace]] (which already holds
every project open). Links are authored/verified with the same provenance and
drift machinery as within-repo intent: the `authored` layer and the
[[crates/rto-spec/src/check.rs#run]] gate of
[[docs/adr/0004-spec-blueprint-authoring-pillar.md]]. The traversal surface reuses
the served graph tools of [[docs/adr/0002-adopt-rmcp-for-networked-mcp-serving.md]]
and [[docs/adr/0006-local-model-serving.md]]. Cross-repo config links target the
config schema of [[docs/adr/0007-configuration-file.md]] — e.g.
[[crates/roteiro/src/config.rs#ServeConfig]] and
[[crates/roteiro/src/config.rs#WorkspaceConfig]].

## Summary

Let one repo's graph reference nodes in another — a **hub** source-of-truth app and
the **spoke** deployment/config repos that configure it. Cross-repo edges use
**project-qualified keys** (`app::sym:…#ServeConfig`), carry the usual
`derived | authored | inferred` provenance, are **resolved at the workspace** (a
join, never a merged store — isolation is preserved), and are **traversable**: from
a spoke you can follow a link straight into the hub's graph. A reference whose
target no longer resolves is **cross-repo drift** — [[crates/rto-spec/src/check.rs#run]]'s
authored-vs-reality gate, run *between* repos.

## Context

A common topology is **hub-and-spoke**: one source repo for an application, and many
deployment repos that are mostly *config implementations* — each pinning a version of
the app and setting/overriding its configuration. Today Roteiro cannot express the
relationship between them:

- Graphs are **per-repo by construction** — each store lives at
  `<repo>/.git/roteiro/graph.db`, discovered by
  [[crates/roteiro/src/main.rs#open_graph]], with repo-relative keys. ADR-0008 kept
  this isolation on purpose (identical relative paths across repos must not collide).
- The workspace server hosts many such graphs in one process, but they share **no
  edges**. You can query each project, not the relationships *across* them.

So the questions this topology actually raises have no answer: *which deployments
override `serve.addr`, and to what?*; *what does one deployment change versus the app
defaults?*; and — most valuably — *which deployments reference config the app no
longer defines?* The last is the cross-repo form of the drift Roteiro already catches
inside a single repo.

An interactive prototype validated the interaction model: a workspace overview (a
config override matrix + drift view over this repo's real config schema), the ability
to **drop into any repo's own detailed graph and back out**, and a **follow-the-link
hop** from a deployment's config key into the hub at the real
[[crates/roteiro/src/config.rs#ServeConfig]] struct. This ADR records the design that
prototype exercised.

## Interview — clarify before writing

- [x] **What problem does this solve, and who has it?** A team with one app repo and
  many deployment/config repos that need to stay consistent with it — they cannot
  currently see overrides or drift across the boundary.
- [x] **Which ADRs does this extend?** [[docs/adr/0008-multi-repo-workspace-serve.md]]
  (the workspace that hosts the graphs) and the isolation invariant of
  [[docs/adr/0001-build-roteiro-unified-codebase-knowledge-graph.md]]; reuses the
  `check` gate of [[docs/adr/0004-spec-blueprint-authoring-pillar.md]] and the config
  schema of [[docs/adr/0007-configuration-file.md]].
- [x] **How are links produced, and by whom?** Three provenances: `inferred`
  (matched automatically), `authored` (declared in the spoke), `derived` (extracted
  from deploy artifacts). See below.
- [x] **Where do the links live, and is isolation preserved?** In the spoke that owns
  them (they travel with that repo); resolved by a join at the workspace. No store is
  merged — isolation holds.
- [x] **What are the risks?** Version skew (a spoke pins an app version), the cost of
  false-positive `inferred` matches, and one server exposing several repos' graphs.

## Decision makers

- The Roteiro Project Team

## Recommended option

**A thin cross-repo link layer over the ADR-0008 workspace, isolation preserved.**

1. **Project-qualified keys.** A cross-repo endpoint is named `<project>::<key>`
   (e.g. `app::sym:rust:crates/…/config.rs#ServeConfig`). Within-repo keys stay bare,
   so nothing about a single repo's graph changes and identical relative paths never
   collide.
2. **Cross-repo link records.** A link is `(src_project, src_key) → (dst_project,
   dst_key)` with an edge `kind`, `provenance`, and (for `inferred`) a `confidence`.
   It is stored in the **spoke that owns it** — committed there, versioned there,
   travelling with that checkout — not in a global store.
3. **Three provenances, matching the model:**
   - **`inferred`** — a deployment's config keys matched to the app's schema by name
     / content similarity, confidence-scored. This is the automatic path for the
     "mostly config" case; no hand-authoring per key.
   - **`authored`** — declared explicitly in the spoke for links that must not
     silently drift (TLS paths, the served model): a `[links]` block in the spoke's
     `roteiro.toml` (ADR-0007) or a `// @rto:ext <project>::<key>` annotation
     (ADR-0004), verified by `check`.
   - **`derived`** — extracted from deployment artifacts where a hard reference
     exists (a Dockerfile `FROM app:1.2`, a Helm values file, a submodule pointer).
     A follow-on; not required for the first cut.
4. **Resolved at the workspace.** [[crates/rto-graph/src/workspace.rs#Workspace]]
   already opens every hosted project on demand; add a resolver that, given a
   qualified key, opens the target project and returns its node. The cross-repo layer
   is a **join over the isolated stores**, never a merge — the ADR-0001/0008 isolation
   invariant is untouched.
5. **Traversable — the follow hop.** Cross-repo edges are first-class in traversal:
   `roteiro explain app::…#ServeConfig` from a spoke resolves into the app's graph, and
   the served tools ([[crates/roteiro/src/main.rs#GraphToolRegistry]],
   [[crates/rto-render/src/mcp.rs#GraphServer]]) accept the far-end `project` — the
   same `project` selector ADR-0008 added, now on the *destination* of a link. This is
   the "follow the link across repos" interaction the prototype validated.
6. **Cross-repo drift is a `check` finding.** An `authored`/`inferred` link whose
   target does not resolve in the pinned app (a removed or renamed key) is a
   violation, surfaced by [[crates/rto-spec/src/check.rs#run]] — the exact
   authored-vs-reality gate of ADR-0004, extended across the repo boundary. A spoke's
   CI can fail when it references config the app has dropped.
7. **Version pinning.** A spoke pins the app at a sha/tag. **First cut:** resolve the
   far end against the app's current `HEAD` graph and record the pin (drift is
   "differs from HEAD"). **Full:** resolve against the app graph *at the pinned sha*
   via the content-addressed graph artifact / history — the harder, later refinement.

## Options considered + consequences

1. **Status quo — isolated per-repo graphs only.** Simple and the default, but cannot
   answer any cross-repo question (overrides, drift, follow). Kept as the behaviour
   when no links are declared.
2. **Merge all repos into one store.** Cross-repo edges become ordinary edges, but it
   **breaks isolation** — identical relative paths collide, provenance and `check`
   lose their per-repo meaning. Rejected for the same reason ADR-0008 rejected it.
3. **An external index outside the graph.** A separate service correlating repos.
   Rejected — it abandons the provenance model and the `check` gate that make the link
   trustworthy, and duplicates the workspace that already holds the graphs.
4. **Chosen — a thin link layer over the workspace.** Pays a modest new surface
   (qualified keys, link records, a resolver, a `check` extension) to get cross-repo
   overrides, drift, and traversal while preserving per-repo isolation and reusing the
   provenance/authoring machinery.

## Consequences

- **New surface:** project-qualified keys; a `[links]` config block and/or a
  `@rto:ext` annotation; a `CrossLink` record in `rto-graph`; a `roteiro links`
  command (list / verify); a cross-repo mode for `check`; and the far-end `project`
  on the traversal/serve tools.
- **Isolation preserved.** Each repo's graph stays pure and per-repo; the link layer
  is a join at the workspace, resolved on demand. No migration for existing repos;
  absent links, everything behaves exactly as today.
- **Freshness & version skew.** Drift detection is only as current as the app graph
  the workspace has for the hub; the pinned-sha resolution (7, full) is the load-
  bearing follow-on and the main open risk.
- **Security.** One workspace server already exposes several repos' graphs (ADR-0008);
  cross-repo traversal does not widen that beyond the loopback/proxy posture already
  documented there.
- **False positives.** `inferred` config matches need a confidence threshold and an
  easy path to promote a good match to `authored` (or suppress a bad one), or the
  drift signal gets noisy.

## Build-plan outline (grounded)

1. **Qualified keys + `CrossLink`** in `rto-graph`: parse `<project>::<key>`; a link
   record with kind/provenance/confidence.
2. **Authored links:** a `[links]` block in the spoke's config
   ([[docs/adr/0007-configuration-file.md]]) and/or a `@rto:ext` annotation
   (ADR-0004), extracted into the spoke's graph and verified by `check`.
3. **Inferred config matcher:** match a spoke's config keys to the app schema
   ([[crates/roteiro/src/config.rs#ServeConfig]] et al.) by name/content, scored.
4. **Workspace resolver:** a method on [[crates/rto-graph/src/workspace.rs#Workspace]]
   to resolve a qualified key across projects (the join).
5. **Traversal:** teach `explain`/`path` and the served tools
   ([[crates/roteiro/src/main.rs#GraphToolRegistry]],
   [[crates/rto-render/src/mcp.rs#GraphServer]]) to follow a qualified far end — the
   follow hop.
6. **Cross-repo drift in `check`** ([[crates/rto-spec/src/check.rs#run]]): an
   unresolved link target is a violation.
7. **Views:** the override matrix / drift / drill-in-and-out / follow-hop the
   prototype demonstrated, as a `render web-graph` output and in
   [[crates/roteiro/src/main.rs#serve_models_endpoint]].
8. **Version-pin resolution:** resolve the far end at the spoke's pinned app sha via
   the graph artifact / history (the full form of consequence 7).

## Document version history

| Version | Date | Notes |
|---------|------|-------|
| 0.1 | 2026-08-12 | Draft for review. Proposes a thin cross-repo link layer over the ADR-0008 workspace: project-qualified keys (`app::key`), `derived`/`authored`/`inferred` link records stored in the owning spoke, resolution as a join at `Workspace` (no merged store — isolation preserved), a traversable follow hop reusing the served tools' `project` selector, and cross-repo drift as a `check` finding. Rejects a merged mega-store (breaks isolation) and an external index (abandons provenance). Version-pin resolution against the pinned app sha noted as the load-bearing follow-on. Grounded in `Workspace`, `open_graph`, the config structs, the served tool registries, and `check::run`; motivated by a validated hub-and-spoke prototype. |
| 1.0 | 2026-08-12 | Accepted and first slice implemented. **Landed:** project-qualified keys (`parse_qualified`) and the resolver on [[crates/rto-graph/src/workspace.rs#Workspace]] (`resolve_qualified` — opens the target project on demand and returns `Ok(None)` for a drifted target); **authored** links via a `[[links]]` table in a spoke's `roteiro.toml`; and a `roteiro links` command that resolves every repo's declared links across the workspace, reports the target each resolves to, and **exits non-zero on drift** (the cross-repo `check` gate), with `--json`. Build-plan steps 1, 2, 4 done; step 6 delivered as a dedicated `links` command rather than folded into `check`. **Deferred, in order:** the **inferred** config-key matcher (step 3 — needs a config-key extractor so YAML/TOML keys become nodes); **derived** deploy-artifact extractors (Dockerfile/Helm/submodule); cross-repo **traversal in the served tools** (step 5 — the follow hop inside `serve`); the **web-graph/serve views** (step 7); and **version-pin resolution** at the spoke's pinned app sha (step 8, the load-bearing follow-on). |
| 1.1 | 2026-08-12 | **Inferred** config-key matcher landed (build-plan step 3, first form). `roteiro links --infer [--hub <project>]` reads each workspace repo's config files, flattens them to dotted keys, and matches every spoke's keys against the hub's by normalised name (bridging `SERVE_ADDR` / `serve.addr` / `serve-addr`), reporting confidence-scored correspondences with no hand-authored links and flagging **orphans** (a spoke key with no hub counterpart — the drift candidate). Informational (exit 0). Dependency-free: **TOML, JSON, and `.env`** (parsers already in the tree). **YAML** is intentionally left out for now (no maintained, `cargo deny`-clean parser is worth adding for the current, non-Kubernetes use cases — revisit if a Helm/k8s config target arrives). Also deferred: promoting matches into graph-native `inferred` cross-repo *edges* (this slice matches at command time, it does not yet persist config-key nodes/edges); and the derived/traversal/views/version-pin follow-ons from v1.0. |
| 1.2 | 2026-08-12 | **Serve-tool traversal landed** (build-plan step 5 — the live follow hop). The served graph tools on both surfaces — the `/v1` `GraphToolRegistry` ([[crates/roteiro/src/main.rs#GraphToolRegistry]]) and the MCP `GraphServer` ([[crates/rto-render/src/mcp.rs#GraphServer]]) — now accept a **project-qualified key** (`<project>::<key>`) in `explain` (and `path`): a qualified key opens the *target* project and resolves there via `Workspace::resolve_qualified`, overriding the call's `project` argument, so a served model looking at one repo can follow a cross-repo link straight into another's graph. `path` stays within a single graph (a qualified `from` selects the project; both endpoints are stripped to bare keys). Tool descriptions advertise the qualified form. Still deferred: graph-native `inferred` cross-repo *edges* (persisting config-key nodes + edges) and version-pin resolution. |
| 1.3 | 2026-08-12 | **Config-key nodes are graph-native** (part of build-plan step 3). Config files (TOML / JSON / `.env`) now extract into `config_key` graph nodes during `sync` — key `cfgkey:<file>#<dotted>`, value in `meta`, with a `contains` edge from the file — so config keys are first-class, queryable (`query --kind config_key`) and visible in the graph view. The flatten/normalise parsing moved to [[crates/rto-graph/src/config_keys.rs#flatten]] and is now shared by the extractor and `roteiro links --infer` (one parser, no drift). `EXTRACT_VERSION` bumped 5→6. Still deferred: persisting the `inferred` cross-repo *edges* between these nodes (the ext-ref/version-pin model) and derived deploy-artifact extractors. |
| 1.4 | 2026-08-12 | **Inferred cross-repo edges are persisted** (completes build-plan step 3). Two changes close the graph-native inferred path. (1) `roteiro links --infer` now reads each repo's config keys **from its graph** (the `config_key` nodes, via [[crates/rto-graph/src/store.rs#Store::config_keys]]) instead of re-parsing files, so the matcher and the stored nodes stay in lock-step (a repo must be synced; unsynced repos are noted, not fatal). (2) `roteiro links --infer --write` persists each spoke's matches as a durable `inferred` import layer under `LINKS_REF` (`import:links`): for every match, an **external-ref node** ([[crates/rto-graph/src/links.rs#external_ref_node]], key `extref:<project>::<key>`, kind `external_ref`) stands in — in the *spoke's* store — for the hub's `config_key` node, so the edge's endpoints both resolve locally and store integrity holds; a `references` edge (spoke `config_key` → external-ref) carries the confidence. The resolver walks the placeholder across repos to the real hub node via [[crates/rto-graph/src/workspace.rs#Workspace::follow_external_ref]] (→ `resolve_qualified`), and the layer is re-applied after every sync with dangling edges pruned when a config key is removed (the same durability path lat.md / Graphify imports use). Still deferred: **derived** deploy-artifact extractors (Dockerfile/Helm/submodule), the web-graph/serve **views**, and **version-pin resolution** at the spoke's pinned app sha (step 8, the load-bearing follow-on). |
| 1.5 | 2026-08-12 | **Views landed** (build-plan step 7) — the config **override matrix + drift** the prototype validated, as `roteiro links --matrix`. It reuses the `--infer` workspace scan, then pivots the per-spoke matches into a hub-key × spoke grid ([[crates/roteiro/src/overview.rs#build]]): each cell is a spoke's overriding value, flagged **differs** when it isn't a redundant restatement of the hub default — the signal a reader scans for — with orphan spoke keys collected into a **drift** section. Rendered three ways: a text table, `--json`, and a **self-contained HTML page** (`--html [--out FILE]`, the "render web-graph" output — inline theme-aware CSS, no external assets, everything escaped). Delivered under `links` rather than a `render` target (which is single-repo) since the matrix needs the whole workspace, mirroring how step 6 became the `links` command. Values come from the graph's `config_key` nodes, so secret redaction (v1.3) carries through — a `.env` `DB_PASSWORD` shows as `<redacted>` in the view. Still deferred: **derived** deploy-artifact extractors (Dockerfile/Helm/submodule — likely unneeded for the current non-k8s, sibling-repo workspaces) and **version-pin resolution** (step 8). |

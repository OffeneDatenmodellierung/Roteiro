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
version: "1.18"
last-modified: 2026-08-26
confluence-url:
---

# ADR-0009: Cross-repo workspace links — interlink a hub app with its deployment repos

| | |
|---|---|
| **State** | Accepted |
| **Architectural Significance** | HIGH |
| **Domain** | Developer Tooling |
| **Document version** | 1.18 |

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
the app and setting/overriding its configuration. It is not the only one, and not
even the common one at depth: a **snowflake** puts one or more levels in between —
`infra1,infra2 → chart → app`, where the chart repo is a spoke of the application and
the hub of the infrastructure repos that consume it. A project's role is therefore a
property of an *edge*, not of the project: the same repo is a hub in one relationship
and a spoke in another, and any model that labels a project with one of the two has
to pick wrongly for the middle of a chain (see v1.15). Today Roteiro cannot express
the relationship between them:

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
| 1.6 | 2026-08-12 | **Derived deploy-artifact extraction, part 1** (build-plan step 3/derived) — the spokes are in fact **Kubernetes** repos, so the derived extractors reversed from "likely unneeded" to load-bearing, and **YAML entered scope** (parser `yaml-rust2`, `cargo deny`-clean). Two per-blob extractors added to [[crates/rto-graph/src/config_keys.rs#flatten]] / [[crates/rto-graph/src/extract.rs#extract_facts]]: (1) **YAML config keys** — a **k8s manifest** (a doc with `apiVersion` + `kind`) is not flattened wholesale (that buries real config under `metadata`/`jobs` noise) but *mined* for what a deployment overrides — ConfigMap/Secret `data`, and each container's `image` and literal `env` vars (Secret values + secret-looking keys redacted); any other YAML (Helm `values.yaml`, kustomize, compose) is flattened like TOML/JSON. `.github/` is excluded (CI workflows are YAML but not app config). Because these become ordinary `config_key` nodes, the **inferred matcher and the v1.5 override matrix pick up k8s spokes for free**. (2) **Dockerfile `FROM`** → an `image_ref` node (`imageref:<file>#<n>`, `meta {image, tag, digest}`) with a `references` edge — the base-image **version pin** a spoke ships; internal multi-stage `FROM`s and `scratch` are skipped. `EXTRACT_VERSION` 6→7. Deferred to the next slice (**1.7**): **git submodule** pins, which need a tree-level gitlink pass through the sync engine (not per-blob); and **version-pin resolution** (step 8) that consumes the `image_ref`/submodule pins. |
| 1.7 | 2026-08-12 | **Derived deploy-artifact extraction, part 2 — git submodule pins**, completing the derived extractors. A submodule is a **tree-level** fact, not a blob: [[crates/rto-graph/src/git.rs#Repo::submodules]] walks the `HEAD` tree for gitlink (commit-mode) entries — each giving a path and the **commit sha it pins** (the version a deployment vendors) — and enriches them with the URL parsed from `.gitmodules`. Because it isn't per-blob, it can't ride the content-addressed cache; instead [[crates/rto-graph/src/sync.rs#append_submodule_nodes]] emits a `submodule` node (`submodule:<path>`, `meta {path, url, sha}`) into the assembled fact set on **every** sync, at all four assembly sites (full, incremental, worktree, index). It replaces any existing submodule nodes first, so the incremental path stays byte-identical to a full sync — an unchanged pin re-adds the same, a bumped pin's new sha wins, a removed submodule (its `.gitmodules` gone) leaves none. Nodes carry `path = .gitmodules` and stand alone (no edge — nothing in the graph is a guaranteed endpoint). Queryable via `query --kind submodule`. **All three derived artifact kinds now land** (Dockerfile image, k8s/Helm config, submodule pin). Still deferred: **version-pin resolution** (step 8) — resolving a spoke's cross-repo links against the hub graph *at the pinned version* (image tag / submodule sha), the load-bearing follow-on that consumes these pins. |
| 1.8 | 2026-08-12 | **Version-pin resolution — step 8, part 1 (the load-bearing follow-on).** Cross-repo drift was always measured against the hub's `HEAD`, so a spoke deploying an *older* hub saw false drift (a since-renamed key) and missed real drift (a key the deployed version dropped). Now it can resolve against the version actually shipped. Validated with a throwaway spike first, then built on one insight: extraction is a pure function of `(path, blob, bytes)` and `sync` already reads a *tree*, so the hub graph at any commit can be materialised **in memory, with no checkout** — and because the object cache is keyed by `(path, oid, env)`, every blob shared with `HEAD` is a cache hit, so resolving an older version only re-does what differs. New primitives: [[crates/rto-graph/src/git.rs#Repo::blobs_at]] (walk any commit/tree; a commit peels to its tree, so a submodule sha works directly) and [[crates/rto-graph/src/sync.rs#sync_tree]] (extract a rev into an ephemeral store). `roteiro links --infer` / `--matrix` gain `--hub-rev <rev>`: the hub's keys come from that pinned version instead of `HEAD`, so a spoke's `SERVE_TOOLS` that is drift against `HEAD` resolves cleanly against the sha it deploys, and every output names the pinned version. Deferred: **8b** — auto-resolving each spoke against the version *it* pins (reading its `submodule`/`image_ref` node), and image-tag/Helm-chart → git-ref mapping (a submodule sha resolves directly; an image tag / `@sha256` digest needs a convention); **8c** — fetching a hub-published graph artifact instead of re-extracting. |
| 1.9 | 2026-08-12 | **Version-pin resolution — step 8b: auto-resolve each spoke against the version *it* pins.** Where `--hub-rev` (v1.8) applied one version to the whole workspace, `roteiro links --infer --pinned` now reads each spoke's own derived pin and resolves that spoke against it — so a workspace of deployments each on a different hub version each gets measured correctly. Detection lives in [[crates/roteiro/src/pins.rs#detect]]: a spoke's `submodule` node whose URL matches the hub (origin, or repo basename → hub name) yields the pinned **sha** directly (unambiguous, so it wins); failing that, an `image_ref` whose image basename matches the hub has its **tag** tried as a hub git ref (`<tag>`, then `v<tag>`) — the release-tag == image-tag convention, no config. Each distinct rev is extracted once and cached across spokes (via [[crates/rto-graph/src/sync.rs#sync_tree]]); a spoke with no detectable pin falls back to the hub's `HEAD`. Per-spoke `hub_rev` / `pin_via` land in the JSON and the text report (`deploy … @ 4e0d5a6afd (via submodule app)`). `--pinned` is `--infer`-only (a per-spoke-heterogeneous matrix has no single hub column) and conflicts with the global `--hub-rev`. Still deferred: **8c** — fetching a hub-published graph artifact instead of re-extracting; and a `roteiro.toml [pins]` convention for image/Helm→ref mappings that don't follow the default tag guess. **This closes the ADR-0009 build plan** (steps 1–8, minus the optional 8c artifact fetch). |
| 1.10 | 2026-08-12 | **Step 8c — the last two refinements, closing ADR-0009 entirely.** (1) **`[pins]` config:** a spoke's `roteiro.toml` can declare `[pins] <name> = "<ref-template>"` (a `{tag}` placeholder), tried by [[crates/roteiro/src/pins.rs#detect]] *before* the default `<tag>`/`v<tag>` guesses — so a project whose git tags don't match its image tags (e.g. image `app:1.2` → git tag `release-1.2`) still auto-resolves. Merged per key (project over user), like the rest of the config. (2) **Published-artifact fetch:** [[crates/roteiro/src/main.rs#config_keys_from_artifact]] looks for a pre-exported graph artifact at `<hub>/.git/roteiro/artifacts/<treeid>.json` (via the new [[crates/rto-graph/src/git.rs#Repo::tree_id_at]]) and, if its recorded tree matches, loads it instead of re-extracting — so a pinned version resolves **even when its commit's blobs aren't present locally** (a shallow clone), and a hub whose CI `roteiro export`s per release skips extraction entirely. Falls back to `sync_tree` when no usable artifact exists. **ADR-0009 is now complete** (build-plan steps 1–8c). |
| 1.11 | 2026-08-13 | **Config keys derived from a typed Rust config struct** — closes the real gap where a hub app defines its config in Rust (a root `Config` with a nested `zerobus: ZerobusConfig` field) while its k8s infra sets those keys: previously only *file*-based config produced `config_key` nodes, so struct-defined keys had no hub counterpart and always read as drift. A struct authored with an explicit **`// @rto:config`** marker (opt-in — a struct is never *guessed* to be config) is treated as a config **root**: [[crates/rto-graph/src/extract.rs#RustExtractor]] (`RustWalk::synthesize_config_keys`) walks its declared fields recursively — descending into nested struct-typed fields, resolved **by name within the same file** — and emits a `config_key` node per **leaf** (`cfgkey:<file.rs>#<dotted>`, e.g. `zerobus.server_endpoint`), tagged `meta.source = "struct"` / `meta.struct = <root>` so struct-derived keys stay distinguishable from file-derived ones while sharing the `config_key` kind — so they flow through [[crates/rto-graph/src/store.rs#Store::config_keys]] → `links --infer`/`--matrix`/the explorer **for free**, and an infra `zerobus.serverEndpoint` (camelCase) links to the struct key via the existing canonicalizing matcher (#262). Extraction additionally records `meta.field_types` (each field → its core type, transparent wrappers like `Option`/`Box` peeled) and `meta.config_root` on struct nodes — the field→type signal a future **cross-file** synthesizer needs. `EXTRACT_VERSION` 8→9 (re-sync regenerates cached facts). Field names are used verbatim as dotted segments (the matcher already bridges snake/camel/kebab, so serde `rename_all` conventions match without being parsed). **Deferred (a clean, documented follow-on):** cross-file/cross-module nested-struct recursion (a config type split across files leaves those deeper leaves unsynthesized — no regression, they orphan exactly as today) and honouring an explicit `#[serde(rename = "…")]` to an unrelated spelling. |
| 1.12 | 2026-08-22 | **`--pinned` is no longer `--infer`-only — the matrix takes it too** (issue #504), reversing the restriction v1.9 recorded. The reason v1.9 gave was sound and is worth restating rather than deleting: *a per-spoke-heterogeneous matrix has no single hub column*. It is right that there is no single **baseline** — each spoke's cells are measured against the hub at that spoke's own rev — and computing `differs` from a shared HEAD column would report a value identical at the deployed version as an override (false drift, the exact thing pinning removes) and a key HEAD has since dropped as an override of nothing. What v1.9 did not separate is the *column* from the *baseline*. The column can stay one column, showing the hub at `HEAD` and **labelled** as such, while each cell carries the value it was actually measured against ([[crates/roteiro/src/overview.rs#MatchInput]]`::hub_value` → [[crates/roteiro/src/overview.rs#Cell]]`::baseline`, rendered as `[vs … at its rev]` in text and a `vs` line in HTML). `differs` is then computed per cell against the revision that spoke deploys, which is the question the matrix is being asked. The view also reports **whether pinning was asked for** (`pinned`) separately from **what was found** (`pins`), so an inert `--pinned` on a workspace with no detectable pins is visibly `0 of N pinned one` rather than byte-identical to a plain run (the rule #505 established for the infer report). Motivation is that the matrix is the *side-by-side* view: spokes deploying different hub versions are precisely the case a single shared rev misreports, so this is where per-spoke resolution is worth most. `--pinned` still conflicts with `--hub-rev` (one version for every spoke and each spoke's own version are opposite requests), and is still refused on the plain authored-link report, which resolves no hub version at all. Unchanged: the served `/links/matrix` endpoint resolves no pins — it reads persisted `external_ref` edges, and per-spoke resolution is a git walk it does not do while serving a request — so it reports `pinned: false`. |
| 1.13 | 2026-08-22 | **Authored `[[links]]` are persisted — the `authored → gold` path is reachable** (issue #573). The ADR has documented that path since v1.0 and the API has described it in three places, but nothing ever wrote an `authored` cross-repo edge: `roteiro links` resolved each declaration, printed it and exited, and the only producer of `external_ref` edges was `--infer --write`, which stamps `Inferred`. So `Provenance::Authored` was unreachable on this surface and [[crates/roteiro/src/graph_api.rs#project_links]]'s match arm was live code waiting for a value nothing could produce. `roteiro links --write` now persists each resolved declaration as an import layer: an [[crates/rto-graph/src/links.rs#external_ref_node_with]] placeholder plus an `authored` `references` edge carrying **no confidence** (a declaration is not scored). Three decisions the issue left open, settled: (1) **persistence is opt-in**, mirroring `--infer --write` — `roteiro links` is a CI gate that exits non-zero on drift, and a gate that mutates as a side effect is a surprise, the more so because it writes into *other repositories'* stores across the workspace; (2) **replacement is per-ref**, which `apply_import_layer` already gives — but under a **new** ref constant, `LINKS_AUTHORED_REF` (`import:links/authored`), because that call is authoritative for its ref and a shared one would make `--infer --write` delete every authored edge and the authored write delete every inferred one, each command silently reclassifying the other on every run; (3) **authored edges survive `sync`**, automatically and with no new machinery, because `reapply_imports` re-validates every persisted layer after each rebuild — a property v1.0 never wrote down. Two consequences recorded rather than left to be discovered: a declaration with no `from` anchor cannot become an edge (an edge needs a source node in that store) and is **reported as unanchored** rather than silently dropped; and the placeholder node is **shared** between the two layers, so its own provenance is whichever layer was applied last and must not be read as the link's — the edges carry that distinction. The workspace vault stops captioning every cross-repo row as a candidate and counts declared against inferred instead. |
| 1.14 | 2026-08-22 | **The topology view is a directed graph, not a star** (issue #572). The loop that builds it skipped the hub — `if Some(name) == hub { continue; }` — so links *out of* the hub were never read. A deployment chain `infra1,infra2 → chart → app` could not be drawn whole: whichever repo won the hub had its outgoing edges silently dropped, and no parameter could work around it because the hub is computed, not selected. The edges were in the store and absent from the payload. The hub is now an ordinary participant: it contributes its persisted `external_ref` edges like any other project and carries its own `keyCount`/`driftCount`. It is deliberately **not** matched against itself — live inference of a project against its own keys would report every key as its own correspondence — so it gets its persisted refs and no inference. Two consequences worth recording. (1) The response array is renamed `spokes` → **`projects`**, each entry gaining `role: "hub" \| "spoke"`: the array now contains the hub, and a key called `spokes` whose first element is the hub is a name that survives one reader and misleads the next; `role` is stated rather than left for a client to re-derive by comparing against `hub`, because a consumer that has to re-derive it will eventually derive it wrongly. This is a wire change to `GET …/topology`, landed together with its only consumer — the vendored explorer is served from this same binary (ADR-0010). (2) The explorer stopped hardcoding two labels that were **accidentally** true: the hub subtitle `"app · source of truth"` and the matrix header `"app (hub)"` rendered a false name for every workspace whose hub is not literally called `app`, and the former also reported the hub's drift as zero. Both now read from the payload. Not changed: `links[]` already carried real `from`/`to` taken off the edge, so direction was never wrong in the data — only incomplete. |
| 1.15 | 2026-08-25 | **The workspace is a snowflake, not a star — sub-hubs are expressible** (issue #623). v1.14 generalised the topology's *edges* into a directed graph so a chain's last hop could be drawn, but left the per-project label two-valued: `"role": if is_hub { "hub" } else { "spoke" }`. That label cannot describe a project which is both, and it does not merely omit the case — it reports it wrongly. In v1.14's own fixture (`infra1,infra2 → chart → app`) `chart` wins the hub on two inbound edges, so **`app` — the one project nothing else depends on — came back labelled `spoke`**. A project's position is now derived from its own in/out degree in a new [[crates/rto-graph/src/topology.rs#ProjectGraph]] and reported as four values: `root` (depended upon, depends on nothing hosted), `intermediate` (**the sub-hub** — both), `leaf` (an ordinary spoke), `isolated` (neither; previously reported as a spoke of a hub it never named). A cycle has no root and every member reports `intermediate`, which is a truthful report of a workspace that declares one rather than an error; nothing recurses, so a cycle cannot hang the view. Each project also carries `parents` — the hosted projects it points into — so a consumer can walk the hierarchy without re-deriving it. It must not re-derive it from `links`: that array is the **merge** of persisted edges and the correspondences `spoke_correspondence` infers live against the hub, and inferred correspondences are a config-key matching heuristic, not a declared dependency — walking them would make every project a child of the hub by construction and invent a `chart ↔ app` cycle out of a name match. So [[crates/rto-graph/src/topology.rs#project_graph]] reads **persisted external-ref edges only**, and [[crates/rto-graph/src/topology.rs#ProjectGraph::busiest_hub]] is now a reduction over that one shared structure rather than a second walk of the same edges. What `hub` means is unchanged and deliberately kept separate from `role`: it is the config-key **baseline the override matrix pivots on**, still the project with the most inbound edges, and in a chain that is the busiest node rather than the root — two different questions that in a star happened to have the same answer. It is reported per project as `isMatrixHub` so a consumer never has to infer it by comparing names. **Breaking**: the published `role` values change, and no `hub`/`spoke` is emitted; this lands in v3.0. Deferred: version pins remain hub-relative per step 8, and cannot be built until this is in — a spoke of a sub-hub pins the sub-hub, not the workspace hub, so the pin field must name its hub (issue #442). |
| 1.16 | 2026-08-26 | **Step 8's second pin source exists** (issue #609). `--pinned` has always documented two ways to find the hub version a spoke deploys: a `submodule` node, and an `image_ref` whose tag is tried as a hub git ref. The first worked. The second had exactly one producer — [[crates/rto-graph/src/extract.rs#dockerfile_facts]], reading Dockerfile `FROM` lines — and `IMAGE_REF_KIND` appeared nowhere else outside tests, so **a spoke that *deploys* an image rather than building one produced no `image_ref` at all**. It did not break at step 8; it never started, for precisely the deployment shape most likely to want it: on a real eight-repo workspace, 0 of 7 spokes had a detectable pin. Config files now yield `image_ref` nodes too ([[crates/rto-graph/src/extract.rs#config_image_refs]]), keyed `imageref:<file>#<dotted-key>` — by the config key rather than by position, mirroring `cfgkey:` so an inserted key above does not renumber the node below. Two shapes count, both settled here rather than guessed: **a whole image string** under a key whose last dotted segment is `image` (`container.api.image` from a k8s manifest, already mined as a `config_key` since v1.6, and a bare Helm `image: app:1.2`); and **the split Helm form**, `<prefix>.repository` with optional `<prefix>.tag` and `<prefix>.registry`, anchored on `repository` because that is the field naming the image — a `tag` alone says which version of nothing. Matching is on the key's **last segment**, not a suffix, so `base_image` is not swallowed; and a value containing whitespace is prose, not a reference. **A tag that cannot resolve still yields a node**, consistently with the Dockerfile path, which emits for `latest` too: the node is the spoke's *claim* about what it deploys, and whether it resolves is [[crates/roteiro/src/pins.rs#detect]]'s question — it tries the `[pins]` template, then `<tag>`, then `v<tag>`, and reports the spoke unpinned when none exist. Collapsing those would erase the distinction v1.12 and issue #505 established between **no claim** and **a claim that cannot be met**. This lives beside the config keys rather than in its own extractor because it is the same bytes, already parsed, asked a different question. `EXTRACT_BASE_VERSION` 12→13: without it a cached `values.yaml` keeps serving the fact set it produced before this existed — no `image_ref`, so no pin — until its bytes change, which for a deployment repo holding a stable version is exactly when they do not. |
| 1.17 | 2026-08-26 | **The hub rule finally has one home** (issue #623, second half). v1.15 consolidated the two rules that existed *inside* `graph_api` but left them there, and `graph_api` is `#[cfg(feature = "explorer")]` while the workspace **vault** renderer is not gated at all. So the first caller outside the web API — version pins in the shareable manifest (#442) — could not legally call the rule it needed, and its only alternatives were to write a third rule or to gate a Markdown export behind a web-API feature. [[crates/rto-graph/src/topology.rs#project_graph]] and [[crates/rto-graph/src/topology.rs#ProjectGraph]] now live in `rto-graph`, below every caller, on the same argument [[crates/rto-graph/src/text.rs#slugify]] and [[crates/rto-graph/src/text.rs#markdown_dialect]] already make for themselves. The four-value role is a typed `ProjectRole` rather than a `&'static str`, `#[non_exhaustive]` because the set describes shapes we have met rather than proving no other exists — a decision #431's widened guard now requires rather than allows to default. `determine_hub` becomes [[crates/rto-graph/src/topology.rs#ProjectGraph::busiest_hub]], and the links in v1.15's row above are repointed to follow it: a history row records what changed, but a link in it has to resolve to where the thing *is*, or the gate is asserting nothing. **One behaviour bug was found in the move rather than shipped by it**: the first draft swallowed the `WorkspaceError` the original propagated, so a project whose store would not open would have contributed no edges — silently moving the hub and changing every role. It now returns `WorkspaceError`, which already carries `Store(#[from] StoreError)`. Behaviour is otherwise unchanged: all 66 served-topology tests pass unmodified, and the module gains five of its own. |
| 1.18 | 2026-08-26 | **The shareable manifest carries version pins** (issue #442, completing step 8's reach into the vault). The manifest reconstructs the workspace *as rendered* — clone here, check out these commits — but that answers a different question from *what is each deployment repo running*, which is rarely the commit it was rendered at. A spoke at `HEAD` deploying `app@1.4.0` describes a live system its own commit id says nothing about. `_Home`'s `### Version pins` table now records, per member, the hub revision it deploys and where that was read from ([[crates/rto-render/src/obsidian.rs#MemberPin]], rendered by [[crates/rto-render/src/obsidian.rs#write_pins]], gathered by [[crates/roteiro/src/main.rs#workspace_pins]]). **Each pin names its hub**, and is resolved against that member's own **parent** in [[crates/rto-graph/src/topology.rs#project_graph]] rather than against the workspace's busiest node — the v1.15/v1.17 snowflake makes "the hub version this member pins" ambiguous otherwise, and taking the busiest node as everyone's hub would report an infra repo as pinning a version of an application it has never referenced. One row per **dependency**, not per member: a project with two parents pins each separately. **A dependency whose version cannot be resolved is reported as `(none detected)` rather than dropped**, and the caption counts found against asked (`1 of 2 dependencies resolve`) — the rule #505 established, and the one that matters most here, because before #609 the image half of detection could not fire at all for a Helm-shaped spoke, so a workspace of entirely unresolved pins is exactly the state that used to be invisible. A pin follows a **declared** dependency: the table reads persisted `external_ref` edges and does not infer, so a workspace that has been synced but never `links --write`-ten shows no section at all — consistent with the section being part of a manifest, where an inferred name match is not a claim worth reproducing. Verified end to end against a spoke with **no Dockerfile**, declaring `image.repository`/`image.tag` in `values.yaml` as a chart does. |

---
Title: A dynamic OKF viewer — the bundle is the source, and it is somebody else's
Space: ARCH
Parent: ADRs

# ADR-specific metadata (unknown keys are ignored; used for indexing/search)
type: adr
adr-id: "0022"
status: Accepted                    # Draft | For Review | Accepted | Rejected | Superseded
architectural-significance: MEDIUM  # SOFT | LOW | MEDIUM | HIGH | VERY HIGH
domain: Developer Tooling
decision-makers: ["The Roteiro Project Team"]
superseded-by:
version: "1.0"
last-modified: 2026-09-02
confluence-url:
---

# ADR-0022: A dynamic OKF viewer — the bundle is the source, and it is somebody else's

| | |
|---|---|
| **Document version** | 1.0 |
| **Status** | Accepted |
| **Decision makers** | The Roteiro Project Team |
| **Related** | [[docs/adr/0021-open-knowledge-format-bundle.md]] · [[docs/adr/0010-explorer-web-app-vendored-js.md]] · [[docs/adr/0008-multi-repo-workspace-serve.md]] · [[docs/adr/0017-dependency-security-policy.md]] |

## Reference

- The specification: <https://github.com/GoogleCloudPlatform/open-knowledge-format/blob/main/SPEC.md> (v0.2)
- The bundle Roteiro emits: `roteiro render okf` ([[docs/adr/0021-open-knowledge-format-bundle.md]])
- The existing served UI whose shape this follows: `roteiro explorer`
- Reading a bundle *as a bundle*: `crates/rto-render/src/okf/inspect.rs`

## Summary

Add a **dynamic OKF viewer**: a served, themed, read-only web view of an Open
Knowledge Format bundle, rendered from the bundle's own markdown and links at
request time rather than from a build step.

It takes a bundle path. It has no knowledge of Roteiro's graph, so it works on
any conformant bundle, and Roteiro's own `okf/` output is merely the default
argument. It ships behind a feature and merges onto `serve` the way `explorer`
already does.

## Context

ADR-0021 made the bundle the graph's shareable form and then closed the loop
halfway: Roteiro **writes** OKF (`render okf`), and since #706 **reads** a peer's
bundle into the graph (`import --from okf`). What it has never had is a way to
*look at* a bundle — ours or anybody's — without importing it.

That gap has three separate costs.

**For a consumer, the bundle is currently write-only in practice.** A bundle is
nested directories of markdown with YAML frontmatter. It opens in Obsidian, and
it renders adequately on GitHub, but neither shows what OKF actually models:
trust tiers, provenance, the link graph between concepts, which links resolve.
The information is in the files and nothing surfaces it.

**For us, nothing consumes our own output.** ADR-0021's argument for an
independent implementation is that a checker of our own construction, run over
our own output, can only catch a mistake we did not make twice. `okf-core` gives
us an independent *reader*; a viewer built on it is the first thing in this
project that would render our bundle the way a stranger's tool would, and
therefore the first that can visibly disagree with us.

**For the ecosystem, there is no OKF viewer at all.** The tooling that exists is
a validator, a CLI and a studio. A themed, read-only viewer that takes any
bundle is a generic artifact, and it is generic *for free* — the reading code
does not know whose bundle it has.

### What already exists, and what it is shaped for

| Surface | Shape | Fit for viewing |
| --- | --- | --- |
| `rto_render::okf::read` | **import**: maps concepts to graph facts, applies provenance, screening and trust adoption | Wrong shape — viewing must not import |
| `rto_render::okf::inspect` | reads a bundle **as a bundle** over `okf_core::Bundle` — `trust_summary`, `link_report`, `diff_report` | Right shape; this is the seam |
| `roteiro explorer` | served read-only JSON API + HTML shell + vendored cytoscape, merged onto `/v1` under `serve` | The precedent for how a UI feature ships here |
| `website/public/style.css` | the roteiro.dev theme | Reusable as-is |

The distinction in the first two rows is the load-bearing one. `read` exists to
answer *"what would this add to our graph"* and necessarily makes decisions —
which provenance class a peer's fact takes, whether to trust their tiers, what
the screener flags. A viewer must make none of those decisions, because it is
showing the document rather than adopting it.

## Decision makers

The Roteiro Project Team.

## Recommended option

**Option 3 — a served viewer over `okf_core::Bundle`, behind its own feature,
merged onto `serve`.**

## Options considered + consequences

### Option 1: Render the bundle to static HTML in `render okf`

- Pros: no server, no new feature, reuses the docs-site renderer wholesale.
- Cons: it is not a viewer, it is a third renderer. It would go stale the moment
  a bundle file changed, which defeats the point for anyone *authoring* OKF; it
  would only ever see bundles we generate, since nothing invokes it on a
  stranger's directory; and it would grow a second, divergent copy of the
  docs-site theme logic. Rejected.

### Option 2: Extend `roteiro explorer` to show OKF

- Pros: one served UI, one feature flag, cytoscape already vendored there.
- Cons: `explorer` serves **the graph** — its API is `/v1/graph/*` over
  `rto_graph::Store`. A bundle is not the graph and must not be loaded into one
  to be viewed; doing so would reintroduce exactly the import step this decision
  exists to avoid, and would make viewing a stranger's bundle require trusting
  it first. Rejected on the same grounds as Option 1's staleness: the wrong
  source of truth.

### Option 3: A served viewer over `okf_core::Bundle` (recommended)

- Pros: reads the bundle at request time, so it is genuinely dynamic and an
  author sees an edit on reload. Uses the independent reader, so what it shows
  is what a third party's tool would see. Generic by construction — it takes a
  path, and Roteiro's own `okf/` is only the default. Follows the `explorer`
  precedent for feature-gating and for merging onto `serve`, so it costs the
  default build nothing.
- Cons: a new served surface, and a new class of untrusted input — see
  **Consequences**. Adds a feature to the matrix that CI must actually build,
  which this project has demonstrably got wrong before.

## Implementation

### The viewer is read-only, in the strong sense

It opens a bundle, renders it, and writes nothing — not to the graph, not to the
store, not to the bundle. `import --from okf` remains the only path by which a
peer's content enters the graph, and it keeps its consent gate. A viewer that
could import would make "have a look at this bundle" a trust decision, which is
precisely the thing ADR-0021 spent a consent prompt avoiding.

### Untrusted input is the main risk, and it is not hypothetical

A bundle is third-party markdown. Rendering it to HTML is an injection surface,
and `screen.rs` exists because ADR-0021 already treats peer bundles as content
that may be written to be *read as instructions*. Consequences:

- markdown is rendered with HTML **disabled**, not sanitised — an allow-list of
  tags is a thing to get wrong, and no OKF document needs raw HTML;
- link targets are resolved within the bundle and rejected if they escape it;
- the screener's classes are surfaced in the UI rather than silently dropped, so
  a reader sees that a document tripped it;
- images embed from within the bundle only, and no remote fetch is issued —
  keeping the offline-by-default posture ADR-0001 sets.

### What is embedded rather than read from the bundle

Per the request that started this: the concept **graph** and any UI imagery are
embedded assets, not bundle content. The graph is rendered with the cytoscape
build already vendored for `explorer`, over edges derived from
`Bundle::links_from`/`backlinks`. The theme is `website/public/style.css`,
embedded so the viewer is self-contained and works with no network.

### Surface

- `roteiro okf view [path] [--port N]` — serve a bundle, defaulting to `okf/`.
- Under `serve`, the viewer merges onto the same port as `/v1` and `/mcp`,
  following `serve_v1_tail`'s existing composition (ADR-0008).
- Feature: `okf-viewer`, pulling `axum` + `tokio`, exactly as `explorer` does.
  **Off by default.**

### CI must build it

`docs/REVIEW_CHECKLIST.md` records that only `--all-features`, the default set,
and three `--no-default-features` shapes are built, and that any other
combination is built by nothing. This project has already been bitten twice by
that — an orphaned stub under `image-ocr`, and four more unused items under
`audio-transcribe`, neither of which any job compiled. A new optional feature
that no job builds in isolation would be the third. The `no-default-features`
job gains this feature as a fourth configuration.

## Advice Received

The shape of this decision came from a review conversation rather than from the
issue tracker, and two points in it changed the design:

- that a viewer keeping the site theme but sourcing content from OKF is
  *generic for free*, which moved it from "a page for our docs" to "a viewer for
  any bundle" and made `okf_core::Bundle` the obvious source;
- that it belongs as an optional extension on `serve` alongside the standard UI
  content, which is what pointed at the `explorer` precedent rather than at a
  new server.

## Document version history

| Version | Date | Notes |
|---------|------|-------|
| 0.1 | 2026-09-02 | Initial draft. Records the gap ADR-0021 left — Roteiro writes OKF and imports it, but cannot look at one — and proposes a served, themed, read-only viewer over `okf_core::Bundle`. The load-bearing distinction is between `okf::read` (import-shaped: provenance, screening, trust adoption) and `okf::inspect` (bundle-shaped), and the viewer builds on the second so that looking at a stranger's bundle is not a trust decision. Rejects rendering to static HTML (a third renderer, stale by construction, and only ever pointed at our own output) and extending `explorer` (serves the graph, so viewing would require importing first). Records the untrusted-input posture — HTML disabled rather than sanitised, no remote fetches, screener classes surfaced — and that the feature must be added to the `no-default-features` job, since two defects have already been found in feature combinations no job builds. |
| 1.0 | 2026-09-02 | **Accepted and implemented.** The recommended option was built as written: `rto_render::okf::view` is the model and `roteiro`'s `okf_viewer` is the HTTP, behind an `okf-viewer` feature that is off by default and costs the default build nothing. `roteiro okf view [path]` serves it alone; under `serve` it nests at **`/okf`**, because the explorer already holds `/` and two UIs cannot both be the root — and only when the project has an `okf/` bundle, since `serve` hosts workspaces rather than a bundle path and a route that 404'd every request would be worse than an absent one. **One thing the draft did not anticipate**: nesting means every generated href must carry the mount prefix, because a concept id contains slashes and so relative hrefs sit at varying depths. A router that emitted absolute unprefixed paths would look right standalone and 404 on every link the moment it was mounted, so `base` is threaded through and `a_nested_mount_prefixes_every_href` pins it. **The split moved in the draft's favour**: only the *server* is behind the feature. Every rule about untrusted content — HTML escaped and never emitted, a link rewritten only when it resolves inside the bundle, no image fetched from off it, screener classes surfaced — lives in `okf::view`, which is unconditional and therefore compiled and tested by every job. That was chosen because this repository has found three defects in a year inside feature combinations nothing built. Two additions beyond the draft: raw HTML is **escaped and shown** rather than dropped, since silently discarding part of a document is its own kind of lie; and a `Content-Security-Policy` of `default-src 'self'` rides every response as a second, independently-failing line behind the escaping. The `no-default-features` job gains clippy **and test** cells for the feature, as the draft committed — tests too, because the route tests are themselves feature-gated and a clippy-only cell would compile them and never run them. No decision in this ADR changes. |

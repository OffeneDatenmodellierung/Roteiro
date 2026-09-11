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
version: "1.2"
last-modified: 2026-09-10
confluence-url:
---

# ADR-0022: A dynamic OKF viewer — the bundle is the source, and it is somebody else's

| | |
|---|---|
| **Document version** | 1.2 |
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

It reads a bundle path and has no knowledge of Roteiro's graph, so it works on
any conformant bundle, and Roteiro's own `okf/` output is merely the default.
It ships behind a feature and merges onto `serve` the way `explorer` already
does.

**Amended in v1.2**: the path stopped being a *command argument* and became the
working directory. The viewer no longer has a server of its own; it is mounted
by `explorer` and by `serve`, which between them offer every bundle the machine
is configured to know about **plus** the current directory. What it reads is
unchanged — a directory of markdown, through `okf_core`, never the graph.

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
- **v1.2 does not reverse this.** The viewer is now *served by* `explorer`, and
  still does not read `explorer`'s data: it is mounted beside the graph API on
  one port, reading the same directories `okf_core` always read. Sharing a
  listener is not sharing a source of truth, and that is the whole distinction
  this rejection turns on.

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

- The viewer has **no command of its own** (v1.2). It merges onto `roteiro
  explorer` and onto `roteiro serve`, at `/okf`, on the port those already use.
- Under `serve`, it follows `serve_v1_tail`'s existing composition (ADR-0008).
- Every OKF bundle the server can see is mounted at `/okf/{slug}`, and `/okf`
  itself either lists them or — when there is one — redirects to it.
- **The path-based entry survives as the working directory**: `roteiro explorer`
  run inside a bundle serves that bundle, whether or not there is a repository
  there, which is what `roteiro okf view <path>` was for.
- Feature: `okf-viewer`, **implying `explorer`** (v1.2) and pulling `axum` +
  `tokio` through it. **Off by default.** It implies rather than parallels
  because the viewer is a mount: with no server to mount it on, every item in it
  is dead code.

### CI must build it

`docs/REVIEW_CHECKLIST.md` records that only `--all-features`, the default set,
and three `--no-default-features` shapes are built, and that any other
combination is built by nothing. This project has already been bitten twice by
that — an orphaned stub under `image-ocr`, and four more unused items under
`audio-transcribe`, neither of which any job compiled. A new optional feature
that no job builds in isolation would be the third. The `no-default-features`
job gains this feature as a fourth configuration.

**And a fifth, in v1.2.** Once `okf-viewer` implied `explorer`, the viewer cell
started building a *superset* of the explorer, so `explorer` alone was left to
`--all-features` — which turns the viewer on and therefore cannot fail on
anything only the smaller shape reaches. It was already broken when that was
noticed. The rule this keeps arriving at: **a cell that subsumes another is not
coverage of it**, so implying a feature costs a new cell for what it implies.

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
| 1.1 | 2026-09-10 | Amended (no issue; found while sizing the knowledge layer). **The graph page drew every concept, and at this repository's own scale it never finished.** It handed 9,766 nodes and 41,980 edges — 8.16 MB — to a force-directed layout synchronously, having been tested only against the 9-concept fixture. The server was never the problem: that payload builds in 0.25 s. Three measurements decided the fix. (a) **No ranking rescues a whole-graph view.** This bundle is hub-and-spoke — max degree 1,670 against a median of 4 — and the 100 highest-degree concepts share just **157** of its 41,980 edges, so any "top N" tier renders as disconnected scatter whatever N is and whatever it ranks by. (b) Neighbourhoods, by contrast, are the right unit: a median concept reaches 3 nodes at one hop and 436 at two. (c) A force-directed layout is quadratic per tick in its repulsion step, so the node count is what decides whether the page renders at all. So `/graph` is now **focused**: without a `focus` it is a ranked **list** of well-connected concepts rather than a drawing, because there is no whole-graph picture worth drawing and pretending otherwise is what made the page unusable; with one it draws that concept's neighbourhood under a node budget, laid out `concentric` on the focus — linear, and it says what the picture means. **Bounded, not capped.** Every scoped response carries `shown_nodes`/`total_nodes`, `shown_edges`/`total_edges` and `beyond` — concepts linked to something drawn that are not drawn — and the page states them, because a view that quietly draws some of its nodes is the defect [[docs/adr/0024-screening-widened.md]] fixed for the binary inventory: the reader cannot tell a small bundle from a truncated picture of a large one. `beyond` is deliberately **one** number covering both ways a view falls short — budget and depth horizon — after reporting them separately cost a test that asserted one and measured the other. `depth` and `limit` are clamped at the edge (3 and 500), because a caller-supplied budget taken at its word would move the original defect into a URL. Payload for the default view: 8,159,353 B → 213,053 B. |
| 1.2 | 2026-09-10 | Amended (no issue; task #22). **The premise changed: the viewer stopped being a server and became a mount.** v1.0 gave it two homes — `roteiro okf view <path>` standing alone, and a nest under `serve` — and the standalone one paid for itself twice over: a second command, a second port, a second tokio runtime, and a route shape (`base` empty, `/` real) that was the *only* one anybody exercised. That last part hid a defect for eight days. `nest("/okf")` serves `/okf` and **not** `/okf/`, so the "Concepts" link — written as `{base}/` — has 404'd under `serve` since v1.0 while working perfectly standalone; `index_href` now writes that rule once instead of at four call sites. **What the fold must not lose is the path.** ADR-0022's whole claim is that the viewer takes a path and knows nothing of Roteiro's graph, so it works on any conformant bundle; a viewer reachable only through a configured workspace would quietly become a viewer for *our* bundles. The resolution is that `explorer` serves the configured workspaces' bundles **and** the current directory when one is there — so `roteiro okf view <path>` is exactly `cd <path> && roteiro explorer`, and it works with no repository, no config and no graph, which is the case that proves the premise survived. **One port now holds many bundles**, so `/okf` gained a mount layer: a bundle per workspace project, deduplicated by canonical path, each at `/okf/{slug}`. Slugs are folded to one readable segment rather than percent-encoded, because this is a URL a person is meant to share — and folding collides, so `disambiguate` suffixes, since `nest` does not complain about a prefix it already holds and the second mount would otherwise take the first's URL and make one bundle silently unreachable. `/okf` itself lists the bundles, or redirects when there is one, because a chooser with one row is a click that tells the reader nothing. **Three failures here are route-table failures no type catches**: that shadowed nest; a chooser registered at `/` instead of at `base`, which makes axum panic at startup the moment the mount layer is merged into a host that owns the root; and the chooser being the one page *not* inside a bundle, so `{base}/okf-viewer.css` resolves to a bundle's route everywhere else and to nothing there — it rendered unstyled with a broken nav. All three are pinned by driving a whole merged router and asserting every `href` it writes resolves. Measured against this repository: six workspaces, one bundle, `/okf` → 307 → `/okf/Roteiro-Roteiro`, 3,878,122 B; and a copied bundle with no repository above it serving on `/okf/bare` with no Explorer link offered, because there is no explorer to link to. **The feature matrix moved, and the isolation cell earned itself back.** `okf-viewer` now **implies `explorer`**: a mount with no mount point is dead code, and `--no-default-features --features okf-viewer -D warnings` said so — 25 never-used items, the whole module. That in turn left `--features explorer` built by nothing but `--all-features`, which cannot fail on it because it turns the viewer on too — and it was already broken, because folding the viewer in made `serve_graph_ui` construct a multi-threaded runtime whose tokio feature only `okf-viewer` enables. The runtime is now chosen by `#[cfg]`, since the current-thread choice stays right for an explorer serving only compiled-in assets, and the `no-default-features` job gains a **fifth** cell for `explorer` alone. This is the fourth defect this project has found in a feature combination no job compiled, which is the reason v1.0 made the cell part of the decision rather than a follow-up; the same reasoning applied to the shape the fold created. |

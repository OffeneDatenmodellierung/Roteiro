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
version: "1.5"
last-modified: 2026-09-14
confluence-url:
---

# ADR-0022: A dynamic OKF viewer — the bundle is the source, and it is somebody else's

|  |  |
|---|---|
| **Document version** | 1.5 |
| **State** | Accepted |
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

**Amended in v1.5**: the mounted viewer's **chrome** now states what the mount
already was. Since v1.2 the viewer has run in the same binary, the same process
and the same port as the explorer, and its header expressed none of that — it
printed the bundle's absolute filesystem path where the shell prints a
breadcrumb, and offered no way back to the workspace at all. Mounted, it now
wears the shell's own project bar: the `← Workspace` control and the
`Roteiro · Workspace ▸ OKF ▸ <label>` trail, reusing `assets/index.html`'s class
names and therefore its rules and the v1.3 token master. It names the bundle
with `Mount::label` — the `<workspace>/<project>` string `okf_mounts` already
resolved for the chooser and the startup line — rather than deriving a second
one. **The standalone form gains none of it, and that is the decision rather
than an omission.** `serve_okf_only` passes `explorer: None` because there is no
workspace above the bundle and no explorer to return to; a trail there would name
two things that do not exist and a back control would link to a route that 404s,
which is exactly the slide into "a viewer for *our* bundles" this ADR exists to
refuse. Standalone keeps the v1.0 header, with the bundle directory's own name
where the absolute path was: those leading segments described the operator's
filesystem rather than the bundle, they are of no use to a reader who was sent
the URL, and `--addr` — with #832's `--scope` — makes **both** forms expressible
off loopback, so this was host information published to every client in the case
anybody actually runs.

**Amended in v1.3**: the viewer's theme is **no longer the docs-site theme**.
Roteiro is one application and must look like one, so the viewer, the workspace
hub, the project graph view and the `links --matrix --html` export are now all
themed from a single token master inside the crate —
`crates/roteiro/src/assets/tokens.css` — in a light and a dark mode. Everything
below that names `website/public/style.css` as this viewer's theme is superseded
by that decision; the docs site itself is untouched and stays standalone. What
the viewer *reads* is still unchanged, and so is the self-contained,
no-network posture: the master is a file the crate can `include_str!`, not an
asset fetched from anywhere.

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
|---|---|---|
| `rto_render::okf::read` | **import**: maps concepts to graph facts, applies provenance, screening and trust adoption | Wrong shape — viewing must not import |
| `rto_render::okf::inspect` | reads a bundle **as a bundle** over `okf_core::Bundle` — `trust_summary`, `link_report`, `diff_report` | Right shape; this is the seam |
| `roteiro explorer` | served read-only JSON API + HTML shell + vendored cytoscape, merged onto `/v1` under `serve` | The precedent for how a UI feature ships here |
| `website/public/style.css` | the roteiro.dev theme | ~~Reusable as-is~~ — **superseded in v1.3**: the viewer is themed by the app's own token master, not by the docs site |

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

  **v1.3 note — the copy happened anyway, by the other route.** Option 3 was
  taken and the viewer still ended up carrying a byte-copy of the docs site's
  `:root`, pinned by a test, because `include_str!` cannot reach out of the
  crate directory. So the con was real and the option choice was not what
  decided it; the divergence risk followed the *theme source*, not the
  renderer. Naming the risk and then pinning the bytes bought four months of
  correctness and no direction: the copy could only ever track the site, never
  the application it shipped inside. v1.3 removes the copy by moving the
  palette inside the crate, where every in-app surface can read it.

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
`Bundle::links_from`/`backlinks`.

The theme, **since v1.3**, is `crates/roteiro/src/assets/tokens.css` — Roteiro's
one token master, light and dark, shared with the explorer shell and the
`links --matrix --html` export. It is embedded, so the viewer is still
self-contained and works with no network; what changed is *which* file it
embeds. Until v1.2 it was a byte-copy of `website/public/style.css`, asserted
equal by `the_viewer_shares_the_sites_palette`, and the reason was mechanical:
`roteiro` publishes to crates.io, `cargo package` takes only files under the
crate directory, and an `include_str!` reaching up to `website/` would have
shipped a crate that does not compile.

That constraint has not gone away — the palette moved to satisfy it rather than
to escape it. Two things made the copy wrong independently of it. It made the
viewer look like the docs site and unlike the application it ships inside, which
is the whole of the v1.3 decision. And it copied the site's **light** `:root`
without the site's dark block, so the viewer was cream-only while the thing it
claimed to share had two modes — a copy can drift, and this one shipped
half-drifted from the day it was made.

`the_viewer_shares_the_sites_palette` is **re-aimed, not deleted**, as
`the_viewer_is_themed_by_the_app_token_master`. The relationship it pinned is
deliberately severed, so its assertion is obsolete; the protection it gave — the
viewer silently drifting from its palette source — is not, and is what the
replacement keeps. It asserts the served stylesheet carries the master verbatim,
that the viewer's own rules declare no palette and no colour literal, and that
every `var(--x)` the viewer names is one the master declares. The third is what
makes it falsifiable: rename a token upstream and it goes red.

### Surface

- The viewer has **no command of its own** (v1.2), and still has none (v1.4).
  It merges onto `roteiro explorer` and onto `roteiro serve`, at `/okf`, on the
  port those already use.
- Under `serve`, it follows `serve_v1_tail`'s existing composition (ADR-0008).
- Every OKF bundle the server can see is mounted at `/okf/{slug}`, and `/okf`
  itself either lists them or — when there is one — redirects to it.
- **The path-based entry survives as the working directory**: `roteiro explorer`
  run inside a bundle serves that bundle, whether or not there is a repository
  there, which is what `roteiro okf view <path>` was for.
- **And, since v1.4, as `--scope bundle <PATH>`** on either server — a *flag*,
  not a command. The working-directory entry above was the whole of it until
  then, and it had a hole: `explorer_cwd_set` calls `Repo::discover`, which
  succeeds inside a repository, so the bundle arm was unreachable there. A user
  standing in a repository could not reach bundle-only mode by any means. That
  made "a viewer for any conformant bundle" true of one directory layout and
  false of the other — the failure the paragraph below is about. See
  [[docs/adr/0008-multi-repo-workspace-serve.md]] for the scopes' own decision.
  Under this scope there is **no `/v1/graph` and no explorer app**, for the
  reason the working-directory entry already gives: there is no graph in a
  bundle, and a page offering an Explorer link to a route that 404s would be
  worse than a page that does not.
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
  any bundle" and made `okf_core::Bundle` the obvious source. **The conclusion
  survives v1.3; the premise does not.** What made the viewer generic was
  sourcing content from `okf_core::Bundle`, and that is untouched. The theme was
  never load-bearing for genericity — it only ever made the viewer look like our
  docs — and a viewer for anybody's bundle has no reason to wear the roteiro.dev
  skin rather than Roteiro's;
- that it belongs as an optional extension on `serve` alongside the standard UI
  content, which is what pointed at the `explorer` precedent rather than at a
  new server.

## Document version history

| Version | Date | Notes |
|---|---|---|
| 0.1 | 2026-09-02 | Initial draft. Records the gap ADR-0021 left — Roteiro writes OKF and imports it, but cannot look at one — and proposes a served, themed, read-only viewer over `okf_core::Bundle`. The load-bearing distinction is between `okf::read` (import-shaped: provenance, screening, trust adoption) and `okf::inspect` (bundle-shaped), and the viewer builds on the second so that looking at a stranger's bundle is not a trust decision. Rejects rendering to static HTML (a third renderer, stale by construction, and only ever pointed at our own output) and extending `explorer` (serves the graph, so viewing would require importing first). Records the untrusted-input posture — HTML disabled rather than sanitised, no remote fetches, screener classes surfaced — and that the feature must be added to the `no-default-features` job, since two defects have already been found in feature combinations no job builds. |
| 1.0 | 2026-09-02 | **Accepted and implemented.** The recommended option was built as written: `rto_render::okf::view` is the model and `roteiro`'s `okf_viewer` is the HTTP, behind an `okf-viewer` feature that is off by default and costs the default build nothing. `roteiro okf view [path]` serves it alone; under `serve` it nests at **`/okf`**, because the explorer already holds `/` and two UIs cannot both be the root — and only when the project has an `okf/` bundle, since `serve` hosts workspaces rather than a bundle path and a route that 404'd every request would be worse than an absent one. **One thing the draft did not anticipate**: nesting means every generated href must carry the mount prefix, because a concept id contains slashes and so relative hrefs sit at varying depths. A router that emitted absolute unprefixed paths would look right standalone and 404 on every link the moment it was mounted, so `base` is threaded through and `a_nested_mount_prefixes_every_href` pins it. **The split moved in the draft's favour**: only the *server* is behind the feature. Every rule about untrusted content — HTML escaped and never emitted, a link rewritten only when it resolves inside the bundle, no image fetched from off it, screener classes surfaced — lives in `okf::view`, which is unconditional and therefore compiled and tested by every job. That was chosen because this repository has found three defects in a year inside feature combinations nothing built. Two additions beyond the draft: raw HTML is **escaped and shown** rather than dropped, since silently discarding part of a document is its own kind of lie; and a `Content-Security-Policy` of `default-src 'self'` rides every response as a second, independently-failing line behind the escaping. The `no-default-features` job gains clippy **and test** cells for the feature, as the draft committed — tests too, because the route tests are themselves feature-gated and a clippy-only cell would compile them and never run them. No decision in this ADR changes. |
| 1.1 | 2026-09-10 | Amended (no issue; found while sizing the knowledge layer). **The graph page drew every concept, and at this repository's own scale it never finished.** It handed 9,766 nodes and 41,980 edges — 8.16 MB — to a force-directed layout synchronously, having been tested only against the 9-concept fixture. The server was never the problem: that payload builds in 0.25 s. Three measurements decided the fix. (a) **No ranking rescues a whole-graph view.** This bundle is hub-and-spoke — max degree 1,670 against a median of 4 — and the 100 highest-degree concepts share just **157** of its 41,980 edges, so any "top N" tier renders as disconnected scatter whatever N is and whatever it ranks by. (b) Neighbourhoods, by contrast, are the right unit: a median concept reaches 3 nodes at one hop and 436 at two. (c) A force-directed layout is quadratic per tick in its repulsion step, so the node count is what decides whether the page renders at all. So `/graph` is now **focused**: without a `focus` it is a ranked **list** of well-connected concepts rather than a drawing, because there is no whole-graph picture worth drawing and pretending otherwise is what made the page unusable; with one it draws that concept's neighbourhood under a node budget, laid out `concentric` on the focus — linear, and it says what the picture means. **Bounded, not capped.** Every scoped response carries `shown_nodes`/`total_nodes`, `shown_edges`/`total_edges` and `beyond` — concepts linked to something drawn that are not drawn — and the page states them, because a view that quietly draws some of its nodes is the defect [[docs/adr/0024-screening-widened.md]] fixed for the binary inventory: the reader cannot tell a small bundle from a truncated picture of a large one. `beyond` is deliberately **one** number covering both ways a view falls short — budget and depth horizon — after reporting them separately cost a test that asserted one and measured the other. `depth` and `limit` are clamped at the edge (3 and 500), because a caller-supplied budget taken at its word would move the original defect into a URL. Payload for the default view: 8,159,353 B → 213,053 B. |
| 1.2 | 2026-09-10 | Amended (no issue; task #22). **The premise changed: the viewer stopped being a server and became a mount.** v1.0 gave it two homes — `roteiro okf view <path>` standing alone, and a nest under `serve` — and the standalone one paid for itself twice over: a second command, a second port, a second tokio runtime, and a route shape (`base` empty, `/` real) that was the *only* one anybody exercised. That last part hid a defect for eight days. `nest("/okf")` serves `/okf` and **not** `/okf/`, so the "Concepts" link — written as `{base}/` — has 404'd under `serve` since v1.0 while working perfectly standalone; `index_href` now writes that rule once instead of at four call sites. **What the fold must not lose is the path.** ADR-0022's whole claim is that the viewer takes a path and knows nothing of Roteiro's graph, so it works on any conformant bundle; a viewer reachable only through a configured workspace would quietly become a viewer for *our* bundles. The resolution is that `explorer` serves the configured workspaces' bundles **and** the current directory when one is there — so `roteiro okf view <path>` is exactly `cd <path> && roteiro explorer`, and it works with no repository, no config and no graph, which is the case that proves the premise survived. **One port now holds many bundles**, so `/okf` gained a mount layer: a bundle per workspace project, deduplicated by canonical path, each at `/okf/{slug}`. Slugs are folded to one readable segment rather than percent-encoded, because this is a URL a person is meant to share — and folding collides, so `disambiguate` suffixes, since `nest` does not complain about a prefix it already holds and the second mount would otherwise take the first's URL and make one bundle silently unreachable. `/okf` itself lists the bundles, or redirects when there is one, because a chooser with one row is a click that tells the reader nothing. **Three failures here are route-table failures no type catches**: that shadowed nest; a chooser registered at `/` instead of at `base`, which makes axum panic at startup the moment the mount layer is merged into a host that owns the root; and the chooser being the one page *not* inside a bundle, so `{base}/okf-viewer.css` resolves to a bundle's route everywhere else and to nothing there — it rendered unstyled with a broken nav. All three are pinned by driving a whole merged router and asserting every `href` it writes resolves. Measured against this repository: six workspaces, one bundle, `/okf` → 307 → `/okf/Roteiro-Roteiro`, 3,878,122 B; and a copied bundle with no repository above it serving on `/okf/bare` with no Explorer link offered, because there is no explorer to link to. **The feature matrix moved, and the isolation cell earned itself back.** `okf-viewer` now **implies `explorer`**: a mount with no mount point is dead code, and `--no-default-features --features okf-viewer -D warnings` said so — 25 never-used items, the whole module. That in turn left `--features explorer` built by nothing but `--all-features`, which cannot fail on it because it turns the viewer on too — and it was already broken, because folding the viewer in made `serve_graph_ui` construct a multi-threaded runtime whose tokio feature only `okf-viewer` enables. The runtime is now chosen by `#[cfg]`, since the current-thread choice stays right for an explorer serving only compiled-in assets, and the `no-default-features` job gains a **fifth** cell for `explorer` alone. This is the fourth defect this project has found in a feature combination no job compiled, which is the reason v1.0 made the cell part of the decision rather than a follow-up; the same reasoning applied to the shape the fold created. |
| 1.3 | 2026-09-13 | Amended (no issue; maintainer decision). **The viewer stopped wearing the docs-site theme.** Roteiro had three in-app web surfaces and three unrelated visual languages: a light workspace hub, a dark project graph view whose values lived under the same names but in a second block, and this cream/serif viewer carrying a byte-copy of `website/public/style.css`. A fourth surface, the `links --matrix --html` export, had a ninth-colour palette of its own that nothing compared to anything. They are now one token master — `crates/roteiro/src/assets/tokens.css`, light and dark — and all four read it: the shell splices it into its inline `<style>`, the viewer prepends it to its served stylesheet, and the export inlines it, which is why the master is a crate file rather than a route (the export is self-contained by requirement and has no server). **The docs site is explicitly out of scope** and unchanged: it is a separate artefact with its own lifecycle, and making a published site depend on an application binary's assets would be the same drift risk pointing the other way. **Dark is a mode, not a second identity.** The project graph view stays dark whatever the OS prefers — a graph canvas reads better that way — but it carries `data-theme="dark"` and gets the master's dark values, so `#view-project` declares nothing and the hand-kept `background: #0d1117` on `body` that had to track it by hand is gone. The other three surfaces gained `prefers-color-scheme`, which closes the asymmetry the copy created: the viewer had taken the site's light `:root` and not its dark block, so it was cream-only while the thing it claimed to share had two modes. **`app.js` stopped hard-coding colour.** Its 42 hex literals for graph nodes and edges did not read the custom properties, so the token layer was authoritative for chrome and fictional for the graph; cytoscape cannot read a custom property, so the values are now resolved with `getComputedStyle` **from the element the graph lives in** — which is what lets one code path serve a light canvas and a forced-dark one — and re-resolved when the OS preference flips under a live graph. Zero colours remain in `app.js`, `index.html`, `okf-viewer.css` or `overview.rs`, and "colour" means every syntax rather than every syntax somebody listed. The first version of that claim was measured with a `#rrggbb` scanner and was **wrong**: two `rgba()` shadows sat in the shell while the guard stayed green, and an undeclared `var(--mono, …)` went unseen because the check skipped any `var()` carrying a fallback. Both were the same defect — a scanner that recognises one form and reads every other form as absence — so the check was **inverted into two allowlists**: a declaration is colour-scanned unless its property is known not to accept a `<color>`, and a scanned value may contain only `var()` references, numbers, and a named set of keywords and functions. `rgba()`, a bare `crimson`, an `oklch()` and a colour hiding in a `var()` fallback are all reported now, none of them by having been thought of. An incomplete allowlist refuses visibly; an incomplete denylist passes silently. **The drift guard was re-aimed, not deleted.** `the_viewer_shares_the_sites_palette` pinned viewer==website byte-for-byte; that relationship is deliberately severed, so the assertion is obsolete and the protection is not. `the_viewer_is_themed_by_the_app_token_master` replaces it, with siblings for the shell and the export, and all of them fail on a renamed token rather than only on a reformatted copy — the failure mode that matters, since an undefined custom property is invalid at computed-value time and renders plausibly wrong rather than erroring (#512). `palette_scopes` became nesting-aware in the same change, because folding a dark-only token into `:root` would have made its second assertion vacuous the moment the palette gained a mode. **One behaviour the token layer has that the literals did not**: a cytoscape graph holds resolved colours, so it keeps whichever mode it was styled under. `#topology` sits outside `#view-project`, so with the project view forcing dark on `<html>` the hidden workspace graph resolved dark too, and walking back to a light workspace left a dark topology on a light panel — reproduced over CDP, and fixed by routing every mode change through the one place that writes the attribute and re-resolves the live graphs. |
| 1.4 | 2026-09-13 | Amended (issue #810). **The path entry came back as a flag, and this ADR has to say so rather than let it arrive silently.** v1.2 retired `roteiro okf view <path>` on the argument that `cd <path> && roteiro explorer` is exactly the same thing, and wrote down that the viewer "has no command of its own". Both halves were load-bearing and only one of them held. The ADR's own premise — "a viewer reachable only through a configured workspace would quietly become a viewer for **our** bundles" — depends on the path entry working; the fold left it working *only outside a repository*, because `explorer_cwd_set` calls `Repo::discover` and the bundle arm sits in that call's `Err`. So inside a repository the mode was unreachable, and the premise this ADR spent a section defending was true of one directory layout and false of the other. `--scope bundle <PATH>` restores it everywhere. **A flag is not a command**, which is why v1.2's sentence survives unamended beside it: no second binary entry point, no second port, no second UI shell, no second runtime — the same `serve_okf_only` the working-directory case already reached, now with two callers instead of one. What is new is a `reason` on its startup line, because "no repository here" is false of a server somebody deliberately pointed at a path, and a person reading a startup line is entitled to a true one. **The SPA stays off under this scope**, answering the question #810 left open: the argument v1.0 made for `/okf` — "a route that 404'd every request would be worse than an absent one" — applies with the sign reversed to an explorer app whose workspace list, search and project routes would all answer about nothing. An empty UI is not a smaller UI. Nothing about the viewer's model, its untrusted-input rules or its theme changes; this is the surface section and only the surface section. |
| 1.5 | 2026-09-14 | Amended (no issue; maintainer decision). **The mounted viewer's chrome caught up with the fold v1.2 performed.** The viewer has been served by the same binary, in the same process, on the same port as the explorer since v1.2, and nothing in its header said so: it printed `self.root.display()` — an absolute path under the operator's home directory — where the shell's project bar prints `Roteiro · Workspace ▸ <project>`, and it offered no control that walked back out to the workspace. A reader who arrived on it could reasonably conclude the viewer was a separate thing that happened to share a port. It now wears the shell's own bar: `assets/index.html`'s `p-back`, `p-crumbs`, `p-crumb-root`, `p-crumb-link`, `p-sep` and `p-crumb-current`, with the same declarations and the v1.3 tokens, and a trail of `Roteiro · Workspace ▸ OKF ▸ <label>` in which `OKF` is a link to the chooser when this server holds more than one bundle and a plain step when it does not — the condition `Nav::bundles` already encodes, because a one-row chooser redirects straight back. The breadcrumb *is* the way up, so the mounted form stops emitting the separate `Explorer` and `All bundles` links it used to: two controls for one destination is the second pattern this deliberately does not invent. The name in the trail is `Mount::label` verbatim, the `<workspace>/<project>` string `okf_mounts` had already resolved for the chooser and the startup line; nothing re-derives it. **The premise v1.2 nearly lost is the thing this change had to be careful with.** `serve_okf_only` passes `explorer: None` because it is the case where a bundle is a stranger's, at an arbitrary path, with no repository and no workspace anywhere above it. A breadcrumb reading `Roteiro · Workspace ▸ …` there names two things that do not exist, and a `← Workspace` control links to a route that 404s — so the standalone form gets neither, keeps v1.0's `name`/`root` header, and keeps its `All bundles` link, since a standalone server can still hold several bundles and its chooser is real. Both forms are pinned verbatim rather than by substring, because the risk here is precisely a later change that improves one and quietly gives the other a trail to nothing. **And the path stopped being published.** `serve` takes `--addr` and #832 added `--scope`, so a non-loopback bind is expressible in both forms; the header was the only place the absolute path reached the wire, and `no_mounted_page_publishes_the_bundle_path` walks every route a mounted bundle answers on — index, graph page, `api/graph.json`, a concept, a 404 — and asserts the bundle's real root appears in none of them. Standalone now shows the bundle directory's own name instead: the leading segments described the host rather than the bundle, and were of no use to a reader who was sent the URL. **One page still prints a path and is deliberately untouched**: the chooser lists `Mount::origin` beside each bundle, which is a v1.2 column whose stated job is telling two similarly-named bundles apart after `slug` folds them. Whether that is worth the same disclosure is a decision about the chooser rather than about the header, so it is stated rather than assumed — `the_chooser_still_names_each_bundles_directory` pins that it does, and the fixture that feeds it was corrected to carry a real path, because one saying `"test"` made the scan pass by having nothing to find. |

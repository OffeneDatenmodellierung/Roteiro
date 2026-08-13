---
Title: Explorer web app — vendored client-side JS (cytoscape.js) for the served UI
Space: ARCH
Parent: ADRs

# ADR-specific metadata (unknown keys are ignored; used for indexing/search)
type: adr
adr-id: "0010"
status: Accepted                    # Draft | For Review | Accepted | Rejected | Superseded
architectural-significance: MEDIUM  # SOFT | LOW | MEDIUM | HIGH | VERY HIGH
domain: Developer Tooling
decision-makers: ["The Roteiro Project Team"]
superseded-by:
version: "1.0"
last-modified: 2026-08-13
confluence-url:
---

# ADR-0010: Explorer web app — vendored client-side JS (cytoscape.js) for the served UI

| | |
|---|---|
| **State** | Accepted |
| **Architectural Significance** | MEDIUM |
| **Domain** | Developer Tooling |
| **Document version** | 1.0 |

## Reference

Puts a **served, interactive web UI** on the read-only graph API introduced by
[[docs/adr/0008-multi-repo-workspace-serve.md]] and the cross-repo views of
[[docs/adr/0009-cross-repo-workspace-links.md]]. The UI is mounted by the
llama-free `roteiro explorer` server ([[crates/roteiro/src/main.rs#run_explorer]])
beside the JSON data API ([[crates/roteiro/src/graph_api.rs#router]]), same-origin.
It consumes only that API's existing routes (`/v1/graph/workspaces`, `.../topology`,
`.../matrix`). It is deliberately distinct from the **script-free** static export
[[crates/roteiro/src/overview.rs#render_html]] (`links --matrix --html`), which
stays a single self-contained HTML file with no JavaScript.

## Summary

Introduce **client-side JavaScript** to Roteiro for the first time — a hand-written
ES app plus **one vendored third-party library, cytoscape.js** — to power the
interactive workspace explorer's topology graph. The library is committed to the
repo as a **single prebuilt UMD file** and embedded in the binary with
`include_str!`, served from `GET /vendor/cytoscape.min.js`. There is **no npm, no
bundler, and no build step**: `cargo build` remains the whole toolchain, and the
asset is served verbatim, same-origin, so there is no CORS surface and no external
network fetch at runtime.

## Context

The `roteiro explorer` server already serves the workspace graph as JSON
(ADR-0008/0009). The workspace view the maintainer signed off on is genuinely
interactive: a radial hub-and-spoke **topology** (a hub app with deployment
satellites, drift badges, gold/slate edges) plus a scrollable **config override
matrix**. Two facts make hand-rolled SVG the wrong tool:

- **A real graph library is warranted.** Laying out, hit-testing, hovering, and
  panning a node-link diagram — here up to ~1,300 nodes across a workspace — is
  exactly what a graph library does well and what bespoke SVG does badly. cytoscape.js
  is the mature, dependency-free, permissively licensed (MIT) choice.
- **The static export must stay script-free.** `links --matrix --html` is a file
  you can email or commit as an artifact; adding JS to it would break that promise.
  So the interactive app is a *separate* surface, served only by the live server.

The open question this ADR settles is **how** to bring in a client-side library
without importing the npm/bundler ecosystem — which would contradict the project's
pure-`cargo`, no-C/C++-toolchain posture for the `explorer` feature (axum + tokio
only, ADR-0008).

## Interview — clarify before writing

- [x] **What problem does this solve, and who has it?** Anyone running
  `roteiro explorer` who wants to *see* the cross-repo topology and overrides
  interactively, not just read JSON or a static table.
- [x] **Why a library at all?** Interactive graph layout/interaction of ~1,300
  nodes is not worth hand-rolling in SVG; cytoscape.js is purpose-built.
- [x] **Why vendored, not a CDN?** Same-origin only (no CORS, works offline/air-gapped,
  no third-party runtime dependency), and reproducible builds (the exact bytes are
  committed and reviewed).
- [x] **Why no build step?** The `explorer` feature is pure Rust (axum + tokio, no
  C/C++). A JS bundler would add a whole parallel toolchain for one library; a
  prebuilt UMD file needs none.
- [x] **What are the risks?** Carrying a ~365 KB minified vendored blob in git, and
  keeping it updated by hand (no `npm audit`). Mitigated by pinning the version,
  recording it here, and the library's small, stable surface.

## Decision makers

- The Roteiro Project Team

## Recommended option

**Vendor a single prebuilt cytoscape.js UMD file, embed it with `include_str!`, and
serve it same-origin from the `explorer` server — no npm, no build step.**

1. **Three embedded assets.** `crates/roteiro/src/assets/` holds `index.html` (the
   shell), `app.js` (our hand-written app), and `cytoscape.min.js` (the vendored
   library). All three are `include_str!`-embedded and served by
   [[crates/roteiro/src/explorer_app.rs#router]]: `GET /` (and `/explorer`) → the
   shell; `GET /app.js`; `GET /vendor/cytoscape.min.js`. Correct content-types,
   `&'static str` bodies.
2. **Served only by `explorer`.** The UI router is merged onto the data API in
   [[crates/roteiro/src/main.rs#run_explorer]] only. A full `serve` build keeps
   exposing just the JSON API — no bundled UI, no change to that surface.
3. **Same-origin, no CORS, no external fetch.** The app fetches `/v1/graph/*` from
   its own origin. Nothing is loaded from a CDN, so the explorer works offline and
   in air-gapped installs, and the served bytes are the reviewed bytes.
4. **No build step.** `cargo build` is the entire toolchain. The vendored file is a
   prebuilt UMD bundle served verbatim; there is no npm, package.json, lockfile, or
   bundler anywhere in the tree.
5. **The static export stays script-free.** [[crates/roteiro/src/overview.rs#render_html]]
   (`links --matrix --html`) remains a single self-contained, JavaScript-free file.
   The interactive app is a distinct surface with a distinct promise.
6. **Pinned and recorded.** The vendored library is cytoscape.js **v3.30.4** (MIT).
   Updating it is a deliberate, reviewed commit that replaces the single file and
   notes the new version here.

## Options considered + consequences

1. **Hand-rolled SVG, no library.** No new dependency, but re-implements graph
   layout/hit-testing/pan-zoom badly for ~1,300 nodes. Rejected — high effort, worse
   result.
2. **CDN `<script>` tag.** Zero bytes in git, but adds a third-party runtime
   dependency, breaks offline/air-gapped use, introduces a cross-origin fetch, and
   makes the served UI non-reproducible. Rejected.
3. **npm + bundler (vite/esbuild).** The conventional path, but imports a whole
   parallel toolchain (node, a lockfile, a bundler) for one library — contradicting
   the pure-`cargo`, no-extra-toolchain posture of the `explorer` feature. Rejected.
4. **Chosen — vendored single UMD file, `include_str!`, same-origin.** Pays one
   reviewed ~365 KB blob in git and a manual update cadence to get an interactive
   graph with no build step, no CORS, and reproducible served bytes.

## Consequences

- **New surface:** a `crates/roteiro/src/assets/` directory (HTML/JS/vendored lib),
  an [[crates/roteiro/src/explorer_app.rs#router]] mounted by `run_explorer`, and
  Roteiro's first client-side JavaScript.
- **Vendored dependency upkeep.** cytoscape.js is pinned (v3.30.4) and updated by a
  deliberate commit; there is no `npm audit`, so the version and rationale live here.
  Its MIT license header travels in the file.
- **Reproducible & offline.** The served UI is exactly the committed bytes, with no
  runtime network dependency — it works in air-gapped installs.
- **Feature isolation preserved.** The UI is gated on `explorer` and served only by
  the llama-free server; `serve`/llama builds are untouched, and the `explorer`
  feature still pulls only axum + tokio (no C/C++ toolchain, no `rto-serve`).
- **Scope seam.** This slice ships the cross-repo **workspace view** only. Clicking a
  repo box or a matrix column emits a navigation *intent*; the per-project drill-in,
  hub/spoke, follow-hop, and Ask tab are later PRs.

## Document version history

| Version | Date | Notes |
|---------|------|-------|
| 1.0 | 2026-08-13 | Accepted and implemented (PR 4, the workspace-view UI). Introduces Roteiro's first vendored client-side JavaScript: a hand-written ES app plus cytoscape.js **v3.30.4** (MIT) committed as a single prebuilt UMD file and `include_str!`-embedded, served same-origin by the `explorer` server ([[crates/roteiro/src/explorer_app.rs#router]], mounted in [[crates/roteiro/src/main.rs#run_explorer]]) at `GET /`, `/app.js`, `/vendor/cytoscape.min.js`. **No npm, no bundler, no build step; no CDN, no CORS, no runtime fetch.** Consumes only the existing read-only API of [[docs/adr/0008-multi-repo-workspace-serve.md]] / [[docs/adr/0009-cross-repo-workspace-links.md]] (`/v1/graph/workspaces`, `.../topology`, `.../matrix`) via [[crates/roteiro/src/graph_api.rs#router]]. The script-free static export [[crates/roteiro/src/overview.rs#render_html]] is explicitly kept JavaScript-free. Rejects hand-rolled SVG (re-implements a graph library badly), a CDN tag (third-party runtime dep, breaks offline, non-reproducible), and npm+bundler (a parallel toolchain for one library). |

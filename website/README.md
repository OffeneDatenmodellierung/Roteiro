# roteiro.dev

The docs site is a **build-output of the graph**: `roteiro render docs` renders
each ADR (and the Build Plan) with a real CommonMark parser and copies the static
theme from `website/public` into `website/dist`. `website/build.sh` wraps that
call (bootstrapping the pinned Rust toolchain if needed).

## Pages

The site is assembled from two kinds of source.

**`website/public/`** is the hand-written landing page and the static theme,
copied verbatim. Because it is copied rather than rendered, `index.html` carries
its own hand-written copy of the site navigation bar; the
`the_landing_page_carries_the_bar_the_renderer_emits` test renders this
repository and fails if that copy stops agreeing with the bar the renderer emits.

**Any markdown document whose frontmatter declares a `site-page:` slug** is
published as `<slug>.html`, listed in the navigation bar, and drift-checked by
`roteiro check` exactly like an ADR:

```markdown
---
site-page: modes        # the slug — publishes as modes.html; [a-z0-9-] only
site-nav: Modes         # short label for the bar (defaults to the page title)
site-order: 3           # position in the bar (unset sorts last, then by slug)
---
```

Publication is **declared, never inferred from a path**, so a document can gain a
public page *in place*: `docs/OFFLINE_SETUP.md`, `docs/history/BUILD_PLAN_V2.md` and
`docs/JSON_SCHEMA.md` are published where they already were, keeping every
existing link to their repository paths. The pages that were split out of the
landing page live in `website/pages/`.

A heading may carry an explicit anchor — `## Heading {#modes}` — which is how the
sections that moved off the landing page kept the URLs that predate the move.
Every other heading gets the slug of its own text.

## Deployment

Two paths exist; **exactly one** should be active to avoid double deploys.

### A. GitHub Actions → Cloudflare Direct Upload (recommended)

`.github/workflows/website.yml` renders the site with a **cached** Rust toolchain
on push to `main` and Direct-Uploads `website/dist` to Cloudflare Pages. This
keeps the (slow) Rust build off Cloudflare's build infra. The deploy job is
**inert until enabled**. To turn it on:

1. Add repo **secrets**: `CLOUDFLARE_API_TOKEN` (a Pages-edit token) and
   `CLOUDFLARE_ACCOUNT_ID`.
2. **Disable** the Cloudflare Pages *Git integration* build (below) so the site
   is not deployed twice.

### B. Cloudflare Pages Git integration (current)

Cloudflare watches this repo and builds on push using these project settings:

| Setting | Value |
|---|---|
| Production branch | `main` |
| Build command | `./website/build.sh` |
| Build output directory | `website/dist` |
| Root directory | `/` (repo root — the build script `cd`s itself) |

This compiles Rust on Cloudflare's build infra every deploy, which is slow; path
A moves that to cached CI. Preview deploys for PRs are created automatically.

Regardless of the active path, the `build` job runs on every PR as a guard, so a
broken `render docs` fails CI rather than the deploy.

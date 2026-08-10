# roteiro.dev

The docs site is a **build-output of the graph**: `roteiro render docs` renders
each ADR (and the Build Plan) with a real CommonMark parser and copies the static
theme from `website/public` into `website/dist`. `website/build.sh` wraps that
call (bootstrapping the pinned Rust toolchain if needed).

## Deployment

Two paths exist; **exactly one** should be active to avoid double deploys.

### A. GitHub Actions → Cloudflare Direct Upload (recommended)

`.github/workflows/website.yml` renders the site with a **cached** Rust toolchain
on push to `main` and Direct-Uploads `website/dist` to Cloudflare Pages. This
keeps the (slow) Rust build off Cloudflare's build infra. The deploy job is
**inert until enabled**. To turn it on:

1. Add repo **secrets**: `CLOUDFLARE_API_TOKEN` (a Pages-edit token) and
   `CLOUDFLARE_ACCOUNT_ID`.
2. **Pin** `cloudflare/wrangler-action` to a commit SHA in the workflow (repo
   convention; it is currently on a version tag with a `TODO`).
3. **Disable** the Cloudflare Pages *Git integration* build (below) so the site
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

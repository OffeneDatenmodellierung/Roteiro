# roteiro.dev

Deployed via **Cloudflare Pages' native GitHub integration** (not a GitHub
Actions workflow) — Cloudflare watches this repo directly and builds on push.

## Cloudflare Pages project settings

| Setting | Value |
|---|---|
| Production branch | `main` |
| Build command | `./website/build.sh` |
| Build output directory | `website/dist` |
| Root directory | `/` (repo root — the build script `cd`s itself) |

Preview deployments are created automatically for pull requests that touch
this repo; no extra config needed.

Once `roteiro render docs` exists, `build.sh` will be replaced by a call into
the `roteiro` CLI, so the site becomes a build output of the graph itself —
see ADR-0001.

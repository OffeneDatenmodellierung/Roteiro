# AGENTS.md — contributing to & reviewing Roteiro

Standards for **every** agent and human working on this repository — GitHub
Copilot code review, Claude Code, Cursor, cloud agents, and contributors. This
file is **tool-agnostic** and the **single source of truth**; vendor-specific
files (e.g. `.github/copilot-instructions.md`) reference it rather than duplicate
it. The review checklist lives in [`docs/REVIEW_CHECKLIST.md`](docs/REVIEW_CHECKLIST.md).

## Orient with the graph, not just grep

Roteiro maintains a provenance-tagged knowledge graph of itself. Prefer it when
orienting:

- `roteiro search "<text>"` — ranked text search; the offline **find-then-explain**
  entry point (curated ADRs/blueprints rank first). Then `query` a returned key.
- `roteiro query <key> --json` — a node and its provenance-labelled edges.
- `roteiro review [--json]` — a **graph-grounded review of your current change**:
  each touched symbol's callers/callees, the ADRs governing it, the drift and
  intent-debt it introduces, and the dependents to re-check.
- `roteiro path <a> <b>` · `roteiro debt` — connections and outstanding
  intent-debt.

For natural-language "what/why" questions, `roteiro serve --models` exposes the
same `search` (plus an OpenAI-compatible `/v1` endpoint); MCP agents get
`search`/`explain`/`path`/`debt` directly. The deeper operational guide (node
keys, when to use each tool, the plan/review flows) is the installable skill at
[`.agents/skills/roteiro/SKILL.md`](.agents/skills/roteiro/SKILL.md) — also
mirrored to `.github/skills/roteiro/` for the Copilot reviewer.

**Before finishing a change**, run `roteiro review` and `roteiro check`.

## Provenance invariants (the core model — never violate)

Every node and edge records *how it was produced*. This is the point of the
project; keep it exact.

- **Every edge is provenance-tagged** `derived` | `authored` | `inferred`. No
  unlabelled edges.
- **`inferred` edges carry a confidence score.** A fuzzy suggestion without a
  score is a bug.
- **`derived` extraction is a deterministic pure function** of `(path, git blob
  id, bytes)`. No clocks, no randomness, no ordering incidentals — sort emitted
  facts. Bump `EXTRACT_VERSION` when extraction *output* changes, so the
  content-addressed cache never serves stale facts.
- **Keep the three layers distinct.** Don't fabricate authored or derived facts,
  and don't silently promote an inferred guess to a hard fact.

## Code standards (the CI gates)

CI (`.github/workflows/ci.yml`) enforces these; run them locally before pushing.

- **MSRV 1.94**, edition 2024, `unsafe_code = "forbid"`.
- `cargo fmt --all --check` — clean.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` — clean
  (pedantic). Prefer fixing over `#[allow(...)]`; when an allow is right, justify
  it in a comment.
- `cargo test --workspace --all-features` — green.
- `cargo run -p roteiro -- check` — green. **CI dogfoods the drift gate on this
  repo**, so ADR `[[path#Symbol]]` links and `// @rto:` annotations must resolve.
- `cargo deny check` and `cargo audit` — clean. **Every new dependency's licence
  must be on the allow-list** — the exact SPDX ids in `deny.toml`: MIT,
  Apache-2.0, Apache-2.0 WITH LLVM-exception, BSD-2-Clause, BSD-3-Clause, ISC,
  Zlib, Unicode-3.0. Verify each grammar/model dep.
- **Offline by default.** The default build needs no network and no model. Keep
  heavy dependencies (llama.cpp, GGUF models, PDF/OCR/vision, the model server)
  behind **feature flags** so the default build stays small, and never touch the
  network without an explicit `[y/N]` consent.

## Pull requests

- **One concern per PR.** Reviewed and CI-green before merge.
- **Architecture is governed by ADRs** in `docs/adr/`. A change that alters an
  architectural decision updates or adds an ADR in the house style (frontmatter
  with `adr-id`, `## ` sections, `[[path#Symbol]]` links, a version-history row).
  See `docs/BUILD_PLAN.md` for the staged roadmap.
- Commit messages and PR descriptions explain the **why**, not just the what.

## Reviewing a change

Use [`docs/REVIEW_CHECKLIST.md`](docs/REVIEW_CHECKLIST.md), and run `roteiro
review` on the branch to ground the review in the graph (callers, governing
ADRs, blast radius) rather than reading the diff in isolation.

## Agent reviews & MCP (feasibility note)

`roteiro review` is the **CLI-first, tool-agnostic** review surface — it needs no
server and works in any agent or CI. Roteiro *also* ships an MCP server
(`roteiro serve --features mcp`) exposing `explain`/`path`/`debt`/`search`, which
a **local** agent (Claude Code, editors) can query during a review.

Wiring the **hosted** GitHub Copilot reviewer to Roteiro's MCP is **unverified**:
GitHub's docs confirm MCP servers are configurable in a repo's Copilot settings,
but do not establish that the hosted reviewer can reach a **self-hosted** MCP
endpoint. Treat MCP-for-review as a bonus for local/self-hosted agents; the CLI
(`roteiro review`) is the portable path. Revisit if GitHub documents self-hosted
MCP reachability.

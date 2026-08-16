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

For natural-language "what/why" questions, `roteiro serve` (the network HTTP
server) exposes the same `search` plus an OpenAI-compatible `/v1` endpoint and the
web UI; MCP agents run `roteiro mcp` and get `search`/`explain`/`path`/`debt`
directly. The deeper operational guide (node
keys, when to use each tool, the plan/review flows) is the installable skill at
[`.agents/skills/roteiro/SKILL.md`](.agents/skills/roteiro/SKILL.md) — also
mirrored to `.github/skills/roteiro/` for the Copilot reviewer.

**Before finishing a change**, run `roteiro review` and `roteiro check`.

### Never assert absence from `grep` alone

`grep` is good at confirming something *is* there and bad at proving it is not:
a negative result only tells you your pattern missed, and you cannot see what
you failed to guess. `roteiro search` ranks over names, keys, paths and captured
prose, so it finds the thing you did not know how to spell.

> **Worked example.** An audit of this repo concluded “no capacity/TTL/LRU
> eviction idiom exists anywhere” after grepping `evict|ttl|prune|capacity|max_`.
> `roteiro search evict` returned `lru_evict_count`, `ModelCache` and
> `tests::budget_evicts_oldest_until_it_fits` in `rto-llama` on the first hit.
> The false negative nearly led to designing a new eviction policy alongside the
> one that already existed.

So, whenever you are about to write *“there is no X”*, *“X does not exist yet”*
or *“this would be the first X”*:

1. Confirm it with `roteiro search` (try two or three vocabularies — the concept,
   the likely identifier, the likely test name), not with `grep` alone.
2. If you still find nothing, write **NOT FOUND with the queries you ran** rather
   than a flat “does not exist”. They are different claims, and only one of them
   is honest.
3. Cite the node keys you relied on, so the next reader can re-run your search
   instead of re-doing your investigation.

A wrong negative is more expensive than a missed positive: it silently
authorises building something the codebase already has.

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
- `cargo test --workspace --all-features` — green. **`--all-features` now
  includes `exec-boxlite`, which will not build until the sandbox runtime is
  provisioned** — deliberately, because otherwise boxlite's own build script
  downloads a 25 MB archive with no digest check of any kind and embeds it in
  the binary. Once, before your first `--all-features` build:

  ```sh
  # Prints the file:// URL, having verified the archive against the digest in
  # crates/rto-exec/src/runtime_pins.rs. This is what CI runs.
  export BOXLITE_RUNTIME_URL="$(python3 scripts/provision-sandbox-runtime.py)"
  ```

  Use the script, not `roteiro security prefetch`. The runtime is a *build*
  input, not an analyzer asset, and obtaining it through the binary is circular:
  a `roteiro` that could prefetch it is a `roteiro` you already built. The script
  reads its digests straight out of `runtime_pins.rs`, so it cannot drift from
  what the build script verifies a moment later.

  The build script fails loudly — with this recipe and the expected digest — if
  the variable is unset, points at a remote URL, or the bytes do not match. Build
  without `exec-boxlite` if you would rather not provision.
- `cargo run -p roteiro -- check` — green. **CI dogfoods the drift gate on this
  repo**, so ADR `[[path#Symbol]]` links and `// @rto:` annotations must resolve.
- `cargo deny --all-features check` and `cargo audit` — clean. **Every new
  dependency's licence must be on the allow-list** — the exact SPDX ids in
  `deny.toml`: MIT, Apache-2.0, Apache-2.0 WITH LLVM-exception, BSD-2-Clause,
  BSD-3-Clause, ISC, Zlib, Unicode-3.0, CDLA-Permissive-2.0. Verify each
  grammar/model dep. Note the `--all-features`: the gate must see what the
  project *ships*, not just the default build, and it costs under a second
  because cargo-deny resolves metadata rather than compiling (ADR-0017, #318).
  A licence outside the list is a **decision**, recorded next to its `deny.toml`
  entry — never an `ignore`, and never "to make CI green".
- **Dependency currency is a mechanism, not a habit** (ADR-0017). Dependabot
  proposes updates weekly under a **minimum release age — at least 48 hours, 3
  days as configured** — so a
  compromised publish has time to be caught before Roteiro depends on it. Native
  and vendored code (llama.cpp, SQLite, tree-sitter) is outside `cargo audit`'s
  reach and is tracked by name in
  [`docs/VENDORED_DEPENDENCIES.md`](docs/VENDORED_DEPENDENCIES.md); a change that
  vendors non-Rust code adds its row in the same PR.
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
- **`!` is a release instruction, not a severity marker.** A `!` after the type
  (`fix(schema)!:`) or a `BREAKING CHANGE:` footer tells release-plz to bump the
  **major version of all seven crates** — they share one
  `[workspace.package] version`, so one `!` in one crate relabels the whole
  workspace, and crates with nothing breaking in them get the new major anyway.
  Use it only when something a consumer of a **published** crate depends on has
  changed: a public Rust API, a CLI flag or its output contract, a config key, or
  an on-disk format some released version actually wrote.

  Do **not** use it for unreleased work. A migration renumbered before it ever
  shipped, an internal type, a test helper, a field no released binary has
  written — none of these are breaking, however invasive the diff looks. The
  question is not "was this a big change?" but "can a user who upgrades notice?"

  Getting this wrong is expensive and cannot be undone by closing the release PR:
  release-plz computes the bump from every unreleased commit relative to the
  **registry**, so a stray `!` re-proposes the major bump on every push until a
  release is actually published. Recovering means pinning the intended version by
  hand. If you are unsure, leave the `!` off and say what changed in the body —
  an under-marked commit is a changelog omission, an over-marked one is a version
  the project cannot walk back.

## Reviewing a change

Use [`docs/REVIEW_CHECKLIST.md`](docs/REVIEW_CHECKLIST.md), and run `roteiro
review` on the branch to ground the review in the graph (callers, governing
ADRs, blast radius) rather than reading the diff in isolation.

## Agent reviews & MCP (feasibility note)

`roteiro review` is the **CLI-first, tool-agnostic** review surface — it needs no
server and works in any agent or CI. Roteiro *also* ships an MCP server
(`roteiro mcp`, built `--features mcp`) exposing `explain`/`path`/`debt`/`search`,
which a **local** agent (Claude Code, editors) can query during a review.

Wiring the **hosted** GitHub Copilot reviewer to Roteiro's MCP is **unverified**:
GitHub's docs confirm MCP servers are configurable in a repo's Copilot settings,
but do not establish that the hosted reviewer can reach a **self-hosted** MCP
endpoint. Treat MCP-for-review as a bonus for local/self-hosted agents; the CLI
(`roteiro review`) is the portable path. Revisit if GitHub documents self-hosted
MCP reachability.

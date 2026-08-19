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
web UI; MCP agents run `roteiro mcp` and get
`search`/`explain`/`context`/`check`/`path`/`debt` directly — every one of them
read-only, and `check` returning a `gate` of `not-run` rather than a clean
verdict when it cannot see the repository or a current graph. The deeper operational guide (node
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
- `cargo test --workspace --all-features` — green. **`--all-features` includes
  `exec-boxlite`.** That builds without a pre-step now: `boxlite` fetches the
  runtime archive over TLS and `rto-exec`'s build script verifies **every
  extracted file** against the per-file digests in `src/runtime_file_pins.rs`
  before anything links, failing on a mismatch, a missing file or an unpinned
  extra one. You still need one pass of your own:

  ```sh
  # The analyzer image. It needs a binary that *has* `exec-boxlite`, so it
  # cannot be folded into anything earlier.
  cargo run -p roteiro --features exec-boxlite -- \
    security prefetch --analyzer semgrep --allow-download   # ~435 MB, pinned by digest
  ```

  **That pass is easy to miss.** The *image* half of `prefetch` is behind
  `#[cfg(feature = "exec-boxlite")]`, so a binary without it compiles the image
  step out entirely. Skip it and `cargo test --workspace --all-features` fails in
  `backend_parity` with `ImageNotProvisioned`.

  **For a build with no network at all**, provision the archive and name it. Then
  `boxlite`'s `curl` reads a local file, and the bytes are verified before they
  are extracted as well as after:

  ```sh
  # The archive belongs to no analyzer, so `--analyzer sandbox` selects it alone
  # rather than also fetching ~260 MB of advisory databases you may not want yet.
  roteiro security prefetch --analyzer sandbox --allow-download   # pinned digest
  export BOXLITE_RUNTIME_URL="file://$HOME/.roteiro/security/boxlite-runtime/boxlite-runtime.tar.gz"
  ```

  `prefetch` is gated on `execution`, a default feature, so **any** build can run
  that — including `--no-default-features --features execution`. Both paths are
  verified; only this one is offline, and neither should be described as the
  other (ADR-0014 v1.3).

  **If you bump the `boxlite` pin**, re-derive the per-file digests rather than
  editing them: `scripts/derive-runtime-file-pins.py` (and `--check` to confirm
  the checked-in file is current). It covers all three pinned targets, not just
  your host's.
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
- **Offline by default.** The default build needs no model and makes no network
  call of its own. It does ship `models` and `exec-subprocess`, because
  `roteiro model pull` and `roteiro security prefetch` are the prerequisites for
  *preparing* to work offline and must exist in a stock install. So "offline by
  default" is a claim about **behaviour**, not about the absence of an HTTP
  client: the socket is compiled in, and only `pull` and
  `prefetch --allow-download`, each after an explicit consent, may open it.
  Likewise `exec-subprocess` compiles in the *capability* to run an analyzer as a
  child process; `--allow-unsandboxed` is what permits an actual run, it is
  required every time, and since the build-time gate is no longer in the default
  path it is the only gate left — do not weaken it. Keep heavy dependencies
  (llama.cpp, PDF/OCR/vision, the model server, the sandbox runtime) behind
  **feature flags** so the default build stays small and needs no toolchain class
  it does not already require.

## Pull requests

- **One concern per PR.** Reviewed and CI-green before merge.
- **Architecture is governed by ADRs** in `docs/adr/`. A change that alters an
  architectural decision updates or adds an ADR in the house style (frontmatter
  with `adr-id`, `## ` sections, `[[path#Symbol]]` links, a version-history row).
  See `docs/BUILD_PLAN.md` for the staged roadmap.
- Commit messages and PR descriptions explain the **why**, not just the what.
- **`!` is a release instruction, not a severity marker.** A `!` after the type
  (`fix(schema)!:`) or a `BREAKING CHANGE:` footer tells release-plz to bump the
  **major version of all nine crates** — they share one
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

- **The `rto-*` crates are published, and are implementation details.** All eight
  of them publish to crates.io because they have to: `crates/roteiro/Cargo.toml`
  depends on them by version, and crates.io rejects a package with path-only
  dependencies — so `cargo install roteiro` does not work unless they all ship.
  Publishing them satisfies the registry. It is not an offer of a stable API.

  So treat their public surface as **internal**. A change that is technically
  breaking for an `rto-*` crate — a field's type, an enum variant, a function
  signature — ships as a **minor** bump and does **not** take a `!`. This is the
  standing answer to *"but this is breaking for a published crate"*: it usually
  is, and it does not matter. The only reverse dependency on crates.io is
  `roteiro` itself, and each version is downloaded 14–26 times, which is CI,
  docs.rs and mirrors rather than users.

  **This does not extend to `roteiro`.** Its CLI flags and their output
  contracts, its config keys, and any on-disk format a released version wrote are
  what people actually depend on, and a change to those is breaking in the
  ordinary way. The distinction is *who could notice*, which is the same test the
  bullet above applies.

  Say what changed in the commit body regardless. The posture removes the
  escalation, not the record — and it is a claim about intent rather than a
  technical guarantee, which is why it is stated where someone can read it before
  depending on one of these crates rather than after.

- **A scratch verification branch needs its own commit.** Some CI behaviour can
  only be observed from a branch with a particular *name*: `ci.yml` skips the
  expensive work on release PRs behind `startsWith(github.head_ref,
  'release-plz-')`, and that predicate is unreachable from any other branch. The
  technique is to push a temporary branch matching it, open a **draft** PR, read
  what the required contexts report, then close the PR and delete the branch.

  The trap is that **check runs attach to a commit SHA, not to a branch.** Push
  the scratch branch at the same commit as the branch under test and both PRs'
  runs land on one SHA, where branch protection reads whichever reported first.
  This has already happened: on #490 the required `msrv` context went green from
  the scratch branch's 21-second cheap-path run while the branch's own full run
  was still `in_progress`, and the PR merged on it. The diff was one YAML file so
  nothing was actually at risk — but the green did not mean what it looked like,
  and it would not always be a YAML file.

  So give the scratch branch its own commit; an empty one is enough. Delete it
  when you are done — and if `git push --delete` is refused by the harness's
  blast-radius policy, **say so in your report** rather than leaving the branch
  stranded for someone else to find.

## Reviewing a change

Use [`docs/REVIEW_CHECKLIST.md`](docs/REVIEW_CHECKLIST.md), and run `roteiro
review` on the branch to ground the review in the graph (callers, governing
ADRs, blast radius) rather than reading the diff in isolation.

## Agent reviews & MCP (feasibility note)

`roteiro review` is the **CLI-first, tool-agnostic** review surface — it needs no
server and works in any agent or CI. Roteiro *also* ships an MCP server
(`roteiro mcp`, built `--features mcp`) exposing
`explain`/`search`/`context`/`check`/`path`/`debt` (but **not** `review` — it is
CLI-first; see `rto_render::mcp`'s module documentation for why),
which a **local** agent (Claude Code, editors) can query during a review.

Wiring the **hosted** GitHub Copilot reviewer to Roteiro's MCP is **unverified**:
GitHub's docs confirm MCP servers are configurable in a repo's Copilot settings,
but do not establish that the hosted reviewer can reach a **self-hosted** MCP
endpoint. Treat MCP-for-review as a bonus for local/self-hosted agents; the CLI
(`roteiro review`) is the portable path. Revisit if GitHub documents self-hosted
MCP reachability.

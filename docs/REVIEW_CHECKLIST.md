<!-- roteiro:ignore-file — this checklist enumerates the intent-debt vocabulary, so it must not catalogue itself. -->

# Review checklist

A tool-agnostic checklist for reviewing a change to Roteiro — usable by any agent
(Copilot, Claude Code, Cursor, …) or a human. It mirrors the standards in
[`AGENTS.md`](../AGENTS.md), the single source of truth. Run `roteiro review` on
the branch first to ground the review in the graph (callers, governing ADRs,
blast radius) rather than the diff alone.

## Provenance (the core model)

- [ ] Every new/changed edge is provenance-tagged `derived` | `authored` |
      `inferred` — no unlabelled edges.
- [ ] Every `inferred` edge carries a **confidence** score.
- [ ] `derived` extraction stays a **deterministic pure function** of `(path,
      blob id, bytes)` — emitted facts sorted; no clock/random/order
      dependence. `EXTRACT_VERSION` bumped if extraction output changed.
- [ ] The three layers stay distinct — no fabricated authored/derived facts, no
      silent promotion of an inferred guess.

## Gates (run locally; CI enforces)

- [ ] `cargo fmt --all --check` clean.
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings`
      clean; any `#[allow(...)]` is justified in a comment.
- [ ] `cargo test --workspace --all-features` green; new behaviour has a test.
      `--all-features` includes `exec-boxlite`, whose build refuses until the
      sandbox runtime is provisioned and pinned — see `AGENTS.md` for the
      one-time `security prefetch` + `BOXLITE_RUNTIME_URL` recipe. **Both
      passes**: the archive, then the image, from an `exec-boxlite` build.
- [ ] **A gate verified on your machine was verified with your machine's state.**
      Three defects in one day were of this shape: a documented command that no
      longer does what the doc says, merged because the person checking had more
      state than the recipe produces — an image pulled during development, a
      binary built with a feature the recipe cannot have yet. Before claiming a
      setup recipe works, run it as written from the state a fresh machine has.
      `ROTEIRO_SECURITY_ASSETS=$(mktemp -d)` gives you that for anything asset-
      backed without touching your real cache. If a test needs state the recipe
      does not create, it must **skip visibly and name the exact command**, the
      way `dependency_axis.rs` does for the OSV database — never fail as though
      the code were broken, and never skip silently.
- [ ] `cargo run -p roteiro -- check` green — ADR `[[…]]` links and `// @rto:`
      annotations resolve (CI dogfoods this).
- [ ] `cargo deny --all-features check` + `cargo audit` clean; any new
      dependency's licence is on the allow-list — and if it is a *new* licence,
      it is admitted with its reasoning recorded beside the `deny.toml` entry,
      not added to turn CI green (ADR-0017).
- [ ] A new `[advisories] ignore` entry states all four of: why it is tolerable,
      how the crate enters the tree, what would trigger a revisit, and whether
      the rationale is feature-scoped — which `cargo deny` cannot enforce, so it
      must say so (ADR-0017).
- [ ] A dependency that vendors or links non-Rust code has a row in
      [`VENDORED_DEPENDENCIES.md`](VENDORED_DEPENDENCIES.md) — `cargo audit`
      cannot see it.

## Design & scope

- [ ] **Offline by default** preserved — no new required network/model in the
      default build; heavy deps are feature-gated; no un-consented network.
- [ ] **MSRV 1.96** and `unsafe_code = "forbid"` respected.
- [ ] Architectural changes are reflected in an **ADR** (house style), and
      authored links stay consistent (`roteiro review` shows no new drift).
- [ ] **One concern** per PR; the commit/PR explains the *why*.
- [ ] Docs/`AGENTS.md`/website updated if the change affects usage or standards.

## Refusals

Roteiro refuses a great deal, deliberately: an uncompiled feature, an
uninstalled model, an unprovisioned asset, a missing third-party binary, a
consent gate that was never granted. Each of those is a moment where the next
thing a person does is decided entirely by the sentence they have just read.

- [ ] **The refusal names the way forward, not only the obstacle.** "`X` is not
      installed" is half a message; `roteiro model pull X` is the other half.
      A reader should never have to search the docs to act on an error.
- [ ] **It names the *right kind* of way forward.** A missing **feature** wants
      a rebuild line; a missing **model** wants a pull; a missing **asset**
      wants a prefetch; a missing **third-party binary** wants an install
      command or its upstream page. Telling someone to recompile when they only
      needed to pull a model is a wrong answer that reads like a right one, and
      it costs an hour before they doubt it. That has shipped here once.
- [ ] **It names the alternative, where one exists.** `security run` cannot
      proceed without the analyzer on `PATH` — but `security ingest` accepts a
      normalized report produced anywhere, including CI. A refusal that hides
      the escape hatch costs someone a capability they already had.
- [ ] **It does not guess the reader's platform.** A canonical ecosystem
      command (`cargo install …`, `pip install …`) is portable and checkable; a
      package-manager guess is wrong for most readers and rots for the rest.
      Where no such command exists, the upstream install page is the durable
      answer, and a URL ages better than a command line.
- [ ] **Naming the way forward is not permission to take it.** A refusal that
      quietly does the thing instead — falls back to the host, to a default
      model, to an unpinned fetch — is the silent downgrade
      [[docs/adr/0019-remote-model-tier.md]] §6 and
      [[docs/adr/0020-build-capable-sandboxed-execution.md]] §6 both forbid.
      Say how; do not do it.

## Triaging an automated reviewer's comments

Automated review is worth having — over twelve PRs in one day, *every* comment
GitHub Copilot left was adjudicated, and **22 of them were real defects that were
accepted and fixed**. Not one was caught by CI or by the author's own
verification, because every one of them *passed*. They were contract-accuracy
defects: code that worked but did not mean what it said.

Those adjudications, plus any added since, live in
[`crates/rto-graph/tests/fixtures/review/`](../crates/rto-graph/tests/fixtures/review/README.md).
Take current counts from that fixture's class table, which a test holds to the
data; the figures quoted here describe the original twelve-PR sample and are not
updated as rows are added.

They are also **measurable rather than anecdotal**: `roteiro review --score
<run.json>` scores any candidate reviewer against that corpus, reporting recall
**per defect class**. Two things it will not do, because the corpus cannot support
them — report an averaged recall (which hides the only actionable fact, *which*
classes a reviewer sees), or call an unmatched finding a false positive (the corpus
records what one reviewer said about those trees, not every defect in them).

But adjudicate before acting, and one rule pays for itself:

- [ ] **A comment claiming the code will not compile is refuted by the CI
      `msrv` job at that commit, not by an investigation.** In that sample every
      false positive was a compile-error claim (a move out of a borrow, three
      times), and *every* compile-error claim was a false positive. A fourth has
      since joined them on #352 — `Duration::from_mins` called "not on MSRV
      1.94", though it is stable since 1.91.0 — so the class now stands at
      **4 for 4**, with no real defect in it. Record a newly adjudicated comment
      in the corpus linked above. `msrv` is
      `cargo check --workspace --all-features` and finishes in about 40 seconds;
      in each case it had already gone green **at the commit the comment was
      left on**, roughly a minute before the comment was posted. So the
      refutation exists before anyone reads the comment: check that job, reply,
      move on. Do not dispatch work to "fix" it.
- [ ] **But green refutes only what it compiled.** Confirm the relevant job
      actually ran at that sha *and* that it covers the configuration the comment
      is about. Four axes, each one a way this CI is narrower than "the build":
      - **Platform.** Every compiling job runs on `ubuntu-latest`, so **nothing
        here compiles `cfg(target_os = "macos")` code**, of which this repo has a
        good deal (Metal, the engine teardown path, the sandbox backend). The
        `GGML_ASSERT` teardown abort was macOS-only and Ubuntu CI was
        structurally blind to it.
      - **Features.** `msrv` and `checks` are `--all-features`;
        `default-features` is the default set. **Neither covers the other** —
        turning features on cannot find a defect in code being cfg'd *out*,
        which is why that job exists. Since #667 a third job,
        `no-default-features`, builds the floor and two combinations above it
        (`--no-default-features`, `+ mcp`, `+ execution`), so that axis **is**
        covered now — but only in those three shapes. Any other feature
        combination is still built by nothing.
      - **Targets.** `msrv` is `cargo check --workspace --all-features` with **no
        `--all-targets`**, so it never compiles `#[cfg(test)]` modules or
        `tests/` targets. The jobs that do compile test code (`checks`,
        `default-features`) run on **stable**. So a claim that *test* code will
        not build **on MSRV 1.96** is refuted by no job in this repository.
      - **Toolchain.** Only `msrv` is on 1.96. A green `stable` build says
        nothing about an MSRV claim.

      If no green job covers the configuration, the claim is unrefuted and you
      owe it a real look. `rto_graph::compile_claim` is this rule as code — the
      coverage model, the four axes, and every case above as a test — so a
      reviewer applying it mechanically and a human applying it by eye reach the
      same answer.
- [ ] Every other class in that sample was real. Treat a non-compile finding as
      probably correct and verify it against the code, rather than assuming the
      reviewer is noisy.
- [ ] When a reviewer's cited line or mechanism is wrong but the underlying
      point is right, **say so in the reply** and fix the substance. A reviewer
      that is silently overruled teaches no one; one whose finding is confirmed
      and sharpened tells the next reader where to look.
- [ ] A fix that only makes the comment go away is not a fix. Prefer removing
      the cause: several of these were one symptom of a rule stated in two
      places that had drifted apart.

## Graph-grounded (from `roteiro review`)

- [ ] Callers of changed symbols still hold (no silently broken contracts).
- [ ] ADRs **governing** a changed symbol remain accurate; update the ADR if the
      decision changed.
- [ ] The **blast radius** (impacted dependents) has been considered.
- [ ] New **intent-debt** (TODO/FIXME/stub/deferred) is intentional and tracked.

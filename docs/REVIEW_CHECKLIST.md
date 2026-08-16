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
      one-time `security prefetch` + `BOXLITE_RUNTIME_URL` recipe.
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
- [ ] **MSRV 1.94** and `unsafe_code = "forbid"` respected.
- [ ] Architectural changes are reflected in an **ADR** (house style), and
      authored links stay consistent (`roteiro review` shows no new drift).
- [ ] **One concern** per PR; the commit/PR explains the *why*.
- [ ] Docs/`AGENTS.md`/website updated if the change affects usage or standards.

## Triaging an automated reviewer's comments

Automated review is worth having — on one measured sample of 25 comments from
GitHub Copilot across 12 PRs, **22 were real defects that were accepted and
fixed**, and not one of them was caught by CI or by the author's own
verification, because every one of them *passed*. They were contract-accuracy
defects: code that worked but did not mean what it said.

But adjudicate before acting, and one rule pays for itself:

- [ ] **A comment claiming the code will not compile is refuted by the CI
      `msrv` job at that commit, not by an investigation.** In that sample every
      false positive was a compile-error claim (a move out of a borrow, three
      times), and *every* compile-error claim was a false positive — 3 for 3,
      with no real defect in that class. `msrv` is
      `cargo check --workspace --all-features` and finishes in about 40 seconds;
      in each case it had already gone green **at the commit the comment was
      left on**, roughly a minute before the comment was posted. So the
      refutation exists before anyone reads the comment: check that job, reply,
      move on. Do not dispatch work to "fix" it.
- [ ] **But green refutes only what it compiled.** `msrv` and `checks` both run
      on `ubuntu-latest` with the pinned MSRV toolchain, over
      `--all-features`. So a green run says nothing about a
      `--no-default-features` build, a different toolchain, or — the one that
      matters most here — **code behind `cfg(target_os = "macos")`**, which this
      repo has a good deal of (Metal, the engine teardown path, the sandbox
      backend). The `GGML_ASSERT` teardown abort was macOS-only and Ubuntu CI
      was structurally blind to it. So: confirm the relevant job actually ran at
      that sha, and that it covers the configuration the comment is about. If it
      does not, the claim is unrefuted and you owe it a real look.
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

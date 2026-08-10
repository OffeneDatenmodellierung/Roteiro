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
- [ ] `cargo run -p roteiro -- check` green — ADR `[[…]]` links and `// @rto:`
      annotations resolve (CI dogfoods this).
- [ ] `cargo deny check` + `cargo audit` clean; any new dependency's licence is
      on the allow-list.

## Design & scope

- [ ] **Offline by default** preserved — no new required network/model in the
      default build; heavy deps are feature-gated; no un-consented network.
- [ ] **MSRV 1.94** and `unsafe_code = "forbid"` respected.
- [ ] Architectural changes are reflected in an **ADR** (house style), and
      authored links stay consistent (`roteiro review` shows no new drift).
- [ ] **One concern** per PR; the commit/PR explains the *why*.
- [ ] Docs/`AGENTS.md`/website updated if the change affects usage or standards.

## Graph-grounded (from `roteiro review`)

- [ ] Callers of changed symbols still hold (no silently broken contracts).
- [ ] ADRs **governing** a changed symbol remain accurate; update the ADR if the
      decision changed.
- [ ] The **blast radius** (impacted dependents) has been considered.
- [ ] New **intent-debt** (TODO/FIXME/stub/deferred) is intentional and tracked.

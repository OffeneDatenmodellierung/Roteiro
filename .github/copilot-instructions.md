# Copilot instructions

The contribution and review standards for this repository live in
[`AGENTS.md`](../AGENTS.md) (single source of truth) and the review checklist in
[`docs/REVIEW_CHECKLIST.md`](../docs/REVIEW_CHECKLIST.md). Follow those.

Key points when suggesting or reviewing changes:

- **Provenance invariants**: every edge is `derived` | `authored` | `inferred`
  (no unlabelled edges); `inferred` edges carry a confidence score; `derived`
  extraction is a deterministic pure function of `(path, blob id, bytes)`.
- **Gates** (CI enforces): `cargo fmt --all --check`; `cargo clippy --workspace
  --all-targets --all-features -- -D warnings` (pedantic); `cargo test
  --workspace --all-features`; `cargo run -p roteiro -- check`; `cargo deny
  check` + `cargo audit`. MSRV **1.94**, `unsafe_code = "forbid"`.
- **Offline by default**: keep heavy deps feature-gated; no un-consented network;
  new dependency licences must be on the `cargo deny` allow-list.
- **One concern per PR**; architectural changes carry an ADR (`docs/adr/`).

Ground reviews in the graph: `roteiro review` reports each changed symbol's
callers/callees, governing ADRs, drift, and blast radius.

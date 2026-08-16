---
Title: Sandboxed analyzer execution — an owned seam, ingest by default, boxlite opt-in
Space: ARCH
Parent: ADRs

# ADR-specific metadata (unknown keys are ignored; used for indexing/search)
type: adr
adr-id: "0014"
status: For Review                  # Draft | For Review | Accepted | Rejected | Superseded
architectural-significance: HIGH    # SOFT | LOW | MEDIUM | HIGH | VERY HIGH
domain: Security Tooling
decision-makers: ["The Roteiro Project Team"]
superseded-by:
version: "1.1"
last-modified: 2026-08-15
confluence-url:
---

# ADR-0014: Sandboxed analyzer execution — an owned seam, ingest by default, boxlite opt-in

| | |
|---|---|
| **State** | For Review |
| **Architectural Significance** | HIGH |
| **Domain** | Security Tooling |
| **Document version** | 1.1 |

## Reference

Decides **how** external analyzers (`cargo-audit`, `semgrep`, successors) are
*executed* — the isolation boundary, the provisioning of their inputs, and the
degradation behaviour when offline. Its sibling
[[docs/adr/0012-analyzer-findings-artifact-model.md]] decides how the **results**
are stored; the two are deliberately separable, and this ADR is the optional half.
Constrained by the offline-first and dependency-light principles of
[[docs/adr/0001-build-roteiro-unified-codebase-knowledge-graph.md]], and follows
the optional-capability precedent of
[[docs/adr/0003-pluggable-embedding-models.md]] and
[[docs/adr/0006-local-model-serving.md]].

## Summary

- Roteiro owns a small **execution seam** (`AnalyzerRunner`) with three
  interchangeable backends behind one normalized result contract.
- **Ingest is the default and always available**: `roteiro security ingest`
  consumes a normalized report produced anywhere (CI, a developer's own tooling).
  Zero install, zero isolation surface, no new dependency.
- **`boxlite` is the opt-in local backend** — Apache-2.0, OCI containers in Linux
  microVMs, `Hypervisor.framework` + libkrun on Apple Silicon, KVM on Linux.
- **Subprocess is the explicit escape hatch**, requiring `--allow-unsandboxed` and
  labelling its evidence `isolation=none`.
- **Assets are pre-downloaded and digest-pinned**, never fetched implicitly. Cold
  cache with no network fails with a named, actionable error.
- **`code_interpreter` remains a non-goal.** The availability of a sandbox must
  not silently convert that decision.

## Context

Roteiro is a `cargo install`-able, offline-first Rust binary with
`unsafe_code = "forbid"` and an Ubuntu-only `--all-features` CI. Running third-party
analyzers raises a genuine question about what boundary, if any, they need.

**The security argument is weaker than it first appears.** `cargo-audit` and
`semgrep` *parse* source, manifests and lockfiles — they do not execute the
analyzed project. A read-only worktree, a scrubbed environment, no ambient
credentials, pinned rules and a pinned advisory database capture most of the real
benefit. A VM boundary only becomes load-bearing when the payload is
*intentionally* arbitrary code, which is `code_interpreter` — explicitly a
non-goal.

**The reproducibility and offline arguments are the strong ones.** A pinned OCI
image with a pinned analyzer version, pinned rules and a pinned advisory DB gives
one command that produces the same findings on any machine, and — critically for
"mostly offline" working — produces them **on a plane**, with digest-level evidence
of exactly what was run. That, not security theatre, is why an owned backend earns
its place.

**The platform premise changed.** Earlier reasoning that macOS offers only
Seatbelt with no local VM story is now **outdated**: boxlite runs Linux microVMs on
Apple Silicon via `Hypervisor.framework` + libkrun, and Apple's own `container`
CLI exists (macOS 26, Apple Silicon).

**Packaging was verified, not assumed.** An earlier investigation asserted boxlite
was unpublished and therefore unmergeable — a git dependency would indeed break
`cargo package`, `cargo install roteiro` and release-plz. Direct checking of the
crates.io API refuted it: `boxlite` is published, 17 versions, default **0.9.7**,
not yanked, with docs.rs and sparse-index both responding. It is an ordinary
registry dependency. *This ADR records the check because the false blocker nearly
cost a design.*

## Decision makers

The Roteiro Project Team.

## Recommended option

### The seam

A new `rto-exec` crate behind an `execution` feature (subfeatures `exec-boxlite`,
`exec-subprocess`), exposing an `AnalyzerRunner` trait. A request names the
analyzer, a **read-only worktree**, `network: Deny`, and explicit user consent; a
response returns normalized findings plus the digest evidence
[[docs/adr/0012-analyzer-findings-artifact-model.md]] records.

| Runner | Availability | Isolation label | Notes |
|---|---|---|---|
| **Ingest** | Always, no feature | `ingested` | Consumes a normalized report; the zero-install default. |
| **boxlite** | `exec-boxlite` | `microvm` | Digest-pinned OCI image; the reproducible local path. |
| **Subprocess** | `exec-subprocess` | `none` | Requires `--allow-unsandboxed`; evidence is labelled honestly. |

Because all three satisfy one contract, CI ingestion and local sandboxed execution
stop being competing architectures and become **the same code path**.

### Why boxlite, and why not the alternatives

**NVIDIA OpenShell** was assessed seriously and rejected — not on platform or GPU
grounds, both of which it passes (arm64 macOS supported; `--gpu` optional and
experimental; its MicroVM driver uses libkrun on `Hypervisor.framework`, the same
mechanism as boxlite). It fails on **embeddability**: it is a CLI plus a local
gateway service and compute drivers, with no Rust bindings found. Roteiro would
shell out to a service rather than link a library, and would inherit a gateway,
driver layer and policy control plane — plus alpha maturity (self-described
"proof-of-life", v0.0.52). Too much weight for an optional feature of a small
binary.

**A Roteiro-owned sandbox** is rejected outright: platform-specific isolation code
is exactly the kind of subsystem this project should not maintain.

**boxlite** is chosen for embeddability: a daemonless, Apache-2.0 Rust library that
links in, with the microVM boundary as a library concern rather than an operational
one.

### Maturity: accepted deliberately, with duties

boxlite is pre-1.0 (v0.9.7), young, and had **two critical advisories fixed in
0.9.0**; downloads are in the low thousands. The project owner has accepted
pre-1.0 status explicitly. What that acceptance *entails* is recorded here so it is
a decision rather than a drift:

- **Pin exactly** (`=0.9.7`-style), never a floating range.
- **Track upstream advisories deliberately.** The failure mode of a security
  boundary is not that it breaks loudly but that it silently stops being a
  boundary.
- **Run `cargo deny` over the fully resolved tree**, which is native/FFI-heavy;
  licence and advisory review of the transitive closure is a gate, not a
  formality.
- Accept that Roteiro is an **early operator** of this code, not a follower.

### Provisioning and degradation ("mostly offline")

The working model is *mostly offline, degrade gracefully, pre-download expected* —
so this is a provisioning contract, not a purity argument. It mirrors the existing
model-pull UX (`roteiro model list/pull`), which already discloses source, licence
and size, requires consent, and verifies hashes atomically:

- **`roteiro security prefetch`** — fetch and verify all pinned assets by digest:
  OCI image, analyzer versions, rule sets, advisory DB. **"Fetch" is what the
  asset needs, not a promise that every asset is downloaded**: as Stage 22
  shipped it, `prefetch` verifies and pins but fetches nothing, because the rule
  set is vendored into the binary and the `RustSec` advisory database is a git
  checkout with no digest-stable URL. Shelling out to `git` to obtain the latter
  would be the host-tool fallback forbidden two bullets below, so it is refused
  with the exact clone command instead. The obligations that matter — pinned
  before use, never implicit, never a host fallback — are unaffected. See
  [[docs/adr/0018-analyzer-coverage-matrix.md]]; the first genuinely downloadable
  asset arrives with `osv-scanner` in Stage 22b.
- **`roteiro security status`** — report each digest, fetch time, and advisory-DB
  age.
- **Cold cache, no network** — fail with a distinct `assets-unavailable-offline`
  error naming the missing digests and the exact prefetch command. **Never**
  silently fall back to host tools; **never** fetch implicitly.
- **Cached but stale advisory DB** — still run, but stamp results with
  `advisory_db_published_at`, `fetched_at` and age, and label them *possibly
  stale*, never *current*.

An optional feature that pulls images or refreshes advisory databases must not be
described as "offline"; it is **offline-capable once provisioned**, and the docs
must say so in those words.

### CI implications

CI is Ubuntu-only with `--all-features`, so `exec-boxlite` must not make
`--all-features` fail on a runner without `/dev/kvm`. Sandbox-requiring tests are
gated on a runtime capability probe and skipped with a visible message; the ingest
and subprocess paths carry the functional coverage. **Apple Silicon microVM
execution is untested in CI** — an accepted, documented gap.

## Options considered + consequences

| Option | Verdict |
|---|---|
| Ingest only (seam (c)) | **Kept as the default**, but insufficient alone — no one-command local run, no digest-level reproducibility. |
| **Ingest + optional boxlite (chosen)** | Zero-install default preserved; reproducible, offline-capable local path for those who opt in. |
| NVIDIA OpenShell | **Rejected** — not embeddable (service + gateway, no Rust bindings), heavyweight, alpha. |
| Apple `container` CLI | **Rejected** — macOS-only and a CLI dependency; noted as evidence the platform premise changed. |
| Roteiro-owned sandbox | **Rejected** — platform isolation code is not this project's business. |
| boxlite-backed `code_interpreter` | **Rejected / out of scope** — a separate product decision, not a backend swap. |

## Consequences

**Positive**

- One analyzer contract serves CI ingestion and local execution.
- Findings become reproducible and evidence-bearing, and work offline once
  provisioned.
- Default install is unchanged: no new dependency unless the feature is enabled.
- The rejected options are recorded, so the platform question does not get
  re-litigated from stale premises.

**Negative / costs**

- A native/FFI-heavy dependency closure to own, on a young upstream, with a
  standing duty to track its advisories.
- A `cargo deny` gate over a much larger resolved tree.
- An untested-in-CI platform (Apple Silicon microVM).
- Feature-matrix complexity: three runners, two subfeatures, and a capability
  probe.

## Status

For Review. Sequenced in [BUILD_PLAN_V2](../BUILD_PLAN_V2.md): the seam and ingest
land in Stage 21 (no boxlite), analyzers in Stage 22, and the boxlite backend in
Stage 24 — which, since publication was verified, is a dependency addition rather
than a packaging problem.

## Version history

| Version | Date | Change |
|---|---|---|
| 1.0 | 2026-08-15 | Initial: the seam, the three backends, boxlite chosen, the provisioning and degradation contract. |
| 1.1 | 2026-08-15 | Clarified what `prefetch` "fetches": as Stage 22 shipped it, it verifies and pins but downloads nothing, because neither shipped asset is a digest-stable download. The pinned-before-use, never-implicit and no-host-fallback obligations are unchanged. See [[docs/adr/0018-analyzer-coverage-matrix.md]]. |

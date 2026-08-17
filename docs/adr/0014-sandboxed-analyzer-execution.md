---
Title: Sandboxed analyzer execution — an owned seam, ingest by default, boxlite opt-in
Space: ARCH
Parent: ADRs

# ADR-specific metadata (unknown keys are ignored; used for indexing/search)
type: adr
adr-id: "0014"
status: Accepted                    # Draft | For Review | Accepted | Rejected | Superseded
architectural-significance: HIGH    # SOFT | LOW | MEDIUM | HIGH | VERY HIGH
domain: Security Tooling
decision-makers: ["The Roteiro Project Team"]
superseded-by:
version: "1.4"
last-modified: 2026-08-17
confluence-url:
---

# ADR-0014: Sandboxed analyzer execution — an owned seam, ingest by default, boxlite opt-in

| | |
|---|---|
| **State** | Accepted |
| **Architectural Significance** | HIGH |
| **Domain** | Security Tooling |
| **Document version** | 1.4 |

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
  labelling its evidence `isolation=none`. *(Update, v1.2: `exec-subprocess` is now
  a default feature, so the build-time half of that gate is gone from a stock
  install and `--allow-unsandboxed` carries it alone. It is unchanged and must
  stay so — see v1.2 below.)*
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
| **Subprocess** | `exec-subprocess` — **in the default set since v1.2** | `none` | Requires `--allow-unsandboxed` on every invocation; evidence is labelled honestly. |

Asset **provisioning** (`security prefetch|status`) is not in this table because it
is not a runner: it downloads, digests, pins and reports, and executes nothing. As
of v1.2 it sits on `execution` alongside `ingest`/`list` rather than behind a
backend feature — which is also what makes the boxlite bootstrap non-circular,
since `exec-boxlite`'s build script demands the verified runtime archive that
`prefetch` is what obtains.

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

### The sandbox runtime: verified where it enters the artifact

`boxlite` compiled from crates.io does not build a hypervisor. Its own build
script fetches a prebuilt runtime tarball with a bare `curl -fsSL`, extracts it,
and `include_bytes!`s **the extracted files** into the rlib. That fetch verifies
nothing. This is the largest single trust decision in the feature, so it is
recorded here rather than left to a build script's comments.

**What is verified, and where.** The extracted files, against per-file SHA-256
digests, in `rto-exec`'s build script, before anything links. A digest mismatch,
a missing pinned file, or an unpinned extra file stops the build — the last of
those because `boxlite` embeds *every* regular file in that directory, so an
extra file there is an extra file in the binary. The pins are **derived** from
the pinned archives by `scripts/derive-runtime-file-pins.py`, which verifies each
archive against its own digest before opening it; nobody hand-writes one.

**Why not before the download.** Because Roteiro cannot get in front of it.
`boxlite`'s build script reads `BOXLITE_RUNTIME_URL` from its own environment,
and cargo runs it *before* `rto-exec`'s — a build script cannot set an
environment variable for a dependency's build script. Verifying an archive and
assuming `boxlite` consumed it was the previous arrangement; it checked a file
the build was never obliged to use.

**The trade, which is real.** Verification moved from before the download to
after extraction. On the default path a malicious archive is therefore unpacked
on the build machine before anything inspects it, and the check only ever
inspects the runtime directory — a member escaping it (`../`, symlink traversal)
is outside what these digests speak for. Both `tar` implementations in play
refuse such members by default and the transport is TLS to a pinned release URL,
so the window is narrow; it is not zero, and anyone asking "why check *after*?"
is entitled to find this paragraph.

**Consent and disclosure.** Enabling `exec-boxlite` — directly or via
`--all-features` — **is** the consent for that fetch, on the same terms as every
other optional capability here. The disclosure rides `cargo:warning=`, which
cargo displays for a dependency's build script on success, on both `cargo build`
and `cargo install` (verified on 1.97.1; a plain `eprintln!` is **not** shown, so
the choice of channel is load-bearing). The build says that it fetched, from
where, that the extracted files were verified and how many, that GPL-2.0 and
LGPL-2.0 binaries are being embedded, and what to run for a build that touches
no network.

**The no-egress path stays.** `BOXLITE_RUNTIME_URL` pointing at a `file://` copy
provisioned by `roteiro security prefetch --analyzer sandbox --allow-download`
verifies the archive *before* extraction as well as after, and `boxlite`'s `curl`
then opens no socket. That is what an air-gapped or egress-controlled build
should use, and CI uses it. Neither path may be described as the other: the
default is **verified but not offline**; the strict path is both.

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

**Accepted** (2026-08-17), and implemented — Stages 21, 22 and 24 (#293, #322, #352), the backend released in **v1.13.0**. Sequenced in [BUILD_PLAN_V2](../BUILD_PLAN_V2.md) and delivered in that order:
the seam and ingest in Stage 21 (no boxlite), analyzers in Stage 22, and the
boxlite backend in Stage 24 — which, publication having been verified, was a
dependency addition rather than the packaging problem it was first reported to
be.

## Version history

| Version | Date | Change |
|---|---|---|
| 1.4 | 2026-08-17 | **Accepted.** No content changed. Status corrected: this ADR described shipped, released behaviour while still reading *For Review*. |
| 1.0 | 2026-08-15 | Initial: the seam, the three backends, boxlite chosen, the provisioning and degradation contract. |
| 1.1 | 2026-08-15 | Clarified what `prefetch` "fetches": as Stage 22 shipped it, it verifies and pins but downloads nothing, because neither shipped asset is a digest-stable download. The pinned-before-use, never-implicit and no-host-fallback obligations are unchanged. See [[docs/adr/0018-analyzer-coverage-matrix.md]]. |
| 1.2 | 2026-08-16 | **`exec-subprocess` joins the default feature set, and provisioning leaves it.** Two changes with one motive — a stock install should be able to prepare itself for offline work. (a) `security prefetch\|status` move from `exec-subprocess` to `execution`: they execute nothing (every `Command::new` in `rto-exec` is in `subprocess.rs`/`boxlite.rs`), the asset module was already shared between backends and owned by neither, and gating provisioning on a backend made the boxlite bootstrap circular. (b) `exec-subprocess` becomes a default, so `security run` ships in a stock install. **This retires half of v1.0's justification and the remaining half must not be weakened.** v1.0 defended the subprocess backend as "asked for at build time **as well as** consented to per run"; the build-time half no longer applies to a default install. What remains — and is unchanged, deliberately — is that `--allow-unsandboxed` is required on **every** invocation, that the run records `isolation=none`, that a cold asset cache refuses rather than fetching, and that Roteiro never installs the analyzer, so an operator has already chosen to have `semgrep`/`osv-scanner` on `PATH`. The flag is now the only gate; do not soften it for consistency with the build-time one that went away. `--no-default-features --features execution` remains a build that provisions and ingests but cannot execute. |
| 1.3 | 2026-08-17 | **The sandbox runtime is verified where it enters the artifact: the extracted files, not the archive.** `boxlite`'s own build script is what fetches, it reads `BOXLITE_RUNTIME_URL` from its own environment, and cargo runs it before `rto-exec`'s — so requiring that variable could never have been what kept the fetch honest, only what kept the build from completing. Measured: with the build script patched to proceed and the variable unset, the build succeeds and `boxlite` embeds 58.4 MB fetched over the network while the verified local archive is never opened. `rto-exec/build.rs` now verifies every file in `DEP_BOXLITE_RUNTIME_DIR` against per-file digests derived from the pinned archives (`scripts/derive-runtime-file-pins.py`), refusing a mismatch, a missing file, an unpinned extra file, or a runtime that was never extracted. **The guarantee is unchanged in strength and now attaches to the bytes that are actually built in** — but it is taken after extraction rather than before the download, and that trade, including the residual path-traversal exposure it opens on the default path, is stated in full above rather than left implicit. Enabling `exec-boxlite` (or `--all-features`) is the consent for the fetch; the disclosure rides `cargo:warning=`, which cargo shows for a dependency's build script on success under both `cargo build` and `cargo install`. `BOXLITE_RUNTIME_URL` remains the strict, no-egress path and is what CI uses. |

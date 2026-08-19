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
version: "1.7"
last-modified: 2026-08-19
confluence-url:
---

# ADR-0014: Sandboxed analyzer execution — an owned seam, ingest by default, boxlite opt-in

| | |
|---|---|
| **State** | Accepted |
| **Architectural Significance** | HIGH |
| **Domain** | Security Tooling |
| **Document version** | 1.7 |

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

### The cache is reused, and dropped on demand — never on a schedule

The asset cache exists to be **reused**. Provisioning a sandbox is expensive —
2.9 GB of image and runtime on the machine this was written on — and a boundary
that costs minutes on every run is a boundary people switch off. Reuse is the
feature that makes the default in [[docs/adr/0020-build-capable-sandboxed-execution.md]] §6
survivable, not an optimisation on top of it.

But reuse with no way out is a trap, and today there is no way out. `prefetch`
obtains and `status` reports; **nothing removes**. Someone who wants a clean
build, or who distrusts a cached layer, or who simply wants the disk back, has
`rm -rf` and a guess about which directory. So provisioning gains its third verb
— `clear` alongside `prefetch` and `status` — governed by three rules.

**It is a command, not a setting.** There is no config key for it, deliberately.
A key that drops a cache is a *standing instruction to throw work away*, and it
would fire when nobody was looking; the entire value of the verb is that it
happens at a moment a person chose. This is the distinction
[[docs/adr/0013-agent-memory-artifact-store.md]] already draws for the memory
cache tier — eviction is a **maintenance act, not a preference** — and it is why
this key is absent from the classification in
[[docs/adr/0007-configuration-file.md]] rather than being a value in it.

**It is safe by construction, not by care.** Everything under the asset cache is
re-obtainable from a pinned digest, so clearing costs time and never
information: ADR-0013's *re-derivable ⇒ evictable*. That property is what makes a
destructive verb acceptable at all, and it is therefore also its **limit** — the
verb may never reach anything that is not re-obtainable. A findings layer is not
re-obtainable; neither is a memory record. `clear` does not touch the store.

**And what may be shared is decided per artifact, not once.** The word "shared"
hides two different things:

- **Content-addressed and verified → shareable across repositories.** A pinned
  image, a runtime archive, a package cache whose entries carry checksums the
  toolchain verifies. A poisoned entry fails verification rather than executing,
  so sharing costs nothing.
- **Not content-addressed, and holding executables → per repository.** A build
  scratch directory holds *compiled build scripts*. Sharing one across
  repositories would let a build script from one repository leave something a
  build in another picks up — which is the execution boundary this ADR exists
  to draw, defeated through a cache rather than through a mount. Cheap to get
  right up front and very hard to notice once wrong.

Which package caches actually satisfy the first test is per-ecosystem and is
**not settled here**: Cargo verifies registry checksums, npm records integrity
hashes, and other ecosystems are weaker. An ecosystem whose cache cannot be
verified gets the second treatment, not the benefit of the doubt.

### `clear` on the MCP surface, and the line it crosses

The MCP tools are read-only, and `security run` was refused partly for mutating.
`clear` mutates too, and is nevertheless offered — which is a line worth crossing
deliberately rather than by extension.

The rule the read-only stance was really protecting is that **a model must not
change what the graph says**. `clear` changes nothing the graph says. It makes
the next run slower and that is the whole of its effect, because everything it
touches is re-obtainable by digest. `security run` is genuinely different: it
writes a findings layer, which *is* a change to what Roteiro reports about your
code.

So the boundary for a mutating MCP tool is stated positively rather than as an
exception: **a tool may drop state that is re-obtainable from a pinned digest,
and may drop nothing else.** That is the same test that makes the CLI verb safe,
applied unchanged, which is what stops it becoming a precedent for a second
mutating tool that is not safe for the same reason.

Two obligations follow. The tool **reports what it freed**, so the cost appears
in the transcript rather than being discovered later as an unexplained re-pull.
And it is **not offered a scope it cannot justify** — clearing an analyzer's
assets and clearing everything are different requests and should be different
arguments, so a model asking for one cannot receive the other.

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
| 1.0 | 2026-08-15 | Initial: the seam, the three backends, boxlite chosen, the provisioning and degradation contract. |
| 1.1 | 2026-08-15 | Clarified what `prefetch` "fetches": as Stage 22 shipped it, it verifies and pins but downloads nothing, because neither shipped asset is a digest-stable download. The pinned-before-use, never-implicit and no-host-fallback obligations are unchanged. See [[docs/adr/0018-analyzer-coverage-matrix.md]]. |
| 1.2 | 2026-08-16 | **`exec-subprocess` joins the default feature set, and provisioning leaves it.** Two changes with one motive — a stock install should be able to prepare itself for offline work. (a) `security prefetch\|status` move from `exec-subprocess` to `execution`: they execute nothing (every `Command::new` in `rto-exec` is in `subprocess.rs`/`boxlite.rs`), the asset module was already shared between backends and owned by neither, and gating provisioning on a backend made the boxlite bootstrap circular. (b) `exec-subprocess` becomes a default, so `security run` ships in a stock install. **This retires half of v1.0's justification and the remaining half must not be weakened.** v1.0 defended the subprocess backend as "asked for at build time **as well as** consented to per run"; the build-time half no longer applies to a default install. What remains — and is unchanged, deliberately — is that `--allow-unsandboxed` is required on **every** invocation, that the run records `isolation=none`, that a cold asset cache refuses rather than fetching, and that Roteiro never installs the analyzer, so an operator has already chosen to have `semgrep`/`osv-scanner` on `PATH`. The flag is now the only gate; do not soften it for consistency with the build-time one that went away. `--no-default-features --features execution` remains a build that provisions and ingests but cannot execute. |
| 1.3 | 2026-08-17 | **The sandbox runtime is verified where it enters the artifact: the extracted files, not the archive.** `boxlite`'s own build script is what fetches, it reads `BOXLITE_RUNTIME_URL` from its own environment, and cargo runs it before `rto-exec`'s — so requiring that variable could never have been what kept the fetch honest, only what kept the build from completing. Measured: with the build script patched to proceed and the variable unset, the build succeeds and `boxlite` embeds 58.4 MB fetched over the network while the verified local archive is never opened. `rto-exec/build.rs` now verifies every file in `DEP_BOXLITE_RUNTIME_DIR` against per-file digests derived from the pinned archives (`scripts/derive-runtime-file-pins.py`), refusing a mismatch, a missing file, an unpinned extra file, or a runtime that was never extracted. **The guarantee is unchanged in strength and now attaches to the bytes that are actually built in** — but it is taken after extraction rather than before the download, and that trade, including the residual path-traversal exposure it opens on the default path, is stated in full above rather than left implicit. Enabling `exec-boxlite` (or `--all-features`) is the consent for the fetch; the disclosure rides `cargo:warning=`, which cargo shows for a dependency's build script on success under both `cargo build` and `cargo install`. `BOXLITE_RUNTIME_URL` remains the strict, no-egress path and is what CI uses. |
| 1.4 | 2026-08-17 | **Accepted.** No content changed. Status corrected: this ADR described shipped, released behaviour while still reading *For Review*. |
| 1.5 | 2026-08-18 | **The sandboxed backend becomes reachable, and becomes the default path of `security run`.** Since Stage 24 `BoxliteRunner` was built, tested (`crates/rto-exec/tests/backend_parity.rs`) and specified here, but `run_security_run` hard-coded `SubprocessRunner` — the isolation boundary this ADR exists to provide could not be asked for from the CLI at all. `security run` now selects the sandbox when **no** flag is given; `--allow-unsandboxed` is what selects the host, and it selects it outright. Three obligations follow and are tested: (a) **no fallback, in either direction.** There is deliberately no input meaning "sandbox, or the host if that fails" — a missing feature, an unpulled image, an unprovisioned asset or an absent hypervisor is a named refusal naming the fix, never a quiet host run, because `RunnerKind`/`isolation` on the stored layer would then be a false statement about how those findings were produced (ADR-0019 §6). (b) **`--allow-unsandboxed` is untouched.** v1.2's warning holds without amendment: it is still required per invocation for the host path, still records `isolation=none`, and the sandbox existing is not a reason to imply or retire it. (c) **`exec-boxlite` stays off by default and the CLI surface does not move with it.** `run` and both flags parse in every build; only the capability is conditional, and it refuses in a sentence that names the feature and the four-step bootstrap. Gating the clap variant instead is how `roteiro model rm` shipped invisible to crates.io users, and is not repeated here. The human line describing isolation is now read back out of the stored `AnalysisRun` rather than from the calling function, so the sentence a user reads and the row `security list` returns cannot disagree. |
| 1.6 | 2026-08-19 | **Provisioning gains its third verb.** `prefetch` obtains and `status` reports; nothing removed, so a user wanting a clean build or the disk back had `rm -rf` and a guess — against 2.9 GB of cached image and runtime. Records that the cache exists to be **reused**, because a boundary costing minutes per run is one people switch off, and that `clear` is a **command and never a config key**: a setting that drops a cache is a standing instruction to throw work away that fires when nobody is looking, where ADR-0013 already holds eviction to be a maintenance act rather than a preference. Its safety and its limit are the same property — everything under the asset cache is re-obtainable from a pinned digest, so `clear` costs time and never information, and may therefore never reach the store. Distinguishes **content-addressed, verified** artifacts (shareable across repositories) from a **build scratch holding compiled build scripts** (per repository, because sharing one would defeat this ADR's execution boundary through a cache rather than through a mount); which package caches qualify is per-ecosystem and deliberately unsettled. Finally, admits `clear` to the MCP surface as its **first mutating tool**, with the permission stated positively so it does not become a precedent by extension: a tool may drop state re-obtainable from a pinned digest and nothing else, must report what it freed, and must not be given a scope wider than the request. |
| 1.7 | 2026-08-19 | **v1.2's `--no-default-features --features execution` claim becomes a checked one.** It was written in v1.2 and never compiled: by issue #445 that configuration had two compile errors and five `-D warnings` rejections, every one an artefact of *exclusion* — items whose only callers are cfg'd out, and a call site that never saw its callee's signature change. `--all-features` cannot find that class by construction, which is #360's lesson one configuration over. No decision here changes; what changes is that the sentence is now enforced by the `no-default-features` CI job rather than asserted. **If that job is ever removed, remove the claim in the same change** — a documented posture nobody compiles is how the last one rotted. |

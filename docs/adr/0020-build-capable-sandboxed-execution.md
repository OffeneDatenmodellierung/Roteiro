---
Title: Build-capable sandboxed execution — running the repository's own build, and the non-goal it narrows
Space: ARCH
Parent: ADRs

# ADR-specific metadata (unknown keys are ignored; used for indexing/search)
type: adr
adr-id: "0020"
status: Draft                       # Draft | For Review | Accepted | Rejected | Superseded
architectural-significance: VERY HIGH  # SOFT | LOW | MEDIUM | HIGH | VERY HIGH
domain: Developer Tooling
decision-makers: ["The Roteiro Project Team"]
superseded-by:
version: "1.0"
last-modified: 2026-08-18
confluence-url:
---

# ADR-0020: Build-capable sandboxed execution — running the repository's own build, and the non-goal it narrows

| | |
|---|---|
| **Document version** | 1.0 |
| **Status** | Draft |
| **Decision makers** | The Roteiro Project Team |
| **Amends** | [[docs/adr/0014-sandboxed-analyzer-execution.md]] |
| **Related** | [[docs/adr/0012-analyzer-findings-artifact-model.md]] · [[docs/adr/0017-dependency-security-policy.md]] · [[docs/adr/0019-remote-model-tier.md]] |

## Reference

- [[docs/adr/0014-sandboxed-analyzer-execution.md]] — the ADR this one amends
- [[docs/adr/0012-analyzer-findings-artifact-model.md]] — the store a build's findings would land in
- [[docs/adr/0017-dependency-security-policy.md]] — the pinning and cooldown discipline a build's inputs must not escape
- [[docs/adr/0019-remote-model-tier.md]] — the precedent for amending a promise in a new ADR rather than editing it away

## Summary

Roteiro **may compile the repository under review inside the sandbox**, so that
builder-class analyzers — `clippy`, `tsc`, `mypy` — can run at all.

ADR-0014 forbids this, in a sentence written before anyone had measured what a
builder does. This ADR overturns it **deliberately and narrowly**, because
ADR-0014 itself warns that *"the availability of a sandbox must not silently
convert"* the decision. A reversal reached one reasonable-looking commit at a
time is the failure it names; this document is the alternative.

Three things are amended, and the amendments are the substance:

1. **ADR-0014's "the security argument is weaker than it first appears" is
   scoped** to parse-only analyzers. It is false for builders, and ADR-0014's own
   subject proves it.
2. **The `code_interpreter` non-goal is narrowed, not withdrawn.** Executing the
   repository's own declared build becomes a goal. Executing code that Roteiro or
   a model *authored* stays a non-goal, and this ADR does not weaken it.
3. **The read-only worktree invariant is relaxed for one runner, not removed.**
   The preflight that enforces it keeps refusing every other request.

And one thing is decided that looks like an engineering detail and is not: **a
build is not an analysis, and the findings store cannot currently tell two builds
apart.** That gap survives fixing everything else here, and it — not the
sandboxing — is the hard part.

## Context

### What ADR-0014 says, and where it is wrong

> **The security argument is weaker than it first appears.** `cargo-audit` and
> `semgrep` *parse* source, manifests and lockfiles — they do not execute the
> analyzed project. […] A VM boundary only becomes load-bearing when the payload
> is *intentionally* arbitrary code, which is `code_interpreter` — explicitly a
> non-goal.

That is sound for the three analyzers ADR-0014 shipped and false for every
builder. `cargo clippy` has `cargo check` semantics: it executes every build
script in the resolved tree and loads every proc macro as a dylib into the
compiler process. Measured against this repository's own lockfile:

| feature set | crates | build scripts | proc macros |
|---|---:|---:|---:|
| default | 355 | **54** | 7 |
| `--all-features` | 672 | **87** | 33 |

So `cargo clippy --all-features` here is roughly 120 units of arbitrary code
executing on the host, with the invoking user's filesystem, SSH keys and registry
credentials.

The sharpest instance is ADR-0014's own subject. `boxlite-0.9.7/build.rs` shells
out to `curl` and embeds what comes back; `crates/rto-exec/build.rs` describes it
as *"a bare `curl -fsSL` […] That fetch verifies nothing"*, which is why this
project pins the runtime itself. **The ADR that dismissed the boundary depends on
a build script that demonstrates why the boundary matters.**

### The threat model inverts, and improves

Under ADR-0014 the sandbox guards against a **malicious analyzer** — a weak
threat, since the analyzers are pinned, reputable and parse-only. That is why the
ADR could honestly call the security case theatre and rest its argument on
reproducibility instead.

A builder inverts it: the sandbox guards against a **malicious repository**.
Reviewing an outside contributor's pull request by running `clippy` on it means
executing that contributor's build scripts and proc macros on your machine.
That is a real, common, and currently unmitigated exposure, and it is a
*stronger* justification than the one ADR-0014 was able to give.

### Why "narrow the non-goal" rather than withdraw it

`code_interpreter` in its original sense means *execute code a model wrote*. What
a builder needs is *execute the build the repository already declares*. Those are
different, and only the second is required here:

- The code executed is **already in the tree**, authored by its committers,
  and would run on the host the moment anyone typed `cargo build`.
- Roteiro contributes **no code** to what runs. It supplies an image, a mount and
  an argv.
- The user is not asking for a scratchpad. They are asking that a build they were
  going to run anyway run **behind a boundary instead of on their laptop**.

Withdrawing the non-goal entirely would license a scratchpad by implication.
Narrowing it keeps the guard that matters while unblocking the capability that is
wanted. **If a scratchpad is ever wanted, it needs its own decision, not this
one.**

## Decision makers

The Roteiro Project Team.

## Recommended option

Adopt build-capable execution as a **distinct runner class**, subject to five
conditions. Each is stated as a requirement because each is a way this decision
turns into the failure ADR-0014 predicted.

### 1. The read-only invariant is relaxed for builders only

`check_request` refuses any request whose worktree is not read-only, and a test
pins it. That preflight **stays**, and keeps refusing reader-class requests. A
builder gets a writable *build directory* — ideally an overlay or a separate
mount — and the source tree stays read-only wherever the toolchain permits.
A blanket relaxation of the preflight is the conversion ADR-0014 warns about.

### 2. Egress stays denied; inputs are mounted, not fetched

The investigation that produced this ADR rejected a config-declared trusted-URL
allowlist, and that rejection stands. A guest-side dependency fetch brings in
bytes **that are then executed** by the build scripts counted above, and it would
be the first install path in the system not terminating in a digest check.
Dependencies reach the guest by a host-produced, read-only mount — for Cargo,
`cargo vendor`, measured at 414 MB / 556 MB / 1.10 GB for this repository, and
regenerated per lockfile change rather than per commit.

### 3. The boundary is recorded, never inferred

`RunnerKind` and `Isolation` already exist to say how a run was produced. A
builder must record which it used, and a build that could not obtain its boundary
must **fail with a named error rather than fall back to the host**. A silent
downgrade here writes a false provenance into the findings store.

### 4. The store must be able to tell two builds apart — and today it cannot

This is the condition most likely to be skipped, and it is the one that survives
fixing everything else.

For every analyzer shipped so far, the thing deciding the answer is a **pinned
asset with a digest** — semgrep's rules, the RustSec checkout, the OSV databases
— stamped into `AnalysisRun.rules_digest`. **For a builder the rule set is the
toolchain, and there is no asset to digest.** The layer key renders
`<prefix>:<analyzer>:<worktree-id>` and nothing else, with `analyzer_version` in
neither the finding key nor the layer key, and the column is `UNIQUE`.

Consequently two builds of one commit that differ in **toolchain version** or in
**feature set** collide on one layer key and **silently replace each other**, and
the replacement reports the displaced findings as removed — which reads as
*fixed*. No amount of sandboxing repairs that; it is a schema question and it
must be answered before builder findings are stored.

### 5. Lint identity is documented, not engineered around

An advisory id is *assigned*, and assignment is a promise. A lint name is a
symbol in a compiler: renamed, removed, or moved between groups at the
compiler's discretion. A renamed lint reads as one defect fixed and one
introduced; a removed lint reads as fixed; an edit to `[workspace.lints]` makes
whole cohorts appear or vanish, so **a configuration change reads as a code
change**.

These readings are inherent and must be surfaced where a user meets them.
Builder findings must **not** be wired into the cross-analyzer join, whose
correctness rests on both upstreams publishing identifiers. Nobody publishes lint
names; they are release notes.

## Consequences

**This is a larger product surface than an analyzer backend.** A writable
build directory brings resource limits, timeouts, artifact retention, cleanup and
abuse considerations that a parse-only run never had. The current guest is 2 vCPU
/ 4096 MiB with a 30-minute execution ceiling, against a `--all-features` build
of this repository that its own manifest measures at *"realistically 3–12 minutes
on 2–4 cores"* — so the ceiling is real, not theoretical.

**The reproducibility argument does not transfer, and must not be reused.** For
readers it is the strong argument: pinned image, pinned rules, pinned advisory
DB, same findings anywhere. For builders the answer depends on the toolchain, the
feature set and the resolved dependency tree, none of which a rules digest pins.
Condition 4 exists because of this.

**`--all-features` on this repository cannot be built under a denied network
today**, because a dependency's build script fetches at build time. The
provisioning contract must reach inside the guest, or that feature set stays out
of scope. This circularity is the sandbox's own build needing the sandbox's own
prefetch, and it is already recorded in ADR-0014.

**Parity weakens.** The current backend-parity test asserts that one analyzer
produces identical findings via subprocess and via microVM. That holds for
readers. It cannot hold for a builder, whose toolchain differs between host and
image — and the test would stay green while defending a claim the feature no
longer makes. It must be re-scoped when this lands.

**And one thing does not change.** Ingest remains the zero-install default. A
`cargo clippy --message-format=json` produced in CI and ingested requires no
sandbox, no vendoring and no egress, records `Isolation::Ingested` honestly, and
is available today. **Nothing in this ADR should make ingest look like the
lesser path**; for most users it will remain the right one.

## Status

**Draft.** The decision is taken — build-capable execution is a goal, and the
`code_interpreter` non-goal is narrowed rather than withdrawn. What is not yet
established is condition 4: the findings store cannot currently represent two
builds of one commit distinctly, and that is a schema question with no proposed
answer at the time of writing.

This ADR should not move to `Accepted` until condition 4 has one, because
accepting it earlier would authorise storing findings the store cannot describe
— which is the defect [[docs/adr/0012-analyzer-findings-artifact-model.md]] exists to
prevent.

## Document version history

| Version | Date | Notes |
|---------|------|-------|
| 1.0 | 2026-08-18 | Initial draft. Narrows ADR-0014's `code_interpreter` non-goal to exclude only model-authored code, scopes its "security argument is weaker" reasoning to parse-only analyzers, and relaxes the read-only worktree invariant for a builder runner only. Records the measured build-script and proc-macro counts that make the case, the inverted threat model, and the five conditions — of which condition 4, the store's inability to distinguish two builds of one commit, is unresolved and blocks acceptance. |

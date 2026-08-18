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
version: "1.2"
last-modified: 2026-08-18
confluence-url:
---

# ADR-0020: Build-capable sandboxed execution — running the repository's own build, and the non-goal it narrows

| | |
|---|---|
| **Document version** | 1.2 |
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

And one thing is decided that looks like an engineering detail and is not:
**builder output is not stored.** A build is a point-in-time assessment of the
code in front of you, produced for the person who asked. It is reported and
discarded, not filed as a fact about the repository.

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

### 4. Builder output is ephemeral — it is not stored

A builder reports to the caller and writes nothing to the findings store. No
layer, no replacement, no history.

This is the condition that makes the rest of the decision tractable, and it
follows from what a lint *is* rather than from convenience. An advisory id is
**assigned**, and assignment is a promise: `RUSTSEC-2020-0071` will mean the same
thing in five years. A lint name is a **symbol in a compiler** — renamed,
removed, or moved between groups at the compiler's discretion, with the old name
surviving only as a deprecation alias. The first is a durable fact about the
repository and earns a place in a store. The second is a tool's opinion about the
code as it stands today, for the person who asked.

Storing the second is what produced every identity problem this decision ran
into. The layer key renders `<prefix>:<analyzer>:<worktree-id>` and nothing else,
with `analyzer_version` in neither the finding key nor the layer key, and the
column is `UNIQUE` — so two builds of one commit differing in toolchain version
or feature set would collide, silently replace each other, and report the
displaced findings as *removed*, which reads as **fixed**. For every stored
analyzer the thing deciding the answer is a pinned asset with a digest; **for a
builder the rule set is the toolchain, and there is no asset to digest.**

**Not storing is the fix, not a workaround.** It removes the collision, the false
"fixed", and the need to key a layer by a toolchain — none of which were problems
worth solving, because the thing being stored did not belong there.

What is given up, stated plainly rather than discovered later: no trend line over
lint debt, no ingesting a CI clippy run to query later, and no cross-analyzer
join with security findings. The first is largely a measurement of toolchain
churn rather than of code; the third was already forbidden by condition 5.

### 5. Lint identity is documented, not engineered around

An advisory id is *assigned*, and assignment is a promise. A lint name is a
symbol in a compiler: renamed, removed, or moved between groups at the
compiler's discretion. A renamed lint reads as one defect fixed and one
introduced; a removed lint reads as fixed; an edit to `[workspace.lints]` makes
whole cohorts appear or vanish, so **a configuration change reads as a code
change**.

Because condition 4 stores nothing, these readings stop being *corruption* and
become *surprise*: there is no stored history for a renamed lint to falsify. They
must still be surfaced where a user meets them — a report saying "3 fewer than
last week" after a toolchain bump misleads whether or not a database was
involved. Builder findings must **not** be wired into the cross-analyzer join,
whose correctness rests on both upstreams publishing identifiers. Nobody
publishes lint names; they are release notes.

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
Condition 4 is the answer to it: output that is never stored makes no
reproducibility claim to break.

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

**Draft.** The decision is taken: build-capable execution is a goal, the
`code_interpreter` non-goal is narrowed rather than withdrawn, and builder output
is reported rather than stored.

An earlier revision was blocked on the findings store's inability to distinguish
two builds of one commit. **Condition 4 dissolves that** — nothing is stored, so
nothing collides. What remains before acceptance is engineering rather than an
unanswered question: a writable build directory that does not relax the
read-only preflight for readers, the argv and environment seam a builder needs,
and a demonstrated refusal path that never falls back to the host.

### What has landed, and what it does not claim

`roteiro lint <analyzer>` ships the **reporting** half, with a `clippy` adapter,
and it runs **on the host**.

Conditions 3, 4 and 5 are built. The run records `isolation: none` and reports it
from the code that ran the process rather than from whoever prints it; its output
is never stored — there is no layer, no entry in the adapter registry `ingest`
resolves against, and no path from the linter to
`Store::replace_findings_layer`; and the three readings above are printed beneath
every report and carried in `--json`, so a scripted consumer is told what a
person is told. The linter is also kept out of the cross-analyzer join, which is
checked by a test rather than left to a reviewer.

Conditions 1 and 2 are **not** built, and nothing was borrowed from them. There
is no writable build directory inside a guest and no host-produced dependency
mount, because there is no guest: the sandboxed builder those conditions describe
does not exist. `check_request`'s read-only preflight is **untouched** and still
refuses every request whose worktree is writable — the linter takes a *different
request shape* rather than a weakened one, which is why shipping this needed no
relaxation of the invariant condition 1 is about.

So the capability available today is exactly the one this document calls the
inverted threat model, unmitigated: linting a branch you are reviewing executes
that branch's build scripts and proc macros on your machine. It is disclosed —
the argv and the isolation are printed before the run, and `roteiro lint --help`
says it in a paragraph — and disclosure is not mitigation. Mitigating it is what
conditions 1 and 2 are for, and they remain the unbuilt work.

## Document version history

| Version | Date | Notes |
|---------|------|-------|
| 1.0 | 2026-08-18 | Initial draft. Narrows ADR-0014's `code_interpreter` non-goal to exclude only model-authored code, scopes its "security argument is weaker" reasoning to parse-only analyzers, and relaxes the read-only worktree invariant for a builder runner only. Records the measured build-script and proc-macro counts that make the case, the inverted threat model, and the five conditions — of which condition 4, the store's inability to distinguish two builds of one commit, is unresolved and blocks acceptance. |
| 1.1 | 2026-08-18 | Condition 4 reversed on the owner's ruling that builder output is local to the person running it rather than an artifact stored for later. Storing a lint was the source of every identity problem the draft catalogued — an advisory id is *assigned* and permanent, a lint name is a symbol in a compiler — so not storing is the fix rather than a workaround. This unblocks acceptance: what remains is engineering, not an open question. Condition 5 softened accordingly, since with no stored history a renamed lint is a surprise rather than a corruption. |
| 1.2 | 2026-08-18 | Records what landed rather than changing any decision: `roteiro lint <analyzer>`, with a `clippy` adapter that reuses the shared normalisation shape and is deliberately absent from the registry `ingest` resolves against. Conditions 3–5 are built and tested — an unstored report, an `isolation: none` read out of the runner, and the renamed / removed / `[workspace.lints]` readings surfaced in both output shapes. Conditions 1–2 are untouched: the run has no boundary, and the read-only preflight was **not** relaxed to fit a builder through it. |

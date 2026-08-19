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
version: "1.4"
last-modified: 2026-08-19
confluence-url:
---

# ADR-0020: Build-capable sandboxed execution — running the repository's own build, and the non-goal it narrows

| | |
|---|---|
| **Document version** | 1.4 |
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

Adopt build-capable execution as a **distinct runner class**, subject to six
conditions. Each is stated as a requirement because each is a way this decision
turns into the failure ADR-0014 predicted.

### 1. Read-only stays the standard, including for builders

`check_request` refuses any request whose worktree is not read-only, and a test
pins it. That preflight **stays** — for readers *and* for builders.

The first draft of this condition hedged: the source tree stays read-only
"wherever the toolchain permits". Measured, the toolchain permits it. With
`CARGO_TARGET_DIR` pointing outside the tree and `--locked`, `cargo clippy`
completes against a source tree on which every write is refused. So the writable
surface a builder needs is a **scratch build directory**, not your code.

**And a measurement is a property of the thing measured.** v1.3 recorded the
result above and `roteiro lint` cited it, in a module doc comment, as a
description of what that module did. It was not: the module *inherited*
`CARGO_TARGET_DIR` by name from the invoking shell and never set it, so on the
ordinary path — a shell with no such variable — the linter wrote `target/` and
`Cargo.lock` into the tree it was reviewing, beneath a paragraph saying it did
not. The probe was sound and the inference from it was not. Whoever establishes
a precondition by hand owes the codebase the line that makes the code establish
it too, and a citation is not that line. `roteiro lint` now sets
`CARGO_TARGET_DIR` itself and passes `--locked`, and a test runs the shipped
command with the variable unset and asserts the tree is unchanged.

That is the difference between this decision and the one ADR-0014 warned about,
and it is a large one: an *additional mount* rather than a *removed guarantee*.
A malicious build script gets a directory that is discarded when the run ends.
It does not get your working tree. A blanket relaxation of the preflight remains
the conversion ADR-0014 predicted, and this condition now forecloses it rather
than rationing it.

One thing was unestablished and is no longer: the probe above had no
dependencies, so it never read or wrote `CARGO_HOME`, and whether a package
cache can be mounted read-only or needs a writable copy was open.

**Measured: a read-only `CARGO_HOME` is sufficient.** With the package cache
`chmod -R a-w`, the source tree `chmod -R a-w`, `CARGO_TARGET_DIR` outside both,
and `--locked --offline`, `cargo clippy` exits **0** and reports
`build-finished` with `success: true`. It emits 20 JSON messages — 13
`compiler-artifact`, **4 `build-script-executed`**, 2 `compiler-message`, 1
`build-finished` — and the scratch directory afterwards holds compiled build
scripts for `serde`, `serde_core`, `proc-macro2` and `quote`. So build scripts
were compiled *from* a read-only cache and *ran*, and neither the cache nor the
source tree was written to. Condition 2's dependency mount can therefore be
read-only like every other input mount, rather than needing a writable copy of a
cache measured elsewhere in this document at 414 MB to 1.10 GB.

**What that does not establish**, stated because the paragraph above is exactly
the kind that gets cited later as more than it is: the cache was **warm**, and a
cold one must still be populated by something that can write. The probe resolved
4 crates with one proc-macro dependency, not this repository, and emphatically
not an `--all-features` build of it — which, as the consequences below record,
cannot be built under a denied network today at all. It says a read-only package
cache is *workable*, not that this repository's full build is.

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

### 6. Sandboxed is the default; the host is opt-in

A builder runs in the sandbox unless the person running it has said otherwise.
This reverses what `roteiro lint` first shipped with, and the reverse is the
correct way round: linting a tree you did not write executes that tree's build
scripts and proc macros with your filesystem and your credentials. That the
*toolchain* is yours does not make the *code* yours.

Host execution is therefore **permitted, not assumed**, and the permission is
layered exactly as ADR-0019 layers the remote tier — for the same reason, which
is that `roteiro.toml` is committed and shared:

| Layer | May deny | May grant |
|---|---|---|
| Built-in default | sandboxed by default | — |
| Project `roteiro.toml` | **yes** | **no** |
| User `~/.roteiro/config.toml` | yes | **yes** |
| Invocation (flag) | yes | **yes** |

A merged line in a shared file that starts running builds on every teammate's
machine is consent by pull request: granted by someone else, noticed by nobody.
The project layer may switch the sandbox *on* for everyone, and may never switch
it *off* for anyone.

It differs from ADR-0019 in one respect, deliberately. There, the user layer and
the invocation must **both** grant. Here **either suffices**. Remote egress sends
your source elsewhere and is worth re-consenting to per run; running a build on
your own machine with your own toolchain is a standing preference a person may
reasonably express once. Requiring both would make the config key useless, since
you would still type the flag on every run.

**There is no fallback.** If the sandbox is selected and unavailable — feature
not compiled, runtime not provisioned, image absent — the run refuses and names
what is missing. It does not quietly become a host run. A boundary that vanishes
while the command still reports success is the silent downgrade ADR-0019 §6
exists to prevent, and it would be worse here than there: the person asked for
isolation and would get execution.

Until conditions 1 and 2 are built the default has nothing to select, so
`roteiro lint` refuses unless host execution has been granted. That is the honest
state rather than an awkward one — the command says the sandbox is the intended
path and is not yet built, and anyone who wants it on their own machine says so
once, in one place. Shipping the opposite default *because* the sandbox is
unfinished would be precisely the conversion this document spends its length
refusing: the availability of a capability quietly deciding a question that was
supposed to be decided deliberately.

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
read-only preflight for readers, and a demonstrated refusal path that never
falls back to the host.

This list used to also name "the argv and environment seam a builder needs", and
that was wrong in a way worth keeping rather than deleting, because the error is
the same one that produced the defect in condition 1. The seam was already
there. `Invocation` carries the argv, and `execute` took an environment list —
but that list held **names to inherit**, and a builder needs **pairs to set**.
The two read alike and do opposite things:

- **Inheriting locates.** `CARGO_HOME`, `RUSTUP_HOME` — where the toolchain is
  on this machine, which only the parent environment knows and which is the
  user's to choose.
- **Setting constrains.** `CARGO_TARGET_DIR` — where the build may write, which
  is a property the runner guarantees and therefore must choose itself.

Reading the one seam as though it were both is precisely how `CARGO_TARGET_DIR`
came to be listed as a passthrough under a promise that it was configured:
inheriting a name the parent has not set is a no-op that reads as a setting. The
two halves are now separate fields on one type, so the confusion is not
expressible rather than merely discouraged, and this blocker is correspondingly
narrower and cheaper than it was written up as — the environment seam exists and
needed splitting, not building.

### What has landed, and what it does not claim

`roteiro lint <analyzer>` ships the **reporting** half, with a `clippy` adapter.
Under condition 6 it runs on the host only when host execution has been granted,
and refuses otherwise, because the sandbox it would prefer does not yet exist.

Conditions 3, 4 and 5 are built. The run records `isolation: none` and reports it
from the code that ran the process rather than from whoever prints it; its output
is never stored — there is no layer, no entry in the adapter registry `ingest`
resolves against, and no path from the linter to
`Store::replace_findings_layer`; and the three readings above are printed beneath
every report and carried in `--json`, so a scripted consumer is told what a
person is told. The linter is also kept out of the cross-analyzer join, which is
checked by a test rather than left to a reviewer.

It also sets its own `CARGO_TARGET_DIR` — a per-checkout directory under
`~/.roteiro/lint/target`, keyed the way the findings store keys a worktree, and
never shared between two trees because ADR-0014 v1.6 holds a scratch directory
full of compiled build scripts to be per-repository. Candidates are checked
against the tree rather than trusted, so pointing `ROTEIRO_HOME` inside the
worktree is refused rather than obeyed, and a **relative** one is refused
outright — cargo resolves a relative `CARGO_TARGET_DIR` against the tree it is
building, so the containment check cannot answer for one and is never asked to. The directory is printed with the report
and carried in `--json`: roteiro overrides a `CARGO_TARGET_DIR` the caller set,
because where a build writes is this command's guarantee and not the caller's to
supply, and an override in silence would be its own surprise.

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
says it in a paragraph — and disclosure is not mitigation. What condition 6 adds
is that it is no longer the *default*: an unmitigated capability may be offered,
but it may not be the answer to a question nobody was asked. Mitigating it is
what conditions 1 and 2 are for, and they remain the unbuilt work.

## Document version history

| Version | Date | Notes |
|---------|------|-------|
| 1.0 | 2026-08-18 | Initial draft. Narrows ADR-0014's `code_interpreter` non-goal to exclude only model-authored code, scopes its "security argument is weaker" reasoning to parse-only analyzers, and relaxes the read-only worktree invariant for a builder runner only. Records the measured build-script and proc-macro counts that make the case, the inverted threat model, and the five conditions — of which condition 4, the store's inability to distinguish two builds of one commit, is unresolved and blocks acceptance. |
| 1.1 | 2026-08-18 | Condition 4 reversed on the owner's ruling that builder output is local to the person running it rather than an artifact stored for later. Storing a lint was the source of every identity problem the draft catalogued — an advisory id is *assigned* and permanent, a lint name is a symbol in a compiler — so not storing is the fix rather than a workaround. This unblocks acceptance: what remains is engineering, not an open question. Condition 5 softened accordingly, since with no stored history a renamed lint is a surprise rather than a corruption. |
| 1.2 | 2026-08-18 | Records what landed rather than changing any decision: `roteiro lint <analyzer>`, with a `clippy` adapter that reuses the shared normalisation shape and is deliberately absent from the registry `ingest` resolves against. Conditions 3–5 are built and tested — an unstored report, an `isolation: none` read out of the runner, and the renamed / removed / `[workspace.lints]` readings surfaced in both output shapes. Conditions 1–2 are untouched: the run has no boundary, and the read-only preflight was **not** relaxed to fit a builder through it. |
| 1.3 | 2026-08-19 | Default posture set by the owner: a builder runs **sandboxed** unless host execution has been granted, and the grant is layered as ADR-0019 layers the remote tier — project `roteiro.toml` may deny it and may never grant it, because a merged line in a shared file is consent granted by someone else and noticed by nobody. Adds condition 6, including that the sandbox never silently falls back to the host. Condition 1 is strengthened rather than amended: measured, `cargo clippy` completes against a fully read-only source tree with `CARGO_TARGET_DIR` outside it, so the preflight is not relaxed at all and a builder's writable surface is an **added scratch mount** rather than a **removed guarantee**. Records the consequence that `roteiro lint` must refuse by default until conditions 1–2 exist. |
| 1.4 | 2026-08-19 | **Corrects a claim this document's own measurement was used to justify.** v1.3 measured that `cargo clippy` completes against a fully read-only source tree with `CARGO_TARGET_DIR` outside it; `roteiro lint` then cited that result as a description of itself while *inheriting* the variable by name and never setting it — so on any shell that had not set one, the linter wrote `target/` and `Cargo.lock` into the tree it was reviewing, under a doc comment saying it did not. The module now sets `CARGO_TARGET_DIR` to a per-checkout directory outside the tree (ADR-0014 v1.6: a build scratch holds compiled build scripts and is never shared between trees) and passes `--locked` so the lockfile is not rewritten either, with a test that runs the shipped command with the variable unset and asserts the tree is unchanged — by content, not merely by filename, since `Cargo.lock` is rewritten in place and a listing of names cannot see that. A relative scratch root is refused rather than resolved, and the refusal is ordered before the containment check because that check resolves against roteiro's working directory while cargo resolves against the worktree, so on a relative path it decides a question about the wrong directory. **Condition 1's open question is answered**: measured with `CARGO_HOME` and the source tree both `chmod -R a-w`, `CARGO_TARGET_DIR` outside and `--locked --offline`, `cargo clippy` exits 0, reports `build-finished` successfully, and executes 4 build scripts compiled from the read-only cache — so a package cache can be mounted read-only rather than copied, with the honest caveat that the cache was warm and the probe was 4 crates rather than an `--all-features` build of this repository. Finally, **withdraws "the argv and environment seam a builder needs" as a blocker**: the seam existed, and was the wrong kind — a list of names to *inherit* where a builder needs pairs to *set*. Conflating the two is what produced the defect above, so they are now separate fields on one type and the blocker is a split rather than a build. |

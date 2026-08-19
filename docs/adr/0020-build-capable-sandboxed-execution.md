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
version: "1.5"
last-modified: 2026-08-19
confluence-url:
---

# ADR-0020: Build-capable sandboxed execution — running the repository's own build, and the non-goal it narrows

| | |
|---|---|
| **Document version** | 1.5 |
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
Dependencies reach the guest by a host-produced, read-only mount.

**The `cargo vendor` this condition anticipated is not needed**, and the saving is
the largest single one in the design. v1.4 measured that a read-only `CARGO_HOME`
suffices; v1.5 ships it. The mount is the host's **existing** package cache —
nothing vendored, nothing regenerated per lockfile change, and none of the
414 MB / 556 MB / 1.10 GB this condition budgeted for.

**What is mounted is not `$CARGO_HOME`.** Building it revealed something the
measurement did not, because the probe had no credentials in it: the root of a
real `CARGO_HOME` holds `credentials.toml` — a crates.io API token — and a
`config.toml` that may carry registry tokens of its own. Mounting the root would
put both in front of the build scripts this whole decision exists to contain, and
*"egress is denied"* is **not** an answer to that: the run's own output comes back
to the host, and `cargo::warning=` is a channel a build script can write to. So
the two subdirectories that hold *packages* are mounted and the root is not:

| host | guest | mode |
|---|---|---|
| the worktree | `/work` | read-only |
| a scratch outside it | `/scratch` | **writable** |
| `$CARGO_HOME/registry` | `/cargo/registry` | read-only |
| `$CARGO_HOME/git` | `/cargo/git` | read-only |

The cost, recorded rather than left to be discovered: a `$CARGO_HOME/config.toml`
that redirects a source is **not** seen by the guest. A project's own
`.cargo/config.toml` is, because it is inside the worktree.

**The failure mode a mounted cache has, and how it is reported.** A guest with no
interface cannot fetch what the host does not already hold. Cargo says so from
inside a machine the user cannot open a shell in, in two wordings that were both
measured — `attempting to make an HTTP request, but --offline was specified` when
the `.crate` file is absent, and `failed to unpack package` when it is present
but unexpanded, because expanding it into a read-only mount is a write. Both have
one remedy, and it is on the host: `cargo fetch --locked`, which downloads *and*
unpacks. That is a **refusal that names the way forward**, not a build error
passed through.

### 2a. The image is **supplied**, and Roteiro will not choose one

This was assumed rather than decided, and building it showed the assumption was
false. `SANDBOX_IMAGES` states the rule an analyzer must meet to earn a pinned
entry — a **published** image, addressable by digest, whose analyzer version is
knowable — and gives the reason: *"inventing one would make Roteiro the publisher
of a security tool's container, which is not a job it is taking on."* An official
Rust image was expected to satisfy it unchanged.

**No image satisfies it for `clippy`.** `rust-lang/docker-rust` builds **every**
stable and nightly variant with `rustup-init --profile minimal`, which installs
`rustc`, `cargo` and `rust-std` and stops. Verified against the Dockerfile source
for `1.97.1/trixie`, `1.97.1/bookworm` and nightly, and then verified again from
inside a running guest: `cargo clippy --version` in
`docker.io/library/rust@sha256:b1b3c9c0…` answers *"'cargo-clippy' is not
installed for the toolchain '1.97.1-aarch64-unknown-linux-gnu'"*. `rustlang/rust`
is the same repository; `instrumentisto/rust` is archived and mirrors it.

That left three options, and the owner's ruling took the third:

1. **Roteiro builds and publishes one.** Refused by the rule above.
2. **Roteiro points at a third party's** — CircleCI's `cimg/rust` and Microsoft's
   `devcontainers/rust` both carry `clippy`, because they install rustup without
   `--profile minimal`. Also refused, and for a sharper reason than the rule
   states: an image is not a dependency of the analysis, it **is the boundary**.
   It is the container somebody else's build scripts execute in. Choosing whose
   container that is, on the user's behalf, in a default, is a security decision
   made by Roteiro and noticed by nobody.
3. **The user supplies it.** `[lint] image`, or `--image`, pinned by digest. An
   image without the linter in it is a named refusal that says how to build one
   (`docs/SANDBOXED_LINTING.md`, two lines of Dockerfile), rather than a
   `cargo clippy` that mysteriously reports "no such command" and an empty
   report over a tree nobody linted.

**A tag is refused**, and *not* on the reproducibility argument this document
retires for builders below. On a plainer one: a tag is a mutable pointer to the
boundary. Whoever controls it can replace what runs, with no version change and
no notice, and the run would go on reporting success. You may choose your own
boundary; you may not choose one that can be swapped under you.

The layering is `[remote] endpoint`'s, **not** `[lint] allow_unsandboxed`'s:
project over user, `--image` over both. A project may choose *where* its team's
boundary comes from without being able to decide *whether* there is one. The
inversion belongs to the permission; a locator does not invert.

**One consequence that follows from supplying the image, and is not obvious.**
The image has to be able to *build* the tree, not merely lint it — `cargo clippy`
has `cargo check` semantics, so every build script runs inside it. Measured on
this repository: `--all-targets` compiles `rto-llama`'s dev-dependency on vendored
llama.cpp, whose build script needs `libclang`, and an image carrying `cmake` but
not `libclang` panics in `bindgen` with no diagnostic to show for it. That is
reported as a build that produced nothing, with the cause named, rather than as
zero findings.

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

Conditions 1–2 add a fourth, and it is the one a person meets first: **a
sandboxed run reports what the image's rustc said, and that is not this
machine's.** Which lints fire is decided by that compiler, so `roteiro lint
clippy` and `cargo clippy` in the same tree on the same day can disagree with no
defect on either side. It is surfaced in the same list as the other three —
printed beneath every report and carried in `--json` — because a scripted
consumer must be told what a person is told, and it is stated again beside the
isolation line, where the image digest it came from is printed.

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

**Parity was expected to weaken, and did not.** This section predicted that the
backend-parity test would stay green while defending a claim the feature no longer
made, and would have to be re-scoped. It did not need to be, and the reason is
worth keeping rather than deleting, because it is the same reason the builder
needed no relaxation of `check_request`: **a builder is not an `AnalyzerRunner`.**
The parity test compares two backends of the *reader* contract — one
`AnalysisRequest`, one `AnalysisResponse`, one stored `AnalysisRun` — and
`roteiro lint` implements none of them. There is no shared claim for a builder to
falsify, so the test still means exactly what it says.

What the prediction was right about is the *fact*: a builder's toolchain does
differ between host and image, and the count moves with it. That is handled where
it is met rather than by a test — condition 5 below, and the report and `--help`
both say it.

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
two builds of one commit. **Condition 4 dissolved that** — nothing is stored, so
nothing collides. What was then named as the remaining engineering — *"a writable
build directory that does not relax the read-only preflight for readers, and a
demonstrated refusal path that never falls back to the host"* — is built, and
demonstrated end to end: a sandboxed `cargo clippy` over a real 212-file worktree
returning 108 diagnostics, recording `isolation: microvm` and the image digest,
with the tree byte-identical before and after; and every refusal exercised
against a real guest, including an image without the linter and both wordings of
a cache too cold to build from.

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

`roteiro lint <analyzer>` ships both halves, with a `clippy` adapter. Under
condition 6 it runs **sandboxed** unless host execution has been granted.

What the layers say did not change and what *"denied"* amounts to did. A project
that denies host execution, a user config that denies it, and `--sandboxed` now
all mean **run in the boundary**, where between v1.3 and this revision they meant
refuse — because there was no boundary. The refusal that remains is narrower: a
sandbox that was selected and **cannot be had**. It never becomes a host run, it
names what is missing, and it names a way forward that would work *for that
reason* — which is why a project denial is told to take it up with the repository
rather than offered a flag that would not help it.

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

Conditions 1 and 2 are **built**, and the default now has something to select.
`roteiro lint clippy` runs `cargo clippy` inside a digest-pinned OCI image in a
microVM, against a read-only worktree, with a writable scratch outside it, a
read-only mount of this machine's package cache, and no network interface.

**Condition 1 turned out smaller than this document first assumed, and the
smallness is the point.** `check_request`'s read-only preflight is **untouched**.
Nine tools were measured and none needed a writable source tree; a builder's
writable surface is a **third `VolumeSpec` with `read_only: false`**, added
beside the two that were already there and already `true`. An *added mount*
rather than a *removed guarantee* is the whole reason this ADR could narrow
ADR-0014's non-goal rather than withdraw it, and it is now a fact about the code
rather than a claim about it: a test asserts the scratch is the **only** writable
volume, and fault injection confirms the test fails when it is not.

The evidence is a build script's own, since a build script is the arbitrary code
this is all about. Run inside the guest, it reports `Linux 6.12.76 aarch64`;
`touch /work/…` → `Read-only file system`; `touch /cargo/registry/…` →
`Read-only file system`; `touch $CARGO_TARGET_DIR/…` → succeeds;
`cat /cargo/credentials.toml` → `No such file or directory`; network interfaces
`lo` and `dummy0`, and DNS resolution fails. Afterwards the worktree is
digest-identical, by **content** rather than by listing — `Cargo.lock` is
rewritten in place and a listing of names cannot see that (v1.4).

**One thing was added that neither condition asked for**, because building both
made it necessary: the scratch is keyed per **backend** as well as per checkout.
Without `--target`, cargo lands both backends' artefacts in `<scratch>/debug`, and
they are different operating systems' executables under the same names — measured,
a guest build script is `ELF 64-bit … ARM aarch64 … GNU/Linux` where the host's is
`Mach-O`. On a macOS host sharing one directory is churn; on a Linux host it is
ADR-0014 v1.6's hazard run backwards, a build script compiled *inside* the
boundary sitting where a later host lint builds. They are siblings, and a test
says so.

**The environment seam is reconciled**, which v1.4 left as two mechanisms for one
concept. `ChildEnv` and the guest's environment now live in one module with two
consumers, and the difference between the consumers is stated rather than tidied
away: the `set` half applies to both, and the `inherit` half is host-only **by
construction**. A guest shares neither this machine's filesystem nor its
environment block, so a name carried across would point at a directory that is
not there. In a guest, everything is set.

What remains unmitigated is narrower than it was and worth naming. Host execution
is still available, still opt-in, still records `isolation: none`, and still
executes the tree's build scripts here. `--all-features` on **this** repository
still cannot be built under a denied network, for the reason recorded above — a
dependency's build script fetches at build time — so that feature set stays out
of scope for a sandboxed run of this repository, though not for others.

## Document version history

| Version | Date | Notes |
|---------|------|-------|
| 1.0 | 2026-08-18 | Initial draft. Narrows ADR-0014's `code_interpreter` non-goal to exclude only model-authored code, scopes its "security argument is weaker" reasoning to parse-only analyzers, and relaxes the read-only worktree invariant for a builder runner only. Records the measured build-script and proc-macro counts that make the case, the inverted threat model, and the five conditions — of which condition 4, the store's inability to distinguish two builds of one commit, is unresolved and blocks acceptance. |
| 1.1 | 2026-08-18 | Condition 4 reversed on the owner's ruling that builder output is local to the person running it rather than an artifact stored for later. Storing a lint was the source of every identity problem the draft catalogued — an advisory id is *assigned* and permanent, a lint name is a symbol in a compiler — so not storing is the fix rather than a workaround. This unblocks acceptance: what remains is engineering, not an open question. Condition 5 softened accordingly, since with no stored history a renamed lint is a surprise rather than a corruption. |
| 1.2 | 2026-08-18 | Records what landed rather than changing any decision: `roteiro lint <analyzer>`, with a `clippy` adapter that reuses the shared normalisation shape and is deliberately absent from the registry `ingest` resolves against. Conditions 3–5 are built and tested — an unstored report, an `isolation: none` read out of the runner, and the renamed / removed / `[workspace.lints]` readings surfaced in both output shapes. Conditions 1–2 are untouched: the run has no boundary, and the read-only preflight was **not** relaxed to fit a builder through it. |
| 1.3 | 2026-08-19 | Default posture set by the owner: a builder runs **sandboxed** unless host execution has been granted, and the grant is layered as ADR-0019 layers the remote tier — project `roteiro.toml` may deny it and may never grant it, because a merged line in a shared file is consent granted by someone else and noticed by nobody. Adds condition 6, including that the sandbox never silently falls back to the host. Condition 1 is strengthened rather than amended: measured, `cargo clippy` completes against a fully read-only source tree with `CARGO_TARGET_DIR` outside it, so the preflight is not relaxed at all and a builder's writable surface is an **added scratch mount** rather than a **removed guarantee**. Records the consequence that `roteiro lint` must refuse by default until conditions 1–2 exist. |
| 1.4 | 2026-08-19 | **Corrects a claim this document's own measurement was used to justify.** v1.3 measured that `cargo clippy` completes against a fully read-only source tree with `CARGO_TARGET_DIR` outside it; `roteiro lint` then cited that result as a description of itself while *inheriting* the variable by name and never setting it — so on any shell that had not set one, the linter wrote `target/` and `Cargo.lock` into the tree it was reviewing, under a doc comment saying it did not. The module now sets `CARGO_TARGET_DIR` to a per-checkout directory outside the tree (ADR-0014 v1.6: a build scratch holds compiled build scripts and is never shared between trees) and passes `--locked` so the lockfile is not rewritten either, with a test that runs the shipped command with the variable unset and asserts the tree is unchanged — by content, not merely by filename, since `Cargo.lock` is rewritten in place and a listing of names cannot see that. A relative scratch root is refused rather than resolved, and the refusal is ordered before the containment check because that check resolves against roteiro's working directory while cargo resolves against the worktree, so on a relative path it decides a question about the wrong directory. **Condition 1's open question is answered**: measured with `CARGO_HOME` and the source tree both `chmod -R a-w`, `CARGO_TARGET_DIR` outside and `--locked --offline`, `cargo clippy` exits 0, reports `build-finished` successfully, and executes 4 build scripts compiled from the read-only cache — so a package cache can be mounted read-only rather than copied, with the honest caveat that the cache was warm and the probe was 4 crates rather than an `--all-features` build of this repository. Finally, **withdraws "the argv and environment seam a builder needs" as a blocker**: the seam existed, and was the wrong kind — a list of names to *inherit* where a builder needs pairs to *set*. Conflating the two is what produced the defect above, so they are now separate fields on one type and the blocker is a split rather than a build. |
| 1.5 | 2026-08-19 | **Conditions 1 and 2 are built**, and one assumption in this document was false. Condition 1 needed no relaxation of `check_request` at all — a builder's writable surface is a third `VolumeSpec` with `read_only: false` beside two that stay `true`, an *added mount* rather than a *removed guarantee*, now asserted by a test rather than argued. Condition 2's `cargo vendor` is not needed either: the package cache is a read-only mount of the host's existing one, so none of the 414 MB / 556 MB / 1.10 GB is spent — but building it found what the probe could not, since the probe had no credentials in it, and **`$CARGO_HOME` itself is therefore not what is mounted**. Its root holds `credentials.toml`, and "egress is denied" does not answer that, because the run's output returns to the host and `cargo::warning=` is a channel a build script can write to; only `registry/` and `git/` are mounted, at the cost — stated here rather than discovered — that a `$CARGO_HOME/config.toml` source redirect is invisible to the guest. **The image is supplied by the user, not chosen by Roteiro**, because the assumption that an official Rust image would satisfy `SANDBOX_IMAGES`' rule is wrong: rust-lang builds every stable *and* nightly variant `--profile minimal`, so no first-party image carries `clippy`, confirmed from inside a running guest. Pointing at a third party's was declined on a sharper ground than the rule states — an image is not a dependency of the analysis, it **is the boundary**, and choosing whose container that is in a default is a security decision made here and noticed by nobody. A tag is refused for the same reason rather than for reproducibility, which this document retires for builders. Adds the fourth reading a lint count carries — a sandboxed run reports what the *image's* rustc said — to condition 5 and to both output shapes. Records that **parity did not weaken** as predicted, because a builder is not an `AnalyzerRunner` and the parity test is a reader-contract test; that the scratch is keyed per **backend** as well as per checkout, since the two put different operating systems' executables under one name; and that the environment seam is reconciled into one type whose `inherit` half is host-only by construction, a guest having no parent environment to inherit from. |

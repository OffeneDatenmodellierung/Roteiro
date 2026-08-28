---
site-page: sandboxing
site-nav: Sandboxing
site-order: 22
---

# Sandboxed execution, and the images you supply

Two commands run somebody else's code inside a microVM, and each of them needs an
image to run it in:

| Command | What runs in the guest | Where its image comes from |
| --- | --- | --- |
| `roteiro lint clippy` | a **builder** — it compiles your tree, so your build scripts execute | `[lint] image`, always yours |
| `roteiro security run <analyzer>` | a **reader** — it parses your tree | a built-in pin where Roteiro has one, `[security.images]` otherwise |

**One document rather than two**, because the rules are one set of rules: a tag is
refused by the same function for both, neither ever pulls during a run, neither
guest has a network interface, neither falls back to the host, and both record the
digest of the image that actually ran. Splitting it would mean two copies of all
of that, and two copies drift — which is the failure the shared pin-checking
function exists to prevent one level down. What differs between the two is small,
specific, and reads better as a contrast in place than as two documents you have
to hold side by side.

If you only care about one of them, read [Part 1](#part-1-roteiro-lint-supplying-a-builder-s-image)
or [Part 2](#part-2-roteiro-security-run-supplying-an-analyzer-s-image); the rules
above are stated in whichever part you land in.

## Part 1 — `roteiro lint`: supplying a builder's image

`roteiro lint clippy` runs the linter **inside a microVM** unless you have said
otherwise. This is the one thing you have to do first: supply the image it runs
in.

### Why there is something to do at all

Roteiro ships pinned images for some *reader* analyzers — `semgrep` has one, and
it is a digest in `SANDBOX_IMAGES` that nobody has to think about. A linter does
not, and the reason is not an oversight:

> An analyzer earns an entry when there is a **published** image whose contents
> can be pinned by digest *and* whose analyzer version is knowable — inventing
> one would make Roteiro the publisher of a security tool's container, which is
> not a job it is taking on.

**No image satisfies that for `clippy`.** `rust-lang/docker-rust` builds every
stable *and* nightly variant with `rustup-init --profile minimal`, which installs
`rustc`, `cargo` and `rust-std` and stops. There is no first-party Rust image
carrying the `clippy` component. You can check this yourself, and Roteiro will
show you rather than assert it:

```console
$ roteiro lint clippy --image docker.io/library/rust@sha256:b1b3c9c0…
Error: the image ran, and `cargo clippy --version` inside it did not work: this image does not carry `clippy`.
  its stderr ended:
  error: 'cargo-clippy' is not installed for the toolchain '1.97.1-aarch64-unknown-linux-gnu'
```

The remaining options were a third party's image or one of Roteiro's own, and
both were declined. The sandbox exists so that somebody else's build scripts run
inside a boundary; **whoever publishes that boundary is a security decision**, and
it is not one Roteiro will make on your behalf, silently, in a default. So it is
yours, and an image without the linter in it is a refusal rather than an empty
report.

### The image

Two lines:

```dockerfile
FROM rust:1.97.1
RUN rustup component add clippy
```

Build it, push it, and take the **digest**:

```console
$ docker buildx build --platform linux/amd64,linux/arm64 -t registry.example/you/rust-clippy:1.97.1 --push .
$ docker buildx imagetools inspect registry.example/you/rust-clippy:1.97.1 | head -2
Name:      registry.example/you/rust-clippy:1.97.1
Digest:    sha256:1234…
```

Use the **index** digest — the one `imagetools inspect` prints for the tag
itself, not a per-platform one. It resolves on both `linux/amd64` and
`linux/arm64`, so CI and an Apple Silicon laptop pin one identifier rather than
two that can drift apart.

#### A tag will be refused

```toml
[lint]
image = "registry.example/you/rust-clippy:1.97.1"   # refused
image = "registry.example/you/rust-clippy@sha256:1234…"  # good
```

Not for reproducibility — ADR-0020 retires that argument for builders, because a
build's answer depends on a toolchain no digest pins. For a plainer reason: the
image **is** the boundary. It is where somebody else's build scripts execute, and
a tag is a mutable pointer to it. Whoever controls the tag can replace what runs,
with no version change and no notice, and the run would go on reporting success.
Choose your own boundary; do not choose one that can be swapped under you.

### Configuring it

```toml
# ~/.roteiro/config.toml — yours
[lint]
image = "registry.example/you/rust-clippy@sha256:1234…"
```

```toml
# roteiro.toml — your team's, committed
[lint]
image = "registry.example/team/rust-clippy@sha256:5678…"
```

Ordinary precedence: **project over user, `--image` over both.** That is
`[remote] endpoint`'s rule, not `[lint] allow_unsandboxed`'s — a project may
choose *where* its team's boundary comes from without being able to decide
*whether* there is one. The permission inverts; a locator does not.

`roteiro config` prints all three layers, so you can see which one is in effect.

### Provisioning it

A run **never** pulls. Provisioning fetches, running reads — the same rule every
other pinned input follows, so a lint can never fail because a registry was
unreachable, nor succeed by quietly fetching something new.

```console
$ roteiro security prefetch --analyzer clippy --allow-download
pulling the sandboxed linter's image, from `[lint] image`: registry.example/you/rust-clippy@sha256:1234…
```

### Your dependencies have to be on this machine already

The guest has **no network interface** — not a blocked one, an absent one — so it
cannot fetch a crate. It builds from a **read-only mount of this machine's own
package cache**, which is why nothing is vendored and nothing is regenerated per
lockfile change.

If the cache does not already hold what the lockfile names, the run refuses and
tells you the one thing that fixes it:

```console
$ roteiro lint clippy
Error: the build needs a dependency that this machine's cargo cache does not hold, and the guest
  has no network to fetch it with …
  Fetch them on the host first, in the tree you are linting:
    cargo fetch --locked
```

`cargo fetch` both downloads *and* unpacks, which is what a read-only cache mount
needs — a `.crate` file that is present but not expanded fails just as a missing
one does, because expanding it would be a write.

#### What is mounted, and what is deliberately not

| mount | mode |
| --- | --- |
| the worktree | read-only |
| a scratch build directory outside it | **writable** |
| `$CARGO_HOME/registry` | read-only |
| `$CARGO_HOME/git` | read-only |

`$CARGO_HOME` **itself** is not mounted, and that is deliberate: it holds
`credentials.toml` — your crates.io API token — and a `config.toml` that may
carry registry tokens of its own. Putting those in front of the build scripts the
sandbox exists to contain would defeat the point, and "egress is denied" is not an
answer, because the run's own output comes back to this machine and
`cargo::warning=` is a channel a build script can write to.

The cost, stated rather than discovered: a `$CARGO_HOME/config.toml` that
redirects a source is **not** seen by the guest. A project's own
`.cargo/config.toml` is, because it is inside the worktree.

### The count can differ from your local `cargo clippy`, legitimately

Which lints fire is decided by the **image's** rustc, and it will not generally
match this machine's. A lint name is a symbol in a compiler, so a different
compiler fires a different set. `roteiro lint clippy` and `cargo clippy` in the
same tree on the same day can disagree with no defect on either side.

Nothing is stored (ADR-0020 v1.1), so this is a **surprise rather than a
corruption**: there is no history for a different compiler to falsify, and no
layer key for two toolchains to collide in. The report names the toolchain it
used, beside the image digest it came from, because that is the only way the
number is comparable to any other number.

### It never falls back

If the sandbox is selected and cannot be had — no image, an image without the
linter, an image not in the local store, no hypervisor, a build without
`exec-boxlite` — the run **refuses and says what is missing**. It does not quietly
become a host run.

That is ADR-0020 §6, and it is worse here than elsewhere: the person asked for
isolation and would get execution, on a tree whose build scripts are the reason
they asked. Running on this host is available, and it is something you say out
loud:

```console
$ roteiro lint clippy --allow-unsandboxed        # this run
```

```toml
# ~/.roteiro/config.toml — standing, for you
[lint]
allow_unsandboxed = true
```

A repository's committed `roteiro.toml` may **deny** host execution for everyone
working in it, and may never grant it — a merged line granting it would be consent
given by someone else and noticed by nobody. In a repository that denies it, the
sandbox is what everyone gets, and `--allow-unsandboxed` does not override that.

## Part 2 — `roteiro security run`: supplying an analyzer's image

### First, the constraint that decides whether any of this will work for you

> **A user image can only serve an analyzer Roteiro already has an adapter for.**

Roteiro does not read analyzer output generically. Each analyzer has a
`normalize()` written in Rust, compiled into the binary, in `ADAPTERS` — it is
what turns that tool's particular JSON into a finding with a stable identity. You
can supply an image. **You cannot supply a parser.**

So an image containing your favourite linter will boot perfectly, run whatever you
put in it, and produce output nothing in Roteiro can read. That is refused *at the
key* rather than discovered as an empty report after a guest has run:

```console
$ roteiro security status
Error: `[security.images] my-favourite-linter` names an analyzer this build cannot read the output of (it can read: cargo-audit, osv-scanner, semgrep).
  An image can only serve an analyzer Roteiro already has an adapter for — the parser is Rust in `ADAPTERS` and cannot be supplied alongside the image. An image carrying some other tool boots perfectly and produces nothing Roteiro can normalise.
  To have findings from a tool that is not on that list, run it yourself and `roteiro security ingest` its report.
```

The list in that message is the authoritative one for your build; `roteiro
security status` prints it too. If the tool you want is not on it, the way to get
its findings into Roteiro is `roteiro security ingest`, which accepts a normalized
report produced anywhere — that path is unchanged and needs no image at all.

**The name is part of the contract too.** Roteiro invokes the analyzer by the
program name its adapter declares — `semgrep`, `osv-scanner`, `cargo-audit` — and
looks it up on the guest's `PATH`. An image that ships the binary somewhere off
`PATH`, or under another name, will boot and then fail to exec. Google's own
`ghcr.io/google/osv-scanner` is exactly that shape: its entrypoint is
`/osv-scanner`, an absolute path, and `/` is not on the `PATH` its config
declares. If you are wrapping an upstream image, this is usually one `RUN ln -s`.

### Why some analyzers have an image and others do not

`SANDBOX_IMAGES` is short, and it stays short:

> An analyzer earns an entry when there is a **published** image whose contents
> can be pinned by digest *and* whose analyzer version is knowable — inventing one
> would make Roteiro the publisher of a security tool's container, which is not a
> job it is taking on.

`semgrep` satisfies that. `cargo-audit` has no official image at all. Until
recently that sentence ended in *"so there is no entry"*, which meant those
analyzers were host-only forever on a command whose default is sandboxed. It now
ends in **"so you supply one"**, which is the same answer `[lint] image` has always
given for builders.

`roteiro security status` tells you which is which, and that column is the point
of the whole feature — Roteiro checked that a built-in image is published,
digest-addressable and of a knowable version, and it checked none of that about a
reference you handed it:

```console
$ roteiro security status --analyzer semgrep
sandbox images  (ADR-0014 — a run never pulls; `prefetch` obtains)
  semgrep      built-in                                   in the local store
               docker.io/semgrep/semgrep@sha256:67319956da3dcb58baf5b322899c15458e3963e7018a86aeeb5cd224e69cb77a
               analyzer version 1.173.0, which Roteiro checked this image carries
```

### Configuring it

A map keyed by analyzer, because this surface has one image *per analyzer* where a
builder has one image full stop:

```toml
# ~/.roteiro/config.toml — yours
[security.images]
osv-scanner = "registry.example/you/osv-scanner@sha256:1234…"
cargo-audit = "registry.example/you/cargo-audit@sha256:5678…"
```

```toml
# roteiro.toml — your team's, committed
[security.images]
osv-scanner = "registry.example/team/osv-scanner@sha256:9abc…"
```

**Ordinary precedence, per analyzer: project over user.** That is `[lint] image`'s
rule and `[remote] endpoint`'s — a project may choose *where* its team's boundary
comes from without being able to decide *whether* there is one. The permission
keys invert; a locator does not.

Per *analyzer*, not per table: in the two files above, `osv-scanner` comes from the
project and `cargo-audit` still comes from you. A project naming an image for one
analyzer does not silently un-declare your image for a different one — they answer
different questions and neither is a narrowing of the other.

`roteiro config` prints every entry with the layer that set it, and **reports** a
bad one rather than refusing over it — it is the command you run precisely because
a key is not doing what you expected, so it must not be the one command that key
stops:

```console
$ roteiro config
[security]  (ADR-0014 — locators, so ordinary precedence: project over user)
  images.semgrep = "docker.io/semgrep/semgrep:1.172.0"  (project)
  ** this entry is refused wherever it is used: the image for `[security.images] semgrep` is "docker.io/semgrep/semgrep:1.172.0", which is a tag rather than a digest.
```

### Declaring an image for an analyzer that already has one

You may. A declared entry **replaces** the built-in pin, and the reason it is
allowed is that the alternative would make Roteiro the sole timekeeper of that
pin: an advisory against the pinned `semgrep` would be un-routable-around until a
Roteiro release. "You may declare an image for any analyzer except the ones we
happened to have got round to pinning" is not a rule anyone can hold in their
head.

**It costs you the asserted version, and that is deliberate.** Roteiro records
`analyzer_version` from the table for a pinned image, because it checked what that
image carries. It has no way to check yours, and repeating the table's answer
would stamp evidence describing an image that was not run. So it records what the
analyzer says about **itself**:

```console
$ roteiro security run semgrep          # with the built-in pin
running (isolation microvm, in docker.io/semgrep/semgrep@sha256:67319956…): semgrep scan --json …
semgrep 1.173.0 produced 4 finding(s) → security:semgrep:61a73fd1647f56af (runner sandboxed, isolation microvm)
```

```console
$ roteiro security run semgrep          # with [security.images] semgrep = "…@sha256:65dcd440…"
running (isolation microvm, in docker.io/semgrep/semgrep@sha256:65dcd440…): semgrep scan --json …
semgrep 1.172.0 produced 4 finding(s) → security:semgrep:61a73fd1647f56af (runner sandboxed, isolation microvm)
```

`1.172.0` is what that image actually carries, read from semgrep's own output. The
table would have said `1.173.0`. An adapter whose output carries no version at all
records `unknown`, which is a truthful answer where a copied one would not be.

The status row says so out loud rather than leaving a blank column:

```console
  semgrep      user-declared (replaces the built-in pin)  in the local store
               docker.io/semgrep/semgrep@sha256:65dcd4408adda7c183a6b4550cb1e9b19f7f627a6fbb7e0559bd466bedc44d7b
               analyzer version is not asserted for an image Roteiro did not choose — the run records what the analyzer says about itself
```

One consequence worth knowing about if you read the test suite: `backend_parity`
asserts that a subprocess run and a sandboxed run produce identical findings, and
that claim holds **for the pinned image only**. A different image carries a
different analyzer, and a different analyzer legitimately finds different things.
Nothing pretends otherwise — the tests now assert which image they ran.

### A tag will be refused

Exactly as for `[lint] image`, by exactly the same function:

```toml
[security.images]
osv-scanner = "registry.example/you/osv-scanner:2.5.0"       # refused
osv-scanner = "registry.example/you/osv-scanner@sha256:1234…" # good
```

```console
$ roteiro security run osv-scanner
Error: the image for `[security.images] osv-scanner` is "registry.example/you/osv-scanner:2.5.0", which is a tag rather than a digest.
  An image is where somebody else's code executes, and a tag is a mutable pointer to it — whoever controls the tag can replace what runs, with no version change and no notice.
  Pin it by digest instead:
    <key> = "docker.io/you/image@sha256:<64 hex>"
  `docker buildx imagetools inspect <reference>` prints it. Use the **index** digest — the one printed for the tag itself — so one reference resolves on both amd64 and arm64 rather than two that can drift apart.
```

The difference between an image Roteiro pinned and one you declared is *who
chose*, never *how strong the pin is*.

### Provisioning it

A run **never** pulls — and that is as true of an image you declared as of one
Roteiro pinned:

```console
$ roteiro security prefetch --analyzer osv-scanner --allow-download
pulling sandbox image for osv-scanner [user-declared] (registry.example/you/osv-scanner@sha256:1234…)
```

The reference is printed **before** a socket is opened, and labelled with who
chose it. That matters more here than for a builder: a reference in a committed
`roteiro.toml` is an image a teammate may have picked for you, and printing whose
choice it was is what turns that into a thing you saw.

If you skip it, the refusal names the exact command:

```console
$ roteiro security run semgrep
Error: assets-unavailable-offline: the image for semgrep is not in the local store
  image: docker.io/semgrep/semgrep@sha256:65dcd4408adda7c183a6b4550cb1e9b19f7f627a6fbb7e0559bd466bedc44d7b
  fetch it with: roteiro security prefetch --analyzer semgrep --allow-download
  (roteiro never pulls an image during a run, so a scan can never depend on a registry being reachable — and that is as true of an image you declared in `[security.images]` as of one Roteiro pinned)
```

`roteiro sandbox status` reports what the store is holding, and `roteiro sandbox
clear` gives the bytes back; a declared image is re-obtainable from its digest
like any other, so clearing costs time and never information.

**Public registries only, for now.** This is a conflict rather than a missing
feature. A private registry needs credentials at pull time, and the only place
they could come from is the ambient environment — inside a feature whose whole
posture is to keep ambient credentials *out*, and whose guest is never handed an
environment at all. Resolving that needs a credential story, not a code path. An
image you declare must be pullable without authentication.

### What the analyzer's guest gets

Less than a builder's, because a reader needs less:

| mount | mode |
| --- | --- |
| the worktree | read-only |
| the pinned asset cache (rule sets, advisory databases) | read-only |

No writable scratch, no package cache, no network device — a reader is handed a
tree and a rule file and told to read them. The environment is not inherited at
all: a microVM does not share the host's environment block, so ambient credentials
are not scrubbed out, they were never in the same kernel.

Your image therefore needs the analyzer and its runtime, and nothing else. It does
**not** need the rule set or the advisory database: those are provisioned on the
host, mounted in, and the digest Roteiro records for them is the digest of the
file it mounted.

### It never falls back, here either

No image, an image not in the local store, an image whose analyzer is not on
`PATH`, no hypervisor, a build without `exec-boxlite` — every one of those is a
refusal naming what is missing. None of them quietly becomes a host run, because
the `runner` and `isolation` recorded on the stored findings would then be a false
statement about how those findings were produced.

Running on this host is available and is something you say out loud:

```console
$ roteiro security run semgrep --allow-unsandboxed   # records isolation=none
```

## See also

- `docs/adr/0020-build-capable-sandboxed-execution.md` — why a builder may
  compile the repository at all, and the six conditions it runs under
- `docs/adr/0014-sandboxed-analyzer-execution.md` — the boundary itself
- `docs/OFFLINE_SETUP.md` — provisioning everything else, once
- `roteiro config` — which layer set each key, and which entries are refused
- `roteiro security status` — which images this build pins, which you declared,
  and which are in the local store

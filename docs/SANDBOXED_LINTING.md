# Sandboxed linting, and the image you have to supply

`roteiro lint clippy` runs the linter **inside a microVM** unless you have said
otherwise. This guide is the one thing you have to do first: supply the image it
runs in.

## Why there is something to do at all

Roteiro ships pinned images for its *reader* analyzers — `semgrep` has one, and
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

## The image

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

### A tag will be refused

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

## Configuring it

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

## Provisioning it

A run **never** pulls. Provisioning fetches, running reads — the same rule every
other pinned input follows, so a lint can never fail because a registry was
unreachable, nor succeed by quietly fetching something new.

```console
$ roteiro security prefetch --analyzer clippy --allow-download
pulling the sandboxed linter's image, from `[lint] image`: registry.example/you/rust-clippy@sha256:1234…
```

## Your dependencies have to be on this machine already

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

### What is mounted, and what is deliberately not

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

## The count can differ from your local `cargo clippy`, legitimately

Which lints fire is decided by the **image's** rustc, and it will not generally
match this machine's. A lint name is a symbol in a compiler, so a different
compiler fires a different set. `roteiro lint clippy` and `cargo clippy` in the
same tree on the same day can disagree with no defect on either side.

Nothing is stored (ADR-0020 v1.1), so this is a **surprise rather than a
corruption**: there is no history for a different compiler to falsify, and no
layer key for two toolchains to collide in. The report names the toolchain it
used, beside the image digest it came from, because that is the only way the
number is comparable to any other number.

## It never falls back

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

## See also

- `docs/adr/0020-build-capable-sandboxed-execution.md` — why a builder may
  compile the repository at all, and the six conditions it runs under
- `docs/adr/0014-sandboxed-analyzer-execution.md` — the boundary itself
- `docs/OFFLINE_SETUP.md` — provisioning everything else, once

---
site-page: offline
site-nav: Offline
site-order: 20
---

# Working offline

Roteiro is **offline-capable, not offline-only**. Nothing it does at *use* time
requires a network — but several capabilities need assets that must be fetched
once, deliberately, while you still have one. This guide is the "once".

The rule it follows: **offline is preferred for working, not for setup.** Prepare
on a good connection, verify, then unplug.

Everything here is verifiable. A **default build** contacts exactly **three
hosts**, from exactly **two** call sites in the whole workspace:

| Host | What | Reached by |
| --- | --- | --- |
| `huggingface.co` | GGUF models | `roteiro model pull` |
| `ocrs-models.s3-accelerate.amazonaws.com` | the OCR model only | `roteiro model pull ocrs-text` |
| `osv-vulnerabilities.storage.googleapis.com` | OSV databases | `roteiro security prefetch --allow-download` |

**`exec-boxlite` adds two more hosts and a third call site** (ADR-0014,
Stage 24). Leave the feature off and none of this applies:

| Host | What | Reached by |
| --- | --- | --- |
| `github.com` | the boxlite sandbox runtime archive | `roteiro security prefetch --analyzer sandbox --allow-download`, **or `boxlite`'s own build script** — see below |
| `docker.io` | the pinned analyzer image | `roteiro security prefetch --analyzer semgrep --allow-download` |

**One of those is reached at build time rather than run time, and that is the
trap.** Building `--features exec-boxlite` with `BOXLITE_RUNTIME_URL` unset lets
`boxlite`'s own build script `curl` the runtime archive from `github.com`. The
bytes are still verified — `rto-exec`'s build script checks every extracted file
against `crates/rto-exec/src/runtime_file_pins.rs` before anything links — but a
socket is opened, so **that build is not an offline build**. On a host with no
egress it fails inside `boxlite`, before Roteiro's own checks are reached.

To keep the build offline, provision the archive first and name it:

```sh
roteiro security prefetch --analyzer sandbox --allow-download
export BOXLITE_RUNTIME_URL="file://$HOME/.roteiro/security/boxlite-runtime/boxlite-runtime.tar.gz"
```

`boxlite`'s `curl` then reads that local file and opens no socket, and the
archive is verified before it is extracted as well as after.

The archive goes through the same `ureq` call site as everything above when
`prefetch` is what obtains it. The image does not: an OCI pull runs through
`oci-client` inside the `boxlite` dependency, so it is the one egress path that
is not first-party code. All of it is pinned — the archive by SHA-256 in
`crates/rto-exec/src/runtime_pins.rs`, the files it extracts to by SHA-256 in
`runtime_file_pins.rs`, the image by manifest digest in `SANDBOX_IMAGES` — so a
host serving different bytes fails the pin rather than being trusted.

Everything else — the semgrep baseline rules, the explorer's JavaScript, every
tree-sitter grammar — is compiled into the binary. There is no lazy fetch, no
implicit fallback and no phone-home. **Nothing Roteiro needs is unprefetchable**,
these two included: a sandboxed *run* never pulls, and refuses with
`ImageNotProvisioned` if the image was not fetched ahead of time.

---

## Step 0 — host toolchain

Before `cargo install`. A working C compiler and linker is a prerequisite of
Rust itself, not of Roteiro; the default build additionally compiles SQLite, 18
tree-sitter grammars, and `ring`'s crypto core (the TLS used by `model pull`)
from C and pregenerated assembly. That is the *same* toolchain class, not an
extra one — no C++, no cmake, no libclang.

```sh
# macOS
xcode-select --install          # linker, C/C++, and libclang
brew install cmake              # ONLY if you are building with `serve`

# Debian / Ubuntu
sudo apt install build-essential        # needed for the DEFAULT build
sudo apt install cmake libclang-dev     # ONLY if you are building with `serve`
```

`serve` compiles llama.cpp from source. Budget for it: on an 18-core machine
that stage alone is ~45 s; **on 2–4 cores expect 3–12 minutes.** Without `cmake`
or `libclang` it does not degrade — the build script panics and `cargo install`
fails outright. `protoc` is *not* required by any feature.

## Step 1 — install with the features you intend to use offline

A feature that is off is not "degraded", it is **absent**: the subcommand does
not exist in the parser and you get `unrecognized subcommand`. Choose now.

**Steps 2 and 3 need nothing extra.** `models` and `exec-subprocess` are both
default features, so a stock `cargo install roteiro` has `roteiro model pull`,
`roteiro security prefetch|status|run` — every command in this guide. That is
the point of the guide: preparing to work offline should not need a special
build.

```sh
cargo install roteiro                       # everything in this guide
cargo install roteiro --features serve      # + local model serving and inference
```

Neither default changes when bytes move. `model pull` is still consent-gated;
`security prefetch` still refuses to download without `--allow-download`; and
running an analyzer **on this host** still refuses without `--allow-unsandboxed`,
every time.

`security run` is sandboxed by default: with no flag it executes the analyzer
inside a digest-pinned OCI image in a microVM. That backend is `exec-boxlite`,
which is *not* a default feature, so on a stock install the default path is a
named refusal that spells out the rebuild rather than quietly running on the host
— see the README for the provisioning order.

> **`--allow-unsandboxed` matters more now, not less.** It is what selects the
> host: a third-party analyzer as a child process here with **no isolation
> boundary**, and the run's own evidence records `isolation=none`. That used to
> be gated twice — once at build time by asking for `exec-subprocess`, and once
> per run by the flag. The build-time half is gone now that the feature is a
> default, so the flag is the only thing left. The sandbox existing does not
> soften it either: nothing implies it, and no failure of the sandbox is ever
> answered by falling back to it. If you want neither, use `roteiro security
> ingest` and never execute anything locally.

If you want a build that provisions and ingests but genuinely **cannot** execute
an analyzer — a locked-down CI image, say — that is still one flag away:

```sh
cargo install roteiro --no-default-features --features execution
```

That build keeps `security ingest|list|prefetch|status` and refuses `security
run` with a message naming the feature it would need.

## Step 2 — warm the model store

Pull only the sections you will actually use. Every file is SHA-256 pinned in
the binary, consent-gated, and **resumable** — if a pull is interrupted, re-run
the same command and it continues over an HTTP range request.

```sh
roteiro model list                                    # store path + what is available

roteiro model pull bge-small-en-v1.5-gguf --yes       #   65 MiB  embeddings: infer, /v1/embeddings
roteiro model pull qwen3-0.6b             --yes       #  380 MiB  spec draft, serve chat + Ask
roteiro model pull ocrs-text              --yes       #   12 MiB  image-ocr only
roteiro model pull smolvlm-500m-gguf      --yes       #  520 MiB  image-vision only
roteiro model pull voxtral-mini-3b        --yes       # 3041 MiB  audio-transcribe only
```

Sizes to plan around: **445 MiB** for a text-only core (embeddings + a small
generative model), **~3.9 GiB** for the smallest useful pick across every
section, **~65.6 GiB** for the entire registry. Audio has no low tier —
`voxtral-mini-3b` at 3.0 GiB is the floor, and it is most of that 3.9 GiB.

A missing model **degrades rather than fails**:

```
$ roteiro spec draft "a test topic"
note: generative model `qwen3-0.6b` is not installed — emitting the scaffold.
      Draft prose with: roteiro model pull qwen3-0.6b
```

## Step 3 — warm the analyzer assets

Three assets, three different provisioning stories. **Order matters**: the clone
must precede the prefetch that pins it.

```sh
# 1. RustSec advisory DB — Roteiro never fetches this one; it is a git checkout
git clone --depth 1 https://github.com/RustSec/advisory-db \
  ~/.roteiro/security/rustsec-advisory-db/db            # ~6 MB, 1196 advisories

# 2. Pin it, plus the vendored semgrep rules (both fully offline operations)
roteiro security prefetch --analyzer cargo-audit
roteiro security prefetch --analyzer semgrep

# 3. OSV databases — the only asset Roteiro downloads for analyzers
roteiro security prefetch --analyzer osv-scanner --allow-download   # ~254 MiB
```

`--allow-download` is required and deliberate. Sizes: npm 209.3 MiB, PyPI 31.8,
Maven 9.6, crates.io 3.2.

> **Do this on a network you trust.** The OSV snapshot has no compile-time digest
> — OSV republishes daily, so there is nothing stable to pin against. The *first*
> fetch is trust-on-first-use; every run afterwards re-verifies against the
> digest recorded then. That makes *when* you prefetch a security decision.

### The analyzers themselves are yours to install

Roteiro provisions rules and databases; it never installs `semgrep`,
`osv-scanner` or `cargo`. If a binary is missing, `security run` says so by name
and points at `roteiro security ingest`:

```
analyzer binary `semgrep` not found on PATH (needed to run `semgrep`). Roteiro does
not install analyzers; install it yourself, or produce the report elsewhere and
use `roteiro security ingest`.
```

**None of the three is mandatory.** Install only the ones whose axis you want —
they overlap deliberately little:

| Analyzer | `security run --analyzer` | What it finds | Languages |
| --- | --- | --- | --- |
| **semgrep** | `semgrep` | Static analysis (SAST) of your *own* code, against a rule set vendored in the binary | Rust, Python, Java, JavaScript, TypeScript, SQL (generic mode) |
| **cargo-audit** | `cargo-audit` | RustSec advisories against `Cargo.lock` — your *dependencies* | Rust only |
| **osv-scanner** | `osv-scanner` | OSV.dev advisories against resolved lockfiles — your *dependencies*, across ecosystems | Python, Java, JavaScript, TypeScript, Rust |

`cargo-audit` and `osv-scanner` overlap on Rust and answer slightly differently;
[ADR-0018](adr/0018-analyzer-coverage-matrix.md) is the record of why, and
`roteiro security list` cross-references them rather than double-counting.

```sh
# macOS
brew install semgrep
brew install osv-scanner
cargo install cargo-audit          # NOT a standalone binary — see below

# Debian / Ubuntu
python3 -m pip install semgrep     # no apt package; pipx works too
#   osv-scanner: no apt package — take a release binary from
#   https://github.com/google/osv-scanner/releases and put it on PATH,
#   or `go install github.com/google/osv-scanner/v2/cmd/osv-scanner@latest`
cargo install cargo-audit
```

**`cargo-audit` is the odd one out.** It is a **cargo subcommand**, not a
standalone program: `cargo install cargo-audit` puts `cargo-audit` in
`~/.cargo/bin` and Roteiro invokes it as `cargo audit`. Do not go looking for a
`cargo-audit` package in a system package manager — installing "the analyzer" for
this one means installing a Rust toolchain, which you already have if you built
Roteiro from source.

**No minimum version is enforced.** This was checked rather than assumed: nothing
in the adapters compares a version. `subprocess.rs` reads `--version` from the
binary and records it as *evidence* on the run — its own comment says "a version
is evidence, not a precondition" — and an analyzer that will not answer
`--version` is recorded as `unknown` and run anyway. So a version older than the
ones below will not be refused; it may simply behave differently from what the
adapters were written against. Those reference versions, for a known-good
comparison, are the ones Stage 22/22b were developed and measured on:

| Analyzer | Developed and measured against |
| --- | --- |
| `semgrep` | **1.173.0** (subprocess/sandbox parity run, 4 identical findings) |
| `osv-scanner` | **2.5.0** (fixtures are real captured output at this version) |
| `cargo-audit` | **0.22.2** (`0.21.2` for the committed report fixtures) |

### `roteiro lint` is a different command with a different answer

`roteiro lint <analyzer>` runs a **linter** — today `clippy` — and prints what it
said. It is not in the table above and never will be, because it does not produce
an artifact: no findings layer, no `lint list`, nothing for `security list` or
`roteiro export` to show afterwards
([ADR-0020](adr/0020-build-capable-sandboxed-execution.md) v1.1). An advisory id
is *assigned*, and assignment is a promise — `RUSTSEC-2020-0071` will mean the
same thing in five years. A lint name is a symbol in a compiler, renamed or
removed at its discretion.

That is why the count it prints is a point in time and not a trend, and it says
so beneath every report:

- a **renamed** lint reads as one defect fixed and one introduced;
- a **removed** lint reads as fixed;
- an edit to `[workspace.lints]`, or an added `#[allow]`, makes whole cohorts
  appear or vanish — a configuration change reading as a code change.

None of those touched the code. Neither does a toolchain bump or a different
`--all-features`, which is why the report names the linter's version, the rustc
version and host, and the feature set it resolved with.

#### It is sandboxed by default, so out of the box it refuses

`roteiro lint` runs the linter in a sandbox unless you have said otherwise
([ADR-0020](adr/0020-build-capable-sandboxed-execution.md) §6) — and **the
sandboxed builder is not built yet**, so with no grant the command explains that
and stops, having run nothing.

That is on purpose, not a gap someone forgot to fill. `clippy` has `cargo check`
semantics: linting compiles the tree, which executes its build scripts and loads
its proc macros with your filesystem and your credentials — 54 build scripts and
7 proc macros in this repository by default, 87 and 33 under `--all-features`. In
your own repository that is the build you were going to run anyway. In a branch
you are reviewing it is somebody else's code, and that the *toolchain* is yours
does not make the *code* yours. Shipping host execution as the default *because*
the sandbox is unfinished would let a missing capability decide a question that
was supposed to be decided deliberately.

So you opt in, and **either of these is enough — you do not need both**:

```sh
# for one run
roteiro lint clippy --allow-unsandboxed

# or standing, in your own ~/.roteiro/config.toml (never in roteiro.toml)
[lint]
allow_unsandboxed = true
```

`--sandboxed` asks for the sandbox explicitly, which is how you opt one run back
out of a standing grant. It refuses today, and **it never falls back**: asking
for isolation and getting execution is the one outcome this command will not
produce.

The grant is layered the way [ADR-0019](adr/0019-remote-model-tier.md) layers the
remote tier, and for the same reason — `roteiro.toml` is committed and shared:

| Layer | May deny | May grant |
|---|---|---|
| Built-in default | sandboxed by default | — |
| Project `roteiro.toml` | **yes** | **no** |
| User `~/.roteiro/config.toml` | yes | **yes** |
| `--allow-unsandboxed` / `--sandboxed` | yes | **yes** |

A merged line that starts running builds on every teammate's machine is consent
granted by someone else and noticed by nobody, so a committed file may switch the
sandbox *on* for everyone and may never switch it *off* for anyone. It differs
from `[remote] enabled` in one respect: there the user layer and the flag must
**both** grant, here **either** suffices — otherwise you would still type the
flag every run and the key would be pointless. `roteiro config` prints the key,
both layers, and which one decided.

```sh
roteiro lint clippy --allow-unsandboxed                 # default features
roteiro lint clippy --allow-unsandboxed --all-features  # a different, also-true number
roteiro lint clippy --allow-unsandboxed --json          # with `"stored": false`
```

#### What it needs, and what it does not

It needs a Rust toolchain with the component installed: a missing `cargo` or a
toolchain without clippy is an **error naming what to install**, never an empty
report — "no diagnostics" and "nothing ran" must not look the same. It does
**not** need a prefetch, because a linter has no pinned rule set to provision:
its rules *are* the toolchain plus this repository's own `[workspace.lints]`.

A granted run still records and prints `isolation: none`, and still discloses its
argv before starting. A grant changes who chose, not what happened.

### Or install none of them: `roteiro security ingest`

`ingest` accepts a normalized report produced **anywhere** — a CI job, a
colleague's machine, a container image that has the analyzer you do not want on
your laptop — and files it as a findings layer that is byte-for-byte the shape a
local run produces. It is seam (c) of
[ADR-0014](adr/0014-sandboxed-analyzer-execution.md) and the zero-install path,
not a consolation prize: `security list`, cross-referencing and the staleness
reporting all work identically over an ingested layer, and the run's evidence
records `isolation=ingested` so nothing is being claimed that was not done.

For a machine that must never execute an analyzer at all, build it out:
`cargo install roteiro --no-default-features --features execution`.

## Step 4 — verify before you unplug

```sh
roteiro model list                # each model: installed, and its size on disk
roteiro security status           # each asset: digest, when fetched, DB age
```

`security status` is the one to read on the way to the airport. It reports each
asset's digest and fetch time, and how old the advisory database is — a stale DB
still runs, but every result is marked possibly-stale rather than current.

A run **never** provisions. On a cold cache it refuses and names the fix rather
than reaching for the network or falling back to whatever the host happens to
have installed:

```
Error: assets-unavailable-offline: semgrep cannot run because its pinned inputs are not provisioned
  missing: semgrep-rules (not yet pinned; never provisioned)
  fix it with: roteiro security prefetch --analyzer semgrep
```

---

## Air-gapped machines

Both stores accept assets placed by hand, and `prefetch` will digest and pin
them **without opening a socket**.

**Models.** Declining the consent prompt prints the exact URL and destination:

```
roteiro would download model `qwen3-0.6b` (~380 MiB, Apache-2.0) from:
  https://huggingface.co/unsloth/Qwen3-0.6B-GGUF/resolve/main/Qwen3-0.6B-Q4_K_M.gguf

non-interactive: not downloading. Re-run with `--yes`, or fetch manually into
  <store>/qwen3-0.6b
```

Fetch it elsewhere, copy it to that path, and `roteiro model list` will see it.

**Analyzer assets.** Place them at these exact paths, then run `prefetch`
*without* `--allow-download` to pin them:

```
~/.roteiro/security/rustsec-advisory-db/db/                 # the advisory-db checkout
~/.roteiro/security/osv-db/db/osv-scalibr/<ECOSYSTEM>/all.zip
```

## Store locations

| Store | Resolution order |
| --- | --- |
| Models | `ROTEIRO_MODEL_STORE` → `ROTEIRO_HOME/models` → `~/.roteiro/models` |
| Analyzer assets | `ROTEIRO_SECURITY_ASSETS` → `ROTEIRO_HOME/security` → `~/.roteiro/security` |

Set `ROTEIRO_HOME` once to relocate both — useful for putting several gigabytes
of models on an external disk.

## Known rough edges

- **`roteiro security prefetch --analyzer osv-scanner`, without the flag, prints
  a fix instruction that is the command you just ran.** Following it verbatim
  never terminates. Add `--allow-download`.
- **The RustSec DB has no Roteiro command that fetches it.** A git checkout has
  no stable single-file URL, so it is `External` by design and the clone above is
  the only route.
- **`roteiro init --fetch` is not offline-friendly.** The hook it writes calls
  `gh release download` on every freshness check, which needs the `gh` CLI —
  Roteiro neither ships it nor checks for it. It degrades to a local rebuild on
  failure, so it is a soft edge rather than a hole, but offline users should
  simply not pass `--fetch`.

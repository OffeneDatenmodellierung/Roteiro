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
|---|---|---|
| `huggingface.co` | GGUF models | `roteiro model pull` |
| `ocrs-models.s3-accelerate.amazonaws.com` | the OCR model only | `roteiro model pull ocrs-text` |
| `osv-vulnerabilities.storage.googleapis.com` | OSV databases | `roteiro security prefetch --allow-download` |

Everything else — the semgrep baseline rules, the explorer's JavaScript, every
tree-sitter grammar — is compiled into the binary. There is no lazy fetch, no
implicit fallback and no phone-home. **Nothing Roteiro needs is unprefetchable**:
an analyzer *run* never provisions, and refuses with `assets-unavailable-offline`
if its pinned inputs were not fetched ahead of time.

There used to be two more hosts, reached only by the opt-in `exec-boxlite`
microVM sandbox: `github.com` for its runtime archive and `docker.io` for the
pinned analyzer image. That backend is removed
([ADR-0030](adr/0030-remove-the-microvm-sandbox.md)).

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

> **`--allow-unsandboxed` is the only gate.** Roteiro has no sandbox: the
> opt-in microVM backend was removed
> ([ADR-0030](adr/0030-remove-the-microvm-sandbox.md)), so `security run` always
> runs a third-party analyzer as a child process here with **no isolation
> boundary**, and the run's own evidence records `isolation=none`. That used to
> be gated twice — once at build time by asking for `exec-subprocess`, and once
> per run by the flag. The build-time half is gone now that the feature is a
> default, so the flag is the only thing left, and saying nothing refuses. If you
> want neither, use `roteiro security ingest` and never execute anything locally.

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
`osv-scanner` or `cargo`. If a binary is missing, `security run` names it, says
how to obtain it, and names the path that needs no binary at all:

```
Error: analyzer binary `semgrep` not found on PATH (needed to run `semgrep`), so nothing ran.
  Roteiro does not install analyzers, and has not installed this one. Semgrep's own install page recommends:
    pipx install semgrep
  Upstream: https://docs.semgrep.dev/getting-started/quickstart
  Or run the analyzer elsewhere — CI, a colleague's machine — and read its report in with `roteiro security ingest`, which needs nothing on PATH and produces the same findings as a local run.
```

The install line comes from the adapter, so it is specific to the **program**
that is missing rather than to the analyzer: a missing `cargo` gets rustup's
page, a missing `cargo-audit` gets `cargo install cargo-audit`, and
`osv-scanner` — which documents no single command — gets its install page
instead of a guess. Saying how is not doing it: nothing on this path installs
anything.

**None of the three is mandatory.** Install only the ones whose axis you want —
they overlap deliberately little:

| Analyzer | `security run --analyzer` | What it finds | Languages |
|---|---|---|---|
| **semgrep** | `semgrep` | Static analysis (SAST) of your *own* code, against a rule set vendored in the binary | Rust, Python, Java, JavaScript, TypeScript, SQL (generic mode) |
| **cargo-audit** | `cargo-audit` | RustSec advisories against `Cargo.lock` — your *dependencies* | Rust only |
| **osv-scanner** | `osv-scanner` | OSV.dev advisories against resolved lockfiles — your *dependencies*, across ecosystems | Python, Java, JavaScript, TypeScript, Rust |

`cargo-audit` and `osv-scanner` overlap on Rust and answer slightly differently;
[ADR-0018](adr/0018-analyzer-coverage-matrix.md) is the record of why, and
`roteiro security list` cross-references them rather than double-counting.

Every command below is the one that project's own install page documents. None
of them is invented here — which is the thing worth knowing when you are reading
this because the tool is not yet in front of you.

This block is deliberately wider than any refusal: when a binary is missing,
`security run` prints the single hint for the program it could not find, and
none of the alternatives below. Treat the two as the same *source*, not as the
same *text* — only the fenced error block above is quoted verbatim from the
code, and only that one is held there by a test.

```sh
# semgrep — https://docs.semgrep.dev/getting-started/quickstart
pipx install semgrep               # what upstream lists first
uv tool install semgrep            # upstream's alternative
#   `brew install semgrep` also exists; upstream's own note is that it "often
#   lags behind the latest release".

# cargo-audit — https://github.com/rustsec/rustsec/tree/main/cargo-audit
cargo install cargo-audit          # NOT a standalone binary — see below

# osv-scanner — https://google.github.io/osv-scanner/installation/
#   No single canonical command. Upstream lists one entry per platform (scoop,
#   winget, brew, pacman, apk, pkg, pkg_add), a prebuilt SLSA3 binary, and
#   `go install github.com/google/osv-scanner/v2/cmd/osv-scanner@latest` for
#   anyone who already has Go. Pick from that page rather than from this one:
#   it is the copy that gets updated.
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
|---|---|
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

#### It runs on this host, so out of the box it refuses

`roteiro lint` runs the linter on this machine, and only once you have said so
([ADR-0020](adr/0020-build-capable-sandboxed-execution.md) §6, as amended by
[ADR-0030](adr/0030-remove-the-microvm-sandbox.md)). With no grant the command
explains how to give one and stops, having run nothing.

That is on purpose, not a gap someone forgot to fill. `clippy` has `cargo check`
semantics: linting compiles the tree, which executes its build scripts and loads
its proc macros with your filesystem and your credentials — 54 build scripts and
7 proc macros in this repository by default, 87 and 33 under `--all-features`. In
your own repository that is the build you were going to run anyway. In a branch
you are reviewing it is somebody else's code, and that the *toolchain* is yours
does not make the *code* yours. There is no sandbox to run it in instead — the
microVM backend was removed — so the only safe default is to ask.

So you opt in, and **either of these is enough — you do not need both**:

```sh
# for one run
roteiro lint clippy --allow-unsandboxed

# or standing, in your own ~/.roteiro/config.toml (never in roteiro.toml)
[lint]
allow_unsandboxed = true
```

The grant is inverted, because `roteiro.toml` is committed and shared:

| Layer | May deny | May grant |
|---|---|---|
| Built-in default | refuses | — |
| Project `roteiro.toml` | **yes** | **no** |
| User `~/.roteiro/config.toml` | yes | **yes** |
| `--allow-unsandboxed` | — | **yes** |

A merged line that starts running builds on every teammate's machine is consent
granted by someone else and noticed by nobody, so a committed file may deny host
execution for everyone and may never grant it for anyone; a project denial is not
overridden by the flag. **Either** the user layer or the flag suffices — requiring
both would mean typing the flag every run, and the key would be pointless.
`roteiro config` prints the key, both layers, and which one decided.

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

Read the `analyzers` block, not only the `assets` block. Each analyzer reports one
of three states, because provisioning and installing are different things with
different fixes:

```
analyzers
  semgrep      ready                          [rust, python, …]
  cargo-audit  binary not found: cargo-audit  [rust]
               not on PATH: cargo-audit — Roteiro does not install analyzers; …
  osv-scanner  assets not provisioned         [python, java, …]
               run `roteiro security prefetch` to provision its assets
```

`ready` means both: the pinned assets are provisioned **and** the analyzer's own
program is on `PATH`. `roteiro security prefetch` fixes the second row's
neighbour and cannot fix the second row — Roteiro never installs an analyzer, so
that one is yours to install before you unplug. This is the check worth doing on
the ground: `prefetch` needs the network, and so does whatever you would install.

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
|---|---|
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

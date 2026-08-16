# Working offline

Roteiro is **offline-capable, not offline-only**. Nothing it does at *use* time
requires a network — but several capabilities need assets that must be fetched
once, deliberately, while you still have one. This guide is the "once".

The rule it follows: **offline is preferred for working, not for setup.** Prepare
on a good connection, verify, then unplug.

Everything here is verifiable. Roteiro contacts exactly **three hosts**, from
exactly **two** call sites in the whole workspace:

| Host | What | Reached by |
| --- | --- | --- |
| `huggingface.co` | GGUF models | `roteiro model pull` |
| `ocrs-models.s3-accelerate.amazonaws.com` | the OCR model only | `roteiro model pull ocrs-text` |
| `osv-vulnerabilities.storage.googleapis.com` | OSV databases | `roteiro security prefetch --allow-download` |

Everything else — the semgrep baseline rules, the explorer's JavaScript, every
tree-sitter grammar — is compiled into the binary. There is no lazy fetch, no
implicit fallback and no phone-home. **Nothing Roteiro needs is unprefetchable.**

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

**Step 2 needs nothing extra.** `models` is a default feature, so `roteiro model
pull` exists in a stock `cargo install roteiro` and the model half of this guide
works out of the box. Its presence changes nothing about when bytes move:
`pull` is still consent-gated, and no other command opens a socket.

```sh
cargo install roteiro                                     # pull models (default)
cargo install roteiro --features exec-subprocess          # + analyzer prefetch/run/status
cargo install roteiro --features serve                    # + local serving and inference
```

Note that `exec-subprocess` is **not** implied by the default build or by
`serve`. If you want `roteiro security prefetch` to exist — Step 3 below — ask
for it explicitly. That asymmetry is deliberate: `exec-subprocess` runs
third-party analyzer binaries on the host with no isolation boundary, which is a
larger thing to hand someone unasked than a consent-gated downloader.

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

**The analyzers themselves are yours to install.** Roteiro provisions rules and
databases; it never installs `semgrep`, `osv-scanner` or `cargo`. If a binary is
missing it says so and points at `roteiro security ingest` as the alternative.

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

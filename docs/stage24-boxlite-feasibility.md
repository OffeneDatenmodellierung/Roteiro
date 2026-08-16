# Stage 24 — `boxlite` feasibility findings (blocking)

Status: **implementation not started; blocked pending an owner decision.**
Date: 2026-08-16. Branch: `feat/stage24-boxlite-runner`.

This note records what was measured, so the decision is made from evidence rather
than from either of the two earlier guesses (*"boxlite is git-only, therefore
unmergeable"* — wrong; *"boxlite is published, therefore an ordinary registry
dependency"* — also wrong, in a different way).

## What was verified as sound

These are positive results and should not be re-derived:

* **`RunnerKind` already names all three backends.** `crates/rto-graph/src/findings.rs`
  defines `Ingested | Subprocess | Sandboxed` with `as_str`/`from_token` covering
  each. Stage 21's contract holds: **no migration, no schema change, no edit to
  that file.** Latest migration stays **13**; `EXTRACT_VERSION` stays **11**.
* **`boxlite` is genuinely published.** Sparse index `index.crates.io/bo/xl/boxlite`:
  17 versions, newest **0.9.7**, not yanked (0.9.6 is yanked). Apache-2.0.
* **The API is a good fit for ADR-0014.** `BoxOptions` carries exactly the knobs the
  ADR asks for: `RootfsSpec::Image(<digest-pinned ref>)`, `VolumeSpec { read_only }`,
  `NetworkSpec::Disabled`, an explicit `env` list, `user`, `cmd`. `SystemCheck::run()`
  is a ready-made capability probe. `ImageHandle::{pull,list}` gives warm-cache
  detection and `ImageInfo.id` is the manifest digest — the image-digest evidence.
* **The isolation properties are real, and were demonstrated on hardware**
  (macOS 26 / Apple Silicon, a live alpine microVM against a pinned manifest digest):

  | Property | Observed |
  |---|---|
  | read-only worktree mount | `touch /work/nope` → `Read-only file system`, rc=1 |
  | egress denied | only `lo` + a `DOWN` `dummy0`; `wget` → `bad address` |
  | scrubbed environment | guest env was exactly `BOXLITE_EXECUTOR, HOME, LC_ALL, PATH, PWD, SHLVL` — no ambient credentials |
  | digest-pinned image | box created from `docker.io/library/alpine@sha256:45e09956…` |

  **The design is not the problem.** The packaging is.

## The blocker: what `boxlite` on crates.io actually is

`boxlite` 0.9.7 does not build a hypervisor from the registry. Its three native
`-sys` crates each detect that they were fetched from crates.io and disable
themselves:

```rust
// libkrun-sys-0.9.7/build.rs, and identically in e2fsprogs-sys, bubblewrap-sys
if manifest_dir.join(".cargo_vcs_info.json").exists() {
    unsafe { env::set_var("BOXLITE_DEPS_STUB", "2") };   // published package
}
```

`libkrun-sys` declares `exclude = ["vendor"]`, so the sources it would otherwise
build are not in the published package at all. Building with `features = ["krun"]`
therefore compiles but **fails to link** on macOS aarch64:

```
Undefined symbols for architecture arm64:
  "_krun_create_ctx", "_krun_set_root", "_hv_vm_create", …  (26 symbols)
```

Instead, `boxlite`'s own `build.rs` downloads a prebuilt tarball, `include_bytes!`s
it into the rlib, and extracts + execs it at run time:

```rust
let default_url = format!(
    "https://github.com/boxlite-ai/boxlite/releases/download/v{version}/boxlite-runtime-v{version}-{target}.tar.gz");
let url = env::var("BOXLITE_RUNTIME_URL").unwrap_or(default_url);
match Self::download_file(&url, &tarball_path) { … }   // plain curl
```

### Four consequences, each measured

1. **The build downloads ~25–60 MB of unverified executables.**
   `download_file` is a bare `curl -fsSL`. There is **no expected-digest check**.
   Searched four ways, all NOT FOUND:
   `grep -nE 'expected|EXPECTED|_SHA256|SHA256:|checksum|digest' build.rs` (one hit,
   a doc comment); `grep -rniE 'sha256|checksum|integrity|signature|sigstore|cosign' build.rs`
   (5 hits, all cache-invalidation or post-hoc hashing); `grep -nE '"[0-9a-f]{64}"' build.rs`;
   `grep -rnE '"[0-9a-f]{64}"' src build.rs`. The only expected-digest constants in
   the tree live in `libkrun-sys/build.rs`, on the code path that stub mode returns
   before reaching. The URL is additionally overridable via `BOXLITE_RUNTIME_URL`.

   This is the exact inverse of ADR-0014's own provisioning contract — *"Assets are
   pre-downloaded and digest-pinned, never fetched implicitly … never fetch
   implicitly"* — one layer below where the ADR was looking.

2. **`--features exec-boxlite` cannot be built offline.** With the download
   unreachable the build is a hard failure (good: not silent), but a failure:
   `error: failed to run custom build command for boxlite v0.9.7 … exit status: 101`.
   A stage whose justification is *offline-capable* would ship a feature that
   cannot be **compiled** on a plane.

3. **`cargo deny` gives a false green.** `cargo deny --config deny.toml check licenses`
   over the resolved 398-package closure reports `licenses ok`. It is right about the
   crates and blind to what ships, because the GPL-family code is inside the
   downloaded tarball, not the crate sources. Contents of
   `boxlite-runtime-v0.9.7-linux-x64-gnu.tar.gz` (the CI platform),
   sha256 `9ae495f55d363e6af04640ab55025ac80b4bf4762e38fa0b8ac80c7604e3148c`:

   | File | Provenance | Licence |
   |---|---|---|
   | `bwrap` | bubblewrap | **LGPL-2.0-or-later** |
   | `mke2fs`, `debugfs` | e2fsprogs | **GPL-2.0** |
   | `libkrunfw.so.5` | a Linux guest kernel | **GPL-2.0** |
   | `boxlite-shim`, `boxlite-guest` | boxlite | Apache-2.0 |

   These are exec'd as separate processes, so the usual reading is aggregation —
   this does **not** make Roteiro's own source GPL, and `MIT OR Apache-2.0` is
   untouched. But they are embedded in the rlib and therefore **distributed inside
   the `roteiro` binary**, which engages GPL-2.0 §3 source-offer and LGPL relinking
   obligations. That is a distribution duty, and per `deny.toml`'s own standard
   (*"a licence outside the plain-permissive set is a decision, never an
   expedient"*) it is the owner's to accept, with a recorded rationale — not
   something to be acquired silently through a gate that cannot see it.

   Separately and more mildly: `pathrs@0.2.5` (`MPL-2.0 OR LGPL-3.0-or-later`) is a
   **second MPL-2.0 crate**, and `deny.toml` explicitly says *"If a second MPL-2.0
   crate ever appears in the tree, re-run that reasoning for it."* It resolves to
   the MPL side and is Linux-only; low stakes, but the trigger the file asks for.

4. **A new host toolchain requirement.** `boxlite-shared/build.rs` requires
   `protoc >= 3.12` unconditionally — no stub escape:
   `Error: "Failed to determine protoc version: No such file or directory (os error 2).
   boxlite requires protoc >= 3.12."` That applies to CI **and** to
   `cargo install roteiro --features exec-boxlite`.

### Why the DoD cannot be demonstrated as written

* *"Same findings via subprocess and via boxlite"* — the subprocess side needs an
  analyzer on the host. `semgrep`, `osv-scanner` and `docker` are **not installed**
  here (`cargo-audit` is), so parity would compare a host `cargo-audit` against a
  container that would have to carry the identical version to differ *only* in
  isolation label and image digest.
* *"No network but a warm cache produces a full run"* — demonstrable at **run**
  time, but false at **build** time (point 2), which is the stronger claim the
  stage rests on.
* *"`cargo deny` clean on the resolved tree"* — passes, but does not gate what
  ships (point 3), so reporting it as met would be misleading.

## Recommendation

Do **not** land `boxlite` as a dependency yet. In preference order:

* **(a) Defer the backend, keep the stage's other value.** Ingest + subprocess
  already carry the functional coverage. Raise an upstream issue asking for a
  pinned digest on the runtime tarball (a one-constant change; `libkrun-sys`
  already has `Fetcher::verify_sha256`), and revisit when it lands.
* **(b) Land seam-only, clearly labelled.** `BoxliteRunner` behind `exec-boxlite`,
  the `SystemCheck::run()` capability probe, and the parity harness — with CI
  compiling under `BOXLITE_DEPS_STUB=1` (verified: clean 8.7s compile, no network,
  and a visible run-time refusal, `Binary 'boxlite-guest' not found`). Honest, but
  Stage 24's DoD would stay open, and it still costs ~398 lockfile packages and a
  `protoc` requirement.
* **(c) Accept with a recorded rationale** — the GPL/LGPL distribution duty, the
  unverified build-time fetch, and the `protoc` requirement — and pin the runtime
  tarball digests ourselves in `assets.rs`, verifying post-extraction. This does
  not remove the unverified fetch; it only detects it after the fact.

## Coordination note

`Cargo.lock` is shared with the in-flight Stage 26 worker. Adding boxlite moves it
by ~398 packages and guarantees a conflict, so it should not be done speculatively.

## Reproduction

Probe sources: `scratchpad/boxprobe` (default features, live microVM run) and
`scratchpad/offprobe` (`BOXLITE_RUNTIME_URL` unreachable; `BOXLITE_DEPS_STUB=1`).
Host: macOS 26.6 / Apple Silicon, Rust 1.97.1, `protobuf` and `pkg-config`
installed via Homebrew for the experiment.

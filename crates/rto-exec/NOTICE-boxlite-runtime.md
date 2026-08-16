# NOTICE — third-party binaries embedded by the `exec-boxlite` feature

This notice applies **only** to builds with `--features exec-boxlite`. A default
`roteiro` build embeds none of the software below and this notice does not apply
to it.

## What is embedded, and how

`exec-boxlite` depends on [`boxlite`](https://github.com/boxlite-ai/boxlite)
v0.9.7 (Apache-2.0). When `boxlite` is compiled from crates.io it does not build
a hypervisor from source; instead its build script takes a prebuilt runtime
archive, embeds the files with `include_bytes!`, and extracts them at first use
to a per-user cache, where they are executed as **separate processes**.

Roteiro provisions and digest-verifies that archive itself before any build can
use it — see `crates/rto-exec/src/runtime_pins.rs` for the pinned SHA-256 of each
platform's artifact.

Because the files are embedded in the compiled library, a distributed `roteiro`
binary built with this feature **distributes** them. The following are therefore
distributed with it:

| File | Upstream project | Licence | Source |
|---|---|---|---|
| `boxlite-shim` | boxlite | Apache-2.0 | <https://github.com/boxlite-ai/boxlite> |
| `boxlite-guest` | boxlite | Apache-2.0 | <https://github.com/boxlite-ai/boxlite> |
| `libkrunfw.so.5` / `libkrunfw.5.dylib` | libkrunfw (a packaged Linux kernel) | **GPL-2.0** | <https://github.com/containers/libkrunfw>, <https://www.kernel.org/> |
| `mke2fs` | e2fsprogs | **GPL-2.0** | <https://github.com/tytso/e2fsprogs> |
| `debugfs` | e2fsprogs | **GPL-2.0** | <https://github.com/tytso/e2fsprogs> |
| `bwrap` (Linux only) | bubblewrap | **LGPL-2.0-or-later** | <https://github.com/containers/bubblewrap> |

The archives themselves are published at
<https://github.com/boxlite-ai/boxlite/releases/tag/v0.9.7> as
`boxlite-runtime-v0.9.7-<platform>.tar.gz`.

## What this does and does not mean

**It does not make Roteiro's own source GPL.** These are complete, independent
programs, executed as separate processes with their own address spaces. Roteiro
communicates with them across a process boundary, which is aggregation rather
than derivation; Roteiro's `MIT OR Apache-2.0` licensing is unaffected, and so
is the licensing of anything you build with it.

**It does create distribution obligations**, because the binaries travel inside
the artifact:

* **GPL-2.0 §3 — source availability.** Anyone who receives a `roteiro` binary
  built with `exec-boxlite` is entitled to the complete corresponding source of
  `libkrunfw`, `mke2fs` and `debugfs`. The upstream URLs above are the standing
  offer; the exact revisions are those of the pinned release artifacts.
* **LGPL-2.0-or-later — relinking.** `bwrap` is distributed unmodified and is
  executed, never linked, so the relinking obligation is satisfied by the
  availability of its unmodified source at the URL above.
* **Apache-2.0 §4 — notice retention.** This file is that retention.

None of these binaries is modified by Roteiro. They are distributed byte-for-byte
as published, which is what keeps the obligations to notice-and-source rather
than to publishing modifications.

## If you would rather not distribute them

Build without `--features exec-boxlite`. Analyzer ingest is always available and
needs no feature at all; the subprocess backend (`--features exec-subprocess`)
adds no third-party binaries. The sandboxed backend is the only path that
embeds anything listed here.

---
site-page: sandboxing
site-nav: Host execution
site-order: 22
---

# Running analyzers and linters on this host

Two commands run somebody else's code on your machine:

| Command | What runs | What it can execute |
|---|---|---|
| `roteiro lint clippy` | a **builder** — it compiles your tree | your tree's build scripts and proc macros |
| `roteiro security run <analyzer>` | a **reader** — it parses your tree | whatever the tree's config tells the tool to load |

Both run as a **child process on this host, with no isolation boundary**. Roteiro
has no sandbox: the opt-in microVM backend (`exec-boxlite`) was removed in
[ADR-0030](adr/0030-remove-the-microvm-sandbox.md), and nothing replaced it — no
container fallback, no other backend. So the only thing between a tree you did
not write and your filesystem and credentials is **your consent**, and neither
command assumes it.

**One document rather than two**, because the rules are one set of rules: neither
runs without a grant, neither falls back to anything when refused, and both
record `isolation: none` so the evidence says what actually happened.

## Why "reader" is not "safe"

It is tempting to think only builders execute repository code. Measured, a
`.pylintrc` containing nothing but an `init-hook=` line was executed by `pylint`,
silently; the same shape holds for `mypy` (`plugins=`), `sqlfluff` and `eslint`.
Whether a tool executes code is a property of **the repository**, not of the tool
([ADR-0020](adr/0020-build-capable-sandboxed-execution.md) §6). That is why
neither command has a per-tool exemption: the consent gate applies to every run.

## `roteiro lint`: granting host execution

`roteiro lint clippy` refuses unless host execution has been granted, and the
refusal says how to grant it. **Either of these is enough — you do not need
both:**

```console
$ roteiro lint clippy --allow-unsandboxed        # this run
```

```toml
# ~/.roteiro/config.toml — standing, for you
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

A merged line granting it would be consent given by someone else and noticed by
nobody, so a repository's `roteiro.toml` may **deny** host execution for everyone
working in it (`allow_unsandboxed = false`) and may never grant it. In a
repository that denies it, `--allow-unsandboxed` does not override that, and the
refusal says so. `roteiro config` prints the key, every layer, and which one
decided.

It needs a Rust toolchain with the `clippy` component installed. A missing
`cargo` or a toolchain without clippy is an **error naming what to install**,
never an empty report. Nothing is stored
([ADR-0020](adr/0020-build-capable-sandboxed-execution.md) v1.1): the report
names the linter's version, the rustc version and the feature set it resolved
with, because a lint count is only comparable to one produced by the same
compiler.

## `roteiro security run`: the flag, every time

`roteiro security run <analyzer>` refuses without `--allow-unsandboxed`, and the
refusal offers the two ways forward: pass the flag, or run the analyzer somewhere
else and `roteiro security ingest` its report.

```console
$ roteiro security run semgrep --allow-unsandboxed   # records isolation=none
```

There is no standing key for this one. The flag is required on **every** run,
and since `exec-subprocess` is a default feature it is the only gate left — it
must not be weakened.

Roteiro never installs an analyzer. It provisions the pinned inputs a host run
reads — rule sets and advisory databases, via `roteiro security prefetch` (see
[Working offline](OFFLINE_SETUP.md)) — and refuses with
`assets-unavailable-offline` when they are missing, rather than fetching during
a run.

## Or execute nothing: `roteiro security ingest`

`ingest` accepts a normalized report produced **anywhere** — a CI job, a
colleague's machine, a container you run yourself — and files it as a findings
layer of exactly the shape a local run produces, recording `isolation=ingested`.
It needs no analyzer on this machine and no grant, and it is the zero-install
default ([ADR-0014](adr/0014-sandboxed-analyzer-execution.md)).

## If you used the sandbox before

The microVM backend is gone, and so is everything that existed only for it:

- `roteiro sandbox status` and `roteiro sandbox clear`, and the `sandbox_status`
  and `sandbox_clear` MCP tools.
- `roteiro security run --sandboxed`, `roteiro lint --sandboxed`,
  `roteiro lint --image` and `roteiro security prefetch --image`. Scripts passing
  them must drop them.
- `roteiro security prefetch --analyzer sandbox` and the image half of
  `prefetch`; the sandbox-images section of `security status`.
- The config keys `[lint] image` and the whole `[security]` table
  (`[security.images]`). A file still carrying them loads, and `roteiro` warns
  once per file that they are inert — delete them.
- The `exec-boxlite` Cargo feature and the `BOXLITE_RUNTIME_URL` variable. Drop
  `--features exec-boxlite` from install commands.

Leftover caches are not removed for you. Delete them by hand if present:

```sh
rm -rf ~/.roteiro/security/boxlite-home      # the old image store — can be many GB
rm -rf ~/.roteiro/security/boxlite-runtime   # the runtime archive
```

(Under `ROTEIRO_SECURITY_ASSETS` or `ROTEIRO_HOME/security` if you relocated the
store.) Findings recorded by an earlier sandboxed run stay readable: their
`runner` and `isolation` fields still say how they were produced.

## See also

- `docs/adr/0030-remove-the-microvm-sandbox.md` — why the sandbox was removed
- `docs/adr/0020-build-capable-sandboxed-execution.md` — why a builder may
  compile the repository at all, and the inverted grant
- `docs/adr/0014-sandboxed-analyzer-execution.md` — the execution seam and
  ingest-by-default
- `docs/OFFLINE_SETUP.md` — provisioning rule sets and advisory databases, once
- `roteiro config` — which layer set each key

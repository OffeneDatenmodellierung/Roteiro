---
Title: Remove the microVM sandbox — analyzers and linters run on the host, behind the consent gate, or not at all
Space: ARCH
Parent: ADRs

# ADR-specific metadata (unknown keys are ignored; used for indexing/search)
type: adr
adr-id: "0030"
status: Accepted                    # Draft | For Review | Accepted | Rejected | Superseded
architectural-significance: HIGH    # SOFT | LOW | MEDIUM | HIGH | VERY HIGH
domain: Security Tooling
decision-makers: ["The Roteiro Project Team"]
superseded-by:
version: "1.0"
last-modified: 2026-11-11
confluence-url:
---

# ADR-0030: Remove the microVM sandbox — analyzers and linters run on the host, behind the consent gate, or not at all

|  |  |
|---|---|
| **State** | Accepted |
| **Architectural Significance** | HIGH |
| **Domain** | Security Tooling |
| **Document version** | 1.0 |

## Reference

Supersedes the `boxlite` backend of
[[docs/adr/0014-sandboxed-analyzer-execution.md]] and the sandboxed-by-default
rule of §6 of [[docs/adr/0020-build-capable-sandboxed-execution.md]]. ADR-0014's
execution seam, ingest-by-default and the subprocess escape hatch stand, as does
ADR-0020 §6's inverted grant. Touches the dependency policy of
[[docs/adr/0017-dependency-security-policy.md]], whose recorded exceptions this
removal retires.

## Summary

Roteiro no longer has a sandbox. The opt-in `exec-boxlite` feature — OCI images
in a Linux microVM via `boxlite` — is removed, and **nothing replaces it**: no
docker or podman fallback, no other backend.

`roteiro security run` and `roteiro lint` keep one execution backend, the host
subprocess, behind the existing consent gate, which is **not weakened**.
`roteiro security ingest` stays the zero-install default and executes nothing.

## Context

ADR-0014 chose `boxlite` as an embeddable, daemonless microVM library, and
accepted its pre-1.0 maturity with duties attached. ADR-0020 §6 then made the
sandbox the default for `roteiro lint`. Measured against what it delivered, the
backend cost more than any other optional feature:

- **Most of `deny.toml`'s ignores.** Three `zstd` vulnerabilities
  (RUSTSEC-2026-0391, -0398, -0403) enter only through `boxlite`'s `zstd` ^0.13.
  The bundled-SQLite CVE (RUSTSEC-2026-0459) cannot be taken because `boxlite`
  requires `rusqlite` ^0.39 and `libsqlite3-sys` admits one version per graph —
  so an opt-in feature held back the SQLite every build ships. Six more ignores
  are unmaintained crates reached only through `boxlite` (`term_size`, `bincode`,
  `adler`, `pin-utils`, `atty`, `jwt`). `cargo deny` cannot scope an ignore to a
  feature, so each one silenced its advisory for the whole workspace.
- **~7.4k lines in `crates/rto-exec`**: the backend, a lint sandbox, an image
  store, image-reference and `file://` parsing, two pin tables and four test
  suites, plus a build script that downloaded the runtime archive and verified
  every extracted file against per-file digests.
- **Embedded GPL-2.0 and LGPL-2.0 executables** (`mke2fs`, `debugfs`,
  `libkrunfw`, `bwrap`) carrying source-offer duties for anyone distributing a
  build with the feature.
- **CI and contributor friction**: `protoc` on every runner, a provisioning
  action, two provisioning scripts, and a ~435 MB analyzer image a contributor
  had to pull before `cargo test --workspace --all-features` would pass.
- **Upstream churn**: `boxlite` broke its generated API in patch releases twice,
  and needed its sibling crate pinned in lockstep (ADR-0014 v1.9, v1.10).

ADR-0014 itself named the security argument for a VM boundary as *"weaker than
it first appears"* for parse-only analyzers; ADR-0020 showed it is real for any
tree whose config a tool executes. The consent gate already covers that case by
refusing to execute without a grant. The sandbox added a second way to say yes,
at the costs above.

## Decision makers

The Roteiro Project Team (owner decision, 2026-11-11).

## Recommended option

### 1. Remove the backend and everything that exists only for it

- The `exec-boxlite` feature in `rto-exec` and `roteiro`; the `boxlite` and
  `boxlite-shared` dependencies and the async runtime they pulled in;
  `crates/rto-exec/build.rs` and `crates/rto-exec/NOTICE-boxlite-runtime.md`.
- The sandbox, image-store, pin and image-reference modules of `rto-exec`, and
  the `backend_parity`, `build_script_fetch_audit`, `runtime_pin_integrity` and
  `sandboxed_builder` tests.
- `scripts/derive-runtime-file-pins.py`, `scripts/provision-sandbox-runtime.py`,
  `.github/actions/provision-sandbox/`, `protoc` in CI, and the
  `BOXLITE_RUNTIME_URL` variable.
- On the CLI: the `roteiro sandbox` subcommand (`status`, `clear`);
  `security run --sandboxed`; `lint --sandboxed` and `lint --image`;
  `security prefetch --image`, `prefetch --analyzer sandbox` and the image-pull
  half of `prefetch`; the sandbox-images section of `security status`.
- On MCP and served chat: the `sandbox_status` and `sandbox_clear` tools and the
  `sandbox` tool class. Every MCP tool is read-only again; the one mutating
  exception ADR-0014 allowed is gone with the tool.
- In config: `[lint] image` and the whole `[security]` table
  (`[security.images]`).

### 2. The consent gate is the only gate, and it refuses by default

- `roteiro security run <analyzer>` requires `--allow-unsandboxed` on **every**
  run. Without it, it refuses and says to pass the flag or `roteiro security
  ingest` a report produced elsewhere.
- `roteiro lint <analyzer>` runs on the host only when granted — by
  `--allow-unsandboxed` for one run, or by `[lint] allow_unsandboxed = true` in
  the **user** config. A project `roteiro.toml` may deny (`false`) and never
  grant; a project denial is not overridden by the flag. ADR-0020 §6's inversion
  is kept unchanged.
- Saying nothing **refuses**, as `roteiro lint` did before the sandbox was
  built, and the refusal names both ways to grant. ADR-0020 §6 made the sandbox
  the default; with no sandbox, the default that keeps §6's intent — never run a
  tree you did not write without being asked — is refusal, not host execution.

### 3. Old configuration and old records still load

- A config file still carrying `[lint] image` or `[security]` loads, and
  `roteiro` warns once per file that the keys are inert — the pattern
  [[docs/adr/0028-one-engine-local-or-hosted.md]] set for `[remote]`.
- An `[mcp] tools` list or `--tools` value still naming `sandbox_status`,
  `sandbox_clear` or `sandbox` loads; the names restrict nothing and are
  reported, rather than failing start-up as an unknown name would.
- Stored findings whose `runner`/`isolation` say `sandboxed`/`microvm` stay
  readable. The findings data model of
  [[docs/adr/0012-analyzer-findings-artifact-model.md]] is unchanged.

### 4. What stays

`roteiro security prefetch --allow-download` still provisions the pinned rule
sets and advisory databases host analyzers read; a run still never provisions.
The `execution` and `exec-subprocess` features, `roteiro security ingest`, and
`--no-default-features --features execution` (a build that cannot execute an
analyzer at all) are unchanged.

## Options considered + consequences

| Option | Outcome |
|---|---|
| Keep `boxlite` opt-in, as ADR-0014 decided | Rejected: the feature is off by default yet its costs land on every build — `deny.toml`'s global ignores, the SQLite version every build ships, CI's `protoc` and image pull. |
| Replace it with a docker/podman backend | Rejected by the owner: it trades one heavy runtime dependency for another, and a daemon-backed container is a weaker and more variable boundary than the one being removed. |
| Keep the code but stop building it | Rejected: unbuilt code rots, still shows up in the dependency graph and in review, and would mislead a reader into thinking a sandbox is available. |
| **Remove it; the consent gate is the only gate** | **Chosen.** One execution backend, a smaller dependency graph, and the default stays refusal. |

## Consequences

- **Breaking for `roteiro`:** the `sandbox` subcommand, `--sandboxed`, `--image`,
  `prefetch --image` and `prefetch --analyzer sandbox`, the two MCP tools, and
  the `[lint] image` and `[security]` keys are gone. Scripts passing the removed
  flags must drop them.
- **The consent gate is now the only isolation.** A granted run executes a
  third-party analyzer or a build on this host with no boundary, and records
  `isolation: none`. `--allow-unsandboxed` must not be weakened; there is nothing
  behind it.
- **Analyzers that only ran sandboxed now run only on the host, with consent.**
  `semgrep` had a pinned image and was the one analyzer a stock sandboxed run
  could use; `roteiro lint clippy` needed a user-supplied image. Both now need a
  host install and a grant. `semgrep` can instead be run elsewhere and its report
  brought in through `ingest`; Clippy cannot, because lint output is deliberately
  not a storable findings layer (ADR-0020 v1.1), so a granted host run is its only
  path.
- **Unblocks the dependency policy.** With `boxlite` gone, the three `zstd`
  ignores and the six unmaintained-crate ignores have nothing to excuse, and
  `rusqlite` is free to move past ^0.39. Taking the SQLite fix (RUSTSEC-2026-0459)
  is a follow-up dependency bump, not part of this decision.
- **No embedded GPL/LGPL executables** in any build, so no source-offer duty for
  distributors.
- **Migration, by hand.** Roteiro does not delete leftover caches:
  `~/.roteiro/security/boxlite-home` (the old image store, possibly many GB) and
  `~/.roteiro/security/boxlite-runtime` (the runtime archive). Remove
  `[security]` and `[lint] image` from configs, and drop
  `--features exec-boxlite` from install commands.
  [[docs/SANDBOXED_LINTING.md]] carries the user-facing steps.
- ADR-0014's maturity duties for `boxlite`, its image-provenance rules and its
  runtime-pinning sections no longer describe anything shipped; they are kept as
  history.

## Status

**Accepted** (2026-11-11), with its implementation.

## Version history

| Version | Date | Change |
|---|---|---|
| 1.0 | 2026-11-11 | Initial. Accepted with the implementation. |

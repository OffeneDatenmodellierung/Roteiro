# rto-exec

The analyzer execution seam for [Roteiro](https://roteiro.dev).

External analyzers (`semgrep`, `cargo-audit`, and successors) can be run in more
than one place: in CI, on a developer's own machine, or — later — locally inside
a sandbox. This crate exists so those are **not competing architectures**. One
trait, `AnalyzerRunner`, defines a single request and a single normalized
response; every backend satisfies it, so callers never learn which one produced a
result.

## One conversion, not two

Per-analyzer **adapters** turn a tool's native output into the normalized report
the store persists. Both execution paths call the same adapter: a subprocess run
hands it the bytes it captured from the analyzer's stdout, and `roteiro security
ingest` hands it the bytes of a report file the same analyzer produced in CI.
Identical `Finding` values are therefore a property of the code, not something a
test has to establish after the fact.

A new analyzer is a new file in `adapter/` and an entry in `ADAPTERS`. Nothing
else: `FindingKey` takes each analyzer's own ordered identity components, so no
schema changes and no migration.

## Backends

| Runner | Availability | Isolation | Notes |
|---|---|---|---|
| `IngestRunner` | always | `ingested` | A report produced elsewhere. Zero install. |
| `SubprocessRunner` | `exec-subprocess` | `none` | Executes on the host. Requires `--allow-unsandboxed`. |
| *(boxlite)* | planned | `microvm` | ADR-0014, Stage 24. |

**`isolation=none` is meant literally.** The subprocess backend switches the
analyzer's own egress off and runs it against pinned inputs with a scrubbed
environment — no `GITHUB_TOKEN`, no `AWS_*`, no agent socket — but a subprocess
on the host can do what the host can do, and nothing here stops it. Only the
sandboxed backend can enforce that boundary. The recorded label says so.

## Assets: provisioning writes, running reads

`roteiro security prefetch` installs and verifies every pinned asset an analyzer
needs and records each digest. A run never provisions. A cold cache fails with
`assets-unavailable-offline`, naming the missing assets, their pinned digests,
and the exact command that fixes it — never an implicit fetch, never a silent
fall back to a host-installed copy.

The shipped semgrep rule set is **vendored and pinned**, and is written for this
repository: `semgrep --config p/default` resolves against a network service,
which would make an "offline" analyzer quietly network-dependent and its results
irreproducible. No Semgrep Registry rule is vendored — those carry the *Semgrep
Rules License v1.0*, which is not on this project's `deny.toml` allow-list, and
`cargo deny` governs crates rather than rule files.

## Storage

Results are persisted by `rto-graph` as a **separate artifact store**: findings
are never nodes or edges, never acquire a provenance class, and never appear in
the exported graph artifact.

See ADR-0012 (the findings artifact model), ADR-0014 (execution and
provisioning) and ADR-0018 (which analyzers cover which languages, and on which
axis).

## Stability

This crate is **an implementation detail of the `roteiro` CLI**. It is published
only because crates.io requires a published package's dependencies to be registry
packages, so `roteiro` cannot ship unless it does.

Its public API carries **no stability guarantee** — breaking changes ship as minor
version bumps. If you depend on it directly, pin an exact version.

Licensed under MIT OR Apache-2.0.

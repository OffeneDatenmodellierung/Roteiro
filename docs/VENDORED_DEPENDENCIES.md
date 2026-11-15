# Vendored and native dependencies

The register ADR-0017 requires. `cargo audit` and `cargo deny` see **Rust crates**;
they do not see the C, C++ and assembly those crates vendor. This file records, for
each such component, what version is actually vendored and where its advisories are
published — so that a native vulnerability has a place it would be noticed, rather
than depending on someone happening to look.

Nothing here is automated yet. That is the honest state, and the last section says
what automating it would take.

## The register

| Component | Vendored inside | Vendored version | Reachable under | Advisories published at |
|---|---|---|---|---|
| **SQLite** | `libsqlite3-sys` 0.39.1 (`bundled`), via `rusqlite` 0.41.1 | **3.54.1** — includes the fix for CVE-2026-119375 (RUSTSEC-2026-0459) | always — the default build | [sqlite.org/cves.html](https://www.sqlite.org/cves.html) |
| **tree-sitter C runtime** | `tree-sitter` 0.26.12 | tracks the crate version | always — the default build | [tree-sitter/tree-sitter security advisories](https://github.com/tree-sitter/tree-sitter/security/advisories) |
| **tree-sitter grammars** (18) | each `tree-sitter-<lang>` crate ships generated `parser.c` + hand-written `scanner.c` | tracks each crate version (see `Cargo.lock`) | always — the default build | the individual grammar repositories, mostly under [github.com/tree-sitter](https://github.com/tree-sitter) |
| **BoringSSL-derived crypto** | `ring` 0.17.14 (C + per-architecture assembly) | tracks the crate version | **always — the default build**, via `ureq` → `rustls` under the default `models` and `execution` features; also `serve` + `tls` | [RustSec](https://rustsec.org/) — see below; upstream [briansmith/ring](https://github.com/briansmith/ring/security) |

**llama.cpp is no longer vendored** (ADR-0033, #969). Its row, and the 13
upstream advisories this register tracked by hand because `cargo audit` cannot
see C++ vendored by a build script, left with the in-process engine; models now
run in the provider, whose own security is the provider's.

## How much of this does `cargo audit` actually cover?

Crate-level advisories are a *proxy* for the vendored code, and the proxy holds
unevenly. Measured against the RustSec database:

| Component | Advisories in RustSec | Verdict |
|---|---|---|
| SQLite | `RUSTSEC-2022-0090` on `libsqlite3-sys`, mirroring CVE-2022-35737 | The proxy has worked at least once. There is no guarantee and no automatic link from an upstream CVE to a Rust advisory. |
| tree-sitter | advisories exist against *some* grammar crates (`tree-sitter-pkl`, `tree-sitter-perl-next` — neither of which this project uses) | RustSec does file against grammar crates, so coverage is plausible but not systematic. |
| `ring` | `RUSTSEC-2025-0007`, `-0009`, `-0010` | Well covered. `ring` is maintained as a Rust crate, so its advisories arrive through the normal channel; it is in this register for completeness, not because it is a blind spot. Note that since `models` became a default feature it reaches **every** shipped binary rather than only `serve` builds — which raises the stakes of that coverage without changing its quality. |

## Checking it automatically

The rows resist mechanisation for honest reasons: SQLite publishes
CVEs as an HTML page rather than a feed, and the tree-sitter grammars are 18
separate repositories with no common advisory channel. For those, the mechanism
is this register plus Dependabot keeping the wrapping crates current — which is
weaker than a gate, and is stated as such rather than dressed up.

## Adding a row

A new dependency that vendors or links non-Rust code gets a row here in the same
change that introduces it. If its advisories have no published home, say so in
the table — an untracked native dependency that is *known* to be untracked is a
different thing from one nobody has looked at.

---

Governed by [ADR-0017](adr/0017-dependency-security-policy.md).

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
| **llama.cpp** (+ ggml) | `llama-cpp-sys-2` 0.1.154, via `llama-cpp-2` | **b10200** — commit `5f55650`, 2026-07-30 | `serve`, `inference-local-models`, `image-vision`, `audio-transcribe` | [ggml-org/llama.cpp security advisories](https://github.com/ggml-org/llama.cpp/security/advisories) |
| **SQLite** | `libsqlite3-sys` 0.37.0 (`bundled`), via `rusqlite` 0.39.0 | **3.51.3** | always — the default build | [sqlite.org/cves.html](https://www.sqlite.org/cves.html) |
| **tree-sitter C runtime** | `tree-sitter` 0.26.12 | tracks the crate version | always — the default build | [tree-sitter/tree-sitter security advisories](https://github.com/tree-sitter/tree-sitter/security/advisories) |
| **tree-sitter grammars** (18) | each `tree-sitter-<lang>` crate ships generated `parser.c` + hand-written `scanner.c` | tracks each crate version (see `Cargo.lock`) | always — the default build | the individual grammar repositories, mostly under [github.com/tree-sitter](https://github.com/tree-sitter) |
| **BoringSSL-derived crypto** | `ring` 0.17.14 (C + per-architecture assembly) | tracks the crate version | `serve` + `tls`, via `rustls` | [RustSec](https://rustsec.org/) — see below; upstream [briansmith/ring](https://github.com/briansmith/ring/security) |

`llama-cpp-sys-2`'s vendored version is not printed anywhere, so it was resolved
the long way and is recorded here to save the next person the trip: the crate's
`.cargo_vcs_info.json` gives the `utilityai/llama-cpp-rs` commit
`bed81ad4ab1a6c904b11d425608e50f976d8ea62`; the `llama.cpp` submodule at that
commit is `5f55650a78f92aff4d48d671423e888fac0469ff`; and
`refs/tags/b10200` in `ggml-org/llama.cpp` points at exactly that commit.

## How much of this does `cargo audit` actually cover?

Crate-level advisories are a *proxy* for the vendored code, and the proxy holds
unevenly. Measured against the RustSec database:

| Component | Advisories in RustSec | Verdict |
|---|---|---|
| llama.cpp | **none** — no advisory names any `llama*` crate | **The real gap.** Upstream has 13 published advisories, including a critical unauthenticated RCE in the RPC backend and repeated heap buffer overflows in GGUF tensor parsing. None of that reaches `cargo audit`. |
| SQLite | `RUSTSEC-2022-0090` on `libsqlite3-sys`, mirroring CVE-2022-35737 | The proxy has worked at least once. There is no guarantee and no automatic link from an upstream CVE to a Rust advisory. |
| tree-sitter | advisories exist against *some* grammar crates (`tree-sitter-pkl`, `tree-sitter-perl-next` — neither of which this project uses) | RustSec does file against grammar crates, so coverage is plausible but not systematic. |
| `ring` | `RUSTSEC-2025-0007`, `-0009`, `-0010` | Well covered. `ring` is maintained as a Rust crate, so its advisories arrive through the normal channel; it is in this register for completeness, not because it is a blind spot. |

llama.cpp is the one that matters. GGUF parsing takes an attacker-supplied file
and is exactly the surface where the heap overflows keep appearing, and Roteiro
parses GGUF whenever a local model is used.

**Currency, at the time of writing (2026-08-15):** vendored **b10200**
(2026-07-30); upstream latest **b10446** (2026-08-15). 246 builds, roughly two
weeks. Moving it means bumping `llama-cpp-2`, which is tracked separately and is
deliberately not part of this change.

## Checking it automatically

There is no tool that does this for us, and inventing one is not the answer. But
the llama.cpp row *is* mechanisable, and the commands below are the ones actually
used to build this file:

```sh
# What is vendored: crate → llama-cpp-rs commit → llama.cpp commit → build tag
sha=$(gh api "repos/utilityai/llama-cpp-rs/contents/llama-cpp-sys-2/llama.cpp?ref=$VCS_SHA" --jq .sha)

# What upstream has published since
gh api repos/ggml-org/llama.cpp/releases/latest --jq .tag_name
gh api repos/ggml-org/llama.cpp/security-advisories \
  --jq '.[] | select(.published_at > "2026-07-30") | {ghsa_id, severity, summary}'
```

A weekly scheduled workflow running that, and failing (or opening an issue) when
an advisory is published after the vendored commit's date, would turn this row
from a document into a gate. It is a small piece of work and it is *not* in this
change, because this change is already a full PR and that one deserves its own
review. It is the obvious next step.

The other rows resist the same treatment for honest reasons: SQLite publishes
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

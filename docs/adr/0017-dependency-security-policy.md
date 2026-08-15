---
Title: Dependency security — current by default, monitored, and never newer than 48 hours
Space: ARCH
Parent: ADRs

# ADR-specific metadata (unknown keys are ignored; used for indexing/search)
type: adr
adr-id: "0017"
status: For Review                  # Draft | For Review | Accepted | Rejected | Superseded
architectural-significance: HIGH    # SOFT | LOW | MEDIUM | HIGH | VERY HIGH
domain: Developer Tooling
decision-makers: ["The Roteiro Project Team"]
superseded-by:
version: "1.0"
last-modified: 2026-08-15
confluence-url:
---

# ADR-0017: Dependency security — current by default, monitored, and never newer than 48 hours

| | |
|---|---|
| **State** | For Review |
| **Architectural Significance** | HIGH |
| **Domain** | Developer Tooling |
| **Document version** | 1.0 |

## Reference

Establishes Roteiro's dependency-security posture as a **mechanism rather than an
intention**: stay current on purpose, monitor continuously, and never adopt a
release less than **48 hours old** unless a human explicitly asks. Closes the gap
where the project's quality bar described gates that the pipeline did not run.

Extends the quality-gate commitments of
[[docs/adr/0001-build-roteiro-unified-codebase-knowledge-graph.md]], and governs the
licence allowlist that ADR-0016 (audio metadata extraction) most recently amended.

> ADR-0016 is referenced by name rather than by `[[link]]` because it is still in
> flight in PR #316 and is not on `main`. The drift gate resolves authored links
> against the graph, so a link to an unmerged file fails `roteiro check` — which
> it did. Restore the `[[docs/adr/0016-audio-metadata-extraction.md]]` link when
> #316 merges. The same applies to this ADR's MPL-2.0 precedent below: the
> `MPL-2.0` allow-list entry and its recorded rationale arrive with #316, so on
> `main` today the `CDLA-Permissive-2.0` entry is the first of its kind rather
> than the second.

## Summary

- **Current by default.** Being behind is itself a risk; the goal is latest.
- **But never newer than 48 hours.** A release must have existed for at least two
  days before Roteiro depends on it — the window in which a compromised or
  malicious publish is typically detected, yanked, or flagged.
- **Automated, scheduled updates** with that minimum age enforced by the updater,
  not by reviewer discipline.
- **Gates run across the feature matrix.** `cargo deny` and `cargo audit` currently
  see only the default features, which is most of the dependency surface unchecked.
- **Native and vendored code is tracked explicitly**, because `cargo audit` cannot
  see it — llama.cpp ships as vendored C++ inside `llama-cpp-sys-2`.
- **Security fixes may bypass the 48-hour hold**, with explicit human approval and a
  recorded reason.

## Context

The project's stated quality bar is *"clippy/fmt/audit/deny gates from day one"*
(ADR-0001). The reality on the day this ADR was written:

- **No automated dependency updates.** No dependabot, no renovate. Every bump is
  manual and therefore sporadic.
- **No `SECURITY.md`**, so no stated way to report a vulnerability.
- **`cargo deny check` runs without `--all-features`**, so the licence and advisory
  gates never see anything reachable only through an optional feature — which is
  most of the interesting surface (`models`, `serve`, `inference-local-models`,
  `pdf-text`, `image-vision`, `audio-transcribe`, `execution`, `audio-metadata`).
  Run across the matrix it **fails today**, on `webpki-roots` (CDLA-Permissive-2.0),
  a crate already in `main`'s lockfile (#318).
- **`cargo audit` cannot see llama.cpp at all.** It is vendored C++ inside
  `llama-cpp-sys-2`; the pinned build is b10200 while upstream is far ahead. GGUF
  parsing is a genuine attack surface — malformed-model heap overflows are a
  recurring class — and nothing in the pipeline would report one.

Each of these was found incidentally, by someone doing something else. That is the
defining symptom of a posture that exists on paper: it is discovered rather than
enforced.

### Why 48 hours specifically

The dominant supply-chain attack on a package registry is a **compromised publish**
of an otherwise-trusted package — a stolen token or a hijacked maintainer account
pushing a malicious version. Such publishes are usually caught fast: by the
maintainer, by registry tooling, or by the first victims. Detection is measured in
hours; the yank follows.

Adopting a release the moment it appears makes Roteiro part of that detection
mechanism. Waiting two days lets someone else be the canary, at the cost of being
at most two days behind. Being *chronically* behind is a larger risk than being two
days behind, which is why this ADR pairs the hold with an explicit commitment to
currency: the delay is a buffer, not a brake.

The number is a judgement, not a derivation. It is long enough to catch the common
case and short enough not to accumulate debt.

## Decision makers

The Roteiro Project Team.

## Recommended option

### 1. Currency with a minimum release age

Dependencies are kept **at the latest published version**, subject to a **minimum
release age of 48 hours**. The updater enforces the age; reviewers do not have to
remember it. A human may explicitly request an earlier adoption, and that request
is recorded in the PR.

### 2. Automated, scheduled updates

An updater bot proposes updates on a schedule, grouped sensibly (patch/minor
together, majors separately, dev-dependencies separately), so that "current" is the
default state rather than an occasional effort. The tool choice is an
implementation detail; the requirements are: **minimum release age**, **grouping**,
**scheduling**, and support for **Cargo**.

### 3. Gates that see what the project actually ships

`cargo deny` and `cargo audit` run across the **feature matrix**, not just default
features. A licence or advisory reachable only under an optional feature must fail
CI, not a hand-run (#318).

This has an immediate consequence to settle deliberately rather than paper over:
**`CDLA-Permissive-2.0`** (via `webpki-roots`, under `models`) is currently
un-allowed and will fail the moment the gate widens. It must be admitted with a
recorded rationale — as MPL-2.0 was — or the dependency changed. **It must not be
allowed merely to turn CI green.**

### 4. Native and vendored code is tracked by name

`cargo audit` covers Rust crates. It does **not** cover vendored C/C++. Roteiro
therefore records, for each such dependency, what version is vendored and where its
advisories are published — starting with llama.cpp inside `llama-cpp-sys-2`. A
vendored component with no advisory-watch story is an untracked risk regardless of
how healthy `cargo audit` looks.

The register is [[docs/VENDORED_DEPENDENCIES.md]]. Measured against RustSec when it
was written, the gap is not theoretical: llama.cpp has **13 published advisories
upstream and none in RustSec**, including a critical unauthenticated RCE and
repeated heap overflows in GGUF tensor parsing — the exact surface Roteiro exposes
whenever a local model is loaded.

### 5. Advisory exceptions carry their reasoning

The existing practice in `deny.toml` — an ignored advisory explains *why*, *how the
crate enters the tree*, and *what would trigger a revisit* — becomes policy. An
`ignore` entry without that is incomplete. Note also that `cargo deny` cannot scope
an ignore to a feature, so a feature-scoped justification is a promise the tool
cannot keep, and must say so.

### 6. `SECURITY.md`

A stated way to report a vulnerability, and what a reporter can expect.

### 7. Security fixes may jump the queue

An update that addresses an **active advisory** may bypass the 48-hour hold with
explicit human approval and a recorded reason. The hold exists to avoid unknown
risk; it must never delay a fix for a known one.

## Options considered + consequences

| Option | Verdict |
|---|---|
| Status quo — manual, occasional bumps | **Rejected.** Chronic staleness plus incidental discovery of gaps. |
| Latest immediately, no hold | **Rejected.** Makes the project a first-wave victim of a compromised publish for no compensating benefit. |
| Pin everything, update rarely | **Rejected.** Trades a rare acute risk for a constant chronic one, and makes each update a large, risky change. |
| **Current, with a 48-hour minimum age (chosen)** | Small, bounded delay; the common attack window is covered; currency is preserved. |
| Longer hold (e.g. 7 days) | Rejected as the default — the marginal detection benefit is small and the staleness cost compounds. Available per-dependency if a specific ecosystem warrants it. |

## Consequences

**Positive**

- Being current becomes the default state, maintained automatically.
- The most common supply-chain attack has a two-day buffer, enforced by tooling
  rather than by discipline.
- Licence and advisory gates finally cover the features the project ships.
- Vendored native code stops being invisible.

**Negative / costs**

- More PR churn, and more CI minutes on a workspace that already builds llama.cpp
  and tree-sitter grammars.
- The 48-hour hold **does not** protect against a compromise that stays undetected
  for longer than that. It reduces exposure; it does not eliminate it.
- Widening the gates surfaces pre-existing failures that must now be decided
  (starting with `CDLA-Permissive-2.0`), which is work that was previously
  invisible — the point, but still work.
- Native-dependency advisory watching is manual until a tool covers it.

## Status

For Review. Implementation — updater configuration, widened CI gates, `SECURITY.md`
and the vendored-dependency register — follows in the same PR where practical, per
the standing rule that a decision and its mechanism should land together.

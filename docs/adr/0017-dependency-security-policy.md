---
Title: Dependency security — current by default, monitored, and held for a minimum release age of at least 48 hours
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
version: "1.1"
last-modified: 2026-08-15
confluence-url:
---

# ADR-0017: Dependency security — current by default, monitored, and held for a minimum release age of at least 48 hours

| | |
|---|---|
| **State** | For Review |
| **Architectural Significance** | HIGH |
| **Domain** | Developer Tooling |
| **Document version** | 1.1 |

## Reference

Establishes Roteiro's dependency-security posture as a **mechanism rather than an
intention**: stay current on purpose, monitor continuously, and never adopt a
release younger than the configured **minimum release age — at least 48 hours** —
unless a human explicitly asks. Closes the gap where the project's quality bar
described gates that the pipeline did not run.

Extends the quality-gate commitments of
[[docs/adr/0001-build-roteiro-unified-codebase-knowledge-graph.md]], and governs the
licence allowlist that
[[docs/adr/0016-audio-metadata-extraction.md]] most recently amended.

## Summary

- **Current by default.** Being behind is itself a risk; the goal is latest.
- **But never newer than the minimum release age, which is at least 48 hours.**
  A release must have existed for at least two days before Roteiro depends on it
  — the window in which a compromised or malicious publish is typically detected,
  yanked, or flagged. **48 hours is a floor, not a target**: the configured value
  may be higher, and currently is (see below).
- **Automated, scheduled updates** with that minimum age enforced by the updater,
  not by reviewer discipline.
- **Gates run across the feature matrix.** `cargo deny` currently sees only the
  default features, which leaves most of the dependency surface unchecked.
  (`cargo audit` does not share this gap — see §3.)
- **Native and vendored code is tracked explicitly**, because `cargo audit` cannot
  see it — llama.cpp ships as vendored C++ inside `llama-cpp-sys-2`.
- **Security fixes may bypass the hold**, with explicit human approval and a
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

It is also the number this ADR is willing to *guarantee*, which is not the same as
the number it wants. The ecosystem has since converged on something slightly
longer — Dependabot's default became three days in July 2026 — and where the
tooling is more conservative than this floor, the tooling wins. 48 hours is the
point below which the project will not go; it was never a claim that 49 hours
would be excessive.

## Decision makers

The Roteiro Project Team.

## Recommended option

### 1. Currency with a minimum release age

Dependencies are kept **at the latest published version**, subject to a **minimum
release age of at least 48 hours**. The updater enforces the age; reviewers do not
have to remember it. A human may explicitly request an earlier adoption, and that
request is recorded in the PR.

**48 hours is a floor, not a target — and this distinction has already bitten.**
v1.0 of this ADR was titled "never newer than 48 hours", which reads as a ceiling.
Implementing it literally would have meant configuring the updater *down* to two
days, because Dependabot began applying a **three-day** cooldown by default on
2026-07-14. That is the letter of the policy defeating its purpose: it would have
reduced safety relative to doing nothing at all, in order to comply with a number
in a document.

So the rule is directional:

> The configured hold must never be **lower** than 48 hours, and never lower than
> the updater's own default. If the platform default rises above the configured
> value, raise the configured value — do not "fix" the configuration downwards to
> match a number in this ADR.

The current configuration is **three days**, inherited from that default and set
explicitly in `.github/dependabot.yml` so the value is visible rather than
implicit. A longer hold is a judgement the project may revisit (see the options
table); a *shorter* one than this floor is a change to this ADR.

### 2. Automated, scheduled updates

An updater bot proposes updates on a schedule, grouped sensibly (patch/minor
together, majors separately, dev-dependencies separately), so that "current" is the
default state rather than an occasional effort. The tool choice is an
implementation detail; the requirements are: **minimum release age**, **grouping**,
**scheduling**, and support for **Cargo**.

### 3. Gates that see what the project actually ships

`cargo deny` runs across the **feature matrix**, not just default features. A
licence or advisory reachable only under an optional feature must fail CI, not a
hand-run (#318).

**`cargo audit` needs no such change, and this is worth stating explicitly so that
nobody "fixes" it later on a false premise.** It reads `Cargo.lock`, which lists
every optional dependency regardless of which features are selected, so it already
covers the whole matrix. Measured: it reports `ttf-parser` — reachable only under
the opt-in `pdf-text` feature — while `cargo deny check` without `--all-features`
returned `advisory-not-detected: no crate matched advisory criteria` for the same
crate. There is no `--all-features` flag to add to `cargo audit`, and adding one
would be meaningless. Issue #318 originally claimed both tools shared the blind
spot; they do not.

The two tools are therefore **not** redundant, and differ in strictness as well as
scope: `cargo audit` exits 0 on an `unmaintained` advisory, whereas `cargo deny
check advisories` fails on it. `cargo deny` is the enforcing gate; `cargo audit`
is the RustSec-native second opinion.

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

An update that addresses an **active advisory** may bypass the hold with
explicit human approval and a recorded reason. The hold exists to avoid unknown
risk; it must never delay a fix for a known one.

## Options considered + consequences

| Option | Verdict |
|---|---|
| Status quo — manual, occasional bumps | **Rejected.** Chronic staleness plus incidental discovery of gaps. |
| Latest immediately, no hold | **Rejected.** Makes the project a first-wave victim of a compromised publish for no compensating benefit. |
| Pin everything, update rarely | **Rejected.** Trades a rare acute risk for a constant chronic one, and makes each update a large, risky change. |
| **Current, with a minimum age of at least 48 hours (chosen)** | Small, bounded delay; the common attack window is covered; currency is preserved. Configured at three days today, following the updater's default. |
| Much longer hold (e.g. 7 days) | Rejected as the default — the marginal detection benefit is small and the staleness cost compounds. Available per-dependency if a specific ecosystem warrants it. Note this rejects *7 days as a policy*, not the few days between the floor and whatever the updater defaults to. |

## Consequences

**Positive**

- Being current becomes the default state, maintained automatically.
- The most common supply-chain attack has an at-least-two-day buffer, enforced by
  tooling rather than by discipline.
- Licence and advisory gates finally cover the features the project ships.
- Vendored native code stops being invisible.

**Negative / costs**

- More PR churn, and more CI minutes on a workspace that already builds llama.cpp
  and tree-sitter grammars.
- The hold **does not** protect against a compromise that stays undetected
  for longer than that. It reduces exposure; it does not eliminate it.
- Widening the gates surfaces pre-existing failures that must now be decided
  (starting with `CDLA-Permissive-2.0`), which is work that was previously
  invisible — the point, but still work.
- Native-dependency advisory watching is manual until a tool covers it.

## Status

For Review. Implementation — updater configuration, widened CI gates, `SECURITY.md`
and the vendored-dependency register — follows in the same PR where practical, per
the standing rule that a decision and its mechanism should land together.

## Version history

| Version | Date | Change |
|---|---|---|
| 1.0 | 2026-08-15 | Initial decision. |
| 1.1 | 2026-08-15 | **48 hours restated as a floor, not a ceiling.** v1.0's title — "never newer than 48 hours" — reads as an upper bound, and implementing it literally would have configured the updater *down* to two days from Dependabot's three-day default (introduced 2026-07-14), reducing safety in order to comply with the prose. Adds the directional rule in §1, corrects the title and Summary, and records the configured value (three days). Also corrects the Summary and §3 claim that `cargo audit` shared `cargo deny`'s feature blind spot — it does not, because it reads `Cargo.lock`; this was measured, and the same error in #318 has been corrected upstream. |
| 1.2 | 2026-08-16 | **Disclosure, not a policy change: `models` became a default feature.** §3's parenthetical "(via `webpki-roots`, under `models`)" now describes a licence in **every** shipped binary rather than an opt-in one. `CDLA-Permissive-2.0` moves into the default set along with `ISC` (`rustls-webpki`, `untrusted`, and the ISC half of `ring`'s `Apache-2.0 AND ISC`) and `BSD-3-Clause` (`subtle`). **No allow-list entry was added or amended** — all four were already allowed, and the CDLA rationale in `deny.toml` never turned on the feature being opt-in. Recorded because this ADR's subject is gates that see what the project actually *ships*, and what it ships changed. |

---
Title: Widening the content screen — four gaps closed, one refused, and none by subprocess
Space: ARCH
Parent: ADRs

# ADR-specific metadata (unknown keys are ignored; used for indexing/search)
type: adr
adr-id: "0024"
status: Draft                       # Draft | For Review | Accepted | Rejected | Superseded
architectural-significance: MEDIUM  # SOFT | LOW | MEDIUM | HIGH | VERY HIGH
domain: Developer Tooling
decision-makers: ["The Roteiro Project Team"]
superseded-by:
version: "0.1"
last-modified: 2026-09-02
confluence-url:
---

# ADR-0024: Widening the content screen — four gaps closed, one refused, and none by subprocess

| | |
|---|---|
| **State** | Draft |
| **Architectural Significance** | MEDIUM |
| **Domain** | Developer Tooling |
| **Document version** | 0.1 |
| **Related** | [[docs/adr/0021-open-knowledge-format-bundle.md]] · [[docs/adr/0017-dependency-security-policy.md]] · [[docs/adr/0019-remote-model-tier.md]] |

## Reference

Affected code: [[crates/rto-graph/src/screen.rs#screen_text]],
[[crates/rto-graph/src/screen.rs#Verdict]], [[crates/rto-graph/src/screen.rs#FindingKind]],
[[crates/rto-render/src/okf/read.rs#read_bundle]].

**Id note.** `0023` is reserved by #749, which is unmerged, so the graph cannot
see it and `spec scaffold` proposes it here. Taken as `0024` by hand. This is the
one case that command cannot get right — an id allocated in a branch is not a
fact about the repository yet — and it is worth recording rather than treating as
a bug in the tool.

## Summary

[[crates/rto-graph/src/screen.rs#screen_text]] screens a peer's bundle before its
text can reach a model. Its own module documentation lists six things it
deliberately does not attempt. An external contributor — the author of
`okf-guard`, which the screen credits as prior art — asked in issue #723 whether
those could be covered.

**Four are closed, one is reshaped, and one is refused.** The refusal and the
reshaping are the decisions; the rest is work.

Nothing here shells out. Issue #723 proposed invoking `okfguard` as a
subprocess, and the reason not to is neither language nor licence: a subprocess
per concept over a 9,511-concept bundle, and a Python runtime as a hard
dependency of `import`.

## Context

The screen was written against okf-guard's model and inverted its aim: okf-guard
protects *your* pipeline from *your* sources, and this protects *this graph* from
*someone else's finished bundle*. That inversion produced a narrower tool on
purpose, and the module says so.

Re-reading those exclusions against the question, **one of them argues against
the wrong thing**:

> No homoglyph or confusable detection. okf-guard maps 23 Cyrillic letters; the
> full Unicode confusables table is far larger, and any subset of it fires on
> legitimately multilingual prose. A peer writing Russian is not an attacker.

That is a correct objection to *"contains Cyrillic"*. It is not an objection to
the rule UTS #39 actually specifies, which is about a **single token mixing
scripts** — `раypal` is an attack and a Russian sentence is not. The exclusion
was defending against a strawman of its own construction.

## Decision makers

- The Roteiro Project Team

## Recommended option

Close four gaps natively, reshape the fifth, refuse the sixth, and make the two
that can grow **configurable but never weakenable**.

## Options considered + consequences

### Option A — shell out to `okfguard`

Rejected. A subprocess per concept is the wrong shape at bundle scale, and it
makes a Python runtime a hard dependency of `roteiro import`. ADR-0017 refused
`okf-validator` for a related reason: a dependency admitted to close a gap has to
be paid for by everyone who builds the tool, not only by those who hit the gap.

### Option B — close everything okf-guard covers

Rejected. Its PDF, DOCX, PPTX and XLSX adapters exist because it screens
*sources before conversion*. An OKF bundle is markdown. Building extraction for
formats a bundle cannot contain would be coverage measured against the wrong
input.

### Option C — close what applies, and say why the rest does not *(recommended)*

| gap | decision | rule |
|---|---|---|
| homoglyphs | **close** | UTS #39 mixed-script *within one token*, never "contains a script" |
| encoded payloads | **close** | decode and re-screen, to a configurable depth |
| non-English directives | **reshape** | control tokens first; phrases via an optional dictionary |
| CSS cascade | **close, bounded** | same-document `<style>` only; no specificity, no inheritance |
| semantic judgement | **refuse** | unchanged — whether text *is* an attack is not decided here |
| binary extraction | **refuse** | a bundle is markdown; the media pipeline is a different path |

#### Encoded payloads: depth is configurable, and defaults to 5

The original exclusion said "recursive decoding is unbounded", which argues
against recursion rather than against depth. Nesting is a real obfuscation, and
the cost of following it is small: base64 shrinks 4:3 and hex 2:1, so **each
level is strictly smaller than the last** and five levels cost less than twice
the first.

```toml
[security.screen]
decode_depth = 5   # 0 disables; default 5
```

Two guards, because depth is where false positives come from. A run is only
decoded if it is long enough to be a payload rather than a word, and recursion
only continues while the output is **plausibly text** — random bytes are not
re-screened, which is what stops a chain of accidental decodes eventually
matching a directive pattern by chance.

#### Directives: control tokens first, phrases by dictionary

"English only" is a real gap. Translating the phrase list is the obvious answer
and the weaker one: five languages of hand-written phrases is a maintenance
burden carrying false-positive risk in languages no maintainer here reads.

The higher-signal, **language-independent** vector is chat-template control
tokens — `<|im_start|>`, `<|system|>`, `[INST]`, `<<SYS>>`, `### System:`. These
carry no language at all and are the most direct way to address a model. They go
in first.

Phrases then become extensible without a code change:

```toml
[security.screen]
directive_dictionaries = ["docs/screen/directives.de.toml"]
```

**A dictionary may only add patterns.** It cannot remove, disable or override a
built-in one. This is the load-bearing rule: a configurable screen whose
configuration can *weaken* it is a screen that is off in exactly the repository
where somebody found it inconvenient, and the failure would be silent. Adding is
safe, subtracting is not, so only adding is possible.

### Consequences

- **More findings on existing bundles.** Concepts that passed may now quarantine.
  That is the point, but it lands on real bundles, so each new class ships with
  its measurement against the four published ones.
- **Two new configuration keys** to keep honest, one of which is a file path a
  repository can point anywhere. Dictionaries are additive, so a hostile
  dictionary can only make the screen *stricter* — the worst case is noise.
- **`decode_depth = 0` is a supported answer**, because a repository that has
  measured the cost and does not want it should be able to say so, and a setting
  people disable by patching the source is worse than one they disable by name.
- **The exclusion list stays**, minus what is closed. It is the honest half of
  this module and deleting it would make the screen look broader than it is.

## Implementation

Each lands separately, with its measurement over the four published bundles:

1. **Control tokens** — smallest, highest signal, no new configuration.
2. **Mixed-script confusables** — UTS #39, with the multilingual-prose case tested.
3. **Encoded payloads** — `decode_depth`, defaulting to 5, with the plausibly-text guard.
4. **Bounded CSS cascade** — same-document `<style>` only.
5. **Directive dictionaries** — last, because additive-only is the property to get right.

## Advice Received

Issue #723, from the author of `okf-guard`. The proposal was a subprocess; the
useful part was the question behind it, which was whether the exclusions were
principled or merely unimplemented. Four of them were unimplemented.

The depth default of 5 and the dictionary mechanism were both directed rather
than proposed: the first because one level only defeats the laziest nesting, the
second because it makes translations somebody else's contribution instead of a
maintainer's backlog.

## Document version history

| Version | Date | Notes |
|---------|------|-------|
| 0.1 | 2026-09-02 | Draft. Closes four of the screen's stated exclusions, reshapes the non-English one around language-independent control tokens plus additive dictionaries, and refuses binary extraction and semantic judgement. `decode_depth` defaults to 5 and is configurable; dictionaries may only ever *add* patterns, because a screen whose configuration can weaken it fails silently in the repository that weakened it. Records that the homoglyph exclusion argued against "contains Cyrillic" rather than against UTS #39's mixed-script rule. |

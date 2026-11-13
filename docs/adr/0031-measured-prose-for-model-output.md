---
Title: Model prose is measured on three separate axes — faithfulness, coverage and style — each deterministic
Space: ARCH
Parent: ADRs

# ADR-specific metadata (unknown keys are ignored; used for indexing/search)
type: adr
adr-id: "0031"
status: Accepted                    # Draft | For Review | Accepted | Rejected | Superseded
architectural-significance: MEDIUM  # SOFT | LOW | MEDIUM | HIGH | VERY HIGH
domain: Inference
decision-makers: ["The Roteiro Project Team"]
superseded-by:
version: "1.1"
last-modified: 2026-11-12
confluence-url:
---

# ADR-0031: Model prose is measured on three separate axes — faithfulness, coverage and style — each deterministic

|  |  |
|---|---|
| **State** | Accepted |
| **Architectural Significance** | MEDIUM |
| **Domain** | Inference |
| **Document version** | 1.1 |

## Reference

- Faithfulness, the axis that already exists: [[crates/rto-faithful/src/lib.rs#check]].
- The one engine every model call goes through: [[docs/adr/0028-one-engine-local-or-hosted.md]].
- Coverage for `spec draft`: [[crates/rto-spec/src/spec.rs#draft_coverage]].
- The prompt `spec draft` sends: [[crates/rto-spec/src/spec.rs#draft_prompt]].
- The style lint: [[crates/rto-graph/src/prose.rs#lint]].
- The verdict and Ask prompts: [[crates/rto-graph/src/reviewer.rs#build_verdict_prompt]],
  [[crates/rto-serve/src/tools.rs#system_prompt]].
- Prior art: V. Mysore, *webSTE100* (MIT), a deterministic linter for
  ASD-STE100 Simplified Technical English with a model repair loop, written up
  after A. Karpathy suggested asking models to write in that standard.

## Summary

Prose a model writes for Roteiro is measured on three axes, each by a
deterministic function of the text and the inputs the model was given, and each
reported separately:

1. **Faithfulness** — every claim traces to something the model was shown.
   `rto-faithful` already does this.
2. **Coverage** — what the model was given and never mentioned.
3. **Style** — a controlled-English lint in the spirit of ASD-STE100: sentence
   length, one instruction per sentence, unapproved words, filler, one name per
   concept.

No model grades a model. The axes are never folded into one score, because each
can be gamed by the others: the shortest answer has the best style score, the
longest the best coverage.

## Context

`rto-faithful` bounds fabrication and says plainly what it does not bound:
*"A rendering that silently drops half the findings is faithful by this
definition. Coverage is a different property with a different check."* Nothing
implemented that check.

Separately, a model asked to write tersely writes less. webSTE100 measured
exactly this: a 430-word answer that scored poorly became a 32-word one that
scored well and lost the steps that mattered. Its author's conclusion — measure
style, completeness and correctness separately — is the shape of this decision.
Roteiro is better placed than a general tool to do it, because it knows the
exact set of graph nodes it handed the model, so completeness is set membership
rather than a judgement.

## Decision makers

The Roteiro Project Team (owner decision, 2026-11-12).

## Recommended option

### 1. Coverage is reported where the output's job is to cover

Coverage applies to `spec draft`, whose prompt hands the model a known set of
symbols and governing decisions to write about. After drafting, Roteiro reports
which of them the document never mentions. It is a **report**, never a gate: a
section of two to four sentences cannot name every symbol, and an omission is a
prompt for the author, not a defect.

It does **not** apply to:

- **Ask.** A search returns more than an answer should use; obliging the answer
  to name every hit would reward padding.
- **The whole-change verdict of `review --llm`.** Its prompt asks for the ONE
  most important thing, so dropping the rest is the instruction, not an
  omission.

A mention is decided by text alone: a symbol's name as a whole identifier, or a
decision's key (`adr:0018`) or number (`ADR-0018`).

### 2. Style is a lint over prose, never over code

The style lint reads prose and skips code spans, fenced blocks, wiki links,
graph keys and paths — which is most of what a Roteiro answer carries. It uses
Roteiro's own word list. The official ASD-STE100 dictionary is ASD's and is not
shipped. It approximates the standard; it does not certify against it.

### 3. Repair is opt-in, bounded, and may not trade coverage for style

`spec draft` may rewrite a section against its lint findings, at most three
passes, only when asked. A rewrite that mentions fewer of the supplied symbols
and decisions than the draft it replaces is discarded however well it scores,
and the best surviving version is kept. Every pass is another model call — and,
against a hosted provider, another copy of repository content sent (ADR-0028) —
which is why it is never the default.

### 4. Prompts ask for controlled English, partially

The `spec draft`, `review --llm` verdict and Ask prompts gain a few of the
standard's rules: short sentences, one instruction per sentence, one name per
concept. Not the whole standard: its dictionary is too strict for prose that
must name code.

### 5. Authored prose may be linted, never gated

The same lint can be run over the ADRs and other authored documents. It reports
and never fails `roteiro check`: authored prose is a person's, and a heuristic
that is wrong sometimes must not block a merge.

## Options considered + consequences

| Option | Outcome |
|---|---|
| An LLM judge for quality | Rejected: non-deterministic, and a model grading a model is what `rto-faithful` exists to avoid. |
| One combined quality score | Rejected: each axis is gamed by the others; a combined score hides which one moved. |
| Gate on style or coverage | Rejected: both are heuristics over prose. A gate that is wrong sometimes blocks people for nothing. |
| **Three deterministic axes, reported separately** | **Chosen.** |

## Consequences

- `spec draft` prints a coverage line and a style line on stderr. Its stdout
  document is unchanged.
- The style lint is English-only and regex-shaped; it will misread some
  sentences. That is acceptable for a report and is why nothing gates on it.
- A repair pass multiplies model calls. It is off unless asked for.

## Status

**Accepted** (2026-11-12). §1, §2 and §4 are implemented; §3 and §5 land in
follow-up changes, each recorded in the version history below.

## Version history

| Version | Date | Change |
|---|---|---|
| 1.0 | 2026-11-12 | Initial. Accepted with the coverage report for `spec draft` (§1). |
| 1.1 | 2026-11-12 | §2 and §4 implemented: `rto_graph::prose::lint`, a `style:` line from `spec draft` beside the coverage line, and the partial controlled-English rules in the `spec draft`, verdict and Ask prompts. The prompt rules are not yet measured against a model. |

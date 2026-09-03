---
Title: Widening the content screen — what a peer's bundle can carry, and what we never say about it
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
version: "0.2"
last-modified: 2026-09-03
confluence-url:
---

# ADR-0024: Widening the content screen — what a peer's bundle can carry, and what we never say about it

| | |
|---|---|
| **State** | Draft |
| **Architectural Significance** | MEDIUM |
| **Domain** | Developer Tooling |
| **Document version** | 0.2 |
| **Related** | [[docs/adr/0021-open-knowledge-format-bundle.md]] · [[docs/adr/0017-dependency-security-policy.md]] · [[docs/adr/0019-remote-model-tier.md]] |

## Reference

Affected code: [[crates/rto-graph/src/screen.rs#screen_text]],
[[crates/rto-graph/src/screen.rs#Verdict]], [[crates/rto-graph/src/screen.rs#FindingKind]],
[[crates/rto-render/src/okf/read.rs#read_bundle]].

**Id note.** When this was scaffolded, `0023` was held by an unmerged branch
(#749, since merged), so the graph could not see it and `spec scaffold` proposed
it here. Taken as `0024` by hand. This is the one case that command cannot get
right — an id allocated in a branch is not yet a fact about the repository — and
it is worth recording rather than filing as a bug against the tool.

## Summary

[[crates/rto-graph/src/screen.rs#screen_text]] screens a peer's bundle before its
text can reach a model. Its own module documentation lists six things it
deliberately does not attempt. An external contributor — the author of
`okf-guard`, which the screen credits as prior art — asked in issue #723 whether
those could be covered.

**Four are closed, one becomes a report *and* a screen, and one stays refused.** The two that are not simply closed are the decisions; the rest is work.

The sharpest correction came from outside the exclusion list. It said "a bundle
is markdown", and a bundle is not: `okf-core` resolves a frontmatter path to any
file, §10's `computation:` names one, and the four published bundles being
all-markdown is a fact about those four bundles. A peer can hand over a valid
bundle citing a hijacked PDF, and **every report we produce would call it clean
without mentioning the PDF exists**.

Nothing here shells out, and the reason is the **shape of the interface** rather
than its cost — see Option A, whose first draft got that wrong and said so.

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

Close four gaps natively, inventory everything a bundle carries, screen whatever
can be read out of it, keep refusing semantic judgement, and make the two that can
grow **configurable but never weakenable**.

## Options considered + consequences

### Option A — consult `okfguard` at the ingestion boundary

Rejected, **but the first version of this section rejected it for two reasons
that were false**, and the correction is worth keeping rather than quietly
replacing.

It said the integration would mean "a subprocess per concept over a
9,511-concept bundle" and "a Python runtime as a hard dependency of `import`".
Neither describes what was proposed. Reading the CLI rather than assuming it:
`okfguard scan -r <dir>` is **one process over a tree**, `--json` is
newline-delimited **per file** rather than a single aggregate, and a standalone
binary consulted when present is an **optional integration point**, not a
dependency anyone building Roteiro has to pay for. Both objections were
answered, correctly, on issue #723.

Three reasons survive, and all three are about what the interface can express:

1. **The screen is per *field*, not per file.** A concept is one file carrying a
   body, a title and a description, and [[crates/rto-render/src/okf/read.rs#read_bundle]]
   screens all three separately — then deliberately *downgrades* a `Block` on the
   title or description, because a title that does not survive falls back to the
   filename while a body has no such fallback. A per-file verdict cannot express
   "block this body, keep this title", which is the outcome the importer actually
   produces.
2. **The screen returns admissible *text*, not a label.** This is the
   `clean_text` observation the module already credits okf-guard for, and it is
   the load-bearing one. [`Screened::admit`] is what may be stored: byte-identical
   on `Pass`; the prose with invisible codepoints and presentation-hidden regions
   removed on a `Quarantine` with no directive; and `None` on a quarantined
   directive, because the words *are* the payload and redacting a phrase leaves a
   sentence that still reads as one. `c.body = body.admit.unwrap_or_default()` is
   what reaches `meta.content`. An exit code — and per-file JSON findings too —
   give a judgement, not the bytes, so the screen would still have to run to
   produce them.
3. **"Consulted when available" is a screen that is absent, silently, wherever it
   is not installed.** The dictionary rule below makes additive-only
   configuration load-bearing for exactly this reason: a screen whose
   configuration can weaken it is off in the repository where somebody found it
   inconvenient, and the failure is quiet. An optional external scanner is
   subtraction by omission — the same failure reached by not installing something
   rather than by editing a file.

ADR-0017's refusal of `okf-validator` is a *related* argument about cost, and it
is not this one. It is cited here only to mark the difference: that dependency
was refused because everyone building the tool would pay for it, and this option
is refused although almost nobody would.

### Option B — close everything okf-guard covers, including binary extraction

Rejected for extraction, **but the reasoning that first rejected it was wrong**
and the correction changed this ADR.

The original argument was "an OKF bundle is markdown, so those adapters cover
formats a bundle cannot contain". That is false. `okf-core`'s own
`resolve_path_field` resolves a frontmatter path to *any* file with `is_file()`
and no extension filter, §10's `computation:` names a file, and the four
published bundles happening to be all-markdown is a property of those four
bundles rather than of the format.

Checked rather than assumed, on a bundle carrying a PDF:

```
GET /f/docs/policy.pdf
HTTP/1.1 200 OK
content-type: application/octet-stream
content-security-policy: default-src 'none'; sandbox; base-uri 'none'
x-content-type-options: nosniff
```

Two things are already sound: the viewer **refuses to link** a non-markdown file
— only images resolve to `/f/`, so a markdown link to a PDF renders as refused
text — and `roteiro import` reads no binary at all.

Serving one is *nearly* sound, and the gap is worth naming precisely rather than
glossed. The response carries a content type from a closed allow-list, the
file-scoped `default-src 'none'; sandbox` policy, and `nosniff` — so an unknown
extension is typed `application/octet-stream` and cannot be sniffed into
something executable. It does **not** carry `Content-Disposition: attachment`.
Most browsers download `application/octet-stream` rather than rendering it, but
that is convention rather than something the response asks for, and "it is served
as an attachment" was the wrong description of what these headers do.

So the inventory below comes with one header: **`/f/` will set
`Content-Disposition: attachment` for anything outside the image allow-list.**
The same reasoning as the content type itself — a bundle does not get to choose
how its bytes are presented — and it costs one header on a path that already
decides, by extension, what each file is allowed to be.

**What is missing is that nobody is told they exist.** `okf validate`, `lint`,
`trust`, `links` and `info` all report on a bundle and none of them mentions that
it carries three PDFs. A person reads "0 violations", decides the source is
trustworthy, and the binaries were never in scope of the thing they read. That is
the gap, and it is a reporting gap rather than an extraction one.

**Extraction is not refused, and the first draft's reason for refusing it was
false twice over.** It said a PDF parser is dependency weight under ADR-0017, and
that the media pipeline is where it would belong "if it is ever wanted".
Roteiro has extracted PDF text since before this ADR: `pdf-extract` is a declared
optional dependency, gated behind the `pdf-text` feature, and
[[crates/rto-graph/src/extract.rs#pdf_content]] runs it size-bounded at 20 MiB
and panic-guarded, feeding a file node's embeddable content. The trade was made
and paid for; the ADR argued against making it.

The stronger correction is the one that follows from that. **Refusing to extract
is refusing to screen.** A binary is unscreenable *because* nothing reads it —
that is a consequence of the decision, not a fact about the format. An inventory
says a bundle carries three PDFs; extraction plus [`screen_text`] says one of
them carries a hidden instruction, which is the question the screen exists to
answer.

So the inventory is the floor rather than the ceiling: what a bundle carries is
always reported, and what can be read is read and then screened like any other
text. Which formats those are, and who decides per file, is
[[docs/adr/0025-document-extraction-consent.md]] — extraction is an *ingestion*
question, and this ADR is about what the screen does with text once it has it.

### Option C — close what applies, and say why the rest does not *(recommended)*

| gap | decision | rule |
|---|---|---|
| homoglyphs | **close** | UTS #39 mixed-script *within one token*, never "contains a script" |
| encoded payloads | **close** | decode and re-screen, to a configurable depth |
| non-English directives | **reshape** | control tokens first; phrases via an optional dictionary |
| CSS cascade | **close, bounded** | same-document `<style>` only; no specificity, no inheritance |
| semantic judgement | **refuse** | unchanged — whether text *is* an attack is not decided here |
| binary files | **report always; screen whatever can be read** | a bundle *can* carry them, and nothing said so |
| serving one | **`Content-Disposition: attachment`** | a bundle does not choose how its bytes are presented |

#### Binary files: an inventory, in every report that describes a bundle

A bundle's non-markdown files are counted and listed — by path, size and
extension — wherever a bundle is summarised, and surfaced in the consent prompt,
which is the moment a person decides whether to trust the source. Nothing is
opened, parsed or extracted.

The screen's own principle applies: **a finding has to do something.** Knowing a
bundle carries an unscreenable file and not saying so is the same failure as
computing a quarantine verdict and then storing the body anyway.

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
  this module and deleting it would make the screen look broader than it is —
  but "a bundle is markdown" comes out of it, because it was not true.
- **Every bundle report grows a line.** For the four published bundles it will
  read zero, which is worth printing rather than omitting: "no unscreenable
  files" is information, and a line that appears only sometimes is one a reader
  learns to stop looking for.

## Implementation

Each lands separately, with its measurement over the four published bundles:

1. **The binary inventory and `Content-Disposition`** — first, because it is the
   half of this ADR that is a reporting gap rather than a detection one, and the
   only half that answers the scenario which prompted it.
2. **Control tokens** — smallest, highest signal, no new configuration.
3. **Mixed-script confusables** — UTS #39, with the multilingual-prose case tested.
4. **Encoded payloads** — `decode_depth`, defaulting to 5, with the plausibly-text guard.
5. **Bounded CSS cascade** — same-document `<style>` only.
6. **Directive dictionaries** — last, because additive-only is the property to get right.

## Advice Received

Issue #723, from the author of `okf-guard`. The proposal was to consult their
CLI at the ingestion boundary; the useful part was the question behind it, which
was whether the exclusions were principled or merely unimplemented. Four of them
were unimplemented.

They also corrected this ADR's first characterisation of that proposal, which had
invented a per-concept subprocess and a hard runtime dependency out of neither.
Option A now records what was actually proposed and rejects it on the interface,
which is the argument that was there to be made.

The depth default of 5 and the dictionary mechanism were both directed rather
than proposed: the first because one level only defeats the laziest nesting, the
second because it makes translations somebody else's contribution instead of a
maintainer's backlog.

## Document version history

| Version | Date | Notes |
|---------|------|-------|
| 0.1 | 2026-09-02 | Draft. Records that "a bundle is markdown" was false — `okf-core` resolves a frontmatter path to any file, so a peer can cite a hijacked PDF and every report would call the bundle clean without mentioning it; binary files are therefore **inventoried** in every bundle report and in the consent prompt, and `/f/` will set `Content-Disposition: attachment` for anything outside the image allow-list — the response typed them but never said how they should be presented, and "served as an attachment" described a behaviour no header asked for. Extraction is **not** refused: the first draft refused it on the grounds that a PDF parser is dependency weight and the media pipeline is where it would belong "if ever wanted", and Roteiro has extracted PDF text since before this ADR — `pdf-extract`, gated behind `pdf-text`, size-bounded and panic-guarded in `extract.rs`. The stronger correction is that **refusing to extract is refusing to screen**: a binary is unscreenable because nothing reads it, so the inventory is a floor and not a ceiling. Which formats can be read, and who decides per file, moves to ADR-0025. Closes four of the screen's stated exclusions, reshapes the non-English one around language-independent control tokens plus additive dictionaries, and refuses only semantic judgement. `decode_depth` defaults to 5 and is configurable; dictionaries may only ever *add* patterns, because a screen whose configuration can weaken it fails silently in the repository that weakened it. Records that the homoglyph exclusion argued against "contains Cyrillic" rather than against UTS #39's mixed-script rule. |
| 0.2 | 2026-09-03 | **Option A rejected the external scanner for two reasons that were false, and they are replaced rather than removed.** It claimed "a subprocess per concept over a 9,511-concept bundle" and "a Python runtime as a hard dependency of `import`"; `okfguard scan -r` is one process over a tree, `--json` is per-file NDJSON, and a standalone binary consulted when present is optional. Both were answered on #723 before this ADR was corrected, so the record and the public answer disagreed until now. The decision stands on three reasons about what the interface can express: the screen is per **field** rather than per file, and downgrades a `Block` on a title because a title falls back to its filename while a body does not; `Screened::admit` returns admissible **text** rather than a label, so an exit code cannot supply what lands in `meta.content`; and a screen consulted only when installed is absent, silently, wherever it is not — subtraction by omission, which is the failure the additive-only dictionary rule exists to prevent. |

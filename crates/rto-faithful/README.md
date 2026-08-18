# rto-faithful

**Rendering faithfulness** for [**Roteiro**](https://roteiro.dev) — the guard that
keeps *"deterministic tools find defects; a model only renders them"* a fact
rather than an intention.

Roteiro's review architecture splits the work in two. Tools find; a model turns
what they found into prose a human wants to read. The model does not review, does
not judge, and does not decide what is worth saying. This crate enforces the half
that is checkable: **every claim in a rendered summary must trace to a specific
tool finding.** A claim citing nothing is a fabrication. A citation naming no
finding is a dangling reference. Both are decided by set membership alone — no
code is read, no network is touched, no model is consulted.

```rust
let verdict = rto_faithful::check(&findings, &rendering);
assert!(verdict.is_faithful());
```

The public surface is that one entry point plus the types it operates on —
`Rendering`/`Segment` in, `Verdict`/`Defect` out — and `STRUCTURAL_EXEMPTIONS`,
which a renderer reads in order to emit a connective the checker will accept.
Nothing else. That sentence is asserted by a test rather than trusted, because
the first draft of this crate described a surface it did not have: a whitespace
helper shipped `pub` while the prose said otherwise. Prose drifting from the
contract it describes is the defect class this crate exists to catch.

## What counts as a claim

An **explicitly delimited span**. The renderer returns a sequence of segments and
says, per segment, which findings it rests on; this crate never parses prose to
find claim boundaries.

Sentence and paragraph boundaries both have to be *computed*, and computing one
means guessing where a sentence ends in text carrying `rto_graph::findings.rs`,
`Vec<String>` and `1.21.1`. A guessing guard is the model-shaped reasoning the
architecture exists to remove. Paragraphs are computable but far too coarse. So
the boundary is declared by the renderer, and declaring it is part of the
contract.

A segment may instead be **structural** — a connective from a short frozen list
(`"In summary:"`, `"Here are the findings:"`, …). Anything not on that list needs
a citation, which is what stops a renderer relabelling its inventions as
scaffolding. **The list does not grow to accommodate a rendering that failed.**
When a rendering fails here, fix the renderer.

Two kinds of sentence are deliberately off the list: anything with a **count**
(*"Here are three findings:"* asserts something about the finding set and can be
wrong) and anything asserting **emptiness** (*"No findings."* — same reason, and a
review that found nothing has nothing to render).

## What it does not catch

A passing verdict means **no claim was invented**. It does not mean the summary is
true, complete, or fairly weighted.

- **Distortion.** A rendering can cite every claim correctly and still mislead —
  through ordering, emphasis, proportion, or a wrong causal gloss on a right fact.
  None of those is set membership.
- **Aboutness.** A citation is checked to *resolve*, never to be *relevant*.
  Pairing real claims with real but unrelated keys passes.
- **Riders.** A fabricated clause inside a span that cites a real finding is
  invisible; the renderer picks the granularity, and coarser spans leave more
  room.
- **Omission.** Dropping half the findings is faithful by this definition.
  Coverage is a different property.

Each of these is pinned by a fixture in `tests/fixtures/` that records a *clean*
verdict on a misleading rendering, so the boundary is met by anyone extending the
crate rather than discovered later.

## Not stored

A rendering is ephemeral — local to whoever ran the review, not an artifact kept
for later (ADR-0020 §4). Nothing here writes a row, opens a store, or knows one
exists. Findings are identified by `rto_graph::FindingKey`, so a citation is the
same string the rest of the system already uses to address a finding.

- **Docs:** <https://roteiro.dev>
- **Source & issues:** <https://github.com/OffeneDatenmodellierung/Roteiro>

Dual-licensed under MIT or Apache-2.0.

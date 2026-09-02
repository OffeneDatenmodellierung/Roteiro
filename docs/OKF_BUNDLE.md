---
site-page: okf-bundle
site-nav: OKF bundle
site-order: 23
---

# The OKF bundle: a build output, and its one stable interface

`roteiro render okf` writes an **Open Knowledge Format** bundle (v0.2) into
`okf/` (or `--out <dir>`): one markdown concept document per graph node, with
YAML frontmatter, nested by kind — and by workspace member when rendering a
workspace.

[OKF](https://github.com/GoogleCloudPlatform/open-knowledge-format/blob/main/SPEC.md)
is Google Cloud's vendor-neutral specification for the pattern this output has
always been: a directory of markdown documents describing concepts, linked to
each other. Its only hard requirement is that every concept carries a non-empty
`type`.

## It replaced the Obsidian vault in 4.0.0, and Obsidian still reads it

`render obsidian` is gone. This is not a loss of Obsidian support: a bundle is
markdown with YAML frontmatter linked by ordinary markdown links, and Obsidian
parses all three — **Open folder as vault still works**. What changed is that the
output targets an open specification with other consumers rather than one
application's conventions.

Two differences an Obsidian user will notice:

- there is no `_Home` note; the bundle root and every section directory carry
  an `index.md` instead. A workspace member's own directory carries none — it is
  a container, and the root index links straight through to `<member>/<section>`,
  so nothing in the bundle is unreachable;
- links are `[title](/path.md)` rather than `[[wikilinks]]`. Obsidian resolves
  both, and only the second is Obsidian-specific.

## What the frontmatter carries, and why it is worth reading

Most OKF producers will emit `type` and little else. Roteiro emits the part the
specification treats as optional and important — **provenance** — because the
graph already distinguishes what a person wrote from what a machine derived.
OKF turns that into trust tiers (§5.3), which a consumer derives from `verified`:

| the graph's provenance | frontmatter | tier a consumer derives |
| --- | --- | --- |
| authored — ADR and blueprint prose | `verified: [{ by: human:<id> }]` | **human-reviewed** |
| derived — deterministic extraction | `verified: [{ by: roteiro/<version> }]` | machine-confirmed |
| inferred — heuristic, carries a confidence | `generated:` alone | unverified |
| external-authored / external-derived — imported from a peer's bundle | that peer's own `verified:` block, re-emitted verbatim | whatever **they** claimed |
| external-inferred — imported unverified, or acknowledged rather than trusted | `generated:` alone | unverified |

`Derived` is machine-confirmed rather than unverified deliberately: it is
reproduced from the AST at a known commit, so a consumer can re-derive it.
`Inferred` gets no `verified` key at all, because claiming otherwise would
launder a guess into a confirmation.

## Roteiro reads a bundle too — a *peer's*, never its own output

`roteiro import --from okf <path>` imports another repository's bundle as
external knowledge. It exists so a cross-repo `[[other-repo#thing]]` reference
resolves to a real concept instead of a placeholder holding only a key.

**It does not make the directory below an input.** That one is still emptied and
rebuilt on every render, and there is no round trip through it. What you import
is a bundle some other repository published.

Imported concepts are tagged `external-*` and can never be mistaken for this
repository's own work. By default they arrive at `external-inferred` whatever the
bundle claimed — their information without their confirmation — because a command
you ran by hand should not silently adopt a stranger's `verified:` block as this
graph's human-reviewed tier. `--trust` preserves the tiers they claimed, and
re-emits their verifier by name on the next render.

Re-running the command is safe in both directions: it replaces that peer's layer
wholesale, so nothing duplicates, and a concept the peer has since deleted is
removed rather than left behind as an orphan.

A bundle is read liberally — an unrecognised `type` is imported, a document that
carries no frontmatter or no `type` is skipped and the reason is printed — but a
directory in which nothing at all parsed is refused rather than imported as
nothing.

### The reader is tested against bundles Roteiro did not write

Reading back one's own output proves a round trip, not interoperability. Since a
reader tested only against its own writer will agree with itself about a dialect
it also invents, the reader is exercised against two of the four bundles
published in the specification's own repository — vendored under
`crates/rto-render/tests/fixtures/okf-upstream/`, provenance and licence recorded
alongside them.

That test found four defects the round trip could not, all of them silent. The
reader hand-parsed a line-oriented YAML subset shaped like Roteiro's own emitter,
and against real third-party markdown it dropped flow mappings
(`generated: { by: …, at: … }`, the form the specification's own examples use),
dropped flow sequences, dropped block sequences whose items sit at the parent
key's indentation (PyYAML's default), and *truncated* folded multi-line scalars
at their first line. Nothing was skipped and nothing was reported.

The trust consequence was the serious one: all nine concepts of the published
`acme_retail` bundle read as **unverified** when eight carry a human sign-off, so
`import --from okf --trust` adopted nothing while reporting success. The reader
now parses frontmatter with a real YAML parser, and the counts are cross-checked
against an independent implementation (below).

### You do not have to remember the command

A workspace member that publishes a bundle at its conventional `okf/` path is
**found automatically** during the workspace scan, and you are asked about it
once:

```
spoke publishes an OKF bundle, and this graph references it.

bundle:   /home/you/GIT/spoke/okf
asking:   not seen before
contains: 12 concept(s), 1 quarantined, 0 blocked by the content screen [invisible-characters]

[t] trust       import at `external-<their tier>`, keeping what they claimed
[a] acknowledge import at `external-inferred`: their information, not their confirmation
[i] ignore      leave the cross-repo placeholder as it is
```

The answer is recorded per peer, so it is asked **once, not every sync**. It
lapses if the bundle moves, or if it starts carrying a class of screening finding
it did not carry when you answered — an ordinary edit does not re-ask you.

You are only asked about peers this repository already references (it holds an
`extref:` placeholder for them), because those are the ones importing would
actually help. `roteiro import --from okf <path>` is still how you read anything
else, and running it records your answer too.

**When there is nobody to ask — a server, a CI job, a pipe — the bundle is
ignored, mentioned once, and nothing is recorded.** A graph does not adopt a
stranger's concepts because nobody was there to object, and a silent run must
never leave behind an answer a person did not give.

### A peer's prose is screened before it is stored

Imported text ends up in `meta.content`, which search results hand to a language
model as grounding. So it is screened first, deterministically — no model is
asked to judge it:

| outcome | what happens |
|---|---|
| **pass** | stored unchanged |
| **quarantine** | the concept is imported; its text is either stripped of invisible characters and hidden regions, or withheld entirely |
| **block** | the concept is not imported at all |

**Block is reserved for text that is both hidden and directive** — instructions
to a model inside an HTML comment, behind `display:none`, or spelled with
zero-width characters. A document that merely *discusses* prompt injection is
quarantined, not refused: its concept, kind and relationships still arrive, so
the cross-repo placeholder still resolves. What it loses is the prose.

The screen catches invisible codepoints (zero-width, bidi controls, the tag
block, C0/C1 controls), content hidden by presentation, and English phrases that
read as instructions to a model. It deliberately does **not** attempt homoglyph
detection, decoding of encoded payloads, non-English patterns, or CSS cascade
resolution — see `crates/rto-graph/src/screen.rs`, which says so at length.


## OKF validation: what Roteiro borrows, and what it refuses

The obvious next feature is `roteiro okf validate` — a conformance gate over a
stranger's bundle. The question is not whether to have one but how much of it to
import, and the answer turned out to be "the model, not the checker".

[`W4G1/okf`](https://github.com/W4G1/okf) is a pure-Rust OKF **v0.2** toolkit on
crates.io (Apache-2.0, compatible with this project's `MIT OR Apache-2.0`). Its
`okf-validator` crate does strict conformance checking with a severity split,
`okf lint` adds hygiene rules, and the `okf` CLI also offers `trust`, `info`,
`links`, `graph`, `computations`, `diff` and `fmt`. It is current, and it is
substantially more complete than anything worth writing here as a side quest.
Notably `okf-core`, the model and parser underneath it, has **zero
dependencies**.

Two other ecosystem tools were read and are *not* suitable as references:
[`okflint`](https://github.com/mattdav/okflint) is a generic engine that
validates against a manifest the producer writes, so it answers "does this match
the rules I declared" rather than "does this conform to OKF v0.2" — and it cannot
run on a third-party bundle at all without one being authored first.
[`okf-schema`](https://github.com/gsemet/okf-schema) ships no canonical OKF
schema; every schema lives inside the bundle being checked and is the producer's
own.

So the standing position is:

- **`okf-core` is adopted.** Roteiro's reader is built on it rather than on a
  second hand-rolled reading of the same specification. It carries **zero**
  dependencies, so it costs one lockfile entry. This is the "worth revisiting"
  above, revisited and taken.
- **`okf-validator` is refused, and the measurement is why.** Adopting it was
  attempted and reverted. None of its dependencies is optional, and it
  syntax-checks fenced code blocks in eight languages, so taking it means taking
  `rustpython-parser`. `cargo deny --all-features check` then fails **thirteen
  times**: seven licence rejections (five `malachite*` crates are
  `LGPL-3.0-only`, plus `tiny-keccak` CC0-1.0 and `unicode_names2`
  Unicode-DFS-2016) and six unmaintained advisories against the `unic-*` crates
  (RUSTSEC-2025-0098, -0100), whose own text says *"No safe upgrade is
  available"*.

  Every one of the thirteen traces to `rustpython-parser` alone — `oxc_parser`
  (58 crates), `sqlparser` (18) and `syn` contribute none. And that parser
  powers exactly **two of the validator's thirty-four checks**,
  `check_code_block_syntax` and `check_computation_script_syntax`; `lint.rs`
  never touches it. So the price is not "a conformance checker", it is a Python
  parser, and ADR-0017 §3 says a licence must not be admitted merely to turn CI
  green.
- **Whether a bundle's *code* parses is not the format's question.** An
  interchange format should validate its own structure; checking that a fenced
  Python block is syntactically valid is a linter's job, and a job this
  workspace already has tree-sitter grammars for. Upstream has been asked to put
  the language parsers behind features
  ([`W4G1/okf`](https://github.com/W4G1/okf)); with that, `okf-validator` would
  become takeable at `default-features = false`.
- **The structural checks are written here, and agree with upstream.**
  `crates/rto-render/src/okf/conform.rs` implements them over `okf-core`'s model
  rather than re-deriving the spec — the failure mode the rest of the ecosystem
  demonstrates. Nothing there parses OKF: frontmatter, trust tiers, actors,
  links, footnotes, headings and computations all come from `okf-core`, and only
  the questions are ours.

  Measured against `okf-validator` as a differential oracle over all four
  published bundles: upstream reports **200** diagnostics, this implementation
  reports **194**, and the entire difference is the **six code-syntax warnings**
  that `roteiro okf syntax` now owns. Nothing is reported here that upstream does
  not. Hygiene agrees exactly — 26/26, 29/29, 78/78 and 39/39.

  The comparison earned its keep: four rules were wrong before it was run, each
  inventing a requirement the specification does not state. The sharpest was a
  warning for a missing `okf_version`, which §8 and §12 both make a *MAY* — it
  fired on all four of the specification's own bundles.
- **Use it as a differential oracle, then let it go.** It was run as a separate
  binary — never a dependency, of this workspace or of its test suite — and the
  agreement it established was frozen into `tests/okf_interop.rs`.

The comparison, against `okf-core` / `okf-validator` **0.2.6** (2026-08-27) on
2026-09-01, over every bundle published in the specification's repository at
commit `ad30107`:

| Bundle | Concepts | `okf trust` | Roteiro's reader | Result |
|---|---|---|---|---|
| `acme_retail` | 9 | 8 human-reviewed, 1 unverified | 8 `external-authored`, 1 `external-inferred` | agree |
| `ga4` | 9 | 9 unverified | 9 `external-inferred` | agree |
| `stackoverflow` | 26 | 26 unverified | 26 `external-inferred` | agree |
| `crypto_bitcoin` | 9 | 9 unverified | 9 `external-inferred` | agree |

Nothing was skipped by either side in any bundle. Roteiro's own `render okf`
output for this repository also validates clean: **0 conformance errors across
9,029 concepts**, judged by a validator that has never seen our output — a
stronger statement than `every_emitted_bundle_is_conformant` can make, since that
test encodes our own reading of §11.

**What removing the oracle costs, and what covers it.** A check that runs once
proves the code was right that day. Two of the four bundles are therefore
vendored as fixtures, so the suite permanently exercises a *foreign* bundle — the
thing phase 1 never did, and the source of every defect this found.

**The limit, stated so it is known rather than rediscovered:** the fixtures
cannot catch a divergence in a YAML shape that no vendored bundle contains. That
is not hypothetical. `stackoverflow` writes `tags: stackoverflow, posts,
deprecated` — a bare comma-separated string where §4.1 asks for a list — in seven
documents, and that bundle is *not* vendored. The oracle is how that shape was
found at all; it is now pinned by
`an_off_spec_shape_is_read_where_a_real_producer_writes_one`, but the **next**
such shape has no tripwire and would be found the same way or not at all.

So the standing advice is to re-run the comparison whenever the reader's parsing
changes, or a new bundle is imported in anger. It is one command
(`cargo install okf`), and `okf::read`'s module documentation records exactly what
to run and what the answer was last time. That is discipline, not automation, and
naming it as such is the point.

### What the four published bundles say about the spec

All four upstream bundles (`ga4`, `acme_retail`, `crypto_bitcoin`,
`stackoverflow`) are conformant — confirmed independently by `okf-validator` and
by `okflint`'s core stage. Two disagreements surfaced and are worth raising
upstream rather than encoding here:

- **`sources[].author` contradicts §7.** §5.1 says `author` uses "the actor
  convention (§7)", but the spec's own Appendix A writes
  `author: team:finance-fpa`, and `acme_retail` writes
  `author: team:data-platform` — neither of which is one of §7's three forms
  (`human:<id>`, `<producer>/<version>`, `process:<id>`). Either §7 needs a
  `team:` form or §5.1 should stop pointing at it. A validator enforcing §7 on
  `author` would flag the specification's own example.
- **`okflint`'s `S204` contradicts §5.** It requires `stale_after` to be a bare
  `YYYY-MM-DD` date and so flags all seven `stale_after` values in Google's
  `acme_retail`, even though §5 states that *every* timestamp-valued key is an
  ISO 8601 datetime with an explicit UTC offset, and §5.5's own example is
  `2026-09-23T00:00:00Z`. This one is a bug in the tool, not in the spec.
## The commands, and which of them gate

Everything under `roteiro okf` **reads**. None of it writes to the graph, and none
of it rewrites a bundle: `roteiro render okf` is the only writer, and
`roteiro import --from okf` is the only path by which a peer's content enters the
graph — with the consent gate above.

| Command | Answers | Gates? |
| --- | --- | --- |
| `okf info` | What is this bundle — size, tiers, staleness, links, computations | never |
| `okf validate` | Does it conform to OKF v0.2 | on any error |
| `okf lint` | Is it hygienic — `L1`–`L12`, plus our `R1` | never |
| `okf trust` | What does it claim about itself, and has any of it expired | `--check`, on staleness |
| `okf links` | Do its internal links resolve | `--check` |
| `okf syntax` | Does its fenced code parse | on any error |
| `okf computations` | What Attested Computations does it declare (§10) | `--check`, on an incomplete contract |
| `okf diff` | What changed between two bundles | never |
| `okf view` | Serves it as a website (`okf-viewer` feature, ADR-0022) | n/a |

Start with `info`; it composes the others' reports rather than deriving anything
of its own, so it cannot disagree with the command that reports a number in
detail.

Two rules hold across all of them. **`--json` selects a format and never changes
what is reported or whether the command gates** — settled on `main` by a bug where
it did both. And **the bare command reports without gating**: reporting is what
you want reading a stranger's bundle, gating is what you want in CI over your own,
and a command that only did one would be wrong half the time.

### Staleness is asked as of a day you name

`okf trust` and `okf info` take `--today YYYY-MM-DD`. §5.4's rule is
`now >= stale_after`, so the answer moves on its own — which makes the host clock
an input, and an undeclared one.

```
$ roteiro okf trust okf/ --today 2026-12-31
  human-reviewed 8, machine-confirmed 0, unverified 1
  stale 7 (as of 2026-12-31)
  human-reviewed     metrics/revenue — verified by human:alice [STALE since 2026-12-31T00:00:00Z]
```

Without it the host's UTC date is used and still printed, so a captured summary
says what it was true *of*. A malformed value is **refused** rather than quietly
replaced by today's date: the flag exists to make a run reproducible, and a typo
that restored the clock would leave a pipeline green and meaningless.

The combination worth looking for is the one above — **human-reviewed and stale**.
The tier alone reads as reassurance, and it is a claim about when somebody last
looked, not about whether it is still true.

## One place Roteiro guarantees more than the specification asks

§11 says consumers **MUST NOT** reject a bundle for broken cross-links. Roteiro
treats a broken authored link as *drift* and fails `roteiro check` over it. Both
are right — the specification is telling consumers to be liberal, and Roteiro is
a producer that promises more than it must. A Roteiro bundle should not contain a
broken link.

## The bundle is deleted and rebuilt on every render

The output directory is **emptied first**. Not merged, not updated in place —
removed, then recreated:

```console
roteiro render okf --out okf     # rm -rf okf, then write it
```

Nothing you put inside survives. Not a note you added there, not a folder you
organised.

This is deliberate and will not change. A bundle is a build output of the graph,
regenerated over itself, so a concept for a symbol you have since renamed does
not linger for ever. The alternative — merging into whatever is already there —
would mean the bundle accumulating concepts for code that no longer exists, which
is worse than losing a file you should not have kept there.

**Keep your own notes outside it and link in.** A sibling folder works; so does
anywhere your editor indexes alongside it.

## Names are derived from keys, and collisions are settled once

A concept's filename is a slug of its node key. Where two keys slug identically
within one directory, the later one carries a short digest of its key — the first
in key order keeps the bare name, so an unchanged graph renders byte-identically.

Long keys are truncated at 200 bytes, and a truncated name always carries the
digest: truncation can *create* a collision that the full keys did not have, by
merging two long keys that share a prefix.

This matters more than it sounds. The Obsidian vault this replaced wrote one flat
directory, and on macOS and Windows two names differing only in case are one
file — so it was once **104 notes short of the count it printed**, silently.
Nesting by kind makes most of that structural, and the renderer asserts that the
number of concepts it reports equals the number of files it wrote.

## Workspace bundles

`render okf -w <name>` renders **one bundle spanning a workspace's member
repositories**, nested per member:

```
okf/
  index.md
  app/
    files/…  decisions/…  symbols/…
  deploy/
    files/…  symbols/…
```

Nesting is what keeps two members' concepts apart. Node keys are
repository-relative, so every repository's `README.md` is the same key —
`file:README.md` — and a flat layout would have one overwrite the other. The
vault solved this by qualifying keys as `<project>::<key>` and hashing every
filename; directories make it structural instead.

Each member is read with **its own** configuration: `[ingest]` toggles and
`[debt] ignore` come from that repository's `roteiro.toml`, not from wherever the
command was run.

Bare `render okf` renders the current project alone, with sections at the bundle
root — deliberately *not* "the workspace containing this repo". A bare render
silently becoming a multi-repo one would move every concept a directory deeper
and break every link into the bundle with no error.

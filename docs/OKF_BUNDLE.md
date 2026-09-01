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

## Why Roteiro does not ship its own OKF validator

The obvious next feature is `roteiro okf validate` — a conformance gate over a
stranger's bundle. It was investigated and deliberately **not** built, because a
good one already exists.

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

- **No Roteiro validator.** Re-implementing `okf-validator` would duplicate real
  work and drift from the spec, which is the failure mode the rest of the
  ecosystem already demonstrates.
- **No runtime dependency on it either.** `okf-validator` pulls a JavaScript
  parser, a Python parser, a SQL parser and `syn` — 94 transitive crates — to
  syntax-check fenced code blocks. That is a large supply-chain surface for a CLI
  convenience, and ADR-0017 exists because this project takes that seriously.
  `okf-core` alone (zero dependencies) is a much better candidate and is worth
  revisiting for the reader itself.
- **Use it as a differential oracle, then let it go.** It was run as a separate
  binary — never a dependency, of this workspace or of its test suite — and the
  agreement it established was frozen into `tests/okf_interop.rs`.

The comparison, against `okf-core` / `okf-validator` **0.2.6** (2026-08-27) on
2026-09-01, over every bundle published in the specification's repository at
commit `ad30107`:

| Bundle | Concepts | `okf trust` | Roteiro's reader | |
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
thing phase 1 never did. What the fixtures cannot catch is a divergence in a
shape no vendored bundle contains; `okf-validator` reading a fifth bundle
differently would still be news. Re-running the comparison is one command
(`cargo install okf`), and `okf::read`'s module documentation records exactly
what to run.

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

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

`Derived` is machine-confirmed rather than unverified deliberately: it is
reproduced from the AST at a known commit, so a consumer can re-derive it.
`Inferred` gets no `verified` key at all, because claiming otherwise would
launder a guess into a confirmation.

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

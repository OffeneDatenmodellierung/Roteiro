---
site-page: obsidian-vault
site-nav: Obsidian vault
site-order: 23
---

# The Obsidian vault: a build output, and its one stable interface

`roteiro render obsidian` writes one markdown note per graph node into `vault/`
(or `--out <dir>`), with each node's edges as `[[wikilinks]]`, so the
provenance-tagged graph is browsable in Obsidian's graph view.

Two things about it are easy to learn the expensive way. Both are stated here
before either can cost you anything.

## The vault is deleted and rebuilt on every render

The output directory is **emptied first**. Not merged, not updated in place —
removed, then recreated:

```
roteiro render obsidian --out vault   # rm -rf vault, then write it
```

**Nothing you put inside the vault survives a render.** Not a note you wrote
there, not a folder you organised, not an Obsidian workspace layout.

This is deliberate and it is not going to change. A vault is a build output of
the graph, and it is regenerated over itself so that the note for a symbol you
renamed six months ago does not linger for ever alongside the one for its
replacement. Orphan cleanup and "your files are safe in here" cannot both be
true, and for a directory whose entire contents are derived from the code, the
first is the useful one.

So the design that makes the vault usable alongside your own writing is:

> **Your notes live outside the vault and link into it.**

That is the arrangement issue #442 settled on, and the paragraph above is why it
is load-bearing rather than a matter of taste. Put your own notes in a sibling
folder inside the same Obsidian vault root — or anywhere Obsidian indexes that
is not the render target — and link into the generated notes by name.

## Note names are the interface, and they changed once

Because the directory is rebuilt, the only thing that persists across a render
is a note *outside* it pointing *in*. Obsidian resolves `[[wikilinks]]` by name.
So the **note names are the entire stable interface the vault exposes**, and
they are worth understanding.

A name is the node's key, made filename-safe, with a hash of the key appended:

| node key | note |
| --- | --- |
| `adr:0001` | `adr-0001-559a2e837953b2ff.md` |
| `file:src/main.rs` | `file-src-main.rs-4a72627453f6780e.md` |
| `sym:rust:src/a.rs#Store` | `sym-rust-src-a.rs-store-b4cbf6633003361f.md` |

The readable part is a hint — lowercased, with anything outside `[a-z0-9._-]`
collapsed to `-`. The 16 hex digits are what make the name **unique**: they are a
hash of the whole, exact key, so no two nodes can land on one file. The full key
is in each note's frontmatter as `key:`, so you can always read a name back to
the node it came from.

You are not expected to type these. Obsidian's autocomplete finds a note from
the readable part; the suffix is there so that the note it finds is the right one.

### The break, and why there is no migration

Before **issue #574** the hash was only appended when a name would otherwise
overrun the filesystem's length limit, and names collided two ways:

- anything outside the safe set became `-`, so `…cytoscape.min.js#$a` and
  `…cytoscape.min.js#a` were one name;
- **macOS and Windows fold filename case**, so `…#A` and `…#a` were two *names*
  but one *file*.

The second is the larger and the quieter: it does not happen on Linux, so a CI
run on Ubuntu could not see it. Measured on Roteiro's own repository, the render
reported 8,239 notes and wrote 8,135 files. 104 nodes had no note, and nothing
said so.

Making every name carry the hash fixes both. The cost is that **every note was
renamed** — all 8,239 of them here; only `_Home.md` kept its name — and there is
**no migration**. A name is derived from its key, and the old name is not
recoverable from the new one, so nothing can rewrite your links for you.

If you have hand-written notes that link into a vault rendered by an earlier
version, those links now resolve to nothing. Re-render, then re-point them —
Obsidian's autocomplete will find the new names from the same readable text you
were already typing. It is a one-time cost, and it was taken now on the grounds
that it only grows: every vault that exists between the defect and the fix is
another set of links that would have to be repaired later instead.

## Workspace vaults

`render obsidian -w <name>` renders **one vault spanning a workspace's member
repositories** rather than the current project alone. Node keys are
repository-relative — every member's `README.md` is `file:README.md` — so member
notes are keyed `<project>::<key>` and named accordingly.

That qualification is what stops members overwriting *each other*. It never
addressed the two mechanisms above, which acted *within* each member, so before
#574 a workspace vault lost a member's worth of notes per member: 104 at one
member, 832 at eight. The hash is what makes the count the render prints equal
the number of files it wrote, in both modes.

Bare `render obsidian` renders the current project with unqualified names even
when that repository is a member of a configured workspace. Workspace mode is
opt-in by name, always — deliberately *not* inferred from the working directory
the way `roteiro links -w` defaults, because a rename that arrives because you
changed directory is a bug, whereas one that arrives in a release is a changelog
entry.

## The count is now an assertion

`render obsidian` prints how many notes it wrote:

```
rendered obsidian vault → vault (8239 note(s) + _Home.md)
```

That number is now the number of files on disk. If two notes ever do share a
name, the render says so on stderr and names the two keys — every name carries a
hash of its key, so a collision would be a hash collision, which is a bug worth
reporting rather than something to work around.

---
Title: The graph's shareable form is an OKF bundle — replacing the Obsidian vault
Space: ARCH
Parent: ADRs

# ADR-specific metadata (unknown keys are ignored; used for indexing/search)
type: adr
adr-id: "0021"
status: Accepted                    # Draft | For Review | Accepted | Rejected | Superseded
architectural-significance: HIGH    # SOFT | LOW | MEDIUM | HIGH | VERY HIGH
domain: Developer Tooling
decision-makers: ["The Roteiro Project Team"]
superseded-by:
version: "1.0"
last-modified: 2026-08-29
confluence-url:
---

# ADR-0021: The graph's shareable form is an OKF bundle — replacing the Obsidian vault

| | |
|---|---|
| **Document version** | 1.0 |
| **Status** | Accepted |
| **Decision makers** | The Roteiro Project Team |
| **Related** | [[docs/adr/0009-cross-repo-workspace-links.md]] · [[docs/adr/0012-analyzer-findings-artifact-model.md]] |

## Reference

- The specification: <https://github.com/GoogleCloudPlatform/open-knowledge-format/blob/main/SPEC.md> (v0.2, published 12 June 2026)
- The renderer: [[crates/rto-render/src/okf.rs]]
- Issue #663

## Context

`roteiro render obsidian` wrote one markdown note per graph node into a flat
directory, with edges as `[[wikilinks]]`, for browsing in Obsidian's graph view.

It was **one-way**. Roteiro wrote it; nothing read it back; no tool but Obsidian
consumed it. It was 3,803 lines of renderer serving one application's import
conventions, and a substantial share of that existed to survive its own layout:
one flat directory on filesystems that fold case, where two names differing only
in case are one file. That defect was not theoretical — this repository's vault
was once **104 notes short of the count it printed**, silently, and the fix was
to append a hash of the key to every filename.

Google Cloud published the Open Knowledge Format on 12 June 2026: a
vendor-neutral specification for the pattern the vault already implemented — a
directory of markdown concept documents with YAML frontmatter, linked to one
another. Its only hard requirement is a non-empty `type`.

## Decision

**The shareable form of the graph is an OKF v0.2 bundle. `render obsidian` is
removed in 4.0.0 and replaced by `render okf`.**

Four things follow, and each is load-bearing.

### The provenance mapping is the reason this is a good fit, not the markdown

OKF derives **trust tiers** (§5.3) from a `verified` key. Roteiro already
computes the distinction those tiers describe, so the mapping is a rename rather
than an invention:

| [[crates/rto-graph/src/provenance.rs#Provenance]] | frontmatter | tier |
|---|---|---|
| `Authored` — ADR and blueprint prose | `verified: [{ by: human:<id> }]` | **human-reviewed** |
| `Derived` — deterministic tree-sitter extraction | `verified: [{ by: roteiro/<version> }]` | machine-confirmed |
| `Inferred` — heuristic, carries a confidence | `generated:` alone | unverified |

`Derived` is **machine-confirmed rather than unverified** deliberately: it is
reproduced from the AST at a known commit, so a consumer can re-derive it and get
the same answer. `Inferred` gets no `verified` key at all, because a similarity
judgement that claimed confirmation would launder a guess into a fact — which is
the distinction the whole graph exists to preserve.

§7 makes the `human:` prefix load-bearing: it is the only thing separating
human-reviewed from machine-confirmed, and producers **MUST** use it for
hand-authored content. Where the authoring human cannot be determined, the
concept claims **no** confirmation rather than the tool's — substituting a
machine actor would move every authored concept down a tier silently, which is
worse than claiming nothing.

**The human is resolved per document, not per repository.** `verified: [{ by:
human:<id> }]` asserts that *that person* stands behind *that document*, so the
name is the author of the commit that last changed the document's own path
([[crates/rto-graph/src/git.rs#Repo]] `last_authors`), and the `at` is that
commit's time rather than the render's. The cheap answer — the `HEAD` commit's
author — is a false claim at scale: a bot merge at `HEAD` would record every ADR
in the repository as human-reviewed by the bot. Claiming a confirmation nobody
made is the worst error available in a format whose whole point is the
distinction, so where the history cannot be read the concept goes unverified.

### Nesting replaces a naming scheme

Concepts nest by kind, and by workspace member when a workspace is rendered. Two
members' `file:README.md` land on different paths because the directories differ,
not because the keys were qualified and the filenames hashed.

This retires both mechanisms the vault needed rather than porting them. The
renderer still asserts that **the number of concepts reported equals the number
of files written**, because that is the property whose failure was invisible.

**A link is resolved against the placement, never re-derived from the key.** A
concept's path depends on the whole set — the member directory it nests under,
and a disambiguating digest when two keys slug alike — so any rule that turns one
key into one path in isolation is guessing, and guessed wrong for 43 links in a
real render of this repository. `assemble` places every concept first, records
`key -> path`, and resolves relationships through that map; a key the map does
not hold is not in the bundle, and its link is dropped rather than written as a
path that does not exist.

### The bundle is a function of the commit

Every timestamp comes from git: a document's own last-change time where it has
one, the `HEAD` commit's time otherwise. Rendering the same commit twice
therefore produces the same bytes, which is what lets a consumer diff two
downloads and learn something. A wall-clock reading would make every render
differ while describing an identical graph.

### Obsidian keeps working, and that is a consequence rather than a goal

A bundle is markdown with YAML frontmatter linked by ordinary markdown links.
Obsidian parses all three, so *Open folder as vault* still works. This is not a
compatibility shim: it follows from the format being plain markdown. An Obsidian
user loses the `_Home` note (each directory carries an `index.md`) and
`[[wikilinks]]` (Obsidian resolves markdown links too).

## Consequences

**This is a breaking change**, hence 4.0.0: `Target::ObsidianVault` is removed
from a public enum. `render obsidian` fails with a message naming its
replacement and stating that Obsidian still reads the output, because a script
that hits a bare "unknown target" learns nothing.

**Roteiro guarantees more than the specification asks.** §11 says consumers
**MUST NOT** reject a bundle for broken cross-links. Roteiro treats a broken
authored link as drift and fails `roteiro check` over it. Both are right: the
specification is telling consumers to be liberal, and Roteiro is a producer that
promises more than it must.

**Two capabilities are not yet re-implemented, and are recorded rather than
quietly dropped:** the workspace version-pin table
([[docs/adr/0009-cross-repo-workspace-links.md]] v1.19) and the cross-member
findings summary ([[docs/adr/0012-analyzer-findings-artifact-model.md]] v1.3).
Both lived in the vault's `_Home`, which has no OKF counterpart. A capability
that stops being published without anyone saying so is precisely what a version
history is for.

**The specification is v0.1-adjacent and will move.** OKF v0.2 describes itself
as a starting point. Pinning the version in the bundle root's `index.md`
(`okf_version`) is what makes a later change legible rather than silent.

## Alternatives considered

**Keep both targets.** Rejected: two document serializations of one graph must be
kept in step by whoever edits either, and the vault had no consumer to justify
the cost.

**Deprecate `obsidian` and remove it at the next major.** Rejected by the owner:
the vault is one-way with no known users, so a migration window would defer the
removal without protecting anyone.

**Adopt OKF additively and leave the vault alone.** Rejected for the same reason
as keeping both, plus it would leave the 104-note defect class alive in a
renderer nobody maintains.

## Document version history

| Version | Date | Notes |
|---------|------|-------|
| 1.0 | 2026-08-29 | Accepted, and implemented in the same change (issue #663). Records the replacement of `render obsidian` by `render okf`, the provenance-to-trust-tier mapping that motivates it, that the `human:` verifier is resolved per document rather than per repository, that links resolve against the placement rather than the key, that the bundle is dated by the commit, and the two `_Home` capabilities — the version-pin table and the findings summary — that have no OKF home yet. |

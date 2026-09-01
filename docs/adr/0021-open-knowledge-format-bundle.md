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
version: "1.2"
last-modified: 2026-09-01
confluence-url:
---

# ADR-0021: The graph's shareable form is an OKF bundle — replacing the Obsidian vault

| | |
|---|---|
| **Document version** | 1.2 |
| **Status** | Accepted |
| **Decision makers** | The Roteiro Project Team |
| **Related** | [[docs/adr/0009-cross-repo-workspace-links.md]] · [[docs/adr/0012-analyzer-findings-artifact-model.md]] · [[docs/adr/0019-remote-model-tier.md]] · [[docs/adr/0008-multi-repo-workspace-serve.md]] |

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
| `ExternalAuthored` / `ExternalDerived` — imported (v1.1) | the peer's own `verified:` block, re-emitted verbatim | whatever **they** claimed |
| `ExternalInferred` — imported, unverified or acknowledged | `generated:` alone | unverified |

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

A **shallow clone** is that case, and it is the one that would have undone this
quietly. At the shallow boundary a commit's parents are absent, so "changed
relative to every parent" is unknowable — and read naively, the single commit a
`fetch-depth: 1` checkout has appears to have introduced the whole tree. A
boundary commit therefore confirms nothing, and the workflow that publishes the
bundle asks for full history so the published artifact is attributed rather than
blank.

**A value cannot forge a key.** Every scalar written into frontmatter comes from
somewhere a person can put anything — a git author name, a heading, a key derived
from a path — so scalars are quoted unconditionally *and* every control character
is escaped. A raw newline in a title does not merely produce ugly YAML: the text
after it begins a line at column 0, and `verified:` written there becomes a
sibling of the title rather than part of it. In a document whose frontmatter
decides a trust tier, that is the injection that matters.

**Only the decision carries the decision's `status`.** An ADR's file, and the
intent-debt markers inside it, are different concepts that happen to share a
path; `status: stable` on a `TODO` inverts what the marker is for. Sections keep
it, because a section of a superseded decision is superseded.

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

**A cross-repo reference resolves to the other member's concept, not to the stub
standing in for it.** A member's graph records a reference into a sibling
repository as an `extref:<project>::<key>` placeholder, because that member
cannot see the target ([[docs/adr/0009-cross-repo-workspace-links.md]]). A
workspace *bundle* can: the sibling is in it. Resolving the placeholder's own key
produces a link that works and teaches nothing — a document whose whole content
is that it is not the document the reader wanted — so the reference follows
through to the real concept, falling back to the placeholder only when that
member or that concept is genuinely absent.

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
user loses the `_Home` note (the bundle root and every section directory carry
an `index.md`) and
`[[wikilinks]]` (Obsidian resolves markdown links too).

### Reading a peer's bundle is the half that makes vendor-neutral pay (v1.1)

The criticism this ADR levelled at the vault was that it was **one-way**: Roteiro
wrote it, nothing read it back. Adopting a specification with other producers and
then reading none of them would swap a proprietary one-way format for a standard
one-way format. So a repository may now import **another repository's** bundle
(issue #706, `roteiro import --from okf <path>`).

**This does not make Roteiro's own output an input.** `render okf` still empties
and rebuilds its output directory on every render, and nothing put inside it
survives — see [[docs/OKF_BUNDLE.md]]. The reader consumes a *peer's* bundle; the
render target remains a build output with no round trip through it.

**Imported facts are `external-*`, and the tier is carried rather than
collapsed.** A foreign bundle's `verified: [{ by: human:alice }]` means Alice
confirmed it *in her repository*. Importing that as `Authored` would assert that
this graph human-authored it — laundering someone else's confirmation, the exact
failure this document names when it says a similarity judgement claiming
confirmation "would launder a guess into a fact".

The variant carries the peer's tier because `origin_for` matches provenance
exhaustively. A flat `External` would force one arm and therefore one answer for
everything imported: mapping to *unverified* downgrades a peer's human-reviewed
concept, mapping to *machine-confirmed* upgrades their similarity guess — and
`render okf` then re-emits the flattened tier outward to the next consumer.
Laundering by round-trip, in the one format chosen because it can express the
difference. **Externality flattens to one level**: a fact imported from B that B
imported from C is `external-*`, not doubly so; which repository it came from is
the import layer's `src_ref`.

**A trusted concept re-emits the peer's own `generated`/`verified` block**,
recovered from the node's `meta.okf.origin`. That is what makes the round trip
honest — Alice's confirmation leaves here still naming Alice — rather than being
re-tiered to unverified on the way out.

**Trust is asked for, never assumed.** The manual command defaults to
*acknowledge*: concepts arrive at `external-inferred` whatever the bundle
claimed, so cross-repo references resolve to real content without this graph
adopting a stranger's confirmations. `--trust` preserves the tiers they claimed
and is a deliberate act. What they claimed is recorded in `meta.okf.claimed`
either way, so the decision is reversible without re-reading anything.

**An imported concept fills the `extref:` placeholder it corresponds to**
([[docs/adr/0009-cross-repo-workspace-links.md]]), which is the payoff: a
cross-repo reference stops resolving to a stub whose whole content is that it is
not the document you wanted. The correspondence is computed **forwards** — this
graph's own placeholder key through the writer's `slug`/`section_for` rule — and
never by inverting a filename, because `slug` is lossy and two keys can produce
one name. An ambiguous match fills **nothing**: a wrong fill attaches a peer's
content to the wrong node, which is worse than an unfilled stub.

**A bundle is read liberally and refused loudly.** An unrecognised `type` is
imported (the specification leaves `type` open), a document with no frontmatter
or no `type` is skipped *with its reason reported*, and an edge to a concept the
bundle does not contain is dropped and counted. A directory in which **nothing**
parsed is refused whole, because importing zero concepts while exiting zero
reports success for having done nothing.

**Discovery is automatic, and consent is not.** A workspace member that
publishes a bundle at its conventional `okf/` path is found during the workspace
scan, so nobody has to remember `roteiro import --from okf`. What is *not*
automatic is reading it: on first contact the operator is asked to **trust**,
**acknowledge** or **ignore**, once, and the answer is recorded against that
source. Automation and consent compose here rather than conflicting — the prompt
is precisely what stops automatic discovery becoming consent-by-installation.

Three things follow, and each was a decision rather than a detail.

**Discovery is scoped to peers this graph already references.** A repo is asked
about peer *P* only when it holds an `extref:P::…` placeholder. Otherwise an
*n*-member workspace would ask *n×(n−1)* questions on first run, most of them
about bundles that would fill nothing — and a prompt nobody can finish reading
is answered by reflex, which is a worse gate than none.

**Unprompted means `ignore`, and it is said once.** A server, a CI job and a
piped invocation all lack a human, and the rule is uniform across them rather
than special-cased. `ignore` is the default because the three answers are not
symmetric in what they cost when wrong: `ignore` leaves the graph as it is
today, and the cost of being wrong is a feature that did not happen;
`acknowledge` writes a stranger's prose into a graph a language model reads as
grounding, and the cost of being wrong is a payload delivered. Refusing to start
— [[docs/adr/0019-remote-model-tier.md]]'s answer for an egress call — is wrong
here, because the bundle is an enhancement rather than the workload, and it
would let any member wedge another person's server by adding a directory.
**Nothing is recorded** on that path: a server's silence must never become an
answer a later interactive run reads back as a decision.

**The recorded answer is a departure from ADR-0019, deliberately.** That ADR
persists no grant at all — one is scoped to a process and dies with it. The
questions differ in how often they recur: a remote call is a deliberate act, and
one prompt per act is proportionate, whereas a workspace scan runs on every
`links`, every `sync` and every server start. The record lives in the consuming
repo's `graph.db` (migration 15) rather than in any committed file, for
ADR-0019's own reason: `roteiro.toml` is shared, so a merged line would be
consent by pull request. It lapses when the bundle **moves**, and when the
bundle starts carrying a class of screening finding it did not carry when the
question was answered. It does **not** lapse on an ordinary edit — a grant is
over a source, not over bytes, and re-asking on every commit would restore the
habituation the record exists to avoid. That leaves a real gap, stated rather
than hidden: a peer can change their visible prose to say something new and the
grant stands.

**A recorded answer is a standing grant, not permission to read once.** Every
later scan that may write re-applies that peer's layer without re-asking, so
their edits arrive and their **withdrawals propagate** — which is what makes
this path inherit phase 1's removal guarantee rather than quietly opt out of it.
The record itself is left alone on a refresh: `decided_at` is when a *person*
answered, and re-stamping the screening fingerprint with whatever the bundle
carries now would silently re-grant consent over content nobody was shown.

### A peer's prose is screened before it becomes node content

Reading a bundle puts a stranger's text into `meta.content`, which
`content_snippet` returns as a search hit's snippet, which backs the
model-facing `search`, `explain` and `context` tools. **A stranger's prose is
therefore stored and then returned verbatim to a language model**, inside a tool
result that model has been told to ground its answers in. Nothing in the OKF
ecosystem guards that direction — `okflint` checks conformance, and `okf-guard`
screens *source documents before generation*, which is the mirror case.

So imported text passes a deterministic screen with **three** outcomes:

- **pass** — admitted unchanged;
- **quarantine** — the concept is imported, and its text is either neutralised
  (invisible codepoints and presentation-hidden regions removed) or withheld
  entirely. The placeholder still resolves to a real node with a kind and its
  relationships, which is the payoff; what it loses is the prose;
- **block** — the concept is not imported at all.

**Block requires concealment *and* direction, together.** Instruction-shaped
text in visible prose is usually a document *about* prompt injection, and
refusing it would make writing about the attack indistinguishable from mounting
it; hidden text alone is usually an HTML comment. Text arranged so a human
reviewing the bundle cannot see it while a model reading the same file can has
one purpose, and scoping refusal to that is what keeps a refusal rare enough to
be believed.

**No model judges any of it.** Using a language model to decide whether text is
attacking a language model is circular, and it would make a workspace scan
depend on inference. The screen is codepoint classes and phrase patterns, and
`crates/rto-graph/src/screen.rs` states at length what it does *not* attempt —
no homoglyphs, no decoding of encoded payloads, English patterns only, no CSS
cascade — because a screen that claims more than it does is worse than a narrow
one that is honest.

**Screening composes with the prompt.** The result is shown at the moment the
question is asked: "this bundle contains 3 concepts with hidden control
characters — trust / acknowledge / ignore?" is answerable in a way that "trust
this bundle?" is not.

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
| 1.0 | 2026-08-29 | Accepted, and implemented in the same change (issue #663). Records the replacement of `render obsidian` by `render okf`, the provenance-to-trust-tier mapping that motivates it, that the `human:` verifier is resolved per document rather than per repository, that links resolve against the placement rather than the key and a cross-repo reference follows through to the other member's concept, that a shallow clone confirms nothing rather than confirming everything, that a scalar cannot forge a frontmatter key, that only the decision carries the decision's `status`, that the bundle is dated by the commit, and the two `_Home` capabilities — the version-pin table and the findings summary — that have no OKF home yet. |
| 1.1 | 2026-09-01 | **Roteiro reads OKF as well as writing it** (issue #706, phase 1). The v1.0 argument was that a vendor-neutral format beats one application's conventions; consuming is the half that pays for it, and until now the bundle was a standard one-way format rather than a proprietary one. Records four decisions. **Imported facts get `external-derived` / `external-authored` / `external-inferred`**, so [[crates/rto-graph/src/provenance.rs#Provenance]] is six tokens rather than three: the tier is carried because `origin_for` matches exhaustively, and a flat `External` would force one arm and therefore either downgrade a peer's human-reviewed concept or upgrade their similarity guess — and then re-emit the flattened tier outward, laundering by round-trip. Externality flattens to one level; the import layer's `src_ref` names who it came from. **A trusted concept re-emits the peer's own `generated`/`verified` block** from `meta.okf.origin`, so their verifier leaves here by name instead of being re-tiered to unverified. **`roteiro import --from okf` defaults to *acknowledge***, landing everything at `external-inferred` whatever the bundle claimed, because a hand-run command that silently adopted a stranger's confirmations is the thing the consent decision exists to prevent; `--trust` preserves their tiers and what they claimed is recorded in `meta.okf.claimed` either way. **An imported concept fills the `extref:` placeholder it corresponds to** ([[docs/adr/0009-cross-repo-workspace-links.md]]), computed forwards through the writer's own naming rule rather than by inverting a lossy `slug`, and filling nothing when the match is ambiguous. Also: a bundle is read liberally per §11 — an unrecognised `type` is imported, a malformed document is skipped with its reason reported — but a directory in which nothing parsed is refused whole. The schema gains migration 14 to widen the store's provenance `CHECK`, which is also the mechanism that makes an older build report such a store as "written by a newer Roteiro" rather than as corrupt. **Not implemented, and deliberately so:** automatic discovery during the workspace scan, and the interactive trust/acknowledge/ignore prompt with its per-source record. Both live in the workspace scan and are deferred to a later phase; this row exists so the issue's decisions are not read as delivered. |
| 1.2 | 2026-09-01 | **Discovery became automatic, consent became a prompt, and a peer's prose is now screened before it becomes node content** (issue #706, phase 2). Completes the two things v1.1 recorded as deliberately absent. **A member's bundle at its conventional `okf/` path is found during the workspace scan**, scoped to peers this graph already holds an `extref:` placeholder for — an *n*-member workspace would otherwise ask *n×(n−1)* questions on first run, most about bundles that would fill nothing. **First contact asks trust / acknowledge / ignore once and records the answer** in the consuming repo's `graph.db` (migration 15) rather than in any committed file, on [[docs/adr/0019-remote-model-tier.md]]'s own reasoning that a shared `roteiro.toml` would make it consent by pull request. This **departs from ADR-0019 on persistence**, which stores no grant at all: the questions differ in how often they recur, and a scan that ran on every `links` and every server start would produce the habituated `y` that ADR's §3 refuses to create. The grant lapses when the bundle **moves** and when it starts carrying a **class of screening finding it did not carry when the question was answered**; it does *not* lapse on an ordinary edit, because a grant is over a source rather than over bytes — a real gap, since a peer can rewrite visible prose and the grant stands. **Unprompted means `ignore`, said once, recorded never:** a server, a CI job and a pipe all lack a human, and the three answers are asymmetric — `ignore` costs a feature that did not happen, `acknowledge` costs a payload delivered to a model. Refusing to start would let any member wedge another person's server with a directory. **A peer's prose is screened** before it reaches `meta.content`, which `content_snippet` returns to the model-facing `search`/`explain`/`context` tools: pass / quarantine / block, where quarantine keeps the concept and neutralises or withholds its text, and **block requires concealment *and* direction together** — visible instruction-shaped prose is usually a document *about* prompt injection, and hidden text alone is usually an HTML comment. Deterministic throughout, because using a model to judge text aimed at a model is circular and would make a scan depend on inference. Learned from `okf-guard` (Apache-2.0) and re-aimed: that tool screens *your* sources *before* generation, and closing the consuming side is the inversion — its zero-width set is widened here to cover bidi controls and matched position-independently, and its count-driven score is replaced by a structural rule. What the screen does **not** attempt is stated in `crates/rto-graph/src/screen.rs`: no homoglyphs, no decoding of encoded payloads, English patterns only, no CSS cascade, nothing retroactive. Also fixed: `--from okf <repo>/okf` derived the peer name `okf`, so a hand-run import and automatic discovery named the same bundle differently and produced two layers and two consent records. Three defects were found in review and are recorded because each was a silent one: **a recorded answer skipped the peer entirely on later scans**, so the layer went stale and phase 1's removal propagation was quietly opted out of on the very path meant to inherit it — a grant is now standing, and refreshes without rewriting the record; **the boolean `hidden` attribute was not detected**, so `<div hidden>` concealing a directive quarantined where it should have blocked; and **the bundle probe did not require a closing frontmatter fence**, so any `index.md` opening with `---` and carrying an `okf_version:` line in its body read as a bundle, against this row's own argument that the probe should be the stricter test. |

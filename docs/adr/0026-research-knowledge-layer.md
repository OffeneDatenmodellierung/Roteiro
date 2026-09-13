---
Title: A research knowledge layer — raw sources in, an authored wiki out
Space: ARCH
Parent: ADRs

# ADR-specific metadata (unknown keys are ignored; used for indexing/search)
type: adr
adr-id: "0026"
status: For Review                  # Draft | For Review | Accepted | Rejected | Superseded
architectural-significance: HIGH    # SOFT | LOW | MEDIUM | HIGH | VERY HIGH
domain: Developer Tooling
decision-makers: ["The Roteiro Project Team"]
superseded-by:
version: "0.3"
last-modified: 2026-09-13
confluence-url:
---

# ADR-0026: A research knowledge layer — raw sources in, an authored wiki out

|  |  |
|---|---|
| **State** | For Review |
| **Architectural Significance** | HIGH |
| **Domain** | Developer Tooling |
| **Document version** | 0.3 |
| **Related** | [[docs/adr/0021-open-knowledge-format-bundle.md]] · [[docs/adr/0022-dynamic-okf-viewer.md]] · [[docs/adr/0025-document-extraction-consent.md]] · [[docs/adr/0019-remote-model-tier.md]] · [[docs/adr/0013-agent-memory-artifact-store.md]] |

## Reference

Affected code: [[crates/rto-graph/src/extract.rs#IngestConfig]],
[[crates/rto-render/src/okf.rs]], [[crates/rto-graph/src/model_choice.rs]],
[[crates/rto-render/src/mcp.rs]].

External: the "LLM wiki" pattern, A. Karpathy, gist `442a6bf5`.

## Summary

Roteiro reads code and produces a knowledge graph. This adds a second source of
knowledge — **documents somebody chose to keep** — and a second kind of concept:
research notes an LLM writes and maintains from them.

Three decisions. A `raw/` source root is added and is **excluded from the walk
extraction already performs**, reached instead by an explicit ingest path under
the consent rule [[docs/adr/0025-document-extraction-consent.md]] set — so a
document is graphed once, as its summary, rather than twice (v0.3, issue #812). A
**`knowledge/` directory becomes part of the authored layer**, maintained by a
model rather than a person, and is projected into the existing OKF bundle beside
`symbols/` and `decisions/`. And the MCP surface narrows to a **read-only**
handful of tools, because the write path this pattern suggests belongs in a
separate service and not on a graph server.

The wiki is **not** where the knowledge lives. That is the whole of the design.

## Context

The pattern this follows was published as a gist and independently converges on
the format Roteiro already emits. Its layout:

```
raw/              # curated sources, immutable
wiki/
  ├── index.md    # catalogue by category
  ├── log.md      # append-only record of ingests
```

`okf-core` declares `RESERVED_FILENAMES: ["index.md", "log.md"]` — the same two
files, with the same two meanings. That is not a coincidence worth ignoring: a
knowledge base assembled from documents converges on a catalogue and a log, and
[[docs/adr/0021-open-knowledge-format-bundle.md]] already standardises both.

**Most of the pipeline exists.** [[docs/adr/0025-document-extraction-consent.md]]
decodes office documents and PDFs and screens the text it decodes; ADR-0005 and
ADR-0016 cover images and audio. Extraction, screening, provenance and rendering
are all in place. What is missing is a *source root that is not code*, and a
*concept kind that is not derived from code*.

The gist's other two operations are also largely built. Its **lint pass** —
contradictions, stale claims, orphan pages — is `okf lint`'s orphan rule,
§5.5's `stale_after`, and `roteiro check`'s drift gate. Its **query with
citations** is `search` then `context`, where a citation is a provenance-labelled
edge rather than a hopeful footnote.

### What Roteiro has that the pattern does not

The gist's wiki has no way to distinguish a claim a person asserted from one a
model summarised. At the scale it is reported to reach — a hundred articles,
several hundred thousand words — that distinction is the difference between a
reference and a pile of plausible text. Roteiro labels every node and edge
`derived | authored | inferred` already, and that vocabulary is the reason this
belongs here rather than in a general-purpose note tool.

### The conflict that decides the design

`roteiro render okf` **deletes and rebuilds its output directory on every run**
(issue #442). The bundle is a *projection* of the graph, and its determinism is
what [[docs/adr/0021-open-knowledge-format-bundle.md]] built so that "a consumer
can diff two downloads and learn something".

The pattern requires the opposite: *"The LLM owns this layer entirely. It creates
pages, updates them when new sources arrive, maintains cross-references."*

Those cannot both be true of one directory. Point a model at the rendered bundle
and tell it to maintain the pages, and the next render eats its work — which is
#442, already learned once.

## Decision makers

The Roteiro Project Team.

## Recommended option

**Option C** — `raw/` as a source root, `knowledge/` as an authored layer in the
repository, both projected into the one existing bundle.

## Options considered + consequences

### Option A — the model maintains the rendered bundle

The pattern as published: point the model at `okf/` and let it own the wiki.

Rejected on the render contract. The output directory is emptied on every render,
so every note would survive exactly until the next `sync`. Suppressing the wipe
for some subdirectories would make the bundle no longer a function of the graph,
which is the property `okf diff` depends on and the reason the wipe exists.

### Option B — a second, separate wiki outside the graph

Keep the model's notes in their own tree, rendered by their own tool.

Rejected for what it gives up. A note could then cite code only by URL, and
nothing would check that citation again. Kept inside the graph, a note links to
`sym:rust:…#Attention` by key, `okf links` reports when that key stops existing,
and `roteiro check` treats a stale reference as drift. That is the payoff, and it
is unavailable to any standalone wiki.

### Option C — a source root and an authored layer *(recommended)*

Three layers, with ownership drawn where the determinism guarantee needs it:

| layer | owner | mutable | provenance |
|---|---|---|---|
| `raw/` | the user | immutable — read, never written | source of truth; **not scanned** — no `file:` nodes (v0.3) |
| `knowledge/` | a model, in the repository | **yes** — committed, diffed, reviewed | `authored` |
| the OKF bundle | `render` | disposable, regenerated | projection of both |

`knowledge/` joins the layer ADRs and blueprints already occupy. A model writing
`knowledge/attention.md` performs the same act as a person writing an ADR, and
inherits everything that comes with it: drift checking, `roteiro check`, review
before merge, and a git history that says who changed what.

The bundle then holds `knowledge/` beside `symbols/`, `files/`, `decisions/`,
`debt/` and `blueprints/` — one more concept kind, no format change, and one link
graph across all of them.

### Consequences

**Good.** Research notes and code become mutually navigable and mutually
drift-checked. The second brain is reviewable, because it arrives as a diff. The
bundle stays a pure function of the graph. Nothing about the OKF format changes.

**Costs.** `knowledge/` is a directory a model writes to, which is a materially
larger blast radius than the annotations it writes today, and the review burden
is real: a hundred generated pages is a hundred pages somebody nominally
approved. The graph grows by a kind whose volume is bounded by how much a user
drops into `raw/` rather than by the size of the codebase.

**A dependency, already paid.** [[docs/adr/0022-dynamic-okf-viewer.md]] v1.1
fixed a graph page that drew every concept; adding research documents to a bundle
that already holds 9,766 would have made it worse. That was solved first
deliberately.

## Implementation

1. `raw/` as a source root **excluded from the standard scan**, reached by an
   explicit ingest path under ADR-0025's per-file consent. Local by default; the
   ingest path resolves symlinks where the scan refuses them, and **must screen
   what it ingests**, because the exclusion removes the screen the walk was
   providing. See Resolved question 2.
2. `knowledge/` recognised as authored-layer input, with `render okf` projecting
   it into the bundle as its own kind.
3. The MCP surface narrowed — see below.
4. `ModelTask::Distil` for the summarisation step.

### The MCP surface narrows, and stays read-only

The current server advertises sixteen tools. Measured against a real bundle this
week, moving fourteen of them from MCP to a skill driving the CLI cost about nine
bytes in `load_skill`'s enum instead of 15,769 in an advertised `tools` array,
and lost nothing: every tool has a `--json` equivalent. **For any client with a
shell, a skill beats MCP on every axis.** MCP earns its place only where a skill
cannot reach — a hosted agent, another machine, a client with no shell — which is
the workspace serve case [[docs/adr/0008-multi-repo-workspace-serve.md]] already
addresses.

So the surface is a handful of tools — search, context, and their project
scoping — and it is **read-only**. The ingestion tool this pattern suggests is
explicitly **out of scope**: it would be the first tool on that surface capable
of unrecoverable change, and uploads need authentication, quotas and size limits
that have nothing to do with serving a graph. A separate upload service is the
right trust boundary, not merely the right module.

Project scoping needs no new design: `list_projects` and a `project` parameter
already exist, and workspace routes already confine a registry to one workspace.

### Distillation runs locally by default

`ModelTask` — `Embed`, `Draft`, `Chat`, `Review` — is the existing per-task tier
switch, and distillation is a fifth variant rather than a fifth bespoke selection
rule. Summarisation is the ideal local workload: batch, latency-insensitive, with
nobody waiting.

**Remote is a different consent question from the one
[[docs/adr/0019-remote-model-tier.md]] answered, and needs that ADR amended
before it is offered here.** Every remote use today is bounded and attended — a
person typed `--allow-remote` about one document. Distilling everything dropped
into `raw/` is *continuous and unattended* egress of material that may be
third-party, licensed, or a client's. The default for this task must be local,
with remote requiring per-run consent rather than a configuration key set once.

And before reaching for a hosted model, reach for a **smaller local one**.
`ModelTask::Embed` already runs on a 65 MiB model. "My machine cannot run the 27B"
is answered by a 3–4B distil model, not by sending the documents away.

## Resolved questions

v0.1 left three questions open. All three are answered here, and **two of the
three were answered by evidence already in the tree rather than by a new
decision** — which is the most useful thing this amendment records.

### 1. Model-authored content needs **no** fourth provenance class

**Resolved: no new class, and no change to
[[docs/adr/0001-build-roteiro-unified-codebase-knowledge-graph.md]]'s decision.**
A model-written `knowledge/` page is `authored`.

v0.1 worried that calling it `authored` "silently equates it with a reviewed
ADR". The premise is wrong at the definition. `authored`'s doc comment reads
*"Authored by a human **or agent** in an ADR, blueprint, or annotation"*
([[crates/rto-graph/src/provenance.rs#Provenance]]), and the precedent v0.1
pointed at states the test explicitly:

> **Never boosted as `authored`.** `authored` means *a human or agent
> deliberately wrote this in a reviewed file* and carries **+40 relevance** in
> `search`. Unreviewed accumulated memory riding that boost is trust-model
> contamination by construction.
> — [[docs/adr/0013-agent-memory-artifact-store.md]], *Trust, placement, privacy*

The discriminator is **reviewed**, not **human**. ADR-0013 rejected `authored`
for agent memory because memory is unreviewed and uncommitted, not because a
model produced it; the same table rejects a `Provenance` variant for it outright
on a second ground — *"memory has no source blob; it would break the
pure-function-of-source promise"*. A `knowledge/` page under Option C is
committed, diffed and reviewed before merge, and it **does** have a source blob.
It passes both tests an ADR passes. So it is not merely tolerable as `authored`;
it is the case `authored` describes.

**Why the human/model distinction is an actor question, not a class question.**
The two axes are already separate in the format. `Provenance` decides a *trust
tier*; OKF's `generated.by` / `verified.by` name the *actors*
([[docs/adr/0021-open-knowledge-format-bundle.md]] §5.3/§7). Issue #799 measures
that Roteiro collapses them — `verified.by` is identical to `generated.by` by
construction ([[crates/rto-render/src/okf.rs]] `origin_for`) — so the bundle
today asserts a human *generated* prose a model drafted. The honest state for a
`knowledge/` page is exactly what OKF provides two keys for:

```yaml
generated: [{ by: <the model that drafted it> }]
verified:  [{ by: human:<the person who reviewed and committed it> }]
```

Same tier, honest actors, no schema change, no Rust break. **#799 is therefore
the work that answers this question**, and it is a render-path change that was
already scoped without reference to this ADR.

**Why `external-*` is not a counter-example.** v1.5 of ADR-0001 did widen the
enum, by qualifying the three classes with *who produced them*. That looks like
the same move, and it is not, for a reason
[[crates/rto-graph/src/provenance.rs#Provenance]] states: externality changes
what **we can check** — *"They could re-derive it from their AST; we cannot,
because we do not have their tree."* A peer's fact has no local source blob and
no locally checkable tier, so the tier itself has to carry the qualifier or
`origin_for` is forced onto one arm and launders by round-trip. A `knowledge/`
page has a source blob in this repository, is drift-checked by `roteiro check`
like any other authored file, and its tier is locally verifiable. Nothing about
it is unavailable to us, so nothing about it needs to ride the tier.

**The second candidate for the slot is separable, and already resolved the same
way.** Issue #801 (bibliographic/citation records) is **closed**, and its own
investigation reached the same verdict independently and for a different reason:
a citation record's author/date/title are read from a document's own bytes, so it
*"stays `derived` under `(path, blob id, bytes)`, needs an `EXTRACT_VERSION`
bump, **no new provenance class**, no schema change"*. Two features wanting one
new class is the situation that should be decided once — and decided once, it is
decided *against*, twice over, on two independent grounds. They are separable
because they sit on opposite sides of the production axis: an extracted citation
is `derived` (a function of bytes), a research note is `authored` (somebody wrote
it and somebody reviewed it). Neither is a fourth *way of producing a graph
fact*, which is the test ADR-0001 sets.

**This resolution binds the layer that depends on it.** `authored` is correct
*because* `knowledge/` is reviewed. It carries +40 in `search`, so if a hundred
generated pages are merged on a nominal approval, the class becomes the lie v0.1
feared — arriving through the review process rather than through the vocabulary.
The Costs section already names that burden; it is now load-bearing rather than
regrettable. A `knowledge/` page that cannot be reviewed should not be merged,
and the fallback if that proves unworkable is **not** a fourth class but ADR-0013's
answer: a separate store, outside `nodes`/`edges`, taking no boost.

**Unblocks:** step 2 of Implementation — `knowledge/` can be wired as
authored-layer input with no schema, wire or enum change. It takes a dependency
on #799 for the actor split, which is a render-path fix, not a blocker to
building the layer.

### 2. `raw/` is **local by default**, and excluded from the standard scan

**Resolved (issue #812, amended into this ADR at v0.3): `raw/` is not one of the
three fixed options. It is a per-user choice — local by default, committed or on
shared storage if a user decides so — and it is reached by an explicit **ingest
path**, not by the walk extraction already performs.**

The default is local because that keeps the redistribution question with the
person who chose to keep the document. A project that committed every PDF
somebody read would be a redistributor of third-party material by default, and
the asymmetry is what settles it: opting *in* is a decision a user can make about
their own corpus, while opting *out* afterwards is not, because git history keeps
the bytes.

The reason the choice is *free* is the exclusion. A document reached through the
standard scan is graphed **twice** — once as the `knowledge/` summary, which
already links to its source, and once as the `raw/` file itself, whose decoded
text lands in `meta.content` and answers the same `search` query. One document,
two nodes, two hits. Excluding `raw/` from the scan removes the duplicate, and it
also removes the constraint that made this question look forced.

#### Why the mechanical argument no longer binds — and why it still matters

v0.2 of this ADR resolved Q2 as *"`raw/` is committed, and ignored is not
available"*, on a finding that is **correct and is retained**: extraction has
exactly two sources of content, and an ignored file is in neither. Committed
blobs come from the `HEAD` tree; new working-tree files come from a
**gitignore-aware** dirwalk, and both call sites say so deliberately —
[[crates/rto-graph/src/sync.rs]] `sync_worktree` and
[[crates/rto-graph/src/git.rs#Repo]] `added_since_head`: *"an ignored file is
absent from the dirwalk, so it enters only by being in the index — which takes a
deliberate `git add -f` … the file will be committed regardless."* There is no
third, non-git filesystem walk.

That finding forced "committed" **only while `raw/` was expected to be scanned**.
It said: if you want nodes from these files and the walk is your only way to get
them, the walk's rules are your rules. Take the walk away and the implication
lapses. `raw/` produces **no `file:` nodes by design**, so whether git can see it
is no longer a question about whether the feature works — and committed,
untracked and ignored become equally functional, which is what makes them a free
choice rather than a forced one.

**The finding is kept because it is the reason the exclusion must be deliberate
rather than incidental.** A `raw/` that is merely *ignored* looks, from the
outside, exactly like a `raw/` that is *excluded*: no nodes either way. The two
are not the same, and the difference only shows up later. An ignored `raw/` is
one `git add -f` — or one user who commits their corpus, as this decision
explicitly permits — away from being scanned after all, at which point the
duplicate nodes appear, the screen behaves differently, and nobody changed a
setting. So the exclusion must be a **rule in the ingest configuration**, holding
for a committed `raw/` exactly as for an ignored one, and not a side effect of
where the bytes happen to live.

**Reproducibility-from-the-repository-alone is therefore not claimed, and this
ADR says so rather than implying it.** Under the default, a clone does not carry
the sources its summaries were derived from. The property that *is* claimed
remains true and is unaffected: `render okf` produces *"a shareable snapshot,
whose whole value is being reproducible from a commit"*
([[crates/roteiro/src/main.rs]]), and it stays reproducible from a commit because
the bundle is a projection of `knowledge/` and the code graph, both of which are
committed. What a clone cannot do is re-derive a summary from a source it does
not have — which is a different property, was never claimed, and must now
**degrade honestly** rather than silently.

#### Requirement: an absent source must be detectable, never silent

Per #812, **a manifest is committed in every mode**, whatever the user chose for
the bytes: source identity, content hash, origin URL, access date, licence. The
failure mode this exists to prevent is *a summary that cites a source the
repository can no longer see and cannot tell you it has lost it* — the same class
as the citation-honesty work in #801, where a reference that looks supported but
resolves to nothing is worse than an admitted gap.

Two consequences follow, and they are requirements rather than observations:

- **Content hashes are load-bearing.** A summary is tied to an exact source
  version by hash, so a source that changed underneath it is detectable. Reuse
  what the project already hashes — the `(path, blob id, bytes)` derivation basis
  — rather than inventing a second scheme.
- **A `knowledge/` page whose source is absent must say so.** `roteiro check`
  already treats a stale authored reference as drift, which is the mechanism;
  what it needs is a manifest entry to resolve against. An absent source is a
  *known* absence, not an error — the distinction ADR-0021 draws between
  unverified and misattributed applies here too.

#### Two opposed symlink rules, and why the opposition is intended

**This is written down because a future reader will otherwise read it as drift
and "fix" it.** The standard scan refuses symlinks in at least three places, each
with its reasoning recorded at the refusal:

- `collect_markdown` ([[crates/roteiro/src/main.rs]]) — *"a symlinked directory
  or file could otherwise pull in out-of-repo content behind a
  repo-relative-looking key."*
- `markdown_files` — which checks **every path component**, not just the leaf,
  and calls itself *"a scope guarantee, not a security boundary"*: what it buys
  is that *"an ordinary symlink sitting in `docs/` does not get quietly written
  through"*, not atomicity against a rename.
- the OKF bundle walk — *"A bundle is somebody **else's** directory"*, so a link
  to an ancestor spins the walk forever and one pointing outside pulls in files
  that are not part of it.

**The ingest path deliberately does the opposite: it resolves symlinks.** Read
against those three, that looks like an inconsistency. It is not, and the
distinguishing property is the same one all three refusals are actually about:
whether a path is claiming to be repository content.

The three refusals protect a **repo-relative key**. Each is guarding a walk whose
output is a node key of the form `file:<path>`, where the path asserts *this
content is at this location in this repository*. A symlink defeats that assertion
silently: the key says `docs/x.md`, the bytes came from `/etc/`, and nothing in
the graph records the difference. The refusal is not "out-of-repo content is
dangerous"; it is "out-of-repo content **behind a repo-relative-looking key** is a
false claim".

The ingest path makes no such claim. Out-of-repo content is precisely what `raw/`
is for — #812 settles that a user may keep their corpus on shared storage — and
an ingested document is recorded as a manifest entry with its own identity,
origin and hash, not as a `file:` node at a repo-relative path. There is no key
for a symlink to falsify. Following the link is the feature.

So the rule is: **a walk that mints `file:` keys refuses symlinks; the ingest
path, which mints none, resolves them.** Anyone tempted to unify the two should
change this paragraph first, because unifying them in either direction breaks
something real — resolving them in the scan reintroduces the false-key defect
three separate call sites were written to prevent, and refusing them in ingest
deletes the shared-storage case this decision exists to permit.

Two things the ingest path inherits from the refusals rather than discarding
with them, because they are about walk safety rather than key honesty: a
**cycle bound**, since a resolved link to an ancestor still spins forever, and a
**recorded resolution**, so the manifest names the path that was actually read
rather than the link that was followed.

#### ⚠️ Requirement: the ingest path must screen, because the exclusion removes the screen that was doing it

**This is the consequence of the exclusion that is easiest to miss, and it is a
requirement on the first shippable slice (#813), not a later hardening step.**

The content screen is wired into the **git-walk** path, at
[[crates/rto-graph/src/extract.rs]] `decoded_content`, and it is an enforcement
point rather than a label: *"`admit` is the whole enforcement half: `None` means
none of it may be stored, and returning the text anyway would make the screen a
label."* Every branch that decodes text out of a binary routes through it — PDF
text and OCR text both.

**Excluding `raw/` from that walk means the screen never runs on it.** ADR-0025
closed the gap where decoded text reached `meta.content` unscreened; this
decision reopens it for exactly the population ADR-0025 was written about —
documents somebody else wrote — unless the ingest path screens explicitly.
Screening is inherited today and must become deliberate.

**And the ingest path's obligation is *larger* than what it replaces, not equal
to it.** ADR-0025 deliberately exempted prose from the screen (Option D), and
that exemption was **measured**: over *"this repository's own 327 prose files"*,
eight lost content and two lost it entirely. That measurement is over
**first-party** prose. A third-party markdown file dropped into `raw/` is the
opposite population, and the carve-out's evidence says nothing about it. So:

- a PDF in `raw/` **loses** a screen it gets today;
- a markdown file in `raw/` **never had one**, and the reason it never had one
  does not transfer to foreign content.

Neither gap is closed by wiring the existing screener up and moving on. #813
states the harder half already: the screen's concealment detection is
HTML-shaped, and a PDF conceals by a different vocabulary — white-on-white text,
text outside the crop box, render mode 3, a text layer disagreeing with the
glyphs. Since a `block` verdict requires concealment **and** direction together,
a format in which concealment cannot be detected can only ever reach
`quarantine`, never `block` — a silent weakening of the rule by document format.

This ADR therefore adopts #813's acceptable outcomes as a condition on the ingest
step: PDF-native concealment detection, or the existing screen **plus an explicit
declaration naming what was not checked**, visible to the user of the ingest
path. A screen that runs, passes, and implies a check it did not perform is not
acceptable, and this repository has paid for that shape twice already.

#### Out-of-line storage: still deferred, and #812 sharpens the same question

"Shared storage" is the out-of-line option, and it is now **decided in principle
and blocked in practice** — which is a change of status from v0.2's plain
deferral, not a change of finding. #812 names the same blocker this ADR reached
independently: *"extraction works only for files already inside a repository,
with no path to ingest a source from outside the tree. If that holds, an
out-of-tree `raw/` is blocked until that limitation is lifted, and the ADR must
say so rather than implying it works."*

It holds. The two sources named at the top of this section are the whole of it.

> **What unblocks it.** One narrower question: *can content be resolved and
> hashed from a manifest entry whose bytes are not in the git object database?*
> The shape exists in this repository twice — `docs/VENDORED_DEPENDENCIES.md` and
> digest-pinned `roteiro security prefetch` images, *"re-obtainable from a pinned
> digest"*. Answering it delivers the shared-storage mode **and** the manifest
> the absent-source requirement above needs, which is why the two should be
> designed together rather than in sequence.

**ADR-0025's per-file consent needs re-checking, not assuming.** It was written
for files in the tree, and #812 flags that its meaning over a path the repository
does not own is an open matter. This ADR does not settle it: consent remains the
gate on ingest, and whether the *record* of that consent can be keyed the same
way when the file is out of tree is part of the manifest question above.

**Contradiction with the framing, recorded because it matters.** This resolution
was first framed against a live corpus at `pixie79/knowledge` said to hold
hundreds of gitignored PDFs across several GB. Inspected on 2026-09-13, that
repository is a single commit `4c25642` ("doc: initial") containing `README.md`,
`AGENTS.md` and `.agents/` — **no `raw/` directory, no `.gitignore`, and no
PDFs**. The corpus is intended, not assembled. That is the right time to take
this decision, since the default cannot be changed retroactively once bytes are
in a history; but it means the size and licence pressure is projected rather than
measured, and a real corpus is the likely trigger for the deferred question
above.

**Unblocks:** step 1 of Implementation, restated — `raw/` as an **excluded**
root reached by an ingest path, which needs no store or fetch machinery for the
local default and is explicitly blocked for shared storage until the manifest
question is answered.


### 3. `log.md` is derived, and it already exists

**Resolved: `log.md` stays in the bundle and is derived from git history. The
append-only reading is declined.**

v0.1 posed this as a contradiction to resolve. It is already resolved in code,
in the direction v0.1 named as the acceptable one:

- `render_log` is a pure function of its arguments —
  *"Render `log.md` (§9): dated groups, newest first"*
  ([[crates/rto-render/src/okf.rs]]).
- `assemble(concepts, title, log: &[LogDay])` writes `/log.md` on every render,
  beside the indexes.
- **Both callers pass `&[]`** ([[crates/roteiro/src/main.rs]], the project and
  workspace render paths), so every bundle Roteiro has ever published ships an
  empty `# Update Log`.

So there is no append-only file to reconcile and never was: the slot is wired,
typed and derived-by-construction, and simply unfed. The open question was
really *what feeds `days`*, and determinism answers it — the log must be a
function of the graph at the rendered commit, exactly as every other file in the
bundle is.

**The source is git history, and it is already trusted for a load-bearing
purpose.** ADR-0021 resolves a concept's `verified.at` to the commit that last
changed that document's own path, *"rather than the render's"* — via
[[crates/rto-graph/src/git.rs#Repo]] `last_authors` — precisely so the bundle
carries no `SystemTime::now()`. The same per-concept `(date, author)` pair,
grouped by date and rendered newest-first, is `log.md`. It costs no new state,
no new command and no new file to maintain; it is byte-reproducible for the same
reason the trust tiers are; and it makes the log say something true — *these
concepts changed on this date* — rather than something a process promised to
append to.

**What is declined, explicitly.** An ingest log that records "this PDF was read
at this time" is *not* derivable from a commit, because ingest time is not a
property of the tree. Two renders of one commit would disagree, which is the
`SystemTime::now()` defect ADR-0021 already refused. If an ingest record is
wanted, it is a committed artifact in `raw/` or `knowledge/` — the ADR-0025
consent record is the natural carrier, since it already records a per-file
decision — and `log.md` reflects *its* commits like any other authored file. The
reserved filename does not oblige Roteiro to keep an append-only file inside a
directory it deletes on every run; §9 reserves a name and a shape, and the shape
is dated groups.

**Unblocks:** nothing in this ADR, which is the point — the bundle needs no
change to hold `knowledge/`. It unblocks a small, independent improvement to
every bundle Roteiro renders, which is now a wiring task against an existing
pure function rather than a design question.


## Advice Received

The direction came from the project owner, who identified the research-base use
case and the narrower MCP surface as one idea rather than two: if the graph is
reached through a skill, what remains for MCP is what a skill cannot do, and the
ingest half of that belongs in a service of its own.

## Document version history

| Version | Date | Change |
|---|---|---|
| 0.1 | 2026-09-10 | Initial draft. Records a second knowledge source — documents rather than code — and the layer split that lets a model maintain a wiki without owning the bundle it renders into. Names the conflict that decides it: the OKF bundle is a projection, emptied on every render, while the pattern being followed requires the model to own the wiki outright. Resolves it by making the model maintain the **authored** layer instead, which is committed, diffed and drift-checked. Also settles the MCP question the same week's magpie work raised: the surface is read-only and narrow, because a skill driving the CLI beat fourteen advertised tools by 15,769 bytes at no loss of capability, and an ingest tool belongs in a separate upload service. Leaves three questions open rather than assuming them: a provenance class for model-authored content, whether `raw/` is committed, and what a reserved append-only `log.md` means inside a regenerated bundle. |
| 0.2 | 2026-09-13 | **Answers all three open questions, two of them from evidence already in the tree.** (1) Model-authored content needs **no fourth provenance class**: v0.1's premise that `authored` means *human* is contradicted at the definition — `provenance.rs` says *"a human **or agent**"*, and ADR-0013, the precedent v0.1 pointed at, sets the test as *"deliberately wrote this in a **reviewed** file"*. ADR-0013 refused `authored` for agent memory because memory is unreviewed and has no source blob; a `knowledge/` page is reviewed and has one, so it passes both tests an ADR passes. The human/model distinction is an **actor** question, which OKF already separates from the tier via `generated.by` / `verified.by` — issue #799 is the render-path fix that makes it honest, and no schema, wire or Rust change is needed. The competing candidate for the same slot, issue #801's citation records, is **closed** having reached the same verdict independently: an extracted citation stays `derived`, *"no new provenance class"*. So ADR-0001 is unchanged in substance and amended only to record a **fourth** consecutive decline. The resolution binds its own precondition: `authored` carries +40 in `search`, so it is correct only while review is genuine. (2) `raw/` is **committed**, and "gitignored" was never available — extraction reads only `HEAD` blobs and a **gitignore-aware** overlay (`sync.rs` / `git.rs`: *"an ignored file is absent from the dirwalk, so it enters only by being in the index — which takes a deliberate `git add -f`"*), so an ignored `raw/` yields no nodes and the option disables the feature rather than storing it differently. Reproducibility-from-a-commit **is** claimed (`main.rs`: *"a shareable snapshot, whose whole value is being reproducible from a commit"*), which an out-of-tree corpus breaks. Out-of-line storage is **deferred, not rejected**, behind one narrower question named here: can `sync` resolve a blob from a committed manifest entry whose bytes are not in the object database, on the `prefetch`/`VENDORED_DEPENDENCIES.md` model? Copyright is a gate on ADR-0025's per-file consent, not a storage question, because ignoring a file answers nothing. (3) `log.md` is **derived and already implemented** — `render_log` is a pure function, `assemble` writes `/log.md` on every render, and both callers pass an empty slice, so every published bundle carries an empty log. The open question was what feeds it: the per-concept commit dates ADR-0021 already resolves for `verified.at` via `last_authors`, grouped by day. An ingest-time log is declined outright, being the `SystemTime::now()` non-determinism ADR-0021 refused. Also records that the live corpus this was framed against holds no `raw/` and no PDFs — the decision is being taken before several GB enter a history that cannot forget them. Status Draft → For Review. |
| 0.3 | 2026-09-13 | **Q2 re-resolved on a maintainer decision** (issues #812, #813). `raw/` is **excluded from the standard scan** and reached by an explicit **ingest path**; without the exclusion a document is graphed twice — once as its `knowledge/` summary, which already links to the source, and once as the `raw/` file whose decoded text answers the same `search`. With no `file:` nodes coming from `raw/`, committed / untracked / ignored stops being forced and becomes a per-user choice with **local as the default** — the redistribution question stays with the person who kept the document, and the asymmetry that settles it is that opting in is reversible where opting out is not, because git history keeps the bytes. v0.2's mechanical finding is **retained and re-purposed rather than withdrawn**: extraction really does have only two sources and an ignored file is in neither, which forced "committed" *only while `raw/` was expected to be scanned*, and is now the reason the exclusion must be a **rule in the ingest configuration** rather than a side effect of where the bytes live — an ignored `raw/` and an excluded `raw/` are indistinguishable from outside, and one `git add -f` separates them. Reproducibility-from-the-repository-alone is explicitly **not** claimed; reproducibility-from-a-commit is unaffected, because the bundle projects `knowledge/` and the code graph, both committed. Adds three requirements the exclusion creates. **(a)** A committed manifest in every mode — identity, content hash, origin, access date, licence — so an absent source is *detectable rather than silent*, with hashes load-bearing and reusing the `(path, blob id, bytes)` basis. **(b)** Both symlink rules recorded together with the reason they oppose, because three refusals and one resolution otherwise read as drift: the scan's refusals (`collect_markdown`, `markdown_files` — *"a scope guarantee, not a security boundary"*, checking every path component — and the bundle walk) all guard a **repo-relative `file:` key** that a symlink falsifies silently, whereas the ingest path mints no such key, so there is nothing to falsify and following the link is the feature; a cycle bound and a recorded resolution are inherited regardless. **(c)** The ingest path **must screen explicitly**, because the screen is wired into the git-walk path at `extract.rs::decoded_content` (*"`admit` is the whole enforcement half"*) and excluding `raw/` removes it. The obligation is *larger* than what it replaces: a PDF in `raw/` loses a screen it gets today, and a markdown file never had one — ADR-0025's prose carve-out was measured over *"this repository's own 327 prose files"*, first-party content whose evidence does not transfer to a foreign corpus. #813's harder half is adopted as a condition: HTML-shaped concealment detection cannot see a PDF's vocabulary, so a `block` verdict — which needs concealment **and** direction — is unreachable in PDFs, and the outcome must be PDF-native detection or an explicit declaration of what was not checked. Out-of-line storage moves from plain deferral to **decided in principle, blocked in practice**, on the same narrower question #812 names independently, now paired with the manifest work. ADR-0025's per-file consent over an out-of-tree path is flagged as needing re-checking rather than assumed. Summary, Option C's layer table and Implementation step 1 corrected, all three having still described `raw/` as joining the roots extraction walks. |

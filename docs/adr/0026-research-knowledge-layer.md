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
version: "0.2"
last-modified: 2026-09-13
confluence-url:
---

# ADR-0026: A research knowledge layer — raw sources in, an authored wiki out

|  |  |
|---|---|
| **State** | For Review |
| **Architectural Significance** | HIGH |
| **Domain** | Developer Tooling |
| **Document version** | 0.2 |
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

Three decisions. A `raw/` source root joins the roots extraction already walks,
under the consent rule [[docs/adr/0025-document-extraction-consent.md]] set. A
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
| `raw/` | the user | immutable — read, never written | source of truth |
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

1. `raw/` as an extraction source root, under ADR-0025's per-file consent.
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

### 2. `raw/` is **committed**, and "ignored" is not available

**Resolved: `raw/` is a committed source root. The `.gitignore` option is not a
storage policy — it disables the feature.**

This was expected to be a judgement about size and copyright. It is settled
mechanically before either is reached. Extraction has exactly two sources of
content, and an ignored file is in neither:

- committed blobs from the `HEAD` tree, and
- an overlay of working-tree changes, whose new-file half is
  [[crates/rto-graph/src/git.rs#Repo]] `untracked_files` — *"Respects
  `.gitignore` / `.git/info/exclude` / global excludes"*.

Both call sites state the union deliberately: *"an ignored file is absent from
the dirwalk, so it enters only by being in the index — which takes a deliberate
`git add -f` \[…\] the file will be committed regardless"*
([[crates/rto-graph/src/sync.rs]] `sync_worktree`; the same note at `git.rs`
`added_since_head`). There is no third, non-git filesystem walk. **A gitignored
`raw/` produces no nodes at all**, so a bundle rendered from it would be empty of
the very documents the layer exists to hold. The only way an ignored PDF reaches
the graph is `git add -f`, which commits it — i.e. option 1 with extra steps.

The reproducibility property v0.1 asked about **is** claimed, and this is why it
matters: `render okf` is documented as producing *"a shareable snapshot, whose
whole value is being reproducible from a commit (#442's manifest half depends on
exactly that)"* ([[crates/roteiro/src/main.rs]], the source-selection note).
Reproducible **from a commit** — so every input must be reachable from that
commit. An out-of-tree `raw/` breaks that directly: two machines at the same SHA
would render different bundles, and `okf diff` would report a difference that no
commit explains.

**Stored out of line is a real third option, and it is deferred rather than
rejected.** The concrete shape is the one this repository already uses twice: a
committed manifest of hashes and locators with the bytes elsewhere —
`docs/VENDORED_DEPENDENCIES.md` for native components, and digest-pinned images
for `roteiro security prefetch`, *"re-obtainable from a pinned digest"*. That
shape restores reproducibility-from-a-commit in principle. It does **not** work
for `raw/` today, for a reason that is about this codebase and not about the
idea: the manifest would name bytes that extraction cannot open, because
extraction resolves content through git blobs, and a hash in a markdown table is
not a blob. Making it work means a content store that `sync` can read from —
which is a design, not a paragraph.

> **Deferred sub-question, and what unblocks it.** Out-of-line `raw/` is blocked
> on one narrower question: *can `sync` resolve a blob from a manifest entry
> whose bytes are not in the git object database?* Answer that — with a
> fetch-and-verify store keyed by the committed hash, on the `prefetch` model —
> and out-of-line becomes available as a size and licensing escape hatch without
> reopening this ADR. Until then, committed is the only option that produces a
> graph.

**Copyright is therefore a gate on the source root, not a storage question.** It
does not get to be answered by ignoring the files, because ignoring them answers
nothing. It is answered where
[[docs/adr/0025-document-extraction-consent.md]] already puts it: extraction per
file is a decision a person makes. Committing a third party's PDF is a
redistribution act the person performing it must intend — so the consent record
ADR-0025 requires is also the place a licence decision is recorded, and a `raw/`
document with no consent record is not extracted and should not be committed.
This ADR does not grant blanket permission to commit a corpus; it says that
whatever *is* committed is what the graph can see.

**Contradiction with the brief, recorded because it matters.** This resolution
was framed against a live corpus at `pixie79/knowledge` said to hold hundreds of
gitignored PDFs across several GB. Inspected on 2026-09-13, that repository
contains `README.md`, `AGENTS.md` and `.agents/` at a single commit `4c25642`
("doc: initial"), with **no `raw/` directory, no `.gitignore`, and no PDFs**. The
corpus is intended, not assembled. That strengthens rather than weakens the
resolution — the decision is being taken *before* several GB are committed to a
history that cannot forget them, which is the only time it is cheap — but it
means the size and licence pressure is projected rather than measured, and a
measured corpus may well be what triggers the deferred sub-question above.

**Unblocks:** step 1 of Implementation — `raw/` as an extraction source root,
with no store or fetch machinery, because committed content is what extraction
already reads.

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

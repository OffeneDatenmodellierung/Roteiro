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
version: "0.13"
last-modified: 2026-09-13
confluence-url:
---

# ADR-0026: A research knowledge layer — raw sources in, an authored wiki out

|  |  |
|---|---|
| **State** | For Review |
| **Architectural Significance** | HIGH |
| **Domain** | Developer Tooling |
| **Document version** | 0.13 |
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

Three decisions, none of them yet built — this is a `For Review` ADR and the
whole of what follows describes a target state. A `raw/` source root **would be
added and excluded** from the walk extraction already performs, reached instead
by an explicit ingest path under the consent rule
[[docs/adr/0025-document-extraction-consent.md]] set — so that a document **would
be** graphed once, as its summary, rather than twice (v0.3, issue #812). Today it
is graphed twice, because the scan has no `raw/` exclusion to apply. A **`knowledge/`
directory would become part of the authored layer**, maintained by a model
rather than a person, and be projected into the existing OKF bundle beside
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
**names**. That is not a coincidence worth ignoring: a knowledge base assembled
from documents converges on a catalogue and a log, and
[[docs/adr/0021-open-knowledge-format-bundle.md]] already standardises both.

The **meanings** are not the same, and Resolved question 3 below settles the
difference rather than inheriting it. The layout above is the reference pattern's,
in which `log.md` is an append-only record of ingests; OKF §9 reserves the name
for dated groups, and this ADR declines the append-only reading. Read the block
above as *what the gist does*, not as what Roteiro adopts.

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
pure-function-of-source promise"*. A `knowledge/` page **as Option C defines it**
is committed, diffed and reviewed before merge, and has a source blob. It would
pass both tests an ADR passes. So it is not merely tolerable as `authored`; it is
the case `authored` describes — and that is a statement about the layer this ADR
proposes, which does not exist yet, rather than about anything in the tree
today.

**Why the human/model distinction is an actor question, not a class question.**
The two axes are already separate in the format. `Provenance` decides a *trust
tier*; OKF's `generated.by` / `verified.by` name the *actors*
([[docs/adr/0021-open-knowledge-format-bundle.md]] §5.3/§7). Issue #799 measures
that Roteiro collapses them — `verified.by` is identical to `generated.by` by
construction ([[crates/rto-render/src/okf.rs]] `origin_for`) — so the bundle
today asserts a human *generated* prose a model drafted. The honest state for a
`knowledge/` page is exactly what OKF provides two keys for:

```yaml
generated:
  by: <producer>/<version>        # the model that drafted it
  at: <commit time>
verified:
  - by: human:<id>                # the person who reviewed and committed it
    at: <commit time>
```

Note the shapes, which are not symmetric: `Frontmatter::render`
([[crates/rto-render/src/okf.rs]]) emits `generated` as a **mapping** and
`verified` as a **sequence**, and both carry `at` as well as `by`.

**The actor token for a model is #799's to settle, and this ADR does not
pre-empt it.** The grammar admits exactly three forms — `human:<id>`,
`<producer>/<version>`, and `process:<id>` ([[crates/rto-render/src/okf.rs#Actor]]) —
and a co-author trailer supplies a display name with no version, so the mapping
onto `<producer>/<version>` *"needs deciding rather than assuming"*, in #799's
own words, with `lint_actor_convention` (L6) the rule it has to satisfy. What
this ADR fixes is only which **key** the model belongs under.

Same tier, honest actors, **no OKF format change and no `Provenance` change** —
but not, as v0.2–v0.8 of this ADR implied, no Rust change at all.
[[crates/rto-render/src/okf.rs#Origin]] holds a **single** `Actor`, and
`Frontmatter::render` writes that one actor into both `generated` and `verified`,
which is the mechanical reason #799 measures them as identical. Splitting them
means `Origin` (or `Frontmatter`) must carry two, which is a breaking change to
`rto-render`'s Rust surface. That cost is real and is **already priced**: under
`AGENTS.md`'s `rto-*` carve-out, recorded at
[[docs/adr/0001-build-roteiro-unified-codebase-knowledge-graph.md]] v1.5, such a
change ships as a **minor** with no `!`. So the claim this ADR makes is the
narrower one — the *wire* and the provenance vocabulary are untouched — and
**#799 is the work that answers this question**, already scoped without reference
to this ADR.

**Why `external-*` is not a counter-example.** v1.5 of ADR-0001 did widen the
enum, by qualifying the three classes with *who produced them*. That looks like
the same move, and it is not, for a reason
[[crates/rto-graph/src/provenance.rs#Provenance]] states: externality changes
what **we can check** — *"They could re-derive it from their AST; we cannot,
because we do not have their tree."* A peer's fact has no local source blob and
no locally checkable tier, so the tier itself has to carry the qualifier or
`origin_for` is forced onto one arm and launders by round-trip. A `knowledge/`
page would have a source blob in this repository and a locally verifiable tier,
and — once step 2 of Implementation lands — **will be** drift-checked by
`roteiro check` like any other authored file. (It is not today:
[[crates/rto-spec/src/layer.rs]] classifies ADRs, blueprints, annotations and
site pages, and has no `knowledge` arm. That is the work this resolution
unblocks, not a property it can assume.) Nothing about such a page would be
unavailable to us, so nothing about it needs to ride the tier.

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
available"*, and stated the mechanics as *"extraction has exactly two sources of
content, and an ignored file is in neither"*. **That was wrong in two ways, and
both corrections make the case for the exclusion stronger rather than weaker**
(review of PR #816; measured 2026-09-13).

**There are more than two sources, and the count was never the point.**
[[crates/rto-graph/src/git.rs#GraphSource]] has three variants — `Committed`,
`Worktree`, `Index` — and `rto-graph` exposes four entry points:
[[crates/rto-graph/src/sync.rs#sync]] (the `HEAD` tree),
[[crates/rto-graph/src/sync.rs#sync_worktree]] (`HEAD` plus a disk overlay),
`sync_index` (the staged blobs, *"exactly what a commit would record"*) and
`sync_tree` (an arbitrary revision). What is actually true, and is what the
argument needed, is that **every input path comes from git's own enumeration of
one tree**: a commit's tree (`HEAD`, or any revision `sync_tree` is given), the
index, or a **workdir-rooted, gitignore-aware dirwalk** ([[crates/rto-graph/src/git.rs#Repo]] `untracked_files`, which also
skips nested repositories, symlinks and non-regular files). There *is* a
filesystem walk — saying otherwise, as v0.4 did, was the same overreach in a new
spelling — but it is rooted at the repository workdir and classified against
git's own view, so it cannot *enumerate* a path outside the tree.

**Reachability of out-of-tree bytes splits on the source, and the split is what
the argument actually needs.** A tracked symlink is a path git can name whose
target is outside, so enumeration is not the whole story:

| source | a tracked symlink resolves to | out-of-tree bytes? |
|---|---|---|
| `Committed` / `sync_tree` | the **git blob**, whose content is the target *path string* | **no** |
| `Worktree` overlay | `std::fs::read` **follows** the link | **yes** |

Measured both ways. Committing a symlink to a file outside the repository and
running a committed sync stores no node for its content at all; doing the same
through the worktree overlay stores the target's bytes under the tracked key (see
the symlink section below, where that is recorded as a live defect).

**This is why the bundle argument survives.** `render okf` and `export`
deliberately read the **committed** source — *"a shareable snapshot, whose whole
value is being reproducible from a commit"*
([[crates/roteiro/src/main.rs]]) — and on that path a symlink is a blob holding a
path string. So out-of-tree bytes cannot enter a **published bundle**, which is
the property #812's blocker rests on. What they can enter is a local worktree
preview and its `search` results, which is a smaller and different claim than
v0.4–v0.8 made, and is stated here rather than glossed.

**`.gitignore` does not un-graph a file git already tracks — it is inert over
tracked paths.** v0.2's *"an ignored file is in neither"* quoted a code comment
scoped to the working-tree overlay and generalised it to the whole system. The
comment is right about what it describes; the generalisation is not. Measured on
a scratch repository:

| case | `git check-ignore` | nodes? |
|---|---|---|
| `raw/` ignored from the start, never tracked | ignored | **no** |
| `raw/paper.md` committed, `.gitignore` added afterwards | **reports it as *not* ignored** | **yes** |
| `raw/paper.md` ignored, then `git add -f` (staged, uncommitted) | ignored | **yes** |

The middle row is the one v0.2 missed, and it needs no `git add -f`: adding an
ignore rule over a tracked path changes nothing at all, and git does not even
consider the file ignored. So the retained half of the finding is narrower than
v0.2 claimed — **an ignored file that git is not already tracking** yields no
nodes — and the third row confirms the code comment exactly as written.

That finding forced "committed" **only while `raw/` was expected to be scanned**.
It said: if you want nodes from these files and the walk is your only way to get
them, the walk's rules are your rules. Take the walk away and the implication
lapses. Under this decision `raw/` **would produce no `file:` nodes by design**
(it produces them today: `IngestConfig` has no `raw` exclusion, so any eligible
committed or worktree file under `raw/` is extracted like any other). So whether
git can see it
is no longer a question about whether the feature works — and committed,
untracked and ignored become equally functional, which is what makes them a free
choice rather than a forced one.

**The finding is kept because it is the reason the exclusion must be deliberate
rather than incidental.** A `raw/` that is merely *ignored* looks, from the
outside, exactly like a `raw/` that is *excluded*: no nodes either way. The two
are not the same, and the difference only shows up later. An ignored `raw/` is
one `git add -f` away from being scanned after all — and, worse, a `raw/` that
was **committed once and ignored afterwards** is being scanned *already*, because
the ignore rule is inert over a tracked path. That is not a hypothetical
sequence: it is precisely the journey this decision permits (a user commits their
corpus) followed by the regret it anticipates (they think better of it and add an
ignore rule). At that point the duplicate nodes are present, the screen is
behaving differently from the ingest path, and nobody changed a setting — the
user believes they opted out and the graph disagrees. So the exclusion must be a **rule in the ingest configuration**, holding
for a committed `raw/` exactly as for an ignored one, and not a side effect of
where the bytes happen to live.

**Reproducibility-from-the-repository-alone is therefore not claimed, and this
ADR says so rather than implying it.** Under the default, a clone does not carry
the sources its summaries were derived from. The property that *is* claimed
remains true and is unaffected: `render okf` produces *"a shareable snapshot,
whose whole value is being reproducible from a commit"*
([[crates/roteiro/src/main.rs]]), and excluding `raw/` does not weaken it: the
bundle would project `knowledge/` alongside the code graph, both committed.
(*Would*: [[crates/rto-spec/src/layer.rs]] has no `knowledge` arm, so nothing
projects it today.)

**One caveat, which predates this decision and is recorded rather than
introduced.** "Reproducible from a commit" already holds only for the derived and
authored layers. `build_graph` rebuilds those from the tree and then calls
`store.reapply_imports()` for **persisted** import layers — Graphify, lat.md, a
peer's OKF bundle — which live in the store rather than in the commit
([[crates/roteiro/src/main.rs]]). Two clones at the same commit with different
import histories therefore render different bundles today. That is a pre-existing
property of the import layer, not a consequence of excluding `raw/`; it is noted
here because this ADR leans on the reproducibility claim and should not overstate
it. What a clone cannot do is re-derive a summary from a source it does
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
  what the project already computes rather than inventing a scheme — but **the
  right thing to reuse is the content hash, not the extraction key**, and v0.3
  conflated them by naming the `(path, blob id, bytes)` tuple. That tuple is the
  *derivation basis* of a derived fact and folds in a path and an extractor
  version; a manifest wants only *these bytes, this version of this work*. The
  function to reuse is [[crates/rto-graph/src/git.rs#Repo]] `blob_oid`, which
  computes a git blob id **from bytes alone** via `gix::objs::compute_hash` — no
  object database required, so it works for bytes that are not in the repository
  at all. That is what makes it the right primitive for an out-of-tree corpus.
- **A `knowledge/` page whose source is absent must say so.** `roteiro check`
  already treats a stale reference as drift for the authored kinds it recognises,
  which is the mechanism to extend; what it needs is a `knowledge` arm (it has
  none today) and a manifest entry to resolve against. **The two states are
  distinct and must not be collapsed**, because the local default makes one of
  them ordinary:
  - *bytes absent, manifest entry present* — the expected state of any clone that
    did not fetch the corpus. **Not drift, and must not fail `check`**; the page
    reports its source as unavailable and stays honest.
  - *no manifest entry at all, or a hash that no longer matches* — a citation to
    something the repository cannot name, or can no longer tie to a version.
    **That is drift**, and it is the failure this requirement exists to catch.

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

The three refusals protect a **tree-relative claim**, in two variants. Two of them
guard a walk whose output is a node key carrying a path — `lat:<path>` for
`collect_markdown`'s importer, `okf:<peer>/<path>` for the bundle walk (and not
`file:<path>`, which is the derived extractor's family) — where the path asserts
*this content is at this location in this tree*.

> **The derived extractor is a fourth case, and it does *not* refuse symlinks.**
> This ADR previously implied the scan refuses them everywhere. It does not.
> `sync_worktree` reads each tracked path with plain `std::fs::read`
> ([[crates/rto-graph/src/sync.rs#sync_worktree]]), which **follows** a link, so
> replacing a tracked regular file with a symlink pointing outside the repository
> makes the extractor store the target's bytes under the tracked path's key.
> Measured on a scratch repository: after `rm docs/note.md && ln -s
> ../outside/secret.md docs/note.md`, a worktree sync stores the out-of-repo
> content as `file:docs/note.md`. That is exactly the false-key defect the other
> three walks were written to prevent, in the one family this ADR named as their
> example. It is **pre-existing behaviour in `main`, not something this decision
> introduces**, and it is reported rather than fixed here because this ADR
> changes no code — but it must be recorded, because an ADR arguing from "the
> scan refuses symlinks" would otherwise rest on a guarantee the scan does not
> give. It also means the ingest path resolving links is *less* anomalous than
> the three refusals suggest; what still distinguishes it is that it mints no
> tree-relative key, not that it is alone in following a link. A symlink defeats that assertion
silently: the key says `docs/x.md`, the bytes came from `/etc/`, and nothing in
the graph records the difference. The third, `markdown_files`, mints no key at
all; it bounds where `docs fmt` may **write**, which is the same claim pointed
the other way — *this path is inside the tree you named*. Either way the refusal
is not "out-of-repo content is dangerous"; it is "out-of-repo content **behind a
tree-relative claim** is a false one".

The ingest path makes no such claim. Out-of-repo content is precisely what `raw/`
is for — #812 settles that a user may keep their corpus on shared storage — and
an ingested document is recorded as a manifest entry with its own identity,
origin and hash, not as a node keyed by a tree-relative path. There is nothing
for a symlink to falsify. Following the link is the feature.

So the rule is: **a walk that makes a tree-relative claim should refuse symlinks;
the ingest path, which makes none, resolves them.** *Should*, not *does* — three
walks implement it and the derived extractor's worktree read does not, which is
the gap recorded above rather than a licence to widen. Anyone tempted to unify
the two should change this paragraph first, because unifying them in either
direction breaks something real — resolving them in the walks that do refuse
reintroduces the false-key defect three separate call sites were written to
prevent, and refusing them in ingest deletes the shared-storage case this
decision exists to permit.

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

Neither gap is closed by wiring the existing screener up and moving on — but
**one of #813's worries is discharged by measurement rather than inherited, and
this ADR should not repeat it as fact.** #813 asks, conditionally, whether *"if
concealment can never be detected in a PDF"* a PDF could then only ever reach
`quarantine`. The antecedent is false. `screen_text` marks a directive
`concealed` when it is revealed **only by stripping invisible codepoints** —
zero-width characters, bidi controls — and returns `Verdict::Block` on that
basis ([[crates/rto-graph/src/screen.rs#screen_text]], guarded by
`strip_then_scan_reveals_a_directive_hidden_by_zero_width_characters`). Those
codepoints are format-independent **as far as the screen is concerned** — nothing
in `screen_text` is HTML-specific about them — so the concealment channel exists
for any text input, PDF included.

> **One step of that is reasoned rather than measured, and this ADR says so
> rather than repeating its own mistake.** Whether a zero-width or bidi codepoint
> in a PDF actually *survives* `pdf_extract` into the string `decoded_content`
> screens is **not established**: the existing guard exercises `screen_text`
> directly, and no fixture drives a real PDF through `pdf_content` into it. It
> plausibly depends on the document's font encoding — a `ToUnicode` CMap can
> carry U+200B where a `WinAnsiEncoding` text stream cannot. **The test that
> settles it is a PDF fixture containing a zero-width-obfuscated directive,
> driven through `pdf_content` → `decoded_content`, asserting `Verdict::Block`**,
> and it should be written before anyone relies on this paragraph. What is
> established is the negative: the presentation-shaped and PDF-native classes are
> definitely not detected.

What does *not* transfer is the **presentation** half — `display:none`, the
`hidden` attribute, a close tag splitting a phrase — and with it the PDF-native
vocabulary #813 lists: white-on-white text, text outside the crop box or beneath
an image, degenerate font sizes, render mode 3, a text layer disagreeing with
the glyphs. So the gap is real but **narrower than "block is unreachable in
PDFs"**: one concealment channel carries over and one does not, and a PDF
concealing by the second class reaches at most `quarantine`.

This ADR therefore adopts #813's acceptable outcomes as a condition on the ingest
step, in **three parts**, because the gaps are not one gap: two of them are in
what the walk screens today (foreign prose, and PDF concealment), and the third
is created by this ADR's own layer split (a model's restatement re-entering as
prose):

1. **Foreign prose must be screened.** ADR-0025's carve-out exempts prose from
   the screen, and that exemption was measured on *first-party* files. A
   third-party markdown document in `raw/` must not inherit it — the ingest path
   screens what it ingests regardless of format, which is a **widening** of
   today's rule rather than a re-wiring of it.
2. **PDF concealment must be honest about its coverage.** Either PDF-native
   detection, or the existing screen **plus an explicit declaration naming what
   was not checked** — specifically that the presentation-shaped and PDF-native
   concealment classes are undetected, while the invisible-codepoint class is
   covered.
3. **The distilled output must be screened too, and this is the one the layer
   split creates.** Screening `raw/` at ingest does not cover `knowledge/`.
   The distillation step this ADR proposes — the `ModelTask::Distil` variant
   named in Implementation, which **does not exist**; the enum today runs
   `Embed`, `Draft`, `Chat`, `Review` and the media tasks — would write a
   `knowledge/*.md` page. That page is **prose**, and prose is exactly what
   `extract.rs` exempts from the screen — so a page
   summarising a foreign document re-enters the graph through the unscreened
   branch no matter how carefully its source was checked. Screening a source and
   then admitting a model's unscreened restatement of it is a gate with a bypass
   beside it. Whether the right answer is to screen `knowledge/` on the authored
   path, or to screen the distiller's output before it is written, is an
   implementation choice; that one of them must happen is not.

**Why (3) is not covered by (1).** Ingest screening is about *bytes somebody
else wrote*. This is about *bytes a model wrote from them*, which arrive in the
one format ADR-0025 measured a carve-out for — and that measurement was over
this repository's own prose, not over machine restatements of third-party
documents. Two different populations, one exemption, and only one of them was
ever in evidence.

A screen that runs, passes, and implies a check it did not perform is not
acceptable, and this repository has paid for that shape twice already.

#### Out-of-line storage: still deferred, and #812 sharpens the same question

"Shared storage" is the out-of-line option, and it is now **decided in principle
and blocked in practice** — which is a change of status from v0.2's plain
deferral, not a change of finding. #812 names the same blocker this ADR reached
independently: *"extraction works only for files already inside a repository,
with no path to ingest a source from outside the tree. If that holds, an
out-of-tree `raw/` is blocked until that limitation is lifted, and the ADR must
say so rather than implying it works."*

It holds. Every extraction source named at the top of this section takes its paths from git's own view of one tree — `HEAD`, the index, or a workdir-rooted dirwalk — and none of them can name a path outside it.

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


### 3. `log.md` is derived, and the machinery already exists

**Resolved: `log.md` is derived from git history, and takes its place in the
bundle once it has content. The append-only reading is declined.**

v0.1 posed this as a contradiction to resolve. It is already resolved in code,
in the direction v0.1 named as the acceptable one:

- `render_log` is a pure function of its arguments —
  *"Render `log.md` (§9): dated groups, newest first"*
  ([[crates/rto-render/src/okf.rs]]).
- `assemble(concepts, title, log: &[LogDay])` takes the log as a parameter and
  writes `/log.md` beside the indexes — but **conditionally**: the file is
  appended only `if !log.is_empty()`.
- **Both callers pass `&[]`** ([[crates/roteiro/src/main.rs]], the project and
  workspace render paths), so **no bundle Roteiro has ever published contains a
  `log.md` at all.** It is omitted, not emitted empty. (v0.3 of this ADR said it
  ships an empty `# Update Log`; that was wrong, and the difference matters — an
  absent reserved file is a bundle that never made the claim, where an empty one
  would be a bundle asserting that nothing happened.)

So there is no append-only file to reconcile and never was: the slot is wired,
typed and derived-by-construction, and simply unfed. The open question was
really *what feeds `days`*, and determinism answers it — the log must be a
function of the graph at the rendered commit, exactly as every other file in the
bundle is.

**The source is git history, and it is already trusted for a load-bearing
purpose.** ADR-0021 resolves a concept's `verified.at` to the commit that last
changed that document's own path, *"rather than the render's"* — via
[[crates/rto-graph/src/git.rs#Repo]] `last_authors` — precisely so the bundle
carries no `SystemTime::now()`. That `(date, author)` pair, grouped by date and
rendered newest-first, is `log.md`. It costs no new state, no new command and no
new file to maintain; it is byte-reproducible for the same reason the trust tiers
are; and it makes the log say something true — *these concepts changed on this
date* — rather than something a process promised to append to.

**The dates are per-concept only for the authored layer, and the log must say so
rather than imply otherwise.** `authored_paths` filters to
`Provenance::Authored` before the history walk ([[crates/roteiro/src/main.rs]]),
deliberately — *"a derived symbol is confirmed by the tool, and looking up who
last touched its file would answer a question nobody asked"*. Every other concept
falls back to the render's `HEAD` commit time, so a log fed naively from all
concepts would put nine thousand derived symbols in one undifferentiated group
dated at `HEAD`, which says nothing. The determinism is unaffected — the fallback
is a *commit* time, not `SystemTime::now()` — but the content would be noise. So
`log.md` is a log of **authored-layer** change, `knowledge/` and ADRs included,
which is both the only per-concept signal available and the only one a reader of
a research bundle wants.

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
| 0.2 | 2026-09-13 | **Answers all three open questions, two of them from evidence already in the tree.** (1) Model-authored content needs **no fourth provenance class**: v0.1's premise that `authored` means *human* is contradicted at the definition — `provenance.rs` says *"a human **or agent**"*, and ADR-0013, the precedent v0.1 pointed at, sets the test as *"deliberately wrote this in a **reviewed** file"*. ADR-0013 refused `authored` for agent memory because memory is unreviewed and has no source blob; a `knowledge/` page is reviewed and has one, so it passes both tests an ADR passes. The human/model distinction is an **actor** question, which OKF already separates from the tier via `generated.by` / `verified.by` — issue #799 is the render-path fix that makes it honest, and no schema, wire or Rust change is needed. The competing candidate for the same slot, issue #801's citation records, is **closed** having reached the same verdict independently: an extracted citation stays `derived`, *"no new provenance class"*. So ADR-0001 is unchanged in substance and amended only to record a **fourth** consecutive decline. The resolution binds its own precondition: `authored` carries +40 in `search`, so it is correct only while review is genuine. (2) `raw/` is **committed**, and "gitignored" was never available, on the ground that an ignored `raw/` yields no nodes. *(v0.4 corrects the mechanics this rested on: there are more than two extraction sources, and `.gitignore` is inert over a path git already tracks. The narrower true claim is that an ignored file **git is not already tracking** yields no nodes. The resolution itself was superseded by v0.3.)* Reproducibility-from-a-commit **is** claimed (`main.rs`: *"a shareable snapshot, whose whole value is being reproducible from a commit"*), which an out-of-tree corpus breaks. Out-of-line storage is **deferred, not rejected**, behind one narrower question named here: can `sync` resolve a blob from a committed manifest entry whose bytes are not in the object database, on the `prefetch`/`VENDORED_DEPENDENCIES.md` model? Copyright was treated as a gate on ADR-0025's per-file consent. *(v0.4: that conflated two questions — ADR-0025's record governs whether a document is **extracted**, not whether it may be redistributed. The redistribution question is the user's, and v0.3 already dropped the claim from the body.)* (3) `log.md` is **derived, and its machinery already exists** — `render_log` is a pure function and both callers pass an empty slice. *(v0.2 said "already implemented"; v0.5 corrected that to the machinery, since no bundle carries the file.)* *(v0.4 corrects the consequence: `assemble` appends `/log.md` only `if !log.is_empty()`, so published bundles **omit** it rather than carrying an empty one.)* The open question was what feeds it: the per-concept commit dates ADR-0021 already resolves for `verified.at` via `last_authors`, grouped by day. An ingest-time log is declined outright, being the `SystemTime::now()` non-determinism ADR-0021 refused. Also records that the live corpus this was framed against holds no `raw/` and no PDFs — the decision is being taken before several GB enter a history that cannot forget them. Status Draft → For Review. |
| 0.3 | 2026-09-13 | **Q2 re-resolved on a maintainer decision** (issues #812, #813). `raw/` is **excluded from the standard scan** and reached by an explicit **ingest path**; without the exclusion a document is graphed twice — once as its `knowledge/` summary, which already links to the source, and once as the `raw/` file whose decoded text answers the same `search`. With no `file:` nodes coming from `raw/`, committed / untracked / ignored stops being forced and becomes a per-user choice with **local as the default** — the redistribution question stays with the person who kept the document, and the asymmetry that settles it is that **opting out stays available while opting in does not reverse**, because once the bytes are committed git history keeps them. v0.2's mechanical finding is **retained and re-purposed rather than withdrawn**: every extraction source is git-mediated and an ignored, untracked file reaches none of them, which forced "committed" *only while `raw/` was expected to be scanned*, and is now the reason the exclusion must be a **rule in the ingest configuration** rather than a side effect of where the bytes live — an ignored `raw/` and an excluded `raw/` are indistinguishable from outside, and one `git add -f` separates them. Reproducibility-from-the-repository-alone is explicitly **not** claimed; reproducibility-from-a-commit is unaffected, because the bundle projects `knowledge/` and the code graph, both committed. Adds three requirements the exclusion creates. **(a)** A committed manifest in every mode — identity, content hash, origin, access date, licence — so an absent source is *detectable rather than silent*, with hashes load-bearing and reusing the `(path, blob id, bytes)` basis. **(b)** Both symlink rules recorded together with the reason they oppose, because three refusals and one resolution otherwise read as drift: the scan's refusals (`collect_markdown`, `markdown_files` — *"a scope guarantee, not a security boundary"*, checking every path component — and the bundle walk) all guard a **repo-relative `file:` key** that a symlink falsifies silently, whereas the ingest path mints no such key *(v0.5 corrects the family: `collect_markdown` mints `lat:`, the bundle walk `okf:`, and `markdown_files` no key at all — the shared invariant is a tree-relative claim, not the `file:` prefix; v0.8 further records that the derived extractor, which does own `file:`, follows symlinks rather than refusing them)*, so there is nothing to falsify and following the link is the feature; a cycle bound and a recorded resolution are inherited regardless. **(c)** The ingest path **must screen explicitly**, because the screen is wired into the git-walk path at `extract.rs::decoded_content` (*"`admit` is the whole enforcement half"*) and excluding `raw/` removes it. The obligation is *larger* than what it replaces: a PDF in `raw/` loses a screen it gets today, and a markdown file never had one — ADR-0025's prose carve-out was measured over *"this repository's own 327 prose files"*, first-party content whose evidence does not transfer to a foreign corpus. #813's harder half is adopted as a condition, and the outcome must be PDF-native concealment detection or an explicit declaration of what was not checked. *(v0.6 corrects the justification given here: `block` is **not** unreachable in PDFs — invisible-codepoint concealment is format-independent and does reach it. Only the presentation-shaped half fails to transfer.)* Out-of-line storage moves from plain deferral to **decided in principle, blocked in practice**, on the same narrower question #812 names independently, now paired with the manifest work. ADR-0025's per-file consent over an out-of-tree path is flagged as needing re-checking rather than assumed. Summary, Option C's layer table and Implementation step 1 corrected, all three having still described `raw/` as joining the roots extraction walks. |
| 0.4 | 2026-09-13 | **Corrections from the PR #816 review round; no decision changes.** Four false statements about the code are fixed, two of them load-bearing. **(a) The Q2 mechanics were wrong twice, and both corrections strengthen the case for the exclusion.** v0.2 said *"extraction has exactly two sources of content, and an ignored file is in neither"*, retained in v0.3. There are more than two — [[crates/rto-graph/src/git.rs#GraphSource]] has `Committed`/`Worktree`/`Index` and `rto-graph` exposes `sync`, `sync_worktree`, `sync_index` and `sync_tree` — and the true property the argument needed is that **every input path is one git can name** — from the `HEAD` tree, the index, or a workdir-rooted gitignore-aware dirwalk — so out-of-tree bytes stay unreachable and the conclusion is untouched. *(v0.7: this row first said "no non-git filesystem walk anywhere", which is itself wrong — `untracked_files` **is** a filesystem walk; it is merely rooted at the workdir and classified against git.)* Worse, **`.gitignore` is inert over a path git already tracks**: v0.2 quoted a code comment scoped to the working-tree overlay and generalised it. Measured on a scratch repository — ignored-and-never-tracked yields no nodes; **committed-then-ignored yields nodes and `git check-ignore` reports the file as *not* ignored**; force-added-and-staged yields nodes. The middle case needs no `git add -f`, and it is exactly the journey this ADR permits (a user commits their corpus) followed by the regret it anticipates (they add an ignore rule and believe they opted out while the graph disagrees) — so it is now the strongest argument that the exclusion must be a rule in the ingest configuration. **(b) `log.md` is omitted, not empty.** `assemble` appends it only `if !log.is_empty()` ([[crates/rto-render/src/okf.rs]]), so with both callers passing `&[]` no published bundle contains the file at all. An absent reserved file never made the claim; an empty one would assert that nothing happened. **(c) The per-concept date feed is authored-only.** `authored_paths` filters to `Provenance::Authored` before the history walk, deliberately, and every other concept falls back to the render's `HEAD` commit time — so a log fed from all concepts would put nine thousand derived symbols in one group dated at `HEAD`. Determinism is unaffected (a commit time, not `SystemTime::now()`); the content would be noise. `log.md` is therefore a log of authored-layer change. **(d) The reproducibility claim is narrowed.** "Reproducible from a commit" already holds only for the derived and authored layers: `build_graph` calls `store.reapply_imports()` for persisted Graphify/lat/OKF layers that live in the store rather than the commit, so two clones at one commit with different import histories already differ. Pre-existing, not caused by excluding `raw/`, recorded because this ADR leans on the claim. **Also:** a sweep for target state written in the present tense — issue #811's defect class (a document asserting what the code does not do), and worse in an ADR, which is the document people trust to describe what *is* — since `crates/rto-spec/src/layer.rs` has **no `knowledge` arm** and cannot drift-check a `knowledge/` page today; the internal contradiction where the context section called `log.md` *"the same two meanings"* as the gist's append-only ingest log, which Resolved question 3 declines; and corrections inside the (unmerged) 0.2 and 0.3 rows so no false code claim ships into `main`'s history. |
| 0.5 | 2026-09-13 | **Second review round on PR #816; four more false code claims corrected, no decision changes.** **(a) The OKF frontmatter example was wrong in three ways** and would have been copied. `Frontmatter::render` ([[crates/rto-render/src/okf.rs]]) emits `generated` as a **mapping** (`by`, `at`) and `verified` as a **sequence** of `{by, at}`; v0.4 showed both as one-line sequences and omitted `at` from each. The placeholder `<the model that drafted it>` is also not an emittable token: the grammar admits exactly `human:<id>`, `<producer>/<version>` and `process:<id>` ([[crates/rto-render/src/okf.rs#Actor]]). The example now shows the real shapes, and the token mapping is handed back to #799 — a co-author trailer gives a display name with no version, which #799 itself says *"needs deciding rather than assuming"* against `lint_actor_convention` (L6). This ADR fixes only which **key** a model belongs under, not how it is spelled. **(b) The symlink invariant named the wrong key family.** v0.4 said the three refusals guard a `file:<path>` key; they do not. `collect_markdown`'s importer mints `lat:<path>` and the bundle walk mints `okf:<peer>/<path>`, while `file:` belongs to the derived extractor. And `markdown_files` mints no key at all — it bounds where `docs fmt` may **write**. The invariant is therefore a **tree-relative claim** in two variants, a key that carries a path and a write that must stay inside a named tree, and the rule reads *a walk that makes a tree-relative claim refuses symlinks; the ingest path, which makes none, resolves them*. The opposition and its reason are unchanged; only the family was misnamed. **(c) One "two sources" residue survived the v0.4 sweep** in the out-of-line section and is corrected to the git-mediated formulation. **(d) Q3's heading overstated what exists.** *"`log.md` is derived, and it already exists"* reads as though bundles carry the file; they do not, since `assemble` omits it while the log is empty. Retitled to *"the machinery already exists"*, which is the true claim — a pure `render_log`, a typed parameter, and no caller feeding it. |
| 0.6 | 2026-09-13 | **Third review pass; one claim of mine disproved by measurement, three defects fixed.** **(a) `block` IS reachable for PDF input, and v0.3–v0.5 implied otherwise.** #813 posed it **conditionally** (*"if concealment can never be detected in a PDF"*) and asked for verification; this ADR carried the conditional forward while asserting its antecedent. Checked now: `screen_text` marks a directive `concealed` when it is revealed **only by stripping invisible codepoints** — zero-width characters, bidi controls — and returns `Verdict::Block` on that basis ([[crates/rto-graph/src/screen.rs#screen_text]], guarded by `strip_then_scan_reveals_a_directive_hidden_by_zero_width_characters`). Those codepoints are format-independent and survive PDF extraction. The **presentation** half (`display:none`, `hidden`, split tags) genuinely does not transfer, and neither does the PDF-native vocabulary #813 lists, so the gap is real but narrower: one concealment channel carries over and one does not, and a PDF concealing by the second class reaches at most `quarantine`. The requirement is unchanged; its justification is now accurate. **(b) The absent-source requirement was self-contradictory** — it called a stale authored reference drift and an absent source a "known absence, not an error" in consecutive sentences. Split explicitly: *bytes absent with a manifest entry present* is the ordinary state of a clone that did not fetch the corpus and **must not fail `check`**; *no manifest entry, or a hash that no longer matches* **is** drift. **(c) The v0.3 history row stated the core asymmetry backwards** — "opting in is reversible where opting out is not", inverting the argument the body makes correctly, since committing is the irreversible direction. Corrected in place, the row being unmerged. **(d)** `docs/adr/README.md` indexed every ADR through 0025 and omitted 0026 entirely, so the catalogue never saw this decision; the entry is added at `For Review`. |
| 0.7 | 2026-09-13 | **Fourth review pass. Three of these are the *same* overreach in successive spellings, which is the finding worth keeping.** **(a)** v0.2 said extraction has *"exactly two sources"*; v0.4 corrected the count but replaced it with *"no non-git filesystem walk anywhere"*; that is also false. [[crates/rto-graph/src/git.rs#Repo]] `untracked_files` **is** a filesystem dirwalk — it is merely rooted at the repository workdir, gitignore-aware, classified against the index, and skipping nested repositories, symlinks and non-regular files. The claim that survives every version of this sentence, and the only one the argument ever needed, is that **every input path is one git can name**: the `HEAD` tree, the index, or that workdir-rooted walk. Nothing can name a path outside the tree, so out-of-tree bytes stay unreachable and #812's blocker holds. Fixed in the body, in the out-of-line section, and inside the v0.4 row that introduced the second spelling. Recorded as a pattern rather than three separate slips: each attempt reached for a *stronger mechanical absolute* than the evidence supported, when the weaker relational claim was sufficient throughout. **(b) The Summary still described the whole design in the present tense**, so a reader meeting the ADR at its first paragraph would take an unbuilt exclusion and an unbuilt projection as current behaviour. It now opens by saying all three decisions are unbuilt, and the reproducibility paragraph no longer says the bundle projects `knowledge/` today. **(c)** ADR-0001's `#[non_exhaustive]` paragraph still read *"three consecutive ADRs"* after v1.7 made it four; fixed there at v1.9, with the v1.3 history row keeping "three" because it records what v1.3 said. **(d)** `docs/adr/README.md` gained its ADR-0026 entry at v0.6, noted here because the catalogue had never listed this ADR at all. |
| 0.8 | 2026-09-13 | **Records a live defect in `main` that this ADR was arguing from the absence of.** The symlink section said the standard scan refuses symlinks, citing three walks. It does — but the **derived extractor does not**, and that is the family the section used as its example. `sync_worktree` reads each tracked path with plain `std::fs::read` ([[crates/rto-graph/src/sync.rs#sync_worktree]]), which follows a link. Measured: replace a tracked `docs/note.md` with a symlink to a file outside the repository, run a worktree sync, and the out-of-repo bytes are stored as `file:docs/note.md` — precisely the false-key defect the other three walks exist to prevent. **Pre-existing in `main`, not introduced by this decision, and reported rather than fixed because this PR changes no code.** Two consequences for the ADR. The rule is restated as *a walk that makes a tree-relative claim **should** refuse symlinks* — should, not does, with the gap named — because an ADR resting on a guarantee the scan does not give is the same defect class as the present-tense sweep at v0.4. And the ingest path resolving links is **less anomalous than three-refusals-versus-one-resolution implied**: what distinguishes it is that it mints no tree-relative key, not that it is alone in following a link. Also sharpens *"every input path is one git can name"* to *"comes from git's own enumeration of one working tree"*, since the former could be read as "tracked" and untracked-but-not-ignored files are enumerated too. |
| 0.9 | 2026-09-13 | **Final review pass; one real cost this ADR had understated, and two residues.** **(a) Splitting `generated` from `verified` needs a Rust change, which v0.2–v0.8 said it did not.** [[crates/rto-render/src/okf.rs#Origin]] holds a **single** `Actor`, and `Frontmatter::render` writes that one actor into both keys — which is the mechanical reason #799 measures them as identical rather than an oversight in the render path. Carrying two actors means changing `Origin` or `Frontmatter`, a breaking change to `rto-render`'s Rust surface. The claim is narrowed to what is true — **no OKF format change and no `Provenance` change** — and the Rust cost is stated and priced against existing precedent: under `AGENTS.md`'s `rto-*` carve-out, recorded at ADR-0001 v1.5, it ships as a **minor** with no `!`. Worth noting the shape of the error: *"no Rust break"* was inferred from *"no wire break"*, and they are separate questions this repository has already had to separate once. **(b)** One more present-tense residue: *"`raw/` produces no `file:` nodes by design"* described the target state as current. It produces them today — `IngestConfig` has no `raw` exclusion — and the text now says so. **(c)** The v0.3 row's superseded claim that all three symlink refusals guard a `file:` key now carries the v0.5 and v0.8 corrections inline, so a reader of the changelog alone is not left with the wrong family. |
| 0.10 | 2026-09-13 | **Reconciles two sections of this ADR that contradicted each other, and closes a gap in its own requirement.** **(a)** v0.7–v0.9 argued that nothing can name a path outside the tree; v0.8 then recorded that the derived extractor follows tracked symlinks. Both cannot stand, and the resolution is an asymmetry worth having: on the **committed** path a tracked symlink resolves to the **git blob**, whose content is the target *path string*, so out-of-tree bytes are unreachable; on the **worktree** overlay `std::fs::read` follows the link and they are reachable. Measured both ways. **This is why the bundle argument survives**: `render okf` and `export` deliberately read the committed source — *"a shareable snapshot, whose whole value is being reproducible from a commit"* — so out-of-tree bytes cannot enter a **published bundle**, which is the property #812's blocker rests on. What they can enter is a local worktree preview and its `search` results. Smaller and different from what v0.4–v0.8 claimed, and now stated rather than glossed. **(b) The screening requirement only covered PDFs**, while the text two paragraphs above establishes that third-party markdown in `raw/` has *never* been screened and that ADR-0025's first-party prose carve-out does not transfer to it. Split into two parts: **foreign prose must be screened** (a widening of today's rule, not a re-wiring), and **PDF concealment must declare its coverage** — naming the presentation-shaped and PDF-native classes as undetected while the invisible-codepoint class is covered. **(c)** The v0.2 row still called `log.md` *"already implemented"*; corrected inline to the machinery, consistent with the Q3 heading fixed at v0.5. |
| 0.11 | 2026-09-13 | **Closes a laundering path the layer split creates, which every previous screening pass missed.** Screening `raw/` at ingest does not cover `knowledge/`. `ModelTask::Distil` writes a `knowledge/*.md` page; that page is **prose**, and prose is precisely what [[crates/rto-graph/src/extract.rs]] exempts from the screen — so a summary of a foreign document re-enters the graph through the unscreened branch however carefully its source was checked. Screening a source and then admitting a model's unscreened restatement of it is a gate with a bypass beside it. Added as a **third part** of the ingest screening requirement, with the implementation choice left open (screen `knowledge/` on the authored path, or screen the distiller's output before it is written) and the obligation not. This is distinct from part (1): ingest screening covers *bytes somebody else wrote*, and this covers *bytes a model wrote from them* — arriving in the one format ADR-0025 measured a carve-out for, where that measurement was over this repository's own prose rather than over machine restatements of third-party documents. Two populations, one exemption, and only one of them ever in evidence. **Also:** *"one working tree"* omitted `sync_tree`'s arbitrary revision, which is listed as an entry point two sentences earlier; now *"one tree — a commit's tree (`HEAD`, or any revision `sync_tree` is given), the index, or a workdir-rooted dirwalk"*. |
| 0.12 | 2026-09-13 | **Qualifies a claim this ADR reasoned rather than measured, and separates two hashes it had conflated.** **(a)** v0.6–v0.11 stated flatly that zero-width and bidi codepoints *"survive PDF text extraction"*, making `block` reachable for PDF input. The screen half is established — nothing in `screen_text`'s codepoint handling is HTML-specific. The **extraction** half is not: the existing guard exercises `screen_text` directly, and no fixture drives a real PDF through `pdf_content` into `decoded_content`. Whether U+200B survives plausibly depends on the document's font encoding — a `ToUnicode` CMap can carry it where a `WinAnsiEncoding` text stream cannot. The paragraph now names the test that would settle it (a PDF fixture with a zero-width-obfuscated directive, asserting `Verdict::Block` through the real path) and marks the claim as pending it. Recorded because it is the **same** error this ADR corrected at v0.6 — asserting an unverified antecedent about PDFs — committed again while correcting it, in the opposite direction. The negative remains established: the presentation-shaped and PDF-native classes are definitely undetected. **(b)** The manifest requirement said to reuse the `(path, blob id, bytes)` basis. That is the *derivation basis of a derived fact*, folding in a path and an extractor version; a manifest wants only *these bytes, this version of this work*. The right primitive is [[crates/rto-graph/src/git.rs#Repo]] `blob_oid`, which computes a git blob id **from bytes alone** through `gix::objs::compute_hash` — needing no object database, and therefore usable on bytes that are not in the repository at all, which is exactly the out-of-tree case. |
| 0.13 | 2026-09-13 | **Three residues from this PR's own edits; no new evidence, no decision change.** **(a)** v0.11 introduced the distilled-output screening requirement using `ModelTask::Distil` as though it were an existing API. It is not — [[crates/rto-graph/src/model_choice.rs#ModelTask]] runs `Embed`, `Draft`, `Chat`, `Review` and the media tasks, and `Distil` is the variant Implementation step 4 *proposes*. Reworded so the requirement is clearly about a step this ADR is asking for, which is also why the requirement matters: it has to be designed in, not retrofitted. **(b)** The same v0.11 edit added a third numbered obligation under a lead saying *"in **two parts**"*, leaving the layer-split requirement looking like an aside outside the accepted condition. Now three, with the split named — two gaps in what the walk screens today (foreign prose, PDF concealment) and one created by this ADR's own layer split. **(c)** One present-tense residue survived v0.7's Summary rewrite: *"so a document is graphed once"* stated the outcome of an unbuilt exclusion as current, in a paragraph whose first sentence says the opposite. It now says a document *would be* graphed once, and that today it is graphed twice because the scan has no `raw/` exclusion to apply. |

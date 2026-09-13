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

Three decisions. This is a `For Review` ADR: the `raw/` root and the
`knowledge/` layer are unbuilt, and the MCP surface exists but its **narrowing**
is not done, so what follows describes a target state throughout. It is written
in the present tense of the mechanism it proposes, because that is the only tense
in which a mechanism can be argued; read every such passage as *would*, and the
passages that report what the code does today say so explicitly. A `raw/` source root **would be
added and excluded** from the walk extraction already performs, reached instead
by an explicit ingest path under the consent rule
[[docs/adr/0025-document-extraction-consent.md]] set — so that a document **would
be** graphed once, as its summary, rather than twice (issue #812). The
duplication is a property of the design *without* the exclusion, not of the
repository today: there are no `knowledge/` summaries yet, so a `raw/` document
that the scan can see at all is currently graphed once, as the raw file — and one
the scan cannot see, a never-tracked file under an ignored `raw/`, has **no node
at all**. The exclusion is what keeps it at one
after the summaries exist. A **`knowledge/`
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
`derived | authored | inferred` already — six tokens since 5.1.0, the three
qualified by `external-*` for a peer's facts — and that vocabulary is the reason
this
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
| `raw/` | the user | immutable — read, never written | source of truth; **would not be scanned** — no `file:` nodes (today the scan has no `raw/` exclusion) |
| `knowledge/` | a model, in the repository | **yes** — committed, diffed, reviewed | `authored` |
| the OKF bundle | `render` | disposable, regenerated | projection of both |

`knowledge/` would join the layer ADRs and blueprints already occupy. A model
writing `knowledge/attention.md` performs the same act as a person writing an
ADR, and would inherit everything that comes with it: drift checking, `roteiro
check`, review before merge, and a git history that says who changed what.
(*Would*: [[crates/rto-spec/src/layer.rs]] has no `knowledge` arm, so none of
that reaches such a page until step 2 lands.)

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
   providing. **A committed manifest ships with it, in every storage mode** —
   identity, content hash (`blob_oid` over the bytes), origin, access date,
   licence — since without it a summary cannot tell an absent source from a
   changed one. **The exclusion must be applied to both readers** — see below.
   See Resolved question 2.
2. `knowledge/` recognised as authored-layer input, with `render okf` projecting
   it into the bundle as its own kind.
3. The MCP surface narrowed — see below.
4. A `ModelTask::Distil` variant for the summarisation step — **a variant this
   ADR proposes**; the enum today runs `Embed`, `Draft`, `Chat`, `Review` and the
   media tasks.

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
pure-function-of-source promise"*.

**Step 2 must reconcile `Authored`'s documentation across four prose sites, and it is
already drifted before this ADR touches it.** The *"human or agent"* half is what
carries this resolution; the **enumeration** attached to it is what must grow —
and there is more than one enumeration, none of which currently matches the code:

| site | what it says `Authored` covers |
|---|---|
| [[crates/rto-graph/src/provenance.rs#Provenance]] | "an ADR, blueprint, or annotation" |
| [[crates/rto-graph/src/model.rs#Node]] `provenance` | "ADR/blueprint/lat sections" |
| [[crates/rto-render/src/okf.rs]] module table | "ADR and blueprint prose" — the narrowest |
| [[docs/adr/0021-open-knowledge-format-bundle.md]] provenance→tier table | "ADR and blueprint prose" — **a governing ADR**, so leaving it stale contradicts the decision, not just a comment |
| **the code** | those, plus `site_page` **and `site_section`** ([[crates/rto-spec/src/site.rs]]), and imported `lat` docs ([[crates/rto-spec/src/lat.rs]]) |

Four prose lists whose scopes overlap but do not agree — two of them identical
(the `rto-render` module table and ADR-0021 both say "ADR and blueprint prose")
and narrower than the other two — over a fifth, wider, actual usage.
So `knowledge/` is not being added to a tidy set of three — it is the occasion to
fix an enumeration that had already fallen behind four times. **That is a
documentation debt this ADR inherits rather than creates**, and naming it here is
the point: an implementation that adds a `layer.rs` arm and updates only
`provenance.rs` leaves **three** other lists contradicting it — `model.rs`, the
`rto-render` module table and ADR-0021 — which is how the drift accumulated in
the first place. None of this is a variant change, so the
vocabulary cost stands as stated — but it is **five edits, not one**, and one of
them is an amendment to ADR-0021 rather than a doc comment.

> Recorded because it happened here too, three times: the first version of this
> table listed three sites and named only `site_page`; review then found a fourth
> list, the missing `site_section`, and finally ADR-0021's own mapping table. An
> inventory written specifically to stop a partial update was itself partial at
> every revision. **Treat the table as a known minimum, not a closed set**, and
> grep `Provenance::Authored` before relying on it.

A `knowledge/` page **as Option C defines it**
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
  - by: human:<id>                # see below on who this actually names
    at: <commit time>
```

`human:<id>` resolves to `commit.author().name` for the commit that last changed
the document's path ([[crates/rto-graph/src/git.rs#Repo]] `last_authors`) — the
**author**, not the committer, and not an independently observed reviewer. That
is the strongest available claim, and ADR-0021 already prices it: the tier means
*a person stands behind this document*, established by authorship of the change
rather than by a recorded review. Calling it "the person who reviewed it", as
earlier drafts of this ADR did, overclaims what the field can know.

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
but **not** no Rust change at all, which earlier drafts of this resolution
implied.
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
authored-layer input with **no OKF format, wire or `Provenance` change**. That is
the claim this resolution makes, and it is the only one it makes: the *rendering*
cost is larger than a `layer.rs` arm, and this ADR states it rather than letting
a reader infer a one-line change. Step 2 needs, at minimum:

- a **`layer.rs` arm**, so the page is recognised as authored input at all;
- the **`Authored` enumeration reconciled** across the five sites above, one of
  which is a governing ADR;
- a **`section_for` arm** — [[crates/rto-render/src/okf.rs#section_for]] routes
  every unrecognised kind to `symbols` (`_ => "symbols"`), so without one a
  research note lands in the symbol directory rather than the `knowledge/`
  section this ADR requires; and
- **`knowledge` added to the body allowlist** — the bundle attaches prose only
  for `"file" | "adr" | "blueprint" | "doc" | "site_page"`
  ([[crates/roteiro/src/main.rs]]), so without it a `knowledge/` concept renders
  with **no body at all**, which is the one thing a research note consists of.

The last two are the ones a reader would otherwise miss, because both fail
*quietly*: an unrouted kind is misfiled rather than rejected, and a page outside
the allowlist is empty rather than absent. It takes a
dependency on #799 for the actor split, which is not a blocker to building the
layer — though, as recorded there, #799 itself needs a breaking `rto-render`
change to carry two actors, shipping as a minor under the `rto-*` carve-out.

### 2. `raw/` is **local by default**, and excluded from the standard scan

**Resolved (maintainer decision, issue #812): `raw/` is not one of the
three fixed options. It is a per-user choice — local by default, committed or on
shared storage if a user decides so — and it is reached by an explicit **ingest
path**, not by the walk extraction already performs.**

The default is local because that keeps the redistribution question with the
person who chose to keep the document. A project that committed every PDF
somebody read would be a redistributor of third-party material by default, and
the asymmetry is what settles it: opting *in* is a decision a user can make about
their own corpus, while opting *out* afterwards is not, because git history keeps
the bytes.

#### The exclusion covers **two** scans, not one

**This is the part of the decision most easily under-built, and it is recorded
here because getting it wrong is silent** (issue #817). "Excluded from the
standard scan" reads as one rule against one walk. There are **two independent
readers of committed blobs**, and `raw/` must be excluded from both:

1. **Derived extraction** — the walk that produces `file:` nodes and
   `meta.content`. This is the one the duplicate-node argument above is about.
2. **The authored layer** — [[crates/rto-spec/src/layer.rs#authored_blobs]] walks
   *every* path independently (`walk_blobs` / `index_files`, plus
   `added_since_head` on the worktree), and `authored_docs_from` then classifies
   what it finds **by content, not by location**.

**Every branch of that classifier is a door, and the count is deliberately not
given here.** `authored_docs_from` is an `if`/`else if` chain over the file's
*text* — ADR declaration, site-page declaration, blueprint shape, and a final
`else` that scans whatever is left for `@rto:` annotations and convention
violations. A committed markdown file under `raw/` — which #812 explicitly
permits — reaches one of those arms by construction, because the chain is total:
there is no arm that declines a file.

Enumerating them was tried and got the number wrong four times running, which is
the argument for not enumerating. **What the decision rests on is the property,
not the list: the authored classifier matches by *content*, not by *location*.**
Read [[crates/rto-spec/src/layer.rs#authored_docs_from]] for the current set; any
branch added there is a new door, and an exclusion written against a list would
silently fail to cover it.

The ADR arm is the sharpest illustration. That rule is a good one — it is what
lets a repository keep its decisions somewhere other than `docs/adr/`, and the
code says so: *"a document that declares `type: adr` has said otherwise, wherever
it sits and whatever it is called"* — and it is exactly what makes an ingested
third-party document dangerous, because **the document decides its own class**.
A paper carrying `type: adr` in its frontmatter is parsed as one of ours. This is the same hazard the three symlink
refusals exist to prevent — *out-of-repo content behind a repo-relative-looking
claim* — arriving through content classification rather than through a link.

**So the exclusion is a rule in the ingest configuration, applied at both
readers, and not a property of where the bytes sit.** That is the same conclusion
the `.gitignore` finding reached from the other direction, and the two together
are why it cannot be left implicit: a `raw/` that is merely untracked is excluded
by accident at both readers, and the moment a user commits their corpus — the
choice this decision grants them — both accidents end at once.

The reason the choice is *free* is the exclusion. Once `knowledge/` exists, a
document reached through the standard scan **would be** graphed **twice** — once
as the `knowledge/` summary, which already links to its source, and once as the
`raw/` file itself, whose decoded text lands in `meta.content` and answers the
same `search` query. (Not today: with no `knowledge/` layer there is no summary
to duplicate, so a `raw/` document is graphed once, as the file. The duplication
is what shipping step 2 *without* step 1's exclusion would create.) One document,
two nodes, two hits. Excluding `raw/` from the scan removes the duplicate, and it
also removes the constraint that made this question look forced.

#### Why the mechanical argument no longer binds — and why it still matters

A first reading of this question answered it as *"`raw/` is committed, and
ignored is not available"*, on the ground that *"extraction has exactly two
sources of content, and an ignored file is in neither"*. **Both halves of that
mechanism are wrong, and both corrections make the case for the exclusion
stronger rather than weaker** (measured 2026-09-13).

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
filesystem walk — denying that was this argument's second overreach, after the
count — but it is rooted at the repository workdir and classified against
git's own view, so it cannot *enumerate* a path outside the tree.

**Reachability of out-of-tree bytes splits on the source, and the split is what
the argument actually needs.** A tracked symlink is a path git can name whose
target is outside, so enumeration is not the whole story:

| scenario | source | what happens | out-of-tree bytes? |
|---|---|---|---|
| a symlink **committed as a symlink** | `Committed`, `sync_tree`, `Index` | **the entry is skipped** — every enumerator filters to regular-file modes (`walk_tree_blobs`: `entry.mode.is_blob()`; `index_files`: `Mode::FILE \| FILE_EXECUTABLE`) | **no** — and no node at all |
| a **tracked regular file replaced on disk** by a symlink | `Worktree` overlay | the tree still holds the regular blob; `std::fs::read` **follows** the link | **yes** |

**Two scenarios, not four sources, and conflating them is what this table got
wrong twice.** A committed symlink is invisible to every tree- and index-based
enumerator for one shared reason — mode filtering — so there is no "resolves to a
blob holding a path string" anywhere; that was a plausible model of git rather
than a reading of the code. The worktree row is a *different situation*: the
committed object is an ordinary blob, and only the on-disk file has been swapped.

Measured both ways. Committing a symlink to a file outside the repository
(`120000 blob …`) and running a sync produces **no node for that path at all** —
the blob count is one lower than the file count; replacing a tracked regular file
on disk stores the target's bytes under the tracked key (see
the symlink section below, where that is recorded as a live defect).

**This is why the bundle argument survives.** `render okf` and `export`
deliberately read the **committed** source — *"a shareable snapshot, whose whole
value is being reproducible from a commit"*
([[crates/roteiro/src/main.rs]]) — and on that path a committed symlink is
**skipped by the tree walk entirely**, so there is nothing to read and no node to
carry it. **A symlink therefore contributes nothing to the derived and authored
layers a bundle is rendered from**, which is the property #812's blocker rests
on. What it can reach is a local worktree preview and its `search` results.

Stated that narrowly on purpose: it is a claim about the **symlink case on the
committed path**, not about everything a bundle can contain. `build_graph`
re-applies persisted Graphify / lat / OKF import layers after rebuilding those
two ([[crates/roteiro/src/main.rs]] `store.reapply_imports()`), and `render okf`
renders the resulting concepts — so a general "no out-of-tree content can enter a
published bundle" would reach past what this reading establishes, and past what
the import layers, which live in the store rather than the commit, would
support.

**`.gitignore` does not un-graph a file git already tracks — it is inert over
tracked paths.** The *"an ignored file is in neither"* reading quoted a code comment
scoped to the working-tree overlay and generalised it to the whole system. The
comment is right about what it describes; the generalisation is not. Measured on
a scratch repository:

| case | `git check-ignore` | nodes? |
|---|---|---|
| `raw/` ignored from the start, never tracked | ignored | **no** |
| `raw/paper.md` committed, `.gitignore` added afterwards | **reports it as *not* ignored** | **yes** |
| `raw/paper.md` ignored, then `git add -f` (staged, uncommitted) | ignored | **yes** |

The middle row is the one that first reading missed, and it needs no `git add -f`: adding an
ignore rule over a tracked path changes nothing at all, and git does not even
consider the file ignored. So the retained half of the finding is narrower than
first claimed — **an ignored file that git is not already tracking** yields no
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
behaving differently from the ingest path, and **no Roteiro setting changed** —
adding a `.gitignore` entry is of course a change, but it is a git one, which is
precisely the point: the ingest behaviour moved without anything in the ingest
configuration moving. The
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
  right thing to reuse is the content hash, not the extraction key**; naming the
  `(path, blob id, bytes)` tuple, as an earlier draft did, conflates them. That tuple is the
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

- a PDF in `raw/` **loses** a screen it gets today — *if* the walk sees it at
  all; under the local default it is untracked and never traversed, so for that
  case the screen is not lost but was never reached;
- a markdown file in `raw/` **never had one** either way, and the reason it never
  had one does not transfer to foreign content.

Both bullets point the same direction: the ingest path is the **first** screen
these documents meet, not a replacement for one they were getting.

Neither gap is closed by wiring the existing screener up and moving on — but
**one of #813's worries is discharged by measurement rather than inherited, and
this ADR should not repeat it as fact.** #813 asks, conditionally, whether *"if
concealment can never be detected in a PDF"* a PDF could then only ever reach
`quarantine`. **The reasoning behind that antecedent is wrong, and its truth is
unestablished** — two different things, and the ADR needs both. What is wrong is
the premise that the screen's concealment detection is exclusively HTML-shaped:
`screen_text` marks a directive
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
> settles it is a PDF fixture containing a zero-width-obfuscated directive**,
> and it should be written before anyone relies on this paragraph. Note the
> assertion has to be chosen with care: `decoded_content` returns the admitted
> `String` and a list of finding *classes*, not a `Verdict`, so a test cannot
> assert `Verdict::Block` through it without changing that signature. Two
> writable forms, neither needing an API change — drive `pdf_content`'s output
> into `screen_text` and assert the verdict there (which is precisely the
> survival question), or drive the fixture through `decoded_content` and assert
> it **admits nothing** while recording the directive class, which is the
> enforcement the screen actually performs. What is
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
step, in **three parts**, because the gaps are not one gap. Two are in what the
existing extraction path does with foreign content — prose it admits without
screening, and PDF concealment it screens only partly — and the third is created
by this ADR's own layer split, a model's restatement re-entering as prose:

1. **Foreign prose must be screened.** ADR-0025's carve-out exempts prose from
   the screen, and that exemption was measured on *first-party* files. A
   third-party markdown document in `raw/` must not inherit it — the ingest path
   screens what it ingests regardless of format, which is a **widening** of
   today's rule rather than a re-wiring of it.
2. **PDF concealment must be honest about its coverage, in two named places.**
   Either PDF-native detection, or the existing screen **plus an explicit
   declaration naming what was not checked** — specifically that the
   presentation-shaped and PDF-native concealment classes are undetected, and
   that the invisible-codepoint class is covered *by the screen* but
   **unverified end-to-end through PDF extraction** (see the qualification
   above). Declaring it simply "covered" would overstate the evidence this ADR
   actually has.

   **Where it must appear, because a requirement a code comment can discharge is
   not a requirement.** The declaration is owed in **both**:

   - **the ingest command's own output, on the run that accepts the document** —
     per accepted document, not once at start-up and not behind a verbosity
     flag, so the person admitting a file cannot avoid reading what the screen
     did not check; and
   - **`docs/`**, so the statement is durable, reviewable and citable rather
     than surviving only in a terminal scrollback.

   A source comment discharges neither. The two surfaces are chosen to fail
   differently: the first is unavoidable but ephemeral, the second durable but
   easy never to open, and only a reader who meets both is reliably informed.
   This is deliberately stronger than #813's *"visible to the user of the upload
   path"*, which a sufficiently literal implementation could satisfy without any
   user ever seeing it.
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
and blocked in practice** — a change of status from a plain deferral, not a
change of finding. #812 names the same blocker this ADR reached
independently: *"extraction works only for files already inside a repository,
with no path to ingest a source from outside the tree. If that holds, an
out-of-tree `raw/` is blocked until that limitation is lifted, and the ADR must
say so rather than implying it works."*

It holds. Every extraction source named at the top of this section takes its paths from git's own view of one tree — a commit's tree (`HEAD` or any revision `sync_tree` is given), the index, or a workdir-rooted dirwalk — and none of them can *enumerate* a path outside it. The one way out-of-tree bytes are read at all is a tracked symlink on the **worktree** overlay, which is a defect rather than a mechanism (see the symlink section) and is absent from the committed path a bundle is rendered from.

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
  workspace render paths), so **neither current render path emits a `log.md` at
  all.** (Stated of the code as it stands: a source reading cannot establish what
  every past release did, and no historical audit was run.) It is omitted, not emitted empty — a distinction worth
  keeping, since an absent reserved file is a bundle that never made the claim,
  where an empty one would be a bundle asserting that nothing happened.

So there is no append-only file to reconcile and never was: the slot is wired,
typed and derived-by-construction, and simply unfed. The open question was
really *what feeds `days`*, and determinism answers it — the log must be a
function of the graph at the rendered commit, exactly as every other file in the
bundle is.

**The source is git history, and it is already trusted for a load-bearing
purpose.** ADR-0021 resolves a concept's `verified.at` to the commit that last
changed that document's own path, *"rather than the render's"* — via
[[crates/rto-graph/src/git.rs#Repo]] `last_authors` — precisely so the bundle
carries no `SystemTime::now()`. **The date half of that record**, grouped by day
and rendered newest-first, is `log.md` — the author is deliberately not carried
into the entry, because `verified.by` already states it per concept and repeating
it per line would put the same name on every row of a single-maintainer
repository without adding information. **The order must be total, not just
newest-first.** [[crates/rto-render/src/okf.rs#render_log]] emits days and
entries in the order its caller supplies, so "grouped by date" alone leaves ties
free and two conforming implementations would emit different bundles — which the
determinism this whole ADR rests on does not permit, and which `okf diff` would
report as a change that no commit explains. The tiebreak is therefore specified
here rather than left to the implementer: **days descending by date; within a
day, entries ascending by **member-qualified** concept key.** Plain key is not
enough: `assemble` scopes identity by workspace member, so *"the same key in two
members is two concepts"* ([[crates/rto-render/src/okf.rs]]) and `file:README.md`
can legitimately appear twice. Qualified by member the order is total, and it is the same principle `assemble` already applies
when it settles filename collisions where the whole set is visible.

**One precondition on granularity.** `last_authors` resolves one record per
repository-relative **path**, not per concept, so every `adr_section` from one
file carries that file's date and "which section changed" is not recoverable from
it. Entries are therefore per **document**, not per section — which is what a
reader of a change log wants anyway, and is stated so the implementer does not
discover it after writing the section-level version.

**The entry payload needs specifying for the same reason**, since `render_log`
takes preformatted strings and would otherwise accept any of them: an entry is
`**Update**: <title> (<member-qualified key>)`, one per changed document, with the title escaped as
frontmatter scalars already are — a raw newline in a heading must not be able to
start a new list item. Ordering alone does not make a render reproducible if two
implementations can disagree about what they are ordering. It costs no
new state, no new command and no
new file to maintain; it is byte-reproducible for the same reason the trust tiers
are; and it makes the log say something true — *these concepts changed on this
date* — rather than something a process promised to append to.

**Two preconditions the feed inherits, both from ADR-0021's own attribution
path.** First, `last_authors` is **history-dependent, not a function of the
rendered commit alone**: at a shallow boundary a commit's parents are absent, so
ADR-0021 records that the concept confirms nothing and *"the workflow that
publishes the bundle asks for full history so the published artifact is
attributed rather than blank"*. A `log.md` fed from it is reproducible **given
full history**, and blank without it — the same precondition the trust tiers
already carry, so it is inherited rather than new, but it must be stated because
"a function of the graph at the rendered commit" is not quite true of it. Second,
the attribution map is looked up **by path, without re-checking provenance**
(`r.authors.get(p)`, [[crates/roteiro/src/main.rs]]), so a *derived* concept
sharing an authored document's path picks up that document's author rather than
falling back. The fallback below is therefore the common case, not a universal
one.

**The dates are per-concept only for the authored layer, and the log must say so
rather than imply otherwise.** `authored_paths` filters to
`Provenance::Authored` before the history walk ([[crates/roteiro/src/main.rs]]),
deliberately — *"a derived symbol is confirmed by the tool, and looking up who
last touched its file would answer a question nobody asked"*. Every other concept
**whose path is not itself an authored one** falls back to the render's `HEAD`
commit time — a derived symbol inside an ADR does inherit that document's date,
per the by-path lookup noted above — so a log fed naively from all
concepts would put nine thousand derived symbols in one undifferentiated group
dated at `HEAD`, which says nothing. The determinism is unaffected — the fallback
is a *commit* time, not `SystemTime::now()` — but the content would be noise. So
`log.md` is a log of **authored-layer** change, `knowledge/` and ADRs included,
which is both the only per-concept signal available and the only one a reader of
a research bundle wants.

**What is declined, explicitly.** An ingest log that records "this PDF was read
at this time" **from the clock at render time** is not derivable from a commit,
because that moment is not a property of the tree. Note the narrow scope: Q2
already requires the manifest to carry a committed **access date**, and once that
timestamp is in the tree it is tree data like any other and can feed a
deterministic render. What is refused is an *unrecorded* ingest moment read from
the clock — not the idea of dating an ingest. Two renders of one commit would disagree, which is the
`SystemTime::now()` defect ADR-0021 already refused. If an ingest record is
wanted, it is a committed artifact in **`knowledge/`** — not `raw/`, which this
ADR excludes from **both** scans, so once that exclusion is in place a file there
becomes no concept at all and `authored_paths` would never see it (today, before
it is built, a committed `raw/*.md` declaring `type: adr` would be read as an ADR
— which is the hazard the two-scan section above exists to close), leaving `log.md` unable to reflect the very
record that was meant to feed it. Placed in `knowledge/`, it is an authored file
like any other and `log.md` reflects its commits. ADR-0025's per-file consent
record is the natural **shape** for it — it already records a per-file decision —
but not, today, the carrier: consent is persisted as a row in the local
`graph.db` ([[crates/rto-graph/src/okf_consent.rs]]), which no clone shares and
no commit records. Making it a committed artifact is the same work as the
manifest in Resolved question 2, which is a further reason to design the two
together. The
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
| 0.2 | 2026-09-13 | **Resolves all three open questions; status Draft → For Review.** **Q1 — no fourth provenance class.** A model-written `knowledge/` page is `authored`. v0.1's premise that `authored` means *human* is contradicted at the definition (*"a human **or agent**"*), and [[docs/adr/0013-agent-memory-artifact-store.md]] — the precedent v0.1 pointed at — sets the test as *"deliberately wrote this in a **reviewed** file"*, refusing `authored` for agent memory because memory is unreviewed and has no source blob. A `knowledge/` page is reviewed and has one. The human/model distinction is an **actor** question, which OKF separates from the tier via `generated.by` / `verified.by`; **#799** is the render-path work that makes it honest. Step 2 must widen `Authored`'s doc comment, which enumerates ADR/blueprint/annotation and not `knowledge/`, and splitting the two actors needs a breaking `rto-render` change since `Origin` holds a single `Actor` — a **minor** under `AGENTS.md`'s `rto-*` carve-out (ADR-0001 v1.5), not a format or `Provenance` change. #801, the competing candidate for the same slot, is **closed** having independently reached *"no new provenance class"*. `authored` carries +40 in `search`, so the resolution holds only while review is genuine. **Q2 — `raw/` is local by default and excluded from the standard scan** (maintainer decision, #812/#813): a scanned `raw/` graphs each document twice, once as its summary and once as the source. With no `file:` nodes from `raw/`, committed / untracked / ignored becomes a free per-user choice, and **local** is the default because opting out stays available while opting in does not reverse — git history keeps the bytes. Reproducibility-from-the-repository-alone is **not** claimed. Three requirements follow: a **committed manifest in every mode** so an absent source is detectable rather than silent (hashed with `Repo::blob_oid`, which computes a git blob id from bytes alone and so works out of tree — not the `(path, blob id, bytes)` extraction key); **both symlink rules recorded with why they oppose**, the scan's refusals guarding a tree-relative claim that the ingest path never makes; and **the ingest path must screen**, in three parts — foreign prose (ADR-0025's carve-out was measured on this repository's own 327 prose files), PDF concealment with an honest coverage declaration, and the **distilled output**, since `knowledge/` pages are prose and prose is exempt, so a model's restatement of a foreign document would otherwise re-enter unscreened. Out-of-line storage is **decided in principle, blocked in practice** on one named question: can content be resolved and hashed from a manifest entry whose bytes are not in the object database? **Q3 — `log.md` is derived; the append-only reading is declined.** `render_log` is a pure function and `assemble` writes the file, but only `if !log.is_empty()`, and both callers pass `&[]` — so no published bundle contains it. The feed is the per-concept commit dates ADR-0021 already resolves for `verified.at`, which exist for the **authored layer only**; an ingest-time log is refused as the `SystemTime::now()` non-determinism ADR-0021 already declined. **Measured facts recorded in the body, several of which corrected this ADR's own drafts:** every extraction input path comes from git's own enumeration of one tree, so out-of-tree bytes cannot be enumerated — but `.gitignore` is **inert over a tracked path** (a committed-then-ignored `raw/` is scanned already, and `git check-ignore` reports it as not ignored), and the derived extractor **follows tracked symlinks** on the worktree overlay, storing out-of-repo bytes under a repo-relative key — a live defect in `main`, reported not fixed, absent from the committed path a bundle renders from. `block` **is** reachable for PDF input through invisible-codepoint concealment as far as the screen goes, though survival through `pdf_extract` is unverified and needs a fixture; the presentation-shaped and PDF-native classes are definitely undetected. Also: the live corpus this was framed against holds no `raw/` and no PDFs — the decision is taken before several GB enter a history that cannot forget them. |

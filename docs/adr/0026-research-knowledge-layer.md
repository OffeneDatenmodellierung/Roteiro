---
Title: A research knowledge layer — raw sources in, an authored wiki out
Space: ARCH
Parent: ADRs

# ADR-specific metadata (unknown keys are ignored; used for indexing/search)
type: adr
adr-id: "0026"
status: Draft                       # Draft | For Review | Accepted | Rejected | Superseded
architectural-significance: HIGH    # SOFT | LOW | MEDIUM | HIGH | VERY HIGH
domain: Developer Tooling
decision-makers: ["The Roteiro Project Team"]
superseded-by:
version: "0.1"
last-modified: 2026-09-10
confluence-url:
---

# ADR-0026: A research knowledge layer — raw sources in, an authored wiki out

|  |  |
|---|---|
| **State** | Draft |
| **Architectural Significance** | HIGH |
| **Domain** | Developer Tooling |
| **Document version** | 0.1 |
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

## Open questions

1. **Does model-authored content need a fourth provenance class?** A page written
   by a model has an author, but not a human one, and calling it `authored`
   silently equates it with a reviewed ADR. [[docs/adr/0013-agent-memory-artifact-store.md]]
   stores something with the same shape and may already hold the precedent.
2. **Is `raw/` committed, ignored, or stored out of line?** This decides whether
   a bundle is reproducible from the repository alone, and it interacts with
   copyright: a repository that commits every PDF someone read is a redistribution
   question, not only a size one.
3. **What does `log.md` record?** The format reserves it and the pattern uses it
   as an append-only ingest log. Roteiro regenerates the bundle wholesale, so an
   append-only file inside it is a contradiction unless its content is derived.

## Advice Received

The direction came from the project owner, who identified the research-base use
case and the narrower MCP surface as one idea rather than two: if the graph is
reached through a skill, what remains for MCP is what a skill cannot do, and the
ingest half of that belongs in a service of its own.

## Document version history

| Version | Date | Change |
|---|---|---|
| 0.1 | 2026-09-10 | Initial draft. Records a second knowledge source — documents rather than code — and the layer split that lets a model maintain a wiki without owning the bundle it renders into. Names the conflict that decides it: the OKF bundle is a projection, emptied on every render, while the pattern being followed requires the model to own the wiki outright. Resolves it by making the model maintain the **authored** layer instead, which is committed, diffed and drift-checked. Also settles the MCP question the same week's magpie work raised: the surface is read-only and narrow, because a skill driving the CLI beat fourteen advertised tools by 15,769 bytes at no loss of capability, and an ingest tool belongs in a separate upload service. Leaves three questions open rather than assuming them: a provenance class for model-authored content, whether `raw/` is committed, and what a reserved append-only `log.md` means inside a regenerated bundle. |

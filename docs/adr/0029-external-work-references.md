---
Title: External works are cited from an authored reference register, rendered APA 7, and refused when incomplete
Space: ARCH
Parent: ADRs

# ADR-specific metadata (unknown keys are ignored; used for indexing/search)
type: adr
adr-id: "0029"
status: For Review                  # Draft | For Review | Accepted | Rejected | Superseded
architectural-significance: MEDIUM  # SOFT | LOW | MEDIUM | HIGH | VERY HIGH
domain: Knowledge Graph
decision-makers: ["The Roteiro Project Team"]
superseded-by:
version: "0.4"
last-modified: 2026-11-13
confluence-url:
---

# ADR-0029: External works are cited from an authored reference register, rendered APA 7, and refused when incomplete

|  |  |
|---|---|
| **State** | For Review |
| **Architectural Significance** | MEDIUM |
| **Domain** | Knowledge Graph |
| **Document version** | 0.4 |

## Reference

- Issue #801 — the goal, five hard constraints, and four investigation comments.
- The attribution half of [[docs/adr/0026-research-knowledge-layer.md]].
- Provenance classes: [[docs/adr/0001-build-roteiro-unified-codebase-knowledge-graph.md]].
- The record: [[crates/rto-graph/src/reference.rs#Reference]]. The renderer:
  [[crates/rto-render/src/apa.rs#reference_list]].
- The register reader: [[crates/rto-spec/src/reference.rs#parse_register]].

## Summary

A checked-in, human-curated **reference register**, `docs/references.toml`,
lists the external works this repository cites. Each entry is one
`Reference` record. The authored layer turns every entry into a `work` node
and every link from an authored document to a work's URL into an edge, so
"what does this document rest on" is a graph query. `roteiro refs` renders the
APA 7 list, and `explain` / `context` carry it on every surface. A record too
incomplete to cite is **listed as refused, with the fields that stopped it** —
never dropped, never guessed.

## Context

Issue #801 asks that an answer about this graph carry a formatted reference
list for the external works it rests on. The record type and the APA 7
renderer were built and hardened first (#807) and nothing called them. The
issue fixes five constraints, all kept here:

1. no network during scan or render, and byte-deterministic output;
2. extracted is not authored — nothing fetched or scraped is laundered into the
   authored layer;
3. refuse rather than fabricate — a record missing a required field renders no
   reference, and says why;
4. publication date and access date stay distinct types;
5. a commit timestamp is never a publication date.

The issue's own measurement: about fifty distinct works are mentioned, about
eighteen carry a locator, and **three** were complete enough to cite without a
lookup. The formatter is the small part; the register is editorial work, and
this decision is mostly about keeping that work honest.

## Decision makers

The Roteiro Project Team.

## Recommended option

### 1. The register is authored data, read by the authored layer

`docs/references.toml` holds `[[reference]]` tables that deserialise straight
into `Reference` — the record type *is* the schema, so the register cannot
accept a shape the renderer has never heard of, and unknown fields are refused.
It is read by the one authored-layer classification
([[crates/rto-spec/src/layer.rs#authored_docs_from]]), by path, exactly as
`docs/adr/` is, and therefore obeys the same path policy and the same three
callers (`check`, the read-only `check` tool, and the graph arm at a rev).

An entry that does not parse is a `malformed-reference` violation **naming the
entry**; the others still load. A register that skipped it would render a list
that is quietly short.

### 2. Each work is an authored `work` node

A new `NodeKind::Work` (`work:<id>`), `authored`, carrying the whole record in
`meta.reference` and its title and URL in `meta.content` for search. Every read
surface renders from the graph, not from the file, so there is one copy of the
record at query time.

`rto-*` crates are internal (AGENTS.md); the variant is additive and ships as a
minor bump.

### 3. A citation edge is `authored`, and needs no `EXTRACT_VERSION` bump

A document cites a work by linking its URL. The link is read with the one link
scanner, [[crates/rto-graph/src/text.rs#markdown_links]] (inline, external,
outside code fences) — no new extractor — and matched against the register's
URLs **by exact string**. A match becomes an `authored` `references` edge from
the document's node (`adr:…`, `site:…`, `blueprint:…`, or `file:…` for other
markdown) to the work.

Why `authored` and not `derived`: the edge is a join of two sources, and one of
them is the register. A derived fact is a pure function of one blob's
`(path, blob id, bytes)` and cannot see a second file, so it could not produce
this edge without becoming a different kind of fact. The authored layer already
does exactly this — an ADR's `[[path#Symbol]]` link is read out of its bytes,
resolved against the graph, and woven in as an `authored` edge by
[[crates/rto-spec/src/check.rs#validate_layer]]. A citation is the same shape: a
human wrote the link, a human registered the work. The derived extraction
output does not change, so `EXTRACT_VERSION` does not move.

Exact match, not fuzzy: a normalised or similarity match would be an `inferred`
edge dressed as an authored one. A link no work claims is not drift — most
links are not citations.

### 4. `roteiro refs`, and `references` on `explain` / `context`

`roteiro refs` with no key renders the whole register; with a key, the works
that node rests on (one hop). The output lists APA entries, then every refused
record with its missing fields, then any in-text ambiguity (`2026a`/`2026b` is
reported, not invented — [[crates/rto-render/src/apa.rs#Ambiguity]]).

`explain` and `context` carry the same list in a `references` field on the CLI
(`--json`), the MCP server and the served tools, through one helper,
[[crates/rto-render/src/cited.rs#with_references]]. The field is **additive**
under `roteiro.query/v1` and omitted when a node cites nothing, so such a node
serialises byte-for-byte as before. No schema version bump.

### 5. The register holds only what the documents attest

Populated with what this repository's documents state:

- **The OKF specification** — ADR-0021 gives the URL, v0.2, "published 12 June
  2026", and Google Cloud as publisher. It renders.
- **`okf-core` and `okf-validator` 0.2.6** — present, and **refused**:
  `docs/OKF_BUNDLE.md` gives "0.2.6 (2026-08-27)" without saying whether that is
  a publication or an access date, so `published` is `unknown`. Settling it will
  surface the `2026a`/`2026b` collision, which is reported rather than resolved.
- **llama.cpp** — absent. Its only date is a commit timestamp (#799).

### 6. The OKF bundle carries the works as `sources[]`, with footnotes

`render okf` gives every concept whose node cites works a `sources[]` entry per
work, after its own file: `resource` (the work's URL, or DOI URL; a work with no
locator points at its register entry rather than at an invented URL), `id` (the
register id), `title`, and `author` (the authors as APA alphabetises them,
joined with `; `). The body gains a `## References` section with one `[^id]`
footnote per source — the APA 7 entry, or the refusal naming what is missing.
Works get their own `references/` section. See
[[crates/rto-render/src/okf.rs#Source]].

That is what lets the three attribution checks in
[[crates/rto-render/src/okf/conform.rs]] fire at all: before this, our output
carried no `sources[].id`, no `author` and no footnote, so none of
`check_attribution`, `L5` or `L6` could ever trigger on it. Now the output is
clean of the first two because it is right — each source is cited by its
footnote — and `L6` reports, at `info`, that an APA author such as
`Google Cloud` is not an OKF actor (`human:`/`process:`/`<producer>/<version>`).
That is accurate, and it is left visible rather than worked around: a work's
author is not the actor who wrote the concept.

**Compatibility.** `id`, `title` and `author` are §5.1 fields, modelled by
`okf-core`'s `Source` and round-tripped by its `to_yaml_value`. `okf-validator`
0.2.6 (read from its source, not run) checks `sources` for shape, a required
`resource` and **duplicate `id`** (V13), footnote attribution (V16) and
derivation cycles (V18), and lints unused sources and the actor convention. None
of those rejects the added keys; V13 cannot fire because register ids are unique;
V18 cannot fire because a work's resource is an external URL, not a concept.
Resource-only entries render byte-for-byte as before, and the bundle stays a
pure function of the graph.

**Import keeps a peer's bibliography.** The reader used to keep only
`resource`. It now also records every entry's `id`, `title` and `author` under
`meta.okf.source_entries` — only when at least one entry carries one, so a
resource-only bundle imports exactly as before.

### 7. Enrichment is an explicit, consented command that writes proposals

`roteiro refs fetch <id> | --all --allow-download` reads a work's URL and writes
what the page states into the register file as a `[[proposal]]`, below a marker
line the tool owns, for a person to review and move into the entry by hand. See
[[crates/roteiro/src/refs_fetch.rs#run]] and
[[crates/rto-spec/src/reference.rs#Proposal]].

- **Shaped like `security prefetch`.** An explicit subcommand behind its own
  feature (`refs-fetch`, on by default, taking the `ureq` the default build
  already has), and no socket without `--allow-download`, given every time. It
  lives in the `roteiro` binary: `rto-graph` still cannot reach the network.
- **Never during a scan or render, and never a graph fact.** A proposal is not
  an entry. `parse_register` validates proposals (an id the register lacks, an
  impossible retrieval day) and returns none of them, so nothing fetched reaches
  a `work` node, a reference list or the bundle until a person moves it. That is
  also why this needs no amendment to the remote-output rule ADR-0019 §5 set
  ("remote output is not a graph fact"): it still is not one.
- **The read date is an access date.** `retrieved` is an `AccessDate`, the one
  clock read in the feature, recorded as exactly what it is.
- **What the page does not state is recorded as not stated.** Each value is
  `Found::Stated` or `Found::NotStated`, which is a fact about the page read,
  not `Unknown` (nobody looked) or `AbsentFromWork` (the work has none). Moving
  it into the entry forces a person to decide which of those it is. A
  publication date is taken only from explicit metadata
  (`citation_publication_date`, `article:published_time`, `DC.date.issued`, …),
  at the precision stated, and never from a `Last-Modified` header, a commit or
  a copyright line.
- **The register's human half is untouched.** Everything above the marker is
  returned byte-for-byte, comments included. Only the proposals section below
  it is regenerated.

## Options considered + consequences

**A derived `url:` node per external link, joined to the work at query time.**
Keeps the edge derived, at the cost of an `EXTRACT_VERSION` bump, a node per
URL across the tree, and a two-hop query for "what does this rest on". Rejected:
it moves the join, it does not remove it.

**Carry the raw records on `Explanation` / `NodeContext` in `rto-graph`.**
Rejected: the rendering lives in `rto-render` and `rto-graph` cannot reach it,
so every surface would still render on its own, and the cached context bundle
would need a compatible-deserialisation story for a field it does not need.

**Use `NodeKind::Other("work")`, as blueprints do.** Workable; rejected for a
first-class variant because the readers match on the kind, and a token string
spelled in two crates is the drift this repository keeps closing.

## Consequences

- URI autolinks (`<https://…>`) cite exactly as inline links do: the shared
  scanner reports them as `LinkKind::Autolink`, so ADR-0021's and ADR-0022's
  citation of the OKF spec makes an edge, as `docs/OKF_BUNDLE.md`'s inline link
  already did. Email autolinks are not read.
- Citations hang from a document's node, not from its section.
- The register needs maintaining; a refusal is its worklist.

## Status

**For Review.** Step 1 (v0.1): register, graph, `refs`, and the `references`
field. Step 2 (v0.2): projection into the OKF bundle's `sources[]`. Step 3
(v0.3): `refs fetch`, an explicit, consent-gated enrichment command that
proposes values for a human to commit.

## Version history

| Version | Date | Change |
|---|---|---|
| 0.1 | 2026-11-11 | Initial. Register, `work` nodes, authored citation edges, `roteiro refs`, and `references` on `explain`/`context`. |
| 0.2 | 2026-11-11 | §6: works projected into OKF `sources[]` with `id`/`title`/`author` and a `[^id]` footnote; import keeps a peer's attribution keys. |
| 0.3 | 2026-11-11 | §7: `roteiro refs fetch --allow-download` writes fetched values as `[[proposal]]`s below a tool-owned marker, with an access date and explicit `not-stated`; never a graph fact. |
| 0.4 | 2026-11-13 | Consequences: autolinks are now read by the shared scanner and cite like inline links, so ADR-0021's citation of the OKF spec makes an edge. |

---
Title: Documents are decoded, screened, and extracted only where somebody said yes
Space: ARCH
Parent: ADRs

# ADR-specific metadata (unknown keys are ignored; used for indexing/search)
type: adr
adr-id: "0025"
status: Draft                       # Draft | For Review | Accepted | Rejected | Superseded
architectural-significance: MEDIUM  # SOFT | LOW | MEDIUM | HIGH | VERY HIGH
domain: Developer Tooling
decision-makers: ["The Roteiro Project Team"]
superseded-by:
version: "0.2"
last-modified: 2026-09-03
confluence-url:
---

# ADR-0025: Documents are decoded, screened, and extracted only where somebody said yes

| | |
|---|---|
| **State** | Draft |
| **Architectural Significance** | MEDIUM |
| **Domain** | Developer Tooling |
| **Document version** | 0.2 |
| **Related** | [[docs/adr/0015-generated-media-content-artifact-store.md]] · [[docs/adr/0017-dependency-security-policy.md]] · [[docs/adr/0021-open-knowledge-format-bundle.md]] |

## Reference

Affected code: [[crates/rto-graph/src/extract.rs#pdf_content]],
[[crates/rto-graph/src/extract.rs#IngestConfig]],
[[crates/rto-graph/src/screen.rs#screen_text]],
[[crates/rto-graph/src/okf_consent.rs#screen_fingerprint]].

**Id note.** `0024` is held by an unmerged branch, so `spec scaffold` proposes it
here and cannot know better. Taken as `0025` by hand — the second instance of the
one case that command is structurally unable to get right, recorded because a
pattern is worth more than a repeated surprise.

## Summary

Roteiro already decodes text out of PDFs. It decodes nothing else binary, and —
this is the part nobody had noticed — **it screens none of what it decodes**.

Three decisions. Office documents join the formats that are decoded, on the same
path and by the same rule that already admits PDFs. **Text decoded out of a
binary** is screened before it becomes searchable content, closing a gap that is
live today rather than hypothetical — and prose is deliberately excluded, for a
reason that was measured rather than argued (see Option D). And because opening a document somebody else
wrote is the risky act, extraction per file is **a decision a person makes**, with
a switch for the repositories that would rather not be asked.

## Context

[[crates/rto-graph/src/extract.rs]] states its own membership rule at the point
where content is assembled, and it is a good one:

> Every branch here **decodes text that exists in the bytes** — that is the whole
> membership rule (ADR-0015). Prose and PDF text are parses; OCR is
> discriminative… An ASR transcript and a VLM description are neither: they are
> generated, they invent fluent text where there is nothing to read.

That line settles where document extraction belongs without needing a new
principle. A DOCX is a ZIP of XML and the text is literally in it; reading it is a
parse, not a generation. So documents are admitted by the rule already written,
and the media pipeline — which exists for the *generated* half — is the wrong
home for them.

Three forces make this worth doing now.

**PDF extraction already exists, and the case for it already carried.**
`pdf-extract` is a declared optional dependency behind the `pdf-text` feature;
[[crates/rto-graph/src/extract.rs#pdf_content]] runs it bounded at 20 MiB and
panic-guarded, degrading a hostile document to a plain file node rather than
aborting a sync. Nothing about Office formats is a harder question than the one
already answered.

**Nothing Roteiro extracts is screened.** `screen_text` has exactly two callers,
both on the OKF path: the importer and the viewer. Text that OCR reads out of an
image, and text `pdf_content` reads out of a PDF, go into `meta.content` — which
search hands to a model — without passing the screen at all. That is a live gap,
not a future one.

**A binary is the input a human reviewer cannot check by eye.** The reason a
repository's own content is not screened is that a person reviewed it. That
argument is weakest exactly here: a reviewer approving a PR containing a DOCX
reviewed a *rendering*, not the bytes, and hidden text is hidden from them too.
Extraction is what turns those bytes into something the screen can read — which
means **refusing to extract is refusing to screen**, and the inventory ADR-0024
adds is a floor rather than a ceiling.

## Decision makers

- The Roteiro Project Team

## Recommended option

Decode Office documents on the extract path, screen the text that path decodes
**out of a binary**, and gate extraction per file behind a remembered answer.

## Options considered + consequences

### Option A — put document extraction in the media pipeline

Rejected, and the rule that rejects it is already written down. ADR-0015 split
`meta.content` from generated media precisely on decoded-versus-generated, and
`extract.rs` enforces that split in a comment at the assembly point. A document's
text is decoded. Filing it with transcripts and VLM descriptions would put a parse
in the store built for confabulation, and would mean a `roteiro media build` was
required before a spreadsheet could be searched.

### Option B — extract documents behind a global switch, no per-file question

Rejected. A single switch makes the choice all-or-nothing across a repository,
and the risk is per file: the cost of parsing one hostile document is not reduced
by having agreed to parse a thousand safe ones. It also gives an operator nowhere
to record "this one, no" short of deleting the file.

### Option D — screen everything the path decodes, prose included

Rejected, and this is what the first draft of this ADR specified. It said the
screen should run "after every branch… so it covers prose, PDF, OCR and any
format added later without a second decision". One call site, no exceptions, and
it reads well.

**Measured over this repository's own 327 prose files before implementing it:**

| file | class | admitted |
|---|---|---|
| `crates/rto-llama/Cargo.toml` | `model-directive` (chat-template-marker) | **nothing** |
| `crates/rto-serve/src/tools.rs` | `model-directive` (fake-system-marker) | **nothing** |
| `docs/REVIEW_CHECKLIST.md` | `hidden-presentation` (HTML comment) | partial |
| `crates/rto-graph/CHANGELOG.md` | `hidden-presentation` (`<div>` with hidden) | partial |

Eight files flagged, four of them meaningfully, **two losing their content
entirely** — on a repository that has never seen a peer's bundle. And they are
flagged for containing exactly what they exist to contain: `chat_template.rs` and
`tools.rs` handle chat templates and tool system prompts, so a chat-template
marker in them is the subject matter, not an attack. ADR-0024's control-token
class will make this strictly worse, because those are the tokens those files
are *about*.

The rule that fixes it is already written down in this ADR's own Context, and the
first draft simply failed to follow it:

> **A binary is the input a human reviewer cannot check by eye.** The reason a
> repository's own content is not screened is that a person reviewed it. That
> argument is weakest exactly here: a reviewer approving a PR containing a DOCX
> reviewed a *rendering*, not the bytes.

That argument distinguishes prose from binaries, and it is the whole basis for
screening the latter. Applying the screen to prose as well throws away the
distinction that justified it: a reviewer approving a Markdown file *did* read
those bytes as text, which is precisely why nothing else in the repository is
screened either.

It is also confirmed that this path never sees a peer's content — `file_node` is
called only from within `extract.rs`, which runs over the repository's own blobs
during sync, while an imported bundle goes through
[[crates/rto-render/src/okf/read.rs#read_bundle]] and its six separate screen
calls. So excluding prose here removes no protection from anything external.

### Option C — decoded, screened, and consented per file *(recommended)*

Four parts.

**Formats.** `.docx`, `.xlsx` and `.pptx` join `.pdf`. Measured against
ADR-0017 rather than asserted, because the first draft of ADR-0024 refused this
work on dependency grounds that turned out to be false:

| | |
|---|---|
| candidate crates (`calamine`, `zip`, `quick-xml`) | 25 |
| already in this lockfile | 16 |
| **net new** | **9** |
| licences among the nine | `MIT` or `Apache-2.0`, every one |
| new allow-list entries required | **none** |

For contrast, `okf-validator` was refused because one dependency produced
thirteen `cargo deny` failures across seven licence rejections and six
unmaintained advisories. This is not that.

**Screening.** The screen runs where content is assembled, before `meta.content`
is set, over **every branch that decodes text out of a binary** — PDF today, OCR
today, Office documents when they land, and anything added later.
`Screened::admit` already carries what may be stored, and `None` already means
nothing may be, so the enforcement half exists.

**Prose is not screened**, and the first draft of this ADR said it should be.
That was wrong, and measurably so — see Option D.

**The switch.** `[ingest] documents`, alongside the `prose`, `pdf` and `ocr`
toggles it sits beside. Off is a supported answer.

**The per-file answer.** Recorded per repository-relative path, and reused, so a
sync does not re-ask about a file already decided. It **lapses** when the
extracted text's screen classes change — the rule
[[crates/rto-graph/src/okf_consent.rs#screen_fingerprint]] already applies to a
peer's bundle, and for the same reason: an author editing a paragraph has not
changed what was agreed to, and a document that has started carrying a class of
finding it did not carry when the question was answered **has**.

**When there is nobody to ask** — a server, a CI job, a pipe — the document is
not extracted, it is mentioned once, and nothing is recorded. This is ADR-0021's
rule for a peer's bundle, and it holds for the same reason: a silent run must
never leave behind an answer a person did not give.

### Consequences

- **A repository will see findings it did not see before.** Screening the extract
  path is a behaviour change for PDF and OCR content that is already stored.
  Existing content is not re-screened retroactively; the first sync after this
  lands is what re-reads it.
- **Nine crates, and a new parser — but the licence and advisory gates need no
  help to see them.** `cargo audit` reads `Cargo.lock`, which lists optional
  dependencies whatever features are on, and CI runs `cargo deny --all-features
  check`, which ADR-0017 §3 decided precisely so that a feature-gated dependency
  cannot hide from it. `pdf-text` is the worked example in that decision and in
  the CI comment beside it, which makes this the least excusable place to have
  assumed otherwise.

  What neither gate measures is the thing that actually matters here: **a parser's
  behaviour on input designed to break it.** `cargo deny` checks licences and
  advisories, not robustness. That is why the bounds and the panic guard below are
  the load-bearing part of admitting a new format, and why they are stated as
  requirements rather than as implementation detail.
- **ZIP formats bring an attack surface PDF does not**: a decompression bomb.
  A size cap on the archive is not sufficient, so the extractor bounds the
  *expanded* size and the compression ratio, and refuses rather than expanding.
- **Asking has a cost.** A repository with four hundred documents must not ask
  four hundred questions on first sync; the switch is what makes that bearable,
  and the batch shape of the prompt is an implementation question this ADR does
  not settle.
- **OCR is included in the screening change**, which is wider than documents, and
  prose is excluded, which is narrower than "the extract path". The line is not
  the call site but the *input*: OCR reads out of an image a reviewer saw
  rendered, so it belongs with PDF and DOCX; Markdown is bytes a reviewer read as
  text, so it belongs with the rest of the unscreened repository.

## Implementation

1. **Screen the extract path** — one call, covering what is already decoded. It
   is first because it is the live gap and it needs no new dependency.
2. **The per-file record and its lapse rule**, reusing `screen_fingerprint`.
3. **`.docx` / `.pptx`** — ZIP + XML, with the expansion bounds.
4. **`.xlsx`** via `calamine`, last: a spreadsheet's text is the least prose-like
   of the four and wants its own thought about what "the text" even is.

## Advice Received

The prompt came from the observation that Roteiro already parses images, so
parsing documents is the same kind of act rather than a new one — with the
suggestion that it be switchable, and asked per file. Both are taken.

Checking it also corrected ADR-0024, which had refused this work on the grounds
that a PDF parser was dependency weight yet to be paid and that the media pipeline
was where it would belong. Roteiro had been extracting PDF text the whole time.

The wider form — that refusing to extract is refusing to screen — is the
consuming-side version of the point [`okf-guard`](https://github.com/darshanNhb/okf-guard)
made in issue #723, which was that a conformant bundle can cite a document nobody
has read.

## Document version history

| Version | Date | Notes |
|---------|------|-------|
| 0.1 | 2026-09-03 | Draft. Office documents join the formats the extract path decodes, admitted by the membership rule ADR-0015 already wrote (decoded, not generated) rather than by a new principle. Everything that path decodes is **screened** before it becomes `meta.content`, closing a live gap: `screen_text` had exactly two callers, both on the OKF path, so OCR'd and PDF text reached a model unscreened. Extraction is gated per file by a remembered answer that lapses on a screen-class change, reusing `screen_fingerprint`, with `[ingest] documents` for repositories that would rather not be asked and ADR-0021's non-interactive rule when there is nobody to ask. Dependency cost measured rather than asserted: 9 net-new crates, every licence already on the allow-list, no new entries — against the thirteen `cargo deny` failures that refused `okf-validator`. |
| 0.2 | 2026-09-03 | **Prose is no longer screened, and the first draft's rule was wrong by measurement.** It said the screen should run "after every branch… so it covers prose, PDF, OCR and any format added later". Run over this repository's own 327 prose files before implementing it, that flags eight and **withholds the content of two entirely** — `rto-llama/Cargo.toml` and `rto-serve/src/tools.rs`, both for a chat-template marker, which is the subject those files exist to handle. ADR-0024's control-token class would make it strictly worse. The rule that fixes it was already in this ADR's own Context and the draft failed to follow it: a binary is the input a human reviewer cannot check by eye, and that is the entire basis for screening it — a reviewer approving a Markdown file *did* read those bytes as text, which is why nothing else in the repository is screened either. The line is the **input**, not the call site: OCR reads out of an image a reviewer saw rendered, so it stays; Markdown does not. Also confirmed that this path never sees a peer's content — `file_node` is called only from `extract.rs` over the repository's own blobs, while an imported bundle goes through `read.rs` and its six separate screen calls — so excluding prose removes no protection from anything external. |

---
Title: Remote model tier — an explicitly consented egress path, and the promises it changes
Space: ARCH
Parent: ADRs

# ADR-specific metadata (unknown keys are ignored; used for indexing/search)
type: adr
adr-id: "0019"
status: Accepted                    # Draft | For Review | Accepted | Rejected | Superseded
architectural-significance: VERY HIGH  # SOFT | LOW | MEDIUM | HIGH | VERY HIGH
domain: Inference
decision-makers: ["The Roteiro Project Team"]
superseded-by:
version: "1.1"
last-modified: 2026-08-17
confluence-url:
---

# ADR-0019: Remote model tier — an explicitly consented egress path, and the promises it changes

| | |
|---|---|
| **State** | Accepted |
| **Architectural Significance** | VERY HIGH |
| **Domain** | Inference |
| **Document version** | 1.1 |

## Reference

Decides whether Roteiro may call a **hosted model**, and under what conditions.
It is the prerequisite for Build Plan V2 Stage 34 and it exists because that
stage cannot be built without changing promises made elsewhere:
[[docs/adr/0006-local-model-serving.md]] on what leaves the machine,
[[docs/adr/0007-configuration-file.md]] on how configuration layers, and
[[docs/adr/0003-pluggable-embedding-models.md]] on consent for model acquisition.
Storage of any remote output is governed by
[[docs/adr/0015-generated-media-content-artifact-store.md]], whose producer
identity this ADR qualifies.

This is the first capability in Roteiro that sends repository content off the
machine. That is the whole of its significance, and it is why this is an ADR and
not a feature flag.

## Summary

Roteiro **may** call a hosted model, as an optional, default-off capability, but
only through a gate the user has deliberately opened. Three specific promises are
amended rather than quietly outgrown, and the amendments are the substance of
this decision:

1. **ADR-0006's "nothing leaves the machine" is scoped**, explicitly, to serving.
2. **ADR-0007's precedence inverts for one key**: the committed project file may
   **deny but never grant** egress.
3. **Principle 10 is exempted** for this one capability, because a remote call
   can be neither digest-pinned nor prefetched.

And one thing is decided that looks like a routing question and is not: **the
local→remote edge is a consent gate, not a model-selection decision.**

## Context

Every model Roteiro uses today is a local single-file GGUF, pinned by URL and
SHA-256, acquired once with explicit consent (ADR-0003) and thereafter used
offline. `roteiro serve` exposes only installed models and never downloads.
`rto-graph` — where extraction and the graph live — structurally cannot reach the
network: `gix` is pinned `default-features = false` specifically to exclude
transports, and the model downloader takes its transport as a caller-supplied
closure.

That is a coherent position, and this ADR does not abandon it. It carves a
single, named exception and says exactly what it costs.

### A terminology collision to avoid first

**"Online mode" is already taken**, and means nearly the opposite of what this
capability does. The website defines it as:

> **Online mode — richer inference with local models.** Pull a real embedding or
> generative model once (with consent), then run everything locally — the
> "online" is a one-time, explicit download, after which inference is offline
> again.

Reusing that phrase for *"may call a hosted API at use time"* would make the
existing documentation actively misleading. This capability is called the
**remote model tier**, and the words `remote` or `hosted` are used throughout.
"Online mode" keeps its existing meaning.

## Decision makers

The Roteiro Project Team.

## Recommended option

Adopt an optional remote model tier, default-off, under the conditions below.

### 1. The local→remote edge is a consent gate, not a routing decision

The cost asymmetry is not symmetric. Mis-routing among local models wastes
tokens. Mis-routing *outward* sends source off the machine for a reason nobody
can inspect afterwards.

Therefore **consent must not be probabilistic**. A classifier may not decide that
a request leaves the machine, at any model quality. Model *resolution* may be as
clever as it likes among local models (Stage 33); the edge is a boolean the user
opened.

This also disposes of the "route to a frontier model when local can't help"
framing. That sounds like a routing rule and is actually a consent boundary
wearing one. If escalation is wanted, it must be a **deterministic, recorded**
check of the local result — empty output, no tool call after `MAX_ROUNDS`, below
a length floor — evaluated *after* the local attempt, with the measured value
recorded. Never a prediction made before it.

### 2. Reachability must not be probed

Online-ness is not observable, and **must not be made observable**. A reachability
probe *is* egress: a DNS lookup leaks the query to a resolver, and doing it to
decide whether egress is permitted inverts the gate.

The correct proxy is **policy, not measurement** — the user said yes. That is also
the deterministic answer, where a probe is not.

### 3. Consent: the project file may deny, never grant

ADR-0007 establishes:

> **CLI flag > project `roteiro.toml` > user `~/.roteiro/config.toml` > built-in
> default.**

For every other key that is right. For this one it is **inverted**, and the
inversion is the point: `roteiro.toml` is **committed and shared by design** — the
ADR's own words are "committed — so a team shares the same, reproducible
settings". A merged line in a shared file authorising egress on every teammate's
machine is not consent; it is consent by pull request, granted by someone else,
noticed by nobody.

So, for the remote-enable key only:

| Layer | May deny | May grant |
|---|---|---|
| Built-in default | denied by default | — |
| Project `roteiro.toml` | **yes** | **no** |
| User `~/.roteiro/config.toml` | yes | **yes — necessary, not sufficient** |
| Invocation (flag, or a TTY prompt) | yes | **yes — necessary, not sufficient** |

**Both** the user layer and the invocation must grant. Neither alone suffices.
The user layer opts *the human* in; the invocation opts *the run* in.

A project may still switch it off for everyone — a locked-down repository is a
legitimate thing to express, and denial has none of the problems of grant.

This deviation must be stated in ADR-0007 itself, not only here, because a reader
of that ADR will otherwise apply the general rule and be wrong.

### 4. What may be sent — decided here, not deferred

Deferring this is how an egress path ships before its guard.

**What the graph holds is narrower than it looks.** Function bodies are not
stored. `meta.content` is capped at 1,500 characters and is populated only from
prose files, doc-comments, PDF/OCR text and audio summaries; extraction asserts
that a `.rs` file node carries no `meta.content`. So a graph-derived prompt
carries **symbol names, headings and topics** — not source text.

**But there is no redaction chokepoint on a prompt.** Extraction redacts
secret-*looking* config values before persistence precisely because the store is
exportable. That mechanism does not apply here, and it is weaker than it sounds
even where it does apply: `is_secret_key` matches **key names only**, against ten
needles (`secret`, `password`, `passwd`, `passphrase`, `token`, `apikey`,
`credential`, `privatekey`, `accesskey`, `pwd`), with **no inspection of values**.
`DATABASE_URL=postgres://user:hunter2@host` matches none of them.

Therefore:

- A remote request must be assembled from an **explicit allow-list** of fields,
  never "whatever the local path happened to build".
- The **exact payload must be inspectable before it is sent** — a dry-run that
  prints what would leave, and a recorded copy of what did.
- Symbol names are commercially sensitive for some users even without bodies, and
  the documentation must say so rather than implying that "no source is sent"
  means "nothing identifying is sent".

### 5. Remote output is not a graph fact

It is not a pure function of `(path, blob id, bytes)`, so it acquires no node, no
edge, no `Provenance` variant, and never appears in `export_factset`. This is the
fourth instance of the rule already applied to analyzer findings
([[docs/adr/0012-analyzer-findings-artifact-model.md]]), agent memory
([[docs/adr/0013-agent-memory-artifact-store.md]]) and generated media
([[docs/adr/0015-generated-media-content-artifact-store.md]]).

**One thing does not transfer, and must be handled.** ADR-0015's `Producer` is not
a label but a *verifiable identity*, folding a `model_digest` **as pinned in the
registry** into a canonical id — which is what makes re-describing with different
weights a new record rather than a silent overwrite. A hosted model has no digest
that can be computed. **A vendor model string is a mutable pointer**: the weights
behind it can change while the name does not, and Roteiro cannot detect it.

So if remote output is ever persisted, it carries an explicit
`ProducerTrust::{PinnedDigest, VendorAsserted}`, and a `VendorAsserted` record
states on its face that its identity is a **claim**. Doing that honestly is worth
more than the feature.

### 6. Principle 10 is exempted, explicitly

Build Plan V2 principle 10 reads:

> **Offline-capable, not "offline".** Optional capabilities may require
> pre-provisioned assets; they must be digest-pinned, explicitly prefetched, and
> must fail with a named, actionable error rather than fetching implicitly or
> silently degrading.

A remote call is fetching by definition. It cannot be digest-pinned and cannot be
prefetched, so this is the first capability the principle can only **exempt**,
never satisfy.

The **second half still binds, and binds harder**: with the tier enabled and no
network, Roteiro must fail with a named, actionable error naming the endpoint —
never silently degrade, and never fall back to a local model without saying so.
An unannounced downgrade is the failure mode this ADR most needs to prevent,
because it produces a different answer with no signal that anything changed.

## Options considered + consequences

| Option | Verdict |
|---|---|
| **Adopt, gated as above** | **Recommended.** Capability without a silent egress path. |
| Adopt with a learned router deciding the edge | **Rejected.** Consent must not be probabilistic, and a weight vector cannot answer *why did this leave?* |
| Adopt with project-level grant (normal ADR-0007 precedence) | **Rejected.** Authorises egress for a whole team from a merged line. |
| Do not adopt | **Viable.** Costs nothing already promised; the local tier is unaffected. |

## Consequences

- **A promise changes.** *"Nothing leaves the machine"* stops being true of
  Roteiro-as-a-whole and becomes true of Roteiro-as-shipped-and-configured. The
  README, the website and ADR-0006 must all say the scoped thing.
- **Stage 34 must land before Stage 27.** Stage 27 re-audits every "offline"
  claim; adding a remote tier afterwards reopens an audit that had just closed.
- **`rto-graph` stays network-free.** The tier lives in its own crate behind its
  own feature; nothing in extraction or the graph gains a transport.
- **Default-off, and it stays default-off.** No release may flip it, and enabling
  it always requires the user layer *and* the invocation.
- **Every remote call is recorded** — endpoint, model string, `ProducerTrust`,
  timestamp — so *"what left this machine, and when?"* is answerable after the
  fact rather than reconstructed.
- **`cargo deny` and ADR-0017 apply to whatever HTTP client it uses.** `ureq` is
  already in the tree, so this need not add a new dependency closure.

## Status

**Accepted** (2026-08-17), and **unbuilt** — a departure from this repository's
habit, worth stating rather than leaving to be noticed. Every ADR accepted before
this one was accepted alongside working code. This is a decision about a
capability that does not exist yet, accepted so Stage 34 has a settled contract to
build against rather than discovering its consent model halfway through.

What that means in practice: the *decision* is not open for re-litigation, but
nothing here has been proved by an implementation. Where building Stage 34 shows a
clause to be unworkable, that is an amendment to this ADR with a version-history
row — never a quiet deviation in code.

## Version history

| Version | Date | Change |
|---|---|---|
| 1.1 | 2026-08-17 | **Accepted.** No content changed; Stage 34 is unblocked. |
| 1.0 | 2026-08-17 | Initial. Written to unblock Stage 34. |

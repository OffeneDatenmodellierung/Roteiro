# `--json` output schemas

Most Roteiro reporting commands accept `--json` for machine consumption (some —
`init`, `render`, `load`, `spec scaffold` — have no JSON form). The **graph-data**
outputs carry a top-level `schema` tag of the form `roteiro.<name>/vN`; these are
the stable, versioned contracts and are **frozen for v1.0**. Operational
summaries (sync/check/import/infer/config/model) are stable in shape but untagged
(see below).

## Compatibility policy

- **Within a major version** (`/vN`), changes are **additive only** — new fields
  may appear; existing fields keep their name, type, and meaning. Consumers must
  ignore unknown fields.
- A **breaking change** (removing/renaming a field, changing a type or meaning)
  **bumps the version** (`/v1` → `/v2`); the old shape is not silently reused.
- **Deprecation, not surprise.** A field slated for removal is first documented as
  deprecated (kept and populated as before) within its current major version, and
  only dropped at the next `/vN` bump. There is no silent removal.
- The schema-tag strings are pinned by a freeze test, so a version change is a
  deliberate, reviewed edit — caught in CI, never accidental.

### What is covered — and what is not

The contract is the **presence, name, type, and meaning of documented fields**
under a given `schema` tag. It deliberately does **not** cover:

- **Field ordering** within an object, or JSON whitespace/pretty-printing.
- **Human-readable text**: the non-`--json` output of any command, log lines, and
  the exact wording of `message`/error strings (their *presence* is stable; their
  prose is not a parsing target).
- **Ordering of array elements** unless a field's docs state an explicit sort
  (e.g. a list documented as sorted by `path`). When in doubt, sort on the
  consumer side.
- **New enum values**: a documented string field (e.g. a node `kind`, a review
  `status`) may gain new values within a major version — treat unknown values as
  a safe "other", don't hard-fail.

### For consumers

- **Read the `schema` tag** and branch on its `/vN`; treat a higher `N` than you
  know as "handle the fields you recognise, ignore the rest."
- **Ignore unknown fields** rather than rejecting the document.
- **Don't depend on field order or whitespace.** Parse structurally.
- Prefer the tagged graph-data schemas for automation; treat the untagged
  operational summaries as stable-within-a-release (see below).

## Versioned schemas (frozen)

Each tag is a `const` in the code, so the emitter and the freeze test share one
source of truth:

| Schema tag | Const | Emitted by | Payload |
| --- | --- | --- | --- |
| `roteiro.query/v1` | `rto_graph::SCHEMA` | `query`, `query --kind`, `context`, `path`, `debt`, `duplicates` | The query surface: an explained node, a node context bundle, a kind listing, a path, a debt report, and the duplicate report (all share the query schema). |
| `roteiro.review/v1` | `review::REVIEW_SCHEMA` (in the `roteiro` crate) | `review` | The graph-grounded review of a change (changed files with per-symbol context, authored-layer drift, blast radius). |
| `roteiro.graph/v1` | `rto_graph::ARTIFACT_SCHEMA` | `export` (and consumed by `load`) | The portable, content-addressed graph artifact (`schema`, `tree`, `facts`). |
| `roteiro.spec/v1` | `rto_spec::SPEC_SCHEMA` | `spec context`, `spec scaffold` | Graph-grounded spec/blueprint authoring context and skeletons. |
| `roteiro.oracle/v1` | `rto_graph::ORACLE_SCHEMA` | `import codegraph` | The codegraph validation-oracle comparison report. |

## Operational summaries (stable shape, untagged)

These `--json` outputs are human-oriented run summaries with a **stable field
shape**, but do not (yet) carry a `schema` tag:

- `sync --json` — `SyncReport` (tree id, blob/cache counts, node/edge totals).
- `check --json` — `CheckReport` (ADR/link/annotation counts, violations).
- `infer --json` — the inference run summary (suggested-edge count; feature-gated).
- `import lat|graphify --json` — the importer's migration report.
- `config --json` — the effective merged configuration.
- `model list --json` — the model registry.

The same **additive-within-a-release** promise applies to them: within a released
**Roteiro major version** (semver, not a schema `/vN`) their documented fields
keep their name, type, and meaning, and only grow. Giving them explicit versioned
`schema` tags is a small, additive follow-up; until then, pin to a Roteiro major
version if you parse them.

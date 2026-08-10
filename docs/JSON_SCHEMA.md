# `--json` output schemas

Every Roteiro command supports `--json` for machine consumption. The
**graph-data** outputs carry a top-level `schema` tag of the form
`roteiro.<name>/vN`; these are the stable, versioned contracts and are **frozen
for v1.0**. Operational summaries (sync/check/import/config/model) are stable in
shape but untagged (see below).

## Compatibility policy

- **Within a major version** (`/vN`), changes are **additive only** — new fields
  may appear; existing fields keep their name, type, and meaning. Consumers must
  ignore unknown fields.
- A **breaking change** (removing/renaming a field, changing a type or meaning)
  **bumps the version** (`/v1` → `/v2`); the old shape is not silently reused.
- The schema-tag strings are pinned by a freeze test, so a change is caught in CI.

## Versioned schemas (frozen)

| Schema tag | Emitted by | Payload |
| --- | --- | --- |
| `roteiro.query/v1` | `query`, `query --kind`, `context`, `path`, `debt`, `infer`, `duplicates` | The query surface: an explained node, a node context bundle, a kind listing, a path, a debt report, and the inference/duplicate reports (all share the query schema). |
| `roteiro.review/v1` | `review` | The graph-grounded review of the working-tree change (changed files, per-symbol context, drift, blast radius). |
| `roteiro.graph/v1` | `export` (and consumed by `load`) | The portable, content-addressed graph artifact (`schema`, `tree`, `facts`). |
| `roteiro.spec/v1` | `spec context`, `spec scaffold` | Graph-grounded spec/blueprint authoring context and skeletons. |
| `roteiro.oracle/v1` | `import codegraph` | The codegraph validation-oracle comparison report. |

## Operational summaries (stable shape, untagged)

These `--json` outputs are human-oriented run summaries with a **stable field
shape**, but do not (yet) carry a `schema` tag:

- `sync --json` — `SyncReport` (tree id, blob/cache counts, node/edge totals).
- `check --json` — `CheckReport` (ADR/link/annotation counts, violations).
- `import lat|graphify --json` — the importer's migration report.
- `config --json` — the effective merged configuration.
- `model list --json` — the model registry.

Adding versioned `schema` tags to these is a small, additive follow-up; until
then, treat their field shape as stable within a major release of Roteiro.

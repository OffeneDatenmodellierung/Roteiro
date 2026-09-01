# Upstream OKF bundles, vendored as interoperability fixtures

## What these are

Two of the four knowledge bundles published in the **Open Knowledge Format**
specification repository, kept here so that Roteiro's OKF reader is tested
against markdown somebody else wrote.

| Source | |
|---|---|
| Repository | <https://github.com/GoogleCloudPlatform/open-knowledge-format> |
| Path upstream | `bundles/acme_retail`, `bundles/ga4` |
| Commit | `ad30107c31c06aec8a7d5636e0d1058118604e6f` (2026-08-21) |
| Licence | Apache License 2.0 (`LICENSE.md` at the repository root; there is no `NOTICE` file) |
| Copyright | Google LLC |

## Why they are vendored rather than fetched

The property under test is *"Roteiro reads a bundle it did not write"*. A test
that fetches the bundles at run time would be a network dependency in the test
suite and would silently change meaning whenever upstream edited a file — which
is the opposite of a guard. Vendoring pins the exact bytes the assertions were
written against.

They are also the best available evidence of what the specification's own
authors consider a conformant v0.2 bundle, which is a different and stronger
thing than a fixture we wrote ourselves to match our own reading of `SPEC.md`.

## Modifications

Apache-2.0 §4(b) requires that modified files carry prominent notice of the
change. Nothing inside any file was altered — every `.md` here is byte-identical
to upstream — but the **selection** was trimmed, and that is recorded here:

- **`acme_retail`** — every `.md` file, unmodified. Its non-markdown files were
  dropped: `viz.html` (43 KB of generated visualisation) and
  `attesters/sql_equality.py`. Neither is read by an OKF consumer; the bundle
  walk takes `.md` only.
- **`ga4`** — trimmed to `index.md` and the `tables/` directory. The rest of the
  bundle (`datasets/`, `references/`, `viz.html`) exercises nothing the
  `acme_retail` fixture does not, and `tables/events_.md` is the file that
  carries the two constructs this fixture exists for (see below).
- The upstream bundles `crypto_bitcoin` and `stackoverflow` are not vendored.
  They were validated during the investigation and behave identically; carrying
  another 360 KB to re-assert the same properties is not worth the repository
  size.

No file has been reformatted, re-indented, or re-serialised. That matters more
than usual here: the whole point of these fixtures is their *authorial* YAML
style, and normalising them would delete the defect they catch.

## What each fixture is for

The two bundles were chosen because they are written in visibly different YAML
styles, and between them they cover every construct Roteiro's reader used to get
wrong:

- **`acme_retail`** uses **flow mappings** throughout —
  `generated: { by: …, at: … }` and `verified:\n  - { by: …, at: … }` — which is
  the form `SPEC.md`'s own examples use. It is also the only upstream bundle
  exercising `type: Attested Computation`, `stale_after`, `status: deprecated`,
  and per-source credibility signals.
- **`ga4`** is machine-serialised in PyYAML's default style: block sequences
  whose items sit at the parent key's own indentation (`tags:\n- analytics`),
  and **folded multi-line scalars** for `description`.

Roteiro's reader originally hand-parsed a line-oriented YAML subset shaped like
its own writer's output. Against these files it silently dropped `generated`,
`verified`, `tags` and `sources`, and *truncated* a folded `description` at its
first line — reading all nine `acme_retail` concepts as unverified when eight are
human-signed. See `okf::read::parse_frontmatter` and `tests/okf_interop.rs`.

## Not a `docs/VENDORED_DEPENDENCIES.md` entry

That register exists for a narrow purpose, stated in its own opening: native C,
C++ and assembly that `cargo audit` and `cargo deny` cannot see, so that a
vulnerability in vendored native code has somewhere it would be noticed. These
are inert markdown test fixtures. They compile to nothing, ship in no binary,
and have no advisory feed. Listing them there would dilute a register whose
value is that every line in it is a real blind spot — the same reasoning
#695 applied to the vendored chat templates.

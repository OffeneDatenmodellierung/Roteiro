# rto-okf-syntax

Syntax checking for the fenced code blocks in an [Open Knowledge Format](https://github.com/W4G1/okf) bundle.

A **pure-Rust core with every code parser behind a feature**, so a consumer
chooses what it is willing to compile. Drop-in for `okf_validator::syntax`.

## Why it exists

`okf-validator` answers two questions with one crate: *is this a conformant OKF
bundle* (frontmatter, trust tiers, provenance, links, concept ids) and *does the
code in this fenced block parse*. Only the first is what an interchange format is
for.

Getting the second bundled in is expensive. `rustpython-parser` is 61 crates and
carries `LGPL-3.0-only` through the `malachite` tree plus six unmaintained
`unic-*` advisories that say *"No safe upgrade is available"*. That fails
Roteiro's `cargo deny` policy on both counts.

And it is expensive in the wrong place. Measured over **all four bundles**
published in the specification repository at `ad30107` — 78 markdown files, 54
concepts:

| Fenced blocks | Count |
| --- | ---: |
| **SQL** | **81** |
| JSON | 1 |
| Python / JavaScript / TypeScript / Rust / Bash | **0** |

Plus one `.py` file, `acme_retail/attesters/sql_equality.py`. So 61 crates and a
copyleft licence buy one file, `oxc`'s 58 crates buy none, and SQL — which is
essentially all of it — is served by `sqlparser` at 18 clean crates.

## Features

With `--no-default-features` this crate parses no programming language, compiles
no grammar, and invokes no C toolchain — and still does everything that needs no
code parser.

| Always available | How |
| --- | --- |
| Lifting fenced blocks out of markdown | pure Rust, in this crate |
| JSON | `serde_json` |
| YAML | `okf_core::yaml` — the same parser that read the frontmatter |
| Shell quoting and bracket balance | a small conservative matcher |

| Feature | Adds | Costs |
| --- | --- | --- |
| `grammars` *(default)* | Python, JavaScript, TypeScript, Rust, Bash | tree-sitter grammars, which compile C |
| `sql` *(default)* | SQL, via `sqlparser` | ~17 crates, two compiling assembly |

`sql` is in the default set despite being the more expensive feature, because SQL
is the only language the corpus actually contains.

## SQL is not handled by a grammar, and that was measured

This crate originally used `tree-sitter-sequel` for SQL, with `sqlparser` as an
optional upgrade. Running both over the real corpus inverted that:

| Backend | SQL blocks rejected |
| --- | ---: |
| `tree-sitter-sequel` | **78 of 78** |
| `sqlparser` | 6 of 78 |

The grammar cannot parse BigQuery's backtick-quoted identifiers —

```sql
FROM `bigquery-public-data.crypto_bitcoin.inputs`
```

— which is how essentially every query in the corpus names its table. So it is
not a weaker option, it is an unusable one, and it has been **dropped rather than
kept as a fallback**: a fallback that reports 78 false errors is worse than
reporting "not checked", which `is_checkable` can say honestly.

`sqlparser`'s six failures are all documentation *fragments* — a bare `ON` join
clause, a bare `SAFE_DIVIDE(...)` — in `stackoverflow/references/`. Both real
Attested Computations pass. That limitation is pinned in
`sql_fragments_are_rejected_and_that_is_known`.

## A checker that cannot check says so

Because backends are optional, a language can be *unsupported in this build*
rather than *clean*. Conflating those makes a check vacuous.

`check_syntax` still returns `Ok` for a language it cannot parse — it is a
drop-in, and refusing a document over our build configuration would be wrong —
but `is_checkable(lang)` reports the truth and `checkable_languages()` gives the
whole set. Callers that summarise results are expected to use them.

## Accuracy, and a caution about accuracy numbers

The corpus in `tests/accuracy.rs` runs in **every** feature configuration and its
SQL cases are lifted from the published bundles rather than invented.

That distinction is the most useful thing in this README. An earlier version of
the corpus contained only SQL *we* had written, reported **zero false
positives**, and was wrong — the real corpus was at 100%. A set of cases you
thought to write down measures what you already suspected. Where a real corpus
exists, use it.

### Strictness is not automatically better either

`syn` is clean, permissively licensed and already in this workspace's lockfile,
so a `strict-rust` feature would cost nothing. It is still absent, because
`syn::parse_file` **rejects `let x = 1;`** — a bare statement is not a valid Rust
*file*, and documentation is full of fragments.
`a_strict_rust_parser_would_be_worse_here` pins that measurement.

`sqlparser` has the same weakness on the same kind of input, and is still the
right choice at 6 failures against 78.

## Scope, for callers

Only **2 of the corpus's 54 concepts** are `Attested Computation` — the ones that
declare a `runtime:` and that an agent is expected to execute. The other ~76 SQL
blocks are illustrative. Checking every fenced block in every document is
arguably the wrong scope, and is where upstream's six spurious warnings come
from. This crate answers "does this parse"; deciding *what* to ask about belongs
to the caller.

## Status

Written to be given away. The API deliberately mirrors upstream's so that
`okf-validator` could depend on this instead of four mandatory language
front-ends — keeping its functionality while letting consumers who only want
conformance opt out. Upstream describes itself as pure Rust, which is why even
tree-sitter is optional here. See [W4G1/okf#4](https://github.com/W4G1/okf/issues/4).

If upstream adopts it, this crate should be deleted and the dependency swapped.
It is versioned `0.x` independently of the rest of the workspace for that reason.

## Licence

`MIT OR Apache-2.0`, as the rest of Roteiro.

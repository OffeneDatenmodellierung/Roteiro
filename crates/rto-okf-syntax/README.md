# rto-okf-syntax

Syntax checking for the fenced code blocks in an [Open Knowledge Format](https://github.com/W4G1/okf) bundle.

A **pure-Rust core with every code parser behind a feature**, so a consumer
chooses what it is willing to compile. Drop-in for `okf_validator::syntax`.

## Why it exists

`okf-validator` answers two questions with one crate: *is this a conformant OKF
bundle* (frontmatter, trust tiers, provenance, links, concept ids) and *does the
Python in this fenced block parse*. Only the first is what an interchange format
is for.

Getting the second bundled in is expensive. `rustpython-parser` is 61 crates and
carries `LGPL-3.0-only` through the `malachite` tree plus six unmaintained
`unic-*` advisories that say *"No safe upgrade is available"*. That fails
Roteiro's `cargo deny` policy on both counts — to buy **2 of the validator's 34
checks**. None of the other parsers are the problem: `oxc_parser`, `sqlparser`
and `syn` are all clean.

The lesson is not "pick better parsers". It is that a consumer should choose what
it compiles.

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
| `grammars` *(default)* | Python, JavaScript, TypeScript, Rust, SQL, Bash | tree-sitter grammars, which compile C |
| `strict-sql` | closes the one known SQL gap | ~17 crates, two compiling assembly |

In Roteiro the default is free: the grammars were already in the lockfile for AST
extraction, so this crate adds **one** entry — itself.

## A checker that cannot check says so

Because backends are optional, a language can be *unsupported in this build*
rather than *clean*. Conflating those makes a check vacuous.

`check_syntax` still returns `Ok` for a language it cannot parse — it is a
drop-in, and refusing a document over our build configuration would be wrong —
but `is_checkable(lang)` reports the truth and `checkable_languages()` gives the
whole set. Callers that summarise results are expected to use them.

## Accuracy

tree-sitter is error-tolerant, so this is a different instrument from a compiler
front-end and the difference is measured, not assumed. Over the corpus in
`tests/accuracy.rs`, which runs in **every** feature configuration:

- **zero false positives** — no valid sample is rejected, including nested
  f-string format specifiers, private class fields, `match` statements, `...`
  placeholders, JSX, window functions and optional chaining;
- **one false negative** — `tree-sitter-sequel` accepts `SELECT FROM;`. That is
  exactly what `strict-sql` fixes.

That direction is deliberate. A checker that rejects valid code is unusable
against third-party bundles; one that occasionally accepts nonsense has merely
failed to fire — and §11 of the specification asks a consumer to read liberally.

### Strictness is not automatically better

`syn` is clean, permissively licensed and already in this workspace's lockfile,
so a `strict-rust` feature would cost nothing. It is still absent, because
`syn::parse_file` **rejects `let x = 1;`** — a bare statement is not a valid Rust
*file*, and documentation is full of fragments. The error-tolerant grammar
accepts it. `a_strict_rust_parser_would_be_worse_here` pins that measurement, so
the reason is guarded rather than remembered.

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

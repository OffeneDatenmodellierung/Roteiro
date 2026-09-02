//! What this checker actually catches, measured rather than claimed.
//!
//! tree-sitter is error-tolerant: it never refuses input, it marks what it could
//! not fit. Substituting it for a compiler front-end is only defensible if the
//! substitution is *measured*, and measured in the right direction — a checker
//! that rejects valid code is unusable against third-party bundles, while one
//! that occasionally accepts nonsense has merely failed to fire.
//!
//! So this corpus is weighted toward **valid but awkward** input: the cases a
//! naive checker gets wrong by being too strict. Nested f-string format
//! specifiers, private class fields, `match` statements, `...` placeholders,
//! window functions, optional chaining. If any of these regressed to a rejection
//! the crate would be actively harmful, which is why they are pinned here.
//!
//! Every test here runs in **every** feature configuration. A case whose
//! language this build cannot check is asserted to report clean rather than
//! skipped, so a `--no-default-features` run still proves something.
//!
//! # Where these cases come from, and why that matters
//!
//! The SQL cases are lifted from the four bundles published in the OKF
//! specification repository at `ad30107`, not invented. That distinction is not
//! pedantry: an earlier version of this file contained only SQL *we* had
//! written, reported **zero** false positives, and was wrong. Against the real
//! corpus the then-default `tree-sitter-sequel` backend rejected **78 of 78**
//! blocks, because it cannot parse `BigQuery`'s backtick-quoted identifiers — and
//! that is how essentially every query in the corpus names its table.
//!
//! A corpus of cases you thought to write down measures what you already
//! suspected. Where a real one exists, use it.

use rto_okf_syntax::{Language, check_syntax, checkable_languages, is_checkable};

/// `(tag, source, should_parse, label)`
type Case = (&'static str, &'static str, bool, &'static str);

const CORPUS: &[Case] = &[
    // ---- python ----------------------------------------------------------
    ("python", "def f(x):\n    return x + 1\n", true, "function"),
    ("python", "import os\nprint(os.getcwd())\n", true, "imports"),
    ("python", "def f(x)\n    return x\n", false, "missing colon"),
    ("python", "def f(:\n", false, "broken signature"),
    ("python", "x = (1, 2\n", false, "unclosed paren"),
    (
        "python",
        "if True:\n    pass\nelse:\n    pass\n",
        true,
        "if/else",
    ),
    ("python", "...\n", true, "bare ellipsis placeholder"),
    ("python", "def f():\n    ...\n", true, "ellipsis body"),
    ("python", "# just a comment\n", true, "comment only"),
    ("python", "x = 1  # trailing\n", true, "trailing comment"),
    (
        "python",
        "async def f():\n    await g()\n",
        true,
        "async/await",
    ),
    (
        "python",
        "match x:\n    case 1:\n        pass\n",
        true,
        "match statement",
    ),
    (
        "python",
        "f\"{a!r:>{w}}\"\n",
        true,
        "nested f-string format spec",
    ),
    (
        "python",
        "@dec\nclass C:\n    x: int = 1\n",
        true,
        "decorator + annotation",
    ),
    // ---- javascript ------------------------------------------------------
    ("js", "const x = 1;\n", true, "const"),
    ("js", "function f(a) { return a; }\n", true, "function"),
    ("js", "const x = ;\n", false, "missing rhs"),
    ("js", "function f( { \n", false, "broken params"),
    ("js", "const {a, b} = obj;\n", true, "destructuring"),
    (
        "js",
        "export default async () => { await x(); };\n",
        true,
        "async arrow export",
    ),
    (
        "js",
        "class A { #p = 1; get p() { return this.#p; } }\n",
        true,
        "private field",
    ),
    ("js", "const t = `a${b}c`;\n", true, "template literal"),
    ("js", "// comment only\n", true, "comment only"),
    (
        "js",
        "x?.y?.[z] ?? w;\n",
        true,
        "optional chaining + nullish",
    ),
    // ---- rust ------------------------------------------------------------
    (
        "rust",
        "fn main() { println!(\"hi\"); }\n",
        true,
        "hello world",
    ),
    ("rust", "fn main( { }\n", false, "broken params"),
    ("rust", "let x = ;\n", false, "missing rhs"),
    (
        "rust",
        "impl<T: Clone> Foo<T> where T: Send { fn f(&self) -> Option<&T> { None } }\n",
        true,
        "generics + where clause",
    ),
    (
        "rust",
        "async fn f() -> Result<(), E> { Ok(()) }\n",
        true,
        "async fn",
    ),
    ("rust", "// comment only\n", true, "comment only"),
    (
        "rust",
        "#[derive(Debug)]\nstruct S { a: u8 }\n",
        true,
        "derive + struct",
    ),
    // A bare statement: not a valid Rust *file*, but overwhelmingly common in
    // documentation. See `a_strict_rust_parser_would_be_worse_here`.
    ("rust", "let x = 1;\n", true, "bare let statement"),
    // ---- sql -------------------------------------------------------------
    ("sql", "SELECT 1;\n", true, "trivial select"),
    (
        "sql",
        "SELECT a, b FROM t WHERE a > 1;\n",
        true,
        "select where",
    ),
    ("sql", "SELCT a FROM t;\n", false, "misspelled keyword"),
    (
        "sql",
        "WITH c AS (SELECT 1 AS x) SELECT x FROM c;\n",
        true,
        "CTE",
    ),
    (
        "sql",
        "SELECT sum(a) OVER (PARTITION BY b ORDER BY c) FROM t;\n",
        true,
        "window function",
    ),
    ("sql", "-- comment only\n", true, "comment only"),
    (
        "sql",
        "SELECT * FROM t WHERE a = $1;\n",
        true,
        "placeholder",
    ),
    // Lifted from crypto_bitcoin/tables/inputs.md. This is the case the
    // synthetic corpus missed and the grammar backend could not read.
    (
        "sql",
        "SELECT\n  block_timestamp,\n  value / 100000000 AS value_btc\n\
         FROM `bigquery-public-data.crypto_bitcoin.inputs`\n\
         WHERE block_timestamp >= '2024-04-17 00:00:00 UTC'\n\
         ORDER BY value DESC\nLIMIT 10;\n",
        true,
        "BigQuery backtick identifiers (real corpus)",
    ),
    (
        "sql",
        "SELECT COUNT(*) AS n FROM `p.d.t` WHERE x IS NOT NULL;\n",
        true,
        "backtick identifier, minimal",
    ),
    // ---- bash ------------------------------------------------------------
    ("bash", "echo hi\n", true, "echo"),
    (
        "bash",
        "for f in *; do echo \"$f\"; done\n",
        true,
        "for loop",
    ),
    ("bash", "if [ -f x ]; then echo y; fi\n", true, "if"),
    ("bash", "# comment only\n", true, "comment only"),
    ("bash", "a=$(b | c)\n", true, "command substitution"),
    // ---- json / yaml -----------------------------------------------------
    ("json", "{\"a\": [1, 2]}\n", true, "object with array"),
    ("json", "{\"a\": }\n", false, "missing value"),
    ("yaml", "a: 1\nb:\n  - c\n", true, "mapping with sequence"),
];

/// **The invariant.** Valid code is never rejected, in any configuration.
///
/// A false positive makes the checker unusable against real bundles and is the
/// one failure mode that would make this crate harmful rather than merely weak.
/// If this fails, switch the checker off rather than patching around it.
#[test]
fn no_valid_sample_is_ever_rejected() {
    let rejected: Vec<String> = CORPUS
        .iter()
        .filter(|(tag, src, should_parse, _)| *should_parse && check_syntax(tag, src).is_err())
        .map(|(tag, src, _, label)| {
            let err = check_syntax(tag, src).unwrap_err();
            format!("  {tag}: {label} -> {err}")
        })
        .collect();
    assert!(
        rejected.is_empty(),
        "valid code was rejected:\n{}",
        rejected.join("\n")
    );
}

/// The corpus, in full, against what this build can actually do.
///
/// A language with no backend compiled in must report **clean**, not an error —
/// that is [`check_syntax`]'s documented contract.
#[test]
fn the_checker_agrees_with_the_corpus() {
    let mut wrong: Vec<String> = Vec::new();
    for (tag, src, should_parse, label) in CORPUS {
        let language = Language::from_tag(tag);
        let expected = if is_checkable(language) {
            *should_parse
        } else {
            true // no backend: must not manufacture an error
        };
        let got = check_syntax(tag, src).is_ok();
        if got != expected {
            wrong.push(format!(
                "  {tag:<8} {label:<32} expected {expected}, got {got}"
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {} corpus cases disagree:\n{}",
        wrong.len(),
        CORPUS.len(),
        wrong.join("\n")
    );
}

/// A floor, so the corpus cannot pass by checking nothing.
///
/// Without this, a build that lost every backend would satisfy both tests above
/// — each case would be "unsupported, reports clean" and the suite would be
/// green while measuring nothing. That is the exact shape of a vacuous guard.
#[test]
fn the_default_build_actually_checks_something() {
    let set = checkable_languages();
    // Three need no parser at all and must survive every configuration; the
    // grammars add four (Python, JavaScript, TypeScript, Rust); `sql` adds SQL.
    // Derived rather than hardcoded, so adding a backend updates this by
    // construction.
    let expected =
        3 + if cfg!(feature = "grammars") { 4 } else { 0 } + usize::from(cfg!(feature = "sql"));
    assert_eq!(
        set.len(),
        expected,
        "the checkable set must match the enabled features: {set:?}"
    );

    // And it must genuinely catch things, not merely decline to reject. Without
    // this the suite would stay green on a build that checked nothing.
    let caught = CORPUS
        .iter()
        .filter(|(tag, src, should_parse, _)| !*should_parse && check_syntax(tag, src).is_err())
        .count();
    let floor = if cfg!(feature = "grammars") { 8 } else { 1 };
    assert!(
        caught >= floor,
        "this build must catch at least {floor} of the corpus's broken samples; \
         caught {caught}"
    );
}

/// The known limitation of the SQL arm: it wants statements, not fragments.
///
/// `sqlparser` rejects a bare join condition or a bare scalar expression, and
/// documentation contains both — six of the 78 SQL blocks in the published
/// corpus are exactly this, all in `stackoverflow/references/`. They are correct
/// as documentation and unparseable as statements.
///
/// Pinned rather than fixed. The alternatives are worse: wrapping a fragment in
/// a synthetic `SELECT` guesses at what the author meant, and the error-tolerant
/// grammar that would accept them rejects 78 of 78 real blocks instead. Six
/// false positives against seventy-two correct answers is the better trade, and
/// naming it here keeps it a known cost rather than a surprise.
///
/// The real fix belongs to the caller, not this crate: only 2 of the corpus's 54
/// concepts are Attested Computations, and those are the ones that declare a
/// `runtime:` and must actually execute. Checking every fenced block in every
/// document is the wrong scope.
#[cfg(feature = "sql")]
#[test]
fn sql_fragments_are_rejected_and_that_is_known() {
    for (fragment, label) in [
        ("ON a.id = b.post_id\n", "bare join condition"),
        ("SAFE_DIVIDE(accepted, total)\n", "bare scalar expression"),
    ] {
        assert!(
            check_syntax("sql", fragment).is_err(),
            "{label} is expected to fail; if it now passes, sqlparser grew \
             fragment support and this limitation can be dropped"
        );
    }
}

/// The shape of real OKF SQL, which is `BigQuery`'s.
///
/// This is the case a synthetic corpus missed entirely and that decided the
/// backend: `tree-sitter-sequel` rejects it, `sqlparser` accepts it, and
/// essentially every query in the published bundles is written this way.
#[cfg(feature = "sql")]
#[test]
fn backtick_quoted_bigquery_identifiers_are_accepted() {
    let real = "SELECT\n  block_timestamp,\n  value / 100000000 AS value_btc\n\
                FROM `bigquery-public-data.crypto_bitcoin.inputs`\n\
                WHERE block_timestamp >= '2024-04-17 00:00:00 UTC'\n\
                ORDER BY value DESC\nLIMIT 10;\n";
    assert!(
        check_syntax("sql", real).is_ok(),
        "this is the dominant shape in the real corpus and must not be rejected"
    );
}

/// Why `syn` is not wired up, measured rather than asserted.
///
/// `syn` is clean, permissively licensed, and already in this workspace's
/// lockfile, so adding it would cost nothing — which makes "we left it out" a
/// claim that needs evidence rather than a preference.
///
/// The evidence is that strictness is the wrong instrument here.
/// `syn::parse_file` parses a Rust **file**, and a file may only contain items.
/// `let x = 1;` is a statement, so the strict parser rejects one of the most
/// common shapes in Rust documentation. The error-tolerant grammar accepts it.
///
/// If this test ever fails because `syn` started accepting bare statements, the
/// argument changes and the feature becomes worth adding.
#[test]
fn a_strict_rust_parser_would_be_worse_here() {
    let fragment = "let x = 1;\n";
    assert!(
        syn::parse_file(fragment).is_err(),
        "syn is expected to reject a bare statement as a file; if it no longer \
         does, reconsider adding a strict-rust feature"
    );
    assert!(
        check_syntax("rust", fragment).is_ok(),
        "this crate must accept a documentation fragment that a strict parser rejects"
    );
    // And it is still not blind: genuinely broken input is caught either way.
    if is_checkable(Language::Rust) {
        assert!(check_syntax("rust", "let x = ;\n").is_err());
    }
}

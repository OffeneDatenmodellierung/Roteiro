//! House-style conventions that are **written down and not enforced**.
//!
//! `AGENTS.md` states one plainly: *"Prefer fixing over `#[allow(...)]`; when an
//! allow is right, justify it in a comment."* Nothing checked it, so the
//! convention held by habit — and habit is what a reviewer spends attention on.
//! Corpus row `3789168273` is that attention being spent: a human noticed a new
//! `#[allow(clippy::cast_possible_truncation)]` with no justification, in review,
//! by reading.
//!
//! # Why this is a rule rather than a lint
//!
//! It is the shape issue #438 calls *cheap*: two machine-readable things that
//! contradict each other — an attribute, and the absence of a comment above it.
//! No tree-sitter, no dataflow, no second parse of the language. Every rule in
//! that tier works this way, and every rule outside it needs to understand what
//! the code *means*.
//!
//! # The warning that comes with it
//!
//! #438 is explicit: these rules are valuable **because** they return one to
//! three hits, and every generalisation to catch a hypothetical converts a
//! zero-noise rule into a noisy one. So this checks exactly what the convention
//! says and nothing adjacent — not `#[expect(…)]` (self-documenting by design,
//! and not what `AGENTS.md` names), not `#[deny]`, not attributes in general.

use crate::check::{Violation, ViolationKind};

/// Whether `line` is an attribute — `#[…]` or an inner `#![…]`.
fn is_attribute(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("#[") || t.starts_with("#![")
}

/// Whether `line` is a comment of any flavour: `//`, `///` or `//!`.
///
/// A doc comment counts. The convention asks for a justification a reader will
/// find, and `AGENTS.md` does not distinguish — several existing allows are
/// justified by the doc comment of the item they sit on, and calling those
/// unjustified would be inventing a stricter rule than the one written down.
fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with("//")
}

/// Whether an `#[allow(…)]` opening at `lines[i]` carries a justification.
///
/// Justified by a trailing comment on the attribute's own line, or by the
/// nearest line above it that is **not another attribute**.
///
/// # Skipping the attributes above is load-bearing, not tidiness
///
/// Measured against this repository: without it, `crates/roteiro/src/main.rs:81`
/// is a false positive. Its justification sits above the `#[derive(clap::Args)]`
/// that sits above the `#[allow]` —
///
/// ```text
/// // `struct_field_names`: the `log_*` prefix is intentional — these are the
/// // global `--log*` flags, and the shared prefix reads clearly at the use site.
/// #[derive(clap::Args, Debug)]
/// #[allow(clippy::struct_field_names)]
/// struct LogArgs {
/// ```
///
/// — which is the ordinary way to write it, and a rule that flags it would be
/// switched off within a week. A **blank** line above is deliberately not
/// skipped: a comment separated from the attribute is prose about something
/// else, and treating it as a justification would make the rule pass on
/// coincidence.
fn is_justified(lines: &[&str], i: usize) -> bool {
    // A trailing comment on the attribute's own line, after its closing `]`.
    if let Some((_, tail)) = lines[i].rsplit_once(']')
        && tail.contains("//")
    {
        return true;
    }
    let mut j = i;
    while j > 0 && is_attribute(lines[j - 1]) {
        j -= 1;
    }
    j > 0 && is_comment(lines[j - 1])
}

/// Every `#[allow(…)]` in `text` that carries no justification, as violations.
///
/// Rust sources only: the attribute is Rust syntax, and a `#[allow(` in prose or
/// in a JSON fixture is a mention rather than a use. `rel_path` decides, because
/// that is the one thing the caller always knows and the content never does
/// reliably — this module's own doc comment above contains the string.
#[must_use]
pub fn scan_unjustified_allows(rel_path: &str, text: &str) -> Vec<Violation> {
    // Case-insensitively, matching the extractor: it lowercases extensions, so
    // `FOO.RS` and `foo.rs` are both Rust there, and a rule that disagreed would
    // skip a file the graph considers Rust source.
    if !std::path::Path::new(rel_path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("rs"))
    {
        return Vec::new();
    }
    // The same whole-file opt-out intent-debt scanning honours, for the same
    // reason and by the same directive: a file that *enumerates* the thing being
    // detected rather than using it would otherwise report itself. This rule's
    // own end-to-end test embeds `#[allow(…)]` in a fixture string, and without
    // this the rule's first act is to flag its own test — which is precisely how
    // a zero-noise rule becomes one people switch off.
    if rto_graph::is_scan_exempt(text.as_bytes()) {
        return Vec::new();
    }
    let lines: Vec<&str> = text.lines().collect();
    lines
        .iter()
        .enumerate()
        .filter(|(_, l)| {
            let t = l.trim_start();
            t.starts_with("#[allow(") || t.starts_with("#![allow(")
        })
        .filter(|(i, _)| !is_justified(&lines, *i))
        .map(|(i, _)| Violation {
            kind: ViolationKind::UnjustifiedAllow,
            message: format!(
                "{rel_path}:{}: `#[allow(…)]` carries no justification — AGENTS.md \
                 asks that an allow be justified in a comment, so a reader can tell \
                 a considered exception from a silenced warning",
                i + 1
            ),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::scan_unjustified_allows;

    fn hits(text: &str) -> Vec<String> {
        scan_unjustified_allows("src/x.rs", text)
            .into_iter()
            .map(|v| v.message)
            .collect()
    }

    #[test]
    fn an_allow_with_a_comment_above_it_is_justified() {
        assert!(hits("// why this is right\n#[allow(clippy::foo)]\nfn f() {}\n").is_empty());
    }

    #[test]
    fn a_bare_allow_is_a_violation_naming_its_line() {
        let h = hits("fn f() {\n    #[allow(clippy::foo)]\n    let x = 1;\n}\n");
        assert_eq!(h.len(), 1);
        assert!(h[0].contains("src/x.rs:2:"), "{}", h[0]);
        assert!(h[0].contains("no justification"), "{}", h[0]);
    }

    #[test]
    fn a_justification_above_an_intervening_attribute_still_counts() {
        // The real shape from `main.rs:81`. Without the skip this is a false
        // positive, and one false positive on an ordinary idiom retires a rule.
        assert!(
            hits(
                "// the prefix is intentional\n#[derive(Debug)]\n#[allow(clippy::foo)]\nstruct S;\n"
            )
            .is_empty()
        );
    }

    #[test]
    fn a_blank_line_separates_a_comment_from_the_attribute() {
        // Prose about something else is not a justification, and accepting it
        // would let the rule pass on coincidence.
        assert_eq!(
            hits("// unrelated prose\n\n#[allow(clippy::foo)]\nfn f() {}\n").len(),
            1
        );
    }

    #[test]
    fn a_trailing_comment_on_the_attribute_line_justifies_it() {
        assert!(hits("#[allow(clippy::foo)] // narrow, and deliberate\nfn f() {}\n").is_empty());
    }

    #[test]
    fn an_inner_allow_at_the_top_of_a_file_is_checked_too() {
        // `#![allow(dead_code)]` in a test-support module is the exact case the
        // convention is about, and it has nothing above it to justify it.
        assert_eq!(hits("#![allow(dead_code)]\nfn f() {}\n").len(), 1);
        assert!(
            hits("//! shared fixture, not every consumer uses every path\n#![allow(dead_code)]\n")
                .is_empty()
        );
    }

    #[test]
    fn only_rust_sources_are_scanned() {
        // This module's own doc comment contains `#[allow(`. A rule that read
        // content rather than the path would report its own documentation.
        let text = "#[allow(clippy::foo)]\n";
        assert!(scan_unjustified_allows("docs/AGENTS.md", text).is_empty());
        assert!(scan_unjustified_allows("fixtures/x.json", text).is_empty());
        assert_eq!(scan_unjustified_allows("src/x.rs", text).len(), 1);
        // …and case-insensitively, as the extractor treats extensions.
        assert_eq!(scan_unjustified_allows("src/X.RS", text).len(), 1);
    }

    #[test]
    fn a_file_declaring_itself_fixture_data_is_exempt() {
        // The same directive intent-debt scanning honours, and for the same
        // reason: this rule's own end-to-end test embeds `#[allow(…)]` in a
        // fixture string, so without the opt-out the rule's first act is to
        // report its own test.
        let text = "// roteiro:ignore-file — fixtures below\n#[allow(clippy::foo)]\nfn f() {}\n";
        assert!(hits(text).is_empty());
        // …and it is the directive doing it, not the comment above the attribute.
        assert_eq!(
            hits("// fixtures below\n\n#[allow(clippy::foo)]\nfn f() {}\n").len(),
            1
        );
    }

    #[test]
    fn expect_is_not_allow() {
        // #438's warning, applied: the convention names `#[allow(...)]`.
        // `#[expect(...)]` fails the build when the lint stops firing, so it
        // documents its own expiry, and widening to it is how a zero-noise rule
        // becomes a noisy one.
        assert!(hits("#[expect(clippy::foo)]\nfn f() {}\n").is_empty());
    }
}

// roteiro:ignore-file — the fixtures below embed the very patterns these rules
// detect (`#[allow(…)]` without justification, a lossy conversion feeding a
// hash). They are test data, not uses: without this directive each rule's first
// act is to report its own test.
//! House-style conventions that **no compiler or linter enforces** — and that
//! this module makes the drift gate enforce.
//!
//! Every violation the scanners here return is folded into the check report's
//! violations, by both the CLI gate and the `tool_check` surface, so a hit fails
//! `roteiro check` and exits non-zero. "Not enforced" describes where these
//! conventions come from, not what happens to them now.
//!
//! `AGENTS.md` states one plainly: *"Prefer fixing over `#[allow(...)]`; when an
//! allow is right, justify it in a comment."* Until this module, nothing checked
//! it, so the convention held by habit — and habit is what a reviewer spends
//! attention on.
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

/// Every lossy string conversion in `text` that feeds a hash, as violations.
///
/// `to_string_lossy` replaces every invalid byte sequence with `U+FFFD`. That is
/// fine for a message a human reads. It is a defect when the result becomes an
/// **identity**, because two inputs differing only in those bytes produce the
/// same string, therefore the same digest, and the second silently overwrites
/// the first.
///
/// That one conversion is the whole of what this scans for. `to_str()` discards
/// differently — it yields `None`, so `unwrap_or_default()` turns *every*
/// non-UTF-8 path into the same empty string rather than a `U+FFFD` rendering of
/// itself — and the rule does not look for it, because this workspace has no
/// instance of it feeding a hash.
///
/// # Why this one and not a general "lossy conversion" rule
///
/// Counted on the commit this rule was written against: **71** `to_string_lossy`
/// call sites across the workspace, of which exactly **one** was a defect. The
/// other 70 build messages, log lines, and error text, where a replacement
/// character is the right outcome. A rule that flagged all of them would be
/// noise, and noise is how a gate stops being read.
///
/// So the rule is narrow by construction: it fires only where the conversion and
/// the hash marker sit on the **same line**, which is what makes the converted
/// value syntactically an argument to the call rather than merely near it.
///
/// The one site it caught — `rto_exec::worktree_id`, the defect the review corpus
/// recorded as `lossy-identity` (issue #438) — is fixed in the same change that
/// added the rule. So on a clean tree this rule reports **nothing**, and its job
/// from here is to stop that shape returning. Those counts are a snapshot of one
/// commit, not an invariant; what does not change is the trade — the rule stays
/// valuable precisely because widening it would convert a zero-noise rule into a
/// noisy one.
///
/// Rust sources only, decided by `rel_path` for the same reason as
/// [`scan_unjustified_allows`]: a mention of `to_string_lossy` in prose is not a
/// use of it, and this doc comment is itself the proof.
#[must_use]
pub fn scan_lossy_identity(rel_path: &str, text: &str) -> Vec<Violation> {
    // The same extension test [`scan_unjustified_allows`] uses, and for the same
    // reason: `ends_with(".rs")` is case-sensitive, so `X.RS` on a case-folding
    // filesystem would skip a file the graph considers Rust source.
    if !std::path::Path::new(rel_path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("rs"))
    {
        return Vec::new();
    }
    // The same whole-file opt-out [`scan_unjustified_allows`] honours, and needed
    // for the same reason: this rule's own tests embed the defect as fixture
    // strings, so without it the rule's first act is to report its own test
    // three times. A rule that flags itself is how a zero-noise rule becomes one
    // people switch off.
    if rto_graph::is_scan_exempt(text.as_bytes()) {
        return Vec::new();
    }
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if !line.contains("to_string_lossy") {
            continue;
        }
        // **Same line only.** A first version looked two lines either side and
        // immediately produced a false positive: a test that pushed
        // `file_name().to_string_lossy()` as a *name* into one tuple field while
        // hashing the file's bytes in the next. Adjacent to a hash is not the
        // same as feeding one, and a rule that cannot tell them apart is the
        // noisy rule issue #438 warns against becoming.
        //
        // Requiring both on one line means the converted value is syntactically
        // an argument to the call. It is narrower than the defect class in
        // general — a conversion bound to a variable and hashed later slips
        // through — and that is the trade: this rule is worth having because it
        // does not cry wolf, not because it is complete.
        if HASH_MARKERS.iter().any(|m| line.contains(m)) {
            out.push(Violation {
                kind: ViolationKind::LossyIdentity,
                message: format!(
                    "{rel_path}:{}: a lossy string conversion reaches a hash — two \
                     inputs differing only in invalid UTF-8 collapse to one digest, \
                     and the second silently replaces the first. Hash the bytes \
                     (`as_os_str().as_encoded_bytes()`) rather than the lossy string, \
                     or reject non-UTF-8 input explicitly.",
                    i + 1
                ),
            });
        }
    }
    out
}

/// What counts as "this value is becoming an identity".
///
/// Named rather than inlined so the set is visible: every addition widens the
/// rule, and this rule's worth is that it stays silent on every correct use of
/// `to_string_lossy` — which, now the one defect is fixed, is all of them.
const HASH_MARKERS: [&str; 5] = ["sha256", "Sha256", "Hasher", "blake3", "digest"];

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
    use super::{ViolationKind, scan_lossy_identity};

    #[test]
    fn a_lossy_conversion_feeding_a_hash_is_reported() {
        let v = scan_lossy_identity(
            "src/runner.rs",
            "let digest = sha256_hex(absolute.to_string_lossy().as_bytes());\n",
        );
        assert_eq!(v.len(), 1, "{v:?}");
        assert_eq!(v[0].kind, ViolationKind::LossyIdentity);
        assert!(v[0].message.contains("src/runner.rs:1"), "{}", v[0].message);
    }

    /// The correct uses — all of them, once the one defect is fixed — must stay
    /// silent, or the rule is noise.
    #[test]
    fn a_lossy_conversion_in_a_message_is_not_reported() {
        for line in [
            "eprintln!(\"cannot read {}\", path.to_string_lossy());",
            "let name = entry.file_name().to_string_lossy().into_owned();",
            "anyhow::bail!(\"{}: unreadable\", p.to_string_lossy())",
        ] {
            assert!(
                scan_lossy_identity("src/x.rs", line).is_empty(),
                "false positive on: {line}"
            );
        }
    }

    /// Adjacent to a hash is not the same as feeding one.
    ///
    /// This is the false positive the first version produced: a name built by a
    /// lossy conversion in one tuple field, and the file's *bytes* hashed in the
    /// next. Two lines apart, and unrelated.
    #[test]
    fn a_conversion_near_a_hash_but_not_in_it_is_not_reported() {
        let src = "found.push((\n    entry.file_name().to_string_lossy().into_owned(),\n    sha256_hex(&bytes),\n));\n";
        assert!(
            scan_lossy_identity("tests/pin.rs", src).is_empty(),
            "a conversion two lines from a hash it does not feed must not fire"
        );
    }

    /// Rust sources only: this module's own prose names the function.
    #[test]
    fn a_mention_in_prose_is_not_a_use() {
        let md = "Call `sha256_hex(p.to_string_lossy().as_bytes())` to hash a path.\n";
        assert!(scan_lossy_identity("docs/guide.md", md).is_empty());
    }

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

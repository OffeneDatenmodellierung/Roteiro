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

/// Whether `line` is a comment that can *justify* an `#[allow(…)]`.
///
/// Every comment except an **outer** doc comment (`///`). A `///` belongs to the
/// item below it and says nothing about why a lint is silenced, so counting it
/// made this rule satisfiable by accident: every `#[allow]` written under
/// ordinary docs — which is where most attributes sit — was exempt whatever
/// those docs said, and the rule could not tell "a considered exception from a
/// silenced warning", the exact distinction its own message claims to enforce.
/// See issue #770.
///
/// **This was a deliberate choice before, and it is being reversed knowingly.**
/// The removed `is_comment` argued that "`AGENTS.md` does not distinguish", and
/// on the letter that was true — so the convention has been tightened alongside
/// this change rather than the rule quietly outrunning it. It also predicted the
/// cost: "several existing allows are justified by the doc comment of the item
/// they sit on". Measured, that is exactly three, all now carrying a real reason.
///
/// **`//!` still counts**, and that is not an oversight. An inner
/// `#![allow(…)]` sits at the top of a file where module prose is the only place
/// its reason can live — `an_inner_allow_at_the_top_of_a_file_is_checked_too`
/// pins exactly that shape. Excluding it was the first cut of this fix and it
/// broke that test, which was right.
///
/// `////` and beyond are plain comments to rustc rather than docs, so the test
/// is `///` *not* followed by another `/`.
fn is_justifying_comment(line: &str) -> bool {
    let t = line.trim_start();
    if !t.starts_with("//") {
        return false;
    }
    // `////` and beyond are plain comments to rustc, not doc comments, so they
    // justify — checked before `///`, which they would otherwise match.
    if t.starts_with("////") {
        return true;
    }
    !t.starts_with("///")
}

/// Whether an `#[allow(…)]` opening at `lines[i]` carries a justification.
///
/// Justified by a trailing comment on the attribute's own line, or by the
/// nearest line above it that is **not another attribute** — and that comment
/// must be a plain `//`, not a doc comment. See [`is_justifying_comment`].
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
    // The language's own field, checked first because it is the form Rust
    // stabilised for exactly this and the one Clippy's
    // `allow_attributes_without_reason` requires.
    if carries_reason(lines, i) {
        return true;
    }
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
    j > 0 && is_justifying_comment(lines[j - 1])
}

/// Whether the attribute opening at `lines[i]` carries a `reason = "…"` field.
///
/// Spans the attribute rather than reading one line, because the multi-line form
/// is the ordinary one once a reason is long enough to be worth writing:
///
/// ```text
/// #[allow(
///     clippy::too_many_lines,
///     reason = "one scanner home; splitting would re-fork the copies this deletes"
/// )]
/// ```
///
/// The scan ends where the attribute's brackets balance, counting **code only**:
/// [`strip_comments`] removes both flavours — `//` and `/* … */`, the latter
/// counting nesting — before a line is measured. Without that, one unbalanced `[`
/// inside a comment kept the scan open and let a *later* attribute's reason
/// justify this one. All three shapes were measured, and each reported **0**
/// violations where 1 is right, because the failure runs the dangerous way: the
/// rule going quiet reads exactly like a clean file.
///
/// ```text
/// #[allow(
///     clippy::a, // see note [1                 ← a line comment
///     clippy::b, /* see note [1 */              ← a block comment
///     clippy::c, /* see /* note */ [1 */        ← a nested block comment
/// )]
/// fn f() {}          // ← reported before, silently accepted after
///
/// #[allow(clippy::d, reason = "stated")]
/// ```
///
/// It is bounded as well, so an unbalanced bracket inside a *string* cannot make
/// it run to the end of the file. That bound is a runaway backstop rather than a
/// formatting limit — see `MAX_SPAN`, which is set two orders of magnitude above
/// the longest attribute in this repository, because hitting it reads as "no
/// reason found" and that is the false positive this rule was retired for.
///
/// Matched on a **token** boundary and on the `=` that follows, so a lint named
/// `…::unreasonable` and prose containing the word "reason" in a trailing comment
/// are not mistaken for the field. That precision matters more here than in the
/// comment paths: those require a human to have written something, while this one
/// reads structure.
fn carries_reason(lines: &[&str], i: usize) -> bool {
    /// Attribute lines scanned before giving up.
    ///
    /// A **runaway backstop**, not a formatting limit, and the difference is why
    /// it is 200 rather than the 40 it started at. Hitting it means "no reason
    /// found", which is a false positive — the failure this whole rule was
    /// retired for — so the bound must sit far above any attribute anyone writes.
    /// Measured on this repository: the longest `#[allow(…)]` here spans **4**
    /// lines, and the other twenty-two are one.
    ///
    /// It is still needed. Comments no longer hold the scan open, but an
    /// unbalanced `[` inside a *string* can — `#[doc = "["]` leaves depth at one —
    /// and without a bound such an attribute would carry the scan to the end of
    /// the file, where any later `reason` would justify it.
    const MAX_SPAN: usize = 200;

    let mut depth = 0i32;
    let mut comment_depth = 0usize;
    for line in lines.iter().skip(i).take(MAX_SPAN) {
        // Comments are neither structure nor the field: cutting them keeps a
        // stray bracket from holding the scan open, and keeps prose from being
        // read as a `reason`. A trailing comment is a justification by its own
        // path anyway, so nothing is lost by ignoring it here.
        let code = strip_comments(line, &mut comment_depth);
        if has_reason_field(&code) {
            return true;
        }
        for c in code.chars() {
            match c {
                '[' => depth += 1,
                ']' => depth -= 1,
                _ => {}
            }
        }
        if depth <= 0 {
            break;
        }
    }
    false
}

/// `line` with its comments removed, carrying the block-comment nesting `depth`
/// across lines.
///
/// Both flavours, because both are legal inside an attribute and either can hide
/// an unbalanced bracket: `#[allow(a, /* note [1 */ b)]` is ordinary Rust. The
/// `//` case was found first and fixed alone; a reviewer pointed out that `/* */`
/// has the identical shape, which it does — so the two are handled in one place
/// rather than as a rule and an exception.
///
/// **Block comments nest** — `/* outer /* inner */ still outer */` is one comment
/// in Rust, not two — so this counts depth rather than holding a flag. A flag
/// leaves comment mode at the first `*/` and emits the outer comment's tail as
/// code, which is how the bracket overrun above comes back: measured on
/// `clippy::a, /* see /* note */ [1 */`, that reported **0** violations where 1
/// is right.
///
/// Not a Rust lexer beyond that, and it does not need to be. It does not know that
/// `//` inside a string literal is not a comment, so `reason = "see http://x"`
/// loses its tail —
/// which costs nothing, because `reason` and its `=` come first and the match has
/// already succeeded by then. Every way this is wrong truncates a line, and
/// truncation can only *lose* a justification, never invent one: the safe
/// direction for a rule whose failure mode is silence.
fn strip_comments(line: &str, depth: &mut usize) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    loop {
        // Inside a block: the next `/*` or `*/`, whichever comes first, and
        // nothing between them is code.
        while *depth > 0 {
            let open = rest.find("/*");
            let close = rest.find("*/");
            // A nested opener before the next closer deepens; otherwise the
            // closer ends this level. Neither, and the comment runs past this
            // line.
            let opens_first = match (open, close) {
                (Some(o), Some(c)) => o < c,
                (Some(_), None) => true,
                _ => false,
            };
            if opens_first {
                let o = open.expect("an opener, by the match above");
                *depth += 1;
                rest = &rest[o + 2..];
            } else if let Some(c) = close {
                *depth -= 1;
                rest = &rest[c + 2..];
            } else {
                return out;
            }
        }
        let line_at = rest.find("//");
        let block_at = rest.find("/*");
        // Whichever opens first wins: `/* // */` is a block, `// /*` is a line.
        let opens_block = match (line_at, block_at) {
            (Some(l), Some(b)) => b < l,
            (None, Some(_)) => true,
            _ => false,
        };
        if opens_block {
            let b = block_at.expect("a block opener, by the match above");
            out.push_str(&rest[..b]);
            *depth = 1;
            rest = &rest[b + 2..];
            continue;
        }
        // No block opener ahead, so the line ends here: at a `//` if there is
        // one, otherwise at its end.
        out.push_str(&rest[..line_at.unwrap_or(rest.len())]);
        return out;
    }
}

/// Whether `line` contains a `reason` **field**: the bare word, followed by `=`.
fn has_reason_field(line: &str) -> bool {
    let bytes = line.as_bytes();
    line.match_indices("reason").any(|(at, _)| {
        let before_ok = at == 0 || !is_ident_byte(bytes[at - 1]);
        let after = line[at + "reason".len()..].trim_start();
        // Not `==`: that is a comparison, and this is a field assignment.
        before_ok && after.starts_with('=') && !after.starts_with("==")
    })
}

/// Whether `b` can appear inside a Rust identifier.
fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
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
                 asks for a `reason = \"…\"` field or a comment, so a reader can tell \
                 a considered exception from a silenced warning",
                i + 1
            ),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{ViolationKind, scan_lossy_identity};

    /// The defect issue #770 records: a doc comment belongs to the item, not to
    /// the attribute, so counting it made this rule satisfiable by accident —
    /// every `#[allow]` under ordinary docs was exempt whatever the docs said.
    #[test]
    fn a_doc_comment_above_an_allow_is_not_a_justification() {
        let src = "/// What this function does.\n#[allow(clippy::too_many_lines)]\nfn f() {}\n";
        let v = scan_unjustified_allows("src/x.rs", src);
        assert_eq!(v.len(), 1, "an outer doc comment must not justify: {v:?}");
        assert_eq!(v[0].kind, ViolationKind::UnjustifiedAllow);
        assert!(v[0].message.contains("src/x.rs:2"), "{}", v[0].message);

        // `//!` is deliberately still a justification — see `is_justifying_comment`.
        assert!(
            scan_unjustified_allows(
                "src/x.rs",
                "//! shared fixture, not every consumer uses every path\n#![allow(dead_code)]\n",
            )
            .is_empty(),
            "module prose is where a file-level allow's reason lives"
        );
    }

    /// The forms that must keep working, or the fix trades one wrong answer for
    /// another and every considered exception in the tree starts shouting.
    #[test]
    fn the_real_justifications_still_count() {
        for src in [
            // A plain comment on the line above — how this codebase writes them.
            "// Exact by construction; see the ranges above.\n#[allow(clippy::cast_sign_loss)]\nfn f() {}\n",
            // Rust's own field, which needs no comment at all.
            "#[allow(clippy::cast_sign_loss, reason = \"exact by construction\")]\nfn f() {}\n",
            // Trailing, after the closing bracket.
            "#[allow(clippy::cast_sign_loss)] // exact by construction\nfn f() {}\n",
            // `////` is a plain comment to rustc, not a doc comment.
            "//// Not a doc comment.\n#[allow(clippy::cast_sign_loss)]\nfn f() {}\n",
            // A comment separated from the attribute by other attributes.
            "// Justified.\n#[must_use]\n#[allow(clippy::cast_sign_loss)]\nfn f() {}\n",
        ] {
            assert!(
                scan_unjustified_allows("src/x.rs", src).is_empty(),
                "must stay silent: {src}"
            );
        }
    }

    /// An allow with nothing above it at all was already reported, and still is —
    /// the fix narrows what counts, it does not change this case.
    #[test]
    fn an_allow_with_no_comment_at_all_is_still_reported() {
        let v =
            scan_unjustified_allows("src/x.rs", "#[allow(clippy::too_many_lines)]\nfn f() {}\n");
        assert_eq!(v.len(), 1, "{v:?}");
    }

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

    /// **The language's own `reason` field is a justification** (issue #753).
    ///
    /// This was the rule's largest defect: 183 false positives and 0 true
    /// positives on a workspace that mandates the attribute form. Worse, such a
    /// workspace could not satisfy both gates — Clippy's
    /// `allow_attributes_without_reason` *requires* the field this rule rejected,
    /// so every allow Clippy accepted, `roteiro check` refused. A gate in that
    /// state is one people pass with `--no-verify`, and the reporter was.
    ///
    /// The principle was already written down in [`is_comment`]: the convention
    /// asks for "a justification a reader will find", and rejecting the form that
    /// sits closest to the allow was inventing a stricter rule than the one
    /// `AGENTS.md` states.
    #[test]
    fn a_reason_field_is_a_justification() {
        assert!(
            hits("#[allow(clippy::foo, reason = \"counts stay under 2^53\")]\nfn f() {}\n")
                .is_empty()
        );
        // The multi-line form, which is the ordinary one once the reason is long
        // enough to be worth writing — and the one a single-line scan misses.
        assert!(
            hits(
                "#[allow(\n    clippy::too_many_lines,\n    reason = \"one home; splitting \
                 would re-fork the copies this deletes\"\n)]\nfn f() {}\n"
            )
            .is_empty()
        );
        // Inner attributes take the field too.
        assert!(hits("#![allow(dead_code, reason = \"test support\")]\nfn f() {}\n").is_empty());
    }

    /// **A stray bracket in a comment does not carry the scan into the next
    /// attribute.**
    ///
    /// The span ends where the *code* brackets balance. Counting the comment too
    /// left the scan open, and the next attribute's `reason` then justified this
    /// one — so a bare allow was silently accepted. That is the dangerous
    /// direction: the rule going quiet reads exactly like a clean file, whereas a
    /// false positive at least argues with you.
    ///
    /// Measured before the fix: this fixture reported **0** violations where it
    /// should report 1.
    #[test]
    fn a_bracket_in_a_comment_does_not_reach_the_next_attribute() {
        let text = "#[allow(\n    clippy::a, // see note [1\n)]\nfn f() {}\n\n\
                    #[allow(clippy::b, reason = \"stated\")]\nfn g() {}\n";
        let h = hits(text);
        assert_eq!(h.len(), 1, "the bare allow is still reported: {h:?}");
        assert!(h[0].contains("src/x.rs:1:"), "{}", h[0]);
    }

    /// **A block comment hides a bracket just as well as a line comment does.**
    ///
    /// `#[allow(a, /* note [1 */ b)]` is ordinary Rust, and the unbalanced `[`
    /// inside it held the span open exactly as the `//` case did — a reviewer
    /// pointed out that the first fix handled one flavour and not the other,
    /// which was true.
    ///
    /// Both directions are asserted: the bare allow is still reported, and a
    /// genuine reason on a *multi-line* attribute carrying a block comment is
    /// still found. Only the first would pass if the fix were "give up whenever a
    /// comment appears".
    #[test]
    fn a_block_comment_does_not_hide_the_end_of_the_attribute() {
        let overrun = "#[allow(\n    clippy::a, /* note [1 */\n)]\nfn f() {}\n\n\
                       #[allow(clippy::b, reason = \"stated\")]\nfn g() {}\n";
        let h = hits(overrun);
        assert_eq!(h.len(), 1, "the bare allow is still reported: {h:?}");
        assert!(h[0].contains("src/x.rs:1:"), "{}", h[0]);

        // A block comment spanning lines does not swallow the reason after it.
        assert!(
            hits(
                "#[allow(\n    clippy::a, /* a note\n       still the note */\n    \
                 reason = \"stated\"\n)]\nfn f() {}\n"
            )
            .is_empty(),
            "a real reason after a multi-line block comment still counts"
        );
    }

    /// **A long attribute still finds its reason.**
    ///
    /// The scan is bounded, and hitting the bound reads as "no reason found" — a
    /// false positive, which is the failure this whole rule was retired for. At
    /// the original 40 lines an `#[allow(…)]` listing many lints with its reason
    /// last would have been flagged despite carrying one.
    ///
    /// Both sides are asserted: the reason is found at 120 lines, and the bound is
    /// still a bound at 400. Only the first would pass if `MAX_SPAN` were removed
    /// altogether, which would let one unbalanced bracket in a string carry the
    /// scan to the end of the file.
    #[test]
    fn a_long_attribute_still_finds_its_reason_and_the_bound_still_bounds() {
        let long = |lints: usize| {
            use std::fmt::Write as _;
            let mut src = String::from("#[allow(\n");
            for n in 0..lints {
                let _ = writeln!(src, "    clippy::lint_{n},");
            }
            src.push_str("    reason = \"stated\"\n)]\nfn f() {}\n");
            src
        };
        assert!(
            hits(&long(120)).is_empty(),
            "a reason 120 lines down is still a reason"
        );
        assert_eq!(
            hits(&long(400)).len(),
            1,
            "and past the backstop the scan gives up, which is what the backstop is"
        );
    }

    /// **A nested block comment is one comment, not two.**
    ///
    /// `/* outer /* inner */ still outer */` nests in Rust. A flag leaves comment
    /// mode at the first `*/` and emits the outer comment's tail as code, which
    /// brings back the bracket overrun the previous two fixes closed — the third
    /// route to the same silent acceptance, so it is worth a test of its own
    /// rather than trusting that the shape is now handled.
    ///
    /// Measured before the fix: this fixture reported **0** violations where 1 is
    /// right.
    #[test]
    fn a_nested_block_comment_does_not_end_early() {
        let overrun = "#[allow(\n    clippy::a, /* see /* note */ [1 */\n)]\nfn f() {}\n\n\
                       #[allow(clippy::b, reason = \"stated\")]\nfn g() {}\n";
        let h = hits(overrun);
        assert_eq!(h.len(), 1, "the bare allow is still reported: {h:?}");
        assert!(h[0].contains("src/x.rs:1:"), "{}", h[0]);

        // And a nested comment does not swallow a real reason that follows it.
        assert!(
            hits(
                "#[allow(\n    clippy::a, /* a /* nested */ note */\n    \
                 reason = \"stated\"\n)]\nfn f() {}\n"
            )
            .is_empty(),
            "a reason after a nested block comment still counts"
        );
    }

    /// **Prose is not the field.**
    ///
    /// Cutting comments before the match is what makes this hold: a trailing
    /// comment mentioning a reason justifies the allow by the *comment* path, so
    /// nothing is lost — but a comment on a **neighbouring** attribute must not
    /// silence a bare one below it.
    #[test]
    fn a_comment_mentioning_a_reason_is_not_the_field() {
        // Blank line above, so the comment path cannot apply either.
        let h = hits("// the reason = it was needed\n\n#[allow(clippy::a)]\nfn f() {}\n");
        assert_eq!(h.len(), 1, "{h:?}");
    }

    /// **A bare allow is still a violation**, which is the half that makes the
    /// rule worth having at all.
    ///
    /// Asserted beside the accepting case rather than alone: a fix that accepted
    /// everything would satisfy the test above and nothing else, and this is the
    /// assertion that separates "reads the attribute" from "gave up".
    #[test]
    fn accepting_a_reason_does_not_accept_a_bare_allow() {
        let text = "#[allow(clippy::a, reason = \"stated\")]\nfn f() {}\n\n\
                    #[allow(clippy::b)]\nfn g() {}\n";
        let h = hits(text);
        assert_eq!(h.len(), 1, "only the bare one is reported: {h:?}");
        assert!(h[0].contains("src/x.rs:4:"), "{}", h[0]);
    }

    /// **The word alone is not the field.**
    ///
    /// `reason` is matched on a token boundary and on the `=` that follows, so a
    /// lint whose name merely contains it does not silence the rule. This one is
    /// worth pinning because the failure is invisible: an allow that looked
    /// justified and was not would leave the gate reporting nothing, which reads
    /// exactly like a clean repository.
    #[test]
    fn a_word_containing_reason_is_not_the_field() {
        assert_eq!(
            hits("#[allow(clippy::unreasonable_x)]\nfn f() {}\n").len(),
            1
        );
        assert_eq!(hits("#[allow(some::reasoning)]\nfn f() {}\n").len(), 1);
        // A comparison is not an assignment.
        assert_eq!(hits("#[allow(cfg(reason == 1))]\nfn f() {}\n").len(), 1);
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

//! How a refusal is written, so that a way forward stays one.
//!
//! #426's rule is that a refusal **names the way forward**. A way forward you
//! cannot paste is not one, and three of this crate's refusals drifted into
//! exactly that at once — which says the way they were written invited it rather
//! than that three people were careless.
//!
//! # The failure this type exists to make unrepresentable
//!
//! Rust's string-continuation escape lets a long message be wrapped in source:
//!
//! ```text
//! "asking for isolation and getting \
//!  execution is the one outcome"
//! ```
//!
//! `\` before the newline swallows the newline **and the next line's
//! indentation**, so that renders as one space. It is correct, and it is
//! *fragile in a way that leaves no trace*: any edit that drops the backslash —
//! a tool that rewrites the literal, a paste through something that treats `\`
//! at end-of-line as its own continuation — silently turns nine columns of
//! source indentation into nine spaces of user-visible text. Nothing fails to
//! compile, no test that greps for a phrase notices, and the message still
//! *reads* correctly in the source. It was found in shipped output:
//!
//! ```text
//! Nothing ran, and nothing fell back to this host: asking for isolation and getting          execution
//! ```
//!
//! # So prose is written as fragments, never as one wrapped literal
//!
//! [`Line::Note`] takes a **list of fragments**, each a complete literal on its
//! own source line, joined with exactly one space when rendered. There is no
//! continuation to lose, because there is none to begin with: wrapping is
//! expressed by the list, which is data, rather than by an escape, which is
//! punctuation. A fragment that somehow acquires stray whitespace is trimmed
//! away rather than printed.
//!
//! [`Line::Command`] is the opposite and deliberately so: rendered **verbatim**,
//! because its whitespace is its content — `for this run:  roteiro lint …` is
//! aligned with the line below it on purpose, and a renderer that normalised it
//! would break the thing it is there to preserve.
//!
//! # And the rules are checked where they cannot be skipped
//!
//! [`Guidance::defects`] states every rule; [`Guidance`]'s `Display` asserts them
//! in debug builds. So **any** test that renders a message checks that message,
//! and a new guidance is covered the first time anything prints it rather than
//! the first time somebody remembers to write a test for it.
//!
//! @rto:0020

use std::fmt;

/// Where a note sits: under the sentence that introduced it.
const NOTE_INDENT: &str = "\n  ";

/// Where something to copy sits: one step further in, so the eye finds it.
const COMMAND_INDENT: &str = "\n    ";

/// One line of a refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Line {
    /// Prose, as fragments joined by a single space.
    ///
    /// A list rather than one wrapped literal — see the module documentation.
    /// Write one source line per fragment and let the join do the wrapping.
    Note(&'static [&'static str]),
    /// Something the reader is meant to copy, rendered exactly as written.
    ///
    /// Its internal whitespace is content, so nothing here is normalised. That
    /// is also why it is a single literal rather than fragments: a command that
    /// needed wrapping is a command nobody can paste.
    Command(&'static str),
}

/// A refusal's body: what is wrong, and what to do about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Guidance(&'static [Line]);

impl Guidance {
    /// Build a guidance from its lines.
    #[must_use]
    pub const fn new(lines: &'static [Line]) -> Self {
        Self(lines)
    }

    /// Its lines, for a caller that needs to inspect rather than print.
    #[must_use]
    pub fn lines(self) -> &'static [Line] {
        self.0
    }

    /// Every way this guidance is malformed, in the order the lines appear.
    ///
    /// Empty for a well-formed one. Separated from the assertion so a test can
    /// report *what* is wrong rather than only that something is, and so the
    /// rules are readable in one place rather than spread through a renderer.
    ///
    /// The rules, and what each of them is for:
    ///
    /// - **A guidance says something.** An empty one is a refusal that names no
    ///   way forward, which is the thing #426 forbids.
    /// - **A fragment is trimmed and single-spaced.** This is the collapsed
    ///   continuation, caught by its signature: source indentation arrives as a
    ///   run of spaces inside a sentence.
    /// - **A fragment is one line.** A `\n` inside one means somebody built a
    ///   multi-line message by hand, around this type rather than with it.
    /// - **A command survives a paste.** `$ ` before a name is a shell
    ///   expansion that will not expand — measured, in a skip message that told
    ///   the reader to run `--image $ ROTEIRO_TEST_LINT_IMAGE`.
    #[must_use]
    pub fn defects(self) -> Vec<String> {
        let mut defects = Vec::new();
        if self.0.is_empty() {
            defects.push("the guidance is empty, so it names no way forward".to_owned());
        }
        for (index, line) in self.0.iter().enumerate() {
            match line {
                Line::Note(fragments) => {
                    if fragments.is_empty() {
                        defects.push(format!("line {index}: a note with no fragments"));
                    }
                    for (at, fragment) in fragments.iter().enumerate() {
                        let where_ = format!("line {index} fragment {at}");
                        if fragment.trim().is_empty() {
                            defects.push(format!("{where_}: empty"));
                        } else if *fragment != fragment.trim() {
                            defects.push(format!(
                                "{where_}: has leading or trailing whitespace ({fragment:?}) — \
                                 fragments are joined with one space, so it is never needed"
                            ));
                        }
                        if fragment.contains("  ") {
                            defects.push(format!(
                                "{where_}: contains a run of spaces ({fragment:?}) — the signature \
                                 of source indentation that leaked into the message"
                            ));
                        }
                        if fragment.contains('\n') || fragment.contains('\t') {
                            defects.push(format!(
                                "{where_}: contains a newline or tab — a note is one line, and \
                                 more lines are more `Line`s"
                            ));
                        }
                    }
                }
                Line::Command(command) => {
                    if command.trim().is_empty() {
                        defects.push(format!("line {index}: an empty command"));
                    } else if *command != command.trim() {
                        defects.push(format!(
                            "line {index}: the command has leading or trailing whitespace \
                             ({command:?}) — indentation is the renderer's"
                        ));
                    }
                    if command.contains('\n') {
                        defects.push(format!(
                            "line {index}: the command spans lines — one nobody can paste in one \
                             go is not a way forward"
                        ));
                    }
                    if let Some(rest) = command.split("$ ").nth(1)
                        && rest.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                    {
                        defects.push(format!(
                            "line {index}: `$ ` before a name ({command:?}) — the shell will not \
                             expand it, so the command as printed does not work"
                        ));
                    }
                    if let Some(column) = misaligned_run(command) {
                        defects.push(format!(
                            "line {index}: a run of spaces at column {column} ({command:?}) that \
                             does not follow a label — alignment follows a `:` and anything else \
                             is a wrapped literal, which a command may not be"
                        ));
                    }
                }
            }
        }
        defects
    }
}

/// Where `command` has a run of spaces that is not deliberate alignment, if it
/// does.
///
/// A [`Line::Command`] is rendered verbatim, so the run-of-spaces rule that
/// protects prose cannot apply to it — its whitespace is its content. But it is
/// exposed to the same hazard, because a command written as a wrapped literal
/// collapses the same way, and *is unreadable when it does*.
///
/// The distinction that separates the two: legitimate alignment in these
/// messages always follows a **label**, which ends in `:` —
/// `for this run:  roteiro lint …` lines up with `standing:      add …`. A run
/// of spaces anywhere else is a continuation that lost its backslash. So a
/// command is written as one literal, and this is what says so.
fn misaligned_run(command: &str) -> Option<usize> {
    let bytes = command.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != b' ' {
            at += 1;
            continue;
        }
        let start = at;
        while at < bytes.len() && bytes[at] == b' ' {
            at += 1;
        }
        // A single space is ordinary; a run is either alignment or a defect.
        if at - start > 1 && start.checked_sub(1).map(|i| bytes[i]) != Some(b':') {
            return Some(start);
        }
    }
    None
}

impl fmt::Display for Guidance {
    /// Render every line, each on its own, indented by what it is.
    ///
    /// A **leading** newline before each line rather than a trailing one, so a
    /// caller can append a guidance to a sentence without having to know whether
    /// it ends in one — `"…is not available here.{guidance}"` is the whole of
    /// how these are used.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Checked here rather than only in a test, so that any test which
        // renders any message checks that message. A malformed guidance is a
        // programming error and there is nothing a user could do about it, which
        // is what makes an assertion the right shape rather than an error.
        debug_assert!(
            self.defects().is_empty(),
            "malformed guidance: {}",
            self.defects().join("; ")
        );
        for line in self.0 {
            match line {
                Line::Note(fragments) => {
                    f.write_str(NOTE_INDENT)?;
                    for (at, fragment) in fragments.iter().enumerate() {
                        if at > 0 {
                            f.write_str(" ")?;
                        }
                        f.write_str(fragment.trim())?;
                    }
                }
                Line::Command(command) => {
                    f.write_str(COMMAND_INDENT)?;
                    f.write_str(command)?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Guidance, Line};

    /// Fragments are joined by exactly one space, and the join is what does the
    /// wrapping — so a message reads identically however its source is broken up.
    #[test]
    fn fragments_join_into_one_sentence_however_the_source_wrapped_them() {
        let one = Guidance::new(&[Line::Note(&["asking for isolation and getting execution"])]);
        let many = Guidance::new(&[Line::Note(&[
            "asking for isolation",
            "and getting",
            "execution",
        ])]);
        assert_eq!(one.to_string(), many.to_string());
        assert_eq!(
            one.to_string(),
            "\n  asking for isolation and getting execution"
        );
    }

    /// The defect that started this: source indentation arriving as user-visible
    /// spaces.
    ///
    /// Both shapes are refused — a fragment padded at its edge, and one with the
    /// run embedded in the middle, which is what a collapsed continuation
    /// actually produces. Checked through [`Guidance::defects`] rather than by
    /// rendering, because rendering a malformed guidance now trips the assertion
    /// in `Display`, which is the point of it.
    #[test]
    fn a_fragment_can_never_leak_source_indentation_into_the_output() {
        const PADDED: Guidance = Guidance::new(&[Line::Note(&["getting", "         execution"])]);
        const EMBEDDED: Guidance = Guidance::new(&[Line::Note(&["getting          execution"])]);

        let defects = PADDED.defects();
        assert!(
            defects.iter().any(|d| d.contains("leading or trailing")),
            "{defects:?}"
        );

        let defects = EMBEDDED.defects();
        assert_eq!(defects.len(), 1, "{defects:?}");
        assert!(defects[0].contains("run of spaces"), "{defects:?}");
    }

    /// `Display` trims anyway, and that is a backstop rather than a duplicate:
    /// `debug_assert!` is compiled out of a release build, and a message that
    /// reached a user with nine spaces in it would be the defect this module
    /// exists for, shipped.
    #[test]
    fn rendering_trims_even_though_a_padded_fragment_is_already_a_defect() {
        // Well-formed, so the assertion is satisfied; the fragments still go
        // through `trim` on the way out.
        const CLEAN: Guidance = Guidance::new(&[Line::Note(&["getting", "execution"])]);
        assert_eq!(CLEAN.to_string(), "\n  getting execution");
    }

    /// A command's whitespace is its content, so it is rendered verbatim — the
    /// two-space alignment in the escape below is deliberate and must survive.
    #[test]
    fn a_command_keeps_the_alignment_that_is_its_content() {
        const ALIGNED: &str = "for this run:  roteiro lint <analyzer> --allow-unsandboxed";
        const GUIDANCE: Guidance = Guidance::new(&[Line::Command(ALIGNED)]);
        assert_eq!(GUIDANCE.to_string(), format!("\n    {ALIGNED}"));
        assert!(
            GUIDANCE.defects().is_empty(),
            "internal alignment is content, not a defect: {:?}",
            GUIDANCE.defects()
        );
    }

    /// `--image $ VAR` was shipped. The shell would not expand it, so the
    /// command as printed does not work — which is the one thing a way forward
    /// may not be.
    #[test]
    fn a_command_whose_shell_expansion_is_broken_is_a_defect() {
        const BROKEN: Guidance = Guidance::new(&[Line::Command(
            "roteiro security prefetch --image $ ROTEIRO_TEST_LINT_IMAGE",
        )]);
        // The fixed form, and a `$` that is not an expansion at all, both pass.
        const FIXED: Guidance = Guidance::new(&[Line::Command(
            "roteiro security prefetch --image $ROTEIRO_TEST_LINT_IMAGE",
        )]);
        const NOT_A_VARIABLE: Guidance = Guidance::new(&[Line::Command("cost: $ 5")]);

        let defects = BROKEN.defects();
        assert_eq!(defects.len(), 1, "{defects:?}");
        assert!(defects[0].contains("will not expand"), "{defects:?}");
        for fine in [FIXED, NOT_A_VARIABLE] {
            assert!(fine.defects().is_empty(), "{:?}", fine.defects());
        }
    }

    /// A command is one literal. Wrapped like prose it collapses the same way,
    /// and unlike prose it is then unpasteable — so the run-of-spaces rule
    /// applies to it too, with alignment after a label carved out.
    #[test]
    fn a_command_may_align_after_a_label_and_may_not_wrap() {
        const ALIGNED: Guidance = Guidance::new(&[
            Line::Command("for this run:  roteiro lint <analyzer> --allow-unsandboxed"),
            Line::Command(
                "standing:      add `[lint] allow_unsandboxed = true` to ~/.roteiro/config.toml",
            ),
            Line::Command("cargo fetch --locked"),
        ]);
        const WRAPPED: Guidance = Guidance::new(&[Line::Command(
            "roteiro security prefetch --analyzer clippy --allow-download                --image $X",
        )]);

        assert!(ALIGNED.defects().is_empty(), "{:?}", ALIGNED.defects());
        let defects = WRAPPED.defects();
        assert_eq!(defects.len(), 1, "{defects:?}");
        assert!(
            defects[0].contains("does not follow a label"),
            "{defects:?}"
        );
    }

    /// Everything else the rules cover, each stated as the thing it prevents.
    #[test]
    fn every_rule_names_the_defect_it_prevents() {
        const EMPTY: Guidance = Guidance::new(&[]);
        const NO_FRAGMENTS: Guidance = Guidance::new(&[Line::Note(&[])]);
        const PADDED: Guidance = Guidance::new(&[Line::Note(&[" padded "])]);
        const TWO_LINES: Guidance = Guidance::new(&[Line::Note(&["two\nlines"])]);
        const MULTI_COMMAND: Guidance = Guidance::new(&[Line::Command("cargo fetch\ncargo build")]);
        const INDENTED: Guidance = Guidance::new(&[Line::Command("  indented")]);

        for (guidance, expected) in [
            (EMPTY, "names no way forward"),
            (NO_FRAGMENTS, "no fragments"),
            (PADDED, "whitespace"),
            (TWO_LINES, "a note is one line"),
            (MULTI_COMMAND, "nobody can paste"),
            (INDENTED, "whitespace"),
        ] {
            let defects = guidance.defects();
            assert!(
                defects.iter().any(|d| d.contains(expected)),
                "expected a defect mentioning {expected:?}, got {defects:?}"
            );
        }
    }

    /// The leading newline is what lets a caller append a guidance to a sentence
    /// without knowing whether that sentence ended in one.
    #[test]
    fn a_guidance_appends_to_a_sentence_rather_than_starting_a_document() {
        let guidance = Guidance::new(&[Line::Note(&["do this"]), Line::Command("that")]);
        assert_eq!(
            format!("something is wrong.{guidance}"),
            "something is wrong.\n  do this\n    that"
        );
        assert!(!guidance.to_string().ends_with('\n'));
    }
}

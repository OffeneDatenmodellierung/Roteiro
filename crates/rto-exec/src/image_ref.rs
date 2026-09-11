//! Whether an image reference is pinned by a digest — the one place that decides.
//!
//! Ungated, like [`crate::guidance`] and [`crate::sandbox_store`], and for a
//! reason of the same shape: **the rule is about a string somebody wrote in a
//! config file, not about which backends were compiled in.** `roteiro config`
//! has to be able to *report* an unpinned reference in a build with no sandbox
//! at all — it is the command an operator runs precisely because a key is not
//! doing what they expected — and a second copy of the check living in the
//! `roteiro` crate for that purpose is how one of the two ends up laxer than the
//! other. There is one function; [`crate::boxlite::pinned_digest`] wraps it into
//! the backend's error type and adds nothing.
//!
//! # Why a tag is refused at all
//!
//! Not reproducibility. ADR-0020 retires that argument for builders, because a
//! build's answer depends on a toolchain no digest pins. The reason is that the
//! image **is** the boundary: it is where somebody else's code executes, and a
//! tag is a mutable pointer to it. Whoever controls the tag can replace what
//! runs, with no version change and no notice, and the run would go on reporting
//! success. You may choose your own boundary; you may not choose one that can be
//! swapped under you.
//!
//! # …and why "a tag is refused" is not the whole of what this module says
//!
//! That argument is correct and it covers **one** of the four ways a reference
//! fails the check. The other three belong to people who have already pinned:
//! a `sha512` digest, an `@` naming no algorithm, a `@sha256:` whose value is
//! the abbreviated form a registry UI displayed. Telling any of them "that is a
//! tag" describes something they did not write, and handing them the
//! mutable-pointer argument answers a question they did not ask. Each defect
//! therefore carries its own sentence and its own guidance — see [`PinDefect`],
//! which records what that cost before it was fixed.
//!
//! @rto:0014
//! @rto:0020

use crate::guidance::{Guidance, Line};

/// Why a **mutable pointer** will not do, and how to get the digest instead.
///
/// A [`Guidance`] rather than a wrapped literal: this text is multi-line and
/// ends in something to paste, and written the other way it leaked its own
/// source indentation into the middle of a sentence (see [`crate::guidance`]).
///
/// The example is deliberately keyless — `<key> = "…"` rather than
/// `image = "…"` — because two different keys reach this message
/// (`[lint] image` and a `[security.images]` entry) and the refusal already
/// names which one it is talking about in its first sentence.
///
/// **This block belongs to [`PinDefect::Tag`] and [`PinDefect::ImplicitLatest`]
/// and to nothing else.** It argues that a tag is mutable and shows how to
/// obtain a digest — advice that is correct for someone who has never typed a
/// digest and *wrong* for someone whose digest is merely malformed, who is being
/// answered a question they did not ask. That is the defect this module was
/// reviewed for; see [`PinDefect`].
pub const PIN_IT: Guidance = Guidance::new(&[
    Line::Note(&[
        "An image is where somebody else's code executes, and a tag is a mutable",
        "pointer to it — whoever controls the tag can replace what runs, with no",
        "version change and no notice.",
    ]),
    Line::Note(&["Pin it by digest instead:"]),
    Line::Command("<key> = \"docker.io/you/image@sha256:<64 hex>\""),
    Line::Note(&[
        "`docker buildx imagetools inspect <reference>` prints it. Use the **index**",
        "digest — the one printed for the tag itself — so one reference resolves on",
        "both amd64 and arm64 rather than two that can drift apart.",
    ]),
]);

/// For a reference already pinned, by an algorithm this does not read.
///
/// **It does not argue that the reference is mutable, because it is not.** A
/// `sha512` digest is content-addressed and immutable; the only thing wrong with
/// it is that Roteiro reads one algorithm. Repeating [`PIN_IT`]'s mutability
/// argument here would tell an operator their immutable pin is a moving target.
pub const SHA256_IS_THE_PIN: Guidance = Guidance::new(&[
    Line::Note(&[
        "The reference is digest-addressed and is not a moving target — Roteiro",
        "simply pins by **sha256**, which is what an OCI registry serves as a",
        "manifest digest.",
    ]),
    Line::Note(&["Use the sha256 digest of the same image:"]),
    Line::Command("<key> = \"docker.io/you/image@sha256:<64 hex>\""),
    Line::Note(&[
        "`docker buildx imagetools inspect <reference>` prints it. Use the **index**",
        "digest — the one printed for the tag itself — so one reference resolves on",
        "both amd64 and arm64 rather than two that can drift apart.",
    ]),
]);

/// For a reference that says `@sha256:` and then gets the value wrong.
///
/// **The reader here has already pinned, correctly, and mistyped.** So this says
/// nothing about tags, offers no argument for pinning, and does not show the
/// shape of the key — they have the shape right. It names the one thing that
/// tends to be true: the value is the short form a registry UI or `docker
/// images` displays, which is a *prefix* of the digest rather than the digest.
pub const CHECK_THE_WHOLE_DIGEST: Guidance = Guidance::new(&[
    Line::Note(&[
        "A sha256 digest is exactly 64 hexadecimal characters. The commonest cause",
        "of a short one is the abbreviated form a registry UI or `docker images`",
        "shows, which is a prefix of the digest and not the digest.",
    ]),
    Line::Note(&["The whole one is printed by:"]),
    Line::Command("docker buildx imagetools inspect <reference>"),
    Line::Note(&[
        "Take the **index** digest — the one printed for the tag itself — so one",
        "reference resolves on both amd64 and arm64 rather than two that can drift",
        "apart.",
    ]),
]);

/// What is actually wrong with a reference that is not a digest pin.
///
/// # Why this exists at all
///
/// [`NotPinned`] used to carry no such thing, and its message said *"which is a
/// **tag** rather than a digest"* for **every** way of failing the check. Three
/// of the four were not tags. `image@sha256:deadbeef` is a reference whose
/// author has already pinned and has pasted the abbreviated digest a registry UI
/// showed them — and they were told they had typed a tag, and then handed
/// [`PIN_IT`], which explains why tags are dangerous and how to obtain a digest.
/// Both halves answer a question they did not ask; neither answers the one they
/// did. Someone in that position looks at a config line visibly containing a
/// digest, reads "that is a tag", and concludes the tool is broken.
///
/// The failure was not carelessness in one string. It was that the *reason* was
/// thrown away at the point it was known: [`pinned_digest`] distinguishes these
/// cases precisely — the comment at the length check has always said so — and
/// then routed all of them through one constructor. So the reason is now **data
/// the refusal carries** rather than a fact the parser knew and discarded, which
/// is what stops a future case being added to the check and silently inheriting
/// somebody else's sentence.
///
/// # The set is closed, and deliberately not `#[non_exhaustive]`
///
/// These are not a taxonomy someone chose; they are the branches of
/// [`pinned_digest`], which are exhaustive by construction — a reference either
/// has no `@`, or has one that does not introduce `sha256:`, or has one that
/// does and is followed by something other than 64 hex characters. Marking this
/// `#[non_exhaustive]` would imply a fifth is anticipated when the parse says
/// there cannot be one, and would be **weaker documentation than the silence**
/// — the reasoning ADR-0001 records for `derived | authored | inferred` (#448),
/// applied where it holds for the same reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinDefect {
    /// `repo:1.2.3` — a written tag, and therefore a mutable pointer.
    Tag,
    /// `repo` — neither tag nor digest, which an OCI resolver reads as
    /// `:latest`.
    ///
    /// Its own variant rather than folded into [`Self::Tag`] for this module's
    /// whole reason: it *is* a mutable pointer and gets the same guidance, but
    /// telling someone who wrote no tag that they wrote one is describing
    /// something they did not do. The remedy is shared; the diagnosis is not.
    ImplicitLatest,
    /// An `@` that does not introduce a sha256 digest — `repo@sha512:…`, or
    /// something that names no algorithm at all.
    ///
    /// Carries the text after the `@` rather than a pre-parsed algorithm, so the
    /// message can quote what was written when there is no algorithm to name.
    NotSha256 {
        /// Everything after the last `@`, as written.
        after_at: String,
    },
    /// `@sha256:` followed by something that is not a sha256 digest — truncated,
    /// over-long, empty, or not hexadecimal.
    MalformedDigest {
        /// What followed `@sha256:`, as written.
        given: String,
    },
}

impl PinDefect {
    /// The guidance that fits **this** defect.
    ///
    /// The method is the point of the type. Three blocks rather than one because
    /// the three readers are in different situations: one has never pinned, one
    /// has pinned by the wrong algorithm, one has pinned correctly and mistyped
    /// the value. A single accurate-but-vague block would be not-false for all
    /// three and useful to none, and would cost [`PinDefect::Tag`] the specific,
    /// correct argument that is the reason the whole rule exists.
    #[must_use]
    pub fn guidance(&self) -> Guidance {
        match self {
            Self::Tag | Self::ImplicitLatest => PIN_IT,
            Self::NotSha256 { .. } => SHA256_IS_THE_PIN,
            Self::MalformedDigest { .. } => CHECK_THE_WHOLE_DIGEST,
        }
    }
}

impl std::fmt::Display for PinDefect {
    /// The clause after *"…which"*, so each defect states what is true of the
    /// reference that was actually received.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tag => f.write_str("is a tag rather than a digest"),
            Self::ImplicitLatest => f.write_str(
                "names neither a tag nor a digest, so a registry resolves it as `:latest` — \
                 a tag by another name",
            ),
            Self::NotSha256 { after_at } => match after_at.split_once(':') {
                // No trailing "…, which is what Roteiro reads": the outer message
                // already opens with "which", and two of them in one sentence is
                // a message people stop reading halfway through.
                Some((algorithm, _)) => {
                    write!(f, "is pinned by {algorithm}, and Roteiro pins by sha256")
                }
                None => write!(
                    f,
                    "has an `@` followed by {after_at:?}, which names no digest algorithm at all"
                ),
            },
            Self::MalformedDigest { given } if given.is_empty() => {
                f.write_str("says `@sha256:` and then stops, so it names no digest")
            }
            Self::MalformedDigest { given } => {
                match given.chars().find(|c| !c.is_ascii_hexdigit()) {
                    // The character rather than its position: an operator scans a
                    // 64-character string for a symbol far faster than they count
                    // to an ordinal.
                    Some(bad) => write!(
                        f,
                        "says `@sha256:{given}`, and {bad:?} is not a hexadecimal digit"
                    ),
                    None => write!(
                        f,
                        "says `@sha256:{given}` — {} hex characters, where a sha256 digest is \
                         exactly 64",
                        given.len()
                    ),
                }
            }
        }
    }
}

/// An image reference that is not pinned by a sha256 digest, and why not.
///
/// Carries `what` because *which setting to go and change* is half of any
/// message a reader can act on, and [`PinDefect`] because **what is wrong with
/// the reference** is the other half. It used to carry only the first and assert
/// the second, which is the defect [`PinDefect`] documents.
///
/// Its audience is deliberately stated wide: anyone who wrote a reference this
/// module will not accept, **including people who have already pinned**. The
/// previous doc comment said this was *"a message met by people who have only
/// ever typed a tag"* — a type narrowing its own audience and then being used
/// for a wider one, which is how the wrong sentence stayed comfortable for three
/// of its four callers.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the image for {what} is {reference:?}, which {defect}.{}", defect.guidance())]
pub struct NotPinned {
    /// What wanted the image, so the reader knows which setting to change.
    pub what: String,
    /// The reference as it was written.
    pub reference: String,
    /// What is wrong with it, which selects both the sentence and the guidance.
    pub defect: PinDefect,
}

/// The digest `reference` is pinned to, or a refusal naming what to fix.
///
/// Holds a [`SANDBOX_IMAGES`](crate::boxlite::SANDBOX_IMAGES) entry, a
/// user-supplied builder image and a `[security.images]` entry to the same
/// standard — the difference between them is *who chose*, never *how strong the
/// pin is*.
///
/// Checked structurally rather than by looking for an `@`: a reference may carry
/// a registry port (`host:5000/repo`) and a tag, so "contains a colon" and
/// "names a digest" are different questions and only one of them is this one.
///
/// Each rejection carries the [`PinDefect`] that produced it. That is not
/// bookkeeping: this function is the only place that *knows* which of the four
/// things went wrong, and discarding it here is what previously left three of
/// four refusals describing a reference nobody had written.
///
/// # Errors
/// Returns [`NotPinned`] with [`PinDefect::Tag`] or [`PinDefect::ImplicitLatest`]
/// when `reference` names no digest at all, [`PinDefect::NotSha256`] when its
/// `@` introduces something other than `sha256:`, and
/// [`PinDefect::MalformedDigest`] when `@sha256:` is followed by anything but 64
/// hexadecimal characters.
pub fn pinned_digest<'a>(what: &str, reference: &'a str) -> Result<&'a str, NotPinned> {
    let refuse = |defect| NotPinned {
        what: what.to_owned(),
        reference: reference.to_owned(),
        defect,
    };

    let Some((_, digest)) = reference.rsplit_once('@') else {
        // Only the **last** path segment can carry a tag: `registry:5000/repo`
        // has a colon and no tag, which is the same distinction that makes the
        // digest check structural rather than a search for a colon.
        let tagged = reference
            .rsplit('/')
            .next()
            .is_some_and(|last| last.contains(':'));
        return Err(refuse(if tagged {
            PinDefect::Tag
        } else {
            PinDefect::ImplicitLatest
        }));
    };

    let Some(hex) = digest.strip_prefix("sha256:") else {
        return Err(refuse(PinDefect::NotSha256 {
            after_at: digest.to_owned(),
        }));
    };

    // Length *and* alphabet: `@sha256:` followed by anything at all would
    // otherwise satisfy a prefix check while naming nothing.
    if hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(digest)
    } else {
        Err(refuse(PinDefect::MalformedDigest {
            given: hex.to_owned(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::{PinDefect, pinned_digest};

    /// A reference is pinned by a sha256 digest or it is refused, and this is
    /// the one place that decides it — for a `SANDBOX_IMAGES` entry, a
    /// user-supplied builder image and a `[security.images]` entry alike.
    ///
    /// The rejections matter more than the acceptance. A prefix check would let
    /// `@sha256:` followed by anything through, and a "contains a colon" check
    /// would reject a registry port — so both the alphabet and the length are
    /// checked, and a port is not confused for a tag.
    #[test]
    fn a_reference_is_pinned_by_a_sha256_digest_or_it_is_refused() {
        let hex = "a".repeat(64);
        for pinned in [
            format!("docker.io/library/rust@sha256:{hex}"),
            // A registry with a port, which contains a colon and is not a tag.
            format!("registry.internal:5000/team/rust-clippy@sha256:{hex}"),
            // A tag *and* a digest: the digest is what resolves, so this is
            // pinned. Refusing it would reject what `docker pull` prints.
            format!("docker.io/library/rust:1.97.1@sha256:{hex}"),
            // Upper-case hex is hex.
            format!("docker.io/library/rust@sha256:{}", "A".repeat(64)),
        ] {
            assert!(pinned_digest("test", &pinned).is_ok(), "{pinned}");
        }

        for unpinned in [
            "docker.io/library/rust",
            "docker.io/library/rust:1.97.1",
            "registry.internal:5000/team/rust-clippy:latest",
            "x@sha256:",
            "x@sha256:deadbeef",
            "x@sha512:aaaa",
        ] {
            assert!(
                pinned_digest("test", unpinned).is_err(),
                "{unpinned} must be refused"
            );
        }
    }

    /// **Every refusal says something true about the reference it received.**
    ///
    /// This is the regression test for the review finding on #541: one message
    /// — *"which is a tag rather than a digest"* — was rendered for all four
    /// ways of failing the check, three of which are not tags. So each case is
    /// pinned to its own sentence **and** excluded from the others: a table of
    /// `contains` assertions alone would have passed happily while every row
    /// rendered the same string.
    #[test]
    fn each_defect_says_what_is_actually_wrong_and_not_what_is_wrong_with_another() {
        let hex = "a".repeat(64);
        // (reference, the phrase this case must say, the phrases it must not)
        let cases: &[(&str, &str, &[&str])] = &[
            (
                "registry.example/you/tool:1.2.3",
                "is a tag rather than a digest",
                &[
                    "names neither",
                    "pinned by",
                    "hex characters",
                    "no digest algorithm",
                ],
            ),
            (
                "registry.example:5000/you/tool:latest",
                "is a tag rather than a digest",
                &["names neither", "pinned by", "hex characters"],
            ),
            (
                "registry.example/you/tool",
                "names neither a tag nor a digest",
                // Not "is a tag": nobody wrote one.
                &["is a tag rather than", "pinned by", "hex characters"],
            ),
            (
                // A registry port and no tag: the colon is not a tag, and the
                // last segment is what decides.
                "registry.example:5000/you/tool",
                "names neither a tag nor a digest",
                &["is a tag rather than", "pinned by", "hex characters"],
            ),
            (
                &format!("registry.example/you/tool@sha512:{hex}"),
                "is pinned by sha512, and Roteiro pins by sha256",
                &["is a tag rather than", "names neither", "hex characters"],
            ),
            (
                "registry.example/you/tool@nonsense",
                "names no digest algorithm at all",
                &["is a tag rather than", "names neither", "hex characters"],
            ),
            (
                "registry.example/you/tool@sha256:",
                "stops, so it names no digest",
                &["is a tag rather than", "names neither", "pinned by"],
            ),
            (
                // The one the review named: the abbreviated digest a registry UI
                // shows. The author has already pinned.
                "registry.example/you/tool@sha256:deadbeef",
                "8 hex characters, where a sha256 digest is exactly 64",
                &["is a tag rather than", "names neither", "pinned by"],
            ),
            (
                &format!("registry.example/you/tool@sha256:{}", "a".repeat(65)),
                "65 hex characters, where a sha256 digest is exactly 64",
                &["is a tag rather than", "names neither"],
            ),
            (
                &format!("registry.example/you/tool@sha256:{}z", "a".repeat(63)),
                "and 'z' is not a hexadecimal digit",
                &[
                    "is a tag rather than",
                    "names neither",
                    "hex characters where",
                ],
            ),
        ];

        for (reference, must_say, must_not_say) in cases {
            let message = pinned_digest("`[security.images] tool`", reference)
                .expect_err(&format!("{reference} must be refused"))
                .to_string();
            assert!(
                message.contains(must_say),
                "{reference} should say {must_say:?}:\n{message}"
            );
            for wrong in *must_not_say {
                assert!(
                    !message.contains(wrong),
                    "{reference} must not say {wrong:?} — that describes a different mistake:\n{message}"
                );
            }
            // Whatever the defect, the two halves a reader acts on are present.
            assert!(message.contains("`[security.images] tool`"), "{message}");
            assert!(message.contains(*reference), "{message}");
            assert!(message.contains("imagetools inspect"), "{message}");
        }
    }

    /// The guidance follows the defect, and the mutability argument is attached
    /// to **only** the references that are actually mutable.
    ///
    /// The wrong-guidance half is the more damaging half of the original defect
    /// and would survive a fix that only reworded the first sentence: someone
    /// whose digest is eight characters was being told why tags are dangerous
    /// and how to obtain a digest, neither of which is their problem. A
    /// `sha512` pin was being called a moving target when it is immutable.
    #[test]
    fn the_guidance_matches_the_defect_rather_than_the_first_case_written() {
        let hex = "a".repeat(64);
        let render = |reference: &str| {
            pinned_digest("`[lint] image`", reference)
                .expect_err("refused")
                .to_string()
        };

        for mutable in ["repo/tool:1.2", "repo/tool"] {
            let message = render(mutable);
            assert!(message.contains("mutable"), "{mutable}: {message}");
            assert!(message.contains("Pin it by digest instead"), "{message}");
        }

        let other_algorithm = render(&format!("repo/tool@sha512:{hex}"));
        assert!(
            !other_algorithm.contains("mutable"),
            "a sha512 digest is immutable; calling it a moving target is false:\n{other_algorithm}"
        );
        assert!(
            other_algorithm.contains("is not a moving target"),
            "{other_algorithm}"
        );
        assert!(other_algorithm.contains("sha256"), "{other_algorithm}");

        let malformed = render("repo/tool@sha256:deadbeef");
        assert!(
            !malformed.contains("mutable"),
            "this reader has already pinned:\n{malformed}"
        );
        assert!(
            !malformed.contains("Pin it by digest instead"),
            "they did pin; the value is what is wrong:\n{malformed}"
        );
        assert!(
            malformed.contains("abbreviated form"),
            "the message names the thing that is usually true:\n{malformed}"
        );

        // Three distinct guidance blocks, so no two defects are being served one
        // block that happens to fit neither.
        let blocks = [
            PinDefect::Tag.guidance().to_string(),
            PinDefect::NotSha256 {
                after_at: "sha512:x".to_owned(),
            }
            .guidance()
            .to_string(),
            PinDefect::MalformedDigest {
                given: "deadbeef".to_owned(),
            }
            .guidance()
            .to_string(),
        ];
        for (i, a) in blocks.iter().enumerate() {
            for b in blocks.iter().skip(i + 1) {
                assert_ne!(a, b, "two defects share one block of guidance");
            }
        }
        // The two mutable-pointer defects deliberately *do* share theirs: the
        // diagnosis differs, the remedy does not.
        assert_eq!(
            PinDefect::Tag.guidance().to_string(),
            PinDefect::ImplicitLatest.guidance().to_string()
        );
    }

    /// The refusal names the key it was asked about, so one message can serve
    /// every surface without any reader being told to edit another's file.
    #[test]
    fn the_refusal_names_whichever_key_carried_the_reference() {
        for what in ["`[lint] image`", "`[security.images] osv-scanner`"] {
            for reference in ["example.com/i:latest", "example.com/i@sha256:beef"] {
                let err = pinned_digest(what, reference).expect_err("refused");
                assert!(err.to_string().contains(what), "{err}");
            }
        }
    }
}

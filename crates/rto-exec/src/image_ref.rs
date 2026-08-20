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
//! @rto:0014
//! @rto:0020

use crate::guidance::{Guidance, Line};

/// Why a tag will not do, and how to get the digest instead.
///
/// A [`Guidance`] rather than a wrapped literal: this text is multi-line and
/// ends in something to paste, and written the other way it leaked its own
/// source indentation into the middle of a sentence (see [`crate::guidance`]).
///
/// The example is deliberately keyless — `<key> = "…"` rather than
/// `image = "…"` — because two different keys now reach this message
/// (`[lint] image` and a `[security.images]` entry) and the refusal already
/// names which one it is talking about in its first sentence.
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

/// An image reference that names no digest, and the key it was written under.
///
/// Carries `what` rather than only the reference because this is a message met
/// by people who have only ever typed a tag: "that is a tag" is half of it, and
/// *which setting to go and change* is the other half.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "the image for {what} is {reference:?}, which is a tag rather than a digest.{}",
    PIN_IT
)]
pub struct NotPinned {
    /// What wanted the image, so the reader knows which setting to change.
    pub what: String,
    /// The reference as it was written.
    pub reference: String,
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
/// # Errors
/// Returns [`NotPinned`] if `reference` carries no `@sha256:<64 hex>` suffix.
pub fn pinned_digest<'a>(what: &str, reference: &'a str) -> Result<&'a str, NotPinned> {
    let unpinned = || NotPinned {
        what: what.to_owned(),
        reference: reference.to_owned(),
    };
    let (_, digest) = reference.rsplit_once('@').ok_or_else(unpinned)?;
    let hex = digest.strip_prefix("sha256:").ok_or_else(unpinned)?;
    // Length *and* alphabet: `@sha256:` followed by anything at all would
    // otherwise satisfy a prefix check while naming nothing.
    if hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(digest)
    } else {
        Err(unpinned())
    }
}

#[cfg(test)]
mod tests {
    use super::pinned_digest;

    /// An image reference is pinned by digest or it is refused, and this is the
    /// one place that decides it — for a `SANDBOX_IMAGES` entry, a user-supplied
    /// builder image and a `[security.images]` entry alike.
    ///
    /// The rejections matter more than the acceptance. A prefix check would let
    /// `@sha256:` followed by anything through, and a "contains a colon" check
    /// would reject a registry port — so both the alphabet and the length are
    /// checked, and a port is not confused for a tag.
    #[test]
    fn an_image_is_pinned_by_digest_or_it_is_refused() {
        let hex = "a".repeat(64);
        for pinned in [
            format!("docker.io/library/rust@sha256:{hex}"),
            // A registry with a port, which contains a colon and is not a tag.
            format!("registry.internal:5000/team/rust-clippy@sha256:{hex}"),
            // A tag *and* a digest: the digest is what resolves, so this is
            // pinned. Refusing it would reject what `docker pull` prints.
            format!("docker.io/library/rust:1.97.1@sha256:{hex}"),
        ] {
            assert_eq!(
                pinned_digest("test", &pinned).expect("pinned"),
                format!("sha256:{hex}"),
                "{pinned}"
            );
        }

        for unpinned in [
            "docker.io/library/rust".to_owned(),
            "docker.io/library/rust:1.97.1".to_owned(),
            "registry.internal:5000/team/rust-clippy:latest".to_owned(),
            // Digest-shaped and not a digest: a prefix check would pass these.
            "x@sha256:".to_owned(),
            "x@sha256:deadbeef".to_owned(),
            format!("x@sha256:{}", "a".repeat(63)),
            format!("x@sha256:{}", "a".repeat(65)),
            format!("x@sha256:{}z", "a".repeat(63)),
            format!("x@sha512:{hex}"),
        ] {
            let err = pinned_digest("`[lint] image`", &unpinned)
                .expect_err(&format!("{unpinned} must be refused"));
            let message = err.to_string();
            // A refusal names what to change, quotes what it was given, and
            // shows the shape it wants.
            assert!(message.contains("`[lint] image`"), "{message}");
            assert!(message.contains(&unpinned), "{message}");
            assert!(message.contains("@sha256:"), "{message}");
            assert!(message.contains("imagetools inspect"), "{message}");
        }
    }

    /// The refusal names the key it was asked about, so one message can serve
    /// both surfaces without either reader being told to edit the other's file.
    #[test]
    fn the_refusal_names_whichever_key_carried_the_tag() {
        for what in ["`[lint] image`", "`[security.images] osv-scanner`"] {
            let err = pinned_digest(what, "example.com/i:latest").expect_err("a tag");
            assert!(err.to_string().contains(what), "{err}");
        }
    }
}

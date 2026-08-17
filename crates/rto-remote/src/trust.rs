//! How much a producer's identity can be trusted — ADR-0019 §5.
//!
//! [[docs/adr/0015-generated-media-content-artifact-store.md]]'s `Producer` is
//! not a label but a *verifiable identity*: it folds a `model_digest` **as pinned
//! in the registry** into a canonical id, which is what makes re-describing an
//! image with different weights a new record rather than a silent overwrite.
//!
//! **That does not transfer to a hosted model, and pretending it does would be
//! the dishonest part of this feature.** A hosted model has no digest anyone can
//! compute. A vendor model string is a **mutable pointer**: the weights behind
//! `some-vendor/some-model-2026-05` can change while the name does not, and
//! Roteiro cannot detect it. Two records naming the same model may have been
//! produced by different weights, and nothing on this machine can tell.
//!
//! So a record states its trust on its face. [`ProducerTrust::VendorAsserted`]
//! is not a lesser grade of the same thing as [`ProducerTrust::PinnedDigest`] —
//! it is a **claim** where the other is a **measurement**, and
//! [`ProducerTrust::caveat`] is the sentence that says so wherever such a record
//! is displayed.

use serde::{Deserialize, Serialize};

/// Whether a producer's identity is measured or asserted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProducerTrust {
    /// The weights were digested on this machine and the digest is part of the
    /// identity — the local-model case ADR-0015 was written for. Two records
    /// with this trust and the same identity really were produced by the same
    /// bytes.
    PinnedDigest,
    /// The identity is a name the vendor chose, and nothing more. Roteiro cannot
    /// verify it, cannot detect a change behind it, and does not claim to.
    VendorAsserted,
}

impl ProducerTrust {
    /// Stable token for `--json` output and for the egress ledger.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PinnedDigest => "pinned_digest",
            Self::VendorAsserted => "vendor_asserted",
        }
    }

    /// Whether this identity was verified on this machine.
    #[must_use]
    pub fn is_verifiable(self) -> bool {
        matches!(self, Self::PinnedDigest)
    }

    /// The sentence a record carrying this trust must be displayed with, or
    /// `None` when the identity speaks for itself.
    #[must_use]
    pub fn caveat(self) -> Option<&'static str> {
        match self {
            Self::PinnedDigest => None,
            Self::VendorAsserted => Some(
                "this identity is a claim, not a measurement: a vendor model string is a \
                 mutable pointer, so the weights behind it can change while the name does \
                 not, and Roteiro cannot detect that",
            ),
        }
    }
}

impl std::fmt::Display for ProducerTrust {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::ProducerTrust;

    /// A vendor-asserted record must state on its face that its identity is a
    /// claim — the one thing ADR-0019 §5 says is worth more than the feature.
    #[test]
    fn a_vendor_asserted_identity_declares_itself_a_claim() {
        let trust = ProducerTrust::VendorAsserted;
        assert!(!trust.is_verifiable());
        let caveat = trust
            .caveat()
            .expect("a vendor-asserted record carries a caveat");
        assert!(caveat.contains("mutable pointer"), "{caveat}");
        assert!(caveat.contains("cannot detect"), "{caveat}");
    }

    /// A digest-pinned identity was measured here, so it needs no caveat — and
    /// the two must not render the same, or the distinction buys nothing.
    #[test]
    fn a_pinned_digest_needs_no_caveat_and_renders_differently() {
        assert!(ProducerTrust::PinnedDigest.is_verifiable());
        assert!(ProducerTrust::PinnedDigest.caveat().is_none());
        assert_ne!(
            ProducerTrust::PinnedDigest.as_str(),
            ProducerTrust::VendorAsserted.as_str()
        );
    }

    /// The token is what the ledger stores, so it round-trips through serde
    /// unchanged — a record whose trust could not be read back would be a record
    /// that cannot answer the question it exists for.
    #[test]
    fn the_token_round_trips_through_serde() {
        for trust in [ProducerTrust::PinnedDigest, ProducerTrust::VendorAsserted] {
            let json = serde_json::to_string(&trust).expect("serialize");
            assert_eq!(json, format!("\"{}\"", trust.as_str()));
            let back: ProducerTrust = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, trust);
        }
    }
}

//! How much a producer's identity can be trusted — ADR-0019 §5.
//!
//! **The type moved to [`rto_graph::trust`], and this module is the re-export.**
//! It was written here, alongside the egress ledger that first recorded it, and
//! it moved when `ModelSource::Remote` had to carry it: `rto-remote` depends on
//! `rto-graph`, so a variant in `rto-graph`'s model resolver cannot name a type
//! defined here.
//!
//! The alternative was a second, parallel enum — one for the resolver, one for
//! the ledger — and that is precisely the thing this grade exists to prevent.
//! `ProducerTrust` answers *"is this identity a measurement or a claim?"*, and an
//! answer that two types could disagree about is not an answer. So there is one
//! definition, and `rto_remote::ProducerTrust`, `rto_remote::trust::ProducerTrust`
//! and `rto_graph::ProducerTrust` are all the same type.
//!
//! Nothing about the decision changed with the address; the documentation of
//! *why* a hosted model can only ever be [`ProducerTrust::VendorAsserted`] moved
//! with the type and is worth reading there.

pub use rto_graph::trust::ProducerTrust;

#[cfg(test)]
mod tests {
    use super::ProducerTrust;

    /// **One type, three paths.** The re-export is not a convenience alias over a
    /// local copy: an endpoint built with `rto_graph`'s spelling and a ledger
    /// entry read back through `rto_remote`'s must be the same value, because a
    /// record whose trust grade depended on which import the writer reached for
    /// would be a record that cannot answer the question it exists for.
    #[test]
    fn the_re_exported_trust_is_the_one_definition() {
        let from_graph: rto_graph::ProducerTrust = rto_graph::ProducerTrust::VendorAsserted;
        // Assigning across the paths is the assertion: it does not compile if
        // they are distinct types.
        let from_remote: ProducerTrust = from_graph;
        let from_crate: crate::ProducerTrust = from_remote;
        assert_eq!(from_crate, ProducerTrust::VendorAsserted);
        assert_eq!(from_crate.as_str(), "vendor_asserted");
        assert!(
            from_crate.caveat().is_some_and(|c| c.contains("claim")),
            "the caveat travels with the type, not with the crate that re-exports it"
        );
    }
}

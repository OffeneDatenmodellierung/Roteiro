//! Edge provenance classes.

use serde::{Deserialize, Serialize};

/// How an edge or node in the graph was produced.
///
/// # Six tokens, because "external" is a modifier and not a class
///
/// Three of these describe work **this** graph did: deterministic extraction,
/// human authorship, heuristic inference. The other three describe the same
/// three claims made by *someone else*, imported from a peer repository's Open
/// Knowledge Format bundle ([[docs/adr/0021-open-knowledge-format-bundle.md]]).
///
/// The peer's tier is carried rather than collapsed, and that is the whole point
/// of the shape. [`crate::Provenance`] is matched exhaustively by
/// `rto_render::okf::origin_for` to produce OKF trust tiers, so a single flat
/// `External` would force one arm — and therefore one answer for everything
/// imported. Either it maps to *unverified*, which **downgrades** a peer's
/// human-reviewed concept, or it maps to *machine-confirmed*, which **upgrades**
/// their similarity guess. `render okf` then re-emits that flattened tier
/// outward to the next consumer: laundering by round-trip, in a format adopted
/// specifically because it can express the distinction.
///
/// # Externality does not nest
///
/// A fact we imported from B, which B had imported from C, is `external-*` —
/// not doubly external. [`Provenance::externalise`] is idempotent for exactly
/// that reason. Which repository the fact came *from* is not a property of the
/// fact: it is the import layer's `src_ref`, which names B.
///
/// # What "external" does **not** modify
///
/// An `external-inferred` edge carries no confidence, where a local
/// [`Provenance::Inferred`] one must (see [`crate::Edge::is_valid`]). A
/// confidence is a number *we computed*; OKF carries none for a relationship, so
/// adopting one would mean inventing it. That asymmetry is deliberate and is
/// enforced by the store's own `CHECK` as well as by Rust.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
// Kebab-case, so serde's tokens and [`Provenance::as_str`]'s are the same six
// strings. They have to be: a persisted import layer is `FactSet` JSON written
// through serde, and the live rows it is applied to are written through
// `as_str`. Two spellings of one value would make a layer and its own applied
// rows disagree about what they hold.
#[serde(rename_all = "kebab-case")]
pub enum Provenance {
    /// Deterministically extracted from source ASTs (tree-sitter). The default:
    /// the overwhelmingly common node/edge, and the correct value when a legacy
    /// cached fact set (serialized before nodes carried provenance) omits it.
    #[default]
    Derived,
    /// Authored by a human or agent in an ADR, blueprint, or annotation.
    Authored,
    /// Heuristically inferred (docs, embeddings); carries a confidence score.
    Inferred,
    /// A peer's [`Provenance::Derived`] fact, imported from their bundle.
    ///
    /// They could re-derive it from their AST; **we** cannot, because we do not
    /// have their tree. So this asserts *they* say it is deterministic, and
    /// nothing about our ability to check.
    ExternalDerived,
    /// A peer's [`Provenance::Authored`] fact, imported from their bundle.
    ///
    /// Someone confirmed it **in their repository**. Importing it as
    /// [`Provenance::Authored`] would assert that this graph human-authored it,
    /// which is the laundering the whole variant exists to refuse.
    ExternalAuthored,
    /// A peer's [`Provenance::Inferred`] fact, imported from their bundle — or
    /// any imported fact taken at *acknowledge* rather than *trust*: their
    /// information without their confirmation.
    ExternalInferred,
}

impl Provenance {
    /// Parse a provenance from its stable string token, returning `None` for an
    /// unrecognised value.
    ///
    /// Two things produce one: a corrupt database row, and a store written by a
    /// **newer** Roteiro that knows a token this build does not. The second is
    /// not hypothetical — this build added three — and it is diagnosed *before*
    /// a row is decoded, by [`crate::Store::schema_ahead`]: every widening of
    /// this set ships with a migration, so a store carrying an unknown token
    /// necessarily records a migration this build has never heard of.
    ///
    /// Deliberately **not** tolerant of an unknown token. A fallback value would
    /// have to be one of the six, and every choice is a claim: assuming
    /// `derived` upgrades an unknown to machine-confirmed, assuming `inferred`
    /// downgrades a confirmed fact. That is the same laundering the external
    /// variants exist to prevent, arriving through the error path instead.
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        match s {
            "derived" => Some(Self::Derived),
            "authored" => Some(Self::Authored),
            "inferred" => Some(Self::Inferred),
            "external-derived" => Some(Self::ExternalDerived),
            "external-authored" => Some(Self::ExternalAuthored),
            "external-inferred" => Some(Self::ExternalInferred),
            _ => None,
        }
    }

    /// Stable string form used in the `SQLite` store.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Derived => "derived",
            Self::Authored => "authored",
            Self::Inferred => "inferred",
            Self::ExternalDerived => "external-derived",
            Self::ExternalAuthored => "external-authored",
            Self::ExternalInferred => "external-inferred",
        }
    }

    /// Every token, in the order the enum declares them — the exact set
    /// [`Provenance::from_token`] accepts and the store's `CHECK` permits.
    ///
    /// Exists so an error message, a schema and a test can each name the
    /// vocabulary without writing a fourth copy of it down.
    #[must_use]
    pub fn tokens() -> &'static [&'static str] {
        &[
            "derived",
            "authored",
            "inferred",
            "external-derived",
            "external-authored",
            "external-inferred",
        ]
    }

    /// Whether this fact came from another repository's bundle rather than from
    /// this graph's own work.
    #[must_use]
    pub fn is_external(self) -> bool {
        matches!(
            self,
            Self::ExternalDerived | Self::ExternalAuthored | Self::ExternalInferred
        )
    }

    /// The tier this provenance claims, with externality stripped: what *kind*
    /// of claim it is, ignoring whose claim it is.
    ///
    /// Use it where the question is genuinely about the tier — rendering a trust
    /// level, ranking a fact's strength. Do **not** use it to decide whether a
    /// fact may be rewritten, re-derived or asserted as this graph's own: those
    /// questions are about ownership, and [`Provenance::is_external`] answers
    /// them.
    #[must_use]
    pub fn tier(self) -> Self {
        match self {
            Self::Derived | Self::ExternalDerived => Self::Derived,
            Self::Authored | Self::ExternalAuthored => Self::Authored,
            Self::Inferred | Self::ExternalInferred => Self::Inferred,
        }
    }

    /// This provenance as a peer's claim: the external variant carrying the same
    /// tier.
    ///
    /// **Idempotent, and that is the decision rather than a convenience.**
    /// Externality flattens to one level: a fact imported from B that B imported
    /// from C is `external-*`, not doubly external, because the fact is external
    /// exactly once and which repository it arrived from is the import layer's
    /// `src_ref`, not the fact's class.
    #[must_use]
    pub fn externalise(self) -> Self {
        match self {
            Self::Derived | Self::ExternalDerived => Self::ExternalDerived,
            Self::Authored | Self::ExternalAuthored => Self::ExternalAuthored,
            Self::Inferred | Self::ExternalInferred => Self::ExternalInferred,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Provenance;

    const ALL: [Provenance; 6] = [
        Provenance::Derived,
        Provenance::Authored,
        Provenance::Inferred,
        Provenance::ExternalDerived,
        Provenance::ExternalAuthored,
        Provenance::ExternalInferred,
    ];

    #[test]
    fn stable_string_forms() {
        assert_eq!(Provenance::Derived.as_str(), "derived");
        assert_eq!(Provenance::Authored.as_str(), "authored");
        assert_eq!(Provenance::Inferred.as_str(), "inferred");
        assert_eq!(Provenance::ExternalDerived.as_str(), "external-derived");
        assert_eq!(Provenance::ExternalAuthored.as_str(), "external-authored");
        assert_eq!(Provenance::ExternalInferred.as_str(), "external-inferred");
    }

    #[test]
    fn from_token_round_trips_and_rejects_unknown() {
        for p in ALL {
            assert_eq!(Provenance::from_token(p.as_str()), Some(p));
        }
        assert_eq!(Provenance::from_token("bogus"), None);
        // Near-misses, because the tokens are a stored wire format and a reader
        // that accepted a variant spelling would let two spellings of one value
        // into the store.
        assert_eq!(Provenance::from_token("external"), None);
        assert_eq!(Provenance::from_token("external_authored"), None);
        assert_eq!(Provenance::from_token("External-Authored"), None);
    }

    /// `tokens()` is the vocabulary, and it must be the *same* vocabulary
    /// `from_token`/`as_str` implement — a third list that drifted would be a
    /// schema `CHECK` permitting a value no code can read, or refusing one it
    /// writes.
    #[test]
    fn the_token_list_is_the_accepted_set() {
        let from_enum: Vec<&str> = ALL.iter().map(|p| p.as_str()).collect();
        assert_eq!(Provenance::tokens(), from_enum.as_slice());
        for token in Provenance::tokens() {
            assert!(
                Provenance::from_token(token).is_some(),
                "`{token}` is listed but not parseable"
            );
        }
    }

    /// The serde wire form and the store token are one set of strings, not two.
    /// A persisted import layer is `FactSet` JSON; the rows it is applied to are
    /// written through `as_str`. If those disagreed, a layer would round-trip
    /// into a different provenance than the one it applied.
    #[test]
    fn serde_and_the_store_token_agree() {
        for p in ALL {
            let json = serde_json::to_string(&p).expect("serialize");
            assert_eq!(json, format!("\"{}\"", p.as_str()));
            let back: Provenance = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, p);
        }
    }

    #[test]
    fn externalise_carries_the_tier_and_flattens() {
        assert_eq!(
            Provenance::Derived.externalise(),
            Provenance::ExternalDerived
        );
        assert_eq!(
            Provenance::Authored.externalise(),
            Provenance::ExternalAuthored
        );
        assert_eq!(
            Provenance::Inferred.externalise(),
            Provenance::ExternalInferred
        );
        // Flattening to one level: importing an already-external fact from a
        // peer who imported it themselves does not deepen anything.
        for p in ALL {
            assert_eq!(
                p.externalise().externalise(),
                p.externalise(),
                "externalise must be idempotent for {p:?}"
            );
            assert!(p.externalise().is_external());
        }
    }

    #[test]
    fn tier_strips_externality_and_is_the_inverse_of_externalise() {
        for p in ALL {
            assert!(!p.tier().is_external());
            assert_eq!(p.tier().externalise(), p.externalise());
            assert_eq!(p.externalise().tier(), p.tier());
        }
        assert_eq!(Provenance::ExternalAuthored.tier(), Provenance::Authored);
        assert_eq!(Provenance::Authored.tier(), Provenance::Authored);
    }

    #[test]
    fn only_the_external_three_are_external() {
        assert!(!Provenance::Derived.is_external());
        assert!(!Provenance::Authored.is_external());
        assert!(!Provenance::Inferred.is_external());
        assert!(Provenance::ExternalDerived.is_external());
        assert!(Provenance::ExternalAuthored.is_external());
        assert!(Provenance::ExternalInferred.is_external());
    }
}

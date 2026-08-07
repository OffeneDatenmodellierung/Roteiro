//! Edge provenance classes.

use serde::{Deserialize, Serialize};

/// How an edge in the graph was produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provenance {
    /// Deterministically extracted from source ASTs (tree-sitter).
    Derived,
    /// Authored by a human or agent in an ADR, blueprint, or annotation.
    Authored,
    /// Heuristically inferred (docs, embeddings); carries a confidence score.
    Inferred,
}

impl Provenance {
    /// Stable string form used in the `SQLite` store.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Derived => "derived",
            Self::Authored => "authored",
            Self::Inferred => "inferred",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Provenance;

    #[test]
    fn stable_string_forms() {
        assert_eq!(Provenance::Derived.as_str(), "derived");
        assert_eq!(Provenance::Authored.as_str(), "authored");
        assert_eq!(Provenance::Inferred.as_str(), "inferred");
    }
}

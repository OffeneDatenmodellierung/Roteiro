//! Minimal ADR frontmatter model and status parsing (house style).

use serde::{Deserialize, Serialize};

/// ADR lifecycle states, exactly as the house style defines them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AdrStatus {
    /// Being drafted.
    Draft,
    /// Circulated for advisory review.
    ForReview,
    /// Decision accepted.
    Accepted,
    /// Decision rejected.
    Rejected,
    /// Replaced by a later ADR.
    Superseded,
}

/// Errors raised while parsing ADR metadata.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    /// The status string is not one of the five house-style states.
    #[error("unknown ADR status: {0}")]
    UnknownStatus(String),
}

impl std::str::FromStr for AdrStatus {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Draft" => Ok(Self::Draft),
            "For Review" => Ok(Self::ForReview),
            "Accepted" => Ok(Self::Accepted),
            "Rejected" => Ok(Self::Rejected),
            "Superseded" => Ok(Self::Superseded),
            other => Err(ParseError::UnknownStatus(other.to_owned())),
        }
    }
}

/// Metadata for one ADR, as read from its frontmatter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdrMeta {
    /// Zero-padded ADR id, e.g. `0001`.
    pub id: String,
    /// Current title (evolves with the decision).
    pub title: String,
    /// Lifecycle state.
    pub status: AdrStatus,
}

#[cfg(test)]
mod tests {
    use super::AdrStatus;

    #[test]
    fn parses_all_house_statuses() {
        for (s, want) in [
            ("Draft", AdrStatus::Draft),
            ("For Review", AdrStatus::ForReview),
            ("Accepted", AdrStatus::Accepted),
            ("Rejected", AdrStatus::Rejected),
            ("Superseded", AdrStatus::Superseded),
        ] {
            assert_eq!(s.parse::<AdrStatus>().expect("parse"), want);
        }
    }

    #[test]
    fn rejects_unknown_status() {
        assert!("Pending".parse::<AdrStatus>().is_err());
    }
}

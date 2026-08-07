//! House-style ADR/blueprint parsing, the intent-confirmation interview, and
//! `roteiro check` drift detection. Sections of ADRs become graph nodes;
//! `[[path#Symbol]]` links and `// @rto:` annotations become `authored` edges.

mod adr;

pub use adr::{AdrMeta, AdrStatus, ParseError};

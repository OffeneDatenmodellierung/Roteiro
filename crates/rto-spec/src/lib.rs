//! House-style ADR/blueprint parsing and `roteiro check` drift detection.
//!
//! An ADR becomes an `adr` node with `adr_section` children; its
//! `[[path#Symbol]]` wiki-links and `@rto:<id>` source annotations become
//! `authored` edges into the derived code graph. [`check::run`] validates those
//! links against the graph and fails on drift (a link to a missing symbol, or a
//! `@rto:` annotation to an unknown or superseded ADR).

mod adr;
mod annotate;
mod check;
mod import;
mod lat;
mod spec;
mod text;

pub use adr::{AdrDoc, AdrMeta, AdrStatus, ParseError, Section, WikiLink, parse_adr};
pub use annotate::{Annotation, scan_annotations};
pub use check::{CheckReport, Violation, ViolationKind, run};
pub use import::{GRAPHIFY_REF, GraphifyImport, ImportError, ImportReport, import_graphify};
pub use lat::{LAT_REF, LatImport, LatReport, import_lat, resolve_lat_ref};
pub use spec::{
    SPEC_SCHEMA, SpecContext, SymbolContext, context, scaffold_adr, scaffold_blueprint,
};

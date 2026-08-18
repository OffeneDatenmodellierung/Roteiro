//! House-style ADR/blueprint parsing and `roteiro check` drift detection.
//!
//! An ADR becomes an `adr` node with `adr_section` children; its
//! `[[path#Symbol]]` wiki-links and `@rto:<id>` source annotations become
//! `authored` edges into the derived code graph. [`check::run`] validates those
//! links against the graph and fails on drift (a link to a missing symbol, or a
//! `@rto:` annotation to an unknown or superseded ADR).

mod adr;
mod annotate;
mod blueprint;
mod check;
mod import;
mod lat;
mod layer;
mod spec;
mod text;
mod tool_check;

pub use adr::{
    AdrDoc, AdrMeta, AdrStatus, DocVersion, InlineVersionRef, ParseError, Section, VersionFacts,
    WikiLink, parse_adr,
};
pub use annotate::{Annotation, scan_annotations};
pub use blueprint::{BlueprintDoc, is_blueprint, parse_blueprint};
pub use check::{CheckReport, Validation, Violation, ViolationKind, run, validate};
pub use import::{GRAPHIFY_REF, GraphifyImport, ImportError, ImportReport, import_graphify};
pub use lat::{
    LAT_REF, LatAnnotation, LatImport, LatReport, import_lat, import_lat_backlinks,
    resolve_lat_ref, scan_lat_annotations,
};
pub use layer::{AuthoredLayer, BlobReader, authored_blobs, authored_layer, authored_layer_from};
pub use spec::{
    SPEC_SCHEMA, SpecContext, SymbolContext, apply_drafts, context, draft_prompt, draft_targets,
    scaffold_adr, scaffold_blueprint,
};
pub use tool_check::{CheckedAgainst, Gate, TOOL_CHECK_SCHEMA, ToolCheck, tool_check};

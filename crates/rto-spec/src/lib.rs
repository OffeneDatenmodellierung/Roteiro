//! House-style ADR/blueprint/site-page parsing and `roteiro check` drift
//! detection.
//!
//! An ADR becomes an `adr` node with `adr_section` children; its
//! `[[path#Symbol]]` wiki-links and `@rto:<id>` source annotations become
//! `authored` edges into the derived code graph. [`check::run`] validates those
//! links against the graph and fails on drift (a link to a missing symbol, or a
//! `@rto:` annotation to an unknown or superseded ADR).
//!
//! [`SitePage`] is the same treatment for the public website: a document that
//! declares itself published becomes a `site_page` node whose links are
//! drift-checked like an ADR's, so roteiro.dev stops being the one documentation
//! surface outside the gate. See [`site`](mod@site) for why publication is a
//! frontmatter marker rather than a directory.

mod adr;
mod annotate;
mod blueprint;
mod check;
mod convention;
mod import;
mod lat;
mod layer;
mod site;
mod spec;
mod text;
mod tool_check;

pub use adr::{
    AdrDoc, AdrHome, AdrMeta, AdrStatus, DocDate, DocVersion, HistoryRow, InlineVersionRef,
    ParseError, Section, VersionFacts, WikiLink, adr_home, parse_adr,
};
pub use annotate::{Annotation, scan_annotations};
pub use blueprint::{BlueprintDoc, is_blueprint, parse_blueprint};
pub use check::{
    CheckReport, Validation, Violation, ViolationKind, run, run_layer, validate, validate_layer,
};
pub use convention::{scan_lossy_identity, scan_unjustified_allows};
pub use import::{GRAPHIFY_REF, GraphifyImport, ImportError, ImportReport, import_graphify};
pub use lat::{
    LAT_REF, LatAnnotation, LatImport, LatReport, import_lat, import_lat_backlinks,
    resolve_lat_ref, scan_lat_annotations,
};
pub use layer::{
    AuthoredDocs, AuthoredLayer, BlobReader, authored_blobs, authored_docs, authored_docs_from,
    authored_layer, authored_layer_from,
};
pub use site::{
    MARKER_FIELD as SITE_PAGE_FIELD, ParseError as SiteParseError, SitePage, is_site_page,
    parse_site_page, site_nav,
};
pub use spec::{
    SPEC_SCHEMA, SpecContext, SymbolContext, apply_drafts, context, draft_prompt, draft_targets,
    scaffold_adr, scaffold_blueprint,
};
pub use tool_check::{CheckedAgainst, Gate, TOOL_CHECK_SCHEMA, ToolCheck, tool_check};

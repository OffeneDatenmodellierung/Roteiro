//! Shared access to the committed analyzer fixtures.
//!
//! Every fixture here is **real output from the real tool**, captured once and
//! committed, so the tests that actually run in CI — which has neither semgrep
//! nor cargo-audit installed — exercise the parsers against what those tools
//! genuinely emit rather than against what a hand-written sample assumed.

// Each integration-test binary compiles its own copy of this module and uses
// only the part it needs, so anything the *other* binaries use reads as dead
// here. Splitting the module per consumer would duplicate the paths instead.
#![allow(dead_code)]

use std::path::PathBuf;

/// Root of this crate's `tests/fixtures` directory.
#[must_use]
pub fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

/// The polyglot source tree — one file per required language.
#[must_use]
pub fn polyglot_root() -> PathBuf {
    fixtures().join("polyglot")
}

/// Real `semgrep --json` output over [`polyglot_root`], captured from semgrep
/// 1.136.0 with the vendored baseline rule set.
#[must_use]
pub fn semgrep_native() -> Vec<u8> {
    read("native/semgrep-polyglot.json")
}

/// Real `cargo audit --json` output.
#[must_use]
pub fn cargo_audit_native() -> Vec<u8> {
    read("native/cargo-audit.json")
}

fn read(relative: &str) -> Vec<u8> {
    let path = fixtures().join(relative);
    std::fs::read(&path).unwrap_or_else(|e| panic!("reading fixture {}: {e}", path.display()))
}

/// Every language the coverage requirement names, paired with the fixture file
/// that must yield at least one finding for it.
///
/// The list is the coverage claim, in executable form: adding a language to
/// ADR-0015's matrix without a fixture that proves it would leave this table
/// disagreeing with the document.
pub const REQUIRED_LANGUAGES: &[(&str, &str)] = &[
    ("rust", "rust/src/deploy.rs"),
    ("python", "python/app.py"),
    ("java", "java/ReportService.java"),
    ("node (javascript)", "node/server.js"),
    ("sql", "sql/schema.sql"),
];

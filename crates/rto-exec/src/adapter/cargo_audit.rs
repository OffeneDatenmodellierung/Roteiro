//! `cargo-audit` — `RustSec` advisories against a resolved `Cargo.lock`.
//!
//! This is the dependency-vulnerability half of the coverage matrix (ADR-0016),
//! and it covers **Rust only**: `cargo audit` reads `Cargo.lock` and nothing
//! else. Python, Java and Node dependency vulnerabilities are a different tool
//! and a separate change; nothing here should be read as covering them.
//!
//! # Why this analyzer is the one that makes staleness real
//!
//! Semgrep's answer depends on the rules and the source. `cargo audit`'s answer
//! depends on an **advisory database that changes without the source changing** —
//! the exact case ADR-0012 built [`AdvisoryDb`] for. Its report states which
//! database it consulted (`last-commit`) and when that database was published
//! (`last-updated`), and both are carried onto the run so a result can be
//! labelled *possibly stale* rather than *current*.
//!
//! # Severity is a mapping, and the tool's own evidence is kept
//!
//! `RustSec` does not publish a qualitative severity level. It publishes a CVSS
//! **vector** on some advisories and an `informational` kind on others. This
//! adapter maps the kind onto [`Severity`] and preserves the raw CVSS vector,
//! aliases and categories verbatim in `meta`. Computing a CVSS base score from
//! the vector — which is what `cargo audit`'s own terminal output does — is
//! deliberately not done here: it is a scoring algorithm with its own versions,
//! and inventing a number that disagreed with the tool's would be worse than
//! carrying the vector unchanged.
//!
//! @rto:0012

use serde::Deserialize;

use crate::adapter::{Adapter, AssetPaths, Invocation, NativeContext};
use crate::ingest::{NormalizedReport, REPORT_SCHEMA, ReportFinding};
use crate::runner::ExecError;
use rto_graph::{AdvisoryDb, Severity};

/// The analyzer id, and the first component of every finding key it produces.
pub const ANALYZER: &str = "cargo-audit";

/// Asset id of the pinned `RustSec` advisory database.
pub const ADVISORY_DB_ASSET: &str = "rustsec-advisory-db";

/// Stands in for the lockfile blob in a finding's identity when the caller could
/// not determine one.
///
/// A `cargo-audit` finding is a claim about *a package version in a particular
/// lockfile*, so the lockfile blob is part of its identity. When it is unknown —
/// a report ingested outside a checkout — the identity stays well-formed and
/// says so, rather than silently keying on an empty component.
pub const UNKNOWN_LOCKFILE: &str = "unknown-lockfile";

/// The adapter.
#[derive(Debug, Clone, Copy)]
pub struct CargoAudit;

impl Adapter for CargoAudit {
    fn analyzer(&self) -> &'static str {
        ANALYZER
    }

    fn summary(&self) -> &'static str {
        "RustSec advisories against Cargo.lock (Rust dependencies only)"
    }

    fn languages(&self) -> &'static [&'static str] {
        &["rust"]
    }

    fn asset_ids(&self) -> &'static [&'static str] {
        &[ADVISORY_DB_ASSET]
    }

    fn command(&self, assets: &AssetPaths<'_>) -> Invocation {
        Invocation {
            program: "cargo".to_owned(),
            args: vec![
                "audit".to_owned(),
                "--json".to_owned(),
                // Egress configured off: never refresh the database mid-run. The
                // database is provisioned and pinned, so a run's answer is a
                // function of inputs that were fixed before it started.
                "--no-fetch".to_owned(),
                "--db".to_owned(),
                assets.arg(ADVISORY_DB_ASSET),
            ],
            // 0 = clean, 1 = vulnerabilities found.
            success_statuses: vec![0, 1],
        }
    }

    fn normalize(
        &self,
        native: &[u8],
        ctx: &NativeContext<'_>,
    ) -> Result<NormalizedReport, ExecError> {
        let output: AuditOutput = serde_json::from_slice(native)?;
        let Some(vulnerabilities) = output.vulnerabilities else {
            return Err(ExecError::MalformedReport(
                "not a cargo-audit report: no `vulnerabilities` object".to_owned(),
            ));
        };

        let lockfile = ctx
            .source
            .lockfile_blob
            .as_deref()
            .filter(|b| !b.trim().is_empty())
            .unwrap_or(UNKNOWN_LOCKFILE);

        let mut findings = Vec::new();
        for entry in &vulnerabilities.list {
            findings.push(convert(entry, "vulnerability", lockfile)?);
        }
        // `warnings` is keyed by kind (`unmaintained`, `unsound`, `yanked`, …),
        // and the set of kinds grows: iterating the map rather than naming the
        // kinds means a new one is reported instead of silently dropped.
        let mut kinds: Vec<&String> = output.warnings.keys().collect();
        kinds.sort();
        for kind in kinds {
            for entry in &output.warnings[kind] {
                findings.push(convert(entry, kind, lockfile)?);
            }
        }

        Ok(NormalizedReport {
            schema: REPORT_SCHEMA.to_owned(),
            analyzer: ANALYZER.to_owned(),
            // `cargo audit --json` carries no version field of its own, so a
            // report ingested from CI records "unknown" unless the caller
            // learned the version another way (a subprocess run asks the binary).
            analyzer_version: ctx.version_or(None),
            started_at: ctx.started_at.clone(),
            ended_at: ctx.ended_at.clone(),
            exit_status: ctx.exit_status,
            // Rules are not a thing for cargo-audit; the advisory database is.
            rules_digest: None,
            image_digest: None,
            // The report's own account of the database wins where it has one;
            // otherwise the caller's provisioning record stands in. `cargo audit`
            // reports nothing here whenever `--db` was passed, so in practice the
            // fallback is what carries the staleness evidence.
            advisory_db: output
                .database
                .and_then(advisory_db)
                .or_else(|| ctx.advisory_db.clone()),
            source: ctx.source.clone(),
            findings,
        })
    }
}

/// The advisory database evidence, or `None` when the report named no commit —
/// an unidentifiable database is not evidence, and a blank digest would read as
/// one.
fn advisory_db(database: Database) -> Option<AdvisoryDb> {
    let digest = database.last_commit?;
    if digest.trim().is_empty() {
        return None;
    }
    Some(AdvisoryDb {
        digest,
        published_at: database.last_updated.filter(|s| !s.trim().is_empty()),
    })
}

/// One vulnerability or warning entry → one normalized finding.
fn convert(entry: &Entry, kind: &str, lockfile: &str) -> Result<ReportFinding, ExecError> {
    let package = entry.package.as_ref().ok_or_else(|| {
        ExecError::MalformedReport(format!("a cargo-audit {kind} entry has no `package`"))
    })?;
    if package.name.trim().is_empty() {
        return Err(ExecError::MalformedReport(format!(
            "a cargo-audit {kind} entry has an unnamed package"
        )));
    }

    // A yanked crate has no advisory, so the warning kind takes the advisory
    // slot in the identity. Every component stays non-empty and the recipe's
    // shape — what fired, on what package, at what version, in which lockfile —
    // is the same either way.
    let advisory = entry.advisory.as_ref();
    let rule = advisory
        .map(|a| a.id.clone())
        .filter(|id| !id.trim().is_empty())
        .unwrap_or_else(|| kind.to_owned());
    let version = if package.version.trim().is_empty() {
        "unknown-version".to_owned()
    } else {
        package.version.clone()
    };

    let title = advisory
        .map(|a| a.title.trim())
        .filter(|t| !t.is_empty())
        .map_or_else(
            || format!("{} {version} is {kind}", package.name),
            str::to_owned,
        );

    Ok(ReportFinding {
        // ADR-0012's recipe: advisory, package, version, lockfile blob.
        identity: vec![
            rule.clone(),
            package.name.clone(),
            version.clone(),
            lockfile.to_owned(),
        ],
        rule,
        severity: severity(kind, advisory),
        title,
        message: advisory
            .map(|a| a.description.trim().to_owned())
            .unwrap_or_default(),
        // The claim is about a resolved dependency, not a location in the
        // source; `Cargo.lock` is the file that decides it.
        path: Some("Cargo.lock".to_owned()),
        span: None,
        meta: serde_json::json!({
            "kind": kind,
            "package": package.name,
            "version": version,
            "patched": entry.versions.as_ref().map(|v| v.patched.clone()).unwrap_or_default(),
            "cvss": advisory.and_then(|a| a.cvss.clone()),
            "aliases": advisory.map(|a| a.aliases.clone()).unwrap_or_default(),
            // Real advisories often carry the CVE under `related` rather than
            // `aliases` — RUSTSEC-2020-0159 lists CVE-2020-26235 there — so
            // dropping it would lose the identifier most people search by.
            "related": advisory.map(|a| a.related.clone()).unwrap_or_default(),
            "categories": advisory.map(|a| a.categories.clone()).unwrap_or_default(),
            "url": advisory.and_then(|a| a.url.clone()),
            "advisory_date": advisory.and_then(|a| a.date.clone()),
        }),
    })
}

/// Map a `RustSec` entry onto [`Severity`].
///
/// `RustSec` publishes no qualitative level, so this is Roteiro's mapping, not the
/// tool's judgement, and the advisory's own CVSS vector is preserved in `meta`
/// unchanged. A vulnerability is `high` because `RustSec`'s bar for one is a
/// security defect with a known impact; the informational kinds are graded below
/// it. An unrecognised kind is kept verbatim rather than being flattened into a
/// level nobody assigned.
fn severity(kind: &str, advisory: Option<&Advisory>) -> Severity {
    // An advisory's own `informational` field is more specific than the bucket
    // it happened to be listed under.
    let kind = advisory
        .and_then(|a| a.informational.as_deref())
        .unwrap_or(kind);
    match kind {
        "vulnerability" => Severity::High,
        "unsound" => Severity::Medium,
        "unmaintained" | "yanked" => Severity::Low,
        "notice" => Severity::Info,
        other => Severity::from_token(other),
    }
}

/// The shape of `cargo audit --json`, narrowed to what is needed. Unknown fields
/// are ignored so a `cargo-audit` upgrade that adds keys does not break ingest.
#[derive(Debug, Deserialize)]
struct AuditOutput {
    #[serde(default)]
    database: Option<Database>,
    /// Absent rather than empty distinguishes "not a cargo-audit report" from
    /// "a clean audit".
    #[serde(default)]
    vulnerabilities: Option<Vulnerabilities>,
    #[serde(default)]
    warnings: std::collections::BTreeMap<String, Vec<Entry>>,
}

#[derive(Debug, Deserialize)]
struct Database {
    #[serde(default, rename = "last-commit")]
    last_commit: Option<String>,
    #[serde(default, rename = "last-updated")]
    last_updated: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Vulnerabilities {
    #[serde(default)]
    list: Vec<Entry>,
}

#[derive(Debug, Deserialize)]
struct Entry {
    #[serde(default)]
    advisory: Option<Advisory>,
    #[serde(default)]
    package: Option<Package>,
    #[serde(default)]
    versions: Option<Versions>,
}

#[derive(Debug, Deserialize)]
struct Advisory {
    #[serde(default)]
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    cvss: Option<String>,
    #[serde(default)]
    informational: Option<String>,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    related: Vec<String>,
    #[serde(default)]
    categories: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Package {
    #[serde(default)]
    name: String,
    #[serde(default)]
    version: String,
}

#[derive(Debug, Deserialize)]
struct Versions {
    #[serde(default)]
    patched: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::{ADVISORY_DB_ASSET, ANALYZER, CargoAudit, UNKNOWN_LOCKFILE};
    use crate::adapter::{Adapter, AssetPaths, NativeContext};
    use crate::runner::ExecError;
    use rto_graph::{Severity, SourceIdentity};

    static SOURCE_WITH_LOCK: std::sync::LazyLock<SourceIdentity> =
        std::sync::LazyLock::new(|| SourceIdentity {
            lockfile_blob: Some("lock123".to_owned()),
            ..SourceIdentity::default()
        });
    static SOURCE_BARE: std::sync::LazyLock<SourceIdentity> =
        std::sync::LazyLock::new(SourceIdentity::default);

    fn ctx(source: &'static SourceIdentity) -> NativeContext<'static> {
        NativeContext {
            started_at: "2026-08-15T09:00:00Z".to_owned(),
            ended_at: "2026-08-15T09:00:02Z".to_owned(),
            analyzer_version: Some("0.21.2".to_owned()),
            exit_status: 1,
            source,
            rules_digest: None,
            advisory_db: None,
            snippets: &crate::snippet::NoSnippets,
        }
    }

    const NATIVE: &str = r#"{
      "database": {
        "advisory-count": 742,
        "last-commit": "9f1e5c0a2b7d4e6f8a0c1b3d5e7f9a1c3e5d7f90",
        "last-updated": "2026-06-01T04:12:00Z"
      },
      "lockfile": {"dependency-count": 412},
      "vulnerabilities": {
        "found": true,
        "count": 1,
        "list": [
          {
            "advisory": {
              "id": "RUSTSEC-2026-0031",
              "package": "openssl",
              "title": "openssl `X509` use-after-free",
              "description": "A crafted certificate chain can free memory still in use.",
              "date": "2026-05-20",
              "url": "https://rustsec.org/advisories/RUSTSEC-2026-0031",
              "cvss": "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H",
              "aliases": ["CVE-2026-1234"],
              "categories": ["memory-corruption"]
            },
            "versions": {"patched": [">=0.10.66"], "unaffected": []},
            "package": {"name": "openssl", "version": "0.10.5"}
          }
        ]
      },
      "warnings": {
        "unmaintained": [
          {
            "kind": "unmaintained",
            "advisory": {
              "id": "RUSTSEC-2024-0436",
              "title": "paste is unmaintained",
              "description": "The author has archived the repository.",
              "informational": "unmaintained"
            },
            "versions": {"patched": []},
            "package": {"name": "paste", "version": "1.0.15"}
          }
        ],
        "yanked": [
          {
            "kind": "yanked",
            "package": {"name": "half-baked", "version": "0.3.1"}
          }
        ]
      }
    }"#;

    #[test]
    fn normalizes_vulnerabilities_and_warnings_together() {
        let report = CargoAudit
            .normalize(NATIVE.as_bytes(), &ctx(&SOURCE_WITH_LOCK))
            .expect("parse");
        assert_eq!(report.analyzer, ANALYZER);
        assert_eq!(report.analyzer_version, "0.21.2");
        assert_eq!(report.findings.len(), 3, "one vuln, two warnings");
        assert!(
            report.rules_digest.is_none(),
            "rules are not a cargo-audit thing"
        );
    }

    /// The advisory database is the whole reason this analyzer needs a staleness
    /// story: the same lockfile at the same commit legitimately yields a
    /// different answer against a newer database.
    #[test]
    fn carries_the_advisory_database_identity_and_publication_date() {
        let report = CargoAudit
            .normalize(NATIVE.as_bytes(), &ctx(&SOURCE_WITH_LOCK))
            .expect("parse");
        let db = report.advisory_db.expect("an advisory database");
        assert_eq!(db.digest, "9f1e5c0a2b7d4e6f8a0c1b3d5e7f9a1c3e5d7f90");
        assert_eq!(db.published_at.as_deref(), Some("2026-06-01T04:12:00Z"));
    }

    /// A database with no commit id cannot be identified, so it is recorded as
    /// absent rather than as a blank digest that would read like evidence.
    #[test]
    fn an_unidentifiable_database_is_recorded_as_none() {
        let native = NATIVE.replace("\"9f1e5c0a2b7d4e6f8a0c1b3d5e7f9a1c3e5d7f90\"", "\"  \"");
        let report = CargoAudit
            .normalize(native.as_bytes(), &ctx(&SOURCE_WITH_LOCK))
            .expect("parse");
        assert!(report.advisory_db.is_none());
    }

    /// `cargo audit` reports `last-commit: null` whenever it is pointed at a
    /// database with `--db` rather than resolving one itself — which is every
    /// pinned, reproducible, offline run. Without the caller's provisioning
    /// record standing in, the *pinned* configuration would be the one with no
    /// staleness evidence, which is exactly backwards.
    #[test]
    fn falls_back_to_the_callers_pinned_database_when_the_report_names_none() {
        let native = r#"{"database":{"advisory-count":1216,"last-commit":null,
            "last-updated":null},"vulnerabilities":{"list":[]},"warnings":{}}"#;
        let mut ctx = ctx(&SOURCE_WITH_LOCK);
        ctx.advisory_db = Some(rto_graph::AdvisoryDb {
            digest: "ec5f7ef066dd".to_owned(),
            published_at: Some("2026-08-12T10:42:29Z".to_owned()),
        });
        let report = CargoAudit
            .normalize(native.as_bytes(), &ctx)
            .expect("parse");
        let db = report
            .advisory_db
            .expect("the pinned database must stand in");
        assert_eq!(db.digest, "ec5f7ef066dd");
        assert_eq!(db.published_at.as_deref(), Some("2026-08-12T10:42:29Z"));
    }

    /// It is a fallback, not an override: when the tool does report a database,
    /// the tool's own account is the evidence.
    #[test]
    fn the_reports_own_database_wins_over_the_callers() {
        let mut ctx = ctx(&SOURCE_WITH_LOCK);
        ctx.advisory_db = Some(rto_graph::AdvisoryDb {
            digest: "from-the-cache".to_owned(),
            published_at: None,
        });
        let report = CargoAudit
            .normalize(NATIVE.as_bytes(), &ctx)
            .expect("parse");
        assert_eq!(
            report.advisory_db.expect("db").digest,
            "9f1e5c0a2b7d4e6f8a0c1b3d5e7f9a1c3e5d7f90"
        );
    }

    #[test]
    fn uses_the_advisory_package_version_lockfile_identity() {
        let report = CargoAudit
            .normalize(NATIVE.as_bytes(), &ctx(&SOURCE_WITH_LOCK))
            .expect("parse");
        let vuln = report
            .findings
            .iter()
            .find(|f| f.rule == "RUSTSEC-2026-0031")
            .expect("the vulnerability");
        assert_eq!(
            vuln.identity,
            vec!["RUSTSEC-2026-0031", "openssl", "0.10.5", "lock123"]
        );
        assert_eq!(vuln.severity, Severity::High);
        assert_eq!(vuln.path.as_deref(), Some("Cargo.lock"));
        assert_eq!(
            vuln.meta["cvss"],
            "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H"
        );
        assert_eq!(vuln.meta["aliases"][0], "CVE-2026-1234");
    }

    /// A yanked crate has no advisory at all. Its identity must still be
    /// well-formed, so the warning kind takes the advisory slot.
    #[test]
    fn a_warning_with_no_advisory_is_keyed_by_its_kind() {
        let report = CargoAudit
            .normalize(NATIVE.as_bytes(), &ctx(&SOURCE_WITH_LOCK))
            .expect("parse");
        let yanked = report
            .findings
            .iter()
            .find(|f| f.rule == "yanked")
            .expect("the yanked warning");
        assert_eq!(
            yanked.identity,
            vec!["yanked", "half-baked", "0.3.1", "lock123"]
        );
        assert_eq!(yanked.severity, Severity::Low);
        assert_eq!(yanked.title, "half-baked 0.3.1 is yanked");
    }

    #[test]
    fn an_informational_advisory_is_graded_below_a_vulnerability() {
        let report = CargoAudit
            .normalize(NATIVE.as_bytes(), &ctx(&SOURCE_WITH_LOCK))
            .expect("parse");
        let unmaintained = report
            .findings
            .iter()
            .find(|f| f.rule == "RUSTSEC-2024-0436")
            .expect("the unmaintained warning");
        assert_eq!(unmaintained.severity, Severity::Low);
    }

    /// Outside a checkout the lockfile blob is unknown. The identity has to stay
    /// well-formed and say so, rather than key on an empty component.
    #[test]
    fn an_unknown_lockfile_is_named_not_blank() {
        let report = CargoAudit
            .normalize(NATIVE.as_bytes(), &ctx(&SOURCE_BARE))
            .expect("parse");
        assert!(
            report
                .findings
                .iter()
                .all(|f| f.identity[3] == UNKNOWN_LOCKFILE)
        );
    }

    /// The lockfile is part of the identity precisely so a finding does not
    /// silently survive a dependency bump under the same key.
    #[test]
    fn a_different_lockfile_is_a_different_finding() {
        let with_lock = CargoAudit
            .normalize(NATIVE.as_bytes(), &ctx(&SOURCE_WITH_LOCK))
            .expect("a");
        let bare = CargoAudit
            .normalize(NATIVE.as_bytes(), &ctx(&SOURCE_BARE))
            .expect("b");
        assert_ne!(with_lock.findings[0].identity, bare.findings[0].identity);
    }

    #[test]
    fn a_clean_audit_is_a_valid_empty_report() {
        let clean = br#"{"database":{"last-commit":"abc"},
            "vulnerabilities":{"found":false,"count":0,"list":[]},"warnings":{}}"#;
        let report = CargoAudit
            .normalize(clean, &ctx(&SOURCE_WITH_LOCK))
            .expect("parse");
        assert!(report.findings.is_empty());
        assert_eq!(report.advisory_db.expect("db").digest, "abc");
    }

    #[test]
    fn refuses_output_that_is_not_a_cargo_audit_report() {
        let err = CargoAudit
            .normalize(br#"{"database":{}}"#, &ctx(&SOURCE_WITH_LOCK))
            .expect_err("must be refused");
        assert!(matches!(err, ExecError::MalformedReport(_)));
        assert!(
            err.to_string().contains("no `vulnerabilities` object"),
            "{err}"
        );

        assert!(matches!(
            CargoAudit.normalize(b"<html>", &ctx(&SOURCE_WITH_LOCK)),
            Err(ExecError::Json(_))
        ));
    }

    #[test]
    fn refuses_an_entry_with_no_package() {
        let native = r#"{"vulnerabilities":{"list":[{"advisory":{"id":"R-1"}}]},"warnings":{}}"#;
        assert!(matches!(
            CargoAudit.normalize(native.as_bytes(), &ctx(&SOURCE_WITH_LOCK)),
            Err(ExecError::MalformedReport(_))
        ));
    }

    /// A warning kind this build has never heard of must be reported, not
    /// dropped: the set of `RustSec` informational kinds grows over time.
    #[test]
    fn an_unknown_warning_kind_is_reported_verbatim() {
        let native = r#"{"vulnerabilities":{"list":[]},"warnings":{
            "future-hazard":[{"package":{"name":"x","version":"1.0.0"}}]}}"#;
        let report = CargoAudit
            .normalize(native.as_bytes(), &ctx(&SOURCE_WITH_LOCK))
            .expect("parse");
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].rule, "future-hazard");
        assert_eq!(
            report.findings[0].severity,
            Severity::Other("future-hazard".to_owned())
        );
    }

    #[test]
    fn the_invocation_pins_the_database_and_refuses_to_refresh_it() {
        let entries = [(ADVISORY_DB_ASSET, std::path::PathBuf::from("/cache/db"))];
        let invocation = CargoAudit.command(&AssetPaths::new(&entries));
        assert_eq!(invocation.program, "cargo");
        assert_eq!(invocation.args[0], "audit");
        assert!(invocation.args.contains(&"--no-fetch".to_owned()));
        let db = invocation
            .args
            .iter()
            .position(|a| a == "--db")
            .map(|i| invocation.args[i + 1].clone())
            .expect("a --db argument");
        assert_eq!(db, "/cache/db");
        assert_eq!(invocation.success_statuses, vec![0, 1]);
    }

    #[test]
    fn covers_rust_dependencies_and_says_nothing_more() {
        assert_eq!(CargoAudit.languages(), &["rust"]);
        assert_eq!(CargoAudit.asset_ids(), &[ADVISORY_DB_ASSET]);
    }
}

//! `osv-scanner` — OSV.dev advisories against resolved dependency manifests.
//!
//! This is the analyzer that makes the dependency axis of the coverage matrix
//! (ADR-0018) match the SAST axis. `cargo-audit` reads `Cargo.lock` and nothing
//! else; `osv-scanner` reads the lockfiles of every ecosystem the project uses —
//! `requirements.txt`, `poetry.lock`, `package-lock.json`, `yarn.lock`,
//! `gradle.lockfile`, `pom.xml`, `Cargo.lock` and more — against one database
//! family with one output format.
//!
//! # One finding per *group*, not per vulnerability entry
//!
//! OSV.dev carries the same advisory under several ids: a Rust advisory arrives
//! as both `RUSTSEC-2020-0071` and `GHSA-wcg3-cvx6-7396`, and `osv-scanner`
//! lists **both** in `vulnerabilities`. It also resolves them itself, in
//! `groups`, where one entry names every id that is the same advisory.
//!
//! This adapter emits one finding per group. Emitting one per `vulnerabilities`
//! entry would double-count every advisory that GitHub has also assigned a GHSA
//! id to — a count that is wrong in the direction that looks like more work than
//! there is. The group's whole id and alias set is preserved in `meta.aliases`,
//! which is what the reporting layer's cross-reference joins on (see
//! [`crate::crossref`]).
//!
//! # Three things this tool does that its documentation does not say
//!
//! All three were found by running `osv-scanner` 2.5.0, and each would have been
//! a silent defect:
//!
//! 1. **Reported paths are absolute, even when the scan target is `.`.** Given
//!    `scan source --recursive .` with the working directory set to the
//!    worktree, every `results[].source.path` still comes back as a full
//!    filesystem path. Storing that verbatim would put the user's home directory
//!    into a persisted finding key — the same class of defect as semgrep's rule
//!    id rewriting (ADR-0018) — and [`crate::check_reported_path`] would refuse
//!    it besides. The path is made worktree-relative here, and a path that is
//!    not under the worktree keeps [`UNKNOWN_SOURCE`] rather than being invented.
//! 2. **`--offline-vulnerabilities` on its own consults no local database.**
//!    With only that flag the scanner reports **zero findings and exits `0`** —
//!    a clean bill of health produced by consulting nothing. The database is
//!    loaded only under `--offline` (or `--download-offline-databases`), so
//!    `--offline` is what this adapter passes, and it is not a stylistic choice.
//! 3. **A missing database under `--offline` fails loudly, and that is wanted.**
//!    `--offline --local-db-path <dir>` with no database there exits `127` with
//!    "no offline version of the OSV database is available". `127` is not in
//!    [`Invocation::success_statuses`], so the run fails instead of recording an
//!    empty result — which is the whole difference between this configuration
//!    and the one above.
//!
//! # Severity is a mapping, and it is the *same* mapping `cargo-audit` uses
//!
//! An OSV record publishes a qualitative level only sometimes: GitHub-sourced
//! records carry `database_specific.severity` (`LOW`/`MODERATE`/`HIGH`/
//! `CRITICAL`), and `RustSec`-sourced records carry
//! `affected[].database_specific.informational` (`unmaintained`, `unsound`,
//! `notice`). Both are mapped here, and the informational mapping is
//! deliberately identical to [`crate::adapter::cargo_audit`]'s — so when the two
//! analyzers report the same Rust advisory, the cross-reference shows one
//! advisory at one severity rather than two that disagree about it.
//!
//! A record with no published level at all is a vulnerability that a curated
//! database chose to publish, so it is graded [`Severity::High`] on the same
//! reasoning `cargo-audit` grades `vulnerability` high. The raw CVSS vectors and
//! the scanner's own `max_severity` score are preserved verbatim in `meta`;
//! computing a base score is not done here for the reason ADR-0018 gives.
//!
//! @rto:0012
//! @rto:0014
//! @rto:0018

use std::path::Path;

use serde::Deserialize;

use crate::adapter::{Adapter, AssetPaths, InstallHint, Invocation, NativeContext};
use crate::guidance::{Guidance, Line};
use crate::ingest::{NormalizedReport, REPORT_SCHEMA, ReportFinding};
use crate::runner::ExecError;
use rto_graph::Severity;

/// The analyzer id, and the first component of every finding key it produces.
pub const ANALYZER: &str = "osv-scanner";

/// Asset id of the pinned per-ecosystem OSV databases.
pub const DB_ASSET: &str = "osv-db";

/// How to obtain `osv-scanner`, for the refusal that finds it absent.
///
/// The one hint with **no command**, and that is the honest answer rather than a
/// gap. Upstream's install page offers scoop, winget, brew, pacman, apk, pkg,
/// `pkg_add`, a prebuilt SLSA3 binary and `go install`, and ranks none of them:
/// eight of those are the reader's-package-manager guess the refusals checklist
/// forbids, and the ninth needs a Go toolchain this reader has no reason to
/// have. So the page is the way forward, and a command invented to fill the slot
/// would be the plausible-but-wrong answer that costs an hour before anyone
/// doubts it.
const INSTALL_HINTS: &[InstallHint] = &[InstallHint {
    program: "osv-scanner",
    guidance: Guidance::new(&[
        Line::Note(&[
            "Roteiro does not install analyzers, and has not installed this one.",
            "osv-scanner documents no single install command — its page lists a",
            "prebuilt binary and one entry per platform, so choose from:",
        ]),
        Line::Command("https://google.github.io/osv-scanner/installation/"),
    ]),
}];

/// Stands in for the manifest path in a finding's identity when the reported
/// path could not be placed inside the worktree.
///
/// An `osv-scanner` finding is a claim about *a package version resolved by a
/// particular manifest*, so the manifest is part of its identity — two lockfiles
/// in one repository can pin the same vulnerable version, and those are two
/// findings, not one. When the reported path is not under the worktree the
/// identity stays well-formed and says so, rather than keying on an absolute
/// path that would differ between machines.
pub const UNKNOWN_SOURCE: &str = "unknown-source";

/// The adapter.
#[derive(Debug, Clone, Copy)]
pub struct OsvScanner;

impl Adapter for OsvScanner {
    fn analyzer(&self) -> &'static str {
        ANALYZER
    }

    fn summary(&self) -> &'static str {
        "OSV.dev advisories against resolved lockfiles (Python, Java, Node, Rust dependencies)"
    }

    fn languages(&self) -> &'static [&'static str] {
        // The ecosystems this build provisions a database for. `osv-scanner`
        // supports more; claiming them here would claim coverage the pinned
        // asset does not provide, since a run consults only the databases that
        // were prefetched.
        &["python", "java", "javascript", "typescript", "rust"]
    }

    fn asset_ids(&self) -> &'static [&'static str] {
        &[DB_ASSET]
    }

    fn host_programs(&self) -> &'static [&'static str] {
        &["osv-scanner"]
    }

    fn install_hints(&self) -> &'static [InstallHint] {
        INSTALL_HINTS
    }

    fn command(&self, assets: &AssetPaths<'_>) -> Invocation {
        Invocation {
            program: "osv-scanner".to_owned(),
            args: vec![
                "scan".to_owned(),
                "source".to_owned(),
                // Egress configured off, and — unlike
                // `--offline-vulnerabilities` alone — this is the flag that
                // actually makes the pinned local database be consulted. See
                // the module docs; the difference between the two is a silent
                // empty result.
                "--offline".to_owned(),
                "--local-db-path".to_owned(),
                assets.arg(DB_ASSET),
                "--format".to_owned(),
                "json".to_owned(),
                "--recursive".to_owned(),
                // An explicit target. Without one the scanner starts its
                // filesystem walk at the root of the filesystem rather than at
                // the working directory.
                ".".to_owned(),
            ],
            // 0 = clean, 1 = vulnerabilities found. Everything else — including
            // the 127 a missing database produces — is a failed scan, and a
            // failed scan must not be stored as a clean one.
            success_statuses: vec![0, 1],
        }
    }

    fn normalize(
        &self,
        native: &[u8],
        ctx: &NativeContext<'_>,
    ) -> Result<NormalizedReport, ExecError> {
        let output: ScanOutput = serde_json::from_slice(native)?;
        let Some(results) = output.results else {
            return Err(ExecError::MalformedReport(
                "not an osv-scanner report: no `results` array".to_owned(),
            ));
        };

        let mut findings = Vec::new();
        for result in &results {
            let source = source_component(result.source.path.as_deref(), ctx.worktree);
            for entry in &result.packages {
                convert_package(entry, &source, &mut findings)?;
            }
        }

        Ok(NormalizedReport {
            schema: REPORT_SCHEMA.to_owned(),
            analyzer: ANALYZER.to_owned(),
            // `osv-scanner --format json` carries no version field, so a report
            // ingested from CI records "unknown" unless the caller learned the
            // version another way (a subprocess run asks the binary).
            analyzer_version: ctx.version_or(None),
            started_at: ctx.started_at.clone(),
            ended_at: ctx.ended_at.clone(),
            exit_status: ctx.exit_status,
            // Rules are not a thing for osv-scanner; the databases are.
            rules_digest: None,
            image_digest: None,
            // The scanner never reports which database snapshot it read, so the
            // caller's provisioning record is the only staleness evidence there
            // is — the same position `cargo audit --db` leaves us in.
            advisory_db: ctx.advisory_db.clone(),
            source: ctx.source.clone(),
            findings,
        })
    }
}

/// One scanned package's groups → findings, appended to `into`.
fn convert_package(
    entry: &PackageEntry,
    source: &str,
    into: &mut Vec<ReportFinding>,
) -> Result<(), ExecError> {
    let Some(package) = entry.package.as_ref() else {
        return Err(ExecError::MalformedReport(
            "an osv-scanner package entry has no `package` object".to_owned(),
        ));
    };
    if package.name.trim().is_empty() {
        return Err(ExecError::MalformedReport(
            "an osv-scanner package entry has an unnamed package".to_owned(),
        ));
    }
    let version = if package.version.trim().is_empty() {
        "unknown-version"
    } else {
        package.version.trim()
    };
    let ecosystem = if package.ecosystem.trim().is_empty() {
        "unknown-ecosystem"
    } else {
        package.ecosystem.trim()
    };

    for group in groups_of(entry) {
        // Deterministic representative: the group's ids sorted, first one. The
        // choice never affects whether two findings cross-reference, because
        // that join is over the whole alias set rather than this one id.
        let mut ids: Vec<&str> = group
            .ids
            .iter()
            .map(|id| id.trim())
            .filter(|id| !id.is_empty())
            .collect();
        ids.sort_unstable();
        ids.dedup();
        let Some(&rule) = ids.first() else {
            return Err(ExecError::MalformedReport(
                "an osv-scanner group names no advisory id".to_owned(),
            ));
        };

        // Every vulnerability entry the group covers, so severity and prose come
        // from all the ids for this advisory rather than from whichever one the
        // scanner happened to list first.
        let members: Vec<&Vulnerability> = entry
            .vulnerabilities
            .iter()
            .filter(|v| ids.contains(&v.id.trim()))
            .collect();

        let aliases = alias_set(&group, &members);
        let title = members
            .iter()
            .filter_map(|v| v.summary.as_deref())
            .map(str::trim)
            .find(|s| !s.is_empty())
            .map_or_else(
                || format!("{} {version} is affected by {rule}", package.name),
                str::to_owned,
            );
        let message = members
            .iter()
            .filter_map(|v| v.details.as_deref())
            .map(str::trim)
            .find(|s| !s.is_empty())
            .unwrap_or_default()
            .to_owned();

        into.push(ReportFinding {
            // Advisory, ecosystem, package, version, manifest — the same shape
            // as `cargo-audit`'s recipe, with the ecosystem added because
            // `osv-scanner` reads more than one.
            identity: vec![
                rule.to_owned(),
                ecosystem.to_owned(),
                package.name.clone(),
                version.to_owned(),
                source.to_owned(),
            ],
            rule: rule.to_owned(),
            severity: severity(&members),
            title,
            message,
            // The claim is about a resolved dependency; the manifest that
            // resolved it is the file that decides it.
            path: (source != UNKNOWN_SOURCE).then(|| source.to_owned()),
            span: None,
            meta: serde_json::json!({
                "ecosystem": ecosystem,
                "package": package.name,
                "version": version,
                // Every id and alias for this advisory. The cross-reference in
                // `crate::crossref` joins on this set, so it is the load-bearing
                // field rather than a decoration.
                "aliases": aliases,
                "ids": ids,
                // The scanner's own CVSS base score, verbatim and unparsed. An
                // empty string is what it reports for an advisory with no score.
                "max_severity": group.max_severity,
                // RustSec's informational kind, where OSV carried one through.
                "informational": informational(&members),
                "cvss": cvss_vectors(&members),
                "withdrawn": members.iter().find_map(|v| v.withdrawn.clone()),
                // The manifest this claim came from, as it appears in the
                // identity — relative where it could be placed in the worktree,
                // and `UNKNOWN_SOURCE` where it could not.
                "source": source,
            }),
        });
    }
    Ok(())
}

/// The groups to convert: the scanner's own, or one per vulnerability when it
/// reported none.
///
/// `groups` is how `osv-scanner` says "these ids are the same advisory". A build
/// or a version that omits it must still produce findings rather than silently
/// nothing, so each vulnerability becomes its own single-id group.
fn groups_of(entry: &PackageEntry) -> Vec<Group> {
    if !entry.groups.is_empty() {
        return entry.groups.clone();
    }
    entry
        .vulnerabilities
        .iter()
        .map(|v| Group {
            ids: vec![v.id.clone()],
            aliases: v.aliases.clone(),
            max_severity: String::new(),
        })
        .collect()
}

/// Every identifier this advisory is known by, sorted and deduplicated: the
/// group's ids, the group's aliases, and each member record's own aliases.
fn alias_set(group: &Group, members: &[&Vulnerability]) -> Vec<String> {
    let mut all: Vec<String> = group
        .ids
        .iter()
        .chain(group.aliases.iter())
        .chain(members.iter().flat_map(|v| v.aliases.iter()))
        .chain(members.iter().map(|v| &v.id))
        .map(|id| id.trim().to_owned())
        .filter(|id| !id.is_empty())
        .collect();
    all.sort();
    all.dedup();
    all
}

/// The `RustSec` informational kind carried through by OSV, if any member has one.
fn informational(members: &[&Vulnerability]) -> Option<String> {
    members
        .iter()
        .flat_map(|v| v.affected.iter())
        .filter_map(|a| a.database_specific.as_ref())
        .filter_map(|d| d.informational.as_deref())
        .map(str::trim)
        .find(|k| !k.is_empty())
        .map(str::to_owned)
}

/// Every CVSS vector the members publish, verbatim and unscored.
fn cvss_vectors(members: &[&Vulnerability]) -> Vec<String> {
    let mut out: Vec<String> = members
        .iter()
        .flat_map(|v| v.severity.iter())
        .map(|s| s.score.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The severity for a group: the highest any of its records publishes.
///
/// The informational arm is deliberately the same mapping
/// [`crate::adapter::cargo_audit`] applies, so the two analyzers agree about a
/// Rust advisory they both report.
fn severity(members: &[&Vulnerability]) -> Severity {
    let mut best: Option<Severity> = None;
    for member in members {
        for level in member_levels(member) {
            if best.as_ref().is_none_or(|b| rank(&level) > rank(b)) {
                best = Some(level);
            }
        }
    }
    // A curated database published this record and gave it no qualitative level.
    // That is a vulnerability, and it is graded on the same reasoning
    // `cargo-audit` grades RustSec's `vulnerability` kind high.
    best.unwrap_or(Severity::High)
}

/// Every qualitative level one record publishes.
fn member_levels(member: &Vulnerability) -> Vec<Severity> {
    let mut levels = Vec::new();
    if let Some(token) = member
        .database_specific
        .as_ref()
        .and_then(|d| d.severity.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        levels.push(from_github(token));
    }
    for affected in &member.affected {
        if let Some(kind) = affected
            .database_specific
            .as_ref()
            .and_then(|d| d.informational.as_deref())
            .map(str::trim)
            .filter(|k| !k.is_empty())
        {
            levels.push(from_informational(kind));
        }
    }
    levels
}

/// GitHub's qualitative severity, which OSV carries verbatim.
fn from_github(token: &str) -> Severity {
    match token.to_ascii_lowercase().as_str() {
        "critical" => Severity::Critical,
        "high" => Severity::High,
        // GitHub's middle level is "moderate"; Roteiro's is "medium".
        "moderate" | "medium" => Severity::Medium,
        "low" => Severity::Low,
        other => Severity::from_token(other),
    }
}

/// `RustSec`'s informational kind — the same mapping `cargo-audit` uses.
fn from_informational(kind: &str) -> Severity {
    match kind {
        "unsound" => Severity::Medium,
        "unmaintained" | "yanked" => Severity::Low,
        "notice" => Severity::Info,
        other => Severity::from_token(other),
    }
}

/// Order the levels so "the highest of these" has an answer. A level nobody
/// assigned ranks below every level somebody did.
fn rank(severity: &Severity) -> u8 {
    match severity {
        Severity::Critical => 5,
        Severity::High => 4,
        Severity::Medium => 3,
        Severity::Low => 2,
        Severity::Info => 1,
        Severity::Other(_) => 0,
    }
}

/// The manifest path as a worktree-relative identity component.
///
/// `osv-scanner` reports absolute paths even when told to scan `.`, so this is
/// what keeps the user's home directory out of a stored finding key. A path that
/// is not under the worktree becomes [`UNKNOWN_SOURCE`]: guessing at a relative
/// form would invent a location the scan never described.
fn source_component(reported: Option<&str>, worktree: Option<&Path>) -> String {
    let Some(reported) = reported.map(str::trim).filter(|p| !p.is_empty()) else {
        return UNKNOWN_SOURCE.to_owned();
    };
    let path = Path::new(reported);
    if path.is_relative() {
        return normalise(reported);
    }
    let Some(worktree) = worktree else {
        return UNKNOWN_SOURCE.to_owned();
    };
    // The worktree path is taken as the caller supplied it and, failing that, in
    // canonical form: on macOS a checkout under `/tmp` is reported back under
    // `/private/tmp`, and those are the same directory.
    let candidates = [
        Some(worktree.to_path_buf()),
        std::fs::canonicalize(worktree).ok(),
    ];
    for candidate in candidates.into_iter().flatten() {
        if let Ok(relative) = path.strip_prefix(&candidate) {
            let relative = relative.to_string_lossy();
            if !relative.is_empty() {
                return normalise(&relative);
            }
        }
    }
    UNKNOWN_SOURCE.to_owned()
}

/// Separators as the store records them, on every platform.
fn normalise(path: &str) -> String {
    path.replace('\\', "/")
}

/// The shape of `osv-scanner --format json`, narrowed to what is needed. Unknown
/// fields are ignored so an `osv-scanner` upgrade that adds keys does not break
/// ingest.
#[derive(Debug, Deserialize)]
struct ScanOutput {
    /// Absent rather than empty distinguishes "not an osv-scanner report" from
    /// "a clean scan".
    #[serde(default)]
    results: Option<Vec<ScanResult>>,
}

#[derive(Debug, Deserialize)]
struct ScanResult {
    #[serde(default)]
    source: Source,
    #[serde(default)]
    packages: Vec<PackageEntry>,
}

#[derive(Debug, Default, Deserialize)]
struct Source {
    #[serde(default)]
    path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PackageEntry {
    #[serde(default)]
    package: Option<PackageId>,
    #[serde(default)]
    vulnerabilities: Vec<Vulnerability>,
    #[serde(default)]
    groups: Vec<Group>,
}

#[derive(Debug, Deserialize)]
struct PackageId {
    #[serde(default)]
    name: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    ecosystem: String,
}

#[derive(Debug, Clone, Deserialize)]
struct Group {
    #[serde(default)]
    ids: Vec<String>,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    max_severity: String,
}

#[derive(Debug, Deserialize)]
struct Vulnerability {
    #[serde(default)]
    id: String,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    details: Option<String>,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    withdrawn: Option<String>,
    #[serde(default)]
    severity: Vec<SeverityScore>,
    #[serde(default)]
    database_specific: Option<DatabaseSpecific>,
    #[serde(default)]
    affected: Vec<Affected>,
}

#[derive(Debug, Deserialize)]
struct SeverityScore {
    #[serde(default)]
    score: String,
}

#[derive(Debug, Deserialize)]
struct DatabaseSpecific {
    #[serde(default)]
    severity: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Affected {
    #[serde(default)]
    database_specific: Option<AffectedDatabaseSpecific>,
}

#[derive(Debug, Deserialize)]
struct AffectedDatabaseSpecific {
    #[serde(default)]
    informational: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{ANALYZER, DB_ASSET, OsvScanner, UNKNOWN_SOURCE, source_component};
    use crate::adapter::{Adapter, AssetPaths, NativeContext};
    use crate::runner::ExecError;
    use rto_graph::{Severity, SourceIdentity};

    static SOURCE: std::sync::LazyLock<SourceIdentity> =
        std::sync::LazyLock::new(SourceIdentity::default);

    fn ctx(worktree: Option<&'static str>) -> NativeContext<'static> {
        NativeContext {
            started_at: "2026-08-16T09:00:00Z".to_owned(),
            ended_at: "2026-08-16T09:00:06Z".to_owned(),
            analyzer_version: Some("2.5.0".to_owned()),
            exit_status: 1,
            source: &SOURCE,
            rules_digest: None,
            advisory_db: None,
            worktree: worktree.map(std::path::Path::new),
            snippets: &crate::snippet::NoSnippets,
        }
    }

    /// Trimmed from a real `osv-scanner` 2.5.0 offline run. The `openssl` entry
    /// is the load-bearing one: the same advisory appears twice, as
    /// `RUSTSEC-2023-0072` and as `GHSA-xphf-cx8h-7q9g`, and `groups` says they
    /// are one thing.
    const NATIVE: &str = r#"{
      "results": [
        {
          "source": {"path": "/repo/Cargo.lock", "type": "lockfile"},
          "packages": [
            {
              "package": {"name": "openssl", "version": "0.10.55", "ecosystem": "crates.io"},
              "vulnerabilities": [
                {
                  "id": "RUSTSEC-2023-0072",
                  "summary": "`openssl` `X509StoreRef::objects` is unsound",
                  "details": "The objects method is unsound.",
                  "aliases": ["GHSA-xphf-cx8h-7q9g"],
                  "database_specific": {"license": "CC0-1.0"},
                  "affected": [{"database_specific": {"informational": "unsound", "cvss": null}}]
                },
                {
                  "id": "GHSA-xphf-cx8h-7q9g",
                  "summary": "`openssl` `X509StoreRef::objects` is unsound",
                  "aliases": ["RUSTSEC-2023-0072"],
                  "database_specific": {"severity": "MODERATE"},
                  "affected": [{"database_specific": {}}]
                }
              ],
              "groups": [
                {
                  "ids": ["RUSTSEC-2023-0072", "GHSA-xphf-cx8h-7q9g"],
                  "aliases": ["GHSA-xphf-cx8h-7q9g", "RUSTSEC-2023-0072"],
                  "max_severity": ""
                }
              ]
            },
            {
              "package": {"name": "derivative", "version": "2.2.0", "ecosystem": "crates.io"},
              "vulnerabilities": [
                {
                  "id": "RUSTSEC-2024-0388",
                  "summary": "`derivative` is unmaintained; consider using an alternative",
                  "database_specific": {"license": "CC0-1.0"},
                  "affected": [{"database_specific": {"informational": "unmaintained"}}]
                }
              ],
              "groups": [
                {"ids": ["RUSTSEC-2024-0388"], "aliases": ["RUSTSEC-2024-0388"], "max_severity": ""}
              ]
            }
          ]
        },
        {
          "source": {"path": "/repo/app/package-lock.json", "type": "lockfile"},
          "packages": [
            {
              "package": {"name": "lodash", "version": "4.17.15", "ecosystem": "npm"},
              "vulnerabilities": [
                {
                  "id": "GHSA-p6mc-m468-83gw",
                  "summary": "Prototype Pollution in lodash",
                  "details": "Versions prior to 4.17.21 are vulnerable.",
                  "aliases": ["CVE-2020-8203"],
                  "severity": [{"type": "CVSS_V3", "score": "CVSS:3.1/AV:N/AC:H/PR:N/UI:N/S:U/C:N/I:H/A:N"}],
                  "database_specific": {"severity": "HIGH"},
                  "affected": [{"database_specific": {}}]
                }
              ],
              "groups": [
                {
                  "ids": ["GHSA-p6mc-m468-83gw"],
                  "aliases": ["CVE-2020-8203", "GHSA-p6mc-m468-83gw"],
                  "max_severity": "7.4"
                }
              ]
            }
          ]
        }
      ]
    }"#;

    /// The headline behaviour: the `openssl` advisory is listed twice by the
    /// scanner and becomes **one** finding, because `groups` already says the
    /// two ids are the same advisory. One finding per vulnerability entry would
    /// double every advisory GitHub has also assigned a GHSA id to.
    #[test]
    fn a_duplicated_advisory_becomes_one_finding_not_two() {
        let report = OsvScanner
            .normalize(NATIVE.as_bytes(), &ctx(Some("/repo")))
            .expect("parse");
        assert_eq!(report.analyzer, ANALYZER);
        assert_eq!(report.findings.len(), 3, "openssl, derivative, lodash");
        let openssl: Vec<_> = report
            .findings
            .iter()
            .filter(|f| f.meta["package"] == "openssl")
            .collect();
        assert_eq!(openssl.len(), 1);
        // …and both ids remain addressable on the one finding.
        let aliases = openssl[0].meta["aliases"].as_array().expect("aliases");
        assert!(aliases.iter().any(|a| a == "RUSTSEC-2023-0072"));
        assert!(aliases.iter().any(|a| a == "GHSA-xphf-cx8h-7q9g"));
    }

    #[test]
    fn uses_the_advisory_ecosystem_package_version_manifest_identity() {
        let report = OsvScanner
            .normalize(NATIVE.as_bytes(), &ctx(Some("/repo")))
            .expect("parse");
        let lodash = report
            .findings
            .iter()
            .find(|f| f.meta["package"] == "lodash")
            .expect("lodash");
        assert_eq!(
            lodash.identity,
            vec![
                "GHSA-p6mc-m468-83gw",
                "npm",
                "lodash",
                "4.17.15",
                "app/package-lock.json"
            ]
        );
        assert_eq!(lodash.path.as_deref(), Some("app/package-lock.json"));
        assert_eq!(lodash.severity, Severity::High);
        assert_eq!(lodash.meta["max_severity"], "7.4");
    }

    /// The scanner reports absolute paths even when the target is `.`. Storing
    /// one verbatim would put the user's home directory into a persisted finding
    /// key — and the shared preflight would refuse it besides.
    #[test]
    fn an_absolute_reported_path_is_made_worktree_relative() {
        let report = OsvScanner
            .normalize(NATIVE.as_bytes(), &ctx(Some("/repo")))
            .expect("parse");
        for finding in &report.findings {
            let path = finding.path.as_deref().expect("a path");
            assert!(!path.starts_with('/'), "{path} is still absolute");
            crate::runner::check_reported_path(path).expect("must pass the preflight");
        }
    }

    /// A report about a tree this checkout does not have still yields a
    /// well-formed identity, and one that says the location is unknown rather
    /// than inventing a relative path.
    #[test]
    fn a_path_outside_the_worktree_is_named_not_guessed() {
        let report = OsvScanner
            .normalize(NATIVE.as_bytes(), &ctx(Some("/elsewhere")))
            .expect("parse");
        assert!(report.findings.iter().all(|f| f.path.is_none()));
        assert!(
            report
                .findings
                .iter()
                .all(|f| f.identity[4] == UNKNOWN_SOURCE)
        );
    }

    #[test]
    fn source_components_cover_relative_absolute_and_unknown() {
        let repo = std::path::Path::new("/repo");
        assert_eq!(
            source_component(Some("/repo/a/Cargo.lock"), Some(repo)),
            "a/Cargo.lock"
        );
        assert_eq!(
            source_component(Some("a/Cargo.lock"), Some(repo)),
            "a/Cargo.lock"
        );
        assert_eq!(
            source_component(Some("/other/Cargo.lock"), Some(repo)),
            UNKNOWN_SOURCE
        );
        assert_eq!(source_component(Some("/repo/x"), None), UNKNOWN_SOURCE);
        assert_eq!(source_component(None, Some(repo)), UNKNOWN_SOURCE);
        assert_eq!(source_component(Some("   "), Some(repo)), UNKNOWN_SOURCE);
    }

    /// The informational mapping is the one `cargo-audit` uses, so the two
    /// analyzers do not disagree about a Rust advisory they both report.
    #[test]
    fn informational_kinds_are_graded_the_way_cargo_audit_grades_them() {
        let report = OsvScanner
            .normalize(NATIVE.as_bytes(), &ctx(Some("/repo")))
            .expect("parse");
        let unsound = report
            .findings
            .iter()
            .find(|f| f.meta["package"] == "openssl")
            .expect("openssl");
        assert_eq!(unsound.severity, Severity::Medium);
        assert_eq!(unsound.meta["informational"], "unsound");

        let unmaintained = report
            .findings
            .iter()
            .find(|f| f.meta["package"] == "derivative")
            .expect("derivative");
        assert_eq!(unmaintained.severity, Severity::Low);
        assert_eq!(unmaintained.meta["informational"], "unmaintained");
    }

    /// A record a curated database published with no qualitative level at all is
    /// still a vulnerability, and is graded on the same reasoning `cargo-audit`
    /// grades `RustSec`'s `vulnerability` kind high.
    #[test]
    fn an_advisory_with_no_published_level_is_graded_high() {
        let native = r#"{"results":[{"source":{"path":"/repo/Cargo.lock"},"packages":[
            {"package":{"name":"x","version":"1.0.0","ecosystem":"crates.io"},
             "vulnerabilities":[{"id":"OSV-1","summary":"bad"}],
             "groups":[{"ids":["OSV-1"],"aliases":[],"max_severity":""}]}]}]}"#;
        let report = OsvScanner
            .normalize(native.as_bytes(), &ctx(Some("/repo")))
            .expect("parse");
        assert_eq!(report.findings[0].severity, Severity::High);
    }

    /// A group whose records disagree takes the highest level any of them
    /// publishes, so a cross-referenced pair does not read as two severities.
    #[test]
    fn a_group_takes_the_highest_level_its_records_publish() {
        let native = r#"{"results":[{"source":{"path":"/repo/p.json"},"packages":[
            {"package":{"name":"x","version":"1.0.0","ecosystem":"npm"},
             "vulnerabilities":[
               {"id":"A-1","summary":"a","database_specific":{"severity":"LOW"}},
               {"id":"B-1","summary":"b","database_specific":{"severity":"CRITICAL"}}],
             "groups":[{"ids":["A-1","B-1"],"aliases":[],"max_severity":"9.8"}]}]}]}"#;
        let report = OsvScanner
            .normalize(native.as_bytes(), &ctx(Some("/repo")))
            .expect("parse");
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].severity, Severity::Critical);
    }

    /// Two manifests in one repository can pin the same vulnerable version. They
    /// are two findings, and the manifest in the identity is what keeps them
    /// distinct rather than colliding into one key.
    #[test]
    fn the_same_advisory_in_two_manifests_is_two_findings() {
        let native = r#"{"results":[
            {"source":{"path":"/repo/a/package-lock.json"},"packages":[
              {"package":{"name":"lodash","version":"4.17.15","ecosystem":"npm"},
               "vulnerabilities":[{"id":"G-1","summary":"pollution"}],
               "groups":[{"ids":["G-1"],"aliases":[],"max_severity":""}]}]},
            {"source":{"path":"/repo/b/package-lock.json"},"packages":[
              {"package":{"name":"lodash","version":"4.17.15","ecosystem":"npm"},
               "vulnerabilities":[{"id":"G-1","summary":"pollution"}],
               "groups":[{"ids":["G-1"],"aliases":[],"max_severity":""}]}]}]}"#;
        let report = OsvScanner
            .normalize(native.as_bytes(), &ctx(Some("/repo")))
            .expect("parse");
        assert_eq!(report.findings.len(), 2);
        assert_ne!(report.findings[0].identity, report.findings[1].identity);
    }

    /// A version of the scanner that reports no `groups` must still produce
    /// findings — silently nothing is the one answer a security tool may not
    /// give.
    #[test]
    fn vulnerabilities_without_groups_are_still_reported() {
        let native = r#"{"results":[{"source":{"path":"/repo/req.txt"},"packages":[
            {"package":{"name":"django","version":"2.2.0","ecosystem":"PyPI"},
             "vulnerabilities":[{"id":"PYSEC-2019-10","summary":"sql injection"}]}]}]}"#;
        let report = OsvScanner
            .normalize(native.as_bytes(), &ctx(Some("/repo")))
            .expect("parse");
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].rule, "PYSEC-2019-10");
    }

    #[test]
    fn a_clean_scan_is_a_valid_empty_report() {
        let report = OsvScanner
            .normalize(br#"{"results":[]}"#, &ctx(Some("/repo")))
            .expect("parse");
        assert!(report.findings.is_empty());
    }

    #[test]
    fn refuses_output_that_is_not_an_osv_scanner_report() {
        let err = OsvScanner
            .normalize(br#"{"experimental_config":{}}"#, &ctx(Some("/repo")))
            .expect_err("must be refused");
        assert!(matches!(err, ExecError::MalformedReport(_)));
        assert!(err.to_string().contains("no `results` array"), "{err}");

        assert!(matches!(
            OsvScanner.normalize(b"<html>", &ctx(Some("/repo"))),
            Err(ExecError::Json(_))
        ));
    }

    #[test]
    fn refuses_a_package_entry_with_no_package() {
        let native = r#"{"results":[{"source":{"path":"/repo/x"},"packages":[
            {"vulnerabilities":[{"id":"A"}]}]}]}"#;
        assert!(matches!(
            OsvScanner.normalize(native.as_bytes(), &ctx(Some("/repo"))),
            Err(ExecError::MalformedReport(_))
        ));
    }

    /// `--offline-vulnerabilities` alone consults no database and reports a
    /// clean scan; `--offline` is what actually loads the pinned one. Verified
    /// against osv-scanner 2.5.0 — see the module docs.
    #[test]
    fn the_invocation_pins_the_database_and_really_goes_offline() {
        let entries = [(DB_ASSET, std::path::PathBuf::from("/cache/osv"))];
        let invocation = OsvScanner.command(&AssetPaths::new(&entries));
        assert_eq!(invocation.program, "osv-scanner");
        assert_eq!(invocation.args[0], "scan");
        assert_eq!(invocation.args[1], "source");
        assert!(invocation.args.contains(&"--offline".to_owned()));
        assert!(
            !invocation
                .args
                .contains(&"--offline-vulnerabilities".to_owned()),
            "that flag alone consults nothing and reports a clean scan"
        );
        let db = invocation
            .args
            .iter()
            .position(|a| a == "--local-db-path")
            .map(|i| invocation.args[i + 1].clone())
            .expect("a --local-db-path argument");
        assert_eq!(db, "/cache/osv");
        // An explicit target: without one the walk starts at the filesystem root.
        assert_eq!(invocation.args.last().map(String::as_str), Some("."));
        // 127 (no database) must not read as a completed scan.
        assert_eq!(invocation.success_statuses, vec![0, 1]);
    }

    #[test]
    fn covers_the_dependency_axis_for_the_ecosystems_it_provisions() {
        assert!(OsvScanner.languages().contains(&"python"));
        assert!(OsvScanner.languages().contains(&"java"));
        assert!(OsvScanner.languages().contains(&"javascript"));
        assert_eq!(OsvScanner.asset_ids(), &[DB_ASSET]);
    }
}

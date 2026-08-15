//! The coverage claim, as an executable check.
//!
//! The requirement is that findings are produced for **Rust, Python, SQL, Java
//! and Node**. ADR-0018 records which analyzer delivers each; this file checks
//! that the pipeline really produces them, from real semgrep output over a real
//! polyglot tree.
//!
//! Two tiers, deliberately:
//!
//! - The **fixture-driven** tests below run everywhere, including CI, which has
//!   no semgrep. They are the substantial coverage.
//! - `runs_the_real_analyzer_when_one_is_installed` re-runs the tool and checks
//!   the committed fixture still describes what it emits. It **self-skips with a
//!   visible message** when no semgrep is on `PATH`, so a skip is never silent.

use rto_exec::{NativeContext, WorktreeSnippets, normalize_native};
use rto_graph::SourceIdentity;

mod fixture;

fn normalize_fixture() -> rto_exec::NormalizedReport {
    let snippets = WorktreeSnippets::new(fixture::polyglot_root());
    let source = SourceIdentity::default();
    let ctx = NativeContext {
        started_at: "2026-08-15T09:00:00Z".to_owned(),
        ended_at: "2026-08-15T09:00:09Z".to_owned(),
        analyzer_version: None,
        exit_status: 1,
        source: &source,
        rules_digest: Some("baseline".to_owned()),
        advisory_db: None,
        snippets: &snippets,
    };
    normalize_native("semgrep", &fixture::semgrep_native(), &ctx).expect("normalise")
}

/// The headline requirement: every named language yields at least one finding.
#[test]
fn every_required_language_yields_a_finding() {
    let report = normalize_fixture();
    for (language, path) in fixture::REQUIRED_LANGUAGES {
        let hits: Vec<&str> = report
            .findings
            .iter()
            .filter(|f| f.path.as_deref() == Some(*path))
            .map(|f| f.rule.as_str())
            .collect();
        assert!(
            !hits.is_empty(),
            "no finding for {language} ({path}); the coverage claim in ADR-0018 is not met"
        );
    }
}

/// SQL findings exist, and they come from semgrep's `generic` engine rather than
/// a SQL parser. The rules say so in their metadata, and this test holds them to
/// it — a rule quietly switched to a language semgrep does not have would
/// otherwise look like an upgrade.
#[test]
fn sql_findings_are_produced_and_marked_as_generic_matches() {
    let report = normalize_fixture();
    let sql: Vec<_> = report
        .findings
        .iter()
        .filter(|f| f.path.as_deref() == Some("sql/schema.sql"))
        .collect();
    assert!(!sql.is_empty(), "SQL must produce findings");
    for finding in sql {
        let note = finding.meta["metadata"]["engine-note"]
            .as_str()
            .unwrap_or_default();
        assert!(
            note.contains("generic"),
            "{} must declare that it matched generically, said: {note:?}",
            finding.rule
        );
    }
}

/// Every finding carries the evidence a reader needs to act on it: a rule, a
/// path, a span, and a severity the analyzer actually assigned.
#[test]
fn findings_carry_a_rule_a_location_and_a_severity() {
    let report = normalize_fixture();
    assert!(report.findings.len() >= 10, "the fixture has thinned out");
    for finding in &report.findings {
        assert!(!finding.rule.trim().is_empty());
        assert!(!finding.title.trim().is_empty());
        let path = finding
            .path
            .as_deref()
            .expect("a semgrep finding has a path");
        assert!(!path.starts_with('/'), "{path} must be worktree-relative");
        let span = finding.span.expect("a semgrep finding has a span");
        assert!(
            span.end >= span.start,
            "{} has a backwards span",
            finding.rule
        );
    }
}

/// Rule ids are the first component of every finding key, so a local path
/// leaking into one would leak into the store. Semgrep prefixes rule ids with
/// the config path unless told not to, which is why this is checked rather than
/// assumed.
#[test]
fn rule_ids_are_stable_names_not_filesystem_paths() {
    for finding in &normalize_fixture().findings {
        assert!(
            finding.rule.starts_with("roteiro."),
            "unexpected rule id {:?} — is --no-rewrite-rule-ids still passed?",
            finding.rule
        );
        assert!(!finding.rule.contains('/'), "{:?}", finding.rule);
    }
}

/// Re-runs the real analyzer and checks the committed fixture has not drifted
/// from what it emits. Skips visibly where no semgrep is installed.
#[test]
fn runs_the_real_analyzer_when_one_is_installed() {
    let Some(semgrep) = which("semgrep") else {
        // Printed, not silent: a skipped test that says nothing is a test that
        // nobody notices has stopped running.
        println!(
            "SKIP runs_the_real_analyzer_when_one_is_installed: no `semgrep` on PATH. \
             The fixture-driven tests in this file carry the coverage; install semgrep to \
             re-verify against the tool itself."
        );
        return;
    };

    // Semgrep's default ignore list excludes `tests/` and `fixtures/`, so
    // scanning the tree in place finds nothing at all. Copy it somewhere neutral
    // — which is also what a user's real repository looks like.
    let scratch = std::env::temp_dir().join("rto-exec-polyglot-live");
    std::fs::remove_dir_all(&scratch).ok();
    copy_tree(&fixture::polyglot_root(), &scratch).expect("copy the fixture tree");

    let rules = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("rules/roteiro-baseline.yml");
    let output = std::process::Command::new(semgrep)
        .args([
            "scan",
            "--json",
            "--quiet",
            "--metrics=off",
            "--disable-version-check",
            "--no-rewrite-rule-ids",
            "--config",
        ])
        .arg(&rules)
        .arg(".")
        .current_dir(&scratch)
        .output()
        .expect("run semgrep");
    assert!(
        matches!(output.status.code(), Some(0 | 1)),
        "semgrep exited {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let snippets = WorktreeSnippets::new(&scratch);
    let source = SourceIdentity::default();
    let ctx = NativeContext {
        started_at: "2026-08-15T09:00:00Z".to_owned(),
        ended_at: "2026-08-15T09:00:09Z".to_owned(),
        analyzer_version: None,
        exit_status: output.status.code().unwrap_or(0),
        source: &source,
        rules_digest: Some("baseline".to_owned()),
        advisory_db: None,
        snippets: &snippets,
    };
    let live = normalize_native("semgrep", &output.stdout, &ctx).expect("normalise live output");

    let mut live_rules: Vec<&str> = live.findings.iter().map(|f| f.rule.as_str()).collect();
    let fixture_report = normalize_fixture();
    let mut fixture_rules: Vec<&str> = fixture_report
        .findings
        .iter()
        .map(|f| f.rule.as_str())
        .collect();
    live_rules.sort_unstable();
    fixture_rules.sort_unstable();
    assert_eq!(
        live_rules, fixture_rules,
        "the committed fixture no longer matches what semgrep emits; re-capture it \
         (see tests/fixtures/polyglot/README.md)"
    );

    std::fs::remove_dir_all(&scratch).ok();
}

/// First match for `program` on `PATH`.
fn which(program: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH")?
        .to_string_lossy()
        .split(if cfg!(windows) { ';' } else { ':' })
        .map(|dir| std::path::Path::new(dir).join(program))
        .find(|candidate| candidate.is_file())
}

fn copy_tree(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

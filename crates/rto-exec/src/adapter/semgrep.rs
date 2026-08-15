//! `semgrep` — static analysis across the project's languages.
//!
//! Semgrep is the SAST half of the coverage matrix (ADR-0015): it parses source
//! and matches patterns against the AST, and it is the one tool that covers
//! Rust, Python, Java, JavaScript and TypeScript with a single output format.
//!
//! # SQL is covered by the generic engine, and that is a real limitation
//!
//! Semgrep's published language list has **no SQL entry at any maturity level** —
//! not GA, not beta, not experimental. SQL findings therefore come from
//! semgrep's `generic` mode, which is Generally available but is a *token*
//! matcher, not a parser: no AST, no dataflow, no type information. A SQL rule
//! can say "this statement grants ALL PRIVILEGES"; it cannot say "this value
//! reaches a query unsanitised". That is stated here, in ADR-0015, and in the
//! rule file itself, so nobody reads a clean SQL scan as an AST-backed one.
//!
//! # Rules are ours, and pinned
//!
//! The `--config` this adapter passes is a local file provisioned by
//! [`crate::assets`], never a registry entry: `semgrep --config p/default` is a
//! network call to a service, which would make an "offline" analyzer quietly
//! network-dependent. The shipped rule set is written for this project and
//! carries the repository's own licence; no rule from the Semgrep Registry is
//! vendored, because Registry rules are under the Semgrep Rules License v1.0
//! rather than an SPDX-allowlisted licence.
//!
//! @rto:0012

use serde::Deserialize;

use crate::adapter::{Adapter, AssetPaths, Invocation, NativeContext, snippet_hash_at};
use crate::ingest::{NormalizedReport, REPORT_SCHEMA, ReportFinding};
use crate::runner::ExecError;
use rto_graph::{Severity, Span};

/// The analyzer id, and the first component of every finding key it produces.
pub const ANALYZER: &str = "semgrep";

/// Asset id of the pinned rule set this adapter runs with.
pub const RULES_ASSET: &str = "semgrep-rules";

/// The adapter.
#[derive(Debug, Clone, Copy)]
pub struct Semgrep;

impl Adapter for Semgrep {
    fn analyzer(&self) -> &'static str {
        ANALYZER
    }

    fn summary(&self) -> &'static str {
        "static analysis (SAST) against a pinned local rule set"
    }

    fn languages(&self) -> &'static [&'static str] {
        // SQL is listed because findings are produced for it, and qualified
        // everywhere it matters: the engine behind it is `generic`, not a SQL
        // parser. See the module docs and ADR-0015.
        &[
            "rust",
            "python",
            "java",
            "javascript",
            "typescript",
            "sql (generic mode)",
        ]
    }

    fn asset_ids(&self) -> &'static [&'static str] {
        &[RULES_ASSET]
    }

    fn command(&self, assets: &AssetPaths<'_>) -> Invocation {
        Invocation {
            program: "semgrep".to_owned(),
            args: vec![
                "scan".to_owned(),
                "--json".to_owned(),
                "--quiet".to_owned(),
                // Egress configured off: no telemetry, no update ping, and a
                // `--config` that is a local file rather than a registry id.
                // Configured, not enforced — see `SubprocessRunner`.
                "--metrics=off".to_owned(),
                "--disable-version-check".to_owned(),
                // Without this, semgrep prefixes every rule id with the
                // *filesystem path* of the config it was loaded from, so a
                // finding key would embed the local asset-cache directory —
                // user-identifying data in a stored record, and a key that
                // differs between two machines running the same scan. Verified
                // against semgrep 1.136.0.
                "--no-rewrite-rule-ids".to_owned(),
                "--config".to_owned(),
                assets.arg(RULES_ASSET),
                ".".to_owned(),
            ],
            // 0 = clean, 1 = findings. Semgrep uses 2 and above for a scan that
            // actually failed, and those must not be normalised into "no
            // findings" — a failed scan is not a clean bill of health.
            success_statuses: vec![0, 1],
        }
    }

    fn normalize(
        &self,
        native: &[u8],
        ctx: &NativeContext<'_>,
    ) -> Result<NormalizedReport, ExecError> {
        let output: SemgrepOutput = serde_json::from_slice(native)?;
        let Some(results) = output.results else {
            return Err(ExecError::MalformedReport(
                "not a semgrep report: no `results` array".to_owned(),
            ));
        };

        let mut findings = Vec::with_capacity(results.len());
        for result in results {
            // `nosemgrep` suppressions arrive as ignored results rather than
            // being omitted. Honouring them here means an in-source suppression
            // behaves the same whether the scan ran locally or in CI.
            if result.extra.is_ignored {
                continue;
            }
            findings.push(convert(&result, ctx)?);
        }

        Ok(NormalizedReport {
            schema: REPORT_SCHEMA.to_owned(),
            analyzer: ANALYZER.to_owned(),
            analyzer_version: ctx.version_or(output.version.as_deref()),
            started_at: ctx.started_at.clone(),
            ended_at: ctx.ended_at.clone(),
            exit_status: ctx.exit_status,
            rules_digest: ctx.rules_digest.clone(),
            image_digest: None,
            // Semgrep consults no advisory database: it matches patterns against
            // source. Claiming one would put a staleness label on a result that
            // has no such axis.
            advisory_db: None,
            source: ctx.source.clone(),
            findings,
        })
    }
}

/// One semgrep result → one normalized finding.
fn convert(result: &SemgrepResult, ctx: &NativeContext<'_>) -> Result<ReportFinding, ExecError> {
    if result.check_id.trim().is_empty() {
        return Err(ExecError::MalformedReport(
            "a semgrep result has no `check_id`".to_owned(),
        ));
    }
    if result.path.trim().is_empty() {
        return Err(ExecError::MalformedReport(format!(
            "semgrep result {:?} has no `path`",
            result.check_id
        )));
    }
    // `Span` is 32-bit, which is a 4 GiB ceiling on a single source file. A
    // larger offset is saturated rather than rejected: the finding is still
    // true, and losing the exact byte on a file that size is not what makes it
    // wrong. Clamping before the identity is built keeps the key and the span
    // agreeing on one number.
    let start = u32::try_from(result.start.offset).unwrap_or(u32::MAX);
    let end = u32::try_from(result.end.offset)
        .unwrap_or(u32::MAX)
        .max(start);
    let message = result.extra.message.trim();

    Ok(ReportFinding {
        // ADR-0012's recipe: rule, path, start byte, snippet hash. The snippet
        // is what makes the key notice that the *code* changed while the rule
        // and offset stayed put, so a re-run does not silently carry an old
        // finding onto new source.
        identity: vec![
            result.check_id.clone(),
            result.path.clone(),
            start.to_string(),
            // Read from the tree, not from `extra.lines`: the open-source
            // semgrep CLI redacts that field to the literal "requires login"
            // unless the caller is authenticated to Semgrep's hosted platform,
            // which would make this component a constant today and change every
            // stored key the day someone logs in. See `crate::snippet`.
            snippet_hash_at(ctx.snippets, &result.path, start, end),
        ],
        rule: result.check_id.clone(),
        severity: severity(&result.extra.severity),
        // Semgrep has no title field; its message is a sentence or a paragraph.
        // The first line is the title, the whole thing is the message, so a
        // listing stays one line per finding without losing detail.
        title: title_from(message, &result.check_id),
        message: message.to_owned(),
        path: Some(result.path.clone()),
        span: Some(Span::new(start, end)),
        meta: serde_json::json!({
            "line": result.start.line,
            "column": result.start.col,
            "end_line": result.end.line,
            "semgrep_severity": result.extra.severity,
            "metadata": result.extra.metadata,
            "engine": result.extra.engine_kind,
        }),
    })
}

/// The first line of `message`, falling back to the rule id when the message is
/// empty — a finding with no title is refused downstream, and the rule id is
/// always more use than a blank.
fn title_from(message: &str, check_id: &str) -> String {
    let first = message.lines().next().unwrap_or("").trim();
    if first.is_empty() {
        check_id.to_owned()
    } else {
        first.to_owned()
    }
}

/// Map semgrep's severity vocabulary onto [`Severity`].
///
/// Semgrep emits the three-level `ERROR`/`WARNING`/`INFO` set, and newer rule
/// metadata also uses `CRITICAL`/`HIGH`/`MEDIUM`/`LOW`. Both are accepted;
/// anything else round-trips verbatim through [`Severity::Other`] rather than
/// being flattened into a level the analyzer did not assign.
fn severity(raw: &str) -> Severity {
    match raw.to_ascii_uppercase().as_str() {
        "CRITICAL" => Severity::Critical,
        "ERROR" | "HIGH" => Severity::High,
        "WARNING" | "MEDIUM" => Severity::Medium,
        "LOW" => Severity::Low,
        "INFO" | "INFORMATION" => Severity::Info,
        _ => Severity::from_token(&raw.to_ascii_lowercase()),
    }
}

/// The shape of `semgrep --json`, narrowed to what is needed.
///
/// Unknown fields are ignored on purpose: semgrep adds keys between minor
/// versions, and a parser that refused them would break on an upgrade that
/// changed nothing this adapter reads.
#[derive(Debug, Deserialize)]
struct SemgrepOutput {
    #[serde(default)]
    version: Option<String>,
    /// Absent rather than empty distinguishes "not a semgrep report" from "a
    /// clean scan", and only the latter is a valid result.
    #[serde(default)]
    results: Option<Vec<SemgrepResult>>,
}

#[derive(Debug, Deserialize)]
struct SemgrepResult {
    check_id: String,
    path: String,
    #[serde(default)]
    start: Position,
    #[serde(default)]
    end: Position,
    #[serde(default)]
    extra: Extra,
}

#[derive(Debug, Default, Deserialize)]
struct Position {
    #[serde(default)]
    line: u64,
    #[serde(default)]
    col: u64,
    #[serde(default)]
    offset: u64,
}

#[derive(Debug, Default, Deserialize)]
struct Extra {
    #[serde(default)]
    message: String,
    #[serde(default)]
    severity: String,
    #[serde(default)]
    lines: String,
    #[serde(default)]
    is_ignored: bool,
    #[serde(default)]
    metadata: serde_json::Value,
    #[serde(default)]
    engine_kind: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{ANALYZER, RULES_ASSET, Semgrep, severity, title_from};
    use crate::adapter::{Adapter, AssetPaths, NativeContext};
    use crate::runner::ExecError;
    use rto_graph::{Severity, SourceIdentity};

    fn ctx() -> NativeContext<'static> {
        static SOURCE: std::sync::LazyLock<SourceIdentity> =
            std::sync::LazyLock::new(SourceIdentity::default);
        NativeContext {
            started_at: "2026-08-15T09:00:00Z".to_owned(),
            ended_at: "2026-08-15T09:00:09Z".to_owned(),
            analyzer_version: None,
            exit_status: 1,
            source: &SOURCE,
            rules_digest: Some("cafe1234".to_owned()),
            // The unit tests here exercise the *parsing*; the snippet component
            // is covered by `tests/equivalence.rs`, which reads a real tree.
            snippets: &crate::snippet::NoSnippets,
        }
    }

    const NATIVE: &str = r#"{
      "version": "1.96.0",
      "results": [
        {
          "check_id": "roteiro.python.subprocess-shell-true",
          "path": "svc/app.py",
          "start": {"line": 12, "col": 5, "offset": 240},
          "end": {"line": 12, "col": 45, "offset": 280},
          "extra": {
            "message": "Shell injection risk.\nPass a list of arguments instead.",
            "severity": "ERROR",
            "lines": "    subprocess.run(cmd, shell=True)",
            "is_ignored": false,
            "metadata": {"category": "security"},
            "engine_kind": "OSS"
          }
        },
        {
          "check_id": "roteiro.python.assert-used",
          "path": "svc/app.py",
          "start": {"line": 3, "col": 1, "offset": 40},
          "end": {"line": 3, "col": 20, "offset": 60},
          "extra": {
            "message": "assert is stripped under -O",
            "severity": "WARNING",
            "lines": "assert user.is_admin",
            "is_ignored": true
          }
        }
      ],
      "errors": [],
      "paths": {"scanned": ["svc/app.py"]}
    }"#;

    #[test]
    fn normalizes_a_native_report() {
        let report = Semgrep.normalize(NATIVE.as_bytes(), &ctx()).expect("parse");
        assert_eq!(report.analyzer, ANALYZER);
        // No version was supplied out of band, so the report's own wins.
        assert_eq!(report.analyzer_version, "1.96.0");
        assert_eq!(report.rules_digest.as_deref(), Some("cafe1234"));
        // Semgrep consults no advisory database, so it must not claim one.
        assert!(report.advisory_db.is_none());

        // The suppressed (`nosemgrep`) result is gone; the live one converted.
        assert_eq!(report.findings.len(), 1);
        let finding = &report.findings[0];
        assert_eq!(finding.rule, "roteiro.python.subprocess-shell-true");
        assert_eq!(finding.severity, Severity::High);
        assert_eq!(finding.title, "Shell injection risk.");
        assert!(finding.message.contains("Pass a list of arguments"));
        assert_eq!(finding.path.as_deref(), Some("svc/app.py"));
        assert_eq!(finding.span.map(|s| (s.start, s.end)), Some((240, 280)));
    }

    /// A stand-in worktree: whatever text was put in it, for any span.
    struct FakeTree(&'static str);

    impl crate::snippet::SnippetSource for FakeTree {
        fn snippet(&self, _path: &str, _start: u32, _end: u32) -> Option<String> {
            Some(self.0.to_owned())
        }
    }

    fn ctx_with_tree(tree: &'static FakeTree) -> NativeContext<'static> {
        let mut ctx = ctx();
        ctx.snippets = tree;
        ctx
    }

    /// The identity recipe ADR-0012 specifies, component by component. It is
    /// asserted positionally because the *order* is the contract: a reordering
    /// would silently re-key every stored finding.
    #[test]
    fn uses_the_rule_path_offset_snippet_identity() {
        static TREE: FakeTree = FakeTree("    subprocess.run(cmd, shell=True)");
        let report = Semgrep
            .normalize(NATIVE.as_bytes(), &ctx_with_tree(&TREE))
            .expect("parse");
        let identity = &report.findings[0].identity;
        assert_eq!(identity[0], "roteiro.python.subprocess-shell-true");
        assert_eq!(identity[1], "svc/app.py");
        assert_eq!(identity[2], "240");
        assert_eq!(
            identity[3],
            crate::adapter::snippet_hash("    subprocess.run(cmd, shell=True)")
        );
    }

    /// The snippet component exists so that new code at an unchanged offset is a
    /// new finding rather than the old one silently carried forward.
    #[test]
    fn changed_code_at_the_same_offset_is_a_different_finding() {
        static BEFORE: FakeTree = FakeTree("subprocess.run(cmd, shell=True)");
        static AFTER: FakeTree = FakeTree("os.system(cmd)");
        let a = Semgrep
            .normalize(NATIVE.as_bytes(), &ctx_with_tree(&BEFORE))
            .expect("a");
        let b = Semgrep
            .normalize(NATIVE.as_bytes(), &ctx_with_tree(&AFTER))
            .expect("b");
        assert_ne!(a.findings[0].identity, b.findings[0].identity);
        // …and only the snippet component moved.
        assert_eq!(a.findings[0].identity[..3], b.findings[0].identity[..3]);
    }

    /// Semgrep's own `extra.lines` is the literal "requires login" in the
    /// open-source CLI, so it must never reach an identity: a finding key that
    /// depended on it would be a constant today and would change the day a user
    /// authenticated. The tree is the source of truth instead.
    #[test]
    fn the_identity_ignores_semgreps_redacted_snippet_field() {
        static TREE: FakeTree = FakeTree("subprocess.run(cmd, shell=True)");
        let redacted = NATIVE.replace(
            r#""lines": "    subprocess.run(cmd, shell=True)","#,
            r#""lines": "requires login","#,
        );
        assert!(
            redacted.contains("requires login"),
            "the fixture was rewritten"
        );
        let from_real = Semgrep
            .normalize(NATIVE.as_bytes(), &ctx_with_tree(&TREE))
            .expect("a");
        let from_redacted = Semgrep
            .normalize(redacted.as_bytes(), &ctx_with_tree(&TREE))
            .expect("b");
        assert_eq!(
            from_real.findings[0].identity,
            from_redacted.findings[0].identity
        );
    }

    /// A report about a tree this checkout does not have still normalises; the
    /// identity says the snippet was unavailable instead of inventing one.
    #[test]
    fn a_missing_tree_yields_a_named_snippet_component() {
        let report = Semgrep.normalize(NATIVE.as_bytes(), &ctx()).expect("parse");
        assert_eq!(report.findings[0].identity[3], crate::adapter::NO_SNIPPET);
    }

    #[test]
    fn maps_both_severity_vocabularies() {
        for (raw, want) in [
            ("ERROR", Severity::High),
            ("WARNING", Severity::Medium),
            ("INFO", Severity::Info),
            ("CRITICAL", Severity::Critical),
            ("HIGH", Severity::High),
            ("MEDIUM", Severity::Medium),
            ("LOW", Severity::Low),
        ] {
            assert_eq!(severity(raw), want, "{raw}");
        }
        // An unknown level is kept verbatim rather than flattened into one the
        // analyzer never assigned.
        assert_eq!(
            severity("EXPERIMENTAL"),
            Severity::Other("experimental".to_owned())
        );
    }

    #[test]
    fn a_clean_scan_is_a_valid_empty_report() {
        let clean = br#"{"version":"1.96.0","results":[],"errors":[]}"#;
        let report = Semgrep.normalize(clean, &ctx()).expect("parse");
        assert!(report.findings.is_empty());
    }

    #[test]
    fn refuses_output_that_is_not_a_semgrep_report() {
        // No `results` key at all: an empty scan and "the wrong file" must not
        // look the same.
        let err = Semgrep
            .normalize(br#"{"version":"1.96.0"}"#, &ctx())
            .expect_err("must be refused");
        assert!(matches!(err, ExecError::MalformedReport(_)));
        assert!(err.to_string().contains("no `results` array"), "{err}");

        assert!(matches!(
            Semgrep.normalize(b"not json", &ctx()),
            Err(ExecError::Json(_))
        ));
    }

    #[test]
    fn refuses_a_result_with_no_rule_or_no_path() {
        for native in [
            r#"{"results":[{"check_id":"  ","path":"a.py","start":{},"end":{},"extra":{}}]}"#,
            r#"{"results":[{"check_id":"r","path":"","start":{},"end":{},"extra":{}}]}"#,
        ] {
            assert!(
                matches!(
                    Semgrep.normalize(native.as_bytes(), &ctx()),
                    Err(ExecError::MalformedReport(_))
                ),
                "{native}"
            );
        }
    }

    /// A result whose `end` offset precedes its `start` would be refused by the
    /// shared validation as a backwards span. Clamping here keeps a merely odd
    /// report usable while still never producing a span that runs backwards.
    #[test]
    fn clamps_a_backwards_span_rather_than_emitting_one() {
        let native = r#"{"results":[{"check_id":"r","path":"a.py",
            "start":{"offset":90},"end":{"offset":10},"extra":{"message":"m","lines":"x"}}]}"#;
        let report = Semgrep.normalize(native.as_bytes(), &ctx()).expect("parse");
        assert_eq!(
            report.findings[0].span.map(|s| (s.start, s.end)),
            Some((90, 90))
        );
    }

    #[test]
    fn a_message_less_finding_is_titled_by_its_rule() {
        assert_eq!(title_from("", "rules.x"), "rules.x");
        assert_eq!(title_from("  first\nsecond", "rules.x"), "first");
    }

    #[test]
    fn the_invocation_configures_egress_off_and_points_at_the_pinned_rules() {
        let entries = [(RULES_ASSET, std::path::PathBuf::from("/cache/rules.yaml"))];
        let invocation = Semgrep.command(&AssetPaths::new(&entries));
        assert_eq!(invocation.program, "semgrep");
        assert!(invocation.args.contains(&"--metrics=off".to_owned()));
        assert!(
            invocation
                .args
                .contains(&"--disable-version-check".to_owned())
        );
        // The `--config` must be the provisioned local file: a registry id here
        // would be a network call, which is exactly what pinning prevents.
        let config = invocation
            .args
            .iter()
            .position(|a| a == "--config")
            .map(|i| invocation.args[i + 1].clone())
            .expect("a --config argument");
        assert_eq!(config, "/cache/rules.yaml");
        // Semgrep exits 1 when it *found* something; treating that as failure
        // would discard every run that mattered.
        assert_eq!(invocation.success_statuses, vec![0, 1]);
    }

    #[test]
    fn declares_the_rule_set_as_the_asset_it_needs() {
        assert_eq!(Semgrep.asset_ids(), &[RULES_ASSET]);
        assert!(Semgrep.languages().contains(&"rust"));
        assert!(
            Semgrep.languages().iter().any(|l| l.starts_with("sql")),
            "SQL coverage must be claimed, and qualified"
        );
    }
}

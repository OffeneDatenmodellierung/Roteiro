//! `clippy` — the Rust toolchain's own linter, normalised for a report that is
//! **never stored**.
//!
//! This adapter has the same shape as every other one in this module: native
//! analyzer output in, a [`NormalizedReport`] out, with the identity recipe and
//! the severity mapping written beside the parser that needs them. What it does
//! **not** have is a place in [`ADAPTERS`], and that omission is the decision
//! rather than an oversight.
//!
//! # Why it is not in the registry
//!
//! [`ADAPTERS`] is the table `roteiro security ingest` consults, so anything in
//! it can be filed as a findings layer. Clippy must not be, and leaving it out
//! is what makes that structural instead of a rule someone has to remember:
//! there is no `--analyzer clippy` for `ingest` to accept, no
//! `security:clippy:<worktree>` layer key to collide, and no path from this file
//! to [`rto_graph::Store::replace_findings_layer`].
//!
//! ADR-0020 v1.1 states the reason, and it is about what a lint *is*. An
//! advisory id is **assigned**, and assignment is a promise: `RUSTSEC-2020-0071`
//! will mean the same thing in five years, which is why it earns a row in a
//! store. A lint name is a **symbol in a compiler** — renamed, removed, or moved
//! between groups at the compiler's discretion, with the old name surviving only
//! as a deprecation alias. The first is a durable fact about the repository; the
//! second is a tool's opinion about the code as it stands today, for the person
//! who asked.
//!
//! Storing the second is what produced every identity problem the investigation
//! behind ADR-0020 found. A layer key renders `<prefix>:<analyzer>:<worktree-id>`
//! and nothing else, `analyzer_version` is in neither the finding key nor the
//! layer key, and the column is `UNIQUE` — so two runs of one commit differing
//! only in toolchain version or feature set would collide, silently replace each
//! other, and report the displaced findings as *removed*, which reads as
//! **fixed**. For every stored analyzer the thing deciding the answer is a
//! pinned asset with a digest; for a linter the rule set is the toolchain, and
//! there is no asset to digest. Not storing removes all of it.
//!
//! # It carries no `package`/`version` pair, deliberately
//!
//! [`crate::crossref`] joins two findings when their identifier sets intersect
//! **and** they name the same package at the same version, both read out of
//! `meta`. A clippy finding therefore cannot enter that join, because this
//! adapter never writes those two keys — the cargo message carries a
//! `package_id` and it is deliberately dropped. That join's correctness rests on
//! both upstreams publishing identifiers, and nobody publishes lint names; they
//! are release notes.
//!
//! [`ADAPTERS`]: crate::adapter::ADAPTERS
//!
//! @rto:0012
//! @rto:0020

use std::path::Path;

use serde::Deserialize;

use crate::adapter::{
    Adapter, AssetPaths, InstallHint, Invocation, NativeContext, RUST_TOOLCHAIN, snippet_hash_at,
};
use crate::guidance::{Guidance, Line};
use crate::ingest::{NormalizedReport, REPORT_SCHEMA, ReportFinding};
use crate::runner::{ExecError, check_reported_path};
use rto_graph::{Severity, Span};

/// The analyzer id. It names a **reporting** analyzer, and is never the first
/// component of a stored key, because nothing this adapter produces is stored.
pub const ANALYZER: &str = "clippy";

/// The command that adds the linter to an already-installed toolchain.
///
/// Named once because two refusals print it: this adapter's install hint, and
/// [`crate::lint::LintError::AnalyzerNotInstalled`], which is raised by the
/// probe that finds `cargo clippy --version` failing on a toolchain that has
/// `cargo`. They are the same instruction reached two ways, and a literal in
/// each is how they would come to disagree.
///
/// Documented at <https://doc.rust-lang.org/clippy/installation.html>, whose
/// full form is `rustup component add clippy [--toolchain=<name>]`; the optional
/// part is dropped because a hint has to be pasteable as printed.
pub const COMPONENT_ADD: &str = "rustup component add clippy";

/// How to obtain each of this linter's two programs.
///
/// The clearest case for keying hints by program rather than by analyzer: the
/// *other* program here wants rustup's front page, and this one wants a
/// component add. Handing a reader whose toolchain is fine an installer link is
/// the wrong *kind* of way forward — the failure `docs/REVIEW_CHECKLIST.md`
/// names — and is why [`RUST_TOOLCHAIN`] is reused for `cargo` and pointedly not
/// for `cargo-clippy`.
const INSTALL_HINTS: &[InstallHint] = &[
    InstallHint {
        program: "cargo",
        guidance: RUST_TOOLCHAIN,
    },
    InstallHint {
        program: "cargo-clippy",
        guidance: Guidance::new(&[
            Line::Note(&[
                "Roteiro does not install toolchain components, and has not installed",
                "this one. Clippy ships with the toolchain and is added to it with:",
            ]),
            Line::Command(COMPONENT_ADD),
            Line::Note(&["Upstream: https://doc.rust-lang.org/clippy/installation.html"]),
        ]),
    },
];

/// The rule recorded for a diagnostic that carries no lint code — a parse error,
/// say. Such a diagnostic still has a location and still matters, so it is
/// reported under a name rather than dropped: an empty result is the one thing a
/// failed build must never look like.
pub const UNCODED_RULE: &str = "rustc";

/// Which features the build under review is resolved with.
///
/// Reported rather than assumed, because it is one of the two axes — the other
/// being the toolchain — that move a lint count without the code changing. On
/// this repository the difference is 355 crates and 54 build scripts at the
/// default set against 672 and 87 at `--all-features` (ADR-0020), so a count
/// quoted without its feature set is not comparable to any other count.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum FeatureSet {
    /// Whatever each crate declares as its default features.
    #[default]
    Defaults,
    /// `--all-features`.
    All,
    /// `--features a,b,c`, as the caller wrote them.
    Explicit(Vec<String>),
}

impl FeatureSet {
    /// The cargo arguments this feature set contributes, in order.
    #[must_use]
    pub fn args(&self) -> Vec<String> {
        match self {
            Self::Defaults => Vec::new(),
            Self::All => vec!["--all-features".to_owned()],
            Self::Explicit(features) => {
                vec!["--features".to_owned(), features.join(",")]
            }
        }
    }

    /// A one-line label for the report — never empty, so a reader is never left
    /// to infer which of the three cases produced a count.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Defaults => "default (each crate's own default features)".to_owned(),
            Self::All => "all (--all-features)".to_owned(),
            Self::Explicit(features) => features.join(", "),
        }
    }
}

/// The adapter.
#[derive(Debug, Clone, Copy)]
pub struct Clippy;

/// What a stream of cargo messages contained beyond the findings themselves.
///
/// Every field is a count of something that did **not** become a finding. They
/// are reported rather than swallowed: a run that silently dropped half its
/// diagnostics and printed a small number would be indistinguishable from a
/// clean tree, which is the shape this project has been bitten by before.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    /// Whether cargo's `build-finished` message reported success. A failed build
    /// still yields the diagnostics it managed to emit, and they are real — but
    /// the set is partial, and a caller must say so.
    pub build_succeeded: bool,
    /// How many `compiler-message` entries the stream carried.
    pub compiler_messages: usize,
    /// Diagnostics with no primary span — rustc's own summaries ("aborting due
    /// to 3 previous errors"), which are about the run rather than about a line
    /// of code.
    pub without_location: usize,
    /// Diagnostics about a file outside the analyzed worktree — a dependency's
    /// source under the cargo registry, most often.
    pub outside_worktree: usize,
    /// Identical diagnostics emitted more than once. `--all-targets` compiles
    /// one file into several targets, so a lint in `src/main.rs` arrives once
    /// per target; they are the same defect and are counted once.
    pub duplicates_collapsed: usize,
}

impl Clippy {
    /// The argv that produces the stream [`Clippy::normalize`] parses, at a
    /// stated feature set.
    ///
    /// `--workspace --all-targets` mirrors the gate this repository already runs
    /// (`AGENTS.md`), so the count a contributor sees here is the count CI will
    /// see. `-D warnings` is deliberately **not** passed: this command reports,
    /// and the levels the repository declares in `[workspace.lints]` are part of
    /// what is being reported.
    #[must_use]
    pub fn invocation(features: &FeatureSet) -> Invocation {
        let mut args = vec![
            "clippy".to_owned(),
            "--workspace".to_owned(),
            "--all-targets".to_owned(),
            // Not a reproducibility flourish — a write. Without it cargo creates
            // or updates `Cargo.lock` when the manifest and the lockfile
            // disagree, and it does that **in the tree being linted**, which is
            // the tree `roteiro lint` promises to leave as it found it. Pointing
            // `CARGO_TARGET_DIR` outside the worktree moves the build artefacts
            // and does nothing about the lockfile; this is the other half of the
            // same guarantee.
            //
            // The cost is real and is the correct one to pay: a tree whose
            // lockfile is missing or stale now refuses to lint rather than
            // silently being modified into a lintable one. `LintError::
            // LockfileWouldBeWritten` is where that refusal is explained.
            "--locked".to_owned(),
        ];
        args.extend(features.args());
        args.push("--message-format=json".to_owned());
        args.push("--quiet".to_owned());
        Invocation {
            program: "cargo".to_owned(),
            args,
            // 0 = the build completed. 101 = it did not, which for a repository
            // that denies a lint group is the ordinary outcome of *finding
            // something* — so treating it as failure would discard exactly the
            // runs that matter. Every other status is a cargo that could not
            // start, and falls through to a hard failure.
            success_statuses: vec![0, 101],
        }
    }

    /// The argv for a run whose network is denied **by a boundary** rather than
    /// by good manners.
    ///
    /// [`Clippy::invocation`] plus `--offline`, and the two are separate
    /// functions rather than a flag on one because they describe different
    /// situations rather than different preferences. On the host, cargo may
    /// legitimately reach a registry for a dependency the user has not fetched
    /// yet; that is their machine and their choice, and `--locked` already stops
    /// the one write into the tree that would follow. In a guest there is no
    /// interface to reach it with, so the question is only whether cargo finds
    /// that out from `--offline` or from a DNS timeout inside a VM.
    ///
    /// It is worth the flag for the error message alone. Without it a missing
    /// crate surfaces as a network failure from inside a machine the user cannot
    /// see; with it, cargo says *"attempting to make an HTTP request, but
    /// --offline was specified"*, which [`crate::lint_sandbox`] turns into the
    /// one thing that would actually help — fetch it on the host first.
    #[must_use]
    pub fn offline_invocation(features: &FeatureSet) -> Invocation {
        let mut invocation = Self::invocation(features);
        // Ahead of `--message-format`/`--quiet` only because `invocation`
        // appends those last; cargo does not care about order, and this keeps
        // the two argvs differing by exactly one token wherever they are printed
        // side by side.
        invocation.args.push("--offline".to_owned());
        invocation
    }

    /// Parse a cargo `--message-format=json` stream, returning the normalised
    /// report **and** what the stream contained besides findings.
    ///
    /// [`Adapter::normalize`] is this without the second half; the counts exist
    /// because the ephemeral report prints them, and a trait shared with stored
    /// analyzers has nowhere to carry them.
    ///
    /// # Errors
    /// Returns [`ExecError::MalformedReport`] when the stream carries no
    /// `build-finished` message — the marker that distinguishes a completed
    /// cargo run from empty output, and therefore a clean tree from a run that
    /// never happened.
    pub fn parse(
        native: &[u8],
        ctx: &NativeContext<'_>,
    ) -> Result<(NormalizedReport, Summary), ExecError> {
        let text = String::from_utf8_lossy(native);
        let mut summary = Summary::default();
        let mut finished = false;
        let mut findings: Vec<ReportFinding> = Vec::new();

        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            // A line this build cannot read is not a reason to lose the run:
            // cargo adds message kinds between releases, and every one of them
            // carries its own `reason`. Unknown shapes are skipped, and the
            // `build-finished` requirement below is what stops that leniency
            // from turning junk into a clean report.
            let Ok(message) = serde_json::from_str::<CargoMessage>(line) else {
                continue;
            };
            match message.reason.as_str() {
                "build-finished" => {
                    finished = true;
                    summary.build_succeeded = message.success.unwrap_or(false);
                }
                "compiler-message" => {
                    summary.compiler_messages += 1;
                    if let Some(diagnostic) = message.message {
                        convert(&diagnostic, ctx, &mut summary, &mut findings);
                    }
                }
                _ => {}
            }
        }

        if !finished {
            return Err(ExecError::MalformedReport(
                "not a cargo --message-format=json stream: no `build-finished` message, so this \
                 is not a completed run and its emptiness means nothing"
                    .to_owned(),
            ));
        }

        findings.sort_by(|a, b| a.identity.cmp(&b.identity));
        let before = findings.len();
        findings.dedup_by(|a, b| a.identity == b.identity);
        summary.duplicates_collapsed = before - findings.len();

        Ok((
            NormalizedReport {
                schema: REPORT_SCHEMA.to_owned(),
                analyzer: ANALYZER.to_owned(),
                analyzer_version: ctx.version_or(None),
                started_at: ctx.started_at.clone(),
                ended_at: ctx.ended_at.clone(),
                exit_status: ctx.exit_status,
                // There is no rule set to digest. The rules **are** the
                // toolchain plus the repository's own `[workspace.lints]`, and
                // neither is a pinned asset — which is precisely why this
                // analyzer's output is reported rather than stored.
                rules_digest: None,
                image_digest: None,
                // A linter consults no advisory database. Claiming one would put
                // a staleness label on a result that has no such axis.
                advisory_db: None,
                source: ctx.source.clone(),
                findings,
            },
            summary,
        ))
    }
}

impl Adapter for Clippy {
    fn analyzer(&self) -> &'static str {
        ANALYZER
    }

    fn summary(&self) -> &'static str {
        "Rust lints from the toolchain's own linter, reported and never stored"
    }

    fn languages(&self) -> &'static [&'static str] {
        &["rust"]
    }

    fn asset_ids(&self) -> &'static [&'static str] {
        // None, and not because it happens to need nothing: a linter's rule set
        // is the toolchain it ships with, so there is no asset to pin and no
        // digest to record. That is the difference this whole adapter turns on.
        &[]
    }

    fn host_programs(&self) -> &'static [&'static str] {
        // The same two-part shape as `cargo-audit`, for the same reason: `cargo
        // clippy` dispatches to a `cargo-clippy` binary on `PATH`. Unlike the
        // others this is a toolchain component rather than a separate install —
        // `rustup component add clippy` — but the *check* is identical, and this
        // adapter is not in `ADAPTERS`, so `roteiro security status` never reports
        // it. Declared because the trait requires an answer and a wrong one here
        // would be waiting for whoever wires a lint status later.
        &["cargo", "cargo-clippy"]
    }

    fn install_hints(&self) -> &'static [InstallHint] {
        INSTALL_HINTS
    }

    fn command(&self, _assets: &AssetPaths<'_>) -> Invocation {
        Self::invocation(&FeatureSet::Defaults)
    }

    fn normalize(
        &self,
        native: &[u8],
        ctx: &NativeContext<'_>,
    ) -> Result<NormalizedReport, ExecError> {
        Self::parse(native, ctx).map(|(report, _)| report)
    }
}

/// Convert one rustc diagnostic, or account for why it produced no finding.
fn convert(
    diagnostic: &Diagnostic,
    ctx: &NativeContext<'_>,
    summary: &mut Summary,
    findings: &mut Vec<ReportFinding>,
) {
    let Some(span) = primary_span(diagnostic) else {
        summary.without_location += 1;
        return;
    };
    let Some(path) = worktree_relative(&span.file_name, ctx.worktree) else {
        summary.outside_worktree += 1;
        return;
    };
    let start = u32::try_from(span.byte_start).unwrap_or(u32::MAX);
    let end = u32::try_from(span.byte_end).unwrap_or(u32::MAX).max(start);
    let message = diagnostic.message.trim();
    let rule = diagnostic
        .code
        .as_ref()
        .map(|c| c.code.trim())
        .filter(|c| !c.is_empty())
        .unwrap_or(UNCODED_RULE)
        .to_owned();

    findings.push(ReportFinding {
        // The recipe semgrep uses — rule, path, start byte, snippet hash — for
        // the same reason it uses it, minus the durability claim: here it orders
        // the report and collapses the same lint reported once per target. It is
        // never a stored key, because there is no store to put it in.
        identity: vec![
            rule.clone(),
            path.clone(),
            start.to_string(),
            snippet_hash_at(ctx.snippets, &path, start, end),
        ],
        rule,
        severity: severity(&diagnostic.level),
        title: title_from(message, &span.file_name),
        message: message.to_owned(),
        path: Some(path),
        span: Some(Span::new(start, end)),
        meta: serde_json::json!({
            "line": span.line_start,
            "column": span.column_start,
            "end_line": span.line_end,
            "rustc_level": diagnostic.level,
        }),
    });
}

/// The span a diagnostic is *about*: its primary one, else its first.
fn primary_span(diagnostic: &Diagnostic) -> Option<&DiagnosticSpan> {
    diagnostic
        .spans
        .iter()
        .find(|s| s.is_primary)
        .or_else(|| diagnostic.spans.first())
}

/// Place a reported file inside the analyzed worktree, or refuse it.
///
/// Cargo reports workspace-relative paths for the crates it is building and
/// absolute ones for anything else, so both shapes arrive. An absolute path
/// under the worktree is relativised; one outside it — a dependency's source in
/// the cargo registry — is not a claim about this repository and is dropped.
/// A relative path that climbs out is refused by the same check the stored path
/// uses, so the two agree on what "inside the tree" means.
fn worktree_relative(file: &str, worktree: Option<&Path>) -> Option<String> {
    let path = Path::new(file);
    let relative = if path.is_absolute() {
        path.strip_prefix(worktree?).ok()?
    } else {
        path.strip_prefix("./").unwrap_or(path)
    };
    let text = relative.to_string_lossy().into_owned();
    check_reported_path(&text).ok()?;
    Some(text)
}

/// The first line of `message`, falling back to the file name when a diagnostic
/// somehow carries no text at all — a titleless finding is refused downstream,
/// and anything is more use than a blank.
fn title_from(message: &str, file: &str) -> String {
    let first = message.lines().next().unwrap_or("").trim();
    if first.is_empty() {
        file.to_owned()
    } else {
        first.to_owned()
    }
}

/// Map rustc's diagnostic levels onto [`Severity`].
///
/// A lint's level is the level the *repository* configured — `[workspace.lints]`
/// is what makes `clippy::all` an error here — so this is a faithful record of
/// how the toolchain was told to treat it, not a judgement of how bad it is.
fn severity(level: &str) -> Severity {
    match level.trim().to_ascii_lowercase().as_str() {
        "error" | "error: internal compiler error" => Severity::High,
        "warning" => Severity::Medium,
        "note" | "help" | "failure-note" => Severity::Info,
        other => Severity::from_token(other),
    }
}

/// One line of `cargo --message-format=json`, narrowed to what is needed.
///
/// `package_id` is **not** deserialized, and that is load-bearing rather than
/// economical: it is the one field that could be turned into the
/// `meta.package` / `meta.version` pair [`crate::crossref`] joins on, and a lint
/// must never enter that join.
#[derive(Debug, Deserialize)]
struct CargoMessage {
    reason: String,
    #[serde(default)]
    message: Option<Diagnostic>,
    /// `build-finished` only.
    #[serde(default)]
    success: Option<bool>,
}

/// A rustc diagnostic, as cargo forwards it.
#[derive(Debug, Deserialize)]
struct Diagnostic {
    #[serde(default)]
    message: String,
    #[serde(default)]
    level: String,
    #[serde(default)]
    code: Option<DiagnosticCode>,
    #[serde(default)]
    spans: Vec<DiagnosticSpan>,
}

#[derive(Debug, Deserialize)]
struct DiagnosticCode {
    #[serde(default)]
    code: String,
}

#[derive(Debug, Deserialize)]
struct DiagnosticSpan {
    #[serde(default)]
    file_name: String,
    #[serde(default)]
    byte_start: u64,
    #[serde(default)]
    byte_end: u64,
    #[serde(default)]
    line_start: u64,
    #[serde(default)]
    line_end: u64,
    #[serde(default)]
    column_start: u64,
    #[serde(default)]
    is_primary: bool,
}

#[cfg(test)]
mod tests {
    use super::{ANALYZER, Clippy, FeatureSet, UNCODED_RULE, severity};
    use crate::adapter::{Adapter, AssetPaths, NativeContext, adapter_for, known_analyzers};
    use crate::runner::ExecError;
    use rto_graph::{Severity, SourceIdentity};

    fn ctx() -> NativeContext<'static> {
        static SOURCE: std::sync::LazyLock<SourceIdentity> =
            std::sync::LazyLock::new(SourceIdentity::default);
        NativeContext {
            started_at: "2026-08-18T09:00:00Z".to_owned(),
            ended_at: "2026-08-18T09:04:00Z".to_owned(),
            analyzer_version: Some("0.1.94".to_owned()),
            exit_status: 101,
            source: &SOURCE,
            rules_digest: None,
            advisory_db: None,
            worktree: Some(std::path::Path::new("/checkout")),
            snippets: &crate::snippet::NoSnippets,
        }
    }

    /// A stream in the shape cargo emits: one clippy lint, the same lint again
    /// from a second target, a coded rustc error, a location-less summary, a
    /// diagnostic about a dependency's source, and the terminator.
    const STREAM: &str = r#"
{"reason":"compiler-artifact","target":{"name":"rto-exec"},"fresh":false}
{"reason":"compiler-message","package_id":"path+file:///checkout#rto-exec@1.23.0","target":{"kind":["lib"],"name":"rto-exec"},"message":{"message":"this expression creates a reference which is immediately dereferenced by the compiler\nchange this to remove the borrow","code":{"code":"clippy::needless_borrow"},"level":"warning","spans":[{"file_name":"crates/rto-exec/src/lib.rs","byte_start":120,"byte_end":132,"line_start":9,"line_end":9,"column_start":13,"is_primary":true}]}}
{"reason":"compiler-message","package_id":"path+file:///checkout#rto-exec@1.23.0","target":{"kind":["test"],"name":"rto-exec"},"message":{"message":"this expression creates a reference which is immediately dereferenced by the compiler\nchange this to remove the borrow","code":{"code":"clippy::needless_borrow"},"level":"warning","spans":[{"file_name":"crates/rto-exec/src/lib.rs","byte_start":120,"byte_end":132,"line_start":9,"line_end":9,"column_start":13,"is_primary":true}]}}
{"reason":"compiler-message","message":{"message":"mismatched types","code":{"code":"E0308"},"level":"error","spans":[{"file_name":"/checkout/crates/roteiro/src/main.rs","byte_start":40,"byte_end":48,"line_start":3,"line_end":3,"column_start":5,"is_primary":true}]}}
{"reason":"compiler-message","message":{"message":"aborting due to 1 previous error","level":"error","spans":[]}}
{"reason":"compiler-message","message":{"message":"unused variable: `x`","code":{"code":"unused_variables"},"level":"warning","spans":[{"file_name":"/home/dev/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/serde-1.0.0/src/lib.rs","byte_start":1,"byte_end":2,"line_start":1,"line_end":1,"column_start":1,"is_primary":true}]}}
{"reason":"build-finished","success":false}
"#;

    #[test]
    fn normalizes_a_cargo_message_stream() {
        let (report, summary) = Clippy::parse(STREAM.as_bytes(), &ctx()).expect("parse");
        assert_eq!(report.analyzer, ANALYZER);
        assert_eq!(report.analyzer_version, "0.1.94");
        // A linter pins nothing: its rule set is the toolchain, and there is no
        // advisory database in the picture at all.
        assert!(report.rules_digest.is_none());
        assert!(report.advisory_db.is_none());

        assert_eq!(summary.compiler_messages, 5);
        assert!(!summary.build_succeeded, "the stream said `success: false`");
        assert_eq!(summary.without_location, 1, "the `aborting due to` summary");
        assert_eq!(summary.outside_worktree, 1, "the dependency's own source");
        assert_eq!(summary.duplicates_collapsed, 1, "lib and test targets");

        let rules: Vec<&str> = report.findings.iter().map(|f| f.rule.as_str()).collect();
        assert_eq!(rules, vec!["E0308", "clippy::needless_borrow"]);

        let lint = &report.findings[1];
        assert_eq!(lint.severity, Severity::Medium);
        assert_eq!(lint.path.as_deref(), Some("crates/rto-exec/src/lib.rs"));
        assert_eq!(lint.span.map(|s| (s.start, s.end)), Some((120, 132)));
        // The title is one line; the whole diagnostic survives in `message`.
        assert_eq!(
            lint.title,
            "this expression creates a reference which is immediately dereferenced by the compiler"
        );
        assert!(lint.message.contains("change this to remove the borrow"));
    }

    /// An absolute path inside the checkout is relativised, so the report reads
    /// the same on two machines and carries nobody's home directory.
    #[test]
    fn relativises_an_absolute_path_inside_the_worktree() {
        let (report, _) = Clippy::parse(STREAM.as_bytes(), &ctx()).expect("parse");
        let error = &report.findings[0];
        assert_eq!(error.path.as_deref(), Some("crates/roteiro/src/main.rs"));
        assert_eq!(error.severity, Severity::High);
    }

    /// The whole point of requiring `build-finished`: empty output from a cargo
    /// that never ran must not read as a tree with nothing wrong in it.
    #[test]
    fn refuses_a_stream_that_never_finished() {
        for native in [
            &b""[..],
            &b"\n"[..],
            &br#"{"reason":"compiler-artifact","fresh":true}"#[..],
            &b"error: no such command: `clippy`"[..],
        ] {
            let err = Clippy::parse(native, &ctx()).expect_err("must be refused");
            assert!(matches!(err, ExecError::MalformedReport(_)));
            assert!(
                err.to_string().contains("build-finished"),
                "the refusal must name what was missing: {err}"
            );
        }
    }

    #[test]
    fn a_completed_clean_build_is_a_valid_empty_report() {
        let (report, summary) =
            Clippy::parse(br#"{"reason":"build-finished","success":true}"#, &ctx()).expect("parse");
        assert!(report.findings.is_empty());
        assert!(summary.build_succeeded);
        assert_eq!(summary.compiler_messages, 0);
    }

    /// A diagnostic with no lint code still has a location, and losing it would
    /// hide a build failure behind an empty result.
    #[test]
    fn a_diagnostic_with_no_lint_code_is_reported_under_a_name() {
        // One cargo message per line, as cargo emits them: this parser reads a
        // stream, not a document, and a message split across lines is not one.
        let native = concat!(
            r#"{"reason":"compiler-message","message":{"message":"expected a semicolon","#,
            r#""level":"error","spans":[{"file_name":"src/a.rs","byte_start":4,"byte_end":5,"#,
            r#""line_start":1,"line_end":1,"column_start":5,"is_primary":true}]}}"#,
            "\n",
            r#"{"reason":"build-finished","success":false}"#
        );
        let (report, _) = Clippy::parse(native.as_bytes(), &ctx()).expect("parse");
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].rule, UNCODED_RULE);
    }

    /// The join in [`crate::crossref`] requires a `package` **and** a `version`
    /// in `meta`. A lint carries neither, so it cannot take part — and the cargo
    /// message's `package_id` must never be turned into them.
    #[test]
    fn carries_no_package_or_version_so_it_cannot_enter_the_dependency_join() {
        let (report, _) = Clippy::parse(STREAM.as_bytes(), &ctx()).expect("parse");
        for finding in &report.findings {
            assert!(finding.meta.get("package").is_none(), "{:?}", finding.meta);
            assert!(finding.meta.get("version").is_none(), "{:?}", finding.meta);
        }
    }

    /// The structural half of "a lint is never stored": `ingest` resolves an
    /// analyzer through the registry, and clippy is not in it. Adding it there
    /// would make `roteiro security ingest --analyzer clippy` file a layer.
    #[test]
    fn is_absent_from_the_registry_that_ingest_can_store() {
        assert!(
            adapter_for(ANALYZER).is_none(),
            "clippy must not be resolvable as a storable analyzer"
        );
        assert!(
            !known_analyzers().contains(&ANALYZER),
            "clippy must not be offered by `ingest`"
        );
    }

    #[test]
    fn maps_rustc_levels_and_keeps_an_unknown_one_verbatim() {
        for (raw, want) in [
            ("error", Severity::High),
            ("warning", Severity::Medium),
            ("note", Severity::Info),
            ("help", Severity::Info),
            ("failure-note", Severity::Info),
        ] {
            assert_eq!(severity(raw), want, "{raw}");
        }
        assert_eq!(severity("lint"), Severity::Other("lint".to_owned()));
    }

    #[test]
    fn the_invocation_mirrors_the_repository_gate_and_states_its_features() {
        let default = Clippy::invocation(&FeatureSet::Defaults);
        assert_eq!(default.program, "cargo");
        assert_eq!(default.args[0], "clippy");
        assert!(default.args.contains(&"--workspace".to_owned()));
        assert!(default.args.contains(&"--all-targets".to_owned()));
        assert!(default.args.contains(&"--message-format=json".to_owned()));
        // Reporting, not gating: the levels the repository declares are part of
        // what is being reported, so they are not overridden.
        assert!(!default.args.iter().any(|a| a == "-D" || a == "warnings"));
        // 101 is what cargo exits with when a denied lint fired, which is the
        // run that matters most.
        assert_eq!(default.success_statuses, vec![0, 101]);

        let all = Clippy::invocation(&FeatureSet::All);
        assert!(all.args.contains(&"--all-features".to_owned()));

        let some = Clippy::invocation(&FeatureSet::Explicit(vec![
            "serve".to_owned(),
            "mcp".to_owned(),
        ]));
        let at = some
            .args
            .iter()
            .position(|a| a == "--features")
            .expect("--features");
        assert_eq!(some.args[at + 1], "serve,mcp");
    }

    #[test]
    fn every_feature_set_labels_itself() {
        assert!(FeatureSet::Defaults.label().contains("default"));
        assert!(FeatureSet::All.label().contains("--all-features"));
        assert_eq!(
            FeatureSet::Explicit(vec!["a".to_owned(), "b".to_owned()]).label(),
            "a, b"
        );
    }

    /// The rule set is the toolchain, so there is nothing to provision and
    /// nothing to digest — the fact the whole decision turns on.
    #[test]
    fn declares_no_pinned_assets() {
        assert!(Clippy.asset_ids().is_empty());
        assert_eq!(Clippy.languages(), &["rust"]);
        assert!(!Clippy.summary().is_empty());
        assert_eq!(
            Clippy.command(&AssetPaths::default()),
            Clippy::invocation(&FeatureSet::Defaults)
        );
    }
}

//! The provisioning contract, end to end: warm cache runs, cold cache fails by
//! name, and hostile analyzer output is refused whole.
//!
//! These are ADR-0014's degradation promises. They are tested against the real
//! asset cache layout — a throwaway root under the temp directory — rather than
//! against a mock, because the promise is about what happens on a machine with
//! no network, and a mock cannot be cold.

#![cfg(feature = "exec-subprocess")]

use rto_exec::{
    AnalysisRequest, AnalyzerRunner, Consent, ExecError, IngestRunner, NativeContext, NoSnippets,
    Worktree, assets, normalize_native, subprocess::SubprocessRunner,
};
use rto_graph::{NetworkPolicy, SourceIdentity};

mod fixture;

/// A throwaway asset-cache root.
struct Cache(std::path::PathBuf);

impl Cache {
    fn cold(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("rto-exec-offline-{name}"));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("create the cache root");
        Self(dir)
    }

    fn warm(name: &str) -> Self {
        let cache = Self::cold(name);
        assets::provision(&cache.0, assets::asset("semgrep-rules").expect("spec"))
            .expect("provision the rule set");
        cache
    }
}

impl Drop for Cache {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// Cold cache, no network: refuse, and say exactly what is missing and what to
/// run. This is the failure ADR-0014 names, so its shape is pinned rather than
/// left to whatever the code happens to print.
#[test]
fn a_cold_cache_fails_by_name_with_an_actionable_message() {
    let cache = Cache::cold("cold");
    let err = SubprocessRunner::new("semgrep", &cache.0, true).expect_err("a cold cache must fail");

    let ExecError::AssetsUnavailableOffline {
        analyzer,
        missing,
        command,
    } = &err
    else {
        panic!("expected assets-unavailable-offline, got {err:?}");
    };
    assert_eq!(analyzer, "semgrep");
    assert_eq!(
        missing.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        vec!["semgrep-rules"]
    );
    assert_eq!(command, "roteiro security prefetch --analyzer semgrep");

    let message = err.to_string();
    for expected in [
        "assets-unavailable-offline",
        "semgrep-rules",
        "never provisioned",
        "roteiro security prefetch --analyzer semgrep",
        "never falls back",
    ] {
        assert!(
            message.contains(expected),
            "{expected:?} missing from:\n{message}"
        );
    }
}

/// Warm cache, no network: the runner is constructible, points at the pinned
/// rules, and the whole path up to executing the binary is satisfied. Execution
/// itself needs semgrep, which CI does not have — but everything Roteiro owns is
/// exercised here.
#[test]
fn a_warm_cache_resolves_without_any_network() {
    let cache = Cache::warm("warm");
    let runner = SubprocessRunner::new("semgrep", &cache.0, true).expect("a warm cache must work");

    let invocation = runner.invocation();
    let config = invocation
        .args
        .iter()
        .position(|a| a == "--config")
        .map(|i| invocation.args[i + 1].clone())
        .expect("a --config argument");
    assert_eq!(
        std::path::PathBuf::from(&config),
        assets::asset_path(&cache.0, assets::asset("semgrep-rules").expect("spec"))
    );
    assert!(
        std::path::Path::new(&config).is_file(),
        "the pinned rules must really be on disk at {config}"
    );
}

/// `prefetch` is the only thing that writes to the cache, and it needs no
/// network for a vendored asset — which is what lets a fresh machine with no
/// connection provision and then scan.
#[test]
fn provisioning_the_rule_set_needs_no_network() {
    let cache = Cache::cold("provision");
    let spec = assets::asset("semgrep-rules").expect("spec");
    let record = assets::provision(&cache.0, spec).expect("provision");
    assert_eq!(record.digest.len(), 64);

    let status = assets::status(&cache.0, Some("semgrep"));
    assert_eq!(status.len(), 1);
    assert_eq!(status[0].verified, Some(true));
    assert_eq!(status[0].age_days, Some(0));
}

/// A run must never provision. If it did, "was this machine provisioned?" would
/// be unanswerable after the fact, and the recorded `rules_digest` would stop
/// meaning anything.
#[test]
fn a_refused_run_leaves_the_cache_untouched() {
    let cache = Cache::cold("no-implicit-fetch");
    let spec = assets::asset("semgrep-rules").expect("spec");
    let before = assets::asset_path(&cache.0, spec).exists();

    SubprocessRunner::new("semgrep", &cache.0, true).expect_err("a cold cache must fail");

    assert!(!before);
    assert!(
        !assets::asset_path(&cache.0, spec).exists(),
        "a refused run must not have provisioned anything"
    );
    assert!(assets::installed(&cache.0, spec).is_none());
}

/// Hostile or broken analyzer output is refused whole. There is no partial
/// result and nothing is written: a report is untrusted input, and half of one
/// in the store is worse than none.
#[test]
fn hostile_analyzer_output_is_refused_with_no_partial_result() {
    let source = SourceIdentity::default();
    let ctx = NativeContext {
        started_at: "2026-08-15T09:00:00Z".to_owned(),
        ended_at: "2026-08-15T09:00:09Z".to_owned(),
        analyzer_version: Some("1.136.0".to_owned()),
        exit_status: 1,
        source: &source,
        rules_digest: None,
        advisory_db: None,
        worktree: None,
        snippets: &NoSnippets,
    };

    // Not this analyzer's format at all.
    assert!(normalize_native("semgrep", br#"{"vulnerabilities":{}}"#, &ctx).is_err());
    assert!(normalize_native("cargo-audit", br#"{"results":[]}"#, &ctx).is_err());
    // Not JSON.
    assert!(normalize_native("semgrep", b"<html>500</html>", &ctx).is_err());
    // An analyzer this build cannot read.
    assert!(matches!(
        normalize_native("totally-made-up", b"{}", &ctx),
        Err(ExecError::UnknownAnalyzer { .. })
    ));

    // A path climbing out of the worktree is refused at the shared validation,
    // *after* the adapter parsed it happily — which is the point of validating
    // once, centrally.
    let escaping = br#"{"version":"1.0","results":[{
        "check_id":"r","path":"../../../etc/shadow",
        "start":{"offset":0},"end":{"offset":4},
        "extra":{"message":"m","severity":"ERROR"}}]}"#;
    let report = normalize_native("semgrep", escaping, &ctx).expect("the adapter parses it");
    let wire = serde_json::to_vec(&report).expect("serialize");
    let request = AnalysisRequest {
        analyzer: "semgrep".to_owned(),
        worktree: Worktree::read_only(fixture::polyglot_root().as_path()).expect("worktree"),
        network: NetworkPolicy::Deny,
        consent: Consent::Granted,
        source: SourceIdentity::default(),
    };
    assert!(matches!(
        IngestRunner::new(wire).run(&request),
        Err(ExecError::PathEscapesWorktree(_))
    ));
}

/// The `--allow-unsandboxed` flag is the whole consent mechanism for a backend
/// with no boundary. Without it there is no runner at all — not a runner that
/// declines later.
#[test]
fn the_unsandboxed_flag_is_required_even_with_a_warm_cache() {
    let cache = Cache::warm("flag");
    let err = SubprocessRunner::new("semgrep", &cache.0, false).expect_err("must refuse");
    let message = err.to_string();
    assert!(message.contains("--allow-unsandboxed"), "{message}");
    assert!(message.contains("no isolation"), "{message}");
}

/// Roteiro never fetches the advisory database, and says so with the command
/// that obtains it rather than going and getting it.
#[test]
fn the_advisory_database_is_explained_never_fetched() {
    let cache = Cache::cold("advisory");
    let spec = assets::asset("rustsec-advisory-db").expect("spec");
    let err = assets::provision(&cache.0, spec).expect_err("must not fetch");
    let message = err.to_string();
    assert!(message.contains("advisory-db"), "{message}");
    assert!(message.contains("roteiro security prefetch"), "{message}");
    assert!(
        !assets::asset_path(&cache.0, spec).exists(),
        "nothing may have been fetched"
    );
}

/// Real `cargo audit --json` output records a `+02:00` offset on its database
/// timestamp. The staleness age has to survive that, because an advisory-DB age
/// that silently fails to compute is a *possibly stale* label nobody ever sees.
#[test]
fn a_real_advisory_database_timestamp_yields_an_age() {
    let source = SourceIdentity::default();
    let ctx = NativeContext {
        started_at: "2026-08-15T09:00:00Z".to_owned(),
        ended_at: "2026-08-15T09:00:02Z".to_owned(),
        analyzer_version: Some("0.22.2".to_owned()),
        exit_status: 1,
        source: &source,
        rules_digest: None,
        advisory_db: None,
        worktree: None,
        snippets: &NoSnippets,
    };
    let report = normalize_native("cargo-audit", &fixture::cargo_audit_native(), &ctx)
        .expect("normalise real output");
    let db = report.advisory_db.expect("real output names its database");
    let published = db.published_at.expect("and when it was published");
    assert!(
        published.contains("+02:00"),
        "the fixture no longer exercises the offset case: {published}"
    );
    let age = rto_exec::age_in_days(&published, "2026-08-15T09:00:00Z")
        .expect("an age must be computable from what the tool really emits");
    assert_eq!(age, 2, "2026-08-12T12:42:29+02:00 is 2 days before the run");
}

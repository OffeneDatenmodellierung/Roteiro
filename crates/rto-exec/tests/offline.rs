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

// ---------------------------------------------------------------------------
// The download-by-URL asset (`osv-scanner`'s OSV databases), Stage 22b.
//
// The fetcher is injected, so every one of these runs with no network at all —
// which is the only honest way to test an offline contract.
// ---------------------------------------------------------------------------

/// A fetcher that writes fixed bytes and records what it was asked for. Never
/// opens a socket.
fn recording_fetcher(
    log: &std::cell::RefCell<Vec<String>>,
) -> impl Fn(&str, &std::path::Path) -> Result<(), String> + '_ {
    move |url, path| {
        log.borrow_mut().push(url.to_owned());
        std::fs::write(path, format!("zip-bytes-for {url}").as_bytes())
            .map_err(|e| format!("{e}"))?;
        Ok(())
    }
}

/// A run must never fetch. `resolve` — the only thing a run calls — has no
/// fetcher to pass, so a cold cache fails by name with the prefetch command
/// rather than quietly acquiring a quarter-gigabyte of databases mid-scan.
#[test]
fn a_run_with_no_osv_database_fails_by_name_and_never_downloads() {
    let cache = Cache::cold("osv-cold");
    let err =
        SubprocessRunner::new("osv-scanner", &cache.0, true).expect_err("a cold cache must fail");
    let ExecError::AssetsUnavailableOffline {
        analyzer,
        missing,
        command,
    } = &err
    else {
        panic!("expected assets-unavailable-offline, got {err:?}");
    };
    assert_eq!(analyzer, "osv-scanner");
    assert_eq!(
        missing.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        vec!["osv-db"]
    );
    assert_eq!(command, "roteiro security prefetch --analyzer osv-scanner");
    assert!(
        !assets::asset_path(&cache.0, assets::asset("osv-db").expect("spec")).exists(),
        "a refused run must not have fetched anything"
    );
}

/// Provisioning without a fetcher is refused too, and the refusal names the
/// command that would work. `provision` is what every path except `roteiro
/// security prefetch --allow-download` calls.
#[test]
fn provisioning_the_osv_database_without_a_fetcher_is_refused_with_the_command() {
    let cache = Cache::cold("osv-nofetch");
    let spec = assets::asset("osv-db").expect("spec");
    let err = assets::provision(&cache.0, spec).expect_err("must not fetch");
    let message = err.to_string();
    assert!(message.contains("does not download"), "{message}");
    assert!(
        message.contains("roteiro security prefetch --analyzer osv-scanner"),
        "{message}"
    );
    // It names a concrete URL, so a reader can see what it would have fetched.
    assert!(message.contains("osv-vulnerabilities.storage"), "{message}");
    assert!(
        !assets::asset_path(&cache.0, spec).exists(),
        "nothing may have been fetched"
    );
}

/// With a fetcher, every declared database is fetched into the layout
/// `osv-scanner --local-db-path` expects, and the result is digest-pinned like
/// any other asset.
#[test]
fn provisioning_with_a_fetcher_installs_every_database_and_pins_it() {
    let cache = Cache::cold("osv-fetch");
    let spec = assets::asset("osv-db").expect("spec");
    let log = std::cell::RefCell::new(Vec::new());
    let fetch = recording_fetcher(&log);

    let record = assets::provision_with(&cache.0, spec, Some(&fetch)).expect("provision");
    let urls = log.borrow().clone();
    assert_eq!(
        urls.len(),
        rto_exec::assets::OSV_DATABASES.len(),
        "every declared database must be fetched"
    );
    assert!(urls.iter().all(|u| u.starts_with("https://")));
    assert_eq!(record.files, Some(urls.len()));
    assert_eq!(record.digest.len(), 64);

    // The layout is not ours to choose: the scanner looks under
    // `<dir>/osv-scalibr/<ECOSYSTEM>/all.zip`.
    let db = assets::asset_path(&cache.0, spec);
    for ecosystem in ["crates.io", "PyPI", "Maven", "npm"] {
        assert!(
            db.join("osv-scalibr")
                .join(ecosystem)
                .join("all.zip")
                .is_file(),
            "no {ecosystem} database at the layout osv-scanner reads"
        );
    }
    // …and a run now resolves, still without a network.
    let resolved = assets::resolve(&cache.0, "osv-scanner").expect("a warm cache must resolve");
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].1, db);
}

/// Re-running `prefetch` over a warm cache needs neither the flag nor the
/// network: it re-digests and re-stamps what is already there. That is what
/// makes `prefetch` safe to run whenever you are unsure.
#[test]
fn reprovisioning_a_warm_osv_cache_needs_no_fetcher() {
    let cache = Cache::cold("osv-rewarm");
    let spec = assets::asset("osv-db").expect("spec");
    let log = std::cell::RefCell::new(Vec::new());
    let fetch = recording_fetcher(&log);
    let first = assets::provision_with(&cache.0, spec, Some(&fetch)).expect("first");
    let fetched = log.borrow().len();

    let second = assets::provision(&cache.0, spec).expect("a warm cache re-provisions");
    assert_eq!(first.digest, second.digest);
    assert_eq!(
        log.borrow().len(),
        fetched,
        "re-provisioning a warm cache must not fetch again"
    );
}

/// A database edited after provisioning is refused, not warned about: a run
/// would otherwise stamp an advisory-database digest that does not describe
/// what it actually read.
#[test]
fn an_osv_database_edited_after_provisioning_is_refused() {
    let cache = Cache::cold("osv-tamper");
    let spec = assets::asset("osv-db").expect("spec");
    let log = std::cell::RefCell::new(Vec::new());
    let fetch = recording_fetcher(&log);
    assets::provision_with(&cache.0, spec, Some(&fetch)).expect("provision");

    let zip = assets::asset_path(&cache.0, spec)
        .join("osv-scalibr")
        .join("npm")
        .join("all.zip");
    std::fs::write(&zip, b"tampered").expect("tamper");

    let err = assets::resolve(&cache.0, "osv-scanner").expect_err("tampering must be refused");
    let ExecError::AssetsUnavailableOffline { missing, .. } = &err else {
        panic!("expected the offline error, got {err:?}");
    };
    assert!(
        missing[0].reason.contains("no longer match"),
        "{}",
        missing[0].reason
    );
}

/// The provisioning record is what `security status` reports, and for a database
/// that means the digest a run will be checked against.
#[test]
fn a_provisioned_osv_database_shows_up_in_status_as_verified() {
    let cache = Cache::cold("osv-status");
    let spec = assets::asset("osv-db").expect("spec");
    let log = std::cell::RefCell::new(Vec::new());
    let fetch = recording_fetcher(&log);
    assets::provision_with(&cache.0, spec, Some(&fetch)).expect("provision");

    let status = assets::status(&cache.0, Some("osv-scanner"));
    assert_eq!(status.len(), 1);
    assert_eq!(status[0].id, "osv-db");
    assert_eq!(status[0].kind, assets::AssetKind::AdvisoryDb);
    assert_eq!(status[0].verified, Some(true));
    assert_eq!(status[0].age_days, Some(0));
}

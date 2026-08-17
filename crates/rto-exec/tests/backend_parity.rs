//! Stage 24's definition of done, executed rather than argued.
//!
//! > The same analyzer produces the same findings via subprocess and via
//! > boxlite, differing only in the isolation label and image digest.
//!
//! This runs a **real `semgrep`** twice over the same tree — once as a host
//! child process, once inside a digest-pinned microVM — and compares what comes
//! back. It is not a mock: if the two disagree, something in the normalized
//! schema is leaking the environment it ran in, and that matters more than the
//! backend does.
//!
//! # Why the comparison is meaningful rather than circular
//!
//! Both backends hand the analyzer's raw stdout to the same [`rto_exec::Adapter`]
//! and the same `assemble`, so a *shared* bug would be invisible here. What this
//! catches is the class of difference the backends really can introduce:
//! filesystem paths, the rule set actually read, the analyzer version, locale
//! and environment. Those are exactly the things that turn one finding into two.
//!
//! # Skipping
//!
//! Every precondition prints why it skipped. A silent skip is how a test that
//! covers nothing gets mistaken for a test that passed.

#![cfg(all(feature = "exec-subprocess", feature = "exec-boxlite"))]

use std::path::{Path, PathBuf};

use rto_exec::{
    AnalysisRequest, AnalyzerRunner, BoxliteRunner, Consent, SubprocessRunner, Worktree,
};
use rto_graph::{Finding, Isolation, NetworkPolicy, RunnerKind, SourceIdentity};

/// A tree with something for the baseline rule set to find, in two languages.
///
/// Two languages on purpose: a single-language scan could agree by accident if
/// only one parser were reachable inside the image.
const FIXTURES: &[(&str, &str)] = &[
    (
        "src/shell.rs",
        r#"use std::process::Command;

pub fn run(script: &str) {
    let _ = Command::new("sh").arg("-c").arg(script).output();
}

pub fn home() -> String {
    std::env::var("HOME").unwrap()
}
"#,
    ),
    (
        "tools/run.py",
        r"import subprocess


def go(cmd):
    subprocess.run(cmd, shell=True)


def evaluate(src):
    return eval(src)
",
    ),
];

#[test]
fn the_same_analyzer_gives_the_same_findings_in_both_backends() {
    let Some(root) = preconditions() else {
        return;
    };
    let tree = fixture_tree();

    let request = AnalysisRequest {
        analyzer: "semgrep".to_owned(),
        worktree: Worktree::read_only(tree.path()).expect("worktree"),
        network: NetworkPolicy::Deny,
        consent: Consent::Granted,
        source: SourceIdentity::default(),
    };

    let host = SubprocessRunner::new("semgrep", &root, true).expect("subprocess runner");
    let sandbox = BoxliteRunner::new("semgrep", &root).expect("boxlite runner");

    let host_run = host.run(&request).expect("the host scan must succeed");
    let sandbox_run = sandbox
        .run(&request)
        .expect("the sandboxed scan must succeed");

    // The scan has to have found something, or "identical" is a claim about two
    // empty lists and this test would pass while proving nothing.
    assert!(
        host_run.findings.len() >= 3,
        "the fixture should trip several baseline rules, got {}",
        host_run.findings.len()
    );

    assert_eq!(
        keys(&host_run.findings),
        keys(&sandbox_run.findings),
        "finding identities diverge between backends — the normalized schema is \
         leaking something environment-dependent"
    );
    assert_eq!(
        host_run.findings, sandbox_run.findings,
        "findings differ beyond their identities"
    );

    // Now the run records: everything must match except the three fields that
    // *describe* where it ran, plus the timestamps and the raw-bytes digest.
    let (a, b) = (&host_run.run, &sandbox_run.run);

    assert_eq!(a.runner, RunnerKind::Subprocess);
    assert_eq!(b.runner, RunnerKind::Sandboxed);
    assert_eq!(a.isolation, Isolation::None);
    assert_eq!(b.isolation, Isolation::MicroVm);
    assert_eq!(a.image_digest, None, "a host run ran in no image");
    assert_eq!(
        b.image_digest.as_deref(),
        Some(sandbox.image().digest),
        "a sandboxed run must stamp the image it ran in"
    );

    assert_eq!(a.layer, b.layer, "the same tree is the same layer");
    assert_eq!(a.analyzer, b.analyzer);
    assert_eq!(
        a.analyzer_version, b.analyzer_version,
        "the pinned image must carry the same semgrep as the host, or the two runs \
         are not comparable — repin SANDBOX_IMAGES to match, or install the matching host \
         version"
    );
    assert_eq!(
        a.rules_digest, b.rules_digest,
        "both backends must read the same rule set — the guest reads the host's file \
         through a read-only mount, so this is one digest of one artifact"
    );
    assert_eq!(a.advisory_db, b.advisory_db);
    assert_eq!(a.source, b.source);
    assert_eq!(a.exit_status, b.exit_status);
    assert_eq!(
        a.command_policy.worktree, b.command_policy.worktree,
        "both mount the tree read-only"
    );
    assert_eq!(
        a.command_policy.environment, b.command_policy.environment,
        "both record a scrubbed environment"
    );
    // Both record `network: Deny`. Only one of them *enforces* it — see the
    // module docs on `rto_exec::subprocess` — and that difference is carried by
    // the isolation label above, which is exactly where it belongs.
    assert_eq!(a.command_policy.network, b.command_policy.network);

    eprintln!(
        "PARITY OK: {} findings identical; subprocess isolation={} image=none, \
         boxlite isolation={} image={}",
        host_run.findings.len(),
        a.isolation.as_str(),
        b.isolation.as_str(),
        b.image_digest.as_deref().unwrap_or("-")
    );
}

/// A sandboxed run must produce a full result with **no network at all**, once
/// its inputs are provisioned.
///
/// This is ADR-0014's "offline-capable once provisioned", checked rather than
/// asserted: egress is denied inside the guest by construction, and the pinned
/// image and rule set are already local, so a successful scan here is a scan
/// that needed nothing from a network.
#[test]
fn a_provisioned_sandbox_scans_with_no_network() {
    let Some(root) = preconditions() else {
        return;
    };
    let tree = fixture_tree();

    let sandbox = BoxliteRunner::new("semgrep", &root).expect("boxlite runner");
    let response = sandbox
        .run(&AnalysisRequest {
            analyzer: "semgrep".to_owned(),
            worktree: Worktree::read_only(tree.path()).expect("worktree"),
            network: NetworkPolicy::Deny,
            consent: Consent::Granted,
            source: SourceIdentity::default(),
        })
        .expect("a provisioned sandbox must scan without a network");

    assert!(!response.findings.is_empty());
    assert_eq!(response.run.isolation, Isolation::MicroVm);
    assert_eq!(
        response.run.image_digest.as_deref(),
        Some(sandbox.image().digest)
    );
    eprintln!(
        "OFFLINE OK: {} findings from a guest with no network interface",
        response.findings.len()
    );
}

/// The rendered identity of each finding, in order.
fn keys(findings: &[Finding]) -> Vec<String> {
    findings.iter().map(|f| f.key.to_string()).collect()
}

/// Everything the two runs need, or `None` with a printed reason.
fn preconditions() -> Option<PathBuf> {
    if which("semgrep").is_none() {
        eprintln!(
            "SKIPPED: `semgrep` is not on PATH, so the subprocess half of the comparison \
             cannot run. Install it to exercise this test."
        );
        return None;
    }

    match rto_exec::sandbox_probe() {
        rto_exec::SandboxProbe::Available => {}
        rto_exec::SandboxProbe::Unavailable(why) => {
            eprintln!(
                "SKIPPED: no microVM is available on this host, so the sandboxed half cannot \
                 run: {why}\n         (this is the expected state on a CI runner with no \
                 /dev/kvm — the ingest and subprocess paths carry the functional coverage there)"
            );
            return None;
        }
    }

    let root = rto_exec::asset_root();
    // The rule set is vendored, so this needs no network and is safe to do here.
    if let Err(e) = rto_exec::provision(&root, rto_exec::asset("semgrep-rules")?) {
        eprintln!("SKIPPED: the baseline rule set could not be provisioned: {e}");
        return None;
    }

    // The image is never pulled by a run, and this test is a run.
    if let Err(e) = BoxliteRunner::new("semgrep", &root) {
        eprintln!(
            "SKIPPED: the sandboxed backend is not ready on this host: {e}\n         \
             provision it with `roteiro security prefetch --analyzer sandbox --allow-download`"
        );
        return None;
    }

    // Constructing the runner does *not* prove the image is there — the runner
    // checks that only when it runs, so this guard used to pass on a host with a
    // working sandbox and an empty image store, and the missing setup step then
    // surfaced as `ImageNotProvisioned` from the assertion itself. A skipped
    // step reading as a broken backend is the failure this check exists to
    // prevent, and it is why the recipe below is spelled out rather than named.
    match rto_exec::boxlite::image_is_provisioned("semgrep", &root) {
        Ok(true) => Some(root),
        Ok(false) => {
            eprintln!(
                "SKIPPED: the pinned semgrep image is not in the local store, so the \
                 sandboxed half cannot run.\n         Provision it with:\n\n           \
                 cargo run -p roteiro --features exec-boxlite -- \\\n             \
                 security prefetch --analyzer semgrep --allow-download\n\n         \
                 The `--features exec-boxlite` is load-bearing: the image half of \
                 `prefetch` is compiled out of any build without it, so the runtime-only \
                 recipe in AGENTS.md provisions the archive and silently not the image."
            );
            None
        }
        // Deliberately not a skip. "Definitely absent" and "could not tell" are
        // different answers, and only the first is safe to treat as "nothing to
        // run here" — a store that will not open is a broken host, and skipping
        // on it would hide exactly what a skip is supposed to make visible.
        Err(e) => panic!(
            "the local image store could not be read, so whether this host can run the sandboxed half is unknown: {e}"
        ),
    }
}

/// A throwaway checkout containing [`FIXTURES`].
struct FixtureTree(PathBuf);

impl FixtureTree {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for FixtureTree {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// A fresh checkout of [`FIXTURES`], at a path no other test can be using.
///
/// **The uniqueness is the point, not tidiness.** Both tests in this file build
/// a tree and `libtest` runs them on separate threads by default, so a shared
/// fixed path meant one test's setup could `remove_dir_all` the tree the other
/// was mid-scan on. The failure would have been intermittent and would have
/// looked like a parity mismatch — a flake in exactly the assertion that is
/// supposed to be the most trustworthy thing in this PR.
///
/// The suffix is the process id plus a per-process counter: unique across
/// concurrent tests within a run, and across two runs happening at once, without
/// pulling in a random-number generator for a directory name.
fn fixture_tree() -> FixtureTree {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);

    let unique = format!(
        "rto-exec-backend-parity-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let dir = std::env::temp_dir().join(unique);
    std::fs::remove_dir_all(&dir).ok();
    for (path, body) in FIXTURES {
        let full = dir.join(path);
        std::fs::create_dir_all(full.parent().expect("parent")).expect("mkdir");
        std::fs::write(&full, body).expect("write fixture");
    }
    FixtureTree(dir)
}

/// Two fixture trees taken at once never share a path.
///
/// Cheap, and it is the property the parity tests silently depend on: if this
/// ever stopped holding they would not fail here, they would fail
/// intermittently over there, as a false parity mismatch.
#[test]
fn concurrent_fixture_trees_do_not_share_a_directory() {
    let (a, b) = (fixture_tree(), fixture_tree());
    assert_ne!(a.path(), b.path());
    assert!(a.path().join("src/shell.rs").is_file());
    assert!(b.path().join("src/shell.rs").is_file());

    // And dropping one leaves the other intact — the exact interference the
    // shared path allowed.
    let surviving = b.path().to_path_buf();
    drop(a);
    assert!(
        surviving.join("src/shell.rs").is_file(),
        "one tree's teardown removed another's fixture"
    );
}

/// Where `name` is on `PATH`, if anywhere.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

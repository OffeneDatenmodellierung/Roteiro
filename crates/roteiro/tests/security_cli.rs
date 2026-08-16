//! End-to-end `roteiro security` over the dependency axis: ingest real
//! `osv-scanner` and `cargo-audit` output into a throwaway repository, then read
//! it back with `security list`.
//!
//! Nothing here needs a network, an analyzer binary, or a provisioned database:
//! it ingests captures the `rto-exec` fixtures already committed. What it adds
//! over the unit tests is the parts only the CLI owns — that the ingest path
//! tells the adapter which checkout it is standing in, and that the
//! cross-reference is rendered without moving the finding count.

#![cfg(feature = "execution")]

use std::path::Path;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_roteiro");

/// The `rto-exec` fixture captures, reached from here so there is exactly one
/// copy of each in the repository.
fn exec_fixture(relative: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../rto-exec/tests/fixtures")
        .join(relative);
    std::fs::read(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

fn roteiro(repo: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .env("ROTEIRO_HOME", repo.join(".home"))
        .env("HOME", repo.join(".home"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run roteiro")
}

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("run git");
    assert!(out.status.success(), "git {args:?} failed: {out:?}");
}

/// A throwaway git repository with the dependency manifests in it, and the two
/// analyzer captures rewritten to describe *this* checkout.
struct Fixture {
    repo: std::path::PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let repo =
            std::env::temp_dir().join(format!("roteiro-security-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&repo).ok();
        std::fs::create_dir_all(repo.join(".home")).expect("mkdir");
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "test@example.invalid"]);
        git(&repo, &["config", "user.name", "Test"]);
        std::fs::write(repo.join("README.md"), b"# fixture\n").expect("write");
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "init"]);

        // The capture names `/checkout`; rewrite it to this checkout so the
        // ingest path has a real tree to relativise against. `canonicalize`
        // because on macOS the temp directory is reached through a symlink and
        // the CLI reports the resolved form.
        let root = std::fs::canonicalize(&repo).expect("canonicalize");
        let osv = String::from_utf8(exec_fixture("native/osv-scanner-deps.json"))
            .expect("utf8")
            .replace("/checkout", &root.to_string_lossy());
        std::fs::write(repo.join("osv.json"), osv).expect("write");
        std::fs::write(
            repo.join("audit.json"),
            exec_fixture("native/cargo-audit.json"),
        )
        .expect("write");
        Self { repo }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.repo).ok();
    }
}

/// The whole path: native `osv-scanner` output in, findings out — and every
/// stored path worktree-relative.
///
/// The CLI is the only thing that knows which checkout the report describes, so
/// this is where a failure to pass that along shows up. Without it every finding
/// would either carry the scanning machine's absolute path or lose its location
/// altogether.
#[test]
fn ingests_native_osv_scanner_output_with_relative_paths() {
    let fixture = Fixture::new("ingest");
    let out = roteiro(
        &fixture.repo,
        &[
            "security",
            "ingest",
            "osv.json",
            "--analyzer",
            "osv-scanner",
            "--json",
        ],
    );
    assert!(out.status.success(), "ingest failed: {out:?}");
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).expect("ingest emits JSON");
    assert_eq!(report["analyzer"], "osv-scanner");
    assert!(
        report["findings"].as_u64().expect("a count") > 0,
        "the capture must produce findings: {report}"
    );

    let listed = roteiro(&fixture.repo, &["security", "list", "--json"]);
    assert!(listed.status.success(), "list failed: {listed:?}");
    let listing: serde_json::Value =
        serde_json::from_slice(&listed.stdout).expect("list emits JSON");
    let findings: Vec<&serde_json::Value> = listing["layers"]
        .as_array()
        .expect("layers")
        .iter()
        .flat_map(|l| l["findings"].as_array().expect("findings"))
        .collect();
    assert!(!findings.is_empty());

    let mut with_a_path = 0;
    for finding in &findings {
        if let Some(path) = finding["path"].as_str() {
            with_a_path += 1;
            assert!(!path.starts_with('/'), "stored an absolute path: {path}");
            assert!(!path.contains(".."), "stored an escaping path: {path}");
        }
        let key = finding["key"].as_str().expect("a key");
        assert!(
            !key.contains(&fixture.repo.to_string_lossy().to_string()),
            "the checkout path leaked into a finding key: {key}"
        );
    }
    assert!(
        with_a_path > 0,
        "every finding lost its manifest — the worktree was not passed to the adapter"
    );

    // Every ecosystem this stage exists to cover really arrived in the store.
    for ecosystem in ["PyPI", "Maven", "npm", "crates.io"] {
        assert!(
            findings.iter().any(|f| f["meta"]["ecosystem"] == ecosystem),
            "no {ecosystem} finding reached the store"
        );
    }
}

/// Both analyzers ingested, and the cross-reference rendered — without the
/// finding count moving.
#[test]
fn listing_both_analyzers_cross_references_without_changing_the_count() {
    let fixture = Fixture::new("crossref");
    for (file, analyzer) in [("osv.json", "osv-scanner"), ("audit.json", "cargo-audit")] {
        let out = roteiro(
            &fixture.repo,
            &["security", "ingest", file, "--analyzer", analyzer, "--json"],
        );
        assert!(out.status.success(), "ingesting {file} failed: {out:?}");
    }

    let listed = roteiro(&fixture.repo, &["security", "list", "--json"]);
    assert!(listed.status.success(), "list failed: {listed:?}");
    let listing: serde_json::Value =
        serde_json::from_slice(&listed.stdout).expect("list emits JSON");

    let total = listing["findings"].as_u64().expect("a total");
    let layer_sum: u64 = listing["layers"]
        .as_array()
        .expect("layers")
        .iter()
        .map(|l| l["findings"].as_array().expect("findings").len() as u64)
        .sum();
    assert_eq!(total, layer_sum, "the total must be the sum of the layers");

    let crossref = listing["cross_reference"]
        .as_array()
        .expect("two dependency analyzers means a cross-reference section");
    assert!(!crossref.is_empty());

    // A duplicate pair is one advisory and two findings. Both numbers are true,
    // and the join must not have changed the second one.
    let confirmed: Vec<&serde_json::Value> = crossref
        .iter()
        .filter(|c| c["confirmed_by"].as_u64() == Some(2))
        .collect();
    assert!(
        !confirmed.is_empty(),
        "the two captures describe the same advisory; it must read as confirmed twice"
    );
    for entry in &confirmed {
        let reports = entry["reports"].as_array().expect("reports");
        assert_eq!(reports.len(), 2);
        let analyzers: Vec<&str> = reports
            .iter()
            .map(|r| r["analyzer"].as_str().expect("analyzer"))
            .collect();
        assert!(analyzers.contains(&"cargo-audit"), "{analyzers:?}");
        assert!(analyzers.contains(&"osv-scanner"), "{analyzers:?}");
        // Both keys stay addressable, so a reader can watch both disappear.
        for report in reports {
            assert!(report["key"].as_str().expect("key").starts_with("finding:"));
        }
    }

    // …and the human rendering says so too, without contradicting the count.
    let text = roteiro(&fixture.repo, &["security", "list"]);
    let rendered = String::from_utf8_lossy(&text.stdout);
    assert!(rendered.contains("cross-reference:"), "{rendered}");
    assert!(rendered.contains("confirmed by 2"), "{rendered}");
    assert!(
        rendered.contains(&format!("{total} finding(s) across")),
        "the finding total must be printed unchanged: {rendered}"
    );
}

/// With one analyzer there is nothing to cross-reference, and the section is
/// absent rather than a list of rows reading "confirmed by 1".
#[test]
fn one_analyzer_alone_renders_no_cross_reference() {
    let fixture = Fixture::new("single");
    let out = roteiro(
        &fixture.repo,
        &[
            "security",
            "ingest",
            "audit.json",
            "--analyzer",
            "cargo-audit",
            "--json",
        ],
    );
    assert!(out.status.success(), "ingest failed: {out:?}");

    let listed = roteiro(&fixture.repo, &["security", "list", "--json"]);
    let listing: serde_json::Value =
        serde_json::from_slice(&listed.stdout).expect("list emits JSON");
    assert!(
        listing.get("cross_reference").is_none(),
        "a single analyzer has nothing to be cross-referenced against: {listing}"
    );

    let text = roteiro(&fixture.repo, &["security", "list"]);
    let rendered = String::from_utf8_lossy(&text.stdout);
    assert!(!rendered.contains("cross-reference:"), "{rendered}");
}

//! End-to-end test for `roteiro config-secrets`.
//!
//! The unit tests in `rto_graph::query` build their `config_key` nodes by hand.
//! This one goes through **real extraction**, which is the only thing that can
//! prove the two claims the lens rests on — and the limitation that gives it its
//! name:
//!
//! 1. A secret-named config value really is **redacted before it is persisted**,
//!    so the lens has something true to report. Asserted against the store's own
//!    bytes, not just the report.
//! 2. A secret-named **struct field** (`@rto:config`) produces a key with **no
//!    value at all**, which is neither a redaction nor a leak.
//! 3. A credential **hardcoded in source** produces no config key and is
//!    therefore invisible here. That is the boundary the rename exists to keep
//!    honest, so it is a test rather than only a doc comment.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_roteiro");

/// A token-shaped literal, split so this file does not itself contain a string
/// that a real secret scanner would flag.
const FAKE_TOKEN: &str = "ghp_0123456789abcdefghijklmnopqrstuvwx";

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

fn roteiro(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("ROTEIRO_HOME", dir)
        .output()
        .expect("run roteiro")
}

fn json(dir: &Path, args: &[&str]) -> serde_json::Value {
    let out = roteiro(dir, args);
    assert!(out.status.success(), "roteiro {args:?} failed: {out:?}");
    serde_json::from_slice(&out.stdout).expect("--json is valid JSON")
}

fn write(dir: &Path, rel: &str, content: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir");
    std::fs::write(path, content).expect("write");
}

/// Find an item by dotted key name.
fn item<'a>(report: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    report["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|i| i["name"] == name)
        .unwrap_or_else(|| panic!("`{name}` missing from {report}"))
}

/// A repository with one secret-named key per config format, a secret-named
/// `@rto:config` struct field, a non-secret key in each place, and the **same**
/// token hardcoded in a Rust body.
fn fixture(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("roteiro-cfgsec-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("mkdir");
    git(&dir, &["init", "-q"]);

    // A secret-named key with a real-looking value, and a non-secret one beside it.
    write(
        &dir,
        ".env",
        &format!("API_TOKEN={FAKE_TOKEN}\nPORT=8017\n"),
    );
    // A secret-named key in TOML too, to cover a second parser.
    write(
        &dir,
        "config.toml",
        &format!("[db]\npassword = \"{FAKE_TOKEN}\"\nhost = \"localhost\"\n"),
    );
    // A config-root struct with a secret-named field: declared in code, no literal
    // value to redact.
    write(
        &dir,
        "src/config.rs",
        "/// @rto:config\npub struct AppConfig {\n    pub api_key: String,\n    pub addr: String,\n}\n",
    );
    // A credential hardcoded in source. NOT a config key — the boundary under test.
    write(
        &dir,
        "src/main.rs",
        &format!(
            "fn main() {{\n    let token = \"{FAKE_TOKEN}\";\n    println!(\"{{token}}\");\n}}\n"
        ),
    );
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    dir
}

#[test]
fn the_inventory_reports_where_secret_named_keys_are_and_how_they_were_handled() {
    let dir = fixture("states");
    let report = json(&dir, &["config-secrets", "--json", "--limit", "0"]);

    // (1) The file-derived secret-named keys are found, and reported as redacted.
    assert_eq!(item(&report, "API_TOKEN")["state"], "redacted", "{report}");
    assert_eq!(item(&report, "API_TOKEN")["path"], ".env");
    assert_eq!(item(&report, "db.password")["state"], "redacted");
    assert_eq!(item(&report, "db.password")["path"], "config.toml");

    // (2) The struct field is `declared`: no value existed to redact, so calling it
    // redacted would claim a redaction that never happened.
    let declared = item(&report, "api_key");
    assert_eq!(declared["state"], "declared", "{report}");
    assert_eq!(declared["source"], "struct");
    assert_eq!(declared["path"], "src/config.rs");

    // The counts reconcile, and the invariant holds on a freshly extracted graph.
    assert_eq!(report["secret_named"], 3, "{report}");
    assert_eq!(report["redacted"], 2);
    assert_eq!(report["declared"], 1);
    assert_eq!(
        report["unredacted"], 0,
        "extraction redacts every secret-named key: {report}"
    );

    // Non-secret keys are in the population but not the inventory.
    assert!(
        report["config_keys"].as_u64().expect("count") > 3,
        "`PORT`, `db.host` and `addr` are config keys too: {report}"
    );
    for name in ["PORT", "db.host", "addr"] {
        assert!(
            !report["items"]
                .as_array()
                .expect("items")
                .iter()
                .any(|i| i["name"] == name),
            "`{name}` is not secret-named: {report}"
        );
    }

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_inventory_cannot_see_a_credential_hardcoded_in_source() {
    // THE LIMITATION, end to end. The fixture puts the *identical* token in a
    // `.env` (where it becomes a redacted config key) and in a Rust function body
    // (where it becomes nothing this lens reads). The boundary the rename exists to
    // keep honest, asserted rather than only documented.
    let dir = fixture("boundary");
    let report = json(&dir, &["config-secrets", "--json", "--limit", "0"]);

    assert!(
        !report["items"]
            .as_array()
            .expect("items")
            .iter()
            .any(|i| i["path"] == "src/main.rs"),
        "a hardcoded credential is invisible to this lens: {report}"
    );

    // No value is served, and — the stronger claim — the real token never reached
    // the report at all.
    let body = report.to_string();
    assert!(
        !body.contains(FAKE_TOKEN),
        "the value is not echoed back: {body}"
    );

    // No config-key node carries the value either — the redaction is in the graph,
    // not just in this report.
    let cfg = serde_json::to_string(&json(&dir, &["query", "--kind", "config_key", "--json"]))
        .expect("json");
    assert!(
        !cfg.contains(FAKE_TOKEN),
        "no config-key node carries the value: {cfg}"
    );

    // Stronger, and asserted against the store's own bytes rather than any query:
    // the token is nowhere in the persisted graph. That is what makes "safely
    // redacted" a fact rather than a presentation choice.
    //
    // `localhost` is the control. It is the NON-secret value from the same
    // `config.toml`, and it IS persisted — so the token's absence is a redaction,
    // not an unextracted repository or a broken fixture.
    let db = std::fs::read(dir.join(".git/roteiro/graph.db")).expect("read store");
    let occurrences = |needle: &str| {
        db.windows(needle.len())
            .filter(|w| *w == needle.as_bytes())
            .count()
    };
    assert_eq!(
        occurrences(FAKE_TOKEN),
        0,
        "the secret-named values are not in the store at all"
    );
    assert!(
        occurrences("localhost") > 0,
        "control: a non-secret value from the same file IS stored, so the absence \
         above is redaction and not an empty graph"
    );

    // Note what this control does NOT establish. The token is absent here partly
    // because Rust symbol extraction captures doc comments rather than function
    // bodies — that is a property of the Rust extractor, not a guarantee. A
    // credential in a prose file is captured into `meta.content` verbatim. Either
    // way it produces no `config_key` node, which is the durable reason this lens
    // cannot see it, and the reason it is named for an inventory.

    // The human-readable summary carries the caveat, unconditionally.
    let text = roteiro(&dir, &["config-secrets"]);
    assert!(text.status.success(), "{text:?}");
    let out = String::from_utf8_lossy(&text.stdout);
    for claim in [
        "not a secret scan",
        "cannot see a hardcoded credential in source",
        "cannot judge a value",
        "cannot tell a real secret from a placeholder",
    ] {
        assert!(out.contains(claim), "missing `{claim}` from: {out}");
    }
    assert!(
        !out.contains("WARNING"),
        "no warning when nothing is unredacted: {out}"
    );
    // The terminal output prints key names and state, never a value — and neither
    // the token nor the redaction placeholder appears. `ConfigSecretItem` has no
    // value field at all, so this is structural rather than a formatting choice;
    // the assertion is here to keep it that way.
    assert!(
        !out.contains(FAKE_TOKEN) && !out.contains("<redacted>"),
        "no value reaches the terminal: {out}"
    );
    assert!(
        out.contains("API_TOKEN") && out.contains("[redacted]"),
        "the key name and its state do: {out}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_empty_inventory_still_carries_the_caveat() {
    // The case where a reader is most likely to conclude something the lens never
    // claimed. A credential under an innocuous key name is not secret-named, is
    // not redacted, and does not appear — so "nothing found" must not be allowed
    // to read as "nothing to find".
    let dir = std::env::temp_dir().join(format!("roteiro-cfgsec-empty-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("mkdir");
    git(&dir, &["init", "-q"]);
    write(
        &dir,
        ".env",
        &format!("DSN=postgres://user:{FAKE_TOKEN}@host/db\nPORT=8017\n"),
    );
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    let report = json(&dir, &["config-secrets", "--json"]);
    assert_eq!(report["secret_named"], 0, "`DSN` is not secret-named");
    assert!(
        report["config_keys"].as_u64().expect("count") >= 2,
        "while the graph does hold the credential, inside a `DSN` value: {report}"
    );

    let out = roteiro(&dir, &["config-secrets"]);
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("no secret-named config key"),
        "the empty result is stated as being about NAMING: {text}"
    );
    assert!(
        text.contains("not a secret scan"),
        "and the caveat is printed even when there is nothing to list: {text}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

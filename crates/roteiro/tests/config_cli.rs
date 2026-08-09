//! End-to-end tests for `roteiro config` (ADR-0007): a project `roteiro.toml`
//! surfaces in the effective config, and a malformed file is a hard error for
//! any command.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_roteiro");

fn roteiro(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        // Isolate from any real user config.
        .env("ROTEIRO_HOME", dir)
        .output()
        .expect("run roteiro")
}

#[test]
fn config_reflects_project_toml_and_rejects_malformed() {
    let dir = std::env::temp_dir().join(format!("roteiro-config-cli-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("mkdir");
    // Project config is discovered at the repo root (alongside `.git`, per
    // ADR-0007), so mark this temp dir as a repository root.
    std::fs::create_dir_all(dir.join(".git")).expect("mkdir .git");
    std::fs::write(
        dir.join("roteiro.toml"),
        "[infer]\nmin_confidence = 0.66\n[duplicates]\nlimit = 7\n[ingest]\npdf = false\n",
    )
    .expect("write config");

    // `config --json` reflects the project file's values.
    let out = roteiro(&dir, &["config", "--json"]);
    assert!(out.status.success(), "config failed: {out:?}");
    let cfg: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("config --json is valid JSON");
    assert_eq!(cfg["infer"]["min_confidence"], 0.66);
    assert_eq!(cfg["duplicates"]["limit"], 7);
    assert_eq!(cfg["ingest"]["pdf"], false);

    // Human output labels provenance.
    let human = roteiro(&dir, &["config"]);
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(text.contains("(project)"), "provenance shown: {text}");

    // A malformed config is a hard error for any command.
    std::fs::write(dir.join("roteiro.toml"), "[infer]\nmin_confidence = = =\n").expect("write");
    let bad = roteiro(&dir, &["config"]);
    assert!(!bad.status.success(), "malformed config must fail");
    assert!(
        String::from_utf8_lossy(&bad.stderr).contains("config"),
        "error mentions the config: {:?}",
        String::from_utf8_lossy(&bad.stderr)
    );

    std::fs::remove_dir_all(&dir).ok();
}

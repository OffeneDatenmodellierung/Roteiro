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
        "[infer]\nmin_confidence = 0.66\n[duplicates]\nlimit = 7\n[ingest]\npdf = false\n\
         [serve]\naddr = \"127.0.0.1:9100\"\ntools = false\n\
         [debt]\nignore = [\"vendor/**\", \"**/generated/*\"]\n\
         [paths]\nmodel_store = \"~/models\"\n",
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
    assert_eq!(cfg["serve"]["addr"], "127.0.0.1:9100");
    assert_eq!(cfg["serve"]["tools"], false);
    assert_eq!(cfg["debt"]["ignore"][0], "vendor/**");
    assert_eq!(cfg["debt"]["ignore"][1], "**/generated/*");
    assert_eq!(cfg["paths"]["model_store"], "~/models");

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

/// `roteiro config` must show **which layer each `[debt] ignore` pattern came
/// from** (issue #321c). The list merges across layers, so one `(project)` label
/// for the whole key would misreport a list holding patterns from both — and the
/// merge is only safe if it is legible.
#[test]
fn config_shows_per_pattern_debt_ignore_provenance() {
    let dir = std::env::temp_dir().join(format!("roteiro-config-debt-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::create_dir_all(dir.join(".git")).expect("mkdir .git");
    // `ROTEIRO_HOME` is `dir`, so `dir/config.toml` is the *user* layer.
    std::fs::write(
        dir.join("config.toml"),
        "[debt]\nignore = [\"vendor/**\", \"target/**\"]\n",
    )
    .expect("write user config");
    std::fs::write(
        dir.join("roteiro.toml"),
        "[debt]\nignore = [\"thirdparty/**\"]\n",
    )
    .expect("write project config");

    let out = roteiro(&dir, &["config"]);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "config failed: {out:?}");
    assert!(
        text.contains("[debt]"),
        "a [debt] section is printed: {text}"
    );
    // Every pattern survives the merge, each tagged with its own origin.
    assert!(
        text.contains("\"vendor/**\"  (user)"),
        "user pattern kept and labelled: {text}"
    );
    assert!(
        text.contains("\"target/**\"  (user)"),
        "second user pattern kept: {text}"
    );
    assert!(
        text.contains("\"thirdparty/**\"  (project)"),
        "project pattern labelled: {text}"
    );

    // `--json` carries the merged effective list.
    let json = roteiro(&dir, &["config", "--json"]);
    let cfg: serde_json::Value = serde_json::from_slice(&json.stdout).expect("valid JSON");
    assert_eq!(cfg["debt"]["ignore"][0], "vendor/**");
    assert_eq!(cfg["debt"]["ignore"][1], "target/**");
    assert_eq!(cfg["debt"]["ignore"][2], "thirdparty/**");

    // With `ignore_reset`, the inherited patterns are dropped *and named*.
    std::fs::write(
        dir.join("roteiro.toml"),
        "[debt]\nignore_reset = true\nignore = [\"thirdparty/**\"]\n",
    )
    .expect("write project config");
    let reset = roteiro(&dir, &["config"]);
    let text = String::from_utf8_lossy(&reset.stdout).into_owned();
    assert!(
        text.contains("ignore_reset = true"),
        "the reset is shown: {text}"
    );
    assert!(
        text.contains("\"vendor/**\"  (discarded from user)"),
        "what the reset dropped is named, not silently gone: {text}"
    );
    assert!(
        !text.contains("\"vendor/**\"  (user)"),
        "and it is no longer in the effective list: {text}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// `roteiro config` must not claim a reset that never happened (PR #343 review).
///
/// A user-layer `ignore_reset` governs nothing — a reset drops what a layer
/// inherits, and the user layer is the lowest — but the effective flag used to
/// inherit it, so this command printed "inherited patterns dropped" directly
/// above a list in which every inherited pattern was still present. The command
/// whose whole job is explaining the configuration was the one making the false
/// statement.
#[test]
fn config_reports_an_inert_user_layer_reset_instead_of_claiming_a_drop() {
    let dir = std::env::temp_dir().join(format!("roteiro-config-inert-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::create_dir_all(dir.join(".git")).expect("mkdir .git");
    // `ROTEIRO_HOME` is `dir`, so `dir/config.toml` is the *user* layer.
    std::fs::write(
        dir.join("config.toml"),
        "[debt]\nignore_reset = true\nignore = [\"vendor/**\", \"target/**\"]\n",
    )
    .expect("write user config");
    std::fs::write(
        dir.join("roteiro.toml"),
        "[debt]\nignore = [\"thirdparty/**\"]\n",
    )
    .expect("write project config");

    let out = roteiro(&dir, &["config"]);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "config failed: {out:?}");

    // The false headline is gone…
    assert!(
        !text.contains("(inherited patterns dropped)"),
        "nothing was dropped, so nothing may claim a drop: {text}"
    );
    // …and no pattern is reported as discarded either.
    assert!(
        !text.contains("(discarded from user)"),
        "no pattern was discarded: {text}"
    );
    // The inert request is still surfaced, since a silent no-op is the failure
    // this key exists to prevent.
    assert!(
        text.contains("NO EFFECT"),
        "an inert user-layer reset must say so: {text}"
    );
    // The merged list is untouched — this was only ever a reporting defect.
    for kept in [
        "\"vendor/**\"  (user)",
        "\"target/**\"  (user)",
        "\"thirdparty/**\"  (project)",
    ] {
        assert!(text.contains(kept), "{kept} must survive the merge: {text}");
    }

    // Moving the same reset to the project layer makes it real: it governs, the
    // drop is reported, and the inert note is not printed.
    std::fs::write(
        dir.join("roteiro.toml"),
        "[debt]\nignore_reset = true\nignore = [\"thirdparty/**\"]\n",
    )
    .expect("write project config");
    let real = roteiro(&dir, &["config"]);
    let text = String::from_utf8_lossy(&real.stdout).into_owned();
    assert!(
        text.contains("(inherited patterns dropped)"),
        "a project-layer reset really drops: {text}"
    );
    assert!(
        text.contains("\"vendor/**\"  (discarded from user)"),
        "and names what it dropped: {text}"
    );
    assert!(
        !text.contains("NO EFFECT"),
        "the inert note must not appear when the reset did take effect: {text}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

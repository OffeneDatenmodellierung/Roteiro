//! End-to-end tests for `roteiro model` and `infer --model` that need no
//! network: the registry listing, and that a non-interactive `pull` declines
//! rather than downloading (offline-by-default). The `model` subcommand only
//! needs the `models` feature (no llama.cpp engine), so the list/pull tests
//! build under it; the `infer --model` test is gated on
//! `inference-local-models` since that command only exists with the engine.
#![cfg(feature = "models")]

use std::path::Path;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_roteiro");

/// Run roteiro with an isolated model store (`ROTEIRO_HOME`) and piped (non-tty)
/// stdin, so `pull` sees a non-interactive session.
fn roteiro(home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(args)
        .env("ROTEIRO_HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(std::process::Child::wait_with_output)
        .expect("run roteiro")
}

fn fresh_home(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("roteiro-model-{}-{name}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

#[test]
fn model_list_shows_registry() {
    let home = fresh_home("list");
    let out = roteiro(&home, &["model", "list"]);
    assert!(out.status.success(), "model list failed: {out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("bge-base-en-v1.5"),
        "registry model listed: {text}"
    );
    assert!(
        text.contains("hashing embedder"),
        "notes the always-available default"
    );
    assert!(
        text.contains("available"),
        "uninstalled model marked available"
    );
    // The status marker is the ASCII-safe, fixed-width form so columns align
    // (no wide/ambiguous glyph); a fresh host has nothing installed.
    assert!(
        text.contains("[available]"),
        "uninstalled model uses the ASCII marker: {text}"
    );
    // The new High-tier coder pick is listed.
    assert!(
        text.contains("qwen3-coder-30b-a3b"),
        "new coder entry listed: {text}"
    );
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn pull_is_declined_non_interactively_and_downloads_nothing() {
    let home = fresh_home("pull");
    let out = roteiro(&home, &["model", "pull", "bge-base-en-v1.5"]);
    // Non-interactive with no `--yes`: must refuse and exit non-zero.
    assert!(!out.status.success(), "pull must decline non-interactively");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("non-interactive"), "explains why: {err}");
    assert!(err.contains("--yes"), "points at the override: {err}");
    // Nothing was written to the store.
    let model_dir = home.join("models/bge-base-en-v1.5");
    assert!(
        !model_dir.join("model.safetensors").exists(),
        "no weights should be downloaded",
    );
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn pull_rejects_unknown_model() {
    let home = fresh_home("unknown");
    let out = roteiro(&home, &["model", "pull", "no-such-model", "--yes"]);
    assert!(!out.status.success(), "unknown model must fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("unknown model"), "clear error: {err}");
    std::fs::remove_dir_all(&home).ok();
}

#[cfg(feature = "inference-local-models")]
#[test]
fn infer_with_uninstalled_model_errors() {
    // A git repo so `infer` can build a graph, but the model isn't pulled.
    let dir = fresh_home("infer");
    std::fs::write(dir.join("a.rs"), "fn a() {}\n").expect("write");
    for args in [
        &["-c", "user.name=t", "-c", "user.email=t@e", "init", "-q"][..],
        &["-c", "user.name=t", "-c", "user.email=t@e", "add", "."][..],
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@e",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "x",
        ][..],
    ] {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(&dir)
                .status()
                .expect("git")
                .success()
        );
    }
    let out = Command::new(BIN)
        .args(["infer", "--model", "bge-base-en-v1.5"])
        .current_dir(&dir)
        .env("ROTEIRO_HOME", &dir)
        .output()
        .expect("run");
    assert!(!out.status.success(), "uninstalled model must error");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not installed"), "points at model pull: {err}");
    std::fs::remove_dir_all(&dir).ok();
}

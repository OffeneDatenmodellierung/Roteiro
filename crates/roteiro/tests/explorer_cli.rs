//! `roteiro explorer` startup validation: an explicit `--workspace-name` that
//! names no configured workspace must **fail fast** — before binding a port —
//! with the `UnknownWorkspace` message listing the known workspaces. Otherwise a
//! typo would boot a server whose flat `/v1/graph/*` routes 404 on every request.
#![cfg(feature = "explorer")]

use std::path::Path;
use std::process::Command;

mod common;
use common::IsolatedHome;

const BIN: &str = env!("CARGO_BIN_EXE_roteiro");

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
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

#[test]
fn explorer_rejects_unknown_workspace_name_at_startup() {
    let base = std::env::temp_dir().join(format!("roteiro-explorer-cli-{}", std::process::id()));
    std::fs::remove_dir_all(&base).ok();
    std::fs::create_dir_all(&base).expect("mkdir");
    // A bare repo is enough: with no workspace config, `explorer` falls back to
    // hosting this repo as a lone workspace named after its directory, so
    // `doesnotexist` is unknown.
    git(&base, &["init"]);

    // Isolated config home so the "no workspace config" fallback this test
    // relies on holds regardless of the developer's real `~/.roteiro`.
    let home = IsolatedHome::new("explorer-cli");
    let mut command = Command::new(BIN);
    command
        .args(["explorer", "--workspace-name", "doesnotexist"])
        .current_dir(&base);
    home.apply(&mut command);
    let out = command.output().expect("run roteiro explorer");

    // It must fail fast (never reaching the bind), naming the unknown workspace
    // and the known one(s) — the existing `WorkspaceError::UnknownWorkspace`.
    assert!(
        !out.status.success(),
        "explorer must exit non-zero for an unknown --workspace-name"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no workspace named `doesnotexist`"),
        "error should name the unknown workspace; got: {stderr}"
    );
    assert!(
        stderr.contains("known:"),
        "error should list the known workspaces; got: {stderr}"
    );

    std::fs::remove_dir_all(&base).ok();
}

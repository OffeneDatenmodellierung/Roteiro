//! End-to-end test for `roteiro init`: it installs working git hooks that keep
//! the graph fresh, driving the real binary against a fixture repository.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The built `roteiro` binary under test.
const BIN: &str = env!("CARGO_BIN_EXE_roteiro");

fn git(dir: &Path, extra_path: Option<&str>, args: &[&str]) {
    let mut cmd = Command::new("git");
    cmd.args([
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
    .current_dir(dir);
    // Put the roteiro binary on PATH so installed hooks can invoke it.
    if let Some(p) = extra_path {
        let path = std::env::var("PATH").unwrap_or_default();
        cmd.env("PATH", format!("{p}:{path}"));
    }
    let status = cmd.status().expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

fn roteiro(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run roteiro")
}

fn write(dir: &Path, rel: &str, content: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir");
    std::fs::write(path, content).expect("write");
}

fn fresh_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("roteiro-init-cli-{}-{name}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

#[test]
fn init_installs_hooks_and_checkout_refreshes_graph() {
    let bindir = Path::new(BIN).parent().unwrap().to_str().unwrap();
    let dir = fresh_dir("hooks");
    git(&dir, None, &["init", "-q"]);
    // Pin the hooks path so an ambient global `core.hooksPath` can't send the
    // managed hooks elsewhere and break the `.git/hooks` assertions below.
    git(&dir, None, &["config", "core.hooksPath", ".git/hooks"]);
    write(
        &dir,
        "src/main.rs",
        "fn main() { helper(); }\nfn helper() {}\n",
    );
    git(&dir, None, &["add", "."]);
    git(&dir, None, &["commit", "-q", "-m", "init"]);

    // `roteiro init` builds the graph and installs the hooks + AGENTS.md.
    let out = roteiro(&dir, &["init"]);
    assert!(out.status.success(), "init failed: {out:?}");
    let hooks = dir.join(".git/hooks");
    for name in ["post-checkout", "post-merge"] {
        let hook = hooks.join(name);
        assert!(hook.exists(), "{name} hook should be installed");
        assert!(
            std::fs::read_to_string(&hook)
                .unwrap()
                .contains("roteiro-managed"),
            "{name} should be a managed hook",
        );
    }
    assert!(
        dir.join("AGENTS.md").exists(),
        "AGENTS.md should be written"
    );
    assert!(
        dir.join(".git/roteiro/graph.db").exists(),
        "graph db should exist"
    );

    // Create a branch with an extra symbol, then wipe the graph and check out
    // back to main: the post-checkout hook must rebuild the graph.
    git(&dir, None, &["checkout", "-q", "-b", "feature"]);
    write(
        &dir,
        "src/main.rs",
        "fn main() { helper(); }\nfn helper() {}\nfn added() {}\n",
    );
    // `bindir`, not `None`. `roteiro init` above installed a **pre-commit** hook,
    // so this commit runs one — and with the ambient PATH it runs whichever
    // `roteiro` the developer happens to have installed, not the binary under
    // test. On a machine with none it silently runs nothing, which is how the
    // hole stayed open; on a machine with an older one it fails on a schema this
    // build wrote, naming a binary the test never meant to involve.
    git(&dir, Some(bindir), &["commit", "-q", "-am", "add fn"]);

    std::fs::remove_dir_all(dir.join(".git/roteiro")).expect("wipe graph");
    git(&dir, Some(bindir), &["checkout", "-q", "main"]);
    assert!(
        dir.join(".git/roteiro/graph.db").exists(),
        "post-checkout hook should have rebuilt the graph",
    );

    // The rebuilt graph reflects main (has `helper`, not the feature's `added`).
    let listing = roteiro(&dir, &["query", "--kind", "fn", "--json"]);
    let json = String::from_utf8_lossy(&listing.stdout);
    assert!(
        json.contains("sym:rust:src/main.rs#helper"),
        "graph should have helper: {json}"
    );
    assert!(
        !json.contains("#added"),
        "graph should reflect main, not feature: {json}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn init_honours_core_hookspath() {
    let dir = fresh_dir("hookspath");
    git(&dir, None, &["init", "-q"]);
    // Point git at a custom hooks directory.
    git(&dir, None, &["config", "core.hooksPath", "team-hooks"]);
    write(&dir, "src/main.rs", "fn main() {}\n");
    git(&dir, None, &["add", "."]);
    git(&dir, None, &["commit", "-q", "-m", "init"]);

    let out = roteiro(&dir, &["init"]);
    assert!(out.status.success(), "init failed: {out:?}");

    // Hooks are installed where git actually looks, not in .git/hooks.
    assert!(
        dir.join("team-hooks/pre-commit").exists(),
        "managed hook should be installed under core.hooksPath",
    );
    assert!(
        !dir.join(".git/hooks/pre-commit").exists(),
        "no hook should be installed in the default .git/hooks when core.hooksPath is set",
    );

    std::fs::remove_dir_all(&dir).ok();
}

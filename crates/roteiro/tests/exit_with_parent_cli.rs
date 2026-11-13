//! A server whose parent dies exits on its own (#857).
//!
//! A test's `Drop` guard cannot run when the test process itself is killed, and
//! a server orphaned that way was found alive 19h49m later. The harness sets
//! `ROTEIRO_EXIT_WITH_PARENT` (see `common::IsolatedHome::apply`); this pins
//! that the variable actually ends an orphaned server.
#![cfg(all(unix, feature = "explorer"))]

mod common;
use common::{IsolatedHome, scratch_dir};
use std::process::Command;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_roteiro");

fn alive(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid])
        .status()
        .is_ok_and(|s| s.success())
}

/// Start `roteiro explorer` orphaned — `sh` backgrounds it, prints its pid, and
/// exits two seconds later, long after the server has recorded `sh` as its
/// parent — with or without the hook, and return the server's pid.
fn orphan(home: &IsolatedHome, cwd: &std::path::Path, hook: bool) -> String {
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(format!(
            "'{BIN}' explorer --addr 127.0.0.1:0 >/dev/null 2>&1 & echo $!; sleep 2"
        ))
        .current_dir(cwd);
    home.apply(&mut cmd);
    if !hook {
        cmd.env_remove("ROTEIRO_EXIT_WITH_PARENT");
    }
    let out = cmd.output().expect("run sh");
    let pid = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    assert!(!pid.is_empty(), "no pid: {out:?}");
    pid
}

#[test]
fn an_orphaned_server_exits_when_its_parent_is_gone() {
    let home = IsolatedHome::new("exit-with-parent");
    let cwd = scratch_dir("exit-with-parent");
    std::fs::create_dir_all(&cwd).expect("mkdir");
    let init = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&cwd)
        .status();
    assert!(init.is_ok_and(|s| s.success()), "git init");

    // Control: without the hook the orphan keeps running, so the assertion
    // below is about the hook and not about a server that never started.
    let control = orphan(&home, &cwd, false);
    std::thread::sleep(Duration::from_secs(2));
    let control_alive = alive(&control);
    let _ = Command::new("kill").args(["-9", &control]).status();
    assert!(
        control_alive,
        "the control server (pid {control}) never ran"
    );

    let pid = orphan(&home, &cwd, true);
    let deadline = Instant::now() + Duration::from_secs(20);
    while alive(&pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    let survived = alive(&pid);
    if survived {
        // Clean up rather than become the leak this test is about.
        let _ = Command::new("kill").args(["-9", &pid]).status();
    }
    assert!(
        !survived,
        "an orphaned `roteiro explorer` (pid {pid}) outlived its parent"
    );
    std::fs::remove_dir_all(&cwd).ok();
}

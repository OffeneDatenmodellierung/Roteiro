//! Regression test for the broken-pipe panic: piping a `roteiro` subcommand's
//! stdout to a reader that closes early (`roteiro query --kind … | head`) must
//! exit cleanly by SIGPIPE, not panic with a "failed printing to stdout: Broken
//! pipe" backtrace. `main` restores the default SIGPIPE disposition (`SIG_DFL`)
//! at startup so a closed stdout pipe terminates the process the Unix way; this
//! test exercises that on `query`, whose listing goes through `println!`.
//!
//! Unix-only: SIGPIPE is a Unix signal, and the fix is a `#[cfg(unix)]` no-op
//! elsewhere.
#![cfg(unix)]

use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_roteiro");

// SIGPIPE is signal 13 on both Linux and macOS. The `sigpipe` crate doesn't
// re-export the constant and `libc` isn't a direct dependency, so name it here.
const SIGPIPE: i32 = 13;

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

#[test]
fn query_exits_cleanly_when_stdout_pipe_closes_early() {
    let dir = std::env::temp_dir().join(format!("roteiro-brokenpipe-cli-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("src")).expect("mkdir");
    // A couple of files so `query --kind file` has nodes to list — i.e. `main`
    // reaches a real `println!` to stdout.
    std::fs::write(dir.join("src/lib.rs"), "pub fn a() {}\npub fn b() {}\n").expect("write");
    std::fs::write(dir.join("README.md"), "# Readme\n").expect("write");
    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    // Pipe stdout, then immediately close our read end: with no reader left on the
    // pipe, the child's first stdout write returns EPIPE. Before the fix that
    // panicked (exit 101, "failed printing to stdout: Broken pipe"); with SIGPIPE
    // reset to SIG_DFL the kernel terminates the process by signal instead.
    let mut child = Command::new(BIN)
        .args(["query", "--kind", "file"])
        .current_dir(&dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn roteiro");
    // Drop the read end (close the pipe) before the child writes its listing.
    drop(child.stdout.take());

    let output = child.wait_with_output().expect("wait roteiro");
    let stderr = String::from_utf8_lossy(&output.stderr);

    // The regression: no Rust panic / broken-pipe backtrace on a closed pipe.
    assert!(
        !stderr.contains("panicked") && !stderr.contains("Broken pipe"),
        "broken-pipe panic regressed: exit={:?} signal={:?}\nstderr:\n{stderr}",
        output.status.code(),
        output.status.signal(),
    );
    assert_ne!(
        output.status.code(),
        Some(101),
        "process panicked (exit 101) instead of exiting on SIGPIPE\nstderr:\n{stderr}",
    );
    // Positively assert the intended mechanism: terminated by SIGPIPE. (If the
    // child managed to buffer its whole short listing and exit 0 before the close
    // was observed, that's also panic-free and acceptable — hence the fallback.)
    assert!(
        output.status.signal() == Some(SIGPIPE) || output.status.success(),
        "expected SIGPIPE termination or clean exit, got exit={:?} signal={:?}\nstderr:\n{stderr}",
        output.status.code(),
        output.status.signal(),
    );
}

//! `roteiro serve --allow-upload` driven through the real binary (issue #814).
//!
//! The unit tests in `upload.rs` pin each limit on the router. These pin the
//! **wiring**: that the route exists only when the flag is given, and that the
//! flag refuses a non-loopback bind and a missing `raw/` before anything starts.
//! A router that is correct but mounted unconditionally passes every unit test.

#![cfg(feature = "upload")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

mod common;
use common::{IsolatedHome, scratch_dir};

const BIN: &str = env!("CARGO_BIN_EXE_roteiro");

/// Killed on drop, including on a panic, so a failure leaves no orphan.
struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn make_repo(dir: &Path) {
    std::fs::create_dir_all(dir).expect("mkdir repo");
    let git = |args: &[&str]| {
        let ok = Command::new("git")
            .args(["-c", "user.name=T", "-c", "user.email=t@example.com"])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .current_dir(dir)
            .status()
            .expect("git")
            .success();
        assert!(ok, "git {args:?}");
    };
    git(&["init", "-q", "."]);
    std::fs::write(dir.join("README.md"), "# fixture\n").expect("readme");
    git(&["add", "-A"]);
    git(&["commit", "-qm", "init"]);
}

fn command(args: &[&str], cwd: &Path, home: &IsolatedHome) -> Command {
    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    home.apply(&mut cmd);
    cmd
}

/// Start `serve` on an ephemeral loopback port and return it with its address.
fn start(extra: &[&str], cwd: &Path, home: &IsolatedHome) -> (Server, String, String) {
    let mut args = vec!["serve", "--addr", "127.0.0.1:0"];
    args.extend_from_slice(extra);
    let mut child = command(&args, cwd, home).spawn().expect("spawn");
    let stderr = child.stderr.take().expect("stderr");
    let server = Server(child);
    let mut seen = String::new();
    for line in BufReader::new(stderr).lines().map_while(Result::ok) {
        seen.push_str(&line);
        seen.push('\n');
        if let Some((_, rest)) = line.split_once(" listening on http://") {
            let addr = rest.split(['/', ' ']).next().expect("addr").to_owned();
            return (server, addr, seen);
        }
    }
    panic!("serve never listened:\n{seen}");
}

fn put(addr: &str, path: &str, ctype: &str, body: &[u8]) -> u16 {
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .expect("timeout");
    write!(
        stream,
        "PUT {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: {ctype}\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .expect("head");
    stream.write_all(body).expect("body");
    let mut raw = String::new();
    stream.read_to_string(&mut raw).expect("response");
    raw.split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or_else(|| panic!("no status in: {raw}"))
}

#[test]
fn without_the_flag_there_is_no_upload_route() {
    let repo = scratch_dir("upload-off");
    make_repo(&repo);
    std::fs::create_dir(repo.join("raw")).expect("raw");
    let home = IsolatedHome::new("upload-off");
    let (_server, addr, _) = start(&[], &repo, &home);
    let status = put(&addr, "/v1/raw/paper.md", "text/markdown", b"# p\n");
    assert!(
        status == 404 || status == 405,
        "the read surface accepted an upload ({status})"
    );
    assert!(!repo.join("raw/paper.md").exists());
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn with_the_flag_a_document_lands_in_raw() {
    let repo = scratch_dir("upload-on");
    make_repo(&repo);
    std::fs::create_dir(repo.join("raw")).expect("raw");
    let home = IsolatedHome::new("upload-on");
    let (_server, addr, stderr) = start(&["--allow-upload"], &repo, &home);
    assert!(stderr.contains("upload surface ON"), "{stderr}");
    assert_eq!(
        put(&addr, "/v1/raw/paper.md", "text/markdown", b"# p\n"),
        201
    );
    assert_eq!(
        std::fs::read(repo.join("raw/paper.md")).expect("stored"),
        b"# p\n"
    );
    let _ = std::fs::remove_dir_all(&repo);
}

/// Run a `serve` that must refuse, returning its stderr. Not `output()`: a
/// refusal that regressed into a running server would hang that forever instead
/// of failing.
fn refusal(args: &[&str], cwd: &Path, home: &IsolatedHome) -> String {
    let log = cwd.join("..").join(format!(
        "upload-refusal-{}.log",
        cwd.file_name().and_then(|n| n.to_str()).unwrap_or("x")
    ));
    let mut cmd = command(args, cwd, home);
    cmd.stderr(std::fs::File::create(&log).expect("log"));
    let mut server = Server(cmd.spawn().expect("spawn"));
    for _ in 0..600 {
        if let Some(status) = server.0.try_wait().expect("wait") {
            let stderr = std::fs::read_to_string(&log).expect("read log");
            let _ = std::fs::remove_file(&log);
            assert!(!status.success(), "exited 0: {stderr}");
            return stderr;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("`roteiro {args:?}` did not refuse; it is still running");
}

#[test]
fn the_flag_refuses_a_non_loopback_bind_and_a_missing_raw() {
    let repo = scratch_dir("upload-refuse");
    make_repo(&repo);
    let home = IsolatedHome::new("upload-refuse");
    // No `raw/` yet: refused, and not created on the user's behalf.
    let stderr = refusal(
        &["serve", "--addr", "127.0.0.1:0", "--allow-upload"],
        &repo,
        &home,
    );
    assert!(stderr.contains("raw/"), "{stderr}");
    assert!(!repo.join("raw").exists());

    std::fs::create_dir(repo.join("raw")).expect("raw");
    let stderr = refusal(
        &["serve", "--addr", "0.0.0.0:0", "--allow-upload"],
        &repo,
        &home,
    );
    assert!(stderr.contains("non-loopback"), "{stderr}");
    let _ = std::fs::remove_dir_all(&repo);
}

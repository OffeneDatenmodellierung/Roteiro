//! End-to-end test for `roteiro mcp`: drive the real binary over an MCP stdio
//! session and confirm it answers a `tools/call` against a fixture graph. (The
//! MCP graph server moved from bare `roteiro serve` to `roteiro mcp`; `serve` is
//! now the network HTTP server.) Only built when the `mcp` feature is enabled.
#![cfg(feature = "mcp")]

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

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
    assert!(status.success(), "git {args:?} failed");
}

#[test]
fn mcp_answers_initialize_and_tools_call() {
    let dir = std::env::temp_dir().join(format!("roteiro-mcp-cli-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("src")).expect("mkdir");
    std::fs::write(
        dir.join("src/main.rs"),
        "fn main() { helper(); }\nfn helper() {}\n",
    )
    .expect("write");
    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    let mut child = Command::new(BIN)
        .arg("mcp")
        .current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn mcp");

    let session = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":\
         {\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\
         \"clientInfo\":{\"name\":\"test\",\"version\":\"0\"}}}\n",
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":\
         {\"name\":\"explain\",\"arguments\":{\"key\":\"sym:rust:src/main.rs#main\"}}}\n",
    );
    child
        .stdin
        .take()
        .unwrap()
        .write_all(session.as_bytes())
        .expect("write session");

    let output = child.wait_with_output().expect("wait");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let responses: Vec<serde_json::Value> = stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("valid json-rpc line"))
        .collect();

    // initialize (id 1) then tools/call (id 2); the notification gets no reply.
    assert_eq!(responses.len(), 2, "expected two responses: {stdout}");
    assert_eq!(responses[0]["id"], 1);
    assert_eq!(responses[0]["result"]["serverInfo"]["name"], "roteiro");

    assert_eq!(responses[1]["id"], 2);
    let text = responses[1]["result"]["content"][0]["text"]
        .as_str()
        .expect("text content");
    let inner: serde_json::Value = serde_json::from_str(text).expect("inner json");
    assert_eq!(inner["node"]["key"], "sym:rust:src/main.rs#main");
    assert!(
        inner["outgoing"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["node"] == "sym:rust:src/main.rs#helper"),
        "explain should report the derived call to helper",
    );

    std::fs::remove_dir_all(&dir).ok();
}

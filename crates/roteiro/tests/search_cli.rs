//! End-to-end test for `roteiro search`: text search returns ranked hits over
//! the graph, and a curated authored doc (an ADR) ranks at or above a same-named
//! code symbol — the "find, then explain" entry point exposed on the CLI.

use std::path::Path;
use std::process::Command;

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

fn roteiro(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run roteiro")
}

#[test]
fn search_ranks_hits_and_finds_by_content() {
    let dir = std::env::temp_dir().join(format!("roteiro-search-cli-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("docs/adr")).expect("mkdir");
    std::fs::create_dir_all(dir.join("src")).expect("mkdir");
    // An authored ADR whose body is about "widgets"…
    std::fs::write(
        dir.join("docs/adr/0001-widgets.md"),
        "---\ntype: adr\nadr-id: \"0001\"\n---\n\n# ADR-0001: Widgets\n\n\
         ## Context\n\nHow the widget subsystem is structured.\n",
    )
    .expect("write");
    // …and a code symbol that merely mentions the word in its name.
    std::fs::write(
        dir.join("src/lib.rs"),
        "/// Build a widget.\npub fn widget() -> u32 { 1 }\n",
    )
    .expect("write");
    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    // JSON: a flat, ranked array; the authored ADR ranks at or above the symbol.
    let out = roteiro(&dir, &["search", "widget", "--json"]);
    assert!(out.status.success(), "search failed: {out:?}");
    let hits: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    let hits = hits.as_array().expect("array");
    assert!(!hits.is_empty(), "expected hits for 'widget'");
    let adr_rank = hits
        .iter()
        .position(|h| h["key"].as_str().is_some_and(|k| k.starts_with("adr:")));
    let sym_rank = hits
        .iter()
        .position(|h| h["key"].as_str().is_some_and(|k| k.contains("#widget")));
    let adr_rank = adr_rank.expect("the ADR should be a hit");
    if let Some(sym_rank) = sym_rank {
        assert!(
            adr_rank <= sym_rank,
            "authored ADR should rank at or above the same-topic symbol: {hits:?}"
        );
    }

    // `--limit` bounds the result count.
    let out = roteiro(&dir, &["search", "widget", "--limit", "1", "--json"]);
    let hits: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    assert_eq!(
        hits.as_array().expect("array").len(),
        1,
        "limit is honoured"
    );

    // A miss exits zero with empty stdout (composes in scripts).
    let out = roteiro(&dir, &["search", "zzzznotpresent"]);
    assert!(out.status.success(), "a miss still exits zero");
    assert!(out.stdout.is_empty(), "stdout stays empty on a miss");

    std::fs::remove_dir_all(&dir).ok();
}

//! End-to-end test for `roteiro context`: a node's context is cached, and when a
//! *dependency* changes, the dependent's cached context is invalidated (rebuilt)
//! — the codegraph-style dirty-propagation this stage delivers.

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

fn json(out: &std::process::Output) -> serde_json::Value {
    assert!(out.status.success(), "command failed: {out:?}");
    serde_json::from_slice(&out.stdout).expect("valid JSON")
}

/// The **maintenance seam reclaims the object cache** (issue #387).
///
/// This is the "when does it run" decision, tested where it is made: the same
/// `--refresh` that sweeps the bounded memory tier, and nowhere else. Not on
/// store open, and not on `sync` — `sync` is reached from `refresh_for_read` on
/// every ordinary query, so sweeping there would put deletion back on the read
/// path the seam exists to keep it off. A plain `roteiro context <key>` therefore
/// leaves the superseded entry alone, and that is asserted first.
#[test]
fn the_maintenance_seam_reclaims_superseded_cache_objects() {
    let dir = std::env::temp_dir().join(format!("roteiro-ctx-gc-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("src")).expect("mkdir");
    std::fs::write(dir.join("src/lib.rs"), "pub fn only() -> u32 { 1 }\n").expect("write");
    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    // A refresh syncs (populating the object cache) *and* runs the seam, so the
    // cache is in its steady post-maintenance state before anything is planted.
    let first = roteiro(&dir, &["context", "--refresh"]);
    assert!(first.status.success(), "initial refresh: {first:?}");
    let objects = dir.join(".git/roteiro/objects");
    let live = cache_files(&objects);
    assert!(!live.is_empty(), "the sync should have cached something");

    // What a previous binary left behind: an entry differing from a live one only
    // in its `-v<version>-` field, at a generation long superseded.
    let (shard, name) = live.first().expect("a live entry").clone();
    let (head, tail) = name.rsplit_once("-v").expect("a key carries a version tag");
    let env = tail.rsplit_once("-e").expect("…and an env tag").1;
    let superseded = objects.join(&shard).join(format!("{head}-v1-e{env}.json"));
    std::fs::write(&superseded, br#"{"nodes":[],"edges":[]}"#).expect("plant");

    // A read is not a sweep: the seam is the only thing that deletes.
    let read = roteiro(&dir, &["context", "file:src/lib.rs"]);
    assert!(read.status.success(), "an ordinary read: {read:?}");
    assert!(
        superseded.exists(),
        "an ordinary read must not reclaim anything",
    );

    let out = roteiro(&dir, &["context", "--refresh"]);
    assert!(out.status.success(), "refresh failed: {out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("object cache swept: 1 superseded object(s) freed"),
        "the seam must say what it reclaimed: {text}",
    );
    assert!(!superseded.exists(), "the superseded entry must be gone");
    assert_eq!(
        cache_files(&objects),
        live,
        "and every live entry must survive: {text}",
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// Every `<shard>/<stem>.json` under an object-cache root, sorted, as
/// `(shard, stem)` pairs. Failures panic rather than being skipped: an empty
/// listing compares equal to another empty listing, so a swallowed read error
/// would let "every live entry survived" pass having checked nothing.
fn cache_files(root: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for shard in std::fs::read_dir(root).expect("read object cache root") {
        let shard = shard.expect("shard entry");
        if !shard.file_type().expect("shard file type").is_dir() {
            continue;
        }
        let prefix = shard.file_name().to_string_lossy().into_owned();
        for entry in std::fs::read_dir(shard.path()).expect("read shard") {
            let name = entry.expect("cache entry").file_name();
            let name = name.to_string_lossy();
            if let Some(stem) = name.strip_suffix(".json") {
                out.push((prefix.clone(), stem.to_owned()));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn dependency_change_invalidates_dependent_context() {
    let dir = std::env::temp_dir().join(format!("roteiro-ctx-cli-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("src")).expect("mkdir");
    // `caller` (in caller.rs) calls `callee` (in callee.rs): a cross-file
    // dependency, so editing callee.rs leaves caller.rs's blob unchanged.
    std::fs::write(dir.join("src/callee.rs"), "pub fn callee() -> u32 { 1 }\n").expect("write");
    std::fs::write(
        dir.join("src/caller.rs"),
        "use crate::callee::callee;\npub fn caller() -> u32 { callee() }\n",
    )
    .expect("write");
    std::fs::write(dir.join("src/lib.rs"), "pub mod callee;\npub mod caller;\n").expect("write");
    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    let caller_key = "sym:rust:src/caller.rs#caller";

    // Fetch the caller's context (populates the cache) and record its fingerprint.
    let first = json(&roteiro(&dir, &["context", caller_key, "--json"]));
    let fp_before = first["fingerprint"]
        .as_str()
        .expect("fingerprint")
        .to_owned();
    // It depends on callee via an outgoing call edge.
    let calls_callee = first["outgoing"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|e| e["node"].as_str().is_some_and(|n| n.contains("#callee")));
    assert!(calls_callee, "caller should call callee: {first}");

    // A refresh now finds everything fresh (nothing changed).
    let clean = json(&roteiro(&dir, &["context", "--refresh", "--json"]));
    assert_eq!(clean["rebuilt"], 0, "nothing changed yet: {clean}");
    assert!(clean["reused"].as_u64().expect("reused") >= 1);

    // Change the *callee*'s body (new blob) and commit; caller.rs is untouched.
    std::fs::write(dir.join("src/callee.rs"), "pub fn callee() -> u32 { 42 }\n").expect("write");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "change callee"]);

    // The caller's cached context is now stale (its dependency changed): a refresh
    // rebuilds it.
    let after = json(&roteiro(&dir, &["context", "--refresh", "--json"]));
    assert!(
        after["rebuilt"].as_u64().expect("rebuilt") >= 1,
        "the dependent's context must be rebuilt: {after}",
    );

    // And the caller's fingerprint has moved.
    let refetched = json(&roteiro(&dir, &["context", caller_key, "--json"]));
    assert_ne!(
        fp_before,
        refetched["fingerprint"].as_str().expect("fingerprint"),
        "dependent fingerprint must change when its dependency changes",
    );

    std::fs::remove_dir_all(&dir).ok();
}

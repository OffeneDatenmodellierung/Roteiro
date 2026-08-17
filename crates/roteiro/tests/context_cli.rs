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

/// The sweep summary must describe **every class it retained**, not just the
/// obvious one — asserted on the rendered text, because the rendered text is the
/// defect surface. The struct fields could all be right while the sentence built
/// from them tells the reader something untrue about an irreversible operation;
/// only reading what the user reads catches that.
///
/// The cache is stocked with all four classes at once:
///
/// - **current** — the live entry the sync just wrote;
/// - **superseded** — an old generation, which goes;
/// - **ahead** — a generation *newer* than this build, as a colleague or another
///   worktree on a newer binary would leave in this shared cache. Retained, and
///   the old summary's "current and previous generation" wording denied it existed;
/// - **unrecognised** — a key `key_generation` cannot parse. Retained by the
///   deliberate "every doubt retains" rule, and previously invisible: it sat
///   inside the retained total with nothing saying it was there, which hides
///   both of the two things it can mean.
#[test]
fn the_sweep_summary_names_every_class_it_retained() {
    let dir = std::env::temp_dir().join(format!("roteiro-ctx-classes-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("src")).expect("mkdir");
    // Exactly one tracked file, so the live count in the rendered line is 1 and
    // the assertion below can be on the whole sentence rather than a fragment.
    std::fs::write(dir.join("src/lib.rs"), "pub fn only() -> u32 { 1 }\n").expect("write");
    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    assert!(
        roteiro(&dir, &["context", "--refresh"]).status.success(),
        "initial refresh",
    );
    let objects = dir.join(".git/roteiro/objects");
    let live = cache_files(&objects);
    assert_eq!(live.len(), 1, "one tracked file, one entry: {live:?}");

    let (shard, name) = live.first().expect("a live entry").clone();
    let (head, tail) = name.rsplit_once("-v").expect("a key carries a version tag");
    let (version, env) = tail.rsplit_once("-e").expect("…and an env tag");
    let version: u32 = version.parse().expect("a numeric version");
    let plant = |file: String, label: &str| {
        std::fs::write(
            objects.join(&shard).join(file),
            br#"{"nodes":[],"edges":[]}"#,
        )
        .unwrap_or_else(|e| panic!("plant the {label} entry: {e}"));
    };
    // Superseded: generation 1 is behind every released generation of this crate.
    plant(format!("{head}-v1-e{env}.json"), "superseded");
    // Ahead: one generation past this build's, in this build's namespace.
    plant(format!("{head}-v{}-e{env}.json", version + 1), "newer");
    // Unrecognised: a well-formed entry filename carrying no version tag at all.
    plant("no-version-tag-here.json".to_owned(), "unrecognised");

    let out = roteiro(&dir, &["context", "--refresh"]);
    assert!(out.status.success(), "refresh failed: {out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    let summary_line = |prefix: &str| {
        text.lines()
            .find(|l| l.starts_with(prefix))
            .unwrap_or_else(|| panic!("no line starting `{prefix}` in:\n{text}"))
            .to_owned()
    };

    // The totals line: one freed, three retained. Bytes vary with the fixture, so
    // the two counts are asserted and the byte fields only for their presence.
    let totals = summary_line("object cache swept:");
    assert!(
        totals.starts_with("object cache swept: 1 superseded object(s) freed (")
            && totals.contains("), 3 retained ("),
        "totals must name both halves: {totals}",
    );

    // The breakdown, in full. This is the assertion the old summary failed: it
    // claimed everything retained was "across the current and previous extractor
    // generation" while holding one entry from a newer build and one key it could
    // not read.
    assert_eq!(
        summary_line("  retained:"),
        "  retained: 1 at this build's generation, 0 at an older generation kept as insurance, \
         1 written by a newer build, 1 whose key this build does not recognise",
    );

    // And an unrecognised key says what it means, rather than sitting in a total.
    assert!(
        summary_line("  an unrecognised key").contains("a bug in the key parser"),
        "an unrecognised key must point at what to investigate:\n{text}",
    );

    // Everything claimed as retained is still on disk; only the superseded went.
    assert_eq!(cache_files(&objects).len(), 3, "{text}");
    assert!(
        !objects
            .join(&shard)
            .join(format!("{head}-v1-e{env}.json"))
            .exists(),
        "the superseded entry must be gone",
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

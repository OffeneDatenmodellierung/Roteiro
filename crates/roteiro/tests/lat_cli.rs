//! End-to-end test for `roteiro import --from lat`: a lat.md directory becomes
//! an authored layer over the derived code graph, links into real symbols are
//! kept, links into missing symbols are pruned, and the layer is durable across
//! a code-changing sync.

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
fn import_lat_authors_links_prunes_stale_and_is_durable() {
    let dir = std::env::temp_dir().join(format!("roteiro-lat-cli-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("src")).expect("mkdir src");
    std::fs::create_dir_all(dir.join("lat.md")).expect("mkdir lat.md");

    // A real symbol the lat graph will link to (validates), plus a link to a
    // symbol that does not exist (must be pruned).
    std::fs::write(dir.join("src/lib.rs"), "pub struct Widget;\n").expect("write src");
    std::fs::write(
        dir.join("lat.md/design.md"),
        "# Design\n\nThe core type is [[src/lib.rs#Widget]].\n\n\
         ## Details\n\nGone: [[src/lib.rs#Ghost]] no longer exists.\n",
    )
    .expect("write lat");

    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    // Import: the Ghost link is pruned; Widget and structure are applied.
    let out = roteiro(&dir, &["import", "--from", "lat", "lat.md", "--json"]);
    assert!(out.status.success(), "import failed: {out:?}");
    let report: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("import --json is valid JSON");
    assert_eq!(report["files"], 1);
    assert_eq!(report["sections"], 2, "# Design and ## Details");
    assert_eq!(report["links_to_code"], 2);
    assert_eq!(report["edges_pruned_stale"], 1, "the Ghost link is pruned");

    // The authored section links to the real symbol, and never to the ghost.
    let q = roteiro(&dir, &["query", "lat:lat.md/design.md#design", "--json"]);
    assert!(q.status.success(), "query failed: {q:?}");
    let node: serde_json::Value = serde_json::from_slice(&q.stdout).expect("query json");
    let outgoing = node["outgoing"].as_array().expect("outgoing");
    assert!(
        outgoing
            .iter()
            .any(|e| e["node"] == "sym:rust:src/lib.rs#Widget" && e["provenance"] == "authored"),
        "section should author-link to the real symbol: {node}",
    );
    assert!(
        !outgoing
            .iter()
            .any(|e| e["node"] == "sym:rust:src/lib.rs#Ghost"),
        "the stale link must not be present: {node}",
    );

    // Durable: a code-changing commit + sync must not drop the imported layer.
    std::fs::write(
        dir.join("src/lib.rs"),
        "pub struct Widget;\npub fn extra() {}\n",
    )
    .expect("edit src");
    git(&dir, &["commit", "-qam", "change"]);
    let sync = roteiro(&dir, &["sync", "--committed"]);
    assert!(sync.status.success(), "sync failed: {sync:?}");

    let q2 = roteiro(&dir, &["query", "--kind", "lat_section", "--json"]);
    assert!(q2.status.success(), "query2 failed: {q2:?}");
    let listing: serde_json::Value = serde_json::from_slice(&q2.stdout).expect("json");
    let nodes = listing["nodes"].as_array().expect("nodes");
    assert_eq!(nodes.len(), 2, "lat sections survive a code-changing sync");

    // Authoritative re-import: dropping the Widget link and re-importing must
    // remove the old edge (proves lat edges are stamped with the import src_ref,
    // which `apply_import_layer` deletes by).
    std::fs::write(
        dir.join("lat.md/design.md"),
        "# Design\n\nNo links here now.\n\n## Details\n\nStill nothing.\n",
    )
    .expect("rewrite lat");
    let reimport = roteiro(&dir, &["import", "--from", "lat", "lat.md", "--json"]);
    assert!(reimport.status.success(), "reimport failed: {reimport:?}");
    let after = roteiro(&dir, &["query", "lat:lat.md/design.md#design", "--json"]);
    let reimported: serde_json::Value = serde_json::from_slice(&after.stdout).expect("json");
    assert!(
        !reimported["outgoing"]
            .as_array()
            .expect("outgoing")
            .iter()
            .any(|e| e["node"] == "sym:rust:src/lib.rs#Widget"),
        "re-import must clear the old Widget reference edge: {reimported}",
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn import_lat_backlinks_from_source_comments() {
    let dir = std::env::temp_dir().join(format!("roteiro-lat-backlink-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("src")).expect("mkdir src");
    std::fs::create_dir_all(dir.join("lat.md")).expect("mkdir lat.md");

    // A source file carrying a `@lat:` backlink to a lat section, and the lat
    // file that defines it.
    std::fs::write(
        dir.join("src/auth.rs"),
        "// @lat: [[auth#OAuth Flow]]\npub fn validate() {}\n",
    )
    .expect("write src");
    std::fs::write(
        dir.join("lat.md/auth.md"),
        "# Auth\n\n## OAuth Flow\n\nThe flow.\n",
    )
    .expect("write lat");

    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    let out = roteiro(&dir, &["import", "--from", "lat", "lat.md", "--json"]);
    assert!(out.status.success(), "import failed: {out:?}");
    let report: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("import --json is valid JSON");
    assert_eq!(
        report["backlinks_resolved"], 1,
        "the @lat backlink resolved"
    );
    assert_eq!(report["backlinks_unresolved"], 0);

    // The source file authors a `references` edge to the lat section.
    let q = roteiro(&dir, &["query", "file:src/auth.rs", "--json"]);
    assert!(q.status.success(), "query failed: {q:?}");
    let node: serde_json::Value = serde_json::from_slice(&q.stdout).expect("query json");
    assert!(
        node["outgoing"]
            .as_array()
            .expect("outgoing")
            .iter()
            .any(|e| e["node"] == "lat:lat.md/auth.md#oauth-flow" && e["provenance"] == "authored"),
        "file should author-link to the lat section: {node}",
    );

    std::fs::remove_dir_all(&dir).ok();
}

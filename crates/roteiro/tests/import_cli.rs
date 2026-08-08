//! End-to-end test for `roteiro import --from graphify`: imports a fixture
//! Graphify export into a repo's store, dropping code and keeping doc/inferred
//! knowledge, and grounding an imported doc to a real file node.

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
fn import_graphify_keeps_docs_drops_code_and_links_files() {
    let dir = std::env::temp_dir().join(format!("roteiro-import-cli-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("docs")).expect("mkdir");
    // A real source file so the derived graph has a `file:docs/design.md` node
    // for the imported doc to ground onto.
    std::fs::write(dir.join("docs/design.md"), "# Design\n").expect("write");
    std::fs::write(dir.join("src.rs"), "fn a() {}\n").expect("write");
    // A Graphify export: one code node/edge (dropped), one doc node + one
    // semantic edge (kept), whose source_file matches the real file above.
    std::fs::write(
        dir.join("graph.json"),
        r#"{
          "directed": false, "multigraph": false,
          "nodes": [
            {"id": "design", "label": "Design note", "file_type": "document", "source_file": "docs/design.md", "_origin": "semantic"},
            {"id": "concept1", "label": "Layering", "file_type": "concept", "_origin": "semantic"},
            {"id": "codeA", "label": "fn a", "file_type": "code", "source_file": "src.rs", "_origin": "ast"}
          ],
          "links": [
            {"source": "design", "target": "concept1", "relation": "conceptually_related_to", "confidence": "INFERRED", "confidence_score": 0.77, "_origin": "semantic"},
            {"source": "codeA", "target": "design", "relation": "references", "confidence": "EXTRACTED", "_origin": "ast"}
          ],
          "hyperedges": []
        }"#,
    )
    .expect("write");
    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    let out = roteiro(&dir, &["import", "--from", "graphify", ".", "--json"]);
    assert!(out.status.success(), "import failed: {out:?}");
    let report: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("import --json is valid JSON");
    assert_eq!(report["nodes_imported"], 2, "doc + concept imported");
    assert_eq!(report["nodes_dropped_code"], 1, "code node dropped");
    assert_eq!(report["edges_imported"], 1, "semantic edge imported");
    assert_eq!(report["edges_dropped_ast"], 1, "ast edge dropped");
    assert_eq!(
        report["docs_linked_to_files"], 1,
        "design.md doc grounded to its file"
    );

    // The imported doc node is queryable and linked to the real file node.
    let q = roteiro(&dir, &["query", "graphify:design", "--json"]);
    assert!(q.status.success(), "query failed: {q:?}");
    let node: serde_json::Value = serde_json::from_slice(&q.stdout).expect("query json");
    let outgoing = node["outgoing"].as_array().expect("outgoing");
    assert!(
        outgoing
            .iter()
            .any(|e| e["node"] == "file:docs/design.md" && e["provenance"] == "inferred"),
        "imported doc should link to the real file node: {node}",
    );

    std::fs::remove_dir_all(&dir).ok();
}

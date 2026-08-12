//! End-to-end test for `roteiro links` (ADR-0009): a spoke repo declares authored
//! cross-repo `[[links]]` into a hub repo's graph; the command resolves each
//! against the workspace, reports the ones that resolve, and flags drift (targets
//! that no longer exist), exiting non-zero so it works as a CI gate.

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
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

fn roteiro(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run roteiro")
}

#[test]
fn links_resolve_across_repos_and_flag_drift() {
    let base = std::env::temp_dir().join(format!("roteiro-links-cli-{}", std::process::id()));
    std::fs::remove_dir_all(&base).ok();
    let app = base.join("app");
    let deploy = base.join("deploy");
    std::fs::create_dir_all(&app).expect("mkdir app");
    std::fs::create_dir_all(&deploy).expect("mkdir deploy");

    // Hub: a real graph with a `file:README.md` node.
    std::fs::write(app.join("README.md"), "# App\n").expect("write");
    git(&app, &["init", "-q"]);
    git(&app, &["add", "."]);
    git(&app, &["commit", "-q", "-m", "init"]);
    assert!(roteiro(&app, &["sync"]).status.success(), "app sync failed");

    // Spoke: authored links — one that resolves, one drift (removed key), one to a
    // project that isn't in the workspace.
    std::fs::write(
        deploy.join("roteiro.toml"),
        "[[links]]\n\
         to = \"app::file:README.md\"\n\
         from = \"file:values.yaml\"\n\
         kind = \"configures\"\n\
         \n\
         [[links]]\n\
         to = \"app::file:gone.md\"\n\
         \n\
         [[links]]\n\
         to = \"ghost::file:x\"\n",
    )
    .expect("write toml");
    git(&deploy, &["init", "-q"]);
    git(&deploy, &["add", "."]);
    git(&deploy, &["commit", "-q", "-m", "init"]);

    let base_s = base.to_str().unwrap();

    // Human output: the resolving link is `ok`, the two bad ones `DRIFT`; the
    // command exits non-zero because there is drift.
    let out = roteiro(&base, &["links", "--workspace", base_s]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success(), "drift must fail the command: {text}");
    assert!(
        text.contains("app::file:README.md") && text.contains("ok"),
        "{text}"
    );
    assert!(
        text.contains("app::file:gone.md") && text.contains("DRIFT"),
        "{text}"
    );

    // JSON: three links, exactly one resolved.
    let out = roteiro(&base, &["links", "--workspace", base_s, "--json"]);
    let arr: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    let arr = arr.as_array().expect("array");
    assert_eq!(arr.len(), 3, "three declared links: {arr:?}");
    let ok = arr.iter().filter(|r| r["status"] == "ok").count();
    let drift = arr.iter().filter(|r| r["status"] == "drift").count();
    assert_eq!((ok, drift), (1, 2), "one resolves, two drift: {arr:?}");
    // The resolved one names its target node.
    let resolved = arr.iter().find(|r| r["status"] == "ok").unwrap();
    assert_eq!(resolved["to"], "app::file:README.md");
    assert!(resolved["detail"].as_str().unwrap().contains("README.md"));

    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn infer_matches_config_keys_across_repos_and_flags_orphans() {
    let base = std::env::temp_dir().join(format!("roteiro-infer-cli-{}", std::process::id()));
    std::fs::remove_dir_all(&base).ok();
    let app = base.join("app");
    let deploy = base.join("deploy");
    std::fs::create_dir_all(&app).expect("mkdir app");
    std::fs::create_dir_all(&deploy).expect("mkdir deploy");

    // Hub: a TOML config with a few keys.
    std::fs::write(
        app.join("config.toml"),
        "[serve]\naddr = \"127.0.0.1:8017\"\ntools = true\n[models]\ngenerative = \"qwen3-0.6b\"\n",
    )
    .expect("write");
    git(&app, &["init", "-q"]);
    git(&app, &["add", "."]);
    git(&app, &["commit", "-q", "-m", "init"]);
    // `--infer` reads config keys from the graph, so both repos must be synced.
    assert!(roteiro(&app, &["sync"]).status.success(), "app sync failed");

    // Spoke: an .env that overrides two keys (different naming convention) and
    // sets one the app doesn't define (the orphan / drift candidate).
    std::fs::write(
        deploy.join("prod.env"),
        "SERVE_ADDR=0.0.0.0:8443\nSERVE_TOOLS=false\nMAX_CONNECTIONS=512\n",
    )
    .expect("write");
    git(&deploy, &["init", "-q"]);
    git(&deploy, &["add", "."]);
    git(&deploy, &["commit", "-q", "-m", "init"]);
    assert!(
        roteiro(&deploy, &["sync"]).status.success(),
        "deploy sync failed"
    );

    let base_s = base.to_str().unwrap();
    let out = roteiro(
        &base,
        &[
            "links",
            "--infer",
            "--hub",
            "app",
            "--workspace",
            base_s,
            "--json",
        ],
    );
    assert!(
        out.status.success(),
        "infer is informational (exit 0): {out:?}"
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    assert_eq!(v["hub"], "app");
    let spoke = &v["spokes"][0];
    assert_eq!(spoke["repo"], "deploy");
    // SERVE_ADDR / SERVE_TOOLS match app's serve.addr / serve.tools by name.
    let matched: Vec<&str> = spoke["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["hub_key"].as_str().unwrap())
        .collect();
    assert!(matched.contains(&"serve.addr"), "{matched:?}");
    assert!(matched.contains(&"serve.tools"), "{matched:?}");
    // MAX_CONNECTIONS has no app counterpart → orphan.
    let orphans: Vec<&str> = spoke["orphans"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["key"].as_str().unwrap())
        .collect();
    assert_eq!(
        orphans,
        vec!["MAX_CONNECTIONS"],
        "the app-undefined key is the orphan"
    );

    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn infer_write_persists_cross_repo_edges_that_survive_sync() {
    let base = std::env::temp_dir().join(format!("roteiro-infer-write-{}", std::process::id()));
    std::fs::remove_dir_all(&base).ok();
    let app = base.join("app");
    let deploy = base.join("deploy");
    std::fs::create_dir_all(&app).expect("mkdir app");
    std::fs::create_dir_all(&deploy).expect("mkdir deploy");

    // Hub with two config keys; spoke overrides both under a different convention.
    std::fs::write(
        app.join("config.toml"),
        "[serve]\naddr = \"127.0.0.1:8017\"\ntools = true\n",
    )
    .expect("write");
    git(&app, &["init", "-q"]);
    git(&app, &["add", "."]);
    git(&app, &["commit", "-q", "-m", "init"]);
    assert!(roteiro(&app, &["sync"]).status.success(), "app sync failed");

    std::fs::write(
        deploy.join("prod.env"),
        "SERVE_ADDR=0.0.0.0:8443\nSERVE_TOOLS=false\n",
    )
    .expect("write");
    git(&deploy, &["init", "-q"]);
    git(&deploy, &["add", "."]);
    git(&deploy, &["commit", "-q", "-m", "init"]);
    assert!(
        roteiro(&deploy, &["sync"]).status.success(),
        "deploy sync failed"
    );

    let base_s = base.to_str().unwrap();

    // Persist the inferred links into the spoke's graph.
    let out = roteiro(
        &base,
        &[
            "links",
            "--infer",
            "--hub",
            "app",
            "--write",
            "--workspace",
            base_s,
            "--json",
        ],
    );
    assert!(out.status.success(), "infer --write failed: {out:?}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    assert_eq!(v["written"], 2, "two matches persisted as edges: {v}");

    // The external-ref target nodes are now queryable in the spoke's graph.
    let q = roteiro(&deploy, &["query", "--kind", "external_ref", "--json"]);
    assert!(q.status.success(), "query failed: {q:?}");
    let text = String::from_utf8_lossy(&q.stdout);
    assert!(
        text.contains("app::cfgkey:config.toml#serve.addr"),
        "external-ref to the hub key must be present: {text}"
    );

    // A re-sync of the spoke must not drop the persisted layer (it is re-applied).
    assert!(
        roteiro(&deploy, &["sync"]).status.success(),
        "deploy re-sync failed"
    );
    let q = roteiro(&deploy, &["query", "--kind", "external_ref", "--json"]);
    let text = String::from_utf8_lossy(&q.stdout);
    assert!(
        text.contains("app::cfgkey:config.toml#serve.addr"),
        "external-ref must survive a re-sync: {text}"
    );

    std::fs::remove_dir_all(&base).ok();
}

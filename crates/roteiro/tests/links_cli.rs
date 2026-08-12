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

fn head_sha(dir: &Path) -> String {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .expect("rev-parse");
    assert!(
        out.status.success(),
        "git rev-parse HEAD failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// Run `links --infer --hub app --workspace <base> --json` plus `extra` and return
/// the parsed report.
fn infer_json(base: &Path, extra: &[&str]) -> serde_json::Value {
    let base_s = base.to_str().unwrap();
    let mut args = vec!["links", "--infer", "--hub", "app", "--workspace", base_s];
    args.extend_from_slice(extra);
    args.push("--json");
    let out = roteiro(base, &args);
    assert!(out.status.success(), "infer {extra:?} failed: {out:?}");
    serde_json::from_slice(&out.stdout).expect("JSON")
}

/// The `hub_key`s a report's first spoke matched.
fn matched_hub_keys(report: &serde_json::Value) -> Vec<String> {
    report["spokes"][0]["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["hub_key"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn infer_resolves_against_a_pinned_hub_version() {
    let base = std::env::temp_dir().join(format!("roteiro-hubrev-cli-{}", std::process::id()));
    std::fs::remove_dir_all(&base).ok();
    let app = base.join("app");
    let deploy = base.join("deploy");
    std::fs::create_dir_all(&app).expect("mkdir app");
    std::fs::create_dir_all(&deploy).expect("mkdir deploy");

    // Hub v1 defines `serve.tools`; v2 renames it to `serve.features`. Sync each so
    // the HEAD graph reflects v2.
    std::fs::write(app.join("config.toml"), "[serve]\ntools = true\n").expect("write");
    git(&app, &["init", "-q"]);
    git(&app, &["add", "."]);
    git(&app, &["commit", "-q", "-m", "v1"]);
    let v1 = head_sha(&app);
    // Tag v1 so we can pin by a *revspec* (a tag), not just a sha — the resolver
    // must accept both.
    git(&app, &["tag", "rel-1"]);
    assert!(roteiro(&app, &["sync"]).status.success(), "app v1 sync");
    std::fs::write(app.join("config.toml"), "[serve]\nfeatures = true\n").expect("write");
    git(&app, &["commit", "-aqm", "v2 rename"]);
    assert!(roteiro(&app, &["sync"]).status.success(), "app v2 sync");

    // Spoke references the *old* key (`SERVE_TOOLS`).
    std::fs::write(deploy.join("prod.env"), "SERVE_TOOLS=true\n").expect("write");
    git(&deploy, &["init", "-q"]);
    git(&deploy, &["add", "."]);
    git(&deploy, &["commit", "-q", "-m", "init"]);
    assert!(roteiro(&deploy, &["sync"]).status.success(), "deploy sync");

    // Against HEAD: the hub no longer defines the key, so it's an orphan (drift).
    let head = infer_json(&base, &[]);
    let orphans: Vec<&str> = head["spokes"][0]["orphans"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["key"].as_str().unwrap())
        .collect();
    assert!(
        orphans.contains(&"SERVE_TOOLS"),
        "drift against HEAD: {head}"
    );
    assert_eq!(head["hub_rev"], serde_json::Value::Null);

    // Against the pinned v1 — named by the *tag* `rel-1`, not a sha — it resolves.
    let by_tag = infer_json(&base, &["--hub-rev", "rel-1"]);
    assert_eq!(by_tag["hub_rev"], "rel-1", "reports the pinned rev (a tag)");
    assert!(
        matched_hub_keys(&by_tag).contains(&"serve.tools".to_owned()),
        "resolves at the pinned version: {by_tag}"
    );
    assert!(
        by_tag["spokes"][0]["orphans"]
            .as_array()
            .unwrap()
            .is_empty(),
        "no drift once resolved against the deployed version: {by_tag}"
    );

    // The same pin named by the raw sha resolves identically — any revspec works.
    let by_sha = infer_json(&base, &["--hub-rev", &v1]);
    assert_eq!(by_sha["hub_rev"], v1);
    assert!(
        matched_hub_keys(&by_sha).contains(&"serve.tools".to_owned()),
        "sha pin resolves too: {by_sha}"
    );

    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn pinned_auto_resolves_each_spoke_against_the_version_it_vendors() {
    let base = std::env::temp_dir().join(format!("roteiro-pinned-cli-{}", std::process::id()));
    std::fs::remove_dir_all(&base).ok();
    let app = base.join("app");
    let deploy = base.join("deploy");
    std::fs::create_dir_all(&app).expect("mkdir app");
    std::fs::create_dir_all(&deploy).expect("mkdir deploy");

    // Hub v1 defines `serve.tools`; v2 renames it. Sync each; capture the v1 sha.
    std::fs::write(app.join("config.toml"), "[serve]\ntools = true\n").expect("write");
    git(&app, &["init", "-q"]);
    git(&app, &["add", "."]);
    git(&app, &["commit", "-q", "-m", "v1"]);
    let v1 = head_sha(&app);
    assert!(roteiro(&app, &["sync"]).status.success(), "app v1 sync");
    std::fs::write(app.join("config.toml"), "[serve]\nfeatures = true\n").expect("write");
    git(&app, &["commit", "-aqm", "v2"]);
    assert!(roteiro(&app, &["sync"]).status.success(), "app v2 sync");

    // Spoke references the old key AND vendors the hub as a submodule pinned to v1.
    std::fs::write(deploy.join("prod.env"), "SERVE_TOOLS=true\n").expect("write");
    std::fs::write(
        deploy.join(".gitmodules"),
        "[submodule \"app\"]\n\tpath = app\n\turl = https://github.com/acme/app.git\n",
    )
    .expect("write .gitmodules");
    git(&deploy, &["init", "-q"]);
    git(&deploy, &["add", "prod.env", ".gitmodules"]);
    // The gitlink pins the hub at its v1 commit.
    git(
        &deploy,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{v1},app"),
        ],
    );
    git(&deploy, &["commit", "-q", "-m", "deploy pinned to app@v1"]);
    assert!(roteiro(&deploy, &["sync"]).status.success(), "deploy sync");

    let base_s = base.to_str().unwrap();
    let out = roteiro(
        &base,
        &[
            "links",
            "--infer",
            "--pinned",
            "--hub",
            "app",
            "--workspace",
            base_s,
            "--json",
        ],
    );
    assert!(out.status.success(), "pinned infer failed: {out:?}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");
    let spoke = &v["spokes"][0];
    assert_eq!(spoke["repo"], "deploy");
    // Auto-detected the v1 pin via the submodule, so the old key resolves.
    assert_eq!(
        spoke["hub_rev"], v1,
        "resolved against the vendored version: {v}"
    );
    assert!(
        spoke["pin_via"]
            .as_str()
            .unwrap_or("")
            .contains("submodule app"),
        "reports the pin source: {v}"
    );
    let matched: Vec<&str> = spoke["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["hub_key"].as_str().unwrap())
        .collect();
    assert!(
        matched.contains(&"serve.tools"),
        "resolves at the pinned version: {v}"
    );

    std::fs::remove_dir_all(&base).ok();
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

    // Now the hub loses every key the spoke matched: the spoke drops to *zero*
    // matches. A re-`--write` must clear the stale inferred links (the layer is
    // re-applied authoritatively even when empty), not leak them.
    std::fs::write(app.join("config.toml"), "[database]\nhost = \"db\"\n").expect("rewrite");
    git(&app, &["commit", "-aqm", "unrelated config"]);
    assert!(
        roteiro(&app, &["sync"]).status.success(),
        "app re-sync failed"
    );
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
    assert!(out.status.success(), "re-infer --write failed: {out:?}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    assert_eq!(v["written"], 0, "no matches remain: {v}");
    // The stale cross-repo *edge* must be cleared: the empty layer is applied
    // authoritatively (not skipped), so the spoke's config key no longer has an
    // inferred `references` edge into the hub. (Orphan external-ref nodes with no
    // edges are cleaned by the next real re-sync, as with any import layer.)
    let q = roteiro(&deploy, &["query", "cfgkey:prod.env#SERVE_ADDR", "--json"]);
    assert!(q.status.success(), "query failed: {q:?}");
    let ex: serde_json::Value = serde_json::from_slice(&q.stdout).expect("valid JSON");
    let outgoing = ex["outgoing"].as_array().cloned().unwrap_or_default();
    assert!(
        !outgoing
            .iter()
            .any(|e| e["node"].as_str().is_some_and(|n| n.starts_with("extref:"))),
        "stale inferred cross-repo edge must be cleared when matches drop to zero: {ex}"
    );

    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn incompatible_link_flag_combinations_are_rejected() {
    // clap constraints fail fast rather than silently running a surprising path.
    for args in [
        ["links", "--infer", "--matrix"].as_slice(),
        ["links", "--write"].as_slice(), // --write without --infer
        ["links", "--html"].as_slice(),  // --html without --matrix
        ["links", "--out", "x.html"].as_slice(), // --out without --html
    ] {
        let out = roteiro(Path::new("."), args);
        assert!(
            !out.status.success(),
            "expected {args:?} to be rejected by clap"
        );
    }
}

#[test]
fn matrix_renders_override_grid_and_drift_across_formats() {
    let base = std::env::temp_dir().join(format!("roteiro-matrix-cli-{}", std::process::id()));
    std::fs::remove_dir_all(&base).ok();
    let app = base.join("app");
    let deploy = base.join("deploy");
    std::fs::create_dir_all(&app).expect("mkdir app");
    std::fs::create_dir_all(&deploy).expect("mkdir deploy");

    // Hub defines serve.addr + serve.tools.
    std::fs::write(
        app.join("config.toml"),
        "[serve]\naddr = \"127.0.0.1:8017\"\ntools = true\n",
    )
    .expect("write");
    git(&app, &["init", "-q"]);
    git(&app, &["add", "."]);
    git(&app, &["commit", "-q", "-m", "init"]);
    assert!(roteiro(&app, &["sync"]).status.success(), "app sync failed");

    // Spoke overrides addr to a *different* value, restates tools identically, and
    // sets one orphan key (drift).
    std::fs::write(
        deploy.join("prod.env"),
        "SERVE_ADDR=0.0.0.0:8443\nSERVE_TOOLS=true\nMAX_CONNECTIONS=512\n",
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
    let common = ["links", "--matrix", "--hub", "app", "--workspace", base_s];

    // JSON: serve.addr is a real override (differs), serve.tools is redundant, and
    // MAX_CONNECTIONS is drift.
    let mut json_args = common.to_vec();
    json_args.push("--json");
    let out = roteiro(&base, &json_args);
    assert!(out.status.success(), "matrix --json failed: {out:?}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    assert_eq!(v["hub"], "app");
    let rows = v["rows"].as_array().expect("rows");
    let addr = rows
        .iter()
        .find(|r| r["hub_key"] == "serve.addr")
        .expect("serve.addr row");
    assert_eq!(
        addr["cells"]["deploy"]["differs"], true,
        "addr is an override"
    );
    let tools = rows
        .iter()
        .find(|r| r["hub_key"] == "serve.tools")
        .expect("serve.tools row");
    assert_eq!(
        tools["cells"]["deploy"]["differs"], false,
        "tools restated identically"
    );
    let drift: Vec<&str> = v["drift"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["key"].as_str().unwrap())
        .collect();
    assert_eq!(drift, vec!["MAX_CONNECTIONS"]);

    // Text: marks the override with ≠ and lists drift.
    let out = roteiro(&base, &common);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("≠ deploy: 0.0.0.0:8443"), "{text}");
    assert!(
        text.contains("drift") && text.contains("MAX_CONNECTIONS"),
        "{text}"
    );

    // HTML: a self-contained page written to the requested file.
    let html_path = base.join("overview.html");
    let mut html_args = common.to_vec();
    html_args.extend(["--html", "--out", html_path.to_str().unwrap()]);
    let out = roteiro(&base, &html_args);
    assert!(out.status.success(), "matrix --html failed: {out:?}");
    let html = std::fs::read_to_string(&html_path).expect("html written");
    assert!(html.starts_with("<!doctype html>"));
    assert!(html.contains("<style>"), "self-contained CSS");
    assert!(html.contains("serve.addr") && html.contains("0.0.0.0:8443"));
    assert!(html.contains("MAX_CONNECTIONS"), "drift shown");

    std::fs::remove_dir_all(&base).ok();
}

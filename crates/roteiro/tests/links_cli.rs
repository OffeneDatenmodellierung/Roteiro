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
fn hub_rev_uses_a_published_graph_artifact_when_present() {
    let base = std::env::temp_dir().join(format!("roteiro-artifact-{}", std::process::id()));
    std::fs::remove_dir_all(&base).ok();
    let app = base.join("app");
    let deploy = base.join("deploy");
    std::fs::create_dir_all(&app).expect("mkdir app");
    std::fs::create_dir_all(&deploy).expect("mkdir deploy");

    std::fs::write(app.join("config.toml"), "[serve]\ntools = true\n").expect("write");
    git(&app, &["init", "-q"]);
    git(&app, &["add", "."]);
    git(&app, &["commit", "-q", "-m", "v1"]);
    let v1 = head_sha(&app);
    assert!(roteiro(&app, &["sync"]).status.success(), "app sync");

    // Export the hub graph, then inject a sentinel config key that is NOT in the
    // actual tree, and publish it at the conventional artifact path for v1's tree.
    // If resolution later surfaces the sentinel, it used the artifact — not extraction.
    let export = roteiro(&app, &["export", "--out", "-"]);
    assert!(export.status.success(), "export failed: {export:?}");
    let mut art: serde_json::Value = serde_json::from_slice(&export.stdout).expect("artifact JSON");
    art["facts"]["nodes"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "key": "cfgkey:art.toml#artifact.only", "kind": "config_key", "name": "artifact.only",
            "path": "art.toml", "lang": null, "blob_hash": null, "span": null,
            "provenance": "derived", "meta": { "key": "artifact.only", "value": "yes" }
        }));
    let tree = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD^{tree}"])
            .current_dir(&app)
            .output()
            .expect("tree")
            .stdout,
    )
    .unwrap()
    .trim()
    .to_owned();
    let art_dir = app.join(".git/roteiro/artifacts");
    std::fs::create_dir_all(&art_dir).expect("mkdir artifacts");
    std::fs::write(art_dir.join(format!("{tree}.json")), art.to_string()).expect("write artifact");

    // Spoke references a key only the *artifact* defines, plus a real one.
    std::fs::write(
        deploy.join("prod.env"),
        "ARTIFACT_ONLY=yes\nSERVE_TOOLS=true\n",
    )
    .expect("write");
    git(&deploy, &["init", "-q"]);
    git(&deploy, &["add", "."]);
    git(&deploy, &["commit", "-q", "-m", "init"]);
    assert!(roteiro(&deploy, &["sync"]).status.success(), "deploy sync");

    let base_s = base.to_str().unwrap();
    let out = roteiro(
        &base,
        &[
            "links",
            "--infer",
            "--hub",
            "app",
            "--hub-rev",
            &v1,
            "--workspace",
            base_s,
            "--json",
        ],
    );
    assert!(out.status.success(), "hub-rev infer failed: {out:?}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");
    let matched = matched_hub_keys(&v);
    assert!(
        matched.contains(&"artifact.only".to_owned()),
        "resolution used the published artifact (sentinel key present): {v}"
    );
    assert!(matched.contains(&"serve.tools".to_owned()), "{v}");

    // A corrupt artifact is "not usable": resolution must fall back to extraction
    // (no sentinel, but the real tree key still resolves), never abort.
    std::fs::write(art_dir.join(format!("{tree}.json")), "{ not valid json").expect("corrupt");
    let v = infer_json(&base, &["--hub-rev", &v1]);
    let matched = matched_hub_keys(&v);
    assert!(
        !matched.contains(&"artifact.only".to_owned()),
        "corrupt artifact must be ignored: {v}"
    );
    assert!(
        matched.contains(&"serve.tools".to_owned()),
        "fell back to extraction: {v}"
    );

    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn pinned_uses_pins_config_template_for_image_tags() {
    let base = std::env::temp_dir().join(format!("roteiro-pinstmpl-{}", std::process::id()));
    std::fs::remove_dir_all(&base).ok();
    let app = base.join("app");
    let deploy = base.join("deploy");
    std::fs::create_dir_all(&app).expect("mkdir app");
    std::fs::create_dir_all(&deploy).expect("mkdir deploy");

    // Hub v1 (serve.tools), tagged `release-1.2` — a scheme the default `1.2`/`v1.2`
    // guess would miss. Then v2 renames the key at HEAD.
    std::fs::write(app.join("config.toml"), "[serve]\ntools = true\n").expect("write");
    git(&app, &["init", "-q"]);
    git(&app, &["add", "."]);
    git(&app, &["commit", "-q", "-m", "v1"]);
    git(&app, &["tag", "release-1.2"]);
    assert!(roteiro(&app, &["sync"]).status.success(), "app v1 sync");
    std::fs::write(app.join("config.toml"), "[serve]\nfeatures = true\n").expect("write");
    git(&app, &["commit", "-aqm", "v2"]);
    assert!(roteiro(&app, &["sync"]).status.success(), "app v2 sync");

    // Spoke: a Dockerfile pins image `app:1.2`, and `[pins]` says its git ref is
    // `release-{tag}`. It also references the old key.
    std::fs::write(deploy.join("Dockerfile"), "FROM registry.io/app:1.2\n").expect("write");
    std::fs::write(deploy.join("prod.env"), "SERVE_TOOLS=true\n").expect("write");
    std::fs::write(
        deploy.join("roteiro.toml"),
        "[pins]\napp = \"release-{tag}\"\n",
    )
    .expect("write");
    git(&deploy, &["init", "-q"]);
    git(&deploy, &["add", "."]);
    git(&deploy, &["commit", "-q", "-m", "init"]);
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
    // The `[pins]` template resolved the image tag to the `release-1.2` git tag.
    assert_eq!(
        spoke["hub_rev"], "release-1.2",
        "resolved via [pins] template: {v}"
    );
    assert!(
        spoke["pin_via"].as_str().unwrap_or("").starts_with("image"),
        "pinned via the image: {v}"
    );
    assert!(
        matched_hub_keys(&v).contains(&"serve.tools".to_owned()),
        "old key resolves at the deployed version: {v}"
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
fn workspace_name_selects_a_named_workspace_from_config() {
    // A `[[workspaces]]` config names a `prod` workspace over a base dir holding a
    // hub + spoke; `roteiro links --workspace-name prod` scopes to it (no
    // `--workspace <root>` needed) and resolves the spoke's authored link.
    let base = std::env::temp_dir().join(format!("roteiro-wsname-cli-{}", std::process::id()));
    std::fs::remove_dir_all(&base).ok();
    let home = base.join("home");
    let app = base.join("app");
    let deploy = base.join("deploy");
    std::fs::create_dir_all(&home).expect("mkdir home");
    std::fs::create_dir_all(&app).expect("mkdir app");
    std::fs::create_dir_all(&deploy).expect("mkdir deploy");

    // Hub: a real graph with a `file:README.md` node.
    std::fs::write(app.join("README.md"), "# App\n").expect("write");
    git(&app, &["init", "-q"]);
    git(&app, &["add", "."]);
    git(&app, &["commit", "-q", "-m", "init"]);

    // A **user** config (via ROTEIRO_HOME) naming a `prod` workspace over `base`.
    std::fs::write(
        home.join("config.toml"),
        format!(
            "[[workspaces]]\nname = \"prod\"\nroots = [\"{}\"]\n",
            base.display()
        ),
    )
    .expect("write config");

    // Sync the hub (ROTEIRO_HOME isolates state from the real ~/.roteiro).
    let sync = Command::new(BIN)
        .args(["sync"])
        .current_dir(&app)
        .env("ROTEIRO_HOME", &home)
        .output()
        .expect("sync");
    assert!(sync.status.success(), "app sync: {sync:?}");

    // Spoke: an authored link into the hub.
    std::fs::write(
        deploy.join("roteiro.toml"),
        "[[links]]\nto = \"app::file:README.md\"\n",
    )
    .expect("write toml");
    git(&deploy, &["init", "-q"]);
    git(&deploy, &["add", "."]);
    git(&deploy, &["commit", "-q", "-m", "init"]);

    // `--workspace-name prod` selects the config workspace; the link resolves `ok`.
    let out = Command::new(BIN)
        .args(["links", "--workspace-name", "prod", "--json"])
        .current_dir(&base)
        .env("ROTEIRO_HOME", &home)
        .output()
        .expect("links by name");
    assert!(
        out.status.success(),
        "links --workspace-name prod failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let arr: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    let arr = arr.as_array().expect("array");
    assert_eq!(arr.len(), 1, "one authored link: {arr:?}");
    assert_eq!(arr[0]["status"], "ok", "{arr:?}");
    assert_eq!(arr[0]["to"], "app::file:README.md");

    // An unknown `--workspace-name` fails, naming the known workspaces.
    let bad = Command::new(BIN)
        .args(["links", "--workspace-name", "bogus"])
        .current_dir(&base)
        .env("ROTEIRO_HOME", &home)
        .output()
        .expect("links bad name");
    assert!(!bad.status.success(), "unknown workspace name must fail");
    let err = String::from_utf8_lossy(&bad.stderr);
    assert!(
        err.contains("bogus") && err.contains("prod"),
        "error should name the unknown and known workspaces: {err}"
    );

    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn unrelated_misconfigured_workspace_does_not_break_legacy_fallback() {
    // A config with a deliberately BROKEN `[[workspaces]]` root (a non-existent
    // path). Running `links` from a repo that belongs to NO configured workspace
    // must still work via the legacy flat `--workspace` fallback — the unrelated bad
    // workspace is never discovered/validated in the fallback path.
    let base = std::env::temp_dir().join(format!("roteiro-wsfallback-cli-{}", std::process::id()));
    std::fs::remove_dir_all(&base).ok();
    let home = base.join("home");
    let app = base.join("app");
    let deploy = base.join("deploy");
    std::fs::create_dir_all(&home).expect("mkdir home");
    std::fs::create_dir_all(&app).expect("mkdir app");
    std::fs::create_dir_all(&deploy).expect("mkdir deploy");

    // Hub with a real node.
    std::fs::write(app.join("README.md"), "# App\n").expect("write");
    git(&app, &["init", "-q"]);
    git(&app, &["add", "."]);
    git(&app, &["commit", "-q", "-m", "init"]);
    let sync = Command::new(BIN)
        .args(["sync"])
        .current_dir(&app)
        .env("ROTEIRO_HOME", &home)
        .output()
        .expect("sync");
    assert!(sync.status.success(), "app sync: {sync:?}");

    // Spoke authored link into the hub — but the spoke belongs to NO configured
    // workspace (the only configured one points at a non-existent root).
    std::fs::write(
        deploy.join("roteiro.toml"),
        "[[links]]\nto = \"app::file:README.md\"\n",
    )
    .expect("write toml");
    git(&deploy, &["init", "-q"]);
    git(&deploy, &["add", "."]);
    git(&deploy, &["commit", "-q", "-m", "init"]);

    // User config names one workspace `other` over a broken (missing) root.
    let broken = base.join("does-not-exist").join("xyz");
    std::fs::write(
        home.join("config.toml"),
        format!(
            "[[workspaces]]\nname = \"other\"\nroots = [\"{}\"]\n",
            broken.display()
        ),
    )
    .expect("write config");

    // From the spoke repo (which matches no configured workspace), a flat
    // `--workspace <base>` run must fall back and resolve the link — the broken
    // `other` workspace is skipped, not fatal.
    let base_s = base.to_str().unwrap();
    let out = Command::new(BIN)
        .args(["links", "--workspace", base_s, "--json"])
        .current_dir(&deploy)
        .env("ROTEIRO_HOME", &home)
        .output()
        .expect("links fallback");
    assert!(
        out.status.success(),
        "legacy fallback must ignore the unrelated broken workspace: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let arr: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    let arr = arr.as_array().expect("array");
    assert!(!arr.is_empty(), "the authored link is reported: {arr:?}");
    assert!(
        arr.iter().all(|r| r["status"] == "ok"),
        "every link resolves via the flat fallback (no drift from the broken workspace): {arr:?}"
    );
    assert!(
        arr.iter().any(|r| r["to"] == "app::file:README.md"),
        "the authored link resolved to the hub node: {arr:?}"
    );

    // But an unknown `--workspace-name` still errors clearly, naming the known ones.
    let bad = Command::new(BIN)
        .args(["links", "--workspace-name", "nope"])
        .current_dir(&deploy)
        .env("ROTEIRO_HOME", &home)
        .output()
        .expect("links bad name");
    assert!(!bad.status.success(), "unknown --workspace-name must fail");
    let err = String::from_utf8_lossy(&bad.stderr);
    assert!(
        err.contains("nope") && err.contains("other"),
        "error names the unknown and the known workspace: {err}"
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

/// The `--matrix` JSON carries each row's hub **source file**, and
/// `roteiro links --matrix --app-config-only` drops a row whose hub key is sourced
/// from a build/tooling file (`Cargo.toml`) — parity with the explorer's client-side
/// "hide tooling config" toggle, which classifies rows by that same per-row file.
#[test]
fn matrix_carries_row_file_and_app_config_only_drops_tooling_rows() {
    let base = std::env::temp_dir().join(format!("roteiro-matrix-tooling-{}", std::process::id()));
    std::fs::remove_dir_all(&base).ok();
    let app = base.join("app");
    let deploy = base.join("deploy");
    std::fs::create_dir_all(&app).expect("mkdir app");
    std::fs::create_dir_all(&deploy).expect("mkdir deploy");

    // Hub defines an app-config key (config.toml#serve.addr) AND a tooling key
    // (Cargo.toml#package.name).
    std::fs::write(
        app.join("config.toml"),
        "[serve]\naddr = \"127.0.0.1:8017\"\n",
    )
    .expect("write");
    std::fs::write(app.join("Cargo.toml"), "[package]\nname = \"app\"\n").expect("write");
    git(&app, &["init", "-q"]);
    git(&app, &["add", "."]);
    git(&app, &["commit", "-q", "-m", "init"]);
    assert!(roteiro(&app, &["sync"]).status.success(), "app sync failed");

    // Spoke overrides both — the app key from its env, the tooling key from its own
    // Cargo.toml — so `--app-config-only` drops the tooling override on BOTH sides
    // (no row, no drift), leaving only the app-config override.
    std::fs::write(deploy.join("prod.env"), "SERVE_ADDR=0.0.0.0:8443\n").expect("write");
    std::fs::write(deploy.join("Cargo.toml"), "[package]\nname = \"deploy\"\n").expect("write");
    git(&deploy, &["init", "-q"]);
    git(&deploy, &["add", "."]);
    git(&deploy, &["commit", "-q", "-m", "init"]);
    assert!(
        roteiro(&deploy, &["sync"]).status.success(),
        "deploy sync failed"
    );

    let base_s = base.to_str().unwrap();
    let common = [
        "links",
        "--matrix",
        "--hub",
        "app",
        "--workspace",
        base_s,
        "--json",
    ];

    // Default: both rows present, each carrying its hub source file.
    let out = roteiro(&base, &common);
    assert!(out.status.success(), "matrix --json failed: {out:?}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    let rows = v["rows"].as_array().expect("rows");
    let addr = rows
        .iter()
        .find(|r| r["hub_key"] == "serve.addr")
        .expect("serve.addr row");
    assert_eq!(
        addr["file"], "config.toml",
        "app-config row carries its file"
    );
    let pkg = rows
        .iter()
        .find(|r| r["hub_key"] == "package.name")
        .expect("package.name row shown by default");
    assert_eq!(pkg["file"], "Cargo.toml", "tooling row carries its file");

    // `--app-config-only`: the tooling-sourced row is dropped (and doesn't resurface
    // as drift, since the spoke's override is tooling-sourced too); the app row stays.
    let mut filtered = common.to_vec();
    filtered.push("--app-config-only");
    let out = roteiro(&base, &filtered);
    assert!(
        out.status.success(),
        "matrix --app-config-only failed: {out:?}"
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    let rows = v["rows"].as_array().expect("rows");
    assert!(
        rows.iter().any(|r| r["hub_key"] == "serve.addr"),
        "app-config row kept: {rows:?}"
    );
    assert!(
        !rows.iter().any(|r| r["hub_key"] == "package.name"),
        "tooling-sourced row dropped under --app-config-only: {rows:?}"
    );
    let drift = v["drift"].as_array().expect("drift");
    assert!(
        !drift
            .iter()
            .any(|d| d["key"] == "package.name" || d["key"] == "PACKAGE_NAME"),
        "tooling override does not resurface as drift: {drift:?}"
    );

    std::fs::remove_dir_all(&base).ok();
}

/// `roteiro query --kind config_key --app-config-only` drops config keys sourced
/// from build/tooling/CI files (here `Cargo.toml`) while keeping real app config
/// (here `config/app.toml`). The DEFAULT (no flag) still lists everything — the
/// filter is strictly opt-in.
#[test]
fn query_app_config_only_drops_tooling_keys() {
    let base = std::env::temp_dir().join(format!("roteiro-appconfig-cli-{}", std::process::id()));
    std::fs::remove_dir_all(&base).ok();
    std::fs::create_dir_all(base.join("config")).expect("mkdir");

    // A tooling manifest (Cargo.toml) alongside real app config (config/app.toml).
    std::fs::write(
        base.join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
    )
    .expect("write Cargo.toml");
    std::fs::write(
        base.join("config/app.toml"),
        "[serve]\naddr = \"0.0.0.0:8080\"\n",
    )
    .expect("write app.toml");
    git(&base, &["init", "-q"]);
    git(&base, &["add", "."]);
    git(&base, &["commit", "-q", "-m", "init"]);

    let keys = |args: &[&str]| -> Vec<String> {
        let out = roteiro(&base, args);
        assert!(out.status.success(), "query failed: {out:?}");
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");
        v["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["key"].as_str().unwrap().to_owned())
            .collect()
    };

    // Default: everything is listed — both the Cargo.toml and the app.toml keys.
    let all = keys(&["query", "--kind", "config_key", "--json"]);
    assert!(
        all.iter().any(|k| k.starts_with("cfgkey:Cargo.toml#")),
        "default lists tooling keys: {all:?}"
    );
    assert!(
        all.iter().any(|k| k.starts_with("cfgkey:config/app.toml#")),
        "default lists app keys: {all:?}"
    );

    // Opt-in: `--app-config-only` drops the tooling keys, keeps the app keys.
    let app_only = keys(&[
        "query",
        "--kind",
        "config_key",
        "--app-config-only",
        "--json",
    ]);
    assert!(
        !app_only.iter().any(|k| k.starts_with("cfgkey:Cargo.toml#")),
        "tooling keys must be dropped: {app_only:?}"
    );
    assert!(
        app_only
            .iter()
            .any(|k| k.starts_with("cfgkey:config/app.toml#")),
        "app keys must remain: {app_only:?}"
    );

    std::fs::remove_dir_all(&base).ok();
}

/// `--app-config-only` is only meaningful for `roteiro links --infer` / `--matrix`
/// (it filters config-key matching). Passed to the plain authored-links report — a
/// mode that does no such matching — it must be REJECTED with a clear error, not
/// silently ignored.
#[test]
fn links_app_config_only_without_infer_or_matrix_is_rejected() {
    let base = std::env::temp_dir().join(format!("roteiro-acoreject-cli-{}", std::process::id()));
    std::fs::remove_dir_all(&base).ok();
    std::fs::create_dir_all(&base).expect("mkdir");
    std::fs::write(base.join("README.md"), "# x\n").expect("write");
    git(&base, &["init", "-q"]);
    git(&base, &["add", "."]);
    git(&base, &["commit", "-q", "-m", "init"]);

    let out = roteiro(&base, &["links", "--app-config-only"]);
    assert!(
        !out.status.success(),
        "plain `links --app-config-only` must fail, not silently ignore the flag: {out:?}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--app-config-only") && stderr.contains("--infer"),
        "error must explain the flag applies to --infer/--matrix: {stderr}"
    );

    std::fs::remove_dir_all(&base).ok();
}

/// Run `links -w one` from `cwd` and assert the scope holds each repo exactly
/// once — the invariant behind issue #501, checked through what the command
/// actually reports.
#[cfg(unix)]
fn assert_scope_holds_each_repo_once(
    base: &Path,
    cwd: &Path,
    cwd_name: &str,
    distinct_repos: usize,
) {
    let run = |extra: &[&str]| {
        let mut args = vec!["links", "-w", "one"];
        args.extend_from_slice(extra);
        Command::new(BIN)
            .args(&args)
            .current_dir(cwd)
            .env("ROTEIRO_HOME", base)
            .output()
            .expect("run roteiro")
    };
    let out = run(&["--json"]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let report: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "links --json ({cwd_name}): {e}; stdout={stdout}; stderr={}",
            String::from_utf8_lossy(&out.stderr)
        )
    });

    // THE invariant: alpha declares that link once, so it is reported once.
    // Before the fix, running from inside alpha reported it twice — once as
    // `alpha`, once as a phantom `alpha-2`.
    let occurrences = report
        .iter()
        .filter(|r| r["to"] == "beta::sym:rust:x#Y")
        .count();
    assert_eq!(
        occurrences, 1,
        "alpha declares this link once, so the workspace scope must contain alpha \
         once and the report must show it once; got {occurrences} occurrence(s) from \
         cwd={cwd_name} — the repo is in the scope set more than once, so its \
         `roteiro.toml` was read and resolved more than once (issue #501). \
         Report: {report:#?}"
    );

    // No reported repo may be a disambiguation suffix with no second repo behind
    // it — that is the duplicate wearing a different name.
    for label in report.iter().filter_map(|r| r["repo"].as_str()) {
        assert!(
            !label.ends_with("-2"),
            "`{label}` is a disambiguation suffix invented for a repo the workspace \
             registry holds only once — the scope set contains one repo twice \
             (issue #501, cwd={cwd_name}). Report: {report:#?}"
        );
    }

    // And the scope size is the number of DISTINCT repos, derived from the
    // fixture rather than written down — a literal count passes for the wrong
    // reason the moment the fixture changes.
    let text = String::from_utf8_lossy(&run(&[]).stdout).into_owned();
    assert!(
        text.contains(&format!("across {distinct_repos} repo(s)")),
        "the scope is the {distinct_repos} distinct repo(s) of this fixture (declared \
         members plus the cwd repo when it is not one), from cwd={cwd_name}; got: {text}"
    );
}

/// `roteiro links -w <name>` must put each repo in the workspace scope **exactly
/// once**, however that repo happens to be spelled (issue #501).
///
/// The defect was a set defect, not a count one. `links_scope_paths` de-duplicated
/// with a `BTreeSet<PathBuf>` over path *strings*, and the two sources spell the
/// same repo differently: the current repo arrives fully symlink-resolved
/// (`current_dir` is `getcwd`), while a configured member keeps whatever the
/// config wrote. So both spellings survived — and `run_links` iterates that list
/// directly, reading the repo's `roteiro.toml` twice and reporting each declared
/// link twice, the second under a fabricated `<name>-2` project the workspace
/// registry does not contain.
///
/// Including the current repo is correct and is asserted too — `links` resolves
/// *this* repo's declarations against a workspace, so it must be in scope even
/// when it is not a declared member. The bug was the missing de-duplication, not
/// the inclusion.
#[test]
#[cfg(unix)]
fn a_repo_is_in_the_links_scope_exactly_once_however_it_is_spelled() {
    let base = std::env::temp_dir().join(format!("roteiro-links-dedup-{}", std::process::id()));
    std::fs::remove_dir_all(&base).ok();
    let real = base.join("real");
    std::fs::create_dir_all(&real).expect("mkdir real");
    for name in ["alpha", "beta", "gamma"] {
        let dir = real.join(name);
        std::fs::create_dir_all(&dir).expect("mkdir repo");
        git(&dir, &["init", "-q"]);
    }
    // A second spelling of the same directory. On a developer Mac `/tmp` and
    // `/var` are already symlinks, so the defect reproduced there by accident; an
    // explicit link makes the test mean the same thing on CI, where they are not.
    let alias = base.join("alias");
    std::os::unix::fs::symlink(&real, &alias).expect("symlink alias -> real");

    // The workspace declares alpha and beta through the ALIAS. A member repo
    // entering the scope from its own cwd arrives resolved, so the two paths
    // differ as strings while naming one repo.
    let members = ["alpha", "beta"];
    let declared = members
        .iter()
        .map(|m| format!("\"{}\"", alias.join(m).to_str().unwrap()))
        .collect::<Vec<_>>()
        .join(", ");
    let config = format!("[[workspaces]]\nname = \"one\"\nrepos = [{declared}]\n");
    for name in ["alpha", "beta", "gamma"] {
        std::fs::write(real.join(name).join("roteiro.toml"), &config).expect("write config");
    }
    // One authored link in alpha, so the report has something to double.
    std::fs::write(
        real.join("alpha").join("roteiro.toml"),
        format!("{config}\n[[links]]\nfrom = \"local\"\nto = \"beta::sym:rust:x#Y\"\n"),
    )
    .expect("write alpha config");

    for cwd_name in ["alpha", "beta", "gamma"] {
        // Enter through the alias: the child's `getcwd` hands the command the
        // resolved spelling, exactly as a shell would.
        let cwd = alias.join(cwd_name);
        let mut distinct: std::collections::BTreeSet<std::path::PathBuf> = members
            .iter()
            .map(|m| alias.join(m).canonicalize().expect("canonicalise member"))
            .collect();
        distinct.insert(cwd.canonicalize().expect("canonicalise cwd"));
        assert_scope_holds_each_repo_once(&base, &cwd, cwd_name, distinct.len());
    }

    // The inclusion itself must not have been removed to fix the count: a
    // non-member cwd is still in scope, which is the whole point of `links`
    // resolving THIS repo's declarations against a workspace.
    std::fs::write(
        real.join("gamma").join("roteiro.toml"),
        format!("{config}\n[[links]]\nfrom = \"local\"\nto = \"beta::sym:rust:g#H\"\n"),
    )
    .expect("write gamma config");
    let out = Command::new(BIN)
        .args(["links", "-w", "one", "--json"])
        .current_dir(alias.join("gamma"))
        .env("ROTEIRO_HOME", &base)
        .output()
        .expect("run roteiro");
    let report: Vec<serde_json::Value> =
        serde_json::from_slice(&out.stdout).expect("links --json (gamma)");
    assert!(
        report.iter().any(|r| r["to"] == "beta::sym:rust:g#H"),
        "a non-member current repo must still be in scope — de-duplicating must not \
         become excluding: {report:#?}"
    );

    std::fs::remove_dir_all(&base).ok();
}

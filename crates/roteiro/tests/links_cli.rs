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

/// A two-repo workspace where the spoke **vendors the hub as a submodule pinned
/// to the hub's v1 commit**, while the hub itself has moved on and renamed the
/// key. Returns the workspace root and that v1 sha.
///
/// Shared by the two `--pinned` tests rather than built twice: they assert the
/// same per-spoke resolution on two different views (`--infer` and `--matrix`),
/// and a fixture that drifted between them would let one view pass against a
/// workspace the other never saw.
fn vendored_hub_workspace(tag: &str) -> (std::path::PathBuf, String) {
    let base = std::env::temp_dir().join(format!("roteiro-{tag}-cli-{}", std::process::id()));
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

    (base, v1)
}

#[test]
fn pinned_auto_resolves_each_spoke_against_the_version_it_vendors() {
    let (base, v1) = vendored_hub_workspace("pinned");
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

/// #504: the same per-spoke resolution, on the **matrix** — the view where it
/// matters most, because the matrix is the side-by-side comparison and spokes
/// deploying different hub versions are exactly the case one shared rev
/// misreports. `--matrix --pinned` was refused by clap until this issue.
#[test]
fn matrix_accepts_pinned_and_resolves_each_spoke_against_its_own_version() {
    let (base, v1) = vendored_hub_workspace("matrixpinned");
    let base_s = base.to_str().unwrap();

    let out = roteiro(
        &base,
        &[
            "links",
            "--matrix",
            "--pinned",
            "--hub",
            "app",
            "--workspace",
            base_s,
            "--json",
        ],
    );
    assert!(
        out.status.success(),
        "`--matrix --pinned` must be accepted: {out:?}"
    );
    let m: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");

    assert_eq!(m["pinned"], true, "the matrix records that it asked: {m}");
    assert_eq!(
        m["pins"]["deploy"]["rev"], v1,
        "and which version this spoke was measured against: {m}"
    );

    // The row is the key that exists at v1, not the one the hub renamed it to —
    // so the pin reached the *comparison*, not only the report about it.
    let keys: Vec<&str> = m["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["hub_key"].as_str().unwrap())
        .collect();
    assert!(
        keys.contains(&"serve.tools"),
        "the matrix compared against the pinned version, not HEAD: {m}"
    );

    // …and the comparison itself used that version. `serve.tools` is `true` at
    // v1 and **absent at HEAD**, so a matrix that computed `differs` off the HEAD
    // column would call an identical value an override — the false drift pinning
    // exists to remove, which is the correctness half ADR-0009 v1.9 declined this
    // combination over.
    let cell = &m["rows"][0]["cells"]["deploy"];
    assert_eq!(
        cell["differs"], false,
        "identical at the deployed revision is not an override: {m}"
    );
    assert_eq!(
        cell["baseline"], "true",
        "and the cell states the baseline it was measured against: {m}"
    );
    // One hub column still, showing HEAD — where this key no longer exists.
    assert_eq!(m["rows"][0]["hub_value"], "", "the column is HEAD: {m}");

    std::fs::remove_dir_all(&base).ok();
}

/// A **struct-derived** hub key has no literal value in code, so `ConfigKey::value`
/// is an empty *placeholder* and `value_known` is false. Recording it as a pinned
/// baseline compares the spoke's genuine value against `""` and reports an
/// override of something that does not exist — the false match `value_known`
/// exists to prevent.
#[test]
fn a_struct_derived_hub_key_is_not_a_pinned_baseline() {
    let base = std::env::temp_dir().join(format!("roteiro-structkey-cli-{}", std::process::id()));
    std::fs::remove_dir_all(&base).ok();
    let app = base.join("app");
    let deploy = base.join("deploy");
    std::fs::create_dir_all(app.join("src")).expect("mkdir app");
    std::fs::create_dir_all(&deploy).expect("mkdir deploy");

    // v1: the key exists ONLY as a `@rto:config` struct field — declared in code,
    // with no literal value anywhere.
    std::fs::write(
        app.join("src/lib.rs"),
        "// @rto:config\npub struct Config {\n    pub tools: String,\n}\n",
    )
    .expect("write");
    git(&app, &["init", "-q"]);
    git(&app, &["add", "."]);
    git(&app, &["commit", "-q", "-m", "v1"]);
    let v1 = head_sha(&app);
    assert!(roteiro(&app, &["sync"]).status.success(), "app v1 sync");

    // HEAD moves the key into a config file with a real value **and drops the
    // struct**, so at HEAD it is known. Without both halves the struct key
    // shadows the file key at HEAD too, both sides read `""`, and the test cannot
    // tell a gated baseline from an ungated one.
    std::fs::write(app.join("config.toml"), "tools = \"head-value\"\n").expect("write");
    std::fs::write(
        app.join("src/lib.rs"),
        "pub struct Config {\n    pub tools: String,\n}\n",
    )
    .expect("write");
    git(&app, &["add", "."]);
    git(&app, &["commit", "-q", "-m", "v2"]);
    assert!(roteiro(&app, &["sync"]).status.success(), "app v2 sync");

    // Deliberately **different** from HEAD's value. If the spoke matched HEAD,
    // falling back to HEAD and refusing to compare would both yield
    // `differs: false` and the test could not tell them apart.
    std::fs::write(deploy.join("prod.env"), "TOOLS=spoke-value\n").expect("write");
    std::fs::write(
        deploy.join(".gitmodules"),
        "[submodule \"app\"]\n\tpath = app\n\turl = https://github.com/acme/app.git\n",
    )
    .expect("write .gitmodules");
    git(&deploy, &["init", "-q"]);
    git(&deploy, &["add", "prod.env", ".gitmodules"]);
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
            "--matrix",
            "--pinned",
            "--hub",
            "app",
            "--workspace",
            base_s,
            "--json",
        ],
    );
    assert!(out.status.success(), "matrix failed: {out:?}");
    let m: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");

    let Some(row) = m["rows"].as_array().and_then(|r| r.first()) else {
        panic!("fixture must produce a row: {m}");
    };
    let cell = &row["cells"]["deploy"];
    assert!(
        cell["baseline"].is_null(),
        "an unknown value is not a baseline: {m}"
    );
    assert_eq!(
        row["hub_value"], "head-value",
        "the fixture must give HEAD a known value, or both sides read \"\" and \
         this test cannot distinguish them: {m}"
    );
    assert_eq!(
        cell["differs"], false,
        "and must not be reported as overriding a value that does not exist: {m}"
    );
    // …and `differs: false` here means "no override is *known*", not "no override
    // exists". Both silent alternatives assert something untrue — comparing
    // against HEAD, a revision this spoke does not run, or declaring equality
    // with a value nobody has — so the cell says the comparison is unavailable.
    assert_eq!(
        cell["baseline_unknown"], true,
        "the cell must say why it made no comparison: {m}"
    );

    std::fs::remove_dir_all(&base).ok();
}

/// A two-repo workspace where `chart` **declares** a `[[links]]` entry pointing
/// at a key in `app`, and both are synced. Returns the workspace root and the
/// chart's path.
///
/// Shared so the two authored-link tests cannot drift apart on the fixture the
/// way they could on two copies of it.
fn declared_link_workspace(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let base = std::env::temp_dir().join(format!("roteiro-{tag}-cli-{}", std::process::id()));
    std::fs::remove_dir_all(&base).ok();
    let app = base.join("app");
    let chart = base.join("chart");
    std::fs::create_dir_all(&app).expect("mkdir app");
    std::fs::create_dir_all(&chart).expect("mkdir chart");

    std::fs::write(app.join("config.toml"), "[batch]\nmax_bytes = 1048576\n").expect("write");
    git(&app, &["init", "-q"]);
    git(&app, &["add", "."]);
    git(&app, &["commit", "-q", "-m", "init"]);
    assert!(roteiro(&app, &["sync"]).status.success(), "app sync");

    std::fs::write(chart.join("values.yaml"), "batch:\n  max_bytes: 2097152\n").expect("write");
    let decl = "[[links]]\nfrom = \"cfgkey:values.yaml#batch.max_bytes\"\n\
                to = \"app::cfgkey:config.toml#batch.max_bytes\"\nkind = \"references\"\n";
    std::fs::write(chart.join("roteiro.toml"), decl).expect("write");
    git(&chart, &["init", "-q"]);
    git(&chart, &["add", "."]);
    git(&chart, &["commit", "-q", "-m", "init"]);
    assert!(roteiro(&chart, &["sync"]).status.success(), "chart sync");

    (base, chart)
}

/// #573: an authored `[[links]]` declaration becomes a durable **`authored`**
/// cross-repo edge, so the `authored → gold` path ADR-0009 documents in three
/// places is reachable at last.
///
/// Covers all three questions the issue left open, because they are one
/// mechanism: the layer is written only under `--write`, replaces by its own ref
/// on re-run, and survives `sync` — and it does none of that to the *inferred*
/// layer, which is why they have separate refs.
#[test]
fn authored_links_are_persisted_as_a_durable_authored_layer() {
    let (base, chart) = declared_link_workspace("authored");
    let base_s = base.to_str().unwrap();
    // Counted on the **edges**, not the placeholder node. Both layers share one
    // placeholder per target (same `extref:` key), so the node's own provenance
    // is whichever layer was applied last and says nothing about a given link.
    // The edge is where `authored` vs `inferred` actually lives.
    let authored_edges = || -> usize {
        let key = "extref:app::cfgkey:config.toml#batch.max_bytes";
        let out = roteiro(&chart, &["query", key, "--json"]);
        if !out.status.success() {
            return 0; // no placeholder at all — nothing was persisted
        }
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");
        v["incoming"].as_array().map_or(0, |a| {
            a.iter().filter(|e| e["provenance"] == "authored").count()
        })
    };

    // Reporting alone must not write — `roteiro links` is a CI gate, and a gate
    // that mutates as a side effect is the surprise this flag exists to avoid.
    let report = roteiro(&base, &["links", "--workspace", base_s]);
    assert!(report.status.success(), "links failed: {report:?}");
    assert_eq!(
        authored_edges(),
        0,
        "a plain report must not persist anything"
    );

    let wrote = roteiro(&base, &["links", "--workspace", base_s, "--write"]);
    assert!(wrote.status.success(), "links --write failed: {wrote:?}");
    assert!(
        String::from_utf8_lossy(&wrote.stdout).contains("persisted 1 authored"),
        "the write must report itself: {wrote:?}"
    );
    assert_eq!(
        authored_edges(),
        1,
        "the declaration is now a persisted fact"
    );

    // Durable across a re-sync — the property `reapply_imports` gives every
    // import layer, and the one ADR-0009 never wrote down for authored links.
    assert!(roteiro(&chart, &["sync"]).status.success(), "re-sync");
    assert_eq!(authored_edges(), 1, "an authored edge survives sync");

    // Removing the declaration removes the edge: the layer is authoritative for
    // its own ref, so a re-run with nothing to say clears what it said before.
    std::fs::write(chart.join("roteiro.toml"), "").expect("write");
    let cleared = roteiro(&base, &["links", "--workspace", base_s, "--write"]);
    assert!(
        cleared.status.success(),
        "links --write failed: {cleared:?}"
    );
    assert!(
        String::from_utf8_lossy(&cleared.stdout).contains("cleared the authored"),
        "clearing is a write and must say so, not look like a no-op: {cleared:?}"
    );
    assert_eq!(authored_edges(), 0, "the stale edge is gone");

    std::fs::remove_dir_all(&base).ok();
}

/// The two link layers replace **independently** — the load-bearing property of
/// giving them separate refs.
///
/// `apply_import_layer` is authoritative per ref: it clears that ref's prior
/// edges before re-applying. Sharing one ref would make `--infer --write` delete
/// every authored edge and the authored write delete every inferred one, each
/// command silently reclassifying the other's work — which would also make
/// ADR-0009's `authored → gold, inferred → slate` meaningless.
#[test]
fn the_authored_and_inferred_link_layers_replace_independently() {
    let (base, chart) = declared_link_workspace("indep");
    let base_s = base.to_str().unwrap();

    assert!(
        roteiro(&base, &["links", "--workspace", base_s, "--write"])
            .status
            .success(),
        "authored write"
    );
    let inferred = roteiro(
        &base,
        &[
            "links",
            "--infer",
            "--write",
            "--hub",
            "app",
            "--workspace",
            base_s,
        ],
    );
    assert!(
        inferred.status.success(),
        "infer --write failed: {inferred:?}"
    );

    let out = roteiro(
        &chart,
        &[
            "query",
            "extref:app::cfgkey:config.toml#batch.max_bytes",
            "--json",
        ],
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");
    let kinds: Vec<&str> = v["incoming"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["provenance"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"authored"),
        "inferring must not delete the authored edge: {v}"
    );
    assert!(
        kinds.contains(&"inferred"),
        "and both provenances coexist on one target: {v}"
    );

    std::fs::remove_dir_all(&base).ok();
}

/// The **no-op** envelope must carry the same contract as a populated one.
///
/// This is the CLI twin of the served endpoint's no-hub branch, and it had the
/// identical defect: a hand-written literal is a second definition of the
/// response, so it silently omitted `pinned`/`pins` and a caller could not tell
/// an asked-for pin resolution from an ordinary no-op. Fixing one and leaving
/// the other is the shape this repository keeps filing issues about, so both are
/// asserted.
#[test]
fn the_no_op_matrix_envelope_still_reports_whether_pinning_was_asked() {
    let base = std::env::temp_dir().join(format!("roteiro-noop-cli-{}", std::process::id()));
    std::fs::remove_dir_all(&base).ok();
    let solo = base.join("solo");
    std::fs::create_dir_all(&solo).expect("mkdir");
    std::fs::write(solo.join("config.toml"), "[serve]\naddr = \"x\"\n").expect("write");
    git(&solo, &["init", "-q"]);
    git(&solo, &["add", "."]);
    git(&solo, &["commit", "-q", "-m", "init"]);
    assert!(roteiro(&solo, &["sync"]).status.success(), "sync");

    let base_s = base.to_str().unwrap();
    // One repo: not enough to infer against anything, so this is the `Nothing`
    // path — the early return whose shape had drifted.
    let run = |extra: &[&str]| -> serde_json::Value {
        let mut args = vec![
            "links",
            "--matrix",
            "--hub",
            "solo",
            "--workspace",
            base_s,
            "--json",
        ];
        args.extend_from_slice(extra);
        let out = roteiro(&base, &args);
        assert!(out.status.success(), "{args:?} failed: {out:?}");
        serde_json::from_slice(&out.stdout).expect("JSON")
    };

    let plain = run(&[]);
    assert!(
        plain["note"].is_string(),
        "the no-op reason survives: {plain}"
    );
    assert_eq!(plain["pinned"], false, "{plain}");

    let pinned = run(&["--pinned"]);
    assert_eq!(
        pinned["pinned"], true,
        "an asked-for pin resolution must be distinguishable from an ordinary \
         no-op, even when there was nothing to resolve: {pinned}"
    );
    assert!(
        pinned["pins"].is_object(),
        "and the map is present: {pinned}"
    );
}

/// `--app-config-only` filters every repo's `HEAD` keys before matching, but the
/// keys extracted from a spoke's **pinned revision** are a separate set that has
/// never been through it. Left unfiltered, a spoke's app key can match a hub
/// *tooling* key that the very same run dropped at `HEAD` — the flag half
/// applied, which is worse than not applied because the asymmetry is invisible.
///
/// (Pre-existing: it reaches `--infer --pinned --app-config-only` too, which
/// shipped in ADR-0009 v1.9. Found on this PR because `--matrix` gained the flag.)
#[test]
fn app_config_only_reaches_the_pinned_revision_too() {
    let base = std::env::temp_dir().join(format!("roteiro-pinfilter-cli-{}", std::process::id()));
    std::fs::remove_dir_all(&base).ok();
    let app = base.join("app");
    let deploy = base.join("deploy");
    std::fs::create_dir_all(&app).expect("mkdir app");
    std::fs::create_dir_all(&deploy).expect("mkdir deploy");

    // At v1 the ONLY definition of `serve.tools` is in tooling config.
    std::fs::write(app.join("deny.toml"), "[serve]\ntools = true\n").expect("write");
    std::fs::write(app.join("config.toml"), "[serve]\naddr = \"127.0.0.1\"\n").expect("write");
    git(&app, &["init", "-q"]);
    git(&app, &["add", "."]);
    git(&app, &["commit", "-q", "-m", "v1"]);
    let v1 = head_sha(&app);
    assert!(roteiro(&app, &["sync"]).status.success(), "app v1 sync");
    // HEAD drops it, so the key exists ONLY at the pinned revision.
    std::fs::write(app.join("deny.toml"), "[serve]\nother = 1\n").expect("write");
    git(&app, &["commit", "-aqm", "v2"]);
    assert!(roteiro(&app, &["sync"]).status.success(), "app v2 sync");

    std::fs::write(deploy.join("prod.env"), "SERVE_TOOLS=true\n").expect("write");
    std::fs::write(
        deploy.join(".gitmodules"),
        "[submodule \"app\"]\n\tpath = app\n\turl = https://github.com/acme/app.git\n",
    )
    .expect("write .gitmodules");
    git(&deploy, &["init", "-q"]);
    git(&deploy, &["add", "prod.env", ".gitmodules"]);
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
    let args = [
        "links",
        "--matrix",
        "--pinned",
        "--hub",
        "app",
        "--workspace",
        base_s,
        "--json",
    ];

    // Without the flag the tooling-sourced row is there — proving the fixture
    // really does match against the pinned revision's tooling key.
    let all: serde_json::Value =
        serde_json::from_slice(&roteiro(&base, &args).stdout).expect("JSON");
    let has_tools = |m: &serde_json::Value| {
        m["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["hub_key"] == "serve.tools")
    };
    assert!(
        has_tools(&all),
        "fixture must produce the row at all: {all}"
    );

    // With it, the row is gone: the pinned revision's keys went through the same
    // filter as HEAD's.
    let mut filtered_args = args.to_vec();
    filtered_args.push("--app-config-only");
    let filtered: serde_json::Value =
        serde_json::from_slice(&roteiro(&base, &filtered_args).stdout).expect("JSON");
    assert!(
        !has_tools(&filtered),
        "a tooling key from the pinned revision must be filtered too: {filtered}"
    );

    std::fs::remove_dir_all(&base).ok();
}

/// A hub that sets the same dotted key in **two files** has two `config_key`
/// nodes with two values. `match_against_hub` picks one and reports its file, so
/// the pinned baseline must be looked up by that same `(file, key)` identity —
/// key alone collapses the two and computes `differs` against whichever happened
/// to land last.
#[test]
fn a_pinned_baseline_is_read_from_the_file_its_match_came_from() {
    let base = std::env::temp_dir().join(format!("roteiro-dupkey-cli-{}", std::process::id()));
    std::fs::remove_dir_all(&base).ok();
    let app = base.join("app");
    let deploy = base.join("deploy");
    std::fs::create_dir_all(&app).expect("mkdir app");
    std::fs::create_dir_all(&deploy).expect("mkdir deploy");

    // v1 sets `serve.tools` in TWO files, with DIFFERENT values.
    std::fs::write(app.join("alt.toml"), "[serve]\ntools = \"from-alt\"\n").expect("write");
    std::fs::write(
        app.join("config.toml"),
        "[serve]\ntools = \"from-config\"\n",
    )
    .expect("write");
    git(&app, &["init", "-q"]);
    git(&app, &["add", "."]);
    git(&app, &["commit", "-q", "-m", "v1"]);
    let v1 = head_sha(&app);
    assert!(roteiro(&app, &["sync"]).status.success(), "app v1 sync");
    // HEAD renames the key away, so neither v1 value survives to HEAD.
    std::fs::write(app.join("alt.toml"), "[serve]\nfeatures = true\n").expect("write");
    std::fs::write(app.join("config.toml"), "[serve]\nfeatures = true\n").expect("write");
    git(&app, &["commit", "-aqm", "v2"]);
    assert!(roteiro(&app, &["sync"]).status.success(), "app v2 sync");

    std::fs::write(deploy.join("prod.env"), "SERVE_TOOLS=from-alt\n").expect("write");
    std::fs::write(
        deploy.join(".gitmodules"),
        "[submodule \"app\"]\n\tpath = app\n\turl = https://github.com/acme/app.git\n",
    )
    .expect("write .gitmodules");
    git(&deploy, &["init", "-q"]);
    git(&deploy, &["add", "prod.env", ".gitmodules"]);
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
            "--matrix",
            "--pinned",
            "--hub",
            "app",
            "--workspace",
            base_s,
            "--json",
        ],
    );
    assert!(out.status.success(), "matrix failed: {out:?}");
    let m: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");

    let row = &m["rows"][0];
    let file = row["file"].as_str().unwrap_or_default();
    let cell = &row["cells"]["deploy"];
    // Whichever file the match came from, the baseline must be *that* file's
    // value — the relation, not a fixed string, so the assertion does not depend
    // on which candidate the matcher happens to pick.
    let expected = match file {
        "alt.toml" => "from-alt",
        "config.toml" => "from-config",
        other => panic!("unexpected hub file {other:?}: {m}"),
    };
    assert_eq!(
        cell["baseline"], expected,
        "the baseline must come from the file the match reports ({file}): {m}"
    );

    std::fs::remove_dir_all(&base).ok();
}

/// The two combinations that must **still** be refused, now that clap no longer
/// carries `requires = "infer"` for `--pinned`.
#[test]
fn pinned_is_still_refused_where_it_cannot_mean_anything() {
    let (base, _) = vendored_hub_workspace("pinnedrefuse");
    let base_s = base.to_str().unwrap();

    // One version for every spoke and each spoke's own version are opposite
    // requests; asking for both is asking to measure drift against two things.
    let both = roteiro(
        &base,
        &[
            "links",
            "--matrix",
            "--pinned",
            "--hub-rev",
            "HEAD",
            "--workspace",
            base_s,
        ],
    );
    assert!(
        !both.status.success(),
        "--pinned with --hub-rev must refuse"
    );

    // And the plain authored-link report resolves no hub version at all. This is
    // the refusal `requires = "infer"` used to give for free, so it is asserted
    // now that it is hand-written.
    let plain = roteiro(&base, &["links", "--pinned", "--workspace", base_s]);
    assert!(!plain.status.success(), "bare --pinned must refuse");
    assert!(
        String::from_utf8_lossy(&plain.stderr).contains("--infer"),
        "and must name where it does apply: {plain:?}"
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
    //
    // `["links", "--write"]` used to be here, and is deliberately gone: since
    // #573 it is the **authored** persist path (`[[links]]` declarations → a
    // durable `authored` import layer), not a mistake. It is covered positively
    // by `authored_links_are_persisted_as_a_durable_authored_layer`.
    for args in [
        ["links", "--infer", "--matrix"].as_slice(),
        ["links", "--html"].as_slice(), // --html without --matrix
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

/// #505: with no spoke pinning anything, `--pinned` fell back to the hub's `HEAD`
/// for every one of them — the right behaviour — and said so **nowhere**. Its
/// output was byte-identical to plain `--infer`, and contained no rev, no `HEAD`
/// and no `@`, so there was nothing in it from which to tell an effective
/// `--pinned` from an inert one.
///
/// That is the silent-wrong-answer class rather than a missing feature: the
/// operator asked *"does this spoke match the hub version it actually deploys?"*
/// and was shown the answer to *"does it match HEAD?"*. Those differ by exactly
/// the drift the flag exists to exclude.
///
/// So the assertion is that the two runs **differ**, and that the difference is a
/// report of the resolution — per spoke, and in a summary line that counts it.
/// The fallback itself is unchanged; only its silence is.
#[test]
fn pinned_reports_the_head_fallback_when_no_spoke_pins_anything() {
    let base = std::env::temp_dir().join(format!("roteiro-nopin-cli-{}", std::process::id()));
    std::fs::remove_dir_all(&base).ok();
    let app = base.join("app");
    let web = base.join("web");
    std::fs::create_dir_all(&app).expect("mkdir app");
    std::fs::create_dir_all(&web).expect("mkdir web");

    std::fs::write(app.join("config.toml"), "[serve]\nfeatures = true\n").expect("write");
    git(&app, &["init", "-q"]);
    git(&app, &["add", "."]);
    git(&app, &["commit", "-q", "-m", "hub"]);
    assert!(roteiro(&app, &["sync"]).status.success(), "app sync");

    // The shape the issue measured on a real 7-spoke workspace: no `.gitmodules`,
    // no Dockerfile, so no `submodule` and no `image_ref` node — nothing to pin
    // with. This is the ordinary infra-repo shape, not an edge case.
    std::fs::write(web.join("prod.env"), "SERVE_FEATURES=true\n").expect("write");
    git(&web, &["init", "-q"]);
    git(&web, &["add", "."]);
    git(&web, &["commit", "-q", "-m", "spoke"]);
    assert!(roteiro(&web, &["sync"]).status.success(), "web sync");

    let base_s = base.to_str().unwrap();
    let run = |extra: &[&str]| -> String {
        let mut args = vec!["links", "--infer", "--hub", "app", "--workspace", base_s];
        args.extend_from_slice(extra);
        let out = roteiro(&base, &args);
        assert!(out.status.success(), "infer {extra:?} failed: {out:?}");
        String::from_utf8(out.stdout).expect("utf8")
    };

    let plain = run(&[]);
    let pinned = run(&["--pinned"]);

    // The defect, stated as the thing that must not be true again.
    assert_ne!(
        plain, pinned,
        "`--pinned` that resolved nothing must not be byte-identical to plain \
         `--infer` (#505)"
    );
    // Per spoke: what it resolved against, and that the answer was not its own.
    assert!(
        pinned.contains("@ HEAD (no pin detected)"),
        "each unpinned spoke must name what it fell back to: {pinned}"
    );
    // And once in summary, so an operator scanning the tail sees it without
    // reading every row — which is where an inert `--pinned` is cheapest to catch.
    assert!(
        pinned.contains("0 of 1 spoke(s) pinned a hub version; 1 resolved against the hub's HEAD"),
        "the summary must count the pins that were found: {pinned}"
    );
    // Plain `--infer` is unchanged: it never claimed to resolve per spoke, so it
    // has nothing to report and must not grow a line saying so.
    assert!(
        !plain.contains("HEAD"),
        "plain `--infer` must be untouched by this: {plain}"
    );

    std::fs::remove_dir_all(&base).ok();
}

/// #535 review (Copilot): `spokes_pinned` counted `hub_rev.is_some()` across every
/// spoke, but `resolve_infer_report` also sets `hub_rev` for the **global**
/// `--hub-rev` case, where no spoke pinned anything. So a plain
/// `--infer --hub-rev <rev> --json` reported `"pinned": false` beside a non-zero
/// `spokes_pinned` — a machine reader could not tell *"seven spokes pinned their
/// own hub version"* from *"all seven got the one rev you named"*.
///
/// That is #505's defect in the field added to fix #505, so it is asserted from
/// both ends: the count is **absent** when nobody was asked, and **present** when
/// they were. Absent rather than `0`, following #410's `check` tool — `0` beside
/// `pinned: false` still reads as "we looked and none pinned", which is a
/// different claim from "we did not ask".
#[test]
fn spokes_pinned_is_absent_unless_the_spokes_were_actually_asked() {
    let base = std::env::temp_dir().join(format!("roteiro-hubrev-count-{}", std::process::id()));
    std::fs::remove_dir_all(&base).ok();
    let app = base.join("app");
    let web = base.join("web");
    std::fs::create_dir_all(&app).expect("mkdir app");
    std::fs::create_dir_all(&web).expect("mkdir web");

    // Hub v1 defines `serve.tools`; v2 renames it. `v1` is the rev named below.
    std::fs::write(app.join("config.toml"), "[serve]\ntools = true\n").expect("write");
    git(&app, &["init", "-q"]);
    git(&app, &["add", "."]);
    git(&app, &["commit", "-q", "-m", "v1"]);
    let v1 = head_sha(&app);
    assert!(roteiro(&app, &["sync"]).status.success(), "app v1 sync");
    std::fs::write(app.join("config.toml"), "[serve]\nfeatures = true\n").expect("write");
    git(&app, &["commit", "-aqm", "v2"]);
    assert!(roteiro(&app, &["sync"]).status.success(), "app v2 sync");

    // A spoke that pins nothing of its own: no `.gitmodules`, no Dockerfile.
    std::fs::write(web.join("prod.env"), "SERVE_TOOLS=true\n").expect("write");
    git(&web, &["init", "-q"]);
    git(&web, &["add", "."]);
    git(&web, &["commit", "-q", "-m", "spoke"]);
    assert!(roteiro(&web, &["sync"]).status.success(), "web sync");

    let base_s = base.to_str().unwrap();
    let run_json = |extra: &[&str]| -> serde_json::Value {
        let mut args = vec!["links", "--infer", "--hub", "app", "--workspace", base_s];
        args.extend_from_slice(extra);
        args.push("--json");
        let out = roteiro(&base, &args);
        assert!(out.status.success(), "infer {extra:?} failed: {out:?}");
        serde_json::from_slice(&out.stdout).expect("JSON")
    };

    // A global `--hub-rev`: every spoke carries that rev, none of them pinned it.
    let global = run_json(&["--hub-rev", &v1]);
    assert_eq!(
        global["pinned"], false,
        "`--pinned` was not passed: {global}"
    );
    assert_eq!(
        global["spokes"][0]["hub_rev"], v1,
        "the spoke still resolves against the named rev — resolution is unchanged: {global}"
    );
    assert!(
        global.get("spokes_pinned").is_none(),
        "`spokes_pinned` must be absent when no spoke was asked to pin — a count \
         here answers a question nobody asked: {global}"
    );

    // `--pinned`: the spokes *were* asked, so the count is present and is the
    // honest zero. Absence and zero must not be the same document.
    let asked = run_json(&["--pinned"]);
    assert_eq!(asked["pinned"], true, "`--pinned` was passed: {asked}");
    assert_eq!(
        asked["spokes_pinned"], 0,
        "asked and none pinned is a real `0`, not an absence: {asked}"
    );

    std::fs::remove_dir_all(&base).ok();
}

/// The human-readable half of the same line. `--hub-rev` is reachable only
/// without `--pinned` (clap rejects the pair), so this line was never *wrong* —
/// but "pinned version" alone does not say which of the command's two pinning
/// senses it means, and naming the source is what the rest of #505 does.
#[test]
fn the_hub_rev_line_names_whose_pin_it_resolved_against() {
    let base = std::env::temp_dir().join(format!("roteiro-hubrev-text-{}", std::process::id()));
    std::fs::remove_dir_all(&base).ok();
    let app = base.join("app");
    let web = base.join("web");
    std::fs::create_dir_all(&app).expect("mkdir app");
    std::fs::create_dir_all(&web).expect("mkdir web");

    std::fs::write(app.join("config.toml"), "[serve]\ntools = true\n").expect("write");
    git(&app, &["init", "-q"]);
    git(&app, &["add", "."]);
    git(&app, &["commit", "-q", "-m", "v1"]);
    let v1 = head_sha(&app);
    assert!(roteiro(&app, &["sync"]).status.success(), "app sync");

    std::fs::write(web.join("prod.env"), "SERVE_TOOLS=true\n").expect("write");
    git(&web, &["init", "-q"]);
    git(&web, &["add", "."]);
    git(&web, &["commit", "-q", "-m", "spoke"]);
    assert!(roteiro(&web, &["sync"]).status.success(), "web sync");

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
        ],
    );
    assert!(out.status.success(), "hub-rev infer failed: {out:?}");
    let text = String::from_utf8(out.stdout).expect("utf8");
    assert!(
        text.contains("pinned by --hub-rev, not by the spokes"),
        "the line must say whose pin the rev came from: {text}"
    );

    std::fs::remove_dir_all(&base).ok();
}

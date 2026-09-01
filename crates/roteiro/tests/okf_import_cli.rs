//! End-to-end tests for `roteiro import --from okf` (issue #706, ADR-0021):
//! another repository's OKF bundle becomes **external** knowledge, an
//! `extref:` placeholder gains real content, and a re-run stays repeatable in
//! both directions — no duplicates, and no accumulation of concepts the peer
//! has since withdrawn.

use std::fmt::Write as _;
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
            "tag.gpgsign=false",
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

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(path, text).expect("write");
}

/// A concept document as a peer's bundle would carry it.
fn concept(type_: &str, title: &str, verified_by: Option<&str>, body: &str) -> String {
    let mut out = format!("---\ntype: \"{type_}\"\ntitle: \"{title}\"\n");
    out.push_str("generated:\n  by: \"roteiro/5.0.0\"\n  at: \"2026-09-01T10:00:00Z\"\n");
    if let Some(by) = verified_by {
        let _ = write!(
            out,
            "verified:\n  - by: \"{by}\"\n    at: \"2026-09-01T10:00:00Z\"\n"
        );
    }
    out.push_str("---\n\n");
    out.push_str(body);
    out
}

/// A repo with a bundle from a peer sitting beside it (outside the repo, since a
/// bundle inside it would be extracted as this repo's own markdown).
fn repo_with_bundle(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let base = std::env::temp_dir().join(format!("roteiro-okf-{tag}-{}", std::process::id()));
    std::fs::remove_dir_all(&base).ok();
    let repo = base.join("repo");
    let bundle = base.join("peer-okf");
    std::fs::create_dir_all(&repo).expect("mkdir repo");

    write(&repo.join("src/lib.rs"), "pub struct Widget;\n");
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "init"]);

    write(
        &bundle.join("index.md"),
        "---\nokf_version: \"0.2\"\n---\n\n# Peer\n",
    );
    write(
        &bundle.join("decisions/adr-0001.md"),
        &concept(
            "adr",
            "Use event sourcing",
            Some("human:alice"),
            "# Use event sourcing\n\nThe peer's decision text.\n\n\
             ## Relationships\n\n### references\n\n* \u{2192} [d](/docs/design.md)\n",
        ),
    );
    write(
        &bundle.join("docs/design.md"),
        &concept("doc", "Design note", None, "# Design note\n\nA guess.\n"),
    );
    (repo, bundle)
}

/// The default is **acknowledge**: a hand-run import takes the peer's
/// information without their confirmation, and `--trust` is what adopts it.
///
/// Defaulting the other way would mean one command silently promoting a
/// stranger's `verified: [{ by: human:… }]` into this graph's human-reviewed
/// tier — and re-emitting it outward on the next `render okf` — which is the
/// consent question #706's decision exists to ask.
#[test]
fn the_default_is_acknowledge_and_trust_preserves_the_peers_tier() {
    let (repo, bundle) = repo_with_bundle("trust");
    let bundle_s = bundle.to_str().unwrap();

    let out = roteiro(&repo, &["import", "--from", "okf", bundle_s, "--json"]);
    assert!(out.status.success(), "import failed: {out:?}");
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");
    assert_eq!(report["trust"], "acknowledge");
    assert_eq!(report["peer"], "peer-okf", "peer defaults to the dir name");
    assert_eq!(report["src_ref"], "import:okf/peer-okf");
    assert_eq!(report["concepts_read"], 2);
    assert_eq!(
        report["concepts_by_provenance"]["external-inferred"], 2,
        "acknowledge lands everything at external-inferred: {report}"
    );

    let key = "okf:peer-okf/decisions/adr-0001.md";
    let q = roteiro(&repo, &["query", key, "--json"]);
    assert!(q.status.success(), "query failed: {q:?}");
    let node: serde_json::Value = serde_json::from_slice(&q.stdout).expect("JSON");
    assert_eq!(node["node"]["name"], "Use event sourcing");
    assert_eq!(
        node["meta"]["okf"]["claimed"]["tier"], "authored",
        "what they claimed is still recorded, as data rather than as provenance"
    );

    // Re-run with --trust: the tiers they claimed are adopted, and each one is
    // carried separately. A flat `External` would collapse these into one.
    let out = roteiro(
        &repo,
        &["import", "--from", "okf", bundle_s, "--trust", "--json"],
    );
    assert!(out.status.success(), "trusted import failed: {out:?}");
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");
    assert_eq!(report["trust"], "trust");
    assert_eq!(
        report["concepts_by_provenance"]["external-authored"], 1,
        "Alice's confirmation is preserved as *hers*: {report}"
    );
    assert_eq!(
        report["concepts_by_provenance"]["external-inferred"], 1,
        "and the unverified concept is not upgraded with it: {report}"
    );

    // The edge between the two imported concepts came across, and carries the
    // source concept's external tier with no confidence attached.
    let q = roteiro(&repo, &["query", key, "--json"]);
    let node: serde_json::Value = serde_json::from_slice(&q.stdout).expect("JSON");
    let outgoing = node["outgoing"].as_array().expect("outgoing");
    assert_eq!(outgoing.len(), 1, "{node}");
    assert_eq!(outgoing[0]["node"], "okf:peer-okf/docs/design.md");
    assert_eq!(outgoing[0]["provenance"], "external-authored");
    assert_eq!(
        outgoing[0]["confidence"],
        serde_json::Value::Null,
        "an imported edge carries no confidence this graph never computed"
    );

    std::fs::remove_dir_all(bundle.parent().expect("base")).ok();
}

/// A re-run must be repeatable in **both** directions. Duplicates were already
/// impossible; what was not is removal — a concept the peer has since deleted
/// used to survive as an orphan carrying `external-*` provenance, so a re-run
/// looked idempotent while quietly accumulating withdrawn facts.
#[test]
fn re_importing_a_smaller_bundle_removes_the_withdrawn_concept() {
    let (repo, bundle) = repo_with_bundle("withdraw");
    let bundle_s = bundle.to_str().unwrap();

    assert!(
        roteiro(&repo, &["import", "--from", "okf", bundle_s])
            .status
            .success()
    );
    let present = |key: &str| roteiro(&repo, &["query", key, "--json"]).status.success();
    assert!(present("okf:peer-okf/decisions/adr-0001.md"));
    assert!(present("okf:peer-okf/docs/design.md"));

    // The peer withdraws a concept, and drops the link to it with it.
    std::fs::remove_file(bundle.join("docs/design.md")).expect("delete concept");
    write(
        &bundle.join("decisions/adr-0001.md"),
        &concept(
            "adr",
            "Use event sourcing",
            Some("human:alice"),
            "# Use event sourcing\n\nThe peer's decision text.\n",
        ),
    );

    let out = roteiro(&repo, &["import", "--from", "okf", bundle_s, "--json"]);
    assert!(out.status.success(), "re-import failed: {out:?}");
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");
    assert_eq!(report["concepts_read"], 1);
    assert_eq!(
        report["nodes_removed"], 1,
        "the withdrawn concept must be removed, not left as an orphan: {report}"
    );
    assert!(
        !present("okf:peer-okf/docs/design.md"),
        "a concept the peer deleted must not survive a re-import"
    );
    assert!(
        present("okf:peer-okf/decisions/adr-0001.md"),
        "and one still asserted must"
    );

    // Genuinely idempotent now: running it again removes nothing further.
    let out = roteiro(&repo, &["import", "--from", "okf", bundle_s, "--json"]);
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");
    assert_eq!(report["nodes_removed"], 0);
    assert_eq!(report["concepts_read"], 1);

    std::fs::remove_dir_all(bundle.parent().expect("base")).ok();
}

/// A two-repo workspace where `chart` declares a `[[links]]` reference into
/// `app`, written through so the `extref:` placeholder is really in the store —
/// plus the (empty) directory `app`'s bundle will be written into.
///
/// Returns `(workspace base, chart repo, bundle dir)`. Mirrors `links_cli.rs`'s
/// fixture of the same name, because the placeholder this test fills is the one
/// that test's mechanism creates.
fn declared_link_workspace() -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let base = std::env::temp_dir().join(format!("roteiro-okf-extref-{}", std::process::id()));
    std::fs::remove_dir_all(&base).ok();
    let app = base.join("app");
    let chart = base.join("chart");
    let bundle = base.join("app-okf");
    std::fs::create_dir_all(&app).expect("mkdir app");
    std::fs::create_dir_all(&chart).expect("mkdir chart");

    write(&app.join("config.toml"), "[batch]\nmax_bytes = 1048576\n");
    git(&app, &["init", "-q"]);
    git(&app, &["add", "."]);
    git(&app, &["commit", "-q", "-m", "init"]);
    assert!(roteiro(&app, &["sync"]).status.success(), "app sync");

    write(&chart.join("values.yaml"), "batch:\n  max_bytes: 2097152\n");
    write(
        &chart.join("roteiro.toml"),
        "[[links]]\nfrom = \"cfgkey:values.yaml#batch.max_bytes\"\n\
         to = \"app::cfgkey:config.toml#batch.max_bytes\"\nkind = \"references\"\n",
    );
    git(&chart, &["init", "-q"]);
    git(&chart, &["add", "."]);
    git(&chart, &["commit", "-q", "-m", "init"]);
    assert!(roteiro(&chart, &["sync"]).status.success(), "chart sync");

    let base_s = base.to_str().unwrap();
    let wrote = roteiro(&base, &["links", "--workspace", base_s, "--write"]);
    assert!(wrote.status.success(), "links --write failed: {wrote:?}");

    (base, chart, bundle)
}

/// The payoff (ADR-0009): an `extref:` placeholder holds a key and nothing else,
/// so a cross-repo reference resolves to a document whose whole content is that
/// it is not the document you wanted. An imported concept that corresponds to
/// one **fills it** — and stays a placeholder, because the workspace resolver
/// still follows `meta.qualified` across repos.
#[test]
fn an_imported_concept_fills_a_cross_repo_placeholder() {
    let (base, chart, bundle) = declared_link_workspace();

    // The placeholder exists, and is exactly as empty as ADR-0009 leaves it.
    let stub = "extref:app::cfgkey:config.toml#batch.max_bytes";
    let before: serde_json::Value =
        serde_json::from_slice(&roteiro(&chart, &["query", stub, "--json"]).stdout).expect("JSON");
    assert_eq!(
        before["meta"]["qualified"],
        "app::cfgkey:config.toml#batch.max_bytes"
    );
    assert!(
        before["meta"].get("content").is_none(),
        "a placeholder starts with no content at all: {before}"
    );

    // `app` publishes a bundle. The filename is the writer's own rule applied to
    // the target key — `slug("cfgkey:config.toml#batch.max_bytes")` — under the
    // section `section_for("cfgkey")` chooses, which is how the correspondence is
    // computed forwards rather than by inverting a lossy slug.
    write(
        &bundle.join("index.md"),
        "---\nokf_version: \"0.2\"\n---\n\n# App\n",
    );
    write(
        &bundle.join("symbols/cfgkey-config-toml-batch-max-bytes.md"),
        &concept(
            "cfgkey",
            "batch.max_bytes",
            Some("human:alice"),
            "# batch.max_bytes\n\nThe write batch ceiling, in bytes.\n",
        ),
    );

    let out = roteiro(
        &chart,
        &[
            "import",
            "--from",
            "okf",
            bundle.to_str().unwrap(),
            "--peer",
            "app",
            "--trust",
            "--json",
        ],
    );
    assert!(out.status.success(), "import failed: {out:?}");
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");
    assert_eq!(
        report["extrefs_filled"],
        serde_json::json!([[stub, "/symbols/cfgkey-config-toml-batch-max-bytes.md"]]),
        "the placeholder must be the node that gained the content: {report}"
    );

    let after: serde_json::Value =
        serde_json::from_slice(&roteiro(&chart, &["query", stub, "--json"]).stdout).expect("JSON");
    assert!(
        after["meta"]["content"]
            .as_str()
            .is_some_and(|c| c.contains("The write batch ceiling")),
        "the stub now carries the peer's real content: {after}"
    );
    assert_eq!(
        after["meta"]["qualified"], "app::cfgkey:config.toml#batch.max_bytes",
        "and is still a placeholder the workspace resolver can follow"
    );
    assert_eq!(after["node"]["name"], "batch.max_bytes");

    // The authored `[[links]]` edge into it survives: filling a stub must not
    // disturb the link it exists to serve.
    let incoming = after["incoming"].as_array().expect("incoming");
    assert!(
        incoming.iter().any(|e| e["provenance"] == "authored"),
        "the declared cross-repo link must still point at it: {after}"
    );

    // And the fill survives a rebuild. `reapply_imports` re-upserts every
    // layer's nodes in `src_ref` order, and `import:links` contributes the *bare*
    // stub under the same key — so a sync would reset the fill to an empty
    // placeholder if the OKF layer did not sort after it. That ordering is a
    // dependency on the ref's spelling, not a coincidence, and this is what
    // notices if it is ever renamed.
    write(&chart.join("values.yaml"), "batch:\n  max_bytes: 4194304\n");
    git(&chart, &["commit", "-qam", "raise the ceiling"]);
    assert!(roteiro(&chart, &["sync"]).status.success(), "resync");

    let synced: serde_json::Value =
        serde_json::from_slice(&roteiro(&chart, &["query", stub, "--json"]).stdout).expect("JSON");
    assert!(
        synced["meta"]["content"]
            .as_str()
            .is_some_and(|c| c.contains("The write batch ceiling")),
        "the fill must survive a rebuild, not be reset to a bare stub: {synced}"
    );
    assert_eq!(
        synced["meta"]["qualified"], "app::cfgkey:config.toml#batch.max_bytes",
        "and still resolve across repos afterwards"
    );

    std::fs::remove_dir_all(&base).ok();
}

/// A partly readable bundle says what it skipped. Silence would leave the graph
/// missing concepts nobody knows to look for; refusing the whole bundle would
/// break §11's instruction to consumers to be liberal.
#[test]
fn a_partly_readable_bundle_reports_its_skips_and_a_hopeless_one_is_refused() {
    let (repo, bundle) = repo_with_bundle("skips");
    let bundle_s = bundle.to_str().unwrap();
    write(&bundle.join("docs/plain.md"), "# Not a concept\n");
    write(
        &bundle.join("docs/typeless.md"),
        "---\ntitle: \"x\"\n---\n\nB\n",
    );

    let out = roteiro(&repo, &["import", "--from", "okf", bundle_s]);
    assert!(out.status.success(), "import failed: {out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("skipped /docs/plain.md: no YAML frontmatter block"),
        "{text}"
    );
    assert!(
        text.contains("skipped /docs/typeless.md: no non-empty `type` (OKF's one required key)"),
        "{text}"
    );

    // A directory in which nothing parsed is not a bundle read badly.
    let empty = bundle.parent().expect("base").join("not-a-bundle");
    write(&empty.join("a.md"), "# nothing here\n");
    let out = roteiro(&repo, &["import", "--from", "okf", empty.to_str().unwrap()]);
    assert!(
        !out.status.success(),
        "a non-bundle must be refused: {out:?}"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("no readable concept among them, so it is not an OKF bundle"),
        "{err}"
    );

    std::fs::remove_dir_all(bundle.parent().expect("base")).ok();
}

/// `--trust` decides how much of a *peer's* claim to adopt, so it means nothing
/// to the other sources. Accepting it silently would let `--from lat --trust`
/// read as though it did something.
#[test]
fn the_okf_only_flags_are_refused_on_another_source() {
    let (repo, bundle) = repo_with_bundle("flags");
    let out = roteiro(&repo, &["import", "--from", "lat", "lat.md", "--trust"]);
    assert!(!out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr).trim(),
        "Error: `--trust` and `--peer` apply to `--from okf` only, not `lat`"
    );

    let out = roteiro(&repo, &["import", "--from", "nope", "x"]);
    assert!(!out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr).trim(),
        "Error: unknown import source `nope` (expected: graphify | lat | okf | codegraph)"
    );

    std::fs::remove_dir_all(bundle.parent().expect("base")).ok();
}

/// The round trip out again: a trusted concept leaves this repository's bundle
/// carrying **the peer's own** `verified:` block, naming their verifier.
///
/// This is a wiring test and it has to be. The reader's unit tests prove
/// `peer_origin` recovers the right origin, and the renderer's prove
/// `origin_for` maps each provenance correctly — and **both stayed green** while
/// `render okf` ignored the recovered origin entirely, because neither of them
/// runs the line that chooses between the two. Found by fault injection; this is
/// the guard that closes it.
///
/// What it forbids is the failure the carried tier exists to prevent, arriving
/// one step later than the enum: Alice confirmed the concept in her repository,
/// and re-rendering it as `generated:` alone would tell the next consumer that
/// nobody confirmed it — or, with the confirmer defaulted to our own tool, that
/// *we* did. Both are laundering by round-trip.
#[test]
fn a_trusted_concept_re_emits_the_peers_verifier_by_name() {
    let (repo, bundle) = repo_with_bundle("render");
    let out = roteiro(
        &repo,
        &[
            "import",
            "--from",
            "okf",
            bundle.to_str().unwrap(),
            "--trust",
        ],
    );
    assert!(out.status.success(), "import failed: {out:?}");

    let rendered = repo.join("out");
    let render = roteiro(
        &repo,
        &["render", "okf", "--out", rendered.to_str().unwrap()],
    );
    assert!(render.status.success(), "render failed: {render:?}");

    // `slug("okf:peer-okf/decisions/adr-0001.md")` under `section_for("adr")`.
    let concept_path = rendered.join("decisions/okf-peer-okf-decisions-adr-0001-md.md");
    let text = std::fs::read_to_string(&concept_path)
        .unwrap_or_else(|e| panic!("no rendered concept at {}: {e}", concept_path.display()));

    assert!(
        text.contains("verified:\n  - by: \"human:alice\""),
        "the peer's verifier must leave by name, not be re-tiered on the way out: {text}"
    );
    assert!(
        !text.contains("verified:\n  - by: \"roteiro/"),
        "this graph must not sign off on a peer's concept as though it confirmed it: {text}"
    );

    // And an *acknowledged* concept must not gain a confirmation on the way out:
    // declining their confirmation on the way in has to survive the round trip,
    // or the decline was cosmetic.
    let out = roteiro(
        &repo,
        &["import", "--from", "okf", bundle.to_str().unwrap()],
    );
    assert!(out.status.success(), "re-import failed: {out:?}");
    let render = roteiro(
        &repo,
        &["render", "okf", "--out", rendered.to_str().unwrap()],
    );
    assert!(render.status.success(), "render failed: {render:?}");
    let text = std::fs::read_to_string(&concept_path).expect("rendered concept");
    assert!(
        !text.contains("verified:"),
        "an acknowledged concept claims no confirmation, here or downstream: {text}"
    );
    assert!(
        text.contains("generated:\n  by: \"human:alice\""),
        "their attribution is still recorded — declined, not erased: {text}"
    );

    std::fs::remove_dir_all(bundle.parent().expect("base")).ok();
}

/// A bundle is somebody **else's** directory, so the walk that reads it does not
/// follow symlinks.
///
/// Two failures, one guard. A link pointing at an ancestor makes `is_dir()` —
/// which follows links — recurse for ever; a link pointing outside the bundle
/// pulls in files that are not part of it and imports them as the peer's
/// concepts. Nothing legitimate is lost: a bundle is a directory of documents.
#[test]
#[cfg(unix)]
fn the_bundle_walk_does_not_follow_symlinks() {
    let (repo, bundle) = repo_with_bundle("symlink");

    // A loop: `decisions/loop` -> the bundle root.
    std::os::unix::fs::symlink(&bundle, bundle.join("decisions/loop")).expect("symlink loop");
    // And an escape: a concept living outside the bundle entirely.
    let outside = bundle.parent().expect("base").join("outside");
    write(
        &outside.join("secret.md"),
        &concept("adr", "Not theirs", Some("human:mallory"), "# Not theirs\n"),
    );
    std::os::unix::fs::symlink(&outside, bundle.join("escape")).expect("symlink escape");

    let out = roteiro(
        &repo,
        &[
            "import",
            "--from",
            "okf",
            bundle.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(out.status.success(), "import failed: {out:?}");
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");
    assert_eq!(
        report["concepts_read"], 2,
        "only the bundle's own two concepts, and the walk terminated: {report}"
    );
    assert!(
        !roteiro(
            &repo,
            &["query", "okf:symlink-okf/escape/secret.md", "--json"]
        )
        .status
        .success(),
        "a document reached only through a symlink out of the bundle is not in it"
    );

    std::fs::remove_dir_all(bundle.parent().expect("base")).ok();
}

/// The peer names the import layer and namespaces every imported key, so a blank
/// one would give `import:okf/` and `okf:/…` — a layer nobody can name and keys
/// that collide with the next unnamed import.
#[test]
fn an_unusable_peer_name_is_refused_rather_than_namespacing_nothing() {
    let (repo, bundle) = repo_with_bundle("peername");
    for name in ["", "  ", ".", ".."] {
        let out = roteiro(
            &repo,
            &[
                "import",
                "--from",
                "okf",
                bundle.to_str().unwrap(),
                "--peer",
                name,
            ],
        );
        assert!(!out.status.success(), "`{name}` must be refused: {out:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stderr).trim(),
            format!(
                "Error: `{name}` is not a usable peer name: it names the import layer and \
                 namespaces every imported concept's key. Pass --peer <NAME>."
            )
        );
    }
    std::fs::remove_dir_all(bundle.parent().expect("base")).ok();
}

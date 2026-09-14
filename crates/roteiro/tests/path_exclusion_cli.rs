// roteiro:ignore-file — the fixtures below deliberately embed a marker word in
// ordinary English prose so the marker scan fires on them; they are test data,
// not real debt in this repository.
//! End-to-end test for `[paths] exclude` / `[paths] opaque` (ADR-0007 v1.9,
//! issue #840, ADR-0026 step 1): **three** states, applied at **every** reader,
//! and reaching `export` because the node is never stored rather than filtered
//! out of a report.
//!
//! # The fixture contains the real defect
//!
//! Two live defects fire on `manifest/papers.json` with no declaration present,
//! and the first half of every test here asserts that they *do* — a test of an
//! exclusion that never sees the thing being excluded is a test of nothing:
//!
//! * **Issue #839** — `config_key` extraction is keyed on file extension alone,
//!   so a `.json` data manifest is mined as configuration, one node per JSON
//!   leaf.
//! * **Issue #838** — marker scanning has no arm for data, so a `.json` file is
//!   treated as prose and scanned in full with the noisy phrase rules armed.
//!   `"Long-Term Follow-Up of Ventricular Arrhythmias"` — a real cardiology
//!   paper title — matches the `follow-up` DEFERRED rule.
//!
//! Neither defect is fixed here, deliberately: this is the mechanism that makes
//! them declarable, not a substitute for fixing them.
//!
//! # And it contains the two-scan hazard
//!
//! `raw/paper.md` declares `type: adr`, which the authored classifier honours
//! **wherever the file sits** — that rule is what lets a repository keep its
//! decisions outside `docs/adr/`, and it is exactly what makes an ingested
//! third-party document dangerous. The authored layer is a *second*, independent
//! reader of committed blobs (issue #817): excluding a path from extraction does
//! not exclude it from there, and an exclusion that reached only one of the two
//! would pass every assertion about `config_key` nodes while still parsing a
//! stranger's paper as one of this project's architecture decisions.

use std::path::{Path, PathBuf};
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
        // Isolate from any real user config: the user layer is a real layer of
        // ADR-0007's precedence, so a developer's own `~/.roteiro/config.toml`
        // would otherwise be an input to this test.
        .env("ROTEIRO_HOME", dir)
        .output()
        .expect("run roteiro")
}

fn write(dir: &Path, rel: &str, content: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir");
    std::fs::write(path, content).expect("write");
}

fn fresh_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("roteiro-paths-cli-{}-{name}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

/// A manifest of downloaded papers: nested JSON objects that `flatten_json`
/// mines into one `config_key` node per leaf (#839), carrying a paper title
/// whose ordinary English trips the `follow-up` marker rule (#838).
const MANIFEST: &str = r#"{
  "downloaded_by_host": {
    "www.example.edu": 3
  },
  "papers": {
    "10.1000/example": {
      "title": "Machine Learning of Remodeled Ventricular Action Potentials and Long-Term Follow-Up of Ventricular Arrhythmias",
      "sha256": "0000000000000000000000000000000000000000000000000000000000000000"
    }
  }
}
"#;

/// A third-party document that **declares itself one of our ADRs**, and carries
/// an annotation pointing at one. Both are content claims, honoured by the
/// authored classifier wherever the file sits.
fn foreign_doc(id: &str) -> String {
    format!(
        "---\n\
         type: adr\n\
         adr-id: \"{id}\"\n\
         status: Accepted\n\
         ---\n\
         \n\
         # ADR-{id}: A paper somebody downloaded\n\
         \n\
         ## Decision\n\
         \n\
         Prose from a source document.\n"
    )
}

/// This repository's own ADR — the **negative control** for the authored
/// reader. Whatever the policy excludes, this must still parse.
const OUR_ADR: &str = "---\n\
                       adr-id: \"0001\"\n\
                       status: Accepted\n\
                       ---\n\
                       \n\
                       # ADR-0001: Thing\n\
                       \n\
                       ## Decision\n\
                       \n\
                       The design centres on [[src/lib.rs#Thing]].\n";

/// Build the fixture repository. `config` is written as `roteiro.toml` when
/// given; with `None` the repository declares nothing, which is the baseline the
/// exclusion is measured against.
fn fixture(name: &str, config: Option<&str>) -> PathBuf {
    let dir = fresh_dir(name);
    git(&dir, &["init", "-q"]);
    // Ordinary source, with an ordinary marker in an ordinary comment. Nothing
    // about it is excluded, and every assertion about it must be identical on
    // both sides of the declaration.
    write(
        &dir,
        "src/lib.rs",
        "// TODO: wire this up\npub struct Thing;\n",
    );
    write(&dir, "docs/adr/0001-thing.md", OUR_ADR);
    write(&dir, "manifest/papers.json", MANIFEST);
    // Distinct ids: two documents claiming one ADR id is a different defect
    // (a duplicate-id violation), and it would mask what this fixture measures.
    write(&dir, "manifest/decision.md", &foreign_doc("0043"));
    write(&dir, "raw/paper.md", &foreign_doc("0042"));
    if let Some(config) = config {
        write(&dir, "roteiro.toml", config);
    }
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    dir
}

/// The exported graph — the surface issue #840 is about. `[debt] ignore` mutes
/// `roteiro debt` and leaves the node here; an exclusion that removes the node
/// cannot.
fn export(dir: &Path) -> serde_json::Value {
    let out = roteiro(dir, &["export", "--out", "-"]);
    assert!(out.status.success(), "export failed: {out:?}");
    serde_json::from_slice(&out.stdout).expect("export --out - is valid JSON")
}

/// Every node in the artifact, as `(key, kind, path)`.
fn nodes(artifact: &serde_json::Value) -> Vec<(String, String, String)> {
    artifact["facts"]["nodes"]
        .as_array()
        .expect("nodes array")
        .iter()
        .map(|n| {
            (
                n["key"].as_str().unwrap_or_default().to_owned(),
                n["kind"].as_str().unwrap_or_default().to_owned(),
                n["path"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

fn keys_under(artifact: &serde_json::Value, prefix: &str) -> Vec<String> {
    nodes(artifact)
        .into_iter()
        .filter(|(_, _, path)| path.starts_with(prefix))
        .map(|(key, _, _)| key)
        .collect()
}

fn node<'a>(artifact: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    artifact["facts"]["nodes"]
        .as_array()
        .expect("nodes array")
        .iter()
        .find(|n| n["key"].as_str() == Some(key))
}

const DECLARED: &str = "[paths]\nexclude = [\"raw/**\"]\nopaque = [\"manifest/**\"]\n";

/// **The declaration must survive the extraction cache.**
///
/// Extraction is cached by `(path, blob id, env)`, and nothing about the *blob*
/// changes when a repository adds a `[paths]` line — the file is byte-identical
/// and its git oid is the same. So unless the policy is part of `env`, the
/// second `sync` is a cache hit and serves back precisely the `config_key` nodes
/// and the fabricated marker the declaration was written to remove, while
/// reporting itself up to date. The graph would then disagree with the
/// configuration with nothing to indicate why — a *silent* wrong answer, which is
/// the failure class this repository keeps closing.
///
/// This is the test that makes `PathPolicy::fingerprint`'s presence in
/// `Extractor::env_tag` a guarantee rather than an argument. It runs both
/// directions: declaring must drop the mined nodes, and **un**-declaring must
/// bring them back, because a one-way check passes just as happily against an
/// extractor that has stopped caching at all.
#[test]
fn declaring_a_path_invalidates_the_extraction_cache_that_holds_its_old_facts() {
    let dir = fixture("cache", None);

    // First sync populates the content-addressed cache with the mined facts.
    let before = export(&dir);
    let mined: Vec<String> = nodes(&before)
        .into_iter()
        .filter(|(key, _, _)| key.starts_with("cfgkey:manifest/papers.json#"))
        .map(|(key, _, _)| key)
        .collect();
    assert!(
        !mined.is_empty(),
        "the first run must mine the manifest, or the cache has nothing stale to serve"
    );
    assert!(
        nodes(&before)
            .iter()
            .any(|(_, kind, path)| kind == "marker" && path.starts_with("manifest/")),
        "and must fabricate the marker"
    );

    // Declare it opaque. The blob is untouched: same bytes, same git oid, so the
    // cache key moves only if the policy is part of the extraction identity.
    write(&dir, "roteiro.toml", DECLARED);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "declare"]);
    let after = export(&dir);
    assert!(
        nodes(&after)
            .iter()
            .all(|(key, _, _)| !key.starts_with("cfgkey:manifest/")),
        "a cache hit here would serve back the very nodes the declaration removes: {:?}",
        keys_under(&after, "manifest/")
    );
    assert!(
        nodes(&after)
            .iter()
            .all(|(_, kind, path)| !(kind == "marker" && path.starts_with("manifest/"))),
        "and the marker with them"
    );
    assert!(
        node(&after, "file:manifest/papers.json").is_some(),
        "while the opaque file node itself is still there"
    );

    // And back again: withdrawing the declaration must restore what it removed,
    // which a merely-broken cache would also appear to do — so this direction is
    // what distinguishes "the key moved" from "nothing is cached any more".
    write(&dir, "roteiro.toml", "[paths]\nexclude = [\"raw/**\"]\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "withdraw"]);
    let restored = export(&dir);
    let again: Vec<String> = nodes(&restored)
        .into_iter()
        .filter(|(key, _, _)| key.starts_with("cfgkey:manifest/papers.json#"))
        .map(|(key, _, _)| key)
        .collect();
    assert_eq!(
        again, mined,
        "withdrawing the declaration restores exactly what it removed"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// **The baseline.** Everything the declaration removes is present without it —
/// asserted first, and in one place, because every other test in this file is
/// worthless if the fixture does not actually reproduce the defects.
#[test]
fn without_a_declaration_the_manifest_is_mined_and_the_foreign_doc_is_one_of_ours() {
    let dir = fixture("baseline", None);
    let artifact = export(&dir);

    // #839: one `config_key` node per JSON leaf, from a file that is data.
    let cfgkeys: Vec<_> = nodes(&artifact)
        .into_iter()
        .filter(|(key, _, _)| key.starts_with("cfgkey:manifest/papers.json#"))
        .collect();
    assert_eq!(
        cfgkeys.len(),
        3,
        "the manifest must be mined as configuration without a declaration, \
         or this fixture does not contain the defect: {cfgkeys:?}"
    );

    // #838: a paper title, scanned as prose, becomes an intent-debt marker.
    let markers: Vec<_> = nodes(&artifact)
        .into_iter()
        .filter(|(key, kind, _)| kind == "marker" && key.contains("manifest/papers.json"))
        .collect();
    assert_eq!(
        markers.len(),
        1,
        "a marker word in a paper title must fabricate a finding without a \
         declaration, or this fixture does not contain the defect: {markers:?}"
    );

    // The second reader: a downloaded document parsed as one of our decisions.
    let adrs: Vec<_> = nodes(&artifact)
        .into_iter()
        .filter(|(_, kind, _)| kind == "adr")
        .map(|(key, _, path)| (key, path))
        .collect();
    assert!(
        adrs.iter().any(|(_, path)| path == "raw/paper.md"),
        "a `type: adr` declaration is honoured wherever the file sits — that is \
         the hazard the exclusion must reach: {adrs:?}"
    );
    assert!(
        adrs.iter().any(|(_, path)| path == "manifest/decision.md"),
        "and under an opaque path too: {adrs:?}"
    );

    // And it is in `export`, which is the whole of issue #840's complaint.
    assert!(
        !keys_under(&artifact, "raw/").is_empty(),
        "the excluded-to-be tree is published without a declaration"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// **`exclude` means no node at all**, at both readers, and therefore in
/// `export` — which is what distinguishes this from `[debt] ignore`.
#[test]
fn an_excluded_path_is_in_no_layer_of_the_graph() {
    let dir = fixture("excluded", Some(DECLARED));
    let artifact = export(&dir);

    assert!(
        keys_under(&artifact, "raw/").is_empty(),
        "an excluded path must contribute no node of any kind: {:?}",
        keys_under(&artifact, "raw/")
    );
    assert!(
        node(&artifact, "file:raw/paper.md").is_none(),
        "not even the file node"
    );

    // Reader two, which walks every path on its own and classifies by content.
    let adrs: Vec<_> = nodes(&artifact)
        .into_iter()
        .filter(|(_, kind, _)| kind == "adr")
        .map(|(_, _, path)| path)
        .collect();
    assert_eq!(
        adrs,
        vec!["docs/adr/0001-thing.md".to_owned()],
        "an excluded document declaring `type: adr` must not be one of ours, and \
         ours must still be: {adrs:?}"
    );

    // Nothing points at it either — an annotation is an authored edge, and an
    // edge whose endpoint was removed is how a half-applied exclusion shows.
    let edges = artifact["facts"]["edges"].as_array().expect("edges array");
    assert!(
        !edges.iter().any(|e| {
            e["src"].as_str().is_some_and(|s| s.contains("raw/"))
                || e["dst"].as_str().is_some_and(|s| s.contains("raw/"))
        }),
        "no edge may name an excluded path"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// **`opaque` means a file node and nothing else** — the third state, which
/// neither `.gitignore` nor `[debt] ignore` nor `[ingest]` can express. Issue
/// #812 needs the manifest *present* so an absent `raw/` is detectable, and
/// needs it *unmined*.
#[test]
fn an_opaque_path_keeps_its_identity_and_loses_everything_derived_from_its_bytes() {
    let dir = fixture("opaque", Some(DECLARED));
    let artifact = export(&dir);

    let file = node(&artifact, "file:manifest/papers.json")
        .expect("an opaque path must still be in the graph — #812 needs it detectable");
    assert_eq!(file["kind"], "file");
    assert_eq!(
        file["meta"]["scan"], "opaque",
        "the class is recorded, so a body absent by declaration is a fact rather \
         than an absence to infer: {file}"
    );
    assert!(
        file["meta"]["content"].is_null(),
        "no content capture: a manifest must not bring every title it lists into \
         the embedding surface: {file}"
    );
    assert!(
        file["blob_hash"].as_str().is_some_and(|h| !h.is_empty()),
        "identity is kept — the hash is what ties a summary to a source version"
    );
    assert_eq!(
        file["meta"]["lines"], 11,
        "and so are the facts about the file rather than about its contents"
    );

    // #839 and #838, declared away rather than fixed.
    assert!(
        nodes(&artifact)
            .iter()
            .all(|(key, _, _)| !key.starts_with("cfgkey:manifest/")),
        "an opaque path is not mined as configuration"
    );
    assert!(
        nodes(&artifact)
            .iter()
            .all(|(_, kind, path)| !(kind == "marker" && path.starts_with("manifest/"))),
        "an opaque path is not scanned for markers"
    );

    // Reader two again: opaque refuses the content classifier exactly as
    // excluded does. Only the *file node* survives, and it is not an ADR.
    let adrs: Vec<_> = nodes(&artifact)
        .into_iter()
        .filter(|(_, kind, _)| kind == "adr")
        .map(|(_, _, path)| path)
        .collect();
    assert_eq!(adrs, vec!["docs/adr/0001-thing.md".to_owned()]);
    assert!(
        node(&artifact, "file:manifest/decision.md").is_some(),
        "an opaque markdown file is still a file"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// **The negative.** It is easy to write an exclusion that excludes too much and
/// passes every test that only looks at the excluded case. Everything outside
/// the declared patterns must be byte-identical to the baseline graph.
#[test]
fn a_path_the_policy_does_not_name_is_extracted_exactly_as_before() {
    let before = fixture("negative-before", None);
    let after = fixture("negative-after", Some(DECLARED));

    let keep = |artifact: &serde_json::Value| -> Vec<(String, String, String)> {
        let mut kept: Vec<_> = nodes(artifact)
            .into_iter()
            .filter(|(_, _, path)| {
                !path.starts_with("raw/")
                    && !path.starts_with("manifest/")
                    && path != "roteiro.toml"
            })
            .collect();
        kept.sort();
        kept
    };

    let a = export(&before);
    let b = export(&after);
    assert_eq!(
        keep(&a),
        keep(&b),
        "declaring `[paths]` must change nothing about a path it does not name"
    );

    // Named explicitly as well as compared as a set, so a future change that
    // drops *both* sides still fails rather than comparing two empty lists.
    let b_keys: Vec<_> = keep(&b).into_iter().map(|(key, _, _)| key).collect();
    for expected in [
        "file:src/lib.rs",
        "marker:src/lib.rs#1",
        "adr:0001",
        "file:docs/adr/0001-thing.md",
    ] {
        assert!(
            b_keys.iter().any(|k| k == expected),
            "`{expected}` must survive the declaration: {b_keys:?}"
        );
    }

    std::fs::remove_dir_all(&before).ok();
    std::fs::remove_dir_all(&after).ok();
}

/// **Reader three**: `media build`/`media status` walks `HEAD` on its own, so an
/// excluded or opaque blob must not become a candidate for description or
/// transcription. Describing a blob is deriving a claim from its bytes.
#[test]
fn media_candidates_are_drawn_from_the_admitted_tree_only() {
    let with_clips = |name: &str, config: Option<&str>| -> PathBuf {
        let dir = fixture(name, config);
        // Distinct bytes per file: identical content is one git blob, and
        // `media_blobs` de-duplicates by blob id, so equal files would collapse
        // into one candidate and hide the difference this test measures.
        for (rel, body) in [
            ("assets/ours.wav", "RIFFours"),
            ("manifest/clip.wav", "RIFFmanifest"),
            ("raw/clip.wav", "RIFFraw"),
        ] {
            write(&dir, rel, body);
        }
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "clips"]);
        dir
    };
    let audio_candidates = |dir: &Path| -> u64 {
        let out = roteiro(dir, &["media", "status", "--json"]);
        assert!(out.status.success(), "media status failed: {out:?}");
        let report: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("media status --json");
        report["candidates"]
            .as_array()
            .expect("candidates")
            .iter()
            .find(|c| c["kind"] == "audio")
            .and_then(|c| c["blobs"].as_u64())
            .expect("audio candidate count")
    };

    let open = with_clips("media-open", None);
    assert_eq!(
        audio_candidates(&open),
        3,
        "all three clips are candidates without a declaration"
    );
    std::fs::remove_dir_all(&open).ok();

    let declared = with_clips("media-declared", Some(DECLARED));
    assert_eq!(
        audio_candidates(&declared),
        1,
        "only the clip outside both declarations remains"
    );
    std::fs::remove_dir_all(&declared).ok();
}

/// Whether any file under `dir`, at any depth, contains `needle`.
fn contains_text(dir: &Path, needle: &str) -> bool {
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries.filter_map(Result::ok).any(|e| {
            let path = e.path();
            if path.is_dir() {
                contains_text(&path, needle)
            } else {
                std::fs::read_to_string(&path).is_ok_and(|text| text.contains(needle))
            }
        })
    })
}

/// **Reader four**: `render okf` reads prose bodies from its own walk of `HEAD`,
/// not from the store. Publishing an excluded document's text into the bundle
/// would put through `render` exactly what the declaration removed from the
/// graph — issue #840's complaint with a different surface on it.
#[test]
fn the_rendered_bundle_carries_no_prose_from_a_declared_path() {
    let body = "Prose from a source document.";
    let bundle_contains = |dir: &Path| -> bool {
        let out = roteiro(dir, &["render", "okf", "--out", "okf"]);
        assert!(out.status.success(), "render okf failed: {out:?}");
        contains_text(&dir.join("okf"), body)
    };

    let open = fixture("okf-open", None);
    assert!(
        bundle_contains(&open),
        "the foreign document's prose is published without a declaration, or \
         this test proves nothing"
    );
    std::fs::remove_dir_all(&open).ok();

    let declared = fixture("okf-declared", Some(DECLARED));
    assert!(
        !bundle_contains(&declared),
        "a declared path's prose must not reach the bundle"
    );
    std::fs::remove_dir_all(&declared).ok();
}

/// The two lists are **independent**, and a repository may declare either
/// alone — so neither test above is passing because the other pattern happened
/// to cover it.
#[test]
fn each_list_acts_on_its_own() {
    let excl = fixture("only-exclude", Some("[paths]\nexclude = [\"raw/**\"]\n"));
    let artifact = export(&excl);
    assert!(keys_under(&artifact, "raw/").is_empty());
    assert!(
        nodes(&artifact)
            .iter()
            .any(|(key, _, _)| key.starts_with("cfgkey:manifest/papers.json#")),
        "declaring only `exclude` must leave `manifest/` mined"
    );
    std::fs::remove_dir_all(&excl).ok();

    let opaq = fixture("only-opaque", Some("[paths]\nopaque = [\"manifest/**\"]\n"));
    let artifact = export(&opaq);
    assert!(
        node(&artifact, "file:raw/paper.md").is_some(),
        "declaring only `opaque` must leave `raw/` extracted"
    );
    assert!(
        nodes(&artifact)
            .iter()
            .all(|(key, _, _)| !key.starts_with("cfgkey:manifest/")),
        "and must still stop the manifest being mined"
    );
    std::fs::remove_dir_all(&opaq).ok();
}

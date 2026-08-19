//! End-to-end tests for `roteiro config` (ADR-0007): a project `roteiro.toml`
//! surfaces in the effective config, and a malformed file is a hard error for
//! any command.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_roteiro");

fn roteiro(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        // Isolate from any real user config.
        .env("ROTEIRO_HOME", dir)
        .output()
        .expect("run roteiro")
}

#[test]
fn config_reflects_project_toml_and_rejects_malformed() {
    let dir = std::env::temp_dir().join(format!("roteiro-config-cli-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("mkdir");
    // Project config is discovered at the repo root (alongside `.git`, per
    // ADR-0007), so mark this temp dir as a repository root.
    std::fs::create_dir_all(dir.join(".git")).expect("mkdir .git");
    std::fs::write(
        dir.join("roteiro.toml"),
        "[infer]\nmin_confidence = 0.66\n[duplicates]\nlimit = 7\n[ingest]\npdf = false\n\
         [serve]\naddr = \"127.0.0.1:9100\"\ntools = false\n\
         [debt]\nignore = [\"vendor/**\", \"**/generated/*\"]\n\
         [paths]\nmodel_store = \"~/models\"\n",
    )
    .expect("write config");

    // `config --json` reflects the project file's values.
    let out = roteiro(&dir, &["config", "--json"]);
    assert!(out.status.success(), "config failed: {out:?}");
    let cfg: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("config --json is valid JSON");
    assert_eq!(cfg["infer"]["min_confidence"], 0.66);
    assert_eq!(cfg["duplicates"]["limit"], 7);
    assert_eq!(cfg["ingest"]["pdf"], false);
    assert_eq!(cfg["serve"]["addr"], "127.0.0.1:9100");
    assert_eq!(cfg["serve"]["tools"], false);
    assert_eq!(cfg["debt"]["ignore"][0], "vendor/**");
    assert_eq!(cfg["debt"]["ignore"][1], "**/generated/*");
    assert_eq!(cfg["paths"]["model_store"], "~/models");

    // Human output labels provenance.
    let human = roteiro(&dir, &["config"]);
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(text.contains("(project)"), "provenance shown: {text}");

    // A malformed config is a hard error for any command.
    std::fs::write(dir.join("roteiro.toml"), "[infer]\nmin_confidence = = =\n").expect("write");
    let bad = roteiro(&dir, &["config"]);
    assert!(!bad.status.success(), "malformed config must fail");
    assert!(
        String::from_utf8_lossy(&bad.stderr).contains("config"),
        "error mentions the config: {:?}",
        String::from_utf8_lossy(&bad.stderr)
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// `roteiro config` must show **which layer each `[debt] ignore` pattern came
/// from** (issue #321c). The list merges across layers, so one `(project)` label
/// for the whole key would misreport a list holding patterns from both — and the
/// merge is only safe if it is legible.
#[test]
fn config_shows_per_pattern_debt_ignore_provenance() {
    let dir = std::env::temp_dir().join(format!("roteiro-config-debt-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::create_dir_all(dir.join(".git")).expect("mkdir .git");
    // `ROTEIRO_HOME` is `dir`, so `dir/config.toml` is the *user* layer.
    std::fs::write(
        dir.join("config.toml"),
        "[debt]\nignore = [\"vendor/**\", \"target/**\"]\n",
    )
    .expect("write user config");
    std::fs::write(
        dir.join("roteiro.toml"),
        "[debt]\nignore = [\"thirdparty/**\"]\n",
    )
    .expect("write project config");

    let out = roteiro(&dir, &["config"]);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "config failed: {out:?}");
    assert!(
        text.contains("[debt]"),
        "a [debt] section is printed: {text}"
    );
    // Every pattern survives the merge, each tagged with its own origin.
    assert!(
        text.contains("\"vendor/**\"  (user)"),
        "user pattern kept and labelled: {text}"
    );
    assert!(
        text.contains("\"target/**\"  (user)"),
        "second user pattern kept: {text}"
    );
    assert!(
        text.contains("\"thirdparty/**\"  (project)"),
        "project pattern labelled: {text}"
    );

    // `--json` carries the merged effective list.
    let json = roteiro(&dir, &["config", "--json"]);
    let cfg: serde_json::Value = serde_json::from_slice(&json.stdout).expect("valid JSON");
    assert_eq!(cfg["debt"]["ignore"][0], "vendor/**");
    assert_eq!(cfg["debt"]["ignore"][1], "target/**");
    assert_eq!(cfg["debt"]["ignore"][2], "thirdparty/**");

    // With `ignore_reset`, the inherited patterns are dropped *and named*.
    std::fs::write(
        dir.join("roteiro.toml"),
        "[debt]\nignore_reset = true\nignore = [\"thirdparty/**\"]\n",
    )
    .expect("write project config");
    let reset = roteiro(&dir, &["config"]);
    let text = String::from_utf8_lossy(&reset.stdout).into_owned();
    assert!(
        text.contains("ignore_reset = true"),
        "the reset is shown: {text}"
    );
    assert!(
        text.contains("\"vendor/**\"  (discarded from user)"),
        "what the reset dropped is named, not silently gone: {text}"
    );
    assert!(
        !text.contains("\"vendor/**\"  (user)"),
        "and it is no longer in the effective list: {text}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// `roteiro config` must not claim a reset that never happened (PR #343 review).
///
/// A user-layer `ignore_reset` governs nothing — a reset drops what a layer
/// inherits, and the user layer is the lowest — but the effective flag used to
/// inherit it, so this command printed "inherited patterns dropped" directly
/// above a list in which every inherited pattern was still present. The command
/// whose whole job is explaining the configuration was the one making the false
/// statement.
#[test]
fn config_reports_an_inert_user_layer_reset_instead_of_claiming_a_drop() {
    let dir = std::env::temp_dir().join(format!("roteiro-config-inert-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::create_dir_all(dir.join(".git")).expect("mkdir .git");
    // `ROTEIRO_HOME` is `dir`, so `dir/config.toml` is the *user* layer.
    std::fs::write(
        dir.join("config.toml"),
        "[debt]\nignore_reset = true\nignore = [\"vendor/**\", \"target/**\"]\n",
    )
    .expect("write user config");
    std::fs::write(
        dir.join("roteiro.toml"),
        "[debt]\nignore = [\"thirdparty/**\"]\n",
    )
    .expect("write project config");

    let out = roteiro(&dir, &["config"]);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "config failed: {out:?}");

    // The false headline is gone…
    assert!(
        !text.contains("(inherited patterns dropped)"),
        "nothing was dropped, so nothing may claim a drop: {text}"
    );
    // …and no pattern is reported as discarded either.
    assert!(
        !text.contains("(discarded from user)"),
        "no pattern was discarded: {text}"
    );
    // The inert request is still surfaced, since a silent no-op is the failure
    // this key exists to prevent.
    assert!(
        text.contains("NO EFFECT"),
        "an inert user-layer reset must say so: {text}"
    );
    // The merged list is untouched — this was only ever a reporting defect.
    for kept in [
        "\"vendor/**\"  (user)",
        "\"target/**\"  (user)",
        "\"thirdparty/**\"  (project)",
    ] {
        assert!(text.contains(kept), "{kept} must survive the merge: {text}");
    }

    // Moving the same reset to the project layer makes it real: it governs, the
    // drop is reported, and the inert note is not printed.
    std::fs::write(
        dir.join("roteiro.toml"),
        "[debt]\nignore_reset = true\nignore = [\"thirdparty/**\"]\n",
    )
    .expect("write project config");
    let real = roteiro(&dir, &["config"]);
    let text = String::from_utf8_lossy(&real.stdout).into_owned();
    assert!(
        text.contains("(inherited patterns dropped)"),
        "a project-layer reset really drops: {text}"
    );
    assert!(
        text.contains("\"vendor/**\"  (discarded from user)"),
        "and names what it dropped: {text}"
    );
    assert!(
        !text.contains("NO EFFECT"),
        "the inert note must not appear when the reset did take effect: {text}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// Build the workspace fixture: member repos, a root holding two of them, a root
/// holding none, and a `roteiro.toml` declaring all three workspace forms.
///
/// `discover_repos_under` tests for a `.git` entry's *existence*, so a marker
/// directory is a repo for scoping purposes — no `git init` is needed to exercise
/// the resolution rule, and `roteiro config` opens no graph.
fn workspace_fixture(dir: &Path) {
    std::fs::create_dir_all(dir.join(".git")).expect("mkdir .git");
    for repo in ["alpha", "beta", "code/one", "code/two", "gamma", "barren"] {
        std::fs::create_dir_all(dir.join(repo)).expect("mkdir repo");
    }
    for repo in ["alpha", "beta", "code/one", "code/two", "gamma"] {
        std::fs::create_dir_all(dir.join(repo).join(".git")).expect("mkdir .git");
    }
    let p = |rel: &str| at(dir, rel);
    std::fs::write(
        dir.join("roteiro.toml"),
        format!(
            "[[workspaces]]\nname = \"pair\"\nrepos = [\"{}\", \"{}\"]\n\n\
             [[workspaces]]\nname = \"wide\"\nroots = [\"{}\"]\n\n\
             [[workspaces]]\nname = \"barren\"\nroots = [\"{}\"]\n\n\
             [[workspaces]]\nname = \"hollow\"\n\n\
             [standalone]\nrepos = [\"{}\"]\n",
            p("alpha"),
            p("beta"),
            p("code"),
            p("barren"),
            p("gamma"),
        ),
    )
    .expect("write config");
}

/// A fixture path as the config and the report both spell it.
fn at(dir: &Path, rel: &str) -> String {
    dir.join(rel).to_str().expect("utf-8 path").to_owned()
}

/// The `resolved:` member list `roteiro config` printed for the workspace named
/// `name`, or `None` when that workspace was not reported at all.
///
/// Parses the block between this workspace's header and the next one, so the
/// assertion is "these repos are in *this* workspace", not "these strings appear
/// somewhere in the output" — the latter passes when every workspace is merged
/// into one indistinguishable list.
fn resolved_members(text: &str, name: &str) -> Option<Vec<String>> {
    let header = format!("{name}  [");
    let is_header =
        |l: &str| l.starts_with("  ") && !l.starts_with("   ") && l.contains("]  (from ");
    let mut lines = text
        .lines()
        .skip_while(|l| !l.trim_start().starts_with(&header));
    lines.next()?;
    let mut members = Vec::new();
    let mut in_resolved = false;
    for line in lines {
        if is_header(line) {
            break;
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with("resolved:") {
            in_resolved = true;
        } else if trimmed.starts_with("declared:") {
            in_resolved = false;
        } else if in_resolved && line.starts_with("      ") {
            members.push(trimmed.to_owned());
        }
    }
    Some(members)
}

/// The human output half of [`config_reports_every_workspace_form_declared_and_resolved`].
fn assert_workspace_text(dir: &Path, text: &str) {
    let p = |rel: &str| at(dir, rel);

    // 1. The two forms that were rendered NOWHERE now are.
    assert!(
        text.contains("[[workspaces]]  (4 declared)"),
        "`roteiro config` must render the `[[workspaces]]` form; the defect printed \
         only the legacy `[workspace]` table, so 4 declared named workspaces showed \
         as nothing at all (issue #499). Output was:\n{text}"
    );
    assert!(
        text.contains("[standalone]"),
        "`roteiro config` must render the `[standalone]` form; the defect rendered it \
         nowhere (issue #499). Output was:\n{text}"
    );

    // 2. The acceptance test: which repos are in workspace X, from this alone.
    assert_eq!(
        resolved_members(text, "pair").as_deref(),
        Some([p("alpha"), p("beta")].as_slice()),
        "a reader must be able to answer \"which repos are in workspace `pair`\" from \
         this output alone (issue #499). Output was:\n{text}"
    );

    // 3. DECLARED is distinguishable from RESOLVED: one root became two repos, and
    //    both numbers are on the page — "I declared one root and got two repos" is
    //    exactly the question people arrive with.
    assert!(
        text.contains("declared: 1 root(s), 0 repo(s)"),
        "the DECLARED roots must be shown, not only what they expanded to: {text}"
    );
    assert_eq!(
        resolved_members(text, "wide").as_deref(),
        Some([p("code/one"), p("code/two")].as_slice()),
        "a declared root must show the repos it RESOLVED to: {text}"
    );

    // 4. Zero members says which kind of nothing it is. "nothing matched" and
    //    "nothing declared" are different facts and an empty list states neither.
    assert!(
        text.contains("nothing MATCHED"),
        "a workspace whose declared root holds no repo must say its declaration \
         matched nothing, not print an empty list: {text}"
    );
    assert!(
        text.contains("nothing DECLARED"),
        "a workspace that names no roots and no repos must say so, distinctly from \
         one whose declaration matched nothing: {text}"
    );

    // 5. Provenance per ADR-0007, as the other sections do.
    assert!(
        text.contains("(from [[workspaces]], project)"),
        "each resolved workspace must name the table and layer it came from: {text}"
    );
    assert!(
        text.contains("(from [standalone] repos, project)"),
        "a standalone group must name the field it came from, since it is one repo \
         and came from exactly one field: {text}"
    );
}

/// The `--json` half: the same information, for a consumer that is not a reader.
fn assert_workspace_json(dir: &Path, cfg: &serde_json::Value) {
    let p = |rel: &str| at(dir, rel);
    let ws = cfg["workspace_resolution"]["workspaces"]
        .as_array()
        .unwrap_or_else(|| {
            panic!(
                "`config --json` must carry the workspace resolution too (issue #499); got: {cfg}"
            )
        });
    let pair = ws.iter().find(|w| w["name"] == "pair").expect("`pair`");
    assert_eq!(
        pair["resolved_repos"],
        serde_json::json!([p("alpha"), p("beta")]),
        "JSON must answer the membership question too: {pair}"
    );
    assert_eq!(
        pair["declared_repos"],
        serde_json::json!([p("alpha"), p("beta")]),
        "JSON must keep DECLARED separate from RESOLVED: {pair}"
    );
    let wide = ws.iter().find(|w| w["name"] == "wide").expect("`wide`");
    assert_eq!(
        wide["declared_roots"],
        serde_json::json!([p("code")]),
        "the declared root survives into JSON: {wide}"
    );
    assert_eq!(
        wide["resolved_repos"].as_array().map(Vec::len),
        Some(2),
        "and the repos it expanded to are alongside it: {wide}"
    );
    let hollow = ws.iter().find(|w| w["name"] == "hollow").expect("`hollow`");
    assert_eq!(
        hollow["resolved_repos"],
        serde_json::json!([]),
        "a workspace that resolves to nothing is an explicit empty list in JSON, \
         never a missing key: {hollow}"
    );
}

/// `roteiro config` must report **all three** workspace forms, and for each one
/// both what was declared and what it resolves to (issue #499).
///
/// The defect: the command rendered a literal `[workspace]` header and the legacy
/// singular table only, so a config declaring named `[[workspaces]]` and a
/// `[standalone]` printed `roots = None` / `repos = None` — a confident "you have
/// no workspaces" from the one command whose job is being believed about what the
/// configuration did. It cost a real misdiagnosis: a repo-count drop was blamed on
/// a resolution bug because no command would say what had resolved.
///
/// The acceptance test is the question a reader actually arrives with: *which
/// repos are in workspace X, right now* — answered from this output alone.
#[test]
fn config_reports_every_workspace_form_declared_and_resolved() {
    let dir = std::env::temp_dir().join(format!("roteiro-config-wsvis-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("mkdir");
    workspace_fixture(&dir);

    let out = roteiro(&dir, &["config"]);
    assert!(out.status.success(), "config failed: {out:?}");
    assert_workspace_text(&dir, &String::from_utf8_lossy(&out.stdout));

    let out = roteiro(&dir, &["config", "--json"]);
    assert!(out.status.success(), "config --json failed: {out:?}");
    let cfg: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    assert_workspace_json(&dir, &cfg);

    std::fs::remove_dir_all(&dir).ok();
}

/// A config `roteiro config` cannot resolve must still be *reported*, not thrown.
/// This is the command you run **because** something else is broken, so a
/// duplicate workspace name has to surface as a line in the output with the
/// declared tables still shown — never as a non-zero exit that tells you nothing.
#[test]
fn an_unresolvable_workspace_set_is_reported_not_thrown() {
    let dir = std::env::temp_dir().join(format!("roteiro-config-wsbad-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join(".git")).expect("mkdir .git");
    std::fs::write(
        dir.join("roteiro.toml"),
        "[[workspaces]]\nname = \"dup\"\nrepos = [\"/nowhere/a\"]\n\
         [[workspaces]]\nname = \"dup\"\nrepos = [\"/nowhere/b\"]\n",
    )
    .expect("write config");

    let out = roteiro(&dir, &["config"]);
    assert!(
        out.status.success(),
        "`roteiro config` must keep working when the config it is explaining does \
         not: {out:?}"
    );
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        text.contains("UNRESOLVED") && text.contains("duplicate workspace name `dup`"),
        "the resolution failure must be reported in the output: {text}"
    );
    assert!(
        text.contains("[[workspaces]]  (2 declared)"),
        "and the declared tables must still be shown, since they are what needs \
         fixing: {text}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

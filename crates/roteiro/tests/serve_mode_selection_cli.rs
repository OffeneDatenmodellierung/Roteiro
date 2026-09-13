//! **Characterisation tests.** What `roteiro explorer` and `roteiro serve`
//! decide to serve, pinned as it behaves *today* — not as it ought to behave.
//!
//! # Why this file exists
//!
//! A `--scope` flag for these two commands will refactor exactly this decision,
//! and until now nothing exercised it: `okf_mounts` and `serve_okf_only` had no
//! test at all, and the `{ws}/{project}` mount label — the string a scope flag is
//! most likely to change by accident — was derived in one `format!` nothing read
//! back. Refactoring untested fallback logic is how behaviour changes silently.
//!
//! So these tests are deliberately **descriptive, not prescriptive**. Where the
//! current behaviour is surprising it is pinned *as surprising*, with the
//! surprise written down beside it. A test here going red does not by itself mean
//! the change is wrong — it means the change moved something this file says is
//! load-bearing, and the mover has to say which and why.
//!
//! # Why it drives the real binary
//!
//! Every decision below reads `std::env::current_dir()`, the config home, or
//! both. The current directory is process-global, and the workspace deliberately
//! contains no `set_current_dir` anywhere — so a unit test would either race
//! every other test in its binary or assert about the developer's own directory.
//! A spawned child gets its own cwd and its own `$ROTEIRO_HOME`, which makes the
//! matrix below expressible at all.
//!
//! The observable is the startup line plus the route table, because those are
//! what actually differ between the modes:
//!
//! | mode | startup line | `/` | `/v1/graph/workspaces` |
//! |------|--------------|-----|------------------------|
//! | graph explorer (`serve_graph_ui`) | `listening on http://ADDR/ (UI) — API at …` | 200, the UI | 200 |
//! | bundles only (`serve_okf_only`)   | `listening on http://ADDR/okf — no repository here, so N OKF bundle(s) only: …` | 307 → `/okf` | 404 |
//!
//! Asserting on both means a refactor cannot satisfy these tests by starting
//! *something* on the port.

#![cfg(feature = "explorer")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

mod common;
use common::{IsolatedHome, scratch_dir};

const BIN: &str = env!("CARGO_BIN_EXE_roteiro");

/// Where both servers mount the viewer — `main.rs`'s `OKF_BASE`, restated here
/// because a test binary cannot reach a private const in the bin crate.
const OKF_BASE: &str = "/okf";

// ---------------------------------------------------------------------------
// 1. Mode selection: the truth table
// ---------------------------------------------------------------------------

/// Which server `roteiro explorer` ends up running, as a function of the inputs
/// that select it.
/// Graph mode carries **which workspaces it served**, because without that the
/// configured cells and the cwd-fallback cell are the same observation: both
/// answer `/v1/graph/workspaces` with 200. "The configured set is served and the
/// cwd repo is not added" is half of what this table claims, and a bare `Graph`
/// does not assert it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Mode {
    /// `serve_graph_ui`: the graph explorer, `/v1/graph/*` + the web app, hosting
    /// exactly these workspaces in this order.
    Graph(Vec<String>),
    /// `serve_okf_only`: bundles and nothing else, `/` redirecting to `/okf`.
    BundlesOnly,
    /// Refuses to start — and **exits**. A server that merely never printed a
    /// startup line is a hang, and `observe_mode` panics rather than reporting it
    /// here: a refusal cell that a hang can satisfy pins nothing.
    Refuses,
}

/// One cell of the mode-selection matrix.
struct Cell {
    /// What the cell is, for the failure message.
    what: &'static str,
    /// Is there resolvable workspace config?
    config: bool,
    /// Is the current directory inside a git repository?
    repo: bool,
    /// Is there an OKF bundle at or under the current directory?
    bundle: bool,
    /// The mode selected today.
    mode: Mode,
}

/// Shorthand for a `Graph` expectation: the workspaces the cell must serve.
fn graph(names: &[&str]) -> Mode {
    Mode::Graph(names.iter().map(|n| (*n).to_owned()).collect())
}

/// **The mode-selection truth table, bundle-free half.**
///
/// {config present, absent} × {inside a repo, not} with no bundle anywhere, so
/// this half holds whether or not `okf-viewer` is compiled in. The bundle column
/// is [`explorer_mode_truth_table_with_a_bundle`] below.
///
/// The cell that matters most is the second: with no repository and no config
/// there is nothing to serve and the command **errors**. That is also the
/// behavioural proof that `serve_okf_only` is never entered with an empty mount
/// list — see [`the_zero_bundle_chooser_page_is_unreachable`].
#[test]
fn explorer_mode_truth_table_without_a_bundle() {
    run_matrix(
        "explorer-modes-nobundle",
        &[
            Cell {
                what: "no config, inside a repo, no bundle → the cwd repo alone",
                config: false,
                repo: true,
                bundle: false,
                // Named after the directory, by `explorer_cwd_set` — and this is
                // the observation that separates the fallback from the
                // configured cells below.
                mode: graph(&["cell0"]),
            },
            Cell {
                what: "no config, no repo, no bundle → nothing to serve",
                config: false,
                repo: false,
                bundle: false,
                mode: Mode::Refuses,
            },
            Cell {
                what: "config, inside a repo, no bundle → the configured set (the \
                       cwd repo is NOT added)",
                config: true,
                repo: true,
                bundle: false,
                mode: graph(&["one", "two"]),
            },
            Cell {
                what: "config, no repo, no bundle → the configured set; a repo is \
                       not required at all",
                config: true,
                repo: false,
                bundle: false,
                mode: graph(&["one", "two"]),
            },
        ],
    );
}

/// **The mode-selection truth table, bundle half.**
///
/// The whole point of the fallback: `serve_okf_only` is reached **only** when
/// there is no repository *and* no config. Two cells guard that jointly, and
/// both are easy to break by accident:
///
/// * *config + no repo + a bundle* stays on the graph path. `run_explorer` only
///   consults `okf_mounts` inside the `Err(no_repo)` arm of the cwd fallback,
///   and that arm is only reached when config resolved to nothing — "a
///   configured set that failed is a fault to report, not a cue to serve
///   something else".
/// * *no config + inside a repo + a bundle* also stays on the graph path, for a
///   different reason: `explorer_cwd_set` calls `Repo::discover`, which succeeds,
///   so the `Err` arm is never taken. Inside a repository the fallback arm is
///   **unreachable** however many bundles are lying around.
#[test]
#[cfg(feature = "okf-viewer")]
fn explorer_mode_truth_table_with_a_bundle() {
    run_matrix(
        "explorer-modes-bundle",
        &[
            Cell {
                what: "no config, inside a repo, a bundle → still the graph; \
                       `Repo::discover` succeeds so the fallback arm is unreachable",
                config: false,
                repo: true,
                bundle: true,
                mode: graph(&["cell0"]),
            },
            Cell {
                what: "no config, no repo, a bundle → bundles only (this is what \
                       `roteiro okf view <path>` became)",
                config: false,
                repo: false,
                bundle: true,
                mode: Mode::BundlesOnly,
            },
            Cell {
                what: "config, inside a repo, a bundle → the configured set",
                config: true,
                repo: true,
                bundle: true,
                mode: graph(&["one", "two"]),
            },
            Cell {
                what: "config, no repo, a bundle → the configured set, NOT bundles \
                       only: config outranks the bundle fallback",
                config: true,
                repo: false,
                bundle: true,
                mode: graph(&["one", "two"]),
            },
        ],
    );
}

/// Without `okf-viewer` there is no bundle fallback to reach: the same cell that
/// serves bundles in the full build refuses outright.
///
/// Pinned because `serve_okf_only` is `#[cfg(all(feature = "explorer", feature =
/// "okf-viewer"))]`, so a `--scope` refactor that moves the decision out of that
/// `cfg` would change what a `--no-default-features --features explorer` build
/// does, in a build no other test in this file runs.
#[test]
#[cfg(not(feature = "okf-viewer"))]
fn explorer_without_the_viewer_has_no_bundle_fallback() {
    run_matrix(
        "explorer-modes-noviewer",
        &[Cell {
            what: "no config, no repo, a bundle, no `okf-viewer` → refuses; there \
                   is no fallback compiled in",
            config: false,
            repo: false,
            bundle: true,
            mode: Mode::Refuses,
        }],
    );
}

/// Config that resolves to **no workspace at all** is its own outcome, distinct
/// from "no config": it bails with a named diagnostic rather than falling back to
/// the cwd repo or to a bundle.
///
/// `explorer` and `serve` reach this from two different functions —
/// `run_explorer` and `build_serve_workspaces` — each carrying its own copy of
/// the sentence, differing only in the `--workspace <ROOT>` clause that applies
/// to one of them. Two copies of a message is two places for a refactor to move
/// one and not the other, so both are pinned.
#[test]
fn a_config_resolving_to_nothing_bails_rather_than_falling_back() {
    let base = scratch_dir("explorer-empty-config");
    std::fs::create_dir_all(&base).expect("mkdir base");
    // A root that exists and holds no repository: `resolved_workspaces` yields one
    // named workspace, and `WorkspaceSet::from_resolved` then finds nothing in it.
    let ghost = base.join("holds-no-repo");
    std::fs::create_dir_all(&ghost).expect("mkdir ghost root");
    // Inside a repo, deliberately: the bail must win over the cwd fallback.
    let cwd = base.join("solo");
    make_repo(&cwd);

    let home = IsolatedHome::new("explorer-empty-config");
    write_config(&home, &[("ghost", ghost.as_path())]);

    for (cmd, expected) in [
        (
            "explorer",
            "no workspaces to serve — run inside a repo, or configure",
        ),
        (
            "serve",
            "no workspaces to serve — run inside a repo, pass `--workspace <ROOT>`, or configure",
        ),
    ] {
        let (status, stderr) = run_refusing(&[cmd, "--addr", &free_addr()], &cwd, &home);
        assert!(
            !status.success(),
            "`roteiro {cmd}` must refuse when config resolves to no workspace; \
             it exited {status:?}"
        );
        assert!(
            stderr.contains(expected),
            "`roteiro {cmd}` must bail with its own `no workspaces to serve` \
             diagnostic; got: {stderr}"
        );
    }

    std::fs::remove_dir_all(&base).ok();
}

// ---------------------------------------------------------------------------
// 2. `okf_mounts`: admission, dedup, and the label
// ---------------------------------------------------------------------------

/// **The mount label is `{ws}/{project}`, and the cwd block is the only source
/// of a mount with no workspace in its name.**
///
/// Two workspaces, deliberately: with one, `{ws}/` could be dropped from the
/// `format!` and every assertion about a single-workspace host would still pass.
/// Here `one/alpha` and `two/beta` differ in the half a scope flag would tidy
/// away, and the third row — plain `okf`, from the current directory — is the one
/// mount `okf_mounts` produces without consulting the workspace set at all.
///
/// Order is pinned too, and it is not incidental: the cwd block runs **last** so
/// that when the current directory *is* a hosted project, the project's
/// `{ws}/{project}` name wins the readable slug over the bare directory name.
/// See [`a_bundle_reachable_twice_is_mounted_once`].
#[test]
#[cfg(feature = "okf-viewer")]
fn mount_labels_are_workspace_slash_project_and_the_cwd_is_the_exception() {
    let fx = TwoWorkspaces::new("okf-labels");
    // A non-repo directory holding a bundle, so the cwd row is present and is
    // plainly not derived from any workspace.
    let cwd = fx.base.join("elsewhere");
    write_bundle(&cwd.join("okf"));

    let server = Server::spawn(&["explorer", "--addr", "{addr}"], &cwd, &fx.home);
    let (addr, _line) = server.wait_for_listening();

    assert_eq!(
        chooser_rows(&addr),
        vec![
            ("one/alpha".to_owned(), canon(&fx.alpha.join("okf"))),
            ("two/beta".to_owned(), canon(&fx.beta.join("okf"))),
            ("okf".to_owned(), canon(&cwd.join("okf"))),
        ],
        "the label is `{{ws}}/{{project}}` for a hosted project and the bare \
         directory name for the current directory's bundle, workspace mounts \
         first"
    );
}

/// A directory called `okf` is admitted **only** when it holds an `index.md`
/// *file*. Not "exists", not "is a directory that looks bundle-ish": `render okf`
/// writes an `index.md` and `okf-core` requires one, so a directory without it
/// would 404 every route it was mounted under.
#[test]
#[cfg(feature = "okf-viewer")]
fn a_bundle_needs_an_index_md_file_to_be_mounted() {
    let fx = TwoWorkspaces::new("okf-admission");
    // `beta` gets an `okf/` directory with content but no `index.md`, and a
    // *directory* called `index.md` inside it besides — `is_file()` is the test,
    // and a directory of that name must not satisfy it.
    std::fs::remove_file(fx.beta.join("okf").join("index.md")).expect("drop index.md");
    std::fs::create_dir_all(fx.beta.join("okf").join("index.md")).expect("mkdir decoy");
    std::fs::write(fx.beta.join("okf").join("concepts.md"), "# c\n").expect("write");

    let cwd = fx.base.join("elsewhere");
    std::fs::create_dir_all(&cwd).expect("mkdir cwd");

    let server = Server::spawn(&["explorer", "--addr", "{addr}"], &cwd, &fx.home);
    let (addr, _line) = server.wait_for_listening();

    // Exactly one mount remains, so `/okf` redirects to it rather than listing.
    assert_eq!(
        single_mount_redirect(&addr),
        Some("/okf/one-alpha".to_owned()),
        "`two/beta` has an `okf/` directory and no `index.md` file, so it is not \
         a bundle and must not be mounted"
    );
}

/// One bundle reachable by two routes is mounted **once**, keyed on the
/// canonical root — and the workspace-derived label is the one that survives,
/// because the cwd block is admitted last.
///
/// The current directory is very often also a hosted project; mounting the same
/// bundle twice under two names would leave a reader wondering which is
/// authoritative.
#[test]
#[cfg(feature = "okf-viewer")]
fn a_bundle_reachable_twice_is_mounted_once() {
    let fx = TwoWorkspaces::new("okf-dedup");
    // Stand inside `alpha` itself: `okf_mounts` reaches `<alpha>/okf` once from
    // the workspace walk and once from the cwd block.
    let server = Server::spawn(&["explorer", "--addr", "{addr}"], &fx.alpha, &fx.home);
    let (addr, _line) = server.wait_for_listening();

    assert_eq!(
        chooser_rows(&addr),
        vec![
            ("one/alpha".to_owned(), canon(&fx.alpha.join("okf"))),
            ("two/beta".to_owned(), canon(&fx.beta.join("okf"))),
        ],
        "`<alpha>/okf` is reachable from the workspace walk and from the current \
         directory; it must appear once, under the workspace label"
    );
}

/// The current directory is read **two ways** — `<cwd>/okf` and `<cwd>` itself —
/// so standing above a bundle and standing inside one both work. The label is
/// the directory name of whichever root was admitted, which is why standing
/// above a bundle labels it `okf` rather than naming the directory you are in.
///
/// Pinned rather than corrected: it is the observable a `--scope` flag would be
/// tempted to "improve", and improving it is a change to what the chooser shows.
#[test]
#[cfg(feature = "okf-viewer")]
fn the_cwd_is_read_both_as_a_bundle_and_as_its_parent() {
    let base = scratch_dir("okf-cwd-readings");
    let home = IsolatedHome::new("okf-cwd-readings");

    // Standing *above* a bundle: the admitted root is `<cwd>/okf`, so the label
    // is the literal string `okf`.
    let above = base.join("above");
    write_bundle(&above.join("okf"));
    let server = Server::spawn(&["explorer", "--addr", "{addr}"], &above, &home);
    let (addr, line) = server.wait_for_listening();
    assert_eq!(
        bundles_only_labels(&line),
        Some(vec!["okf".to_owned()]),
        "standing above a bundle admits `<cwd>/okf`, whose directory name is `okf`"
    );
    assert_eq!(single_mount_redirect(&addr), Some("/okf/okf".to_owned()));
    drop(server);

    // Standing *inside* one: the admitted root is the cwd, so the label is the
    // directory you are in.
    let inside = base.join("my-bundle");
    write_bundle(&inside);
    let server = Server::spawn(&["explorer", "--addr", "{addr}"], &inside, &home);
    let (addr, line) = server.wait_for_listening();
    assert_eq!(
        bundles_only_labels(&line),
        Some(vec!["my-bundle".to_owned()]),
        "standing inside a bundle admits the cwd, whose directory name names it"
    );
    assert_eq!(
        single_mount_redirect(&addr),
        Some("/okf/my-bundle".to_owned())
    );

    std::fs::remove_dir_all(&base).ok();
}

/// A project served from a bare `graph.db` has no repository on disk, so it
/// contributes no mount however many bundles sit beside the process — only the
/// cwd block can speak for it.
///
/// This is the single-repo `serve` fallback (`Workspace::single`), and it is why
/// that server's startup note says `okf` and not `default/<repo>`.
#[test]
#[cfg(feature = "okf-viewer")]
fn a_project_with_no_repository_on_disk_contributes_no_mount() {
    let base = scratch_dir("okf-no-repo-root");
    let repo = base.join("solo");
    make_repo(&repo);
    write_bundle(&repo.join("okf"));
    let home = IsolatedHome::new("okf-no-repo-root");

    // `serve` with no config and no `--workspace` hosts the cwd repo via
    // `Workspace::single`, whose project has no `project_root`.
    let server = Server::spawn(&["serve", "--addr", "{addr}"], &repo, &home);
    let (_addr, line) = server.wait_for_listening();

    assert!(
        line.contains("+ /okf (OKF: okf)"),
        "the one mount must come from the cwd block, labelled by directory name \
         — a `Workspace::single` project has no repository root to look beside. \
         got: {line}"
    );
    assert!(
        !line.contains("default/solo"),
        "there is no `{{ws}}/{{project}}` mount to be had here; got: {line}"
    );

    std::fs::remove_dir_all(&base).ok();
}

// ---------------------------------------------------------------------------
// 3. The empty case
// ---------------------------------------------------------------------------

/// **No bundle anywhere ⇒ `/okf` is not mounted at all**, and therefore the
/// chooser's own zero-bundle page cannot be reached in production.
///
/// `mount_okf` returns the router untouched when the mount list is empty — "a
/// `/okf` that 404s every request is a worse answer than an absent one" — so the
/// only caller that could render `okf_viewer`'s "No bundle is mounted" page never
/// passes it an empty list. The other caller, `serve_okf_only`, is guarded by
/// `if !mounts.is_empty()` at its one call site, which is exactly why
/// [`explorer_mode_truth_table_without_a_bundle`]'s second cell **errors**
/// instead of starting an empty bundle server.
///
/// Both halves are asserted here: the startup line carries no `/okf` note, and
/// `/okf` itself 404s while the explorer beside it is up. The dead page itself is
/// pinned in `main.rs`'s `okf_mount_tests`, which shows it renders when called —
/// so this is "unreachable", not "absent".
#[test]
#[cfg(feature = "okf-viewer")]
fn the_zero_bundle_chooser_page_is_unreachable() {
    let base = scratch_dir("okf-empty");
    let repo = base.join("bundleless");
    make_repo(&repo);
    let home = IsolatedHome::new("okf-empty");

    let server = Server::spawn(&["explorer", "--addr", "{addr}"], &repo, &home);
    let (addr, line) = server.wait_for_listening();

    assert!(
        !line.contains("/okf"),
        "with no bundle found the startup line must not advertise `/okf`; got: {line}"
    );
    assert_eq!(
        http_get(&addr, "/okf").0,
        404,
        "`mount_okf` returns the router untouched on an empty mount list, so \
         there is no `/okf` route to answer — not even with the chooser's \
         zero-bundle page"
    );
    assert_eq!(
        http_get(&addr, "/okf/anything").0,
        404,
        "nothing below `/okf` is mounted either"
    );
    assert_eq!(
        http_get(&addr, "/v1/graph/projects").0,
        200,
        "the rest of the router is untouched — this is `mount_okf` declining to \
         merge, not a server that failed to build"
    );

    std::fs::remove_dir_all(&base).ok();
}

// ---------------------------------------------------------------------------
// 4. Flag semantics, as they actually are
// ---------------------------------------------------------------------------

/// **`--workspace ROOT` replaces what is served.** With no config, passing it
/// switches `serve` off the single-repo fallback entirely: the current
/// directory's repo is no longer hosted, only the repos under ROOT.
///
/// The fallback fires *only* when nothing selects a workspace — no config, no
/// `--workspace`, no `--workspace-name` — so this flag does not add the roots to
/// the cwd repo, it displaces it.
#[test]
fn workspace_root_displaces_the_cwd_repo() {
    let base = scratch_dir("serve-workspace-root");
    let cwd = base.join("standing-here");
    make_repo(&cwd);
    let root = base.join("roots");
    make_repo(&root.join("alpha"));
    let home = IsolatedHome::new("serve-workspace-root");

    // Without the flag: the cwd repo, hosted as `default`.
    let server = Server::spawn(&["serve", "--addr", "{addr}"], &cwd, &home);
    let (addr, _line) = server.wait_for_listening();
    assert_eq!(
        graph_projects(&addr),
        vec!["standing-here".to_owned()],
        "with nothing selecting a workspace, `serve` hosts the current repo alone"
    );
    drop(server);

    // With it: the roots under ROOT, and the cwd repo is gone.
    let server = Server::spawn(
        &[
            "serve",
            "--workspace",
            &root.display().to_string(),
            "--addr",
            "{addr}",
        ],
        &cwd,
        &home,
    );
    let (addr, _line) = server.wait_for_listening();
    assert_eq!(
        graph_projects(&addr),
        vec!["alpha".to_owned()],
        "`--workspace ROOT` narrows the served set to ROOT's repos — the current \
         directory's repo is NOT folded in alongside"
    );

    std::fs::remove_dir_all(&base).ok();
}

/// **`-w`/`--workspace-name` does not narrow anything.** It selects the default
/// the *flat* `/v1/graph/*` routes bind to, and that is all: every other
/// workspace is still hosted, still listed by `/v1/graph/workspaces`, and still
/// fully readable through its nested `/v1/graph/workspaces/{ws}/…` routes.
///
/// The asymmetry with `--workspace` above is genuinely surprising — one flag
/// scopes the server and the other scopes a default — and a `--scope` flag is
/// likely to be tempted to tidy it. Pinned so that tidying is a visible,
/// deliberate change rather than a silent one.
#[test]
fn workspace_name_selects_a_default_without_hiding_the_others() {
    let fx = TwoWorkspaces::new("explorer-w-flag");
    let cwd = fx.base.join("elsewhere");
    std::fs::create_dir_all(&cwd).expect("mkdir cwd");

    let server = Server::spawn(
        &["explorer", "-w", "one", "--addr", "{addr}"],
        &cwd,
        &fx.home,
    );
    let (addr, line) = server.wait_for_listening();

    // The flat routes bind to `one` …
    assert_eq!(
        graph_projects(&addr),
        vec!["alpha".to_owned()],
        "`-w one` is what the flat `/v1/graph/projects` resolves to"
    );
    // … and nothing else is hidden.
    assert!(
        line.contains("2 workspace(s): one, two"),
        "both workspaces are still hosted; `-w` did not narrow the set. got: {line}"
    );
    assert_eq!(
        workspace_names(&addr),
        vec!["one".to_owned(), "two".to_owned()],
        "`/v1/graph/workspaces` still advertises every workspace"
    );
    assert_eq!(
        projects_of(&addr, "two"),
        vec!["beta".to_owned()],
        "the nested route reaches the workspace `-w` did not select — this is \
         the asymmetry with `--workspace ROOT`, which really does narrow"
    );
}

/// **A pinned defect, recorded rather than fixed.**
///
/// With no workspace config, `roteiro serve -w NAME` inside a repository can
/// never start — for **any** `NAME` — and the error it gives tells you to do the
/// thing you are already doing.
///
/// `build_serve_workspaces`' single-repo fallback is guarded by
/// `resolved.is_empty() && workspace_roots.is_empty() && workspace_name.is_none()`,
/// so passing `-w` suppresses it. With nothing in config there is then nothing to
/// fold, the workspace set comes out empty, and the function bails with
/// "no workspaces to serve — run inside a repo, …" — addressed to somebody who is
/// inside a repo. `default`, the name the fallback itself would have used, does
/// not work either.
///
/// `roteiro explorer -w NAME` in the same directory answers precisely, because it
/// builds the cwd fallback set *first* and then validates the name against it:
/// "no workspace named `bogus` (known: plain)". Both halves are asserted here, so
/// the asymmetry is the subject and not a detail of one message.
///
/// **This test is expected to go red when the defect is fixed.** That is what it
/// is for: it is here so a `--scope` refactor cannot quietly change this into
/// some third behaviour, and so a deliberate fix arrives with a test diff that
/// says what changed. `roteiro mcp` reaches the same code and presumably behaves
/// the same way; it is not asserted here because this file is about `serve` and
/// `explorer`.
#[test]
fn serve_with_a_workspace_name_and_no_config_cannot_start_at_all() {
    let base = scratch_dir("serve-w-no-config");
    let repo = base.join("plain");
    make_repo(&repo);
    let home = IsolatedHome::new("serve-w-no-config");

    // No value of `-w` gets `serve` off the ground: not a made-up one, not the
    // repo's own directory name, not `default`.
    for name in ["bogus", "plain", "default"] {
        let (status, stderr) =
            run_refusing(&["serve", "-w", name, "--addr", &free_addr()], &repo, &home);
        assert!(
            !status.success(),
            "`serve -w {name}` unexpectedly started; if that is the fix, this \
             test is the changelog entry for it"
        );
        assert!(
            stderr.contains("no workspaces to serve — run inside a repo"),
            "`serve -w {name}` bails with the generic diagnostic — which advises \
             running inside a repo, from inside a repo, and never mentions that \
             `-w` is what suppressed the single-repo fallback. got: {stderr}"
        );
        assert!(
            !stderr.contains("no workspace named"),
            "`serve` does not reach the precise `UnknownWorkspace` message that \
             `explorer` gives; got: {stderr}"
        );
    }

    // The same flag, the same directory, the other command: precise.
    let (status, stderr) = run_refusing(
        &["explorer", "-w", "bogus", "--addr", &free_addr()],
        &repo,
        &home,
    );
    assert!(!status.success(), "`explorer -w bogus` must refuse too");
    assert!(
        stderr.contains("no workspace named `bogus`") && stderr.contains("known: plain"),
        "`explorer` validates `-w` against the cwd fallback set and says so — \
         this is the half `serve` is missing; got: {stderr}"
    );

    std::fs::remove_dir_all(&base).ok();
}

// ---------------------------------------------------------------------------
// Matrix driver
// ---------------------------------------------------------------------------

/// Run every cell and report **all** mismatches at once: a truth table read one
/// failure at a time hides how much a refactor moved.
fn run_matrix(label: &str, cells: &[Cell]) {
    let base = scratch_dir(label);
    std::fs::create_dir_all(&base).expect("mkdir matrix base");
    // The configured set, built once and shared: no cell mutates it.
    let cfg = TwoWorkspaces::new(&format!("{label}-cfg"));
    // A home with no config at all, for the `config: false` cells.
    let unconfigured = IsolatedHome::new(&format!("{label}-none"));

    let mut failures: Vec<String> = Vec::new();
    for (i, cell) in cells.iter().enumerate() {
        let cwd = base.join(format!("cell{i}"));
        if cell.repo {
            make_repo(&cwd);
        } else {
            std::fs::create_dir_all(&cwd).expect("mkdir cell cwd");
        }
        if cell.bundle {
            write_bundle(&cwd.join("okf"));
        }
        let home = if cell.config {
            &cfg.home
        } else {
            &unconfigured
        };
        let observed = observe_mode(cell.what, &cwd, home);
        if observed != cell.mode {
            failures.push(format!(
                "  cell {i}: {}\n    config={} repo={} bundle={}\n    \
                 expected {:?}, observed {:?}",
                cell.what, cell.config, cell.repo, cell.bundle, cell.mode, observed
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "`roteiro explorer` selected a different server than this table records. \
         These are characterisation tests: if the change is intended, move the \
         table and say which cell moved and why.\n{}",
        failures.join("\n")
    );

    std::fs::remove_dir_all(&base).ok();
}

/// Start `roteiro explorer` in `cwd` and classify what it became, by the startup
/// line **and** the route table — so "it started" can never pass for "it started
/// in this mode".
///
/// # Refusal is an exit, not an absence of output
///
/// The first version returned [`Mode::Refuses`] as soon as no startup line
/// arrived, which meant a server that *hung* — bound nothing, printed nothing,
/// stayed alive — satisfied the refusal cells. A cell that a hang can satisfy
/// pins nothing, and this file exists to rule exactly that out, so the child is
/// now required to have **exited** and its status is reported.
fn observe_mode(what: &str, cwd: &Path, home: &IsolatedHome) -> Mode {
    let addr = free_addr();
    let mut child = spawn(&["explorer", "--addr", &addr], cwd, home);
    let lines = stderr_lines(&mut child);
    let mut server = Server { child, lines };

    let Some(line) = server.wait_for_line(|l| l.contains(" listening on http://")) else {
        // No startup line. That is a refusal only if the process is gone; a live
        // one is a hang, and saying "refused" about it would be a false green.
        //
        // **Waited for, not sampled.** `wait_for_line` returns `None` the moment
        // stderr *closes*, and a closed pipe is not a reaped process: the child
        // has written its error and dropped the descriptor, but the exit has not
        // been collected yet. A single `try_wait` there reads `None` and calls a
        // perfectly ordinary refusal a hang. That raced green on this developer's
        // machine and red on CI — twice at the same commit — which is how it was
        // found.
        let status =
            wait_for_exit(&mut server.child, Duration::from_secs(30)).unwrap_or_else(|| {
                panic!(
                    "{what}: `roteiro explorer` printed no listening line and was \
                     STILL RUNNING 30s later — that is a hang, not a refusal. \
                     Reporting it as `Refuses` would let a startup deadlock \
                     satisfy this cell."
                )
            });
        assert!(
            !status.success(),
            "{what}: `roteiro explorer` exited SUCCESSFULLY without ever \
             listening ({status:?}). A refusal cell means a non-zero exit, not \
             merely the absence of a server."
        );
        return Mode::Refuses;
    };

    let graph_line = line.contains(&format!("http://{addr}/ (UI)"));
    let bundles_line = line.contains(&format!("http://{addr}/okf — no repository here"));
    // `/v1/graph/workspaces` and not `/v1/graph/projects`: the flat route 400s
    // with "several workspaces configured" whenever the set holds more than one
    // and nothing selected a default, which is two of the cells below. The
    // workspace route answers in every graph-mode cell and exists in no other
    // mode, which is exactly the property a mode probe needs.
    let (graph_status, _, _) = http_get(&addr, "/v1/graph/workspaces");
    let (root_status, root_location, _) = http_get(&addr, "/");

    match (graph_line, bundles_line) {
        (true, false) => {
            assert_eq!(
                graph_status, 200,
                "{what}: the startup line says graph explorer but \
                 `/v1/graph/workspaces` answered {graph_status} — the line and \
                 the router disagree"
            );
            // Read back from the router rather than parsed out of the startup
            // line: the line is what the server *said*, this is what it serves.
            Mode::Graph(workspace_names(&addr))
        }
        (false, true) => {
            assert_eq!(
                graph_status, 404,
                "{what}: the startup line says bundles-only but \
                 `/v1/graph/workspaces` answered {graph_status} — there is no \
                 graph in this mode"
            );
            assert_eq!(
                (root_status, root_location.as_str()),
                (307, OKF_BASE),
                "{what}: bundles-only redirects `/` to `{OKF_BASE}` specifically; \
                 any other redirect target is a different contract"
            );
            Mode::BundlesOnly
        }
        _ => panic!("{what}: unrecognised startup line — a new mode? got: {line}"),
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Two named workspaces, one repo each, each carrying a rendered bundle — so
/// `{ws}/` in the mount label is load-bearing and a dropped workspace is visible.
///
/// `alpha`/`beta` are the repository paths the mount assertions compare origins
/// against, so they are read only by the `okf-viewer` tests. Allowed rather than
/// `cfg`-gated per field: the fixture is one thing in both builds, and splitting
/// it would put a `#[cfg]` on two fields and both their initialisers to silence
/// a lint about a build that simply asks less of it.
#[cfg_attr(not(feature = "okf-viewer"), allow(dead_code))]
struct TwoWorkspaces {
    base: PathBuf,
    alpha: PathBuf,
    beta: PathBuf,
    home: IsolatedHome,
}

impl TwoWorkspaces {
    fn new(label: &str) -> Self {
        let base = scratch_dir(label);
        let alpha = base.join("wsA").join("alpha");
        let beta = base.join("wsB").join("beta");
        make_repo(&alpha);
        make_repo(&beta);
        write_bundle(&alpha.join("okf"));
        write_bundle(&beta.join("okf"));
        let home = IsolatedHome::new(label);
        let (root_a, root_b) = (base.join("wsA"), base.join("wsB"));
        write_config(
            &home,
            &[("one", root_a.as_path()), ("two", root_b.as_path())],
        );
        Self {
            base,
            alpha,
            beta,
            home,
        }
    }
}

impl Drop for TwoWorkspaces {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.base).ok();
    }
}

/// Write `<home>/config.toml` with one `[[workspaces]]` per `(name, root)`.
fn write_config(home: &IsolatedHome, workspaces: &[(&str, &Path)]) {
    use std::fmt::Write as _;
    let mut toml = String::new();
    for (name, root) in workspaces {
        // **Single quotes.** A TOML *literal* string takes no escapes, and a
        // path is the one value most likely to contain a backslash: on Windows
        // `Path::display()` yields `C:\\Users\\…`, and `\\U` inside a basic
        // (double-quoted) string is an invalid escape, so the config would fail
        // to parse and every config-backed cell would silently fall back to the
        // no-config path. The scratch paths here never contain an apostrophe,
        // which is the only thing a literal string cannot hold.
        let _ = write!(
            toml,
            "[[workspaces]]\nname = '{name}'\nroots = ['{}']\n\n",
            root.display()
        );
    }
    std::fs::write(home.path().join("config.toml"), toml).expect("write config.toml");
}

/// The smallest thing `okf_mounts` will admit: a directory holding an
/// `index.md` **file**.
fn write_bundle(root: &Path) {
    std::fs::create_dir_all(root).expect("mkdir bundle");
    std::fs::write(
        root.join("index.md"),
        "---\nokf_version: \"0.2\"\n---\n\n# Bundle\n",
    )
    .expect("write index.md");
}

/// A fresh git repo with one commit, so `serve` can build a graph for it.
fn make_repo(dir: &Path) {
    std::fs::create_dir_all(dir).expect("mkdir repo");
    git(dir, &["init", "-q", "."]);
    std::fs::write(dir.join("README.md"), "# fixture\n").expect("write README");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "init"]);
}

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

// ---------------------------------------------------------------------------
// Process helpers
// ---------------------------------------------------------------------------

/// A spawned server, killed when it goes out of scope — **including on a panic**.
/// Every assertion here is about a process that is still running, so every
/// failure leaves one behind, and an orphan keeps its port.
struct Server {
    child: Child,
    lines: Receiver<String>,
}

impl Server {
    /// Spawn `args` in `cwd`, substituting a free address for `{addr}`.
    fn spawn(args: &[&str], cwd: &Path, home: &IsolatedHome) -> Self {
        let addr = free_addr();
        let owned: Vec<String> = args.iter().map(|a| a.replace("{addr}", &addr)).collect();
        let borrowed: Vec<&str> = owned.iter().map(String::as_str).collect();
        let mut child = spawn(&borrowed, cwd, home);
        let lines = stderr_lines(&mut child);
        Self { child, lines }
    }

    /// Block until a stderr line satisfies `pred`, or the child exits first.
    fn wait_for_line(&self, pred: impl Fn(&str) -> bool) -> Option<String> {
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            match self.lines.recv_timeout(Duration::from_millis(250)) {
                Ok(line) => {
                    if pred(&line) {
                        return Some(line);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                // stderr closed: the child is on its way out and will print no more.
                Err(RecvTimeoutError::Disconnected) => return None,
            }
        }
        None
    }

    /// The `listening on` line, plus the address it names — failing loudly if the
    /// server never got there, because every caller's assertions assume it did.
    fn wait_for_listening(&self) -> (String, String) {
        let line = self
            .wait_for_line(|l| l.contains(" listening on http://"))
            .expect("the server never reported a listening address");
        let rest = line
            .split_once(" listening on http://")
            .expect("listening line")
            .1;
        let addr = rest
            .split(['/', ' '])
            .next()
            .expect("address in listening line")
            .to_owned();
        (addr, line)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

fn spawn(args: &[&str], cwd: &Path, home: &IsolatedHome) -> Child {
    let mut command = Command::new(BIN);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    home.apply(&mut command);
    command
        .spawn()
        .unwrap_or_else(|e| panic!("spawn roteiro {args:?}: {e}"))
}

/// Block until `child` exits, or `grace` elapses — `None` meaning it is still
/// running.
///
/// Exists because "stderr closed" and "the process exited" are two events and
/// arrive in that order, so anything that treats the first as the second is a
/// race. Both callers here need the distinction: one classifies a refusal, the
/// other asserts one.
fn wait_for_exit(child: &mut Child, grace: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + grace;
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return Some(status);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Pump the child's stderr into a channel on a thread, so a caller can wait for a
/// line without blocking on a pipe that may never close.
fn stderr_lines(child: &mut Child) -> Receiver<String> {
    let stderr = child.stderr.take().expect("piped stderr");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

/// Run a command that is supposed to **refuse**, with a deadline, returning
/// `(exit status, stderr)`.
///
/// # Why not `Command::output()`
///
/// Because a test that pins "this refuses" has to go *red* when it stops
/// refusing, and `output()` reads both pipes to EOF — so against a command that
/// successfully started a server it blocks until the server is killed, which
/// nothing here would do. The test would hang, not fail, and a hung test in CI
/// reads as an infrastructure problem rather than as the regression it is.
///
/// Found the honest way: injecting "delete `run_explorer`'s `no workspaces to
/// serve` bail" made this test hang for thirty minutes instead of failing in two
/// seconds.
fn run_refusing(
    args: &[&str],
    cwd: &Path,
    home: &IsolatedHome,
) -> (std::process::ExitStatus, String) {
    let mut child = spawn(args, cwd, home);
    let lines = stderr_lines(&mut child);
    // Wrapped so the child is killed even if an assertion below unwinds.
    let mut server = Server { child, lines };
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut stderr = String::new();
    loop {
        while let Ok(line) = server.lines.try_recv() {
            stderr.push_str(&line);
            stderr.push('\n');
        }
        if let Some(status) = server.child.try_wait().expect("try_wait") {
            // Drain whatever the reader thread still holds before reporting.
            while let Ok(line) = server.lines.recv_timeout(Duration::from_millis(200)) {
                stderr.push_str(&line);
                stderr.push('\n');
            }
            return (status, stderr);
        }
        assert!(
            Instant::now() < deadline,
            "`roteiro {args:?}` was expected to refuse and exit; it is still \
             running after 90s, which means it started a server. stderr so far:\n\
             {stderr}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A loopback address nothing is listening on, by binding port 0 and releasing
/// it. Racy in principle; a fixed port collides between concurrent test binaries
/// for certain.
fn free_addr() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    drop(listener);
    format!("127.0.0.1:{}", addr.port())
}

// ---------------------------------------------------------------------------
// HTTP helpers — a hand-rolled GET, because this test binary carries no client
// ---------------------------------------------------------------------------

/// `(status, location, body)` for one GET, without following redirects: the
/// redirect itself is part of what distinguishes the two modes.
fn http_get(addr: &str, path: &str) -> (u16, String, String) {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut stream = loop {
        match TcpStream::connect(addr) {
            Ok(s) => break s,
            Err(e) if Instant::now() < deadline => {
                let _ = e;
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("connect {addr}: {e}"),
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .expect("read timeout");
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
    )
    .expect("write request");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read response");
    let raw = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = raw
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("malformed HTTP response from {addr}{path}: {raw}"));
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .unwrap_or_else(|| panic!("no status code in: {head}"));
    let location = head
        .lines()
        .find_map(|l| {
            l.strip_prefix("location: ")
                .or_else(|| l.strip_prefix("Location: "))
        })
        .unwrap_or_default()
        .trim()
        .to_owned();
    (status, location, body.to_owned())
}

/// The `(label, origin)` of every row the `/okf` chooser lists, in page order.
#[cfg(feature = "okf-viewer")]
fn chooser_rows(addr: &str) -> Vec<(String, PathBuf)> {
    let (status, location, body) = http_get(addr, "/okf");
    assert_eq!(
        status, 200,
        "`/okf` should list the mounts; it answered {status} (location: \
         `{location}`). A single mount redirects instead — this fixture has more."
    );
    let mut rows = Vec::new();
    for row in body.split("<li>").skip(1) {
        let label = between(row, "\">", "</a>");
        let origin = between(row, "<span class=\"deg\">", "</span>");
        rows.push((unescape(&label), canon(Path::new(&unescape(&origin)))));
    }
    assert!(!rows.is_empty(), "no chooser rows parsed out of: {body}");
    rows
}

/// Resolve a path the way a comparison here has to: macOS puts the scratch
/// directories under a symlinked `/var` → `/private/var`, and a mount's origin is
/// canonical or not depending on **which half of `okf_mounts` produced it** — the
/// cwd block goes through `std::env::current_dir()` (resolved), the workspace walk
/// through the configured root as written (not resolved). That difference is a
/// property of this machine's temp directory, not of the code under test, so it is
/// normalised away rather than pinned.
#[cfg(feature = "okf-viewer")]
fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Where `/okf` redirects when exactly one bundle is mounted, or `None` when it
/// answered with a page instead.
#[cfg(feature = "okf-viewer")]
fn single_mount_redirect(addr: &str) -> Option<String> {
    let (status, location, _) = http_get(addr, "/okf");
    (status == 307).then_some(location)
}

/// The bundle labels a `serve_okf_only` startup line names, or `None` when the
/// line is not that mode's.
#[cfg(feature = "okf-viewer")]
fn bundles_only_labels(line: &str) -> Option<Vec<String>> {
    let tail = line.split_once(" OKF bundle(s) only: ")?.1;
    Some(tail.split(", ").map(str::to_owned).collect())
}

/// `GET /v1/graph/projects` → the hosted project names. Hand-parsed: this test
/// binary carries no JSON dependency and the shape is fixed by `graph_api`'s own
/// tests.
fn graph_projects(addr: &str) -> Vec<String> {
    json_string_array(&http_get(addr, "/v1/graph/projects").2, "\"projects\":[")
}

/// The project names of one **nested** workspace route.
fn projects_of(addr: &str, workspace: &str) -> Vec<String> {
    let body = http_get(addr, &format!("/v1/graph/workspaces/{workspace}/projects")).2;
    json_string_array(&body, "\"projects\":[")
}

/// Every workspace `/v1/graph/workspaces` advertises, in served order.
fn workspace_names(addr: &str) -> Vec<String> {
    let body = http_get(addr, "/v1/graph/workspaces").2;
    body.split("\"name\":\"")
        .skip(1)
        .filter_map(|s| s.split('"').next())
        .map(str::to_owned)
        .collect()
}

fn json_string_array(body: &str, key: &str) -> Vec<String> {
    let start = body
        .find(key)
        .unwrap_or_else(|| panic!("no `{key}` in {body}"))
        + key.len();
    let end = start
        + body[start..]
            .find(']')
            .unwrap_or_else(|| panic!("unterminated array after `{key}` in {body}"));
    body[start..end]
        .split(',')
        .map(|s| s.trim().trim_matches('"').to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(feature = "okf-viewer")]
fn between(haystack: &str, open: &str, close: &str) -> String {
    let start = haystack
        .find(open)
        .unwrap_or_else(|| panic!("no `{open}` in {haystack}"))
        + open.len();
    let end = start
        + haystack[start..]
            .find(close)
            .unwrap_or_else(|| panic!("no `{close}` after `{open}` in {haystack}"));
    haystack[start..end].to_owned()
}

/// Undo the viewer's HTML escaping, so a label or a path compares as written.
#[cfg(feature = "okf-viewer")]
fn unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

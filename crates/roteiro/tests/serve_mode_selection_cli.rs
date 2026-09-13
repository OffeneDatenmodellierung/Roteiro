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
    /// For a [`Mode::Refuses`] cell, a fragment the diagnostic must contain.
    ///
    /// Without it the cell says only "exited non-zero", which any early failure
    /// satisfies — a bad address, an unreadable config, a panic — so the cell
    /// would stop pinning *which* refusal this is.
    refusal: Option<&'static str>,
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
        "explorer",
        &[
            Cell {
                what: "no config, inside a repo, no bundle → the cwd repo alone",
                config: false,
                repo: true,
                bundle: false,
                // Named after the directory, by `explorer_cwd_set` — and this is
                // the observation that separates the fallback from the
                // configured cells below, and from `serve`'s `default`.
                mode: graph(&["cell0"]),
                refusal: None,
            },
            Cell {
                what: "no config, no repo, no bundle → nothing to serve",
                config: false,
                repo: false,
                bundle: false,
                mode: Mode::Refuses,
                refusal: Some("Could not find a git repository"),
            },
            Cell {
                what: "config, inside a repo, no bundle → the configured set (the \
                       cwd repo is NOT added)",
                config: true,
                repo: true,
                bundle: false,
                mode: graph(&["one", "two"]),
                refusal: None,
            },
            Cell {
                what: "config, no repo, no bundle → the configured set; a repo is \
                       not required at all",
                config: true,
                repo: false,
                bundle: false,
                mode: graph(&["one", "two"]),
                refusal: None,
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
        "explorer",
        &[
            Cell {
                what: "no config, inside a repo, a bundle → still the graph; \
                       `Repo::discover` succeeds so the fallback arm is unreachable",
                config: false,
                repo: true,
                bundle: true,
                mode: graph(&["cell0"]),
                refusal: None,
            },
            Cell {
                what: "no config, no repo, a bundle → bundles only (this is what \
                       `roteiro okf view <path>` became)",
                config: false,
                repo: false,
                bundle: true,
                mode: Mode::BundlesOnly,
                refusal: None,
            },
            Cell {
                what: "config, inside a repo, a bundle → the configured set",
                config: true,
                repo: true,
                bundle: true,
                mode: graph(&["one", "two"]),
                refusal: None,
            },
            Cell {
                what: "config, no repo, a bundle → the configured set, NOT bundles \
                       only: config outranks the bundle fallback",
                config: true,
                repo: false,
                bundle: true,
                mode: graph(&["one", "two"]),
                refusal: None,
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
        "explorer",
        &[Cell {
            what: "no config, no repo, a bundle, no `okf-viewer` → refuses; there \
                   is no fallback compiled in",
            config: false,
            repo: false,
            bundle: true,
            mode: Mode::Refuses,
            refusal: Some("Could not find a git repository"),
        }],
    );
}

/// **The same question asked of `roteiro serve`.**
///
/// `serve` and `explorer` decide what to serve in two different functions —
/// `build_serve_workspaces` and `run_explorer` — and a `--scope` flag will touch
/// both. The explorer tables above cannot speak for `serve`: its fallback is
/// guarded differently (`resolved.is_empty() && workspace_roots.is_empty() &&
/// workspace_name.is_none()`), it names the single-repo workspace `default`
/// rather than after the directory, and it has no bundles-only mode at all.
///
/// No bundle column here, because there is nothing for it to select: `serve`
/// never reaches `serve_okf_only`. What bundles do to `serve` is a mount, and
/// that is pinned by `a_project_with_no_known_repository_root_contributes_no_mount`.
///
/// These cells run with no model installed (the isolated home has none), so
/// `run_serve_network` degrades to the same llama-free graph server `explorer`
/// serves — which is what makes the two comparable at all.
#[test]
fn serve_mode_truth_table() {
    run_matrix(
        "serve-modes",
        "serve",
        &[
            Cell {
                what: "no config, inside a repo → the cwd repo alone, hosted as \
                       `default` (NOT named after the directory, as `explorer` \
                       names it)",
                config: false,
                repo: true,
                bundle: false,
                mode: graph(&["default"]),
                refusal: None,
            },
            Cell {
                what: "no config, no repo → nothing to serve; the single-repo \
                       fallback needs a git cwd and says so",
                config: false,
                repo: false,
                bundle: false,
                mode: Mode::Refuses,
                refusal: Some("Could not find a git repository"),
            },
            Cell {
                what: "config, inside a repo → the configured set; the cwd repo is \
                       NOT added, exactly as for `explorer`",
                config: true,
                repo: true,
                bundle: false,
                mode: graph(&["one", "two"]),
                refusal: None,
            },
            Cell {
                what: "config, no repo → the configured set; `serve` needs no repo \
                       once config selects one",
                config: true,
                repo: false,
                bundle: false,
                mode: graph(&["one", "two"]),
                refusal: None,
            },
        ],
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
    let base = Scratch::new("explorer-empty-config");
    std::fs::create_dir_all(&base.path).expect("mkdir base");
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
/// **canonical** root — and the workspace-derived label is the one that
/// survives, because the cwd block is admitted last.
///
/// The current directory is very often also a hosted project; mounting the same
/// bundle twice under two names would leave a reader wondering which is
/// authoritative.
///
/// # Why the fixture goes through a symlink
///
/// Standing inside a configured repo reaches the same bundle twice, but by the
/// *same spelling* — and a dedup keyed on the raw path string passes that just
/// as happily as one keyed on `canonicalize`. So workspace `one`'s root is a
/// symlink here: the walk yields `<base>/wsA-link/alpha/okf` and the cwd yields
/// `<base>/wsA/alpha/okf`. Same directory, two strings, and only
/// `okf_root_key`'s canonicalisation collapses them.
///
/// Unix-only for the symlink. The property is not platform-specific; the
/// cheapest way to produce two spellings of one directory is.
#[test]
#[cfg(all(unix, feature = "okf-viewer"))]
fn a_bundle_reachable_twice_is_mounted_once() {
    let fx = TwoWorkspaces::via_symlink("okf-dedup");
    // Stand inside `alpha` by its REAL path, while config reaches it through the
    // link — so the two admissions disagree on the spelling and agree on the
    // directory.
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
    let base = Scratch::new("okf-cwd-readings");
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
}

/// A project whose workspace knows **no repository root** contributes no mount,
/// however plainly a repository is sitting there — only the cwd block can speak
/// for it.
///
/// The name matters, because the earlier one ("no repository on disk") described
/// something this fixture does not do: `make_repo` creates a real git repository
/// and `serve` is run inside it. What is missing is not the repository but the
/// *workspace's knowledge* of it. `serve`'s single-repo fallback builds
/// `Workspace::single` from an already-open store, so `project_root` returns
/// `None` for that project and `okf_mounts` skips it — which is why the startup
/// note reads `okf` (the cwd block, labelled by directory name) and never
/// `default/solo`.
///
/// `okf_mounts` treats that skip as the ordinary shape of a `--db` project
/// rather than a fault, and the same `None` arrives here by a different route.
/// Pinned through the route a person can actually reach from the CLI.
#[test]
#[cfg(feature = "okf-viewer")]
fn a_project_with_no_known_repository_root_contributes_no_mount() {
    let base = Scratch::new("okf-no-repo-root");
    let repo = base.join("solo");
    make_repo(&repo);
    write_bundle(&repo.join("okf"));
    let home = IsolatedHome::new("okf-no-repo-root");

    // `serve` with no config and no `--workspace` hosts the cwd repo via
    // `Workspace::single` — a project built from an open store, so it carries no
    // `project_root` even though `repo` below is a real repository.
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
    let base = Scratch::new("okf-empty");
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
    let base = Scratch::new("serve-workspace-root");
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
        &["serve", "--workspace", utf8_arg(&root), "--addr", "{addr}"],
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
    let base = Scratch::new("serve-w-no-config");
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
}

// ---------------------------------------------------------------------------
// Matrix driver
// ---------------------------------------------------------------------------

/// Run every cell and report **all** mismatches at once: a truth table read one
/// failure at a time hides how much a refactor moved.
fn run_matrix(label: &str, cmd: &'static str, cells: &[Cell]) {
    let base = Scratch::new(label);
    std::fs::create_dir_all(&base.path).expect("mkdir matrix base");
    // The configured set, built once and shared: no cell mutates it.
    let cfg = TwoWorkspaces::new(&format!("{label}-cfg"));
    // A home with no config at all, for the `config: false` cells.
    let unconfigured = IsolatedHome::new(&format!("{label}-none"));

    let mut failures: Vec<String> = Vec::new();
    for (i, cell) in cells.iter().enumerate() {
        let cwd = base.join(&format!("cell{i}"));
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
        let (observed, stderr) = observe_mode(cell.what, cmd, &cwd, home);
        if let Some(expected) = cell.refusal
            && observed == Mode::Refuses
            && !stderr.contains(expected)
        {
            failures.push(format!(
                "  cell {i}: {}\n    refused, but not with the pinned \
                 diagnostic\n    expected to contain {expected:?}\n    got: {}",
                cell.what,
                stderr.trim()
            ));
            continue;
        }
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
        "`roteiro {cmd}` selected a different server than this table records. \
         These are characterisation tests: if the change is intended, move the \
         table and say which cell moved and why.\n{}",
        failures.join("\n")
    );
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
fn observe_mode(what: &str, cmd: &'static str, cwd: &Path, home: &IsolatedHome) -> (Mode, String) {
    let addr = free_addr();
    let mut child = spawn(&[cmd, "--addr", &addr], cwd, home);
    let lines = stderr_lines(&mut child);
    let mut server = Server { child, lines };

    let mut stderr = String::new();
    let Some(line) = server.wait_for_line(|l| l.contains(" listening on http://"), &mut stderr)
    else {
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
                    "{what}: `roteiro {cmd}` printed no listening line and was \
                     STILL RUNNING 30s later — that is a hang, not a refusal. \
                     Reporting it as `Refuses` would let a startup deadlock \
                     satisfy this cell."
                )
            });
        assert!(
            !status.success(),
            "{what}: `roteiro {cmd}` exited SUCCESSFULLY without ever \
             listening ({status:?}). A refusal cell means a non-zero exit, not \
             merely the absence of a server."
        );
        stderr.push_str(&stderr_of(&server));
        // The drain can surface a startup line the scan above never saw — one
        // written just before a non-zero exit. Unchecked, that start-then-exit
        // would be classified `Refuses` and satisfy a refusal cell, which is the
        // same hole `run_refusing` had one commit earlier; it was fixed there and
        // not carried across to here.
        assert!(
            !stderr.contains(" listening on http://"),
            "{what}: `roteiro {cmd}` STARTED A SERVER and then exited \
             ({status:?}). That is not a refusal, and classifying it as one would \
             let this cell pass on a server that came up.\n{stderr}"
        );
        return (Mode::Refuses, stderr);
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
            (Mode::Graph(workspace_names(&addr)), String::new())
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
            (Mode::BundlesOnly, String::new())
        }
        _ => panic!("{what}: unrecognised startup line — a new mode? got: {line}"),
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A scratch tree that removes itself, **after** the servers standing in it.
///
/// Rust drops bindings in reverse declaration order, so a test that declares its
/// `Scratch` first and its `Server`s later gets the right order for free: every
/// child is killed, then the tree goes. That ordering is the whole point.
/// Removing a directory a live process is sitting in fails on Windows, and the
/// `.ok()` these call sites used would swallow the failure and leak the fixture
/// on every *successful* run.
///
/// Written as a guard rather than a `drop(server)` before each cleanup because
/// the manual version had already been got wrong: it was added at one of the
/// three sites that needed it and forgotten at the other two. A rule that must
/// be remembered at every call site is a rule that will be missed at one.
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new(label: &str) -> Self {
        let path = scratch_dir(label);
        Self { path }
    }

    fn join(&self, rel: &str) -> PathBuf {
        self.path.join(rel)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.path).ok();
    }
}

/// Two named workspaces, one repo each, each carrying a rendered bundle — so
/// `{ws}/` in the mount label is load-bearing and a dropped workspace is visible.
///
#[cfg_attr(
    not(feature = "okf-viewer"),
    allow(
        dead_code,
        reason = "`alpha`/`beta` are the repository paths the mount assertions \
                  compare origins against, so only the `okf-viewer` tests read \
                  them. Allowed rather than `cfg`-gated per field: the fixture is \
                  one thing in both builds, and splitting it would put a `#[cfg]` \
                  on two fields and both their initialisers to silence a lint \
                  about a build that simply asks less of it."
    )
)]
struct TwoWorkspaces {
    base: PathBuf,
    alpha: PathBuf,
    beta: PathBuf,
    home: IsolatedHome,
}

impl TwoWorkspaces {
    fn new(label: &str) -> Self {
        Self::build(label, false)
    }

    /// The same fixture, but with workspace `one`'s root reached through a
    /// **symlink**, so the workspace walk spells `alpha`'s bundle
    /// `<base>/wsA-link/alpha/okf` while the cwd block spells it
    /// `<base>/wsA/alpha/okf`. Two spellings, one directory — which is the only
    /// arrangement that can tell a canonical dedup key from a raw one.
    // Gated exactly as its one caller is: the dedup test is `all(unix,
    // okf-viewer)`, so a `--features explorer` build has no use for this and
    // would otherwise report it as dead.
    #[cfg(all(unix, feature = "okf-viewer"))]
    fn via_symlink(label: &str) -> Self {
        Self::build(label, true)
    }

    fn build(label: &str, symlink_root_a: bool) -> Self {
        let base = scratch_dir(label);
        let alpha = base.join("wsA").join("alpha");
        let beta = base.join("wsB").join("beta");
        make_repo(&alpha);
        make_repo(&beta);
        write_bundle(&alpha.join("okf"));
        write_bundle(&beta.join("okf"));
        let home = IsolatedHome::new(label);
        let mut root_a = base.join("wsA");
        if symlink_root_a {
            let link = base.join("wsA-link");
            #[cfg(unix)]
            std::os::unix::fs::symlink(&root_a, &link).expect("symlink wsA");
            root_a = link;
        }
        let root_b = base.join("wsB");
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
        // Refused rather than lossily converted. TOML is UTF-8 by definition and
        // roteiro's `roots` is a `Vec<String>`, so a path that is not valid UTF-8
        // cannot be expressed in this config at *all* — and `display()` would
        // quietly substitute replacement characters, pointing the workspace at a
        // directory that does not exist while the test read as a behaviour
        // failure. There is nothing to fix in the escaping here; the fixture is
        // simply unrepresentable, and it should say so.
        let root = root.to_str().unwrap_or_else(|| {
            panic!(
                "fixture path is not valid UTF-8 and so cannot be written into \
                 TOML at all: {}. `std::env::temp_dir()` is presumably rooted \
                 somewhere unusual — this is a limit of the config format, not \
                 of the escaping below.",
                root.display()
            )
        });
        // An escaped basic string, rather than either quoting style used raw. A
        // *basic* string left unescaped breaks on Windows, where
        // `Path::display()` yields `C:\\Users\\…` and `\\U` is not a valid
        // escape; a *literal* string cannot represent an apostrophe, and
        // `std::env::temp_dir()` is rooted wherever the environment says, so
        // `/tmp/o'brien/…` is a legal home for it. Escaping assumes nothing
        // about either — see `toml_escape`, which also handles the control
        // characters a basic string rejects.
        //
        // The failure is loud, not silent: `config::load` treats malformed TOML
        // as a hard error and never a silent partial parse
        // (`crates/roteiro/src/config.rs:2143-2144`), so the child exits with a
        // parse error and the cells fail rather than quietly falling back to the
        // no-config path. Measured: under `TMPDIR=/private/tmp/o'brien dir/` a
        // literal-string version failed 7 of the 12 tests here.
        let _ = write!(
            toml,
            "[[workspaces]]\nname = \"{name}\"\nroots = [\"{}\"]\n\n",
            toml_escape(root)
        );
    }
    std::fs::write(home.path().join("config.toml"), toml).expect("write config.toml");
}

/// Escape a value for a TOML **basic** string.
///
/// Backslash and quote are the obvious two. The rest are not decoration: a basic
/// string also rejects raw control characters other than tab, and Unix permits
/// any byte but `/` and NUL in a directory name — so a `temp_dir()` containing a
/// newline is unusual, not impossible, and "unusual" is the assumption this
/// helper exists to stop making.
fn toml_escape(raw: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            // Everything else TOML calls a control character, in the `\uXXXX`
            // form the grammar names for exactly this.
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                let _ = write!(out, "\\u{:04X}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
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

/// A fixture path as a command-line argument, refusing rather than mangling.
///
/// The same limit `write_config` runs into, by a different route: `--workspace`
/// is a `Vec<String>` in the CLI, so roteiro cannot take a non-UTF-8 root there
/// either. `display()` would hand the child a *different* path — replacement
/// characters for the invalid bytes — and the resulting "alpha not found" would
/// read as a defect in what is being pinned rather than as a fixture that could
/// never have been expressed.
fn utf8_arg(path: &Path) -> &str {
    path.to_str().unwrap_or_else(|| {
        panic!(
            "fixture path is not valid UTF-8, so it cannot be passed as \
             `--workspace` at all: {}. That is a limit of the CLI's `Vec<String>`, \
             not something this test can escape around.",
            path.display()
        )
    })
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

    /// Block until a stderr line satisfies `pred`, or the child exits first,
    /// **keeping every line it read** in `sink`.
    ///
    /// The sink is not bookkeeping: the lines this scan discards on its way past
    /// are the diagnostic, and a caller that then asks "why did it refuse?" finds
    /// the channel already drained. That produced an empty `got:` in a refusal
    /// assertion the first time it was asked for.
    fn wait_for_line(&self, pred: impl Fn(&str) -> bool, sink: &mut String) -> Option<String> {
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            match self.lines.recv_timeout(Duration::from_millis(250)) {
                Ok(line) => {
                    sink.push_str(&line);
                    sink.push('\n');
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
        let mut seen = String::new();
        let line = self
            .wait_for_line(|l| l.contains(" listening on http://"), &mut seen)
            .unwrap_or_else(|| {
                panic!("the server never reported a listening address. stderr:\n{seen}")
            });
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

/// Everything the reader thread still holds, drained until it is gone.
///
/// Called once the child has exited, so the sender is already on its way out;
/// the bound is a backstop, not a timeout anyone should reach.
fn stderr_of(server: &Server) -> String {
    let mut out = String::new();
    let drain_by = Instant::now() + Duration::from_secs(10);
    loop {
        match server.lines.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => {
                out.push_str(&line);
                out.push('\n');
            }
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) if Instant::now() >= drain_by => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
    out
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
        // Fail *fast* the moment it starts serving, instead of spending the whole
        // deadline discovering it. `serve_with_a_workspace_name_and_no_config…`
        // pins a defect and is expected to go red when that defect is fixed —
        // three cases each burning 90s would turn a legible "this now starts"
        // into a four-and-a-half-minute timeout that reads like CI trouble.
        assert!(
            !stderr.contains(" listening on http://"),
            "`roteiro {args:?}` was expected to refuse, and it STARTED A SERVER. \
             If that is the fix, this test is the changelog entry for it.\n{stderr}"
        );
        if let Some(status) = server.child.try_wait().expect("try_wait") {
            // Drain until the reader thread is *gone*, not until it pauses. A
            // `recv_timeout` window treats a slow scheduler as end-of-output, so
            // under load the diagnostic this function exists to return can be the
            // part that goes missing. Disconnected means the thread hit EOF and
            // dropped the sender, which is the only honest end.
            let drain_by = Instant::now() + Duration::from_secs(10);
            loop {
                match server.lines.recv_timeout(Duration::from_millis(100)) {
                    Ok(line) => {
                        stderr.push_str(&line);
                        stderr.push('\n');
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                    Err(RecvTimeoutError::Timeout) if Instant::now() >= drain_by => break,
                    Err(RecvTimeoutError::Timeout) => {}
                }
            }
            // Re-checked after draining, not only before: the lines that arrive
            // between the last poll and the exit are exactly the ones a
            // start-then-exit would be hiding in, and accepting that as a refusal
            // is the contract failing quietly.
            assert!(
                !stderr.contains(" listening on http://"),
                "`roteiro {args:?}` was expected to refuse, and it STARTED A \
                 SERVER before exiting {status:?}. If that is the fix, this test \
                 is the changelog entry for it.\n{stderr}"
            );
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
///
/// **One pass, not a chain of `replace`.** The viewer encodes `&` first, so a
/// path that genuinely contains the text `&lt;` reaches the page as `&amp;lt;`;
/// decoding `&amp;` and then `&lt;` turns it into `<` and reports a mismatch
/// against a path that was never wrong. Scanning once and consuming each entity
/// whole cannot re-read its own output.
#[cfg(feature = "okf-viewer")]
fn unescape(s: &str) -> String {
    const ENTITIES: &[(&str, char)] = &[
        ("&amp;", '&'),
        ("&lt;", '<'),
        ("&gt;", '>'),
        ("&quot;", '"'),
        ("&#39;", '\''),
    ];
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    'outer: while !rest.is_empty() {
        let Some(i) = rest.find('&') else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        for (entity, decoded) in ENTITIES {
            if let Some(tail) = rest.strip_prefix(entity) {
                out.push(*decoded);
                rest = tail;
                continue 'outer;
            }
        }
        // A bare `&` the viewer did not write: pass it through and move past it,
        // so the scan always advances.
        out.push('&');
        rest = &rest[1..];
    }
    out
}

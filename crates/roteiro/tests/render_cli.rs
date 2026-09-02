// roteiro:ignore-file — the fixtures below deliberately embed `TODO`/`FIXME` to
// exercise the detector; they are test data, not real debt in this repo.
//! End-to-end tests for `roteiro render`: drives the real binary against a
//! fixture repo and checks each target's output.
//!
//! `render docs` — the site is produced with themed ADR pages, an index, and the
//! copied static assets.
//!
//! `render okf` — an Open Knowledge Format bundle (v0.2): concept documents with
//! YAML frontmatter, nested by kind and by workspace member, plus the reserved
//! `index.md`. It replaced the Obsidian vault in 4.0.0.
//!
//! The property these tests exist for is **losslessness**. The vault wrote one
//! flat directory and lost 104 notes of 8,144 to filesystem name folding, and the
//! count it printed did not know — so the assertions here are that the number
//! printed equals the number of files written, and that two concepts which want
//! the same name both survive. Asserting that two paths differ would not have
//! caught the original defect.

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

/// Run git as a named person at a fixed instant — for any command that writes a
/// commit, `commit` and `merge` alike.
///
/// Both halves are the point: the fixture has to distinguish *who* touched
/// *which* document, and it has to date each commit so the bundle's timestamps
/// can be asserted literally rather than compared against a clock.
fn git_as(dir: &Path, who: &str, when: &str, args: &[&str]) {
    let status = Command::new("git")
        .args([
            "-c",
            &format!("user.name={who}"),
            "-c",
            &format!("user.email={}@example.com", who.to_ascii_lowercase()),
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .env("GIT_AUTHOR_DATE", when)
        .env("GIT_COMMITTER_DATE", when)
        .current_dir(dir)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} as {who} failed");
}

/// Commit the index as a named person at a fixed instant.
fn commit_as(dir: &Path, who: &str, when: &str, message: &str) {
    git_as(dir, who, when, &["commit", "-q", "-m", message]);
}

fn write(dir: &Path, rel: &str, content: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir");
    std::fs::write(path, content).expect("write");
}

/// A fixture directory of its own per test: cargo runs the tests in this file as
/// threads of one process, so the pid alone would have them share — and delete —
/// each other's repository.
fn fresh_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("roteiro-render-cli-{tag}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

#[test]
fn render_docs_builds_site_from_adrs_and_assets() {
    let dir = fresh_dir("docs");
    git(&dir, &["init", "-q"]);
    // Minimal static assets + one ADR.
    write(&dir, "website/public/style.css", "body{color:#111}\n");
    write(&dir, "website/public/index.html", "<h1>Home</h1>\n");
    write(&dir, "website/public/favicon.svg", "<svg/>\n");
    write(
        &dir,
        "docs/adr/0001-example.md",
        "---\nadr-id: \"0001\"\nstatus: Accepted\n---\n\n# ADR-0001: Example\n\n## Context\n\n| a | b |\n|---|---|\n| 1 | 2 |\n",
    );
    write(&dir, "docs/adr/README.md", "index, not an ADR\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    let out = Command::new(BIN)
        .args(["render", "docs", "--out", "site"])
        .current_dir(&dir)
        .output()
        .expect("run render");
    assert!(out.status.success(), "render failed: {out:?}");

    let site = dir.join("site");
    // Static assets copied.
    assert!(site.join("style.css").exists());
    assert!(site.join("index.html").exists());
    assert!(site.join("favicon.svg").exists());

    // ADR page rendered and themed; README skipped.
    let page = std::fs::read_to_string(site.join("adr/0001-example.html")).expect("adr page");
    assert!(page.starts_with("<!doctype html>"));
    assert!(page.contains("<h1 id=\"adr-0001-example\">ADR-0001: Example</h1>"));
    assert!(page.contains("<table>"), "GFM table should render");
    assert!(!page.contains("adr-id"), "frontmatter should be stripped");
    assert!(page.contains("← Back to roteiro.dev"));
    assert!(
        !site.join("adr/README.html").exists(),
        "README is not an ADR page"
    );

    // Index lists the ADR by title.
    let index = std::fs::read_to_string(site.join("adr/index.html")).expect("index");
    assert!(index.contains("<a href=\"0001-example.html\">ADR-0001: Example</a>"));

    std::fs::remove_dir_all(&dir).ok();
}

/// Stands in for the destination of the current page's entry, which is an
/// unlinked `<span aria-current="page">` and so has none.
///
/// A sentinel rather than a skipped entry: "this one is deliberately not a link"
/// is the whole of the you-are-here marker, and dropping it would leave a hole in
/// the comparison exactly where that marker lives.
const CURRENT_PAGE: &str = "<marked current, not a link>";

/// The ordered `(label, destination)` pairs of a page's site bar.
///
/// Both halves, because where a link *goes* is most of what a navigation bar is.
/// An earlier version of this compared labels alone and was blind to the
/// destinations — it passed against a landing page whose `Modes` entry pointed at
/// a page that does not exist, which is precisely the defect this whole change
/// set exists to prevent (issue #446 was five links correct where they were
/// written and broken where they were served).
fn bar_entries(html: &str) -> Vec<(String, String)> {
    let Some(start) = html.find("<nav class=\"sitenav\">") else {
        return Vec::new();
    };
    let bar = &html[start..];
    let bar = &bar[..bar.find("</nav>").unwrap_or(bar.len())];

    let mut entries = Vec::new();
    let mut rest = bar;
    // Walk `<tag …>text` pairs. The bar is one element per line on the
    // hand-written landing page and all on one line when rendered, so this reads
    // the markup rather than the layout.
    while let Some(open) = rest.find('<') {
        let Some((tag, after)) = rest[open + 1..].split_once('>') else {
            break;
        };
        let label: String = after.chars().take_while(|c| *c != '<').collect();
        let label = label.trim().to_owned();
        if !label.is_empty() {
            let dest = tag
                .split_once("href=\"")
                .and_then(|(_, r)| r.split_once('"'))
                .map_or_else(|| CURRENT_PAGE.to_owned(), |(href, _)| href.to_owned());
            entries.push((label, dest));
        }
        rest = after;
    }
    entries
}

/// [`bar_entries`], with the marked entry resolved to `own_href` — the one
/// destination a page cannot state about itself, supplied by the caller that
/// knows which page it is reading.
///
/// Asserts the marker appears exactly once on the way through: a bar that marks
/// no page, or two, is broken in its own right and would otherwise be normalised
/// into looking fine.
fn bar_as_seen_from(html: &str, own_href: &str) -> Vec<(String, String)> {
    let entries = bar_entries(html);
    let marked = entries
        .iter()
        .filter(|(_, dest)| dest == CURRENT_PAGE)
        .count();
    assert_eq!(
        marked, 1,
        "the bar on {own_href} marks {marked} pages as current, expected exactly 1: {entries:?}"
    );
    entries
        .into_iter()
        .map(|(label, dest)| {
            if dest == CURRENT_PAGE {
                (label, own_href.to_owned())
            } else {
                (label, dest)
            }
        })
        .collect()
}

#[test]
fn a_declared_site_page_is_emitted_with_the_shared_bar() {
    let dir = fresh_dir("sitepage");
    git(&dir, &["init", "-q"]);
    write(&dir, "website/public/style.css", "body{color:#111}\n");
    write(&dir, "website/public/index.html", "<h1>Home</h1>\n");
    write(
        &dir,
        "docs/adr/0001-example.md",
        "---\nadr-id: \"0001\"\nstatus: Accepted\n---\n\n# ADR-0001: Example\n",
    );
    // Publication is a frontmatter marker, not a path, so these three sit in
    // three different places on purpose.
    write(
        &dir,
        "website/pages/modes.md",
        "---\nsite-page: modes\nsite-nav: Modes\nsite-order: 2\n---\n\n\
         # The five ways to run it {#modes}\n\n## Offline mode\n\nNo models, no network.\n",
    );
    write(
        &dir,
        "docs/GUIDE.md",
        "---\nsite-page: guide\nsite-nav: Guide\nsite-order: 1\n---\n\n\
         # A guide\n\nSequenced in [the plan](BUILD_PLAN_V2.md).\n",
    );
    write(
        &dir,
        "docs/history/BUILD_PLAN_V2.md",
        "---\nsite-page: build-plan-v2\nsite-nav: Roadmap\nsite-order: 3\n---\n\n# Roadmap\n",
    );
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    let out = Command::new(BIN)
        .args(["render", "docs", "--out", "site"])
        .current_dir(&dir)
        .output()
        .expect("run render");
    assert!(out.status.success(), "render failed: {out:?}");
    let site = dir.join("site");

    // Each declared page is published as `<slug>.html` — the slug is the URL.
    let modes = std::fs::read_to_string(site.join("modes.html")).expect("modes page");
    assert!(
        site.join("guide.html").exists(),
        "docs/GUIDE.md → guide.html"
    );
    assert!(site.join("build-plan-v2.html").exists());

    // The anchor the section carried out of the landing page still lands.
    assert!(
        modes.contains("id=\"modes\""),
        "explicit anchor preserved: {modes}"
    );
    assert!(!modes.contains("site-page"), "frontmatter is not content");

    // One bar, in `site-order`, identical on every page and marking the current.
    // The destinations are asserted as well as the labels: they are where the
    // declared slug shows up in the navigation, so `docs/GUIDE.md → guide.html`
    // is pinned here and not merely in the file names on disk.
    let expected: Vec<(String, String)> = [
        ("Home", "./"),
        ("Guide", "guide.html"),
        ("Modes", "modes.html"),
        ("Roadmap", "build-plan-v2.html"),
    ]
    .into_iter()
    .map(|(label, dest)| (label.to_owned(), dest.to_owned()))
    .collect();
    for page in ["modes.html", "guide.html", "build-plan-v2.html"] {
        let html = std::fs::read_to_string(site.join(page)).expect("page");
        assert_eq!(bar_as_seen_from(&html, page), expected, "bar on {page}");
    }
    assert!(
        modes.contains("<span aria-current=\"page\">Modes</span>"),
        "current page unlinked: {modes}"
    );

    // Issue #446: a link correct in the repository must resolve to the page the
    // site actually serves, which is the slug — not the source's file name.
    let guide = std::fs::read_to_string(site.join("guide.html")).expect("guide page");
    assert!(
        guide.contains("href=\"build-plan-v2.html\""),
        "link resolved to the published slug: {guide}"
    );
    assert!(
        !guide.contains("BUILD_PLAN_V2.html"),
        "the file-name guess is gone: {guide}"
    );
}

#[test]
fn a_source_link_is_aimed_at_the_repository_at_the_rendered_commit() {
    // Issue #456: `docs/history/BUILD_PLAN.md` cites code as evidence for its claims.
    // That link is correct in a checkout and dead on the site, which publishes
    // documents and not source — so it is re-aimed at the repository's web view,
    // pinned to the commit the site was built from.
    let dir = fresh_dir("sourcelink");
    git(&dir, &["init", "-q"]);
    write(&dir, "website/public/index.html", "<h1>Home</h1>\n");
    write(
        &dir,
        "docs/adr/0001-example.md",
        "---\nadr-id: \"0001\"\nstatus: Accepted\n---\n\n# ADR-0001: Example\n\n\
         Root config: [Cargo](../../Cargo.toml).\n",
    );
    write(
        &dir,
        "docs/history/BUILD_PLAN.md",
        "# Build Plan\n\nEvidence: [sync](../../crates/x/src/sync.rs).\n\n\
         Site link: [adrs](adr/).\n",
    );
    write(&dir, "crates/x/src/sync.rs", "pub fn f() {}\n");
    write(&dir, "Cargo.toml", "[workspace]\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    git(&dir, &["remote", "add", "origin", "git@github.com:o/r.git"]);
    let sha = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&dir)
            .output()
            .expect("rev-parse")
            .stdout,
    )
    .expect("utf8");
    let sha = sha.trim();

    let out = Command::new(BIN)
        .args(["render", "docs", "--out", "site"])
        .current_dir(&dir)
        .output()
        .expect("run render");
    assert!(out.status.success(), "render failed: {out:?}");

    let plan =
        std::fs::read_to_string(dir.join("site/history/build-plan.html")).expect("plan page");
    assert!(
        plan.contains(&format!(
            "href=\"https://github.com/o/r/blob/{sha}/crates/x/src/sync.rs\""
        )),
        "pinned to the rendered commit, not to a branch: {plan}"
    );
    // A link written *for* the site is correct there and must survive untouched.
    assert!(plan.contains("href=\"adr/\""), "{plan}");

    // An ADR sits one level down, so it takes two hops to leave the site — and
    // the resolution is against the ADR's own directory, not the site root.
    let adr = std::fs::read_to_string(dir.join("site/adr/0001-example.html")).expect("adr page");
    assert!(
        adr.contains(&format!(
            "href=\"https://github.com/o/r/blob/{sha}/Cargo.toml\""
        )),
        "{adr}"
    );
}

#[test]
fn without_an_origin_remote_a_source_link_is_left_as_authored() {
    // A rewrite that silently produced a broken URL would be worse than the link
    // it replaces, so a repository with no mappable `origin` renders exactly the
    // site it rendered before this existed.
    let dir = fresh_dir("noorigin");
    git(&dir, &["init", "-q"]);
    write(&dir, "website/public/index.html", "<h1>Home</h1>\n");
    write(
        &dir,
        "docs/adr/0001-example.md",
        "---\nadr-id: \"0001\"\nstatus: Accepted\n---\n\n# ADR-0001: Example\n",
    );
    write(
        &dir,
        "docs/history/BUILD_PLAN.md",
        "# Build Plan\n\nEvidence: [sync](../crates/x/src/sync.rs).\n",
    );
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    let out = Command::new(BIN)
        .args(["render", "docs", "--out", "site"])
        .current_dir(&dir)
        .output()
        .expect("run render");
    assert!(out.status.success(), "render failed: {out:?}");
    let plan =
        std::fs::read_to_string(dir.join("site/history/build-plan.html")).expect("plan page");
    assert!(!plan.contains("github.com"), "nothing invented: {plan}");
    assert!(
        plan.contains("href=\"../crates/x/src/sync.rs\""),
        "left exactly as authored: {plan}"
    );
}

#[test]
fn the_bar_on_the_landing_page_is_rendered_over_whatever_the_file_carried() {
    // Issue #508: `website/public/index.html` is copied verbatim, so its nav was
    // a hand-maintained copy of the list `site_nav` derives — and a page added to
    // one list and not the other is published, reachable, and linked from every
    // page except the front one. The copy is now overwritten, which is why the
    // deliberately wrong list below does not survive.
    //
    // Note what this does *not* buy: the original defect was a link that did not
    // exist, and no link auditor can find one of those. This removes the second
    // list rather than checking it.
    let dir = fresh_dir("landingnav");
    git(&dir, &["init", "-q"]);
    write(
        &dir,
        "website/public/index.html",
        "<h1>Roteiro</h1>\n<nav class=\"sitenav\">\n<a href=\"gone.html\">Gone</a>\n</nav>\n\
         <p>tail</p>\n",
    );
    write(
        &dir,
        "docs/adr/0001-example.md",
        "---\nadr-id: \"0001\"\nstatus: Accepted\n---\n\n# ADR-0001: Example\n",
    );
    write(
        &dir,
        "website/pages/modes.md",
        "---\nsite-page: modes\nsite-nav: Modes\nsite-order: 1\n---\n\n# Modes\n",
    );
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    let out = Command::new(BIN)
        .args(["render", "docs", "--out", "site"])
        .current_dir(&dir)
        .output()
        .expect("run render");
    assert!(out.status.success(), "render failed: {out:?}");

    let landing = std::fs::read_to_string(dir.join("site/index.html")).expect("landing page");
    assert!(
        !landing.contains("gone.html"),
        "the hand-written list is overwritten, not merged: {landing}"
    );
    assert_eq!(
        bar_as_seen_from(&landing, "./"),
        vec![
            ("Home".to_owned(), "./".to_owned()),
            ("Modes".to_owned(), "modes.html".to_owned()),
        ],
        "the landing page carries the computed bar: {landing}"
    );
    // Only the bar is touched; the rest of the hand-written page is untouched.
    assert!(landing.starts_with("<h1>Roteiro</h1>\n"), "{landing}");
    assert!(landing.ends_with("<p>tail</p>\n"), "{landing}");
}

#[test]
fn the_landing_page_carries_the_bar_the_renderer_emits() {
    // roteiro.dev's landing page is hand-written HTML whose site bar `render
    // docs` now *replaces* with the computed one (issue #508). That makes this a
    // guard on the replacement having happened: rename the `sitenav` marker away
    // and the landing page keeps whatever it was carrying, silently, which is
    // what this catches. It renders *this* repository and holds the two against
    // each other.
    let out_dir = std::env::temp_dir().join(format!("roteiro-website-bar-{}", std::process::id()));
    std::fs::remove_dir_all(&out_dir).ok();
    let out = Command::new(BIN)
        .args(["render", "docs", "--out"])
        .arg(&out_dir)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run render");
    assert!(out.status.success(), "render failed: {out:?}");

    let landing = std::fs::read_to_string(out_dir.join("index.html")).expect("landing page");
    // The landing page is served at the site root, so that is what its own
    // marked entry points at.
    let landing_bar = bar_as_seen_from(&landing, "./");
    assert!(
        !landing_bar.is_empty(),
        "the landing page carries a site bar"
    );

    // Every rendered page's bar is built once from the published pages, so any
    // one of them is the authority the hand-written copy must match.
    let rendered = std::fs::read_dir(&out_dir)
        .expect("read site")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| {
            p.extension().and_then(|e| e.to_str()) == Some("html")
                && p.file_name().and_then(|n| n.to_str()) != Some("index.html")
                && std::fs::read_to_string(p).is_ok_and(|h| h.contains("<nav class=\"sitenav\">"))
        })
        .expect("at least one rendered site page");
    let rendered_href = rendered
        .file_name()
        .and_then(|n| n.to_str())
        .expect("rendered page name");
    let emitted = bar_as_seen_from(
        &std::fs::read_to_string(&rendered).expect("rendered page"),
        rendered_href,
    );
    assert_eq!(
        landing_bar,
        emitted,
        "website/public/index.html's site bar disagrees with the bar {} carries \
         (labels *and* destinations are compared)",
        rendered.display()
    );
    std::fs::remove_dir_all(&out_dir).ok();
}

/// A two-repo workspace: a hub `app` declaring config keys, and a spoke `deploy`
/// overriding them under a different naming convention, with a **user** config
/// (via `ROTEIRO_HOME`) naming a `prod` workspace over both. Returns
/// `(base, home)`.
///
/// Deliberately gives both repos a `README.md`: that is the collision issue #442
/// is about — node keys are repository-relative, so both are `file:README.md`.
fn workspace_fixture(tag: &str) -> (PathBuf, PathBuf) {
    let base = fresh_dir(tag);
    let home = base.join("home");
    let app = base.join("app");
    let deploy = base.join("deploy");
    for d in [&home, &app, &deploy] {
        std::fs::create_dir_all(d).expect("mkdir");
    }

    write(&app, "README.md", "# App\n\nThe hub.\n");
    write(
        &app,
        "config.toml",
        "[serve]\naddr = \"127.0.0.1:8017\"\ntools = true\n",
    );
    git(&app, &["init", "-q"]);
    git(&app, &["add", "."]);
    git(&app, &["commit", "-q", "-m", "init"]);

    write(&deploy, "README.md", "# Deploy\n\nThe spoke.\n");
    write(
        &deploy,
        "prod.env",
        "SERVE_ADDR=0.0.0.0:8443\nSERVE_TOOLS=false\n",
    );
    // A **declared** cross-repo link into `app` (ADR-0009). This is the thing a
    // workspace bundle exists to put both ends of in one place, so the fixture
    // that stands for a workspace has to have one — without it, every assertion
    // about cross-repo resolution is made against a bundle that contains no
    // cross-repo reference.
    write(
        &deploy,
        "roteiro.toml",
        "[[links]]\nfrom = \"cfgkey:prod.env#SERVE_ADDR\"\n\
         to = \"app::cfgkey:config.toml#serve.addr\"\nkind = \"references\"\n",
    );
    git(&deploy, &["init", "-q"]);
    git(&deploy, &["add", "."]);
    git(&deploy, &["commit", "-q", "-m", "init"]);

    std::fs::write(
        home.join("config.toml"),
        format!(
            "[[workspaces]]\nname = \"prod\"\nrepos = [\"{}\", \"{}\"]\n",
            app.display(),
            deploy.display()
        ),
    )
    .expect("write config");

    (base, home)
}

/// Run the binary in `dir` with the fixture's isolated `ROTEIRO_HOME`.
fn roteiro_in(dir: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("ROTEIRO_HOME", home)
        .output()
        .expect("run roteiro")
}

/// Every concept is written, and the count printed is the count written.
///
/// The property the Obsidian vault failed: it wrote one flat directory, two keys
/// whose names folded together became one file, and **104 notes of 8,144** were
/// lost while the printed total counted them as written. So this asserts the
/// number, not that any particular pair of names differs — the original defect
/// was invisible to the latter.
///
/// The fixture is the real vendored `cytoscape.min.js`, because it is a genuine
/// source of near-colliding symbol names at scale. A hand-written pair would
/// exercise the disambiguator without establishing that it holds over a
/// repository.
#[test]
fn every_concept_is_written_and_the_count_printed_is_the_count_written() {
    let dir = fresh_dir("okf-lossless");
    git(&dir, &["init", "-q"]);

    let vendored = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/assets/cytoscape.min.js");
    assert!(
        vendored.is_file(),
        "the vendored bundle this test measures against is missing at {vendored:?} — \
         it moved or was removed, and the test must be re-pointed rather than \
         allowed to pass over a repository with no colliding keys"
    );
    std::fs::create_dir_all(dir.join("assets")).expect("mkdir");
    std::fs::copy(&vendored, dir.join("assets/cytoscape.min.js")).expect("copy bundle");
    // Keys that collide by the slug rule too, so that half is exercised even if
    // the bundle is ever slimmed.
    write(
        &dir,
        "src/lib.rs",
        "pub struct Store;\npub fn store() {}\npub mod r#mod {\n    pub struct STORE;\n}\n",
    );
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    let out = Command::new(BIN)
        .args(["render", "okf", "--out", "bundle"])
        .current_dir(&dir)
        .output()
        .expect("run render okf");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "render failed: {stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let printed: usize = stdout
        .split(" concept(s)")
        .next()
        .and_then(|p| p.rsplit('(').next())
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or_else(|| panic!("no concept count in: {stdout}"));

    let bundle = dir.join("bundle");
    let mut written = 0usize;
    let mut stack = vec![bundle.clone()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).expect("read_dir") {
            let path = e.expect("entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|x| x == "md")
                && path
                    .file_name()
                    .is_some_and(|n| n != "index.md" && n != "log.md")
            {
                written += 1;
            }
        }
    }
    assert!(printed > 0, "the fixture must produce concepts: {stdout}");
    assert_eq!(
        printed, written,
        "the count printed must equal the files written — a bundle that lost one \
         silently is the defect this replaces"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// `--workspace-name` is rejected when it names nothing, rather than quietly
/// rendering the current project as if the flag had not been passed.
#[test]
fn render_okf_rejects_an_unknown_workspace_name() {
    let dir = fresh_dir("okf-unknown-ws");
    git(&dir, &["init", "-q"]);
    write(&dir, "src/lib.rs", "pub struct Thing;\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    let out = Command::new(BIN)
        .args([
            "render",
            "okf",
            "--workspace-name",
            "nope",
            "--out",
            "bundle",
        ])
        .current_dir(&dir)
        .output()
        .expect("run render okf");
    assert!(
        !out.status.success(),
        "an unknown workspace must fail, not fall back to the current project"
    );
    assert!(
        !dir.join("bundle").exists(),
        "and must not have written a bundle first"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// The removed target names its replacement rather than reporting a bare
/// "unknown target", because a script passing `obsidian` needs to learn what
/// happened to it.
#[test]
fn render_obsidian_explains_that_it_became_okf() {
    let dir = fresh_dir("okf-removed-target");
    git(&dir, &["init", "-q"]);
    write(&dir, "src/lib.rs", "pub struct Thing;\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    let out = Command::new(BIN)
        .args(["render", "obsidian"])
        .current_dir(&dir)
        .output()
        .expect("run render obsidian");
    assert!(!out.status.success(), "the removed target must fail");
    let msg = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(msg.contains("render okf"), "names its replacement: {msg}");
    assert!(
        msg.contains("Obsidian still opens"),
        "and says Obsidian still works, since that is the reader's first question: {msg}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A prose document's concept carries the document, not a summary of it.
///
/// A node's `meta.content` is an embedding budget — collapsed and truncated — so
/// a bundle built from it would be a few percent of each source on one line. The
/// point of the bundle is to give an agent the text.
#[test]
fn a_prose_concept_carries_its_whole_source() {
    let dir = fresh_dir("okf-prose");
    git(&dir, &["init", "-q"]);
    let body = "# Title\n\nA distinctive sentence that must survive whole.\n";
    write(&dir, "README.md", body);
    write(&dir, "src/lib.rs", "pub struct Thing;\n");
    write(&dir, "roteiro.toml", "[ingest]\nprose = true\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    let out = Command::new(BIN)
        .args(["render", "okf", "--out", "bundle"])
        .current_dir(&dir)
        .output()
        .expect("run render okf");
    assert!(
        out.status.success(),
        "render failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let files = dir.join("bundle/files");
    let found = std::fs::read_dir(&files)
        .expect("files/ must exist")
        .filter_map(std::result::Result::ok)
        .map(|e| std::fs::read_to_string(e.path()).unwrap_or_default())
        .any(|t| t.contains("A distinctive sentence that must survive whole."));
    assert!(found, "the prose body must reach the bundle");
    std::fs::remove_dir_all(&dir).ok();
}

/// A workspace bundle keeps both members' `README.md`.
///
/// Node keys are repository-relative, so every repo's README is the same key —
/// `file:README.md`. That is the collision issue #442 is about, and the vault
/// survived it by qualifying keys as `<project>::<key>` and hashing each
/// filename, because it wrote one flat directory. A bundle nests by member, so
/// the structure carries it.
///
/// The assertion is again the count, plus that each member's own text is present:
/// two files whose *paths* differ but whose *contents* are the same document
/// would satisfy a path-only check while having lost one.
#[test]
fn a_workspace_bundle_keeps_both_members_readme() {
    let (base, home) = workspace_fixture("okf-ws");
    let app = base.join("app");

    let out = roteiro_in(
        &app,
        &home,
        &[
            "render",
            "okf",
            "--workspace-name",
            "prod",
            "--out",
            "bundle",
        ],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "workspace render failed: {stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let bundle = app.join("bundle");
    let mut readmes = Vec::new();
    let mut readme_paths = Vec::new();
    let mut stack = vec![bundle.clone()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).expect("read_dir") {
            let path = e.expect("entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|x| x == "md") {
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                if text.contains("type: \"file\"") && text.contains("README.md") {
                    readme_paths.push(path.strip_prefix(&bundle).unwrap_or(&path).to_path_buf());
                    readmes.push(text);
                }
            }
        }
    }
    assert_eq!(
        readmes.len(),
        2,
        "both members' README concepts must be written, not one overwriting the \
         other: found {}",
        readmes.len()
    );
    // And they must be the two *different* documents, not one written twice.
    assert!(
        readmes.iter().any(|t| t.contains("The hub.")),
        "the hub's README text is missing"
    );
    assert!(
        readmes.iter().any(|t| t.contains("The spoke.")),
        "the spoke's README text is missing"
    );
    // …and under their own members. Without this the test passes even when
    // member nesting is removed entirely, because the slug disambiguator rescues
    // the collision on its own — both files are still written, just flat. That
    // version of this test survived fault injection and proved nothing about
    // nesting, which is the property the workspace renderer exists for.
    let dirs: std::collections::BTreeSet<String> = readme_paths
        .iter()
        .filter_map(|p| p.iter().next())
        .map(|c| c.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        dirs.len(),
        2,
        "each member's README belongs under its own member directory, got {dirs:?}"
    );
    std::fs::remove_dir_all(&base).ok();
}

/// Every `.md` file in a bundle, as `(bundle-relative path, content)`.
fn bundle_files(root: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read_dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|x| x == "md") {
                let rel = path
                    .strip_prefix(root)
                    .expect("under root")
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, std::fs::read_to_string(&path).expect("read")));
            }
        }
    }
    out.sort();
    out
}

/// The `human:` verifier names whoever last touched **that document**, and the
/// bundle is dated by the **commit**, not by the clock.
///
/// Both halves are claims a consumer acts on. OKF derives the human-reviewed
/// trust tier (§5.3) from `verified[].by`, so attributing every authored concept
/// to the `HEAD` author records a review that person never did — and a bundle is
/// a build output of one commit, so a wall-clock timestamp makes two renders of
/// that commit differ while describing an identical graph.
///
/// The fixture separates the two on purpose: `HEAD` is authored by someone who
/// touched **neither** ADR, so a per-repository attribution has a name to leak
/// and this test has something to catch.
#[test]
fn the_verifier_is_the_documents_own_author_and_the_bundle_is_dated_by_the_commit() {
    const ADA: &str = "2020-01-02T03:04:05+00:00";
    const GRACE: &str = "2021-02-03T04:05:06+00:00";
    const HEAD: &str = "2022-03-04T05:06:07+00:00";

    let dir = fresh_dir("okf-attribution");
    git(&dir, &["init", "-q"]);

    let adr = |id: &str, title: &str| {
        format!(
            "---\nadr-id: \"{id}\"\nstatus: Accepted\n---\n\n# ADR-{id}: {title}\n\n\
             ## Context\n\nProse.\n"
        )
    };
    write(&dir, "docs/adr/0001-alpha.md", &adr("0001", "Alpha"));
    git(&dir, &["add", "."]);
    commit_as(&dir, "Ada", ADA, "alpha");

    write(&dir, "docs/adr/0002-beta.md", &adr("0002", "Beta"));
    git(&dir, &["add", "."]);
    commit_as(&dir, "Grace", GRACE, "beta");

    // A last commit touching neither ADR. Under a per-repository attribution
    // this name would appear on both of them.
    write(&dir, "src/lib.rs", "pub struct Thing;\n");
    git(&dir, &["add", "."]);
    commit_as(&dir, "Mallory", HEAD, "unrelated code");

    let out = Command::new(BIN)
        .args(["render", "okf", "--out", "bundle"])
        .current_dir(&dir)
        .output()
        .expect("run render okf");
    assert!(
        out.status.success(),
        "render failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let files = bundle_files(&dir.join("bundle"));

    // The fixture is load-bearing only if the ADRs reached the bundle as
    // human-verified concepts at all — otherwise every assertion below is
    // satisfied by an empty search.
    let human_verified = files
        .iter()
        .filter(|(_, text)| text.contains("verified:\n  - by: \"human:"))
        .count();
    assert!(
        human_verified >= 2,
        "the fixture must produce human-verified concepts: {:?}",
        files.iter().map(|(p, _)| p).collect::<Vec<_>>()
    );

    let carrying = |needle: String| -> Vec<String> {
        files
            .iter()
            .filter(|(_, text)| text.contains(&needle))
            .map(|(path, _)| path.clone())
            .collect()
    };

    // Each ADR is confirmed by its own author, at that author's own commit time.
    for (source, who, when) in [
        ("docs/adr/0001-alpha.md", "Ada", "2020-01-02T03:04:05Z"),
        ("docs/adr/0002-beta.md", "Grace", "2021-02-03T04:05:06Z"),
    ] {
        let concepts: Vec<&(String, String)> = files
            .iter()
            .filter(|(_, text)| text.contains(&format!("- resource: \"/{source}\"")))
            .filter(|(_, text)| text.contains("verified:\n  - by: \"human:"))
            .collect();
        assert!(
            !concepts.is_empty(),
            "no human-verified concept is sourced from {source}"
        );
        for (path, text) in &concepts {
            assert!(
                text.contains(&format!("by: \"human:{who}\"")),
                "{path} is sourced from {source} but is not confirmed by {who}:\n{text}"
            );
            assert!(
                text.contains(&format!("at: \"{when}\"")),
                "{path} must carry {who}'s own commit time {when}:\n{text}"
            );
        }
    }

    // The `HEAD` author confirmed nothing: they touched neither document.
    let leaked = carrying("human:Mallory".to_owned());
    assert!(
        leaked.is_empty(),
        "the HEAD author must not be recorded as confirming documents they never \
         touched: {leaked:?}"
    );

    // And what no document dates — the derived layer — carries the commit's own
    // time. A `SystemTime::now()` here would read as the year the test ran.
    let head_dated = carrying("at: \"2022-03-04T05:06:07Z\"".to_owned());
    assert!(
        !head_dated.is_empty(),
        "concepts with no document of their own must be dated by HEAD, not by the \
         wall clock: {:?}",
        files
            .iter()
            .filter(|(_, t)| t.contains("generated:"))
            .map(|(p, _)| p)
            .collect::<Vec<_>>()
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// Two renders of one commit produce the same bytes.
///
/// Stated over the whole bundle rather than over the timestamp alone, because
/// reproducibility is a property of the artifact: a consumer diffing two
/// downloads to see what changed learns nothing if every render differs. What it
/// catches is *set-dependent* output — slug disambiguation, map iteration, sort
/// order — going unstable.
///
/// **It does not catch a wall clock**, and was measured not to: two renders in
/// one test run land in the same second, so `SystemTime::now()` formats to the
/// same string and this passes. The timestamp is pinned by
/// [`the_verifier_is_the_documents_own_author_and_the_bundle_is_dated_by_the_commit`],
/// which asserts the literal `HEAD` commit time. Both are needed; neither
/// subsumes the other.
#[test]
fn two_renders_of_one_commit_are_byte_identical() {
    let dir = fresh_dir("okf-reproducible");
    git(&dir, &["init", "-q"]);
    write(&dir, "src/lib.rs", "pub struct Thing;\npub fn thing() {}\n");
    write(
        &dir,
        "docs/adr/0001-a.md",
        "---\nadr-id: \"0001\"\nstatus: Accepted\n---\n\n# ADR-0001: A\n\n## Context\n\nProse.\n",
    );
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    let render = |out: &str| {
        let done = Command::new(BIN)
            .args(["render", "okf", "--out", out])
            .current_dir(&dir)
            .output()
            .expect("run render okf");
        assert!(
            done.status.success(),
            "render failed: {}",
            String::from_utf8_lossy(&done.stderr)
        );
        bundle_files(&dir.join(out))
    };

    let once = render("bundle-a");
    let twice = render("bundle-b");
    assert!(
        !once.is_empty(),
        "the fixture must produce a bundle to compare"
    );
    assert_eq!(
        once, twice,
        "a bundle rendered twice from one commit must not differ"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A shallow clone attributes **nothing**, rather than attributing everything to
/// the one commit it has.
///
/// This is where the per-document fix would quietly undo itself. `actions/checkout`
/// defaults to `fetch-depth: 1`, and at the shallow boundary the history a
/// comparison needs is absent — so a naive "differs from every parent" reads the
/// single commit present as having introduced the whole tree, and every ADR is
/// confirmed by whoever pushed last. That is the original defect, reappearing in
/// exactly the job that publishes the artifact.
///
/// **Two mechanisms in `last_authors` hold this, and either alone suffices**, so
/// a single-fault injection cannot prove this test non-vacuous: removing the
/// `shallow_commits()` boundary check leaves it green, and so does reverting the
/// all-or-nothing parent read. Both together make it fail, naming a human. What
/// *does* fail it on its own is the defect it was written for — attributing every
/// path to the newest commit containing it.
///
/// Unverified is the honest answer: absence of `verified` is a tier, and it says
/// *nobody has confirmed this*, which is true of a bundle built from a repository
/// whose history is not there. The workflow asks for full history so the published
/// bundle does not land here.
#[test]
fn a_shallow_clone_claims_no_human_verifier_rather_than_the_wrong_one() {
    let dir = fresh_dir("okf-shallow");
    git(&dir, &["init", "-q"]);
    let adr =
        "---\nadr-id: \"0001\"\nstatus: Accepted\n---\n\n# ADR-0001: A\n\n## Context\n\nProse.\n";
    write(&dir, "docs/adr/0001-a.md", adr);
    git(&dir, &["add", "."]);
    commit_as(&dir, "Ada", "2020-01-02T03:04:05+00:00", "the adr");

    write(&dir, "src/lib.rs", "pub struct Thing;\n");
    git(&dir, &["add", "."]);
    commit_as(
        &dir,
        "Mallory",
        "2022-03-04T05:06:07+00:00",
        "unrelated code",
    );

    // The same fixture at full depth *does* attribute, or the assertion below
    // would hold for a reason that has nothing to do with shallowness.
    let deep = Command::new(BIN)
        .args(["render", "okf", "--out", "deep"])
        .current_dir(&dir)
        .output()
        .expect("run render okf");
    assert!(
        deep.status.success(),
        "{}",
        String::from_utf8_lossy(&deep.stderr)
    );
    assert!(
        bundle_files(&dir.join("deep"))
            .iter()
            .any(|(_, t)| t.contains("by: \"human:Ada\"")),
        "the full-depth control must attribute, or this test proves nothing"
    );

    let shallow = dir.join("shallow");
    git(
        &dir,
        &[
            "clone",
            "-q",
            "--depth",
            "1",
            &format!("file://{}", dir.display()),
            shallow.to_str().expect("utf-8 path"),
        ],
    );
    assert!(
        shallow.join(".git/shallow").is_file(),
        "the clone must actually be shallow, or this test proves nothing"
    );

    let out = Command::new(BIN)
        .args(["render", "okf", "--out", "bundle"])
        .current_dir(&shallow)
        .output()
        .expect("run render okf");
    assert!(
        out.status.success(),
        "a shallow checkout must still render: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let files = bundle_files(&shallow.join("bundle"));
    assert!(
        !files.is_empty(),
        "the shallow render must produce a bundle"
    );
    let claimed: Vec<&String> = files
        .iter()
        .filter(|(_, text)| text.contains("by: \"human:"))
        .map(|(path, _)| path)
        .collect();
    assert!(
        claimed.is_empty(),
        "no human may be named when the history that would name them is absent: \
         {claimed:?}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// The frontmatter block of a bundle file, as `(key, value)` pairs for the
/// top-level scalars. Nested lists are skipped: this reads the keys a consumer
/// branches on, not the whole document.
fn frontmatter_of(text: &str) -> Vec<(String, String)> {
    let Some(rest) = text.strip_prefix("---\n") else {
        return Vec::new();
    };
    let Some(end) = rest.find("\n---\n") else {
        return Vec::new();
    };
    rest[..end]
        .lines()
        .filter(|l| !l.starts_with(' ') && !l.starts_with('-'))
        .filter_map(|l| l.split_once(": "))
        .map(|(k, v)| (k.to_owned(), v.trim_matches('"').to_owned()))
        .collect()
}

/// An ADR's `status` reaches the decision and its sections, and **nothing else
/// that happens to live in the same file**.
///
/// OKF's `status` (§4) is a claim about the concept carrying it. The `file:` node
/// for an ADR is the document, not the decision; a `marker` is a piece of
/// unfinished work *inside* the document. Labelling either `stable` because the
/// decision above them was accepted asserts something nobody wrote — and a debt
/// marker reading "stable" inverts what the marker is for.
///
/// A section keeps the status, because a section of a superseded decision is
/// superseded.
#[test]
fn an_adrs_status_does_not_leak_onto_the_file_or_its_debt_markers() {
    let dir = fresh_dir("okf-status-scope");
    git(&dir, &["init", "-q"]);
    write(
        &dir,
        "docs/adr/0001-a.md",
        "---\nadr-id: \"0001\"\nstatus: Superseded\n---\n\n# ADR-0001: A\n\n## Context\n\n\
         Prose, and a marker: TODO tidy this up.\n",
    );
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    let out = Command::new(BIN)
        .args(["render", "okf", "--out", "bundle"])
        .current_dir(&dir)
        .output()
        .expect("run render okf");
    assert!(
        out.status.success(),
        "render failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let mut by_kind: std::collections::BTreeMap<String, Vec<(String, Option<String>)>> =
        std::collections::BTreeMap::new();
    for (path, text) in bundle_files(&dir.join("bundle")) {
        let fm = frontmatter_of(&text);
        let Some((_, kind)) = fm.iter().find(|(k, _)| k == "type") else {
            continue;
        };
        let status = fm
            .iter()
            .find(|(k, _)| k == "status")
            .map(|(_, v)| v.clone());
        by_kind
            .entry(kind.clone())
            .or_default()
            .push((path, status));
    }

    // The fixture must produce all three kinds, or the assertions below are
    // satisfied by concepts that were never emitted.
    for kind in ["adr", "adr_section", "file", "marker"] {
        assert!(
            by_kind.contains_key(kind),
            "the fixture must emit a `{kind}` concept: {:?}",
            by_kind.keys().collect::<Vec<_>>()
        );
    }

    // The decision and its sections carry the mapped status …
    for kind in ["adr", "adr_section"] {
        for (path, status) in &by_kind[kind] {
            assert_eq!(
                status.as_deref(),
                Some("deprecated"),
                "{path} is the decision (or part of it) and must carry its status"
            );
        }
    }
    // … and nothing else does, however much of the file it shares.
    for kind in ["file", "marker"] {
        for (path, status) in &by_kind[kind] {
            assert_eq!(
                status.as_deref(),
                None,
                "{path} is a `{kind}`, not the decision — it must claim no lifecycle \
                 of the decision's"
            );
        }
    }

    std::fs::remove_dir_all(&dir).ok();
}

/// A commit whose parent cannot be read attributes **nothing through that
/// commit**, rather than comparing against the parents that happen to be there.
///
/// Dropping an unreadable parent shrinks the set a commit is compared against,
/// and a smaller set makes "changed relative to every parent" easier to satisfy —
/// so the *merge* looks like it introduced what the missing branch actually
/// added. The fixture is exactly that shape: Bob writes the ADR on a branch,
/// Mallory merges it, and Bob's commit is then removed from the object store.
///
/// Measured, not supposed. Dropping unreadable parents renders this repository
/// successfully and records **Mallory** as having confirmed the ADR Bob wrote:
/// a silent false claim in the one field OKF derives the human-reviewed tier
/// from. Refusing is the correct outcome — the history that would name the author
/// is not there to be read.
#[test]
fn a_commit_whose_parent_cannot_be_read_attributes_nobody() {
    const BASE: &str = "2020-01-02T03:04:05+00:00";
    const SIDE: &str = "2021-02-03T04:05:06+00:00";
    const MERGE: &str = "2022-03-04T05:06:07+00:00";

    let dir = fresh_dir("okf-unreadable-parent");
    git(&dir, &["init", "-q"]);
    write(&dir, "src/lib.rs", "pub struct Thing;\n");
    git(&dir, &["add", "."]);
    commit_as(&dir, "Ada", BASE, "base");

    git(&dir, &["checkout", "-q", "-b", "feature"]);
    write(
        &dir,
        "docs/adr/0001-a.md",
        "---\nadr-id: \"0001\"\nstatus: Accepted\n---\n\n# ADR-0001: A\n\n## Context\n\nProse.\n",
    );
    git(&dir, &["add", "."]);
    commit_as(&dir, "Bob", SIDE, "the adr");
    let side = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&dir)
            .output()
            .expect("rev-parse")
            .stdout,
    )
    .expect("utf-8")
    .trim()
    .to_owned();

    git(&dir, &["checkout", "-q", "main"]);
    git_as(
        &dir,
        "Mallory",
        MERGE,
        &["merge", "-q", "--no-ff", "-m", "merge", "feature"],
    );

    // Intact, the ADR is Bob's. Without this the assertion below would hold on a
    // bundle that attributes nothing for reasons unrelated to the damage.
    let control = Command::new(BIN)
        .args(["render", "okf", "--out", "control"])
        .current_dir(&dir)
        .output()
        .expect("run render okf");
    assert!(
        control.status.success(),
        "the control render must succeed: {}",
        String::from_utf8_lossy(&control.stderr)
    );
    assert!(
        bundle_files(&dir.join("control"))
            .iter()
            .any(|(_, t)| t.contains("by: \"human:Bob\"")),
        "the intact fixture must attribute the ADR to Bob, or the damage below \
         proves nothing"
    );

    // Remove the side commit's object: its tree is still referenced by the merge,
    // so the repository still checks out — only the *history* is unreadable.
    let object = dir.join(".git/objects").join(&side[..2]).join(&side[2..]);
    assert!(object.is_file(), "expected a loose object at {object:?}");
    std::fs::remove_file(&object).expect("remove the side commit");

    let out = Command::new(BIN)
        .args(["render", "okf", "--out", "bundle"])
        .current_dir(&dir)
        .output()
        .expect("run render okf");

    // Refusing outright is a fine answer, and is what happens today. What must
    // never happen is a bundle that names the merge's author as the ADR's.
    if out.status.success() {
        let named: Vec<String> = bundle_files(&dir.join("bundle"))
            .into_iter()
            .filter(|(_, t)| t.contains("human:Mallory"))
            .map(|(p, _)| p)
            .collect();
        assert!(
            named.is_empty(),
            "the merge's author must not inherit the confirmation of a branch whose \
             history cannot be read: {named:?}"
        );
    }

    std::fs::remove_dir_all(&dir).ok();
}

/// **In a workspace bundle, a cross-repo link resolves into the other member.**
///
/// This is the property the workspace bundle exists for. ADR-0009 and #442 put it
/// as the cross-repo link finally having *both* endpoints in one output; a bundle
/// whose cross-repo links do not resolve is the single-project bundle with extra
/// directories.
///
/// Two things make it un-catchable by the guards already here. OKF §11 tells
/// consumers they **MUST NOT** reject a bundle for a broken cross-link, so the
/// conformance test stays green. And a member-scoped lookup finds the
/// `extref:<project>::<key>` **placeholder**, which is a real file in the bundle —
/// so an existence check is satisfied by a link that lands the reader on a
/// document whose whole content is that it is not the one they wanted. The
/// assertion here is therefore the *destination*.
///
/// Driven through the real binary, because the risk is precisely that the renderer
/// and the graph disagree about what a cross-repo key looks like — a hand-made key
/// would test my assumption rather than the graph's.
#[test]
fn a_cross_repo_link_resolves_into_the_other_member() {
    let (base, home) = workspace_fixture("okf-xrepo");
    let app = base.join("app");
    let deploy = base.join("deploy");

    // The declared `[[links]]` entry only becomes an `extref:` node once each
    // member has a graph and `links --write` has attached it (#573). Without this
    // the bundle contains no cross-repo reference at all, and every assertion
    // below would pass over its absence — which is exactly how a fixture stops
    // testing the thing it was written for.
    for member in [&app, &deploy] {
        assert!(
            roteiro_in(member, &home, &["sync"]).status.success(),
            "sync {member:?}"
        );
    }
    let linked = roteiro_in(
        &app,
        &home,
        &["links", "--workspace-name", "prod", "--write"],
    );
    assert!(
        linked.status.success(),
        "links --write failed: {}{}",
        String::from_utf8_lossy(&linked.stdout),
        String::from_utf8_lossy(&linked.stderr)
    );

    let out = roteiro_in(
        &app,
        &home,
        &[
            "render",
            "okf",
            "--workspace-name",
            "prod",
            "--out",
            "bundle",
        ],
    );
    assert!(
        out.status.success(),
        "workspace render failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let files = bundle_files(&app.join("bundle"));
    let emitted: std::collections::BTreeSet<&str> = files.iter().map(|(p, _)| p.as_str()).collect();

    // The placeholder must be there, or the reference never reached the graph and
    // there is nothing to resolve.
    assert!(
        emitted
            .iter()
            .any(|p| p.starts_with("deploy/") && p.contains("extref-app-")),
        "the fixture must produce a cross-repo placeholder in `deploy`: {emitted:?}"
    );

    let referrer = "deploy/symbols/cfgkey-prod-env-serve-addr.md";
    let (_, text) = files
        .iter()
        .find(|(p, _)| p == referrer)
        .unwrap_or_else(|| panic!("no {referrer} in {emitted:?}"));

    let mut targets: Vec<String> = Vec::new();
    let mut rest = text.as_str();
    while let Some(open) = rest.find("](/") {
        rest = &rest[open + 3..];
        let Some(close) = rest.find(')') else { break };
        targets.push(rest[..close].to_owned());
        rest = &rest[close..];
    }
    assert!(
        !targets.is_empty(),
        "{referrer} must emit relationship links:\n{text}"
    );
    for target in &targets {
        assert!(
            emitted.contains(target.as_str()),
            "{referrer} links to /{target}, which the bundle does not contain: \
             {emitted:?}"
        );
    }
    assert!(
        targets.iter().any(|t| t.starts_with("app/")),
        "the cross-repo reference must land in `app`, not on `deploy`'s own \
         placeholder: {targets:?}"
    );

    std::fs::remove_dir_all(&base).ok();
}

/// A `site-page:` slug may name a **path**, and the page it produces has to work
/// from where it actually sits.
///
/// The unit tests cover the slug rule and the link rewrite separately; this is
/// the one that would have caught them being right individually and wrong
/// together — the directory has to be created, the theme and nav have to climb
/// back to the root, and a link from an ADR has to land on it.
#[test]
fn a_slug_that_names_a_directory_is_served_from_it() {
    let dir = fresh_dir("nestedslug");
    git(&dir, &["init", "-q"]);
    write(&dir, "website/public/index.html", "<h1>Home</h1>\n");
    write(&dir, "website/public/style.css", "body{}\n");
    write(
        &dir,
        "website/pages/modes.md",
        "---\nsite-page: modes\nsite-nav: Modes\nsite-order: 1\n---\n\n# Modes\n",
    );
    write(
        &dir,
        "docs/history/BUILD_PLAN_V2.md",
        "---\nsite-page: history/build-plan-v2\nsite-nav: Roadmap\nsite-order: 3\n---\n\n\
         # Roadmap\n",
    );
    write(
        &dir,
        "docs/adr/0001-example.md",
        "---\nadr-id: \"0001\"\nstatus: Accepted\n---\n\n# ADR-0001: Example\n\n\
         Sequenced in [V2](../history/BUILD_PLAN_V2.md).\n",
    );
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    let out = Command::new(BIN)
        .args(["render", "docs", "--out", "site"])
        .current_dir(&dir)
        .output()
        .expect("run render");
    assert!(out.status.success(), "render failed: {out:?}");
    let site = dir.join("site");

    // Served from the directory its slug names, not from the root.
    let page = std::fs::read_to_string(site.join("history/build-plan-v2.html"))
        .expect("page under history/");
    assert!(
        !site.join("build-plan-v2.html").exists(),
        "and not also at the root"
    );

    // A page one level down climbs for everything outside it: theme, nav, ADRs.
    assert!(
        page.contains("href=\"../style.css\""),
        "theme resolves from where the page sits: {page}"
    );
    assert!(
        page.contains("href=\"../modes.html\""),
        "nav entries are root-relative and must climb: {page}"
    );

    // And the ADR's repository-correct link lands on it.
    let adr = std::fs::read_to_string(site.join("adr/0001-example.html")).expect("adr page");
    assert!(
        adr.contains("href=\"../history/build-plan-v2.html\""),
        "the ADR link resolves to the served path: {adr}"
    );
}

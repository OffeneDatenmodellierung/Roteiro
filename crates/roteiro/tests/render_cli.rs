// roteiro:ignore-file — the fixtures below deliberately embed `TODO`/`FIXME` to
// exercise the detector; they are test data, not real debt in this repo.
//! End-to-end tests for `roteiro render`: drives the real binary against a
//! fixture repo and checks each target's output.
//!
//! `render docs` — the site is produced with themed ADR pages, an index, and the
//! copied static assets.
//!
//! `render obsidian` — the vault's `_Home` overview scopes intent debt by the
//! repository's `[debt] ignore` (ADR-0007 v1.1). The Obsidian render is one of
//! the seven surfacing stages Stage 26 enumerates, and the last of them to be
//! given the shared ignore list: the CLI and the graph API were fixed under
//! issue #321 while the vault was missed, because the fix went to the surfaces
//! that had been *reported* rather than to that enumeration. Both of `_Home`'s
//! debt tables are covered here, since an unscoped call in either makes the page
//! disagree with itself as well as with `roteiro debt`.

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

/// A file of `lines` newline-terminated lines whose first is a single `marker`
/// comment, long enough to clear `debt-density`'s default `min_lines`.
fn source(marker: &str, lines: usize) -> String {
    use std::fmt::Write as _;
    let mut s = format!("// {marker}: deferred\n");
    for i in 1..lines {
        let _ = writeln!(s, "pub const N{i}: u32 = {i};");
    }
    s
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

#[test]
fn render_obsidian_home_scopes_debt_by_the_ignore_config() {
    let dir = fresh_dir("obsidian");
    git(&dir, &["init", "-q"]);
    // Two files with one marker each, of a different category so `_Home`'s
    // category table can be read as an assertion, and both over `min_lines` so
    // both are eligible for its density table.
    write(&dir, "src/lib.rs", &source("TODO", 100));
    write(&dir, "vendor/dep.rs", &source("FIXME", 100));
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    let render = |dir: &Path| -> String {
        let out = Command::new(BIN)
            .args(["render", "obsidian", "--out", "vault"])
            .current_dir(dir)
            // Isolate from any real user config.
            .env("ROTEIRO_HOME", dir)
            .output()
            .expect("run render");
        assert!(out.status.success(), "render obsidian failed: {out:?}");
        std::fs::read_to_string(dir.join("vault/_Home.md")).expect("_Home.md")
    };

    // The control: with no ignore configured, the vendored marker is in scope and
    // shows in both tables. Without this half, the assertions below would also
    // pass if `_Home` had simply stopped reporting debt.
    let home = render(&dir);
    assert!(
        home.contains("| fixme | 1 |") && home.contains("| todo | 1 |"),
        "both markers counted with no ignore config: {home}"
    );
    assert!(
        home.contains("vendor/dep.rs") && home.contains("src/lib.rs"),
        "both files ranked with no ignore config: {home}"
    );

    // Ignore the vendored tree — the same config `roteiro debt` reads — and the
    // vendored marker leaves *both* of `_Home`'s tables. The density table is the
    // one Stage 26 Q1 added and the category totals are older, so a fix to either
    // alone leaves the page contradicting itself on the same screen.
    write(&dir, "roteiro.toml", "[debt]\nignore = [\"vendor/**\"]\n");
    let home = render(&dir);
    assert!(
        !home.contains("fixme"),
        "ignored marker must not reach the category totals: {home}"
    );
    assert!(
        home.contains("| todo | 1 |"),
        "the marker still in scope is still counted: {home}"
    );
    assert!(
        !home.contains("vendor"),
        "ignored file must not reach the density table: {home}"
    );
    assert!(
        home.contains("src/lib.rs"),
        "the file still in scope is still ranked: {home}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// The ordered labels of a page's site bar, with the current page's unlinked
/// marker flattened to its label — so two pages' bars compare equal when they
/// list the same pages in the same order.
fn bar_labels(html: &str) -> Vec<String> {
    let Some(start) = html.find("<nav class=\"sitenav\">") else {
        return Vec::new();
    };
    let bar = &html[start..];
    let bar = &bar[..bar.find("</nav>").map_or(bar.len(), |i| i)];
    bar.split('<')
        .filter_map(|chunk| chunk.split_once('>'))
        .map(|(_, text)| text.trim().to_owned())
        .filter(|text| !text.is_empty())
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
        "docs/BUILD_PLAN_V2.md",
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
    let expected = ["Home", "Guide", "Modes", "Roadmap"];
    for page in ["modes.html", "guide.html", "build-plan-v2.html"] {
        let html = std::fs::read_to_string(site.join(page)).expect("page");
        assert_eq!(bar_labels(&html), expected, "bar on {page}");
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
fn the_landing_page_carries_the_bar_the_renderer_emits() {
    // roteiro.dev's landing page is hand-written HTML that `render docs` copies
    // verbatim, so its site bar is the one bar nothing generates — and a bar
    // that disagrees with the rest of the site is how a page becomes
    // unreachable from its neighbours. This renders *this* repository and holds
    // the two against each other.
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
    let landing_bar = bar_labels(&landing);
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
    let emitted = bar_labels(&std::fs::read_to_string(&rendered).expect("rendered page"));
    assert_eq!(
        landing_bar,
        emitted,
        "website/public/index.html's site bar disagrees with the bar {} carries",
        rendered.display()
    );
    std::fs::remove_dir_all(&out_dir).ok();
}

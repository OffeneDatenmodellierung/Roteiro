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

/// A prose document longer than extraction's `MAX_CONTENT` budget (1500 chars),
/// so the note it renders into is the *whole* file rather than a prefix, and
/// structured enough that whitespace collapse is visible: headings, a table and a
/// fenced code block all stop being themselves on one line.
fn document() -> String {
    use std::fmt::Write as _;
    let mut s = String::from(
        "# Working offline\n\nRoteiro is **offline-capable**.\n\n\
         | Host | What |\n| --- | --- |\n| `example.com` | models |\n\n\
         ```sh\nroteiro model pull\n```\n\n## Detail\n\n",
    );
    for i in 0..60 {
        let _ = writeln!(s, "Paragraph {i} of the document body.\n");
    }
    s
}

#[test]
fn render_obsidian_gives_a_prose_note_its_whole_source() {
    let dir = fresh_dir("obsidian-prose");
    git(&dir, &["init", "-q"]);
    let doc = document();
    write(&dir, "docs/OFFLINE.md", &doc);
    // An ADR: it carries the same path as its `adr`/`adr_section` nodes, which is
    // how a path-only rule would leak the document into all of them.
    write(
        &dir,
        "docs/adr/0001-example.md",
        "---\nadr-id: \"0001\"\nstatus: Accepted\n---\n\n# ADR-0001: Example\n\n## Context\n\nBecause.\n",
    );
    write(&dir, "src/lib.rs", "/// Doc comment.\npub fn f() {}\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    let out = Command::new(BIN)
        .args(["render", "obsidian", "--out", "vault"])
        .current_dir(&dir)
        .env("ROTEIRO_HOME", &dir)
        .output()
        .expect("run render");
    assert!(out.status.success(), "render obsidian failed: {out:?}");

    let note = std::fs::read_to_string(dir.join("vault/file-docs-OFFLINE.md.md")).expect("note");
    // The whole document, verbatim — not a 1500-char prefix of it.
    assert!(
        note.contains(doc.trim()),
        "the note must reproduce its source: {note}"
    );
    assert!(
        note.contains("Paragraph 59 of the document body."),
        "the tail of the document past the extraction cap is present: {note}"
    );
    // ...and with its structure, which is the half a character count cannot show.
    assert!(
        note.contains("\n| Host | What |\n") && note.contains("\n```sh\n"),
        "a table and a fence need their own lines: {note}"
    );

    // The ADR's own notes are untouched: the document belongs to the `file:` node
    // that is the document, and duplicating it across 1 ADR + 4 section notes is
    // the failure mode of matching on the path alone.
    let adr = std::fs::read_to_string(dir.join("vault/adr-0001.md")).expect("adr note");
    assert!(
        !adr.contains("## Context\n\nBecause."),
        "an adr note is its title, status and links — not the file: {adr}"
    );
    let section = std::fs::read_to_string(dir.join("vault/adr-0001-context.md")).expect("section");
    assert!(
        !section.contains("# ADR-0001: Example"),
        "a section note must not carry the whole document: {section}"
    );

    // A symbol's doc comment is a summary of a definition, not a document, and is
    // unchanged — nothing here widened beyond prose files.
    let sym = std::fs::read_to_string(dir.join("vault/sym-rust-src-lib.rs-f.md")).expect("sym");
    assert!(
        sym.contains("## Content\n\nDoc comment."),
        "doc comments render as before: {sym}"
    );
    assert!(
        !sym.contains("pub fn f()"),
        "a symbol note does not gain its file's source: {sym}"
    );

    // Nor does a *source* file node: only prose paths are selected, so a `.rs`
    // file note is what it always was. Without this, dropping the prose filter
    // would pour every source file into the vault and no test would notice.
    let rs = std::fs::read_to_string(dir.join("vault/file-src-lib.rs.md")).expect("rs note");
    assert!(
        !rs.contains("pub fn f()"),
        "a source file is not prose: {rs}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// An ADR whose two sections are each longer than extraction's `MAX_CONTENT`
/// budget (1500 chars) and whose prose is unmistakably per-section, so a note
/// holding the wrong span, or a truncated one, cannot pass by coincidence.
fn adr_document() -> String {
    use std::fmt::Write as _;
    let mut s = String::from(
        "---\nadr-id: \"0001\"\nstatus: Accepted\n---\n\n\
         # ADR-0001: Example\n\n| | |\n|---|---|\n| **State** | Accepted |\n\n\
         ## Context\n\n",
    );
    for i in 0..40 {
        let _ = writeln!(s, "CONTEXTWORD paragraph {i} about the situation.\n");
    }
    let _ = write!(s, "## Decision\n\n");
    for i in 0..40 {
        let _ = writeln!(s, "DECISIONWORD paragraph {i} about the choice.\n");
    }
    s
}

/// The defect in #545, end to end: an ADR's notes were empty because `rto-spec`
/// stored no content for them, and the renderer had nothing to show.
///
/// The three assertions that matter are *whole*, *own* and *not the document*.
/// Byte counts alone would pass on a note that had merely grown, and a
/// "contains its text" check alone would pass on the note that holds the entire
/// ADR — which is the specific wrong answer a path-only rule produces.
#[test]
fn render_obsidian_gives_an_adr_section_note_its_own_section() {
    let dir = fresh_dir("obsidian-adr");
    git(&dir, &["init", "-q"]);
    let doc = adr_document();
    write(&dir, "docs/adr/0001-example.md", &doc);
    write(&dir, "src/lib.rs", "/// Doc comment.\npub fn f() {}\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    let out = Command::new(BIN)
        .args(["render", "obsidian", "--out", "vault"])
        .current_dir(&dir)
        .env("ROTEIRO_HOME", &dir)
        .output()
        .expect("run render");
    assert!(out.status.success(), "render obsidian failed: {out:?}");

    let read = |name: &str| {
        std::fs::read_to_string(dir.join("vault").join(name))
            .unwrap_or_else(|e| panic!("{name}: {e}"))
    };

    // A section note is its own section, whole.
    let context = read("adr-0001-context.md");
    assert!(
        context.contains("## Content"),
        "the defect: the note had no content at all: {context}"
    );
    assert!(
        context.contains("CONTEXTWORD paragraph 39 about the situation."),
        "the last paragraph of the section is present, so it is not capped: {context}"
    );
    assert!(
        !context.contains("DECISIONWORD"),
        "and the next section's prose is not: {context}"
    );
    assert!(
        !context.contains("| **State** | Accepted |"),
        "nor the preamble: {context}"
    );

    let decision = read("adr-0001-decision.md");
    assert!(
        decision.contains("DECISIONWORD paragraph 39 about the choice."),
        "{decision}"
    );
    assert!(!decision.contains("CONTEXTWORD"), "{decision}");

    // Structure survives: the whole point of rendering the source rather than the
    // whitespace-collapsed `meta.content`. One line would mean the note was built
    // from the store after all.
    assert!(
        content_lines(&context) > 40,
        "the section keeps its line structure, got {} line(s): {context}",
        content_lines(&context)
    );

    // The `adr:` note gets the preamble — and not the body its sections carry, or
    // the whole document would be stored and rendered twice over.
    let adr = read("adr-0001.md");
    assert!(
        adr.contains("| **State** | Accepted |"),
        "the ADR note carries the span that belongs to no section: {adr}"
    );
    assert!(
        !adr.contains("CONTEXTWORD") && !adr.contains("DECISIONWORD"),
        "the ADR note does not restate its sections: {adr}"
    );

    // The `file:` note is still the whole document, as #544 left it. This is what
    // makes the split above a division of labour rather than a loss.
    let file = read("file-docs-adr-0001-example.md.md");
    assert!(
        file.contains("CONTEXTWORD paragraph 39 about the situation.")
            && file.contains("DECISIONWORD paragraph 39 about the choice."),
        "the document note still holds all of it: {file}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// Lines in a note's `## Content` section.
fn content_lines(note: &str) -> usize {
    let body = note.split_once("## Content\n\n").map_or("", |(_, r)| r);
    let body = body.split_once("\n## ").map_or(body, |(head, _)| head);
    body.trim_end().lines().count()
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
    // Issue #456: `docs/BUILD_PLAN.md` cites code as evidence for its claims.
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
        "docs/BUILD_PLAN.md",
        "# Build Plan\n\nEvidence: [sync](../crates/x/src/sync.rs).\n\n\
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

    let plan = std::fs::read_to_string(dir.join("site/build-plan.html")).expect("plan page");
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
        "docs/BUILD_PLAN.md",
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
    let plan = std::fs::read_to_string(dir.join("site/build-plan.html")).expect("plan page");
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

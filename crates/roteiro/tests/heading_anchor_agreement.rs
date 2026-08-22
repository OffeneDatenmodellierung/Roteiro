//! Issue #524: a heading's `id` in the rendered page and its section key in the
//! graph must name the same place, for **every** heading of every published
//! document.
//!
//! They diverged because the two sides implemented the rule twice: `rto_render`
//! honoured an explicit `{#offline}` attribute and `rto_spec` slugified the
//! heading text regardless, so five of this repository's site headings had one
//! address on the page and a different key in the graph — *correct on the
//! surface everyone looks at, wrong in the one tools read.* Both files claimed
//! the agreement in prose while the code only had it on one branch of two.
//!
//! # Why this asserts a relation and pins no strings
//!
//! In the shape #467 established: it does not assert that `modes.md` has a
//! section called `offline`. It renders each page, reads the `id` attributes the
//! renderer actually emitted, parses the same page through the graph's own
//! parser, and asserts the two sequences are equal. That is true by construction
//! and survives any future change to either rule — including a change that
//! renames every anchor, which is exactly the change a literal-string test would
//! obstruct while proving nothing.
//!
//! It is deliberately driven off the **rendered HTML** rather than the
//! renderer's internal id helper: the artifact a reader's browser resolves
//! against is the page, so the page is what has to agree.

use std::path::{Path, PathBuf};

/// Every published site page's `(path, markdown)`, read from the repository.
///
/// The **marker** decides membership, and the walk is repo-wide so that is
/// actually true: site pages are not only under `website/pages` — six of the
/// fourteen live under `docs/` — and a check that greps one directory covers
/// less than it claims. A page that lands somewhere new is picked up here
/// without anyone remembering to add a path.
///
/// `target/` and `.git/` are skipped because they hold build output and object
/// storage, not authored documents; an unreadable entry is skipped rather than
/// failing the walk, since a broken symlink is not a site page.
fn site_pages() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut out = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            let name = entry.file_name();
            if path.is_dir() {
                if !matches!(name.to_str(), Some("target" | ".git")) {
                    stack.push(path);
                }
            } else if path.extension().is_some_and(|e| e == "md")
                && let Ok(text) = std::fs::read_to_string(&path)
                && rto_spec::is_site_page(&text)
            {
                let rel = path
                    .strip_prefix(&root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned();
                out.push((rel, text));
            }
        }
    }
    // A directory walk yields in filesystem order, which differs between
    // machines; the assertions below are per-page, but a stable order keeps a
    // failure reproducible.
    out.sort();
    out
}

/// The `id` of every `<h2>` the renderer emitted, in document order.
///
/// `h2` only, because that is the level `rto_spec` records as a section: a page's
/// `h1` is the document, and an `h3` is body text inside a section. Comparing
/// levels the two sides do not agree to track would fail for a reason that is
/// not this defect.
fn rendered_h2_ids(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(i) = rest.find("<h2") {
        rest = &rest[i + 3..];
        // The tag's own attributes end at the first `>`; an `id=` after that
        // belongs to something else.
        let Some(end) = rest.find('>') else { break };
        let tag = &rest[..end];
        if let Some(j) = tag.find("id=\"") {
            let after = &tag[j + 4..];
            if let Some(k) = after.find('"') {
                out.push(after[..k].to_owned());
            }
        }
        rest = &rest[end..];
    }
    out
}

/// The ids the renderer emits for headings **inside a blockquote**.
///
/// `rto_spec` finds a section by scanning for `## ` at the start of a line;
/// `rto_render` parses, and a heading inside a blockquote is still a heading. So
/// `> ## Title` is an addressable `<h2>` on the page and no section in the graph
/// — the same disagreement #524 is about, reached by a different mechanism
/// (scan versus parse rather than attribute versus text).
///
/// Subtracted as a **relation** rather than listed as an exemption, so it
/// shrinks on its own: un-blockquote the heading and this returns nothing, with
/// no test to remember to edit. One heading in this repository is affected
/// (`docs/BUILD_PLAN_V2.md:16`); indented and setext headings would diverge the
/// same way and none exist today.
fn blockquoted_h2_ids(md: &str) -> Vec<String> {
    md.lines()
        .filter_map(|l| l.trim_start().strip_prefix('>'))
        .filter_map(|l| l.trim_start().strip_prefix("## "))
        .map(rto_graph::heading_id)
        .collect()
}

#[test]
fn every_heading_id_equals_its_graph_section_key() {
    let pages = site_pages();
    assert!(
        pages.len() >= 10,
        "the fixture must find the real site, or this passes vacuously: {}",
        pages.len()
    );

    let mut checked = 0usize;
    for (path, text) in &pages {
        let parsed = rto_spec::parse_site_page(path, text)
            .unwrap_or_else(|e| panic!("{path} is a site page but does not parse: {e}"));
        let graph: Vec<String> = parsed.sections.iter().map(|s| s.slug.clone()).collect();

        let html = rto_render::render_site_page(
            text,
            "fallback",
            &[],
            "",
            &rto_render::PublishedPages::new(),
            None,
        );
        let quoted = blockquoted_h2_ids(text);
        let rendered: Vec<String> = rendered_h2_ids(&html.html)
            .into_iter()
            .filter(|id| !quoted.contains(id))
            .collect();

        assert_eq!(
            rendered, graph,
            "{path}: the rendered heading ids and the graph's section keys \
             disagree — a `[[{path}#section]]` link would resolve in the graph \
             and scroll nowhere in the browser"
        );
        checked += graph.len();
    }

    // The relation above is satisfiable by two empty lists, so say how much was
    // actually compared. Without this the test passes just as loudly against a
    // parser that stopped finding sections at all.
    assert!(
        checked >= 40,
        "only {checked} heading(s) compared across {} page(s) — too few for this \
         to be evidence",
        pages.len()
    );
}

/// The half of #524 that made the divergence invisible: a heading carrying an
/// explicit `{#id}` must be addressed by that id on **both** sides.
///
/// Asserted separately because the relation test above would still pass if both
/// sides ignored the attribute — agreement is necessary and not sufficient, and
/// the ruling on #524 was that the author's declared address wins.
#[test]
fn an_explicit_attribute_is_the_address_on_both_sides() {
    let md = "---\nsite-page: probe\nsite-nav: Probe\nsite-order: 1\n---\n\n\
              # Probe\n\n## 1 · Offline mode — the default {#offline}\n\nBody.\n";

    let parsed = rto_spec::parse_site_page("website/pages/probe.md", md).expect("parses");
    assert_eq!(
        parsed
            .sections
            .iter()
            .map(|s| s.slug.as_str())
            .collect::<Vec<_>>(),
        ["offline"],
        "the graph keys the section by the address the author declared"
    );
    // …and the title still reads as prose, with the attribute gone.
    assert_eq!(parsed.sections[0].title, "1 · Offline mode — the default");

    let html = rto_render::render_site_page(
        md,
        "fallback",
        &[],
        "",
        &rto_render::PublishedPages::new(),
        None,
    );
    assert_eq!(rendered_h2_ids(&html.html), ["offline"]);

    // Taken **verbatim**, not re-slugified. An id chosen to survive `slugify`
    // unchanged would let a re-slugifying implementation pass this test, so the
    // probe uses one that does not: `slugify` would answer `offline-mode`, and
    // answering it would mean the page has an anchor the author never wrote.
    let mixed = md.replace("{#offline}", "{#Offline_Mode}");
    let parsed = rto_spec::parse_site_page("website/pages/probe.md", &mixed).expect("parses");
    assert_eq!(
        parsed.sections[0].slug, "Offline_Mode",
        "the author's address is the key, character for character"
    );
    let html = rto_render::render_site_page(
        &mixed,
        "fallback",
        &[],
        "",
        &rto_render::PublishedPages::new(),
        None,
    );
    assert_eq!(rendered_h2_ids(&html.html), ["Offline_Mode"]);
}

/// The known divergence, asserted rather than merely subtracted.
///
/// `blockquoted_h2_ids` quietly removes this class from the comparison above, and
/// a quiet subtraction is indistinguishable from a bug that happens to cancel.
/// So the class is stated here as its own fact: the page addresses the heading,
/// the graph does not record it.
///
/// This is **not** the `{#id}` defect wearing another hat — it is the scan-versus-
/// parse half, and closing it means `rto_spec` parsing for sections rather than
/// scanning lines, which also decides indented and setext headings. Filed
/// separately rather than widened into #524.
#[test]
fn a_blockquoted_heading_is_addressable_in_the_page_and_absent_from_the_graph() {
    let md = "---\nsite-page: probe\nsite-nav: Probe\nsite-order: 1\n---\n\n\
              # Probe\n\n> ## A quoted heading\n\n## A real one\n";

    let parsed = rto_spec::parse_site_page("website/pages/probe.md", md).expect("parses");
    assert_eq!(
        parsed
            .sections
            .iter()
            .map(|s| s.slug.as_str())
            .collect::<Vec<_>>(),
        ["a-real-one"],
        "the graph's line scan does not see a heading inside a blockquote"
    );

    let html = rto_render::render_site_page(
        md,
        "fallback",
        &[],
        "",
        &rto_render::PublishedPages::new(),
        None,
    );
    assert_eq!(
        rendered_h2_ids(&html.html),
        ["a-quoted-heading", "a-real-one"],
        "…while the renderer parses, so the page addresses both"
    );

    // And the helper that reconciles them names exactly the extra one, so the
    // subtraction above cannot hide a second, unrelated difference.
    assert_eq!(blockquoted_h2_ids(md), ["a-quoted-heading"]);
}

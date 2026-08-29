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

use std::path::Path;

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
        // ` id="`, with the leading space, so `data-id="` and `aria-id="` are not
        // mistaken for the attribute a browser resolves a fragment against. A
        // bare `id="` search would read the wrong value and — worse here — would
        // read it *silently*, since the comparison is against another list this
        // test computed rather than against a constant anyone would recognise.
        if let Some(j) = tag.find(" id=\"") {
            let after = &tag[j + 5..];
            if let Some(k) = after.find('"') {
                out.push(after[..k].to_owned());
            }
        }
        rest = &rest[end..];
    }
    out
}

/// Every id the renderer emits for a site page is a section key in the graph, and
/// vice versa — compared page by page across the real site.
///
/// A link into a section resolves through the graph and lands through the `id`,
/// so the moment the two disagree the graph says a place exists and the browser
/// scrolls nowhere. Asserting the **relation** rather than literal strings is
/// what makes this test find divergences nobody had thought of: it was written
/// for the `{#id}` defect (#524) and immediately produced a different one — the
/// blockquoted heading of #621.
///
/// **That exemption is gone.** This used to subtract blockquoted ids from the
/// comparison, because `rto_spec` scanned for `## ` and could not see them.
/// `rto_spec` parses now, so there is no class held back and no subtraction to
/// keep honest — which matters, because an agreement test is only ever as strong
/// as the things it declines to compare.
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
        // No subtraction any more. This comparison used to remove blockquoted
        // headings, because the graph scanned for `## ` at column 0 and could not
        // see them (#621). `rto_spec` parses now, so the relation is exact: every
        // id the renderer emits is a section key, with no class held back.
        let rendered = rendered_h2_ids(&html.html);

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

/// The divergence #621 recorded, now asserted as **agreement**.
///
/// This test used to say the opposite, and deliberately: the page addressed a
/// blockquoted heading and the graph did not record it, because `rto_spec`
/// scanned for `## ` at column 0 while the renderer parsed. It was stated as its
/// own fact rather than left as a quiet subtraction, precisely so that closing it
/// would show up here as a failing assertion rather than as nothing at all.
///
/// It did. Kept — inverted — rather than deleted, because the class is the one a
/// line scan would silently lose again.
#[test]
fn a_blockquoted_heading_is_addressable_in_the_page_and_recorded_in_the_graph() {
    let md = "---\nsite-page: probe\nsite-nav: Probe\nsite-order: 1\n---\n\n\
              # Probe\n\n> ## A quoted heading\n\n## A real one\n";

    let parsed = rto_spec::parse_site_page("website/pages/probe.md", md).expect("parses");
    assert_eq!(
        parsed
            .sections
            .iter()
            .map(|s| s.slug.as_str())
            .collect::<Vec<_>>(),
        ["a-quoted-heading", "a-real-one"],
        "a heading inside a blockquote is a heading, and the graph now records it"
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
        "…and the page addresses both, as it always did"
    );
}

/// The other two classes #621 measured at zero instances, so nothing in the real
/// site exercises them — which is exactly why they need a fixture.
///
/// An indented heading and a setext heading are headings to the parser and to the
/// renderer. Under the old line scan both were invisible to the graph, and both
/// arrive through ordinary authoring rather than deliberate cleverness.
#[test]
fn indented_and_setext_headings_are_sections_too() {
    let md = "---\nsite-page: probe2\nsite-nav: Probe\nsite-order: 2\n---\n\n\
              # Probe\n\n\u{20}\u{20}## Indented by two\n\nSetext heading\n---\n\n## Plain\n";

    let parsed = rto_spec::parse_site_page("website/pages/probe2.md", md).expect("parses");
    assert_eq!(
        parsed
            .sections
            .iter()
            .map(|s| s.slug.as_str())
            .collect::<Vec<_>>(),
        ["indented-by-two", "setext-heading", "plain"],
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
        ["indented-by-two", "setext-heading", "plain"],
        "the two sides agree on all three"
    );
}

/// A `## ` inside a fenced block is the mirror error: a scan counts it, a parser
/// knows it is a code sample. Asserted because the fix could plausibly have
/// traded one direction of the bug for the other.
#[test]
fn a_hash_hash_inside_a_fence_is_a_code_sample_not_a_section() {
    let md = "---\nsite-page: probe3\nsite-nav: Probe\nsite-order: 3\n---\n\n\
              # Probe\n\n```\n## Not a heading\n```\n\n## Real\n";

    let parsed = rto_spec::parse_site_page("website/pages/probe3.md", md).expect("parses");
    assert_eq!(
        parsed
            .sections
            .iter()
            .map(|s| s.slug.as_str())
            .collect::<Vec<_>>(),
        ["real"],
    );
}

/// `rendered_h2_ids` reads the `id` attribute and not one that merely ends in
/// `id`.
///
/// A bare `id="` search also matches `data-id="` and `aria-labelledby-id="`, and
/// it would fail *silently*: this test compares two computed lists, so a reader
/// would see two plausible slugs disagree and look for the bug in the slug rule
/// rather than in the scraper. The renderer emits no such attribute today, which
/// is exactly why the guard has to be explicit rather than incidental.
#[test]
fn only_the_id_attribute_is_read_not_one_that_merely_ends_in_id() {
    assert_eq!(
        rendered_h2_ids(r#"<h2 data-id="wrong" id="right">Title</h2>"#),
        ["right"]
    );
    assert_eq!(
        rendered_h2_ids(r#"<h2 id="first">A</h2><p>x</p><h2 id="second">B</h2>"#),
        ["first", "second"]
    );
    // An `id` on something that is not an h2 is not a section anchor.
    assert!(rendered_h2_ids(r#"<h3 id="deeper">C</h3>"#).is_empty());
}

/// Two headings claiming one id: the page addresses both, so the graph must key
/// both (#629).
///
/// The renderer suffixed the second to `same-2` and `rto_spec` keyed both
/// `same`, so `SitePageDoc::facts` built the same node twice and the second
/// **upserted the first out of the graph** — section A gone, and the `same-2`
/// anchor the page publishes addressable by nothing.
///
/// A synthetic fixture rather than a corpus sweep, and deliberately: measured
/// across every markdown file in the repository there are **zero** duplicate
/// heading ids, so `every_heading_id_equals_its_graph_section_key` passes on the
/// broken implementation exactly as loudly as on the fixed one. A guard held up
/// by the absence of the input is not a guard.
#[test]
fn two_headings_claiming_one_id_are_two_sections_on_both_sides() {
    let md = "---\nsite-page: probe4\nsite-nav: Probe\nsite-order: 4\n---\n\n\
              # Probe\n\n## A {#same}\n\n## B {#same}\n";

    let parsed = rto_spec::parse_site_page("website/pages/probe4.md", md).expect("parses");
    assert_eq!(
        parsed
            .sections
            .iter()
            .map(|s| (s.slug.as_str(), s.title.as_str()))
            .collect::<Vec<_>>(),
        [("same", "A"), ("same-2", "B")],
        "both sections survive, and the second is keyed by the address the page \
         actually gives it"
    );

    let html = rto_render::render_site_page(
        md,
        "fallback",
        &[],
        "",
        &rto_render::PublishedPages::new(),
        None,
    );
    assert_eq!(rendered_h2_ids(&html.html), ["same", "same-2"]);
}

/// The case that makes a dedup *local to either side* the wrong fix: the id an
/// `<h2>` gets depends on headings that are not `<h2>`s.
///
/// `# Same` then `## Same` renders as `same` / `same-2`, because the renderer
/// numbers across every level. `rto_spec` records only `##` sections, so a dedup
/// over its own subset would key that h2 `same` — agreeing with nothing, and
/// wrong in a way the duplicate-`##` case above cannot reveal. The numbering has
/// to be computed over all headings and filtered afterwards, which is why it
/// lives in `rto_graph::headings` and not in either caller.
///
/// Asserted for an `h1` above and an `h3` below, so a fix that happened to count
/// only *outer* levels fails too.
#[test]
fn an_h2_whose_id_another_level_took_is_suffixed_on_both_sides() {
    for other in ["# Same", "### Same"] {
        let md = format!(
            "---\nsite-page: probe5\nsite-nav: Probe\nsite-order: 5\n---\n\n\
             # Probe\n\n{other}\n\n## Same\n"
        );

        let parsed = rto_spec::parse_site_page("website/pages/probe5.md", &md).expect("parses");
        assert_eq!(
            parsed
                .sections
                .iter()
                .map(|s| s.slug.as_str())
                .collect::<Vec<_>>(),
            ["same-2"],
            "{other}: the graph keys the h2 by the anchor the page gives it, not \
             by what it would have got had it been alone"
        );

        let html = rto_render::render_site_page(
            &md,
            "fallback",
            &[],
            "",
            &rto_render::PublishedPages::new(),
            None,
        );
        assert_eq!(rendered_h2_ids(&html.html), ["same-2"], "{other}");
    }
}

/// The other document-level rule that moved with the dedup: a heading that names
/// nothing is numbered by its **position among all headings**.
///
/// `## ###` slugifies to the empty string. The renderer has always given it
/// `section-N`; the graph keyed it `""`, so `site:probe#` was a node naming an
/// anchor no page has. Both rules were in the renderer for the same stated
/// reason and both had the same defect, so both moved — leaving one behind would
/// have kept a divergence of exactly the shape being fixed.
///
/// `section-2`, not `section-1`: the page's `# Probe` is the first heading. That
/// is the all-levels counting again, in the other rule.
#[test]
fn a_heading_that_names_nothing_is_numbered_by_position_on_both_sides() {
    let md = "---\nsite-page: probe6\nsite-nav: Probe\nsite-order: 6\n---\n\n\
              # Probe\n\n## ###\n\n## Real\n";

    let parsed = rto_spec::parse_site_page("website/pages/probe6.md", md).expect("parses");
    assert_eq!(
        parsed
            .sections
            .iter()
            .map(|s| s.slug.as_str())
            .collect::<Vec<_>>(),
        ["section-2", "real"],
    );

    let html = rto_render::render_site_page(
        md,
        "fallback",
        &[],
        "",
        &rto_render::PublishedPages::new(),
        None,
    );
    assert_eq!(rendered_h2_ids(&html.html), ["section-2", "real"]);
}

/// An ADR is rendered to a page too, so it has both halves of #629 — and its
/// parser still finds sections by scanning for `## `.
///
/// That scan is the reason the id cannot be deduplicated where it is computed:
/// the numbering has to come from the document-wide parse, which the scan then
/// reads by byte offset. This asserts the outcome rather than the mechanism —
/// two sections, each keyed by the anchor the rendered ADR gives it, with **both
/// bodies** intact, since the upsert took a section's text with it.
#[test]
fn an_adr_with_two_headings_claiming_one_id_keeps_both_sections() {
    let md = "---\ntype: adr\nadr-id: 9999\nstatus: Accepted\ntitle: Probe\n---\n\n\
              # Probe\n\n## Notes {#n}\n\nfirst\n\n## Other {#n}\n\nsecond\n";

    let doc = rto_spec::parse_adr("docs/adr/9999-probe.md", md).expect("parses");
    assert_eq!(
        doc.sections
            .iter()
            .map(|s| (s.slug.as_str(), s.title.as_str(), s.text.as_str()))
            .collect::<Vec<_>>(),
        [("n", "Notes", "first"), ("n-2", "Other", "second")],
    );

    let rendered = rto_render::render_adr(md, "fallback", &rto_render::PublishedPages::new(), None);
    assert_eq!(rendered_h2_ids(&rendered.html), ["n", "n-2"]);
}

/// How far #629's fix reaches into an ADR, stated so the next reader does not
/// have to infer it from silence.
///
/// #621 moved `rto_spec::site` from scanning to parsing. It did **not** move
/// `parse_adr`, which still decides *which* headings are sections by looking for
/// `## ` at column 0 — so a blockquoted heading in an ADR is addressable on the
/// rendered ADR page and absent from the graph, exactly the defect #621 closed
/// one document class over. That is #621's remaining half and not this one's.
///
/// What #629 does reach is the **id** of the sections the scan does find, and
/// this is where the two interact: the h2 below is keyed `quoted-2`, because the
/// numbering comes from a parse that saw the blockquoted heading take `quoted`
/// first. A dedup written into the scan could not have known that and would have
/// keyed it `quoted` — a link resolving in the graph and scrolling nowhere.
///
/// So the missing section is a gap, and the surviving one is correct. Those are
/// different failures, and only the first is still open.
#[test]
fn an_adrs_sections_are_still_scanned_for_but_keyed_by_the_parse() {
    let md = "---\ntype: adr\nadr-id: 9998\nstatus: Accepted\ntitle: Probe\n---\n\n\
              # Probe\n\n> ## Quoted\n\n## Quoted\n";

    let doc = rto_spec::parse_adr("docs/adr/9998-probe.md", md).expect("parses");
    assert_eq!(
        doc.sections
            .iter()
            .map(|s| s.slug.as_str())
            .collect::<Vec<_>>(),
        ["quoted-2"],
        "the blockquoted heading is not a section here (#621's remaining half), \
         and the one that is gets the id the page gives it (#629)"
    );

    let rendered = rto_render::render_adr(md, "fallback", &rto_render::PublishedPages::new(), None);
    assert_eq!(
        rendered_h2_ids(&rendered.html),
        ["quoted", "quoted-2"],
        "the page addresses both, as it always did"
    );
}

/// A blueprint is never rendered, so the anchor half of #629 cannot reach it —
/// the lost node is the whole of the damage, and it is enough.
///
/// `BlueprintDoc::facts` builds `blueprint:<path>#<slug>` per section, so two
/// `## Notes` headings produced one node and the second silently replaced the
/// first. Asserted here because "no renderer, no divergence" is the argument
/// that would justify leaving this parser alone, and it is wrong.
#[test]
fn a_blueprint_with_two_identical_headings_keeps_both_sections() {
    let md = "# Thing — Technical Implementation Plan\n\n## Notes\n\nfirst\n\n## Notes\n\nsecond\n";

    let doc = rto_spec::parse_blueprint("docs/plans/thing.md", md);
    assert_eq!(
        doc.sections
            .iter()
            .map(|s| s.slug.as_str())
            .collect::<Vec<_>>(),
        ["notes", "notes-2"],
    );

    let facts = doc.facts();
    let section_nodes = facts
        .nodes
        .iter()
        .filter(|n| n.key.starts_with("blueprint:docs/plans/thing.md#"))
        .count();
    assert_eq!(
        section_nodes, 2,
        "two headings, two nodes — the upsert is what #629 was"
    );
}

/// A `[[…]]` **inside a heading** is the one construct on which the two sides
/// still disagree — stated here, with its measurement, rather than left to be
/// rediscovered.
///
/// `rto_render` rewrites wiki-links to their display form *before* computing ids
/// (`docs.rs`: `rewrite_wiki_links` then `heading_ids`), so it sees `See
/// ADR-0001`. `rto_spec` reads the authored source and sees the raw link. For
/// `## See [[docs/adr/0001-….md]]`:
///
/// ```text
/// graph     see-docs-adr-0001-build-roteiro-unified-codebase-knowledge-graph-md
/// rendered  see-adr-0001
/// ```
///
/// **Not introduced by #621** — the old line scan read the same raw text, so this
/// predates parsing — and **0 headings in `website/pages` or `docs` contain a
/// wiki-link**, which is why the agreement test above passes.
///
/// Left as a stated limit rather than fixed because the fix is a real choice, not
/// an oversight. Either `rto_spec` learns the render-time link rewriting — which
/// needs the ADR prefix, an HTML concern the authored layer has no business
/// knowing — or the renderer computes ids from the authored source instead of the
/// rewritten text, changing the anchor of any such heading. Neither is obviously
/// right for zero occurrences.
///
/// It is not unguarded: a real instance fails `every_heading_id_equals_its_graph_section_key`
/// loudly, because that comparison renders the whole page.
#[test]
fn a_wiki_link_in_a_heading_is_the_one_remaining_disagreement() {
    let md = "---\nsite-page: p\nsite-nav: P\nsite-order: 1\n---\n\n\
              # P\n\n## See [[docs/adr/0001-build-roteiro-unified-codebase-knowledge-graph.md]]\n";

    let parsed = rto_spec::parse_site_page("website/pages/p.md", md).expect("parse");
    assert_eq!(
        parsed.sections[0].slug,
        "see-docs-adr-0001-build-roteiro-unified-codebase-knowledge-graph-md",
        "the graph keys from the authored source"
    );

    let html = rto_render::render_site_page(
        md,
        "adr/",
        &[],
        "",
        &rto_render::PublishedPages::new(),
        None,
    );
    assert_eq!(
        rendered_h2_ids(&html.html),
        ["see-adr-0001"],
        "the renderer keys from the display form it rewrote first"
    );
}

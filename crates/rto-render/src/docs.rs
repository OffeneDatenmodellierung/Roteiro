//! The documentation-site renderer: ADR markdown → themed HTML pages, produced
//! deterministically so CI diffs are meaningful. Replaces the shell
//! `md2html.awk` stopgap with a real `CommonMark` parser (`pulldown-cmark`),
//! fixing the whole class of hand-rolled-parser bugs (backtick runs, tables,
//! heading edge cases) we hit before.
//!
//! Page chrome (theme, nav, back-link, footer) matches the previous site so the
//! switch is drop-in. This module is pure string generation; the `roteiro`
//! binary owns walking `docs/adr` and copying static assets.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use pulldown_cmark::{CowStr, Event, Options, Parser, Tag, TagEnd, html};

/// A rendered ADR: its title (for the index) and the full themed HTML page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedAdr {
    /// The ADR title (first `# ` heading, or the fallback passed to
    /// [`render_adr`]).
    pub title: String,
    /// The complete HTML document.
    pub html: String,
}

/// Where each source document is **actually published**: the file the site
/// serves, keyed by the source markdown's file name.
///
/// [`rewrite_doc_link`] used to derive a link's target from the link's own
/// spelling — `../BUILD_PLAN_V2.md` → `../BUILD_PLAN_V2.html` — which is correct
/// only while every document is served under its own stem. Site pages ended
/// that: a page is published as its declared `site-page:` slug, and a slug is
/// URL-safe by construction (`[a-z0-9-]+`), so `docs/BUILD_PLAN_V2.md` is served
/// as `build-plan-v2.html`. The rewrite then pointed four correct repository
/// links at a page that is never emitted — issue #446, live on roteiro.dev.
///
/// So the served name is *looked up* rather than guessed. The renderer is handed
/// the index of what the site emits, which is the only thing that knows the
/// answer.
///
/// Keyed by file name rather than by full path because the site mirrors the
/// repository's layout — `docs/*.md` at the root, `docs/adr/*.md` under `adr/` —
/// so a link's directory hops are already correct and only the final segment can
/// differ. A file name claimed by two published documents is recorded as
/// **ambiguous** and left unrewritten: guessing which one a link meant is how a
/// link silently points at the wrong page, which is worse than the 404 it
/// replaces.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PublishedPages(BTreeMap<String, Option<String>>);

impl PublishedPages {
    /// An empty index: every `.md` link falls back to its own stem.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `source_file` (a markdown file name, e.g. `BUILD_PLAN_V2.md`)
    /// is served as `served_as` (e.g. `build-plan-v2.html`).
    ///
    /// A second, differing claim on one file name makes it ambiguous; see the
    /// type's documentation for why that is left unrewritten.
    pub fn publish(&mut self, source_file: &str, served_as: &str) {
        self.0
            .entry(source_file.to_owned())
            .and_modify(|slot| {
                if slot.as_deref() != Some(served_as) {
                    *slot = None;
                }
            })
            .or_insert_with(|| Some(served_as.to_owned()));
    }

    /// The file `source_file` is served as, or `None` when it is unknown or
    /// ambiguous.
    fn served(&self, source_file: &str) -> Option<&str> {
        self.0.get(source_file)?.as_deref()
    }
}

/// One page in the site navigation bar: where it goes and what it is called.
///
/// Built by the caller from the authored site pages (`rto_spec::site_nav` puts
/// them in order), and passed to [`render_site_page`] whole so every page emits
/// the *same* bar. A per-page bar assembled independently is a bar that can
/// disagree with itself, which is how a page ends up unreachable from its
/// neighbours.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavEntry {
    /// Root-relative href (e.g. `modes.html`, or `./` for the landing page).
    pub href: String,
    /// Short label shown in the bar.
    pub label: String,
}

/// An entry in the ADR/docs index page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    /// Relative href (e.g. `0001-….html`).
    pub href: String,
    /// Display title.
    pub title: String,
}

/// Convert `CommonMark` `md` to an HTML fragment (GitHub tables + strikethrough,
/// and Roteiro `[[wiki-links]]` resolved). Resolves ADR links relative to the
/// ADR directory; use [`render_doc`] for root-level pages.
///
/// A fragment renderer has no site to be a part of, so it carries no
/// [`PublishedPages`]: a `.md` link is rewritten to its own stem, which is right
/// for an ADR and a guess for anything published under a slug.
#[must_use]
pub fn markdown_to_html(md: &str) -> String {
    render_markdown(md, "", &PublishedPages::new())
}

/// Render `md` to HTML: resolve `[[wiki-links]]` (ADR links use `adr_prefix` as
/// their href prefix), rewrite ordinary `[…](*.md)` links to their rendered
/// `.html` targets, then run `CommonMark` with GitHub tables/strikethrough.
fn render_markdown(md: &str, adr_prefix: &str, pages: &PublishedPages) -> String {
    let pre = rewrite_wiki_links(md, adr_prefix);
    let ids = heading_ids(&pre);
    let mut next_id = 0usize;
    // Rewrite link destinations pointing at local Markdown files to the HTML the
    // site actually serves (e.g. `adr/0001-….md` → `adr/0001-….html`), and give
    // every heading a stable `id` so it can be linked to.
    let parser = Parser::new_ext(&pre, options()).map(|event| match event {
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Link {
            link_type,
            dest_url: rewrite_doc_link(&dest_url, pages).map_or(dest_url, CowStr::from),
            title,
            id,
        }),
        Event::Start(Tag::Heading {
            level,
            classes,
            attrs,
            ..
        }) => {
            let id = ids.get(next_id).cloned().map(CowStr::from);
            next_id += 1;
            Event::Start(Tag::Heading {
                level,
                id,
                classes,
                attrs,
            })
        }
        other => other,
    });
    let mut out = String::new();
    html::push_html(&mut out, parser);
    out
}

/// The `CommonMark` dialect the whole site is parsed with: GitHub tables and
/// strikethrough, plus **heading attributes** (`## Heading {#anchor}`).
///
/// Heading attributes are how a URL outlives a restructure. A page split out of
/// the old single-page site keeps the anchor the old page published — the
/// heading declares `{#modes}` and lands at `#modes` — instead of silently
/// becoming whatever the new heading text happens to slugify to. External links
/// point at those anchors and cannot be updated, so the alternative is not a
/// tidier URL; it is a dead one.
fn options() -> Options {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_HEADING_ATTRIBUTES);
    opts
}

/// The `id` for every heading in `md`, in document order.
///
/// An explicit `{#anchor}` wins; otherwise the id is [`rto_graph::slugify`] of
/// the heading text — the same function that builds the section's node key, so
/// an authored link to `site:modes#offline-mode` lands on the heading the graph
/// says it does. A heading whose text slugifies to nothing (`## ###`) falls back
/// to its position, and a repeat gets a `-2`, `-3`, … suffix, because two
/// elements sharing an `id` means one of them is unreachable.
///
/// Computed from a *first parse* rather than a line scan: heading text can be
/// spread over several inline events, and `#` inside a fenced block is not a
/// heading at all. Parsing twice costs a document-sized pass and cannot be wrong
/// about what the renderer will see, because it is the same parser.
fn heading_ids(md: &str) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut current: Option<(Option<String>, String)> = None;
    for event in Parser::new_ext(md, options()) {
        match event {
            Event::Start(Tag::Heading { id, .. }) => {
                current = Some((id.map(|i| i.to_string()), String::new()));
            }
            Event::Text(t) | Event::Code(t) => {
                if let Some((_, text)) = current.as_mut() {
                    text.push_str(&t);
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                let Some((explicit, text)) = current.take() else {
                    continue;
                };
                let base = explicit
                    .filter(|e| !e.is_empty())
                    .unwrap_or_else(|| rto_graph::slugify(&text));
                let base = if base.is_empty() {
                    format!("section-{}", ids.len() + 1)
                } else {
                    base
                };
                let n = seen.entry(base.clone()).or_insert(0);
                *n += 1;
                ids.push(if *n == 1 { base } else { format!("{base}-{n}") });
            }
            _ => {}
        }
    }
    ids
}

/// Rewrite a relative link to a local Markdown file so it points at the rendered
/// HTML page the site serves, preserving any `#fragment`. Returns `None` for
/// external, protocol-relative, `mailto:`, pure-anchor, or non-`.md` links, which
/// are left unchanged.
fn rewrite_doc_link(dest: &str, pages: &PublishedPages) -> Option<String> {
    if dest.starts_with("http://")
        || dest.starts_with("https://")
        || dest.starts_with("//")
        || dest.starts_with("mailto:")
        || dest.starts_with('#')
    {
        return None;
    }
    let (path, frag) = dest
        .split_once('#')
        .map_or((dest, None), |(p, f)| (p, Some(f)));
    path.strip_suffix(".md")?;
    // Only the final segment can differ between the repository and the site, so
    // the link's own directory hops are kept verbatim; see [`PublishedPages`].
    let (dir, file) = path.rsplit_once('/').map_or(("", path), |(d, f)| (d, f));
    let served = match pages.served(file) {
        Some(served) => served.to_owned(),
        // Unknown or ambiguous: fall back to the stem rewrite this has always
        // done, which is right for every ADR (each is served under its own
        // stem) and no worse than before for anything else.
        None => format!("{}.html", file.trim_end_matches(".md")),
    };
    let sep = if dir.is_empty() { "" } else { "/" };
    Some(match frag {
        Some(frag) => format!("{dir}{sep}{served}#{frag}"),
        None => format!("{dir}{sep}{served}"),
    })
}

/// Render one ADR markdown document to a themed HTML page. Leading YAML
/// frontmatter is stripped; the title is the first `# ` heading, or `fallback`
/// if there is none. ADR `[[…]]` links resolve to sibling ADR pages.
#[must_use]
pub fn render_adr(markdown: &str, fallback_title: &str, pages: &PublishedPages) -> RenderedAdr {
    let body = strip_frontmatter(markdown);
    let title = first_heading(body).unwrap_or_else(|| fallback_title.to_owned());
    let content = render_markdown(body, "", pages);
    let nav = "<p class=\"nav\"><a href=\"../\">← Roteiro home</a> · \
               <a href=\"./\">All ADRs</a> · <a href=\"../build-plan.html\">Build Plan</a></p>";
    let html = page(&format!("{title} — Roteiro"), "../", nav, &content);
    RenderedAdr { title, html }
}

/// Render a root-level "lifetime doc" (e.g. the Build Plan) to a themed page.
/// Its `[[docs/adr/…]]` links resolve into the `adr/` subdirectory.
#[must_use]
pub fn render_doc(markdown: &str, fallback_title: &str, pages: &PublishedPages) -> RenderedAdr {
    let body = strip_frontmatter(markdown);
    let title = first_heading(body).unwrap_or_else(|| fallback_title.to_owned());
    let content = render_markdown(body, "adr/", pages);
    let nav = "<p class=\"nav\"><a href=\"./\">← Roteiro home</a> · \
               <a href=\"adr/\">ADRs</a></p>";
    let html = page(&format!("{title} — Roteiro"), "./", nav, &content);
    RenderedAdr { title, html }
}

/// Render one **site page** — a document that declared itself published — to a
/// themed root-level page carrying the site navigation bar.
///
/// `nav` is the whole bar, in order; `current_href` is this page's own entry,
/// which is marked `aria-current="page"` and rendered unlinked so the reader can
/// see where they are. A `current_href` that matches nothing in `nav` simply
/// yields a bar with nothing marked, which is what a preview of an unlisted page
/// should look like rather than an error.
///
/// The title is the first `# ` heading, or `fallback_title`. `[[docs/adr/…]]`
/// links resolve into the `adr/` subdirectory, exactly as they do for the Build
/// Plan: a site page is a root-level document.
#[must_use]
pub fn render_site_page(
    markdown: &str,
    fallback_title: &str,
    nav: &[NavEntry],
    current_href: &str,
    pages: &PublishedPages,
) -> RenderedAdr {
    let body = strip_frontmatter(markdown);
    let title = first_heading(body).unwrap_or_else(|| fallback_title.to_owned());
    let content = render_markdown(body, "adr/", pages);
    let bar = render_nav(nav, current_href);
    let html = page(&format!("{title} — Roteiro"), "./", &bar, &content);
    RenderedAdr { title, html }
}

/// The site navigation bar: one link per page, the current one marked.
///
/// Plain anchors in a `<nav>`, styled by `website/public/style.css`. No script:
/// the explorer is deliberately vendored with no build step (ADR-0010), and a
/// navigation bar that needs JavaScript to be a navigation bar would be the
/// first thing on this site that does.
#[must_use]
pub fn render_nav(nav: &[NavEntry], current_href: &str) -> String {
    let mut out = String::from("<nav class=\"sitenav\">");
    for entry in nav {
        if entry.href == current_href {
            let _ = write!(
                out,
                "<span aria-current=\"page\">{}</span>",
                escape_html(&entry.label)
            );
        } else {
            let _ = write!(
                out,
                "<a href=\"{}\">{}</a>",
                escape_attr(&entry.href),
                escape_html(&entry.label)
            );
        }
    }
    out.push_str("</nav>");
    out
}

/// Render the docs index: any `lifetime` docs (Build Plan, …) then the ADRs.
#[must_use]
pub fn render_adr_index(lifetime: &[IndexEntry], entries: &[IndexEntry]) -> String {
    let mut list = String::new();
    if !lifetime.is_empty() {
        list.push_str("<h1>Documentation</h1><ul>");
        for e in lifetime {
            let _ = write!(
                list,
                "<li><a href=\"{}\">{}</a></li>",
                escape_attr(&e.href),
                escape_html(&e.title)
            );
        }
        list.push_str("</ul>");
    }
    list.push_str("<h1>Architecture Decision Records</h1><ul>");
    for e in entries {
        let _ = write!(
            list,
            "<li><a href=\"{}\">{}</a></li>",
            escape_attr(&e.href),
            escape_html(&e.title)
        );
    }
    list.push_str("</ul>");
    let nav = "<p class=\"nav\"><a href=\"../\">← Roteiro home</a></p>";
    page("Documentation — Roteiro", "../", nav, &list)
}

/// Rewrite Roteiro `[[wiki-links]]` into Markdown, honouring code spans/fences:
/// `[[docs/adr/<slug>.md]]` (optionally `#section`) becomes a link to that ADR
/// page (`<adr_prefix><slug>.html`); any other `[[…]]` (code/file references,
/// for which the site has no page) becomes inline code so it renders cleanly
/// instead of leaking literal brackets.
fn rewrite_wiki_links(md: &str, adr_prefix: &str) -> String {
    let mut out = String::new();
    let mut in_fence = false;
    for line in md.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if in_fence {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        rewrite_line_outside_code(line, adr_prefix, &mut out);
        out.push('\n');
    }
    out
}

/// Rewrite wiki-links in one line, leaving `CommonMark` inline code spans
/// untouched. A code span opens with a run of *n* backticks and closes with the
/// next run of *exactly* *n* backticks; anything between (including `[[…]]`
/// examples) is emitted verbatim. Backtick runs with no matching close are
/// literal text and do not shield what follows.
fn rewrite_line_outside_code(line: &str, adr_prefix: &str, out: &mut String) {
    let bytes = line.as_bytes();
    let mut text_start = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'`' {
            i += 1;
            continue;
        }
        let run_start = i;
        while i < bytes.len() && bytes[i] == b'`' {
            i += 1;
        }
        let run = i - run_start;
        if let Some(rel) = find_closing_run(&bytes[i..], run) {
            // Text before the opening delimiter is ordinary prose.
            rewrite_wiki_in(&line[text_start..run_start], adr_prefix, out);
            let code_end = i + rel + run;
            out.push_str(&line[run_start..code_end]); // span, delimiters included
            i = code_end;
            text_start = i;
        }
        // No close → treat the run as literal text; keep it in the pending
        // buffer (rewrite_wiki_in leaves backticks alone) and keep scanning.
    }
    rewrite_wiki_in(&line[text_start..], adr_prefix, out);
}

/// Byte offset (within `bytes`) of the next backtick run of *exactly* `run`
/// backticks, or `None`. Longer or shorter runs are skipped, per `CommonMark`.
fn find_closing_run(bytes: &[u8], run: usize) -> Option<usize> {
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'`' {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && bytes[i] == b'`' {
            i += 1;
        }
        if i - start == run {
            return Some(start);
        }
    }
    None
}

/// Rewrite every `[[…]]` in one non-code text segment.
fn rewrite_wiki_in(seg: &str, adr_prefix: &str, out: &mut String) {
    let mut rest = seg;
    while let Some(open) = rest.find("[[") {
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        if let Some(close) = after.find("]]") {
            out.push_str(&wiki_target(&after[..close], adr_prefix));
            rest = &after[close + 2..];
        } else {
            out.push_str("[[");
            rest = after;
        }
    }
    out.push_str(rest);
}

/// Resolve one wiki-link's inner text to Markdown.
fn wiki_target(inner: &str, adr_prefix: &str) -> String {
    let inner = inner.trim();
    let path = inner.split_once('#').map_or(inner, |(p, _)| p.trim());
    if let Some(rest) = path.strip_prefix("docs/adr/")
        && let Some(stem) = rest.strip_suffix(".md")
    {
        return format!("[{}]({adr_prefix}{stem}.html)", adr_label(stem));
    }
    // Code/file reference — the site has no page for it; show it as code.
    format!("`{inner}`")
}

/// A display label for an ADR filename stem: `0001-build-…` → `ADR-0001`.
fn adr_label(stem: &str) -> String {
    let digits: String = stem.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        stem.to_owned()
    } else {
        format!("ADR-{digits}")
    }
}

/// Wrap body HTML in the themed page chrome. `root` is the relative path to the
/// site root (e.g. `"../"` for pages under `adr/`).
fn page(title: &str, root: &str, nav: &str, body: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <link rel=\"icon\" href=\"{root}favicon.svg\" type=\"image/svg+xml\">\
         <link rel=\"icon\" href=\"{root}favicon.ico\" type=\"image/x-icon\" sizes=\"16x16 32x32 48x48\">\
         <link rel=\"apple-touch-icon\" href=\"{root}apple-touch-icon.png\">\
         <link rel=\"stylesheet\" href=\"{root}style.css\">\
         <title>{title}</title></head><body>\
         {nav}{body}\
         <p class=\"backlink\"><a href=\"{root}\">← Back to roteiro.dev</a></p>\
         <footer>Dual-licensed MIT OR Apache-2.0 · The Roteiro Project Team</footer>\
         </body></html>",
        title = escape_html(title),
    )
}

/// Strip a leading `---`-delimited YAML frontmatter block.
fn strip_frontmatter(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("---\n") else {
        return text;
    };
    match rest.find("\n---\n") {
        Some(end) => &rest[end + 5..],
        None => rest.strip_suffix("\n---").unwrap_or(text),
    }
}

/// The text of the first `# ` heading, if any.
fn first_heading(body: &str) -> Option<String> {
    body.lines()
        .find_map(|l| l.strip_prefix("# ").map(|h| h.trim().to_owned()))
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_attr(s: &str) -> String {
    escape_html(s).replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::{
        IndexEntry, NavEntry, PublishedPages, markdown_to_html, render_adr, render_adr_index,
        render_doc, render_markdown, render_nav, render_site_page,
    };

    /// The site index most tests do not exercise: with it empty, a `.md` link
    /// falls back to its own stem, which is what every assertion below predates.
    fn no_pages() -> PublishedPages {
        PublishedPages::new()
    }

    fn nav() -> Vec<NavEntry> {
        vec![
            NavEntry {
                href: "./".into(),
                label: "Home".into(),
            },
            NavEntry {
                href: "modes.html".into(),
                label: "Modes & Co".into(),
            },
        ]
    }

    #[test]
    fn markdown_renders_headings_and_tables() {
        let html = markdown_to_html("# Title\n\n| a | b |\n|---|---|\n| 1 | 2 |\n");
        assert!(html.contains("<h1 id=\"title\">Title</h1>"), "{html}");
        assert!(html.contains("<table>"));
        assert!(html.contains("<td>1</td>"));
    }

    #[test]
    fn adr_wiki_links_become_sibling_page_links() {
        // An ADR-to-ADR wiki link resolves to the sibling .html; a code/file
        // reference becomes inline code; both stop leaking literal `[[ ]]`.
        let md = "See [[docs/adr/0001-build-roteiro.md]] and \
                  [[crates/rto-graph/src/store.rs#Store]] here.\n";
        let html = markdown_to_html(md);
        assert!(
            html.contains("<a href=\"0001-build-roteiro.html\">ADR-0001</a>"),
            "ADR wiki-link → sibling page: {html}"
        );
        assert!(
            html.contains("<code>crates/rto-graph/src/store.rs#Store</code>"),
            "code reference → inline code: {html}"
        );
        assert!(
            !html.contains("[["),
            "no literal wiki brackets leak: {html}"
        );
    }

    #[test]
    fn wiki_links_inside_code_are_left_literal() {
        // A documented example of the syntax, in backticks or a fence, must not
        // be rewritten.
        let inline = markdown_to_html("use `[[docs/adr/0001-x.md]]` in prose\n");
        assert!(
            inline.contains("<code>[[docs/adr/0001-x.md]]</code>"),
            "{inline}"
        );
        let fenced = markdown_to_html("```\n[[docs/adr/0001-x.md]]\n```\n");
        assert!(
            fenced.contains("[[docs/adr/0001-x.md]]"),
            "fence literal: {fenced}"
        );
    }

    #[test]
    fn multi_backtick_code_spans_are_honoured() {
        // A tight double-backtick span (`` ``…`` ``) and the Build Plan's
        // nested-backtick example must both survive verbatim — the previous
        // single-backtick split rewrote the wiki-link inside them.
        let tight = markdown_to_html("say ``[[docs/adr/0001-x.md]]`` please\n");
        assert!(
            tight.contains("<code>[[docs/adr/0001-x.md]]</code>"),
            "{tight}"
        );
        assert!(!tight.contains("<a "), "no link inside code span: {tight}");

        let nested = markdown_to_html("its `` `[[path#Symbol]]` `` example\n");
        assert!(
            nested.contains("<code>`[[path#Symbol]]`</code>"),
            "{nested}"
        );
        assert!(
            !nested.contains("<a "),
            "no link inside nested span: {nested}"
        );

        // An unterminated run is literal and does not shield a later real link.
        let stray = markdown_to_html("a ` stray tick then [[docs/adr/0001-x.md]]\n");
        assert!(
            stray.contains("<a href=\"0001-x.html\">ADR-0001</a>"),
            "unterminated backtick must not shield: {stray}"
        );
    }

    #[test]
    fn markdown_md_links_are_rewritten_to_html() {
        // Ordinary `[text](path.md)` links must point at the rendered `.html`,
        // preserving fragments; external and anchor links are left alone.
        let html = markdown_to_html(
            "See [ADR-1](adr/0001-x.md) and [§2](adr/0001-x.md#context) and \
             [home](https://x.dev) and [top](#intro).\n",
        );
        assert!(html.contains("href=\"adr/0001-x.html\""), "{html}");
        assert!(html.contains("href=\"adr/0001-x.html#context\""), "{html}");
        assert!(
            html.contains("href=\"https://x.dev\""),
            "external unchanged: {html}"
        );
        assert!(html.contains("href=\"#intro\""), "anchor unchanged: {html}");
        assert!(!html.contains(".md\""), "no raw .md hrefs remain: {html}");
    }

    #[test]
    fn render_doc_links_adrs_into_subdir() {
        // A root-level lifetime doc (Build Plan) resolves ADR links into `adr/`.
        let r = render_doc(
            "# Build Plan\n\nGoverned by [[docs/adr/0001-x.md]].\n",
            "Build Plan",
            &no_pages(),
        );
        assert_eq!(r.title, "Build Plan");
        assert!(
            r.html.contains("<a href=\"adr/0001-x.html\">ADR-0001</a>"),
            "root doc → adr/ prefix: {}",
            r.html
        );
        // Root-level chrome: assets/back-link relative to site root.
        assert!(r.html.contains("href=\"./style.css\""));
        // Full favicon set — root-relative from the site root.
        assert!(r.html.contains("href=\"./favicon.svg\""));
        assert!(r.html.contains("href=\"./favicon.ico\""));
        assert!(
            r.html
                .contains("rel=\"apple-touch-icon\" href=\"./apple-touch-icon.png\"")
        );
    }

    const ADR: &str = "---\nadr-id: \"0001\"\nstatus: Accepted\n---\n\n# ADR-0001: Example\n\n## Context\n\nSome `code` and a [link](https://x).\n";

    #[test]
    fn render_adr_strips_frontmatter_and_themes() {
        let r = render_adr(ADR, "fallback", &no_pages());
        assert_eq!(r.title, "ADR-0001: Example");
        // Frontmatter is gone; heading + section rendered.
        assert!(!r.html.contains("adr-id"));
        assert!(
            r.html
                .contains("<h1 id=\"adr-0001-example\">ADR-0001: Example</h1>")
        );
        // The section anchor matches the section's node key (`adr:0001#context`),
        // so a link through the graph lands on the heading in the browser.
        assert!(r.html.contains("<h2 id=\"context\">Context</h2>"));
        assert!(r.html.contains("<code>code</code>"));
        // Themed chrome present.
        assert!(
            r.html
                .contains("<link rel=\"stylesheet\" href=\"../style.css\">")
        );
        // Full favicon set (SVG + `.ico` fallback for browsers without SVG-favicon
        // support, e.g. Safari) — root-relative from a sub-page.
        assert!(r.html.contains("href=\"../favicon.svg\""));
        assert!(r.html.contains("href=\"../favicon.ico\""));
        assert!(
            r.html
                .contains("rel=\"apple-touch-icon\" href=\"../apple-touch-icon.png\"")
        );
        assert!(r.html.contains("← Roteiro home"));
        assert!(r.html.contains("← Back to roteiro.dev"));
        assert!(r.html.starts_with("<!doctype html>"));
    }

    #[test]
    fn render_adr_falls_back_without_h1() {
        let r = render_adr("no frontmatter, no heading\n", "slug-name", &no_pages());
        assert_eq!(r.title, "slug-name");
    }

    #[test]
    fn index_lists_entries_and_escapes() {
        let entries = [
            IndexEntry {
                href: "0001-x.html".into(),
                title: "First & <best>".into(),
            },
            IndexEntry {
                href: "0002-y.html".into(),
                title: "Second".into(),
            },
        ];
        let lifetime = [IndexEntry {
            href: "../build-plan.html".into(),
            title: "Build Plan".into(),
        }];
        let html = render_adr_index(&lifetime, &entries);
        assert!(html.contains("<a href=\"../build-plan.html\">Build Plan</a>"));
        assert!(html.contains("<a href=\"0001-x.html\">First &amp; &lt;best&gt;</a>"));
        assert!(html.contains("<a href=\"0002-y.html\">Second</a>"));
        // First entry precedes second (order preserved).
        assert!(html.find("0001-x").unwrap() < html.find("0002-y").unwrap());
        // Lifetime docs listed before the ADRs.
        assert!(html.find("build-plan").unwrap() < html.find("0001-x").unwrap());
    }

    #[test]
    fn an_explicit_anchor_survives_the_split_that_moved_its_section() {
        // The hazard this mechanism exists for. The old single-page site
        // published `#modes`, `#crossrepo`, `#remote-tier` — short, hand-chosen
        // ids that no heading text slugifies to. External links point at them and
        // cannot be updated, so a page that inherits a section must be able to
        // inherit its anchor verbatim.
        let html = markdown_to_html(
            "## The five ways to run it {#modes}\n\n## Cross-repo: a hub and its spokes {#crossrepo}\n",
        );
        assert!(
            html.contains("<h2 id=\"modes\">The five ways to run it</h2>"),
            "{html}"
        );
        assert!(
            html.contains("<h2 id=\"crossrepo\">Cross-repo: a hub and its spokes</h2>"),
            "{html}"
        );
        // The attribute is markup, not part of the heading's text.
        assert!(!html.contains("{#"), "no literal attribute leaks: {html}");
    }

    #[test]
    fn generated_anchors_match_the_graph_s_section_keys_and_stay_unique() {
        // `rto_spec` builds `<doc>#<slugify(heading)>` section keys from the same
        // function, so a link that resolves in the graph lands on the heading.
        let html = markdown_to_html("## Install & build\n\n## Install & build\n\n## ###\n");
        assert!(html.contains("id=\"install-build\""), "{html}");
        // A repeat is suffixed rather than duplicated: two elements sharing an
        // `id` makes one of them unreachable.
        assert!(html.contains("id=\"install-build-2\""), "{html}");
        // A heading that slugifies to nothing still gets a usable anchor.
        assert!(html.contains("id=\"section-3\""), "{html}");
    }

    #[test]
    fn inline_code_counts_as_heading_text() {
        // The old page's headings look like `What <code>init</code> sets up`.
        // Dropping the code span would slugify only the prose around it and give
        // the section an anchor nobody would guess.
        let html = markdown_to_html("### What `init` sets up\n");
        assert!(
            html.contains("<h3 id=\"what-init-sets-up\">"),
            "code span is part of the heading's text: {html}"
        );
    }

    #[test]
    fn a_hash_inside_a_fence_is_not_a_heading() {
        // The id list is computed from a real parse, so fenced content cannot
        // shift every subsequent heading's anchor by one.
        let html = markdown_to_html("```\n## Not a heading\n```\n\n## Real\n");
        assert!(html.contains("<h2 id=\"real\">Real</h2>"), "{html}");
    }

    #[test]
    fn a_site_page_carries_the_bar_with_itself_marked() {
        let r = render_site_page(
            "---\nsite-page: modes\n---\n\n# The five ways to run it\n\nSee [[docs/adr/0019-remote.md]].\n",
            "fallback",
            &nav(),
            "modes.html",
            &no_pages(),
        );
        assert_eq!(r.title, "The five ways to run it");
        // Frontmatter is chrome for the graph, not content for the reader.
        assert!(!r.html.contains("site-page"), "{}", r.html);
        // The current page is unlinked and marked; its neighbour is a link.
        assert!(
            r.html
                .contains("<span aria-current=\"page\">Modes &amp; Co</span>"),
            "{}",
            r.html
        );
        assert!(r.html.contains("<a href=\"./\">Home</a>"), "{}", r.html);
        // A root-level page: assets and ADR links resolve from the site root.
        assert!(r.html.contains("href=\"./style.css\""), "{}", r.html);
        assert!(
            r.html
                .contains("<a href=\"adr/0019-remote.html\">ADR-0019</a>"),
            "{}",
            r.html
        );
    }

    #[test]
    fn the_bar_is_plain_anchors_and_escapes_its_labels() {
        let bar = render_nav(&nav(), "nothing.html");
        assert!(bar.starts_with("<nav class=\"sitenav\">"), "{bar}");
        // Nothing marked when the current page is not in the bar — a preview of
        // an unlisted page, not an error.
        assert!(!bar.contains("aria-current"), "{bar}");
        assert!(bar.contains("Modes &amp; Co"), "escaped label: {bar}");
        // No script: the site has no build step and this must not introduce one.
        assert!(!bar.contains("<script"), "{bar}");
    }

    #[test]
    fn a_link_resolves_to_the_page_the_site_actually_serves() {
        // Issue #446: four ADRs link `../BUILD_PLAN_V2.md`, which is correct in
        // the repository. Published under a `site-page:` slug, that document is
        // served as `build-plan-v2.html` — so rewriting the link to its own stem
        // aims it at a page that is never emitted.
        let mut pages = PublishedPages::new();
        pages.publish("BUILD_PLAN_V2.md", "build-plan-v2.html");
        let html = render_markdown("See [V2](../BUILD_PLAN_V2.md).\n", "", &pages);
        assert!(
            html.contains("href=\"../build-plan-v2.html\""),
            "served name, and the link's own hop kept: {html}"
        );
        // A fragment survives the substitution.
        let frag = render_markdown("[s](../BUILD_PLAN_V2.md#stage-21)\n", "", &pages);
        assert!(
            frag.contains("href=\"../build-plan-v2.html#stage-21\""),
            "{frag}"
        );
        // An unpublished document still falls back to its stem, unchanged.
        let other = render_markdown("[x](../REVIEW_CHECKLIST.md)\n", "", &pages);
        assert!(
            other.contains("href=\"../REVIEW_CHECKLIST.html\""),
            "{other}"
        );
    }

    #[test]
    fn a_file_name_two_documents_claim_is_left_alone() {
        // Guessing which one a link meant would silently point it at the wrong
        // page — worse than the 404 the lookup exists to remove.
        let mut pages = PublishedPages::new();
        pages.publish("GUIDE.md", "guide.html");
        pages.publish("GUIDE.md", "other-guide.html");
        let html = render_markdown("[g](GUIDE.md)\n", "", &pages);
        assert!(html.contains("href=\"GUIDE.html\""), "unrewritten: {html}");
        // Re-publishing the *same* target is not a conflict.
        let mut same = PublishedPages::new();
        same.publish("GUIDE.md", "guide.html");
        same.publish("GUIDE.md", "guide.html");
        let html = render_markdown("[g](GUIDE.md)\n", "", &same);
        assert!(html.contains("href=\"guide.html\""), "{html}");
    }

    #[test]
    fn site_pages_render_deterministically() {
        let md = "---\nsite-page: a\n---\n\n# A\n\n## S\n";
        assert_eq!(
            render_site_page(md, "f", &nav(), "a.html", &no_pages()),
            render_site_page(md, "f", &nav(), "a.html", &no_pages())
        );
    }

    #[test]
    fn rendering_is_deterministic() {
        assert_eq!(
            render_adr(ADR, "f", &no_pages()),
            render_adr(ADR, "f", &no_pages())
        );
    }
}

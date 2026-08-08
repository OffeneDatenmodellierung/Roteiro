//! The documentation-site renderer: ADR markdown → themed HTML pages, produced
//! deterministically so CI diffs are meaningful. Replaces the shell
//! `md2html.awk` stopgap with a real `CommonMark` parser (`pulldown-cmark`),
//! fixing the whole class of hand-rolled-parser bugs (backtick runs, tables,
//! heading edge cases) we hit before.
//!
//! Page chrome (theme, nav, back-link, footer) matches the previous site so the
//! switch is drop-in. This module is pure string generation; the `roteiro`
//! binary owns walking `docs/adr` and copying static assets.

use std::fmt::Write as _;

use pulldown_cmark::{Options, Parser, html};

/// A rendered ADR: its title (for the index) and the full themed HTML page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedAdr {
    /// The ADR title (first `# ` heading, or the fallback passed to
    /// [`render_adr`]).
    pub title: String,
    /// The complete HTML document.
    pub html: String,
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
#[must_use]
pub fn markdown_to_html(md: &str) -> String {
    render_markdown(md, "")
}

/// Render `md` to HTML: resolve `[[wiki-links]]` (ADR links use `adr_prefix` as
/// their href prefix), then run `CommonMark` with GitHub tables/strikethrough.
fn render_markdown(md: &str, adr_prefix: &str) -> String {
    let pre = rewrite_wiki_links(md, adr_prefix);
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    let parser = Parser::new_ext(&pre, opts);
    let mut out = String::new();
    html::push_html(&mut out, parser);
    out
}

/// Render one ADR markdown document to a themed HTML page. Leading YAML
/// frontmatter is stripped; the title is the first `# ` heading, or `fallback`
/// if there is none. ADR `[[…]]` links resolve to sibling ADR pages.
#[must_use]
pub fn render_adr(markdown: &str, fallback_title: &str) -> RenderedAdr {
    let body = strip_frontmatter(markdown);
    let title = first_heading(body).unwrap_or_else(|| fallback_title.to_owned());
    let content = render_markdown(body, "");
    let nav = "<p class=\"nav\"><a href=\"../\">← Roteiro home</a> · \
               <a href=\"./\">All ADRs</a> · <a href=\"../build-plan.html\">Build Plan</a></p>";
    let html = page(&format!("{title} — Roteiro"), "../", nav, &content);
    RenderedAdr { title, html }
}

/// Render a root-level "lifetime doc" (e.g. the Build Plan) to a themed page.
/// Its `[[docs/adr/…]]` links resolve into the `adr/` subdirectory.
#[must_use]
pub fn render_doc(markdown: &str, fallback_title: &str) -> RenderedAdr {
    let body = strip_frontmatter(markdown);
    let title = first_heading(body).unwrap_or_else(|| fallback_title.to_owned());
    let content = render_markdown(body, "adr/");
    let nav = "<p class=\"nav\"><a href=\"./\">← Roteiro home</a> · \
               <a href=\"adr/\">ADRs</a></p>";
    let html = page(&format!("{title} — Roteiro"), "./", nav, &content);
    RenderedAdr { title, html }
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
    use super::{IndexEntry, markdown_to_html, render_adr, render_adr_index, render_doc};

    #[test]
    fn markdown_renders_headings_and_tables() {
        let html = markdown_to_html("# Title\n\n| a | b |\n|---|---|\n| 1 | 2 |\n");
        assert!(html.contains("<h1>Title</h1>"));
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
    fn render_doc_links_adrs_into_subdir() {
        // A root-level lifetime doc (Build Plan) resolves ADR links into `adr/`.
        let r = render_doc(
            "# Build Plan\n\nGoverned by [[docs/adr/0001-x.md]].\n",
            "Build Plan",
        );
        assert_eq!(r.title, "Build Plan");
        assert!(
            r.html.contains("<a href=\"adr/0001-x.html\">ADR-0001</a>"),
            "root doc → adr/ prefix: {}",
            r.html
        );
        // Root-level chrome: assets/back-link relative to site root.
        assert!(r.html.contains("href=\"./style.css\""));
    }

    const ADR: &str = "---\nadr-id: \"0001\"\nstatus: Accepted\n---\n\n# ADR-0001: Example\n\n## Context\n\nSome `code` and a [link](https://x).\n";

    #[test]
    fn render_adr_strips_frontmatter_and_themes() {
        let r = render_adr(ADR, "fallback");
        assert_eq!(r.title, "ADR-0001: Example");
        // Frontmatter is gone; heading + section rendered.
        assert!(!r.html.contains("adr-id"));
        assert!(r.html.contains("<h1>ADR-0001: Example</h1>"));
        assert!(r.html.contains("<h2>Context</h2>"));
        assert!(r.html.contains("<code>code</code>"));
        // Themed chrome present.
        assert!(
            r.html
                .contains("<link rel=\"stylesheet\" href=\"../style.css\">")
        );
        assert!(r.html.contains("← Roteiro home"));
        assert!(r.html.contains("← Back to roteiro.dev"));
        assert!(r.html.starts_with("<!doctype html>"));
    }

    #[test]
    fn render_adr_falls_back_without_h1() {
        let r = render_adr("no frontmatter, no heading\n", "slug-name");
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
    fn rendering_is_deterministic() {
        assert_eq!(render_adr(ADR, "f"), render_adr(ADR, "f"));
    }
}

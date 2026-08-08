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

/// An entry in the ADR index page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    /// Relative href (e.g. `0001-….html`).
    pub href: String,
    /// Display title.
    pub title: String,
}

/// Convert `CommonMark` `md` to an HTML fragment, with GitHub-style tables and
/// strikethrough enabled (the house ADR style uses pipe tables).
#[must_use]
pub fn markdown_to_html(md: &str) -> String {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    let parser = Parser::new_ext(md, opts);
    let mut out = String::new();
    html::push_html(&mut out, parser);
    out
}

/// Render one ADR markdown document to a themed HTML page. Leading YAML
/// frontmatter is stripped; the title is the first `# ` heading, or `fallback`
/// if there is none.
#[must_use]
pub fn render_adr(markdown: &str, fallback_title: &str) -> RenderedAdr {
    let body = strip_frontmatter(markdown);
    let title = first_heading(body).unwrap_or_else(|| fallback_title.to_owned());
    let content = markdown_to_html(body);
    let nav = "<p class=\"nav\"><a href=\"../\">← Roteiro home</a> · \
               <a href=\"./\">All ADRs</a></p>";
    let html = page(&format!("{title} — Roteiro"), "../", nav, &content);
    RenderedAdr { title, html }
}

/// Render the ADR index page listing `entries` in the given order.
#[must_use]
pub fn render_adr_index(entries: &[IndexEntry]) -> String {
    let mut list = String::from("<h1>Architecture Decision Records</h1><ul>");
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
    page("Architecture Decision Records — Roteiro", "../", nav, &list)
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
    use super::{IndexEntry, markdown_to_html, render_adr, render_adr_index};

    #[test]
    fn markdown_renders_headings_and_tables() {
        let html = markdown_to_html("# Title\n\n| a | b |\n|---|---|\n| 1 | 2 |\n");
        assert!(html.contains("<h1>Title</h1>"));
        assert!(html.contains("<table>"));
        assert!(html.contains("<td>1</td>"));
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
        let html = render_adr_index(&entries);
        assert!(html.contains("<a href=\"0001-x.html\">First &amp; &lt;best&gt;</a>"));
        assert!(html.contains("<a href=\"0002-y.html\">Second</a>"));
        // First entry precedes second (order preserved).
        assert!(html.find("0001-x").unwrap() < html.find("0002-y").unwrap());
    }

    #[test]
    fn rendering_is_deterministic() {
        assert_eq!(render_adr(ADR, "f"), render_adr(ADR, "f"));
    }
}

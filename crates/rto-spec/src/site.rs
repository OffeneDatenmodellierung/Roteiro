//! House-style **site page** parsing: the documents published to roteiro.dev.
//!
//! A site page is the third authored document class, alongside ADRs (ADR-0001)
//! and blueprints (ADR-0004). It exists so the public website stops being the
//! one documentation surface outside `roteiro check`.
//!
//! # Publication is declared, never inferred
//!
//! A markdown file is a site page **iff its frontmatter carries a non-empty
//! `site-page:` slug**. Nothing about its location says so.
//!
//! That is deliberate. `docs/` mixes published material with internal material —
//! `docs/REVIEW_CHECKLIST.md` and `docs/BUILD_PLAN_V2.md` are working documents,
//! while `docs/BUILD_PLAN.md` is already rendered to the site — so the line was
//! never a clean directory boundary and a path convention could only ever
//! approximate it. A path rule also has to be *remembered*: it lives in a
//! renderer someone has to go read, and the file itself gives no hint either way.
//! A frontmatter marker puts the decision in the document, where the person
//! writing or reviewing it is already looking, and makes "is this published?" a
//! question the file answers about itself.
//!
//! It also means publishing a document does **not** require moving it.
//! `docs/OFFLINE_SETUP.md` can gain a public page in place, keeping every
//! existing link to its repository path intact.
//!
//! # What being a site page buys
//!
//! Structurally these mirror blueprints: `## ` headings become sections and
//! `[[path#Symbol]]` wiki-links become the *authored* layer over code, validated
//! against the derived graph by [`crate::check`] exactly like ADR links. So a
//! page that claims `security run` needs `--allow-unsandboxed` can be made to
//! cite the code it describes, and the citation drifts loudly when the code moves.
//!
//! Keys are slug-based (`site:<slug>`), since the slug — not the source path — is
//! what the published URL is built from.

use rto_graph::{Edge, EdgeKind, FactSet, Node, NodeKind, Provenance};

use crate::adr::{Section, WikiLink, clean_value, resolve_target, split_frontmatter};
use crate::text::{first_h1, heading_id, heading_text};

/// The frontmatter field that declares a document published, and carries its
/// slug. Its presence is the whole classification rule.
pub const MARKER_FIELD: &str = "site-page";

/// Where a page sorts in the site navigation when it declares no `site-order`.
/// Above any explicitly-ordered page, so an author who forgets the field gets a
/// page at the end of the bar rather than a page silently tied for first.
const DEFAULT_ORDER: u32 = 10_000;

/// Why a document that *declared* itself a site page could not be parsed as one.
///
/// Every variant is drift rather than a warning, for the reason
/// [`crate::layer::AuthoredLayer::malformed`] gives for ADRs: the file asked to
/// be published, so failing quietly would drop a page from the site while the
/// gate stayed green.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// The `site-page` slug is not URL-safe (`[a-z0-9-]+`, not `-`-terminated).
    ///
    /// Checked rather than slugified-on-the-fly: the slug *is* the published
    /// URL, so quietly rewriting `Getting Started` to `getting-started` would
    /// make the author's intended link the broken one.
    #[error("site-page slug `{0}` is not URL-safe (expected lowercase a-z, 0-9 and `-`)")]
    InvalidSlug(String),
    /// `site-order` was present but not a non-negative integer.
    #[error("site-order `{0}` is not a non-negative integer")]
    InvalidOrder(String),
}

/// A fully-parsed site page: what the site publishes, and the authored links
/// the check validates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SitePage {
    /// Repository-relative path of the source markdown.
    pub path: String,
    /// The declared slug; the page is published as `<slug>.html`.
    pub slug: String,
    /// Page title, from `title:` frontmatter, the first `# ` heading, or the slug.
    pub title: String,
    /// Short navigation label, from `site-nav:` frontmatter, defaulting to
    /// [`Self::title`].
    pub nav: String,
    /// Sort position in the navigation bar; ties break on [`Self::slug`].
    pub order: u32,
    /// `## ` sections in document order.
    pub sections: Vec<Section>,
    /// Authored `[[…]]` links in document order.
    pub links: Vec<WikiLink>,
}

impl SitePage {
    /// The natural key of this page's node (`site:<slug>`).
    #[must_use]
    pub fn key(&self) -> String {
        format!("site:{}", self.slug)
    }

    /// The published filename for this page (`<slug>.html`).
    #[must_use]
    pub fn href(&self) -> String {
        format!("{}.html", self.slug)
    }

    /// The authored nodes and structural edges: a `site_page` node, one
    /// `site_section` node per section, and `contains` edges between them.
    /// Wiki-links are *not* included — [`crate::check`] validates them against
    /// the code graph before they become edges.
    #[must_use]
    pub fn facts(&self) -> FactSet {
        let key = self.key();
        let mut node = Node::new(
            key.clone(),
            NodeKind::Other("site_page".into()),
            self.title.clone(),
        )
        .with_provenance(Provenance::Authored);
        node.path = Some(self.path.clone());
        node.meta = serde_json::json!({ "slug": self.slug, "nav": self.nav, "order": self.order });
        let mut fs = FactSet::new().with_node(node);

        for section in &self.sections {
            let skey = format!("{key}#{}", section.slug);
            let mut snode = Node::new(
                skey.clone(),
                NodeKind::Other("site_section".into()),
                section.title.clone(),
            )
            .with_provenance(Provenance::Authored);
            snode.path = Some(self.path.clone());
            fs = fs.with_node(snode).with_edge(Edge::authored(
                key.clone(),
                skey,
                EdgeKind::Contains,
            ));
        }
        fs
    }
}

/// Whether a markdown document declares itself published: its frontmatter
/// carries a non-empty [`MARKER_FIELD`].
///
/// Path-independent by design (see the module docs). Callers apply this only to
/// non-ADR markdown, so an ADR that somehow carried the field is still an ADR —
/// ADRs are recognised first, and are published by their own mechanism.
#[must_use]
pub fn is_site_page(text: &str) -> bool {
    field(text, MARKER_FIELD).is_some_and(|v| !v.is_empty())
}

/// Parse a house-style site page at `rel_path`.
///
/// # Errors
/// Returns [`ParseError::InvalidSlug`] if the declared slug is not URL-safe, or
/// [`ParseError::InvalidOrder`] if `site-order` is not a non-negative integer.
pub fn parse_site_page(rel_path: &str, text: &str) -> Result<SitePage, ParseError> {
    let (_, body) = split_frontmatter(text);
    let slug = field(text, MARKER_FIELD).unwrap_or_default().to_owned();
    if !is_url_safe_slug(&slug) {
        return Err(ParseError::InvalidSlug(slug));
    }
    let order = match field(text, "site-order").filter(|v| !v.is_empty()) {
        Some(raw) => raw
            .parse::<u32>()
            .map_err(|_| ParseError::InvalidOrder(raw.to_owned()))?,
        None => DEFAULT_ORDER,
    };

    let title = field(text, "title")
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
        .or_else(|| first_h1(body))
        .unwrap_or_else(|| slug.clone());
    let nav = field(text, "site-nav")
        .filter(|v| !v.is_empty())
        .map_or_else(|| title.clone(), str::to_owned);

    let key = format!("site:{slug}");
    // Walk the body exactly as the ADR and blueprint parsers do: track the
    // current section so links are attributed to it, and skip fenced code so a
    // documented `[[…]]` example is not mistaken for a real authored link.
    let mut sections = Vec::new();
    let mut links = Vec::new();
    let mut current: Option<String> = None;
    let mut in_fence = false;
    for line in body.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if let Some(heading) = line.strip_prefix("## ") {
            let title = heading_text(heading);
            // The heading's *declared* id, not the slug of its text: an explicit
            // `{#offline}` is an address the author wrote, and the renderer has
            // always honoured it. Slugifying the text regardless made the section
            // key name a place the page does not have (#524).
            let slug = heading_id(heading);
            current = Some(slug.clone());
            // No `text`: `Section` is shared with `parse_adr`, and only that
            // parser populates it so far. A site page section note is empty in
            // the vault for the same reason an ADR's was (#545) — the fix is the
            // same shape and is deliberately not in that PR's scope, which is
            // ADRs. 62 notes here against 199 there.
            sections.push(Section {
                slug,
                title,
                text: String::new(),
            });
        }
        for raw in crate::text::scan_wiki_links(line) {
            let from = match &current {
                Some(slug) => format!("{key}#{slug}"),
                None => key.clone(),
            };
            if let Some(target_key) = resolve_target(&raw) {
                links.push(WikiLink {
                    from,
                    raw,
                    target_key,
                });
            }
        }
    }

    Ok(SitePage {
        path: rel_path.to_owned(),
        slug,
        title,
        nav,
        order,
        sections,
        links,
    })
}

/// The site navigation order: every page, sorted by `site-order` then slug.
///
/// One function rather than a sort each caller writes, for the reason
/// [`crate::layer::authored_layer_from`] gives about the classification rule: the
/// bar the renderer emits and the order the check reports have to be the same
/// order, or a page's position becomes a thing two surfaces disagree about.
/// Ties break on the slug so the bar is deterministic — the site is diffed in CI.
#[must_use]
pub fn site_nav(pages: &[SitePage]) -> Vec<&SitePage> {
    let mut nav: Vec<&SitePage> = pages.iter().collect();
    nav.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.slug.cmp(&b.slug)));
    nav
}

/// Read one frontmatter field's cleaned value, case-insensitively on the key.
/// Returns `None` when there is no frontmatter or no such field.
fn field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let (frontmatter, _) = split_frontmatter(text);
    frontmatter.lines().find_map(|line| {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return None;
        }
        let (k, v) = line.split_once(':')?;
        k.trim().eq_ignore_ascii_case(key).then(|| clean_value(v))
    })
}

/// Whether `slug` is safe to use verbatim as a published filename: a non-empty
/// run of `a-z`, `0-9` and `-`, not starting or ending with `-`.
fn is_url_safe_slug(slug: &str) -> bool {
    !slug.is_empty()
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::{ParseError, is_site_page, parse_site_page, site_nav};

    const PAGE: &str = "---\nsite-page: modes\nsite-nav: Modes\nsite-order: 3\ntitle: The five ways to run it\n---\n\n# The five ways to run it\n\n## Offline mode {#offline}\n\nSee [[crates/roteiro/src/main.rs#run_check]].\n";

    #[test]
    fn publication_is_declared_by_frontmatter_not_by_path() {
        // The marker is the whole rule: an internal working document under the
        // same directory is not a page, and a page is a page wherever it lives.
        assert!(is_site_page(PAGE));
        assert!(!is_site_page("---\nstatus: draft\n---\n\n# Internal\n"));
        assert!(!is_site_page("# No frontmatter at all\n"));
        // Present but empty is not a declaration — it is a half-finished edit.
        assert!(!is_site_page("---\nsite-page:\n---\n\n# X\n"));
    }

    #[test]
    fn parses_slug_nav_order_title_sections_and_links() {
        let p = parse_site_page("docs/site/modes.md", PAGE).expect("parse");
        assert_eq!(p.slug, "modes");
        assert_eq!(p.href(), "modes.html");
        assert_eq!(p.key(), "site:modes");
        assert_eq!(p.nav, "Modes");
        assert_eq!(p.order, 3);
        assert_eq!(p.title, "The five ways to run it");
        // The explicit anchor is markup, not part of the section's *name* — and
        // since #524 it **is** its address, because the renderer has always made
        // it the heading's `id` and a key that named somewhere else was a link
        // that resolved here and scrolled nowhere there.
        assert_eq!(p.sections.len(), 1);
        assert_eq!(p.sections[0].title, "Offline mode");
        assert_eq!(p.sections[0].slug, "offline");
        // The authored link resolves like an ADR's, attributed to its section.
        assert_eq!(p.links.len(), 1);
        assert_eq!(p.links[0].from, "site:modes#offline");
        assert_eq!(
            p.links[0].target_key,
            "sym:rust:crates/roteiro/src/main.rs#run_check"
        );
    }

    #[test]
    fn title_falls_back_to_h1_then_slug_and_nav_to_title() {
        let h1 = parse_site_page("p.md", "---\nsite-page: build\n---\n\n# Install & build\n")
            .expect("parse");
        assert_eq!(h1.title, "Install & build");
        assert_eq!(h1.nav, "Install & build", "nav defaults to the title");
        let bare =
            parse_site_page("p.md", "---\nsite-page: build\n---\n\nno heading\n").expect("parse");
        assert_eq!(bare.title, "build");
    }

    #[test]
    fn an_anchored_h1_does_not_put_its_anchor_in_the_graph() {
        // #469: with no `title:` in frontmatter the H1 is the title, and that
        // value becomes a `site:` node title — so it reaches `roteiro search`
        // and everything else that reads the store. The rendered page never
        // showed it, which is exactly why it survived.
        let p = parse_site_page(
            "website/pages/modes.md",
            "---\nsite-page: modes\n---\n\n# The five ways to run it {#modes}\n",
        )
        .expect("parse");
        assert_eq!(p.title, "The five ways to run it");
        assert_eq!(
            p.nav, "The five ways to run it",
            "nav defaults to the title"
        );
        assert!(
            !p.title.contains("{#"),
            "the anchor reached the node title: {:?}",
            p.title
        );
    }

    #[test]
    fn an_explicit_anchor_keys_the_section_and_markup_still_reduces_to_text() {
        // Was `section_keys_are_unchanged_by_reading_headings_with_the_parser`,
        // and it asserted `1-offline-mode-the-default` for the first heading.
        //
        // That was the right guard for the change it was written against —
        // folding a hand-written stripper into the shared rule must not
        // **silently** re-key a section, because a section key is a graph key and
        // not display text. #524 re-keys it *deliberately*: the heading declares
        // `{#offline}`, the renderer has always emitted that as its `id`, and the
        // graph now agrees. The word the old comment turned on was `silently`.
        //
        // The second heading is why this test still earns its place: a heading
        // with no anchor is unaffected, so markup in `## What `init` sets up`
        // still reduces to the text a reader sees.
        let p = parse_site_page(
            "website/pages/modes.md",
            "---\nsite-page: modes\n---\n\n# T\n\n## 1 · Offline mode — the default {#offline}\n\n## What `init` sets up\n",
        )
        .expect("parse");
        let slugs: Vec<_> = p.sections.iter().map(|s| s.slug.as_str()).collect();
        assert_eq!(slugs, ["offline", "what-init-sets-up"]);
        // The title is now the text a reader sees, markup and all removed.
        assert_eq!(p.sections[1].title, "What init sets up");
    }

    #[test]
    fn unordered_pages_sort_after_ordered_ones() {
        let ordered =
            parse_site_page("a.md", "---\nsite-page: a\nsite-order: 9\n---\n# A\n").expect("parse");
        let unordered = parse_site_page("b.md", "---\nsite-page: b\n---\n# B\n").expect("parse");
        assert!(
            ordered.order < unordered.order,
            "a page that forgets site-order lands at the end, not tied for first"
        );
    }

    #[test]
    fn a_slug_that_is_not_url_safe_is_drift_not_a_silent_rewrite() {
        // The slug *is* the URL. Slugifying it here would publish a page at an
        // address the author never wrote, so this is an error instead.
        for bad in ["Getting Started", "modes/", "-lead", "trail-", "Modes"] {
            let text = format!("---\nsite-page: {bad}\n---\n# X\n");
            assert_eq!(
                parse_site_page("p.md", &text),
                Err(ParseError::InvalidSlug(bad.to_owned())),
                "slug `{bad}` must be rejected"
            );
        }
        assert!(parse_site_page("p.md", "---\nsite-page: cross-repo-2\n---\n# X\n").is_ok());
    }

    #[test]
    fn a_non_numeric_order_is_drift() {
        assert_eq!(
            parse_site_page("p.md", "---\nsite-page: a\nsite-order: first\n---\n# A\n"),
            Err(ParseError::InvalidOrder("first".to_owned()))
        );
    }

    #[test]
    fn quoted_and_commented_frontmatter_reads_like_an_adr_s() {
        let p = parse_site_page(
            "p.md",
            "---\nsite-page: \"cross-repo\"\nsite-order: 7 # after config\n---\n# X\n",
        )
        .expect("parse");
        assert_eq!(p.slug, "cross-repo");
        assert_eq!(p.order, 7);
    }

    #[test]
    fn documented_wiki_link_examples_in_fences_are_not_authored_links() {
        let p = parse_site_page(
            "p.md",
            "---\nsite-page: a\n---\n# A\n\n```\n[[crates/x.rs#Y]]\n```\n",
        )
        .expect("parse");
        assert!(p.links.is_empty(), "{:?}", p.links);
    }

    #[test]
    fn nav_sorts_by_order_then_slug() {
        let page = |slug: &str, order: &str| {
            parse_site_page(
                "p.md",
                &format!("---\nsite-page: {slug}\n{order}---\n# {slug}\n"),
            )
            .expect("parse")
        };
        let pages = [
            page("zulu", "site-order: 1\n"),
            page("beta", ""),
            page("alpha", ""),
            page("mike", "site-order: 1\n"),
        ];
        let order: Vec<&str> = site_nav(&pages).iter().map(|p| p.slug.as_str()).collect();
        // Explicit order wins; ties break on slug; unordered pages come last.
        assert_eq!(order, ["mike", "zulu", "alpha", "beta"]);
    }

    #[test]
    fn facts_carry_the_page_and_its_sections() {
        let p = parse_site_page("docs/site/modes.md", PAGE).expect("parse");
        let fs = p.facts();
        let page = fs
            .nodes
            .iter()
            .find(|n| n.key == "site:modes")
            .expect("page node");
        assert_eq!(page.kind.as_str(), "site_page");
        assert_eq!(page.path.as_deref(), Some("docs/site/modes.md"));
        assert_eq!(page.meta["slug"], "modes");
        assert!(
            fs.nodes.iter().any(|n| n.key == "site:modes#offline"),
            "section node, keyed by the anchor the heading declares (#524)"
        );
        assert!(
            fs.edges
                .iter()
                .any(|e| e.src == "site:modes" && e.dst == "site:modes#offline"),
            "contains edge"
        );
    }
}

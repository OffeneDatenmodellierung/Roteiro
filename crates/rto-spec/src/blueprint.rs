//! House-style **blueprint** (technical implementation plan) parsing.
//!
//! Blueprints are the ADR's sibling in the authoring pillar (ADR-0004): a
//! graph-grounded build plan rather than a decision record. Unlike ADRs they
//! carry **no YAML frontmatter** — a blueprint is identified by its house-style
//! H1 (`… — Technical Implementation Plan`) or by living under `docs/blueprint`.
//! Structurally they mirror ADRs: `## ` headings become sections and
//! `[[path#Symbol]]` wiki-links become the *authored* layer over code, validated
//! against the derived graph by [`crate::check`] exactly like ADR links.
//!
//! Keys are path-based (`blueprint:<path>`), since a blueprint has no numeric id.

use rto_graph::{Edge, EdgeKind, FactSet, Node, NodeKind};

use crate::adr::{Section, WikiLink, first_h1, resolve_target};

/// The house-style marker in a blueprint's H1 (from `roteiro spec … blueprint`),
/// including the leading em dash so a doc whose H1 merely mentions the phrase in
/// prose is not misclassified — detection matches the scaffold's `… —
/// Technical Implementation Plan` exactly.
const MARKER: &str = "— Technical Implementation Plan";

/// A fully-parsed blueprint: title, section structure, and authored links.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlueprintDoc {
    /// Repository-relative path of the blueprint file.
    pub path: String,
    /// Title, from the first `# ` heading (or the file stem).
    pub title: String,
    /// `## ` sections in document order.
    pub sections: Vec<Section>,
    /// Authored `[[…]]` links in document order.
    pub links: Vec<WikiLink>,
}

impl BlueprintDoc {
    /// The natural key of this blueprint's node (`blueprint:<path>`).
    #[must_use]
    pub fn key(&self) -> String {
        format!("blueprint:{}", self.path)
    }

    /// The authored nodes and structural edges: a `blueprint` node, one
    /// `blueprint_section` node per section, and `contains` edges between them.
    /// Wiki-links are *not* included — [`crate::check`] validates them against the
    /// code graph before they become edges.
    #[must_use]
    pub fn facts(&self) -> FactSet {
        let key = self.key();
        let mut node = Node::new(
            key.clone(),
            NodeKind::Other("blueprint".into()),
            self.title.clone(),
        );
        node.path = Some(self.path.clone());
        let mut fs = FactSet::new().with_node(node);

        for section in &self.sections {
            let skey = format!("{key}#{}", section.slug);
            let mut snode = Node::new(
                skey.clone(),
                NodeKind::Other("blueprint_section".into()),
                section.title.clone(),
            );
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

/// Whether a markdown file is a house-style blueprint: it lives under
/// `docs/blueprint`/`docs/blueprints`, or its first H1 carries the
/// `— Technical Implementation Plan` marker. Callers apply this only to non-ADR
/// markdown (ADRs are recognised first).
#[must_use]
pub fn is_blueprint(rel_path: &str, text: &str) -> bool {
    let lower = rel_path.to_ascii_lowercase();
    lower.starts_with("docs/blueprint") || first_h1(text).is_some_and(|h| h.contains(MARKER))
}

/// Parse a house-style blueprint markdown document at `rel_path`. Infallible:
/// with no frontmatter there is nothing that can fail to parse (an empty or
/// heading-less file yields a titled node with no sections/links).
#[must_use]
pub fn parse_blueprint(rel_path: &str, text: &str) -> BlueprintDoc {
    let key = format!("blueprint:{rel_path}");
    let title = first_h1(text).unwrap_or_else(|| stem_of(rel_path));

    // Walk the body, tracking the current section so links are attributed to it.
    // Fenced code blocks are skipped so documented `[[…]]` examples are not
    // mistaken for real authored links (as in ADR parsing).
    let mut sections = Vec::new();
    let mut links = Vec::new();
    let mut current: Option<String> = None;
    let mut in_fence = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if let Some(heading) = line.strip_prefix("## ") {
            let title = heading.trim().to_owned();
            let slug = crate::text::slugify(&title);
            current = Some(slug.clone());
            sections.push(Section { slug, title });
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

    BlueprintDoc {
        path: rel_path.to_owned(),
        title,
        sections,
        links,
    }
}

/// The file stem (basename without extension) of a path.
fn stem_of(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.')
        .map_or(name, |(stem, _)| stem)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::{is_blueprint, parse_blueprint};
    use rto_graph::{EdgeKind, NodeKind};

    const BP: &str = "# Token flow — Technical Implementation Plan\n\n\
                      Grounded in: [[docs/adr/0004-x.md]].\n\n\
                      > **Status.** Design → build.\n\n\
                      ## 1. Crate placement\n\n\
                      Touches [[crates/rto-graph/src/store.rs#Store]].\n\n\
                      ## 2. Design\n\n\
                      ```\n[[not/a/real#Link]]\n```\n\nDone.\n";

    #[test]
    fn detects_blueprints_by_marker_or_path() {
        assert!(is_blueprint("docs/plans/token.md", BP), "H1 marker");
        assert!(
            is_blueprint("docs/blueprint/anything.md", "# Plain\n"),
            "path prefix"
        );
        assert!(
            !is_blueprint("docs/notes/x.md", "# Just a note\n"),
            "neither marker nor path"
        );
        // The phrase alone (no leading em dash) does not qualify — the marker is
        // anchored to the scaffold's `… — Technical Implementation Plan` H1.
        assert!(
            !is_blueprint(
                "docs/notes/y.md",
                "# Our Technical Implementation Plan overview\n"
            ),
            "phrase without the em-dash marker is not a blueprint"
        );
    }

    #[test]
    fn parses_title_sections_and_links() {
        let bp = parse_blueprint("docs/plans/token.md", BP);
        assert_eq!(bp.key(), "blueprint:docs/plans/token.md");
        assert_eq!(bp.title, "Token flow — Technical Implementation Plan");

        // Numbered house-style headings keep the number in the slug.
        let slugs: Vec<_> = bp.sections.iter().map(|s| s.slug.as_str()).collect();
        assert_eq!(slugs, ["1-crate-placement", "2-design"]);

        // Two resolvable links; the fenced `[[not/a/real#Link]]` is ignored.
        assert_eq!(bp.links.len(), 2);
        assert_eq!(bp.links[0].from, "blueprint:docs/plans/token.md");
        assert_eq!(bp.links[0].target_key, "file:docs/adr/0004-x.md");
        assert_eq!(
            bp.links[1].from,
            "blueprint:docs/plans/token.md#1-crate-placement"
        );
        assert_eq!(
            bp.links[1].target_key,
            "sym:rust:crates/rto-graph/src/store.rs#Store"
        );
    }

    #[test]
    fn facts_carry_blueprint_and_section_nodes() {
        let bp = parse_blueprint("docs/plans/token.md", BP);
        let fs = bp.facts();
        let bp_node = fs
            .nodes
            .iter()
            .find(|n| n.key == "blueprint:docs/plans/token.md")
            .expect("blueprint node");
        assert_eq!(bp_node.kind, NodeKind::Other("blueprint".into()));
        assert!(
            fs.nodes
                .iter()
                .any(|n| n.kind == NodeKind::Other("blueprint_section".into())
                    && n.key.ends_with("#2-design"))
        );
        // The blueprint contains its two sections.
        assert_eq!(
            fs.edges
                .iter()
                .filter(|e| e.kind == EdgeKind::Contains
                    && e.src == "blueprint:docs/plans/token.md")
                .count(),
            2
        );
    }
}

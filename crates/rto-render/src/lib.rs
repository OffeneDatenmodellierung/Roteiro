//! Renderers over the Roteiro graph. All outputs — docs site, OKF bundle,
//! and the optional MCP server (feature `mcp`) — are build products of the
//! same store, so humans and agents always see the same data.

mod docs;
/// The OKF bundle renderer (#663) — the open-format successor to the vault.
pub mod okf;
/// The one place a tool's class is written, and the report that stands in for a
/// class an operator did not load (#664).
pub mod tool_class;
/// The one place each shared tool description is written (#590).
pub mod tool_text;

#[cfg(feature = "mcp")]
pub mod mcp;

pub use docs::{
    IndexEntry, NavEntry, PublishedPages, RenderedAdr, SourceBase, markdown_to_html, render_adr,
    render_adr_index, render_doc, render_doc_at, render_nav, render_site_page, replace_site_nav,
};

/// A render target for the graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Static documentation website (ADRs, blueprints, AI context pages).
    DocsSite,
    /// An Open Knowledge Format bundle (OKF v0.2).
    ///
    /// Replaced `ObsidianVault` in 4.0.0. The vault was one-way — Roteiro wrote
    /// it, nothing read it back, and only Obsidian could consume it. An OKF
    /// bundle is markdown with YAML frontmatter in nested directories, so
    /// Obsidian still opens it as a vault; what changed is that the output now
    /// targets a specification with other consumers rather than one
    /// application's conventions.
    OkfBundle,
}

impl Target {
    /// Stable CLI name for this target.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DocsSite => "docs",
            Self::OkfBundle => "okf",
        }
    }

    /// Parse a target from its CLI name.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "docs" => Some(Self::DocsSite),
            "okf" => Some(Self::OkfBundle),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Target;

    #[test]
    fn target_names_are_stable() {
        assert_eq!(Target::DocsSite.as_str(), "docs");
        assert_eq!(Target::OkfBundle.as_str(), "okf");
        assert_eq!(Target::parse("docs"), Some(Target::DocsSite));
        assert_eq!(Target::parse("okf"), Some(Target::OkfBundle));
        // The removed target must not silently resolve to something else: a
        // script still passing `obsidian` should be told, not quietly redirected.
        assert_eq!(Target::parse("obsidian"), None);
        assert_eq!(Target::parse("nope"), None);
    }
}

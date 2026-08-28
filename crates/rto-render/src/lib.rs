//! Renderers over the Roteiro graph. All outputs — docs site, Obsidian vault,
//! and the optional MCP server (feature `mcp`) — are build products of the
//! same store, so humans and agents always see the same data.

mod docs;
mod obsidian;
/// The one place a tool's class is written, and the report that stands in for a
/// class an operator did not load (#664).
pub mod tool_class;
/// The one place each shared tool description is written (#590).
pub mod tool_text;

#[cfg(feature = "mcp")]
pub mod mcp;

pub use docs::{
    IndexEntry, NavEntry, PublishedPages, RenderedAdr, SourceBase, markdown_to_html, render_adr,
    render_adr_index, render_doc, render_nav, render_site_page, replace_site_nav,
};
pub use obsidian::{
    AdrEntry, ConfigSecretSummary, CouplingEntry, Coverage, CrossLink, DensityEntry, FindingEntry,
    HOME_NOTE, MemberPin, RenderedUnder, VaultNote, VaultScope, VaultSummary, WorkspaceSummary,
    note_name, render_home, render_note, render_note_scoped, render_workspace_home,
    scoped_note_name,
};

/// A render target for the graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Static documentation website (ADRs, blueprints, AI context pages).
    DocsSite,
    /// Obsidian-compatible markdown vault.
    ObsidianVault,
}

impl Target {
    /// Stable CLI name for this target.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DocsSite => "docs",
            Self::ObsidianVault => "obsidian",
        }
    }

    /// Parse a target from its CLI name.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "docs" => Some(Self::DocsSite),
            "obsidian" => Some(Self::ObsidianVault),
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
        assert_eq!(Target::ObsidianVault.as_str(), "obsidian");
        assert_eq!(Target::parse("docs"), Some(Target::DocsSite));
        assert_eq!(Target::parse("obsidian"), Some(Target::ObsidianVault));
        assert_eq!(Target::parse("nope"), None);
    }
}

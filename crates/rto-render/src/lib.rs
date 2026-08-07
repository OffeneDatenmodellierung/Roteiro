//! Renderers over the Roteiro graph. All outputs — docs site, Obsidian vault,
//! and the optional MCP server (feature `mcp`) — are build products of the
//! same store, so humans and agents always see the same data.

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
}

#[cfg(test)]
mod tests {
    use super::Target;

    #[test]
    fn target_names_are_stable() {
        assert_eq!(Target::DocsSite.as_str(), "docs");
        assert_eq!(Target::ObsidianVault.as_str(), "obsidian");
    }
}

//! Roteiro umbrella CLI. Subcommands are stubs in v0.0.1; each returns a
//! clear "not yet implemented" message so agents and hooks fail loudly, not
//! silently. See ADR-0001 for the roadmap.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "roteiro", version, about = "Provenance-tagged codebase knowledge graph")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scaffold Roteiro in the current repository (store, hooks, agent skill).
    Init,
    /// Incrementally update the graph for the current tree (content-addressed).
    Sync,
    /// Verify authored links against code and ADR states; non-zero on drift.
    Check,
    /// One-shot import from lat.md, Graphify, or codegraph.
    Import {
        /// Source tool: lat | graphify | codegraph
        #[arg(long)]
        from: String,
    },
    /// Render the graph: docs site or Obsidian vault.
    Render {
        /// Target: docs | obsidian
        target: String,
    },
    /// Spec authoring (intent interview, house-style ADR scaffolding).
    Spec,
    /// Start the MCP server (built with `--features mcp`).
    #[cfg(feature = "mcp")]
    Serve,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let name = match cli.command {
        Command::Init => "init",
        Command::Sync => "sync",
        Command::Check => "check",
        Command::Import { .. } => "import",
        Command::Render { .. } => "render",
        Command::Spec => "spec",
        #[cfg(feature = "mcp")]
        Command::Serve => "serve",
    };
    anyhow::bail!("`roteiro {name}` is not implemented yet (v0.0.1 scaffold; see ADR-0001)")
}

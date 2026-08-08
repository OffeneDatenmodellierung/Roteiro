//! Roteiro umbrella CLI. Subcommands are stubs in v0.0.1; each returns a
//! clear "not yet implemented" message so agents and hooks fail loudly, not
//! silently. See ADR-0001 for the roadmap.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "roteiro",
    version,
    about = "Provenance-tagged codebase knowledge graph",
    long_about = "Roteiro — the pilot book for your codebase.\n\n\
        One SQLite store holding structure, intent, and context as a single \
        provenance-tagged knowledge graph, queryable by humans and AI agents \
        alike. Subcommands are scaffolds while the graph core lands; see \
        ADR-0001 and docs/BUILD_PLAN.md for the roadmap.",
    arg_required_else_help = true,
    propagate_version = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scaffold Roteiro in the current repository (store, hooks, agent skill).
    Init,
    /// Incrementally update the graph for the current tree (content-addressed).
    ///
    /// By default this includes uncommitted edits to tracked files (a
    /// pre-commit preview); pass `--committed` to sync only the `HEAD` tree.
    Sync {
        /// Emit the sync report as JSON.
        #[arg(long)]
        json: bool,
        /// Sync only the committed `HEAD` tree, ignoring uncommitted edits.
        #[arg(long)]
        committed: bool,
    },
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
        Command::Sync { json, committed } => return run_sync(json, committed),
        Command::Init => "init",
        Command::Check => "check",
        Command::Import { .. } => "import",
        Command::Render { .. } => "render",
        Command::Spec => "spec",
        #[cfg(feature = "mcp")]
        Command::Serve => "serve",
    };
    anyhow::bail!("`roteiro {name}` is not implemented yet (scaffold; see docs/BUILD_PLAN.md)")
}

/// Sync the graph for the current repository, optionally including uncommitted
/// edits to tracked files.
fn run_sync(json: bool, committed_only: bool) -> anyhow::Result<()> {
    use rto_graph::{ObjectCache, Registry, Repo, Store, sync, sync_worktree};

    let cwd = std::env::current_dir()?;
    let repo = Repo::discover(&cwd)?;

    // Graph DB is per-worktree (under the worktree git dir); the extraction
    // cache is shared across worktrees (under the common git dir).
    let store_dir = repo.git_dir().join("roteiro");
    std::fs::create_dir_all(&store_dir)?;
    let mut store = Store::open(&store_dir.join("graph.db"))?;
    let cache = ObjectCache::open(repo.common_dir().join("roteiro").join("objects"))?;

    let report = if committed_only {
        sync(&mut store, &repo, &cache, &Registry)?
    } else {
        sync_worktree(&mut store, &repo, &cache, &Registry)?
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        let tree = &report.tree[..report.tree.len().min(12)];
        let dirty = if report.blobs_dirty > 0 {
            format!(" +{} uncommitted", report.blobs_dirty)
        } else {
            String::new()
        };
        if report.no_op {
            println!(
                "up to date (tree {tree}{dirty}) — {} nodes, {} edges",
                report.nodes, report.edges
            );
        } else {
            println!(
                "synced tree {tree}{dirty} — {} blobs ({} extracted, {} cached) → {} nodes, {} edges",
                report.blobs_total,
                report.blobs_extracted,
                report.blobs_cached,
                report.nodes,
                report.edges
            );
        }
    }
    Ok(())
}

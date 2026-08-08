//! Roteiro umbrella CLI. Wires the graph, spec, and render crates behind
//! subcommands; owns argument parsing, process I/O, and exit codes. See
//! ADR-0001 for the roadmap.
//!
//! @rto:0001

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
    Check {
        /// Emit the check report as JSON.
        #[arg(long)]
        json: bool,
    },
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
        Command::Check { json } => return run_check(json),
        Command::Init => "init",
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

/// Validate the authored layer (ADR `[[…]]` links and `@rto:` annotations)
/// against the derived graph; exit non-zero on drift.
fn run_check(json: bool) -> anyhow::Result<()> {
    use rto_graph::{ObjectCache, Registry, Repo, Store, sync};

    let cwd = std::env::current_dir()?;
    let repo = Repo::discover(&cwd)?;
    let store_dir = repo.git_dir().join("roteiro");
    std::fs::create_dir_all(&store_dir)?;
    let mut store = Store::open(&store_dir.join("graph.db"))?;
    let cache = ObjectCache::open(repo.common_dir().join("roteiro").join("objects"))?;

    // Build the derived graph, then read the authored inputs straight from the
    // HEAD tree (ADRs under docs/adr, `@rto:` annotations everywhere else).
    sync(&mut store, &repo, &cache, &Registry)?;

    let mut docs = Vec::new();
    let mut annotations = Vec::new();
    for blob in repo.walk_blobs()? {
        let bytes = repo.read_blob(&blob.oid)?;
        let text = String::from_utf8_lossy(&bytes);
        let file = std::path::Path::new(&blob.path);
        let is_md = file
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("md"));
        let name = file
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let is_adr = blob.path.starts_with("docs/adr/") && is_md && name != "README.md";
        if is_adr {
            match rto_spec::parse_adr(&blob.path, &text) {
                Ok(doc) => docs.push(doc),
                Err(e) => eprintln!("warning: skipping {}: {e}", blob.path),
            }
        } else {
            annotations.extend(rto_spec::scan_annotations(&blob.path, &text));
        }
    }

    let report = rto_spec::run(&mut store, &docs, &annotations)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        for v in &report.violations {
            eprintln!("drift [{}]: {}", v.kind.label(), v.message);
        }
        println!(
            "checked {} ADR(s): {} link(s) ok, {} annotation(s) ok, {} violation(s)",
            report.adrs,
            report.links_ok,
            report.annotations_ok,
            report.violations.len(),
        );
    }

    if report.has_violations() {
        std::process::exit(1);
    }
    Ok(())
}

//! Roteiro umbrella CLI. Wires the graph, spec, and render crates behind
//! subcommands; owns argument parsing, process I/O, and exit codes. See
//! ADR-0001 for the roadmap.
//!
//! @rto:0001

use clap::{Parser, Subcommand};

mod init;

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
    /// Query the graph: explain a node, or list all nodes of a kind.
    Query {
        /// Node key to explain (e.g. `sym:rust:…#Store`, `adr:0001`, `file:…`).
        key: Option<String>,
        /// List all nodes of this kind instead of explaining a key.
        #[arg(long, conflicts_with = "key")]
        kind: Option<String>,
        /// Emit the result as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Find a shortest path between two nodes (edges followed either direction).
    Path {
        /// Start node key.
        from: String,
        /// Goal node key.
        to: String,
        /// Emit the result as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Export the assembled graph to a portable JSON artifact.
    Export {
        /// Output file (default: `roteiro-graph.json`); `-` writes to stdout.
        #[arg(long)]
        out: Option<String>,
    },
    /// Load a graph artifact into the local store, skipping extraction.
    Load {
        /// Artifact file to load (`-` reads from stdin).
        file: String,
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
        /// Output directory (default: `website/dist` for docs, `vault` for obsidian).
        #[arg(long)]
        out: Option<String>,
    },
    /// Spec authoring (intent interview, house-style ADR scaffolding).
    Spec,
    /// Suggest `inferred` similarity edges (built with `--features inference`).
    #[cfg(feature = "inference")]
    Infer {
        /// Minimum confidence (cosine similarity) for a suggestion, `0.0..=1.0`.
        #[arg(long, default_value_t = 0.4)]
        min_confidence: f64,
        /// Maximum suggestions per node.
        #[arg(long, default_value_t = 5)]
        top_k: usize,
        /// Emit the report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Start the MCP server (built with `--features mcp`).
    #[cfg(feature = "mcp")]
    Serve {
        /// Serve networked over streamable HTTP at ADDR (e.g. `127.0.0.1:8080`)
        /// instead of stdio. Terminate TLS at a reverse proxy.
        #[arg(long, value_name = "ADDR")]
        http: Option<String>,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let name = match cli.command {
        Command::Sync { json, committed } => return run_sync(json, committed),
        Command::Check { json } => return run_check(json),
        Command::Query { key, kind, json } => return run_query(key, kind, json),
        Command::Path { from, to, json } => return run_path(&from, &to, json),
        Command::Export { out } => return run_export(out),
        Command::Load { file } => return run_load(&file),
        Command::Init => return run_init(),
        Command::Render { target, out } => return run_render(&target, out),
        Command::Import { .. } => "import",
        Command::Spec => "spec",
        #[cfg(feature = "inference")]
        Command::Infer {
            min_confidence,
            top_k,
            json,
        } => return run_infer(min_confidence, top_k, json),
        #[cfg(feature = "mcp")]
        Command::Serve { http } => return run_serve(http),
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

/// Open the repository and its per-worktree store and shared object cache.
fn open_graph() -> anyhow::Result<(rto_graph::Repo, rto_graph::Store, rto_graph::ObjectCache)> {
    use rto_graph::{ObjectCache, Repo, Store};
    let cwd = std::env::current_dir()?;
    let repo = Repo::discover(&cwd)?;
    let store_dir = repo.git_dir().join("roteiro");
    std::fs::create_dir_all(&store_dir)?;
    let store = Store::open(&store_dir.join("graph.db"))?;
    let cache = ObjectCache::open(repo.common_dir().join("roteiro").join("objects"))?;
    Ok((repo, store, cache))
}

/// Build the full graph into `store`: the derived code graph (via `sync`) plus
/// the authored ADR layer read from the `HEAD` tree. Returns the authored-layer
/// check report (used by `check`; ignored by `query`).
fn build_graph(
    repo: &rto_graph::Repo,
    store: &mut rto_graph::Store,
    cache: &rto_graph::ObjectCache,
) -> anyhow::Result<rto_spec::CheckReport> {
    use rto_graph::{Registry, sync};
    sync(store, repo, cache, &Registry)?;

    let mut docs = Vec::new();
    let mut annotations = Vec::new();
    let mut malformed = Vec::new();
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
                // A malformed ADR is drift, not a skippable warning: it would let
                // the gate pass while silently dropping authored intent.
                Err(e) => malformed.push(rto_spec::Violation {
                    kind: rto_spec::ViolationKind::MalformedAdr,
                    message: format!("{}: cannot parse ADR: {e}", blob.path),
                }),
            }
        } else {
            annotations.extend(rto_spec::scan_annotations(&blob.path, &text));
        }
    }

    let mut report = rto_spec::run(store, &docs, &annotations)?;
    report.violations.extend(malformed);
    Ok(report)
}

/// Validate the authored layer (ADR `[[…]]` links and `@rto:` annotations)
/// against the derived graph; exit non-zero on drift.
fn run_check(json: bool) -> anyhow::Result<()> {
    let (repo, mut store, cache) = open_graph()?;
    let report = build_graph(&repo, &mut store, &cache)?;

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

/// Scaffold Roteiro in the current repository: build the initial graph, install
/// the `post-checkout`/`post-merge` hooks, and add the `AGENTS.md` snippet.
fn run_init() -> anyhow::Result<()> {
    let (repo, mut store, cache) = open_graph()?;

    let report = build_graph(&repo, &mut store, &cache)?;
    let nodes = store.node_count()?;
    let edges = store.edge_count()?;

    // Hooks live under the common git dir so they are shared across worktrees.
    let hooks_dir = repo.common_dir().join("hooks");
    for name in init::MANAGED_HOOKS {
        match init::install_hook(&hooks_dir, name)? {
            init::HookOutcome::Installed => println!("installed hook: {name}"),
            init::HookOutcome::Updated => println!("refreshed hook: {name}"),
            init::HookOutcome::SkippedForeign => eprintln!(
                "warning: existing non-Roteiro `{name}` hook left untouched; \
                 add `roteiro sync --committed` to it to keep the graph fresh"
            ),
        }
    }

    if let Some(workdir) = repo.workdir() {
        let path = workdir.join("AGENTS.md");
        if init::ensure_agents(&path)? {
            println!("wrote Roteiro section to {}", path.display());
        }
    }

    println!("roteiro initialised — graph has {nodes} nodes, {edges} edges");
    if report.has_violations() {
        eprintln!(
            "note: {} authored-layer violation(s); run `roteiro check` for details",
            report.violations.len()
        );
    }
    Ok(())
}

/// Suggest `inferred` similarity edges over the graph and apply them. Builds the
/// full derived + authored graph first, then adds the fuzzy suggestion layer.
#[cfg(feature = "inference")]
fn run_infer(min_confidence: f64, top_k: usize, json: bool) -> anyhow::Result<()> {
    use rto_graph::{FactSet, InferenceConfig, Provenance, infer_edges};

    if !(0.0..=1.0).contains(&min_confidence) {
        anyhow::bail!("--min-confidence must be in 0.0..=1.0 (got {min_confidence})");
    }

    let (repo, mut store, cache) = open_graph()?;
    build_graph(&repo, &mut store, &cache)?;

    // Inference is authoritative: clear any prior inferred edges first so the
    // result reflects exactly the current flags (build_graph may no-op when the
    // tree is unchanged, which would otherwise leave stale suggestions behind).
    store.delete_edges_by_provenance(Provenance::Inferred)?;

    let config = InferenceConfig {
        min_confidence,
        top_k,
    };
    let edges = infer_edges(&store, config)?;
    let count = edges.len();
    // Inferred edges are additive suggestions; applying them never alters the
    // derived/authored facts already in the store.
    store.apply_factset(&FactSet {
        nodes: vec![],
        edges,
    })?;

    if json {
        let report = serde_json::json!({
            "min_confidence": min_confidence,
            "top_k": top_k,
            "inferred_edges": count,
        });
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "inferred {count} similarity edge(s) (min-confidence {min_confidence}, top-k {top_k}); \
             query them with `roteiro query <key>`",
        );
    }
    Ok(())
}

/// Query the graph: explain a node's provenance-labelled neighbourhood, or list
/// all nodes of a kind. Builds the full (derived + authored) graph first so
/// results reflect the current source and ADRs.
fn run_query(key: Option<String>, kind: Option<String>, json: bool) -> anyhow::Result<()> {
    use rto_graph::{NodeKind, explain, list_kind};

    let (repo, mut store, cache) = open_graph()?;
    build_graph(&repo, &mut store, &cache)?;

    match (key, kind) {
        (Some(key), _) => {
            let Some(ex) = explain(&store, &key)? else {
                anyhow::bail!(
                    "no node with key `{key}` (try `roteiro query --kind <kind>` to list nodes)"
                );
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&ex)?);
            } else {
                println!("{}  ({})  {}", ex.node.key, ex.node.kind, ex.node.name);
                if let Some(path) = &ex.node.path {
                    println!("  path: {path}");
                }
                if !ex.outgoing.is_empty() {
                    println!("  outgoing:");
                    for e in &ex.outgoing {
                        println!("    -[{}/{}]-> {}", e.kind, e.provenance, e.node);
                    }
                }
                if !ex.incoming.is_empty() {
                    println!("  incoming:");
                    for e in &ex.incoming {
                        println!("    <-[{}/{}]- {}", e.kind, e.provenance, e.node);
                    }
                }
            }
        }
        (None, Some(kind)) => {
            let listing = list_kind(&store, &NodeKind::from_token(&kind))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&listing)?);
            } else {
                println!("{} ({}):", listing.kind, listing.nodes.len());
                for n in &listing.nodes {
                    println!("  {}  {}", n.key, n.name);
                }
            }
        }
        (None, None) => {
            anyhow::bail!("provide a node key to explain, or `--kind <kind>` to list nodes");
        }
    }
    Ok(())
}

/// Find and print a shortest path between two nodes. Exits non-zero if the two
/// nodes are not connected, so it is usable as a reachability assertion.
fn run_path(from: &str, to: &str, json: bool) -> anyhow::Result<()> {
    let (repo, mut store, cache) = open_graph()?;
    build_graph(&repo, &mut store, &cache)?;

    let result = rto_graph::path(&store, from, to)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else if result.found {
        println!("{from}");
        for hop in &result.hops {
            let arrow = if hop.direction == "outgoing" {
                "->"
            } else {
                "<-"
            };
            println!("  {arrow}[{}/{}] {}", hop.kind, hop.provenance, hop.node);
        }
        println!("({} hop(s))", result.length);
    } else {
        // Keep stdout machine-readable/empty on failure; report to stderr.
        eprintln!("no path from `{from}` to `{to}`");
    }

    if !result.found {
        std::process::exit(1);
    }
    Ok(())
}

/// Assemble the full graph and write it as a portable JSON artifact.
fn run_export(out: Option<String>) -> anyhow::Result<()> {
    use rto_graph::GraphArtifact;

    let (repo, mut store, cache) = open_graph()?;
    build_graph(&repo, &mut store, &cache)?;
    let artifact = GraphArtifact::from_store(&store)?;
    let json = artifact.to_json()?;

    let out = out.unwrap_or_else(|| "roteiro-graph.json".to_owned());
    if out == "-" {
        println!("{json}");
    } else {
        std::fs::write(&out, format!("{json}\n"))?;
        eprintln!(
            "exported {} nodes, {} edges → {out}",
            artifact.facts.nodes.len(),
            artifact.facts.edges.len()
        );
    }
    Ok(())
}

/// Load a graph artifact into the local store, replacing its contents. Lets a
/// fresh clone obtain a ready-made graph without re-extraction.
fn run_load(file: &str) -> anyhow::Result<()> {
    use rto_graph::{GraphArtifact, Repo, Store};

    let json = if file == "-" {
        std::io::read_to_string(std::io::stdin())?
    } else {
        std::fs::read_to_string(file)?
    };
    let artifact = GraphArtifact::from_json(&json)?;

    let cwd = std::env::current_dir()?;
    let repo = Repo::discover(&cwd)?;
    let store_dir = repo.git_dir().join("roteiro");
    std::fs::create_dir_all(&store_dir)?;
    let mut store = Store::open(&store_dir.join("graph.db"))?;
    artifact.load_into(&mut store)?;

    println!(
        "loaded {} nodes, {} edges from {file}",
        store.node_count()?,
        store.edge_count()?
    );
    Ok(())
}

/// Serve the graph over the Model Context Protocol. Builds the full graph, then
/// serves over stdio (default) or streamable HTTP (`--http <addr>`).
#[cfg(feature = "mcp")]
fn run_serve(http: Option<String>) -> anyhow::Result<()> {
    let (repo, mut store, cache) = open_graph()?;
    build_graph(&repo, &mut store, &cache)?;

    match http {
        Some(addr) => {
            let addr: std::net::SocketAddr = addr
                .parse()
                .map_err(|e| anyhow::anyhow!("invalid --http address `{addr}`: {e}"))?;
            eprintln!("roteiro MCP server listening on http://{addr}/mcp");
            rto_render::mcp::serve_http(store, addr).map_err(|e| anyhow::anyhow!("{e}"))
        }
        None => rto_render::mcp::serve_stdio(store).map_err(|e| anyhow::anyhow!("{e}")),
    }
}

/// Render a build-output of the graph: the docs site or an Obsidian vault.
fn run_render(target: &str, out: Option<String>) -> anyhow::Result<()> {
    match rto_render::Target::parse(target) {
        Some(rto_render::Target::DocsSite) => render_docs(out),
        Some(rto_render::Target::ObsidianVault) => render_obsidian(out),
        None => anyhow::bail!("unknown render target `{target}` (expected: docs | obsidian)"),
    }
}

/// Render the documentation site: copy static assets, then render each ADR and
/// the ADR index into `<out>` (default `website/dist`).
fn render_docs(out: Option<String>) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let repo = rto_graph::Repo::discover(&cwd)?;
    let root = repo
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("cannot render docs in a bare repository"))?;
    let out = out.map_or_else(|| root.join("website/dist"), std::path::PathBuf::from);

    if out.exists() {
        std::fs::remove_dir_all(&out)?;
    }
    std::fs::create_dir_all(out.join("adr"))?;
    copy_dir(&root.join("website/public"), &out)?;

    // Render each ADR (skip the directory README), in a deterministic order.
    let adr_dir = root.join("docs/adr");
    let mut files: Vec<_> = std::fs::read_dir(&adr_dir)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("md"))
        .filter(|p| p.file_name().and_then(|n| n.to_str()) != Some("README.md"))
        .collect();
    files.sort();

    let mut entries = Vec::new();
    for path in &files {
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("adr");
        let md = std::fs::read_to_string(path)?;
        let rendered = rto_render::render_adr(&md, stem);
        std::fs::write(out.join("adr").join(format!("{stem}.html")), &rendered.html)?;
        entries.push(rto_render::IndexEntry {
            href: format!("{stem}.html"),
            title: rendered.title,
        });
    }
    std::fs::write(
        out.join("adr").join("index.html"),
        rto_render::render_adr_index(&entries),
    )?;

    println!(
        "rendered docs → {} ({} ADR page(s))",
        out.display(),
        entries.len()
    );
    Ok(())
}

/// Render an Obsidian vault: one linked markdown note per graph node in `<out>`
/// (default `vault`).
fn render_obsidian(out: Option<String>) -> anyhow::Result<()> {
    let (repo, mut store, cache) = open_graph()?;
    build_graph(&repo, &mut store, &cache)?;
    let out = out.map_or_else(
        || std::path::PathBuf::from("vault"),
        std::path::PathBuf::from,
    );
    if out.exists() {
        std::fs::remove_dir_all(&out)?;
    }
    std::fs::create_dir_all(&out)?;

    let mut count = 0usize;
    for key in store.all_keys()? {
        if let Some(ex) = rto_graph::explain(&store, &key)? {
            let note = rto_render::render_note(&ex);
            std::fs::write(out.join(&note.filename), &note.content)?;
            count += 1;
        }
    }
    println!(
        "rendered obsidian vault → {} ({count} note(s))",
        out.display()
    );
    Ok(())
}

/// Recursively copy the contents of `src` into `dst`.
fn copy_dir(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

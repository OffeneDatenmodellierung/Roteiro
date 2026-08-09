//! Roteiro umbrella CLI. Wires the graph, spec, and render crates behind
//! subcommands; owns argument parsing, process I/O, and exit codes. See
//! ADR-0001 for the roadmap.
//!
//! @rto:0001

use clap::{Parser, Subcommand};

mod config;
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
    /// Fetch a node's cached context bundle (its provenance-labelled
    /// neighbourhood), or refresh all cached contexts that have gone stale.
    Context {
        /// Node key to fetch context for. Omit together with `--refresh`.
        key: Option<String>,
        /// Rebuild every cached context whose node or a neighbour changed, prune
        /// entries for deleted nodes, and report the counts.
        #[arg(long, conflicts_with = "key")]
        refresh: bool,
        /// Emit the result as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show the effective, merged configuration. The default human-readable
    /// output labels each value's provenance (which layer set it); `--json`
    /// emits only the effective config, without provenance.
    Config {
        /// Emit the effective config as JSON (no provenance; use the default
        /// text output to see which layer set each value).
        #[arg(long)]
        json: bool,
    },
    /// List intent-debt markers (TODOs, stubs, deferred work) in the graph.
    Debt {
        /// Restrict to these categories (repeatable): todo | fixme | hack |
        /// stub | deferred. Omit to list all.
        #[arg(long, value_name = "CATEGORY")]
        kind: Vec<String>,
        /// Emit the report as JSON.
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
    /// Import from an external knowledge graph (graphify, lat), or compare
    /// against a codegraph snapshot as a validation oracle.
    Import {
        /// Source: graphify | lat (imported), or codegraph (compared, oracle-only).
        #[arg(long)]
        from: String,
        /// Path to the source: a Graphify dir/`graph.json`, a `lat.md/` dir, or a
        /// codegraph `.db` snapshot.
        path: String,
        /// Emit the migration report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Render the graph: docs site or Obsidian vault.
    Render {
        /// Target: docs | obsidian
        target: String,
        /// Output directory (default: `website/dist` for docs, `vault` for obsidian).
        #[arg(long)]
        out: Option<String>,
    },
    /// Graph-grounded spec/blueprint authoring (ADR-0004). Tier 0: offline,
    /// deterministic — no model required.
    Spec {
        #[command(subcommand)]
        action: SpecAction,
    },
    /// Suggest `inferred` similarity edges (built with `--features inference`).
    #[cfg(feature = "inference")]
    Infer {
        /// Minimum confidence (cosine similarity) for a suggestion, `0.0..=1.0`.
        /// Overrides `[infer] min_confidence` in config; default 0.4.
        #[arg(long)]
        min_confidence: Option<f64>,
        /// Maximum suggestions per node. Overrides `[infer] top_k`; default 5.
        #[arg(long)]
        top_k: Option<usize>,
        /// Use a pulled local model by name instead of the offline default
        /// (requires `--features inference-local-models`; falls back to the
        /// hashing embedder if the model is not installed). Overrides
        /// `[models] embedding`.
        #[arg(long, value_name = "NAME")]
        model: Option<String>,
        /// Emit the report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Report likely-duplicate content: nodes with identical content (same git
    /// blob) or near-identical embeddings (built with `--features inference`).
    #[cfg(feature = "inference")]
    #[command(visible_alias = "dup")]
    Duplicates {
        /// Minimum cosine similarity for a near-duplicate pair, `0.0..=1.0`.
        /// Exact (same-blob) duplicates are always reported. Overrides
        /// `[duplicates] min_similarity`; default 0.9.
        #[arg(long)]
        min_similarity: Option<f64>,
        /// Maximum pairs to report. Overrides `[duplicates] limit`; default 50.
        #[arg(long)]
        limit: Option<usize>,
        /// Emit the report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Manage pluggable local models: list the registry, pull with consent
    /// (`--features models`).
    #[cfg(feature = "models")]
    Model {
        #[command(subcommand)]
        action: ModelAction,
    },
    /// Start a server: the MCP graph server (`--features mcp`) or the local
    /// OpenAI-compatible model endpoint (`--models`, `--features serve`).
    #[cfg(any(feature = "mcp", feature = "serve"))]
    Serve {
        /// Serve the OpenAI-compatible `/v1` endpoint over installed models
        /// (ADR-0006), instead of the MCP graph server. Needs `--features serve`.
        #[arg(long)]
        models: bool,
        /// MCP server: serve networked over streamable HTTP at ADDR (e.g.
        /// `127.0.0.1:8080`) instead of stdio. Terminate TLS at a reverse proxy.
        #[arg(long, value_name = "ADDR")]
        http: Option<String>,
        /// Model server (`--models`): bind ADDR (default `127.0.0.1:8017`). A
        /// non-loopback address is warned about (no auth — front with a proxy).
        #[arg(long, value_name = "ADDR")]
        addr: Option<String>,
    },
}

/// `roteiro spec` actions (ADR-0004).
#[derive(Subcommand)]
enum SpecAction {
    /// Assemble graph-grounded context for a topic: related symbols (with their
    /// callers/callees and governing ADRs) and related docs. The grounding to
    /// start authoring from.
    Context {
        /// Topic to search the graph for (e.g. a symbol, module, or concept).
        topic: String,
        /// Maximum symbols and docs to include.
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Emit the context as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Emit a house-style, graph-grounded, `check`-clean ADR/blueprint skeleton
    /// for a topic — with an interview checklist and a build-plan outline.
    Scaffold {
        /// Topic the artifact is about (grounds the skeleton against the graph).
        topic: String,
        /// Title (defaults to the topic).
        #[arg(long)]
        title: Option<String>,
        /// Artifact kind: `adr` (numbered decision) or `blueprint` (technical
        /// implementation plan).
        #[arg(long, default_value = "adr")]
        kind: String,
        /// Write to this file instead of stdout.
        #[arg(long)]
        out: Option<String>,
    },
    /// Scaffold, then draft the placeholder sections offline with a small local
    /// instruct model (ADR-0004 Tier 1). Needs `--features inference-local-models`
    /// and a pulled generative model; falls back to the plain scaffold otherwise.
    Draft {
        /// Topic the artifact is about (grounds the draft against the graph).
        topic: String,
        /// Title (defaults to the topic).
        #[arg(long)]
        title: Option<String>,
        /// Artifact kind: `adr` or `blueprint`.
        #[arg(long, default_value = "adr")]
        kind: String,
        /// Write to this file instead of stdout.
        #[arg(long)]
        out: Option<String>,
    },
}

/// `roteiro model` actions.
#[cfg(feature = "models")]
#[derive(Subcommand)]
enum ModelAction {
    /// List registry models and which are installed for this platform.
    List,
    /// Download a model into `~/.roteiro/models` (asks before fetching).
    Pull {
        /// Registry model name (see `roteiro model list`).
        name: String,
        /// Skip the confirmation prompt and download immediately.
        #[arg(long)]
        yes: bool,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    // Load layered config once (project `roteiro.toml` + user `~/.roteiro/
    // config.toml`); a malformed file is a hard error for any command (ADR-0007).
    let cwd = std::env::current_dir()?;
    let cfg = config::load(&cwd)?;
    // Resolve the ingestion toggles once; every command that (re)builds the graph
    // extracts with the same set so they share one cache, never thrashing it.
    let ingest = cfg.effective.ingest.resolve();
    match cli.command {
        Command::Sync { json, committed } => run_sync(ingest, json, committed),
        Command::Check { json } => run_check(ingest, json),
        Command::Query { key, kind, json } => run_query(ingest, key, kind, json),
        Command::Context { key, refresh, json } => run_context(ingest, key, refresh, json),
        Command::Debt { kind, json } => run_debt(ingest, &kind, json),
        Command::Path { from, to, json } => run_path(ingest, &from, &to, json),
        Command::Export { out } => run_export(ingest, out),
        Command::Load { file } => run_load(&file),
        Command::Init => run_init(ingest),
        Command::Render { target, out } => run_render(ingest, &target, out),
        Command::Import { from, path, json } => run_import(ingest, &from, &path, json),
        Command::Spec { action } => run_spec(&cfg.effective, ingest, action),
        Command::Config { json } => run_config(&cfg, json),
        #[cfg(feature = "inference")]
        Command::Infer {
            min_confidence,
            top_k,
            model,
            json,
        } => run_infer(&cfg.effective, ingest, min_confidence, top_k, model, json),
        #[cfg(feature = "inference")]
        Command::Duplicates {
            min_similarity,
            limit,
            json,
        } => run_duplicates(&cfg.effective, ingest, min_similarity, limit, json),
        #[cfg(feature = "models")]
        Command::Model { action } => run_model(action),
        #[cfg(any(feature = "mcp", feature = "serve"))]
        Command::Serve { models, http, addr } => {
            run_serve(ingest, &cfg.effective, models, http, addr)
        }
    }
}

/// Which layer set a value, given whether the project/user layers carry it.
fn provenance(proj: bool, usr: bool) -> &'static str {
    if proj {
        "project"
    } else if usr {
        "user"
    } else {
        "default"
    }
}

/// Print the effective, merged configuration and each value's provenance
/// (`project` / `user` / `default`) — the answer to "why did it use that?".
fn run_config(loaded: &config::Loaded, json: bool) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(&loaded.effective)?);
        return Ok(());
    }
    println!(
        "project config: {}",
        loaded
            .project_path
            .as_deref()
            .map_or_else(|| "(none)".to_owned(), |p| p.display().to_string())
    );
    println!(
        "user config:    {}",
        loaded
            .user_path
            .as_deref()
            .map_or_else(|| "(none)".to_owned(), |p| p.display().to_string())
    );
    print_config_sections(loaded);
    println!("\n(unset values fall back to built-in defaults; a CLI flag overrides config)");
    Ok(())
}

/// Print each config section's values with provenance labels.
fn print_config_sections(loaded: &config::Loaded) {
    let source = provenance;
    let e = &loaded.effective;
    let (p, u) = (&loaded.project, &loaded.user);
    println!("\n[models]");
    println!(
        "  embedding  = {:?}  ({})",
        e.models.embedding,
        source(p.models.embedding.is_some(), u.models.embedding.is_some())
    );
    println!(
        "  generative = {:?}  ({})",
        e.models.generative,
        source(p.models.generative.is_some(), u.models.generative.is_some())
    );
    println!("[infer]");
    println!(
        "  min_confidence = {:?}  ({})",
        e.infer.min_confidence,
        source(
            p.infer.min_confidence.is_some(),
            u.infer.min_confidence.is_some()
        )
    );
    println!(
        "  top_k          = {:?}  ({})",
        e.infer.top_k,
        source(p.infer.top_k.is_some(), u.infer.top_k.is_some())
    );
    println!("[duplicates]");
    println!(
        "  min_similarity = {:?}  ({})",
        e.duplicates.min_similarity,
        source(
            p.duplicates.min_similarity.is_some(),
            u.duplicates.min_similarity.is_some()
        )
    );
    println!(
        "  limit          = {:?}  ({})",
        e.duplicates.limit,
        source(p.duplicates.limit.is_some(), u.duplicates.limit.is_some())
    );
    println!("[ingest]");
    println!(
        "  prose  = {:?}  ({})",
        e.ingest.prose,
        source(p.ingest.prose.is_some(), u.ingest.prose.is_some())
    );
    println!(
        "  pdf    = {:?}  ({})",
        e.ingest.pdf,
        source(p.ingest.pdf.is_some(), u.ingest.pdf.is_some())
    );
    println!(
        "  ocr    = {:?}  ({})",
        e.ingest.ocr,
        source(p.ingest.ocr.is_some(), u.ingest.ocr.is_some())
    );
    println!(
        "  vision = {:?}  ({})",
        e.ingest.vision,
        source(p.ingest.vision.is_some(), u.ingest.vision.is_some())
    );
    println!("[serve]");
    println!(
        "  addr   = {:?}  ({})",
        e.serve.addr,
        source(p.serve.addr.is_some(), u.serve.addr.is_some())
    );
    println!(
        "  models = {:?}  ({})",
        e.serve.models,
        source(p.serve.models.is_some(), u.serve.models.is_some())
    );
    println!(
        "  tools  = {:?}  ({})",
        e.serve.tools,
        source(p.serve.tools.is_some(), u.serve.tools.is_some())
    );
}

/// Sync the graph for the current repository, optionally including uncommitted
/// edits to tracked files.
fn run_sync(
    ingest: rto_graph::IngestConfig,
    json: bool,
    committed_only: bool,
) -> anyhow::Result<()> {
    use rto_graph::{ObjectCache, Registry, Repo, Store, sync, sync_worktree};

    let cwd = std::env::current_dir()?;
    let repo = Repo::discover(&cwd)?;

    // Graph DB is per-worktree (under the worktree git dir); the extraction
    // cache is shared across worktrees (under the common git dir).
    let store_dir = repo.git_dir().join("roteiro");
    std::fs::create_dir_all(&store_dir)?;
    let mut store = Store::open(&store_dir.join("graph.db"))?;
    let cache = ObjectCache::open(repo.common_dir().join("roteiro").join("objects"))?;

    let registry = Registry::new(ingest);
    let report = if committed_only {
        sync(&mut store, &repo, &cache, &registry)?
    } else {
        sync_worktree(&mut store, &repo, &cache, &registry)?
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
    ingest: rto_graph::IngestConfig,
) -> anyhow::Result<rto_spec::CheckReport> {
    use rto_graph::{Registry, sync};
    sync(store, repo, cache, &Registry::new(ingest))?;

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

    // Re-apply any persisted import layers (Graphify, lat.md, …) on top of the
    // freshly-rebuilt derived + authored graph, so imported knowledge is durable
    // across code-changing syncs. Dangling edges (endpoints removed by a sync)
    // are tolerated.
    store.reapply_imports()?;
    Ok(report)
}

/// Validate the authored layer (ADR `[[…]]` links and `@rto:` annotations)
/// against the derived graph; exit non-zero on drift.
fn run_check(ingest: rto_graph::IngestConfig, json: bool) -> anyhow::Result<()> {
    let (repo, mut store, cache) = open_graph()?;
    let report = build_graph(&repo, &mut store, &cache, ingest)?;

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
        // Report intent debt alongside drift (a summary, not a gate).
        println!("{}", debt_summary(&rto_graph::debt(&store, &[])?));
    }

    if report.has_violations() {
        std::process::exit(1);
    }
    Ok(())
}

/// Scaffold Roteiro in the current repository: build the initial graph, install
/// the `post-checkout`/`post-merge` hooks, and add the `AGENTS.md` snippet.
fn run_init(ingest: rto_graph::IngestConfig) -> anyhow::Result<()> {
    let (repo, mut store, cache) = open_graph()?;

    let report = build_graph(&repo, &mut store, &cache, ingest)?;
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

/// Resolve a config-sourced embedding model name against this binary's feature
/// set. When built with `inference-local-models`, the name is honoured; when
/// built without it, a config-set model can't be loaded, so — per ADR-0007's
/// "missing-feature keys warn" rule — emit a warning and fall back to the
/// offline default (returning `None`) rather than hard-failing. An explicit
/// `--model` flag bypasses this and is validated directly by the embedder.
#[cfg(feature = "inference-local-models")]
fn config_embedding_model(name: Option<&str>) -> Option<String> {
    name.map(str::to_owned)
}

/// See the `inference-local-models` variant: without local models a
/// config-sourced embedding model is warned about and ignored.
#[cfg(all(feature = "inference", not(feature = "inference-local-models")))]
fn config_embedding_model(name: Option<&str>) -> Option<String> {
    if let Some(name) = name {
        eprintln!(
            "warning: config `[models] embedding = {name:?}` needs the \
             `inference-local-models` feature; this build ignores it and uses \
             the offline default (pass `--model` to force an error instead)"
        );
    }
    None
}

/// Suggest `inferred` similarity edges over the graph and apply them. Builds the
/// full derived + authored graph first, then adds the fuzzy suggestion layer.
#[cfg(feature = "inference")]
fn run_infer(
    cfg: &config::Config,
    ingest: rto_graph::IngestConfig,
    min_confidence: Option<f64>,
    top_k: Option<usize>,
    model: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    use rto_graph::{FactSet, InferenceConfig};

    // Precedence: CLI flag > config > built-in default.
    let min_confidence = min_confidence.or(cfg.infer.min_confidence).unwrap_or(0.4);
    let top_k = top_k.or(cfg.infer.top_k).unwrap_or(5);
    // An explicit `--model` flag is always honoured (and errors below if this
    // binary lacks local-model support). A model coming *only* from config must
    // degrade gracefully per ADR-0007: warn and fall back to the offline default
    // rather than hard-failing a build that can't use it.
    let model = model.or_else(|| config_embedding_model(cfg.models.embedding.as_deref()));

    if !(0.0..=1.0).contains(&min_confidence) {
        anyhow::bail!("--min-confidence must be in 0.0..=1.0 (got {min_confidence})");
    }

    let (repo, mut store, cache) = open_graph()?;
    build_graph(&repo, &mut store, &cache, ingest)?;

    // Inference is authoritative over *its own* suggestions: clear prior
    // embedding-produced edges first so the result reflects exactly the current
    // flags (build_graph may no-op on an unchanged tree, which would otherwise
    // leave stale suggestions). Edges from other producers (e.g. a Graphify
    // import) carry a different src_ref and are left untouched.
    store.delete_edges_by_src_ref(rto_graph::EMBED_REF)?;

    let config = InferenceConfig {
        min_confidence,
        top_k,
    };

    // Choose the embedder: a pulled local model if requested and installed,
    // otherwise the offline hashing default.
    let (edges, embedder_label) = infer_with_embedder(&store, config, model.as_deref())?;
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
            "embedder": embedder_label,
            "inferred_edges": count,
        });
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "inferred {count} similarity edge(s) via {embedder_label} \
             (min-confidence {min_confidence}, top-k {top_k}); \
             query them with `roteiro query <key>`",
        );
    }
    Ok(())
}

/// Report likely-duplicate content (identical blobs + near-identical
/// embeddings) over the current graph. Read-only: builds the graph but applies
/// nothing. Uses the offline hashing embedder.
#[cfg(feature = "inference")]
fn run_duplicates(
    cfg: &config::Config,
    ingest: rto_graph::IngestConfig,
    min_similarity: Option<f64>,
    limit: Option<usize>,
    json: bool,
) -> anyhow::Result<()> {
    use rto_graph::DuplicateConfig;

    // Precedence: CLI flag > config > built-in default.
    let min_similarity = min_similarity
        .or(cfg.duplicates.min_similarity)
        .unwrap_or(0.9);
    let limit = limit.or(cfg.duplicates.limit).unwrap_or(50);

    if !(0.0..=1.0).contains(&min_similarity) {
        anyhow::bail!("--min-similarity must be in 0.0..=1.0 (got {min_similarity})");
    }

    let (repo, mut store, cache) = open_graph()?;
    build_graph(&repo, &mut store, &cache, ingest)?;

    let report = rto_graph::duplicates(
        &store,
        DuplicateConfig {
            min_similarity,
            limit,
        },
    )?;

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else if report.pairs.is_empty() {
        println!("no duplicate content found (min-similarity {min_similarity})");
    } else {
        let shown = if report.total > report.pairs.len() {
            format!(" (showing top {})", report.pairs.len())
        } else {
            String::new()
        };
        println!("{} duplicate pair(s){shown}:", report.total);
        for p in &report.pairs {
            let tag = if p.exact { "exact" } else { "~sim " };
            println!("  [{tag} {:.2}] {}  <->  {}", p.similarity, p.a, p.b);
        }
    }
    Ok(())
}

/// Run inference with the requested embedder, returning the edges and a label
/// describing which embedder was used. Without the `inference-local-models`
/// feature, only the hashing embedder exists.
#[cfg(all(feature = "inference", not(feature = "inference-local-models")))]
fn infer_with_embedder(
    store: &rto_graph::Store,
    config: rto_graph::InferenceConfig,
    model: Option<&str>,
) -> anyhow::Result<(Vec<rto_graph::Edge>, String)> {
    use rto_graph::infer_edges_with;
    if model.is_some() {
        anyhow::bail!(
            "--model requires the `inference-local-models` feature; \
             rebuild with `--features inference-local-models`"
        );
    }
    Ok((
        infer_edges_with(store, config, &rto_graph::HashEmbedder)?,
        "hashing embedder (offline default)".to_owned(),
    ))
}

/// Feature-rich variant: honour `--model` by loading a local candle model.
#[cfg(feature = "inference-local-models")]
fn infer_with_embedder(
    store: &rto_graph::Store,
    config: rto_graph::InferenceConfig,
    model: Option<&str>,
) -> anyhow::Result<(Vec<rto_graph::Edge>, String)> {
    use rto_graph::{HashEmbedder, LocalEmbedder, Platform, infer_edges_with};

    let Some(name) = model else {
        return Ok((
            infer_edges_with(store, config, &HashEmbedder)?,
            "hashing embedder (offline default)".to_owned(),
        ));
    };
    // Only accept a known registry model whose host-variant files are all
    // present — never an arbitrary directory under the store root.
    let spec = rto_graph::find_model(name)
        .ok_or_else(|| anyhow::anyhow!("unknown model `{name}` (see `roteiro model list`)"))?;
    let variant = spec
        .variant_for(Platform::host())
        .ok_or_else(|| anyhow::anyhow!("no variant of `{name}` for this platform"))?;
    if !rto_graph::is_installed(name, variant) {
        anyhow::bail!(
            "model `{name}` is not installed — run `roteiro model pull {name}` \
             (or omit --model to use the offline default)"
        );
    }
    let dir = rto_graph::model_dir(name);
    let embedder =
        LocalEmbedder::load(&dir).map_err(|e| anyhow::anyhow!("loading model `{name}`: {e}"))?;
    let edges = infer_edges_with(store, config, &EmbedderAdapter(&embedder))?;
    Ok((
        edges,
        format!("local model `{name}` (dim {})", embedder.dim()),
    ))
}

/// Adapts a fallible [`LocalEmbedder`] to the infallible [`rto_graph::Embedder`]
/// trait: on an embedding failure it returns an **empty** vector rather than
/// aborting the whole run. An empty vector has a length no real embedding
/// shares, so [`rto_graph::similarity`] scores it `0.0` against every other
/// node — i.e. that node simply receives no suggestions.
#[cfg(feature = "inference-local-models")]
struct EmbedderAdapter<'a>(&'a rto_graph::LocalEmbedder);

#[cfg(feature = "inference-local-models")]
impl rto_graph::Embedder for EmbedderAdapter<'_> {
    fn embed(&self, text: &str) -> Vec<f32> {
        self.0.embed(text).unwrap_or_default()
    }
}

/// Manage pluggable local embedding models: list the registry or pull a model.
#[cfg(feature = "models")]
fn run_model(action: ModelAction) -> anyhow::Result<()> {
    match action {
        ModelAction::List => {
            run_model_list();
            Ok(())
        }
        ModelAction::Pull { name, yes } => run_model_pull(&name, yes),
    }
}

/// Print the registry, marking which models are installed for this host.
#[cfg(feature = "models")]
fn run_model_list() {
    use rto_graph::{ModelKind, Platform, REGISTRY, ResourceTier};

    let host = Platform::host();
    println!(
        "platform: {}   model store: {}",
        host.as_str(),
        rto_graph::store_root().display()
    );
    println!("(the built-in hashing embedder is always available with no model)");

    // Group the opinionated picks by section, then by resource tier, so the
    // "which should I pull?" answer reads off a machine's resources.
    let sections = [
        (ModelKind::Embedding, "Embedding (`roteiro infer --model`)"),
        (ModelKind::Generative, "Generative (`roteiro spec draft`)"),
        (
            ModelKind::Ocr,
            "OCR — image text (`roteiro sync` with --features image-ocr)",
        ),
        (
            ModelKind::Vision,
            "Vision — image description (`roteiro sync` with --features image-vision)",
        ),
    ];
    let tiers = [
        (ResourceTier::Low, "low  (any laptop)"),
        (ResourceTier::Mid, "mid  (~16 GB)"),
        (ResourceTier::High, "high (workstation / 64 GB)"),
    ];

    for (kind, heading) in sections {
        println!("\n{heading}:");
        for (tier, tier_label) in tiers {
            for spec in REGISTRY.iter().filter(|s| s.kind == kind && s.tier == tier) {
                let variant = spec.variant_for(host);
                let installed = variant.is_some_and(|v| rto_graph::is_installed(spec.name, v));
                let mark = if installed {
                    "✓ installed"
                } else {
                    "  available"
                };
                let dim = if spec.dim > 0 {
                    format!(", dim {}", spec.dim)
                } else {
                    String::new()
                };
                // Generative sub-role (instruct/coding/reasoning); empty otherwise.
                let role = spec
                    .role
                    .as_str()
                    .map(|r| format!(", {r}"))
                    .unwrap_or_default();
                println!(
                    "  [{tier_label}] {mark}  {name}  ({licence}{role}{dim}, ~{size} MiB)\n      {desc}",
                    name = spec.name,
                    licence = spec.licence,
                    size = spec.size_mib,
                    desc = spec.description,
                );
            }
        }
    }
}

/// Download a model into the store, asking for consent first (unless `--yes` or
/// non-interactive, in which case the manual command is printed instead).
#[cfg(feature = "models")]
fn run_model_pull(name: &str, yes: bool) -> anyhow::Result<()> {
    use rto_graph::{Platform, ensure_model_dir, find_model};
    use std::io::Write as _;

    let spec = find_model(name)
        .ok_or_else(|| anyhow::anyhow!("unknown model `{name}` (see `roteiro model list`)"))?;
    let variant = spec
        .variant_for(Platform::host())
        .ok_or_else(|| anyhow::anyhow!("no variant of `{name}` for this platform"))?;

    let stdin_is_tty = std::io::IsTerminal::is_terminal(&std::io::stdin());
    if !yes {
        // Print exactly what would be fetched — source + licence + size.
        eprintln!(
            "roteiro would download model `{name}` (~{} MiB, {}) from:",
            spec.size_mib, spec.licence
        );
        for f in variant.files {
            eprintln!("  {}", f.url);
        }
        if !stdin_is_tty {
            // Never fetch without an explicit human yes; print the manual route.
            eprintln!(
                "\nnon-interactive: not downloading. Re-run with `--yes`, or fetch manually into {}",
                rto_graph::model_dir(name).display()
            );
            anyhow::bail!("download declined (non-interactive)");
        }
        eprint!("Download now? [y/N] ");
        std::io::stderr().flush().ok();
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y" | "yes" | "Yes") {
            anyhow::bail!("download declined");
        }
    }

    let dir = ensure_model_dir(name)?;
    for f in variant.files {
        let dest = dir.join(f.name);
        eprintln!("fetching {} …", f.name);
        if f.sha256.is_empty() {
            // Make the absence of a pinned hash explicit rather than silently
            // "passing" verification (an empty hash is treated as unpinned).
            eprintln!(
                "  warning: no checksum pinned for {} — integrity NOT verified",
                f.name
            );
        }
        // Stream the response straight to disk, hashing as it writes and
        // installing atomically — so a 20 GiB model never buffers in memory.
        let reader = http_reader(f.url)?;
        rto_graph::download_verified(reader, &dest, f.sha256)
            .map_err(|e| anyhow::anyhow!("downloading {}: {e}", f.name))?;
    }
    let use_hint = match spec.kind {
        rto_graph::ModelKind::Embedding => format!("roteiro infer --model {name}"),
        rto_graph::ModelKind::Generative => "roteiro spec draft <topic>".to_owned(),
        rto_graph::ModelKind::Ocr => {
            "roteiro sync (a build with --features image-ocr OCRs images)".to_owned()
        }
        rto_graph::ModelKind::Vision => {
            "roteiro sync (a build with --features image-vision describes images)".to_owned()
        }
    };
    println!(
        "installed `{name}` → {}  (use it with `{use_hint}`)",
        dir.display()
    );
    Ok(())
}

/// Open a streaming HTTPS reader for `url` (the body is not buffered whole).
#[cfg(feature = "models")]
fn http_reader(url: &str) -> anyhow::Result<impl std::io::Read> {
    Ok(ureq::get(url)
        .call()
        .map_err(|e| anyhow::anyhow!("GET {url}: {e}"))?
        .into_body()
        .into_reader())
}

/// Import an external knowledge graph into the store (or, for codegraph, compare
/// against it as a validation oracle).
fn run_import(
    ingest: rto_graph::IngestConfig,
    from: &str,
    path: &str,
    json: bool,
) -> anyhow::Result<()> {
    match from {
        "graphify" => run_import_graphify(ingest, path, json),
        "lat" => run_import_lat(ingest, path, json),
        "codegraph" => run_compare_codegraph(ingest, path, json),
        other => {
            anyhow::bail!("unknown import source `{other}` (expected: graphify | lat | codegraph)")
        }
    }
}

/// Compare Roteiro's derived graph against a codegraph `SQLite` snapshot and report
/// agreement/divergence. codegraph is a **validation oracle only** — its
/// structural edges are not imported (Roteiro re-derives them). Exits zero; the
/// report is informational.
fn run_compare_codegraph(
    ingest: rto_graph::IngestConfig,
    path: &str,
    json: bool,
) -> anyhow::Result<()> {
    let (repo, mut store, cache) = open_graph()?;
    // Build the derived graph so there is something to compare against.
    build_graph(&repo, &mut store, &cache, ingest)?;

    let report = rto_graph::compare_codegraph(std::path::Path::new(path), &store)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        if let Some(commit) = &report.source_commit {
            let short = &commit[..commit.len().min(12)];
            println!("codegraph oracle — snapshot indexed at {short}");
        }
        println!(
            "symbols: {} matched, {} scope-only diffs (same symbol, different \
             module scope), {} codegraph-only, {} roteiro-only \
             (codegraph {}, roteiro {}; {} constants are a known Roteiro gap)",
            report.symbols_matched,
            report.symbols_scope_diff,
            report.codegraph_only,
            report.roteiro_only,
            report.symbols_codegraph,
            report.symbols_roteiro,
            report.constants_codegraph,
        );
        println!(
            "calls: {}/{} codegraph internal calls agree ({} not re-derived — \
             Roteiro links only unambiguous calls)",
            report.calls_agree, report.calls_codegraph, report.calls_codegraph_only,
        );
        for key in report.codegraph_only_sample.iter().take(10) {
            println!("  codegraph-only: {key}");
        }
        for key in report.roteiro_only_sample.iter().take(10) {
            println!("  roteiro-only:   {key}");
        }
    }
    Ok(())
}

/// Import a lat.md directory: its markdown sections and `[[…]]` links become an
/// `authored` layer over the code graph (a doc node per file, a section node per
/// heading, `contains`/`references` edges). Durable and validated: links into
/// code that no longer exists are pruned by [`rto_graph::Store::apply_import_layer`].
fn run_import_lat(ingest: rto_graph::IngestConfig, path: &str, json: bool) -> anyhow::Result<()> {
    let (repo, mut store, cache) = open_graph()?;
    let root = repo
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("cannot import into a bare repository"))?;

    let cwd = std::env::current_dir()?;
    let dir = {
        let p = std::path::Path::new(path);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            cwd.join(p)
        }
    };
    if !dir.is_dir() {
        anyhow::bail!(
            "lat directory not found: {} (expected a lat.md/ dir)",
            dir.display()
        );
    }

    // Collect every markdown file under the directory, keyed by its repo-relative
    // path so node keys (`lat:<path>`) are stable and links resolve consistently.
    let mut files = Vec::new();
    collect_markdown(&dir, root, &mut files)?;
    // Sort by path only; the content is never a tie-breaker (paths are unique).
    files.sort_by(|a, b| a.0.cmp(&b.0));
    if files.is_empty() {
        anyhow::bail!("no .md files under {}", dir.display());
    }

    let imported = rto_spec::import_lat(&files);

    // Build the derived + authored graph first so code links validate against it.
    build_graph(&repo, &mut store, &cache, ingest)?;
    let applied = store.apply_import_layer(rto_spec::LAT_REF, &imported.facts)?;

    let r = &imported.report;
    if json {
        let mut report = serde_json::to_value(r)?;
        report["edges_applied"] = serde_json::json!(applied.edges_applied);
        report["edges_pruned_stale"] = serde_json::json!(applied.edges_pruned);
        report["durable"] = serde_json::json!(true);
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "imported lat.md: {} file(s), {} section(s), {} link(s) \
             ({} to sections, {} to code); {} edge(s) applied, {} stale pruned — persisted (durable)",
            r.files,
            r.sections,
            r.links_total,
            r.links_to_sections,
            r.links_to_code,
            applied.edges_applied,
            applied.edges_pruned,
        );
    }
    Ok(())
}

/// Recursively collect `*.md` files under `dir`, pushing `(repo-relative path,
/// contents)` pairs. Paths use `/` separators for stable, portable node keys.
/// Errors if a file is outside the repository `root`, since a non-repo-relative
/// key would be unstable and would import content from outside the repo.
///
/// Symlinks are **not** followed (checked via [`std::fs::DirEntry::file_type`],
/// which does not traverse the link): a symlinked directory or file could
/// otherwise pull in out-of-repo content behind a repo-relative-looking key.
fn collect_markdown(
    dir: &std::path::Path,
    root: &std::path::Path,
    out: &mut Vec<(String, String)>,
) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            collect_markdown(&path, root, out)?;
        } else if file_type.is_file() && path.extension().and_then(|e| e.to_str()) == Some("md") {
            let rel = path.strip_prefix(root).map_err(|_| {
                anyhow::anyhow!(
                    "lat file {} is outside the repository ({}); the lat.md \
                     directory must live inside the repo",
                    path.display(),
                    root.display()
                )
            })?;
            let rel = rel.to_string_lossy().replace('\\', "/");
            out.push((rel, std::fs::read_to_string(&path)?));
        }
    }
    Ok(())
}

/// Import a Graphify export: keep doc/concept/inferred knowledge, drop its code
/// structure (Roteiro re-derives that), and ground imported docs to real files.
/// The import is **durable**: its facts are persisted (keyed by
/// [`rto_spec::GRAPHIFY_REF`]) and re-applied by `build_graph` after every sync,
/// so they survive a later code-changing sync (dangling edges are tolerated).
fn run_import_graphify(
    ingest: rto_graph::IngestConfig,
    path: &str,
    json: bool,
) -> anyhow::Result<()> {
    use rto_graph::{Edge, EdgeKind};

    // Accept either the Graphify output directory or a graph.json directly.
    let p = std::path::Path::new(path);
    let graph_json = if p.is_dir() {
        p.join("graph.json")
    } else {
        p.to_path_buf()
    };
    let text = std::fs::read_to_string(&graph_json)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", graph_json.display()))?;
    let imported = rto_spec::import_graphify(&text)?;

    let (repo, mut store, cache) = open_graph()?;
    build_graph(&repo, &mut store, &cache, ingest)?;

    // Assemble the full import layer: Graphify's own nodes/edges plus grounding
    // links. When an imported node's `source_file` matches a `file:<path>` node
    // already in the graph, add an inferred edge linking the imported knowledge
    // to the derived file.
    let mut facts = imported.facts.clone();
    let mut linked = 0usize;
    for node in &imported.facts.nodes {
        if let Some(path) = &node.path {
            let file_key = format!("file:{path}");
            if store.get_node(&file_key)?.is_some() {
                let mut edge =
                    Edge::inferred(node.key.clone(), file_key, EdgeKind::References, 0.9);
                edge.src_ref = Some(rto_spec::GRAPHIFY_REF.to_owned());
                facts.edges.push(edge);
                linked += 1;
            }
        }
    }

    // Apply and persist the whole Graphify layer authoritatively: this replaces
    // any prior Graphify import (its edges, including grounding links), validates
    // each edge against the current graph — dropping cross-references to code
    // that is not present — and stores only the validated layer, so it is durable
    // across future syncs without keeping stale data.
    let applied = store.apply_import_layer(rto_spec::GRAPHIFY_REF, &facts)?;

    let r = &imported.report;
    if json {
        let mut report = serde_json::to_value(r)?;
        report["docs_linked_to_files"] = serde_json::json!(linked);
        report["edges_pruned_stale"] = serde_json::json!(applied.edges_pruned);
        report["durable"] = serde_json::json!(true);
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "imported graphify: {} node(s) ({} dropped as code), {} inferred edge(s) \
             ({} ast dropped, {} dangling skipped), {} hyperedge group(s); \
             {linked} doc(s) linked to files, {} stale pruned — persisted (durable)",
            r.nodes_imported,
            r.nodes_dropped_code,
            r.edges_imported,
            r.edges_dropped_ast,
            r.edges_skipped_dangling,
            r.hyperedges_imported,
            applied.edges_pruned,
        );
    }
    Ok(())
}

/// Graph-grounded spec/blueprint authoring (ADR-0004).
fn run_spec(
    cfg: &config::Config,
    ingest: rto_graph::IngestConfig,
    action: SpecAction,
) -> anyhow::Result<()> {
    match action {
        SpecAction::Context { topic, limit, json } => run_spec_context(ingest, &topic, limit, json),
        SpecAction::Scaffold {
            topic,
            title,
            kind,
            out,
        } => run_spec_scaffold(ingest, &topic, title.as_deref(), &kind, out.as_deref()),
        SpecAction::Draft {
            topic,
            title,
            kind,
            out,
        } => run_spec_draft(cfg, ingest, &topic, title.as_deref(), &kind, out.as_deref()),
    }
}

/// Build the derived+authored graph, then a house-style, grounded scaffold for
/// `topic` of the given `kind` (`adr` | `blueprint`). Returns the scaffold
/// markdown, its label (e.g. `ADR-0007`), and the grounded context — shared by
/// `spec scaffold` (Tier 0) and `spec draft` (Tier 1).
fn build_scaffold(
    ingest: rto_graph::IngestConfig,
    topic: &str,
    title: Option<&str>,
    kind: &str,
) -> anyhow::Result<(String, String, rto_spec::SpecContext)> {
    if kind != "adr" && kind != "blueprint" {
        anyhow::bail!("unknown --kind `{kind}` (expected: adr | blueprint)");
    }
    let (repo, mut store, cache) = open_graph()?;
    build_graph(&repo, &mut store, &cache, ingest)?;
    let root = repo
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("cannot scaffold in a bare repository"))?;
    let ctx = rto_spec::context(&store, topic, 10)?;

    let (md, label) = if kind == "adr" {
        let adr_id = next_adr_id(&root.join("docs/adr"));
        (
            rto_spec::scaffold_adr(topic, title, &adr_id, &today_utc(), &ctx),
            format!("ADR-{adr_id}"),
        )
    } else {
        (
            rto_spec::scaffold_blueprint(topic, title, &ctx),
            "blueprint".to_owned(),
        )
    };
    Ok((md, label, ctx))
}

/// Write `md` to `out` (or stdout), announcing `label` on stderr when writing.
fn emit_artifact(md: &str, label: &str, out: Option<&str>) -> anyhow::Result<()> {
    match out {
        Some(path) => {
            std::fs::write(path, md)?;
            eprintln!("wrote {label} → {path}");
        }
        None => print!("{md}"),
    }
    Ok(())
}

/// Emit a graph-grounded, house-style ADR or blueprint skeleton (ADR-0004 Tier 0).
fn run_spec_scaffold(
    ingest: rto_graph::IngestConfig,
    topic: &str,
    title: Option<&str>,
    kind: &str,
    out: Option<&str>,
) -> anyhow::Result<()> {
    let (md, label, _ctx) = build_scaffold(ingest, topic, title, kind)?;
    emit_artifact(&md, &format!("{label} scaffold"), out)
}

/// Draft the scaffold's placeholder sections with a small local instruct model
/// (ADR-0004 Tier 1). Needs `--features inference-local-models` and a pulled
/// generative model; without a model it emits the plain scaffold + a hint.
// Stage 20: `spec draft` generation runs on **llama.cpp** (the `serve` feature's
// engine) — the inference-core unify direction (ADR-0006) — falling back to the
// candle `LocalGenerator` only on a `inference-local-models`-without-`serve` build.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
fn run_spec_draft(
    cfg: &config::Config,
    ingest: rto_graph::IngestConfig,
    topic: &str,
    title: Option<&str>,
    kind: &str,
    out: Option<&str>,
) -> anyhow::Result<()> {
    use rto_graph::{
        ModelKind, ModelRole, Platform, REGISTRY, ResourceTier, find_model, is_installed,
    };

    let (scaffold, label, ctx) = build_scaffold(ingest, topic, title, kind)?;

    // Model pick: `[models] generative` from config if set (and a real generative
    // entry), otherwise the low-tier default (runs anywhere).
    let Some(spec) = cfg
        .models
        .generative
        .as_deref()
        .and_then(find_model)
        .filter(|m| m.kind == ModelKind::Generative)
        .or_else(|| {
            // Deterministic default as the registry grows: the low-tier *instruct*
            // model (qwen3-0.6b), not just any low-tier generative (which now
            // includes coding/reasoning picks).
            REGISTRY.iter().find(|m| {
                m.kind == ModelKind::Generative
                    && m.role == ModelRole::Instruct
                    && m.tier == ResourceTier::Low
            })
        })
    else {
        anyhow::bail!("no generative model in the registry");
    };
    let installed = spec
        .variant_for(Platform::host())
        .is_some_and(|v| is_installed(spec.name, v));
    if !installed {
        eprintln!(
            "note: generative model `{0}` is not installed — emitting the scaffold. \
             Draft prose with: roteiro model pull {0}",
            spec.name
        );
        return emit_artifact(&scaffold, &format!("{label} scaffold"), out);
    }

    if cfg!(debug_assertions) {
        eprintln!(
            "note: unoptimized build — local generation is very slow; use a \
             release build (`cargo build --release`) for usable speed."
        );
    }
    let drafts = draft_sections(spec.name, &scaffold, topic, &ctx)?;
    eprintln!(
        "drafted {} section(s) with {} (via {GEN_BACKEND})",
        drafts.len(),
        spec.name
    );
    let md = rto_spec::apply_drafts(&scaffold, &drafts);
    emit_artifact(&md, &format!("{label} draft"), out)
}

/// Draft each placeholder section of `scaffold` with the local generative model,
/// via **llama.cpp** (`serve`) — the Stage-20 inference-core path.
#[cfg(feature = "serve")]
const GEN_BACKEND: &str = "llama.cpp";

/// Max tokens generated per drafted section.
#[cfg(feature = "serve")]
const DRAFT_MAX_TOKENS: u32 = 800;

#[cfg(feature = "serve")]
fn draft_sections(
    model: &str,
    scaffold: &str,
    topic: &str,
    ctx: &rto_spec::SpecContext,
) -> anyhow::Result<Vec<(String, String)>> {
    use rto_serve::Engine as _; // brings `.chat` into scope

    let served = vec![rto_serve::llama::Served {
        name: model.to_owned(),
        path: rto_graph::model_dir(model).join("model.gguf"),
        mmproj: None,
    }];
    let engine = rto_serve::llama::LlamaEngine::new(served, 0)
        .map_err(|e| anyhow::anyhow!("starting llama.cpp: {e}"))?;

    let mut drafts = Vec::new();
    for (heading, hint) in rto_spec::draft_targets(scaffold) {
        let prompt = rto_spec::draft_prompt(topic, ctx, &heading, &hint);
        let completion = engine
            .chat(&rto_serve::ChatRequest {
                model: model.to_owned(),
                messages: vec![rto_serve::Message {
                    role: "user".to_owned(),
                    content: prompt,
                }],
                images: vec![],
                temperature: 0.0,
                max_tokens: DRAFT_MAX_TOKENS,
            })
            .map_err(|e| anyhow::anyhow!("drafting `{heading}`: {e}"))?;
        if !completion.content.trim().is_empty() {
            drafts.push((heading, completion.content));
        }
    }
    Ok(drafts)
}

/// Transitional fallback: draft with the candle `LocalGenerator` on a build that
/// has `inference-local-models` but not `serve`.
#[cfg(all(not(feature = "serve"), feature = "inference-local-models"))]
const GEN_BACKEND: &str = "candle";

#[cfg(all(not(feature = "serve"), feature = "inference-local-models"))]
fn draft_sections(
    model: &str,
    scaffold: &str,
    topic: &str,
    ctx: &rto_spec::SpecContext,
) -> anyhow::Result<Vec<(String, String)>> {
    let mut generator = rto_graph::LocalGenerator::load(&rto_graph::model_dir(model))
        .map_err(|e| anyhow::anyhow!("loading {model}: {e}"))?;
    let cfg = rto_graph::GenConfig::default();
    let mut drafts = Vec::new();
    for (heading, hint) in rto_spec::draft_targets(scaffold) {
        let prompt = rto_spec::draft_prompt(topic, ctx, &heading, &hint);
        let prose = generator
            .generate(None, &prompt, &cfg)
            .map_err(|e| anyhow::anyhow!("drafting `{heading}`: {e}"))?;
        if !prose.trim().is_empty() {
            drafts.push((heading, prose));
        }
    }
    Ok(drafts)
}

/// `spec draft` without a generation backend: guide the user to enable one.
#[cfg(not(any(feature = "serve", feature = "inference-local-models")))]
fn run_spec_draft(
    _cfg: &config::Config,
    _ingest: rto_graph::IngestConfig,
    _topic: &str,
    _title: Option<&str>,
    _kind: &str,
    _out: Option<&str>,
) -> anyhow::Result<()> {
    anyhow::bail!(
        "`spec draft` needs a generation backend: build with `--features serve` \
         (llama.cpp) or `--features inference-local-models` (candle), then \
         `roteiro model pull qwen3-0.6b`. (`spec scaffold` works with no model.)"
    )
}

/// The next zero-padded ADR id: one past the highest `NNNN-*.md` under `adr_dir`
/// (or `0001` if none/absent).
fn next_adr_id(adr_dir: &std::path::Path) -> String {
    let mut max = 0u32;
    if let Ok(entries) = std::fs::read_dir(adr_dir) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                let digits: String = name.chars().take_while(char::is_ascii_digit).collect();
                if let Ok(n) = digits.parse::<u32>() {
                    max = max.max(n);
                }
            }
        }
    }
    format!("{:04}", max + 1)
}

/// Today's UTC date as `YYYY-MM-DD`, dependency-free (Hinnant's civil-from-days).
fn today_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let days = i64::try_from(secs / 86_400).unwrap_or(0) + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let doe = days - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = year + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Assemble and print graph-grounded context for a topic (ADR-0004 Tier 0): the
/// related symbols with their neighbourhood and governing ADRs, plus related
/// docs. Builds the full derived + authored graph first so results are grounded.
fn run_spec_context(
    ingest: rto_graph::IngestConfig,
    topic: &str,
    limit: usize,
    json: bool,
) -> anyhow::Result<()> {
    let (repo, mut store, cache) = open_graph()?;
    build_graph(&repo, &mut store, &cache, ingest)?;

    let ctx = rto_spec::context(&store, topic, limit)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&ctx)?);
    } else {
        println!("context for \"{}\":", ctx.topic);
        if ctx.symbols.is_empty() && ctx.docs.is_empty() {
            println!("  (nothing in the graph matches — try `roteiro query --kind fn` to browse)");
        }
        if !ctx.symbols.is_empty() {
            println!("  symbols:");
            for s in &ctx.symbols {
                println!("    {}  ({})", s.node.key, s.node.kind);
                if let Some(c) = &s.container {
                    println!("      in: {c}");
                }
                if !s.called_by.is_empty() {
                    println!("      called by: {}", s.called_by.join(", "));
                }
                if !s.calls.is_empty() {
                    println!("      calls: {}", s.calls.join(", "));
                }
                if !s.authored_by.is_empty() {
                    println!("      governed by: {}", s.authored_by.join(", "));
                }
            }
        }
        if !ctx.docs.is_empty() {
            println!("  docs:");
            for d in &ctx.docs {
                println!("    {}  {}", d.key, d.name);
            }
        }
        if !ctx.related_adrs.is_empty() {
            println!("  related ADRs: {}", ctx.related_adrs.join(", "));
        }
    }
    Ok(())
}

/// Query the graph: explain a node's provenance-labelled neighbourhood, or list
/// all nodes of a kind. Builds the full (derived + authored) graph first so
/// results reflect the current source and ADRs.
fn run_query(
    ingest: rto_graph::IngestConfig,
    key: Option<String>,
    kind: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    use rto_graph::{NodeKind, explain, list_kind};

    let (repo, mut store, cache) = open_graph()?;
    build_graph(&repo, &mut store, &cache, ingest)?;

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

/// Fetch a node's cached context bundle, or (`--refresh`) reconcile all cached
/// contexts with the current graph — rebuilding stale ones and pruning entries
/// for deleted nodes. The cache is dependency-aware: a change to a node or any of
/// its neighbours invalidates its cached context (see `rto_graph::context`).
fn run_context(
    ingest: rto_graph::IngestConfig,
    key: Option<String>,
    refresh: bool,
    json: bool,
) -> anyhow::Result<()> {
    use rto_graph::{context, refresh_contexts};

    let (repo, mut store, cache) = open_graph()?;
    build_graph(&repo, &mut store, &cache, ingest)?;

    if refresh {
        let report = refresh_contexts(&store)?;
        if json {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            println!(
                "context cache refreshed: {} rebuilt, {} reused, {} pruned",
                report.rebuilt, report.reused, report.pruned
            );
        }
        return Ok(());
    }

    let Some(key) = key else {
        anyhow::bail!("provide a node key, or `--refresh` to refresh all cached contexts");
    };
    let Some(ctx) = context(&store, &key)? else {
        anyhow::bail!("no node with key `{key}` (try `roteiro query --kind <kind>` to list nodes)");
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&ctx)?);
    } else {
        println!("{}  ({})  {}", ctx.node.key, ctx.node.kind, ctx.node.name);
        println!("  fingerprint: {}", ctx.fingerprint);
        if !ctx.outgoing.is_empty() {
            println!("  outgoing:");
            for e in &ctx.outgoing {
                println!("    -[{}/{}]-> {}", e.kind, e.provenance, e.node);
            }
        }
        if !ctx.incoming.is_empty() {
            println!("  incoming:");
            for e in &ctx.incoming {
                println!("    <-[{}/{}]- {}", e.kind, e.provenance, e.node);
            }
        }
    }
    Ok(())
}

/// List intent-debt markers (TODOs, stubs, deferred work) in the graph, grouped
/// by category. A report, not a gate: it always exits zero.
fn run_debt(ingest: rto_graph::IngestConfig, kinds: &[String], json: bool) -> anyhow::Result<()> {
    let (repo, mut store, cache) = open_graph()?;
    build_graph(&repo, &mut store, &cache, ingest)?;

    let report = rto_graph::debt(&store, kinds)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        for item in &report.items {
            let loc = match (&item.path, item.line) {
                (Some(p), Some(l)) => format!("{p}:{l}"),
                (Some(p), None) => p.clone(),
                _ => item.key.clone(),
            };
            println!("  [{}] {loc}  {}", item.category, item.text);
        }
        println!("{}", debt_summary(&report));
    }
    Ok(())
}

/// A one-line summary of a [`rto_graph::DebtReport`], e.g.
/// `intent debt: 12 marker(s) (deferred 5, stub 4, todo 3)`.
fn debt_summary(report: &rto_graph::DebtReport) -> String {
    if report.total == 0 {
        return "intent debt: none".to_owned();
    }
    let breakdown: Vec<String> = report
        .by_category
        .iter()
        .map(|(cat, n)| format!("{cat} {n}"))
        .collect();
    format!(
        "intent debt: {} marker(s) ({})",
        report.total,
        breakdown.join(", ")
    )
}

/// Find and print a shortest path between two nodes. Exits non-zero if the two
/// nodes are not connected, so it is usable as a reachability assertion.
fn run_path(
    ingest: rto_graph::IngestConfig,
    from: &str,
    to: &str,
    json: bool,
) -> anyhow::Result<()> {
    let (repo, mut store, cache) = open_graph()?;
    build_graph(&repo, &mut store, &cache, ingest)?;

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
fn run_export(ingest: rto_graph::IngestConfig, out: Option<String>) -> anyhow::Result<()> {
    use rto_graph::GraphArtifact;

    let (repo, mut store, cache) = open_graph()?;
    build_graph(&repo, &mut store, &cache, ingest)?;
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

/// Dispatch `roteiro serve`: the OpenAI-compatible model endpoint (`--models`,
/// ADR-0006) or the MCP graph server. Each backend is feature-gated; a build
/// lacking the relevant feature reports how to enable it.
#[cfg(any(feature = "mcp", feature = "serve"))]
fn run_serve(
    ingest: rto_graph::IngestConfig,
    cfg: &config::Config,
    models: bool,
    http: Option<String>,
    addr: Option<String>,
) -> anyhow::Result<()> {
    if models {
        serve_models_endpoint(cfg, ingest, addr)
    } else {
        serve_mcp(ingest, http)
    }
}

/// Serve the graph over the Model Context Protocol. Builds the full graph, then
/// serves over stdio (default) or streamable HTTP (`--http <addr>`).
#[cfg(feature = "mcp")]
fn serve_mcp(ingest: rto_graph::IngestConfig, http: Option<String>) -> anyhow::Result<()> {
    let (repo, mut store, cache) = open_graph()?;
    build_graph(&repo, &mut store, &cache, ingest)?;

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

/// MCP serving is unavailable without the `mcp` feature.
#[cfg(all(not(feature = "mcp"), feature = "serve"))]
fn serve_mcp(_ingest: rto_graph::IngestConfig, _http: Option<String>) -> anyhow::Result<()> {
    anyhow::bail!(
        "MCP serving needs the `mcp` feature (build with `--features mcp`); \
         use `--models` for the OpenAI-compatible model endpoint"
    )
}

/// Serve installed generative models over the loopback, OpenAI-compatible `/v1`
/// endpoint (ADR-0006). Serves only installed models; never downloads.
#[cfg(feature = "serve")]
fn serve_models_endpoint(
    cfg: &config::Config,
    ingest: rto_graph::IngestConfig,
    addr: Option<String>,
) -> anyhow::Result<()> {
    use rto_graph::{ModelKind, Platform, REGISTRY, is_installed, model_dir};

    // Serve every installed **GGUF** model over the llama.cpp path: generative →
    // `/v1/chat/completions`, embedding → `/v1/embeddings`, vision → multimodal
    // `/v1/chat/completions`. Servable = the variant ships a `model.gguf` (this
    // excludes the candle safetensors/`model-q4_0` entries), and — for vision —
    // also an `mmproj.gguf` projector. OCR is not a served model. The
    // `[serve] models` allow-list narrows it further if set.
    let host = Platform::host();
    let wanted = cfg.serve.models.as_deref();
    let has_file = |m: &rto_graph::ModelSpec, name: &str| {
        m.variant_for(host)
            .is_some_and(|v| v.files.iter().any(|f| f.name == name))
    };
    let served: Vec<rto_serve::llama::Served> = REGISTRY
        .iter()
        .filter(|m| wanted.is_none_or(|w| w.iter().any(|n| n == m.name)))
        .filter(|m| has_file(m, "model.gguf"))
        .filter(|m| match m.kind {
            ModelKind::Generative | ModelKind::Embedding => true,
            // A vision model is only servable with its multimodal projector.
            ModelKind::Vision => has_file(m, "mmproj.gguf"),
            ModelKind::Ocr => false,
        })
        .filter(|m| m.variant_for(host).is_some_and(|v| is_installed(m.name, v)))
        .map(|m| rto_serve::llama::Served {
            name: m.name.to_owned(),
            path: model_dir(m.name).join("model.gguf"),
            mmproj: has_file(m, "mmproj.gguf").then(|| model_dir(m.name).join("mmproj.gguf")),
        })
        .collect();
    if served.is_empty() {
        anyhow::bail!(
            "no installed GGUF models to serve — pull one first \
             (`roteiro model pull qwen3-0.6b` for chat, \
             `roteiro model pull bge-small-en-v1.5-gguf` for embeddings, or \
             `roteiro model pull smolvlm-500m-gguf` for vision; \
             see `roteiro model list`)"
        );
    }

    // Address precedence: CLI flag > `[serve] addr` > default loopback.
    let addr = addr
        .or_else(|| cfg.serve.addr.clone())
        .unwrap_or_else(|| "127.0.0.1:8017".to_owned());
    let socket: std::net::SocketAddr = addr
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid serve address `{addr}`: {e}"))?;
    if !socket.ip().is_loopback() {
        eprintln!(
            "warning: binding a non-loopback address ({socket}) — the endpoint \
             has no auth; front it with a reverse proxy (ADR-0006)"
        );
    }

    let names = served
        .iter()
        .map(|s| s.name.clone())
        .collect::<Vec<_>>()
        .join(", ");
    let engine = rto_serve::llama::LlamaEngine::new(served, 0)
        .map_err(|e| anyhow::anyhow!("starting llama.cpp: {e}"))?;
    let engine: std::sync::Arc<dyn rto_serve::Engine> = std::sync::Arc::new(engine);

    // Auto-register the graph tools (ADR-0006) unless disabled: build the graph
    // once so the served model can `explain`/`search`/`path`/`debt` this repo.
    if cfg.serve.tools.unwrap_or(true) {
        let (repo, mut store, cache) = open_graph()?;
        build_graph(&repo, &mut store, &cache, ingest)?;
        let tools = std::sync::Arc::new(GraphToolRegistry::new(store));
        eprintln!(
            "roteiro model server listening on http://{socket}/v1 (graph tools on) — serving: {names}"
        );
        rto_serve::serve_blocking_with_tools(engine, tools, socket)
    } else {
        eprintln!("roteiro model server listening on http://{socket}/v1 — serving: {names}");
        rto_serve::serve_blocking(engine, socket)
    }
}

/// A [`rto_serve::ToolRegistry`] backing the served model with Roteiro's graph
/// query tools (ADR-0006). Wraps the store in a mutex (queries take `&Store` and
/// the registry is shared across request threads).
#[cfg(feature = "serve")]
struct GraphToolRegistry {
    store: std::sync::Mutex<rto_graph::Store>,
}

#[cfg(feature = "serve")]
impl GraphToolRegistry {
    fn new(store: rto_graph::Store) -> Self {
        Self {
            store: std::sync::Mutex::new(store),
        }
    }
}

#[cfg(feature = "serve")]
impl rto_serve::ToolRegistry for GraphToolRegistry {
    fn tools(&self) -> Vec<rto_serve::ToolDef> {
        use serde_json::json;
        vec![
            rto_serve::ToolDef {
                name: "explain".to_owned(),
                description: "Explain a graph node by key (its record and immediate \
                              neighbours), e.g. `fn:foo` or `file:src/main.rs`."
                    .to_owned(),
                parameters: json!({
                    "type": "object",
                    "properties": { "key": { "type": "string" } },
                    "required": ["key"],
                }),
            },
            rto_serve::ToolDef {
                name: "search".to_owned(),
                description: "Search graph nodes by text; returns the top matches with keys."
                    .to_owned(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string" },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 25 },
                    },
                    "required": ["query"],
                }),
            },
            rto_serve::ToolDef {
                name: "path".to_owned(),
                description: "Find a shortest path between two node keys.".to_owned(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "from": { "type": "string" },
                        "to": { "type": "string" },
                    },
                    "required": ["from", "to"],
                }),
            },
            rto_serve::ToolDef {
                name: "debt".to_owned(),
                description: "List intent-debt markers (todo/fixme/hack/stub/deferred), \
                              optionally filtered by category."
                    .to_owned(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "categories": { "type": "array", "items": { "type": "string" } },
                    },
                }),
            },
        ]
    }

    fn call(&self, name: &str, args: &serde_json::Value) -> Result<String, String> {
        let store = self
            .store
            .lock()
            .map_err(|_| "store mutex poisoned".to_owned())?;
        let str_arg = |k: &str| args.get(k).and_then(serde_json::Value::as_str);
        match name {
            "explain" => {
                let key = str_arg("key").ok_or("`explain` needs a string `key`")?;
                let r = rto_graph::explain(&store, key).map_err(|e| e.to_string())?;
                serde_json::to_string(&r).map_err(|e| e.to_string())
            }
            "search" => {
                let query = str_arg("query").ok_or("`search` needs a string `query`")?;
                // `limit` is model-controlled: clamp to 1..=25 (results are
                // truncated before feed-back anyway) so a huge value can't
                // waste work; the schema advertises the same bound.
                let limit = args
                    .get("limit")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|n| usize::try_from(n).ok())
                    .unwrap_or(10)
                    .clamp(1, 25);
                let r = rto_graph::search(&store, query, limit).map_err(|e| e.to_string())?;
                serde_json::to_string(&r).map_err(|e| e.to_string())
            }
            "path" => {
                let from = str_arg("from").ok_or("`path` needs a string `from`")?;
                let to = str_arg("to").ok_or("`path` needs a string `to`")?;
                let r = rto_graph::path(&store, from, to).map_err(|e| e.to_string())?;
                serde_json::to_string(&r).map_err(|e| e.to_string())
            }
            "debt" => {
                let categories: Vec<String> = args
                    .get("categories")
                    .and_then(serde_json::Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                let r = rto_graph::debt(&store, &categories).map_err(|e| e.to_string())?;
                serde_json::to_string(&r).map_err(|e| e.to_string())
            }
            other => Err(format!("unknown tool `{other}`")),
        }
    }
}

/// The model endpoint is unavailable without the `serve` feature.
#[cfg(all(not(feature = "serve"), feature = "mcp"))]
fn serve_models_endpoint(
    _cfg: &config::Config,
    _ingest: rto_graph::IngestConfig,
    _addr: Option<String>,
) -> anyhow::Result<()> {
    anyhow::bail!(
        "`serve --models` needs the `serve` feature (build with `--features serve`, \
         which pulls the llama.cpp engine)"
    )
}

/// Render a build-output of the graph: the docs site or an Obsidian vault.
fn run_render(
    ingest: rto_graph::IngestConfig,
    target: &str,
    out: Option<String>,
) -> anyhow::Result<()> {
    match rto_render::Target::parse(target) {
        Some(rto_render::Target::DocsSite) => render_docs(out),
        Some(rto_render::Target::ObsidianVault) => render_obsidian(ingest, out),
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

    // Render lifetime docs (the Build Plan) as first-class root-level pages,
    // and list them above the ADRs on the index.
    let mut lifetime = Vec::new();
    let build_plan = root.join("docs/BUILD_PLAN.md");
    if build_plan.is_file() {
        let md = std::fs::read_to_string(&build_plan)?;
        let rendered = rto_render::render_doc(&md, "Build Plan");
        std::fs::write(out.join("build-plan.html"), &rendered.html)?;
        lifetime.push(rto_render::IndexEntry {
            // The index lives under adr/, so link up one level.
            href: "../build-plan.html".to_owned(),
            title: rendered.title,
        });
    }

    std::fs::write(
        out.join("adr").join("index.html"),
        rto_render::render_adr_index(&lifetime, &entries),
    )?;

    println!(
        "rendered docs → {} ({} ADR page(s), {} lifetime doc(s))",
        out.display(),
        entries.len(),
        lifetime.len(),
    );
    Ok(())
}

/// Render an Obsidian vault: one linked markdown note per graph node in `<out>`
/// (default `vault`).
fn render_obsidian(ingest: rto_graph::IngestConfig, out: Option<String>) -> anyhow::Result<()> {
    let (repo, mut store, cache) = open_graph()?;
    build_graph(&repo, &mut store, &cache, ingest)?;
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

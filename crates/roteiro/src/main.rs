//! Roteiro umbrella CLI. Wires the graph, spec, and render crates behind
//! subcommands; owns argument parsing, process I/O, and exit codes. See
//! ADR-0001 for the roadmap.
//!
//! @rto:0001

use clap::{Parser, Subcommand};

mod config;
mod infer_links;
mod init;
mod overview;
mod pins;
mod review;

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
    Init {
        /// Install freshness hooks that fetch the CI-published graph artifact
        /// (via `gh`) before rebuilding locally. Opt-in: the hooks only reach the
        /// network with this flag, and fall back to a local rebuild on any miss.
        #[arg(long)]
        fetch: bool,
        /// Also regenerate the local Obsidian vault (`vault/`, gitignored) from
        /// the graph on every checkout/merge/commit, so it stays current.
        #[arg(long)]
        vault: bool,
    },
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
    /// Graph-grounded review of the current working-tree change: for each
    /// touched symbol, its callers/callees, the ADRs governing it, related docs,
    /// plus the intent-debt and authored drift the change introduces and the
    /// blast radius of dependents to check. Non-zero exit when the change
    /// introduces drift. The CLI-first review surface (MCP tools are a bonus).
    Review {
        /// Emit the review as JSON.
        #[arg(long)]
        json: bool,
        /// Review the commit range `<base>..HEAD` (any revspec — a branch,
        /// `HEAD~3`, a sha) against the committed graph, instead of the
        /// working-tree change. Use for a whole-branch review (e.g. `--base main`).
        #[arg(long)]
        base: Option<String>,
    },
    /// Verify authored links against code and ADR states; non-zero on drift.
    ///
    /// By default this validates the working tree — tracked files as they are on
    /// disk, unstaged edits included (not the git index). Pass `--staged` to
    /// validate exactly the git index (what a commit would record — the precise
    /// pre-commit gate), or `--committed` to validate only the `HEAD` tree (the
    /// CI merge gate).
    Check {
        /// Emit the check report as JSON.
        #[arg(long)]
        json: bool,
        /// Validate only the committed `HEAD` tree, ignoring uncommitted edits.
        #[arg(long, conflicts_with = "staged")]
        committed: bool,
        /// Validate the git index — exactly what a commit would record (staged
        /// changes only, not unstaged working-tree edits).
        #[arg(long)]
        staged: bool,
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
    /// Search the graph by text — ranked hits over names, keys, paths and
    /// captured content (doc/ADR/blueprint prose). The entry point for
    /// "what/why" questions; then `query` a returned key to explain it.
    Search {
        /// Free-text query (one or more words).
        query: String,
        /// Maximum number of hits to return.
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Emit the results as JSON.
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
    /// Verify cross-repo links across a workspace (ADR-0009): resolve each repo's
    /// authored `[[links]]` against the other repos' graphs, reporting the target
    /// each resolves to and flagging **drift** (targets that no longer exist).
    /// Exits non-zero when any link is unresolved, so it works as a CI gate.
    ///
    /// With `--infer`, instead auto-match each repo's **config keys** (TOML / JSON
    /// / `.env`) against a hub repo's — surfacing correspondences with no
    /// hand-authored links, and flagging orphan keys (drift candidates).
    ///
    /// With `--matrix`, render the cross-repo **config override matrix + drift**
    /// view (ADR-0009 "views") — every hub key against each spoke that overrides
    /// it — as a text table, `--json`, or a self-contained `--html` page.
    Links {
        /// Workspace root to include (repeatable); combined with `[workspace]`
        /// config and the current repo.
        #[arg(long, value_name = "ROOT")]
        workspace: Vec<String>,
        /// Infer links by matching config keys across repos, instead of verifying
        /// authored `[[links]]`. Mutually exclusive with `--matrix`.
        #[arg(long, conflicts_with = "matrix")]
        infer: bool,
        /// Render the cross-repo config override matrix + drift view instead of the
        /// authored-link report.
        #[arg(long)]
        matrix: bool,
        /// The source-of-truth project to match against (default: the repo with the
        /// most config keys). Applies to `--infer` and `--matrix`.
        #[arg(long, value_name = "PROJECT")]
        hub: Option<String>,
        /// Resolve against the hub at a **pinned version** (a commit sha / tag / any
        /// git rev — e.g. the sha a spoke's submodule points at) instead of its
        /// `HEAD`, so drift is measured against the version actually deployed
        /// (ADR-0009 step 8). Applies to `--infer` and `--matrix`.
        #[arg(long, value_name = "REV")]
        hub_rev: Option<String>,
        /// With `--infer`: resolve **each spoke against the hub version it itself
        /// pins** — read from the spoke's `submodule` / `image_ref` node — instead
        /// of one version for all (ADR-0009 step 8b). Spokes with no detectable pin
        /// fall back to the hub's `HEAD`.
        #[arg(long, requires = "infer", conflicts_with_all = ["matrix", "hub_rev"])]
        pinned: bool,
        /// With `--infer`: persist the inferred correspondences into each spoke's
        /// graph as durable cross-repo edges (an `inferred` import layer that
        /// survives sync), instead of only reporting them.
        #[arg(long, requires = "infer")]
        write: bool,
        /// With `--matrix`: write a self-contained HTML page (the `render web-graph`
        /// output) to `--out` (default `roteiro-overview.html`; `-` for stdout).
        #[arg(long, requires = "matrix")]
        html: bool,
        /// With `--matrix --html`: output file (default `roteiro-overview.html`).
        #[arg(long, value_name = "FILE", requires = "html")]
        out: Option<String>,
        /// Emit the report as JSON.
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
        /// Load even if the artifact's tree does not match the working `HEAD`.
        /// By default a mismatch is refused, so a fetched CI artifact for a
        /// different commit never installs a wrong graph (the hook then rebuilds).
        #[arg(long)]
        force: bool,
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
        /// For the MCP-only server; with `--models`, use `--mcp` (+ `--addr`).
        #[arg(long, value_name = "ADDR", conflicts_with = "models")]
        http: Option<String>,
        /// Model server (`--models`): bind ADDR (default `127.0.0.1:8017`). A
        /// non-loopback address is warned about (no auth — front with a proxy).
        #[arg(long, value_name = "ADDR")]
        addr: Option<String>,
        /// Model server (`--models`): terminate TLS in-process using this PEM
        /// certificate-chain file (paired with `--tls-key`). Overrides
        /// `[serve] tls_cert`. Set both `--tls-cert` and `--tls-key` for HTTPS,
        /// or neither for plain HTTP; setting only one is an error.
        #[arg(long, value_name = "FILE")]
        tls_cert: Option<String>,
        /// Model server (`--models`): the PEM private-key file for `--tls-cert`.
        #[arg(long, value_name = "FILE")]
        tls_key: Option<String>,
        /// Workspace mode (ADR-0008): host every git repo under ROOT
        /// (repeatable), so one server — holding the model once — answers
        /// questions about many projects, selected per call by `project`.
        /// Combined with `[workspace]` config. Omit for single-repo serving
        /// (the current directory's repo).
        #[arg(long, value_name = "ROOT")]
        workspace: Vec<String>,
        /// Workspace mode: (re)build each project's graph the first time it is
        /// queried, instead of serving whatever its hooks last left. Slower on
        /// first touch, but never serves a stale or missing graph (ADR-0008).
        #[arg(long)]
        sync_on_access: bool,
        /// With `--models`: also mount the MCP graph server at `/mcp` on the
        /// **same port**, so one process serves both `/v1` and `/mcp` over one
        /// Workspace (needs `--features serve,mcp`). Only meaningful with
        /// `--models` — the plain `serve` already is the MCP server.
        #[arg(long, requires = "models")]
        mcp: bool,
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
    /// Scaffold, then draft the unfilled sections offline with a small local
    /// instruct model (ADR-0004 Tier 1). Needs a generation backend
    /// (`--features serve` or `--features inference-local-models`, both
    /// llama.cpp) and a pulled generative model; falls back to the plain
    /// scaffold otherwise.
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

/// Expand a leading `~/` (or a bare `~`) to the user's home directory; any other
/// path is returned unchanged. Used for config paths such as `[paths]
/// model_store`.
fn expand_tilde(path: &str) -> std::path::PathBuf {
    let home = || std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    if path == "~"
        && let Some(h) = home()
    {
        return std::path::PathBuf::from(h);
    }
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(h) = home()
    {
        return std::path::Path::new(&h).join(rest);
    }
    std::path::PathBuf::from(path)
}

// `main` is a one-arm-per-subcommand dispatcher; splitting the match further just
// scatters the CLI wiring, so the line-count lint is noise here.
#[allow(clippy::too_many_lines)]
fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    // Load layered config once (project `roteiro.toml` + user `~/.roteiro/
    // config.toml`); a malformed file is a hard error for any command (ADR-0007).
    let cwd = std::env::current_dir()?;
    let cfg = config::load(&cwd)?;
    // Resolve the ingestion toggles once; every command that (re)builds the graph
    // extracts with the same set so they share one cache, never thrashing it.
    let ingest = cfg.effective.ingest.resolve();
    // Paths excluded from the intent-debt scan (`[debt] ignore`), shared by
    // `debt` and `check`'s debt summary.
    let debt_ignore: &[String] = cfg.effective.debt.ignore.as_deref().unwrap_or(&[]);
    // Honour `[paths] model_store` before any command touches the model store.
    // The registry lives behind the `models` feature; on a build without it a
    // configured path is inert, so we warn rather than silently ignore it.
    if let Some(dir) = cfg.effective.paths.model_store.as_deref() {
        let dir = expand_tilde(dir);
        #[cfg(feature = "models")]
        rto_graph::set_model_store(dir);
        #[cfg(not(feature = "models"))]
        {
            let _ = dir;
            eprintln!(
                "warning: `[paths] model_store` is set but this build lacks the \
                 `models` feature; the setting has no effect"
            );
        }
    }
    match cli.command {
        Command::Sync { json, committed } => run_sync(ingest, json, committed),
        Command::Check {
            json,
            committed,
            staged,
        } => run_check(ingest, json, committed, staged, debt_ignore),
        Command::Review { json, base } => run_review(ingest, json, base.as_deref()),
        Command::Query { key, kind, json } => run_query(ingest, key, kind, json),
        Command::Search { query, limit, json } => run_search(ingest, &query, limit, json),
        Command::Context { key, refresh, json } => run_context(ingest, key, refresh, json),
        Command::Debt { kind, json } => run_debt(ingest, &kind, json, debt_ignore),
        Command::Path { from, to, json } => run_path(ingest, &from, &to, json),
        Command::Links {
            workspace,
            infer,
            matrix,
            hub,
            hub_rev,
            pinned,
            write,
            html,
            out,
            json,
        } => {
            let pin = PinnedHub {
                rev: hub_rev.as_deref(),
                auto: pinned,
                ingest,
            };
            if matrix {
                run_links_matrix(
                    &cfg.effective,
                    &workspace,
                    hub.as_deref(),
                    pin,
                    html,
                    out,
                    json,
                )
            } else if infer {
                run_links_infer(&cfg.effective, &workspace, hub.as_deref(), pin, write, json)
            } else {
                run_links(&cfg.effective, &workspace, json)
            }
        }
        Command::Export { out } => run_export(ingest, out),
        Command::Load { file, force } => run_load(&file, force),
        Command::Init { fetch, vault } => run_init(ingest, fetch, vault),
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
        Command::Serve {
            models,
            http,
            addr,
            tls_cert,
            tls_key,
            workspace,
            sync_on_access,
            mcp,
        } => run_serve(
            ingest,
            &cfg.effective,
            ServeOptions {
                models,
                http,
                addr,
                tls_cert,
                tls_key,
                mcp,
            },
            &workspace,
            sync_on_access,
        ),
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

/// Print `value` as pretty JSON to stdout — the shared `--json` output path for
/// every subcommand.
fn emit_json<T: serde::Serialize>(value: &T) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

/// Print the effective, merged configuration and each value's provenance
/// (`project` / `user` / `default`) — the answer to "why did it use that?".
fn run_config(loaded: &config::Loaded, json: bool) -> anyhow::Result<()> {
    if json {
        emit_json(&loaded.effective)?;
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
    println!(
        "  audio  = {:?}  ({})",
        e.ingest.audio,
        source(p.ingest.audio.is_some(), u.ingest.audio.is_some())
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

    print_workspace_section(e, p, u);
}

/// Print the `[workspace]` and `[[links]]` config sections (ADR-0008/0009), with
/// each value's provenance. Split out of [`print_config_sections`] to keep it
/// under the line budget.
fn print_workspace_section(e: &config::Config, p: &config::Config, u: &config::Config) {
    println!("[workspace]");
    println!(
        "  roots = {:?}  ({})",
        e.workspace.roots,
        provenance(p.workspace.roots.is_some(), u.workspace.roots.is_some())
    );
    println!(
        "  repos = {:?}  ({})",
        e.workspace.repos,
        provenance(p.workspace.repos.is_some(), u.workspace.repos.is_some())
    );
    if !e.links.is_empty() {
        println!(
            "[[links]]  ({} cross-repo link(s), ADR-0009)",
            e.links.len()
        );
        for l in &e.links {
            println!(
                "  → {}  ({})",
                l.to,
                l.kind.as_deref().unwrap_or("references")
            );
        }
    }
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
        emit_json(&report)?;
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

/// The bytes of a tracked file's authored source: the committed `HEAD` blob when
/// `committed`, otherwise its **working-tree** copy — the file as it is on disk,
/// which includes any unstaged edits and is *not* the git index. (This matches
/// [`rto_graph::sync_worktree`], which the derived graph is built from, so the
/// authored and derived layers stay consistent.) Returns `Ok(None)` when a
/// worktree file has been deleted, so the caller drops it.
fn read_source(
    repo: &rto_graph::Repo,
    blob: &rto_graph::BlobRef,
    source: GraphSource,
) -> anyhow::Result<Option<Vec<u8>>> {
    match source {
        // Worktree: the file as it stands on disk (unstaged edits included), or
        // `None` if it was deleted there.
        GraphSource::Worktree => match repo.workdir() {
            Some(workdir) => match std::fs::read(workdir.join(&blob.path)) {
                Ok(bytes) => Ok(Some(bytes)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e.into()),
            },
            None => Ok(Some(repo.read_blob(&blob.oid)?)),
        },
        // Committed reads the `HEAD` blob; Index reads the staged blob — for both,
        // `blob.oid` is already the right object (the blob list came from that
        // tree), so read it directly.
        GraphSource::Committed | GraphSource::Index => Ok(Some(repo.read_blob(&blob.oid)?)),
    }
}

/// Which tree the graph is built from: the committed `HEAD`, the working tree
/// (uncommitted edits on disk), or the git index (the staged tree a commit would
/// record). Selects the sync engine and the authored-layer source together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GraphSource {
    /// The committed `HEAD` tree (the CI merge gate).
    Committed,
    /// The working tree: `HEAD` plus uncommitted edits to tracked files on disk.
    Worktree,
    /// The git index — exactly what a commit would record (the pre-commit gate).
    Index,
}

/// Build the full graph into `store`: the derived code graph plus the authored
/// ADR layer, both from the tree named by `source`. Returns the authored-layer
/// check report (used by `check`; ignored by `query`).
fn build_graph(
    repo: &rto_graph::Repo,
    store: &mut rto_graph::Store,
    cache: &rto_graph::ObjectCache,
    ingest: rto_graph::IngestConfig,
    source: GraphSource,
) -> anyhow::Result<rto_spec::CheckReport> {
    use rto_graph::{Registry, sync, sync_index, sync_worktree};
    let registry = Registry::new(ingest);
    match source {
        GraphSource::Committed => sync(store, repo, cache, &registry)?,
        GraphSource::Worktree => sync_worktree(store, repo, cache, &registry)?,
        GraphSource::Index => sync_index(store, repo, cache, &registry)?,
    };

    // The authored-layer file set must match the derived tree: the staged files
    // in Index mode (so a staged-new ADR is seen), else the `HEAD` tree.
    let blobs = match source {
        GraphSource::Index => repo.index_files()?,
        GraphSource::Committed | GraphSource::Worktree => repo.walk_blobs()?,
    };
    let mut docs = Vec::new();
    let mut blueprints = Vec::new();
    let mut annotations = Vec::new();
    let mut malformed = Vec::new();
    for blob in blobs {
        // Parse the authored source from the same tree the derived layer used.
        let Some(bytes) = read_source(repo, &blob, source)? else {
            continue;
        };
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
        } else if is_md && rto_spec::is_blueprint(&blob.path, &text) {
            // House-style blueprints (no frontmatter) author `[[…]]` links like
            // ADRs; their links are drift-checked against the derived graph too.
            blueprints.push(rto_spec::parse_blueprint(&blob.path, &text));
        } else {
            annotations.extend(rto_spec::scan_annotations(&blob.path, &text));
        }
    }

    let mut report = rto_spec::run(store, &docs, &blueprints, &annotations)?;
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
fn run_check(
    ingest: rto_graph::IngestConfig,
    json: bool,
    committed: bool,
    staged: bool,
    debt_ignore: &[String],
) -> anyhow::Result<()> {
    let source = if staged {
        GraphSource::Index
    } else if committed {
        GraphSource::Committed
    } else {
        GraphSource::Worktree
    };
    let (repo, mut store, cache) = open_graph()?;
    let report = build_graph(&repo, &mut store, &cache, ingest, source)?;

    if json {
        emit_json(&report)?;
    } else {
        for v in &report.violations {
            eprintln!("drift [{}]: {}", v.kind.label(), v.message);
        }
        println!(
            "checked {} ADR(s), {} blueprint(s): {} link(s) ok, {} annotation(s) ok, {} violation(s)",
            report.adrs,
            report.blueprints,
            report.links_ok,
            report.annotations_ok,
            report.violations.len(),
        );
        // Report intent debt alongside drift (a summary, not a gate).
        println!(
            "{}",
            debt_summary(&rto_graph::debt(&store, &[], debt_ignore)?)
        );
    }

    if report.has_violations() {
        std::process::exit(1);
    }
    Ok(())
}

/// Assemble a graph-grounded review and print it (human or `--json`); exit
/// non-zero if the change introduces drift. With `base`, review the commit range
/// `base..HEAD` against the committed graph; otherwise the working-tree change.
fn run_review(
    ingest: rto_graph::IngestConfig,
    json: bool,
    base: Option<&str>,
) -> anyhow::Result<()> {
    let (repo, mut store, cache) = open_graph()?;
    // Range review is over committed history, so build the committed (HEAD)
    // graph; a working-tree review overlays uncommitted edits so the graph and
    // the change set agree. Either way, capture the authored-layer drift.
    let source = if base.is_some() {
        GraphSource::Committed
    } else {
        GraphSource::Worktree
    };
    let report = build_graph(&repo, &mut store, &cache, ingest, source)?;
    let changed =
        if let Some(base) = base {
            repo.changed_between(base)?
        } else {
            // Working-tree review: tracked edits/deletes, plus brand-new untracked
            // files as additions — the overlaid graph already includes them, so the
            // change set must too or their symbols would go unreviewed.
            let mut changed = repo.changed_files()?;
            changed.extend(repo.untracked_files()?.into_iter().map(|path| {
                rto_graph::ChangedFile {
                    path,
                    status: rto_graph::ChangeStatus::Added,
                }
            }));
            changed.sort_by(|a, b| a.path.cmp(&b.path));
            // The two sets are normally disjoint (tracked vs untracked), but some
            // intermediate git states can overlap — dedupe by path so the review
            // never lists a file twice. A tracked entry sorts before its untracked
            // duplicate only by chance, so prefer keeping the first of each path.
            changed.dedup_by(|a, b| a.path == b.path);
            changed
        };
    let review = review::build(&store, &changed, &report.violations)?;

    if json {
        emit_json(&review)?;
    } else {
        print_review(&review, base);
    }
    if review.has_drift() {
        std::process::exit(1);
    }
    Ok(())
}

/// Render a review report as a compact, scannable summary.
fn print_review(review: &review::ReviewReport, base: Option<&str>) {
    if review.changed_files == 0 {
        match base {
            Some(base) => println!("no changes in {base}..HEAD to review"),
            None => println!("no working-tree changes to review"),
        }
        return;
    }
    for file in &review.files {
        println!("\n{} [{}]", file.path, file.status);
        for sym in &file.symbols {
            println!("  {} {}", sym.kind, sym.name);
            let show = |label: &str, keys: &[String]| {
                if !keys.is_empty() {
                    println!("    {label}: {}", keys.join(", "));
                }
            };
            show("called by", &sym.callers);
            show("calls", &sym.callees);
            show("governed by", &sym.governed_by);
            if !sym.related.is_empty() {
                let rel: Vec<String> = sym.related.iter().map(|r| r.node.clone()).collect();
                println!("    related: {}", rel.join(", "));
            }
        }
        if !file.debt.is_empty() {
            println!("  intent-debt: {}", file.debt.len());
        }
    }
    if !review.impacted.is_empty() {
        let names: Vec<&str> = review.impacted.iter().map(|i| i.key.as_str()).collect();
        println!("\nimpacted (blast radius): {}", names.join(", "));
    }
    if review.has_drift() {
        println!("\ndrift introduced by this change:");
        for d in &review.drift {
            println!("  [{}] {}", d.kind, d.message);
        }
    } else {
        println!("\nno authored-layer drift introduced");
    }
    println!(
        "\nreviewed {} changed file(s), {} impacted node(s), {} drift item(s)",
        review.changed_files,
        review.impacted.len(),
        review.drift.len()
    );
}

/// Scaffold Roteiro in the current repository: build the initial graph, install
/// the managed git hooks (`post-checkout`/`post-merge`/`post-commit` freshness +
/// a `pre-commit` drift gate), and add the `AGENTS.md` snippet.
fn run_init(ingest: rto_graph::IngestConfig, fetch: bool, vault: bool) -> anyhow::Result<()> {
    let (repo, mut store, cache) = open_graph()?;

    let report = build_graph(&repo, &mut store, &cache, ingest, GraphSource::Committed)?;
    let nodes = store.node_count()?;
    let edges = store.edge_count()?;

    // Install where git actually looks for hooks — honours `core.hooksPath`, and
    // otherwise the common git dir (shared across worktrees).
    let hooks_dir = repo.hooks_dir();
    for name in init::MANAGED_HOOKS {
        match init::install_hook(&hooks_dir, name, fetch, vault)? {
            init::HookOutcome::Installed => println!("installed hook: {name}"),
            init::HookOutcome::Updated => println!("refreshed hook: {name}"),
            init::HookOutcome::SkippedForeign => {
                let advice = if *name == "pre-commit" {
                    "add `roteiro check` to it to gate drift-introducing commits"
                } else {
                    "add `roteiro sync --committed` to it to keep the graph fresh"
                };
                eprintln!("warning: existing non-Roteiro `{name}` hook left untouched; {advice}");
            }
        }
    }

    if let Some(workdir) = repo.workdir() {
        let path = workdir.join("AGENTS.md");
        if init::ensure_agents(&path)? {
            println!("wrote Roteiro section to {}", path.display());
        }

        // Install the agent skill (the on-demand operational guide). Always the
        // cross-tool `.agents/skills` location; also GitHub's `.github/skills`
        // when the repo already uses `.github`, since its Copilot reviewer reads
        // that path.
        let mut skill_bases = vec![workdir.join(".agents")];
        if workdir.join(".github").is_dir() {
            skill_bases.push(workdir.join(".github"));
        }
        for base in &skill_bases {
            let full = init::skill_path(base);
            let rel = full.strip_prefix(workdir).unwrap_or(&full);
            match init::install_skill(base)? {
                init::HookOutcome::Installed => println!("installed skill: {}", rel.display()),
                init::HookOutcome::Updated => println!("refreshed skill: {}", rel.display()),
                init::HookOutcome::SkippedForeign => {
                    eprintln!(
                        "warning: existing non-Roteiro `{}` left untouched",
                        rel.display()
                    );
                }
            }
        }
    }

    // With `--vault`, render the vault once now so it exists immediately (the
    // installed hooks keep it fresh thereafter).
    if vault {
        render_obsidian(ingest, None)?;
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
    build_graph(&repo, &mut store, &cache, ingest, GraphSource::Committed)?;

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
        emit_json(&report)?;
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
    build_graph(&repo, &mut store, &cache, ingest, GraphSource::Committed)?;

    let report = rto_graph::duplicates(
        &store,
        DuplicateConfig {
            min_similarity,
            limit,
        },
    )?;

    if json {
        emit_json(&report)?;
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

/// Feature-rich variant: honour `--model` by loading a local **GGUF** embedding
/// model through the shared llama.cpp engine (ADR-0003 v1.2) — no candle.
#[cfg(feature = "inference-local-models")]
fn infer_with_embedder(
    store: &rto_graph::Store,
    config: rto_graph::InferenceConfig,
    model: Option<&str>,
) -> anyhow::Result<(Vec<rto_graph::Edge>, String)> {
    use rto_graph::{HashEmbedder, Platform, infer_edges_with};

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
    // Reject non-embedding models upfront: a generative/OCR/vision model would
    // otherwise be "accepted", then fail every embed call — and because embed
    // errors degrade to empty vectors (see `LlamaEmbedder`), that would surface
    // silently as "no suggestions" rather than a clear error.
    if spec.kind != rto_graph::ModelKind::Embedding {
        anyhow::bail!(
            "model `{name}` is a {} model, not an embedding model — \
             `infer --model` needs an embedding model (see `roteiro model list`)",
            spec.kind.as_str()
        );
    }
    let variant = spec
        .variant_for(Platform::host())
        .ok_or_else(|| anyhow::anyhow!("no variant of `{name}` for this platform"))?;
    if !rto_graph::is_installed(name, variant) {
        anyhow::bail!(
            "model `{name}` is not installed — run `roteiro model pull {name}` \
             (or omit --model to use the offline default)"
        );
    }
    let embedder = LlamaEmbedder::new(name)?;
    let edges = infer_edges_with(store, config, &embedder)?;
    Ok((edges, format!("local model `{name}` (llama.cpp)")))
}

/// A GGUF embedding model behind the [`rto_graph::Embedder`] trait, backed by the
/// shared llama.cpp engine. On an embedding failure it returns an **empty**
/// vector rather than aborting the run — an empty vector shares no length with a
/// real embedding, so [`rto_graph::similarity`] scores it `0.0` against every
/// node (that node simply receives no suggestions).
#[cfg(feature = "inference-local-models")]
struct LlamaEmbedder {
    engine: rto_llama::llama::LlamaEngine,
    model: String,
}

#[cfg(feature = "inference-local-models")]
impl LlamaEmbedder {
    fn new(name: &str) -> anyhow::Result<Self> {
        let engine = rto_llama::llama::LlamaEngine::new(
            vec![rto_llama::llama::Served {
                name: name.to_owned(),
                path: rto_graph::model_dir(name).join("model.gguf"),
                mmproj: None,
            }],
            0,
        )
        .map_err(|e| anyhow::anyhow!("loading model `{name}`: {e}"))?;
        Ok(Self {
            engine,
            model: name.to_owned(),
        })
    }
}

#[cfg(feature = "inference-local-models")]
impl rto_graph::Embedder for LlamaEmbedder {
    fn embed(&self, text: &str) -> Vec<f32> {
        use rto_llama::Engine as _;
        self.engine
            .embed(&self.model, &[text.to_owned()])
            .ok()
            .and_then(|mut v| v.pop())
            .unwrap_or_default()
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
        (
            ModelKind::Audio,
            "Audio — speech transcription (`roteiro sync` with --features audio-transcribe)",
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
        rto_graph::ModelKind::Audio => {
            "roteiro sync (a build with --features audio-transcribe transcribes audio)".to_owned()
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
    build_graph(&repo, &mut store, &cache, ingest, GraphSource::Committed)?;

    let report = rto_graph::compare_codegraph(std::path::Path::new(path), &store)?;

    if json {
        emit_json(&report)?;
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

    let mut imported = rto_spec::import_lat(&files);

    // Also import `@lat:` backlinks from source comments across the committed
    // tree: each resolved reference becomes an authored `file → lat section`
    // edge folded into the same lat layer, so it persists and is re-derived with
    // the rest of the import (and pruned on re-import).
    let mut backlinks = Vec::new();
    for blob in repo.walk_blobs()? {
        let bytes = repo.read_blob(&blob.oid)?;
        let text = String::from_utf8_lossy(&bytes);
        backlinks.extend(rto_spec::scan_lat_annotations(&blob.path, &text));
    }
    let (backlink_edges, unresolved) = rto_spec::import_lat_backlinks(&files, &backlinks);
    imported.report.backlinks_resolved = backlink_edges.len();
    imported.report.backlinks_unresolved = unresolved;
    imported.facts.edges.extend(backlink_edges);

    // Build the derived + authored graph first so code links validate against it.
    build_graph(&repo, &mut store, &cache, ingest, GraphSource::Committed)?;
    let applied = store.apply_import_layer(rto_spec::LAT_REF, &imported.facts)?;

    let r = &imported.report;
    if json {
        let mut report = serde_json::to_value(r)?;
        report["edges_applied"] = serde_json::json!(applied.edges_applied);
        report["edges_pruned_stale"] = serde_json::json!(applied.edges_pruned);
        report["durable"] = serde_json::json!(true);
        emit_json(&report)?;
    } else {
        println!(
            "imported lat.md: {} file(s), {} section(s), {} link(s) \
             ({} to sections, {} to code), {} backlink(s) ({} unresolved); \
             {} edge(s) applied, {} stale pruned — persisted (durable)",
            r.files,
            r.sections,
            r.links_total,
            r.links_to_sections,
            r.links_to_code,
            r.backlinks_resolved,
            r.backlinks_unresolved,
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
    build_graph(&repo, &mut store, &cache, ingest, GraphSource::Committed)?;

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
        emit_json(&report)?;
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
    build_graph(&repo, &mut store, &cache, ingest, GraphSource::Committed)?;
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

/// Draft the scaffold's unfilled sections with a small local instruct model
/// (ADR-0004 Tier 1). Needs a generation backend (`serve` or
/// `inference-local-models`, both llama.cpp) and a pulled generative model;
/// without a model it emits the plain scaffold + a hint.
// Stage 20: `spec draft` generation runs on **llama.cpp** (the shared `rto-llama`
// engine, ADR-0006) — available whenever either the `serve` or the
// `inference-local-models` feature is on.
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

/// The generation backend label shown after drafting.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
const GEN_BACKEND: &str = "llama.cpp";

/// Max tokens generated per drafted section.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
const DRAFT_MAX_TOKENS: u32 = 800;

/// Draft each unfilled section of `scaffold` with the local generative model
/// through the shared **llama.cpp** engine (`rto-llama`, ADR-0003 v1.2) — no
/// candle. Available under either `serve` or `inference-local-models`.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
fn draft_sections(
    model: &str,
    scaffold: &str,
    topic: &str,
    ctx: &rto_spec::SpecContext,
) -> anyhow::Result<Vec<(String, String)>> {
    use rto_llama::Engine as _; // brings `.chat` into scope

    let engine = rto_llama::llama::LlamaEngine::new(
        vec![rto_llama::llama::Served {
            name: model.to_owned(),
            path: rto_graph::model_dir(model).join("model.gguf"),
            mmproj: None,
        }],
        0,
    )
    .map_err(|e| anyhow::anyhow!("starting llama.cpp: {e}"))?;

    let mut drafts = Vec::new();
    for (heading, hint) in rto_spec::draft_targets(scaffold) {
        let prompt = rto_spec::draft_prompt(topic, ctx, &heading, &hint);
        let completion = engine
            .chat(&rto_llama::ChatRequest {
                model: model.to_owned(),
                messages: vec![rto_llama::Message {
                    role: "user".to_owned(),
                    content: prompt,
                }],
                images: vec![],
                audio: vec![],
                temperature: 0.0,
                max_tokens: DRAFT_MAX_TOKENS,
            })
            .map_err(|e| anyhow::anyhow!("drafting `{heading}`: {e}"))?;
        // A reasoning-capable GGUF (Qwen3, DeepSeek-R1, …) emits a
        // `<think>…</think>` block before its answer; keep only the answer so the
        // reasoning never lands in the drafted document.
        let prose = strip_thinking(&completion.content);
        if !prose.trim().is_empty() {
            drafts.push((heading, prose));
        }
    }
    Ok(drafts)
}

/// Drop a leading `<think>…</think>` reasoning block, returning the answer that
/// follows it. Text with no closing `</think>` is returned unchanged.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
fn strip_thinking(text: &str) -> String {
    match text.find("</think>") {
        Some(end) => text[end + "</think>".len()..].trim_start().to_owned(),
        None => text.to_owned(),
    }
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
         or `--features inference-local-models` (both llama.cpp), then \
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
    build_graph(&repo, &mut store, &cache, ingest, GraphSource::Committed)?;

    let ctx = rto_spec::context(&store, topic, limit)?;
    if json {
        emit_json(&ctx)?;
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
    build_graph(&repo, &mut store, &cache, ingest, GraphSource::Committed)?;

    match (key, kind) {
        (Some(key), _) => {
            let Some(ex) = explain(&store, &key)? else {
                anyhow::bail!(
                    "no node with key `{key}` (try `roteiro query --kind <kind>` to list nodes)"
                );
            };
            if json {
                emit_json(&ex)?;
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
                emit_json(&listing)?;
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

/// Search the graph by text and print ranked hits (highest score first). A
/// read-only report: it exits zero even when nothing matches, keeping stdout
/// empty (or an empty JSON array) so it composes in scripts.
fn run_search(
    ingest: rto_graph::IngestConfig,
    query: &str,
    limit: usize,
    json: bool,
) -> anyhow::Result<()> {
    let (repo, mut store, cache) = open_graph()?;
    build_graph(&repo, &mut store, &cache, ingest, GraphSource::Committed)?;

    let hits = rto_graph::search(&store, query, limit)?;
    if json {
        emit_json(&hits)?;
    } else if hits.is_empty() {
        // Keep stdout empty on a miss; report to stderr.
        eprintln!("no matches for `{query}`");
    } else {
        for hit in &hits {
            println!("  {:>4}  {:<8}  {}", hit.score, hit.node.kind, hit.node.key);
        }
        println!("{} hit(s)", hits.len());
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
    build_graph(&repo, &mut store, &cache, ingest, GraphSource::Committed)?;

    if refresh {
        let report = refresh_contexts(&store)?;
        if json {
            emit_json(&report)?;
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
        emit_json(&ctx)?;
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

/// List intent-debt markers in the graph, grouped
/// by category. A report, not a gate: it always exits zero.
fn run_debt(
    ingest: rto_graph::IngestConfig,
    kinds: &[String],
    json: bool,
    debt_ignore: &[String],
) -> anyhow::Result<()> {
    let (repo, mut store, cache) = open_graph()?;
    build_graph(&repo, &mut store, &cache, ingest, GraphSource::Committed)?;

    let report = rto_graph::debt(&store, kinds, debt_ignore)?;
    if json {
        emit_json(&report)?;
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
    build_graph(&repo, &mut store, &cache, ingest, GraphSource::Committed)?;

    let result = rto_graph::path(&store, from, to)?;
    if json {
        emit_json(&result)?;
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

/// The outcome of resolving one authored cross-repo link (ADR-0009).
#[derive(serde::Serialize)]
struct LinkResult {
    /// The repo (project) the link was declared in.
    repo: String,
    /// The declared local anchor (`from`), if any.
    from: Option<String>,
    /// The project-qualified target (`to`).
    to: String,
    /// The relationship label.
    kind: String,
    /// `ok` (resolved) or `drift` (target unresolved).
    status: &'static str,
    /// For a resolved link: the target node's kind and name; for drift: why.
    detail: String,
}

/// Project (display) names for `paths`, matching how [`rto_graph::Workspace`]
/// names them — the repo directory name, with `-2`/`-3`/… suffixes disambiguating
/// collisions — so a report's `repo` label (and the `<project>` used in a link
/// key) never diverge when two repos share a directory name.
fn workspace_project_names(paths: &[std::path::PathBuf]) -> Vec<(&std::path::PathBuf, String)> {
    use std::collections::HashMap;
    let mut counts: HashMap<String, usize> = HashMap::new();
    paths
        .iter()
        .map(|p| {
            let base = p
                .file_name()
                .map_or_else(|| "repo".to_owned(), |s| s.to_string_lossy().into_owned());
            let n = counts.entry(base.clone()).or_insert(0);
            *n += 1;
            let name = if *n == 1 { base } else { format!("{base}-{n}") };
            (p, name)
        })
        .collect()
}

/// Verify a workspace's authored cross-repo links (ADR-0009). For every repo in
/// the workspace (the cwd repo plus any `--workspace`/`[workspace]` roots), read
/// its `[[links]]` and resolve each project-qualified `to` against the other
/// repos' graphs. A target that no longer resolves is **drift** — the cross-repo
/// form of `roteiro check`. Exits non-zero if any link drifts.
fn run_links(cfg: &config::Config, cli_roots: &[String], json: bool) -> anyhow::Result<()> {
    use std::collections::BTreeSet;

    // Repos in scope: the workspace roots/repos, plus the current repo so
    // `roteiro links` run inside a spoke resolves against its siblings.
    let mut paths: BTreeSet<std::path::PathBuf> =
        collect_workspace_repo_paths(&cfg.workspace, cli_roots)?
            .into_iter()
            .collect();
    if let Ok(cwd) = std::env::current_dir()
        && let Ok(repo) = rto_graph::Repo::discover(&cwd)
        && let Some(wd) = repo.workdir()
    {
        paths.insert(wd.to_path_buf());
    }
    if paths.is_empty() {
        anyhow::bail!(
            "no repos in scope — run inside a repo, pass `--workspace <root>`, or set \
             `[workspace]` in roteiro.toml"
        );
    }
    let paths: Vec<std::path::PathBuf> = paths.into_iter().collect();
    let workspace = rto_graph::Workspace::from_repo_paths(&paths)?;

    // Collect each repo's declared links from its own config.
    let mut results: Vec<LinkResult> = Vec::new();
    for (path, repo_name) in workspace_project_names(&paths) {
        let repo_cfg = config::load(path)?.effective;
        for link in &repo_cfg.links {
            let kind = link.kind.clone().unwrap_or_else(|| "references".to_owned());
            let (status, detail) = match workspace.resolve_qualified(&link.to) {
                Ok(Some(node)) => ("ok", format!("{} {}", node.kind.as_str(), node.name)),
                Ok(None) => ("drift", "no such node in the target project".to_owned()),
                Err(e) => ("drift", e.to_string()),
            };
            results.push(LinkResult {
                repo: repo_name.clone(),
                from: link.from.clone(),
                to: link.to.clone(),
                kind,
                status,
                detail,
            });
        }
    }

    let drift = results.iter().filter(|r| r.status == "drift").count();
    if json {
        emit_json(&results)?;
    } else if results.is_empty() {
        println!(
            "no cross-repo links declared across {} repo(s) (add `[[links]]` to a repo's roteiro.toml)",
            paths.len()
        );
    } else {
        for r in &results {
            let marker = if r.status == "ok" { "ok   " } else { "DRIFT" };
            println!("  [{marker}] {} → {}  ({})", r.repo, r.to, r.detail);
        }
        println!(
            "{} link(s) across {} repo(s): {} ok, {} drift",
            results.len(),
            paths.len(),
            results.len() - drift,
            drift
        );
    }

    if drift > 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// One spoke project's inferred config correspondences with the hub (ADR-0009).
#[derive(serde::Serialize)]
struct InferredRepo {
    /// The spoke project (repo dir name).
    repo: String,
    /// Config keys that matched a hub key.
    matches: Vec<infer_links::KeyMatch>,
    /// Config keys with no hub counterpart — likely drift.
    orphans: Vec<infer_links::ConfigKey>,
    /// The hub rev this spoke resolved against, when it pins one (`--pinned`,
    /// ADR-0009 step 8b); `None` means the hub's `HEAD`.
    #[serde(skip_serializing_if = "Option::is_none")]
    hub_rev: Option<String>,
    /// Where the pin came from (e.g. `submodule vendor/app`), when auto-detected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pin_via: Option<String>,
}

/// The `graph.db` path for the repo at `path` (`<repo>/.git/roteiro/graph.db`).
fn graph_db_path(path: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    let repo = rto_graph::Repo::discover(path)?;
    Ok(repo.git_dir().join("roteiro").join("graph.db"))
}

/// The config keys of every in-scope repo, read **from each repo's graph** (its
/// `config_key` nodes), keyed by project name (dir name, `-2`/`-3` on collision),
/// alongside a name→path map for persistence and the names of repos with no graph
/// yet (noted, not fatal). Reading from the graph — not re-parsing files — keeps
/// the matcher and the stored nodes in lock-step (ADR-0009 feature 2b).
type WorkspaceConfigKeys = (
    std::collections::BTreeMap<String, Vec<infer_links::ConfigKey>>,
    std::collections::BTreeMap<String, std::path::PathBuf>,
    Vec<String>,
);
fn collect_workspace_config_keys(
    paths: &[std::path::PathBuf],
) -> anyhow::Result<WorkspaceConfigKeys> {
    let mut by_project = std::collections::BTreeMap::new();
    let mut project_paths = std::collections::BTreeMap::new();
    let mut unsynced = Vec::new();
    for (path, name) in workspace_project_names(paths) {
        project_paths.insert(name.clone(), path.clone());
        let db = graph_db_path(path)?;
        if !db.exists() {
            unsynced.push(name);
            continue;
        }
        let keys = rto_graph::Store::open(&db)?.config_keys()?;
        if !keys.is_empty() {
            by_project.insert(name, keys);
        }
    }
    Ok((by_project, project_paths, unsynced))
}

/// A ready cross-repo inference over the workspace, or a reason there's nothing to
/// show (an informational no-op the caller reports without failing).
enum InferScan {
    /// Nothing to infer/show, with a human reason (empty or single-repo workspace).
    Nothing(String),
    /// A hub was picked and every spoke matched against it.
    Ready(InferReady),
}

/// The result of a successful workspace scan: the hub, each spoke's matches, and
/// the raw per-project config keys (for values) and paths (for persistence).
struct InferReady {
    hub_name: String,
    /// The pinned hub rev the match was resolved against, if any (ADR-0009 step 8).
    hub_rev: Option<String>,
    report: Vec<InferredRepo>,
    by_project: std::collections::BTreeMap<String, Vec<infer_links::ConfigKey>>,
    project_paths: std::collections::BTreeMap<String, std::path::PathBuf>,
}

/// How to source the hub's config keys (ADR-0009 step 8): its `HEAD` graph (`rev`
/// `None`, `auto` false), one **explicit pinned version** for all spokes (`rev`
/// set, `--hub-rev`), or **each spoke's own pin** auto-detected (`auto`,
/// `--pinned`). Extracted in-memory with `ingest`.
#[derive(Clone, Copy)]
struct PinnedHub<'a> {
    rev: Option<&'a str>,
    auto: bool,
    ingest: rto_graph::IngestConfig,
}

/// The config keys of the repo at `repo_path` **as of `rev`** (any git rev), read
/// from an ephemeral in-memory graph extracted at that point via
/// [`rto_graph::sync_tree`] — content-addressed, so unchanged blobs are cache hits.
/// Backs version-pin resolution (ADR-0009 step 8).
fn config_keys_at_rev(
    repo_path: &std::path::Path,
    rev: &str,
    ingest: rto_graph::IngestConfig,
) -> anyhow::Result<Vec<infer_links::ConfigKey>> {
    let repo = rto_graph::Repo::discover(repo_path)?;
    let cache = rto_graph::ObjectCache::open(repo.common_dir().join("roteiro").join("objects"))?;
    let reg = rto_graph::Registry::new(ingest);
    config_keys_at_rev_with(&repo, &cache, reg, rev)
}

/// The config keys of `repo` at `rev`, reusing a caller-owned repo and object cache
/// — the expensive-to-open resources — so resolving many pins under `--pinned` opens
/// them once and pays only the (cache-aware) `sync_tree` per distinct rev. (The
/// `Registry` is a `Copy` config, so it is passed by value.)
fn config_keys_at_rev_with(
    repo: &rto_graph::Repo,
    cache: &rto_graph::ObjectCache,
    reg: rto_graph::Registry,
    rev: &str,
) -> anyhow::Result<Vec<infer_links::ConfigKey>> {
    // Prefer a pre-published graph artifact for this version (ADR-0009 step 8c): it
    // resolves even when the pinned commit's blobs aren't present locally (a shallow
    // clone) and skips extraction entirely.
    if let Some(keys) = config_keys_from_artifact(repo, rev)? {
        return Ok(keys);
    }
    let mut store = rto_graph::Store::open_in_memory()?;
    rto_graph::sync_tree(&mut store, repo, cache, &reg, rev)?;
    Ok(store.config_keys()?)
}

/// Config keys from a pre-published graph artifact for `rev`, if one exists at the
/// conventional location (`<repo>/.git/roteiro/artifacts/<treeid>.json`) and its
/// recorded tree matches — a hub's CI can `roteiro export` there per release.
/// `None` when there is no usable artifact (the caller re-extracts).
fn config_keys_from_artifact(
    repo: &rto_graph::Repo,
    rev: &str,
) -> anyhow::Result<Option<Vec<infer_links::ConfigKey>>> {
    let tree = repo.tree_id_at(rev)?;
    let path = repo
        .common_dir()
        .join("roteiro")
        .join("artifacts")
        .join(format!("{tree}.json"));
    // A missing, unreadable, corrupt, or tree-mismatched artifact is "not usable" —
    // return `None` so the caller falls back to re-extraction rather than aborting.
    let Ok(json) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    let Ok(artifact) = rto_graph::GraphArtifact::from_json(&json) else {
        return Ok(None);
    };
    if artifact.tree.as_deref() != Some(tree.as_str()) {
        return Ok(None);
    }
    let mut store = rto_graph::Store::open_in_memory()?;
    if store.rebuild(&artifact.facts, None).is_err() {
        return Ok(None);
    }
    Ok(Some(store.config_keys()?))
}

/// Scan the in-scope repos (workspace roots + the cwd repo), read each one's config
/// keys **from its graph**, pick the hub (named, else the repo with the most keys),
/// and match every spoke against it. With `pin.rev`, the hub's keys come from that
/// pinned version instead of its `HEAD` (ADR-0009 step 8). Shared by `--infer` and
/// `--matrix`. Bails only on a bad `--hub` (or an unresolvable pin); an empty or
/// single-repo workspace is a [`InferScan::Nothing`].
fn scan_workspace_infer(
    cfg: &config::Config,
    cli_roots: &[String],
    hub: Option<&str>,
    pin: PinnedHub<'_>,
) -> anyhow::Result<InferScan> {
    // Repos in scope (same set as `roteiro links`): workspace roots + the cwd repo.
    let mut path_set: std::collections::BTreeSet<std::path::PathBuf> =
        collect_workspace_repo_paths(&cfg.workspace, cli_roots)?
            .into_iter()
            .collect();
    if let Ok(cwd) = std::env::current_dir()
        && let Ok(repo) = rto_graph::Repo::discover(&cwd)
        && let Some(wd) = repo.workdir()
    {
        path_set.insert(wd.to_path_buf());
    }
    let paths: Vec<std::path::PathBuf> = path_set.into_iter().collect();
    if paths.is_empty() {
        return Ok(InferScan::Nothing(
            "no repos in scope; run inside a repo, pass `--workspace <root>`, or set `[workspace]`"
                .to_owned(),
        ));
    }

    let (mut by_project, project_paths, unsynced) = collect_workspace_config_keys(&paths)?;
    if by_project.len() < 2 {
        let hint = if unsynced.is_empty() {
            String::new()
        } else {
            format!(
                " ({} repo(s) not synced: {})",
                unsynced.len(),
                unsynced.join(", ")
            )
        };
        return Ok(InferScan::Nothing(format!(
            "need at least two synced repos with config files (TOML / JSON / .env) — found {}{hint}",
            by_project.len()
        )));
    }

    // Pick the hub: named, else the repo with the most config keys.
    let hub_name = match hub {
        Some(h) => {
            if !by_project.contains_key(h) {
                anyhow::bail!(
                    "no repo named `{h}` with config (have: {})",
                    by_project.keys().cloned().collect::<Vec<_>>().join(", ")
                );
            }
            h.to_owned()
        }
        None => by_project
            .iter()
            .max_by_key(|(_, v)| v.len())
            .map(|(k, _)| k.clone())
            .expect("non-empty"),
    };

    // Version-pin resolution: swap the hub's HEAD keys for those of the pinned
    // version the spokes actually deploy (ADR-0009 step 8), extracted in-memory.
    if let Some(rev) = pin.rev {
        let hub_path = project_paths
            .get(&hub_name)
            .ok_or_else(|| anyhow::anyhow!("no path for hub `{hub_name}`"))?;
        let keys = config_keys_at_rev(hub_path, rev, pin.ingest)
            .map_err(|e| anyhow::anyhow!("resolving hub `{hub_name}` at `{rev}`: {e}"))?;
        by_project.insert(hub_name.clone(), keys);
    }

    let report = resolve_infer_report(&by_project, &hub_name, &project_paths, pin)?;

    Ok(InferScan::Ready(InferReady {
        hub_name,
        hub_rev: pin.rev.map(str::to_owned),
        report,
        by_project,
        project_paths,
    }))
}

/// Open a spoke's graph and detect the hub version it pins (ADR-0009 step 8b),
/// or `None` if it is unsynced or pins nothing recognisable to the hub.
fn detect_spoke_pin(
    spoke_path: &std::path::Path,
    hub_dir: &str,
    hub_origin: Option<&str>,
    hub_repo: &rto_graph::Repo,
) -> anyhow::Result<Option<pins::SpokePin>> {
    let db = graph_db_path(spoke_path)?;
    if !db.exists() {
        return Ok(None);
    }
    let store = rto_graph::Store::open(&db)?;
    // The spoke's `[pins]` config supplies image/Helm → ref templates (ADR-0009 8c).
    // A malformed config is a hard error (the config contract), not silently ignored.
    let templates = config::load(spoke_path)?.effective.pins;
    pins::detect(&store, hub_dir, hub_origin, hub_repo, &templates)
}

/// Match every spoke against the right hub key set: the hub base (its `HEAD`, or the
/// explicit `--hub-rev` already swapped into `by_project`), or — under `--pinned`
/// (`pin.auto`) — the hub version each spoke *itself* pins, extracted per rev and
/// cached (ADR-0009 step 8b). Records per-spoke which pin, if any, was used.
fn resolve_infer_report(
    by_project: &std::collections::BTreeMap<String, Vec<infer_links::ConfigKey>>,
    hub_name: &str,
    project_paths: &std::collections::BTreeMap<String, std::path::PathBuf>,
    pin: PinnedHub<'_>,
) -> anyhow::Result<Vec<InferredRepo>> {
    let hub_base = by_project[hub_name].as_slice();
    // Under `--pinned`, set the hub up **once** — repo, object cache, extractor, its
    // real directory name (not the `-2`-suffixed workspace label) and origin — so
    // per-rev work is just the cache-aware `sync_tree`.
    let hub = if pin.auto {
        let hub_path = project_paths
            .get(hub_name)
            .ok_or_else(|| anyhow::anyhow!("no path for hub `{hub_name}`"))?;
        let repo = rto_graph::Repo::discover(hub_path)?;
        let cache =
            rto_graph::ObjectCache::open(repo.common_dir().join("roteiro").join("objects"))?;
        let reg = rto_graph::Registry::new(pin.ingest);
        let dir = hub_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(hub_name)
            .to_owned();
        let origin = repo.origin_url();
        Some((repo, cache, reg, dir, origin))
    } else {
        None
    };

    let mut rev_cache: std::collections::BTreeMap<String, Vec<infer_links::ConfigKey>> =
        std::collections::BTreeMap::new();
    let mut report = Vec::new();
    for (name, keys) in by_project.iter().filter(|(n, _)| n.as_str() != hub_name) {
        let (hub_rev, pin_via) = match &hub {
            // `--pinned`: resolve against the version this spoke pins, if any.
            Some((repo, cache, reg, dir, origin)) => {
                match detect_spoke_pin(&project_paths[name], dir, origin.as_deref(), repo)? {
                    Some(p) => {
                        if !rev_cache.contains_key(&p.rev) {
                            let k = config_keys_at_rev_with(repo, cache, *reg, &p.rev).map_err(
                                |e| {
                                    anyhow::anyhow!(
                                        "resolving hub `{hub_name}` at `{}`: {e}",
                                        p.rev
                                    )
                                },
                            )?;
                            rev_cache.insert(p.rev.clone(), k);
                        }
                        (Some(p.rev), Some(p.via))
                    }
                    None => (None, None),
                }
            }
            // Global: HEAD, or the explicit `--hub-rev` already in `hub_base`.
            None => (pin.rev.map(str::to_owned), None),
        };
        let hub_keys: &[infer_links::ConfigKey] = match &hub_rev {
            Some(rev) if pin.auto => rev_cache[rev].as_slice(),
            _ => hub_base,
        };
        let (matches, orphans) = infer_links::match_against_hub(keys, hub_keys);
        report.push(InferredRepo {
            repo: name.clone(),
            matches,
            orphans,
            hub_rev,
            pin_via,
        });
    }
    Ok(report)
}

/// `roteiro links --infer`: match each workspace repo's config keys (TOML / JSON
/// / `.env`) against a hub repo's, surfacing correspondences with no
/// hand-authored links and flagging orphan keys (drift candidates). Config keys
/// are read **from each repo's graph** (the `config_key` nodes a `sync` extracts),
/// so a repo must have been synced. Informational — always exits zero (these are
/// confidence-scored suggestions, not a gate).
///
/// With `write`, the correspondences are also persisted into each spoke's graph as
/// an `inferred` cross-repo import layer (external-ref target + `references` edge,
/// ADR-0009 feature 2b) that survives later syncs.
fn run_links_infer(
    cfg: &config::Config,
    cli_roots: &[String],
    hub: Option<&str>,
    pin: PinnedHub<'_>,
    write: bool,
    json: bool,
) -> anyhow::Result<()> {
    // Having nothing to infer is a **successful no-op** (exit 0) — `--infer` is
    // informational, so a CI script can run it opportunistically in a single repo
    // without failing — but still say why.
    let ready = match scan_workspace_infer(cfg, cli_roots, hub, pin)? {
        InferScan::Nothing(reason) => {
            if json {
                emit_json(&serde_json::json!({ "hub": null, "spokes": [], "note": reason }))?;
            } else {
                eprintln!("nothing to infer — {reason}");
            }
            return Ok(());
        }
        InferScan::Ready(r) => r,
    };
    let hub_key_count = ready.by_project[&ready.hub_name].len();

    // Optionally persist the correspondences into each spoke's graph as a durable
    // `inferred` cross-repo import layer (ADR-0009 feature 2b).
    let written = if write {
        persist_inferred_links(&ready.hub_name, &ready.report, &ready.project_paths)?
    } else {
        0
    };

    if json {
        emit_json(&serde_json::json!({
            "hub": ready.hub_name,
            "hub_rev": ready.hub_rev,
            "spokes": ready.report,
            "written": written,
        }))?;
    } else {
        if let Some(rev) = &ready.hub_rev {
            println!(
                "resolved against {} @ {rev} (pinned version)",
                ready.hub_name
            );
        }
        print_infer_report(&ready.report, &ready.hub_name, hub_key_count);
        if write {
            println!("\npersisted {written} inferred cross-repo edge(s) into spoke graphs");
        }
    }
    Ok(())
}

/// `roteiro links --matrix`: render the cross-repo **config override matrix + drift**
/// view (ADR-0009 step 7). Reuses the `--infer` scan, then pivots the per-spoke
/// matches into a hub-key × spoke grid — as a text table, `--json`, or a
/// self-contained HTML page (`--html`, the "render web-graph" output).
fn run_links_matrix(
    cfg: &config::Config,
    cli_roots: &[String],
    hub: Option<&str>,
    pin: PinnedHub<'_>,
    html: bool,
    out: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    let ready = match scan_workspace_infer(cfg, cli_roots, hub, pin)? {
        InferScan::Nothing(reason) => {
            if json {
                emit_json(
                    &serde_json::json!({ "hub": null, "rows": [], "drift": [], "note": reason }),
                )?;
            } else {
                eprintln!("nothing to show — {reason}");
            }
            return Ok(());
        }
        InferScan::Ready(r) => r,
    };

    // Hub key → value, so the matrix can flag which overrides actually differ.
    let hub_values: std::collections::BTreeMap<String, String> = ready.by_project[&ready.hub_name]
        .iter()
        .map(|c| (c.key.clone(), c.value.clone()))
        .collect();

    // Turn each spoke's matches/orphans into matrix inputs, looking its own values
    // back up from its config keys.
    let spokes = ready
        .report
        .iter()
        .map(|rep| {
            // Key values by (file, key): a `config_key` node is per-(file, key), so
            // the same key in two files must not collide to an arbitrary value.
            let vals: std::collections::HashMap<(&str, &str), &str> = ready.by_project[&rep.repo]
                .iter()
                .map(|c| ((c.file.as_str(), c.key.as_str()), c.value.as_str()))
                .collect();
            overview::SpokeInput {
                name: rep.repo.clone(),
                matches: rep
                    .matches
                    .iter()
                    .map(|m| overview::MatchInput {
                        hub_key: m.hub_key.clone(),
                        spoke_key: m.spoke_key.clone(),
                        spoke_value: vals
                            .get(&(m.spoke_file.as_str(), m.spoke_key.as_str()))
                            .copied()
                            .unwrap_or("")
                            .to_owned(),
                        confidence: m.confidence,
                    })
                    .collect(),
                orphans: rep
                    .orphans
                    .iter()
                    .map(|o| (o.key.clone(), o.value.clone()))
                    .collect(),
            }
        })
        .collect();

    // When resolving a pinned version, label the hub with its rev so every output
    // (text header, HTML title, JSON `hub`) says which version drift was measured
    // against.
    let hub_label = match &ready.hub_rev {
        Some(rev) => format!("{} @ {rev}", ready.hub_name),
        None => ready.hub_name.clone(),
    };
    let matrix = overview::build(&hub_label, &hub_values, spokes);

    if json {
        emit_json(&matrix)?;
    } else if html {
        let page = overview::render_html(&matrix);
        let out = out.unwrap_or_else(|| "roteiro-overview.html".to_owned());
        if out == "-" {
            println!("{page}");
        } else {
            std::fs::write(&out, page)?;
            eprintln!(
                "wrote override matrix ({} row(s), {} drift) → {out}",
                matrix.rows.len(),
                matrix.drift.len()
            );
        }
    } else {
        print!("{}", overview::render_text(&matrix));
    }
    Ok(())
}

/// Persist each spoke's inferred correspondences into that spoke's graph as an
/// `inferred` import layer (external-ref target nodes + `references` edges, under
/// [`rto_graph::LINKS_REF`]), returning how many edges were applied. Re-applying
/// is authoritative — the layer replaces any prior inferred links — and the layer
/// survives later syncs (dangling edges pruned when a config key is removed).
fn persist_inferred_links(
    hub_name: &str,
    report: &[InferredRepo],
    project_paths: &std::collections::BTreeMap<String, std::path::PathBuf>,
) -> anyhow::Result<usize> {
    let mut written = 0usize;
    for spoke in report {
        // Apply the layer for *every* spoke, even with no matches: `apply_import_layer`
        // is what clears this ref's prior edges, so a spoke whose matches have since
        // disappeared must still be re-applied (with an empty layer) to remove its
        // stale inferred links — otherwise the "authoritative re-apply" would leak them.
        let facts = infer_links::link_facts(hub_name, &spoke.matches);
        let path = project_paths
            .get(&spoke.repo)
            .ok_or_else(|| anyhow::anyhow!("no path for spoke `{}`", spoke.repo))?;
        let db = graph_db_path(path)?;
        if !db.exists() {
            continue; // unsynced spoke: nothing to attach edges to
        }
        let mut store = rto_graph::Store::open(&db)?;
        let applied = store.apply_import_layer(rto_graph::LINKS_REF, &facts)?;
        written += applied.edges_applied;
    }
    Ok(written)
}

/// Human-readable rendering of the inferred cross-repo config report.
/// Abbreviate a 40-hex commit sha to 10 chars; leave short refs (tags) as-is.
fn short_rev(rev: &str) -> &str {
    if rev.len() == 40 && rev.bytes().all(|b| b.is_ascii_hexdigit()) {
        &rev[..10]
    } else {
        rev
    }
}

fn print_infer_report(report: &[InferredRepo], hub_name: &str, hub_keys: usize) {
    println!("inferred config links (hub: {hub_name}, {hub_keys} keys)");
    let (mut nm, mut no) = (0usize, 0usize);
    for r in report {
        // Under `--pinned`, say which hub version this spoke resolved against.
        let pin = match (&r.hub_rev, &r.pin_via) {
            (Some(rev), Some(via)) => format!("  @ {} (via {via})", short_rev(rev)),
            (Some(rev), None) => format!("  @ {}", short_rev(rev)),
            _ => String::new(),
        };
        println!(
            "\n  {} — {} match(es), {} orphan(s){pin}",
            r.repo,
            r.matches.len(),
            r.orphans.len()
        );
        for m in &r.matches {
            println!(
                "    {:<28} ~ {hub_name}::{:<24} ({:.2})",
                m.spoke_key, m.hub_key, m.confidence
            );
            nm += 1;
        }
        for o in &r.orphans {
            println!(
                "    {:<28} orphan — no {hub_name} counterpart (drift?)",
                o.key
            );
            no += 1;
        }
    }
    println!(
        "\n{nm} match(es), {no} orphan(s) across {} spoke(s)",
        report.len()
    );
}

/// Assemble the full graph and write it as a portable JSON artifact.
fn run_export(ingest: rto_graph::IngestConfig, out: Option<String>) -> anyhow::Result<()> {
    use rto_graph::GraphArtifact;

    let (repo, mut store, cache) = open_graph()?;
    build_graph(&repo, &mut store, &cache, ingest, GraphSource::Committed)?;
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
/// fresh clone (or a `post-merge`/`post-checkout` hook) obtain a ready-made graph
/// without re-extraction. Unless `force`, the artifact's tree must match the
/// working `HEAD` — so a CI artifact fetched for a different commit is refused
/// (non-zero exit) and the caller rebuilds instead. A matching load also sets the
/// sync state to that tree, so a following `sync` no-ops.
fn run_load(file: &str, force: bool) -> anyhow::Result<()> {
    use rto_graph::{GraphArtifact, Repo, Store};

    let json = if file == "-" {
        std::io::read_to_string(std::io::stdin())?
    } else {
        std::fs::read_to_string(file)?
    };
    let artifact = GraphArtifact::from_json(&json)?;

    let cwd = std::env::current_dir()?;
    let repo = Repo::discover(&cwd)?;

    // Refuse an artifact that isn't provably for the current HEAD, so a fetched
    // artifact never installs a graph that doesn't match the checkout. A missing
    // tree can't be verified, so it is refused too (both overridable with --force).
    if !force {
        let head = repo.head_tree_id()?;
        let short = |t: &str| t[..t.len().min(12)].to_owned();
        match artifact.tree.as_deref() {
            Some(tree) if tree == head => {}
            Some(tree) => anyhow::bail!(
                "artifact tree {} does not match HEAD tree {} — refusing to load a mismatched graph (pass --force to override, or run `roteiro sync`)",
                short(tree),
                short(&head)
            ),
            None => anyhow::bail!(
                "artifact records no tree, so it cannot be verified against HEAD — pass --force to load it anyway, or run `roteiro sync`"
            ),
        }
    }

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

/// The parsed `serve` flags (from the clap `Command::Serve` arm), bundled so the
/// dispatch stays a single struct rather than a long argument list.
#[cfg(any(feature = "mcp", feature = "serve"))]
// Several fields (addr/TLS/mcp) are only read on the `serve` path; in an
// mcp-only build the model endpoint is a stub, so they are legitimately unused.
#[cfg_attr(not(feature = "serve"), allow(dead_code))]
struct ServeOptions {
    /// Serve the OpenAI-compatible `/v1` model endpoint (`--models`).
    models: bool,
    /// MCP-only HTTP bind address (`--http`), when not serving `--models`.
    http: Option<String>,
    /// Model-server bind address (`--addr`).
    addr: Option<String>,
    /// In-process TLS certificate chain (`--tls-cert`).
    tls_cert: Option<String>,
    /// In-process TLS private key (`--tls-key`).
    tls_key: Option<String>,
    /// With `--models`, also mount `/mcp` on the same port (`--mcp`).
    mcp: bool,
}

/// Dispatch `roteiro serve`: the OpenAI-compatible model endpoint (`--models`,
/// ADR-0006) or the MCP graph server — optionally **both on one port** (`--models
/// --mcp`, ADR-0008). Each backend is feature-gated; a build lacking the relevant
/// feature reports how to enable it.
#[cfg(any(feature = "mcp", feature = "serve"))]
fn run_serve(
    ingest: rto_graph::IngestConfig,
    cfg: &config::Config,
    opts: ServeOptions,
    workspace_roots: &[String],
    sync_on_access: bool,
) -> anyhow::Result<()> {
    use std::sync::Arc;
    let repo_paths = collect_workspace_repo_paths(&cfg.workspace, workspace_roots)?;
    let workspace: Arc<rto_graph::Workspace> = if repo_paths.is_empty() {
        // Single-repo serve: build the cwd repo's graph now and host it alone.
        let (repo, mut store, cache) = open_graph()?;
        build_graph(&repo, &mut store, &cache, ingest, GraphSource::Committed)?;
        let name = repo
            .workdir()
            .and_then(std::path::Path::file_name)
            .map_or_else(|| "repo".to_owned(), |s| s.to_string_lossy().into_owned());
        Arc::new(rto_graph::Workspace::single(name, store))
    } else {
        // Workspace serve: open existing graphs on demand and allow SIGHUP to
        // reload the registry. By default no sync (each repo's hooks keep it
        // fresh, ADR-0008); with --sync-on-access, (re)build a project's graph on
        // first touch.
        let mut ws = rto_graph::Workspace::from_repo_paths(&repo_paths)?;
        if sync_on_access {
            ws = ws.with_on_open(Arc::new(move |db: &std::path::Path| {
                sync_project_graph(db, ingest).map_err(|e| e.to_string())
            }));
        }
        let ws = Arc::new(ws);
        let names = ws.names();
        eprintln!(
            "roteiro workspace: {} project(s){} — {}",
            names.len(),
            if sync_on_access {
                ", sync-on-access"
            } else {
                ""
            },
            names.join(", ")
        );
        install_workspace_reload(&ws, cfg.workspace.clone(), workspace_roots.to_vec());
        ws
    };
    if opts.models {
        serve_models_endpoint(cfg, workspace, &opts)
    } else {
        serve_mcp(workspace, opts.http)
    }
}

/// Repo paths a workspace `serve` hosts: everything under the CLI `--workspace`
/// roots and `[workspace]` config `roots` (each scanned), plus any explicit
/// `repos`. Empty ⇒ single-repo serving of the current directory's repo. Shared
/// by `serve` startup, SIGHUP reload, and `roteiro links` (ADR-0009).
fn collect_workspace_repo_paths(
    ws_cfg: &config::WorkspaceConfig,
    cli_roots: &[String],
) -> anyhow::Result<Vec<std::path::PathBuf>> {
    let mut repo_paths: Vec<std::path::PathBuf> = Vec::new();
    let roots = cli_roots
        .iter()
        .map(String::as_str)
        .chain(ws_cfg.roots.iter().flatten().map(String::as_str));
    for root in roots {
        repo_paths.extend(discover_repos_under(std::path::Path::new(root))?);
    }
    for repo in ws_cfg.repos.iter().flatten() {
        repo_paths.push(std::path::PathBuf::from(repo));
    }
    Ok(repo_paths)
}

/// `serve --sync-on-access` hook: (re)build the graph for the repo whose store
/// is `graph_db` (`<repo>/.git/roteiro/graph.db`), before it is first served.
/// Rebuilds from the committed tree, matching how the freshness hooks sync.
#[cfg(any(feature = "mcp", feature = "serve"))]
fn sync_project_graph(
    graph_db: &std::path::Path,
    ingest: rto_graph::IngestConfig,
) -> anyhow::Result<()> {
    use rto_graph::{ObjectCache, Repo, Store};
    // graph.db → roteiro → .git → repo directory (three parents up).
    let repo_dir = graph_db
        .parent()
        .and_then(std::path::Path::parent)
        .and_then(std::path::Path::parent)
        .ok_or_else(|| anyhow::anyhow!("unexpected graph.db path: {}", graph_db.display()))?;
    let repo = Repo::discover(repo_dir)?;
    let store_dir = repo.git_dir().join("roteiro");
    std::fs::create_dir_all(&store_dir)?;
    let mut store = Store::open(&store_dir.join("graph.db"))?;
    let cache = ObjectCache::open(repo.common_dir().join("roteiro").join("objects"))?;
    build_graph(&repo, &mut store, &cache, ingest, GraphSource::Committed)?;
    Ok(())
}

/// Install a SIGHUP handler that re-scans the workspace roots and reloads the
/// registry in place, so a running server picks up added/removed repos without a
/// restart (ADR-0008). Runs on a dedicated thread with its own tokio runtime,
/// independent of the serve runtime; reload is thread-safe (the `Workspace`
/// serialises its own state). Best-effort: if SIGHUP cannot be registered, the
/// server still runs, just without live reload.
#[cfg(all(unix, any(feature = "mcp", feature = "serve")))]
fn install_workspace_reload(
    ws: &std::sync::Arc<rto_graph::Workspace>,
    ws_cfg: config::WorkspaceConfig,
    cli_roots: Vec<String>,
) {
    let ws = ws.clone();
    std::thread::spawn(move || {
        // A current-thread runtime with just the I/O driver — all unix signal
        // handling needs (no timers), keeping this self-contained.
        let Ok(rt) = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
        else {
            return;
        };
        rt.block_on(async move {
            let mut hup =
                match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()) {
                    Ok(sig) => sig,
                    Err(e) => {
                        eprintln!("workspace reload disabled (cannot watch SIGHUP): {e}");
                        return;
                    }
                };
            eprintln!("send SIGHUP to reload the workspace (pick up added/removed repos)");
            while hup.recv().await.is_some() {
                let result = collect_workspace_repo_paths(&ws_cfg, &cli_roots)
                    .and_then(|paths| ws.reload_from(paths).map_err(anyhow::Error::from));
                match result {
                    Ok(names) => eprintln!(
                        "workspace reloaded: {} project(s) — {}",
                        names.len(),
                        names.join(", ")
                    ),
                    Err(e) => eprintln!("workspace reload failed (registry unchanged): {e}"),
                }
            }
        });
    });
}

/// On non-Unix, SIGHUP reload is unavailable; the server runs without it.
#[cfg(all(not(unix), any(feature = "mcp", feature = "serve")))]
fn install_workspace_reload(
    _ws: &std::sync::Arc<rto_graph::Workspace>,
    _ws_cfg: config::WorkspaceConfig,
    _cli_roots: Vec<String>,
) {
}

/// Git repos to host for a workspace `root`: the root itself if it is a repo,
/// plus each immediate subdirectory that is one. Shallow by design — a code
/// directory holding sibling checkouts is the common case, and a deep scan would
/// be slow and surprising.
fn discover_repos_under(root: &std::path::Path) -> anyhow::Result<Vec<std::path::PathBuf>> {
    let is_repo = |dir: &std::path::Path| dir.join(".git").exists();
    let mut repos = Vec::new();
    if is_repo(root) {
        repos.push(root.to_path_buf());
    }
    let entries = std::fs::read_dir(root)
        .map_err(|e| anyhow::anyhow!("reading workspace root `{}`: {e}", root.display()))?;
    let mut children: Vec<std::path::PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir() && is_repo(p))
        .collect();
    children.sort();
    repos.extend(children);
    Ok(repos)
}

/// Serve the graph over the Model Context Protocol, over stdio (default) or
/// streamable HTTP (`--http <addr>`). The `workspace` hosts one project
/// (single-repo, already synced) or many (opened on demand); tools select one
/// with `project` (ADR-0008).
#[cfg(feature = "mcp")]
fn serve_mcp(
    workspace: std::sync::Arc<rto_graph::Workspace>,
    http: Option<String>,
) -> anyhow::Result<()> {
    match http {
        Some(addr) => {
            let addr: std::net::SocketAddr = addr
                .parse()
                .map_err(|e| anyhow::anyhow!("invalid --http address `{addr}`: {e}"))?;
            eprintln!("roteiro MCP server listening on http://{addr}/mcp");
            rto_render::mcp::serve_http(workspace, addr).map_err(|e| anyhow::anyhow!("{e}"))
        }
        None => rto_render::mcp::serve_stdio(workspace).map_err(|e| anyhow::anyhow!("{e}")),
    }
}

/// MCP serving is unavailable without the `mcp` feature.
#[cfg(all(not(feature = "mcp"), feature = "serve"))]
fn serve_mcp(
    _workspace: std::sync::Arc<rto_graph::Workspace>,
    _http: Option<String>,
) -> anyhow::Result<()> {
    anyhow::bail!(
        "MCP serving needs the `mcp` feature (build with `--features mcp`); \
         use `--models` for the OpenAI-compatible model endpoint"
    )
}

/// Serve installed generative models over the loopback, OpenAI-compatible `/v1`
/// endpoint (ADR-0006). Serves only installed models; never downloads.
/// Collect the installed GGUF models eligible to serve over `/v1` (ADR-0006):
/// generative and embedding always, vision only with its `mmproj` projector;
/// OCR/audio are sync-time ingestion models, not endpoints. Narrowed by the
/// `[serve] models` allow-list if set. Returns the list; serving is the caller's.
#[cfg(feature = "serve")]
fn served_models(cfg: &config::Config) -> Vec<rto_serve::llama::Served> {
    use rto_graph::{ModelKind, Platform, REGISTRY, is_installed, model_dir};
    let host = Platform::host();
    let wanted = cfg.serve.models.as_deref();
    let has_file = |m: &rto_graph::ModelSpec, name: &str| {
        m.variant_for(host)
            .is_some_and(|v| v.files.iter().any(|f| f.name == name))
    };
    REGISTRY
        .iter()
        .filter(|m| wanted.is_none_or(|w| w.iter().any(|n| n == m.name)))
        .filter(|m| has_file(m, "model.gguf"))
        .filter(|m| match m.kind {
            ModelKind::Generative | ModelKind::Embedding => true,
            // A vision model is only servable with its multimodal projector.
            ModelKind::Vision => has_file(m, "mmproj.gguf"),
            // OCR and audio are sync-time ingestion models, not `/v1` endpoints.
            ModelKind::Ocr | ModelKind::Audio => false,
        })
        .filter(|m| m.variant_for(host).is_some_and(|v| is_installed(m.name, v)))
        .map(|m| rto_serve::llama::Served {
            name: m.name.to_owned(),
            path: model_dir(m.name).join("model.gguf"),
            mmproj: has_file(m, "mmproj.gguf").then(|| model_dir(m.name).join("mmproj.gguf")),
        })
        .collect()
}

#[cfg(feature = "serve")]
fn serve_models_endpoint(
    cfg: &config::Config,
    workspace: std::sync::Arc<rto_graph::Workspace>,
    opts: &ServeOptions,
) -> anyhow::Result<()> {
    let (addr, tls_cert, tls_key) = (
        opts.addr.clone(),
        opts.tls_cert.clone(),
        opts.tls_key.clone(),
    );

    let served = served_models(cfg);
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
    // Keep models resident up to the configured budget (MiB → bytes), loading on
    // demand and unloading the least-recently-used past it (ADR-0006). Unset ⇒ 0
    // ⇒ a single resident model.
    let budget_bytes = cfg
        .serve
        .memory_budget_mb
        .unwrap_or(0)
        .saturating_mul(1024 * 1024);
    let engine = rto_serve::llama::LlamaEngine::new_with_budget(served, 0, budget_bytes)
        .map_err(|e| anyhow::anyhow!("starting llama.cpp: {e}"))?;
    let engine: std::sync::Arc<dyn rto_serve::Engine> = std::sync::Arc::new(engine);

    // TLS precedence mirrors `addr`: CLI flag > `[serve]` config.
    let tls = resolve_serve_tls(
        tls_cert.or_else(|| cfg.serve.tls_cert.clone()),
        tls_key.or_else(|| cfg.serve.tls_key.clone()),
    )?;
    serve_v1_tail(cfg, workspace, opts, engine, socket, tls, &names)
}

/// Resolve the in-process TLS pair: both a cert and a key give HTTPS, neither
/// gives plain HTTP, exactly one is an error. (CLI-over-config precedence is
/// applied by the caller.)
#[cfg(feature = "serve")]
fn resolve_serve_tls(
    cert: Option<String>,
    key: Option<String>,
) -> anyhow::Result<Option<(std::path::PathBuf, std::path::PathBuf)>> {
    match (cert, key) {
        (Some(cert), Some(key)) => Ok(Some((
            std::path::PathBuf::from(cert),
            std::path::PathBuf::from(key),
        ))),
        (Some(_), None) | (None, Some(_)) => anyhow::bail!(
            "TLS needs both a certificate and a key — set both `--tls-cert`/`--tls-key` \
             (or `[serve] tls_cert`/`tls_key`), or neither for plain HTTP"
        ),
        (None, None) => Ok(None),
    }
}

/// Assemble the graph tools and serve the endpoint: `/v1` alone, or — with
/// `--mcp` — `/v1` **and** `/mcp` merged on one port (ADR-0008). Blocks until
/// shutdown.
#[cfg(feature = "serve")]
fn serve_v1_tail(
    cfg: &config::Config,
    workspace: std::sync::Arc<rto_graph::Workspace>,
    opts: &ServeOptions,
    engine: std::sync::Arc<dyn rto_serve::Engine>,
    socket: std::net::SocketAddr,
    tls: Option<(std::path::PathBuf, std::path::PathBuf)>,
    names: &str,
) -> anyhow::Result<()> {
    let scheme = if tls.is_some() { "https" } else { "http" };
    // Auto-register the graph tools (ADR-0006) unless disabled, so the served
    // model can `explain`/`search`/`path`/`debt` — across every hosted project
    // (ADR-0008), selected by a `project` argument.
    let tools: Option<std::sync::Arc<dyn rto_serve::ToolRegistry>> =
        if cfg.serve.tools.unwrap_or(true) {
            Some(std::sync::Arc::new(GraphToolRegistry::new(
                workspace.clone(),
            )))
        } else {
            None
        };
    let tools_note = if tools.is_some() {
        " (graph tools on)"
    } else {
        ""
    };

    // `--models --mcp`: mount the MCP graph server at `/mcp` on the SAME port as
    // `/v1`, so one process (one loaded model, one Workspace) serves both surfaces
    // (ADR-0008). Both are just axum path prefixes, so the routers merge.
    if opts.mcp {
        #[cfg(feature = "mcp")]
        {
            let v1 = match tools {
                Some(tools) => rto_serve::app_with_tools(engine, tools),
                None => rto_serve::app(engine),
            };
            let combined = v1.merge(rto_render::mcp::mcp_router(workspace));
            eprintln!(
                "roteiro server listening on {scheme}://{socket} — /v1{tools_note} + /mcp — serving: {names}"
            );
            return match tls {
                Some((cert, key)) => {
                    rto_serve::serve_blocking_router_tls(combined, socket, &cert, &key)
                }
                None => rto_serve::serve_blocking_router(combined, socket),
            };
        }
        #[cfg(not(feature = "mcp"))]
        anyhow::bail!(
            "`serve --models --mcp` needs the `mcp` feature (build with `--features serve,mcp`)"
        );
    }

    eprintln!(
        "roteiro model server listening on {scheme}://{socket}/v1{tools_note} — serving: {names}"
    );
    match tls {
        Some((cert, key)) => rto_serve::serve_blocking_tls(engine, tools, socket, &cert, &key),
        None => match tools {
            Some(tools) => rto_serve::serve_blocking_with_tools(engine, tools, socket),
            None => rto_serve::serve_blocking(engine, socket),
        },
    }
}

/// Resolve a tool-call key against a `project`: a project-qualified key
/// (`<project>::<key>`) follows a **cross-repo link** into that project (ADR-0009),
/// overriding the call's `project`; a bare key uses `project`. Owned parts, so a
/// query closure can capture them.
#[cfg(feature = "serve")]
fn qualified_or(key: &str, project: Option<&str>) -> (Option<String>, String) {
    match rto_graph::parse_qualified(key) {
        Some((p, bare)) => (Some(p.to_owned()), bare.to_owned()),
        None => (project.map(str::to_owned), key.to_owned()),
    }
}

/// A [`rto_serve::ToolRegistry`] backing the served model with Roteiro's graph
/// query tools (ADR-0006), over a [`rto_graph::Workspace`] of one or more
/// projects (ADR-0008). When several projects are hosted, every tool takes a
/// `project` selector and a `list_projects` tool is offered; a single-project
/// workspace behaves exactly as before (no `project` needed).
#[cfg(feature = "serve")]
struct GraphToolRegistry {
    workspace: std::sync::Arc<rto_graph::Workspace>,
}

#[cfg(feature = "serve")]
impl GraphToolRegistry {
    fn new(workspace: std::sync::Arc<rto_graph::Workspace>) -> Self {
        Self { workspace }
    }

    /// Resolve `project` (if hosting several) and run `query` against its store,
    /// serialising the result to JSON. Flattens the workspace/store/serialise
    /// error layers into the registry's `String` error.
    fn run<T: serde::Serialize>(
        &self,
        project: Option<&str>,
        query: impl FnOnce(&rto_graph::Store) -> Result<T, rto_graph::StoreError>,
    ) -> Result<String, String> {
        let result = self
            .workspace
            .with_store(project, query)
            .map_err(|e| e.to_string())?;
        let value = result.map_err(|e| e.to_string())?;
        serde_json::to_string(&value).map_err(|e| e.to_string())
    }
}

#[cfg(feature = "serve")]
impl rto_serve::ToolRegistry for GraphToolRegistry {
    fn tools(&self) -> Vec<rto_serve::ToolDef> {
        use serde_json::json;
        // `project` is an optional selector on every tool, and `list_projects` is
        // always offered — matching the MCP surface (whose schema the rmcp macro
        // generates statically, so it can't hide them). Uniform beats an
        // asymmetric surface; a single-project server resolves the sole project
        // for a bare call, and `list_projects` simply returns that one.
        let with_project = |mut props: serde_json::Value| {
            let obj = props.as_object_mut().expect("object schema");
            obj.insert(
                "project".to_owned(),
                json!({
                    "type": "string",
                    "description": "Optional: which hosted project to query (see \
                                    `list_projects`); omit if the server hosts one.",
                }),
            );
            props
        };

        let mut tools = vec![
            rto_serve::ToolDef {
                name: "explain".to_owned(),
                description: "Explain a graph node by key (its record and immediate \
                              neighbours), e.g. `fn:foo` or `file:src/main.rs`. A key may be \
                              project-qualified (`<project>::<key>`) to follow a cross-repo \
                              link into another hosted project (see `list_projects`)."
                    .to_owned(),
                parameters: json!({
                    "type": "object",
                    "properties": with_project(json!({ "key": { "type": "string" } })),
                    "required": ["key"],
                }),
            },
            rto_serve::ToolDef {
                name: "search".to_owned(),
                description: "Search graph nodes by text — names, keys, paths, and captured \
                              content (doc comments, README/ADR/blueprint prose). Returns the \
                              top matches with keys; curated ADRs/blueprints and READMEs rank \
                              first, so this is the entry point for \"what is X / why\" questions. \
                              Then call `explain` on a returned key for detail."
                    .to_owned(),
                parameters: json!({
                    "type": "object",
                    "properties": with_project(json!({
                        "query": { "type": "string" },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 25 },
                    })),
                    "required": ["query"],
                }),
            },
            rto_serve::ToolDef {
                name: "path".to_owned(),
                description: "Find a shortest path between two node keys. A path lives \
                              within one project; a project-qualified `from` \
                              (`<project>::<key>`) selects it (see `list_projects`)."
                    .to_owned(),
                parameters: json!({
                    "type": "object",
                    "properties": with_project(json!({
                        "from": { "type": "string" },
                        "to": { "type": "string" },
                    })),
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
                    "properties": with_project(json!({
                        "categories": { "type": "array", "items": { "type": "string" } },
                    })),
                }),
            },
        ];
        tools.push(rto_serve::ToolDef {
            name: "list_projects".to_owned(),
            description: "List the projects this server hosts (often just one). Pass one as \
                          `project` to the other tools to query it (ADR-0008)."
                .to_owned(),
            parameters: json!({ "type": "object", "properties": {} }),
        });
        tools
    }

    fn projects(&self) -> Vec<String> {
        self.workspace.names()
    }

    fn call(&self, name: &str, args: &serde_json::Value) -> Result<String, String> {
        let str_arg = |k: &str| args.get(k).and_then(serde_json::Value::as_str);
        let project = str_arg("project");
        match name {
            "list_projects" => serde_json::to_string(&serde_json::json!({
                "projects": self.workspace.names(),
            }))
            .map_err(|e| e.to_string()),
            "explain" => {
                let key = str_arg("key").ok_or("`explain` needs a string `key`")?;
                // A project-qualified key (`<project>::<key>`) follows a cross-repo
                // link into that project, overriding the `project` argument (ADR-0009).
                let (proj, bare) = qualified_or(key, project);
                self.run(proj.as_deref(), move |store| {
                    rto_graph::explain(store, &bare)
                })
            }
            "search" => {
                let query = str_arg("query")
                    .ok_or("`search` needs a string `query`")?
                    .to_owned();
                // `limit` is model-controlled: clamp to 1..=25 (results are
                // truncated before feed-back anyway) so a huge value can't
                // waste work; the schema advertises the same bound.
                let limit = args
                    .get("limit")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|n| usize::try_from(n).ok())
                    .unwrap_or(10)
                    .clamp(1, 25);
                self.run(project, |store| rto_graph::search(store, &query, limit))
            }
            "path" => {
                let from = str_arg("from").ok_or("`path` needs a string `from`")?;
                let to = str_arg("to").ok_or("`path` needs a string `to`")?;
                // A path lives within one graph; a qualified `from` selects the
                // project, and a qualifier on either endpoint is stripped to a
                // bare, in-store key (ADR-0009).
                let (proj, from_bare) = qualified_or(from, project);
                let to_bare = rto_graph::parse_qualified(to)
                    .map_or_else(|| to.to_owned(), |(_, b)| b.to_owned());
                self.run(proj.as_deref(), move |store| {
                    rto_graph::path(store, &from_bare, &to_bare)
                })
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
                self.run(project, |store| rto_graph::debt(store, &categories, &[]))
            }
            other => Err(format!("unknown tool `{other}`")),
        }
    }
}

/// The model endpoint is unavailable without the `serve` feature.
#[cfg(all(not(feature = "serve"), feature = "mcp"))]
fn serve_models_endpoint(
    _cfg: &config::Config,
    _workspace: std::sync::Arc<rto_graph::Workspace>,
    _opts: &ServeOptions,
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

    // Render lifetime docs (the Build Plan and the house-style blueprints) as
    // first-class root-level pages, and list them above the ADRs on the index.
    // Their `[[docs/adr/…]]` links resolve into the `adr/` subdirectory (the
    // `render_doc` prefix), which is correct for a root-level page.
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
    // Blueprints live under docs/blueprint(s)/ (ADR-0004); the overall project
    // blueprint is one. Render each to a root-level page like the Build Plan.
    for dir in ["docs/blueprint", "docs/blueprints"] {
        let bp_dir = root.join(dir);
        if !bp_dir.is_dir() {
            continue;
        }
        let mut bps: Vec<_> = std::fs::read_dir(&bp_dir)?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("md"))
            .filter(|p| p.file_name().and_then(|n| n.to_str()) != Some("README.md"))
            .collect();
        bps.sort();
        for path in &bps {
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("blueprint");
            let md = std::fs::read_to_string(path)?;
            let rendered = rto_render::render_doc(&md, stem);
            std::fs::write(out.join(format!("{stem}.html")), &rendered.html)?;
            lifetime.push(rto_render::IndexEntry {
                href: format!("../{stem}.html"),
                title: rendered.title,
            });
        }
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
    build_graph(&repo, &mut store, &cache, ingest, GraphSource::Committed)?;
    let out = out.map_or_else(
        || std::path::PathBuf::from("vault"),
        std::path::PathBuf::from,
    );
    if out.exists() {
        std::fs::remove_dir_all(&out)?;
    }
    std::fs::create_dir_all(&out)?;

    // A web "blob" base for clickable Source links, from the origin remote + the
    // rendered commit (an absolute URL, so it works in the downloaded vault too).
    // `None` when there is no mappable remote — notes then omit the link.
    let commit = repo.head_commit_id().ok();
    let remote = repo.origin_url();
    let source_base = match (remote.as_deref(), commit.as_deref()) {
        (Some(r), Some(c)) => source_blob_base(r, c),
        _ => None,
    };

    let mut count = 0usize;
    for key in store.all_keys()? {
        if let Some(ex) = rto_graph::explain(&store, &key)? {
            let note = rto_render::render_note(&ex, source_base.as_deref());
            std::fs::write(out.join(&note.filename), &note.content)?;
            count += 1;
        }
    }

    // The overview note: what was scanned, structure, provenance, ADRs, debt.
    let repo_url = remote.as_deref().and_then(repo_web_root);
    let home = rto_render::render_home(&vault_summary(&repo, &store, repo_url, commit)?);
    std::fs::write(out.join(&home.filename), &home.content)?;

    println!(
        "rendered obsidian vault → {} ({count} note(s) + {})",
        out.display(),
        rto_render::HOME_NOTE
    );
    Ok(())
}

/// Aggregate the store into the figures the vault's `_Home` overview shows.
fn vault_summary(
    repo: &rto_graph::Repo,
    store: &rto_graph::Store,
    repo_url: Option<String>,
    commit: Option<String>,
) -> anyhow::Result<rto_render::VaultSummary> {
    use rto_graph::{NodeKind, Provenance};

    let project = repo
        .workdir()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("this project")
        .to_owned();

    // Node counts by kind, most-frequent first (ties broken by kind for stability).
    let mut by_kind: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for node in store.all_nodes()? {
        *by_kind.entry(node.kind.as_str().to_owned()).or_default() += 1;
    }
    let mut node_counts: Vec<(String, usize)> = by_kind.into_iter().collect();
    node_counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    // Edge counts per provenance (only non-zero classes). Store errors propagate
    // rather than silently reporting a zero count.
    let mut edge_provenance = Vec::new();
    for p in [
        Provenance::Derived,
        Provenance::Authored,
        Provenance::Inferred,
    ] {
        let n = store.edges_by_provenance(p)?.len();
        if n > 0 {
            edge_provenance.push((p.as_str().to_owned(), n));
        }
    }

    // ADRs with their lifecycle status.
    let adrs = store
        .nodes_by_kind(&NodeKind::Adr)?
        .into_iter()
        .map(|n| rto_render::AdrEntry {
            key: n.key,
            name: n.name,
            status: n
                .meta
                .get("status")
                .and_then(|v| v.as_str())
                .map(ToOwned::to_owned),
        })
        .collect();

    let debt = rto_graph::debt(store, &[], &[])?
        .by_category
        .into_iter()
        .collect();

    Ok(rto_render::VaultSummary {
        project,
        total_nodes: usize::try_from(store.node_count()?)?,
        total_edges: usize::try_from(store.edge_count()?)?,
        node_counts,
        edge_provenance,
        adrs,
        debt,
        repo_url,
        commit,
    })
}

/// Web root for a git remote URL (`https://<host>/<owner>/<repo>`), or `None` if
/// it isn't a URL shape we can map. Handles `git@host:owner/repo(.git)`,
/// `ssh://[user@]host/owner/repo(.git)`, and `http(s)://[user@]host/owner/repo(.git)`.
fn repo_web_root(remote: &str) -> Option<String> {
    let s = remote.trim();
    // Normalise the remote's various spellings to `host/owner/repo…`.
    let hostpath = if let Some(rest) = s.strip_prefix("git@") {
        // `github.com:owner/repo` → `github.com/owner/repo`
        rest.replacen(':', "/", 1)
    } else {
        let rest = s
            .strip_prefix("ssh://")
            .or_else(|| s.strip_prefix("https://"))
            .or_else(|| s.strip_prefix("http://"))?;
        // Strip any `user@` credentials prefix.
        rest.rsplit_once('@').map_or(rest, |(_, r)| r).to_owned()
    };
    let hostpath = hostpath
        .strip_suffix(".git")
        .unwrap_or(&hostpath)
        .trim_end_matches('/');
    // Require at least `host/segment` so a bare host doesn't produce a broken link.
    if hostpath.split('/').filter(|s| !s.is_empty()).count() < 2 {
        return None;
    }
    Some(format!("https://{hostpath}"))
}

/// Web "blob" base for a file at `commit` on the `remote`'s host — e.g.
/// `https://github.com/owner/repo/blob/<commit>` — so `<base>/<path>` links to the
/// exact file. GitLab uses the `/-/blob/` infix; other hosts (GitHub, Gitea,
/// Codeberg, …) use `/blob/`. `None` for an unmappable remote.
fn source_blob_base(remote: &str, commit: &str) -> Option<String> {
    let root = repo_web_root(remote)?;
    let infix = if root.contains("gitlab") {
        "/-/blob/"
    } else {
        "/blob/"
    };
    Some(format!("{root}{infix}{commit}"))
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

#[cfg(test)]
mod url_tests {
    use super::{repo_web_root, source_blob_base};

    #[test]
    fn repo_web_root_maps_common_remote_forms() {
        let want = Some("https://github.com/OffeneDatenmodellierung/Roteiro".to_owned());
        for remote in [
            "git@github.com:OffeneDatenmodellierung/Roteiro.git",
            "https://github.com/OffeneDatenmodellierung/Roteiro.git",
            "https://github.com/OffeneDatenmodellierung/Roteiro",
            "ssh://git@github.com/OffeneDatenmodellierung/Roteiro.git",
            "https://user:tok@github.com/OffeneDatenmodellierung/Roteiro.git",
        ] {
            assert_eq!(repo_web_root(remote), want, "{remote}");
        }
        // Unmappable / degenerate remotes yield no link rather than a broken one.
        assert_eq!(repo_web_root("file:///tmp/x.git"), None);
        assert_eq!(repo_web_root("git@github.com:"), None, "no owner/repo");
    }

    #[test]
    fn source_blob_base_uses_host_specific_infix() {
        assert_eq!(
            source_blob_base("git@github.com:o/r.git", "abc123"),
            Some("https://github.com/o/r/blob/abc123".to_owned())
        );
        // GitLab's blob path is `/-/blob/`.
        assert_eq!(
            source_blob_base("git@gitlab.com:o/r.git", "abc123"),
            Some("https://gitlab.com/o/r/-/blob/abc123".to_owned())
        );
    }
}

#[cfg(test)]
mod workspace_tests {
    use super::discover_repos_under;

    #[test]
    fn discovers_the_root_and_immediate_repo_subdirs_only() {
        // A workspace root holding two repo checkouts and one plain directory.
        let base = std::env::temp_dir().join(format!("rto-disc-{}", std::process::id()));
        std::fs::remove_dir_all(&base).ok();
        for sub in ["alpha/.git", "beta/.git", "notarepo", "beta/deep/.git"] {
            std::fs::create_dir_all(base.join(sub)).expect("mkdir");
        }
        let found = discover_repos_under(&base).expect("scan");
        // The root itself is not a repo here; `alpha` and `beta` are, in sorted
        // order; `notarepo` is skipped and the scan is shallow (no `beta/deep`).
        assert_eq!(found, vec![base.join("alpha"), base.join("beta")]);

        // When the root itself is a repo, it is included first.
        std::fs::create_dir_all(base.join(".git")).expect("mkdir root .git");
        let found = discover_repos_under(&base).expect("scan");
        assert_eq!(
            found,
            vec![base.clone(), base.join("alpha"), base.join("beta")]
        );

        std::fs::remove_dir_all(&base).ok();
    }
}

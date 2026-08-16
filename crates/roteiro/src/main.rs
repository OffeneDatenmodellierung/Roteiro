//! Roteiro umbrella CLI. Wires the graph, spec, and render crates behind
//! subcommands; owns argument parsing, process I/O, and exit codes. See
//! ADR-0001 for the roadmap.
//!
//! @rto:0001

use clap::{Parser, Subcommand};

mod config;
// The read-only `/v1/graph/*` JSON API. Its runtime callers are `run_explorer`
// (the llama-free standalone server) and `serve_v1_tail` (merged onto `/v1` in a
// full `serve` build), so under `explorer` the router is always live.
#[cfg(feature = "explorer")]
mod graph_api;
// The served workspace-explorer web app (HTML shell + hand-written ES app +
// vendored cytoscape.js), same-origin over the `explorer` server's data API.
#[cfg(feature = "explorer")]
mod explorer_app;
mod infer_links;
mod init;
mod overview;
mod pins;
mod review;
// Structured logging / telemetry init (ADR-0011): the single place the tracing
// subscriber is built — the unchanged human-text stdout layer plus an opt-in
// rotating, OTEL-shaped JSON file layer.
mod telemetry;

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
    #[command(flatten)]
    log: LogArgs,
}

/// Global telemetry flags (ADR-0011), available on every subcommand. They enable
/// and configure an **optional** rotating log **file** in an OpenTelemetry-shaped
/// JSON format; stdout logging is unchanged and always on. Each also reads an env
/// var (the flag wins), and all override `[telemetry]` in config.
// `struct_field_names`: the `log_*` prefix is intentional — these are the global
// `--log*` flags, and the shared prefix reads clearly at the (single) use site.
#[derive(clap::Args, Debug)]
#[allow(clippy::struct_field_names)]
struct LogArgs {
    /// Also write logs to this rotating FILE (OTEL-shaped JSON), in addition to
    /// stdout. Unset ⇒ file logging is off. Overrides `[telemetry] file`.
    #[arg(
        long = "log-file",
        global = true,
        value_name = "PATH",
        env = "ROTEIRO_LOG_FILE"
    )]
    log_file: Option<String>,
    /// Enable file logging at the default path (`$ROTEIRO_HOME/logs/roteiro.log`)
    /// when `--log-file` is not given.
    #[arg(long = "log", global = true)]
    log_enable: bool,
    /// Rotation cadence for the log file: daily (default) | hourly | minutely |
    /// never. Overrides `[telemetry] rotation`.
    #[arg(
        long = "log-rotation",
        global = true,
        value_name = "CADENCE",
        env = "ROTEIRO_LOG_ROTATION"
    )]
    log_rotation: Option<String>,
    /// Log file format: otel (default) | json | text. Overrides `[telemetry] format`.
    #[arg(
        long = "log-format",
        global = true,
        value_name = "FORMAT",
        env = "ROTEIRO_LOG_FORMAT"
    )]
    log_format: Option<String>,
}

impl LogArgs {
    /// Fold the global flags into the [`telemetry::Overrides`] the init seam takes.
    fn overrides(&self) -> telemetry::Overrides {
        telemetry::Overrides {
            file: self.log_file.clone(),
            enable_default: self.log_enable,
            rotation: self.log_rotation.clone(),
            format: self.log_format.clone(),
        }
    }
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
        /// When listing `--kind config_key`, drop keys that come from build /
        /// tooling / CI config (`Cargo.toml`, `.github/` workflows, nextest, …),
        /// leaving only application config. Opt-in; the default lists everything.
        #[arg(long)]
        app_config_only: bool,
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
        /// Maximum number of hits to return, **per channel**.
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Also search model-generated media content — ASR transcripts and VLM
        /// descriptions from `roteiro media build`. **Off by default**:
        /// generated text is not a graph fact, and a model asked to transcribe
        /// silence returns confident invented prose (ADR-0015).
        ///
        /// When on, generated hits come back in their **own channel**, each
        /// marked `[generated]` with the producer that wrote it, ranked by a
        /// scorer that has no `authored` boost, and never mixed in with the
        /// graph hits. The limit applies per channel, so opting in never
        /// displaces a graph hit.
        #[arg(long)]
        include_generated: bool,
        /// Emit the results as JSON. Without `--include-generated` this is the
        /// long-standing array of hits; with it, the two-channel object
        /// (`{schema, hits, generated}`) — so only a caller that opted in sees a
        /// different shape.
        #[arg(long)]
        json: bool,
    },
    /// Generated media content: build it, report on it, discard it (ADR-0015).
    ///
    /// An ASR transcript or a VLM description is **generated**, not decoded —
    /// asked to transcribe digital silence a model returns fluent invented
    /// prose — so it is not a `derived` fact and lives in a **separate artifact
    /// store**, keyed by source blob and producer identity. Nothing here writes
    /// a node or an edge: `roteiro export` is unaffected by every one of these
    /// commands, and the content is reachable from `search` only via
    /// `--include-generated`.
    Media {
        #[command(subcommand)]
        action: MediaAction,
    },
    /// Episodic agent memory: record what a session learned, list it, forget it
    /// (ADR-0013).
    ///
    /// A lesson, a failed approach, a decision — knowledge with **no generating
    /// function**, which no amount of re-extraction brings back — so it is not a
    /// `derived` fact, and it was not deliberately written into a reviewed file,
    /// so it is not `authored` either. It lives in a **separate artifact store**
    /// and never borrows the graph's trust: nothing here writes a node or an
    /// edge, `roteiro export` is unaffected by every one of these commands, and
    /// memory does not enter `search` at all.
    ///
    /// **Memory is unredacted by construction.** Extraction redacts
    /// secret-looking config values before persisting them, because the graph is
    /// exportable; this store records prose you wrote, which has no such
    /// chokepoint and can contain pasted tokens, stack traces or customer names.
    /// It lives in `.git/roteiro/`, so it is never committed and never pushed,
    /// and `memory forget` is the way to take something back.
    Memory {
        #[command(subcommand)]
        action: MemoryAction,
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
        /// Select a **named** workspace from config (`[[workspaces]]`/`[standalone]`)
        /// to scope the report to. Default: the workspace containing the current
        /// repo, else today's flat `[workspace]` scope. Any `--workspace <ROOT>` is
        /// still unioned into the selected workspace.
        #[arg(long = "workspace-name", short = 'w', value_name = "NAME")]
        workspace_name: Option<String>,
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
        /// Exclude build / tooling / CI config (`Cargo.toml`, `.github/` workflows,
        /// nextest, …) from cross-repo matching, so `--infer`/`--matrix` compare and
        /// drift-check only application config — sharpening drift. Opt-in; the
        /// default considers every config key. Applies to `--infer` and `--matrix`.
        #[arg(long)]
        app_config_only: bool,
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
    /// Analyzer findings: ingest a normalized report and list what is live
    /// (ADR-0012). Findings are a **separate artifact store** — they are never
    /// nodes or edges, never carry a provenance class, and never appear in the
    /// exported graph artifact, because an analyzer's verdict is asserted at a
    /// point in time against rules and an advisory database that change
    /// independently of the source tree.
    #[cfg(feature = "execution")]
    Security {
        #[command(subcommand)]
        action: SecurityAction,
    },
    /// Start the network HTTP server: the OpenAI-compatible `/v1` model endpoint
    /// (+ graph tools + Ask) when built `--features serve` with a model installed,
    /// always alongside the read-only `/v1/graph/*` API and the `/` web UI (ADR-0006,
    /// ADR-0008, ADR-0010). Binds `[serve] addr` (default `127.0.0.1:8017`). A build
    /// without the model feature (or with none installed) degrades gracefully to the
    /// llama-free graph API + UI (Ask disabled) instead of failing. For the STDIO /
    /// networked MCP graph server, use `roteiro mcp`.
    #[cfg(any(feature = "mcp", feature = "serve", feature = "explorer"))]
    Serve {
        /// Bind ADDR (default `[serve] addr`, else `127.0.0.1:8017`). A non-loopback
        /// address is warned about (no auth — front it with a reverse proxy).
        #[arg(long, value_name = "ADDR")]
        addr: Option<String>,
        /// Terminate TLS in-process using this PEM certificate-chain file (paired
        /// with `--tls-key`). Overrides `[serve] tls_cert`. Set both `--tls-cert`
        /// and `--tls-key` for HTTPS, or neither for plain HTTP; setting only one
        /// is an error.
        #[arg(long, value_name = "FILE")]
        tls_cert: Option<String>,
        /// The PEM private-key file for `--tls-cert`.
        #[arg(long, value_name = "FILE")]
        tls_key: Option<String>,
        /// Workspace mode (ADR-0008): host every git repo under ROOT
        /// (repeatable), so one server — holding the model once — answers
        /// questions about many projects, selected per call by `project`.
        /// Combined with `[workspace]` config. Omit for single-repo serving
        /// (the current directory's repo).
        #[arg(long, value_name = "ROOT")]
        workspace: Vec<String>,
        /// Select a **named** workspace from config (`[[workspaces]]`/`[standalone]`,
        /// else the legacy `[workspace]` folded to `default`) as the default the flat
        /// `/v1/graph/*` routes bind to. Default: the workspace containing the current
        /// repo, else the sole configured workspace. Nested
        /// `/v1/graph/workspaces/{ws}/…` routes address a workspace explicitly and
        /// ignore this. An unknown name fails fast, listing the known ones.
        #[arg(long = "workspace-name", short = 'w', value_name = "NAME")]
        workspace_name: Option<String>,
        /// Workspace mode: (re)build each project's graph the first time it is
        /// queried, instead of serving whatever its hooks last left. Slower on
        /// first touch, but never serves a stale or missing graph (ADR-0008).
        #[arg(long)]
        sync_on_access: bool,
        /// Also mount the MCP graph server at `/mcp` on the **same port**, so one
        /// process serves both `/v1` and `/mcp` over one Workspace (needs
        /// `--features serve,mcp`).
        #[arg(long)]
        mcp: bool,
        /// Deprecated: `--models` is now the default for `serve` — drop the flag.
        /// Kept so existing scripts keep working; prints a one-line notice.
        #[arg(long, hide = true)]
        models: bool,
        /// Deprecated: use `roteiro mcp --http ADDR`. Kept as an alias that still
        /// starts the networked MCP server; prints a one-line notice.
        #[arg(
            long,
            value_name = "ADDR",
            hide = true,
            conflicts_with_all = ["models", "addr", "tls_cert", "tls_key", "mcp"]
        )]
        http: Option<String>,
    },
    /// Start the MCP graph server (ADR-0002): STDIO by default, or networked over
    /// streamable HTTP with `--http ADDR`. Exposes the `explain`/`search`/`path`/
    /// `debt` graph tools to MCP clients. This is the graph server — for the
    /// OpenAI-compatible model endpoint + web UI, use `roteiro serve`.
    #[cfg(any(feature = "mcp", feature = "serve"))]
    Mcp {
        /// Serve networked over streamable HTTP at ADDR (e.g. `127.0.0.1:8080`)
        /// instead of STDIO. Terminate TLS at a reverse proxy.
        #[arg(long, value_name = "ADDR")]
        http: Option<String>,
        /// Workspace mode (ADR-0008): host every git repo under ROOT (repeatable),
        /// selected per call by `project`. Combined with `[workspace]` config.
        /// Omit for single-repo serving (the current directory's repo).
        #[arg(long, value_name = "ROOT")]
        workspace: Vec<String>,
        /// Select a **named** workspace from config as the default the flat tools
        /// operate on (see `serve --workspace-name`). An unknown name fails fast.
        #[arg(long = "workspace-name", short = 'w', value_name = "NAME")]
        workspace_name: Option<String>,
        /// Workspace mode: (re)build each project's graph the first time it is
        /// queried, instead of serving whatever its hooks last left (ADR-0008).
        #[arg(long)]
        sync_on_access: bool,
    },
    /// Serve the read-only graph explorer JSON API (`/v1/graph/*`) over HTTP,
    /// **llama-free** (ADR-0008): axum only — no model, no MCP, no C/C++
    /// toolchain. Multi-workspace aware — it builds a `WorkspaceSet` from config
    /// (`[[workspaces]]` / `[standalone]`, else the current repo alone), lists it
    /// at `GET /v1/graph/workspaces`, and serves each workspace's graph both under
    /// `/v1/graph/workspaces/{ws}/…` and, for the default workspace, flat under
    /// `/v1/graph/…`. Read-only: serves whatever each repo's graph currently holds
    /// (run `roteiro sync` to refresh). It also serves the interactive
    /// **workspace-explorer web app** at `GET /` (ADR-0010, same-origin over this
    /// API). The **Ask** tab remains out of scope; it needs the `serve` build's
    /// `/v1/chat/completions`, which this server deliberately does not offer. Needs
    /// `--features explorer`.
    #[cfg(feature = "explorer")]
    Explorer {
        /// Bind address (default `[serve] addr`, else `127.0.0.1:8017`). A
        /// non-loopback address is warned about — the API has no auth, so front it
        /// with a reverse proxy.
        #[arg(long, value_name = "ADDR")]
        addr: Option<String>,
        /// The workspace the flat `/v1/graph/*` routes operate on. Default: the
        /// sole configured workspace, else the one containing the current repo.
        /// Nested `/v1/graph/workspaces/{ws}/…` routes always address a workspace
        /// explicitly and ignore this.
        #[arg(long = "workspace-name", short = 'w', value_name = "NAME")]
        workspace_name: Option<String>,
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

/// `roteiro media` actions.
///
/// Deliberately **not** feature-gated. The store, its status and its clearing
/// work in every build, so a default binary can still report what a
/// feature-enabled one produced and can still discard a producer it no longer
/// trusts. Only `build` needs a generator, and it says so by name when it has
/// none rather than quietly doing nothing.
#[derive(Subcommand)]
enum MediaAction {
    /// Generate content for media blobs that lack a record for the current
    /// producer.
    ///
    /// **Incremental by default**: a blob already described by exactly this
    /// model, quantisation, projector, prompt and sampling configuration is
    /// skipped without loading the model. A producer whose identity changed
    /// writes a **new record beside the old one**, never an overwrite, so two
    /// models' descriptions of the same clip can be compared.
    ///
    /// A **pre-generation gate** refuses blobs with nothing to read — digital
    /// silence, flat-colour images — before any model is loaded, and records
    /// *why*, with the value it measured, so `media status` can show the skip
    /// rather than leaving a hole. Tune it under `[media]`; `--force` overrides
    /// it for one run.
    ///
    /// Requires the matching feature (`audio-transcribe` / `image-vision`) and
    /// the model on disk; without either it fails with the command that fixes
    /// it. Honours `[ingest] audio` / `vision`, which mean *may this run
    /// generate at all*.
    Build {
        /// Transcribe audio blobs. Default: both modalities.
        #[arg(long)]
        audio: bool,
        /// Describe image blobs. Default: both modalities.
        #[arg(long)]
        vision: bool,
        /// Only this source blob (a git blob id, as printed by `media status`
        /// and shown in the explorer). The per-blob rebuild the explorer's
        /// "rebuild" action hands you: pair it with `--force` to redo exactly
        /// one description without touching the rest of the tree.
        #[arg(long, value_name = "BLOB")]
        blob: Option<String>,
        /// Regenerate blobs that already have a record for the current producer,
        /// replacing it. Without this, `build` does no work on a second run.
        ///
        /// **Also overrides the pre-generation gate**, so a silent clip is sent
        /// to the model anyway — which is a legitimate thing to ask for, and a
        /// flag named `--force` that quietly declined would be worse than none.
        #[arg(long)]
        force: bool,
        /// Emit the build report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Report what is stored: how many records, by which producer and model,
    /// when, and how much of the tree's media is described.
    ///
    /// Also lists the producers *this binary* could run right now, so "0
    /// records" is legible as **cannot generate** (no feature, or no model)
    /// rather than **nothing to generate**.
    Status {
        /// Emit the status report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Discard records — all of them, or one producer's.
    ///
    /// The graph is untouched: dropping a model you no longer trust costs you
    /// nothing but the generation time.
    Clear {
        /// Only this producer's records (a `media:<kind>:<model>:<id>` token, as
        /// printed by `media status`). Omit to clear every record.
        #[arg(long, value_name = "ID")]
        producer: Option<String>,
        /// Emit the result as JSON.
        #[arg(long)]
        json: bool,
    },
}

/// `roteiro memory` actions.
///
/// Ungated, like [`MediaAction`] and for the same reason: the store is `SQLite`
/// and serde, it needs no model and no feature, and a lesson an agent learned
/// must be recordable from whatever build is to hand.
#[derive(Subcommand)]
enum MemoryAction {
    /// Record one thing this session learned.
    ///
    /// The body is prose. Pass `-` to read it from stdin, which is how you record
    /// something with newlines in it — a stack trace, a diff, a write-up.
    ///
    /// `--anchor` ties the record to a node key, capturing that node's blob hash
    /// so a later read can tell **the code changed underneath this** from **the
    /// thing it was about is gone**. A key naming no node is accepted, not
    /// refused: a lesson about deleted code is often the most valuable one.
    ///
    /// **The anchor is also what decides where the record applies.** It applies
    /// to any tree where the anchor resolves with the same blob — whichever
    /// branch you wrote it on — and to no tree where it does not. Leave
    /// `--anchor` off for a general lesson about the repository ("CI is
    /// Ubuntu-only"), which applies everywhere.
    ///
    /// `--supersedes` records, explicitly, that this overrules an earlier record.
    /// The earlier one drops out of `memory list` immediately — regardless of age,
    /// because the test is a recorded pointer and not a clock — and stays readable
    /// under `--include-superseded`.
    ///
    /// Nothing here touches the graph, and the body is stored **verbatim and
    /// unredacted**: see `roteiro memory --help`.
    Add {
        /// The prose to record. `-` reads it from stdin.
        body: String,
        /// What kind of knowledge this is: lesson (default) | attempt | decision
        /// | pattern | outcome.
        #[arg(long, value_name = "KIND", default_value = rto_graph::MemoryKind::Lesson.as_str())]
        kind: rto_graph::MemoryKind,
        /// A coarse **namespace** — which repo or project this belongs to in a
        /// multi-repo workspace. **Not a branch label.** Where a record applies
        /// is decided by its `--anchor`, not by where it was written, so nothing
        /// isolates or inherits by scope: `memory list --scope` matches exactly.
        #[arg(long, value_name = "SCOPE", default_value = rto_graph::DEFAULT_MEMORY_SCOPE)]
        scope: String,
        /// Node key to anchor the record to (as printed by `roteiro search`).
        #[arg(long, value_name = "KEY")]
        anchor: Option<String>,
        /// Your own confidence in it, in `[0.0, 1.0]`. **Not** the score an
        /// `inferred` edge carries, and never read as one.
        #[arg(long, value_name = "FLOAT")]
        confidence: Option<f64>,
        /// The id of a record this one overrules.
        #[arg(long, value_name = "ID")]
        supersedes: Option<i64>,
        /// Emit the stored record as JSON.
        #[arg(long)]
        json: bool,
    },
    /// List what is remembered, newest first.
    ///
    /// Live records only by default: anything explicitly superseded is gone from
    /// this listing the moment its successor is written.
    ///
    /// Every row says whether it **applies to this tree**, and why. A record
    /// applies when its anchor resolves here with the same blob (the association
    /// is present in the same format), or when it has no anchor at all — a
    /// general lesson about the repository, which applies everywhere. A record
    /// whose anchor is `drifted`, `vanished` or `unverifiable` does not apply
    /// *here*: it is shown, marked, and still applies wherever its anchor does
    /// resolve. Nothing is ever dropped, and all of this is computed on every read
    /// and stored nowhere.
    List {
        /// Only this namespace, matched exactly. Not a branch filter — see
        /// `memory add --help`.
        #[arg(long, value_name = "SCOPE")]
        scope: Option<String>,
        /// Only this kind: lesson | attempt | decision | pattern | outcome.
        #[arg(long, value_name = "KIND")]
        kind: Option<rto_graph::MemoryKind>,
        /// Only records anchored to this node key.
        #[arg(long, value_name = "KEY")]
        anchor: Option<String>,
        /// Also show records that have been superseded — the audit view of the
        /// chain, which supersession keeps rather than deletes.
        #[arg(long)]
        include_superseded: bool,
        /// At most this many records (the newest ones).
        #[arg(long, value_name = "N", default_value_t = 50)]
        limit: usize,
        /// Emit the listing as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Delete one record. **The only way anything leaves this store.**
    ///
    /// Episodic memory is unbounded and never auto-evicted — no sweep, no TTL, no
    /// capacity bound — so this command is the whole reclamation story, and the
    /// whole privacy story: a record that captured a token or a customer name goes
    /// by asking.
    ///
    /// If the record had superseded another, that other one becomes **live
    /// again** and is named in the output: leaving it hidden would be supersession
    /// by a record that no longer exists.
    Forget {
        /// The record id, as printed by `memory list`.
        id: i64,
        /// Emit the result as JSON.
        #[arg(long)]
        json: bool,
    },
}

/// `roteiro security` actions.
#[cfg(feature = "execution")]
#[derive(Subcommand)]
enum SecurityAction {
    /// Ingest a normalized analyzer report produced elsewhere — a CI job, or a
    /// developer's own tooling — as a replaceable findings layer.
    ///
    /// The layer is keyed `security:<analyzer>:<worktree-id>` and is replaced
    /// **wholesale**, so re-ingesting is idempotent and a finding that has been
    /// fixed disappears instead of lingering. Nothing is written to the graph:
    /// `roteiro export` is unaffected by this command.
    Ingest {
        /// Report file (`-` reads from stdin). Either a normalized
        /// `roteiro.findings/v1` report, or an analyzer's own native output —
        /// `semgrep --json`, `cargo audit --json` — in which case `--analyzer`
        /// names which one it is.
        file: String,
        /// The analyzer FILE is native output from. Required for native output,
        /// ignored for a normalized report (which names its own analyzer).
        #[arg(long, value_name = "NAME")]
        analyzer: Option<String>,
        /// Emit the ingest report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// List the live findings for this worktree, newest run per analyzer.
    List {
        /// Only this analyzer's layer (e.g. `cargo-audit`).
        #[arg(long, value_name = "NAME")]
        analyzer: Option<String>,
        /// Emit the listing as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Run an analyzer against this worktree as a **child process on this
    /// host**, with no isolation (`--features exec-subprocess`).
    ///
    /// The analyzer's own egress is switched off and its inputs are pinned and
    /// pre-provisioned, but a subprocess on this host can do what this host can
    /// do — so the run's evidence records `isolation=none`, and
    /// `--allow-unsandboxed` is required to say you accept that. Assets are
    /// never fetched here: a cold cache fails and names the prefetch command.
    #[cfg(feature = "exec-subprocess")]
    Run {
        /// The analyzer to run (`roteiro security status` lists them).
        analyzer: String,
        /// Accept that this run has no isolation boundary. Required.
        #[arg(long)]
        allow_unsandboxed: bool,
        /// Emit the run report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Install and verify every pinned asset an analyzer needs, recording each
    /// digest — the one command that writes to the asset cache
    /// (`--features exec-subprocess`).
    #[cfg(feature = "exec-subprocess")]
    Prefetch {
        /// Only this analyzer's assets. Default: all of them.
        #[arg(long, value_name = "NAME")]
        analyzer: Option<String>,
        /// Emit the result as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Report each pinned asset's digest and fetch time, the advisory-database
    /// age behind each live findings layer, and which languages the shipped
    /// analyzers cover (`--features exec-subprocess`).
    #[cfg(feature = "exec-subprocess")]
    Status {
        /// Only this analyzer.
        #[arg(long, value_name = "NAME")]
        analyzer: Option<String>,
        /// Emit the status as JSON.
        #[arg(long)]
        json: bool,
    },
}

/// Exit `1` because a **gate** failed (drift, unresolved link, no path) — the
/// long-standing contract for `check`/`review`/`path`/`links`, which report the
/// failure on stdout/stderr and then set a non-zero status without an `Err`.
///
/// `std::process::exit` never runs destructors, so `main`'s
/// [`rto_graph::MediaEngineGuard`] would not fire here: release the media engines
/// first, or a gate failure on a run that described an image would abort in
/// ggml-metal's exit-time teardown and report 134 instead of 1 (issue #291).
///
/// The shared llama.cpp backend goes the same way, and in the same order —
/// engines, then backend (issue #296). It is released *after* the call above and
/// cannot be released out of turn: while any engine still holds a handle,
/// `release_shared_backend` declines.
///
/// This is the **only** sanctioned exit-without-`Err` in the CLI. A subcommand
/// that wants a non-zero status returns an error and lets `main` unwind, which is
/// what `security ingest` does — nothing in the findings path calls
/// `std::process::exit` directly, so nothing there can skip the guard.
fn exit_gate_failure() -> ! {
    let _released = rto_graph::release_media_engines();
    #[cfg(feature = "inference-local-models")]
    let _backend = rto_llama::backend::release_shared_backend();
    std::process::exit(1)
}

// `main` is a one-arm-per-subcommand dispatcher; splitting the match further just
// scatters the CLI wiring, so the line-count lint is noise here.
#[allow(clippy::too_many_lines)]
fn main() -> anyhow::Result<()> {
    // Restore the default SIGPIPE disposition before any output. Rust sets
    // SIGPIPE to `SIG_IGN` at startup, so writing to a closed stdout pipe
    // (`roteiro query … | head`) returns EPIPE and the `println!` family panics
    // with a broken-pipe backtrace instead of the process exiting quietly like a
    // normal Unix CLI. Resetting to `SIG_DFL` here — first line, before
    // `Cli::parse()` (which may itself print `--help`/`--version`) and before
    // dispatch, so every subcommand is covered — makes a closed pipe terminate
    // the process by signal, as expected. A no-op off Unix. The long-running
    // `serve`/`explorer` paths never write to a closing stdout pipe in normal
    // operation, so this does not affect them. (We forbid `unsafe` workspace-wide,
    // hence the `sigpipe` wrapper rather than a raw `libc::signal` call.)
    sigpipe::reset();
    let cli = Cli::parse();
    // Load layered config once (project `roteiro.toml` + user `~/.roteiro/
    // config.toml`); a malformed file is a hard error for any command (ADR-0007).
    let cwd = std::env::current_dir()?;
    let cfg = config::load(&cwd)?;
    // Initialise logging once, here — the single subscriber-build seam (ADR-0011).
    // Stdout logging is unchanged; a rotating OTEL-JSON file sink is added only when
    // `[telemetry] file` / `--log-file` / `--log` enables it. The returned guard
    // flushes the non-blocking file writer on exit, so it is held for all of `main`.
    let _log_guard = telemetry::init(&cli.log.overrides(), &cfg.effective.telemetry)?;
    // Resolve the ingestion toggles once; every command that (re)builds the graph
    // extracts with the same set so they share one cache, never thrashing it.
    let ingest = cfg.effective.ingest.resolve();
    // The pre-generation gate's thresholds (`[media]`), resolved once alongside
    // the ingestion toggles — the two answer adjacent questions about media:
    // *may* this run generate at all, and *should* it bother for this blob.
    let gate = cfg.effective.media.resolve();
    // Paths excluded from the intent-debt scan (`[debt] ignore`), shared by
    // `debt` and `check`'s debt summary.
    let debt_ignore: &[String] = cfg.effective.debt.ignore.as_deref().unwrap_or(&[]);
    // Honour `[paths] model_store` before any command touches the model store.
    // The registry lives behind the `models` feature; on a build without it a
    // configured path is inert, so we warn rather than silently ignore it.
    if let Some(dir) = cfg.effective.paths.model_store.as_deref() {
        let dir = config::expand_tilde(dir).into_owned();
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
    // Own the process's single llama.cpp backend for the rest of `main` (issue
    // #296). Declared **before** the media guard below precisely so that it drops
    // *after* it: Rust drops locals in reverse declaration order, and llama.cpp
    // requires every model be freed before the backend. This covers the engines
    // `rto-graph` never sees — `serve`'s resident model, `infer --model`, `spec
    // draft` — while `release_media_engines` covers the extractors'; both call the
    // same idempotent release, and neither can free the backend out of turn,
    // because an engine that still holds a handle makes the release decline.
    #[cfg(feature = "inference-local-models")]
    let _backend = rto_llama::backend::SharedBackendGuard::hold();
    // Own the media extractors' native engines for the rest of `main`. Extraction
    // loads a llama.cpp vision/ASR engine once and reuses it across blobs; on the
    // Metal backend that engine must be *dropped* before libc's C++ finalizers run,
    // or ggml-metal's device teardown finds a non-empty residency set and aborts a
    // successful run with SIGABRT (exit 134, issue #291). Dropping this guard at the
    // end of `main` releases them while Rust destructors still run — on the normal
    // path, on a `?` error out of a subcommand, and on an unwinding panic alike.
    // The `std::process::exit` gate failures below skip destructors, so they call
    // `exit_gate_failure`, which releases first.
    let _engines = rto_graph::MediaEngineGuard::hold();
    match cli.command {
        Command::Sync { json, committed } => run_sync(ingest, json, committed),
        Command::Check {
            json,
            committed,
            staged,
        } => run_check(ingest, json, committed, staged, debt_ignore),
        Command::Review { json, base } => run_review(ingest, json, base.as_deref()),
        Command::Query {
            key,
            kind,
            app_config_only,
            json,
        } => run_query(ingest, key, kind, app_config_only, json),
        Command::Search {
            query,
            limit,
            include_generated,
            json,
        } => run_search(ingest, &query, limit, include_generated, json),
        Command::Media { action } => run_media(ingest, gate, action),
        Command::Memory { action } => run_memory(action),
        Command::Context { key, refresh, json } => run_context(ingest, key, refresh, json),
        Command::Debt { kind, json } => run_debt(ingest, &kind, json, debt_ignore),
        Command::Path { from, to, json } => run_path(ingest, &from, &to, json),
        Command::Links {
            workspace,
            workspace_name,
            infer,
            matrix,
            hub,
            hub_rev,
            pinned,
            write,
            html,
            out,
            app_config_only,
            json,
        } => {
            let pin = PinnedHub {
                rev: hub_rev.as_deref(),
                auto: pinned,
                ingest,
            };
            let scope = LinksScope {
                cli_roots: &workspace,
                workspace_name: workspace_name.as_deref(),
            };
            let opts = InferOptions {
                hub: hub.as_deref(),
                pin,
                app_config_only,
            };
            if matrix {
                run_links_matrix(&cfg.effective, &scope, opts, html, out, json)
            } else if infer {
                run_links_infer(&cfg.effective, &scope, opts, write, json)
            } else {
                // `--app-config-only` only filters config-key matching, which the
                // plain authored-links report doesn't do. Reject it here rather than
                // silently ignoring it, so the flag never looks like it took effect.
                if app_config_only {
                    anyhow::bail!(
                        "`--app-config-only` applies only to `roteiro links --infer` / `--matrix` \
                         (it filters cross-repo config-key matching); \
                         `roteiro query --kind config_key --app-config-only` supports it too"
                    );
                }
                run_links(&cfg.effective, &scope, json)
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
        #[cfg(feature = "execution")]
        Command::Security { action } => run_security(action),
        #[cfg(any(feature = "mcp", feature = "serve", feature = "explorer"))]
        Command::Serve {
            models,
            http,
            addr,
            tls_cert,
            tls_key,
            workspace,
            workspace_name,
            sync_on_access,
            mcp,
        } => run_serve(
            ingest,
            &cfg.effective,
            &ServeOptions {
                models,
                http,
                addr,
                tls_cert,
                tls_key,
                mcp,
            },
            &workspace,
            workspace_name.as_deref(),
            sync_on_access,
        ),
        #[cfg(any(feature = "mcp", feature = "serve"))]
        Command::Mcp {
            http,
            workspace,
            workspace_name,
            sync_on_access,
        } => run_mcp(
            ingest,
            &cfg.effective,
            http,
            &workspace,
            workspace_name.as_deref(),
            sync_on_access,
        ),
        #[cfg(feature = "explorer")]
        Command::Explorer {
            addr,
            workspace_name,
        } => run_explorer(&cfg.effective, addr, workspace_name.as_deref()),
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
    print_telemetry_section(e, p, u);
    print_workspace_section(e, p, u);
}

/// Print the `[telemetry]` config section (ADR-0011), with each value's
/// provenance. Split out of [`print_config_sections`] to keep it under the line
/// budget.
fn print_telemetry_section(e: &config::Config, p: &config::Config, u: &config::Config) {
    println!("[telemetry]");
    println!(
        "  file     = {:?}  ({})",
        e.telemetry.file,
        provenance(p.telemetry.file.is_some(), u.telemetry.file.is_some())
    );
    println!(
        "  rotation = {:?}  ({})",
        e.telemetry.rotation,
        provenance(
            p.telemetry.rotation.is_some(),
            u.telemetry.rotation.is_some()
        )
    );
    println!(
        "  format   = {:?}  ({})",
        e.telemetry.format,
        provenance(p.telemetry.format.is_some(), u.telemetry.format.is_some())
    );
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
        exit_gate_failure();
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
        exit_gate_failure();
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

    // Fixed-width, ASCII-safe status markers (same length either way) keep the
    // columns aligned regardless of a terminal's wide/ambiguous glyph handling.
    const MARK_INSTALLED: &str = "[installed]";
    const MARK_AVAILABLE: &str = "[available]";
    const _: () = assert!(MARK_INSTALLED.len() == MARK_AVAILABLE.len());

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
    // Tier acts as a sub-heading, so it reads once per group instead of being
    // repeated as a prefix on every row.
    let tiers = [
        (ResourceTier::Low, "low  (any laptop)"),
        (ResourceTier::Mid, "mid  (~16 GB)"),
        (ResourceTier::High, "high (workstation / 64 GB)"),
    ];

    // Pad the name column to the widest registry name so metadata lines up, and
    // indent description continuation lines to start under the name column.
    let name_w = REGISTRY.iter().map(|s| s.name.len()).max().unwrap_or(0);
    let desc_indent = 4 + MARK_AVAILABLE.len() + 1;

    for (kind, heading) in sections {
        println!("\n{heading}:");
        for (tier, tier_label) in tiers {
            let mut specs = REGISTRY
                .iter()
                .filter(|s| s.kind == kind && s.tier == tier)
                .peekable();
            // Skip a tier with no picks in this section rather than print an
            // empty sub-heading.
            if specs.peek().is_none() {
                continue;
            }
            println!("  {tier_label}");
            for spec in specs {
                let variant = spec.variant_for(host);
                let installed = variant.is_some_and(|v| rto_graph::is_installed(spec.name, v));
                let mark = if installed {
                    MARK_INSTALLED
                } else {
                    MARK_AVAILABLE
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
                    "    {mark} {name:<name_w$}  {licence}{role}{dim}, ~{size} MiB",
                    name = spec.name,
                    licence = spec.licence,
                    size = spec.size_mib,
                );
                println!("{:desc_indent$}{desc}", "", desc = spec.description);
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
/// The source-file component of a `config_key` node key (`cfgkey:<file>#<dotted>`),
/// or `None` for any other node key. Neither the file path nor the dotted key
/// contains `#`, so the first `#` cleanly separates them. Used to classify a
/// config key as app vs tooling config for `--app-config-only`.
fn cfgkey_file(node_key: &str) -> Option<&str> {
    node_key
        .strip_prefix("cfgkey:")
        .map(|rest| rest.split_once('#').map_or(rest, |(file, _)| file))
}

fn run_query(
    ingest: rto_graph::IngestConfig,
    key: Option<String>,
    kind: Option<String>,
    app_config_only: bool,
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
                print_audio_stream(&ex);
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
            let mut listing = list_kind(&store, &NodeKind::from_token(&kind))?;
            // `--app-config-only`: drop config keys sourced from build/tooling/CI
            // files, keeping only real app config. Opt-in — off by default, so the
            // listing is unchanged unless the flag is passed. A config-key node's
            // key is `cfgkey:<file>#<dotted>`, so classify from that file component.
            if app_config_only {
                listing.nodes.retain(|n| match cfgkey_file(&n.key) {
                    Some(file) => !rto_graph::is_tooling_config_path(file),
                    None => true,
                });
            }
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

/// Print an `audio_stream` node's stream line, and nothing at all for any other
/// node (ADR-0016).
///
/// The line is the node's own rendered `meta.content` — the same string
/// [`rto_graph::search`] matches on — so the human surface and the search index
/// cannot drift apart, and there is no second place to remember to qualify a
/// duration. That matters here specifically: an MP3's duration is often an
/// **estimate**, and ADR-0016 requires every surface that shows one to say so.
/// It does, because `AudioDuration`'s rendering carries the marker with the
/// number rather than beside it.
///
/// Deliberately narrow: it fires on the node kind, not on "any node with
/// content", so ADRs and READMEs keep printing exactly as they did.
fn print_audio_stream(ex: &rto_graph::Explanation) {
    for line in audio_stream_lines(ex) {
        println!("{line}");
    }
}

/// The lines [`print_audio_stream`] emits — empty for any node that is not an
/// `audio_stream`. Split out from the printing so the gate is testable without
/// capturing stdout.
fn audio_stream_lines(ex: &rto_graph::Explanation) -> Vec<String> {
    if ex.node.kind != "audio_stream" {
        return Vec::new();
    }
    let Some(content) = ex.meta.get("content").and_then(|v| v.as_str()) else {
        return Vec::new();
    };
    content
        .lines()
        .enumerate()
        // The first line is the stream shape; any that follow are tags.
        .map(|(i, line)| {
            if i == 0 {
                format!("  stream: {line}")
            } else {
                format!("    {line}")
            }
        })
        .collect()
}

/// Search the graph by text and print ranked hits (highest score first). A
/// read-only report: it exits zero even when nothing matches, keeping stdout
/// empty (or an empty JSON array) so it composes in scripts.
///
/// With `--include-generated`, model-generated media content is searched too and
/// printed **under its own heading, after the graph hits**, each line prefixed
/// `[generated]` and tagged with the producer that wrote it (ADR-0015). The two
/// channels are ranked separately and limited separately: opting in cannot
/// displace a graph hit, and a generated hit can never be read as an extracted
/// fact.
fn run_search(
    ingest: rto_graph::IngestConfig,
    query: &str,
    limit: usize,
    include_generated: bool,
    json: bool,
) -> anyhow::Result<()> {
    let (repo, mut store, cache) = open_graph()?;
    build_graph(&repo, &mut store, &cache, ingest, GraphSource::Committed)?;

    let opts = rto_graph::SearchOptions {
        limit,
        include_generated,
    };
    let results = rto_graph::search_channels(&store, query, opts)?;
    if json {
        // Without the opt-in, emit exactly the shape callers already parse: the
        // bare array of graph hits. Adding a wrapper for everyone would be a
        // breaking change to pay for a feature they did not ask for.
        if include_generated {
            emit_json(&results)?;
        } else {
            emit_json(&results.hits)?;
        }
        return Ok(());
    }
    if results.hits.is_empty() && results.generated.is_empty() {
        // Keep stdout empty on a miss; report to stderr.
        eprintln!("no matches for `{query}`");
        return Ok(());
    }
    for hit in &results.hits {
        println!("  {:>4}  {:<8}  {}", hit.score, hit.node.kind, hit.node.key);
    }
    println!("{} hit(s)", results.hits.len());
    if !results.generated.is_empty() {
        println!();
        println!("generated media content — produced by a model, not extracted from the source:");
        for hit in &results.generated {
            println!(
                "  {:>4}  [generated:{}]  {}  ({})",
                hit.score, hit.kind, hit.path, hit.producer
            );
        }
        println!("{} generated hit(s)", results.generated.len());
    }
    Ok(())
}

/// The `--json` shape of `roteiro media clear`.
#[derive(serde::Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
struct MediaClearReport {
    /// The producer cleared, or `null` for every producer.
    producer: Option<String>,
    /// Records removed.
    removed: usize,
}

/// Dispatch a `roteiro media` action.
fn run_media(
    ingest: rto_graph::IngestConfig,
    gate: rto_graph::GateThresholds,
    action: MediaAction,
) -> anyhow::Result<()> {
    match action {
        MediaAction::Build {
            audio,
            vision,
            blob,
            force,
            json,
        } => run_media_build(
            media_options(ingest, gate, audio, vision, force)?,
            blob.as_deref(),
            json,
        ),
        MediaAction::Status { json } => run_media_status(json),
        MediaAction::Clear { producer, json } => run_media_clear(producer.as_deref(), json),
    }
}

/// Which modalities a `media build` invocation should run: the flags if either
/// was given, otherwise both — then narrowed by `[ingest]`, which decides
/// whether generation is permitted in this repository at all. `gate` carries the
/// pre-generation thresholds through unchanged.
///
/// A modality asked for **explicitly** but disabled in config is an error, not a
/// silent no-op: the operator asked for something the configuration forbids, and
/// being told so is the difference between a policy and a surprise.
fn media_options(
    ingest: rto_graph::IngestConfig,
    gate: rto_graph::GateThresholds,
    audio: bool,
    vision: bool,
    force: bool,
) -> anyhow::Result<rto_graph::MediaBuildOptions> {
    let explicit = audio || vision;
    let (mut want_audio, mut want_vision) = if explicit {
        (audio, vision)
    } else {
        (true, true)
    };
    for (asked, allowed, name) in [
        (
            &mut want_audio,
            ingest.generates(rto_graph::MediaKind::Audio),
            "audio",
        ),
        (
            &mut want_vision,
            ingest.generates(rto_graph::MediaKind::Vision),
            "vision",
        ),
    ] {
        if *asked && !allowed {
            if explicit {
                anyhow::bail!(
                    "`[ingest] {name} = false` in this repository's configuration forbids \
                     generating {name} content; remove the setting to allow it"
                );
            }
            *asked = false;
        }
    }
    if !want_audio && !want_vision {
        anyhow::bail!(
            "`[ingest]` disables both audio and vision generation in this repository, so \
             there is nothing for `media build` to do"
        );
    }
    Ok(rto_graph::MediaBuildOptions {
        audio: want_audio,
        vision: want_vision,
        force,
        thresholds: gate,
    })
}

/// Generate content for media blobs that lack a record for the current producer,
/// optionally narrowed to a single `blob`.
///
/// Nothing here touches the graph. The blobs come from the `HEAD` tree, the
/// records go to the media store, and `roteiro export` is byte-identical either
/// side of the call.
fn run_media_build(
    opts: rto_graph::MediaBuildOptions,
    blob: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    let (repo, mut store, _cache) = open_graph()?;
    // Resolved before any model is loaded, so a build with nothing to do costs
    // nothing — and a build this binary cannot perform says so immediately.
    let mut blobs = rto_graph::media_blobs(&repo)?;
    if let Some(wanted) = blob {
        blobs.retain(|b| b.blob_id == wanted);
        if blobs.is_empty() {
            anyhow::bail!(
                "no media blob `{wanted}` in this tree — `roteiro media status --json` lists \
                 the blob ids that are there"
            );
        }
    }
    // Assembling a producer checks that its model is *installed*; it does not
    // load it. The GGUF load happens on the first blob that actually reaches the
    // model, so a run the gate refuses outright never pays for one (ADR-0015).
    let producers = rto_graph::media::producers::installed(opts)?;
    let refs: Vec<&dyn rto_graph::MediaProducer> = producers.iter().map(AsRef::as_ref).collect();

    let report = rto_graph::build_media(&mut store, &blobs, &refs, opts, |blob| {
        repo.read_blob(&blob.blob_id).ok()
    })?;
    if json {
        emit_json(&report)?;
    } else if report.candidates == 0 {
        println!("no media blobs to describe in this tree");
    } else {
        println!(
            "{} candidate(s) — {} generated, {} already described, \
             {} refused by the gate (no model loaded), {} produced nothing",
            report.candidates,
            report.generated,
            report.skipped_existing,
            report.gated,
            report.empty,
        );
        if report.gated > 0 {
            println!("  see `roteiro media status` for each refusal and the value it measured");
        }
        for producer in &report.producers {
            println!("  producer  {producer}");
        }
    }
    Ok(())
}

/// Report what the media store holds, and what this binary could add to it.
fn run_media_status(json: bool) -> anyhow::Result<()> {
    let (repo, store, _cache) = open_graph()?;
    let blobs = rto_graph::media_blobs(&repo)?;
    let status = rto_graph::media_status(&store, &blobs)?;
    if json {
        emit_json(&status)?;
        return Ok(());
    }
    // "media record(s)", not "generated record(s)": since the gate a record can
    // be a refusal, and calling a skip "generated" would misreport the one thing
    // this command exists to make legible.
    println!(
        "{} media record(s), {} of them gate refusals",
        status.records,
        status.skipped.len(),
    );
    for producer in &status.producers {
        println!(
            "  {}  {} ({}, {})  {} record(s) ({} skipped), latest {}",
            producer.producer_id,
            producer.model,
            producer.kind,
            producer.quantisation,
            producer.records,
            producer.skipped,
            producer.latest,
        );
    }
    for candidate in &status.candidates {
        println!(
            "  {:<7} {} blob(s) in tree, {} described, {} skipped by the gate",
            candidate.kind, candidate.blobs, candidate.described, candidate.skipped,
        );
    }
    // Each refusal, with the value that caused it. Without this line a skipped
    // blob is an indistinguishable hole, and an operator cannot tell "nothing
    // was generated for this" from "nothing was generated, and here is why".
    for entry in &status.skipped {
        println!(
            "  skipped    {}  {}  ({})",
            entry.path, entry.skip, entry.producer_id,
        );
    }
    for producer in &status.available_producers {
        let state = if producer.current {
            "current"
        } else {
            "would write new records"
        };
        println!(
            "  available  {}  {} ({state})",
            producer.producer_id, producer.model
        );
    }
    // The distinction that was missing before ADR-0015: nothing stored because
    // nothing *can* be generated here. Reported per modality, and — the part that
    // matters — separating the two reasons it can be unavailable. They call for
    // completely different actions, and telling someone to recompile when all
    // they need is a download costs them an afternoon.
    for kind in [rto_graph::MediaKind::Audio, rto_graph::MediaKind::Vision] {
        if status.available_producers.iter().any(|p| p.kind == kind) {
            continue;
        }
        if kind.compiled_in() {
            println!(
                "  unavailable  {kind}: the model is not installed — run `roteiro model pull {}`",
                kind.model(),
            );
        } else {
            println!(
                "  unavailable  {kind}: this build has no {kind} generator — rebuild with \
                 `--features {}`, then `roteiro model pull {}`",
                kind.feature(),
                kind.model(),
            );
        }
    }
    Ok(())
}

/// Discard records, wholly or per producer. The graph is untouched.
fn run_media_clear(producer: Option<&str>, json: bool) -> anyhow::Result<()> {
    let (_repo, mut store, _cache) = open_graph()?;
    let removed = store.clear_media_content(producer)?;
    if json {
        emit_json(&MediaClearReport {
            producer: producer.map(ToOwned::to_owned),
            removed,
        })?;
    } else {
        match producer {
            Some(id) => println!("removed {removed} record(s) produced by {id}"),
            None => println!("removed {removed} record(s)"),
        }
        println!("the graph is unchanged");
    }
    Ok(())
}

/// Dispatch a `roteiro memory` action (ADR-0013).
///
/// The graph is deliberately **not** built or re-synced by any of these: what a
/// session learned is not derived from the tree, so recording it neither needs a
/// fresh graph nor may alter one. `add` reads `nodes` to capture an anchor's
/// evidence and `list` reads it to check that evidence, and that is the whole of
/// the interaction — `nodes`/`edges` are never written, which is what keeps
/// `roteiro export` byte-identical across every command here.
fn run_memory(action: MemoryAction) -> anyhow::Result<()> {
    match action {
        MemoryAction::Add {
            body,
            kind,
            scope,
            anchor,
            confidence,
            supersedes,
            json,
        } => run_memory_add(
            &body,
            kind,
            &scope,
            anchor.as_deref(),
            confidence,
            supersedes,
            json,
        ),
        MemoryAction::List {
            scope,
            kind,
            anchor,
            include_superseded,
            limit,
            json,
        } => run_memory_list(
            scope.as_deref(),
            kind,
            anchor.as_deref(),
            include_superseded,
            limit,
            json,
        ),
        MemoryAction::Forget { id, json } => run_memory_forget(id, json),
    }
}

/// Record one memory. `body` of `-` reads the prose from stdin, so something with
/// newlines in it — a stack trace, a diff — does not have to be quoted onto one
/// command line.
#[allow(clippy::fn_params_excessive_bools)]
fn run_memory_add(
    body: &str,
    kind: rto_graph::MemoryKind,
    scope: &str,
    anchor: Option<&str>,
    confidence: Option<f64>,
    supersedes: Option<i64>,
    json: bool,
) -> anyhow::Result<()> {
    let body = if body == "-" {
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
        buf
    } else {
        body.to_owned()
    };

    let (_repo, mut store, _cache) = open_graph()?;
    let id = store.record_memory(&rto_graph::MemoryWrite {
        scope,
        kind,
        anchor,
        body: &body,
        confidence,
        supersedes,
    })?;
    // Read the record back rather than echoing the request: what is printed is
    // what was stored, including the anchor evidence captured from the graph and
    // the anchor state that evidence yields right now.
    let record = store
        .memory_record(id)?
        .ok_or_else(|| anyhow::anyhow!("memory record {id} vanished immediately after writing"))?;

    if json {
        emit_json(&record)?;
        return Ok(());
    }
    println!("recorded memory #{id} — {} in scope `{scope}`", record.kind);
    println!("  {}", applicability(&record));
    if record.anchor.is_none() {
        // The no-anchor case is a *choice* with consequences, and the consequence
        // is the good one, so say it rather than leaving a blank where the anchor
        // line would be.
        println!("          a general lesson: it applies in every tree, on every branch");
    } else if !record.applies {
        // Said at write time, not only at read time: anchoring to something that
        // has already moved is legitimate — that is how you record a lesson about
        // deleted code — but the operator should know that is what they just did.
        println!(
            "          the anchor does not resolve in this tree, so the record is \
             kept and marked rather than applied here"
        );
    }
    if let Some(target) = supersedes {
        println!("  supersedes #{target}, which leaves `memory list` from now on");
    }
    // The one property of this store an operator must not have to look up.
    println!(
        "stored verbatim in .git/roteiro/ — never committed, never pushed, and never \
         redacted; `roteiro memory forget {id}` takes it back"
    );
    Ok(())
}

/// List what is remembered, newest generation first.
fn run_memory_list(
    scope: Option<&str>,
    kind: Option<rto_graph::MemoryKind>,
    anchor: Option<&str>,
    include_superseded: bool,
    limit: usize,
    json: bool,
) -> anyhow::Result<()> {
    let (_repo, store, _cache) = open_graph()?;
    let listing = store.memory_listing(&rto_graph::MemoryFilter {
        scope,
        kind,
        anchor_key: anchor,
        include_superseded,
        limit: Some(limit),
    })?;

    if json {
        emit_json(&listing)?;
        return Ok(());
    }
    if listing.records.is_empty() {
        // The counts make an empty result legible: nothing matched is a very
        // different report from nothing is stored.
        if listing.live == 0 && listing.superseded == 0 {
            println!("nothing remembered yet (`roteiro memory add \"<what you learned>\"`)");
        } else {
            println!(
                "no memory record matched; {} live and {} superseded record(s) are stored",
                listing.live, listing.superseded
            );
        }
        return Ok(());
    }
    let mut inapplicable = 0_usize;
    for record in &listing.records {
        if !record.applies {
            inapplicable += 1;
        }
        // `as_str`, not the `Display` impls: those write straight to the
        // formatter, so a width given here would be silently ignored and the
        // columns would not line up.
        println!(
            "#{:<4} {:<8} scope={:<12} {}",
            record.id,
            record.kind.as_str(),
            record.scope,
            applicability(record),
        );
        println!("      {}", first_line(&record.body, 96));
        if let Some(by) = record.superseded_by {
            println!("      superseded by #{by}");
        }
    }
    // Say once, at the end, that the marked rows are not being withheld — they
    // are stored, they are shown, and they apply somewhere else.
    if inapplicable > 0 {
        println!(
            "{inapplicable} record(s) do not apply to this tree — their anchors do not \
             resolve here in the same form. Kept and shown, never pruned; they apply \
             wherever those anchors do resolve."
        );
    }
    // Name the hidden rows only when there are some: an invitation to pass
    // `--include-superseded` on a store where nothing has ever been superseded
    // reads as though something is being withheld.
    let hidden = if include_superseded || listing.superseded == 0 {
        ""
    } else {
        " (hidden — pass --include-superseded)"
    };
    println!(
        "{} shown; {} live, {} superseded{hidden}",
        listing.records.len(),
        listing.live,
        listing.superseded,
    );
    Ok(())
}

/// Delete one record — the only path by which anything leaves this store.
fn run_memory_forget(id: i64, json: bool) -> anyhow::Result<()> {
    let (_repo, mut store, _cache) = open_graph()?;
    let Some(forgotten) = store.forget_memory(id)? else {
        anyhow::bail!("no memory record with id {id} (`roteiro memory list` shows what is stored)");
    };
    if json {
        emit_json(&forgotten)?;
        return Ok(());
    }
    println!("forgot memory #{id}");
    if !forgotten.restored.is_empty() {
        // Never silent: these records were hidden on the authority of the one
        // just deleted, and that authority is now gone.
        let ids = forgotten
            .restored
            .iter()
            .map(|r| format!("#{r}"))
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "it had superseded {ids}, which {} live again",
            plural_is(forgotten.restored.len())
        );
    }
    println!("the graph is unchanged");
    Ok(())
}

/// `is`/`are`, for a count.
fn plural_is(n: usize) -> &'static str {
    if n == 1 { "is" } else { "are" }
}

/// How one record relates to the tree it was just resolved against: whether it
/// applies, and on what evidence.
///
/// The two cases that both lack a usable anchor are rendered **differently and
/// unmistakably**, because they have opposite answers and conflating them would
/// make the rule meaningless:
///
/// - *no anchor was ever recorded* — a general lesson about the repository, which
///   applies everywhere: `applies — repo-wide (no anchor)`;
/// - *an anchor was recorded and did not resolve here* — the association is not
///   in this tree in the same form: `does not apply here — vanished: <key>`.
fn applicability(record: &rto_graph::MemoryRecord) -> String {
    let verdict = if record.applies {
        "applies"
    } else {
        "does not apply here"
    };
    match &record.anchor {
        // Anchored: name the state and the key, so the reason is checkable rather
        // than a verdict the operator has to take on trust.
        Some(anchor) => format!("{verdict} — {}: {}", record.anchor_state, anchor.key),
        // Never anchored. Said in words rather than as the bare token
        // `unanchored`, which reads like a failure and is the opposite.
        None => format!("{verdict} — repo-wide (no anchor)"),
    }
}

/// The first line of `body`, truncated to `max` characters, so a multi-line
/// memory occupies one row of a listing. Truncation is marked, never silent.
fn first_line(body: &str, max: usize) -> String {
    let line = body.lines().next().unwrap_or("").trim();
    let multi = body.lines().nth(1).is_some();
    if line.chars().count() <= max && !multi {
        return line.to_owned();
    }
    let head: String = line.chars().take(max).collect();
    format!("{head}…")
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
        exit_gate_failure();
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

/// How a `roteiro links` invocation is scoped: the additive `--workspace <ROOT>`
/// paths and the optional `--workspace-name` selector. Threaded through the
/// authored-links, `--infer`, and `--matrix` reports together.
struct LinksScope<'a> {
    /// Repeatable `--workspace <ROOT>` roots, always unioned into the scope.
    cli_roots: &'a [String],
    /// `--workspace-name <NAME>`: select a configured workspace, or `None` to
    /// default to the one containing the cwd (else today's flat `[workspace]`).
    workspace_name: Option<&'a str>,
}

/// The current repo's working-tree directory (canonicalised), or `None` when the
/// cwd is not inside a git repo. Used to find which configured workspace owns the
/// cwd, at the path level — no graph is opened.
fn cwd_repo_workdir() -> Option<std::path::PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let repo = rto_graph::Repo::discover(&cwd).ok()?;
    let wd = repo.workdir()?.to_path_buf();
    Some(wd.canonicalize().unwrap_or(wd))
}

/// The configured workspace whose **discovered member repos** include `cwd_wd`, or
/// `None` if none do. Membership is decided purely at the path level
/// ([`rto_graph::discover_repos_under`] + explicit repos) — no `Workspace` is built
/// and no graph is opened — so one unrelated **misconfigured** group (an unreadable
/// root) is skipped here rather than aborting selection.
fn workspace_containing_cwd<'a>(
    resolved: &'a [rto_graph::ResolvedWorkspace],
    cwd_wd: &std::path::Path,
) -> Option<&'a rto_graph::ResolvedWorkspace> {
    let canon = |p: &std::path::Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let is_cwd = |p: &std::path::Path| canon(p).as_path() == cwd_wd;
    resolved.iter().find(|rw| {
        // A broken root must not break selection: skip a root that won't read.
        let in_root = rw.roots.iter().any(|root| {
            rto_graph::discover_repos_under(std::path::Path::new(root))
                .is_ok_and(|repos| repos.iter().any(|r| is_cwd(r)))
        });
        in_root
            || rw
                .repos
                .iter()
                .any(|repo| is_cwd(std::path::Path::new(repo)))
    })
}

/// The repos `roteiro links` operates on: the selected workspace's members
/// (`--workspace-name`, else the workspace containing the cwd, else today's flat
/// `[workspace]` scope), unioned with any additive `--workspace <ROOT>` paths and
/// the current repo (so links run inside a spoke resolve against its siblings).
/// Shared by the authored-links, `--infer`, and `--matrix` reports.
///
/// Selection is short-circuited so the **legacy-fallback** path builds and
/// validates nothing beyond today's `[workspace]` scope: only the *one* selected
/// group is discovered, never the whole configured set. So a single unrelated
/// misconfigured `[[workspaces]]` never breaks `links` in directories that should
/// just fall back — a configured workspace is only used when explicitly named or
/// when the cwd actually belongs to it. (For a legacy `[workspace]`-only config the
/// fallback *is* the `default` group's scope, so behaviour is unchanged.)
fn links_scope_paths(
    cfg: &config::Config,
    scope: &LinksScope<'_>,
) -> anyhow::Result<Vec<std::path::PathBuf>> {
    use std::collections::BTreeSet;

    let cli_roots = scope.cli_roots;
    let resolved = cfg.resolved_workspaces()?;

    // Pick the one group to scope to (by name, else cwd-containment) — WITHOUT
    // building/validating the whole set; anything else falls back to the flat scope.
    let chosen: Option<&rto_graph::ResolvedWorkspace> = if let Some(name) = scope.workspace_name {
        // Explicit selection: it must name a configured workspace, else a clear
        // error listing the known ones.
        Some(resolved.iter().find(|r| r.name == name).ok_or_else(|| {
            let known = resolved
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::anyhow!("no workspace named `{name}` (known: {known})")
        })?)
    } else if resolved.is_empty() {
        None
    } else {
        // Default: the workspace containing the cwd repo, else fall back (build
        // nothing) — never eagerly select/validate an unrelated configured group.
        cwd_repo_workdir().and_then(|wd| workspace_containing_cwd(&resolved, &wd))
    };

    let mut paths: BTreeSet<std::path::PathBuf> = BTreeSet::new();
    match chosen {
        Some(rw) => {
            // Discover only the selected group's repo dirs (needed to load each
            // repo's config downstream), unioning the additive `--workspace` roots.
            for root in rw
                .roots
                .iter()
                .map(String::as_str)
                .chain(cli_roots.iter().map(String::as_str))
            {
                paths.extend(rto_graph::discover_repos_under(std::path::Path::new(root))?);
            }
            for repo in &rw.repos {
                paths.insert(std::path::PathBuf::from(repo));
            }
        }
        // No configured workspace selected ⇒ today's flat `[workspace]` + `--workspace`.
        None => paths.extend(collect_workspace_repo_paths(&cfg.workspace, cli_roots)?),
    }

    // Always include the current repo so links run inside a spoke resolve against
    // its siblings.
    if let Ok(cwd) = std::env::current_dir()
        && let Ok(repo) = rto_graph::Repo::discover(&cwd)
        && let Some(wd) = repo.workdir()
    {
        paths.insert(wd.to_path_buf());
    }

    Ok(paths.into_iter().collect())
}

/// Verify a workspace's authored cross-repo links (ADR-0009). For every repo in
/// the workspace (the cwd repo plus any `--workspace`/`[workspace]` roots), read
/// its `[[links]]` and resolve each project-qualified `to` against the other
/// repos' graphs. A target that no longer resolves is **drift** — the cross-repo
/// form of `roteiro check`. Exits non-zero if any link drifts.
fn run_links(cfg: &config::Config, scope: &LinksScope<'_>, json: bool) -> anyhow::Result<()> {
    let paths = links_scope_paths(cfg, scope)?;
    if paths.is_empty() {
        anyhow::bail!(
            "no repos in scope — run inside a repo, pass `--workspace <root>`, or set \
             `[workspace]` in roteiro.toml"
        );
    }
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
        exit_gate_failure();
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

/// Drop build/tooling/CI config keys (see [`rto_graph::is_tooling_config_path`])
/// from every project, then discard any project left with no keys — so
/// `--app-config-only` matches and drift-checks only application config. Used by
/// `roteiro links --infer`/`--matrix`; a no-op unless the flag is set.
fn retain_app_config_keys(
    by_project: &mut std::collections::BTreeMap<String, Vec<infer_links::ConfigKey>>,
) {
    for keys in by_project.values_mut() {
        keys.retain(|k| !rto_graph::is_tooling_config_path(&k.file));
    }
    by_project.retain(|_, keys| !keys.is_empty());
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

/// The cross-repo inference inputs shared by `--infer` and `--matrix`: which repo
/// is the hub, how its version is pinned, and whether to consider only app config
/// (dropping build/tooling/CI keys). Grouped so the two entry points stay under
/// clippy's argument-count limit and thread one value.
#[derive(Clone, Copy)]
struct InferOptions<'a> {
    hub: Option<&'a str>,
    pin: PinnedHub<'a>,
    app_config_only: bool,
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
    scope: &LinksScope<'_>,
    opts: InferOptions<'_>,
) -> anyhow::Result<InferScan> {
    let InferOptions {
        hub,
        pin,
        app_config_only,
    } = opts;
    // Repos in scope: the same selection as `roteiro links` (selected workspace,
    // else today's flat `[workspace]` scope, plus `--workspace` roots and the cwd).
    let paths = links_scope_paths(cfg, scope)?;
    if paths.is_empty() {
        return Ok(InferScan::Nothing(
            "no repos in scope; run inside a repo, pass `--workspace <root>`, or set `[workspace]`"
                .to_owned(),
        ));
    }

    let (mut by_project, project_paths, unsynced) = collect_workspace_config_keys(&paths)?;
    // `--app-config-only`: drop build/tooling/CI config keys from every repo before
    // matching, so cross-repo correspondences and drift compare only app config.
    // Opt-in — off by default, so matching is unchanged unless the flag is passed.
    if app_config_only {
        retain_app_config_keys(&mut by_project);
    }
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
        let mut keys = config_keys_at_rev(hub_path, rev, pin.ingest)
            .map_err(|e| anyhow::anyhow!("resolving hub `{hub_name}` at `{rev}`: {e}"))?;
        // Keep the pinned hub's keys consistent with the filtered spokes.
        if app_config_only {
            keys.retain(|k| !rto_graph::is_tooling_config_path(&k.file));
        }
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
    scope: &LinksScope<'_>,
    opts: InferOptions<'_>,
    write: bool,
    json: bool,
) -> anyhow::Result<()> {
    // Having nothing to infer is a **successful no-op** (exit 0) — `--infer` is
    // informational, so a CI script can run it opportunistically in a single repo
    // without failing — but still say why.
    let ready = match scan_workspace_infer(cfg, scope, opts)? {
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
    scope: &LinksScope<'_>,
    opts: InferOptions<'_>,
    html: bool,
    out: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    let ready = match scan_workspace_infer(cfg, scope, opts)? {
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
                        // The hub key's source file, so the matrix row can be
                        // classified as app vs tooling config (parity with the API).
                        file: m.hub_file.clone(),
                        spoke_key: m.spoke_key.clone(),
                        spoke_value: vals
                            .get(&(m.spoke_file.as_str(), m.spoke_key.as_str()))
                            .copied()
                            .unwrap_or("")
                            .to_owned(),
                        confidence: m.confidence,
                        // `links --infer` matches are, by definition, inferred.
                        provenance: rto_graph::Provenance::Inferred,
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

/// Dispatch a `roteiro security` action.
#[cfg(feature = "execution")]
fn run_security(action: SecurityAction) -> anyhow::Result<()> {
    match action {
        SecurityAction::Ingest {
            file,
            analyzer,
            json,
        } => run_security_ingest(&file, analyzer.as_deref(), json),
        SecurityAction::List { analyzer, json } => run_security_list(analyzer.as_deref(), json),
        #[cfg(feature = "exec-subprocess")]
        SecurityAction::Run {
            analyzer,
            allow_unsandboxed,
            json,
        } => run_security_run(&analyzer, allow_unsandboxed, json),
        #[cfg(feature = "exec-subprocess")]
        SecurityAction::Prefetch { analyzer, json } => {
            run_security_prefetch(analyzer.as_deref(), json)
        }
        #[cfg(feature = "exec-subprocess")]
        SecurityAction::Status { analyzer, json } => run_security_status(analyzer.as_deref(), json),
    }
}

/// The `--json` shape of `roteiro security ingest`.
#[cfg(feature = "execution")]
#[derive(serde::Serialize)]
struct SecurityIngestReport {
    /// The replaceable layer that was written.
    layer: String,
    /// The analyzer the report came from, and its version.
    analyzer: String,
    analyzer_version: String,
    /// Which backend produced the result, and what isolation it really had.
    runner: rto_graph::RunnerKind,
    isolation: rto_graph::Isolation,
    /// Findings written, and owned rows removed from the previous layer.
    findings: usize,
    removed: usize,
    /// Whether a previous run of this layer was replaced.
    replaced: bool,
    /// SHA-256 of the exact report bytes these findings came from.
    report_digest: String,
}

/// The `--json` shape of `roteiro security list`.
#[cfg(feature = "execution")]
#[derive(serde::Serialize)]
struct SecurityListing {
    /// Every live layer with its run evidence and findings.
    layers: Vec<rto_graph::FindingsLayer>,
    /// Total findings across those layers.
    findings: usize,
}

/// Just enough of a normalized report to learn which analyzer it claims to be
/// from, and whether it is a normalized report at all.
#[cfg(feature = "execution")]
#[derive(serde::Deserialize)]
struct AnalyzerPeek {
    /// Present only on a normalized report. Native analyzer output has no such
    /// field, which is exactly how the two are told apart.
    #[serde(default)]
    schema: Option<String>,
    #[serde(default)]
    analyzer: Option<String>,
}

/// Which analyzer a report file belongs to, and whether it is already
/// normalized.
///
/// A normalized report names its own analyzer, so `--analyzer` is not needed and
/// is ignored if given (the runner re-checks the report against the request
/// anyway, so nothing is weakened by peeking). Native output names nothing, so
/// `--analyzer` is required — and guessing from the JSON's shape is deliberately
/// not done: two analyzers' formats could overlap, and silently attributing a
/// report to the wrong tool would mis-key every finding in it.
#[cfg(feature = "execution")]
fn report_analyzer(
    bytes: &[u8],
    source: &str,
    requested: Option<&str>,
) -> anyhow::Result<(String, bool)> {
    let peek: AnalyzerPeek = serde_json::from_slice(bytes).map_err(|e| {
        anyhow::anyhow!("{source} is not JSON, so it is not an analyzer report: {e}")
    })?;

    if peek.schema.as_deref() == Some(rto_exec::REPORT_SCHEMA) {
        let analyzer = peek.analyzer.ok_or_else(|| {
            anyhow::anyhow!("{source} claims to be a normalized report but names no analyzer")
        })?;
        return Ok((analyzer, false));
    }

    let analyzer = requested.ok_or_else(|| {
        anyhow::anyhow!(
            "{source} is not a normalized `{}` report, so it must be an analyzer's own output —              say which analyzer with `--analyzer <name>` (known: {})",
            rto_exec::REPORT_SCHEMA,
            rto_exec::known_analyzers().join(", ")
        )
    })?;
    Ok((analyzer.to_owned(), true))
}

/// The best available evidence of when a report file was produced: the file's
/// modification time.
///
/// Native analyzer output carries no timestamps — neither `semgrep --json` nor
/// `cargo audit --json` stamps the window it ran in — and an
/// [`rto_graph::AnalysisRun`] must say when it happened. The file's mtime is the
/// only honest answer available, so it is used and documented as such rather
/// than replaced by "now", which would claim the analyzer ran during the ingest.
#[cfg(feature = "execution")]
fn report_written_at(file: &str) -> String {
    std::fs::metadata(file)
        .and_then(|m| m.modified())
        .map_or_else(
            |_| rto_exec::rfc3339_utc(std::time::SystemTime::now()),
            rto_exec::rfc3339_utc,
        )
}

/// The advisory-database evidence this machine has provisioned for `analyzer`.
///
/// Only meaningful in a build with the subprocess backend, which is the build
/// that has an asset cache; otherwise there is nothing provisioned to describe.
#[cfg(feature = "exec-subprocess")]
fn advisory_db_evidence(analyzer: &str) -> Option<rto_graph::AdvisoryDb> {
    rto_exec::assets::advisory_db_evidence(&rto_exec::asset_root(), analyzer)
}

/// Without the subprocess backend there is no asset cache, so there is nothing
/// provisioned to describe — and saying nothing is the honest answer, not a
/// degraded one.
#[cfg(all(feature = "execution", not(feature = "exec-subprocess")))]
fn advisory_db_evidence(_analyzer: &str) -> Option<rto_graph::AdvisoryDb> {
    None
}

/// The git blob id of `Cargo.lock`, when this checkout has one.
///
/// `cargo-audit` keys a finding partly by lockfile blob, so a finding stays
/// distinct when the lockfile changes underneath the same advisory. Computing it
/// the same way on both the ingest and the run path is what makes those two
/// paths produce identical keys.
#[cfg(feature = "execution")]
fn lockfile_blob(repo: &rto_graph::Repo) -> Option<String> {
    let lockfile = repo.workdir()?.join("Cargo.lock");
    repo.blob_oid(&std::fs::read(lockfile).ok()?).ok()
}

/// Ingest a normalized analyzer report as a replaceable findings layer.
///
/// The graph is deliberately **not** built or touched here: an analyzer's verdict
/// is not derived from the tree, so it neither needs a fresh graph nor may alter
/// one. The store is opened, the layer is replaced wholesale, and `nodes`/`edges`
/// are never written — which is what keeps `roteiro export` byte-identical across
/// an ingest.
#[cfg(feature = "execution")]
fn run_security_ingest(file: &str, analyzer: Option<&str>, json: bool) -> anyhow::Result<()> {
    use rto_exec::{AnalysisRequest, AnalyzerRunner, Consent, IngestRunner, Worktree};

    let from_stdin = file == "-";
    let bytes = if from_stdin {
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut std::io::stdin(), &mut buf)?;
        buf
    } else {
        std::fs::read(file)?
    };

    let (analyzer, is_native) = report_analyzer(&bytes, file, analyzer)?;

    let (repo, mut store, _cache) = open_graph()?;
    let worktree_path = repo
        .workdir()
        .unwrap_or_else(|| repo.git_dir())
        .to_path_buf();
    let request = AnalysisRequest {
        analyzer,
        worktree: Worktree::read_only(&worktree_path)?,
        // Egress denied: ingest reads a local file and nothing else.
        network: rto_graph::NetworkPolicy::Deny,
        // Naming a report file on the command line *is* the consent for reading
        // it. A backend that fetches assets or executes a container will have to
        // ask, and this is the field it asks through.
        consent: Consent::Granted,
        // Best-effort source identity from the checkout we are standing in; the
        // report's own record fills whatever this cannot supply.
        source: rto_graph::SourceIdentity {
            commit: repo.head_commit_id().ok(),
            tree: repo.head_tree_id().ok(),
            lockfile_blob: lockfile_blob(&repo),
        },
    };

    // Native output goes through the analyzer's adapter first — the *same*
    // adapter a local `security run` uses on the bytes it captured. That is what
    // makes a CI report and a local run produce identical findings, rather than
    // two conversions that have to be kept in step.
    let bytes = if is_native {
        let written_at = if from_stdin {
            rto_exec::rfc3339_utc(std::time::SystemTime::now())
        } else {
            report_written_at(file)
        };
        let snippets = rto_exec::WorktreeSnippets::new(&worktree_path);
        let ctx = rto_exec::NativeContext {
            started_at: written_at.clone(),
            ended_at: written_at,
            // Native output rarely names the version that produced it; the
            // adapter records "unknown" rather than an empty field.
            analyzer_version: None,
            // The producer's exit status is not in the file. `0` is recorded
            // because the report exists and parsed; the findings themselves are
            // the evidence of what it found.
            exit_status: 0,
            source: &request.source,
            rules_digest: None,
            // The same provisioning record a local `security run` would read, so
            // a CI report and a local run describe the same pinned database.
            // `None` when nothing is provisioned, which is honest: an ingested
            // report says nothing about what this machine has.
            advisory_db: advisory_db_evidence(&request.analyzer),
            snippets: &snippets,
        };
        let report = rto_exec::normalize_native(&request.analyzer, &bytes, &ctx)?;
        serde_json::to_vec(&report)?
    } else {
        bytes
    };

    let response = IngestRunner::new(bytes).run(&request)?;
    let applied = store.replace_findings_layer(&response.run, &response.findings)?;

    if json {
        emit_json(&SecurityIngestReport {
            layer: applied.layer,
            analyzer: response.run.analyzer,
            analyzer_version: response.run.analyzer_version,
            runner: response.run.runner,
            isolation: response.run.isolation,
            findings: applied.findings,
            removed: applied.removed,
            replaced: applied.replaced,
            report_digest: response.run.report_digest,
        })?;
    } else {
        let digest = &response.run.report_digest[..12];
        println!(
            "ingested {} finding(s) from {} {} → {} (runner {}, isolation {}, report {digest}…)",
            applied.findings,
            response.run.analyzer,
            response.run.analyzer_version,
            applied.layer,
            response.run.runner.as_str(),
            response.run.isolation.as_str(),
        );
        if applied.replaced {
            println!(
                "replaced the previous layer: {} finding(s) removed, {} now live",
                applied.removed, applied.findings
            );
        }
        if let Some(db) = &response.run.advisory_db {
            // Say when the advisory data was published and how old that makes
            // it, never that it is current: the same analyzer at the same commit
            // with a newer database legitimately reports something different.
            println!("{}", advisory_db_line(db));
        }
    }
    Ok(())
}

/// List the live findings layers for this worktree.
#[cfg(feature = "execution")]
fn run_security_list(analyzer: Option<&str>, json: bool) -> anyhow::Result<()> {
    let (_repo, store, _cache) = open_graph()?;
    let layers = store.findings_layers(analyzer)?;
    let total: usize = layers.iter().map(|l| l.findings.len()).sum();

    if json {
        emit_json(&SecurityListing {
            layers,
            findings: total,
        })?;
        return Ok(());
    }

    if layers.is_empty() {
        match analyzer {
            Some(name) => println!("no findings ingested for `{name}`"),
            None => println!("no findings ingested (`roteiro security ingest <report.json>`)"),
        }
        return Ok(());
    }
    for layer in &layers {
        println!(
            "{} — {} {} ({}, isolation {}), {} finding(s)",
            layer.run.layer,
            layer.run.analyzer,
            layer.run.analyzer_version,
            layer.run.runner.as_str(),
            layer.run.isolation.as_str(),
            layer.findings.len(),
        );
        for finding in &layer.findings {
            let where_ = finding.path.as_deref().unwrap_or("-");
            println!(
                "  {:<8} {:<24} {where_}  {}",
                finding.severity.as_str(),
                finding.rule,
                finding.title
            );
        }
    }
    println!("{total} finding(s) across {} layer(s)", layers.len());
    Ok(())
}

/// The `--json` shape of `roteiro security run`.
#[cfg(feature = "exec-subprocess")]
#[derive(serde::Serialize)]
struct SecurityRunReport {
    layer: String,
    analyzer: String,
    analyzer_version: String,
    runner: rto_graph::RunnerKind,
    isolation: rto_graph::Isolation,
    /// The exact argv that was executed, so a run is reproducible by hand.
    command: Vec<String>,
    findings: usize,
    removed: usize,
    replaced: bool,
    exit_status: i32,
    started_at: String,
    ended_at: String,
    report_digest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    rules_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    advisory_db: Option<rto_graph::AdvisoryDb>,
}

/// Run an analyzer as a child process on this host and file its findings.
///
/// Everything that makes this defensible happens before a process starts: the
/// `--allow-unsandboxed` flag, the analyzer being one this build knows, and its
/// pinned assets being provisioned. A cold cache therefore fails having executed
/// nothing at all.
#[cfg(feature = "exec-subprocess")]
fn run_security_run(analyzer: &str, allow_unsandboxed: bool, json: bool) -> anyhow::Result<()> {
    use rto_exec::{AnalysisRequest, AnalyzerRunner, Consent, SubprocessRunner, Worktree};

    let runner = SubprocessRunner::new(analyzer, &rto_exec::asset_root(), allow_unsandboxed)?;

    let (repo, mut store, _cache) = open_graph()?;
    let worktree_path = repo
        .workdir()
        .unwrap_or_else(|| repo.git_dir())
        .to_path_buf();
    let request = AnalysisRequest {
        analyzer: analyzer.to_owned(),
        worktree: Worktree::read_only(&worktree_path)?,
        network: rto_graph::NetworkPolicy::Deny,
        // The flag is the consent. It is a separate, explicit act from asking
        // for the run, because what is being consented to — executing a
        // third-party binary on this host with no boundary — is not what
        // "analyze my code" implies.
        consent: Consent::Granted,
        source: rto_graph::SourceIdentity {
            commit: repo.head_commit_id().ok(),
            tree: repo.head_tree_id().ok(),
            lockfile_blob: lockfile_blob(&repo),
        },
    };

    let invocation = runner.invocation();
    if !json {
        // Disclose what is about to run before it runs. A command that executes
        // a third-party binary should never leave the user guessing which one,
        // with which arguments.
        eprintln!(
            "running (isolation none, on this host): {} {}",
            invocation.program,
            invocation.args.join(" ")
        );
    }

    let response = runner.run(&request)?;
    let applied = store.replace_findings_layer(&response.run, &response.findings)?;

    let mut command = vec![invocation.program];
    command.extend(invocation.args);

    if json {
        emit_json(&SecurityRunReport {
            layer: applied.layer,
            analyzer: response.run.analyzer,
            analyzer_version: response.run.analyzer_version,
            runner: response.run.runner,
            isolation: response.run.isolation,
            command,
            findings: applied.findings,
            removed: applied.removed,
            replaced: applied.replaced,
            exit_status: response.run.exit_status,
            started_at: response.run.started_at,
            ended_at: response.run.ended_at,
            report_digest: response.run.report_digest,
            rules_digest: response.run.rules_digest,
            advisory_db: response.run.advisory_db,
        })?;
        return Ok(());
    }

    println!(
        "{} {} produced {} finding(s) → {} (runner {}, isolation {})",
        response.run.analyzer,
        response.run.analyzer_version,
        applied.findings,
        applied.layer,
        response.run.runner.as_str(),
        response.run.isolation.as_str(),
    );
    println!(
        "  isolation none — the analyzer ran on this host. Its egress was configured off and its \
         inputs were pinned, but nothing enforced that."
    );
    if let Some(digest) = &response.run.rules_digest {
        println!("  rules {digest}");
    }
    if let Some(db) = &response.run.advisory_db {
        println!("  {}", advisory_db_line(db));
    }
    if applied.replaced {
        println!(
            "  replaced the previous layer: {} finding(s) removed, {} now live",
            applied.removed, applied.findings
        );
    }
    Ok(())
}

/// One line describing an advisory database's identity, publication date and
/// age — never the word *current*.
///
/// ADR-0012 is explicit: a cached-but-old database still runs, but its results
/// are labelled *possibly stale*. Saying how old, in days, is what turns that
/// label from a disclaimer into information.
#[cfg(feature = "execution")]
fn advisory_db_line(db: &rto_graph::AdvisoryDb) -> String {
    let Some(published) = db.published_at.as_deref() else {
        return format!(
            "advisory db {} — publication date unknown, so results are possibly stale",
            db.digest
        );
    };
    let now = rto_exec::rfc3339_utc(std::time::SystemTime::now());
    match rto_exec::age_in_days(published, &now) {
        Some(days) => format!(
            "advisory db {} published {published} ({days} day(s) ago) — results are possibly \
             stale, never current: a newer database can legitimately say something different",
            db.digest
        ),
        None => format!(
            "advisory db {} published {published} — results are possibly stale, never current",
            db.digest
        ),
    }
}

/// The `--json` shape of `roteiro security prefetch`.
#[cfg(feature = "exec-subprocess")]
#[derive(serde::Serialize)]
struct SecurityPrefetchReport {
    root: String,
    provisioned: Vec<rto_exec::InstalledAsset>,
}

/// Install and verify every pinned asset, recording each digest.
///
/// This is the only command that writes to the asset cache. It fetches nothing
/// over the network: the rule set is compiled into this binary, and the advisory
/// database is a directory the operator provides — if it is absent, this says
/// where it looked and which command obtains it, and does not go and get it.
#[cfg(feature = "exec-subprocess")]
fn run_security_prefetch(analyzer: Option<&str>, json: bool) -> anyhow::Result<()> {
    let root = rto_exec::asset_root();
    let specs: Vec<&rto_exec::AssetSpec> = match analyzer {
        Some(name) => {
            let specs = rto_exec::assets_for(name);
            if specs.is_empty() {
                anyhow::bail!(
                    "no assets for analyzer `{name}` in this build (known: {})",
                    rto_exec::known_analyzers().join(", ")
                );
            }
            specs
        }
        None => rto_exec::ASSETS.iter().collect(),
    };

    let mut provisioned = Vec::with_capacity(specs.len());
    let mut failures = Vec::new();
    for spec in specs {
        if !json {
            // Disclose source and licence before installing, exactly as
            // `roteiro model pull` does.
            eprintln!(
                "provisioning {} for {} ({}, {})",
                spec.id,
                spec.analyzer,
                spec.kind.as_str(),
                spec.licence
            );
        }
        match rto_exec::provision(&root, spec) {
            Ok(record) => provisioned.push(record),
            // One unprovisionable asset must not hide the others: report it and
            // carry on, then fail at the end with everything that went wrong.
            Err(e) => failures.push(format!("{e}")),
        }
    }

    if json {
        emit_json(&SecurityPrefetchReport {
            root: root.display().to_string(),
            provisioned,
        })?;
    } else {
        for record in &provisioned {
            let files = record
                .files
                .map(|n| format!(" over {n} file(s)"))
                .unwrap_or_default();
            println!(
                "{} → {} (digest {}{files}, fetched {})",
                record.id,
                root.join(&record.id).display(),
                &record.digest[..12.min(record.digest.len())],
                record.fetched_at
            );
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(
            "{} asset(s) could not be provisioned:\n{}",
            failures.len(),
            failures.join("\n")
        )
    }
}

/// The `--json` shape of `roteiro security status`.
#[cfg(feature = "exec-subprocess")]
#[derive(serde::Serialize)]
struct SecurityStatusReport {
    root: String,
    analyzers: Vec<AnalyzerCoverage>,
    assets: Vec<rto_exec::AssetStatus>,
    layers: Vec<LayerStaleness>,
}

/// What one shipped analyzer covers — the coverage matrix, read off the code
/// rather than off a document, so the two cannot drift apart unnoticed.
#[cfg(feature = "exec-subprocess")]
#[derive(serde::Serialize)]
struct AnalyzerCoverage {
    analyzer: &'static str,
    summary: &'static str,
    languages: &'static [&'static str],
    /// Whether every asset it needs is provisioned and still matches its digest.
    ready: bool,
}

/// The staleness of the advisory data behind one live findings layer.
#[cfg(feature = "exec-subprocess")]
#[derive(serde::Serialize)]
struct LayerStaleness {
    layer: String,
    analyzer: String,
    findings: usize,
    runner: rto_graph::RunnerKind,
    isolation: rto_graph::Isolation,
    #[serde(skip_serializing_if = "Option::is_none")]
    advisory_db: Option<rto_graph::AdvisoryDb>,
    /// Days between the advisory database's publication and now.
    #[serde(skip_serializing_if = "Option::is_none")]
    advisory_db_age_days: Option<i64>,
    /// `true` whenever an advisory database is involved at all. Never `false`
    /// meaning "current" — only "this result has no advisory-data axis".
    possibly_stale: bool,
}

/// Report what is provisioned, what it covers, and how old the advisory data
/// behind each live layer is.
///
// One arm per reported section — assets, analyzers, layers — plus their `--json`
// shapes. Splitting it would scatter one screen of output across three
// functions that only ever run together.
#[allow(clippy::too_many_lines)]
#[cfg(feature = "exec-subprocess")]
fn run_security_status(analyzer: Option<&str>, json: bool) -> anyhow::Result<()> {
    let root = rto_exec::asset_root();
    let assets = rto_exec::status(&root, analyzer);

    let analyzers: Vec<AnalyzerCoverage> = rto_exec::ADAPTERS
        .iter()
        .filter(|a| analyzer.is_none_or(|name| a.analyzer() == name))
        .map(|adapter| AnalyzerCoverage {
            analyzer: adapter.analyzer(),
            summary: adapter.summary(),
            languages: adapter.languages(),
            ready: rto_exec::resolve(&root, adapter.analyzer()).is_ok(),
        })
        .collect();

    // Staleness comes from the *runs*, because the advisory database's
    // publication date is something the analyzer reported, not something
    // provisioning could know.
    let now = rto_exec::rfc3339_utc(std::time::SystemTime::now());
    let layers: Vec<LayerStaleness> = open_graph()
        .and_then(|(_repo, store, _cache)| Ok(store.findings_layers(analyzer)?))
        .unwrap_or_default()
        .into_iter()
        .map(|layer| {
            let age = layer
                .run
                .advisory_db
                .as_ref()
                .and_then(|db| db.published_at.as_deref())
                .and_then(|published| rto_exec::age_in_days(published, &now));
            LayerStaleness {
                layer: layer.run.layer,
                analyzer: layer.run.analyzer,
                findings: layer.findings.len(),
                runner: layer.run.runner,
                isolation: layer.run.isolation,
                possibly_stale: layer.run.advisory_db.is_some(),
                advisory_db: layer.run.advisory_db,
                advisory_db_age_days: age,
            }
        })
        .collect();

    if json {
        emit_json(&SecurityStatusReport {
            root: root.display().to_string(),
            analyzers,
            assets,
            layers,
        })?;
        return Ok(());
    }

    println!("asset cache: {}", root.display());
    println!("\nanalyzers");
    for coverage in &analyzers {
        println!(
            "  {:<12} {}  [{}]",
            coverage.analyzer,
            if coverage.ready {
                "ready"
            } else {
                "not provisioned"
            },
            coverage.languages.join(", ")
        );
        println!("               {}", coverage.summary);
    }

    println!("\nassets");
    for asset in &assets {
        match &asset.installed {
            Some(record) => println!(
                "  {:<22} {:<12} digest {} fetched {} ({} day(s) ago){}",
                asset.id,
                asset.kind.as_str(),
                &record.digest[..12.min(record.digest.len())],
                record.fetched_at,
                asset.age_days.unwrap_or_default(),
                if asset.verified == Some(false) {
                    "  ** ON-DISK BYTES NO LONGER MATCH — re-run prefetch **"
                } else {
                    ""
                }
            ),
            None => println!(
                "  {:<22} {:<12} not provisioned — run `roteiro security prefetch`",
                asset.id,
                asset.kind.as_str()
            ),
        }
    }

    if layers.is_empty() {
        println!("\nno findings layers yet");
        return Ok(());
    }
    println!("\nlive findings layers");
    for layer in &layers {
        println!(
            "  {} — {} finding(s), runner {}, isolation {}",
            layer.layer,
            layer.findings,
            layer.runner.as_str(),
            layer.isolation.as_str()
        );
        match (&layer.advisory_db, layer.advisory_db_age_days) {
            (Some(db), Some(days)) => println!(
                "    advisory db {} published {} ({days} day(s) ago) — possibly stale, never current",
                db.digest,
                db.published_at.as_deref().unwrap_or("unknown")
            ),
            (Some(db), None) => println!(
                "    advisory db {} — publication date unreadable, so possibly stale",
                db.digest
            ),
            (None, _) => println!("    no advisory database — this analyzer has no staleness axis"),
        }
    }
    Ok(())
}

/// Serve the read-only graph explorer JSON API over HTTP, **llama-free**
/// (ADR-0008). Builds a [`rto_graph::WorkspaceSet`] from config and serves
/// [`graph_api`]'s router directly on a small tokio runtime — axum only, no
/// `rto-serve`, no model, no MCP, no C/C++ toolchain. No graph is (re)built here:
/// it serves whatever each repo's store already holds (a read-only view;
/// `roteiro sync` refreshes it). The **Ask** tab is deliberately absent — it
/// needs the `serve` build's `/v1/chat/completions`, which this server does not
/// offer.
#[cfg(feature = "explorer")]
fn run_explorer(
    cfg: &config::Config,
    addr: Option<String>,
    workspace_name: Option<&str>,
) -> anyhow::Result<()> {
    use std::sync::Arc;

    // Build the workspace set from config (ADR-0008). When no workspace is
    // configured (the common single-repo case), fall back to hosting the current
    // directory's repo alone, so `roteiro explorer` "just works" with no config.
    let resolved = cfg.resolved_workspaces()?;
    let set = if resolved.is_empty() {
        explorer_cwd_set()?
    } else {
        rto_graph::WorkspaceSet::from_resolved(resolved)?
    };
    if set.names().is_empty() {
        anyhow::bail!(
            "no workspaces to serve — run inside a repo, or configure \
             `[[workspaces]]` / `[standalone]` in roteiro.toml"
        );
    }
    let set = Arc::new(set);

    // Validate an explicit `--workspace-name` once, up front: an unknown name must
    // fail fast — with the existing `UnknownWorkspace` message that lists the known
    // workspaces — rather than booting a server whose flat `/v1/graph/*` routes
    // would then 404 on every request. The cwd-default / single-workspace paths
    // pass no name and are unaffected.
    if let Some(name) = workspace_name {
        set.select(Some(name))?;
    }

    // The default workspace for the flat `/v1/graph/*` routes: the (now-validated)
    // `--workspace-name`, else the workspace containing the current repo. A lone
    // configured workspace resolves itself, so `None` is fine there (see
    // `WorkspaceSet::select`).
    let default = explorer_default_workspace(&set, workspace_name);
    serve_graph_ui(cfg, "explorer", set, default, addr)
}

/// Bind and run the llama-free graph server: the read-only `/v1/graph/*` JSON API
/// merged with the static workspace-explorer web app, on a small current-thread
/// tokio runtime — axum only, no `rto-serve`, no model, no MCP (ADR-0008,
/// ADR-0010). Shared by `roteiro explorer` and the llama-free degrade path of
/// `roteiro serve` (a build without the `serve` feature, or with no model
/// installed). `cmd` names the calling command for the startup line. Blocks until
/// shutdown.
#[cfg(feature = "explorer")]
fn serve_graph_ui(
    cfg: &config::Config,
    cmd: &'static str,
    set: std::sync::Arc<rto_graph::WorkspaceSet>,
    default: Option<String>,
    addr: Option<String>,
) -> anyhow::Result<()> {
    // Address precedence: CLI flag > `[serve] addr` > default loopback.
    let addr = addr
        .or_else(|| cfg.serve.addr.clone())
        .unwrap_or_else(|| "127.0.0.1:8017".to_owned());
    let socket: std::net::SocketAddr = addr
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid {cmd} address `{addr}`: {e}"))?;
    if !socket.ip().is_loopback() {
        eprintln!(
            "warning: binding a non-loopback address ({socket}) — the graph API \
             has no auth; front it with a reverse proxy"
        );
    }

    // The read-only data API plus the served web app (HTML shell, our ES app, and
    // the vendored cytoscape.js) — same-origin, so the app fetches `/v1/graph/*`
    // with no CORS. The Ask tab stays off: this llama-free server has no
    // `/v1/chat/completions`. A `roteiro serve` build with a model installed mounts these same
    // surfaces beside `/v1` (with Ask on) via `mount_explorer_surfaces` instead.
    let router = graph_api::router(set.clone(), default.clone()).merge(explorer_app::router());

    // A small current-thread runtime is all the axum server needs; no rto-serve,
    // no llama.cpp runtime. Blocks until shutdown.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(socket).await?;
        let default_note = default
            .as_deref()
            .map_or_else(String::new, |d| format!(" (default workspace: {d})"));
        eprintln!(
            "roteiro {cmd} listening on http://{socket}/ (UI) — \
             API at http://{socket}/v1/graph — {} workspace(s): {}{default_note}",
            set.names().len(),
            set.names().join(", "),
        );
        axum::serve(listener, router)
            .await
            .map_err(anyhow::Error::from)
    })
}

/// The single-repo fallback for `roteiro explorer`: host the current directory's
/// repo as a lone standalone (`linked:false`) workspace, named after its
/// working-tree directory. Its `graph.db` is opened on demand (read-only) — the
/// explorer never builds a graph, so an unsynced repo simply reports "no graph"
/// per project rather than being silently rebuilt.
#[cfg(feature = "explorer")]
fn explorer_cwd_set() -> anyhow::Result<rto_graph::WorkspaceSet> {
    let cwd = std::env::current_dir()?;
    let repo = rto_graph::Repo::discover(&cwd)?;
    let workdir = repo.workdir().unwrap_or(&cwd);
    let name = workdir
        .file_name()
        .map_or_else(|| "repo".to_owned(), |s| s.to_string_lossy().into_owned());
    let ws = rto_graph::Workspace::from_repo_paths([workdir])?;
    Ok(rto_graph::WorkspaceSet::from_workspaces([(
        name, ws, false,
    )]))
}

/// The default workspace the flat `/v1/graph/*` routes bind to: an explicit
/// `--workspace-name` if given (already validated against `set` at startup, so it
/// is passed through here), else the workspace whose discovered members include
/// the current repo's `graph.db`. `None` lets a single-workspace set resolve its
/// sole workspace (and a multi-workspace set report "ambiguous" until a nested
/// `/workspaces/{ws}/…` route is used).
#[cfg(feature = "explorer")]
fn explorer_default_workspace(
    set: &rto_graph::WorkspaceSet,
    workspace_name: Option<&str>,
) -> Option<String> {
    if let Some(name) = workspace_name {
        return Some(name.to_owned());
    }
    // Reuse the one place the on-disk `<repo>/.git/roteiro/graph.db` layout lives.
    let db = graph_db_path(&std::env::current_dir().ok()?).ok()?;
    set.containing(&db).map(str::to_owned)
}

/// The parsed `serve` flags (from the clap `Command::Serve` arm), bundled so the
/// dispatch stays a single struct rather than a long argument list.
#[cfg(any(feature = "mcp", feature = "serve", feature = "explorer"))]
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

/// The server a `serve`/`mcp` invocation resolves to, factored out of the
/// feature-gated `run_*` functions so the command→backend mapping is unit-testable
/// without binding a socket (see the `cli_routing` tests). `roteiro serve` (no
/// `--http`) is the network HTTP server; `roteiro mcp` is STDIO or, with `--http`,
/// networked MCP; the deprecated `serve --http` also maps to networked MCP.
#[cfg(any(feature = "mcp", feature = "serve", feature = "explorer"))]
// `McpStdio` is only ever produced by `route_mcp`, which exists solely in a
// build carrying the `mcp`/`serve` MCP backend; a pure-`explorer` build never
// constructs it. Silence dead-code there rather than duplicating the enum.
#[cfg_attr(not(any(feature = "mcp", feature = "serve")), allow(dead_code))]
#[derive(Debug, PartialEq, Eq)]
enum ServerRoute {
    /// The network HTTP server: `/v1` (+ Ask) when a model backend is available,
    /// else the llama-free graph API + web UI. `roteiro serve` with no `--http`.
    Network,
    /// The MCP graph server over STDIO. `roteiro mcp` with no `--http`.
    McpStdio,
    /// The MCP graph server over streamable HTTP at ADDR. `roteiro mcp --http`, or
    /// the deprecated `roteiro serve --http`.
    McpHttp(String),
}

/// `roteiro serve` routing: `--http ADDR` (deprecated) → networked MCP, else the
/// network HTTP server. The single source of truth the dispatcher and the routing
/// tests share.
#[cfg(any(feature = "mcp", feature = "serve", feature = "explorer"))]
fn route_serve(http: Option<String>) -> ServerRoute {
    match http {
        Some(addr) => ServerRoute::McpHttp(addr),
        None => ServerRoute::Network,
    }
}

/// `roteiro mcp` routing: `--http ADDR` → networked MCP, else STDIO.
#[cfg(any(feature = "mcp", feature = "serve"))]
fn route_mcp(http: Option<String>) -> ServerRoute {
    match http {
        Some(addr) => ServerRoute::McpHttp(addr),
        None => ServerRoute::McpStdio,
    }
}

/// The one-line stderr deprecation notice (if any) for a `roteiro serve`
/// invocation, so the old-flag → new-command guidance is unit-testable. `None`
/// means the invocation uses the current, non-deprecated surface. `--http` wins
/// over `--models` because it also changes the backend (→ MCP), not just the flag.
#[cfg(any(feature = "mcp", feature = "serve", feature = "explorer"))]
fn serve_deprecation_notice(models: bool, http: Option<&str>) -> Option<String> {
    if let Some(addr) = http {
        Some(format!(
            "note: `roteiro serve --http <ADDR>` is deprecated; use `roteiro mcp --http {addr}`"
        ))
    } else if models {
        Some(
            "note: `roteiro serve --models` is now the default — the `--models` flag is redundant"
                .to_owned(),
        )
    } else {
        None
    }
}

/// Dispatch `roteiro serve`: the **network HTTP server** (ADR-0006/0008/0010). By
/// default the OpenAI-compatible `/v1` model endpoint plus the read-only
/// `/v1/graph/*` API and the `/` web UI; a build without the `serve` feature (or
/// with no model installed) degrades to the llama-free graph API + UI (Ask off)
/// rather than failing. The deprecated `--http`/`--models` flags still work, each
/// with a one-line stderr notice, so existing scripts keep running.
#[cfg(any(feature = "mcp", feature = "serve", feature = "explorer"))]
fn run_serve(
    ingest: rto_graph::IngestConfig,
    cfg: &config::Config,
    opts: &ServeOptions,
    workspace_roots: &[String],
    workspace_name: Option<&str>,
    sync_on_access: bool,
) -> anyhow::Result<()> {
    if let Some(notice) = serve_deprecation_notice(opts.models, opts.http.as_deref()) {
        eprintln!("{notice}");
    }
    match route_serve(opts.http.clone()) {
        ServerRoute::Network => {
            let ws = build_serve_workspaces(
                ingest,
                cfg,
                "serve",
                workspace_roots,
                workspace_name,
                sync_on_access,
            )?;
            run_serve_network(cfg, ws, workspace_name, opts)
        }
        // Deprecated `serve --http ADDR` → the networked MCP server (now `roteiro
        // mcp --http`). Kept so existing MCP-over-HTTP scripts don't break.
        ServerRoute::McpHttp(addr) => {
            #[cfg(any(feature = "mcp", feature = "serve"))]
            {
                let ws = build_serve_workspaces(
                    ingest,
                    cfg,
                    "mcp",
                    workspace_roots,
                    workspace_name,
                    sync_on_access,
                )?;
                serve_mcp(ws.flat, Some(addr))
            }
            #[cfg(not(any(feature = "mcp", feature = "serve")))]
            {
                let _ = (ingest, workspace_roots, sync_on_access);
                anyhow::bail!(
                    "`roteiro serve --http {addr}` (MCP over HTTP) needs the `mcp` feature — \
                     this build has only the llama-free graph server; rebuild with `--features mcp`"
                )
            }
        }
        // `route_serve` never yields STDIO — `serve` is always the network server.
        ServerRoute::McpStdio => unreachable!("`roteiro serve` never routes to STDIO MCP"),
    }
}

/// Dispatch `roteiro mcp`: the **MCP graph server** — STDIO by default, or
/// networked over streamable HTTP with `--http ADDR` (ADR-0002). Builds the same
/// workspace views as `serve` and serves the MCP router over the flattened
/// workspace (`explain`/`search`/`path`/`debt`, per-call `project` selection).
#[cfg(any(feature = "mcp", feature = "serve"))]
fn run_mcp(
    ingest: rto_graph::IngestConfig,
    cfg: &config::Config,
    http: Option<String>,
    workspace_roots: &[String],
    workspace_name: Option<&str>,
    sync_on_access: bool,
) -> anyhow::Result<()> {
    let ws = build_serve_workspaces(
        ingest,
        cfg,
        "mcp",
        workspace_roots,
        workspace_name,
        sync_on_access,
    )?;
    match route_mcp(http) {
        ServerRoute::McpStdio => serve_mcp(ws.flat, None),
        ServerRoute::McpHttp(addr) => serve_mcp(ws.flat, Some(addr)),
        // `route_mcp` only yields the two MCP transports.
        ServerRoute::Network => unreachable!("`roteiro mcp` never routes to the network server"),
    }
}

/// Serve the network HTTP server for `roteiro serve`. Prefers the full model
/// endpoint (`/v1` + graph tools + Ask + UI) when built `--features serve` with a
/// model installed; otherwise degrades to the llama-free graph API + web UI, never
/// hard-failing when a UI can be served. A build that can serve neither reports how
/// to enable one.
#[cfg(any(feature = "mcp", feature = "serve", feature = "explorer"))]
// Which `return` is the tail expression depends on which server backends are
// compiled in (the `serve`/`explorer`/neither blocks below are mutually exclusive
// by `cfg`), so an early `return` that is redundant under one feature set is load-
// bearing under another. Keep them explicit rather than restructuring per-cfg.
#[allow(clippy::needless_return)]
fn run_serve_network(
    cfg: &config::Config,
    ws: ServeWorkspaces,
    workspace_name: Option<&str>,
    opts: &ServeOptions,
) -> anyhow::Result<()> {
    let ServeWorkspaces { set, flat } = ws;

    // The full model server: `/v1` (+ graph tools + Ask + UI). Only when a model is
    // actually installed — otherwise fall through to the llama-free UI below.
    #[cfg(feature = "serve")]
    {
        if !served_models(cfg).is_empty() {
            return serve_models_endpoint(cfg, set, flat, workspace_name, opts);
        }
    }

    // Degrade: the llama-free `/v1/graph/*` API + web UI (Ask off) — today's
    // `explorer` behaviour, so a non-`serve` build (or a `serve` build with no
    // model) still serves something useful instead of erroring (point 3).
    #[cfg(feature = "explorer")]
    {
        if opts.mcp {
            eprintln!(
                "note: `--mcp` is ignored here — there is no `/v1` model server to mount `/mcp` \
                 beside (no model / no `serve` feature); run `roteiro mcp` for the MCP server"
            );
        }
        let default = explorer_default_workspace(&set, workspace_name);
        let _ = &flat; // the llama-free UI uses `set`; `flat` backs the model tools only.
        return serve_graph_ui(cfg, "serve", set, default, opts.addr.clone());
    }

    #[cfg(not(feature = "explorer"))]
    {
        let _ = (cfg, &set, &flat, workspace_name, opts);
        #[cfg(feature = "serve")]
        anyhow::bail!(
            "no installed GGUF models to serve — pull one \
             (`roteiro model pull qwen3-0.6b`; see `roteiro model list`), or rebuild with \
             `--features explorer` for the llama-free graph API + web UI"
        );
        #[cfg(not(feature = "serve"))]
        anyhow::bail!(
            "this build has no network server — for the MCP graph server use `roteiro mcp`; \
             rebuild with `--features serve` (model endpoint) or `--features explorer` (graph UI)"
        );
    }
}

/// Build the two workspace views a `serve`/`mcp` process holds — `set` (the full
/// multi-workspace [`rto_graph::WorkspaceSet`] backing the read-only `/v1/graph/*`
/// API + UI) and `flat` (one workspace over every hosted project, backing the model
/// tools + MCP router) — from config plus any `--workspace <ROOT>` roots (ADR-0008).
/// Shared by [`run_serve`] and [`run_mcp`]. `cmd` names the caller for the startup
/// line. A lone repo with no workspace config builds its graph now and hosts it
/// alone as `default` (the one path still needing a git cwd).
#[cfg(any(feature = "mcp", feature = "serve", feature = "explorer"))]
fn build_serve_workspaces(
    ingest: rto_graph::IngestConfig,
    cfg: &config::Config,
    cmd: &str,
    workspace_roots: &[String],
    workspace_name: Option<&str>,
    sync_on_access: bool,
) -> anyhow::Result<ServeWorkspaces> {
    use std::sync::Arc;

    // The same source of truth `roteiro explorer` uses (`Config::resolved_workspaces()`:
    // the legacy `[workspace]` folded to `default`, every `[[workspaces]]`, and
    // `[standalone]`), plus any explicit `--workspace <ROOT>` folded into `default`.
    let resolved = cfg.resolved_workspaces()?;

    // The true single-repo fallback fires ONLY when nothing selects a workspace: no
    // configured workspaces, no `--workspace <ROOT>`, and no `--workspace-name`. Then
    // build the current repo's graph now and host it alone as `default` (this is the
    // one path that still needs a git cwd — a lone repo with no config still "just
    // works", sharing the one store handle between `set` and `flat`).
    if resolved.is_empty() && workspace_roots.is_empty() && workspace_name.is_none() {
        let (repo, mut store, cache) = open_graph()?;
        build_graph(&repo, &mut store, &cache, ingest, GraphSource::Committed)?;
        let name = repo
            .workdir()
            .and_then(std::path::Path::file_name)
            .map_or_else(|| "repo".to_owned(), |s| s.to_string_lossy().into_owned());
        let flat = Arc::new(rto_graph::Workspace::single(name, store));
        let set = Arc::new(rto_graph::WorkspaceSet::from_single(
            "default",
            flat.clone(),
            flat.is_multi(),
        ));
        return Ok(ServeWorkspaces { set, flat });
    }

    // Multi-workspace serve: host every configured workspace, plus any explicit
    // `--workspace <ROOT>` (folded into `default`). `set` backs the read-only
    // `/v1/graph/*` API and the served UI, workspace-aware. `flat` is one workspace
    // over EVERY project across ALL those workspaces, backing the model tool registry,
    // the `/v1/{project}/…` routing, and the MCP router. Existing graphs are opened on
    // demand; SIGHUP reloads the set of repos, and `--sync-on-access` (re)builds a
    // project's graph on first touch (ADR-0008).
    let effective = fold_cli_roots(resolved, workspace_roots);
    let set = Arc::new(rto_graph::WorkspaceSet::from_resolved(effective.clone())?);
    // A friendly error when nothing resolves — an empty config, only stale roots, or a
    // `-w` naming nothing to serve — BEFORE `from_repo_paths` would surface a raw
    // `WorkspaceError::Empty`. Mirrors `run_explorer`'s message.
    if set.names().is_empty() {
        anyhow::bail!(
            "no workspaces to serve — run inside a repo, pass `--workspace <ROOT>`, \
             or configure `[[workspaces]]` / `[standalone]` in roteiro.toml"
        );
    }
    // Validate `--workspace-name` once, up front: an unknown name fails fast (listing
    // the known workspaces) rather than booting a server whose flat routes would 404.
    if let Some(name) = workspace_name {
        set.select(Some(name))?;
    }
    let paths = resolved_repo_paths(&effective, &[])?;
    let mut ws = rto_graph::Workspace::from_repo_paths(&paths)?;
    if sync_on_access {
        ws = ws.with_on_open(Arc::new(move |db: &std::path::Path| {
            sync_project_graph(db, ingest).map_err(|e| e.to_string())
        }));
    }
    let flat = Arc::new(ws);
    eprintln!(
        "roteiro {cmd}: {} workspace(s) [{}] — {} project(s){} — {}",
        set.names().len(),
        set.names().join(", "),
        flat.names().len(),
        if sync_on_access {
            ", sync-on-access"
        } else {
            ""
        },
        flat.names().join(", ")
    );
    install_workspace_reload(&flat, cfg.clone(), workspace_roots.to_vec());
    Ok(ServeWorkspaces { set, flat })
}

/// The two workspace views a `serve` process holds. `set` is the full
/// multi-workspace [`rto_graph::WorkspaceSet`] (ADR-0008) that backs the read-only
/// `/v1/graph/*` API and the served explorer UI — workspace-aware, listed at
/// `GET /v1/graph/workspaces`. `flat` is a single [`rto_graph::Workspace`] over
/// **every** project across all those workspaces, backing the model tool registry,
/// the `/v1/{project}/…` chat routing, and the MCP router — so the served model can
/// query any hosted project by name. For the single-repo fallback the two share the
/// one store handle; otherwise `flat` opens each project's store on demand.
#[cfg(any(feature = "mcp", feature = "serve", feature = "explorer"))]
struct ServeWorkspaces {
    /// The full multi-workspace set (read-only graph API + UI).
    set: std::sync::Arc<rto_graph::WorkspaceSet>,
    /// One flattened workspace over every hosted project (model tools + MCP).
    flat: std::sync::Arc<rto_graph::Workspace>,
}

/// The union of member repo paths across every resolved workspace group, plus any
/// additive `--workspace <ROOT>` paths — the repo set the flattened model
/// [`rto_graph::Workspace`] hosts (and the SIGHUP reload re-scans). Discovered the
/// same way [`rto_graph::WorkspaceSet::from_resolved`] discovers each group's repos
/// (roots scanned + explicit repos), deduplicated by path so a repo named in two
/// groups is hosted once.
#[cfg(any(feature = "mcp", feature = "serve", feature = "explorer"))]
fn resolved_repo_paths(
    resolved: &[rto_graph::ResolvedWorkspace],
    cli_roots: &[String],
) -> anyhow::Result<Vec<std::path::PathBuf>> {
    use std::collections::BTreeSet;
    let mut seen: BTreeSet<std::path::PathBuf> = BTreeSet::new();
    let mut out: Vec<std::path::PathBuf> = Vec::new();
    let mut push = |p: std::path::PathBuf, out: &mut Vec<std::path::PathBuf>| {
        // Dedupe by the canonical path where possible, so the same repo reached via
        // two groups (or a root + an explicit repo) is hosted once.
        let key = p.canonicalize().unwrap_or_else(|_| p.clone());
        if seen.insert(key) {
            out.push(p);
        }
    };
    for root in cli_roots {
        for repo in rto_graph::discover_repos_under(std::path::Path::new(root))? {
            push(repo, &mut out);
        }
    }
    for rw in resolved {
        for root in &rw.roots {
            for repo in rto_graph::discover_repos_under(std::path::Path::new(root))? {
                push(repo, &mut out);
            }
        }
        for repo in &rw.repos {
            push(std::path::PathBuf::from(repo), &mut out);
        }
    }
    Ok(out)
}

/// Fold explicit `--workspace <ROOT>` CLI roots into the resolved workspace groups as
/// the `default` workspace (ADR-0008): unioned into an existing `default` (the legacy
/// `[workspace]`), else added as a new linked `default` group. So repos a user names
/// on the command line are hosted as a first-class named workspace — surfaced by the
/// read-only graph API AND reachable by the served model — exactly like configured
/// workspaces, rather than being merged only into the flat model view. No roots ⇒ the
/// groups are returned unchanged. `default` is the only name derivable from CLI roots
/// (it never collides with a `[[workspaces]]`/`[standalone]` name, which are distinct
/// and, for a legacy `[workspace]`, already fold to `default`).
#[cfg(any(feature = "mcp", feature = "serve", feature = "explorer"))]
fn fold_cli_roots(
    mut resolved: Vec<rto_graph::ResolvedWorkspace>,
    cli_roots: &[String],
) -> Vec<rto_graph::ResolvedWorkspace> {
    if cli_roots.is_empty() {
        return resolved;
    }
    match resolved.iter_mut().find(|r| r.name == "default") {
        Some(default) => default.roots.extend(cli_roots.iter().cloned()),
        None => resolved.push(rto_graph::ResolvedWorkspace {
            name: "default".to_owned(),
            roots: cli_roots.to_vec(),
            repos: Vec::new(),
            linked: true,
        }),
    }
    resolved
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
    // Expand a leading `~` in every root/repo (config- or CLI-sourced) so git
    // never receives a literal `~`, matching the new multi-workspace path
    // (`Config::resolved_workspaces`).
    for root in roots {
        repo_paths.extend(rto_graph::discover_repos_under(&config::expand_tilde(
            root,
        ))?);
    }
    for repo in ws_cfg.repos.iter().flatten() {
        repo_paths.push(config::expand_tilde(repo).into_owned());
    }
    Ok(repo_paths)
}

/// `serve --sync-on-access` hook: (re)build the graph for the repo whose store
/// is `graph_db` (`<repo>/.git/roteiro/graph.db`), before it is first served.
/// Rebuilds from the committed tree, matching how the freshness hooks sync.
#[cfg(any(feature = "mcp", feature = "serve", feature = "explorer"))]
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
#[cfg(all(unix, any(feature = "mcp", feature = "serve", feature = "explorer")))]
fn install_workspace_reload(
    ws: &std::sync::Arc<rto_graph::Workspace>,
    cfg: config::Config,
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
                // Re-derive the full repo set the same way startup did: every
                // configured workspace's members (`resolved_workspaces()`) plus the
                // additive `--workspace <ROOT>` paths — so a SIGHUP picks up repos
                // added/removed across ALL workspaces, not just the legacy block.
                let result = cfg
                    .resolved_workspaces()
                    .and_then(|resolved| resolved_repo_paths(&resolved, &cli_roots))
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
#[cfg(all(
    not(unix),
    any(feature = "mcp", feature = "serve", feature = "explorer")
))]
fn install_workspace_reload(
    _ws: &std::sync::Arc<rto_graph::Workspace>,
    _cfg: config::Config,
    _cli_roots: Vec<String>,
) {
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

/// The served model ids eligible to back **chat / Ask**, in preference order.
///
/// Embedding models (BERT encoders like `bge-*`) are **excluded**: they cannot
/// generate, and routing one through `/v1/chat/completions` aborts llama.cpp's
/// decode path with a `GGML_ASSERT` (see [`rto_serve::llama`]'s chat guard). The
/// remaining chat-capable models (generative + vision) are returned with the
/// **default first** — the Ask UI sends `models[0]` — resolved as: the configured
/// `[models] generative` model when it is served and generative, else the first
/// served generative model, else the first chat-capable model. Never an embedding
/// model. `served_ids` is the engine's served set (registry names), so every id
/// resolves in the registry.
///
/// Gated on `explorer` too: the Ask model pool only exists when the explorer UI
/// (and its `/v1/graph/capabilities` route) is compiled in.
#[cfg(all(feature = "serve", feature = "explorer"))]
fn chat_capable_model_ids(cfg: &config::Config, served_ids: &[String]) -> Vec<String> {
    use rto_graph::{ModelKind, find_model};
    let is_generative = |id: &str| find_model(id).is_some_and(|s| s.kind == ModelKind::Generative);
    // Drop embedding-only models; keep generative + vision (both can chat).
    let mut ids: Vec<String> = served_ids
        .iter()
        .filter(|id| !find_model(id).is_some_and(|s| s.kind == ModelKind::Embedding))
        .cloned()
        .collect();
    // Pick the default and rotate it to the front, preserving the order of the
    // rest (a stable, predictable capabilities list).
    let default_pos = cfg
        .models
        .generative
        .as_deref()
        .and_then(|g| ids.iter().position(|id| id == g && is_generative(id)))
        .or_else(|| ids.iter().position(|id| is_generative(id)));
    if let Some(pos) = default_pos {
        ids[..=pos].rotate_right(1);
    }
    ids
}

#[cfg(all(test, feature = "serve", feature = "explorer"))]
mod chat_model_selection {
    use super::{chat_capable_model_ids, config};

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn embedding_models_are_excluded_and_a_generative_is_default() {
        // The bug's exact served order: bge (embedding) FIRST, then generatives.
        // The Ask pool must drop bge and lead with a generative, so `models[0]`
        // (what the UI sends) can never be the crashing embedding model.
        let served = ids(&["bge-small-en-v1.5-gguf", "qwen3-0.6b", "qwen3-8b"]);
        let out = chat_capable_model_ids(&config::Config::default(), &served);
        assert_eq!(out, ids(&["qwen3-0.6b", "qwen3-8b"]));
        assert!(!out.iter().any(|m| m.contains("bge")), "no embedding model");
    }

    #[test]
    fn configured_generative_is_preferred_as_the_default() {
        // `[models] generative` wins the default slot when served and generative.
        let mut cfg = config::Config::default();
        cfg.models.generative = Some("qwen3-8b".to_owned());
        let served = ids(&["bge-small-en-v1.5-gguf", "qwen3-0.6b", "qwen3-8b"]);
        let out = chat_capable_model_ids(&cfg, &served);
        assert_eq!(out, ids(&["qwen3-8b", "qwen3-0.6b"]));
    }

    #[test]
    fn vision_models_stay_in_the_pool_but_a_generative_leads() {
        // Vision models can chat, so they remain — but a generative is the default.
        let served = ids(&["bge-small-en-v1.5-gguf", "smolvlm-500m-gguf", "qwen3-0.6b"]);
        let out = chat_capable_model_ids(&config::Config::default(), &served);
        assert_eq!(out, ids(&["qwen3-0.6b", "smolvlm-500m-gguf"]));
    }

    #[test]
    fn with_no_generative_a_vision_model_leads_never_an_embedding() {
        // Only an embedding + a vision model served: the embedding is dropped and
        // the (chat-capable) vision model becomes the default — never the encoder.
        let served = ids(&["bge-small-en-v1.5-gguf", "smolvlm-500m-gguf"]);
        let out = chat_capable_model_ids(&config::Config::default(), &served);
        assert_eq!(out, ids(&["smolvlm-500m-gguf"]));
    }

    #[test]
    fn an_embedding_only_serve_offers_no_chat_model() {
        // Nothing chat-capable ⇒ empty pool ⇒ the UI keeps Ask disabled (no request
        // with an embedding model is ever sent).
        let served = ids(&["bge-small-en-v1.5-gguf"]);
        assert!(chat_capable_model_ids(&config::Config::default(), &served).is_empty());
    }
}

#[cfg(feature = "serve")]
fn serve_models_endpoint(
    cfg: &config::Config,
    set: std::sync::Arc<rto_graph::WorkspaceSet>,
    flat: std::sync::Arc<rto_graph::Workspace>,
    workspace_name: Option<&str>,
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
    serve_v1_tail(
        cfg,
        ServeSurfaces {
            set,
            flat,
            workspace_name,
        },
        opts,
        engine,
        socket,
        tls,
        &names,
    )
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

/// The workspace surfaces a `roteiro serve` model-server process serves, bundled so
/// [`serve_v1_tail`] stays within the argument-count budget: the full multi-workspace
/// `set` (read-only `/v1/graph/*` API + UI, explorer builds) and the flattened `flat`
/// workspace over every hosted project (model tools + MCP), plus the validated
/// `--workspace-name` that picks the default flat-route workspace.
#[cfg(feature = "serve")]
struct ServeSurfaces<'a> {
    /// The full configured workspace set (read-only graph API + served UI).
    set: std::sync::Arc<rto_graph::WorkspaceSet>,
    /// One flattened workspace over every hosted project (model tools + MCP).
    flat: std::sync::Arc<rto_graph::Workspace>,
    /// The validated `--workspace-name` the flat `/v1/graph/*` routes default to.
    workspace_name: Option<&'a str>,
}

/// A served tool registry behind a trait object, shared into the router.
#[cfg(feature = "serve")]
type SharedToolRegistry = std::sync::Arc<dyn rto_serve::ToolRegistry>;

/// Per-workspace tool registries, keyed by workspace name — each confined to that
/// workspace's projects, backing `/v1/workspaces/{ws}/chat/completions` (ADR-0008).
#[cfg(feature = "serve")]
type WorkspaceToolRegistries = std::collections::HashMap<String, SharedToolRegistry>;

/// Assemble the graph tools and serve the endpoint: `/v1` alone, or — with
/// `--mcp` — `/v1` **and** `/mcp` merged on one port (ADR-0008). Blocks until
/// shutdown.
#[cfg(feature = "serve")]
fn serve_v1_tail(
    cfg: &config::Config,
    surfaces: ServeSurfaces<'_>,
    opts: &ServeOptions,
    engine: std::sync::Arc<dyn rto_serve::Engine>,
    socket: std::net::SocketAddr,
    tls: Option<(std::path::PathBuf, std::path::PathBuf)>,
    names: &str,
) -> anyhow::Result<()> {
    let ServeSurfaces {
        set,
        flat,
        workspace_name,
    } = surfaces;
    // `set` (the full workspace set) and `workspace_name` back the read-only
    // `/v1/graph/*` API + UI, only mounted in an `explorer` build. `set` is also read
    // here to build the per-workspace registries — but only when tools are enabled —
    // so keep the non-explorer discard to cover the tools-disabled case; the model
    // tools and MCP router use the flattened `flat` workspace regardless.
    #[cfg(not(feature = "explorer"))]
    let _ = (&set, workspace_name);
    let scheme = if tls.is_some() { "https" } else { "http" };
    // Auto-register the graph tools (ADR-0006) unless disabled, so the served model
    // can `explain`/`search`/`path`/`debt`. Two registries share the one on/off
    // switch (ADR-0008): `tools` is the flattened view over EVERY hosted project of
    // every workspace (backing the unscoped `/v1/chat/completions`, `/v1/{project}/…`
    // and MCP), while `workspace_tools` holds one registry per configured workspace,
    // each confined to that workspace's OWN projects, over the same store handles the
    // set already holds. The per-workspace registries back
    // `/v1/workspaces/{ws}/chat/completions`, so a workspace-level Ask cannot see or
    // answer about a project outside the selected workspace.
    let (tools, workspace_tools): (Option<SharedToolRegistry>, WorkspaceToolRegistries) =
        if cfg.serve.tools.unwrap_or(true) {
            let flat_tools: SharedToolRegistry =
                std::sync::Arc::new(GraphToolRegistry::new(flat.clone()));
            let per_ws: WorkspaceToolRegistries = set
                .workspace_handles()
                .into_iter()
                .map(|(name, ws)| {
                    let reg: SharedToolRegistry = std::sync::Arc::new(GraphToolRegistry::new(ws));
                    (name, reg)
                })
                .collect();
            (Some(flat_tools), per_ws)
        } else {
            (None, WorkspaceToolRegistries::new())
        };
    let tools_note = if tools.is_some() {
        " (graph tools on)"
    } else {
        ""
    };

    // The chat-capable served model ids, captured before the engine is moved into
    // the router, so an explorer build can advertise them as the Ask capability
    // (below). Embedding models are filtered out and the default is placed first —
    // the Ask UI must never send an embedding model to `/v1/chat/completions`.
    #[cfg(feature = "explorer")]
    let model_ids: Vec<String> = {
        let served_ids: Vec<String> = engine.models().into_iter().map(|m| m.id).collect();
        chat_capable_model_ids(cfg, &served_ids)
    };

    // Build the `/v1` model router, then merge any extra read-only/MCP surfaces
    // onto it — all sharing one port and one Workspace (ADR-0008). `/v1/graph` and
    // `/mcp` are just axum path prefixes, so the routers merge. Serving the router
    // directly is equivalent to `serve_blocking[_with_tools]` (they build the same
    // app), so this path also covers the plain (`/v1`-only) case.
    let router = match tools {
        Some(tools) => rto_serve::app_with_workspace_tools(engine, tools, workspace_tools),
        None => rto_serve::app(engine),
    };

    // With the explorer UI compiled in (`--features serve,explorer`), a
    // `roteiro serve` process with a model installed is the single coherent way to run the whole
    // explorer + Ask experience (ADR-0010): mount the read-only `/v1/graph/*` data
    // API AND the static web app beside `/v1`, and advertise the chat endpoint the
    // model router already exposes so the UI enables its Ask tab. The engine built
    // above backs both `/v1/chat/completions` and the graph tools — nothing is
    // duplicated; the pure `explorer` build never reaches here and keeps Ask off.
    #[cfg(feature = "explorer")]
    let router = {
        // The workspace the flat `/v1/graph/*` routes bind to: the validated
        // `--workspace-name`, else the one containing the cwd, else the sole
        // configured workspace (mirrors `run_explorer`).
        let default = explorer_default_workspace(&set, workspace_name);
        mount_explorer_surfaces(router, set, default, model_ids)
    };
    #[cfg(feature = "explorer")]
    let graph_note = " + /v1/graph + / (UI, Ask on)";
    #[cfg(not(feature = "explorer"))]
    let graph_note = "";

    // `--models --mcp`: also mount the MCP graph server at `/mcp` on the SAME port.
    if opts.mcp {
        #[cfg(feature = "mcp")]
        {
            let combined = router.merge(rto_render::mcp::mcp_router(flat));
            eprintln!(
                "roteiro server listening on {scheme}://{socket} — /v1{tools_note}{graph_note} + /mcp — serving: {names}"
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
            "`roteiro serve --mcp` needs the `mcp` feature (build with `--features serve,mcp`)"
        );
    }

    eprintln!(
        "roteiro model server listening on {scheme}://{socket}/v1{tools_note}{graph_note} — serving: {names}"
    );
    match tls {
        Some((cert, key)) => rto_serve::serve_blocking_router_tls(router, socket, &cert, &key),
        None => rto_serve::serve_blocking_router(router, socket),
    }
}

/// Merge the explorer's read-only data API and its static web app onto a
/// `roteiro serve` model-server router, advertising the mounted Ask (chat) endpoint. The graph
/// API is multi-workspace-aware (ADR-0008): `set` is the FULL configured
/// [`rto_graph::WorkspaceSet`], so `GET /v1/graph/workspaces` lists every hosted
/// workspace and each is reachable both flat (via `default`) and under
/// `/v1/graph/workspaces/{ws}/…`. `default` names the workspace the flat routes
/// bind to (`--workspace-name`, else the cwd's, else the sole one). `model_ids` are
/// the served generative models, surfaced in `/v1/graph/capabilities` so the web
/// app can name the model backing Ask. Factored out so the wiring is unit-testable
/// with a mock engine (no llama.cpp).
#[cfg(all(feature = "serve", feature = "explorer"))]
fn mount_explorer_surfaces(
    router: axum::Router,
    set: std::sync::Arc<rto_graph::WorkspaceSet>,
    default: Option<String>,
    model_ids: Vec<String>,
) -> axum::Router {
    let caps = crate::graph_api::Capabilities {
        ask: true,
        models: model_ids,
    };
    router
        .merge(crate::graph_api::router_with_capabilities(
            set, default, caps,
        ))
        .merge(crate::explorer_app::router())
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
                              top matches with keys and, for content-bearing nodes, a short \
                              `snippet` of the node's actual content to ground your answer; \
                              curated ADRs/blueprints and READMEs rank first, so this is the \
                              entry point for \"what is X / why\" questions. Read the `snippet`, \
                              and call `explain` on a returned key for the full content."
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

// The `roteiro media` surface: argument shapes, the config narrowing that turns
// flags into a [`rto_graph::MediaBuildOptions`], and the `--json` shape callers
// parse. Ungated, exactly as [`MediaAction`] is — the store, its status and its
// clearing exist in every build, so these shapes must hold in every build too.
//
// The behaviour they wrap — incrementality, the pre-generation gate, artifact
// purity — is tested where it lives, in `rto-graph`.
#[cfg(test)]
mod media_cli {
    use super::{Cli, Command, MediaAction, MediaClearReport, media_options};
    use clap::Parser as _;

    fn parse<const N: usize>(args: [&str; N]) -> Command {
        Cli::try_parse_from(args).expect("parse").command
    }

    fn action<const N: usize>(args: [&str; N]) -> MediaAction {
        let Command::Media { action } = parse(args) else {
            panic!("expected Media");
        };
        action
    }

    /// `(audio, vision, blob, force, json)`. Every toggle off is the shape
    /// `media_options` reads as "both modalities".
    fn build<const N: usize>(args: [&str; N]) -> (bool, bool, Option<String>, bool, bool) {
        let MediaAction::Build {
            audio,
            vision,
            blob,
            force,
            json,
        } = action(args)
        else {
            panic!("expected Build");
        };
        (audio, vision, blob, force, json)
    }

    #[test]
    fn build_takes_no_flags_and_defaults_them_all_off() {
        assert_eq!(
            build(["roteiro", "media", "build"]),
            (false, false, None, false, false)
        );
    }

    #[test]
    fn build_accepts_each_modality_and_the_force_and_json_flags() {
        assert_eq!(
            build(["roteiro", "media", "build", "--audio", "--force", "--json"]),
            (true, false, None, true, true)
        );
        assert_eq!(
            build(["roteiro", "media", "build", "--vision"]),
            (false, true, None, false, false)
        );
        // Both named explicitly is the same request as neither, and must parse.
        assert_eq!(
            build(["roteiro", "media", "build", "--audio", "--vision"]),
            (true, true, None, false, false)
        );
    }

    /// The per-blob rebuild the explorer's rebuild action hands the operator:
    /// one blob id, plus `--force` to actually redo it.
    #[test]
    fn build_narrows_to_one_blob() {
        assert_eq!(
            build(["roteiro", "media", "build", "--blob", "a1b2c3d4", "--force",]),
            (false, false, Some("a1b2c3d4".to_owned()), true, false)
        );
        // `--blob` takes a value; bare, it is a parse error rather than a
        // silent whole-tree rebuild.
        assert!(Cli::try_parse_from(["roteiro", "media", "build", "--blob"]).is_err());
    }

    #[test]
    fn status_takes_only_json() {
        let MediaAction::Status { json } = action(["roteiro", "media", "status"]) else {
            panic!("expected Status");
        };
        assert!(!json);
        let MediaAction::Status { json } = action(["roteiro", "media", "status", "--json"]) else {
            panic!("expected Status");
        };
        assert!(json);
    }

    #[test]
    fn clear_defaults_to_every_producer_and_can_narrow() {
        let MediaAction::Clear { producer, json } = action(["roteiro", "media", "clear"]) else {
            panic!("expected Clear");
        };
        assert_eq!(producer, None);
        assert!(!json);

        let MediaAction::Clear { producer, json } = action([
            "roteiro",
            "media",
            "clear",
            "--producer",
            "media:audio:voxtral-mini-3b:0123456789abcdef",
            "--json",
        ]) else {
            panic!("expected Clear");
        };
        assert_eq!(
            producer.as_deref(),
            Some("media:audio:voxtral-mini-3b:0123456789abcdef"),
            "a producer id is taken verbatim — `:` must not be treated as a separator"
        );
        assert!(json);
    }

    #[test]
    fn media_requires_an_action() {
        assert!(Cli::try_parse_from(["roteiro", "media"]).is_err());
        assert!(Cli::try_parse_from(["roteiro", "media", "nonsense"]).is_err());
    }

    #[test]
    fn no_modality_flag_means_both() {
        let opts = media_options(
            rto_graph::IngestConfig::default(),
            rto_graph::GateThresholds::default(),
            false,
            false,
            false,
        )
        .expect("both modalities are allowed by default");
        assert!(opts.audio && opts.vision);
        assert!(!opts.force);
        // The gate's thresholds arrive intact — a build must not quietly run
        // with the gate off because the plumbing dropped them.
        assert_eq!(opts.thresholds, rto_graph::GateThresholds::default());
    }

    /// `[media] gate = false` reaches the build as thresholds nothing can fall
    /// below, which is how the gate is turned off without a second flag for
    /// every call site to remember.
    #[test]
    fn a_disabled_gate_resolves_to_thresholds_that_refuse_nothing() {
        let cfg = crate::config::MediaConfig {
            gate: Some(false),
            silence_rms: Some(0.5),
            image_variance: Some(0.5),
        };
        assert_eq!(cfg.resolve(), rto_graph::GateThresholds::disabled());

        // Unset values fall back to the conservative defaults, and a set one
        // overrides only itself.
        let tuned = crate::config::MediaConfig {
            gate: None,
            silence_rms: Some(0.01),
            image_variance: None,
        }
        .resolve();
        assert_eq!(
            tuned,
            rto_graph::GateThresholds {
                silence_rms: 0.01,
                ..rto_graph::GateThresholds::default()
            }
        );
    }

    #[test]
    fn a_modality_disabled_in_config_is_dropped_silently_but_refused_when_asked_for() {
        let no_audio = rto_graph::IngestConfig {
            audio: false,
            ..rto_graph::IngestConfig::default()
        };
        let gate = rto_graph::GateThresholds::default();
        // Implicit: narrow to what is permitted, no error.
        let opts =
            media_options(no_audio, gate, false, false, false).expect("vision is still allowed");
        assert!(!opts.audio && opts.vision);

        // Explicit: the operator asked for something the configuration forbids,
        // and is told so rather than silently getting nothing.
        let err = media_options(no_audio, gate, true, false, false).expect_err("must be refused");
        let message = err.to_string();
        assert!(
            message.contains("[ingest] audio = false"),
            "the refusal must name the setting: {message}"
        );
    }

    #[test]
    fn disabling_both_modalities_leaves_nothing_to_do() {
        let none = rto_graph::IngestConfig {
            audio: false,
            vision: false,
            ..rto_graph::IngestConfig::default()
        };
        let err = media_options(
            none,
            rto_graph::GateThresholds::default(),
            false,
            false,
            false,
        )
        .expect_err("must be refused");
        assert!(
            err.to_string().contains("nothing for `media build` to do"),
            "unhelpful error: {err}"
        );
    }

    #[test]
    fn the_clear_json_shape_is_the_documented_one() {
        let value = serde_json::to_value(MediaClearReport {
            producer: Some("media:audio:voxtral-mini-3b:0123456789abcdef".to_owned()),
            removed: 3,
        })
        .expect("serialize");
        assert_eq!(value["removed"], 3);
        assert_eq!(
            value["producer"],
            "media:audio:voxtral-mini-3b:0123456789abcdef"
        );

        // Clearing everything reports a `null` producer, not an absent key: a
        // caller can tell "all producers" from "this one" without guessing.
        let value = serde_json::to_value(MediaClearReport {
            producer: None,
            removed: 0,
        })
        .expect("serialize");
        assert!(value["producer"].is_null());
    }
}

// The `roteiro memory` surface: argument shapes, the defaults, and the one-line
// rendering a listing uses. Ungated, exactly as [`MemoryAction`] is — the store
// is `SQLite` and serde, so these shapes hold in every build and on every
// platform, with no model and no network.
//
// The behaviour they wrap — anchoring and drift, supersession, artifact purity,
// rebuild survival — is tested where it lives, in `rto-graph`.
#[cfg(test)]
mod memory_cli {
    use super::{Cli, Command, MemoryAction, applicability, first_line, plural_is};
    use clap::Parser as _;

    fn parse<const N: usize>(args: [&str; N]) -> Command {
        Cli::try_parse_from(args).expect("parse").command
    }

    fn action<const N: usize>(args: [&str; N]) -> MemoryAction {
        let Command::Memory { action } = parse(args) else {
            panic!("expected Memory");
        };
        action
    }

    /// The defaults are the whole ergonomics of `memory add`: a body, and
    /// nothing else to think about.
    #[test]
    fn add_needs_only_a_body_and_defaults_the_rest() {
        let MemoryAction::Add {
            body,
            kind,
            scope,
            anchor,
            confidence,
            supersedes,
            json,
        } = action(["roteiro", "memory", "add", "what I learned"])
        else {
            panic!("expected Add");
        };
        assert_eq!(body, "what I learned");
        assert_eq!(kind, rto_graph::MemoryKind::Lesson);
        assert_eq!(scope, rto_graph::DEFAULT_MEMORY_SCOPE);
        assert_eq!(anchor, None);
        assert_eq!(confidence, None);
        assert_eq!(supersedes, None);
        assert!(!json);

        // A body is required: `memory add` with nothing to record is a mistake,
        // not an empty record.
        assert!(Cli::try_parse_from(["roteiro", "memory", "add"]).is_err());
    }

    #[test]
    fn add_accepts_every_flag() {
        let MemoryAction::Add {
            body,
            kind,
            scope,
            anchor,
            confidence,
            supersedes,
            json,
        } = action([
            "roteiro",
            "memory",
            "add",
            "-",
            "--kind",
            "attempt",
            "--scope",
            "feat/stage23",
            "--anchor",
            "sym:rust:src/a.rs#f",
            "--confidence",
            "0.8",
            "--supersedes",
            "7",
            "--json",
        ])
        else {
            panic!("expected Add");
        };
        assert_eq!(body, "-", "`-` reaches the handler, which reads stdin");
        assert_eq!(kind, rto_graph::MemoryKind::Attempt);
        assert_eq!(scope, "feat/stage23");
        assert_eq!(anchor.as_deref(), Some("sym:rust:src/a.rs#f"));
        assert_eq!(confidence, Some(0.8));
        assert_eq!(supersedes, Some(7));
        assert!(json);
    }

    /// The kind vocabulary is closed at the command line too, and the refusal
    /// lists what is accepted rather than only saying no.
    #[test]
    fn an_unknown_kind_is_refused_with_the_vocabulary() {
        for kind in rto_graph::MemoryKind::ALL {
            let MemoryAction::Add { kind: parsed, .. } =
                action(["roteiro", "memory", "add", "body", "--kind", kind.as_str()])
            else {
                panic!("expected Add");
            };
            assert_eq!(parsed, kind);
        }
        let Err(err) = Cli::try_parse_from(["roteiro", "memory", "add", "body", "--kind", "note"])
        else {
            panic!("an unknown kind must not parse");
        };
        let err = err.to_string();
        assert!(
            err.contains("lesson"),
            "the refusal must name the set: {err}"
        );
    }

    #[test]
    fn list_defaults_to_live_records_only() {
        let MemoryAction::List {
            scope,
            kind,
            anchor,
            include_superseded,
            limit,
            json,
        } = action(["roteiro", "memory", "list"])
        else {
            panic!("expected List");
        };
        assert_eq!((scope, kind, anchor), (None, None, None));
        assert!(
            !include_superseded,
            "superseded knowledge is hidden unless asked for",
        );
        assert_eq!(limit, 50);
        assert!(!json);
    }

    #[test]
    fn list_narrows_by_scope_kind_and_anchor() {
        let MemoryAction::List {
            scope,
            kind,
            anchor,
            include_superseded,
            limit,
            json,
        } = action([
            "roteiro",
            "memory",
            "list",
            "--scope",
            "repo",
            "--kind",
            "decision",
            "--anchor",
            "sym:rust:src/a.rs#f",
            "--include-superseded",
            "--limit",
            "5",
            "--json",
        ])
        else {
            panic!("expected List");
        };
        assert_eq!(scope.as_deref(), Some("repo"));
        assert_eq!(kind, Some(rto_graph::MemoryKind::Decision));
        assert_eq!(anchor.as_deref(), Some("sym:rust:src/a.rs#f"));
        assert!(include_superseded);
        assert_eq!(limit, 5);
        assert!(json);
    }

    /// `forget` takes exactly one id. Bare, it is a parse error rather than a
    /// command that might mean "forget everything".
    #[test]
    fn forget_requires_one_id() {
        let MemoryAction::Forget { id, json } = action(["roteiro", "memory", "forget", "12"])
        else {
            panic!("expected Forget");
        };
        assert_eq!(id, 12);
        assert!(!json);
        assert!(Cli::try_parse_from(["roteiro", "memory", "forget"]).is_err());
        assert!(Cli::try_parse_from(["roteiro", "memory", "forget", "abc"]).is_err());
    }

    /// A listing row is one line, and truncation is marked — a body silently cut
    /// at the terminal width would misreport what is stored.
    #[test]
    fn a_listing_row_is_one_marked_line() {
        assert_eq!(first_line("short", 96), "short");
        assert_eq!(first_line("  padded  ", 96), "padded");
        assert_eq!(
            first_line("first\nsecond", 96),
            "first…",
            "a second line is truncation and must be marked as such",
        );
        assert_eq!(
            first_line(&"x".repeat(100), 10),
            format!("{}…", "x".repeat(10))
        );
        // Counted in characters, not bytes, so a multi-byte body cannot panic on
        // a split boundary.
        assert_eq!(first_line("ééééé", 3), "ééé…");
        assert_eq!(first_line("", 96), "");
    }

    #[test]
    fn restored_records_are_pluralised() {
        assert_eq!(plural_is(1), "is");
        assert_eq!(plural_is(2), "are");
    }

    /// Build a record with a given anchor state, to check how a listing renders
    /// it. Only the fields `applicability` reads matter here.
    fn record(anchor: Option<&str>, state: rto_graph::AnchorState) -> rto_graph::MemoryRecord {
        rto_graph::MemoryRecord {
            id: 1,
            scope: rto_graph::DEFAULT_MEMORY_SCOPE.to_owned(),
            kind: rto_graph::MemoryKind::Lesson,
            anchor: anchor.map(|key| rto_graph::MemoryAnchor {
                key: key.to_owned(),
                blob: Some("blob1".to_owned()),
                path: None,
            }),
            anchor_state: state,
            applies: state.applies(),
            body: "a body".to_owned(),
            confidence: None,
            tree: None,
            created_at: "2026-08-15 12:00:00".to_owned(),
            superseded_by: None,
            superseded_at: None,
        }
    }

    /// **The two no-usable-anchor cases must not read alike.** A record that never
    /// anchored is repo-wide and applies; a record whose anchor failed to resolve
    /// does not apply here. They are opposite answers, so a listing that rendered
    /// them the same way would hide the whole scope rule behind a shrug.
    #[test]
    fn a_record_with_no_anchor_reads_differently_from_one_that_failed_to_resolve() {
        use rto_graph::AnchorState;

        let none = applicability(&record(None, AnchorState::Unanchored));
        assert_eq!(none, "applies — repo-wide (no anchor)");
        assert!(
            !none.contains("unanchored"),
            "the bare token reads like a failure and this is the opposite: {none}",
        );

        let vanished = applicability(&record(
            Some("sym:rust:src/gone.rs#dead"),
            AnchorState::Vanished,
        ));
        assert_eq!(
            vanished,
            "does not apply here — vanished: sym:rust:src/gone.rs#dead"
        );
        assert_ne!(none, vanished, "opposite answers must not render alike");

        // The reason is always named alongside the verdict, so an operator can
        // check it rather than take it on trust.
        for (state, token) in [
            (AnchorState::Valid, "applies — valid"),
            (AnchorState::Drifted, "does not apply here — drifted"),
            (
                AnchorState::Unverifiable,
                "does not apply here — unverifiable",
            ),
        ] {
            let line = applicability(&record(Some("sym:rust:a.rs#f"), state));
            assert!(line.starts_with(token), "{state}: {line}");
            assert!(line.ends_with("sym:rust:a.rs#f"), "{state}: {line}");
        }
    }
}

// The `roteiro security` surface: argument shapes, the analyzer peek that keeps
// `ingest` a one-argument command, and the `--json` shapes callers parse. The
// behaviour these wrap — layer replacement, idempotence, artifact purity — is
// tested where it lives, in `rto-graph` and `rto-exec`.
#[cfg(all(test, feature = "execution"))]
mod security_cli {
    use super::{
        Cli, Command, SecurityAction, SecurityIngestReport, SecurityListing, report_analyzer,
    };
    use clap::Parser as _;

    fn parse<const N: usize>(args: [&str; N]) -> Command {
        Cli::try_parse_from(args).expect("parse").command
    }

    fn action<const N: usize>(args: [&str; N]) -> SecurityAction {
        let Command::Security { action } = parse(args) else {
            panic!("expected Security");
        };
        action
    }

    #[test]
    fn ingest_takes_one_report_argument() {
        let SecurityAction::Ingest {
            file,
            analyzer,
            json,
        } = action(["roteiro", "security", "ingest", "r.json"])
        else {
            panic!("expected Ingest");
        };
        assert_eq!(file, "r.json");
        assert_eq!(analyzer, None, "a normalized report names its own analyzer");
        assert!(!json);
    }

    #[test]
    fn ingest_reads_stdin_and_emits_json_like_its_neighbours() {
        let SecurityAction::Ingest {
            file,
            analyzer,
            json,
        } = action(["roteiro", "security", "ingest", "-", "--json"])
        else {
            panic!("expected Ingest");
        };
        assert_eq!(file, "-", "`-` is stdin, matching `roteiro load`");
        assert_eq!(analyzer, None);
        assert!(json);
    }

    #[test]
    fn ingest_takes_an_analyzer_for_native_output() {
        let SecurityAction::Ingest { analyzer, .. } = action([
            "roteiro",
            "security",
            "ingest",
            "--analyzer",
            "semgrep",
            "semgrep.json",
        ]) else {
            panic!("expected Ingest");
        };
        assert_eq!(analyzer.as_deref(), Some("semgrep"));
    }

    #[test]
    fn list_defaults_to_every_analyzer_and_can_narrow() {
        let SecurityAction::List { analyzer, json } = action(["roteiro", "security", "list"])
        else {
            panic!("expected List");
        };
        assert_eq!(analyzer, None);
        assert!(!json);

        let SecurityAction::List { analyzer, json } = action([
            "roteiro",
            "security",
            "list",
            "--analyzer",
            "cargo-audit",
            "--json",
        ]) else {
            panic!("expected List");
        };
        assert_eq!(analyzer.as_deref(), Some("cargo-audit"));
        assert!(json);
    }

    #[test]
    fn security_requires_an_action() {
        assert!(Cli::try_parse_from(["roteiro", "security"]).is_err());
    }

    #[test]
    fn the_analyzer_is_read_out_of_a_normalized_report() {
        let report = br#"{"schema":"roteiro.findings/v1","analyzer":"cargo-audit"}"#;
        let (analyzer, native) = report_analyzer(report, "r.json", None).expect("peek");
        assert_eq!(analyzer, "cargo-audit");
        assert!(!native);

        // A normalized report names its own analyzer, so `--analyzer` cannot
        // relabel it — the runner would refuse the mismatch anyway, and letting
        // the flag win here would only move the error further from its cause.
        let (analyzer, _) = report_analyzer(report, "r.json", Some("semgrep")).expect("peek");
        assert_eq!(analyzer, "cargo-audit");
    }

    /// Native output names no analyzer, and its format is *not* guessed from the
    /// JSON's shape: two analyzers' formats could overlap, and attributing a
    /// report to the wrong tool would mis-key every finding in it.
    #[test]
    fn native_output_requires_an_explicit_analyzer() {
        let native = br#"{"version":"1.136.0","results":[]}"#;
        let err = report_analyzer(native, "sg.json", None).expect_err("must ask");
        let message = err.to_string();
        assert!(message.contains("--analyzer"), "{message}");
        assert!(
            message.contains("semgrep"),
            "it must list what it knows: {message}"
        );

        let (analyzer, is_native) =
            report_analyzer(native, "sg.json", Some("semgrep")).expect("peek");
        assert_eq!(analyzer, "semgrep");
        assert!(is_native);
    }

    #[test]
    fn a_file_that_is_not_json_fails_with_a_message_naming_it() {
        let err = report_analyzer(b"not json", "r.json", None).expect_err("must fail");
        let message = err.to_string();
        assert!(
            message.contains("r.json") && message.contains("not JSON"),
            "unhelpful error: {message}"
        );
    }

    #[test]
    fn the_json_shapes_are_the_documented_ones() {
        let report = SecurityIngestReport {
            layer: "security:cargo-audit:ab12cd34".to_owned(),
            analyzer: "cargo-audit".to_owned(),
            analyzer_version: "0.21.0".to_owned(),
            runner: rto_graph::RunnerKind::Ingested,
            isolation: rto_graph::Isolation::Ingested,
            findings: 2,
            removed: 3,
            replaced: true,
            report_digest: "abc".to_owned(),
        };
        let value = serde_json::to_value(&report).expect("serialize");
        assert_eq!(value["runner"], "ingested");
        assert_eq!(value["isolation"], "ingested");
        assert_eq!(value["removed"], 3);
        assert_eq!(value["replaced"], true);

        let listing = SecurityListing {
            layers: Vec::new(),
            findings: 0,
        };
        let value = serde_json::to_value(&listing).expect("serialize");
        assert_eq!(value["findings"], 0);
        assert!(value["layers"].as_array().expect("array").is_empty());
    }

    /// The flag that accepts an unsandboxed run is required, and it is a flag
    /// rather than a default — a build with this backend must not be able to
    /// execute a third-party binary because somebody forgot to say no.
    #[cfg(feature = "exec-subprocess")]
    #[test]
    fn run_requires_the_unsandboxed_flag_to_be_given_explicitly() {
        let SecurityAction::Run {
            analyzer,
            allow_unsandboxed,
            json,
        } = action(["roteiro", "security", "run", "semgrep"])
        else {
            panic!("expected Run");
        };
        assert_eq!(analyzer, "semgrep");
        assert!(
            !allow_unsandboxed,
            "the flag must default to off; the runner refuses without it"
        );
        assert!(!json);

        let SecurityAction::Run {
            allow_unsandboxed, ..
        } = action([
            "roteiro",
            "security",
            "run",
            "semgrep",
            "--allow-unsandboxed",
        ])
        else {
            panic!("expected Run");
        };
        assert!(allow_unsandboxed);
    }

    #[cfg(feature = "exec-subprocess")]
    #[test]
    fn run_needs_an_analyzer_named() {
        assert!(Cli::try_parse_from(["roteiro", "security", "run"]).is_err());
    }

    #[cfg(feature = "exec-subprocess")]
    #[test]
    fn prefetch_and_status_default_to_every_analyzer() {
        let SecurityAction::Prefetch { analyzer, json } =
            action(["roteiro", "security", "prefetch"])
        else {
            panic!("expected Prefetch");
        };
        assert_eq!(analyzer, None);
        assert!(!json);

        let SecurityAction::Status { analyzer, json } = action([
            "roteiro",
            "security",
            "status",
            "--analyzer",
            "semgrep",
            "--json",
        ]) else {
            panic!("expected Status");
        };
        assert_eq!(analyzer.as_deref(), Some("semgrep"));
        assert!(json);
    }

    /// ADR-0012: a cached-but-old advisory database still runs, but its results
    /// are labelled *possibly stale* — **never** *current*. That word is the
    /// contract, so it is checked rather than trusted to review.
    #[cfg(feature = "execution")]
    #[test]
    fn the_advisory_line_says_possibly_stale_and_never_current() {
        use super::advisory_db_line;

        let dated = advisory_db_line(&rto_graph::AdvisoryDb {
            digest: "ec5f7ef0".to_owned(),
            published_at: Some("2020-01-01T00:00:00Z".to_owned()),
        });
        assert!(dated.contains("possibly stale"), "{dated}");
        assert!(dated.contains("ec5f7ef0"), "{dated}");
        assert!(
            dated.contains("day(s) ago"),
            "an age makes it actionable: {dated}"
        );
        assert!(
            !dated.contains("is current") && !dated.contains("up to date"),
            "a database must never be described as current: {dated}"
        );

        // An unknown publication date is *more* reason to say possibly stale,
        // not less.
        let undated = advisory_db_line(&rto_graph::AdvisoryDb {
            digest: "ec5f7ef0".to_owned(),
            published_at: None,
        });
        assert!(undated.contains("possibly stale"), "{undated}");
    }
}

// Command → backend routing for `roteiro serve` / `roteiro mcp`: prove that the
// new default `serve` is the network server, `mcp` is the STDIO/HTTP MCP server,
// and each deprecated alias still parses, routes to the right backend, and carries
// its one-line deprecation notice. Pure parsing/dispatch — no socket is bound.
// Gated on ANY server backend so the routing is exercised in every valid combo —
// `serve`-only and `mcp`-only builds route commands too, not just `serve,explorer`.
// The `serve`-surface items exist under any of the three features; the MCP-specific
// items (`route_mcp`, the `Command::Mcp` variant) need `mcp`/`serve`, so the tests
// that touch them carry a narrower `#[cfg]` of their own.
#[cfg(all(test, any(feature = "serve", feature = "mcp", feature = "explorer")))]
mod cli_routing {
    #[cfg(any(feature = "mcp", feature = "serve"))]
    use super::route_mcp;
    use super::{Cli, Command, ServerRoute, route_serve, serve_deprecation_notice};
    use clap::Parser as _;

    fn parse<const N: usize>(args: [&str; N]) -> Command {
        Cli::try_parse_from(args).expect("parse").command
    }

    #[test]
    fn bare_serve_routes_to_the_network_server() {
        let Command::Serve {
            http, models, mcp, ..
        } = parse(["roteiro", "serve"])
        else {
            panic!("expected Serve");
        };
        assert_eq!(http, None);
        assert!(!models);
        assert!(!mcp);
        assert_eq!(route_serve(http), ServerRoute::Network);
        assert_eq!(serve_deprecation_notice(models, None), None);
    }

    // `route_mcp` and the `Command::Mcp` variant exist only with the `mcp`/`serve`
    // MCP backend; a pure-`explorer` build has no `mcp` command to route.
    #[cfg(any(feature = "mcp", feature = "serve"))]
    #[test]
    fn mcp_routes_to_stdio_by_default() {
        let Command::Mcp { http, .. } = parse(["roteiro", "mcp"]) else {
            panic!("expected Mcp");
        };
        assert_eq!(http, None);
        assert_eq!(route_mcp(http), ServerRoute::McpStdio);
    }

    #[cfg(any(feature = "mcp", feature = "serve"))]
    #[test]
    fn mcp_http_routes_to_networked_mcp() {
        let Command::Mcp { http, .. } = parse(["roteiro", "mcp", "--http", "127.0.0.1:8080"])
        else {
            panic!("expected Mcp");
        };
        assert_eq!(
            route_mcp(http),
            ServerRoute::McpHttp("127.0.0.1:8080".to_owned())
        );
    }

    #[cfg(any(feature = "mcp", feature = "serve"))]
    #[test]
    fn mcp_carries_the_workspace_options() {
        let Command::Mcp {
            workspace,
            workspace_name,
            sync_on_access,
            ..
        } = parse([
            "roteiro",
            "mcp",
            "-w",
            "api",
            "--workspace",
            "/repos",
            "--sync-on-access",
        ])
        else {
            panic!("expected Mcp");
        };
        assert_eq!(workspace, vec!["/repos".to_owned()]);
        assert_eq!(workspace_name.as_deref(), Some("api"));
        assert!(sync_on_access);
    }

    // Deprecated `serve --models`: still parses, still routes to the network server
    // (now the default), and emits the "redundant" notice.
    #[test]
    fn deprecated_serve_models_is_the_default_with_a_notice() {
        let Command::Serve { http, models, .. } = parse(["roteiro", "serve", "--models"]) else {
            panic!("expected Serve");
        };
        assert!(models);
        assert_eq!(route_serve(http.clone()), ServerRoute::Network);
        let notice = serve_deprecation_notice(models, http.as_deref()).expect("notice");
        assert!(
            notice.contains("--models") && notice.contains("default"),
            "unexpected notice: {notice}"
        );
    }

    // Deprecated `serve --http ADDR`: still parses, routes to networked MCP, and
    // points at `roteiro mcp --http`.
    #[test]
    fn deprecated_serve_http_routes_to_mcp_with_a_notice() {
        let Command::Serve { http, models, .. } =
            parse(["roteiro", "serve", "--http", "127.0.0.1:9"])
        else {
            panic!("expected Serve");
        };
        assert_eq!(
            route_serve(http.clone()),
            ServerRoute::McpHttp("127.0.0.1:9".to_owned())
        );
        let notice = serve_deprecation_notice(models, http.as_deref()).expect("notice");
        assert!(
            notice.contains("roteiro mcp --http"),
            "unexpected notice: {notice}"
        );
    }

    // The deprecated MCP path (`--http`) and the network server (`--models`) are
    // mutually exclusive, so an ambiguous mix is rejected at parse time.
    #[test]
    fn serve_http_conflicts_with_models() {
        assert!(
            Cli::try_parse_from(["roteiro", "serve", "--http", "127.0.0.1:9", "--models"]).is_err()
        );
    }
}

// Per-workspace graph-tool registries (ADR-0008): each configured workspace gets
// a `GraphToolRegistry` confined to its OWN projects, built from
// `WorkspaceSet::workspace_handles`. These back the workspace-scoped Ask, so a
// workspace-level question can never see or answer about a project outside the
// selected workspace. Serve-gated (the registry is), driven purely from in-memory
// stores — no engine, no HTTP, no llama.cpp.
#[cfg(all(test, feature = "serve"))]
mod workspace_scoped_tools {
    use super::GraphToolRegistry;

    /// A two-workspace set — `api` (project `api`) and `docs` (project `docs`) —
    /// built from in-memory stores, plus the flattened workspace over BOTH, exactly
    /// as `serve` holds them. Returns `(set, flat)`.
    fn two_workspace_set() -> (
        std::sync::Arc<rto_graph::WorkspaceSet>,
        std::sync::Arc<rto_graph::Workspace>,
    ) {
        let ws_api =
            rto_graph::Workspace::single("api", rto_graph::Store::open_in_memory().expect("store"));
        let ws_docs = rto_graph::Workspace::single(
            "docs",
            rto_graph::Store::open_in_memory().expect("store"),
        );
        let set = std::sync::Arc::new(rto_graph::WorkspaceSet::from_workspaces([
            ("api".to_owned(), ws_api, true),
            ("docs".to_owned(), ws_docs, false),
        ]));
        let flat = std::sync::Arc::new(rto_graph::Workspace::from_stores([
            ("api", rto_graph::Store::open_in_memory().expect("store")),
            ("docs", rto_graph::Store::open_in_memory().expect("store")),
        ]));
        (set, flat)
    }

    /// The `GraphToolRegistry` for one named workspace of the set (as `serve_v1_tail`
    /// builds them via `WorkspaceSet::workspace_handles`).
    fn registry_for(set: &rto_graph::WorkspaceSet, ws: &str) -> GraphToolRegistry {
        let handle = set
            .workspace_handles()
            .into_iter()
            .find(|(name, _)| name == ws)
            .map(|(_, h)| h)
            .expect("workspace present");
        GraphToolRegistry::new(handle)
    }

    #[test]
    fn list_projects_returns_only_the_selected_workspaces_projects() {
        use rto_serve::ToolRegistry as _;
        let (set, flat) = two_workspace_set();

        // The per-workspace registry lists ONLY its own project.
        let api = registry_for(&set, "api");
        let out = api.call("list_projects", &serde_json::json!({})).unwrap();
        let json: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            json["projects"],
            serde_json::json!(["api"]),
            "the `api` workspace Ask must list only `api`"
        );
        assert_eq!(api.projects(), vec!["api".to_owned()]);

        let docs = registry_for(&set, "docs");
        let out = docs.call("list_projects", &serde_json::json!({})).unwrap();
        let json: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(json["projects"], serde_json::json!(["docs"]));

        // Contrast: the flattened registry (unscoped route) still spans both, so the
        // narrowing is real, not an artefact of the fixture.
        let flat = GraphToolRegistry::new(flat);
        let out = flat.call("list_projects", &serde_json::json!({})).unwrap();
        let json: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(json["projects"], serde_json::json!(["api", "docs"]));
    }

    #[test]
    fn a_tool_call_for_an_out_of_workspace_project_is_refused() {
        use rto_serve::ToolRegistry as _;
        let (set, _flat) = two_workspace_set();
        let api = registry_for(&set, "api");

        // Naming `docs` (another workspace's project) from the `api` workspace Ask is
        // an error, not a silent answer from `docs`'s graph.
        let err = api
            .call(
                "explain",
                &serde_json::json!({ "key": "fn:x", "project": "docs" }),
            )
            .expect_err("out-of-workspace project must be refused");
        assert!(
            err.contains("no project named `docs`"),
            "the refusal names the unknown project (was: {err})"
        );

        // A project-qualified key into another workspace's project is refused too.
        let err = api
            .call("explain", &serde_json::json!({ "key": "docs::fn:x" }))
            .expect_err("qualified out-of-workspace key must be refused");
        assert!(err.contains("no project named `docs`"), "was: {err}");

        // The in-workspace project resolves (empty store ⇒ the node is absent, but
        // that is a normal in-scope answer, not a scoping refusal).
        let ok = api.call(
            "explain",
            &serde_json::json!({ "key": "fn:x", "project": "api" }),
        );
        assert!(
            ok.is_ok(),
            "the workspace's own project must still resolve: {ok:?}"
        );
    }
}

// The full explorer + Ask wiring a `roteiro serve` build with a model installed stands up: the UI, the
// `/v1/graph/capabilities` signal (ask:true + served models), and the graph-tools
// chat route — all mounted by `mount_explorer_surfaces` over the one engine.
// Gated on `serve,explorer` and driven with a mock engine (no llama.cpp, no
// model download, no real inference — we prove the routing, not generation).
#[cfg(all(test, feature = "serve", feature = "explorer"))]
mod serve_explorer_wiring {
    use super::{GraphToolRegistry, mount_explorer_surfaces};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt as _;
    use tower::ServiceExt as _; // for `oneshot`

    /// A stand-in [`rto_serve::Engine`] that serves one model and echoes a fixed
    /// reply — enough to exercise the HTTP wiring without building llama.cpp.
    struct MockEngine;

    impl rto_serve::Engine for MockEngine {
        fn models(&self) -> Vec<rto_serve::ModelInfo> {
            vec![rto_serve::ModelInfo {
                id: "qwen3-0.6b".to_owned(),
            }]
        }

        fn chat_stream(
            &self,
            _req: &rto_serve::ChatRequest,
            on_token: &mut dyn FnMut(&str),
        ) -> Result<rto_serve::CompletionStats, rto_serve::EngineError> {
            on_token("a grounded answer");
            Ok(rto_serve::CompletionStats {
                prompt_tokens: 1,
                completion_tokens: 3,
                finish_reason: rto_serve::FinishReason::Stop,
            })
        }
    }

    /// The merged router exactly as `serve_v1_tail` assembles it — parameterised
    /// over the `set` (the workspace-aware graph API) and `flat` (the model tool
    /// registry over every hosted project). `/v1` model app (graph tools on) +
    /// `/v1/graph/*` (capabilities ask:true) + the static web app.
    fn serve_router_for(
        set: std::sync::Arc<rto_graph::WorkspaceSet>,
        flat: std::sync::Arc<rto_graph::Workspace>,
        default: Option<String>,
    ) -> axum::Router {
        let engine: std::sync::Arc<dyn rto_serve::Engine> = std::sync::Arc::new(MockEngine);
        let tools: std::sync::Arc<dyn rto_serve::ToolRegistry> =
            std::sync::Arc::new(GraphToolRegistry::new(flat));
        // Mirror `serve_v1_tail`: one flattened registry over every project, plus a
        // per-workspace registry confined to each configured workspace's projects.
        let workspace_tools: std::collections::HashMap<
            String,
            std::sync::Arc<dyn rto_serve::ToolRegistry>,
        > = set
            .workspace_handles()
            .into_iter()
            .map(|(name, ws)| {
                let reg: std::sync::Arc<dyn rto_serve::ToolRegistry> =
                    std::sync::Arc::new(GraphToolRegistry::new(ws));
                (name, reg)
            })
            .collect();
        let model_ids = engine.models().into_iter().map(|m| m.id).collect();
        let base = rto_serve::app_with_workspace_tools(engine, tools, workspace_tools);
        mount_explorer_surfaces(base, set, default, model_ids)
    }

    /// The legacy single-repo `serve` wiring: one `repo` workspace folded into a
    /// one-entry `default` set (as `run_serve`'s single-repo fallback does), sharing
    /// the one store handle.
    fn serve_router() -> axum::Router {
        let store = rto_graph::Store::open_in_memory().expect("in-memory store");
        let flat = std::sync::Arc::new(rto_graph::Workspace::single("repo", store));
        let set = std::sync::Arc::new(rto_graph::WorkspaceSet::from_single(
            "default",
            flat.clone(),
            flat.is_multi(),
        ));
        serve_router_for(set, flat, Some("default".to_owned()))
    }

    /// A multi-workspace `serve` wiring built entirely from in-memory stores — no
    /// git repo, no cwd, no `open_graph()`. Two configured workspaces (`api` linked,
    /// `docs` standalone), each with its own project store; `flat` unions every
    /// project so the model tools reach any of them. Proves `serve` hosts the full
    /// configured set from ANY directory.
    fn multi_serve_router() -> axum::Router {
        let ws_api =
            rto_graph::Workspace::single("api", rto_graph::Store::open_in_memory().expect("store"));
        let ws_docs = rto_graph::Workspace::single(
            "docs",
            rto_graph::Store::open_in_memory().expect("store"),
        );
        let set = std::sync::Arc::new(rto_graph::WorkspaceSet::from_workspaces([
            ("api".to_owned(), ws_api, true),
            ("docs".to_owned(), ws_docs, false),
        ]));
        // The flattened model workspace over every hosted project.
        let flat = std::sync::Arc::new(rto_graph::Workspace::from_stores([
            ("api", rto_graph::Store::open_in_memory().expect("store")),
            ("docs", rto_graph::Store::open_in_memory().expect("store")),
        ]));
        // Multi-workspace ⇒ no implicit default flat workspace (a client addresses
        // one via `/v1/graph/workspaces/{ws}/…`).
        serve_router_for(set, flat, None)
    }

    async fn get(uri: &str) -> (StatusCode, String, String) {
        get_on(serve_router(), uri).await
    }

    async fn get_on(router: axum::Router, uri: &str) -> (StatusCode, String, String) {
        let resp = router
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let ct = resp
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        (status, ct, String::from_utf8_lossy(&body).into_owned())
    }

    #[tokio::test]
    async fn capabilities_report_ask_on_and_the_served_model() {
        let (status, ct, body) = get("/v1/graph/capabilities").await;
        assert_eq!(status, StatusCode::OK);
        assert!(ct.contains("application/json"), "content-type was {ct}");
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["ask"], true, "the serve build enables Ask");
        assert_eq!(
            json["models"],
            serde_json::json!(["qwen3-0.6b"]),
            "capabilities name the served model"
        );
    }

    /// An engine serving an embedding model FIRST, then a generative one — the
    /// exact shape that triggered the crash (`bge` leads the served list).
    struct EmbeddingAndGenerativeEngine;

    impl rto_serve::Engine for EmbeddingAndGenerativeEngine {
        fn models(&self) -> Vec<rto_serve::ModelInfo> {
            ["bge-small-en-v1.5-gguf", "qwen3-0.6b"]
                .into_iter()
                .map(|id| rto_serve::ModelInfo { id: id.to_owned() })
                .collect()
        }
        fn chat_stream(
            &self,
            _req: &rto_serve::ChatRequest,
            on_token: &mut dyn FnMut(&str),
        ) -> Result<rto_serve::CompletionStats, rto_serve::EngineError> {
            on_token("ok");
            Ok(rto_serve::CompletionStats {
                prompt_tokens: 1,
                completion_tokens: 1,
                finish_reason: rto_serve::FinishReason::Stop,
            })
        }
    }

    #[tokio::test]
    async fn capabilities_omit_the_embedding_model_from_ask() {
        // End-to-end: the served set leads with `bge`, but the Ask capability the
        // web app reads must list ONLY the generative model — so `models[0]` (what
        // the UI POSTs) can never be the process-aborting embedding model.
        let store = rto_graph::Store::open_in_memory().expect("in-memory store");
        let flat = std::sync::Arc::new(rto_graph::Workspace::single("repo", store));
        let set = std::sync::Arc::new(rto_graph::WorkspaceSet::from_single(
            "default",
            flat.clone(),
            flat.is_multi(),
        ));
        let engine: std::sync::Arc<dyn rto_serve::Engine> =
            std::sync::Arc::new(EmbeddingAndGenerativeEngine);
        let tools: std::sync::Arc<dyn rto_serve::ToolRegistry> =
            std::sync::Arc::new(GraphToolRegistry::new(flat));
        let served_ids: Vec<String> = engine.models().into_iter().map(|m| m.id).collect();
        let model_ids =
            super::chat_capable_model_ids(&super::config::Config::default(), &served_ids);
        let base = rto_serve::app_with_tools(engine, tools);
        let router = mount_explorer_surfaces(base, set, Some("default".to_owned()), model_ids);

        let (status, _ct, body) = get_on(router, "/v1/graph/capabilities").await;
        assert_eq!(status, StatusCode::OK);
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            json["models"],
            serde_json::json!(["qwen3-0.6b"]),
            "the embedding model must be excluded from the Ask pool"
        );
    }

    #[tokio::test]
    async fn the_explorer_ui_is_served_beside_the_model_endpoint() {
        let (status, ct, body) = get("/").await;
        assert_eq!(status, StatusCode::OK);
        assert!(ct.starts_with("text/html"), "content-type was {ct}");
        assert!(body.contains("<!doctype html>"));
        assert!(body.contains("/app.js"), "the shell loads our app");
    }

    #[tokio::test]
    async fn the_graph_grounded_chat_route_is_mounted() {
        // Prove the project-scoped chat route the Ask tab posts to exists on this
        // merged router: a well-formed request reaches the (mock) engine and gets
        // a 200 completion — not a 404 that would mean the route is missing.
        let body = serde_json::json!({
            "model": "qwen3-0.6b",
            "messages": [{ "role": "user", "content": "what is this repo?" }],
            "stream": false,
        });
        let resp = serve_router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/repo/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "the project-scoped chat route must be mounted and reachable"
        );
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            json["choices"][0]["message"]["content"], "a grounded answer",
            "the mounted route returns the engine's completion"
        );
    }

    // -- multi-workspace serve (no git cwd, no `open_graph()`) --------------

    #[tokio::test]
    async fn serve_hosts_all_configured_workspaces_with_no_cwd_repo() {
        // The whole point of the change: a `roteiro serve` model-server process built from
        // `[[workspaces]]`/`[standalone]` config hosts EVERY configured workspace
        // and lists them at `/v1/graph/workspaces` — with no repo discovered from
        // the current directory and no `open_graph()` on the cwd (this router is
        // assembled purely from in-memory stores).
        let (status, ct, body) = get_on(multi_serve_router(), "/v1/graph/workspaces").await;
        assert_eq!(status, StatusCode::OK);
        assert!(ct.contains("application/json"), "content-type was {ct}");
        let arr: serde_json::Value = serde_json::from_str(&body).unwrap();
        let arr = arr.as_array().expect("workspaces array");
        assert_eq!(arr.len(), 2, "both configured workspaces are hosted");
        // Stable (name) order: `api` (linked) then `docs` (standalone).
        assert_eq!(arr[0]["name"], "api");
        assert_eq!(arr[0]["linked"], true);
        assert_eq!(arr[1]["name"], "docs");
        assert_eq!(arr[1]["linked"], false, "a standalone repo is unlinked");
    }

    #[tokio::test]
    async fn nested_graph_routes_reach_each_configured_workspace() {
        // Every hosted workspace is reachable under its explicit path segment, so a
        // multi-workspace serve is not limited to a single default workspace: each
        // workspace's `/projects` route resolves within that named workspace.
        for (ws, project) in [("api", "api"), ("docs", "docs")] {
            let (status, _, body) = get_on(
                multi_serve_router(),
                &format!("/v1/graph/workspaces/{ws}/projects"),
            )
            .await;
            assert_eq!(
                status,
                StatusCode::OK,
                "workspace `{ws}` must be reachable via its nested route"
            );
            assert!(
                body.contains(project),
                "workspace `{ws}` hosts project `{project}` (was: {body})"
            );
        }
    }

    #[tokio::test]
    async fn the_model_tools_span_every_hosted_project() {
        // The graph-grounded chat route the served model uses must resolve a
        // project in ANY configured workspace — the flattened tool workspace unions
        // them all. Posting to `/v1/{project}/chat/completions` for a project drawn
        // from each workspace reaches the (mock) engine (200), not a 404.
        for project in ["api", "docs"] {
            let body = serde_json::json!({
                "model": "qwen3-0.6b",
                "messages": [{ "role": "user", "content": "what is this project?" }],
                "stream": false,
            });
            let resp = multi_serve_router()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/v1/{project}/chat/completions"))
                        .header("content-type", "application/json")
                        .body(Body::from(serde_json::to_vec(&body).unwrap()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::OK,
                "the project-scoped chat route must resolve `{project}` across the set"
            );
        }
    }

    #[tokio::test]
    async fn the_unscoped_chat_route_is_preserved() {
        // The UNSCOPED `/v1/chat/completions` (the default, tested path used by MCP
        // and generic OpenAI clients) is untouched by the workspace-scoping change:
        // it stays mounted, its tools still span every hosted project, and it reaches
        // the (mock) engine (200, a completion). The workspace-level Ask no longer
        // uses it — it posts to the scoped route below — but the default semantics
        // must not change.
        let body = serde_json::json!({
            "model": "qwen3-0.6b",
            "messages": [{ "role": "user", "content": "tell me about the docs repo" }],
            "stream": false,
        });
        let resp = multi_serve_router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "the unscoped chat route must stay mounted"
        );
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            json["choices"][0]["message"]["content"], "a grounded answer",
            "the unscoped route returns the engine's completion"
        );
    }

    #[tokio::test]
    async fn the_workspace_scoped_chat_route_reaches_each_configured_workspace() {
        // The WORKSPACE-level Ask posts to `/v1/workspaces/{ws}/chat/completions`
        // (ADR-0008): each configured workspace has its own registry, confined to
        // that workspace's projects. Prove every workspace's scoped route is mounted
        // and reaches the (mock) engine (200, a completion), so the panel has a
        // per-workspace endpoint to hit.
        for ws in ["api", "docs"] {
            let body = serde_json::json!({
                "model": "qwen3-0.6b",
                "messages": [{ "role": "user", "content": "what does this workspace do?" }],
                "stream": false,
            });
            let resp = multi_serve_router()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/v1/workspaces/{ws}/chat/completions"))
                        .header("content-type", "application/json")
                        .body(Body::from(serde_json::to_vec(&body).unwrap()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::OK,
                "the workspace-scoped chat route must resolve `{ws}`"
            );
            let bytes = resp.into_body().collect().await.unwrap().to_bytes();
            let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                json["choices"][0]["message"]["content"], "a grounded answer",
                "the scoped route returns the engine's completion"
            );
        }
    }

    #[tokio::test]
    async fn an_unknown_workspace_scoped_chat_route_is_a_404() {
        // A workspace-scoped Ask naming a workspace the server does not host is a 404
        // (the addressed scope does not exist) — never silently answered from another
        // workspace's registry.
        let body = serde_json::json!({
            "model": "qwen3-0.6b",
            "messages": [{ "role": "user", "content": "anything" }],
            "stream": false,
        });
        let resp = multi_serve_router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/workspaces/does-not-exist/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "an unknown workspace must 404, not resolve against another workspace"
        );
    }

    #[tokio::test]
    async fn the_single_workspace_scoped_chat_route_matches_the_default() {
        // A single-repo serve folds its one workspace into a `default` set entry
        // (sharing the flat store handle), so `/v1/workspaces/default/chat/completions`
        // is mounted and behaves like the unscoped route — the single-workspace path
        // is unchanged, just also addressable by name.
        let resp = serve_router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/workspaces/default/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "model": "qwen3-0.6b",
                            "messages": [{ "role": "user", "content": "what is this repo?" }],
                            "stream": false,
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "the single workspace must be addressable by its `default` name"
        );
    }

    #[test]
    fn unknown_workspace_name_fails_fast_listing_the_known_ones() {
        // `run_serve` validates `--workspace-name` up front via `set.select`: an
        // unknown name is a fast error naming the known workspaces, never a booted
        // server whose flat routes 404 on every request.
        let ws_api =
            rto_graph::Workspace::single("api", rto_graph::Store::open_in_memory().unwrap());
        let ws_docs =
            rto_graph::Workspace::single("docs", rto_graph::Store::open_in_memory().unwrap());
        let set = rto_graph::WorkspaceSet::from_workspaces([
            ("api".to_owned(), ws_api, true),
            ("docs".to_owned(), ws_docs, false),
        ]);
        let err = set.select(Some("nope")).err().expect("unknown must error");
        let msg = err.to_string();
        assert!(msg.contains("no workspace named `nope`"), "was: {msg}");
        assert!(msg.contains("api") && msg.contains("docs"), "was: {msg}");
        // A valid name (and the legacy `default` single-repo fold) still resolve.
        assert!(set.select(Some("api")).is_ok());
    }

    #[test]
    fn legacy_single_repo_folds_to_one_default_workspace() {
        // The single-repo fallback path: one pre-built store, wrapped as the sole
        // `default` workspace of a one-entry set (sharing the handle), exactly as
        // `run_serve` does when no `[[workspaces]]`/`[standalone]` is configured.
        let flat = std::sync::Arc::new(rto_graph::Workspace::single(
            "repo",
            rto_graph::Store::open_in_memory().unwrap(),
        ));
        let set = rto_graph::WorkspaceSet::from_single("default", flat.clone(), flat.is_multi());
        assert_eq!(set.names(), vec!["default".to_owned()]);
        // A bare selection resolves the sole workspace, and it hosts the one repo.
        assert_eq!(set.select(None).unwrap().names(), vec!["repo".to_owned()]);
    }
}

/// The `serve`/`mcp` path that flattens every configured workspace's repos into the
/// one model workspace (`resolved_repo_paths`), independent of the current
/// directory.
#[cfg(all(test, any(feature = "serve", feature = "mcp")))]
mod serve_workspace_paths_tests {
    use super::{fold_cli_roots, resolved_repo_paths};
    use rto_graph::ResolvedWorkspace;

    #[test]
    fn cli_roots_fold_into_a_default_workspace() {
        // With no configured groups, `--workspace <ROOT>` becomes a new linked
        // `default` workspace — so the CLI roots are a first-class named workspace
        // (surfaced by the graph API), not merely merged into the flat model view.
        let folded = fold_cli_roots(Vec::new(), &["/a".to_owned(), "/b".to_owned()]);
        assert_eq!(folded.len(), 1);
        assert_eq!(folded[0].name, "default");
        assert!(folded[0].linked);
        assert_eq!(folded[0].roots, vec!["/a".to_owned(), "/b".to_owned()]);

        // An existing `default` (the legacy `[workspace]`) is EXTENDED, not
        // duplicated, so CLI roots union with the configured ones.
        let existing = vec![ResolvedWorkspace {
            name: "default".to_owned(),
            roots: vec!["/cfg".to_owned()],
            repos: vec!["/cfg/extra".to_owned()],
            linked: true,
        }];
        let folded = fold_cli_roots(existing, &["/cli".to_owned()]);
        assert_eq!(folded.len(), 1, "no duplicate `default` group");
        assert_eq!(folded[0].roots, vec!["/cfg".to_owned(), "/cli".to_owned()]);
        assert_eq!(
            folded[0].repos,
            vec!["/cfg/extra".to_owned()],
            "repos untouched"
        );

        // No CLI roots ⇒ the groups are returned unchanged (named groups survive).
        let named = vec![ResolvedWorkspace {
            name: "api".to_owned(),
            roots: vec!["/api".to_owned()],
            repos: Vec::new(),
            linked: true,
        }];
        let folded = fold_cli_roots(named.clone(), &[]);
        assert_eq!(folded, named);
    }

    #[test]
    fn unions_every_group_and_cli_root_deduped_by_path() {
        // Two synthetic repos under a scanned root, plus an explicit repo — spread
        // across a linked group and a standalone singleton, with one repo named in
        // BOTH a group root and an explicit repo to prove de-duplication.
        let base = std::env::temp_dir().join(format!("rto-srv-paths-{}", std::process::id()));
        std::fs::remove_dir_all(&base).ok();
        for sub in ["scan/alpha/.git", "scan/beta/.git", "solo/gamma/.git"] {
            std::fs::create_dir_all(base.join(sub)).expect("mkrepo");
        }
        let scan = base.join("scan").to_string_lossy().into_owned();
        let alpha = base.join("scan/alpha").to_string_lossy().into_owned();
        let gamma_root = base.join("solo").to_string_lossy().into_owned();

        let resolved = vec![
            ResolvedWorkspace {
                name: "linked".to_owned(),
                roots: vec![scan.clone()],
                // `alpha` is also discovered under `scan` → must appear once.
                repos: vec![alpha.clone()],
                linked: true,
            },
            ResolvedWorkspace {
                name: "gamma".to_owned(),
                roots: Vec::new(),
                repos: vec![base.join("solo/gamma").to_string_lossy().into_owned()],
                linked: false,
            },
        ];
        // A `--workspace <ROOT>` that re-scans the same `solo` dir must not double it.
        let paths = resolved_repo_paths(&resolved, &[gamma_root]).expect("union");

        let mut got: Vec<_> = paths
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        got.sort();
        assert_eq!(
            got,
            vec!["alpha".to_owned(), "beta".to_owned(), "gamma".to_owned()],
            "each repo is hosted exactly once across all groups + cli roots"
        );

        std::fs::remove_dir_all(&base).ok();
    }
}

#[cfg(test)]
mod workspace_tests {
    use rto_graph::discover_repos_under;

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

#[cfg(test)]
mod audio_stream_tests {
    use super::audio_stream_lines;
    use rto_graph::{Explanation, NodeSummary};

    /// An explanation of a node with the given kind and rendered content.
    fn explanation(kind: &str, content: &str) -> Explanation {
        let mut node = rto_graph::Node::new("k", rto_graph::NodeKind::from_token(kind), "clip.mp3");
        node.path = Some("assets/clip.mp3".to_owned());
        Explanation {
            schema: rto_graph::SCHEMA,
            node: NodeSummary {
                key: node.key.clone(),
                kind: kind.to_owned(),
                name: node.name.clone(),
                path: node.path.clone(),
                lang: None,
            },
            meta: serde_json::json!({ "content": content }),
            outgoing: Vec::new(),
            incoming: Vec::new(),
        }
    }

    /// The stream line is printed for an audio node, tags indented under it —
    /// and, crucially, an estimated duration is shown **as** an estimate, because
    /// the line is the same rendering search indexes rather than a second one
    /// assembled here (ADR-0016).
    #[test]
    fn an_audio_stream_node_prints_its_stream_and_marks_an_estimate() {
        let lines = audio_stream_lines(&explanation(
            "audio_stream",
            "mp3, 44100 Hz, mono, 26122 ms (estimated)\nartist: Someone",
        ));
        assert_eq!(
            lines,
            vec![
                "  stream: mp3, 44100 Hz, mono, 26122 ms (estimated)".to_owned(),
                "    artist: Someone".to_owned(),
            ],
        );
    }

    /// Any other node prints nothing extra. The gate is the node *kind*, not "has
    /// content": an ADR or a README carries content too, and this must not start
    /// dumping it into `roteiro query`'s output.
    #[test]
    fn other_kinds_print_nothing_extra() {
        assert!(audio_stream_lines(&explanation("adr", "A decision record.")).is_empty());
        assert!(audio_stream_lines(&explanation("file", "Some prose.")).is_empty());
        // An audio node with no rendered content prints nothing rather than a
        // blank line.
        let mut bare = explanation("audio_stream", "");
        bare.meta = serde_json::json!({});
        assert!(audio_stream_lines(&bare).is_empty());
    }
}

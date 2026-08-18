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
// The remote model tier's transport (ADR-0019) — **the only module in this
// workspace that can send repository content off the machine.** It lives here,
// in the binary, and not in `rto-remote`, which takes the transport as a
// caller-supplied closure precisely so the code that decides whether bytes may
// leave is not the code that can make them leave.
#[cfg(feature = "remote")]
mod remote_transport;
// Ask over the remote model tier: the served `Engine`, wrapped so the hosted
// model is one more served id and `rto-serve` gains nothing (ADR-0019, Stage 34
// part 2b). Needs `serve` as well as `remote` — without a chat endpoint there is
// no Ask to wire.
#[cfg(all(feature = "remote", feature = "serve"))]
mod remote_engine;
mod review;
// The LLM reviewer's driver and the corpus replay that measures it (Stage 35b).
// Present under `test` as well as under a generation backend: the diff
// reconstruction and the parent-module lookup are pure git and string work, and
// their tests are the ones that matter in CI — which has no model store, so the
// backend-gated half of the module never runs there.
#[cfg(any(feature = "serve", feature = "inference-local-models", test))]
mod review_llm;
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
        #[arg(long, conflicts_with = "score")]
        base: Option<String>,
        /// Score a candidate reviewer's findings against the adjudicated review
        /// corpus instead of reviewing this working tree.
        ///
        /// Takes a JSON `roteiro.review-run/v1` document: the commits the
        /// candidate was run against, and what it said about each. Reports recall
        /// **per defect class** — never averaged, since which classes a reviewer
        /// can see is the only thing an implementer can act on. Needs no model and
        /// no network.
        #[arg(long, value_name = "RUN.json", conflicts_with = "base")]
        score: Option<String>,
        /// Score against a corpus file other than the one in this repository —
        /// for scoring a candidate on someone else's adjudicated set.
        #[arg(long, value_name = "CORPUS.jsonl", requires = "score")]
        corpus: Option<String>,
        /// Review the change with a local generative model, one file at a time
        /// (Stage 35b), instead of the graph-grounded report.
        ///
        /// Local only. `ModelTask::Review` may use the remote tier under
        /// ADR-0019, but a remote review would send the diff and the payload
        /// allow-list carries no source-code field — so there is no
        /// `--allow-remote` here until there is one there.
        #[arg(long, conflicts_with_all = ["score", "replay"])]
        llm: bool,
        /// Replay the LLM reviewer over every commit the adjudicated corpus
        /// covers, writing a `roteiro.review-run/v1` document here for `--score`
        /// to read. The harness that turns a reviewer into a number.
        #[arg(long, value_name = "RUN.json", conflicts_with_all = ["score", "base"])]
        replay: Option<String>,
        /// CI check-run evidence (a JSON array of `CheckRun`) for the
        /// compile-claim filter. Without it nothing is refuted and nothing is
        /// withheld: the filter is opt-in on evidence, never a blanket
        /// suppression.
        #[arg(long, value_name = "CHECKS.json")]
        checks: Option<String>,
        /// Replay only the first N corpus commits — a smoke run that does not
        /// cost a full pass.
        #[arg(long, value_name = "N", requires = "replay")]
        limit: Option<usize>,
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
        /// Also search episodic agent memory — what earlier sessions learned
        /// (`roteiro memory add`). **Off by default**: memory is accumulated,
        /// unreviewed and unredacted, so it is asked for rather than delivered
        /// (ADR-0013).
        ///
        /// When on, memory hits come back in their **own channel**, each marked
        /// `[memory]` with the anchor state that says whether it applies to this
        /// tree, ranked by a scorer that has no `authored` boost — the +40 is for
        /// intent someone deliberately wrote into a reviewed file, which this is
        /// not. Superseded records never appear; drifted ones do, marked.
        #[arg(long)]
        include_memory: bool,
        /// Emit the results as JSON. Without `--include-generated` or
        /// `--include-memory` this is the long-standing array of hits; with
        /// either, the multi-channel object (`{schema, hits, generated, memory}`)
        /// — so only a caller that opted in sees a different shape.
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
    /// Inspect the remote model tier — Roteiro's one explicitly-consented
    /// egress path (ADR-0019). **Nothing under this command sends anything.**
    ///
    /// The tier is off unless *both* your own `~/.roteiro/config.toml` and the
    /// invocation grant it. A committed `roteiro.toml` may switch it off for
    /// everyone, and may never switch it on for anyone.
    #[cfg(feature = "remote")]
    Remote {
        /// What to inspect.
        #[command(subcommand)]
        cmd: RemoteCmd,
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
    /// Rank files by intent-debt **density** — markers per 1,000 lines — rather
    /// than by raw marker count, which ranks the biggest file first by
    /// construction.
    ///
    /// A report, not a gate: it always exits zero. The denominator is the file's
    /// length in lines as recorded at extraction — every line, blanks and
    /// comments included, *not* source lines of code. Markers are filtered
    /// exactly as `roteiro debt` filters them, `[debt] ignore` globs included.
    DebtDensity {
        /// Restrict to these categories (repeatable): todo | fixme | hack |
        /// stub | deferred. Omit to count all.
        #[arg(long, value_name = "CATEGORY")]
        kind: Vec<String>,
        /// Rank by: `density` | `markers` | `lines`. Defaults to `density`.
        #[arg(long, value_name = "ORDER", default_value = "density")]
        order: String,
        /// Max rows to show; `0` shows every ranked file.
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Exclude files shorter than this from the ranking (`0` ranks every
        /// file). Short files are still counted and reported: one marker in a
        /// 10-line file is 100 per 1,000 lines, which is arithmetic rather than
        /// a finding.
        #[arg(long, value_name = "N", default_value_t = rto_graph::DEFAULT_MIN_LINES)]
        min_lines: u32,
        /// Emit the report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Inventory the **secret-named** config keys in the graph: where they are,
    /// what they are called, and whether their values were redacted before being
    /// persisted.
    ///
    /// NOT A SECRET SCANNER. Extraction redacts a secret-named config value
    /// before it ever reaches the store, so this reports *"secret-named config
    /// keys are present and safely redacted"* — with paths and key names, never
    /// values. It **cannot** find a hardcoded credential in source code (that is
    /// not a config key and produces no node here), cannot judge whether a value
    /// is valid, and cannot tell a real secret from a placeholder. An empty
    /// report means "no secret-*named* config key" — a statement about naming,
    /// not a clean bill of health.
    ///
    /// A report, not a gate: it always exits zero.
    ConfigSecrets {
        /// Max rows to show; `0` shows every secret-named key.
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Emit the report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Rank nodes by **directed** call coupling: fan-in (how many distinct
    /// symbols call this one) and fan-out (how many it calls), over `calls`
    /// edges only.
    ///
    /// A report, not a gate: it always exits zero. Unlike an undirected degree
    /// ranking, it separates "everything calls this" from "this calls
    /// everything" — the two have identical degree and opposite meaning.
    Coupling {
        /// Rank by: `total` | `fan_in` | `fan_out`. Defaults to `total`.
        #[arg(long, value_name = "ORDER", default_value = "total")]
        order: String,
        /// Max rows to show; `0` shows every coupled node.
        #[arg(long, default_value_t = 20)]
        limit: usize,
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
    /// (default feature `models`; absent under `--no-default-features`).
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
        /// Serve the **remote model tier** as Ask's model, granting it for the
        /// life of this server process (ADR-0019 v1.2).
        ///
        /// Read this before using it. Unlike a one-shot command, the invocation
        /// here is the *process*: every Ask this server answers, for as long as
        /// it runs, sends graph-derived context to the hosted model — including
        /// requests made by anyone else who can reach the port. Your
        /// `~/.roteiro/config.toml` must grant too, `serve` binds loopback by
        /// default, and every call is on the egress ledger (`roteiro remote
        /// log`). The grant dies with the process and is never persisted.
        #[cfg(all(feature = "remote", feature = "serve"))]
        #[arg(long, conflicts_with = "no_remote")]
        allow_remote: bool,
        /// Deny the remote tier for this server whatever the config layers say.
        #[cfg(all(feature = "remote", feature = "serve"))]
        #[arg(long)]
        no_remote: bool,
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
    ///
    /// **Local unless you say otherwise.** With `--features remote` and every
    /// layer of ADR-0019's consent granting, `--allow-remote` sends the drafting
    /// request to the hosted model instead. Nothing leaves without that flag:
    /// this command never prompts, because a prompt on the default path is how a
    /// habituated "y" becomes consent-by-default. `roteiro remote dry-run` shows
    /// the shape of what would leave, and `roteiro remote status` says whether
    /// the gate would open.
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
        /// Draft with the **remote model tier** instead of a local model,
        /// granting *this run* (ADR-0019). Necessary and not sufficient: your
        /// `~/.roteiro/config.toml` must grant too, and a run that passes this
        /// and is refused fails rather than quietly drafting locally.
        #[cfg(feature = "remote")]
        #[arg(long, conflicts_with = "no_remote")]
        allow_remote: bool,
        /// Deny the remote tier for this run whatever the config layers say.
        #[cfg(feature = "remote")]
        #[arg(long)]
        no_remote: bool,
    },
}

/// `roteiro remote` actions — the remote model tier's inspection surface
/// (ADR-0019).
///
/// **Exactly one subcommand here sends anything, and it is the one named for
/// it.** The guard landed first, on its own, in a build that compiled no backend
/// at all (part 1); `call` is what part 2 added beneath it. `dry-run` shows the
/// exact bytes a call *would* send; `status` says whether it would be permitted,
/// and why; `log` reads the record of what actually left; `call` does the one
/// thing the other three exist to make inspectable.
///
/// **`status` and `dry-run` never prompt**, and the rule is worth stating rather
/// than leaving to be noticed: they are the commands you run to find out what
/// would happen, and a command that asks permission in order to tell you is
/// useless. The TTY form of the invocation grant therefore lives with `call`
/// alone — see [`remote_transport::may_prompt`].
#[cfg(feature = "remote")]
#[derive(Subcommand)]
enum RemoteCmd {
    /// Report the consent gate: what each layer said, what this run would be
    /// permitted to do, and what a request would disclose.
    Status {
        /// Grant *this run* (the invocation half of consent). Necessary and not
        /// sufficient: your `~/.roteiro/config.toml` must grant too.
        #[arg(long, conflicts_with = "no_remote")]
        allow_remote: bool,
        /// Deny this run, whatever the config layers say.
        #[arg(long)]
        no_remote: bool,
        /// Emit the gate's state as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Print the **exact payload** a remote call would send, and send nothing.
    ///
    /// Takes no consent flag on purpose: an inspection is not a disclosure, so
    /// it must be available to someone deciding whether to grant one.
    DryRun {
        /// The instruction to ask.
        instruction: String,
        /// A graph node key to include as context (repeatable). Only the node's
        /// key, kind, name, path and captured prose are sent — never its other
        /// metadata, and never source.
        #[arg(long = "key", value_name = "KEY")]
        keys: Vec<String>,
        /// Emit the payload and its disclosure as JSON.
        #[arg(long)]
        json: bool,
    },
    /// **Send** the payload `dry-run` prints, if every layer of consent allows
    /// it — the one command in Roteiro that puts repository content on a wire.
    ///
    /// Requires the user layer *and* this invocation. With the user layer
    /// granting and no `--allow-remote`, an interactive terminal is shown the
    /// exact bytes and asked; a non-interactive one is refused and told which
    /// flag to pass, because a pipe cannot consent.
    Call {
        /// The instruction to ask.
        instruction: String,
        /// A graph node key to include as context (repeatable). Only the node's
        /// key, kind, name, path and captured prose are sent — never its other
        /// metadata, and never source.
        #[arg(long = "key", value_name = "KEY")]
        keys: Vec<String>,
        /// Grant *this run*, without being asked. Necessary and not sufficient:
        /// your `~/.roteiro/config.toml` must grant too.
        #[arg(long, conflicts_with = "no_remote")]
        allow_remote: bool,
        /// Deny this run, whatever the config layers say. Refuses before
        /// anything is assembled, recorded or sent.
        #[arg(long)]
        no_remote: bool,
        /// Emit the answer and what it cost in disclosure as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Read the egress ledger: **what left this machine, and when.**
    Log {
        /// Show at most this many calls, most recent last. `0` shows every one.
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Emit the ledger as JSON.
        #[arg(long)]
        json: bool,
    },
}

/// `roteiro model` actions.
#[cfg(feature = "models")]
#[derive(Subcommand)]
enum ModelAction {
    /// List registry models, which are installed for this platform, and how much
    /// disk each installed one occupies.
    List,
    /// Download a model into `~/.roteiro/models` (asks before fetching).
    ///
    /// An interrupted download is **resumed** on the next run: the partial file
    /// is kept and continued with an HTTP range request. A partial that cannot
    /// be shown to still match the remote — different checksum, size or URL — is
    /// discarded and refetched rather than trusted.
    Pull {
        /// Registry model name (see `roteiro model list`).
        name: String,
        /// Skip the confirmation prompt and download immediately.
        #[arg(long)]
        yes: bool,
    },
    /// Remove an installed model's files from the store, reporting what was
    /// freed.
    ///
    /// Deletes the model's whole directory, including any `.partial` left by an
    /// abandoned pull. The registry entry is untouched, so `model pull` can
    /// fetch it again.
    ///
    /// **This cannot tell whether a running `roteiro serve` is using the model.**
    /// Roteiro keeps no lock or pid file over the store, so there is nothing to
    /// check. On Unix a server that already has the file open keeps working from
    /// its open handle until it restarts; on Windows the removal fails while the
    /// file is held. Stop the server first if you are unsure.
    #[command(visible_alias = "remove")]
    Rm {
        /// Registry model name (see `roteiro model list`).
        name: String,
        /// Skip the confirmation prompt and remove immediately.
        #[arg(long)]
        yes: bool,
        /// Emit the removal report as JSON.
        #[arg(long)]
        json: bool,
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
    /// Recall what is worth reading first: the live records, **ranked by
    /// evidence**.
    ///
    /// `score = confidence × anchor_penalty × decay(age)`, computed on every read
    /// and stored nowhere. A stored score that ticked down would rewrite the store
    /// every time you looked at it, and recall would depend on when you last did.
    ///
    /// The order of the terms is the model, and it is **evidence first, clock
    /// last**:
    ///
    /// * a **superseded** record is not ranked at all — it left the moment its
    ///   successor was written, regardless of age or score;
    /// * the **anchor** dominates: a record whose anchor still resolves here in
    ///   the same format outranks one whose code has moved, which outranks one
    ///   whose code is gone. None of them is dropped — drift demotes, and
    ///   `--applicable-only` is the caller's choice, never the store's;
    /// * **age** is last, and by default is not priced at all: `--decay none`
    ///   means the same store and the same tree recall the same records in the
    ///   same order every time. Age is counted in records written since, never in
    ///   wall-clock, because the store is shared across worktrees.
    Recall {
        /// Free-text query. Every word must appear in the body, the anchor key or
        /// the anchor path. A **filter, not a scorer**: narrowing the query
        /// changes which records come back, never how they are ranked.
        query: Option<String>,
        /// How age is priced: `none` (default, reproducible) | `linear[:span]` |
        /// `exponential[:half-life]`, in generations.
        #[arg(long, value_name = "MODE", default_value_t = rto_graph::Decay::default())]
        decay: rto_graph::Decay,
        /// Only this namespace, matched exactly. Not a branch filter.
        #[arg(long, value_name = "SCOPE")]
        scope: Option<String>,
        /// Only this kind: lesson | attempt | decision | pattern | outcome.
        #[arg(long, value_name = "KIND")]
        kind: Option<rto_graph::MemoryKind>,
        /// Only records anchored to this node key.
        #[arg(long, value_name = "KEY")]
        anchor: Option<String>,
        /// Withhold records that do not apply to this tree. Off by default: a
        /// lesson about code that has moved or gone is demoted and labelled, not
        /// hidden, and is often the one worth reading.
        #[arg(long)]
        applicable_only: bool,
        /// At most this many records — the best-ranked, not the newest.
        #[arg(long, value_name = "N", default_value_t = 10)]
        limit: usize,
        /// Emit the ranking as JSON, every term included.
        #[arg(long)]
        json: bool,
    },
    /// Report on, or sweep, the **bounded cache tier** — the other half of the
    /// two-tier store (ADR-0013).
    ///
    /// Everything in that tier is re-derivable by definition, so evicting it costs
    /// cycles and never information. That is exactly why it can be bounded and
    /// episodic memory cannot: `roteiro memory forget` remains the only thing that
    /// removes a *remembered* record, and no sweep can reach one.
    ///
    /// Without `--sweep` this only reports. The sweep also runs as part of
    /// `roteiro context --refresh`, which is the maintenance seam it belongs to —
    /// never on a read path, so an ordinary query never mutates the store.
    Cache {
        /// Evict oldest-first until the tier fits the budget, rather than only
        /// reporting.
        #[arg(long)]
        sweep: bool,
        /// Budget in whole megabytes for this run, overriding the default (256)
        /// and `ROTEIRO_CACHE_BUDGET_MB`.
        #[arg(long, value_name = "MB")]
        budget_mb: Option<u64>,
        /// Emit the report as JSON.
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
    /// host**, with no isolation.
    ///
    /// Gated on `exec-subprocess`, which is on by default. The analyzer's own
    /// egress is switched off and its inputs are pinned and pre-provisioned, but
    /// a subprocess on this host can do what this host can do — so the run's
    /// evidence records `isolation=none`, and **`--allow-unsandboxed` is
    /// required, on every invocation**, to say you accept that. Since the
    /// feature became a default that flag is the only gate left, so it is not
    /// optional and will not be made so. Assets are never fetched here: a cold
    /// cache fails and names the prefetch command.
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
    /// Not available in this build — rebuild with `--features exec-subprocess`.
    ///
    /// The variant exists purely so the answer is a sentence rather than clap's
    /// `unrecognized subcommand`. Shipping a feature-gated command that vanishes
    /// without explanation is the exact defect this release fixes for
    /// `roteiro model`, and it would be perverse to reintroduce it here. Only a
    /// `--no-default-features` build reaches this.
    #[cfg(not(feature = "exec-subprocess"))]
    Run {
        /// The analyzer that would be run.
        analyzer: String,
        /// Accepted so the refusal names the feature rather than the flag.
        #[arg(long)]
        allow_unsandboxed: bool,
        /// Accepted for the same reason.
        #[arg(long)]
        json: bool,
    },
    /// Install and verify every pinned asset an analyzer needs, recording each
    /// digest — the one command that writes to the asset cache.
    ///
    /// Gated on `execution`, which is on by default: provisioning downloads,
    /// digests and pins, and executes no analyzer. It is deliberately reachable
    /// from a build with *no* execution backend, because that is what
    /// bootstraps one — `exec-boxlite` requires the verified runtime archive at
    /// compile time.
    #[cfg(feature = "execution")]
    Prefetch {
        /// Only this analyzer's assets. Default: all of them.
        #[arg(long, value_name = "NAME")]
        analyzer: Option<String>,
        /// Allow downloading the assets that are fetched by URL — today that is
        /// `osv-scanner`'s per-ecosystem OSV databases, roughly **260 MB**.
        ///
        /// Without it a downloadable asset that is not already present is
        /// refused with the command that obtains it, exactly as the
        /// operator-provisioned `RustSec` checkout is. Provisioning is the only
        /// thing that may fetch, and even here it is asked for rather than
        /// assumed: `prefetch` is a command people run when unsure, and a
        /// quarter-gigabyte download is not a reasonable answer to that.
        #[arg(long)]
        allow_download: bool,
        /// Emit the result as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Report each pinned asset's digest and fetch time, the advisory-database
    /// age behind each live findings layer, and which languages the shipped
    /// analyzers cover.
    ///
    /// Gated on `execution` alongside `prefetch`: reporting on the asset cache
    /// executes nothing.
    #[cfg(feature = "execution")]
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
    // every surface that reports intent debt for *this* repository — `debt`,
    // `debt-density`, `check`'s debt summary, and the Obsidian `_Home` overview.
    // One list, so no two surfaces can disagree about what is in scope
    // (ADR-0007 v1.1, issues #321 and #372).
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
    // Publish the `[models]` pins to the resolver, once, before any command picks
    // a model (Stage 33). A process-wide slot rather than a threaded parameter
    // because the OCR pin has to reach `ocr_content`, which runs per blob deep
    // inside extraction — exactly the reason `[paths] model_store` above is set
    // the same way. Nothing is validated here: a name is checked when a task is
    // resolved, so `roteiro config` can *report* a bad pin rather than being the
    // one command a bad pin stops you from running.
    #[cfg(feature = "models")]
    rto_graph::set_model_pins(cfg.effective.models.resolve());
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
        Command::Review {
            json,
            base,
            score,
            corpus,
            llm,
            replay,
            checks,
            limit,
        } => match (score, replay, llm) {
            (Some(run), _, _) => review::run_score(&run, corpus.as_deref(), json),
            (None, Some(out), _) => run_replay(&out, checks.as_deref(), limit),
            (None, None, true) => run_llm_review(base.as_deref(), checks.as_deref()),
            (None, None, false) => run_review(ingest, json, base.as_deref()),
        },
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
            include_memory,
            json,
        } => run_search(
            ingest,
            &query,
            limit,
            include_generated,
            include_memory,
            json,
        ),
        Command::Media { action } => run_media(ingest, gate, action),
        Command::Memory { action } => run_memory(action),
        Command::Context { key, refresh, json } => run_context(ingest, key, refresh, json),
        Command::Debt { kind, json } => run_debt(ingest, &kind, json, debt_ignore),
        Command::DebtDensity {
            kind,
            order,
            limit,
            min_lines,
            json,
        } => run_debt_density(ingest, &kind, &order, limit, min_lines, json, debt_ignore),
        Command::ConfigSecrets { limit, json } => run_config_secrets(ingest, limit, json),
        Command::Coupling { order, limit, json } => run_coupling(ingest, &order, limit, json),
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
        Command::Init { fetch, vault } => run_init(ingest, fetch, vault, debt_ignore),
        Command::Render { target, out } => run_render(ingest, &target, out, debt_ignore),
        Command::Import { from, path, json } => run_import(ingest, &from, &path, json),
        // The whole `Loaded` config, not only the effective merge: `spec draft`
        // can reach the remote tier, and ADR-0019 §3's consent gate has to read
        // the project and user layers *separately* — a merged `[remote] enabled`
        // cannot tell a grant that may stand from one that may not.
        Command::Spec { action } => run_spec(&cfg, ingest, action),
        Command::Config { json } => run_config(&cfg, json),
        #[cfg(feature = "remote")]
        Command::Remote { cmd } => run_remote(&cfg, ingest, cmd),
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
            #[cfg(all(feature = "remote", feature = "serve"))]
            allow_remote,
            #[cfg(all(feature = "remote", feature = "serve"))]
            no_remote,
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
                // Decided **here**, before anything is built or bound, and it is
                // the whole of ADR-0019 v1.2's "the invocation is the server
                // process": one gate consultation, at startup, whose answer the
                // engine then carries for its lifetime. A `--allow-remote` the
                // gate refuses fails here — the server does not start and then
                // quietly answer locally, which would be the process-shaped form
                // of the unannounced downgrade.
                #[cfg(all(feature = "remote", feature = "serve"))]
                remote: remote_grant_for(&cfg, invocation_grant(allow_remote, no_remote), "serve")?,
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
pub(crate) fn emit_json<T: serde::Serialize>(value: &T) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

/// Print the effective, merged configuration and each value's provenance
/// (`project` / `user` / `default`) — the answer to "why did it use that?".
fn run_config(loaded: &config::Loaded, json: bool) -> anyhow::Result<()> {
    if json {
        // The effective config, plus one **added** top-level key carrying the
        // model resolution. Added rather than nested: every existing path
        // (`infer.min_confidence`, `debt.ignore`, …) is where it was, so a
        // consumer written against the old shape keeps working.
        let mut value = serde_json::to_value(&loaded.effective)?;
        if let Some(obj) = value.as_object_mut() {
            obj.insert("model_resolution".to_owned(), model_resolution_json(loaded));
        }
        emit_json(&value)?;
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
    print_models_section(loaded);
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
    print_remote_section(loaded);
    print_debt_section(loaded);
    print_telemetry_section(e, p, u);
    print_workspace_section(e, p, u);
}

/// Print `[remote]` — the one table whose `enabled` key does **not** follow this
/// report's `project > user` rule.
///
/// It gets its own function, and a stated rule, for the reason ADR-0019 gives
/// for restating the inversion in ADR-0007 as well as in itself: a reader who
/// applies the general precedence here will be wrong, and will be wrong about
/// the one key where being wrong means believing egress is off when it is on (or
/// the reverse). So the per-layer values are printed rather than the merged one,
/// and each is labelled with what that layer is allowed to do.
#[cfg(feature = "remote")]
fn print_remote_section(loaded: &config::Loaded) {
    let e = &loaded.effective;
    println!("\n[remote]  (ADR-0019 — off by default; run `roteiro remote status` for the gate)");
    println!(
        "  enabled  = {:?}  (project: {:?} — may deny, never grant; user: {:?} — may grant)",
        e.remote.enabled, loaded.project.remote.enabled, loaded.user.remote.enabled
    );
    println!(
        "  endpoint = {:?}  ({})",
        e.remote.endpoint,
        provenance(
            loaded.project.remote.endpoint.is_some(),
            loaded.user.remote.endpoint.is_some()
        )
    );
    println!(
        "  model    = {:?}  ({})",
        e.remote.model,
        provenance(
            loaded.project.remote.model.is_some(),
            loaded.user.remote.model.is_some()
        )
    );
    if loaded.project.remote.enabled == Some(true) {
        println!(
            "  note: the project file's `enabled = true` was read and ignored — a committed \
             file may deny egress but never grant it"
        );
    }
    println!(
        "  a granted run still needs `--allow-remote`: your user config opts you in, the \
         invocation opts the run in"
    );
}

/// Without the `remote` feature there is no tier, and the section says so rather
/// than being absent — an omitted section reads as "no such setting", which
/// would leave someone with `[remote] enabled = true` in their config believing
/// it was honoured.
#[cfg(not(feature = "remote"))]
fn print_remote_section(_loaded: &config::Loaded) {
    println!(
        "\n[remote]  (ADR-0019 — not built: this binary has no remote model tier, so nothing \
         here can send anything, whatever the keys say)"
    );
}

/// Print `[models]`: the five keys as set, each with the layer that set it, then
/// the resolution table saying what those keys *did*.
///
/// Its own function because the two halves answer different questions — what is
/// configured, and what is used — and because the section is now five keys and
/// six surfaces, which is most of a screen on its own.
fn print_models_section(loaded: &config::Loaded) {
    let e = &loaded.effective;
    let (p, u) = (&loaded.project, &loaded.user);
    println!("\n[models]");
    for (label, key) in [
        ("embedding ", "embedding"),
        ("generative", "generative"),
        ("vision    ", "vision"),
        ("audio     ", "audio"),
        ("ocr       ", "ocr"),
    ] {
        println!(
            "  {label} = {:?}  ({})",
            e.models.get(key),
            provenance(p.models.get(key).is_some(), u.models.get(key).is_some())
        );
    }
    print_model_resolution(loaded);
}

/// The model resolution as JSON: one entry per surface, each carrying the model,
/// the rule, the layer a pin came from, and whether the weights are on disk — or
/// the error, for a key that cannot be honoured.
///
/// The same content as the human table, because a `--json` consumer asking "why
/// did it use that model?" is asking the same question and must not have to parse
/// prose for the answer.
#[cfg(feature = "models")]
fn model_resolution_json(loaded: &config::Loaded) -> serde_json::Value {
    let pins = loaded.effective.models.resolve();
    let entries: Vec<serde_json::Value> = rto_graph::resolve_models(&pins)
        .into_iter()
        .map(|(task, result)| match result {
            Ok(choice) => serde_json::json!({
                "task": task.as_str(),
                "surface": task.surface(),
                "config_key": task.config_key(),
                "model": choice.model,
                "source": choice.source.as_str(),
                "layer": (choice.source == rto_graph::ModelSource::Pinned).then(|| provenance(
                    loaded.project.models.get(task.config_key()).is_some(),
                    loaded.user.models.get(task.config_key()).is_some(),
                )),
                "installed": choice.installed,
                "why": choice.why(),
            }),
            Err(err) => serde_json::json!({
                "task": task.as_str(),
                "surface": task.surface(),
                "config_key": task.config_key(),
                "error": err.to_string(),
            }),
        })
        .collect();
    serde_json::Value::Array(entries)
}

/// Without the registry there is nothing to resolve against; an empty array says
/// "nothing resolved" without inventing entries that were never computed.
#[cfg(not(feature = "models"))]
fn model_resolution_json(_loaded: &config::Loaded) -> serde_json::Value {
    serde_json::Value::Array(Vec::new())
}

/// Print **which model serves each surface, and why** — the resolution table
/// (Stage 33).
///
/// The five `[models]` keys printed above say what was *set*. Six surfaces
/// consume them, two of those share a key, and three of them had no key at all
/// before this stage — so the keys alone do not answer the question ADR-0007
/// promised `roteiro config` would answer: *why did it use that model?* This
/// does, in the same spirit as the per-pattern `[debt] ignore` provenance below:
/// report the thing the operator actually observes, not only the input it came
/// from.
///
/// A key that cannot be honoured prints as its own error rather than being
/// suppressed or replaced by a default. This is the command someone runs
/// *because* a pin is not doing what they expected, so it is the one command a
/// bad pin must not stop — every other model surface refuses outright.
#[cfg(feature = "models")]
fn print_model_resolution(loaded: &config::Loaded) {
    let pins = loaded.effective.models.resolve();
    println!("  resolution — the model each surface uses, and the rule that chose it:");
    for (task, result) in rto_graph::resolve_models(&pins) {
        let key = task.config_key();
        match result {
            Ok(choice) => {
                // Where a pin came from, on the same `project`/`user` terms as
                // every other value in this report.
                let layer = if choice.source == rto_graph::ModelSource::Pinned {
                    format!(
                        " in {} config",
                        provenance(
                            loaded.project.models.get(key).is_some(),
                            loaded.user.models.get(key).is_some(),
                        )
                    )
                } else {
                    String::new()
                };
                // `Some(false)` only: `installed: None` means the question does
                // not apply (a remote resolution has no weights on any disk), and
                // labelling that "[not installed]" would send a reader to `model
                // pull` for a model no registry lists.
                let state = if choice.model.is_some() && choice.installed == Some(false) {
                    "  [not installed]"
                } else {
                    ""
                };
                println!(
                    "    {:<12}{:<30}{:<34}  ({}{}){state}",
                    task.as_str(),
                    task.surface(),
                    choice.label(),
                    choice.why(),
                    layer,
                );
            }
            // Deliberately not styled as a warning: it is the answer to the
            // question, and the answer is that this surface will refuse to run.
            Err(err) => println!(
                "    {:<12}{:<30}UNRESOLVED — {err}",
                task.as_str(),
                task.surface()
            ),
        }
    }
}

/// Without the registry there is nothing to resolve a name against, so the keys
/// are reported as set and nothing more. Said out loud rather than omitted: a
/// silently missing section reads as "there is no resolution", not as "this build
/// cannot compute one".
#[cfg(not(feature = "models"))]
fn print_model_resolution(_loaded: &config::Loaded) {
    println!(
        "  resolution — unavailable: this build lacks the `models` feature, so it \
         has no registry to resolve these names against"
    );
}

/// The egress ledger's path: `$ROTEIRO_HOME/remote/egress.jsonl`, else
/// `~/.roteiro/remote/egress.jsonl`.
///
/// Beside the model store and the logs rather than inside the repository, and
/// deliberately so on two counts. It is a record of what *this machine*
/// disclosed, which does not partition by repository — the question "what left
/// this machine, and when?" is asked by a person, not by a checkout. And a
/// per-repo ledger would sooner or later be committed, which would publish
/// verbatim copies of everything that was ever sent.
///
/// # Errors
/// When neither `ROTEIRO_HOME` nor a home directory can be found, since there is
/// then nowhere to record a call and — per [`rto_remote::call_with`] — a call
/// that cannot be recorded does not happen.
#[cfg(feature = "remote")]
fn remote_ledger() -> anyhow::Result<rto_remote::Ledger> {
    let home = config::roteiro_home().ok_or_else(|| {
        anyhow::anyhow!(
            "no home directory and no `ROTEIRO_HOME`, so the remote tier has nowhere to record \
             what it sends — and a call that cannot be recorded is a call Roteiro will not make"
        )
    })?;
    Ok(rto_remote::Ledger::at(
        home.join("remote").join("egress.jsonl"),
    ))
}

/// Build the endpoint from `[remote] endpoint` / `[remote] model`.
///
/// Always [`rto_remote::ProducerTrust::VendorAsserted`]: a hosted model has no
/// digest this machine can compute, so nothing here may claim otherwise.
///
/// **The single constructor.** Every surface that can send — `remote
/// status|dry-run|call`, `spec draft --allow-remote`, `serve --allow-remote` —
/// gets its endpoint from here, which is what lets
/// [`refuse_local_id_collision`] be one check rather than five.
///
/// # Errors
/// When either key is unset, when `[remote] model` collides with a local model id
/// ([`refuse_local_id_collision`]), or when the endpoint is one
/// [`rto_remote::Endpoint::new`] refuses.
#[cfg(feature = "remote")]
fn remote_endpoint(cfg: &config::Config) -> anyhow::Result<rto_remote::Endpoint> {
    let endpoint = cfg.remote.endpoint.as_deref().unwrap_or_default();
    let model = cfg.remote.model.as_deref().unwrap_or_default();
    if endpoint.trim().is_empty() {
        anyhow::bail!(
            "`[remote] endpoint` is not set, so there is nowhere to send to. Set it in \
             `roteiro.toml` or `~/.roteiro/config.toml` — either layer may choose the \
             destination; only your user config can grant the tier"
        );
    }
    refuse_local_id_collision(model)?;
    Ok(rto_remote::Endpoint::new(
        endpoint,
        model,
        rto_remote::ProducerTrust::VendorAsserted,
    )?)
}

/// **Refuse a `[remote] model` that squats on a local model id.**
///
/// Nothing stopped `[remote] model = "qwen3-0.6b"` before this. Under
/// `serve --allow-remote` that id becomes a served model, so `/v1/models` lists
/// it twice and every request naming it is answered by the hosted model — and
/// `qwen3-0.6b` is, to anyone reading, the name of a local GGUF.
///
/// **That is egress under a false name.** The user layer granted, so it is not
/// unauthorised; but ADR-0019's whole thesis is that the local→remote edge is a
/// gate somebody opened *knowingly* — §4 requires the payload to be inspectable
/// and §1 forbids consent from being probabilistic. Content leaving under a name
/// that reads as local is unrecognisable as egress, which is the same harm one
/// step removed: the record in `roteiro remote log` is honest and the name in
/// front of the operator is not.
///
/// So it is refused, at the one place an endpoint is built, and refused for
/// **every** surface rather than only for `serve` — `remote status` should say so
/// before anyone grants anything.
///
/// # Refused, not resolved
///
/// Neither silent resolution is acceptable, and both were available:
///
/// * *Let the remote win* — the current behaviour, and the defect.
/// * *Let the local win* — quieter, and worse: a tier the operator granted, paid
///   for and can see in `remote status` would be **inert**, with the reason
///   nowhere on screen. A capability that silently does nothing is the failure
///   mode this ADR keeps naming in other clothes.
///
/// # The registry, not the served set
///
/// Checked against [`rto_graph::find_model`] — every id Roteiro can serve — and
/// not against what happens to be installed. A configuration that is legal today
/// and illegal after `roteiro model pull` would be a trap, and the served set is
/// a subset of the registry, so the wider check subsumes the narrower one and is
/// deterministic besides.
///
/// # Errors
/// When `model` names a registry entry. The message names both the key and the
/// id, because "that name is taken" is unactionable without saying by what.
#[cfg(feature = "remote")]
fn refuse_local_id_collision(model: &str) -> anyhow::Result<()> {
    let model = model.trim();
    let Some(spec) = rto_graph::find_model(model) else {
        return Ok(());
    };
    anyhow::bail!(
        "`[remote] model = {model:?}` collides with a local model id: `{name}` is a {kind} model \
         in Roteiro's own registry, and that is the namespace local models are served under. \
         Refusing.\n\n\
         Under `serve --allow-remote` this id would appear twice in `/v1/models` and every \
         request naming it would be answered by the hosted model at `[remote] endpoint` — so \
         repository content would leave this machine under a name that reads as local. You \
         granted the tier, so that is not unauthorised; it is unrecognisable, which ADR-0019 \
         treats as the same harm one step removed.\n\n\
         Set `[remote] model` to the hosted model's own vendor string (vendor strings are not \
         registry names), or run `roteiro model list` to see which names are taken. Roteiro will \
         not resolve this for you in either direction: preferring the local model would leave a \
         tier you granted silently inert.",
        name = spec.name,
        kind = spec.kind.as_str(),
    )
}

/// An open gate, with everything a call needs and nothing it could re-decide.
///
/// Held together in one value so no surface can hold a [`rto_remote::Decision`]
/// without the [`rto_remote::Endpoint`] it was granted for, or the other way
/// round. [`remote_grant_for`] is the only constructor, so a surface cannot
/// assemble one out of a gate it did not consult.
#[cfg(feature = "remote")]
struct RemoteGrant {
    /// Where the call goes, and under what model string.
    endpoint: rto_remote::Endpoint,
    /// The gate's answer, passed through to [`rto_remote::call_with`] verbatim
    /// rather than re-derived — that function re-checks it, and a second
    /// implementation of "may this send?" is exactly what ADR-0019 forbids.
    decision: rto_remote::Decision,
}

/// **The remote tier's answer for a surface whose default is local** — `spec
/// draft` and `serve`/Ask, as distinct from `roteiro remote call`, which exists
/// to send.
///
/// Returns `Some` when the gate is open, `None` when this run is deliberately
/// local, and an **error** when the run asked to go remote and cannot. That
/// third case is the whole reason this is a function rather than three lines at
/// each call site:
///
/// > A run that would have gone remote but was denied must say so rather than
/// > silently answering locally.
///
/// Passing `--allow-remote` is someone asking for the hosted model's answer.
/// Giving them a local model's answer instead is a *different answer with no
/// signal that anything changed* — the same unannounced downgrade ADR-0019 §6
/// forbids on a network failure, arriving through the consent gate instead of
/// through a socket. So a shut gate under an explicit grant stops the run, names
/// the layer that shut it, and offers that layer's own remedy.
///
/// An absent flag is **not** that case. It is the ordinary local run, and it is
/// silent: there is nothing to announce when nobody asked for anything.
///
/// # This surface never prompts
///
/// `roteiro remote call` shows a TTY the exact bytes and asks, because sending
/// is what that command does. Here the default is local, and a prompt on the
/// default path turns a habituated "y" into consent-by-default — the thing
/// ADR-0019 §3 built a two-layer gate to prevent. The flag is the only way.
///
/// # Errors
/// When an explicit `--allow-remote` was refused by any layer, and when the gate
/// opens but `[remote] endpoint`/`model` cannot produce a usable endpoint.
#[cfg(feature = "remote")]
fn remote_grant_for(
    cfg: &config::Loaded,
    invocation: Option<bool>,
    surface: &str,
) -> anyhow::Result<Option<RemoteGrant>> {
    let decision = rto_remote::consent::decide(cfg.remote_config_grant(), invocation);
    // Printed whatever the outcome: a committed `enabled = true` that does
    // nothing is worse left mysterious, and it is equally mysterious on a run
    // that went remote for other reasons.
    if let Some(note) = decision.ignored_project_grant_note() {
        eprintln!("{note}\n");
    }

    if decision.granted() {
        // Built only now, so an unusable endpoint cannot be reported to someone
        // who never asked to send anything.
        return Ok(Some(RemoteGrant {
            endpoint: remote_endpoint(&cfg.effective)?,
            decision,
        }));
    }

    if invocation == Some(true) {
        anyhow::bail!(
            "`{surface} --allow-remote` asked for the remote model tier and the gate refused: \
             {reason}\n\nRoteiro did **not** fall back to a local model. A different model is \
             a different answer, and handing you one without saying so is the failure ADR-0019 \
             most needs to prevent. Re-run without `--allow-remote` to use the local model \
             deliberately, or `roteiro remote status` to see every layer.",
            reason = decision.reason,
        );
    }
    Ok(None)
}

/// The invocation half of consent, from the two flags.
///
/// `None` — neither flag — is the common case and is *not* a grant: ADR-0019
/// requires the run to opt in as well as the human, so an absent flag denies.
#[cfg(feature = "remote")]
fn invocation_grant(allow_remote: bool, no_remote: bool) -> Option<bool> {
    match (allow_remote, no_remote) {
        (true, false) => Some(true),
        (false, true) => Some(false),
        // `conflicts_with` makes `(true, true)` unreachable from the CLI; read as
        // no grant, which is the safe reading of an ambiguous invocation.
        _ => None,
    }
}

/// `roteiro remote …` — the tier's surface. Three subcommands inspect; one
/// sends, and it is the one called `call`.
#[cfg(feature = "remote")]
fn run_remote(
    cfg: &config::Loaded,
    ingest: rto_graph::IngestConfig,
    cmd: RemoteCmd,
) -> anyhow::Result<()> {
    match cmd {
        RemoteCmd::Status {
            allow_remote,
            no_remote,
            json,
        } => run_remote_status(cfg, invocation_grant(allow_remote, no_remote), json),
        RemoteCmd::DryRun {
            instruction,
            keys,
            json,
        } => run_remote_dry_run(cfg, ingest, &instruction, &keys, json),
        RemoteCmd::Call {
            instruction,
            keys,
            allow_remote,
            no_remote,
            json,
        } => run_remote_call(
            cfg,
            ingest,
            &instruction,
            &keys,
            invocation_grant(allow_remote, no_remote),
            json,
        ),
        RemoteCmd::Log { limit, json } => run_remote_log(limit, json),
    }
}

/// Assemble the payload for `dry-run` and for `call` — **one function, so the
/// preview and the act cannot describe different requests.**
///
/// `rto_remote::Payload::body` already holds the two *renderings* level; this
/// holds the two *assemblies* level, which is the other half of the same
/// guarantee. A `call` that read one more node than its `dry-run` did would
/// print an honest preview of a request that never existed.
#[cfg(feature = "remote")]
fn remote_payload(
    ingest: rto_graph::IngestConfig,
    instruction: &str,
    keys: &[String],
) -> anyhow::Result<rto_remote::Payload> {
    let mut nodes = Vec::with_capacity(keys.len());
    if !keys.is_empty() {
        let (repo, mut store, cache) = open_graph()?;
        refresh_for_read(&repo, &mut store, &cache, ingest, GraphSource::Committed)?;
        for key in keys {
            let node = store.get_node(key)?.ok_or_else(|| {
                anyhow::anyhow!(
                    "no node with key `{key}` (try `roteiro query --kind <kind>` to list nodes)"
                )
            })?;
            nodes.push(node);
        }
    }
    Ok(rto_remote::Payload::new(instruction, &nodes)?)
}

/// Report the consent gate, layer by layer, plus the disclosure a grant would
/// authorise.
///
/// Prints the layers **before** the decision, because a reader who disagrees
/// with the decision needs to see the inputs to find out where they went wrong —
/// and because "your project file said yes and it was ignored" is only
/// intelligible next to the file that said it.
#[cfg(feature = "remote")]
fn run_remote_status(
    cfg: &config::Loaded,
    invocation: Option<bool>,
    json: bool,
) -> anyhow::Result<()> {
    let grant = cfg.remote_config_grant();
    let decision = rto_remote::consent::decide(grant, invocation);
    let endpoint = remote_endpoint(&cfg.effective);
    let ledger = remote_ledger()?;

    if json {
        return emit_json(&serde_json::json!({
            "granted": decision.granted(),
            "reason": decision.reason.as_str(),
            "explanation": decision.reason.explain(),
            "remedy": decision.reason.remedy(),
            "project_grant_ignored": decision.project_grant_ignored,
            "layers": {
                "project": cfg.project.remote.enabled,
                "user": cfg.user.remote.enabled,
                "invocation": invocation,
            },
            "endpoint": endpoint.as_ref().ok().map(rto_remote::Endpoint::url),
            "model": endpoint.as_ref().ok().map(rto_remote::Endpoint::model),
            "trust": rto_remote::ProducerTrust::VendorAsserted.as_str(),
            "endpoint_error": endpoint.as_ref().err().map(ToString::to_string),
            "ledger": ledger.path().display().to_string(),
            "backend": "ureq",
            // Whether, never what. A status command that echoed a credential
            // would put one in every terminal scrollback and CI log that ran it.
            "credential_env": remote_transport::API_KEY_ENV,
            "credential_set": remote_transport::api_key_is_set(),
        }));
    }

    println!("remote model tier (ADR-0019) — off unless every layer below allows it\n");
    println!("consent layers:");
    println!(
        "  project roteiro.toml     = {:?}   (may deny; may never grant)",
        cfg.project.remote.enabled
    );
    println!(
        "  user ~/.roteiro/config   = {:?}   (may grant — necessary, not sufficient)",
        cfg.user.remote.enabled
    );
    println!(
        "  this invocation          = {invocation:?}   (may grant — necessary, not sufficient)"
    );
    println!(
        "\ndecision: {}\n  because {}",
        if decision.granted() {
            "GRANTED"
        } else {
            "DENIED"
        },
        decision.reason.explain()
    );
    if let Some(remedy) = decision.reason.remedy() {
        println!("  to change it: {remedy}");
    }
    if let Some(note) = decision.ignored_project_grant_note() {
        println!("\n{note}");
    }
    match &endpoint {
        Ok(e) => println!(
            "\nendpoint: {}\nmodel:    {}  (trust: {} — {})",
            e.url(),
            e.model(),
            e.trust().as_str(),
            e.trust().caveat().unwrap_or("verified on this machine"),
        ),
        Err(err) => println!("\nendpoint: unusable — {err}"),
    }
    println!("ledger:   {}", ledger.path().display());
    // Said plainly, because part 1's builds said the opposite and someone
    // upgrading is entitled to notice the change rather than discover it.
    println!(
        "\nbackend:  ureq (compiled in). This build **can** send, when the gate above is open — \
         it is\n          the one capability in Roteiro that does. `roteiro remote dry-run` \
         shows the exact\n          bytes; `roteiro remote call` is what sends them."
    );
    println!(
        "credential: {} ({} — it is an environment variable, not a config key, because \
         `roteiro.toml`\n            is committed by design)",
        if remote_transport::api_key_is_set() {
            "set"
        } else {
            "not set"
        },
        remote_transport::API_KEY_ENV,
    );
    println!("\n{}", rto_remote::Payload::disclosure());
    Ok(())
}

/// Print the exact bytes a call would send, having sent nothing.
///
/// The payload is assembled from the graph by the **same** allow-list the call
/// path uses, so this is a preview of the act rather than a description of it:
/// `rto_remote::dry_run` and `rto_remote::call_with` render the body through one
/// function.
#[cfg(feature = "remote")]
fn run_remote_dry_run(
    cfg: &config::Loaded,
    ingest: rto_graph::IngestConfig,
    instruction: &str,
    keys: &[String],
    json: bool,
) -> anyhow::Result<()> {
    let endpoint = remote_endpoint(&cfg.effective)?;
    let payload = remote_payload(ingest, instruction, keys)?;
    let body = rto_remote::dry_run(&endpoint, &payload);

    if json {
        return emit_json(&serde_json::json!({
            "endpoint": endpoint.url(),
            "model": endpoint.model(),
            "trust": endpoint.trust().as_str(),
            "fields": payload.fields_present(),
            "bytes": body.len(),
            "body": body,
            "disclosure": rto_remote::Payload::disclosure(),
            "sent": false,
        }));
    }

    println!("DRY RUN — nothing was sent, and nothing was recorded.\n");
    println!("would POST to: {}", endpoint.url());
    println!("model:         {}", endpoint.model());
    println!(
        "carrying:      {} ({} bytes)",
        payload.fields_present().join(", "),
        body.len()
    );
    println!("\n--- the exact body ---\n{body}\n--- end of body ---\n");
    println!("{}", rto_remote::Payload::disclosure());
    Ok(())
}

/// **Send.** The one command in Roteiro that puts repository content on a wire.
///
/// The order below is the contract, and every step before the last is a refusal
/// point:
///
/// 1. **Assemble**, through the same function `dry-run` uses, so the bytes are
///    the bytes it showed.
/// 2. **Decide**, from the config layers plus this invocation.
/// 3. **Ask, if and only if asking is the missing half.** With the user layer
///    granting and no `--allow-remote`, an interactive terminal is shown the
///    exact body and the full disclosure and asked once. Every other denial is
///    reported rather than prompted — see [`remote_transport::may_prompt`] for
///    which, and why a prompt may never stand in for the user layer.
/// 4. **Hand it to `rto_remote::call_with`**, which records before it sends and
///    refuses to send if it cannot record.
/// 5. **Read what came back**, and refuse anything that is not a whole answer.
///
/// Nothing here falls back to a local model at any step, and the errors say so
/// in as many words. That is Principle 10's second half, which ADR-0019 says
/// binds harder for this capability than for any other.
#[cfg(feature = "remote")]
fn run_remote_call(
    cfg: &config::Loaded,
    ingest: rto_graph::IngestConfig,
    instruction: &str,
    keys: &[String],
    invocation: Option<bool>,
    json: bool,
) -> anyhow::Result<()> {
    let endpoint = remote_endpoint(&cfg.effective)?;
    let payload = remote_payload(ingest, instruction, keys)?;
    let body = rto_remote::dry_run(&endpoint, &payload);

    let grant = cfg.remote_config_grant();
    let mut decision = rto_remote::consent::decide(grant, invocation);
    if let Some(note) = decision.ignored_project_grant_note() {
        eprintln!("{note}\n");
    }
    if !decision.granted() && remote_transport::may_prompt(decision.reason) {
        // A prompt is the invocation, not a second chance at the config: the gate
        // is re-run with the answer rather than the decision being patched, so it
        // stays the single implementation of who may send.
        //
        // `Invocation::Prompt` rather than a bare `Some(bool)`, and that is the
        // whole of the fix for #386's second review comment: collapsing the two
        // forms made a declined prompt report itself as `--no-remote`, telling
        // someone they had passed a flag they never typed. On the consent path of
        // all places, a message that misreports *how* consent was withheld
        // undermines the thing it is reporting on.
        let said_yes = ask_to_send(&endpoint, &body, &payload)?;
        decision =
            rto_remote::consent::decide_with(grant, rto_remote::Invocation::Prompt(said_yes));
    }

    let ledger = remote_ledger()?;
    let raw = rto_remote::call_with(
        &endpoint,
        &payload,
        decision,
        &ledger,
        &|| rto_exec::rfc3339_utc(std::time::SystemTime::now()),
        Some(&remote_transport::call),
    )?;
    let answer = rto_remote::response::parse(&raw)?;
    let discrepancy = answer.model_discrepancy(endpoint.model());

    if json {
        return emit_json(&serde_json::json!({
            "answer": answer.text,
            "endpoint": endpoint.url(),
            "model_requested": endpoint.model(),
            "model_answered": answer.model,
            "model_discrepancy": discrepancy,
            "trust": endpoint.trust().as_str(),
            "trust_caveat": endpoint.trust().caveat(),
            "disclosed": payload.fields_present(),
            "sent_bytes": body.len(),
            "ledger": ledger.path().display().to_string(),
        }));
    }
    if let Some(note) = discrepancy {
        eprintln!("{note}\n");
    }
    println!("{}", answer.text);
    // On stderr, so piping the answer somewhere does not also pipe the receipt —
    // but printed every time, because a disclosure that only shows up when you
    // ask for it is one people stop seeing.
    eprintln!(
        "\n— {} bytes ({}) sent to {} as {} ({}); recorded in {}",
        body.len(),
        payload.fields_present().join(", "),
        endpoint.url(),
        endpoint.model(),
        endpoint.trust().as_str(),
        ledger.path().display(),
    );
    Ok(())
}

/// Ask this terminal to grant *this run*, having shown it exactly what would
/// leave.
///
/// Returns the answer alone — `true` for a yes, `false` for anything else. The
/// caller wraps it in [`rto_remote::Invocation::Prompt`], which is what keeps a
/// declined prompt from being reported as `--no-remote`: an answer is an explicit
/// denial rather than an absence, so the gate reports `PromptDeclined` — *"you
/// read it and said no"* — rather than either `InvocationUnset` and its advice to
/// pass a flag, or `InvocationDenied` and its claim that one was passed.
///
/// **A non-interactive stdin is never prompted and never granted.** A pipe
/// cannot consent, and treating an unattended run as a yes is exactly the
/// consent-by-default this ADR exists to prevent; the refusal names the flag
/// that would work instead.
#[cfg(feature = "remote")]
fn ask_to_send(
    endpoint: &rto_remote::Endpoint,
    body: &str,
    payload: &rto_remote::Payload,
) -> anyhow::Result<bool> {
    use std::io::Write as _;

    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        anyhow::bail!(
            "the remote model tier is enabled for you but not for this run, and this run is \
             not interactive, so there is nobody to ask. Nothing was sent. Re-run with \
             `--allow-remote` to grant it deliberately, or `roteiro remote dry-run` to see \
             the exact bytes first"
        );
    }
    eprint!(
        "{}",
        remote_transport::prompt_text(endpoint, body, &payload.fields_present())
    );
    eprint!("Send this now? [y/N] ");
    std::io::stderr().flush().ok();
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes" | "Yes"))
}

/// Read the egress ledger — what left this machine, and when.
///
/// An empty ledger is reported as empty and never as an error: "nothing has left
/// this machine" is the answer most of the time, and it is the answer the reader
/// wants stated rather than inferred from silence.
#[cfg(feature = "remote")]
fn run_remote_log(limit: usize, json: bool) -> anyhow::Result<()> {
    let ledger = remote_ledger()?;
    let entries = ledger.read()?;
    let shown = if limit == 0 || limit >= entries.len() {
        &entries[..]
    } else {
        &entries[entries.len() - limit..]
    };

    if json {
        return emit_json(&serde_json::json!({
            "ledger": ledger.path().display().to_string(),
            "total": entries.len(),
            "entries": shown,
        }));
    }

    println!("egress ledger: {}", ledger.path().display());
    if entries.is_empty() {
        println!("\nnothing has left this machine.");
        return Ok(());
    }
    println!(
        "{} entr{}\n",
        entries.len(),
        if entries.len() == 1 { "y" } else { "ies" }
    );
    for entry in shown {
        match entry {
            rto_remote::Entry::Egress(e) => println!(
                "{}  SENT      {} bytes to {} as {} ({})\n            carrying: {}",
                e.at,
                e.bytes,
                e.endpoint,
                e.model,
                e.trust.as_str(),
                e.fields.join(", "),
            ),
            rto_remote::Entry::Outcome(o) if o.ok => {
                println!("{}  RETURNED  {} bytes", o.at, o.response_bytes);
            }
            rto_remote::Entry::Outcome(o) => println!("{}  FAILED    {}", o.at, o.detail),
            // `Entry` is `#[non_exhaustive]`, so this arm is required. It prints
            // rather than skips, because on an egress log a line nobody mentions
            // reads as a line that is not there — the same "no record, so
            // presumably nothing left" default `rto_remote::record` argues
            // against.
            //
            // Today it is unreachable, and by a route worth knowing about:
            // `Ledger::read` drops lines that will not deserialize, and an
            // unknown `event` tag is one of those. So a future Roteiro's third
            // line kind would vanish at the serde layer *before* reaching here.
            // That is a gap in `read`, not in this match, and it is left for the
            // change that introduces such a kind — which is the change that can
            // test it.
            other => println!(
                "{}  ?         a line this build does not understand ({other:?})",
                other.at()
            ),
        }
    }
    Ok(())
}

/// Print `[debt]` with **per-pattern** provenance.
///
/// `ignore` merges across layers rather than replacing, so one
/// `(project)`/`(user)` label for the whole key would misreport a list holding
/// patterns from both. Each pattern therefore carries its own origin, and an
/// `ignore_reset` prints the inherited patterns it discarded — silently dropping
/// the user layer is the bug the merge exists to fix, so the escape hatch must
/// not reintroduce it.
///
/// A reset that did **not** happen is reported just as carefully. This section
/// once announced "inherited patterns dropped" whenever the *effective* flag was
/// set, and the effective flag inherited the user layer's — so a user-layer
/// reset, which governs nothing, produced that headline directly above a list
/// where every inherited pattern was still present. The claim, not the merge, was
/// wrong; the flag now records the merge that actually ran
/// ([`config::Config::overlaid_with`]) and an inert user-layer reset says so in
/// its own words.
fn print_debt_section(loaded: &config::Loaded) {
    println!("[debt]");
    let sources = loaded.debt_ignore_sources();
    if sources.is_empty() {
        println!("  ignore = []  (default)");
    } else {
        println!(
            "  ignore = ({} pattern(s), merged across layers)",
            sources.len()
        );
        for (pattern, layer) in sources {
            println!("    {pattern:?}  ({layer})");
        }
    }
    if loaded.effective.debt.ignore_reset == Some(true) {
        println!("  ignore_reset = true  (inherited patterns dropped)");
        for pattern in loaded.debt_ignore_discarded() {
            println!("    {pattern:?}  (discarded from user)");
        }
    } else if loaded.debt_ignore_reset_was_inert() {
        // A reset drops what a layer inherits, and the user layer is the bottom
        // one, so its flag never reaches the merge. Said out loud rather than left
        // to the docs: a key argued for on the grounds that "a reset cannot fail
        // quietly" must not quietly do nothing, least of all in the command whose
        // job is explaining what the configuration did.
        println!(
            "  ignore_reset = true in the user config had NO EFFECT — a reset \
             drops what a layer inherits, and the user layer is the lowest, so \
             there is nothing beneath it to drop. Set it in the project's \
             `roteiro.toml` to discard the user layer's patterns."
        );
    }
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

/// Say so, on stderr, when the graph store was holding a **different** working
/// tree and was therefore rebuilt rather than trusted (issue #330).
///
/// `graph.db` is an assembled view of one tree. A store that has come to describe
/// another one — restored from a backup, copied with a `.git` directory, or
/// reached after a layout change that put it under the shared common git dir —
/// would otherwise let `sync` answer "up to date" about a graph nobody is looking
/// at. The sync engine corrects that automatically; this makes the correction
/// *visible*, so the answer is never a confident wrong one, and so the one slow
/// run it causes has a stated reason instead of looking like a hang.
fn report_foreign_worktree(report: &rto_graph::SyncReport) {
    if let Some(previous) = &report.rebuilt_from_foreign_worktree {
        eprintln!(
            "note: this graph store was assembled from a different working tree \
             ({previous}); it has been rebuilt for this one rather than reused. \
             The extraction cache is shared across worktrees, so this costs little."
        );
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

    // `sync`'s whole contract is the rewrite, so there is no serve-as-is fallback
    // here the way there is for a read: skipping the work and still printing
    // "synced" would be the silent downgrade wearing a success message.
    ensure_graph_writable(&store)?;

    let registry = Registry::new(ingest);
    let report = if committed_only {
        sync(&mut store, &repo, &cache, &registry)?
    } else {
        sync_worktree(&mut store, &repo, &cache, &registry)?
    };

    if json {
        emit_json(&report)?;
    } else {
        report_foreign_worktree(&report);
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

/// Refuse to **rewrite** a graph store that a newer Roteiro has already written
/// (issue #342).
///
/// A store ahead of this build opens without complaint, and every path that
/// reassembles the graph — `sync`, and the implicit refresh below — would then
/// re-extract the whole tree under this build's older `EXTRACT_VERSION` and
/// commit the result over the newer build's. Nothing fails and nothing warns;
/// the graph simply gets worse. That is the entire bug, so this is the entire
/// fix: the write stops, loudly, naming both versions.
///
/// It lives here rather than in `rto_graph` because the refusal is a policy the
/// *caller* chooses — the library reports the condition ([`rto_graph::Store::schema_ahead`])
/// and stays free of it. That also keeps the whole change additive on a
/// published 1.x API; see [`rto_graph::SchemaAhead`] for why that mattered.
fn ensure_graph_writable(store: &rto_graph::Store) -> anyhow::Result<()> {
    match store.schema_ahead()? {
        Some(ahead) => Err(anyhow::Error::new(ahead)),
        None => Ok(()),
    }
}

/// Refresh the graph for a **read** command (`query`/`explain`, `search`,
/// `context`, `path`), and serve the store as-is when this build must not
/// rewrite it.
///
/// These commands rebuild before reading so results reflect the current source.
/// That refresh is a graph rewrite like any other, so on a store from the future
/// it is the same silent downgrade — `roteiro search` alone destroys the newer
/// build's content today. But refusing the *command* would be the wrong trade:
/// reads against such a store are provably sound (migrations are additive in
/// effect), and issue #342 is explicit that they must keep working. So the
/// rewrite is skipped, the reason goes to stderr, and the question still gets an
/// answer — from the graph the newer build left, which is better content than
/// this build could produce anyway.
///
/// Gate and write commands (`sync`, `check`, `review`, the importers, `export`)
/// deliberately do **not** come through here: for them a stale-but-unrefreshed
/// graph would be a confident wrong verdict, and a hard refusal is the honest
/// answer.
fn refresh_for_read(
    repo: &rto_graph::Repo,
    store: &mut rto_graph::Store,
    cache: &rto_graph::ObjectCache,
    ingest: rto_graph::IngestConfig,
    source: GraphSource,
) -> anyhow::Result<()> {
    if let Some(ahead) = store.schema_ahead()? {
        eprintln!(
            "note: {ahead}\n      Answering from the graph that newer build left; \
             it has not been refreshed for the current source."
        );
        return Ok(());
    }
    build_graph(repo, store, cache, ingest, source)?;
    Ok(())
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
    // Every caller of this function reassembles the graph — derived layer,
    // authored layer, and re-applied imports — so this is the one chokepoint the
    // write guard needs (issue #342). `run_sync` carries its own; it does not
    // come through here.
    ensure_graph_writable(store)?;
    let registry = Registry::new(ingest);
    let sync_report = match source {
        GraphSource::Committed => sync(store, repo, cache, &registry)?,
        GraphSource::Worktree => sync_worktree(store, repo, cache, &registry)?,
        GraphSource::Index => sync_index(store, repo, cache, &registry)?,
    };
    // `check` and `review` gate on this graph, so if it had been assembled from
    // another working tree the rebuild that just happened is the difference
    // between a correct verdict and a confident wrong one. Say so (issue #330).
    report_foreign_worktree(&sync_report);

    // The authored-layer file set must match the derived tree: the staged files
    // in Index mode (so a staged-new ADR is seen), the `HEAD` tree in Committed
    // mode, and in Worktree mode `HEAD` **plus untracked files** — because that
    // is precisely what `sync_worktree` overlaid into the derived layer.
    //
    // Getting this wrong is issue #330's observed symptom, and it is a *silent*
    // wrong answer rather than a loud one. `sync_worktree` walks untracked files
    // deliberately, "so the working-tree `sync`/`check`/`review` see new work
    // that isn't staged yet" — but the authored set here read only `HEAD`, so a
    // brand-new ADR had its symbols extracted while the file was never parsed as
    // an ADR. `check` then reported 17 ADRs with 18 on disk, `sync` said "up to
    // date", and nothing indicated that the newest decision was missing. The two
    // layers disagreed about which tree they were describing, in one worktree,
    // with no second worktree involved.
    let blobs = match source {
        GraphSource::Index => repo.index_files()?,
        GraphSource::Committed => repo.walk_blobs()?,
        GraphSource::Worktree => {
            let mut blobs = repo.walk_blobs()?;
            // `untracked_files` is defined against the index, so it cannot
            // return a path already in `blobs`. The synthesized oid is unused:
            // `read_source` reads Worktree content from disk by path, and an
            // untracked file has no git object to read anyway. (A bare repo has
            // no working tree, and `untracked_files` returns nothing there, so
            // the oid-reading fallback is never reached with one of these.)
            blobs.extend(
                repo.untracked_files()?
                    .into_iter()
                    .map(|path| rto_graph::BlobRef {
                        path,
                        oid: String::new(),
                    }),
            );
            blobs
        }
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

#[cfg(any(feature = "serve", feature = "inference-local-models"))]
/// The repository root a git-backed review works from.
fn review_repo_root() -> std::path::PathBuf {
    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
}

/// `roteiro review --llm` — review the change with the local generative model.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
fn run_llm_review(base: Option<&str>, checks: Option<&str>) -> anyhow::Result<()> {
    review_llm::run_llm(&review_repo_root(), base, checks)
}

/// `roteiro review --replay` — measure the reviewer against the corpus.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
fn run_replay(out: &str, checks: Option<&str>, limit: Option<usize>) -> anyhow::Result<()> {
    review_llm::run_replay(&review_repo_root(), out, checks, limit)
}

/// The same two surfaces in a build with no generation backend.
///
/// A named refusal rather than a missing subcommand: `--llm` is documented in
/// `--help` whatever the build, so a stock install that answered
/// `unrecognized argument` would send someone to check their spelling instead of
/// their features. This is the shape `models` was moved into `default` for.
#[cfg(not(any(feature = "serve", feature = "inference-local-models")))]
fn run_llm_review(_base: Option<&str>, _checks: Option<&str>) -> anyhow::Result<()> {
    anyhow::bail!(
        "`review --llm` needs a local generation backend, which this build does not \
         have. Rebuild with `--features serve` (or `inference-local-models`). \
         `roteiro review` without `--llm` needs no model and still works."
    )
}

/// See [`run_llm_review`]'s backend-less twin.
#[cfg(not(any(feature = "serve", feature = "inference-local-models")))]
fn run_replay(_out: &str, _checks: Option<&str>, _limit: Option<usize>) -> anyhow::Result<()> {
    anyhow::bail!(
        "`review --replay` needs a local generation backend, which this build does \
         not have. Rebuild with `--features serve` (or `inference-local-models`). \
         `roteiro review --score <run.json>` scores an existing run with no model."
    )
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
fn run_init(
    ingest: rto_graph::IngestConfig,
    fetch: bool,
    vault: bool,
    debt_ignore: &[String],
) -> anyhow::Result<()> {
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
        render_obsidian(ingest, None, debt_ignore)?;
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

/// The embedding model `[models] embedding` names, **resolved** rather than read
/// straight out of the config (Stage 33).
///
/// The difference is what happens to a wrong value. Read straight, an unknown
/// name or a generative model reached the embedder and failed there, several
/// layers from the key that caused it; resolved, it is a named error quoting the
/// key. An explicit `--model` flag still wins over this and is validated by the
/// embedder, which is where a flag belongs.
///
/// # Errors
/// The resolver's error when `[models] embedding` names no known model, or names
/// one that is not an embedding model.
#[cfg(all(feature = "inference", feature = "models"))]
fn configured_embedding_model(_cfg: &config::Config) -> anyhow::Result<Option<&'static str>> {
    Ok(rto_graph::resolve_model(rto_graph::ModelTask::Embed)?.model)
}

/// Without the registry there is nothing to resolve a name *against*, so the
/// configured name passes through unvalidated to [`config_embedding_model`],
/// which warns that this build cannot honour it either way.
#[cfg(all(feature = "inference", not(feature = "models")))]
fn configured_embedding_model(cfg: &config::Config) -> anyhow::Result<Option<&str>> {
    Ok(cfg.models.embedding.as_deref())
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
    let model = match model {
        Some(flag) => Some(flag),
        None => config_embedding_model(configured_embedding_model(cfg)?),
    };

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

/// Manage pluggable local embedding models: list the registry, pull a model, or
/// remove one to reclaim its disk.
#[cfg(feature = "models")]
fn run_model(action: ModelAction) -> anyhow::Result<()> {
    match action {
        ModelAction::List => {
            run_model_list();
            Ok(())
        }
        ModelAction::Pull { name, yes } => run_model_pull(&name, yes),
        ModelAction::Rm { name, yes, json } => run_model_rm(&name, yes, json),
    }
}

/// The `--json` shape of `roteiro model rm`.
#[cfg(feature = "models")]
#[derive(serde::Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
struct ModelRmReport {
    /// The model removed.
    model: String,
    /// The directory that was removed.
    dir: String,
    /// The files removed, sorted.
    files: Vec<String>,
    /// Bytes reclaimed.
    freed_bytes: u64,
    /// The same figure for humans (e.g. `17.3 GiB`).
    freed: String,
}

/// Remove an installed model's files, reporting what was freed.
///
/// Refuses — non-zero, with the command that would install it — when the model
/// is not on disk, rather than reporting a cheerful zero-byte success.
#[cfg(feature = "models")]
fn run_model_rm(name: &str, yes: bool, json: bool) -> anyhow::Result<()> {
    use rto_graph::{find_model, installed_size, model_dir, remove_model};
    use std::io::Write as _;

    // An unknown name and an uninstalled model are different mistakes and get
    // different advice.
    if find_model(name).is_none() {
        anyhow::bail!("unknown model `{name}` (see `roteiro model list`)");
    }
    // "Installed" means the directory is **there**, not that it has bytes in it.
    // An abandoned pull leaves an empty directory (or one whose size cannot be
    // read), and refusing to remove those would strand the exact debris this
    // command exists to clear. Size is for reporting and the prompt only — never
    // for deciding whether there is anything to remove.
    let dir = model_dir(name);
    if !dir.exists() {
        anyhow::bail!(
            "`{name}` is not installed — nothing to remove (install it with `roteiro model pull {name}`)"
        );
    }
    let size = installed_size(name);

    if !yes {
        eprintln!(
            "roteiro would remove `{name}` from {}, freeing {}",
            dir.display(),
            human_bytes(size)
        );
        if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            anyhow::bail!("removal declined (non-interactive; re-run with `--yes`)");
        }
        eprint!("Remove now? [y/N] ");
        std::io::stderr().flush().ok();
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y" | "yes" | "Yes") {
            anyhow::bail!("removal declined");
        }
    }

    let removed = remove_model(name)
        .map_err(|e| anyhow::anyhow!("removing `{name}` from {}: {e}", dir.display()))?;
    if json {
        emit_json(&ModelRmReport {
            model: name.to_owned(),
            dir: removed.dir.display().to_string(),
            files: removed.files.clone(),
            freed_bytes: removed.bytes,
            freed: human_bytes(removed.bytes),
        })?;
    } else {
        println!(
            "removed `{name}` from {} — freed {} ({} file(s))",
            removed.dir.display(),
            human_bytes(removed.bytes),
            removed.files.len()
        );
        println!("re-install it with `roteiro model pull {name}`");
    }
    Ok(())
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
                // For an installed model report what it actually occupies (which
                // is what `model rm` would reclaim), not the registry's estimate
                // of the download. They differ: the store may also hold a
                // `.partial` from an abandoned pull.
                let on_disk = if installed {
                    format!(
                        ", {} on disk",
                        human_bytes(rto_graph::installed_size(spec.name))
                    )
                } else {
                    String::new()
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
                    "    {mark} {name:<name_w$}  {licence}{role}{dim}, ~{size} MiB{on_disk}",
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
        // installing atomically — so a 20 GiB model never buffers in memory —
        // and resume from whatever an interrupted earlier attempt left behind.
        rto_graph::download_resumable(
            &dest,
            f.url,
            f.sha256,
            |from| http_range_reader(f.url, from),
            report_download_event,
        )
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

/// Open a streaming HTTPS reader for `url` starting at byte `from` (the body is
/// never buffered whole).
///
/// A non-zero `from` sends `Range: bytes=<from>-`, so an interrupted pull
/// continues instead of restarting. What comes back is classified by
/// [`rto_graph::interpret_range_response`]: a `206` is the requested tail, a
/// `200` is the whole file however it was asked for — and the caller must not
/// confuse the two.
#[cfg(feature = "models")]
fn http_range_reader(
    url: &str,
    from: u64,
) -> Result<rto_graph::RangeReply<impl std::io::Read>, rto_graph::DownloadError> {
    let mut req = ureq::get(url);
    if from > 0 {
        req = req.header("Range", format!("bytes={from}-"));
    }
    // `ureq` turns 4xx/5xx into errors; 200 and 206 both arrive here as `Ok`.
    let resp = req
        .call()
        .map_err(|e| rto_graph::DownloadError::Transport(format!("GET {url}: {e}").into()))?;

    // Copy the headers out before `into_body` consumes the response.
    let status = resp.status().as_u16();
    let header = |name: &str| {
        resp.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let accept_ranges = header("accept-ranges");
    let content_range = header("content-range");
    let content_length = header("content-length").and_then(|v| v.trim().parse::<u64>().ok());

    let (kind, total) = rto_graph::interpret_range_response(
        status,
        accept_ranges.as_deref(),
        content_range.as_deref(),
        content_length,
        from,
    )?;
    let reader = resp.into_body().into_reader();
    Ok(match kind {
        rto_graph::RangeKind::Partial => rto_graph::RangeReply::Partial { reader, total },
        rto_graph::RangeKind::Full { detail } => rto_graph::RangeReply::Full {
            reader,
            total,
            detail,
        },
    })
}

/// Narrate a download's notable moments on stderr — resumption, a discarded
/// partial and why, a server that will not do ranges.
///
/// These are exactly the things that would otherwise look like the command
/// silently doing the wrong amount of work.
#[cfg(feature = "models")]
fn report_download_event(event: rto_graph::DownloadEvent) {
    use rto_graph::DownloadEvent as E;
    match event {
        E::DiscardedPartial { bytes, reason } => {
            eprintln!("  discarding the {} partial: {reason}", human_bytes(bytes));
        }
        E::Resuming { offset, total } => match total {
            Some(t) => eprintln!(
                "  resuming from {} of {} ({} to go)",
                human_bytes(offset),
                human_bytes(t),
                human_bytes(t.saturating_sub(offset))
            ),
            None => eprintln!("  resuming from {}", human_bytes(offset)),
        },
        E::AlreadyComplete { bytes } => {
            eprintln!("  {} already downloaded — verifying", human_bytes(bytes));
        }
        E::RangeUnsupported { discarded, detail } => {
            eprintln!(
                "  the server will not resume ({detail}); restarting from zero and discarding {}",
                human_bytes(discarded)
            );
        }
        E::KeptPartial { bytes } => {
            eprintln!(
                "  transfer failed; keeping {} for a later `roteiro model pull` to resume from",
                human_bytes(bytes)
            );
        }
        E::PoisonedPartial { bytes } => {
            eprintln!(
                "  checksum failed; discarding all {} — those bytes cannot be resumed from",
                human_bytes(bytes)
            );
        }
    }
}

/// Format a byte count for a human: `1.4 GiB`, `812.0 MiB`, `947 B`.
///
/// No longer gated on `models`: the object-cache sweep reports reclaimed bytes in
/// every build (`object_sweep_line`), so this is reachable without it.
fn human_bytes(bytes: u64) -> String {
    #[expect(
        clippy::cast_precision_loss,
        reason = "a display string; f64 is exact well past any plausible model size"
    )]
    let mut value = bytes as f64;
    let mut unit = "B";
    for next in ["KiB", "MiB", "GiB", "TiB"] {
        if value < 1024.0 {
            break;
        }
        value /= 1024.0;
        unit = next;
    }
    if unit == "B" {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {unit}")
    }
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
    cfg: &config::Loaded,
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
            #[cfg(feature = "remote")]
            allow_remote,
            #[cfg(feature = "remote")]
            no_remote,
        } => run_spec_draft(
            cfg,
            ingest,
            &topic,
            title.as_deref(),
            &kind,
            out.as_deref(),
            #[cfg(feature = "remote")]
            invocation_grant(allow_remote, no_remote),
        ),
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

/// Draft the scaffold's unfilled sections (ADR-0004 Tier 1) — **locally unless
/// this run said otherwise.**
///
/// Two backends now, and which one runs is a *consent gate*, not a preference:
/// the local llama.cpp path (`serve` / `inference-local-models`), and — with
/// `--features remote` and every layer of ADR-0019's consent granting — the
/// hosted model. The tier is decided **before** the model is resolved, so the
/// two live decisions are made in one place and in the right order.
///
/// Three rules hold here, and each one is a failure mode ADR-0019 names:
///
/// 1. **No `--allow-remote`, no egress.** The flag is the invocation half of
///    consent and there is no TTY prompt on this path. `roteiro remote call`
///    prompts because sending *is* what that command does; here the default is
///    local, and a prompt on a default path is how a habituated "y" turns into
///    consent-by-default.
/// 2. **A refused `--allow-remote` stops the run.** Someone who typed the flag
///    asked for the hosted model. Handing them the local model's answer instead
///    is the unannounced downgrade this ADR most needs to prevent — the same
///    failure as a silent fall back on a network error, wearing a different hat.
///    See [`remote_grant_for`].
/// 3. **The remote path is not the local prompt on a wire.** It rebuilds the
///    request through [`rto_remote::Payload`], so graph content reaches the
///    endpoint only as allow-listed [`rto_remote::ContextItem`]s. See
///    [`spec_draft_remote`].
// Stage 20: local `spec draft` generation runs on **llama.cpp** (the shared
// `rto-llama` engine, ADR-0006) — available whenever either the `serve` or the
// `inference-local-models` feature is on. Stage 34 part 2b added the remote
// backend beside it, which is why this function is reachable under `remote`
// alone: a build with the tier and no llama.cpp can still draft, and that is the
// point of a tier meant for work local models cannot do.
#[cfg(any(
    feature = "serve",
    feature = "inference-local-models",
    feature = "remote"
))]
fn run_spec_draft(
    cfg: &config::Loaded,
    ingest: rto_graph::IngestConfig,
    topic: &str,
    title: Option<&str>,
    kind: &str,
    out: Option<&str>,
    #[cfg(feature = "remote")] invocation: Option<bool>,
) -> anyhow::Result<()> {
    let (scaffold, label, ctx) = build_scaffold(ingest, topic, title, kind)?;

    // Decided first, and it either grants, denies locally, or **stops the run**.
    // Nothing below this line can turn a refusal into a local answer.
    #[cfg(feature = "remote")]
    let grant = remote_grant_for(cfg, invocation, "spec draft")?;
    #[cfg(feature = "remote")]
    let tier = grant
        .as_ref()
        .map_or(rto_graph::RemoteTier::Unavailable, |g| {
            rto_graph::RemoteTier::Granted {
                trust: g.endpoint.trust(),
            }
        });
    #[cfg(not(feature = "remote"))]
    let tier = rto_graph::RemoteTier::Unavailable;

    // Model pick, via the one resolver (Stage 33): the remote tier if this run
    // granted it, else `[models] generative` if it pins one, else the low-tier
    // instruct default that runs anywhere. This used to search the registry here,
    // and — the part worth replacing — it *filtered* a pin that was not a
    // generative model, which silently fell back to the default and left the
    // configuration looking honoured.
    //
    // Resolved from the config in hand rather than from the process-wide pins:
    // both hold the same table (the pins are published from this very config at
    // startup), and taking the argument keeps the dependency visible in the
    // signature. The process-wide slot exists for the call sites config cannot
    // reach — OCR, per blob, inside extraction.
    let choice = rto_graph::resolve_model_with_remote(
        rto_graph::ModelTask::Draft,
        &cfg.effective.models.resolve(),
        tier,
    )?;

    #[cfg(feature = "remote")]
    if let Some(grant) = grant {
        debug_assert!(
            choice.source.is_remote(),
            "a granted tier must be the resolution `spec draft` acts on"
        );
        return spec_draft_remote(&grant, topic, &scaffold, &label, &ctx, out);
    }

    spec_draft_local(&choice, topic, &scaffold, &label, &ctx, out)
}

/// Draft with the local llama.cpp model the resolver chose.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
fn spec_draft_local(
    choice: &rto_graph::ModelChoice,
    topic: &str,
    scaffold: &str,
    label: &str,
    ctx: &rto_spec::SpecContext,
    out: Option<&str>,
) -> anyhow::Result<()> {
    use rto_graph::ModelSource;

    let Some(model) = choice.model else {
        anyhow::bail!("no generative model in the registry");
    };
    if choice.installed == Some(false) {
        // A **pinned** model that is not installed is a hard error naming the
        // key, matching what `roteiro infer` has always done with a configured
        // embedding model: the operator asked for that model specifically, and
        // quietly producing a model-less scaffold answers a question they did not
        // ask. An *unpinned* default that is not installed keeps the note-and-
        // scaffold path unchanged — nothing was asked for, so nothing is refused.
        if choice.source == ModelSource::Pinned {
            return Err(choice
                .require_installed()
                .expect_err("an uninstalled pin errors")
                .into());
        }
        eprintln!(
            "note: generative model `{model}` is not installed — emitting the scaffold. \
             Draft prose with: roteiro model pull {model}"
        );
        return emit_artifact(scaffold, &format!("{label} scaffold"), out);
    }

    if cfg!(debug_assertions) {
        eprintln!(
            "note: unoptimized build — local generation is very slow; use a \
             release build (`cargo build --release`) for usable speed."
        );
    }
    let drafts = draft_sections(model, scaffold, topic, ctx)?;
    eprintln!(
        "drafted {} section(s) with {} (via {GEN_BACKEND})",
        drafts.len(),
        model
    );
    let md = rto_spec::apply_drafts(scaffold, &drafts);
    emit_artifact(&md, &format!("{label} draft"), out)
}

/// `spec draft` in a build with the remote tier and **no local generator**: the
/// resolver still ran, and the honest answer is that this build has nothing to
/// draft with unless the run grants the tier.
///
/// Reachable only under `--features remote` without `serve` or
/// `inference-local-models`, and it deliberately does not emit a bare scaffold:
/// `spec scaffold` is the command for that, and quietly substituting it here
/// would answer a question nobody asked.
#[cfg(all(
    feature = "remote",
    not(any(feature = "serve", feature = "inference-local-models"))
))]
fn spec_draft_local(
    _choice: &rto_graph::ModelChoice,
    _topic: &str,
    _scaffold: &str,
    _label: &str,
    _ctx: &rto_spec::SpecContext,
    _out: Option<&str>,
) -> anyhow::Result<()> {
    anyhow::bail!(
        "this build has the remote model tier but no local generation backend, and this run \
         did not grant the tier — so there is nothing to draft with. Pass `--allow-remote` \
         (your `~/.roteiro/config.toml` must grant it too — see `roteiro remote status`), or \
         rebuild with `--features inference-local-models`. (`spec scaffold` works with no \
         model at all.)"
    )
}

/// **Draft with the hosted model — one remote call per unfilled section.**
///
/// # The prompt is rebuilt, not forwarded
///
/// The local path calls [`rto_spec::draft_prompt`], which interpolates the
/// grounded symbol and ADR names **into a string**. Sending that string as the
/// instruction would put graph content on the wire without it ever passing
/// through [`rto_remote::ContextItem::from_node`] — the allow-list would still
/// exist and would simply have nothing to do, which is the "whatever the local
/// path happened to build" assembly ADR-0019 §4 forbids by name.
///
/// So this rebuilds: [`remote_draft_instruction`] carries the *task* and no
/// graph content at all, and the nodes travel as allow-listed context items.
/// The consequence is stated rather than hidden — **the remote draft is not the
/// local prompt.** It cannot be, without unmaking the guard.
///
/// # One call per section, and each one is an egress
///
/// Sections are drafted separately because they are separate questions, and each
/// call is recorded on its own line of the ledger. A single batched call would
/// read as one disclosure in the record when it was several.
///
/// # Errors
/// Every refusal in [`rto_remote::call_with`] plus [`rto_remote::response::parse`]'s
/// — a truncated generation, an endpoint-reported error, an unwritable ledger.
/// **None of them falls back to a local model**, and the messages say so.
#[cfg(feature = "remote")]
fn spec_draft_remote(
    grant: &RemoteGrant,
    topic: &str,
    scaffold: &str,
    label: &str,
    ctx: &rto_spec::SpecContext,
    out: Option<&str>,
) -> anyhow::Result<()> {
    let nodes = remote_context_nodes(ctx)?;
    let ledger = remote_ledger()?;
    let targets = rto_spec::draft_targets(scaffold);

    let mut drafts = Vec::with_capacity(targets.len());
    let mut sent_bytes = 0usize;
    let mut fields: Vec<&'static str> = Vec::new();
    for (heading, hint) in targets {
        let instruction = remote_draft_instruction(topic, &heading, &hint);
        let payload = rto_remote::Payload::new(&instruction, &nodes)?;
        if fields.is_empty() {
            fields = payload.fields_present();
        }
        sent_bytes += rto_remote::dry_run(&grant.endpoint, &payload).len();
        let raw = rto_remote::call_with(
            &grant.endpoint,
            &payload,
            grant.decision,
            &ledger,
            &|| rto_exec::rfc3339_utc(std::time::SystemTime::now()),
            Some(&remote_transport::call),
        )?;
        let answer = rto_remote::response::parse(&raw)?;
        if let Some(note) = answer.model_discrepancy(grant.endpoint.model()) {
            eprintln!("{note}");
        }
        drafts.push((heading, strip_thinking(&answer.text)));
    }

    // On stderr and unconditional, on the same terms as `roteiro remote call`'s
    // receipt: a disclosure that only shows up when you ask for it is one people
    // stop seeing. The trust caveat rides with it, because the drafted prose is
    // about to be written to a file and the file will not carry one.
    eprintln!(
        "\ndrafted {} section(s) with {} via the remote model tier\n\
         — {} call(s), {} bytes total ({}) sent to {} ({}); recorded in {}",
        drafts.len(),
        grant.endpoint.model(),
        drafts.len(),
        sent_bytes,
        fields.join(", "),
        grant.endpoint.url(),
        grant.endpoint.trust().as_str(),
        ledger.path().display(),
    );
    if let Some(caveat) = grant.endpoint.trust().caveat() {
        eprintln!("  note: {caveat}");
    }

    let md = rto_spec::apply_drafts(scaffold, &drafts);
    emit_artifact(&md, &format!("{label} draft"), out)
}

/// The instruction for one remotely-drafted section — **the task, and no graph
/// content.**
///
/// Deliberately not [`rto_spec::draft_prompt`]: that function names the grounded
/// symbols and ADRs inline, and inline is exactly where the allow-list cannot
/// see them. Here the graph arrives as [`rto_remote::ContextItem`]s beside this
/// text, so what leaves the machine is decided by
/// [`rto_remote::ContextItem::from_node`] and by nothing else.
#[cfg(feature = "remote")]
fn remote_draft_instruction(topic: &str, heading: &str, hint: &str) -> String {
    let focus = if hint.is_empty() {
        String::new()
    } else {
        format!("Focus: {hint}. ")
    };
    format!(
        "You are drafting the \"{heading}\" section of a house-style technical document about \
         \"{topic}\". {focus}Write 2–4 precise, technical sentences. Reference only the graph \
         nodes given to you; do not invent symbols, files, or facts, and say so if what you \
         were given is not enough. Output only the prose, no heading."
    )
}

/// The context nodes a remote draft may carry, read back out of the store.
///
/// [`rto_spec::SpecContext`] holds [`rto_graph::NodeSummary`]s, not nodes, and a
/// summary cannot be fed to [`rto_remote::ContextItem::from_node`] — which is
/// the point: the allow-list reads five named fields off a real node, and
/// re-deriving it from a summary would be a second, unreviewed allow-list. So
/// the keys are resolved back to nodes and the one allow-list does the rest.
///
/// A key that no longer resolves is **dropped rather than raised**: the graph was
/// built moments ago by `build_scaffold`, so a miss means a concurrent change,
/// and sending less context is the safe direction to fail in.
///
/// Bounded by [`rto_remote::payload::MAX_CONTEXT_ITEMS`] here as well as there,
/// so the cap is a truncation of what is *requested* rather than a refusal at
/// assembly time.
#[cfg(feature = "remote")]
fn remote_context_nodes(ctx: &rto_spec::SpecContext) -> anyhow::Result<Vec<rto_graph::Node>> {
    let keys: Vec<&str> = ctx
        .symbols
        .iter()
        .map(|s| s.node.key.as_str())
        .chain(ctx.docs.iter().map(|d| d.key.as_str()))
        .take(rto_remote::payload::MAX_CONTEXT_ITEMS)
        .collect();
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let (_repo, store, _cache) = open_graph()?;
    let mut nodes = Vec::with_capacity(keys.len());
    for key in keys {
        if let Some(node) = store.get_node(key)? {
            nodes.push(node);
        }
    }
    Ok(nodes)
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
///
/// Applied to remote completions as well as local ones: a hosted reasoning model
/// emits the same block, and leaving it in would splice a model's scratch
/// thinking into a drafted document.
#[cfg(any(
    feature = "serve",
    feature = "inference-local-models",
    feature = "remote"
))]
fn strip_thinking(text: &str) -> String {
    match text.find("</think>") {
        Some(end) => text[end + "</think>".len()..].trim_start().to_owned(),
        None => text.to_owned(),
    }
}

/// [`strip_thinking`] for `review_llm`, which needs it for the same reason and
/// must not grow a second copy: a reviewer that parsed a model's `<think>` block
/// would read its scratch reasoning as findings, and a reasoning model deliberates
/// about defects it then rejects.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
pub fn strip_thinking_public(text: &str) -> String {
    strip_thinking(text)
}

/// `spec draft` without any generation backend: guide the user to enable one.
///
/// Three now, not two — the remote tier is a backend for this command as of
/// Stage 34 part 2b, and naming only the llama.cpp pair would send someone to a
/// C/C++ toolchain they may not want when a build flag they already have would
/// do.
#[cfg(not(any(
    feature = "serve",
    feature = "inference-local-models",
    feature = "remote"
)))]
fn run_spec_draft(
    _cfg: &config::Loaded,
    _ingest: rto_graph::IngestConfig,
    _topic: &str,
    _title: Option<&str>,
    _kind: &str,
    _out: Option<&str>,
) -> anyhow::Result<()> {
    anyhow::bail!(
        "`spec draft` needs a generation backend: build with `--features serve` \
         or `--features inference-local-models` (both llama.cpp), then \
         `roteiro model pull qwen3-0.6b` — or `--features remote` for the \
         explicitly-consented hosted tier (ADR-0019), which needs no local model \
         and sends only what `roteiro remote dry-run` shows. \
         (`spec scaffold` works with no model at all.)"
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
    refresh_for_read(&repo, &mut store, &cache, ingest, GraphSource::Committed)?;

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
/// `[generated]` and tagged with the producer that wrote it (ADR-0015).
/// `--include-memory` does the same for episodic agent memory (ADR-0013), under a
/// heading of its own, each line prefixed `[memory]` and carrying the anchor state
/// that says whether the record applies to this tree.
///
/// The channels are ranked separately and limited separately: opting in cannot
/// displace a graph hit, and neither a generated nor a remembered hit can ever be
/// read as an extracted fact.
#[allow(clippy::fn_params_excessive_bools)]
fn run_search(
    ingest: rto_graph::IngestConfig,
    query: &str,
    limit: usize,
    include_generated: bool,
    include_memory: bool,
    json: bool,
) -> anyhow::Result<()> {
    let (repo, mut store, cache) = open_graph()?;
    refresh_for_read(&repo, &mut store, &cache, ingest, GraphSource::Committed)?;

    let opts = rto_graph::SearchOptions {
        limit,
        include_generated,
        include_memory,
    };
    let results = rto_graph::search_channels(&store, query, opts)?;
    if json {
        // Without an opt-in, emit exactly the shape callers already parse: the
        // bare array of graph hits. Adding a wrapper for everyone would be a
        // breaking change to pay for a feature they did not ask for.
        if include_generated || include_memory {
            emit_json(&results)?;
        } else {
            emit_json(&results.hits)?;
        }
        return Ok(());
    }
    if results.hits.is_empty() && results.generated.is_empty() && results.memory.is_empty() {
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
    if !results.memory.is_empty() {
        println!();
        println!(
            "agent memory — what earlier sessions learned; unreviewed, unredacted, and not \
             a graph fact:"
        );
        for hit in &results.memory {
            // The anchor state is on the line rather than in a footnote: a lesson
            // about code that has moved is worth reading *and* worth labelling,
            // and the label is the difference between the two.
            println!(
                "  {:>4}  [memory:{}]  #{}  ({})",
                hit.score, hit.kind, hit.id, hit.anchor_state,
            );
            if let Some(snippet) = &hit.snippet {
                println!("        {snippet}");
            }
        }
        let inapplicable = results.memory.iter().filter(|h| !h.applies).count();
        println!("{} memory hit(s)", results.memory.len());
        if inapplicable > 0 {
            println!(
                "{inapplicable} of them do not apply to this tree — their anchors do not \
                 resolve here in the same form. Ranked lower, never withheld."
            );
        }
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
        // The model *this repository* would use, not the built-in default: a
        // project that pinned `[models] audio` and is told to pull the default
        // would pull the wrong model and still be unable to build. A pin that
        // cannot be honoured is reported as itself, since "pull this" is not the
        // fix for it.
        let (model, why) = match resolved_media_model(kind) {
            Ok(pair) => pair,
            Err(err) => {
                println!("  unavailable  {kind}: {err}");
                continue;
            }
        };
        if kind.compiled_in() {
            println!(
                "  unavailable  {kind}: the model is not installed — run \
                 `roteiro model pull {model}`{why}"
            );
        } else {
            println!(
                "  unavailable  {kind}: this build has no {kind} generator — rebuild with \
                 `--features {}`, then `roteiro model pull {model}`{why}",
                kind.feature(),
            );
        }
    }
    Ok(())
}

/// The model `media status` should tell an operator to pull for `kind`, and the
/// parenthetical saying where that choice came from — or the resolver's error,
/// for a `[models]` pin that cannot be honoured.
///
/// Naming the *resolved* model matters here more than it looks: a project that
/// pinned `[models] audio` and was told to pull the built-in default would pull
/// the wrong weights and still not be able to build.
#[cfg(feature = "models")]
fn resolved_media_model(kind: rto_graph::MediaKind) -> Result<(String, String), String> {
    match rto_graph::resolve_model(kind.task()) {
        Ok(choice) => Ok((choice.label().to_owned(), format!(" ({})", choice.why()))),
        Err(err) => Err(err.to_string()),
    }
}

/// Without the registry there is nothing to resolve against — and nothing a pin
/// could be honoured *with* — so the modality's built-in default is both the
/// only answer available and the correct one.
#[cfg(not(feature = "models"))]
fn resolved_media_model(kind: rto_graph::MediaKind) -> Result<(String, String), String> {
    Ok((kind.model().to_owned(), String::new()))
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
        MemoryAction::Recall {
            query,
            decay,
            scope,
            kind,
            anchor,
            applicable_only,
            limit,
            json,
        } => run_memory_recall(
            &rto_graph::RecallOptions {
                scope: scope.as_deref(),
                kind,
                anchor_key: anchor.as_deref(),
                query: query.as_deref(),
                decay,
                applicable_only,
                limit: Some(limit),
            },
            json,
        ),
        MemoryAction::Cache {
            sweep,
            budget_mb,
            json,
        } => run_memory_cache(sweep, budget_mb, json),
        MemoryAction::Forget { id, json } => run_memory_forget(id, json),
    }
}

/// The cache budget for this run: an explicit `--budget-mb` if one was given,
/// otherwise whatever `ROTEIRO_CACHE_BUDGET_MB` or the 256 MB default says.
fn resolve_cache_budget(budget_mb: Option<u64>) -> anyhow::Result<u64> {
    match budget_mb {
        Some(mb) => mb
            .checked_mul(1024 * 1024)
            .ok_or_else(|| anyhow::anyhow!("--budget-mb {mb} is more megabytes than there are")),
        None => Ok(rto_graph::cache_budget_bytes()?),
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

/// Recall the live records, ranked by evidence.
fn run_memory_recall(opts: &rto_graph::RecallOptions<'_>, json: bool) -> anyhow::Result<()> {
    let (_repo, store, _cache) = open_graph()?;
    let recall = store.recall_memory(opts)?;

    if json {
        emit_json(&recall)?;
        return Ok(());
    }
    if recall.results.is_empty() {
        if recall.live == 0 && recall.superseded == 0 {
            println!("nothing remembered yet (`roteiro memory add \"<what you learned>\"`)");
        } else {
            println!(
                "nothing matched; {} live and {} superseded record(s) are stored",
                recall.live, recall.superseded
            );
        }
        return Ok(());
    }
    for recalled in &recall.results {
        let record = &recalled.record;
        println!(
            "{:>5.3}  #{:<4} {:<8} {}",
            recalled.score,
            record.id,
            record.kind.as_str(),
            applicability(record),
        );
        // The terms, not just the product: a ranking that cannot be taken apart
        // is a ranking the reader has to take on trust, and depreciating by
        // evidence is worth nothing if the evidence is not visible.
        println!(
            "        confidence {:.2} × anchor {:.2} × decay {:.2}  (age {} generation(s))",
            recalled.base_confidence, recalled.anchor_penalty, recalled.decay_factor, recalled.age,
        );
        println!("        {}", first_line(&record.body, 96));
    }
    println!(
        "{} of {} live record(s), ranked at generation {} with decay {}{}",
        recall.results.len(),
        recall.live,
        recall.generation,
        recall.decay,
        if recall.reproducible {
            " (reproducible: the same store and tree recall this exactly)"
        } else {
            " (not reproducible: the ranking moves as records are written)"
        },
    );
    if recall.superseded > 0 {
        println!(
            "{} superseded record(s) are stored and never recalled — regardless of age. \
             `roteiro memory list --include-superseded` is the audit view.",
            recall.superseded,
        );
    }
    Ok(())
}

/// Report on — or sweep — the bounded cache tier.
fn run_memory_cache(sweep: bool, budget_mb: Option<u64>, json: bool) -> anyhow::Result<()> {
    let budget = resolve_cache_budget(budget_mb)?;
    let (_repo, mut store, _cache) = open_graph()?;

    if sweep {
        let swept = store.sweep_agent_cache(budget)?;
        if json {
            emit_json(&swept)?;
            return Ok(());
        }
        println!(
            "swept {} cache entr(ies): {} evicted, {} pinned, {} bytes freed",
            swept.scanned, swept.evicted, swept.pinned, swept.freed_bytes,
        );
        println!(
            "  {} of {} bytes retained (generation {})",
            swept.retained_bytes, swept.budget_bytes, swept.generation,
        );
        if swept.over_budget {
            // Never silent: a bound that failed to bind and a bound with nothing
            // to do look identical from the outside and mean opposite things.
            println!(
                "  still over budget — what remains is pinned: this generation's own work \
                 with a valid anchor, plus the most-recently-used entry, which is always kept"
            );
        }
        println!("episodic memory is untouched: no sweep can reach it");
        return Ok(());
    }

    let stats = store.agent_cache_stats(budget)?;
    if json {
        emit_json(&stats)?;
        return Ok(());
    }
    println!(
        "cache tier: {} entr(ies), {} of {} bytes (generation {})",
        stats.entries, stats.bytes, stats.budget_bytes, stats.generation,
    );
    if stats.bytes > stats.budget_bytes {
        println!("  over budget — `roteiro memory cache --sweep` reclaims it");
    }
    let (live, superseded) = store.memory_counts()?;
    println!(
        "episodic memory: {live} live, {superseded} superseded — unbounded by design, and \
         removed only by `roteiro memory forget`",
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

/// What the object-cache sweep reclaimed, and **why it kept what it kept**.
///
/// Both halves are load-bearing. "0 objects freed" on a cache holding a single
/// generation is the correct, healthy answer and reads as a failure without the
/// retained count beside it — and the retained count is itself four different
/// things. `sweep_superseded` keeps the live generation, the older generation it
/// holds as insurance, anything written by a *newer* build sharing this cache,
/// and every key it could not parse. A summary naming only the first two would
/// be describing an irreversible operation inaccurately, which is the failure
/// this project keeps finding: a message that reports a scope it did not act on.
///
/// So each class is named and counted, including the zeroes — a count that is
/// only printed once it is non-zero is a count nobody notices becoming non-zero.
/// Bytes are reported for the totals only: the split by class would need a second
/// stat pass over the whole cache, which is not worth it for a status line.
fn object_sweep_lines(reclaimed: &rto_graph::ReclaimReport) -> String {
    let swept = &reclaimed.sweep;
    // Never deleted, and never quietly folded into a total either: an
    // unrecognised key is either a format this build no longer writes or a bug in
    // the key parser, and both are things the reader would want to go and look at.
    let advisory = if reclaimed.kept_unrecognised > 0 {
        "\n  an unrecognised key is always kept — but it means either a format this build no \
         longer writes, or a bug in the key parser. Both are worth a look."
    } else {
        ""
    };
    format!(
        "object cache swept: {} superseded object(s) freed ({}), {} retained ({})\
         \n  retained: {} at this build's generation, {} at an older generation kept as \
         insurance, {} written by a newer build, {} whose key this build does not \
         recognise{advisory}",
        swept.removed,
        human_bytes(swept.freed_bytes),
        swept.retained,
        human_bytes(swept.retained_bytes),
        reclaimed.kept_current,
        reclaimed.kept_recent,
        reclaimed.kept_ahead,
        reclaimed.kept_unrecognised,
    )
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
    refresh_for_read(&repo, &mut store, &cache, ingest, GraphSource::Committed)?;

    if refresh {
        let report = refresh_contexts(&store)?;
        // **The maintenance seam.** The bounded cache tier is swept here and
        // nowhere else — never on a read path, so an ordinary query never mutates
        // the store (ADR-0013). Nothing episodic is reachable from this call:
        // `agent_memory` carries no size or recency column for a capacity policy
        // to grip it by, so `roteiro memory forget` stays the only thing that
        // removes a remembered record.
        let swept = store.sweep_agent_cache(resolve_cache_budget(None)?)?;
        // The same seam, the other cache. The on-disk object cache is the larger
        // of the two by an order of magnitude and had no reclaim at all: an
        // `EXTRACT_VERSION` bump orphaned every entry and nothing ever removed one
        // (issue #387). It hangs here rather than growing a second maintenance
        // concept, and for the same reason the tier sweep does — `sync` is reached
        // from `refresh_for_read` on every ordinary query, so sweeping there would
        // put deletion back on the read path this seam exists to keep it off.
        let reclaimed = rto_graph::sweep_superseded(&cache, rto_graph::DEFAULT_KEEP_GENERATIONS)?;
        if json {
            // The long-standing shape, unchanged: callers already parse this
            // object, and wrapping it for everyone would be a breaking change to
            // pay for maintenance they did not ask about. The machine-readable
            // sweep is `roteiro memory cache --sweep --json`. What a sweep
            // *did* is still never silent — it goes to stderr, where it cannot
            // corrupt the parsed stdout, and only when there is something to say.
            emit_json(&report)?;
            if swept.evicted > 0 || swept.over_budget {
                eprintln!(
                    "cache tier swept: {} evicted, {} of {} bytes retained{}",
                    swept.evicted,
                    swept.retained_bytes,
                    swept.budget_bytes,
                    if swept.over_budget {
                        " (still over budget: what remains is pinned)"
                    } else {
                        ""
                    },
                );
            }
            // Same rule as the tier sweep above: only when there is something
            // to say. An unrecognised key counts as something to say — it is the
            // one class the reader may need to act on.
            if reclaimed.sweep.removed > 0
                || reclaimed.sweep.failed > 0
                || reclaimed.kept_unrecognised > 0
            {
                eprintln!("{}", object_sweep_lines(&reclaimed));
            }
        } else {
            println!(
                "context cache refreshed: {} rebuilt, {} reused, {} pruned",
                report.rebuilt, report.reused, report.pruned
            );
            println!(
                "cache tier swept: {} evicted, {} pinned, {} of {} bytes retained",
                swept.evicted, swept.pinned, swept.retained_bytes, swept.budget_bytes,
            );
            if swept.over_budget {
                println!(
                    "  still over budget — what remains is pinned (this generation's own \
                     work, and the most-recently-used entry, which is always kept)"
                );
            }
            println!("{}", object_sweep_lines(&reclaimed));
            if reclaimed.sweep.failed > 0 {
                println!(
                    "  {} superseded object(s) could not be deleted — check permissions on \
                     the cache directory",
                    reclaimed.sweep.failed,
                );
            }
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

/// Parse a `--order` token into a [`rto_graph::DensityOrder`], naming the
/// accepted set from the type itself so the CLI and the tool schemas cannot
/// drift apart.
fn parse_density_order(token: &str) -> anyhow::Result<rto_graph::DensityOrder> {
    rto_graph::DensityOrder::from_token(token).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown --order `{token}` (expected: {})",
            rto_graph::DensityOrder::tokens().join(" | ")
        )
    })
}

/// Rank files by intent-debt density. A report, not a gate: it always exits
/// zero, for the reasons in [`rto_graph::debt_density`]'s own documentation.
fn run_debt_density(
    ingest: rto_graph::IngestConfig,
    kinds: &[String],
    order: &str,
    limit: usize,
    min_lines: u32,
    json: bool,
    debt_ignore: &[String],
) -> anyhow::Result<()> {
    let order = parse_density_order(order)?;
    let (repo, mut store, cache) = open_graph()?;
    build_graph(&repo, &mut store, &cache, ingest, GraphSource::Committed)?;

    let report = rto_graph::debt_density(&store, kinds, debt_ignore, order, limit, min_lines)?;
    if json {
        emit_json(&report)?;
    } else {
        for item in &report.items {
            let cats: Vec<String> = item
                .by_category
                .iter()
                .map(|(cat, n)| format!("{cat} {n}"))
                .collect();
            println!(
                "  {:>8.2}/kloc  {:>4} in {:>6} lines  {}  ({})",
                item.per_kloc,
                item.markers,
                item.lines,
                item.path,
                cats.join(", ")
            );
        }
        println!("{}", density_summary(&report));
    }
    Ok(())
}

/// A one- or two-line summary of a [`rto_graph::DebtDensityReport`]. States the
/// population and the repository-wide baseline alongside the shown rows, because
/// a per-file density means nothing without one to compare it against, and names
/// what the denominator is so the figure is not read as source lines of code.
fn density_summary(report: &rto_graph::DebtDensityReport) -> String {
    if report.files_with_markers == 0 {
        return "intent-debt density: no markers in this graph".to_owned();
    }
    let mut excluded = Vec::new();
    if report.short_files > 0 {
        excluded.push(format!(
            "{} file(s) under {} lines",
            report.short_files, report.min_lines
        ));
    }
    if report.unknown_length_files > 0 {
        excluded.push(format!(
            "{} file(s) of unrecorded length",
            report.unknown_length_files
        ));
    }
    let excluded = if excluded.is_empty() {
        String::new()
    } else {
        format!(", excluding {}", excluded.join(" and "))
    };
    format!(
        "intent-debt density: {} of {} ranked file(s) shown, by {}; \
         {} marker(s) over {} file(s){excluded}\n\
         baseline: {:.2} marker(s) per 1,000 lines across {} ranked line(s) \
         — file length, blanks and comments included, not source lines of code",
        report.items.len(),
        report.ranked_files,
        report.order,
        report.total_markers,
        report.files_with_markers,
        report.overall_per_kloc,
        report.total_lines,
    )
}

/// Inventory the secret-named config keys and their redaction state. A report,
/// not a gate: it always exits zero — including when `unredacted` is non-zero,
/// because that finding is about **this store**, not about the user's source, and
/// failing their build over Roteiro's own import layer would be the wrong
/// response. See [`rto_graph::config_secrets`] for what this cannot do.
fn run_config_secrets(
    ingest: rto_graph::IngestConfig,
    limit: usize,
    json: bool,
) -> anyhow::Result<()> {
    let (repo, mut store, cache) = open_graph()?;
    build_graph(&repo, &mut store, &cache, ingest, GraphSource::Committed)?;

    let report = rto_graph::config_secrets(&store, limit)?;
    if json {
        emit_json(&report)?;
    } else {
        for item in &report.items {
            let loc = item.path.as_deref().unwrap_or(&item.key);
            println!("  [{:>8}] {loc}  {}", item.state.as_str(), item.name);
        }
        for line in config_secrets_summary(&report) {
            println!("{line}");
        }
    }
    Ok(())
}

/// The summary lines for a [`rto_graph::ConfigSecretReport`], as separate lines
/// so the caveat is never truncated onto the end of a count.
///
/// The caveat is unconditional — printed even for an empty report, because an
/// empty report is exactly where a reader is most likely to conclude something
/// this lens never claimed.
fn config_secrets_summary(report: &rto_graph::ConfigSecretReport) -> Vec<String> {
    let mut lines = Vec::new();
    if report.secret_named == 0 {
        lines.push(format!(
            "config secrets: no secret-named config key among {} config key(s)",
            report.config_keys
        ));
    } else {
        lines.push(format!(
            "config secrets: {} of {} secret-named key(s) shown, across {} file(s); \
             {} redacted, {} declared in code without a value, {} unredacted",
            report.items.len(),
            report.secret_named,
            report.files,
            report.redacted,
            report.declared,
            report.unredacted,
        ));
    }
    if report.redacted_not_secret_named > 0 {
        lines.push(format!(
            "  plus {} redacted value(s) under non-secret key name(s) (e.g. a k8s \
             Secret's data), counted but not listed",
            report.redacted_not_secret_named
        ));
    }
    if report.unredacted > 0 {
        // Loud, because extraction cannot produce this state: something else put
        // an unredacted value in this store.
        lines.push(format!(
            "WARNING: {} secret-named key(s) carry an unredacted value. Extraction \
             always redacts, so these came from an import layer — inspect the \
             importing tool, not the source repository",
            report.unredacted
        ));
    }
    lines.push(
        "note: an inventory of secret-NAMED config keys, not a secret scan — it \
         cannot see a hardcoded credential in source, cannot judge a value (it \
         never sees one), and cannot tell a real secret from a placeholder"
            .to_owned(),
    );
    lines
}

/// Parse a `--order` token into a [`rto_graph::CouplingOrder`], naming the
/// accepted set from the type itself so the CLI and the tool schemas cannot
/// drift apart.
fn parse_coupling_order(token: &str) -> anyhow::Result<rto_graph::CouplingOrder> {
    rto_graph::CouplingOrder::from_token(token).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown --order `{token}` (expected: {})",
            rto_graph::CouplingOrder::tokens().join(" | ")
        )
    })
}

/// Rank nodes by directed call coupling. A report, not a gate: it always exits
/// zero.
fn run_coupling(
    ingest: rto_graph::IngestConfig,
    order: &str,
    limit: usize,
    json: bool,
) -> anyhow::Result<()> {
    let order = parse_coupling_order(order)?;
    let (repo, mut store, cache) = open_graph()?;
    build_graph(&repo, &mut store, &cache, ingest, GraphSource::Committed)?;

    let report = rto_graph::coupling(&store, order, limit)?;
    if json {
        emit_json(&report)?;
    } else {
        for item in &report.items {
            let loc = item.path.as_deref().unwrap_or(&item.key);
            println!(
                "  in {:>4}  out {:>4}  I={:.2}  {}  ({loc})",
                item.fan_in, item.fan_out, item.instability, item.name
            );
        }
        println!("{}", coupling_summary(&report));
    }
    Ok(())
}

/// A one-line summary of a [`rto_graph::CouplingReport`]. States the population
/// as well as the shown rows, so a capped list cannot be read as the whole
/// graph, and names the edge kind, so the number is never mistaken for a degree
/// over every edge.
fn coupling_summary(report: &rto_graph::CouplingReport) -> String {
    if report.coupled_nodes == 0 {
        return "call coupling: no `calls` edges in this graph".to_owned();
    }
    let mut excluded = Vec::new();
    if report.self_calls > 0 {
        excluded.push(format!("{} self-call(s)", report.self_calls));
    }
    if report.cross_language_calls > 0 {
        excluded.push(format!(
            "{} cross-language name collision(s)",
            report.cross_language_calls
        ));
    }
    let excluded = if excluded.is_empty() {
        String::new()
    } else {
        format!(", excluding {}", excluded.join(" and "))
    };
    format!(
        "call coupling: {} of {} coupled node(s) shown, by {}; {} `calls` edge(s){excluded}",
        report.items.len(),
        report.coupled_nodes,
        report.order,
        report.call_edges,
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
    refresh_for_read(&repo, &mut store, &cache, ingest, GraphSource::Committed)?;

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
        #[cfg(not(feature = "exec-subprocess"))]
        SecurityAction::Run { analyzer, .. } => anyhow::bail!(
            "`roteiro security run {analyzer}` needs the `exec-subprocess` feature, which this \
             build does not have; rebuild with `--features exec-subprocess` (it is in the default \
             set, so this is a `--no-default-features` build). To get findings without executing \
             an analyzer here, run it elsewhere and use `roteiro security ingest` — this build \
             can still `prefetch` and `status` the assets."
        ),
        #[cfg(feature = "execution")]
        SecurityAction::Prefetch {
            analyzer,
            allow_download,
            json,
        } => run_security_prefetch(analyzer.as_deref(), allow_download, json),
        #[cfg(feature = "execution")]
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
    /// Total findings across those layers. **Unchanged** by the cross-reference
    /// below — see [`SecurityCrossReference`].
    findings: usize,
    /// Dependency advisories seen across analyzers, where more than one
    /// dependency analyzer has a live layer. Empty otherwise, because there is
    /// nothing to cross-reference against.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    cross_reference: Vec<SecurityCrossReference>,
}

/// One advisory in the `--json` cross-reference (ADR-0018 v1.1).
///
/// A **view**, not a record: every finding it names is still in its own layer
/// under its own key, and `findings` above still counts them all. This is what
/// makes a duplicate pair read as one advisory confirmed by two analyzers rather
/// than as a count that silently halved.
#[cfg(feature = "execution")]
#[derive(serde::Serialize)]
struct SecurityCrossReference {
    /// The advisory's canonical id — the RUSTSEC id where both sides publish one.
    advisory: String,
    /// Every identifier it is published under.
    aliases: Vec<String>,
    /// The package and resolved version it is about.
    package: String,
    version: String,
    /// How many distinct analyzers reported it. `1` is a normal state, not a
    /// discrepancy: the two databases are pinned independently, and `yanked` is
    /// not an advisory kind OSV can carry at all.
    confirmed_by: usize,
    /// Which analyzers, and the still-addressable finding key each one wrote.
    reports: Vec<SecurityCrossReferenceReport>,
}

/// One analyzer's report inside a [`SecurityCrossReference`].
#[cfg(feature = "execution")]
#[derive(serde::Serialize)]
struct SecurityCrossReferenceReport {
    analyzer: String,
    /// The finding key, unchanged and still addressable.
    key: String,
    /// The id *this* analyzer fired, which need not be the canonical one.
    rule: String,
    severity: rto_graph::Severity,
}

/// The cross-reference for a listing, or empty when there is nothing to cross-
/// reference.
///
/// Below two dependency analyzers the section is suppressed rather than shown
/// with every row reading "confirmed by 1" — that would be noise dressed as
/// information, and it is exactly the case where a single source carries no
/// signal about agreement either way.
#[cfg(feature = "execution")]
fn security_cross_reference(layers: &[rto_graph::FindingsLayer]) -> Vec<SecurityCrossReference> {
    let correspondences = rto_exec::cross_reference(layers);
    let mut analyzers: Vec<&str> = correspondences
        .iter()
        .flat_map(|c| c.analyzers())
        .collect::<Vec<_>>();
    analyzers.sort_unstable();
    analyzers.dedup();
    if analyzers.len() < 2 {
        return Vec::new();
    }
    correspondences
        .into_iter()
        .map(|c| SecurityCrossReference {
            advisory: c.advisory,
            aliases: c.aliases,
            package: c.package,
            version: c.version,
            confirmed_by: c
                .reports
                .iter()
                .map(|r| &r.analyzer)
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            reports: c
                .reports
                .into_iter()
                .map(|r| SecurityCrossReferenceReport {
                    analyzer: r.analyzer,
                    key: r.key,
                    rule: r.rule,
                    severity: r.severity,
                })
                .collect(),
        })
        .collect()
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
/// Every `execution` build has an asset cache to describe now that provisioning
/// no longer sits behind a backend feature, so this no longer needs a
/// there-is-nothing-to-say counterpart. An unprovisioned cache is reported as
/// unprovisioned by `resolve`, which is a different thing from a build that
/// could not have one.
#[cfg(feature = "execution")]
fn advisory_db_evidence(analyzer: &str) -> Option<rto_graph::AdvisoryDb> {
    rto_exec::assets::advisory_db_evidence(&rto_exec::asset_root(), analyzer)
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
            // The checkout we are standing in, so an analyzer that reports
            // absolute paths — `osv-scanner` does — produces the same
            // worktree-relative finding keys here as it does under `security
            // run`. A report about some *other* tree simply will not relativise,
            // and the adapter records that rather than guessing.
            worktree: Some(&worktree_path),
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

    let cross_reference = security_cross_reference(&layers);

    if json {
        emit_json(&SecurityListing {
            layers,
            findings: total,
            cross_reference,
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
    print_cross_reference(&cross_reference);
    Ok(())
}

/// Render the cross-reference block beneath a listing (ADR-0018 v1.1).
///
/// The total above is printed **before** this and is not adjusted by it: a
/// duplicate pair is two findings and one advisory, and both numbers are true.
/// Advisories two analyzers agree on come first, because agreement between
/// independent sources is the evidence this decision exists to keep; the
/// single-source ones follow, counted rather than listed, and labelled as the
/// ordinary state they are.
#[cfg(feature = "execution")]
fn print_cross_reference(cross_reference: &[SecurityCrossReference]) {
    if cross_reference.is_empty() {
        return;
    }
    let (confirmed, single): (Vec<_>, Vec<_>) =
        cross_reference.iter().partition(|c| c.confirmed_by > 1);

    println!();
    println!(
        "cross-reference: {} advisory/advisories across analyzers",
        cross_reference.len()
    );
    for entry in &confirmed {
        let analyzers: Vec<&str> = entry
            .reports
            .iter()
            .map(|r| r.analyzer.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        println!(
            "  {} — {} {} — confirmed by {} ({})",
            entry.advisory,
            entry.package,
            entry.version,
            entry.confirmed_by,
            analyzers.join(", ")
        );
        // Both keys stay addressable: a reader who fixes the advisory must see
        // both of these disappear, and cannot do that without being told them.
        for report in &entry.reports {
            println!("      {}", report.key);
        }
    }
    if !single.is_empty() {
        let mut by_analyzer: std::collections::BTreeMap<&str, usize> =
            std::collections::BTreeMap::new();
        for entry in &single {
            for report in &entry.reports {
                *by_analyzer.entry(report.analyzer.as_str()).or_default() += 1;
            }
        }
        let breakdown: Vec<String> = by_analyzer
            .iter()
            .map(|(analyzer, count)| format!("{count} only in {analyzer}"))
            .collect();
        // Not a discrepancy. The two databases are pinned and prefetched
        // independently, so they legitimately differ for a window; and `yanked`
        // comes from the crates.io index rather than from an advisory, so OSV
        // cannot carry it at all.
        println!(
            "  {} reported by one analyzer ({}) — expected: the databases are pinned separately, \
             and some kinds only one of them carries",
            single.len(),
            breakdown.join(", ")
        );
    }
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
#[cfg(feature = "execution")]
#[derive(serde::Serialize)]
struct SecurityPrefetchReport {
    root: String,
    provisioned: Vec<rto_exec::InstalledAsset>,
}

/// Stream `url` into `path`, for the one asset kind that is fetched by URL.
///
/// The transport lives here rather than in `rto-exec` on purpose: that crate
/// has no network dependency and does not acquire one to provision an asset.
/// It takes this as a function, so the code that *could* fetch is reachable
/// only from `prefetch` — see [`rto_exec::provision_with`].
///
/// The body is streamed to disk rather than buffered: `npm/all.zip` alone is
/// around 210 MB, and reading that into a `Vec` to write it straight back out
/// would be a needless spike.
///
/// # A body of unknown length is refused, because this is where the pin is set
///
/// [`rto_exec::AssetSource::Download`] has **no compile-time digest** — the
/// upstream files are rebuilt daily, so what gets recorded as the asset's pin is
/// the digest of whatever this function wrote. Bytes that arrive here are
/// therefore self-certifying: nothing downstream can contradict them, and
/// `security status` will report a truncated database as present and matching.
///
/// `std::io::copy` returns `Ok` on a clean early EOF, so completeness has to be
/// established from the response's framing. Measured against **ureq 3.4.0**,
/// four of the five framings detect a truncated transfer on their own:
///
/// | Framing | Truncation detected |
/// |---|---|
/// | `Content-Length`, short body | yes — `UnexpectedEof` |
/// | `Content-Encoding: gzip` | yes — decode error |
/// | chunked, dropped mid-chunk | yes — `UnexpectedEof` |
/// | chunked, terminator missing | yes — `UnexpectedEof` |
/// | **close-delimited** (neither header) | **no — `Ok`** |
///
/// The last row is the hole, and it is not fixable by counting: that framing
/// *defines* the body as ending when the connection closes, so a mid-transfer
/// drop is indistinguishable from a complete file. A length that cannot be
/// established is not a length that checks out, so such a response is refused
/// rather than pinned — the same rule this feature applies to a cold cache,
/// which fails by name instead of quietly fetching.
///
/// `Accept-Encoding: identity` is sent to ask for the one framing that can be
/// verified end to end. The shipped OSV URLs answer it with `Content-Length` and
/// no transfer encoding, so this refuses nothing that works today. A mirror that
/// can only serve close-delimited bodies is not supported by `--allow-download`;
/// its files can still be placed in the documented cache layout by hand, which
/// `prefetch` then digests and pins without fetching anything.
///
/// The payload is deliberately **not** parsed. These are zips, and a structural
/// check would be redundant: `osv-scanner` 2.5.0 already refuses a truncated
/// database loudly — exit `127`, *"zip: not a valid zip file"* — and `127` is not
/// a declared success status, so a corrupt database fails a scan rather than
/// silently shrinking it. Parsing here would duplicate that for one asset kind
/// while doing nothing for a future non-zip one.
#[cfg(feature = "execution")]
fn download_asset_file(url: &str, path: &std::path::Path) -> Result<(), String> {
    let response = ureq::get(url)
        // Ask for the framing whose completeness can be checked. ureq decodes a
        // compressed body and then reports no `Content-Length` at all, which
        // would leave nothing to verify against.
        .header("Accept-Encoding", "identity")
        .call()
        .map_err(|e| format!("GET {url}: {e}"))?;

    let declared = declared_body_length(
        response
            .headers()
            .get("content-length")
            .and_then(|value| value.to_str().ok()),
    )
    .ok_or_else(|| {
        format!(
            "GET {url}: the response declares no usable Content-Length, so a complete download \
             cannot be told from a truncated one. Refusing rather than digesting bytes of unknown \
             completeness and recording them as this asset's pin. If the server cannot send a \
             Content-Length, place the file in the asset cache by hand — prefetch verifies and \
             pins what is already there without fetching."
        )
    })?;

    let mut reader = response.into_body().into_reader();
    let mut file =
        std::fs::File::create(path).map_err(|e| format!("creating {}: {e}", path.display()))?;
    // Names the URL as well as the path. The common failure here is the peer
    // hanging up mid-body — ureq enforces the declared framing and surfaces that
    // from this call — which is a fact about the transfer, not the local file.
    let written = std::io::copy(&mut reader, &mut file)
        .map_err(|e| format!("transferring {url} to {}: {e}", path.display()))?;
    std::io::Write::flush(&mut file).map_err(|e| format!("flushing {}: {e}", path.display()))?;

    verify_transferred(url, declared, written)
}

/// The body length the response declares, or `None` if it declares none usable.
///
/// Split out so the parsing has a test that does not need a socket. An empty,
/// negative or non-numeric value is `None` rather than an error: the caller's
/// answer to "no usable length" is the same refusal either way, and it says so
/// in one place.
#[cfg(feature = "execution")]
fn declared_body_length(header: Option<&str>) -> Option<u64> {
    header?.trim().parse::<u64>().ok()
}

/// Check what was written against what the server said it was sending.
///
/// Belt and braces over ureq's own `Content-Length` enforcement, which was
/// measured to catch this case already (see [`download_asset_file`]). It is kept
/// because the guarantee belongs to this function rather than to a dependency's
/// current behaviour: this is the only code that can put network bytes into a
/// pinned asset, and a ureq upgrade must not be able to silently relax it.
#[cfg(feature = "execution")]
fn verify_transferred(url: &str, declared: u64, written: u64) -> Result<(), String> {
    if written == declared {
        return Ok(());
    }
    Err(format!(
        "GET {url}: the server declared {declared} byte(s) and the transfer produced {written}. \
         A short asset must not be digested and pinned as a complete one, so nothing was installed."
    ))
}

/// Which assets `prefetch` provisions for a given `--analyzer`, or all of them.
///
/// Lifted out of [`run_security_prefetch`] so it can be tested. The fallback
/// below is the whole reason `--analyzer sandbox` works, and the comment
/// describing it in `rto-exec`'s asset table once outlived it by long enough to
/// put a wrong recipe in `AGENTS.md` (#362). A rule worth documenting twice is a
/// rule worth asserting once.
#[cfg(feature = "execution")]
fn assets_to_prefetch(analyzer: Option<&str>) -> anyhow::Result<Vec<&'static rto_exec::AssetSpec>> {
    let Some(name) = analyzer else {
        return Ok(rto_exec::ASSETS.iter().collect());
    };

    let mut specs = rto_exec::assets_for(name);
    if specs.is_empty() {
        // Some assets belong to no single analyzer — the sandbox runtime is
        // shared by every analyzer that runs in one — so fall back to selecting
        // by the spec's own owner. That is what makes `--analyzer sandbox`
        // provision the runtime without also fetching a quarter-gigabyte of
        // advisory databases.
        specs = rto_exec::ASSETS
            .iter()
            .filter(|s| s.analyzer == name)
            .collect();
    }
    if specs.is_empty() {
        anyhow::bail!(
            "no assets for `{name}` in this build (analyzers: {}; shared: {})",
            rto_exec::known_analyzers().join(", "),
            rto_exec::SANDBOX
        );
    }
    Ok(specs)
}

/// Install and verify every pinned asset, recording each digest.
///
/// This is the only command that writes to the asset cache, and the only one
/// that can reach the network at all — and only with `--allow-download`. The
/// rule set is compiled into this binary; the `RustSec` advisory database is a
/// directory the operator provides, and if it is absent this says where it
/// looked and which command obtains it rather than going and getting it; the
/// OSV databases are fetched by URL, which is what `--allow-download` is for.
#[cfg(feature = "execution")]
fn run_security_prefetch(
    analyzer: Option<&str>,
    allow_download: bool,
    json: bool,
) -> anyhow::Result<()> {
    let root = rto_exec::asset_root();
    let specs = assets_to_prefetch(analyzer)?;

    let fetcher: &rto_exec::Fetcher<'_> = &download_asset_file;
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
            if allow_download && let rto_exec::AssetSource::Download { files } = spec.source {
                // Name every URL before opening a socket to any of them. What
                // this command talks to is the operator's business, and a
                // quarter-gigabyte transfer should not be a surprise.
                eprintln!("  downloading {} file(s):", files.len());
                for file in files {
                    eprintln!("    {}", file.url);
                }
            }
            if let rto_exec::AssetSource::PinnedArchive { archives } = spec.source {
                // This one embeds third-party executables into any binary built
                // against it, some of them GPL-2.0 and LGPL-2.0. The full record
                // is printed before installation rather than linked, because a
                // licence obligation nobody read is how it gets breached.
                for archive in archives {
                    eprintln!("    {} → sha256 {}", archive.target, archive.sha256);
                }
                eprintln!("\n{}\n", rto_exec::SANDBOX_RUNTIME_NOTICE);
            }
        }
        // The fetcher is passed only when the operator asked for downloads. A
        // downloadable asset that is already present still provisions without
        // it, so re-running `prefetch` over a warm cache needs no flag and no
        // network.
        match rto_exec::provision_with(&root, spec, allow_download.then_some(fetcher)) {
            Ok(record) => provisioned.push(record),
            // One unprovisionable asset must not hide the others: report it and
            // carry on, then fail at the end with everything that went wrong.
            Err(e) => failures.push(format!("{e}")),
        }
    }

    // The pinned analyzer images live in boxlite's own store rather than the
    // asset cache, so they are provisioned alongside it rather than by it. Same
    // rule as everything else here: only with `--allow-download`, and a run
    // never does it.
    #[cfg(feature = "exec-boxlite")]
    if allow_download {
        for image in rto_exec::boxlite::SANDBOX_IMAGES {
            if analyzer.is_some_and(|name| name != image.analyzer) {
                continue;
            }
            if !json {
                eprintln!(
                    "pulling sandbox image for {} ({})",
                    image.analyzer, image.reference
                );
            }
            if let Err(e) = rto_exec::boxlite::provision_image(image.analyzer, &root) {
                failures.push(format!("{e}"));
            }
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
#[cfg(feature = "execution")]
#[derive(serde::Serialize)]
struct SecurityStatusReport {
    root: String,
    analyzers: Vec<AnalyzerCoverage>,
    assets: Vec<rto_exec::AssetStatus>,
    layers: Vec<LayerStaleness>,
}

/// What one shipped analyzer covers — the coverage matrix, read off the code
/// rather than off a document, so the two cannot drift apart unnoticed.
#[cfg(feature = "execution")]
#[derive(serde::Serialize)]
struct AnalyzerCoverage {
    analyzer: &'static str,
    summary: &'static str,
    languages: &'static [&'static str],
    /// Whether every asset it needs is provisioned and still matches its digest.
    ready: bool,
}

/// The staleness of the advisory data behind one live findings layer.
#[cfg(feature = "execution")]
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
#[cfg(feature = "execution")]
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
    /// The remote model tier's grant for **this server process**, or `None` for
    /// a server that will answer only from local models (ADR-0019 v1.2).
    ///
    /// Resolved once, in the command dispatch, and carried rather than
    /// re-derived: the gate has one implementation and a long-lived process must
    /// not consult it repeatedly and get different answers as files change under
    /// it. It is not persisted anywhere and dies with the process.
    #[cfg(all(feature = "remote", feature = "serve"))]
    remote: Option<RemoteGrant>,
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
    use rto_graph::{ModelKind, ModelTask, Platform, REGISTRY, is_installed, model_dir};
    let host = Platform::host();
    let wanted = cfg.serve.models.as_deref();
    let has_file = |m: &rto_graph::ModelSpec, name: &str| {
        m.variant_for(host)
            .is_some_and(|v| v.files.iter().any(|f| f.name == name))
    };
    // What `/v1` can serve is exactly what one of its two endpoints can use:
    // `/v1/chat/completions` (generative + vision) or `/v1/embeddings`. Asked of
    // the capability table (Stage 33) rather than re-listed here, so a new model
    // kind cannot become servable by being forgotten in this match.
    let endpoint_capable =
        |kind: ModelKind| ModelTask::Chat.capable(kind) || ModelTask::Embed.capable(kind);
    REGISTRY
        .iter()
        .filter(|m| wanted.is_none_or(|w| w.iter().any(|n| n == m.name)))
        .filter(|m| has_file(m, "model.gguf"))
        .filter(|m| endpoint_capable(m.kind))
        // A vision model is only servable with its multimodal projector.
        .filter(|m| m.kind != ModelKind::Vision || has_file(m, "mmproj.gguf"))
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
/// Since Stage 33 both halves come from the shared resolver rather than from a
/// rule written here: the membership filter is
/// [`rto_graph::ModelTask::capable`] — this function's own exclusion rule,
/// generalised into the table every surface now shares — and the pinned default
/// is [`rto_graph::resolve_model`], so a `[models] generative` naming an
/// embedding model is refused here for the same reason, with the same message,
/// as it is refused in `spec draft`.
///
/// Gated on `explorer` too: the Ask model pool only exists when the explorer UI
/// (and its `/v1/graph/capabilities` route) is compiled in.
///
/// # The remote tier, when this process granted it, leads
///
/// `remote_id` is the hosted model's served id, or `None` on a server that will
/// answer only locally. When it is set it goes to the front unconditionally and
/// the `[models] generative` rotation below does not run: a server started with
/// `--allow-remote` was started to use the tier, and leaving a local model in
/// `models[0]` would make the flag do nothing that the person who typed it could
/// see. The local models stay in the pool and stay addressable — only the
/// default moved.
///
/// The pin is *not* silently ignored in the process: `resolve_with_remote` has
/// already resolved it (so a broken one still failed at startup) and reported
/// the displacement through `ModelChoice::why`.
#[cfg(all(feature = "serve", feature = "explorer"))]
fn chat_capable_model_ids(
    cfg: &config::Config,
    served_ids: &[String],
    remote_id: Option<&str>,
) -> Vec<String> {
    use rto_graph::{ModelKind, ModelTask, find_model};
    let is_generative = |id: &str| find_model(id).is_some_and(|s| s.kind == ModelKind::Generative);
    // Drop what cannot chat; keep generative + vision (both can). An id the
    // registry does not know is kept, exactly as before — `served_ids` is the
    // engine's set of registry names, so that case is unreachable, and dropping
    // an unrecognised id would be a silent narrowing of the Ask pool.
    let mut ids: Vec<String> = served_ids
        .iter()
        .filter(|id| find_model(id).is_none_or(|s| ModelTask::Chat.capable(s.kind)))
        .cloned()
        .collect();
    // The granted tier leads, and nothing below reorders past it.
    if let Some(remote) = remote_id
        && let Some(pos) = ids.iter().position(|id| id == remote)
    {
        ids[..=pos].rotate_right(1);
        return ids;
    }
    // Pick the default and rotate it to the front, preserving the order of the
    // rest (a stable, predictable capabilities list). A pin the resolver refuses
    // is *not* silently replaced by the first served generative model: `serve`
    // validated it at startup and never got here.
    // Resolved from the passed config rather than from the process-wide pins, so
    // this stays a pure function of its arguments and the selection tests below
    // can drive it without touching process state.
    let pinned = rto_graph::resolve_model_with(ModelTask::Chat, &cfg.models.resolve())
        .ok()
        .filter(|c| c.source == rto_graph::ModelSource::Pinned)
        .and_then(|c| c.model);
    let default_pos = pinned
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
        let out = chat_capable_model_ids(&config::Config::default(), &served, None);
        assert_eq!(out, ids(&["qwen3-0.6b", "qwen3-8b"]));
        assert!(!out.iter().any(|m| m.contains("bge")), "no embedding model");
    }

    #[test]
    fn configured_generative_is_preferred_as_the_default() {
        // `[models] generative` wins the default slot when served and generative.
        let mut cfg = config::Config::default();
        cfg.models.generative = Some("qwen3-8b".to_owned());
        let served = ids(&["bge-small-en-v1.5-gguf", "qwen3-0.6b", "qwen3-8b"]);
        let out = chat_capable_model_ids(&cfg, &served, None);
        assert_eq!(out, ids(&["qwen3-8b", "qwen3-0.6b"]));
    }

    #[test]
    fn vision_models_stay_in_the_pool_but_a_generative_leads() {
        // Vision models can chat, so they remain — but a generative is the default.
        let served = ids(&["bge-small-en-v1.5-gguf", "smolvlm-500m-gguf", "qwen3-0.6b"]);
        let out = chat_capable_model_ids(&config::Config::default(), &served, None);
        assert_eq!(out, ids(&["qwen3-0.6b", "smolvlm-500m-gguf"]));
    }

    #[test]
    fn with_no_generative_a_vision_model_leads_never_an_embedding() {
        // Only an embedding + a vision model served: the embedding is dropped and
        // the (chat-capable) vision model becomes the default — never the encoder.
        let served = ids(&["bge-small-en-v1.5-gguf", "smolvlm-500m-gguf"]);
        let out = chat_capable_model_ids(&config::Config::default(), &served, None);
        assert_eq!(out, ids(&["smolvlm-500m-gguf"]));
    }

    /// **A granted tier takes the default slot the Ask UI sends.** A server
    /// started with `--allow-remote` was started to use the tier; leaving a local
    /// model in `models[0]` would make the flag do nothing its user could see.
    /// The local models stay in the pool and stay addressable by name — only the
    /// default moved, which is the difference between a default and a removal.
    #[cfg(feature = "remote")]
    #[test]
    fn a_granted_remote_model_leads_the_ask_pool_without_removing_the_local_ones() {
        let served = ids(&["bge-small-en-v1.5-gguf", "qwen3-0.6b", "some-vendor/model"]);
        let out = chat_capable_model_ids(
            &config::Config::default(),
            &served,
            Some("some-vendor/model"),
        );
        assert_eq!(out, ids(&["some-vendor/model", "qwen3-0.6b"]));

        // …and it outranks a `[models] generative` pin, which the resolver has
        // already reported as displaced rather than silently skipped.
        let mut cfg = config::Config::default();
        cfg.models.generative = Some("qwen3-8b".to_owned());
        let served = ids(&["qwen3-0.6b", "qwen3-8b", "some-vendor/model"]);
        let out = chat_capable_model_ids(&cfg, &served, Some("some-vendor/model"));
        assert_eq!(out[0], "some-vendor/model");
        assert!(out.contains(&"qwen3-8b".to_owned()), "{out:?}");

        // With no grant the pool is exactly what it was before the tier existed.
        assert_eq!(
            chat_capable_model_ids(&cfg, &served, None)[0],
            "qwen3-8b",
            "an ungranted server is unchanged"
        );
    }

    #[test]
    fn an_embedding_only_serve_offers_no_chat_model() {
        // Nothing chat-capable ⇒ empty pool ⇒ the UI keeps Ask disabled (no request
        // with an embedding model is ever sent).
        let served = ids(&["bge-small-en-v1.5-gguf"]);
        assert!(chat_capable_model_ids(&config::Config::default(), &served, None).is_empty());
    }
}

/// The tier this server process resolves models with — the grant taken at
/// startup, re-expressed for the shared resolver.
///
/// A function rather than an inline expression so both `serve` resolutions (the
/// startup validation and the Ask pool) read the same value, and so the
/// non-`remote` build has one place saying "there is no tier here" instead of
/// two.
#[cfg(all(feature = "serve", feature = "remote"))]
fn serve_remote_tier(opts: &ServeOptions) -> rto_graph::RemoteTier {
    opts.remote
        .as_ref()
        .map_or(rto_graph::RemoteTier::Unavailable, |g| {
            rto_graph::RemoteTier::Granted {
                trust: g.endpoint.trust(),
            }
        })
}

/// Without the `remote` feature there is no tier to resolve with, ever.
#[cfg(all(feature = "serve", not(feature = "remote")))]
fn serve_remote_tier(_opts: &ServeOptions) -> rto_graph::RemoteTier {
    rto_graph::RemoteTier::Unavailable
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

    // Refuse a `[models] generative` this endpoint cannot honour **before** the
    // listener opens (Stage 33). `serve` is long-running, so a configuration
    // error has to fail at startup or it becomes a per-request surprise — and the
    // Ask panel sends `models[0]`, so a bad pin there is the one that used to be
    // silently replaced by whatever happened to be served first.
    //
    // Resolved with the tier so the failure ordering stays right in both
    // directions: a broken pin still fails here even on a server that will answer
    // remotely, because a broken configuration is broken whether or not this
    // process reads it (`resolve_with_remote`).
    rto_graph::resolve_model_with_remote(
        rto_graph::ModelTask::Chat,
        &cfg.models.resolve(),
        serve_remote_tier(opts),
    )?;

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
    // With the tier granted for this process, the hosted model joins the served
    // set as one more id — `rto-serve` is not modified, and the socket stays in
    // `remote_transport`. See `remote_engine` for the reduction that keeps the
    // payload allow-list load-bearing over a chat transcript, and for what a
    // process-scoped grant does and does not license.
    #[cfg(feature = "remote")]
    let engine: std::sync::Arc<dyn rto_serve::Engine> = match &opts.remote {
        Some(grant) => {
            eprintln!(
                "remote model tier: ON for this server process — every Ask it answers sends \
                 graph-derived\n  context to {} as {} ({}). Recorded in {}; the grant dies with \
                 this process.",
                grant.endpoint.url(),
                grant.endpoint.model(),
                grant.endpoint.trust().as_str(),
                remote_ledger()?.path().display(),
            );
            std::sync::Arc::new(remote_engine::RemoteBackedEngine::new(
                engine,
                grant.endpoint.clone(),
                grant.decision,
                remote_ledger()?,
                flat.clone(),
            ))
        }
        None => engine,
    };

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
        // The hosted model's id when this process granted the tier, so it takes
        // the default slot the Ask UI sends. Read from `opts` rather than
        // inferred from the served set: which id is the remote one is a fact
        // about the grant, and guessing it from the engine would be a second,
        // weaker answer to the same question.
        #[cfg(feature = "remote")]
        let remote_id = opts.remote.as_ref().map(|g| g.endpoint.model());
        #[cfg(not(feature = "remote"))]
        let remote_id: Option<&str> = None;
        chat_capable_model_ids(cfg, &served_ids, remote_id)
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

/// The served-chat `debt_density` tool definition. Lifted out of
/// [`GraphToolRegistry::tools`] to keep that function readable, not because it
/// is shared: the MCP server declares its own (see `rto_render::mcp`).
///
/// `with_project` adds the workspace `project` selector every tool carries.
#[cfg(feature = "serve")]
fn debt_density_tool_def(
    with_project: &impl Fn(serde_json::Value) -> serde_json::Value,
) -> rto_serve::ToolDef {
    use serde_json::json;
    rto_serve::ToolDef {
        name: "debt_density".to_owned(),
        description: "Rank FILES by intent-debt DENSITY — markers per 1,000 lines — \
                      rather than by raw marker count, which ranks the biggest file \
                      first by construction. Each row carries `markers`, `lines`, \
                      `per_kloc` and a per-category split; `overall_per_kloc` is the \
                      repository baseline to read a file's figure against. Use `debt` \
                      instead when the question is which markers exist, not where \
                      they are concentrated. \
                      Two limits to pass on rather than reporting a number as a \
                      finding: the denominator is FILE LENGTH — every line, blanks \
                      and comments included — not source lines of code, so figures \
                      run lower than an SLOC tool's and flatter verbose or generated \
                      files; and the markers beneath it include prose matches (`for \
                      now`, `deferred`, `tbd`), so a design document can rank as \
                      dense debt. This is a measurement, not a gate."
            .to_owned(),
        parameters: json!({
            "type": "object",
            "properties": with_project(json!({
                "categories": {
                    "type": "array",
                    "items": { "type": "string" },
                },
                "order": {
                    "type": "string",
                    "enum": rto_graph::DensityOrder::tokens(),
                },
                "limit": { "type": "integer", "minimum": 1, "maximum": 100 },
                "min_lines": { "type": "integer", "minimum": 0 },
            })),
        }),
    }
}

/// The served-chat `config_secrets` tool definition. Lifted out of
/// [`GraphToolRegistry::tools`] to keep that function readable, not because it is
/// shared: the MCP server declares its own (see `rto_render::mcp`).
///
/// The description carries the limitations in full, and deliberately at length.
/// The rename from the shortlist's original secret-*scanner* title is
/// load-bearing, and a served model sees only this string — so every limit it must
/// pass on to the user is stated here, not left in the Rust doc comment.
///
/// `with_project` adds the workspace `project` selector every tool carries.
#[cfg(feature = "serve")]
fn config_secrets_tool_def(
    with_project: &impl Fn(serde_json::Value) -> serde_json::Value,
) -> rto_serve::ToolDef {
    use serde_json::json;
    rto_serve::ToolDef {
        name: "config_secrets".to_owned(),
        description: "Inventory the SECRET-NAMED config keys in the graph: their file \
                      paths, their key names, and whether each value was redacted \
                      before being stored (`state` = redacted | declared | present). \
                      Answers \"which of this repo's config surfaces deal in \
                      credentials\" and \"did anything unredacted get into this \
                      graph\". \
                      THIS IS NOT A SECRET SCANNER — state the limits when you report \
                      it, and never imply a security guarantee. It CANNOT find a \
                      hardcoded credential in source code: it reads config-key nodes, \
                      so a token in a Rust or Python string literal produces nothing \
                      here and is invisible. It CANNOT judge whether a value is valid, \
                      because it never sees one — values are redacted before they \
                      reach the store. It CANNOT tell a real secret from a \
                      placeholder: `API_TOKEN=changeme` in a committed `.env.example` \
                      and a live token are the same row. And an EMPTY RESULT DOES NOT \
                      MEAN THERE ARE NO SECRETS — it means no config key is \
                      secret-NAMED; a credential under an innocuous key like `dsn` or \
                      `endpoint` never appears. If asked to scan for secrets, say \
                      plainly that this tool cannot do it."
            .to_owned(),
        parameters: json!({
            "type": "object",
            "properties": with_project(json!({
                "limit": { "type": "integer", "minimum": 1, "maximum": 200 },
            })),
        }),
    }
}

/// The `(order, limit, min_lines)` triple for a served-chat `debt_density` call,
/// lifted out of [`GraphToolRegistry::call`] to keep that dispatcher readable.
///
/// An unrecognised `order` is an `Err` rather than a silent fall back to
/// `density`: a model told it ranked by `markers` when it did not will state that
/// as fact to the user. `limit` is model-controlled, so it is clamped to the
/// advertised bound — a huge value cannot make the server do pointless work.
#[cfg(feature = "serve")]
fn density_args(args: &serde_json::Value) -> Result<(rto_graph::DensityOrder, usize, u32), String> {
    let order = match args.get("order").and_then(serde_json::Value::as_str) {
        None => rto_graph::DensityOrder::default(),
        Some(token) => rto_graph::DensityOrder::from_token(token).ok_or_else(|| {
            format!(
                "unknown order `{token}` (expected {})",
                rto_graph::DensityOrder::tokens().join("|")
            )
        })?,
    };
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(20)
        .clamp(1, 100);
    let min_lines = args
        .get("min_lines")
        .and_then(serde_json::Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(rto_graph::DEFAULT_MIN_LINES);
    Ok((order, limit, min_lines))
}

/// The served-chat `coupling` tool definition. Lifted out of
/// [`GraphToolRegistry::tools`] to keep that function readable, not because it
/// is shared: the MCP server declares its own (see `rto_render::mcp`).
///
/// `with_project` adds the workspace `project` selector every tool carries.
#[cfg(feature = "serve")]
fn coupling_tool_def(
    with_project: &impl Fn(serde_json::Value) -> serde_json::Value,
) -> rto_serve::ToolDef {
    use serde_json::json;
    rto_serve::ToolDef {
        name: "coupling".to_owned(),
        description: "Rank symbols by DIRECTED call coupling over `calls` edges: \
                      `fan_in` (how many distinct symbols call this one), `fan_out` \
                      (how many it calls), `instability` = fan_out/(fan_in+fan_out). \
                      `order`=fan_in finds what the codebase most depends on, \
                      `order`=fan_out the symbols that reach furthest. Call edges \
                      are resolved by simple name, so a short generically-named \
                      function can absorb every call to that name — say so if you \
                      report a high `fan_in` on one."
            .to_owned(),
        parameters: json!({
            "type": "object",
            "properties": with_project(json!({
                "order": {
                    "type": "string",
                    "enum": rto_graph::CouplingOrder::tokens(),
                },
                "limit": { "type": "integer", "minimum": 1, "maximum": 100 },
            })),
        }),
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
            debt_density_tool_def(&with_project),
            config_secrets_tool_def(&with_project),
            coupling_tool_def(&with_project),
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
                // Same rule as the CLI and the graph API: the *target* project's
                // own `[debt] ignore` governs its scan. A model that is told a
                // different number than `roteiro debt` prints has no way to tell
                // which one is the repository's actual debt.
                let ignore =
                    config::debt_ignore_for(&self.workspace, project).map_err(|e| e.to_string())?;
                self.run(project, |store| {
                    rto_graph::debt(store, &categories, &ignore)
                })
            }
            "debt_density" => {
                let categories: Vec<String> = args
                    .get("categories")
                    .and_then(serde_json::Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                let (order, limit, min_lines) = density_args(args)?;
                // The same `[debt] ignore` rule as `debt` above, for the same
                // reason: a density computed over a different marker set than
                // `roteiro debt-density` prints is a number nobody can reconcile.
                let ignore =
                    config::debt_ignore_for(&self.workspace, project).map_err(|e| e.to_string())?;
                self.run(project, |store| {
                    rto_graph::debt_density(store, &categories, &ignore, order, limit, min_lines)
                })
            }
            "config_secrets" => {
                // `limit` is model-controlled: clamp to the advertised bound so a
                // huge value cannot make the server do pointless work.
                let limit = args
                    .get("limit")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|n| usize::try_from(n).ok())
                    .unwrap_or(50)
                    .clamp(1, 200);
                self.run(project, |store| rto_graph::config_secrets(store, limit))
            }
            "coupling" => {
                // An unrecognised `order` is an error rather than a silent fall
                // back to `total`: a model told it ranked by `fan_in` when it did
                // not will state that as fact to the user.
                let order = match str_arg("order") {
                    None => rto_graph::CouplingOrder::default(),
                    Some(token) => {
                        rto_graph::CouplingOrder::from_token(token).ok_or_else(|| {
                            format!(
                                "unknown order `{token}` (expected {})",
                                rto_graph::CouplingOrder::tokens().join("|")
                            )
                        })?
                    }
                };
                // `limit` is model-controlled: clamp to the advertised bound so a
                // huge value cannot make the server do pointless work.
                let limit = args
                    .get("limit")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|n| usize::try_from(n).ok())
                    .unwrap_or(20)
                    .clamp(1, 100);
                self.run(project, |store| rto_graph::coupling(store, order, limit))
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
    debt_ignore: &[String],
) -> anyhow::Result<()> {
    match rto_render::Target::parse(target) {
        Some(rto_render::Target::DocsSite) => render_docs(out),
        Some(rto_render::Target::ObsidianVault) => render_obsidian(ingest, out, debt_ignore),
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
fn render_obsidian(
    ingest: rto_graph::IngestConfig,
    out: Option<String>,
    debt_ignore: &[String],
) -> anyhow::Result<()> {
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
    let home = rto_render::render_home(&vault_summary(
        &repo,
        &store,
        repo_url,
        commit,
        debt_ignore,
    )?);
    std::fs::write(out.join(&home.filename), &home.content)?;

    println!(
        "rendered obsidian vault → {} ({count} note(s) + {})",
        out.display(),
        rto_render::HOME_NOTE
    );
    Ok(())
}

/// Aggregate the store into the figures the vault's `_Home` overview shows.
///
/// `debt_ignore` is the repository's `[debt] ignore` list, threaded from `main`
/// so `_Home` scopes intent debt exactly as `roteiro debt`, `roteiro
/// debt-density`, `check` and the graph API do (ADR-0007 v1.1).
fn vault_summary(
    repo: &rto_graph::Repo,
    store: &rto_graph::Store,
    repo_url: Option<String>,
    commit: Option<String>,
    debt_ignore: &[String],
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

    // Scoped by the repository's own `[debt] ignore`: a vault that counted the
    // vendored tree the CLI excludes would report a different debt for the same
    // repository, which is the disagreement ADR-0007 v1.1 exists to prevent.
    let debt = rto_graph::debt(store, &[], debt_ignore)?
        .by_category
        .into_iter()
        .collect();

    // Where that debt is concentrated, which the category counts above cannot
    // show. Capped for the same reason as the coupling table below; the full
    // ranking is `roteiro debt-density` / `GET .../debt/density`. The same
    // `debt_ignore` as the category counts above — the two tables sit on one
    // page and must be two lenses on one marker set, not two marker sets.
    let densest_files = rto_graph::debt_density(
        store,
        &[],
        debt_ignore,
        rto_graph::DensityOrder::Density,
        HOME_COUPLING_ROWS,
        rto_graph::DEFAULT_MIN_LINES,
    )?
    .items
    .into_iter()
    .map(|i| rto_render::DensityEntry {
        path: i.path,
        markers: i.markers,
        lines: i.lines,
        per_kloc: i.per_kloc,
    })
    .collect();

    let config_secrets = vault_config_secrets(store)?;

    // Directed call coupling, ranked by fan-in — "what does this codebase most
    // depend on". Capped, because `_Home` is an overview, not the report; the
    // full ranking is `roteiro coupling` / `GET .../coupling`.
    let most_called =
        rto_graph::coupling(store, rto_graph::CouplingOrder::FanIn, HOME_COUPLING_ROWS)?
            .items
            .into_iter()
            // A node with no callers is not "depended on"; with a short graph the
            // fan-in ranking's tail is all zeroes and would pad the table with noise.
            .filter(|i| i.fan_in > 0)
            .map(|i| rto_render::CouplingEntry {
                key: i.key,
                name: i.name,
                fan_in: i.fan_in,
                fan_out: i.fan_out,
            })
            .collect();

    Ok(rto_render::VaultSummary {
        project,
        total_nodes: usize::try_from(store.node_count()?)?,
        total_edges: usize::try_from(store.edge_count()?)?,
        node_counts,
        edge_provenance,
        adrs,
        debt,
        densest_files,
        config_secrets,
        most_called,
        repo_url,
        commit,
    })
}

/// The `_Home` note's secret-named config-key figures, or `None` when the graph
/// holds none — the overview then omits the section rather than rendering a row of
/// zeroes, which would read as "scanned, and clean". See
/// [`rto_graph::config_secrets`] for why this lens cannot support that reading.
///
/// Key **names** are deliberately left out of the vault: a note is browsed
/// casually and out of context, and `roteiro config-secrets` is where the names
/// belong, alongside the full caveat. The file list is capped for the same reason
/// the other overview tables are.
fn vault_config_secrets(
    store: &rto_graph::Store,
) -> anyhow::Result<Option<rto_render::ConfigSecretSummary>> {
    let secrets = rto_graph::config_secrets(store, 0)?;
    if secrets.secret_named == 0 {
        return Ok(None);
    }
    // `items` is ordered by `(path, …)`, so equal paths are adjacent and `dedup`
    // is enough to make this the distinct-file list.
    let mut files: Vec<String> = secrets
        .items
        .iter()
        .filter_map(|i| i.path.clone())
        .collect();
    files.dedup();
    files.truncate(HOME_COUPLING_ROWS);
    Ok(Some(rto_render::ConfigSecretSummary {
        secret_named: secrets.secret_named,
        redacted: secrets.redacted,
        declared: secrets.declared,
        unredacted: secrets.unredacted,
        files,
    }))
}

/// Rows in the vault `_Home` note's directed-coupling and debt-density tables.
/// An overview figure: enough to see the shape of the codebase, not the whole
/// ranking.
const HOME_COUPLING_ROWS: usize = 10;

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

// The `roteiro config-secrets` summary lines. Tested here rather than end-to-end
// because the state that matters most — a secret-named key carrying an unredacted
// value — is one extraction cannot produce (it always redacts), so no repository
// fixture can reach it. Only an import layer can, and the warning must still be
// right when it does.
#[cfg(test)]
mod config_secrets_summary_tests {
    use super::config_secrets_summary;

    fn report(secret_named: usize, unredacted: usize) -> rto_graph::ConfigSecretReport {
        rto_graph::ConfigSecretReport {
            schema: rto_graph::SCHEMA,
            limit: 0,
            config_keys: 100,
            secret_named,
            redacted: secret_named - unredacted,
            declared: 0,
            unredacted,
            redacted_not_secret_named: 0,
            files: 1,
            items: Vec::new(),
        }
    }

    /// The caveat every summary must carry, whatever it found.
    const CAVEAT: &str = "not a secret scan";

    #[test]
    fn the_caveat_is_unconditional() {
        // Including — especially — when nothing was found, which is where a reader
        // is most likely to conclude something this lens never claimed.
        let empty = config_secrets_summary(&report(0, 0)).join("\n");
        assert!(
            empty.contains("no secret-named config key") && empty.contains(CAVEAT),
            "{empty}"
        );
        assert!(
            config_secrets_summary(&report(3, 0))
                .join("\n")
                .contains(CAVEAT),
            "and when something was"
        );
    }

    #[test]
    fn an_unredacted_value_is_warned_about_and_attributed_to_the_import() {
        let clean = config_secrets_summary(&report(3, 0)).join("\n");
        assert!(!clean.contains("WARNING"), "nothing to warn about: {clean}");

        let leaky = config_secrets_summary(&report(3, 1)).join("\n");
        assert!(leaky.contains("WARNING"), "{leaky}");
        assert!(
            leaky.contains("came from an import layer"),
            "and it points at the importing tool, not the source repository: {leaky}"
        );
        assert!(leaky.contains(CAVEAT), "the caveat still travels: {leaky}");
    }

    #[test]
    fn a_redaction_under_a_non_secret_name_is_explained_when_present() {
        // Without this line a reader comparing `redacted` against the number of
        // redacted values in the graph finds an unexplained surplus.
        let mut r = report(2, 0);
        assert!(
            !config_secrets_summary(&r)
                .join("\n")
                .contains("non-secret key name"),
            "silent when there are none"
        );
        r.redacted_not_secret_named = 4;
        assert!(
            config_secrets_summary(&r)
                .join("\n")
                .contains("plus 4 redacted value(s) under non-secret key name(s)"),
            "{:?}",
            config_secrets_summary(&r)
        );
    }
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

    // --- `memory recall` and `memory cache` (Stage 25) ------------------------

    /// **`recall` defaults to the reproducible answer.** No query, no filters, and
    /// `decay none` — so the same store and the same tree recall the same records
    /// in the same order, and pricing age at all is something a caller asks for.
    #[test]
    fn recall_defaults_to_no_query_and_no_decay() {
        let MemoryAction::Recall {
            query,
            decay,
            scope,
            kind,
            anchor,
            applicable_only,
            limit,
            json,
        } = action(["roteiro", "memory", "recall"])
        else {
            panic!("expected Recall");
        };
        assert_eq!(query, None, "a bare recall ranks everything live");
        assert_eq!(decay, rto_graph::Decay::None);
        assert!(decay.is_reproducible(), "and says so");
        assert_eq!((scope, kind, anchor), (None, None, None));
        assert!(
            !applicable_only,
            "a drifted record is demoted and labelled, not withheld by default",
        );
        assert_eq!(limit, 10);
        assert!(!json);
    }

    #[test]
    fn recall_accepts_every_flag_and_both_decay_shapes() {
        let MemoryAction::Recall {
            query,
            decay,
            scope,
            kind,
            anchor,
            applicable_only,
            limit,
            json,
        } = action([
            "roteiro",
            "memory",
            "recall",
            "retry loop",
            "--decay",
            "exponential:25",
            "--scope",
            "repo",
            "--kind",
            "attempt",
            "--anchor",
            "sym:rust:src/a.rs#f",
            "--applicable-only",
            "--limit",
            "3",
            "--json",
        ])
        else {
            panic!("expected Recall");
        };
        assert_eq!(query.as_deref(), Some("retry loop"));
        assert_eq!(decay, rto_graph::Decay::Exponential { half_life: 25 });
        assert_eq!(scope.as_deref(), Some("repo"));
        assert_eq!(kind, Some(rto_graph::MemoryKind::Attempt));
        assert_eq!(anchor.as_deref(), Some("sym:rust:src/a.rs#f"));
        assert!(applicable_only);
        assert_eq!(limit, 3);
        assert!(json);

        // A bare mode takes its documented default parameter.
        let MemoryAction::Recall { decay, .. } =
            action(["roteiro", "memory", "recall", "--decay", "linear"])
        else {
            panic!("expected Recall");
        };
        assert_eq!(
            decay,
            rto_graph::Decay::Linear {
                span: rto_graph::DEFAULT_DECAY_SPAN
            }
        );

        // And an unknown mode is refused rather than silently treated as `none`,
        // which would promise reproducibility nobody asked for.
        assert!(
            Cli::try_parse_from(["roteiro", "memory", "recall", "--decay", "clock"]).is_err(),
            "an unrecognised decay mode is not consent to any other one",
        );
    }

    /// `memory cache` **reports** by default: sweeping is something you ask for,
    /// even though everything it can evict is re-derivable.
    #[test]
    fn cache_reports_unless_asked_to_sweep() {
        let MemoryAction::Cache {
            sweep,
            budget_mb,
            json,
        } = action(["roteiro", "memory", "cache"])
        else {
            panic!("expected Cache");
        };
        assert!(!sweep);
        assert_eq!(budget_mb, None, "the default budget is the configured one");
        assert!(!json);

        let MemoryAction::Cache {
            sweep, budget_mb, ..
        } = action([
            "roteiro",
            "memory",
            "cache",
            "--sweep",
            "--budget-mb",
            "512",
        ])
        else {
            panic!("expected Cache");
        };
        assert!(sweep);
        assert_eq!(budget_mb, Some(512));
    }

    /// The budget resolves to megabytes of bytes, an explicit flag wins over the
    /// environment and the default, and an unrepresentable one is refused rather
    /// than wrapped into a small number.
    #[test]
    fn the_cache_budget_resolves_from_the_flag_then_the_default() {
        assert_eq!(
            super::resolve_cache_budget(Some(512)).expect("budget"),
            512 * 1024 * 1024,
        );
        assert_eq!(
            super::resolve_cache_budget(None).expect("budget"),
            rto_graph::DEFAULT_CACHE_BUDGET_BYTES,
            "and with no flag and no environment override, the documented 256 MB",
        );
        assert!(
            super::resolve_cache_budget(Some(u64::MAX)).is_err(),
            "a budget that cannot be expressed in bytes is refused, not wrapped",
        );
    }

    /// **Memory is opt-in on `search`, exactly as generated content is**, and the
    /// two opt-ins are independent: neither implies the other.
    #[test]
    fn search_keeps_memory_behind_its_own_opt_in() {
        let Command::Search {
            include_generated,
            include_memory,
            ..
        } = parse(["roteiro", "search", "retry loop"])
        else {
            panic!("expected Search");
        };
        assert!(!include_generated, "and generated content stays opt-in too");
        assert!(!include_memory);

        let Command::Search {
            include_generated,
            include_memory,
            ..
        } = parse(["roteiro", "search", "retry loop", "--include-memory"])
        else {
            panic!("expected Search");
        };
        assert!(
            !include_generated,
            "asking for memory must not also turn on another store's channel",
        );
        assert!(include_memory);
    }
}

// The one path that can put network bytes into the pinned asset cache.
//
// `AssetSource::Download` has no compile-time digest: whatever arrives here is
// digested and recorded as the asset's own pin, so a truncated database would be
// certified by `security status` as present and matching. These tests serve raw
// HTTP over a loopback `TcpListener` — no external network, no fixtures — and
// assert both that a bad transfer fails and that it leaves nothing a later
// `prefetch` would treat as installed.
#[cfg(all(test, feature = "execution"))]
mod asset_download {
    use super::{declared_body_length, download_asset_file, verify_transferred};
    use std::io::{Read as _, Write as _};

    /// Serve `response` verbatim to exactly one client, then close.
    ///
    /// Raw bytes rather than a framework, because what is under test *is* the
    /// framing: a declared length the body does not honour, or a body with no
    /// declared length at all.
    fn serve_once(response: &'static [u8]) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                // Read the request line so the client is not writing into a
                // closed socket, then answer and hang up.
                let mut buf = [0u8; 2048];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(response);
                let _ = stream.flush();
            }
        });
        format!("http://{addr}/all.zip")
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("roteiro-dl-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    /// The positive control. Without it the refusals below could all be passing
    /// because nothing ever succeeds.
    #[test]
    fn a_complete_download_succeeds_and_writes_every_byte() {
        // 10 bytes: the 4-byte zip signature plus `hello!`.
        let url = serve_once(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nPK\x03\x04hello!");
        let dir = scratch("complete");
        let path = dir.join("all.zip");
        download_asset_file(&url, &path).expect("a complete transfer must install");
        assert_eq!(std::fs::read(&path).expect("read"), b"PK\x03\x04hello!");
    }

    /// The reviewed defect: a server that declares a length and then closes
    /// cleanly part-way through. This must fail rather than install a short
    /// database that `status` would certify.
    ///
    /// **This test pins ureq, not this crate.** Measured against ureq 3.4.0, the
    /// transport enforces `Content-Length` framing itself and raises
    /// `UnexpectedEof` here, so reverting the guard in `download_asset_file`
    /// does *not* turn this red — only a transport that stopped enforcing it
    /// would. It is kept deliberately, as the regression pin on the dependency
    /// behaviour the rest of this design now leans on.
    #[test]
    fn a_truncated_body_fails_and_leaves_nothing_installed() {
        let url = serve_once(b"HTTP/1.1 200 OK\r\nContent-Length: 4096\r\n\r\nPK\x03\x04truncated");
        let dir = scratch("truncated");
        let path = dir.join("all.zip");

        let err = download_asset_file(&url, &path).expect_err("a short body must not succeed");
        // The refusal comes from the transport — ureq enforces `Content-Length`
        // framing and reports the peer hanging up — so the assertion is that the
        // failure is attributable, not that it carries this function's own
        // mismatch wording.
        assert!(err.contains(&url), "the failure must name the URL: {err}");

        // Whatever bytes arrived must not survive as a usable asset. The
        // fetcher writes to a staging path that `download_all` removes on
        // failure; what must never exist is a complete-looking file.
        let installed = std::fs::read(&path).unwrap_or_default();
        assert_ne!(
            installed.len(),
            4096,
            "a partial body must never look like the declared asset"
        );
    }

    /// The framing that no byte count can rescue: neither `Content-Length` nor
    /// chunked, so the body is defined as ending when the connection closes and
    /// a mid-transfer drop is indistinguishable from a complete file.
    ///
    /// Measured against ureq 3.4.0, this is the one framing where a truncated
    /// transfer reads as `Ok`. An unestablishable length is refused rather than
    /// pinned.
    #[test]
    fn a_body_of_unknown_length_is_refused_rather_than_pinned() {
        let url = serve_once(b"HTTP/1.1 200 OK\r\n\r\nPK\x03\x04could-be-half-a-database");
        let dir = scratch("unframed");
        let path = dir.join("all.zip");

        let err =
            download_asset_file(&url, &path).expect_err("an unverifiable length must be refused");
        assert!(err.contains("Content-Length"), "{err}");
        assert!(
            err.contains("truncated") || err.contains("completeness"),
            "the refusal must say what it could not establish: {err}"
        );
        assert!(
            !path.exists(),
            "nothing may be written when the response cannot be verified"
        );
    }

    /// End to end, through the code `prefetch` actually runs: a bad server must
    /// leave the asset unprovisioned, with no record and no stray bytes for a
    /// later `provision`/`status` to fold into a pin.
    #[test]
    fn a_failed_fetch_leaves_the_asset_unprovisioned_and_the_cache_clean() {
        let url = serve_once(b"HTTP/1.1 200 OK\r\n\r\nhalf-a-database");
        let root = scratch("provision");
        // `DownloadFile` holds `&'static str`; a loopback URL has a fresh port
        // each run, so it is leaked for the life of the test process.
        let leaked: &'static str = Box::leak(url.into_boxed_str());
        let files: &'static [rto_exec::DownloadFile] =
            Box::leak(Box::new([rto_exec::DownloadFile {
                path: "osv-scalibr/crates.io/all.zip",
                url: leaked,
            }]));
        let spec = rto_exec::AssetSpec {
            id: "osv-db",
            analyzer: "osv-scanner",
            kind: rto_exec::AssetKind::AdvisoryDb,
            source: rto_exec::AssetSource::Download { files },
            file: "",
            licence: "test",
        };

        let fetcher: &rto_exec::Fetcher<'_> = &download_asset_file;
        let err = rto_exec::provision_with(&root, &spec, Some(fetcher))
            .expect_err("an unverifiable download must not provision");
        assert!(
            err.to_string().contains("Content-Length"),
            "the provisioning failure must carry the fetcher's reason: {err}"
        );

        // Nothing recorded, and nothing left in the tree that a later successful
        // provision would digest into the pin.
        let status = rto_exec::status(&root, Some("osv-scanner"));
        assert_eq!(status.len(), 1);
        assert!(
            status[0].installed.is_none(),
            "a failed fetch must not read as provisioned"
        );
        assert_eq!(status[0].verified, None);

        let strays: Vec<std::path::PathBuf> = walk(&root);
        assert!(
            strays.is_empty(),
            "a failed fetch must leave no bytes behind: {strays:?}"
        );
    }

    /// Every file under `dir`, recursively.
    fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(walk(&path));
            } else {
                out.push(path);
            }
        }
        out
    }

    /// The request asks for the one framing whose completeness can be checked.
    ///
    /// Served by a listener that content-negotiates: it answers a request
    /// carrying `Accept-Encoding: identity` with a length-framed body, and
    /// anything else with a close-delimited one. Succeeding therefore proves the
    /// header was sent — without it, this download would be refused as
    /// unverifiable, which is the fail-closed direction but a needless one
    /// against a server that would have obliged.
    #[test]
    fn the_request_asks_for_a_framing_it_can_verify() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 2048];
                let read = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..read]).to_ascii_lowercase();
                let response: &[u8] = if request.contains("accept-encoding: identity") {
                    b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nPK\x03\x04ok"
                } else {
                    b"HTTP/1.1 200 OK\r\n\r\nPK\x03\x04ok"
                };
                let _ = stream.write_all(response);
                let _ = stream.flush();
            }
        });
        let url = format!("http://{addr}/all.zip");
        let dir = scratch("negotiated");
        let path = dir.join("all.zip");

        download_asset_file(&url, &path)
            .expect("a server offering a verifiable framing must be taken up on it");
        assert_eq!(std::fs::read(&path).expect("read"), b"PK\x03\x04ok");
    }

    /// A length that cannot be parsed is the same answer as no length at all —
    /// stated once, so a blank or malformed header cannot read as zero bytes.
    #[test]
    fn only_a_real_number_counts_as_a_declared_length() {
        assert_eq!(declared_body_length(Some("3374965")), Some(3_374_965));
        assert_eq!(declared_body_length(Some("  42 ")), Some(42));
        assert_eq!(declared_body_length(Some("0")), Some(0));
        assert_eq!(declared_body_length(None), None);
        assert_eq!(declared_body_length(Some("")), None);
        assert_eq!(declared_body_length(Some("lots")), None);
        assert_eq!(declared_body_length(Some("-1")), None);
        assert_eq!(declared_body_length(Some("1.5")), None);
    }

    /// The mismatch is reported with both counts, so the failure says how short
    /// the transfer was rather than only that something went wrong.
    ///
    /// Exercised directly: ureq 3.4.0 enforces `Content-Length` framing itself
    /// and errors before this branch is reached, so it is defence against a
    /// dependency changing that — which is precisely why it is tested here
    /// rather than assumed to be unreachable.
    #[test]
    fn a_short_transfer_is_reported_with_both_counts() {
        verify_transferred("https://example.invalid/all.zip", 4096, 4096)
            .expect("an exact transfer is fine");

        let err = verify_transferred("https://example.invalid/all.zip", 4096, 1200)
            .expect_err("a short transfer must fail");
        assert!(err.contains("4096"), "{err}");
        assert!(err.contains("1200"), "{err}");
        assert!(err.contains("https://example.invalid/all.zip"), "{err}");
        assert!(
            err.contains("pinned"),
            "the message must say why it matters: {err}"
        );

        // A body *longer* than declared is equally wrong: it is not the resource
        // the server described.
        assert!(verify_transferred("https://example.invalid/all.zip", 10, 11).is_err());
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
        security_cross_reference,
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
            cross_reference: Vec::new(),
        };
        let value = serde_json::to_value(&listing).expect("serialize");
        assert_eq!(value["findings"], 0);
        assert!(value["layers"].as_array().expect("array").is_empty());
        // Nothing to cross-reference is an absent section, not an empty one:
        // a consumer must not have to tell "no duplicates" from "not computed".
        assert!(value.get("cross_reference").is_none());
    }

    /// The cross-reference is suppressed below two dependency analyzers, and
    /// present at two.
    ///
    /// With one analyzer every row would read "confirmed by 1", which is noise
    /// dressed as information: a single source carries no signal about agreement
    /// in either direction. With two it is exactly the evidence ADR-0018 v1.1
    /// decided to keep.
    #[test]
    fn the_cross_reference_appears_only_once_there_is_something_to_compare() {
        fn layer(analyzer: &str, rule: &str, package: &str) -> rto_graph::FindingsLayer {
            rto_graph::FindingsLayer {
                run: rto_graph::AnalysisRun {
                    layer: format!("security:{analyzer}:ab12cd34"),
                    analyzer: analyzer.to_owned(),
                    analyzer_version: "1.0.0".to_owned(),
                    runner: rto_graph::RunnerKind::Ingested,
                    isolation: rto_graph::Isolation::Ingested,
                    image_digest: None,
                    rules_digest: None,
                    advisory_db: None,
                    command_policy: rto_graph::CommandPolicy {
                        network: rto_graph::NetworkPolicy::Deny,
                        worktree: rto_graph::WorktreeAccess::ReadOnly,
                        environment: rto_graph::EnvironmentPolicy::Scrubbed,
                    },
                    source: rto_graph::SourceIdentity::default(),
                    started_at: "2026-08-16T09:00:00Z".to_owned(),
                    ended_at: "2026-08-16T09:00:01Z".to_owned(),
                    exit_status: 1,
                    report_digest: "0".repeat(64),
                },
                findings: vec![rto_graph::Finding {
                    key: rto_graph::FindingKey::new(analyzer, &[rule.to_owned()]).expect("key"),
                    rule: rule.to_owned(),
                    severity: rto_graph::Severity::High,
                    title: format!("{package} is affected"),
                    message: String::new(),
                    path: None,
                    span: None,
                    meta: serde_json::json!({"package": package, "version": "1.0.0"}),
                }],
            }
        }

        let one = vec![layer("cargo-audit", "RUSTSEC-2026-0001", "widget")];
        assert!(
            security_cross_reference(&one).is_empty(),
            "one analyzer has nothing to be cross-referenced against"
        );

        let two = vec![
            layer("cargo-audit", "RUSTSEC-2026-0001", "widget"),
            layer("osv-scanner", "RUSTSEC-2026-0001", "widget"),
        ];
        let crossref = security_cross_reference(&two);
        assert_eq!(crossref.len(), 1, "one advisory, not two problems");
        assert_eq!(crossref[0].confirmed_by, 2);
        assert_eq!(crossref[0].reports.len(), 2, "both keys stay addressable");
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

    /// Provisioning is reachable from **any** `execution` build, including one
    /// with no execution backend compiled in at all.
    ///
    /// This is the compile-and-expose half of the provisioning/execution split,
    /// and it is exactly the shape a future refactor breaks silently: move the
    /// `assets` module back behind a backend feature and a
    /// `--no-default-features --features execution` build stops being able to
    /// `prefetch` — which is also the build that has to bootstrap
    /// `exec-boxlite`, whose build script demands the verified runtime archive
    /// at compile time. The gate below is a `cfg`, so it fails as a compile
    /// error in that build rather than as a wrong answer here.
    #[cfg(feature = "execution")]
    #[test]
    fn provisioning_is_reachable_without_any_execution_backend() {
        for argv in [
            ["roteiro", "security", "prefetch"],
            ["roteiro", "security", "status"],
        ] {
            assert!(
                Cli::try_parse_from(argv).is_ok(),
                "`{}` must parse in every `execution` build",
                argv.join(" ")
            );
        }
        // And the asset machinery they call is linked in, not merely parseable.
        assert!(!rto_exec::asset_root().as_os_str().is_empty());
        assert!(!rto_exec::ASSETS.is_empty());
    }

    /// `run` is the one thing an `execution`-only build must *not* be able to do
    /// — and it must say so by name rather than as `unrecognized subcommand`.
    #[cfg(not(feature = "exec-subprocess"))]
    #[test]
    fn run_is_absent_but_names_the_feature() {
        let SecurityAction::Run { analyzer, .. } =
            action(["roteiro", "security", "run", "cargo-audit"])
        else {
            panic!("expected the explanatory Run stub to parse");
        };
        assert_eq!(analyzer, "cargo-audit");
        let message = crate::run_security(SecurityAction::Run {
            analyzer: "cargo-audit".to_owned(),
            allow_unsandboxed: true,
            json: false,
        })
        .expect_err("an execution-only build must refuse to run an analyzer")
        .to_string();
        assert!(message.contains("exec-subprocess"), "{message}");
        assert!(message.contains("security ingest"), "{message}");
    }

    /// `--analyzer sandbox` provisions the runtime archive and nothing else.
    ///
    /// The bootstrap recipe printed by `rto-exec/build.rs` — and repeated in
    /// `README.md`, `AGENTS.md` and `docs/OFFLINE_SETUP.md` — rests entirely on
    /// this. If the selection ever stopped resolving the shared runtime, that
    /// recipe would provision nothing and every one of those documents would be
    /// wrong, with no test to say so: the fallback it depends on is reached only
    /// when `assets_for` comes back empty, which no other case exercises.
    ///
    /// Asserted here rather than trusted because the comment that described this
    /// selection was stale for long enough to ship a wrong recipe (#362).
    #[cfg(feature = "execution")]
    #[test]
    fn the_sandbox_analyzer_prefetches_the_runtime_archive_alone() {
        let selected: Vec<&str> = super::assets_to_prefetch(Some(rto_exec::SANDBOX))
            .expect("the shared runtime is always selectable")
            .iter()
            .map(|spec| spec.id)
            .collect();
        assert_eq!(
            selected,
            vec![rto_exec::RUNTIME_ASSET],
            "`prefetch --analyzer sandbox` must select the runtime archive and nothing else — \
             it is what someone bootstrapping `exec-boxlite` runs, and the advisory databases \
             are a quarter-gigabyte they do not need yet"
        );

        // The other half of the same rule: a bare `prefetch` still takes
        // everything, so the fallback narrowed nothing it should not have.
        assert_eq!(
            super::assets_to_prefetch(None).expect("all assets").len(),
            rto_exec::ASSETS.len()
        );

        let unknown = super::assets_to_prefetch(Some("no-such-analyzer"))
            .expect_err("an unknown analyzer must be named, not silently empty")
            .to_string();
        assert!(unknown.contains("no-such-analyzer"), "{unknown}");
        assert!(unknown.contains(rto_exec::SANDBOX), "{unknown}");
    }

    #[cfg(feature = "execution")]
    #[test]
    fn prefetch_and_status_default_to_every_analyzer() {
        let SecurityAction::Prefetch {
            analyzer,
            allow_download,
            json,
        } = action(["roteiro", "security", "prefetch"])
        else {
            panic!("expected Prefetch");
        };
        assert_eq!(analyzer, None);
        assert!(!json);
        // Downloading is asked for, never assumed: a bare `prefetch` must not
        // start a quarter-gigabyte transfer.
        assert!(!allow_download);

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

    /// A one-project registry whose graph is `main` → `helper`: identical
    /// undirected degree, opposite direction.
    fn called_registry() -> GraphToolRegistry {
        use rto_graph::{Edge, EdgeKind, FactSet, Node, NodeKind};
        let mut store = rto_graph::Store::open_in_memory().expect("store");
        store
            .apply_factset(
                &FactSet::new()
                    .with_node(Node::new("sym:rust:a.rs#main", NodeKind::Fn, "main"))
                    .with_node(Node::new("sym:rust:a.rs#helper", NodeKind::Fn, "helper"))
                    .with_edge(Edge::derived(
                        "sym:rust:a.rs#main",
                        "sym:rust:a.rs#helper",
                        EdgeKind::Calls,
                    )),
            )
            .expect("apply");
        GraphToolRegistry::new(std::sync::Arc::new(rto_graph::Workspace::single(
            "api", store,
        )))
    }

    /// The served-chat registry is a **separate** registry from the MCP server's,
    /// so a lens reaching MCP proves nothing about the chat surface. This is the
    /// chat side.
    #[test]
    fn coupling_is_advertised_and_answers_directionally() {
        use rto_serve::ToolRegistry as _;
        let reg = called_registry();

        let advertised = reg.tools();
        let def = advertised
            .iter()
            .find(|t| t.name == "coupling")
            .expect("`coupling` advertised to the served model");
        assert_eq!(
            def.parameters["properties"]["order"]["enum"],
            serde_json::json!(rto_graph::CouplingOrder::tokens()),
            "the schema's accepted orders come from the type, so they cannot drift"
        );

        let out = reg
            .call("coupling", &serde_json::json!({ "order": "fan_in" }))
            .expect("coupling");
        let json: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(json["items"][0]["key"], "sym:rust:a.rs#helper", "{json}");

        let out = reg
            .call("coupling", &serde_json::json!({ "order": "fan_out" }))
            .expect("coupling");
        let json: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(json["items"][0]["key"], "sym:rust:a.rs#main", "{json}");
    }

    #[test]
    fn coupling_refuses_an_unknown_order_rather_than_reordering_silently() {
        use rto_serve::ToolRegistry as _;
        let err = called_registry()
            .call("coupling", &serde_json::json!({ "order": "degree" }))
            .expect_err("an unknown order must be refused");
        assert!(err.contains("unknown order `degree`"), "was: {err}");
    }

    /// A one-project registry with two files carrying the **same** marker count
    /// and lengths twenty-fold apart: indistinguishable under `debt`, separated
    /// under `debt_density`.
    fn marked_registry() -> GraphToolRegistry {
        use rto_graph::{FactSet, Node, NodeKind};
        let file = |path: &str, lines: u64| {
            let mut n = Node::new(format!("file:{path}"), NodeKind::File, path);
            n.path = Some(path.to_owned());
            n.meta = serde_json::json!({ "bytes": lines * 30, "lines": lines });
            n
        };
        let marker = |path: &str, line: u32| {
            let mut n = Node::new(
                format!("marker:{path}#{line}"),
                NodeKind::Marker,
                "TODO x", // roteiro:ignore
            );
            n.path = Some(path.to_owned());
            n.meta = serde_json::json!({
                "category": "todo", // roteiro:ignore
                "text": "TODO x",   // roteiro:ignore
                "line": line,
            });
            n
        };
        let mut store = rto_graph::Store::open_in_memory().expect("store");
        let mut facts = FactSet::new()
            .with_node(file("big.rs", 4000))
            .with_node(file("small.rs", 200));
        for line in 1..=10 {
            facts = facts.with_node(marker("big.rs", line));
            facts = facts.with_node(marker("small.rs", line));
        }
        store.apply_factset(&facts).expect("apply");
        GraphToolRegistry::new(std::sync::Arc::new(rto_graph::Workspace::single(
            "api", store,
        )))
    }

    /// The served-chat registry is a **separate** registry from the MCP server's,
    /// so a lens reaching MCP proves nothing about the chat surface. This is the
    /// chat side.
    #[test]
    fn debt_density_is_advertised_and_normalises_by_file_length() {
        use rto_serve::ToolRegistry as _;
        let reg = marked_registry();

        let advertised = reg.tools();
        let def = advertised
            .iter()
            .find(|t| t.name == "debt_density")
            .expect("`debt_density` advertised to the served model");
        assert_eq!(
            def.parameters["properties"]["order"]["enum"],
            serde_json::json!(rto_graph::DensityOrder::tokens()),
            "the schema's accepted orders come from the type, so they cannot drift"
        );
        // The caveat travels with the tool, so a model reporting a figure can
        // pass on what the denominator actually is.
        assert!(
            def.description.contains("not source lines of code"),
            "was: {}",
            def.description
        );

        let out = reg
            .call("debt_density", &serde_json::json!({}))
            .expect("call");
        let json: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(json["items"][0]["path"], "small.rs", "{json}");
        assert_eq!(json["items"][0]["per_kloc"], 50.0);
        assert_eq!(json["items"][1]["path"], "big.rs");
        assert_eq!(json["items"][1]["per_kloc"], 2.5);
        assert_eq!(
            json["items"][0]["markers"], json["items"][1]["markers"],
            "the same raw count `debt` would report: {json}"
        );

        // `markers` is the control: on the raw count the two tie and break on path.
        let out = reg
            .call("debt_density", &serde_json::json!({ "order": "markers" }))
            .expect("call");
        let json: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(json["items"][0]["path"], "big.rs", "{json}");
    }

    #[test]
    fn debt_density_refuses_an_unknown_order_rather_than_reordering_silently() {
        use rto_serve::ToolRegistry as _;
        let err = marked_registry()
            .call("debt_density", &serde_json::json!({ "order": "count" }))
            .expect_err("an unknown order must be refused");
        assert!(err.contains("unknown order `count`"), "was: {err}");
    }

    /// A one-project registry with one secret-named config key (redacted), one
    /// struct-declared secret-named key (no value), and one that is not
    /// secret-named — the three cases `config_secrets` must keep apart.
    fn configured_registry() -> GraphToolRegistry {
        use rto_graph::{FactSet, Node, NodeKind};
        let cfg = |path: &str, dotted: &str, meta: serde_json::Value| {
            let mut n = Node::new(
                format!("cfgkey:{path}#{dotted}"),
                NodeKind::Other("config_key".to_owned()),
                dotted,
            );
            n.path = Some(path.to_owned());
            n.meta = meta;
            n
        };
        let mut store = rto_graph::Store::open_in_memory().expect("store");
        store
            .apply_factset(
                &FactSet::new()
                    .with_node(cfg(
                        ".env",
                        "API_TOKEN",
                        serde_json::json!({ "key": "API_TOKEN", "value": "<redacted>" }),
                    ))
                    .with_node(cfg(
                        "src/config.rs",
                        "serve.api_key",
                        serde_json::json!({ "key": "serve.api_key", "source": "struct" }),
                    ))
                    .with_node(cfg(
                        ".env",
                        "PORT",
                        serde_json::json!({ "key": "PORT", "value": "8017" }),
                    )),
            )
            .expect("apply");
        GraphToolRegistry::new(std::sync::Arc::new(rto_graph::Workspace::single(
            "api", store,
        )))
    }

    /// The served-chat registry is a **separate** registry from the MCP server's,
    /// so a lens reaching MCP proves nothing about the chat surface. This is the
    /// chat side.
    #[test]
    fn config_secrets_is_advertised_and_reports_state_never_a_value() {
        use rto_serve::ToolRegistry as _;
        let reg = configured_registry();

        assert!(
            reg.tools().iter().any(|t| t.name == "config_secrets"),
            "`config_secrets` advertised to the served model"
        );

        let out = reg
            .call("config_secrets", &serde_json::json!({}))
            .expect("config_secrets");
        let json: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(json["config_keys"], 3, "the population: {json}");
        assert_eq!(json["secret_named"], 2, "`PORT` is not: {json}");
        assert_eq!(json["redacted"], 1);
        assert_eq!(json["declared"], 1, "the struct-derived key: {json}");
        assert_eq!(json["unredacted"], 0);
        // Ordered by `(path, name, key)` — an inventory, not a ranking.
        assert_eq!(json["items"][0]["name"], "API_TOKEN");
        assert_eq!(json["items"][1]["state"], "declared");
        assert!(
            !out.contains("<redacted>") && json["items"][0].get("value").is_none(),
            "no value reaches the model, not even the placeholder: {out}"
        );
    }

    #[test]
    fn config_secrets_tool_description_refuses_the_scanner_reading() {
        // The rename is load-bearing and a served model sees only this string, so
        // each limitation must be stated where the model will read it. A separate
        // registry from MCP's means a separate assertion.
        use rto_serve::ToolRegistry as _;
        let tools = configured_registry().tools();
        let def = tools
            .iter()
            .find(|t| t.name == "config_secrets")
            .expect("advertised");
        for claim in [
            "NOT A SECRET SCANNER",
            "CANNOT find a hardcoded credential in source code",
            "never sees one",
            "real secret from a placeholder",
            "EMPTY RESULT DOES NOT MEAN THERE ARE NO SECRETS",
        ] {
            assert!(
                def.description.contains(claim),
                "missing `{claim}` from: {}",
                def.description
            );
        }
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
            super::chat_capable_model_ids(&super::config::Config::default(), &served_ids, None);
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

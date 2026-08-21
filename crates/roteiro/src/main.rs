//! Roteiro umbrella CLI. Wires the graph, spec, and render crates behind
//! subcommands; owns argument parsing, process I/O, and exit codes. See
//! ADR-0001 for the roadmap.
//!
//! @rto:0001

use clap::{Parser, Subcommand};
// Which tree the graph is built from. It lives in `rto_graph` rather than
// here because the authored-layer walk that must agree with it does too
// (`rto_spec::authored_layer`) — see `GraphSource` for why the two are one
// choice.
use rto_graph::GraphSource;

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
        /// Give the reviewer **graph context** — the ADRs governing the code under
        /// review, and the doc comments elsewhere in the file the diff does not
        /// show — assembled from the graph as it was at each reviewed commit.
        ///
        /// This is the one variable Stage 35b PR 2 varies. Without it the reviewer
        /// sees the file's diff and nothing else, which is the baseline the graph
        /// arm has to beat; the two runs are otherwise identical, down to the
        /// model. The arm is recorded in the run document so a comparison can be
        /// audited from the artifacts rather than from their filenames.
        #[arg(long)]
        graph_context: bool,
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
        /// Maximum number of hits to return, **per channel**; `0` is unlimited.
        ///
        /// Unlimited means every match, in every channel asked for — ranked as
        /// usual and simply not cut. It is bounded by the query rather than by
        /// the graph: every token must appear in a hit, so a second word
        /// narrows it sharply, and a query with no tokens still matches
        /// nothing at `0` exactly as at any other limit (issue #393).
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
        /// fall back to the hub's `HEAD`, and **the report says so** — per spoke,
        /// and in a summary line counting how many pinned anything. Falling back
        /// silently would make an inert `--pinned` byte-identical to plain
        /// `--infer`, which is the answer to a different question (#505).
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
    ///
    /// **The output directory is deleted and rebuilt on every render.** A vault is
    /// a build-output of the graph, regenerated over itself so that a note for a
    /// symbol you have since renamed does not linger for ever. Nothing you put
    /// inside it survives: keep your own notes *outside* the vault and link into
    /// it (issue #442).
    ///
    /// **Note names changed in issue #574 and there is no migration.** They used
    /// to collide — two keys could produce one file, and on macOS and Windows two
    /// names differing only in case are the same file, so this repository's vault
    /// was 104 notes short of the count it printed. Every name now carries a hash
    /// of its key, so every node gets its own note; the cost is that every note
    /// was renamed, and a hand-written link into a vault rendered by an earlier
    /// version now resolves to nothing. Re-point those links (Obsidian's
    /// autocomplete will find the new names) or re-create them from the new vault.
    Render {
        /// Target: docs | obsidian
        target: String,
        /// Output directory (default: `website/dist` for docs, `vault` for
        /// obsidian). **Emptied first** — see the command's help.
        #[arg(long)]
        out: Option<String>,
        /// `obsidian` only: render a **named** workspace from config
        /// (`[[workspaces]]`/`[standalone]`) as **one vault spanning its member
        /// repositories**, instead of the current project alone (issue #442).
        /// Member notes are keyed `<project>::<key>`, because node keys are
        /// repository-relative and every member's `README.md` would otherwise
        /// claim the same note; the filename is derived from that key — a
        /// readable lowercase hint, then a hash of the whole key — so no
        /// filename contains `::`. An unknown name fails fast, listing the
        /// known ones.
        ///
        /// **Omitted, the current project alone is rendered, with unqualified
        /// names.** Deliberately *not* "the workspace containing the current repo"
        /// (which is how `links -w` defaults): a user's own notes live outside the
        /// vault and link into it by name, so a bare `render obsidian` silently
        /// becoming a multi-repo render would rename every note and break every
        /// one of those links with no error. Workspace mode is opt-in, by name,
        /// always — a name may move because a release says so, never because of
        /// where the command was run.
        #[arg(long = "workspace-name", short = 'w', value_name = "NAME")]
        workspace_name: Option<String>,
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
    /// Run a linter over this worktree and print what it found. **Nothing is
    /// stored** — no findings layer, no history, nothing `roteiro export` or
    /// `roteiro security list` can see afterwards (ADR-0020 v1.1).
    ///
    /// That is the difference from `roteiro security run`, and it follows from
    /// what a lint *is*. An advisory id is **assigned**, and assignment is a
    /// promise: `RUSTSEC-2020-0071` will mean the same thing in five years. A
    /// lint name is a **symbol in a compiler** — renamed or removed at its
    /// discretion — so it is a tool's opinion about the code as it stands today,
    /// for the person who asked. Read a count from this command as a point in
    /// time and nothing more:
    ///
    /// - a **renamed** lint reads as one defect fixed and one introduced;
    ///
    /// - a **removed** lint reads as fixed;
    ///
    /// - an edit to `[workspace.lints]`, or an added `#[allow]`, makes whole
    ///   cohorts appear or vanish — a configuration change reading as a code
    ///   change.
    ///
    /// None of those touched the code, and a bumped toolchain or a different
    /// `--all-features` will move the number too. The report names the
    /// toolchain, the feature set and the isolation it had, because there is no
    /// stored run record carrying them.
    ///
    /// It is a **report, not a gate**: it exits 0 whether or not it found
    /// anything, because a lint count is not a verdict this command is in a
    /// position to pass. The gate is `cargo clippy -- -D warnings`, which CI
    /// already runs. When the build did not complete, the report says so — in a
    /// line of its own, and as `build_succeeded` in `--json` — so a partial
    /// result is never quietly a small one.
    ///
    /// **The linter runs sandboxed by default** (ADR-0020 §6). `cargo clippy`
    /// has `cargo check` semantics, so running it here would compile this tree
    /// on this machine, executing its build scripts and loading its proc macros
    /// with your filesystem and your credentials. In your own repository that is
    /// the build you were going to run anyway; in a branch you are reviewing it
    /// is somebody else's code, and that the toolchain is yours does not make
    /// the code yours.
    ///
    /// **You have to supply the image**, in `[lint] image` or `--image`, pinned
    /// by digest. Roteiro ships no default and will not choose one: no
    /// first-party Rust image carries the `clippy` component — rust-lang builds
    /// every stable and nightly variant `--profile minimal` — and picking a
    /// third party's would make somebody else's container the boundary your
    /// build scripts run in, chosen here and noticed by nobody. See
    /// `docs/SANDBOXED_LINTING.md`, which shows the Dockerfile.
    ///
    /// **The count can differ from a local `cargo clippy`, legitimately.** Which
    /// lints fire is decided by the image's rustc, which is not this machine's,
    /// and a lint name is a symbol in a compiler. Nothing is stored, so that is
    /// a surprise rather than a corruption — the report names the toolchain it
    /// used, beside the image digest it came from.
    ///
    /// The guest has no network interface, so it builds from a read-only mount
    /// of this machine's cargo cache. If a dependency is not already there, the
    /// run refuses and tells you to `cargo fetch` on the host first.
    ///
    /// To allow host execution instead, **either** is enough — you do not need
    /// both:
    ///
    /// - for one run: `--allow-unsandboxed`;
    ///
    /// - standing, for you: `[lint] allow_unsandboxed = true` in
    ///   `~/.roteiro/config.toml`.
    ///
    /// A project's `roteiro.toml` may **deny** host execution for everyone
    /// working in the repository, and may never grant it: that file is committed,
    /// so a merged line granting it would be consent given by someone else and
    /// noticed by nobody. `roteiro config` shows which layer decided.
    ///
    /// Nothing ever falls back. If the sandbox is asked for and unavailable the
    /// command says so and stops; it does not quietly run on the host instead.
    #[cfg(feature = "execution")]
    Lint {
        /// Which linter to run (`clippy`).
        analyzer: String,
        /// Run the linter in the sandbox — already the default, so this pins the
        /// intent against a change of defaults, and against a user-config grant
        /// you would rather not apply to this run. If the sandbox cannot be had
        /// it refuses by name rather than running here.
        #[arg(long, conflicts_with = "allow_unsandboxed")]
        sandboxed: bool,
        /// Accept that this run has no isolation boundary, and compile this tree
        /// on this host. Grants host execution for this run alone; the standing
        /// form is `[lint] allow_unsandboxed` in your own config.
        #[arg(long)]
        allow_unsandboxed: bool,
        /// The digest-pinned OCI image to lint inside, overriding `[lint]
        /// image` for this run. Roteiro ships no default and will not pick one:
        /// no first-party Rust image carries `clippy`, and choosing a third
        /// party's would make somebody else's container the boundary your build
        /// scripts run in. A tag is refused — see `docs/SANDBOXED_LINTING.md`.
        #[arg(long, value_name = "REFERENCE")]
        image: Option<String>,
        /// Resolve the build with **every** feature enabled (`--all-features`).
        /// This changes what is compiled and therefore what is linted, so the
        /// report names the feature set it used.
        #[arg(long, conflicts_with = "features")]
        all_features: bool,
        /// Resolve the build with these features (comma- or space-separated).
        #[arg(long, value_name = "LIST")]
        features: Option<String>,
        /// Emit the report as JSON.
        #[arg(long)]
        json: bool,
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
    /// The **sandbox image store**: what it is holding, and dropping it (#433).
    ///
    /// Separate from `security` because its subject is different. `security`'s
    /// verbs are about *analyzers* — their findings, their pinned assets, their
    /// readiness — and this one is about the machine-global cache of OCI images
    /// and the ext4 disks derived from them, which after ADR-0014 v1.6's builder
    /// work is no longer one image per analyzer.
    ///
    /// Both verbs are safe by the same property, which is also their limit:
    /// everything in this store is re-obtainable from a pinned digest, so
    /// clearing costs time and never information — and `clear` may therefore
    /// never reach anything that is not. It cannot see a findings layer, a memory
    /// record or `graph.db`, and it refuses rather than guesses when it meets
    /// something under the store root it does not recognise.
    #[cfg(feature = "execution")]
    Sandbox {
        #[command(subcommand)]
        action: SandboxAction,
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
        /// At most this many records (the newest ones); `0` is unlimited.
        ///
        /// Unlimited means every record the filters admit, in the same order and
        /// simply not cut. The same reading of `0` as `search --limit` and
        /// `memory recall --limit`, which is the point: one parameter must not
        /// mean different things on different surfaces (issues #375, #452).
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
        /// At most this many records — the best-ranked, not the newest; `0` is
        /// unlimited.
        ///
        /// Unlimited means every live record, still ranked and simply not cut.
        /// The same reading of `0` as `search --limit` and `memory list
        /// --limit`, which is the point: one parameter must not mean different
        /// things on different surfaces (issues #375, #452).
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
    /// Run an analyzer against this worktree and file its findings.
    ///
    /// **The sandbox is the default.** With no flag, the analyzer is executed
    /// inside a digest-pinned OCI image in a microVM (`exec-boxlite`), and the
    /// run's evidence records `isolation=microvm`. That backend is not in the
    /// default feature set, so on a stock build this is a named refusal that
    /// says which feature to rebuild with — never a silent absence, and never a
    /// silent downgrade to the host.
    ///
    /// `--allow-unsandboxed` selects the other backend: a child process on this
    /// host, with no boundary (`exec-subprocess`, on by default). It is required
    /// for that path and means exactly what it has always meant — consent to
    /// execute a third-party binary here with nothing between it and this
    /// machine. It is not implied by anything, and the sandbox existing does not
    /// weaken it.
    ///
    /// Whichever backend runs, assets are never fetched: a cold cache fails and
    /// names the `prefetch` command, having executed nothing.
    Run {
        /// The analyzer to run (`roteiro security status` lists them).
        analyzer: String,
        /// Execute inside the sandbox. This is already the default; saying it
        /// explicitly pins the intent so a script cannot be re-aimed at the
        /// host by a change of defaults.
        #[arg(long, conflicts_with = "allow_unsandboxed")]
        sandboxed: bool,
        /// Accept that this run has no isolation boundary, and execute the
        /// analyzer as a child process on this host. Required for that path.
        #[arg(long)]
        allow_unsandboxed: bool,
        /// Emit the run report as JSON.
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
        /// Pull this digest-pinned linter image instead of `[lint] image`.
        ///
        /// The counterpart of `roteiro lint --image`, and it exists so the pair
        /// is symmetric: an image you can lint with is an image you can
        /// provision, without having to write it into a config file first to
        /// try it.
        #[arg(long, value_name = "REFERENCE")]
        image: Option<String>,
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

/// The two verbs of the sandbox image store (#433, ADR-0014 v1.6).
///
/// `status` and `clear`, and deliberately no third: there is no config key that
/// drops this cache. A setting that evicts is a *standing instruction to throw
/// work away* which fires when nobody is looking, where ADR-0013 already holds
/// eviction to be a maintenance act rather than a preference.
#[cfg(feature = "execution")]
#[derive(Subcommand)]
enum SandboxAction {
    /// Report what the sandbox image store is holding: one row per image, with
    /// its sizes, and the store's total.
    ///
    /// Offered alongside `clear` rather than as a convenience. A destructive verb
    /// with no way to see what it will destroy is invoked blind, which is
    /// ADR-0014 v1.6's third rule. Each row's reference is what `clear --image`
    /// takes, so what to type next is on the screen.
    Status {
        /// Emit the status as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Drop cached images, and report what that freed.
    ///
    /// **What it may touch is bounded by why it is safe.** Everything in this
    /// store is re-obtainable from a pinned digest, so clearing costs time and
    /// never information — and the verb may therefore never reach anything that
    /// is not. It refuses, naming what it found, rather than clearing anything it
    /// cannot classify.
    ///
    /// Shared blobs are handled as a set difference over the whole index, not as
    /// a walk of the named image: a layer some other cached image still uses
    /// stays, and every surviving image is re-resolved blob by blob afterwards
    /// and reported.
    Clear {
        /// Drop this image, by the reference `roteiro sandbox status` lists it
        /// under.
        #[arg(long, value_name = "REFERENCE")]
        image: Option<String>,
        /// Drop the pinned image this analyzer runs in — the same request as
        /// `--image`, spelled the way the analyzer table spells it.
        #[arg(long, value_name = "NAME")]
        analyzer: Option<String>,
        /// Drop **every** cached image, and the bytes under the store root that
        /// no image claims.
        ///
        /// A separate argument from `--image` and `--analyzer` on purpose: "clear
        /// this one" and "clear everything" are different requests, and a caller
        /// asking for one must not be able to receive the other by supplying
        /// nothing (ADR-0014 v1.6).
        #[arg(long)]
        everything: bool,
        /// Report what would be removed, and remove nothing.
        #[arg(long)]
        dry_run: bool,
        /// Emit the report as JSON.
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
            graph_context,
        } => match (score, replay, llm) {
            (Some(run), _, _) => review::run_score(&run, corpus.as_deref(), json),
            (None, Some(out), _) => {
                run_replay(&out, checks.as_deref(), limit, graph_context, ingest)
            }
            (None, None, true) => {
                run_llm_review(base.as_deref(), checks.as_deref(), graph_context, ingest)
            }
            (None, None, false) => run_review(ingest, json, base.as_deref(), debt_ignore),
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
        Command::Render {
            target,
            out,
            workspace_name,
        } => run_render(
            &cfg.effective,
            ingest,
            &target,
            out,
            debt_ignore,
            workspace_name.as_deref(),
        ),
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
        Command::Lint {
            analyzer,
            sandboxed,
            allow_unsandboxed,
            image,
            all_features,
            features,
            json,
        } => run_lint(
            &analyzer,
            &lint_features(all_features, features.as_deref())?,
            lint_host_decision(&cfg, sandboxed, allow_unsandboxed),
            json,
            image.as_deref().or(cfg.effective.lint.image.as_deref()),
        ),
        #[cfg(feature = "execution")]
        Command::Security { action } => run_security(
            action,
            cfg.effective.lint.image.as_deref(),
            &cfg.effective.security,
        ),
        #[cfg(feature = "execution")]
        Command::Sandbox { action } => run_sandbox(action),
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

/// Render `[serve] max_context_tokens` as what it **means**, not as the number
/// it is stored as.
///
/// `0` is a sentinel for "no ceiling of your own — each model's trained window",
/// and printing it as the bare string `0` is the defect issues #453 and #375
/// both name: *a sentinel rendered as itself rather than as its meaning*. Here it
/// is at its worst, because `roteiro config` exists to answer "why did it do
/// that?" — an operator reading `0` sees a limit of zero and concludes the
/// opposite of the truth.
///
/// Unset and an explicit `0` mean the same thing but are **not** printed the
/// same, because at this point they are still distinguishable (`None` vs
/// `Some(0)` survive the merge) and the difference is exactly what an operator
/// debugging a config file needs: whether the line they wrote is being read at
/// all. Collapsing them would discard that.
///
/// A non-zero value is reported with the rule that actually applies to it, since
/// the ceiling never raises a window past the model's own `n_ctx_train` — see
/// [`rto_llama`]'s `window_for_request`.
fn max_context_tokens_display(value: Option<u32>) -> String {
    match value {
        None => "unset — each model's trained window".to_owned(),
        Some(0) => "0 — each model's trained window".to_owned(),
        Some(n) => format!("{n} — or the model's trained window, whichever is lower"),
    }
}

#[cfg(test)]
mod config_render_tests {
    use super::max_context_tokens_display;

    /// The defect this guards, and the reason it is a test rather than a
    /// comment: a bare `0` in `roteiro config` tells an operator the opposite of
    /// the truth — that the window is zero, when `0` means "no ceiling of mine,
    /// use each model's trained window". Same class as #453 and #375.
    #[test]
    fn the_zero_sentinel_is_rendered_as_its_meaning_not_as_zero() {
        let shown = max_context_tokens_display(Some(0));
        assert!(
            shown.contains("trained window"),
            "an explicit `0` must state what it means: {shown}"
        );
        assert_ne!(shown, "0", "`0` must never be rendered as bare `0`");
    }

    /// Unset means the same as `0`, and is also a rule rather than a number —
    /// `None` must not surface as the literal `None` either.
    #[test]
    fn unset_is_rendered_as_its_meaning_not_as_none() {
        let shown = max_context_tokens_display(None);
        assert!(
            shown.contains("trained window"),
            "unset must state what it means: {shown}"
        );
        assert!(
            !shown.contains("None"),
            "unset must not surface as `None`: {shown}"
        );
    }

    /// The two are equivalent in effect but **distinguishable in the report**,
    /// which is the point: an operator debugging a config file needs to know
    /// whether the line they wrote is being read at all. If the merge ever
    /// collapses `Some(0)` into `None`, this fails and says so.
    #[test]
    fn unset_and_explicit_zero_read_differently() {
        assert_ne!(
            max_context_tokens_display(None),
            max_context_tokens_display(Some(0)),
            "unset and an explicit `0` must be told apart in the report"
        );
        assert!(max_context_tokens_display(Some(0)).starts_with('0'));
        assert!(max_context_tokens_display(None).starts_with("unset"));
    }

    /// A real ceiling still leads with its number — the meaning is added, not
    /// substituted, so the value an operator wrote is the first thing they see.
    #[test]
    fn a_real_ceiling_leads_with_its_number_and_states_the_clamp() {
        let shown = max_context_tokens_display(Some(32_768));
        assert!(
            shown.starts_with("32768"),
            "a set value must lead with itself: {shown}"
        );
        // The clamp is the other half of "why did it do that?": a ceiling above
        // a model's trained window does not raise that model's window.
        assert!(
            shown.contains("trained window"),
            "a set value must say it is still bounded per model: {shown}"
        );
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
        // The effective config, plus **added** top-level keys carrying the model
        // and workspace resolutions. Added rather than nested: every existing path
        // (`infer.min_confidence`, `debt.ignore`, …) is where it was, so a
        // consumer written against the old shape keeps working.
        //
        // `workspace_resolution` carries what the text output carries (issue
        // #499): the effective config already serialises the *declared*
        // `workspace`/`workspaces`/`standalone` tables, and this adds the
        // membership each of them resolves to, so `--json` can answer "which
        // repos are in workspace X" without re-deriving the rule.
        let mut value = serde_json::to_value(&loaded.effective)?;
        if let Some(obj) = value.as_object_mut() {
            obj.insert("model_resolution".to_owned(), model_resolution_json(loaded));
            obj.insert(
                "workspace_resolution".to_owned(),
                serde_json::to_value(workspace_resolution(loaded))?,
            );
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
    // Reported because this is the key an operator comes to this command to
    // check: "why did that request get the window it got?" (issue #486). Both
    // the unset case and an explicit `0` stand for a *rule* rather than a
    // number, so both are spelled out — see `max_context_tokens_display`.
    println!(
        "  max_context_tokens = {}  ({})",
        max_context_tokens_display(e.serve.max_context_tokens),
        source(
            p.serve.max_context_tokens.is_some(),
            u.serve.max_context_tokens.is_some()
        )
    );
    print_remote_section(loaded);
    print_lint_section(loaded);
    print_security_section(loaded);
    print_debt_section(loaded);
    print_telemetry_section(e, p, u);
    print_workspace_section(loaded);
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

/// Print `[lint]` — the **other** table whose key does not follow this report's
/// `project > user` rule (ADR-0020 §6).
///
/// Its own function, beside [`print_remote_section`] and for the same reason: a
/// reader who applies the general precedence here will be wrong, and being wrong
/// about this key means believing builds run on your machine when they do not,
/// or the reverse. So the per-layer values are printed rather than only the
/// merged one, and each is labelled with what that layer is allowed to do.
///
/// It also names the **default** in the header, because for this key the default
/// is the whole story: unset means sandboxed, and sandboxed means the run needs
/// an image nobody can supply but the reader. A section that printed two `None`s
/// without saying so would be accurate and useless.
#[cfg(feature = "execution")]
fn print_lint_section(loaded: &config::Loaded) {
    println!("\n[lint]  (ADR-0020 §6 — sandboxed by default; the host is opt-in)");
    println!(
        "  allow_unsandboxed = {:?}  (project: {:?} — may deny, never grant; user: {:?} — may \
         grant)",
        loaded.effective.lint.allow_unsandboxed,
        loaded.project.lint.allow_unsandboxed,
        loaded.user.lint.allow_unsandboxed,
    );
    if loaded.project.lint.allow_unsandboxed == Some(true) {
        println!(
            "  note: the project file's `allow_unsandboxed = true` was read and ignored — a \
             committed file may deny host execution but never grant it"
        );
    }
    // The one line that stops this section from being read as `[remote]`'s
    // twin. The two tables look identical and differ in exactly this, so the
    // difference is stated where someone is looking at both.
    println!(
        "  either this key or `--allow-unsandboxed` is enough — unlike `[remote] enabled`, which \
         needs its flag as well"
    );
    // Ordinary precedence, unlike the key above it, so the layers are printed
    // the way `[remote] endpoint` prints them rather than the way
    // `allow_unsandboxed` does — the two rules sitting in one table is exactly
    // the sort of thing a reader has to be able to see rather than infer.
    println!(
        "  image             = {:?}  (project: {:?}; user: {:?} — ordinary precedence, project \
         over user, `--image` over both)",
        loaded.effective.lint.image, loaded.project.lint.image, loaded.user.lint.image,
    );
    if loaded.effective.lint.image.is_none() {
        println!(
            "  no image is set, so a sandboxed lint refuses. Roteiro ships no default: no \
             first-party Rust image carries `clippy` (rust-lang/docker-rust builds every variant \
             `--profile minimal`), and choosing a third party's would make somebody else's \
             container the boundary your build scripts run in. See docs/SANDBOXED_LINTING.md."
        );
    }
}

/// Without `execution` there is no `roteiro lint`, and the section says so
/// rather than being absent — an omitted section reads as "no such setting",
/// which would leave someone with `allow_unsandboxed = true` in their config
/// believing it was honoured.
#[cfg(not(feature = "execution"))]
fn print_lint_section(_loaded: &config::Loaded) {
    println!(
        "\n[lint]  (ADR-0020 §6 — not built: this binary has no `roteiro lint`, so nothing here \
         permits anything, whatever the keys say)"
    );
}

/// Print `[security]` — the images analyzers are sandboxed in, per layer.
///
/// Its own function beside [`print_lint_section`], and the header names the
/// precedence *because* it sits next to a table that does not follow it. `[lint]`
/// holds one key that inverts and one that does not; this table holds only
/// locators, so a reader coming from the section above has to be told that the
/// ordinary rule is back, rather than left to infer which of the two neighbours
/// they are looking at.
///
/// It **reports** a bad entry rather than refusing over one. That is ADR-0007
/// v1.3's rule and it is not a softening: the refusal fires at every consuming
/// site, and this is the command an operator runs precisely because a key is not
/// doing what they expected — the one command it must not stop.
fn print_security_section(loaded: &config::Loaded) {
    println!("\n[security]  (ADR-0014 — locators, so ordinary precedence: project over user)");
    let effective = &loaded.effective.security.images;
    if effective.is_empty() {
        println!(
            "  images = {{}}  — nothing declared, so `security run` uses only the images this \
             build pins. An analyzer with no pin has no sandboxed path; declare one under \
             `[security.images]` to give it one. See docs/SANDBOXED_LINTING.md."
        );
    } else {
        for (analyzer, reference) in effective {
            // Per entry rather than per table, for the reason `[debt] ignore`
            // reports per pattern: the effective map can hold entries from both
            // layers at once, so one label for the table would be a lie.
            let layer = match (
                loaded.project.security.images.get(analyzer),
                loaded.user.security.images.get(analyzer),
            ) {
                (Some(_), Some(_)) => "project (over user)",
                (Some(_), None) => "project",
                (None, Some(_)) => "user",
                (None, None) => "unknown",
            };
            println!("  images.{analyzer} = {reference:?}  ({layer})");
        }
    }
    for problem in loaded.effective.security.problems() {
        // Named as read-and-refused rather than printed bare, so nobody reads
        // this section as a list of settings that are in force.
        println!("  ** this entry is refused wherever it is used: {problem}");
    }
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

/// Print every workspace config section (ADR-0008/0009), with each value's
/// provenance: the legacy singular `[workspace]`, the named `[[workspaces]]`, the
/// `[standalone]` table, the **resolved** workspaces those three produce, and
/// `[[links]]`.
///
/// This once printed the `[workspace]` table and nothing else, so a config
/// declaring three workspaces via `[[workspaces]]`/`[standalone]` rendered as
/// `roots = None` / `repos = None` — a confident "you have none" from the one
/// command whose job is being believed about what the configuration did (issue
/// #499). Both halves are now reported, because they answer different questions:
/// the **declared** tables say what you wrote, and the **resolved** list says
/// which repos that turned into, which is the only way "I declared two roots and
/// got seven repos" is visible.
fn print_workspace_section(loaded: &config::Loaded) {
    let e = &loaded.effective;
    let (p, u) = (&loaded.project, &loaded.user);
    println!("[workspace]  (legacy singular table; folds in as the `default` workspace when set)");
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
    print_named_workspaces_section(e, p, u);
    print_standalone_section(e, p, u);
    print_resolved_workspaces_section(loaded);
    print_links_section(e);
}

/// Print the declared `[[workspaces]]` entries. The array is overlaid whole (a
/// project layer declaring any wins outright), so provenance is reported once for
/// the array rather than per entry.
fn print_named_workspaces_section(e: &config::Config, p: &config::Config, u: &config::Config) {
    if e.workspaces.is_empty() {
        println!("[[workspaces]]  (none declared)");
        return;
    }
    println!(
        "[[workspaces]]  ({} declared)  ({})",
        e.workspaces.len(),
        provenance(!p.workspaces.is_empty(), !u.workspaces.is_empty())
    );
    for w in &e.workspaces {
        println!("  {}", w.name);
        println!("    roots = {:?}", w.roots);
        println!("    repos = {:?}", w.repos);
    }
}

/// Print the declared `[standalone]` table, with per-field provenance (it merges
/// field-by-field, project over user, like `[workspace]`).
fn print_standalone_section(e: &config::Config, p: &config::Config, u: &config::Config) {
    println!(
        "[standalone]  (each discovered repo becomes its own unlinked, single-repo workspace)"
    );
    println!(
        "  roots = {:?}  ({})",
        e.standalone.roots,
        provenance(p.standalone.roots.is_some(), u.standalone.roots.is_some())
    );
    println!(
        "  repos = {:?}  ({})",
        e.standalone.repos,
        provenance(p.standalone.repos.is_some(), u.standalone.repos.is_some())
    );
}

/// Print the `[[links]]` section — this repo's authored cross-repo links.
fn print_links_section(e: &config::Config) {
    if e.links.is_empty() {
        return;
    }
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

/// Print what the three declared tables actually **resolve** to: each workspace
/// `-w <name>` can select, and the repos in it right now.
///
/// A group that resolves to no repos says which kind of nothing it is, because
/// "nothing declared" and "nothing matched" are different facts and an empty list
/// states neither. A group whose roots will not read is reported on its own line
/// rather than failing the command: `roteiro config` is what you run *because*
/// something else is broken, so it has to keep working when it is.
fn print_resolved_workspaces_section(loaded: &config::Loaded) {
    let resolution = workspace_resolution(loaded);
    if let Some(err) = &resolution.error {
        println!("[workspaces resolved]  UNRESOLVED — {err}");
        println!("  the declared tables above still stand; fix the config and re-run");
        return;
    }
    if resolution.workspaces.is_empty() {
        println!(
            "[workspaces resolved]  (none — no `[workspace]`, `[[workspaces]]` or `[standalone]` \
             names anything, so `serve`/`explorer`/`links` fall back to the current repo alone)"
        );
        return;
    }
    println!(
        "[workspaces resolved]  ({} workspace(s) — what `-w <name>` selects, and who is in each)",
        resolution.workspaces.len()
    );
    for w in &resolution.workspaces {
        println!(
            "  {}  [{}]  (from {}, {})",
            w.name,
            if w.linked { "linked" } else { "standalone" },
            w.declared_in,
            w.source
        );
        println!(
            "    declared: {} root(s), {} repo(s)",
            w.declared_roots.len(),
            w.declared_repos.len()
        );
        for r in &w.declared_roots {
            println!("      root  {r}");
        }
        for r in &w.declared_repos {
            println!("      repo  {r}");
        }
        print_resolved_members(w);
    }
}

/// The `resolved:` lines for one workspace: its members, or which kind of nothing
/// it came to.
fn print_resolved_members(w: &WorkspaceReport) {
    if let Some(err) = &w.resolution_error {
        println!("    resolved: UNKNOWN — {err}");
        return;
    }
    let Some(repos) = &w.resolved_repos else {
        return;
    };
    if repos.is_empty() {
        if w.declared_roots.is_empty() && w.declared_repos.is_empty() {
            println!(
                "    resolved: 0 repo(s) — nothing DECLARED (this workspace names no roots \
                 and no repos)"
            );
        } else {
            println!(
                "    resolved: 0 repo(s) — nothing MATCHED (the declared roots/repos above \
                 read, but no git repo was found under them)"
            );
        }
        return;
    }
    println!("    resolved: {} repo(s)", repos.len());
    for r in repos {
        println!("      {r}");
    }
}

/// One workspace as `roteiro config` reports it (issue #499): the table it was
/// declared in, the layer that table came from (ADR-0007 provenance), what was
/// declared, and what that declaration resolves to right now.
#[derive(serde::Serialize)]
struct WorkspaceReport {
    /// The selector `-w <name>` takes.
    name: String,
    /// `true` for a linked group (`[workspace]` / `[[workspaces]]`); `false` for a
    /// `[standalone]` repo, which is its own single-repo workspace.
    linked: bool,
    /// The config table this group came from — for `[standalone]`, the field
    /// within it, since a standalone group is one repo and came from exactly one.
    declared_in: String,
    /// The layer that declaration came from: `project`, `user`, or `default`.
    /// `[workspace]` merges per field, so its two fields are reported separately
    /// when they disagree rather than one of them being picked.
    source: String,
    /// Root directories as declared (before any scanning).
    declared_roots: Vec<String>,
    /// Explicit member repos as declared.
    declared_repos: Vec<String>,
    /// The member repos this group resolves to now. Absent only when resolution
    /// failed; an empty list is a real answer (see `resolution_error`).
    #[serde(skip_serializing_if = "Option::is_none")]
    resolved_repos: Option<Vec<String>>,
    /// Why this group could not be resolved (e.g. an unreadable root), reported
    /// per entry rather than failing the whole command.
    #[serde(skip_serializing_if = "Option::is_none")]
    resolution_error: Option<String>,
}

/// Every workspace the config resolves to, for both the text and `--json` output
/// of `roteiro config`.
#[derive(serde::Serialize)]
struct WorkspaceResolution {
    /// The resolved groups, in resolution order (`default`, then each
    /// `[[workspaces]]`, then each `[standalone]` repo).
    workspaces: Vec<WorkspaceReport>,
    /// Set when the *set* could not be resolved at all — a duplicate linked
    /// workspace name, or an unreadable `[standalone]` root. Then `workspaces` is
    /// empty and the declared tables are all that can honestly be reported.
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// Whether a resolved `[standalone]` group's repo was named explicitly in
/// `[standalone] repos` (rather than discovered under `[standalone] roots`) — the
/// two fields carry separate provenance, and the group came from one of them.
/// Compared after `~` expansion, the same normalisation
/// [`config::Config::resolved_workspaces`] applied on the way in.
fn standalone_repo_is_explicit(e: &config::Config, repo: Option<&String>) -> bool {
    let Some(repo) = repo else { return false };
    e.standalone
        .repos
        .iter()
        .flatten()
        .any(|r| config::expand_tilde(r).as_ref() == std::path::Path::new(repo))
}

/// Build the workspace resolution report: [`config::Config::resolved_workspaces`]
/// for the group list, and [`resolved_repo_paths`] for each group's membership —
/// the same two functions `serve` and `explorer` use, deliberately rather than a
/// second derivation of the rule, which is how a "what did it resolve to" report
/// comes to disagree with what actually resolved.
fn workspace_resolution(loaded: &config::Loaded) -> WorkspaceResolution {
    let e = &loaded.effective;
    let (p, u) = (&loaded.project, &loaded.user);
    // `[[workspaces]]` is overlaid whole (project wins outright when it declares
    // any); `[workspace]` and `[standalone]` merge per field, so each of their
    // fields carries its own provenance and none is folded into another's.
    let named_source = provenance(!p.workspaces.is_empty(), !u.workspaces.is_empty());
    let ws_roots_src = provenance(p.workspace.roots.is_some(), u.workspace.roots.is_some());
    let ws_repos_src = provenance(p.workspace.repos.is_some(), u.workspace.repos.is_some());
    let legacy_source = if ws_roots_src == ws_repos_src {
        ws_roots_src.to_owned()
    } else {
        format!("roots {ws_roots_src}, repos {ws_repos_src}")
    };
    let sa_roots_src = provenance(p.standalone.roots.is_some(), u.standalone.roots.is_some());
    let sa_repos_src = provenance(p.standalone.repos.is_some(), u.standalone.repos.is_some());
    let resolved = match e.resolved_workspaces() {
        Ok(r) => r,
        Err(err) => {
            return WorkspaceResolution {
                workspaces: Vec::new(),
                error: Some(err.to_string()),
            };
        }
    };
    let workspaces = resolved
        .into_iter()
        .map(|rw| {
            let (declared_in, source) = if rw.linked {
                if e.workspaces.iter().any(|w| w.name == rw.name) {
                    ("[[workspaces]]".to_owned(), named_source.to_owned())
                } else {
                    ("[workspace]".to_owned(), legacy_source.clone())
                }
            } else if standalone_repo_is_explicit(e, rw.repos.first()) {
                // A standalone group is exactly one repo, so it came from exactly
                // one field — attribute it to that one rather than to whichever
                // field of the table happens to be set.
                ("[standalone] repos".to_owned(), sa_repos_src.to_owned())
            } else {
                ("[standalone] roots".to_owned(), sa_roots_src.to_owned())
            };
            let (resolved_repos, resolution_error) =
                match resolved_repo_paths(std::slice::from_ref(&rw), &[]) {
                    Ok(paths) => (
                        Some(paths.iter().map(|p| p.display().to_string()).collect()),
                        None,
                    ),
                    Err(err) => (None, Some(err.to_string())),
                };
            WorkspaceReport {
                name: rw.name,
                linked: rw.linked,
                declared_in,
                source,
                declared_roots: rw.roots,
                declared_repos: rw.repos,
                resolved_repos,
                resolution_error,
            }
        })
        .collect();
    WorkspaceResolution {
        workspaces,
        error: None,
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
    let cwd = std::env::current_dir()?;
    open_graph_at(&cwd)
}

/// [`open_graph`], for a repository found from `dir` rather than the current
/// directory — a workspace vault renders each member in turn, from wherever the
/// command was run.
fn open_graph_at(
    dir: &std::path::Path,
) -> anyhow::Result<(rto_graph::Repo, rto_graph::Store, rto_graph::ObjectCache)> {
    use rto_graph::{ObjectCache, Repo, Store};
    let repo = Repo::discover(dir)?;
    let store_dir = repo.git_dir().join("roteiro");
    std::fs::create_dir_all(&store_dir)?;
    let store = Store::open(&store_dir.join("graph.db"))?;
    let cache = ObjectCache::open(repo.common_dir().join("roteiro").join("objects"))?;
    Ok((repo, store, cache))
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

/// Parse the authored layer out of `blobs` and apply it to `store`, returning the
/// check report.
///
/// # Why this is a shared function and not two similar loops
///
/// The authored layer's *classification* — which path is an ADR, which markdown
/// is a blueprint, which file merely carries `@rto:` annotations, and that a
/// malformed ADR is drift rather than a skippable warning — is a rule with one
/// correct answer. It had one caller when there was one tree to build from
/// ([`build_graph`]); Stage 35b PR 2 added a second ([`build_graph_at_rev`], for
/// the graph arm's context at a historical `reviewed_sha`); and the read-only
/// `check` tool surfaces are a third, reaching it through
/// [`rto_spec::tool_check`].
///
/// That third caller is why the rule itself now lives in
/// [`rto_spec::authored_layer_from`] rather than in this function: a tool surface
/// must not write, and everything below is a write. This function is the writing
/// half — `rto_spec::run` plus the malformed ADRs — wrapped around the shared
/// classification, so the two halves compose without either being copied.
///
/// Copying the loop would leave the callers free to drift, which is the exact
/// shape this repository has closed three times already — the `[debt]` ignore
/// honoured on three surfaces and not a fourth, `limit=0` meaning two things
/// across five endpoints, and `ReviewSet::collect` in `review_llm`. A graph arm
/// whose ADRs were classified by a slightly different rule than `check`'s would
/// be measuring its own reimplementation.
///
/// `read` yields a blob's authored bytes, or `None` when the tree has no such file
/// (a worktree deletion); the caller supplies it because *where* the bytes come
/// from is precisely what differs between a tree and a rev.
fn apply_authored_layer(
    store: &mut rto_graph::Store,
    blobs: Vec<rto_graph::BlobRef>,
    read: &dyn Fn(&rto_graph::BlobRef) -> anyhow::Result<Option<Vec<u8>>>,
) -> anyhow::Result<rto_spec::CheckReport> {
    // `authored_docs_from` rather than `authored_layer_from`: the latter is the
    // former with the site pages thrown away, and the whole point of the website
    // becoming a document class is that `check` sees it. `run_layer` is the same
    // pairing on the writing side — one call over every class, so a new one
    // cannot be added to the parser and forgotten here.
    let docs = rto_spec::authored_docs_from(blobs, read)?;
    let mut report = rto_spec::run_layer(store, &docs)?;
    report.violations.extend(docs.layer.malformed);
    Ok(report)
}

// Present in a build that can review (or in one running the tests that hold this
// assembly to the corpus), and absent from a stock binary where nothing calls it.
// `test` is in the gate deliberately: the graph arm's correctness — that it is
// built at the reviewed commit, and that it actually sends something — is checked
// in the build CI runs, which has no generation backend.
/// Build the full graph **as of an arbitrary git rev** into `store` — the graph
/// arm's context source (Stage 35b PR 2).
///
/// [`rto_graph::sync_tree`] gives the derived layer at `rev` and is
/// content-addressed, so a rev sharing most blobs with an already-synced tree is
/// nearly free. But it populates the derived layer *only*, by design: it exists
/// for version-pin resolution, which needs no ADRs. The graph arm needs exactly
/// the opposite — the **authored** layer is where a governing decision lives, and
/// a governing decision is the one thing a per-file diff reviewer structurally
/// cannot see. So the authored layer is re-applied here at the same rev, through
/// the same classification [`build_graph`] uses.
///
/// Reviewing a commit against the ADRs of `HEAD` rather than of that commit would
/// be a silent wrong answer of the same family as scoring against a PR head: the
/// numbers would come out clean and would describe a repository that did not
/// exist when the code was written.
#[cfg(any(feature = "serve", feature = "inference-local-models", test))]
fn build_graph_at_rev(
    repo: &rto_graph::Repo,
    store: &mut rto_graph::Store,
    cache: &rto_graph::ObjectCache,
    ingest: rto_graph::IngestConfig,
    rev: &str,
) -> anyhow::Result<()> {
    let registry = rto_graph::Registry::new(ingest);
    rto_graph::sync_tree(store, repo, cache, &registry, rev)?;
    apply_authored_layer(store, repo.blobs_at(rev)?, &|blob| {
        Ok(Some(repo.read_blob(&blob.oid)?))
    })?;
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

    // The authored-layer file set must match the derived tree — the two layers
    // disagreeing about which tree they describe is issue #330, and it was a
    // *silent* wrong answer rather than a loud one. Both halves of that rule live
    // in `rto-spec` now: `authored_blobs` is the file set (staged files in Index
    // mode, `HEAD` in Committed, `HEAD` **plus untracked files** in Worktree —
    // precisely what `sync_worktree` overlaid into the derived layer), and
    // `authored_layer_from`, which `apply_authored_layer` wraps, is the
    // classification.
    //
    // They are split because the read-only `check` tool surfaces need the same
    // two rules and cannot go through `apply_authored_layer`: that one ends in
    // `rto_spec::run`, which writes. So the shared code is the half *below* the
    // write, and there is still exactly one copy of each rule.
    let report = apply_authored_layer(store, rto_spec::authored_blobs(repo, source)?, &|blob| {
        Ok(repo.read_source(blob, source)?)
    })?;

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
            "checked {} ADR(s), {} blueprint(s), {} site page(s): {} link(s) ok, {} annotation(s) ok, {} violation(s)",
            report.adrs,
            report.blueprints,
            report.site_pages,
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

/// The arm a `--graph-context` flag selects.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
fn review_arm(graph_context: bool) -> review_llm::ReviewArm {
    if graph_context {
        review_llm::ReviewArm::Graph
    } else {
        review_llm::ReviewArm::DiffOnly
    }
}

/// `roteiro review --llm` — review the change with the local generative model.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
fn run_llm_review(
    base: Option<&str>,
    checks: Option<&str>,
    graph_context: bool,
    ingest: rto_graph::IngestConfig,
) -> anyhow::Result<()> {
    review_llm::run_llm(
        &review_repo_root(),
        base,
        checks,
        review_arm(graph_context),
        ingest,
    )
}

/// `roteiro review --replay` — measure the reviewer against the corpus.
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
fn run_replay(
    out: &str,
    checks: Option<&str>,
    limit: Option<usize>,
    graph_context: bool,
    ingest: rto_graph::IngestConfig,
) -> anyhow::Result<()> {
    review_llm::run_replay(
        &review_repo_root(),
        out,
        checks,
        limit,
        review_arm(graph_context),
        ingest,
    )
}

/// The same two surfaces in a build with no generation backend.
///
/// A named refusal rather than a missing subcommand: `--llm` is documented in
/// `--help` whatever the build, so a stock install that answered
/// `unrecognized argument` would send someone to check their spelling instead of
/// their features. This is the shape `models` was moved into `default` for.
#[cfg(not(any(feature = "serve", feature = "inference-local-models")))]
fn run_llm_review(
    _base: Option<&str>,
    _checks: Option<&str>,
    _graph_context: bool,
    _ingest: rto_graph::IngestConfig,
) -> anyhow::Result<()> {
    anyhow::bail!(
        "`review --llm` needs a local generation backend, which this build does not \
         have. Rebuild with `--features serve` (or `inference-local-models`). \
         `roteiro review` without `--llm` needs no model and still works."
    )
}

/// See [`run_llm_review`]'s backend-less twin.
#[cfg(not(any(feature = "serve", feature = "inference-local-models")))]
fn run_replay(
    _out: &str,
    _checks: Option<&str>,
    _limit: Option<usize>,
    _graph_context: bool,
    _ingest: rto_graph::IngestConfig,
) -> anyhow::Result<()> {
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
    debt_ignore: &[String],
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
    // The repository's own `[debt] ignore`, on the same footing as `debt`,
    // `check` and the graph API: `review`'s per-file `debt` is that same
    // inventory scoped to the change, so it must be scoped by the same list
    // (issue #409).
    let review = review::build(&store, &changed, &report.violations, debt_ignore)?;

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
#[must_use]
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
///
/// The `Result` is therefore always `Ok` here, and that is the point rather than
/// an oversight: it is the signature the `models` twin above needs, so the one
/// caller can stay feature-agnostic and keep reporting an unhonourable pin as
/// itself. Removing the wrapper would split the call site in two along a seam
/// that has nothing to do with what `media status` prints.
///
/// `expect` rather than `allow` so this stays honest in both directions: the
/// item exists *only* where the lint fires, so if the twin ever gains a failure
/// this build can share, the unfulfilled expectation says so.
#[cfg(not(feature = "models"))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the Result is the cfg-parallel twin's signature, not this arm's"
)]
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
        Some(select_resolved_workspace(&resolved, name)?)
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

    // One repo, however it is spelled, is one member (issue #501). The
    // `BTreeSet<PathBuf>` above only collapses *identical* strings, and the two
    // sources spell the same repo differently: the current repo arrives fully
    // symlink-resolved (`current_dir` is `getcwd`), while a configured member
    // keeps whatever the config wrote. So `/tmp/x` and `/private/tmp/x` both
    // survived — and this is a set defect, not a count one: `run_links` iterates
    // these paths directly, so it read that repo's `[[links]]` twice and reported
    // (and counted as drift) each link twice, the second under a fabricated
    // `<name>-2` project the workspace registry does not contain, while
    // `--infer` could elect that phantom as the hub and match a repo against
    // itself.
    Ok(dedupe_repo_paths(paths))
}

/// The identity of a repo path for de-duplication: its canonical form, falling
/// back to the path exactly as written when it will not canonicalise (it does not
/// exist yet). The fallback keeps such a path in the set, so the error still
/// comes from the code whose job is to report it rather than from a silent drop
/// here.
fn repo_path_identity(p: &std::path::Path) -> std::path::PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// De-duplicate repo paths by [`repo_path_identity`], keeping each repo's first
/// spelling and the input order. The one place the "same repo reached by two
/// paths" rule lives on the CLI side — shared by `roteiro links` scoping
/// ([`links_scope_paths`]) and the serve/reload path ([`resolved_repo_paths`]) —
/// because a second copy of the rule is how the two came to disagree.
fn dedupe_repo_paths<I>(paths: I) -> Vec<std::path::PathBuf>
where
    I: IntoIterator<Item = std::path::PathBuf>,
{
    let mut seen: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();
    paths
        .into_iter()
        .filter(|p| seen.insert(repo_path_identity(p)))
        .collect()
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
        // `pinned` and `spokes_pinned` are what let a machine reader tell an
        // effective `--pinned` from an inert one: a spoke that pinned nothing
        // omits `hub_rev` entirely, so without the envelope saying the flag was in
        // effect, the JSON has the same silence the text had (#505).
        let mut envelope = serde_json::json!({
            "hub": &ready.hub_name,
            "hub_rev": &ready.hub_rev,
            "pinned": opts.pin.auto,
            "spokes": &ready.report,
            "written": written,
        });
        // `spokes_pinned` counts spokes that pinned a hub version **themselves**,
        // so it is only meaningful when `--pinned` asked them. Under a global
        // `--hub-rev` every spoke carries that one rev in `hub_rev` without having
        // pinned anything, and counting it there reported `pinned: false` beside a
        // non-zero count — the answer to a question nobody asked, in the field
        // added to stop exactly that (#505 review).
        //
        // Omitted rather than zeroed, following #410's `check`: `0` beside
        // `pinned: false` still reads as "we looked and none pinned", which is a
        // different claim from "we did not ask". `pinned` is always present, so a
        // reader has an unconditional field to branch on and never has to probe
        // for the key to know whether to expect it — the same shape as that tool's
        // always-present `gate` beside its omitted `report`, and the same idiom the
        // spoke rows already use for `hub_rev` and `pin_via`.
        if opts.pin.auto {
            let counted = ready.report.iter().filter(|r| r.hub_rev.is_some()).count();
            envelope["spokes_pinned"] = counted.into();
        }
        emit_json(&envelope)?;
    } else {
        if let Some(rev) = &ready.hub_rev {
            // Name *whose* pin. This line is reachable only via `--hub-rev` (it
            // conflicts with `--pinned`, and `--pinned` leaves `hub_rev` unset), so
            // it was never wrong — but "pinned version" alone does not say which of
            // the command's two pinning senses it means, which is the
            // under-specification #505 is about.
            println!(
                "resolved against {} @ {rev} (pinned by --hub-rev, not by the spokes)",
                ready.hub_name
            );
        }
        print_infer_report(&ready.report, &ready.hub_name, hub_key_count, opts.pin.auto);
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

/// `pinned` is whether `--pinned` asked for per-spoke pin resolution, and it is a
/// parameter rather than something inferred from the rows because **the rows a
/// no-op produces are indistinguishable from the rows the flag was never passed
/// for**. A spoke with no detectable pin falls back to the hub's `HEAD`, which is
/// the right behaviour and was also, until #505, a silent one: with no spoke
/// pinning anything the output was byte-identical to plain `--infer`, and nothing
/// in it named a rev, so an operator who asked *"does this match the version it
/// deploys?"* was shown the answer to *"does it match HEAD?"* with no way to tell.
/// So the fallback is reported, not changed — per spoke, and once in summary.
fn print_infer_report(report: &[InferredRepo], hub_name: &str, hub_keys: usize, pinned: bool) {
    println!("inferred config links (hub: {hub_name}, {hub_keys} keys)");
    let (mut nm, mut no) = (0usize, 0usize);
    for r in report {
        // Under `--pinned`, say which hub version this spoke resolved against —
        // including when the answer is "none of its own, so the hub's HEAD".
        let pin = match (&r.hub_rev, &r.pin_via) {
            (Some(rev), Some(via)) => format!("  @ {} (via {via})", short_rev(rev)),
            (Some(rev), None) => format!("  @ {}", short_rev(rev)),
            (None, _) if pinned => "  @ HEAD (no pin detected)".to_owned(),
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
    // The line that makes an inert `--pinned` legible at a glance. Printed even
    // when every spoke pinned something, because "7 of 7" is the confirmation the
    // operator wanted and costs one line to give.
    if pinned {
        let pinned_count = report.iter().filter(|r| r.hub_rev.is_some()).count();
        let fell_back = report.len() - pinned_count;
        println!(
            "{pinned_count} of {} spoke(s) pinned a hub version; {fell_back} resolved against the hub's HEAD",
            report.len()
        );
    }
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
///
/// `lint_image` is the `[lint] image` the config layers settled on. It reaches
/// `prefetch` because a builder's image is **supplied rather than pinned by
/// Roteiro** (ADR-0020 conditions 1-2): there is no entry in `SANDBOX_IMAGES` to
/// iterate for it, so the only place that knows what to pull is the config.
///
/// `security` is `[security.images]`, and it reaches three actions rather than
/// one. `run` needs the image it is about to boot; `prefetch` needs it for the
/// same reason `lint_image` does, since a declared image is not in any table to
/// iterate; and `status` needs it because a reader who cannot tell a built-in
/// image from a declared one has lost the thing the pin was for (issue #434).
#[cfg(feature = "execution")]
fn run_security(
    action: SecurityAction,
    lint_image: Option<&str>,
    security: &config::SecurityConfig,
) -> anyhow::Result<()> {
    match action {
        SecurityAction::Ingest {
            file,
            analyzer,
            json,
        } => run_security_ingest(&file, analyzer.as_deref(), json),
        SecurityAction::List { analyzer, json } => run_security_list(analyzer.as_deref(), json),
        SecurityAction::Run {
            analyzer,
            sandboxed,
            allow_unsandboxed,
            json,
        } => run_security_run(
            &analyzer,
            select_backend(sandboxed, allow_unsandboxed),
            json,
            security.image_for(&analyzer),
        ),
        #[cfg(feature = "execution")]
        SecurityAction::Prefetch {
            analyzer,
            image,
            allow_download,
            json,
        } => run_security_prefetch(
            analyzer.as_deref(),
            allow_download,
            json,
            image.as_deref().or(lint_image),
            security,
        ),
        #[cfg(feature = "execution")]
        SecurityAction::Status { analyzer, json } => {
            run_security_status(analyzer.as_deref(), json, security)
        }
    }
}

/// Dispatch the sandbox store's two verbs.
#[cfg(feature = "execution")]
fn run_sandbox(action: SandboxAction) -> anyhow::Result<()> {
    match action {
        SandboxAction::Status { json } => run_sandbox_status(json),
        SandboxAction::Clear {
            image,
            analyzer,
            everything,
            dry_run,
            json,
        } => run_sandbox_clear(image, analyzer.as_deref(), everything, dry_run, json),
    }
}

/// The reference the pinned image table holds for `analyzer`.
///
/// Gated, because the table is: `SANDBOX_IMAGES` is compiled only under
/// `exec-boxlite`. The *store* is not gated and neither is clearing it — a
/// machine that filled this cache with one build must be able to empty it with
/// another — so the absence of the table is a **named refusal that says what to
/// type instead**, rather than an argument that silently is not there.
#[cfg(all(feature = "execution", feature = "exec-boxlite"))]
fn sandbox_image_for(analyzer: &str) -> anyhow::Result<String> {
    rto_exec::boxlite::image_for(analyzer).map_or_else(
        || {
            anyhow::bail!(
                "no pinned sandbox image for analyzer `{analyzer}`.\n  \
                 `roteiro sandbox status` lists what the store is holding, \
                 and `--image` takes any of those references."
            )
        },
        |image| Ok(image.reference.to_owned()),
    )
}

/// The same, in a build with no pinned image table to consult.
#[cfg(all(feature = "execution", not(feature = "exec-boxlite")))]
fn sandbox_image_for(analyzer: &str) -> anyhow::Result<String> {
    anyhow::bail!(
        "this build cannot map analyzer `{analyzer}` to an image: the pinned image table is \
         compiled only with `--features exec-boxlite`.\n  \
         The store itself is readable and clearable without it — run \
         `roteiro sandbox status` and pass one of the references it lists to `--image`."
    )
}

/// Turn the three selectors into the one scope, or refuse naming all three.
///
/// The refusal is the point rather than a formality. ADR-0014 v1.6 requires that
/// "clear this image" and "clear everything" be **different arguments**, so
/// supplying neither must not fall through to either — least of all to the
/// destructive one.
#[cfg(feature = "execution")]
fn sandbox_scope(
    image: Option<String>,
    analyzer: Option<&str>,
    everything: bool,
) -> anyhow::Result<rto_exec::Scope> {
    let asked =
        usize::from(image.is_some()) + usize::from(analyzer.is_some()) + usize::from(everything);
    if asked > 1 {
        anyhow::bail!(
            "`--image`, `--analyzer` and `--everything` are different requests; pass exactly one."
        );
    }
    if everything {
        return Ok(rto_exec::Scope::Everything);
    }
    if let Some(reference) = image {
        return Ok(rto_exec::Scope::Image(reference));
    }
    if let Some(analyzer) = analyzer {
        return Ok(rto_exec::Scope::Image(sandbox_image_for(analyzer)?));
    }
    anyhow::bail!(
        "`roteiro sandbox clear` needs to be told what to drop.\n  \
         `--image REFERENCE` or `--analyzer NAME` drops one; `--everything` drops all of them.\n  \
         Run `roteiro sandbox status` first — it lists each reference and what it is costing."
    )
}

/// Report what the sandbox image store is holding.
#[cfg(feature = "execution")]
fn run_sandbox_status(json: bool) -> anyhow::Result<()> {
    let report = rto_exec::sandbox_status(&rto_exec::asset_root())?;
    if json {
        return emit_json(&report);
    }

    // The scope is printed, not implied. There is one of these per asset root and
    // every repository on the machine shares it, so a figure read next to a
    // project name would otherwise be attributed to that project.
    println!("sandbox image store: {}  (machine-wide)", report.store);
    if !report.present {
        println!("\nnothing is cached — the store does not exist yet");
        return Ok(());
    }
    if report.images.is_empty() {
        println!("\nno images cached");
    } else {
        println!("\nimages");
    }
    for image in &report.images {
        println!("  {}", image.reference);
        println!(
            "      {}  ({} reclaimable on its own)",
            human_bytes(image.bytes.total),
            human_bytes(image.bytes.exclusive)
        );
        println!(
            "      layers {} · extracted {} · disk image {} · base {}",
            human_bytes(image.bytes.layers),
            human_bytes(image.bytes.extracted),
            human_bytes(image.bytes.disk_image),
            human_bytes(image.bytes.base_disk)
        );
        // The object tally is the pulled content — manifest, config, one per
        // distinct layer. Not the derived trees and disks, which are built on
        // first run: an image that has only ever been pulled is complete without
        // them, and counting them would report every fresh pull as damaged.
        println!(
            "      {} layer(s), {}/{} object(s) present, cached {}{}",
            image.layers,
            image.objects.present,
            image.objects.expected,
            image.cached_at,
            if image.pull_complete {
                ""
            } else {
                "  ** the index calls this pull incomplete — re-run `roteiro security prefetch` **"
            }
        );
    }

    sandbox_status_tail(&report);
    Ok(())
}

/// The parts of a status screen that are about the store rather than an image.
///
/// Lifted out so [`run_sandbox_status`] is one loop over images: these four
/// sections each answer a different question — what is not attributable, what
/// will never be cleared, what is holding the store open, and what to type next.
#[cfg(feature = "execution")]
fn sandbox_status_tail(report: &rto_exec::SandboxStatus) {
    if !report.unattributed.is_empty() {
        println!("\nunattributed — no cached image claims these bytes");
        for entry in &report.unattributed {
            println!("  {}  {}", human_bytes(entry.bytes), entry.path);
        }
        println!("  `--everything` removes them; a per-image clear never does.");
    }
    if !report.preserved.is_empty() {
        println!("\nnever cleared — no pinned digest re-obtains these");
        for entry in &report.preserved {
            println!("  {}\n      {}", entry.path, entry.reason);
        }
    }
    if report.live_boxes > 0 {
        println!(
            "\n{} box(es) are registered in this store; `clear` will refuse until they are gone",
            report.live_boxes
        );
    }

    println!("\ntotal {}", human_bytes(report.total_bytes));
    // Offered only when there is something to reclaim. A cleared store is a few
    // kilobytes of empty index, and telling somebody they could reclaim that is
    // how a useful line becomes one people stop reading.
    if !report.images.is_empty() || !report.unattributed.is_empty() {
        println!("  `roteiro sandbox clear --everything` reclaims it; every byte is re-obtainable");
        println!("  by `roteiro security prefetch`, so it costs a re-pull and never information.");
    }
    // The boundary, said out loud. This verb owns the image store and not the
    // rest of the asset cache, and a reader who has just been told a total wants
    // to know whether it was the whole of what is on disk.
    println!("  The pinned assets beside it — the sandbox runtime, analyzer rules — are");
    println!("  reported by `roteiro security status` and are not in this total.");
}

/// Drop cached images and report what that freed.
#[cfg(feature = "execution")]
fn run_sandbox_clear(
    image: Option<String>,
    analyzer: Option<&str>,
    everything: bool,
    dry_run: bool,
    json: bool,
) -> anyhow::Result<()> {
    let scope = sandbox_scope(image, analyzer, everything)?;
    let root = rto_exec::asset_root();
    let report = if dry_run {
        rto_exec::sandbox_plan(&root, &scope)?.0
    } else {
        rto_exec::sandbox_clear(&root, &scope)?
    };
    if json {
        return emit_json(&report);
    }

    println!("sandbox image store: {}  (machine-wide)", report.store);
    let verb = if report.applied {
        "cleared"
    } else {
        "would clear"
    };
    println!("{verb}: {}", report.requested);

    if report.removed.is_empty() && report.removed_unattributed.is_empty() {
        println!("\nnothing to remove");
        return Ok(());
    }
    println!("\nimages");
    for removed in &report.removed {
        println!(
            "  {}\n      {}, {} object(s)",
            removed.reference,
            human_bytes(removed.freed_bytes),
            removed.objects_removed
        );
    }
    for entry in &report.removed_unattributed {
        println!(
            "  unattributed: {}\n      {}",
            entry.path,
            human_bytes(entry.bytes)
        );
    }

    sandbox_clear_tail(&report);
    Ok(())
}

/// The freed-bytes accounting and the survivors' verification.
///
/// Both are obligations rather than niceties. ADR-0014 v1.6 requires the verb to
/// **report what it freed**, so an 8.5 GB re-pull appears in the transcript rather
/// than turning up later as a mystery. And a `clear` that cannot demonstrate the
/// surviving images are still complete is one nobody trusts twice (#433) — so the
/// survivors are re-resolved against the filesystem after the deletion, and the
/// tally is printed whether or not it is reassuring.
#[cfg(feature = "execution")]
fn sandbox_clear_tail(report: &rto_exec::ClearReport) {
    println!("\nfreed {}", human_bytes(report.freed_bytes));
    if report.applied {
        println!(
            "  store {} → {} (measured {})",
            human_bytes(report.store_bytes_before),
            human_bytes(report.store_bytes_after),
            human_bytes(report.measured_freed_bytes())
        );
    }

    if !report.preserved.is_empty() {
        println!("\nleft alone — no pinned digest re-obtains these");
        for entry in &report.preserved {
            println!("  {}\n      {}", entry.path, entry.reason);
        }
    }

    if !report.applied {
        println!("\nnothing was removed — drop `--dry-run` to apply this");
        return;
    }
    if report.retained.is_empty() {
        println!("\nno images remain");
        return;
    }
    println!("\nsurviving images, re-checked against the filesystem");
    for image in &report.retained {
        println!(
            "  {}\n      {}/{} object(s) present{}",
            image.reference,
            image.objects.present,
            image.objects.expected,
            if image.complete {
                ""
            } else {
                "  ** INCOMPLETE — this image can no longer run; re-run `roteiro security prefetch` **"
            }
        );
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
///
/// That suppression used to be written out here, and is now
/// [`rto_exec::cross_reference_across_analyzers`], because writing it out here was
/// what let the model-facing `security_list` document the guard and not have it
/// (PR #468 review). One description with two implementations is this repository's
/// recurring defect; one implementation with two call sites is the fix.
#[cfg(feature = "execution")]
fn security_cross_reference(layers: &[rto_graph::FindingsLayer]) -> Vec<SecurityCrossReference> {
    rto_exec::cross_reference_across_analyzers(layers)
        .into_iter()
        .map(|c| {
            // From `Correspondence::confirmed_by`, not a second count written here:
            // the same number now appears on the CLI and on both model-facing tool
            // surfaces, and one concept reporting different figures on different
            // surfaces is issue #321. Read before `c` is taken apart below.
            let confirmed_by = c.confirmed_by();
            SecurityCrossReference {
                advisory: c.advisory,
                aliases: c.aliases,
                package: c.package,
                version: c.version,
                confirmed_by,
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
            }
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
        // Fragments joined by exactly one space, never one `\`-wrapped literal.
        // A continuation swallows the next line's indentation, so an edit that
        // drops one turns source columns into user-visible text — which is how
        // this refusal came to ship 14 of them (#522). The rule and the reason
        // are `rto_exec::guidance`'s; wrapping is the list, not an escape, so
        // there is no continuation here to lose.
        let message = [
            format!(
                "{source} is not a normalized `{}` report,",
                rto_exec::REPORT_SCHEMA
            ),
            "so it must be an analyzer's own output —".to_owned(),
            "say which analyzer with `--analyzer <name>`".to_owned(),
            format!("(known: {})", rto_exec::known_analyzers().join(", ")),
        ]
        .join(" ");
        anyhow::anyhow!(message)
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
///
/// Gated on having a backend rather than on `execution`, because the only thing
/// that constructs it is [`execute_and_file`] — see there for why the two arms
/// of the `any` are named separately instead of collapsed to `exec-subprocess`.
#[cfg(all(
    feature = "execution",
    any(feature = "exec-boxlite", feature = "exec-subprocess")
))]
#[derive(serde::Serialize)]
struct SecurityRunReport {
    layer: String,
    analyzer: String,
    analyzer_version: String,
    runner: rto_graph::RunnerKind,
    isolation: rto_graph::Isolation,
    /// The pinned image the analyzer ran inside, absent for a host run. Carried
    /// beside `runner` and `isolation` because those three together are the
    /// answer to "where did this finding come from", and a consumer that had
    /// only the first two could not tell one sandboxed run from another.
    #[serde(skip_serializing_if = "Option::is_none")]
    image_digest: Option<String>,
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

/// Which backend `roteiro security run` was asked for.
///
/// Named rather than a `bool` because the two are not opposites of one
/// permission: they are different machines to run on, with different evidence,
/// and the choice is made once — here — instead of being re-derived from flags
/// at each place that cares.
#[cfg(feature = "execution")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunBackend {
    /// A pinned OCI image in a microVM.
    Sandboxed,
    /// A child process on this host, with no boundary.
    Subprocess,
}

/// Resolve the two flags into the backend that will run.
///
/// **The sandbox is what you get for saying nothing.** `--allow-unsandboxed` is
/// the only thing that selects the host, and it selects it outright: there is no
/// input to this function that means "try the sandbox and settle for the host",
/// because that is the silent downgrade ADR-0019 §6 exists to prevent. If the
/// sandbox cannot run, the caller refuses and says why; it does not quietly
/// produce findings stamped with an isolation boundary that was not there.
///
/// Split out as a pure function so the rule is testable in a build with no
/// backend compiled in at all — the build where getting it wrong is least
/// likely to be noticed.
#[cfg(feature = "execution")]
fn select_backend(sandboxed: bool, allow_unsandboxed: bool) -> RunBackend {
    // clap's `conflicts_with` rejects both together before we are called; this
    // says so where the assumption is relied upon, rather than leaving the
    // precedence of an impossible combination to be inferred from the `if`.
    debug_assert!(
        !(sandboxed && allow_unsandboxed),
        "`--sandboxed` and `--allow-unsandboxed` are mutually exclusive at the parser"
    );
    if allow_unsandboxed {
        RunBackend::Subprocess
    } else {
        RunBackend::Sandboxed
    }
}

/// Run an analyzer against this worktree and file its findings.
///
/// Dispatches to the backend [`select_backend`] chose. Each arm either runs on
/// the backend that was asked for or fails naming what is missing; **no arm
/// substitutes the other backend**, so the `runner` and `isolation` recorded on
/// the stored layer are always the ones the user asked for.
#[cfg(feature = "execution")]
fn run_security_run(
    analyzer: &str,
    backend: RunBackend,
    json: bool,
    declared_image: Option<&str>,
) -> anyhow::Result<()> {
    match backend {
        RunBackend::Sandboxed => run_security_run_sandboxed(analyzer, json, declared_image),
        // A host run has no image, so `[security.images]` is not consulted and
        // not silently ignored either: there is nothing for it to name. The
        // recorded `isolation=none` is the whole of what that run is.
        RunBackend::Subprocess => run_security_run_on_this_host(analyzer, json),
    }
}

/// Execute the analyzer inside a digest-pinned OCI image in a microVM.
///
/// Everything refusable is refused before a guest boots: the hypervisor probe
/// and the pinned asset cache inside [`rto_exec::BoxliteRunner::new`], then the
/// image pull below. Each of those already carries the command that fixes it, so
/// this adds no wording of its own — a second, paraphrased copy of a message
/// that lives in `rto-exec` is a copy that goes stale.
#[cfg(all(feature = "execution", feature = "exec-boxlite"))]
fn run_security_run_sandboxed(
    analyzer: &str,
    json: bool,
    declared_image: Option<&str>,
) -> anyhow::Result<()> {
    use rto_exec::BoxliteRunner;

    let root = rto_exec::asset_root();
    let runner = BoxliteRunner::new(analyzer, &root, declared_image)?;
    let reference = runner.image().reference.clone();

    // Preflight the local image store. The backend checks this too, but only
    // after it has opened a runtime and started building a box — so without
    // this, the commonest first-run failure on a provisioned host arrives after
    // a visible pause, looking like something broke rather than something was
    // never fetched. `roteiro` never pulls during a run, so the answer cannot be
    // to fetch it here — and that is as true of an image the operator declared
    // as of one Roteiro pinned.
    if !rto_exec::boxlite::image_is_provisioned(analyzer, &root, declared_image)? {
        return Err(rto_exec::boxlite::SandboxError::ImageNotProvisioned {
            analyzer: analyzer.to_owned(),
            reference,
        }
        .into());
    }

    let invocation = runner.invocation();
    execute_and_file(&runner, analyzer, invocation, Some(&reference), json)
}

/// The same command in a build without `exec-boxlite`, which is every stock
/// build: a refusal that names the feature, the bootstrap, and the alternative.
///
/// Deliberately a runtime error rather than a `cfg` on the clap variant. Gating
/// the variant is how `roteiro model rm` shipped invisible to every crates.io
/// user — `unrecognized subcommand` for a command the documentation describes.
/// The flag parses everywhere; only the capability is conditional, and it says
/// so in a sentence.
///
/// It does **not** offer to run the analyzer on the host instead. Naming
/// `--allow-unsandboxed` as *an* option is honest — it is a different, weaker
/// thing the user may choose — but it is stated as a downgrade with its
/// consequence attached, not as a fallback this command would take on its own.
#[cfg(all(feature = "execution", not(feature = "exec-boxlite")))]
fn run_security_run_sandboxed(
    analyzer: &str,
    _json: bool,
    _declared_image: Option<&str>,
) -> anyhow::Result<()> {
    anyhow::bail!(
        "sandbox-unavailable: `roteiro security run {analyzer}` runs sandboxed by default, and \
         this build has no sandbox backend — `exec-boxlite` was not compiled in. It is not in the \
         default feature set, so a released binary and a plain `cargo build` both land here.\n  \
         To get one, in this order:\n    \
         1. roteiro security prefetch --analyzer sandbox --allow-download\n    \
         2. export BOXLITE_RUNTIME_URL=\"file://$HOME/.roteiro/security/boxlite-runtime/boxlite-runtime.tar.gz\"\n    \
         3. cargo install roteiro --features exec-boxlite     (or `cargo build --features exec-boxlite`)\n    \
         4. roteiro security prefetch --analyzer {analyzer} --allow-download   (pulls the pinned image; needs the binary from step 3)\n  \
         Step 4 needs a binary that already has the feature, which is why it comes last.\n  \
         Without rebuilding, the alternatives are to run the analyzer elsewhere and \
         `roteiro security ingest` its report, or to accept an unisolated run with \
         `--allow-unsandboxed` — that executes a third-party binary on this host with nothing \
         between it and your machine, and its findings are recorded isolation=none. Roteiro will \
         not make that substitution for you."
    );
}

/// Execute the analyzer as a child process on this host, with no isolation.
///
/// Reached only via `--allow-unsandboxed`. The flag is the consent, and it is a
/// separate, explicit act from asking for the run, because what is being
/// consented to — executing a third-party binary here with no boundary — is not
/// what "analyze my code" implies. That has not changed now that a sandbox
/// exists; if anything the flag says more, since there is now something better
/// to have chosen instead.
#[cfg(all(feature = "execution", feature = "exec-subprocess"))]
fn run_security_run_on_this_host(analyzer: &str, json: bool) -> anyhow::Result<()> {
    use rto_exec::SubprocessRunner;

    // `true` is the consent the caller already collected: this function is
    // reachable only from `RunBackend::Subprocess`, which only `--allow-unsandboxed`
    // selects.
    let runner = SubprocessRunner::new(analyzer, &rto_exec::asset_root(), true)?;
    let invocation = runner.invocation();
    execute_and_file(&runner, analyzer, invocation, None, json)
}

/// The same, in a `--no-default-features` build that dropped `exec-subprocess`.
#[cfg(all(feature = "execution", not(feature = "exec-subprocess")))]
fn run_security_run_on_this_host(analyzer: &str, _json: bool) -> anyhow::Result<()> {
    anyhow::bail!(
        "`roteiro security run {analyzer} --allow-unsandboxed` needs the `exec-subprocess` \
         feature, which this build does not have; rebuild with `--features exec-subprocess` (it \
         is in the default set, so this is a `--no-default-features` build). To get findings \
         without executing an analyzer here, run it elsewhere and use `roteiro security ingest` \
         — this build can still `prefetch` and `status` the assets."
    );
}

/// Run the request on `runner`, replace the analyzer's layer, and report.
///
/// The half of `security run` that is the same whichever backend produced the
/// result — and it is shared rather than duplicated for a reason beyond tidiness:
/// **every user-visible claim about isolation below is read back out of
/// `response.run`**, the record that was just written to the store. Nothing here
/// is told which backend ran, so nothing here can say something the stored
/// evidence does not.
///
/// `image_reference` is the exception, and only for the line printed *before* the
/// run, where there is no record yet to read.
///
/// Gated on **having a backend**, not on `execution`. Its two callers are
/// `run_security_run_sandboxed` (`exec-boxlite`) and
/// `run_security_run_on_this_host` (`exec-subprocess`), and an `execution`-only
/// build compiles neither — both are replaced there by refusal arms that bail
/// before anything runs. So in that build this function, `SecurityRunReport` and
/// `isolation_note` were genuinely dead and `-D warnings` said so (issue #445).
///
/// The `any` is written out rather than collapsed to `exec-subprocess`, which
/// today implies it: the union of the two call sites is what makes the gate
/// correct, and `crates/roteiro/Cargo.toml` records that `exec-boxlite`'s
/// implication of `exec-subprocess` is CLI plumbing "worth untangling" later. If
/// that happens, an `exec-boxlite`-only build still needs this and the `any`
/// still says so.
#[cfg(all(
    feature = "execution",
    any(feature = "exec-boxlite", feature = "exec-subprocess")
))]
fn execute_and_file(
    runner: &dyn rto_exec::AnalyzerRunner,
    analyzer: &str,
    invocation: rto_exec::Invocation,
    image_reference: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    use rto_exec::{AnalysisRequest, Consent, Worktree};

    let (repo, mut store, _cache) = open_graph()?;
    let worktree_path = repo
        .workdir()
        .unwrap_or_else(|| repo.git_dir())
        .to_path_buf();
    let request = AnalysisRequest {
        analyzer: analyzer.to_owned(),
        worktree: Worktree::read_only(&worktree_path)?,
        network: rto_graph::NetworkPolicy::Deny,
        consent: Consent::Granted,
        source: rto_graph::SourceIdentity {
            commit: repo.head_commit_id().ok(),
            tree: repo.head_tree_id().ok(),
            lockfile_blob: lockfile_blob(&repo),
        },
    };

    if !json {
        // Disclose what is about to run before it runs. A command that executes
        // a third-party binary should never leave the user guessing which one,
        // with which arguments, or where.
        let argv = format!("{} {}", invocation.program, invocation.args.join(" "));
        match image_reference {
            Some(reference) => eprintln!(
                "running (isolation {}, in {reference}): {argv}",
                runner.isolation().as_str()
            ),
            None => eprintln!(
                "running (isolation {}, on this host): {argv}",
                runner.isolation().as_str()
            ),
        }
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
            image_digest: response.run.image_digest,
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
    println!("  {}", isolation_note(&response.run));
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

/// One line describing the boundary a completed run actually had.
///
/// Derived from the [`rto_graph::AnalysisRun`] that was stored, never from which
/// function called it, so the sentence a user reads and the row a later
/// `security list` reads cannot disagree. A backend that reported the wrong
/// isolation would be caught by its own output rather than described in the
/// terms it was asked for.
///
/// Gated with [`execute_and_file`], its only caller — a build with no backend
/// files no run, so there is no stored record for this to read out.
#[cfg(all(
    feature = "execution",
    any(feature = "exec-boxlite", feature = "exec-subprocess")
))]
fn isolation_note(run: &rto_graph::AnalysisRun) -> String {
    match run.isolation {
        rto_graph::Isolation::None => "isolation none — the analyzer ran on this host. Its egress \
                                       was configured off and its inputs were pinned, but nothing \
                                       enforced that."
            .to_owned(),
        rto_graph::Isolation::MicroVm => format!(
            "isolation microvm — the analyzer ran inside a microVM, in image {}, with the \
             worktree mounted read-only and no network device.",
            run.image_digest.as_deref().unwrap_or("(undeclared)")
        ),
        // Not reachable from this command: `security run` executes something, so
        // it never files an ingested layer. Stated rather than `unreachable!`,
        // because a wrong sentence is a better failure here than a panic after a
        // scan has already been stored.
        rto_graph::Isolation::Ingested => {
            "isolation ingested — this layer records no local execution.".to_owned()
        }
    }
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

/// The three readings a lint count carries that a findings count does not.
///
/// One list, printed by the human report and serialised into the JSON one, so
/// the two cannot drift and a scripted consumer is told exactly what a person is
/// told. Each is a way the number moves **without the code changing**, and none
/// of them is a defect this command could detect on the user's behalf: with
/// nothing stored there is no history to diff, which is the point — ADR-0020 v1.1
/// makes these a *surprise* to be documented rather than a corruption of a
/// stored series.
///
/// Gated with the linter it annotates. `run_lint` and `print_lint_footnotes`,
/// the only two things that read this list, are
/// `all(execution, exec-subprocess)`; an `execution`-only build gets the
/// `run_lint` that refuses by name and never prints a count, so there is no
/// count for these readings to qualify (issue #445).
#[cfg(all(feature = "execution", feature = "exec-subprocess"))]
const LINT_CAVEATS: &[&str] = &[
    "a renamed lint reads as one defect fixed and one introduced — the compiler renamed a symbol, \
     the code did not change",
    "a removed lint reads as fixed — the compiler dropped the check, nobody fixed anything",
    "an edit to `[workspace.lints]`, or an added `#[allow]`, makes whole cohorts appear or vanish \
     — a configuration change reading as a code change",
    "a sandboxed run reports what the *image's* rustc said, and it is not this machine's — a lint \
     name is a symbol in a compiler, so a different compiler legitimately fires a different set. \
     `roteiro lint clippy` and `cargo clippy` in the same tree on the same day can disagree with \
     no defect on either side",
];

/// Resolve every layer into the decision `roteiro lint` runs under (ADR-0020 §6).
///
/// A thin adapter, not a second implementation: the layering rule lives in
/// `rto_exec::lint`, so the answer this produces and the answer
/// `roteiro config`'s `[lint]` section echoes come from the same code. What it
/// adds is the translation from two clap booleans to the three-state
/// [`rto_exec::LintRequested`] — a pair of bools has a fourth state the flags
/// cannot express, and turning it into an enum here is what keeps the impossible
/// combination out of the gate.
///
/// Split out as a pure function so all four rows of the ADR's table are testable
/// without running a linter, in a build that has no linter to run.
#[cfg(feature = "execution")]
fn lint_host_decision(
    cfg: &config::Loaded,
    sandboxed: bool,
    allow_unsandboxed: bool,
) -> rto_exec::LintDecision {
    // clap's `conflicts_with` rejects both together before we are called; this
    // says so where the assumption is relied upon rather than leaving the
    // precedence of an impossible combination to be inferred from the match.
    debug_assert!(
        !(sandboxed && allow_unsandboxed),
        "`--sandboxed` and `--allow-unsandboxed` are mutually exclusive at the parser"
    );
    let requested = match (allow_unsandboxed, sandboxed) {
        (true, _) => rto_exec::LintRequested::Host,
        (_, true) => rto_exec::LintRequested::Sandbox,
        _ => rto_exec::LintRequested::Unset,
    };
    rto_exec::decide_lint_host(
        rto_exec::LintConfigGrant::from_layers(
            cfg.project.lint.allow_unsandboxed,
            cfg.user.lint.allow_unsandboxed,
        ),
        requested,
    )
}

/// Resolve the two feature flags into the set the build will be resolved with.
///
/// Split out as a pure function because it decides one of the two axes — the
/// other being the toolchain — that move a lint count without the code moving,
/// and the report has to name whichever it picked.
///
/// # Errors
/// Returns an error when `--features` was given nothing to enable, which is
/// almost always a shell that ate the list rather than a request for the default
/// set.
#[cfg(feature = "execution")]
fn lint_features(
    all_features: bool,
    features: Option<&str>,
) -> anyhow::Result<rto_exec::FeatureSet> {
    if all_features {
        return Ok(rto_exec::FeatureSet::All);
    }
    let Some(list) = features else {
        return Ok(rto_exec::FeatureSet::Defaults);
    };
    let names: Vec<String> = list
        .split([',', ' '])
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .map(str::to_owned)
        .collect();
    if names.is_empty() {
        anyhow::bail!(
            "`--features {list:?}` names no features. Drop the flag to lint each crate's default \
             feature set, or pass `--all-features`."
        );
    }
    Ok(rto_exec::FeatureSet::Explicit(names))
}

/// The `--json` shape of `roteiro lint`.
///
/// It carries what a stored run would have carried on its `AnalysisRun` —
/// analyzer, version, toolchain, feature set, isolation, argv, window, exit
/// status — because **there is no `AnalysisRun` here** and an ephemeral report
/// still has to be honest about its inputs. It also carries `stored: false`,
/// which is not decoration: a consumer can assert from the payload alone that
/// this command wrote nothing, rather than having to trust the documentation.
// Gated on the *backend*, not just on `execution`: every field below is read out
// of an `rto_exec` type that needs `exec-subprocess`, so an `execution`-only
// build has no linter to report on and no types to report it with.
#[cfg(all(feature = "execution", feature = "exec-subprocess"))]
#[derive(serde::Serialize)]
struct LintReport {
    analyzer: String,
    analyzer_version: String,
    toolchain: rto_exec::Toolchain,
    /// The feature set the build was resolved with, as a label rather than a
    /// flag — `default` is a real answer and an empty string is not.
    features: String,
    isolation: rto_graph::Isolation,
    /// The exact argv, so the run is reproducible by hand.
    command: Vec<String>,
    /// The workspace root that was linted.
    worktree: String,
    /// The digest-pinned image the run happened inside, absent for a host run.
    ///
    /// What `isolation` is made of. A consumer that wants to know what executed
    /// this tree's build scripts can look the digest up; without it `"micro_vm"`
    /// is a claim with nothing behind it.
    #[serde(skip_serializing_if = "Option::is_none")]
    image: Option<String>,
    /// The `CARGO_TARGET_DIR` roteiro set for the run, outside the worktree.
    ///
    /// Present so `command` is genuinely reproducible by hand — the argv needs
    /// this variable to reproduce where the build wrote — and so that roteiro
    /// overriding a `CARGO_TARGET_DIR` the caller had set is something the run
    /// *says*, rather than something the caller discovers.
    scratch: String,
    started_at: String,
    ended_at: String,
    exit_status: i32,
    /// Whether the build completed. `false` with findings present means the
    /// diagnostics are what it managed to emit, not the whole picture.
    build_succeeded: bool,
    /// **Always `false`.** Nothing here reaches the findings store.
    stored: bool,
    counts: LintCounts,
    findings: Vec<LintFinding>,
    /// [`LINT_CAVEATS`], verbatim.
    caveats: Vec<String>,
}

/// What the linter's output contained besides reported findings.
///
/// Every field is something that did **not** become a finding, reported rather
/// than dropped silently: a run that quietly discarded half its diagnostics
/// would be indistinguishable from a clean tree.
// Gated on the *backend*, not just on `execution`: every field below is read out
// of an `rto_exec` type that needs `exec-subprocess`, so an `execution`-only
// build has no linter to report on and no types to report it with.
#[cfg(all(feature = "execution", feature = "exec-subprocess"))]
#[derive(serde::Serialize)]
struct LintCounts {
    reported: usize,
    /// Diagnostics about files outside this worktree — a dependency's own source.
    outside_worktree: usize,
    /// The same diagnostic emitted once per target by `--all-targets`.
    duplicates_collapsed: usize,
    /// Diagnostics with no location: rustc's own "aborting due to N errors".
    without_location: usize,
}

/// One diagnostic in the `--json` report.
///
/// Deliberately **not** the report's `identity` recipe: that vector orders and
/// deduplicates an ephemeral report and is never a stored key, and serialising
/// it would offer a consumer something that looks addressable and is not.
// Gated on the *backend*, not just on `execution`: every field below is read out
// of an `rto_exec` type that needs `exec-subprocess`, so an `execution`-only
// build has no linter to report on and no types to report it with.
#[cfg(all(feature = "execution", feature = "exec-subprocess"))]
#[derive(serde::Serialize)]
struct LintFinding {
    rule: String,
    severity: rto_graph::Severity,
    title: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    span: Option<rto_graph::Span>,
}

/// Run a linter over this worktree and print what it said.
///
/// The store is never opened. That is the strongest available form of "nothing
/// is written": there is no layer key to collide, no `replace_findings_layer`
/// call to make, and no handle through which a later edit could add one.
#[cfg(all(feature = "execution", feature = "exec-subprocess"))]
fn run_lint(
    analyzer: &str,
    features: &rto_exec::FeatureSet,
    decision: rto_exec::LintDecision,
    json: bool,
    image: Option<&str>,
) -> anyhow::Result<()> {
    let dir = std::env::current_dir()?;
    // A committed grant that was discarded is reported before anything else,
    // whichever way the decision went: a team that wrote `allow_unsandboxed =
    // true` into `roteiro.toml` is doing something reasonable and ineffective,
    // and silence would leave them believing it worked.
    if let Some(note) = decision.ignored_project_grant_note() {
        eprintln!("{note}");
    }
    // Disclose what is about to run before it runs, exactly as `security run`
    // does — and only once the grant is in hand, so a refused run never prints a
    // command line it did not execute.
    //
    // Host execution is now something a person opted into, per run or standing
    // (ADR-0020 §6), so this is no longer the *only* thing standing between the
    // user and a build on their machine. It is still worth printing: a grant
    // given once in a config file is not a memory of which argv it authorised,
    // and the isolation is named beside it because that is what was consented to.
    //
    // The argv comes from the same place the run will take it, and is `None` for
    // an analyzer this build does not drive: announcing clippy's command under
    // another analyzer's name would be a small lie printed in the one place a
    // user is relying on being told the truth. The refusal itself comes from the
    // run below, which owns that wording.
    if let (false, Some(invocation)) = (
        json,
        // The backend's own argv, not the host's: the two differ by `--offline`,
        // and a disclosure line that is one token away from what ran is worse
        // than none, because it is trusted.
        rto_exec::lint_invocation(analyzer, features, decision.backend()),
    ) {
        // Future tense, and the tense is the point. This used to say "running",
        // which was true while the only thing that could refuse after it was the
        // grant — and the grant was checked first, so nothing was ever announced
        // that did not then run. A sandboxed run has refusals of its own *after*
        // this line (no image, not provisioned, no hypervisor, a cache too cold),
        // so "running" would announce a build that never happened. Disclosure
        // still comes before anything executes, which is its whole job.
        eprintln!(
            "{analyzer} will run {}: {} {}",
            decision.reason.explanation(),
            invocation.program,
            invocation.args.join(" ")
        );
    }
    let outcome = rto_exec::run_lint(analyzer, &dir, features, decision, image)?;

    let findings: Vec<LintFinding> = outcome
        .report
        .findings
        .iter()
        .map(|f| LintFinding {
            rule: f.rule.clone(),
            severity: f.severity.clone(),
            title: f.title.clone(),
            message: f.message.clone(),
            path: f.path.clone(),
            line: f.meta.get("line").and_then(serde_json::Value::as_u64),
            span: f.span,
        })
        .collect();
    let counts = LintCounts {
        reported: findings.len(),
        outside_worktree: outcome.summary.outside_worktree,
        duplicates_collapsed: outcome.summary.duplicates_collapsed,
        without_location: outcome.summary.without_location,
    };

    if json {
        emit_json(&LintReport {
            analyzer: outcome.analyzer.to_owned(),
            analyzer_version: outcome.report.analyzer_version.clone(),
            toolchain: outcome.toolchain.clone(),
            features: outcome.features.label(),
            isolation: outcome.isolation,
            command: outcome.command.clone(),
            worktree: outcome.worktree.display().to_string(),
            image: outcome.image.clone(),
            scratch: outcome.scratch.display().to_string(),
            started_at: outcome.report.started_at.clone(),
            ended_at: outcome.report.ended_at.clone(),
            exit_status: outcome.report.exit_status,
            build_succeeded: outcome.summary.build_succeeded,
            // Not computed from anything: this command has no code path that
            // could write, so the field is the contract rather than a result.
            stored: false,
            counts,
            findings,
            caveats: LINT_CAVEATS.iter().map(|c| (*c).to_owned()).collect(),
        })?;
        return Ok(());
    }
    print_lint_report(&outcome, &findings, &counts);
    Ok(())
}

/// Render a lint report for a person.
#[cfg(all(feature = "execution", feature = "exec-subprocess"))]
fn print_lint_report(
    outcome: &rto_exec::LintOutcome,
    findings: &[LintFinding],
    counts: &LintCounts,
) {
    println!(
        "{} {} — {} diagnostic(s), nothing stored",
        outcome.analyzer, outcome.report.analyzer_version, counts.reported
    );
    println!(
        "  toolchain {} on {}",
        outcome.toolchain.rustc, outcome.toolchain.host
    );
    println!("  features  {}", outcome.features.label());
    // Stated, not hidden. Roteiro sets `CARGO_TARGET_DIR` for this run rather
    // than inheriting whatever the shell had, because "the build writes nothing
    // into the tree you are linting" has to be this command's property and not
    // the caller's — but a variable overridden in silence is its own surprise,
    // so the run names where it went.
    println!(
        "  build dir {} — set by roteiro, outside the worktree; the tree is not written to",
        outcome.scratch.display()
    );
    match &outcome.image {
        Some(image) => {
            println!(
                "  isolation {} — the linter compiled this tree inside a microVM with no network \
                 interface; its build scripts and proc macros ran there, not here",
                outcome.isolation.as_str()
            );
            println!("  image     {image}");
            // Named where a user meets it rather than only in the docs. The
            // guest's rustc decides which lints fire, and it is not this
            // machine's — so a count from here and a count from `cargo clippy`
            // may legitimately differ. Nothing is stored, so this is a surprise
            // rather than a corruption (ADR-0020 v1.1), but a surprise is still
            // something a person should be told before they go looking for the
            // bug that is not there.
            println!(
                "  the toolchain above is the image's, not this machine's — which lints fire is \
                 decided by that rustc, so this count and a local `cargo clippy` can differ \
                 legitimately"
            );
        }
        None => println!(
            "  isolation {} — the linter compiled this tree on this host, so its build scripts \
             and proc macros ran here too",
            outcome.isolation.as_str()
        ),
    }
    for finding in findings {
        let at = match (&finding.path, finding.line) {
            (Some(path), Some(line)) => format!("{path}:{line}"),
            (Some(path), None) => path.clone(),
            _ => "-".to_owned(),
        };
        println!(
            "  {:<8} {:<34} {at}  {}",
            finding.severity.as_str(),
            finding.rule,
            finding.title
        );
    }
    print_lint_footnotes(outcome, counts);
}

/// The counts of what was *not* reported, and the readings the number carries.
#[cfg(all(feature = "execution", feature = "exec-subprocess"))]
fn print_lint_footnotes(outcome: &rto_exec::LintOutcome, counts: &LintCounts) {
    for (count, what) in [
        (
            counts.outside_worktree,
            "not reported: about a file outside this worktree (a dependency's own source)",
        ),
        (
            counts.duplicates_collapsed,
            "counted once: the same diagnostic arrived again for another target",
        ),
        (
            counts.without_location,
            "not reported: no location — rustc's own \"aborting due to …\" summaries",
        ),
    ] {
        if count > 0 {
            println!("  {count} diagnostic(s) {what}");
        }
    }
    if !outcome.summary.build_succeeded {
        println!(
            "  the build did not complete — these diagnostics are what it managed to emit, not \
             the whole picture"
        );
    }
    println!();
    println!("read this as a point in time, not a trend:");
    for caveat in LINT_CAVEATS {
        println!("  - {caveat}");
    }
    println!(
        "nothing was written: `roteiro security list` and `roteiro export` are unchanged by this \
         command, and there is no lint history to compare against."
    );
}

/// The same command in a build without `exec-subprocess`: a refusal that names
/// the feature and the alternative.
///
/// A runtime error rather than a `cfg` on the clap variant, for the reason
/// `security run` records — gating the variant is how a documented command
/// shipped invisible to crates.io users as `unrecognized subcommand`.
#[cfg(all(feature = "execution", not(feature = "exec-subprocess")))]
fn run_lint(
    analyzer: &str,
    _features: &rto_exec::FeatureSet,
    _decision: rto_exec::LintDecision,
    _json: bool,
    _image: Option<&str>,
) -> anyhow::Result<()> {
    anyhow::bail!(
        "`roteiro lint {analyzer}` needs the `exec-subprocess` feature, which this build does not \
         have; rebuild with `--features exec-subprocess` (it is in the default set, so this is a \
         `--no-default-features` build). There is no ingest path to fall back on: a lint is \
         reported and never stored, so there is no artifact for another machine to hand over."
    );
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
    if specs.is_empty() && rto_exec::LINT_ANALYZERS.contains(&name) {
        // A linter has **no pinned assets at all**, and an empty list is the
        // right answer rather than an error. Its rule set is the toolchain, and
        // the toolchain arrives in an image the user supplies (ADR-0020
        // conditions 1-2) — so there is nothing here to digest, and the one
        // thing `prefetch --analyzer clippy` does have to do is pull that image,
        // which happens further down against `[lint] image` rather than against
        // this table. Bailing here made the natural command for provisioning a
        // linter fail with "no assets", which is true and useless.
        return Ok(Vec::new());
    }
    if specs.is_empty() {
        anyhow::bail!(
            "no assets for `{name}` in this build (analyzers: {}; linters: {}; shared: {})",
            rto_exec::known_analyzers().join(", "),
            rto_exec::LINT_ANALYZERS.join(", "),
            rto_exec::SANDBOX
        );
    }
    Ok(specs)
}

/// Pull every sandbox image this configuration would run, reporting failures.
///
/// Lifted out of [`run_security_prefetch`] because it grew a second source and
/// with it a fallible resolution step — but the split earns its place beyond the
/// line count: this is the **only** function in the CLI that can pull an image,
/// so what it discloses before opening a socket is worth reading on its own.
///
/// Returns what went wrong rather than short-circuiting, for the reason the asset
/// loop above does the same: one unobtainable image must not hide the others, and
/// a `prefetch` that stopped at the first failure would have to be run once per
/// problem to find out how many there were.
#[cfg(all(feature = "execution", feature = "exec-boxlite"))]
fn pull_sandbox_images(
    root: &std::path::Path,
    analyzer: Option<&str>,
    json: bool,
    security: &config::SecurityConfig,
) -> Vec<String> {
    // The **inventory**, not the table: since issue #434 the set of images an
    // analyzer run can use is the built-in pins composed with
    // `[security.images]`, and iterating the table alone would leave a declared
    // image obtainable by no command at all. Resolution refuses a tag, an
    // analyzer with no adapter, or an entry naming nothing — here rather than at
    // a run, which is the point of validating the whole map instead of only the
    // entries somebody happens to provision.
    let inventory = match rto_exec::boxlite::image_inventory(&security.declared()) {
        Ok(inventory) => inventory,
        Err(e) => return vec![format!("{e}")],
    };

    let mut failures = Vec::new();
    for image in inventory {
        if analyzer.is_some_and(|name| name != image.analyzer) {
            continue;
        }
        if !json {
            // Named before a socket is opened, and labelled with who chose it. A
            // declared reference can come from a committed `roteiro.toml`, so it
            // is the one image a teammate may have picked for you; printing
            // whose choice it was is what turns that into a thing you saw.
            eprintln!(
                "pulling sandbox image for {} [{}] ({})",
                image.analyzer,
                image.source.as_str(),
                image.reference
            );
        }
        let declared = security.image_for(&image.analyzer);
        if let Err(e) = rto_exec::boxlite::provision_image(&image.analyzer, root, declared) {
            failures.push(format!("{e}"));
        }
    }
    failures
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
    lint_image: Option<&str>,
    security: &config::SecurityConfig,
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
        failures.extend(pull_sandbox_images(&root, analyzer, json, security));
    }

    // The sandboxed linter's image, which is **supplied rather than pinned**
    // (ADR-0020 conditions 1-2). It is pulled here rather than by a run for the
    // rule that governs every other input: provisioning fetches, running reads.
    //
    // Named before a socket is opened, like every download above, and for a
    // sharper reason than the others: this reference can come from a committed
    // `roteiro.toml`, so it is the one asset a teammate may have chosen for you.
    // Printing it is what turns that into a thing you saw.
    #[cfg(feature = "exec-boxlite")]
    if allow_download
        && let Some(reference) = lint_image
        && analyzer.is_none_or(|name| rto_exec::LINT_ANALYZERS.contains(&name))
    {
        if !json {
            eprintln!("pulling the sandboxed linter's image, from `[lint] image`: {reference}");
        }
        if let Err(e) = rto_exec::boxlite::pull_reference("`[lint] image`", reference, &root) {
            failures.push(format!("{e}"));
        }
    }
    #[cfg(not(feature = "exec-boxlite"))]
    let _ = (lint_image, security);

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
///
/// The two row types come from [`rto_exec::tool_security`] rather than being
/// declared here, and they used to be declared here. They moved when `security
/// list`/`status` reached the model-facing tool surfaces (issue #435):
/// `host_readiness` and `possibly_stale` are judgements about evidence — "this host
/// could actually run it", "an advisory database is involved, so never call this
/// current" — and three copies of a judgement is three chances for one of them to
/// say something weaker. Issue #321 is what that costs.
///
/// `analyzers[].ready: bool` is **gone**, replaced by `host_readiness` plus the two
/// facts it summarises (`assets_provisioned`, `missing_programs`). That is a
/// breaking change to this command's `--json` and it is the point of issue #464: the
/// boolean was named for running and computed from provisioning, so a consumer
/// reading it could not have been right.
#[cfg(feature = "execution")]
#[derive(serde::Serialize)]
struct SecurityStatusReport {
    root: String,
    analyzers: Vec<rto_exec::AnalyzerCoverage>,
    /// The sandbox images this configuration would run, each labelled with who
    /// chose it and whether it is in the local store.
    ///
    /// Added alongside `analyzers` rather than inside it, and that is not
    /// squeamishness about a breaking change. `AnalyzerCoverage` is shared with
    /// both model-facing tool surfaces and answers *"could this host run it"*
    /// from the asset cache and `PATH`; which container it would run in is a
    /// different question with a different remedy, and issue #464 is what it
    /// costs to answer two questions in one field.
    ///
    /// Absent in a build with no sandbox backend, where there is no such thing
    /// as an image to report.
    #[cfg(feature = "exec-boxlite")]
    images: Vec<SandboxImageStatus>,
    assets: Vec<rto_exec::AssetStatus>,
    layers: Vec<rto_exec::LayerStaleness>,
}

/// One sandbox image as `security status` reports it.
///
/// `source` is the field issue #434 turns on. A user who cannot tell a built-in
/// pin from an image named in a committed `roteiro.toml` has lost exactly what
/// the pin was for — Roteiro checked that a built-in image is published,
/// digest-addressable and of a knowable version, and it checked none of that
/// about a reference it was handed.
#[cfg(all(feature = "execution", feature = "exec-boxlite"))]
#[derive(serde::Serialize)]
struct SandboxImageStatus {
    #[serde(flatten)]
    image: rto_exec::boxlite::ResolvedImage,
    /// Whether it is already in the local store. `false` means a run refuses —
    /// nothing pulls implicitly.
    provisioned: bool,
}

/// Resolve every sandbox image and ask the local store about each one.
///
/// Its own function so the `--json` and human arms report from one computation:
/// a status blob and a status listing disagreeing about which images are present
/// is the shape of defect `tool_security` exists to prevent.
///
/// # Errors
/// The first [`rto_exec::boxlite::resolve_image`] refusal — a tag, an analyzer
/// with no adapter, an entry naming nothing. A status that skipped a broken key
/// and printed the rest would be the quiet answer this command exists to
/// replace.
#[cfg(all(feature = "execution", feature = "exec-boxlite"))]
fn sandbox_image_status(
    root: &std::path::Path,
    analyzer: Option<&str>,
    security: &config::SecurityConfig,
) -> anyhow::Result<Vec<SandboxImageStatus>> {
    rto_exec::boxlite::image_inventory(&security.declared())?
        .into_iter()
        .filter(|image| analyzer.is_none_or(|name| name == image.analyzer))
        .map(|image| {
            let declared = security.image_for(&image.analyzer);
            // "Could not tell" is not "absent" — `image_is_provisioned` errors
            // rather than answering `false` on a store it cannot read, and that
            // distinction is preserved here rather than flattened into a column.
            let provisioned =
                rto_exec::boxlite::image_is_provisioned(&image.analyzer, root, declared)?;
            Ok(SandboxImageStatus { image, provisioned })
        })
        .collect()
}

/// One analyzer's readiness as a display column.
///
/// `binary not found` carries the program name because that is the actionable part
/// and Roteiro is not going to install it for you. The other two states are
/// self-describing.
///
/// Lifted out of [`run_security_status`] so the arm that names a program and the
/// arm that names a `prefetch` are read side by side, rather than one being a
/// formatted string inside a `println!` argument list.
#[cfg(feature = "execution")]
fn analyzer_state_line(coverage: &rto_exec::AnalyzerCoverage) -> String {
    match coverage.host_readiness {
        rto_exec::Readiness::BinaryNotFound => {
            format!("binary not found: {}", coverage.missing_programs.join(", "))
        }
        state => state.as_str().to_owned(),
    }
}

/// Report what is provisioned, what it covers, and how old the advisory data
/// behind each live layer is.
///
// One arm per reported section — assets, analyzers, layers — plus their `--json`
// shapes. Splitting it would scatter one screen of output across three
// functions that only ever run together.
#[allow(clippy::too_many_lines)]
#[cfg(feature = "execution")]
fn run_security_status(
    analyzer: Option<&str>,
    json: bool,
    security: &config::SecurityConfig,
) -> anyhow::Result<()> {
    let root = rto_exec::asset_root();
    let assets = rto_exec::status(&root, analyzer);

    let analyzers = rto_exec::coverage_matrix(&root, analyzer);
    #[cfg(feature = "exec-boxlite")]
    let images = sandbox_image_status(&root, analyzer, security)?;
    #[cfg(not(feature = "exec-boxlite"))]
    let _ = security;

    // Staleness comes from the *runs*, because the advisory database's
    // publication date is something the analyzer reported, not something
    // provisioning could know. `layer_staleness` is that rule, shared with the two
    // tool surfaces so `possibly_stale` has one definition.
    let now = rto_exec::rfc3339_utc(std::time::SystemTime::now());
    let stored = open_graph()
        .and_then(|(_repo, store, _cache)| Ok(store.findings_layers(analyzer)?))
        .unwrap_or_default();
    let layers = rto_exec::layer_staleness(&stored, &now);

    if json {
        emit_json(&SecurityStatusReport {
            root: root.display().to_string(),
            analyzers,
            #[cfg(feature = "exec-boxlite")]
            images,
            assets,
            layers,
        })?;
        return Ok(());
    }

    println!("asset cache: {}", root.display());
    println!("\nanalyzers");
    for coverage in &analyzers {
        // Three states, because the remedy differs (issue #464). `ready` used to be
        // printed on the strength of the assets alone, which is true about
        // provisioning and reads as a claim about running.
        println!(
            "  {:<12} {:<31}  [{}]",
            coverage.analyzer,
            analyzer_state_line(coverage),
            coverage.languages.join(", ")
        );
        println!("               {}", coverage.summary);
        // The remedy, named where the state is, and named only for the one Roteiro
        // performs. `binary not found` says which program is absent and stops
        // there: Roteiro does not install analyzers (ADR-0014), and an install
        // command it has not verified on this host would be a way forward that does
        // not lead anywhere — see issue #430, which is the work of establishing
        // those commands.
        if !coverage.missing_programs.is_empty() {
            println!(
                "               not on PATH: {} — Roteiro does not install analyzers; \
                 install it yourself, or produce the report elsewhere and use `roteiro \
                 security ingest`",
                coverage.missing_programs.join(", ")
            );
        } else if !coverage.assets_provisioned {
            println!("               run `roteiro security prefetch` to provision its assets");
        }
    }

    #[cfg(feature = "exec-boxlite")]
    print_sandbox_images(&images);

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

/// Print the sandbox images, saying of each one **who chose it**.
///
/// Its own section rather than a column on the analyzer rows, because it answers
/// a different question with a different remedy: an analyzer row says whether
/// this host could run the tool, and this says which container it would run it
/// in and whether that container is here yet.
///
/// The `source` column is the whole reason issue #434 asked for this. Roteiro
/// vouches for a built-in pin — published, digest-addressable, of a knowable
/// version — and vouches for none of that about a reference it was handed, so a
/// reader who cannot tell them apart has lost the thing the pin was for. The
/// version line says so out loud rather than leaving a blank column to be read
/// as an omission.
#[cfg(all(feature = "execution", feature = "exec-boxlite"))]
fn print_sandbox_images(images: &[SandboxImageStatus]) {
    println!("\nsandbox images  (ADR-0014 — a run never pulls; `prefetch` obtains)");
    if images.is_empty() {
        println!(
            "  none — this build pins no image and `[security.images]` declares none, so \
             `security run` has no sandboxed path. See docs/SANDBOXED_LINTING.md."
        );
        return;
    }
    for row in images {
        println!(
            "  {:<12} {:<41}  {}",
            row.image.analyzer,
            row.image.source.as_str(),
            if row.provisioned {
                "in the local store"
            } else {
                "NOT PROVISIONED — a run refuses"
            }
        );
        println!("               {}", row.image.reference);
        match &row.image.analyzer_version {
            Some(version) => println!(
                "               analyzer version {version}, which Roteiro checked this image \
                 carries"
            ),
            // Stated rather than left blank. A missing column reads as an
            // omission; this is a consequence of choosing your own image and the
            // reader should meet it here rather than in a findings record.
            None => println!(
                "               analyzer version is not asserted for an image Roteiro did not \
                 choose — the run records what the analyzer says about itself"
            ),
        }
        if !row.provisioned {
            println!(
                "               obtain it: roteiro security prefetch --analyzer {} \
                 --allow-download",
                row.image.analyzer
            );
        }
    }
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
/// (roots scanned + explicit repos), deduplicated by [`repo_path_identity`] so a
/// repo named in two groups is hosted once.
///
/// Not feature-gated: `roteiro config` resolves each workspace's membership
/// through this same function (issue #499), so what `config` *reports* and what
/// `serve` *hosts* are the one computation and cannot drift.
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
        if seen.insert(repo_path_identity(&p)) {
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
    // The ceiling a request's context may grow to, not the window every request
    // gets (issue #486). This argument was the literal `0` — which fell through
    // to a hardcoded 4,096 that no configuration key could reach, on every model
    // from one trained at 262,144 tokens to one trained at 512. Unset ⇒ 0 ⇒ each
    // model's own trained window; a value bounds every served model at once.
    let max_context_tokens = cfg.serve.max_context_tokens.unwrap_or(0);
    let engine =
        rto_serve::llama::LlamaEngine::new_with_budget(served, max_context_tokens, budget_bytes)
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
///
/// # This surface and the MCP one carry the same tools
///
/// `rto_render::mcp` declares its own schemas — the `rmcp` macro generates them
/// statically from the argument structs — but every tool that exists on one
/// surface exists on the other, and the shared ones delegate to the same
/// function rather than re-deriving an answer. `check` and `context` were added
/// to both in one change for that reason.
///
/// # `roteiro security` is not on either tool surface
///
/// `security ingest` and `security run` both reach
/// [`rto_graph::Store::replace_findings_layer`] — `run_security_ingest`
/// directly, and `security run` through `execute_and_file`, which **both** of
/// its backends share since ADR-0019 (PR #407). So sandboxing does not make
/// `run` read-only: it writes whichever backend `select_backend` picks. Neither
/// may ever appear here.
///
/// `security run` additionally *executes an analyzer*. It now defaults to a
/// microVM, which makes it safer for the person who typed it and changes nothing
/// about this: a model asking for a tool is not a human consenting to execution,
/// and `--allow-unsandboxed` is a gate that exists to be typed by a person
/// (ADR-0019 §6 refuses even a silent downgrade from sandbox to host).
/// `security prefetch` opens the network under an explicit consent and writes the
/// asset cache. All three are permanent refusals, not gaps.
///
/// `security list` and `security status` are read-only and are here, as
/// `security_list` and `security_status`, behind `all(serve, execution)` (issue
/// #435). Neither is the CLI's `--json` served through: both go through
/// [`rto_exec::tool_security`], which adds the two things a CLI does not need.
/// First, a `coverage` discriminator, because `security list --json` is
/// `{"layers": [], "findings": 0}` on a repository nobody has analyzed and
/// `findings: 0` is also what a clean run reports — so the never-run case carries
/// no `report` at all rather than an empty one. Second, `security_status` returns
/// two labelled scopes, `machine` and `repository`: its asset half is
/// machine-global (`rto_exec::asset_root`) and its layer half follows the selected
/// project, and on a tool surface that asymmetry is invisible unless the document
/// says it.
///
/// The gating is load-bearing rather than incidental. These two are on
/// `execution`, and the MCP pair is on the same feature via
/// `rto-render/execution`, so both surfaces gain and lose them together —
/// `both_tool_surfaces_offer_the_same_tools` below cannot be satisfied by adding
/// them to one.
///
/// # Why there is no `review` tool, here or on MCP
///
/// `roteiro review` is the third thing the review surface computes and is
/// deliberately absent from both. Two reasons, and the second is the substantive
/// one.
///
/// **Size.** `review --base HEAD~3` on this repository emits 435,208 bytes of
/// JSON — roughly 109k tokens, most of a context window for one call — and unlike
/// a ranking there is no `limit` that would make it a page: a review is *the whole
/// change*, and a truncated one is a review that quietly skipped part of the diff.
///
/// **A different debt number.** `review`'s per-file output carries `debt`. The
/// MCP crate cannot reach the target project's `roteiro.toml`, so it could not
/// apply that project's `[debt] ignore` — and issue #321 is exactly the defect of
/// one concept reporting different numbers on different surfaces, which had
/// already recurred across three of them before it was fixed, then once more on a
/// fourth (`_Home`, issue #372) and a fifth (`review` itself, issue #409). Adding
/// a surface that reports a different figure again, for convenience, is how that
/// recurs. The review surface stays CLI-first (`roteiro review [--json]`): it
/// needs no server and works in any agent or CI.
///
/// Note that `roteiro review` **does** apply `[debt] ignore` as of issue #409:
/// `Command::Review` is handed the list, and [`review::build`] reports only the
/// markers [`rto_graph::debt`] retains under it. So the gap above is this
/// registry's, and exposing `review` here would mean *introducing* a divergence
/// the CLI no longer has — not inheriting one it still does.
#[cfg(feature = "serve")]
struct GraphToolRegistry {
    workspace: std::sync::Arc<rto_graph::Workspace>,
    /// The pinned-asset cache this registry reports on and, for `sandbox_clear`,
    /// deletes from — **held rather than resolved at the call**.
    ///
    /// The same field, for the same reason, as `rto_render::mcp`'s `GraphServer`:
    /// `rto_exec::asset_root()` resolves from the process environment, and
    /// `unsafe_code = "forbid"` means a test cannot redirect it — so a test that
    /// reaches the one mutating tool reaches the developer's own cache. That is
    /// not hypothetical; fault-injecting the refusal, to prove the test catches
    /// its absence, cleared 8.7 GB on the machine this was written on.
    #[cfg(feature = "execution")]
    asset_root: std::path::PathBuf,
}

#[cfg(feature = "serve")]
impl GraphToolRegistry {
    fn new(workspace: std::sync::Arc<rto_graph::Workspace>) -> Self {
        Self {
            workspace,
            #[cfg(feature = "execution")]
            asset_root: rto_exec::asset_root(),
        }
    }

    /// Point this registry's asset-cache reads and removals at `root`.
    ///
    /// Test-only, and it is what lets a test exercise the one tool here that
    /// deletes without deleting the developer's cache.
    #[cfg(all(test, feature = "execution"))]
    fn with_asset_root(mut self, root: std::path::PathBuf) -> Self {
        self.asset_root = root;
        self
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

    /// The `security_list` call. Lifted out of [`GraphToolRegistry::call`] to keep
    /// that dispatcher readable, as its `debt_density` argument parsing already is.
    ///
    /// # Errors
    /// An unknown `analyzer`, or the store/serialise errors [`Self::run`] flattens.
    #[cfg(feature = "execution")]
    fn security_list(
        &self,
        project: Option<&str>,
        args: &serde_json::Value,
    ) -> Result<String, String> {
        let analyzer = security_analyzer_arg(args)?;
        // `limit` is model-controlled: clamped to the advertised bound by
        // `model_limit`, floor included. It bounds findings PER LAYER, so a small
        // page still reaches every analyzer's layer — a document-wide bound would
        // spend itself on the first layer and report the rest as empty, which reads
        // as "that analyzer found nothing".
        let limit = model_limit(args, 20, 100);
        self.run(project, move |store| {
            store
                .findings_layers(analyzer.as_deref())
                .map(|layers| rto_exec::security_list(layers, limit))
        })
    }

    /// The `security_status` call, in two labelled scopes. Lifted out of
    /// [`GraphToolRegistry::call`] alongside [`Self::security_list`].
    ///
    /// # Errors
    /// An unknown `analyzer`, an unknown or ambiguous `project`, or the
    /// store/serialise errors [`Self::run`] flattens.
    #[cfg(feature = "execution")]
    fn security_status(
        &self,
        project: Option<&str>,
        args: &serde_json::Value,
    ) -> Result<String, String> {
        let analyzer = security_analyzer_arg(args)?;
        // The **resolved** project name, so the document's `repository` half names
        // the graph it describes rather than echoing an argument a bare call
        // omitted. Resolved before any machine-global work is reported, so the two
        // halves never come from different questions.
        let project_name = self.workspace.resolve(project).map_err(|e| e.to_string())?;
        // Machine-global, and read outside `run` because it is not the project's to
        // answer: the asset root describes this host whichever project was
        // selected. The document's two `scope` fields are what say so to the model.
        let root = self.asset_root.clone();
        let now = rto_exec::rfc3339_utc(std::time::SystemTime::now());
        self.run(project, move |store| {
            store.findings_layers(analyzer.as_deref()).map(|layers| {
                rto_exec::security_status(&root, analyzer.as_deref(), &project_name, &layers, &now)
            })
        })
    }

    /// The `sandbox_status` call: the machine-global image store, no project.
    ///
    /// Not routed through [`Self::run`], and that is the point rather than an
    /// omission: there is no project in this question. One sandbox store exists
    /// per asset root and every hosted project shares it, so resolving a project
    /// here would attach a selector to an answer it cannot change.
    ///
    /// # Errors
    /// The store errors [`rto_exec::StoreError`] carries, or a serialisation
    /// failure.
    #[cfg(feature = "execution")]
    fn sandbox_status(&self) -> Result<String, String> {
        let report = rto_exec::sandbox_status(&self.asset_root).map_err(|e| e.to_string())?;
        serde_json::to_string(&report).map_err(|e| e.to_string())
    }

    /// The `sandbox_clear` call — the one tool on either surface that mutates.
    ///
    /// # Errors
    /// A scope that names neither selector or both, the store errors
    /// [`rto_exec::StoreError`] carries, or a serialisation failure.
    #[cfg(feature = "execution")]
    fn sandbox_clear(&self, args: &serde_json::Value) -> Result<String, String> {
        let image = args
            .get("image")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let everything = args
            .get("everything")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        // Neither argument defaults to the other, and the absence of both is not a
        // request. ADR-0014 v1.6: "clear this image" and "clear everything" are
        // different requests, so silence must not resolve to the destructive one.
        // The MCP surface refuses the same two shapes in the same two ways —
        // `sandbox_clear_refuses_a_scope_it_was_not_given` on each.
        let scope = match (image, everything) {
            (Some(_), true) => {
                return Err(
                    "`image` and `everything` are different requests; pass exactly one.".to_owned(),
                );
            }
            (None, true) => rto_exec::Scope::Everything,
            (Some(reference), false) => rto_exec::Scope::Image(reference),
            (None, false) => {
                return Err(
                    "nothing was named to drop. Pass `image` with a reference from \
                            `sandbox_status`, or `everything: true`. Supplying neither does \
                            not mean everything."
                        .to_owned(),
                );
            }
        };
        let report = if args
            .get("dry_run")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            rto_exec::sandbox_plan(&self.asset_root, &scope).map(|(report, _doomed)| report)
        } else {
            rto_exec::sandbox_clear(&self.asset_root, &scope)
        }
        .map_err(|e| e.to_string())?;
        serde_json::to_string(&report).map_err(|e| e.to_string())
    }
}

/// The served-chat `sandbox_status` tool definition — the mirror of the MCP one
/// in `rto_render::mcp`, which declares its own schema.
///
/// **No `project`.** Every other tool here takes one; this store is machine-global
/// and has no per-repository half for a selector to choose between, so offering
/// the argument would imply an answer that changes with it.
#[cfg(all(feature = "serve", feature = "execution"))]
fn sandbox_status_tool_def() -> rto_serve::ToolDef {
    use serde_json::json;
    rto_serve::ToolDef {
        name: "sandbox_status".to_owned(),
        description: "Report what the SANDBOX IMAGE STORE on THIS MACHINE is holding: one \
                      row per cached container image, with its reference, its digests, how \
                      many of its objects are on disk, and its size broken down into \
                      layers, extracted trees, the derived ext4 disk image and the guest \
                      base. \
                      MACHINE-GLOBAL, and `scope` says so. There is one of these per asset \
                      root and EVERY repository this server hosts shares it, so never \
                      attribute a size here to the project you are discussing. It takes no \
                      `project` argument because it has no per-repository half. \
                      `bytes.total` is what an image references; `bytes.exclusive` is what \
                      dropping THAT IMAGE ALONE would free — they differ when another \
                      cached image shares a layer, so quote `exclusive` when you say what \
                      clearing one image gives back. `objects` counts the PULLED content \
                      (manifest, config, one per distinct layer); the extracted trees and \
                      disk images are built on first run, so a pulled-but-never-run image \
                      is complete without them. `unattributed` is bytes no image claims; \
                      `preserved` is state no pinned digest re-obtains, which \
                      `sandbox_clear` never removes. \
                      Read this BEFORE `sandbox_clear` and show the user the numbers. \
                      Every `reference` here is a value `sandbox_clear` takes as `image`. \
                      Read-only."
            .to_owned(),
        parameters: json!({ "type": "object", "properties": {} }),
    }
}

/// The served-chat `sandbox_clear` tool definition — the mirror of the MCP one.
///
/// The **only** definition on either surface that describes a tool which changes
/// something, so its description carries the three things a model has to do with
/// it: look first, pass exactly one scope, and quote what came back.
#[cfg(all(feature = "serve", feature = "execution"))]
fn sandbox_clear_tool_def() -> rto_serve::ToolDef {
    use serde_json::json;
    rto_serve::ToolDef {
        name: "sandbox_clear".to_owned(),
        description: "DELETE cached container images from the SANDBOX IMAGE STORE on THIS \
                      MACHINE, and report what that freed. This is the ONE tool here that \
                      changes anything, and what makes it admissible is also its limit: \
                      everything it drops is re-obtainable from a pinned digest, so it \
                      costs a re-download and NEVER information. It cannot reach a findings \
                      layer, a memory record or the graph. \
                      MACHINE-GLOBAL. One store per asset root, shared by EVERY repository \
                      this server hosts, so clearing on behalf of one project slows the \
                      next sandboxed run for all of them. No `project` argument; `scope` in \
                      the result says `machine`. \
                      TELL THE USER FIRST. Call `sandbox_status` and show them what is \
                      cached and what it costs; a re-pull is minutes and gigabytes. \
                      `image` and `everything` are DIFFERENT REQUESTS and neither has a \
                      default: pass `image` with a reference from `sandbox_status`, or \
                      `everything: true`. Supplying neither is an ERROR and does not mean \
                      everything; supplying both is an error too. `dry_run: true` reports \
                      what would go and removes nothing — `applied` says which happened. \
                      REPORT WHAT IT FREED: `freed_bytes`, with `store_bytes_before` and \
                      `store_bytes_after` measured either side. Quote a figure rather than \
                      saying it worked. \
                      `retained` is every surviving image re-checked against the disk AFTER \
                      the deletion. If any `complete` is false, SAY SO PROMINENTLY — that \
                      is a damaged store, not a successful clear, and `roteiro security \
                      prefetch` is the repair. \
                      It refuses rather than guessing: a registered box, an unrecognised \
                      entry under the store root, or an index row pointing outside it all \
                      stop it with nothing removed."
            .to_owned(),
        parameters: json!({
            "type": "object",
            "properties": {
                "image": {
                    "type": "string",
                    "description": "Drop this image, by the `reference` `sandbox_status` \
                                    lists it under. Mutually exclusive with `everything`.",
                },
                "everything": {
                    "type": "boolean",
                    "description": "Drop every cached image, and the bytes under the store \
                                    root no image claims. Mutually exclusive with `image`; \
                                    supplying neither is an error.",
                },
                "dry_run": {
                    "type": "boolean",
                    "description": "Report what would be removed and remove nothing.",
                },
            },
        }),
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
                      dense debt. This is a measurement, not a gate. \
                      `limit` is 1-100 (default 20) — no unlimited setting."
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
                      plainly that this tool cannot do it. \
                      `limit` is 1-200 (default 50) — no unlimited setting."
            .to_owned(),
        parameters: json!({
            "type": "object",
            "properties": with_project(json!({
                "limit": { "type": "integer", "minimum": 1, "maximum": 200 },
            })),
        }),
    }
}

/// The served-chat `security_list` tool definition. Lifted out of
/// [`GraphToolRegistry::tools`] for the same reason its neighbours are, and
/// declared here rather than shared with MCP: the `rmcp` macro generates that
/// surface's schema statically from an argument struct, so there is no one
/// declaration for the two to share. `both_tool_surfaces_offer_the_same_tools` is
/// what keeps them level instead.
///
/// The description carries the never-run-is-not-clean warning in full, and has to:
/// a served model sees only this string, and reading `coverage: 0 findings` as a
/// clean repository is the single most likely misuse of this tool.
///
/// `with_project` adds the workspace `project` selector every tool carries.
#[cfg(all(feature = "serve", feature = "execution"))]
fn security_list_tool_def(
    with_project: &impl Fn(serde_json::Value) -> serde_json::Value,
) -> rto_serve::ToolDef {
    use serde_json::json;
    rto_serve::ToolDef {
        name: "security_list".to_owned(),
        description: "List the SECURITY FINDINGS stored for this repository — every live \
                      findings layer with the run evidence behind it (analyzer, version, \
                      backend, isolation, advisory database, report digest) and a page of \
                      its findings. \
                      READ `coverage` FIRST. It is `analyzed` or \
                      `no-analyzer-on-record`, and the second is a real outcome that is \
                      NOT a clean repository: it means no analyzer result is on record \
                      here. A `no-analyzer-on-record` result carries NO `report` at all — \
                      so if you are looking for `findings` and there is no `report`, \
                      nothing was checked and you must say so rather than report zero \
                      findings. An analyzer that ran and found nothing is the OTHER case: \
                      `coverage` is `analyzed` and `findings` is 0. \
                      BOUNDED, and it tells you when it bound something. `limit` is \
                      findings PER LAYER; each layer carries its true `findings` count, \
                      the `page` actually returned, `truncated`, and how many were \
                      `omitted`. A page keeps the most severe findings first, so what is \
                      omitted is the least severe — never conclude a severity is absent \
                      from a truncated page. \
                      `cross_reference` is a VIEW over those findings, not a \
                      replacement: it groups dependency advisories both analyzers \
                      reported, `confirmed_by` says how many said so, `1` is a normal \
                      state rather than a discrepancy, and the `findings` total above is \
                      unchanged by it. \
                      This is read-only: it cannot run an analyzer, and it cannot ingest \
                      a report. Ask the user to run `roteiro security run` or `roteiro \
                      security ingest` — a tool call is not a person consenting to \
                      execution. \
                      `limit` is 1-100 (default 20) — no unlimited setting."
            .to_owned(),
        parameters: json!({
            "type": "object",
            "properties": with_project(json!({
                "analyzer": {
                    "type": "string",
                    "enum": rto_exec::known_analyzers(),
                },
                "limit": { "type": "integer", "minimum": 1, "maximum": 100 },
            })),
        }),
    }
}

/// The served-chat `security_status` tool definition. Lifted out of
/// [`GraphToolRegistry::tools`] like its neighbours.
///
/// It advertises **no `limit`**, and that is a property of the answer: the document
/// is one row per shipped analyzer, one per pinned asset and one per live findings
/// layer — counts, never findings — so its size is fixed by what is installed
/// rather than by how much was found. `security_status_advertises_no_bound_on_either_surface`
/// is what stops a `limit` being added here "for consistency" with nothing
/// honouring it, which is the schema/clamp drift issue #402 fixed once.
///
/// `with_project` adds the workspace `project` selector every tool carries.
#[cfg(all(feature = "serve", feature = "execution"))]
fn security_status_tool_def(
    with_project: &impl Fn(serde_json::Value) -> serde_json::Value,
) -> rto_serve::ToolDef {
    use serde_json::json;
    rto_serve::ToolDef {
        name: "security_status".to_owned(),
        description: "Report SECURITY READINESS in TWO SEPARATELY SCOPED SECTIONS, and \
                      the distinction is the whole point of the tool — do not merge them \
                      when you report it. \
                      `machine` (scope `machine`) describes THIS HOST: the pinned-asset \
                      cache under `asset_root`, and each shipped analyzer's coverage \
                      matrix with its `host_readiness`. It says nothing whatsoever about \
                      whether anything has been run, and it is identical for every project \
                      this server hosts. \
                      `host_readiness` is THREE states, not a boolean, because the fix \
                      differs and only one of them is Roteiro's to perform. `ready` = \
                      assets provisioned AND the analyzer's program on PATH. \
                      `assets-not-provisioned` = ask the user to run `roteiro security \
                      prefetch`. `binary-not-found` = `missing_programs` names what is \
                      absent, and ROTEIRO NEVER INSTALLS ANALYZERS — ask the user to \
                      install it, or to produce a report elsewhere and `roteiro security \
                      ingest` it. Both underlying facts (`assets_provisioned`, \
                      `missing_programs`) are ALWAYS present, so when the state is not \
                      `ready` read both before telling the user what to do: a host can be \
                      missing an asset AND a binary, and `host_readiness` names only the \
                      first remedy. \
                      Do not read `ready` as more than it says — it is readiness to run ON \
                      THIS HOST. The sandboxed backend supplies the analyzer from a \
                      digest-pinned image, so `binary-not-found` does not block it, and \
                      this tool does not inspect the image store, so it reports no sandbox \
                      verdict at all. \
                      `repository` (scope `repository`) describes ONE PROJECT — the one \
                      named in its own `project` field, which the `project` argument \
                      selects: which findings layers are live, how many findings each \
                      holds, and how old the advisory database behind each one is. \
                      `possibly_stale` is `true` whenever an advisory database is \
                      involved and NEVER means current; `false` means only that the \
                      result has no advisory-data axis. \
                      READ `repository.coverage` before concluding anything. It is \
                      `analyzed` or `no-analyzer-on-record`; the second carries no \
                      `layers` at all and means nothing has been analyzed in that \
                      project, which is NOT a clean repository. \
                      It needs no `limit`: this is one row per shipped analyzer, one per \
                      pinned asset and one per live layer — COUNTS, NEVER FINDINGS. Use \
                      `security_list` for the findings themselves. \
                      This is read-only: it cannot provision an asset. `roteiro security \
                      prefetch` opens the network under an explicit human consent and is \
                      not available here — ask the user to run it."
            .to_owned(),
        parameters: json!({
            "type": "object",
            "properties": with_project(json!({
                "analyzer": {
                    "type": "string",
                    "enum": rto_exec::known_analyzers(),
                },
            })),
        }),
    }
}

/// A served-chat `analyzer` argument, validated against the shipped adapter set.
///
/// An unknown name is an **error**, not a document saying no result is on record.
/// The `enum` in the schema above declares the legal values, and this is what
/// happens to a model that sends something else anyway: on the CLI a mistyped
/// `--analyzer` prints "no findings ingested for `<name>`" to the person who typed
/// it and can see their own typo, whereas the same document handed to a model reads
/// as "that analyzer has never been run here" — a security claim built on a
/// spelling mistake. Same rule as an unrecognised `order`, and the same reason.
#[cfg(all(feature = "serve", feature = "execution"))]
fn security_analyzer_arg(args: &serde_json::Value) -> Result<Option<String>, String> {
    match args.get("analyzer").and_then(serde_json::Value::as_str) {
        None => Ok(None),
        Some(name) if rto_exec::known_analyzers().contains(&name) => Ok(Some(name.to_owned())),
        Some(name) => Err(format!(
            "unknown analyzer `{name}` (expected {})",
            rto_exec::known_analyzers().join("|")
        )),
    }
}

/// The page size for one served-chat tool call: the model's `limit` if it sent a
/// usable one, else `default`, clamped into `1..=max`.
///
/// # Why the floor is `1` here when the library reads `0` as unlimited
///
/// [`rto_graph::window`] — and, since issue #393, the search channels too — read
/// `limit == 0` as *unlimited*. **These tools deliberately do not offer that
/// reading**, and this is where they decline it.
///
/// The reason is the one the ceiling already exists for: a tool result is spent
/// against a model's context window, so every tool here advertises a maximum
/// (`25`, `100`, `200`). Were `0` unlimited it would be the one value that
/// escaped that maximum — a ceiling that holds for `1_000_000` and not for `0`.
/// Each tool's parameter schema declares `"minimum": 1`, so `0` is outside the
/// advertised contract; this clamp is what a client that sends it anyway gets,
/// and it is the *smallest expressible page*, never an empty result. That is the
/// point: the defect #393 is about is a caller asking for something and being
/// handed silence, and no value a model can send produces silence here.
///
/// The library and the tools therefore do not disagree about what `0` means. The
/// library defines it; this surface does not accept it, and its schema says so.
/// The matching notes live on [`rto_graph::window`] and on
/// `rto_render::mcp::model_limit` — if this rule changes, all three change with it.
#[cfg(feature = "serve")]
fn model_limit(args: &serde_json::Value, default: usize, max: usize) -> usize {
    args.get("limit")
        .and_then(serde_json::Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(default)
        .clamp(1, max)
}

/// The `categories` filter for a served-chat `debt`/`debt_density` call: the
/// string members of the `categories` array, or empty (= all) when it is absent
/// or holds nothing usable.
///
/// Lifted out of [`GraphToolRegistry::call`] because both arms need it
/// identically — and because two copies of a filter is how the two tools would
/// come to disagree about what a model asked for.
#[cfg(feature = "serve")]
fn categories_arg(args: &serde_json::Value) -> Vec<String> {
    args.get("categories")
        .and_then(serde_json::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// The `(order, limit, min_lines)` triple for a served-chat `debt_density` call,
/// lifted out of [`GraphToolRegistry::call`] to keep that dispatcher readable.
///
/// An unrecognised `order` is an `Err` rather than a silent fall back to
/// `density`: a model told it ranked by `markers` when it did not will state that
/// as fact to the user. `limit` is model-controlled, so it goes through
/// [`model_limit`], which is where the `1..=max` contract and its reasoning live.
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
    let limit = model_limit(args, 20, 100);
    let min_lines = args
        .get("min_lines")
        .and_then(serde_json::Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(rto_graph::DEFAULT_MIN_LINES);
    Ok((order, limit, min_lines))
}

/// The served-chat `context` tool definition. **Kept level with the MCP
/// `context` tool by construction, not by memory**: both wrap
/// [`rto_graph::tool_context`], and the bound they advertise is
/// [`rto_graph::TOOL_CONTEXT_EDGE_CAP`] interpolated rather than typed, so the
/// number in the description cannot fall behind the number the code enforces.
///
/// Note what this tool does **not** take. There is no `refresh`: `roteiro context
/// --refresh` rebuilds stale cached bundles and *prunes* entries for deleted
/// nodes, and both tool surfaces are read-only. And there is no `limit`: a
/// context bundle is one node's neighbourhood, so the only honest argument is the
/// key, and the cap is fixed.
///
/// `with_project` adds the workspace `project` selector every tool carries.
#[cfg(feature = "serve")]
fn context_tool_def(
    with_project: &impl Fn(serde_json::Value) -> serde_json::Value,
) -> rto_serve::ToolDef {
    use serde_json::json;
    let cap = rto_graph::TOOL_CONTEXT_EDGE_CAP;
    rto_serve::ToolDef {
        name: "context".to_owned(),
        description: format!(
            "Fetch a node's CONTEXT BUNDLE: the node, its metadata, and its one-hop \
             provenance-labelled neighbourhood, with a validity `fingerprint` that moves \
             when the node or any neighbour changes. Takes `key` and nothing else. \
             BOUNDED, and it tells you when it bound something: each direction carries at \
             most {cap} edges. When more exist, `truncated` is true, \
             `outgoing.total`/`incoming.total` give the real counts, and `omitted` names \
             each edge kind and how many of it are missing — so an absent `imports` edge \
             means there are none, and a large file's missing definitions are counted \
             rather than silently dropped. Read `omitted` before concluding anything from \
             an absence."
        ),
        parameters: json!({
            "type": "object",
            "properties": with_project(json!({ "key": { "type": "string" } })),
            "required": ["key"],
        }),
    }
}

/// The served-chat `check` tool definition. The MCP surface declares its own (the
/// `rmcp` macro generates that schema statically); both call
/// [`rto_spec::tool_check`], so the verdict itself is defined once.
///
/// The description leads with `gate` on purpose. `roteiro check` is a gate whose
/// answer is an exit code; here it is a document, and the failure mode a document
/// has that an exit code does not is being read as clean when it never ran. See
/// [`rto_spec::ToolCheck`].
///
/// `with_project` adds the workspace `project` selector every tool carries.
#[cfg(feature = "serve")]
fn check_tool_def(
    with_project: &impl Fn(serde_json::Value) -> serde_json::Value,
) -> rto_serve::ToolDef {
    use serde_json::json;
    rto_serve::ToolDef {
        name: "check".to_owned(),
        description: "Run the AUTHORED-LAYER DRIFT CHECK — the same gate `roteiro check` \
                      exits non-zero on and the pre-commit hook reads — and return its \
                      verdict as data: ADR `[[path#Symbol]]` links that no longer resolve, \
                      `@rto:` annotations pointing at unknown or superseded ADRs, malformed \
                      ADRs, and duplicate `adr-id`s. \
                      READ `gate` FIRST. It is `pass`, `fail`, or `not-run`, and `not-run` \
                      is a real outcome: a check needs the project's repository on disk and \
                      a graph synced from the current HEAD, and when it cannot have both it \
                      refuses rather than answering about a tree that is nobody's. A \
                      `not-run` result carries NO `report` at all — so if you are looking \
                      for `violations` and there is no `report`, nothing was checked and \
                      you must say so rather than report a clean repository. \
                      `not_run_reason` says what to fix (usually: run `roteiro sync`). \
                      Read-only: it does not rebuild the graph, which is the one thing the \
                      CLI gate does that this cannot."
            .to_owned(),
        parameters: json!({
            "type": "object",
            "properties": with_project(json!({})),
        }),
    }
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
                      report a high `fan_in` on one. \
                      `limit` is 1-100 (default 20) — no unlimited setting."
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
                              and call `explain` on a returned key for the full content. \
                              `limit` is 1-25 (default 10) — there is no unlimited \
                              setting; narrow the query instead of asking for more."
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
            context_tool_def(&with_project),
            check_tool_def(&with_project),
            debt_density_tool_def(&with_project),
            config_secrets_tool_def(&with_project),
            coupling_tool_def(&with_project),
        ];
        // The two read-only `security` subcommands (issue #435). Gated on
        // `execution` because that is the feature carrying the analyzer surface at
        // all, and the MCP pair is gated on the same one via
        // `rto-render/execution` — so both surfaces offer them or neither does, and
        // `both_tool_surfaces_offer_the_same_tools` cannot be satisfied by one.
        //
        // The other three are permanent refusals with their reasons in this
        // registry's documentation: `ingest` and `run` write a findings layer
        // (`run` through `execute_and_file`, which both its backends share, so
        // ADR-0019's sandboxed default did not change this), and `prefetch` opens
        // the network under an explicit human consent.
        #[cfg(feature = "execution")]
        {
            tools.push(security_list_tool_def(&with_project));
            tools.push(security_status_tool_def(&with_project));
            // The sandbox store's pair (#433), on the same gate and for the same
            // reason. `sandbox_clear` is the one tool on either surface that
            // changes anything; ADR-0014 v1.6 admits it by a rule rather than as
            // an exception, and `sandbox_status` is offered beside it because a
            // destructive verb with no way to see what it will destroy is invoked
            // blind. Neither takes `with_project`: the store is machine-global.
            tools.push(sandbox_status_tool_def());
            tools.push(sandbox_clear_tool_def());
        }
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
                // `limit` is model-controlled: `model_limit` clamps it to the
                // advertised `1..=25`. The floor is deliberate and outlives
                // issue #393 — `rto_graph::search` now reads `0` as unlimited,
                // and this surface still does not offer that. See `model_limit`.
                let limit = model_limit(args, 10, 25);
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
            "context" => {
                let key = str_arg("key").ok_or("`context` needs a string `key`")?;
                // A project-qualified key follows a cross-repo link into that
                // project, exactly as `explain` does (ADR-0009).
                let (proj, bare) = qualified_or(key, project);
                // `rto_graph::tool_context` builds on `build_context`, never on the
                // cached `context`: that one writes an entry on a miss and *prunes*
                // one for a deleted node, and pruning is `roteiro context
                // --refresh`'s maintenance, kept off read paths so an ordinary
                // query never mutates the store (ADR-0013). Hence no `refresh`
                // argument here — there is nothing a model could send that writes.
                self.run(proj.as_deref(), move |store| {
                    rto_graph::tool_context(store, &bare)
                })
            }
            "check" => {
                // The *target* project's own repository, never the one this server
                // was started in — the same rule `debt_ignore_for` follows below,
                // for the same reason. `None` is the `not-run` case rather than a
                // fallback: a check answered from some other repository's files is
                // the defect, not a graceful degradation.
                let root = self
                    .workspace
                    .project_root(project)
                    .map_err(|e| e.to_string())?;
                self.run(project, |store| {
                    rto_spec::tool_check(store, root.as_deref())
                })
            }
            "debt" => {
                let categories = categories_arg(args);
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
                let categories = categories_arg(args);
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
                // `limit` is model-controlled: clamped to the advertised bound by
                // `model_limit`, floor included.
                let limit = model_limit(args, 50, 200);
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
                // `limit` is model-controlled: clamped to the advertised bound by
                // `model_limit`, floor included.
                let limit = model_limit(args, 20, 100);
                self.run(project, |store| rto_graph::coupling(store, order, limit))
            }
            // Both bodies live on the registry above: the read-only pair needs a
            // resolved project name and a page bound, which is more than fits in a
            // dispatch arm without pushing `call` past its line budget.
            #[cfg(feature = "execution")]
            "security_list" => self.security_list(project, args),
            #[cfg(feature = "execution")]
            "security_status" => self.security_status(project, args),
            // Neither takes `project`: one sandbox store per asset root, shared by
            // every hosted project, so there is nothing for a selector to select.
            #[cfg(feature = "execution")]
            "sandbox_status" => self.sandbox_status(),
            #[cfg(feature = "execution")]
            "sandbox_clear" => self.sandbox_clear(args),
            other => Err(format!("unknown tool `{other}`")),
        }
    }
}

/// Render a build-output of the graph: the docs site or an Obsidian vault.
fn run_render(
    cfg: &config::Config,
    ingest: rto_graph::IngestConfig,
    target: &str,
    out: Option<String>,
    debt_ignore: &[String],
    workspace_name: Option<&str>,
) -> anyhow::Result<()> {
    match rto_render::Target::parse(target) {
        // `--workspace-name` scopes a *vault*; the docs site is this repository's
        // published website and has no workspace form. Rejected rather than
        // ignored, so the flag never looks like it took effect.
        Some(rto_render::Target::DocsSite) if workspace_name.is_some() => anyhow::bail!(
            "`--workspace-name` applies only to `roteiro render obsidian` \
             (it renders a workspace as one vault); the docs site is per-repository"
        ),
        Some(rto_render::Target::DocsSite) => render_docs(out),
        Some(rto_render::Target::ObsidianVault) => match workspace_name {
            Some(name) => render_obsidian_workspace(cfg, name, out),
            None => render_obsidian(ingest, out, debt_ignore),
        },
        None => anyhow::bail!("unknown render target `{target}` (expected: docs | obsidian)"),
    }
}

/// A path's file name, or `""` — every path here comes from `read_dir`, which
/// does not yield one without.
fn doc_file_name(path: &std::path::Path) -> &str {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
}

/// A path's file stem, or `fallback` for a name that is not valid UTF-8.
fn doc_stem<'a>(path: &'a std::path::Path, fallback: &'a str) -> &'a str {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(fallback)
}

/// Everything the docs site is about to publish, discovered before a byte of it
/// is written.
///
/// Two of the outputs need the whole site up front, which is why this is a phase
/// of its own rather than work done as each page is rendered: the navigation bar
/// is the same on every page, and a `.md` link on the first page can point at
/// the last one.
struct SiteSources {
    /// ADR sources, in a deterministic order.
    adrs: Vec<std::path::PathBuf>,
    /// The Build Plan, when the repository has one.
    build_plan: Option<std::path::PathBuf>,
    /// House-style blueprint sources (ADR-0004).
    blueprints: Vec<std::path::PathBuf>,
    /// Documents that declare themselves published (`site-page:`).
    pages: Vec<rto_spec::SitePage>,
    /// What the site serves each source document as.
    published: rto_render::PublishedPages,
    /// The one navigation bar, in `site_nav` order.
    nav: Vec<rto_render::NavEntry>,
    /// Web blob base for the commit being rendered — where a link that leaves
    /// the site points instead (issue #456). `None` when the repository has no
    /// `origin` remote, or one whose URL maps to no web view; links are then
    /// left exactly as authored. See [`rto_render::SourceBase`].
    blob_base: Option<String>,
}

/// Discover every source the docs site publishes, and derive the two things
/// that have to be known site-wide: [`SiteSources::published`] and
/// [`SiteSources::nav`].
fn discover_site_sources(
    repo: &rto_graph::Repo,
    root: &std::path::Path,
) -> anyhow::Result<SiteSources> {
    // Each ADR (skip the directory README), in a deterministic order.
    let mut adrs: Vec<_> = std::fs::read_dir(root.join("docs/adr"))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("md"))
        .filter(|p| doc_file_name(p) != "README.md")
        .collect();
    adrs.sort();

    let build_plan = Some(root.join("docs/BUILD_PLAN.md")).filter(|p| p.is_file());

    // Blueprints live under docs/blueprint(s)/ (ADR-0004); the overall project
    // blueprint is one. Each is rendered to a root-level page like the Build Plan.
    let mut blueprints: Vec<std::path::PathBuf> = Vec::new();
    for dir in ["docs/blueprint", "docs/blueprints"] {
        let bp_dir = root.join(dir);
        if !bp_dir.is_dir() {
            continue;
        }
        let mut bps: Vec<_> = std::fs::read_dir(&bp_dir)?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("md"))
            .filter(|p| doc_file_name(p) != "README.md")
            .collect();
        bps.sort();
        blueprints.append(&mut bps);
    }

    // The documents that declare themselves published. Read through the one
    // classification rule in `rto-spec`, so the pages this emits are exactly the
    // pages `roteiro check` gates. `Worktree` rather than `Committed` because
    // everything else here reads the working tree from disk, and a preview that
    // rendered `HEAD` while claiming to render your edits would be the
    // two-layers-disagreeing bug of issue #330 with a public URL on it.
    let pages = rto_spec::authored_docs(repo, GraphSource::Worktree)?.site;

    // What the site serves each source document as. A slug is URL-safe by
    // construction, so a page's published name need not resemble its file name —
    // and rewriting `../BUILD_PLAN_V2.md` to `../BUILD_PLAN_V2.html` aimed four
    // correct repository links at a page that is never emitted (issue #446).
    let mut published = rto_render::PublishedPages::new();
    for path in &adrs {
        published.publish(
            doc_file_name(path),
            &format!("{}.html", doc_stem(path, "adr")),
        );
    }
    if build_plan.is_some() {
        published.publish("BUILD_PLAN.md", "build-plan.html");
    }
    for path in &blueprints {
        published.publish(
            doc_file_name(path),
            &format!("{}.html", doc_stem(path, "blueprint")),
        );
    }
    for page in &pages {
        let name = page.path.rsplit('/').next().unwrap_or(&page.path);
        published.publish(name, &page.href());
    }

    // One bar, built once and handed to every page, so the site cannot disagree
    // with itself about where its pages are. `site_nav` is the ordering the check
    // reports, not a second sort. The landing page is `./`: it is hand-written
    // HTML copied from `website/public` rather than a rendered page, so it
    // carries its own copy of this bar — held to this one by the
    // `the_landing_page_carries_the_bar_the_renderer_emits` test.
    let nav = std::iter::once(rto_render::NavEntry {
        href: "./".to_owned(),
        label: "Home".to_owned(),
    })
    .chain(
        rto_spec::site_nav(&pages)
            .into_iter()
            .map(|p| rto_render::NavEntry {
                href: p.href(),
                label: p.nav.clone(),
            }),
    )
    .collect();

    // Where a link out of the site points instead. Pinned to the rendered
    // commit, exactly as the vault renderer's Source links are — GitHub serves a
    // blob by sha forever, so the link survives the file being renamed, and one
    // repository with two answers to "which commit does a source link mean"
    // would be its own defect. `None` (no `origin`, or an unmappable one) leaves
    // every link as authored; `rto_render::SourceBase` says why that beats both
    // refusing and degrading to plain text.
    let blob_base = match (repo.origin_url().as_deref(), repo.head_commit_id().ok()) {
        (Some(remote), Some(commit)) => source_blob_base(remote, &commit),
        _ => None,
    };

    Ok(SiteSources {
        adrs,
        build_plan,
        blueprints,
        pages,
        published,
        nav,
        blob_base,
    })
}

/// Render the documentation site: copy static assets, then render each ADR, the
/// lifetime docs and every published site page into `<out>` (default
/// `website/dist`).
fn render_docs(out: Option<String>) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let repo = rto_graph::Repo::discover(&cwd)?;
    let root = repo
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("cannot render docs in a bare repository"))?;
    let out = out.map_or_else(|| root.join("website/dist"), std::path::PathBuf::from);

    let src = discover_site_sources(&repo, root)?;

    if out.exists() {
        std::fs::remove_dir_all(&out)?;
    }
    std::fs::create_dir_all(out.join("adr"))?;
    copy_dir(&root.join("website/public"), &out)?;

    // The landing page is the one page nothing renders — `website/public` is
    // copied verbatim — so its navigation bar was a hand-maintained copy of the
    // list `site_nav` derives (issue #508). Overwrite that copy with the real
    // bar, so there is no second list left to drift. `replace_site_nav` explains
    // why a page with no bar is left alone, and why no link auditor could have
    // found the bug this removes.
    let landing = out.join("index.html");
    if let Ok(html) = std::fs::read_to_string(&landing)
        && let Some(rendered) = rto_render::replace_site_nav(&html, &src.nav, "./")
    {
        std::fs::write(&landing, rendered)?;
    }

    let mut entries = Vec::new();
    for path in &src.adrs {
        let stem = doc_stem(path, "adr");
        let md = std::fs::read_to_string(path)?;
        let source = doc_source_base(src.blob_base.as_deref(), root, path);
        let rendered = rto_render::render_adr(&md, stem, &src.published, source.as_ref());
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
    if let Some(build_plan) = &src.build_plan {
        let md = std::fs::read_to_string(build_plan)?;
        let source = doc_source_base(src.blob_base.as_deref(), root, build_plan);
        let rendered = rto_render::render_doc(&md, "Build Plan", &src.published, source.as_ref());
        std::fs::write(out.join("build-plan.html"), &rendered.html)?;
        lifetime.push(rto_render::IndexEntry {
            // The index lives under adr/, so link up one level.
            href: "../build-plan.html".to_owned(),
            title: rendered.title,
        });
    }
    for path in &src.blueprints {
        let stem = doc_stem(path, "blueprint");
        let md = std::fs::read_to_string(path)?;
        let source = doc_source_base(src.blob_base.as_deref(), root, path);
        let rendered = rto_render::render_doc(&md, stem, &src.published, source.as_ref());
        std::fs::write(out.join(format!("{stem}.html")), &rendered.html)?;
        lifetime.push(rto_render::IndexEntry {
            href: format!("../{stem}.html"),
            title: rendered.title,
        });
    }

    // The published site pages, each written as `<slug>.html` — the slug is the
    // URL, which is why it is validated rather than derived.
    for page in &src.pages {
        let path = root.join(&page.path);
        let md = std::fs::read_to_string(&path)?;
        let href = page.href();
        let source = doc_source_base(src.blob_base.as_deref(), root, &path);
        let rendered = rto_render::render_site_page(
            &md,
            &page.title,
            &src.nav,
            &href,
            &src.published,
            source.as_ref(),
        );
        std::fs::write(out.join(&href), &rendered.html)?;
    }

    std::fs::write(
        out.join("adr").join("index.html"),
        rto_render::render_adr_index(&lifetime, &entries),
    )?;

    println!(
        "rendered docs → {} ({} ADR page(s), {} lifetime doc(s), {} site page(s))",
        out.display(),
        entries.len(),
        lifetime.len(),
        src.pages.len(),
    );
    Ok(())
}

/// The blob holding a node's full prose text, or `None` if it has none.
///
/// The kind check is the whole of the rule and is load-bearing in both
/// directions. A **file** node *is* the document, so it gets it. Every other node
/// that shares the path does not: an `adr`/`adr_section` node's path is the ADR
/// markdown file, and matching on the path alone would paste the entire document
/// into all twenty ADR notes and all 179 of their section notes, next to the
/// `file:` note that already carries it once. A symbol's `meta.content` is a doc
/// comment — a summary of a definition, not a document — and is right as it is.
///
/// `blobs` is already filtered to prose paths, so a non-prose file node (source
/// code, config, an image) simply misses the lookup and keeps whatever extraction
/// captured for it. PDF and OCR text stay capped too: they are extraction
/// *results*, not bytes this call site could re-read.
fn prose_blob_oid<'a>(
    blobs: &'a std::collections::HashMap<String, String>,
    ex: &rto_graph::Explanation,
) -> Option<&'a str> {
    if ex.node.kind != rto_graph::NodeKind::File.as_str() {
        return None;
    }
    blobs.get(ex.node.path.as_deref()?).map(String::as_str)
}

/// The ADR file behind an `adr` or `adr_section` node, or `None` for anything
/// else.
///
/// The mirror image of [`prose_blob_oid`]'s kind check, and load-bearing for the
/// same reason read the other way round. These are precisely the nodes that carry
/// an ADR's path *without being the document*, so each is entitled to a **slice**
/// of that file and none of them to the whole of it — [`rto_spec::AdrDoc::text_for_key`]
/// does the splitting. A path-only rule here would paste all 16 KB of ADR-0015
/// into its twenty-odd notes at once.
fn adr_blob_oid<'a>(
    blobs: &'a std::collections::HashMap<String, String>,
    ex: &rto_graph::Explanation,
) -> Option<&'a str> {
    if ex.node.kind != rto_graph::NodeKind::Adr.as_str()
        && ex.node.kind != rto_graph::NodeKind::AdrSection.as_str()
    {
        return None;
    }
    blobs.get(ex.node.path.as_deref()?).map(String::as_str)
}

/// Render an Obsidian vault for **the current project**: one linked markdown note
/// per graph node in `<out>` (default `vault`).
///
/// This is the whole of `roteiro render obsidian` with no `--workspace-name`. It
/// renders exactly the nodes it always did, with exactly the same bodies — but as
/// of issue #574 **not under the same names**: [`rto_render::note_name`] was not
/// injective under filename case folding, and this repository's vault was
/// silently one file short for each of 104 keys. The count printed below is now
/// the number of notes on disk, which before that fix it was not.
///
/// There is no migration. A hand-written note *outside* the vault that linked in
/// by name now points at nothing — the name is derived from the key, and the old
/// one is not recoverable from the new. See [`rto_render::note_name`] for why the
/// break was taken now rather than deferred.
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
    reset_vault_dir(&out)?;

    let commit = repo.head_commit_id().ok();
    let remote = repo.origin_url();
    let source_base = member_source_base(remote.as_deref(), commit.as_deref());

    let mut names = NoteNames::default();
    let count = write_member_notes(
        &repo,
        &store,
        ingest,
        &out,
        &rto_render::VaultScope::PROJECT,
        source_base.as_deref(),
        &mut names,
    )?;

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
    names.report();
    Ok(())
}

/// Render **one vault spanning a named workspace's member repositories** (issue
/// #442 part 1).
///
/// Nothing here is a new export surface: every note is one a per-project vault
/// would already have rendered for that member, and the only edges shown are the
/// ones the graph already holds. What changes is the *span* — members can no
/// longer overwrite each other's notes, `_Home` is the workspace overview, and
/// the cross-repo links ADR-0009 persists finally have both of their endpoints in
/// one vault.
///
/// Two related names, because conflating them sends a reader looking for a file
/// that does not exist:
///
/// - the **key** a note is rendered from is project-qualified,
///   `<project>::<key>` — ADR-0009's cross-repo form, reused rather than
///   reinvented, which is why a cross-repo link resolves to a note in this vault;
/// - the **note name** is [`rto_render::note_name`] of that key: `:` slugs to
///   `-`, the whole hint lowercases, and a hash of the key is appended. There is
///   no `::` in a filename.
///
/// So `app`'s `file:README.md` is keyed `app::file:README.md` and written to
/// `app-file-readme.md-a114bde6dcaba1c1.md`.
///
/// Qualification is what stops *members* colliding; it does nothing about the two
/// mechanisms that were losing notes *within* a member, so before issue #574 a
/// workspace vault lost those once per member and the total scaled with the
/// member count. It is the hash, not the prefix, that makes the count printed
/// below equal the number of files written.
///
/// Each member is read with **its own** configuration — `[ingest]` toggles and
/// `[debt] ignore` come from that repository's `roteiro.toml`, not the one the
/// command happened to be run in — so a member's section reports exactly the
/// figures its own `roteiro debt` would (ADR-0007 v1.1). Rendering the same
/// repository from two directories must not produce two different numbers.
fn render_obsidian_workspace(
    cfg: &config::Config,
    workspace_name: &str,
    out: Option<String>,
) -> anyhow::Result<()> {
    let paths = workspace_member_paths(cfg, workspace_name)?;
    // Names come from `Workspace::from_repo_paths` rather than being re-derived
    // here, because that is the rule that produced the `<project>::<key>` targets
    // already recorded in the members' external-ref nodes. Two naming rules would
    // mean cross-repo links that resolve at query time and dangle in the vault.
    let ws = rto_graph::Workspace::from_repo_paths(&paths)?;
    let member_names = ws.names();
    let members: std::collections::BTreeSet<String> = member_names.iter().cloned().collect();

    let out = out.map_or_else(
        || std::path::PathBuf::from("vault"),
        std::path::PathBuf::from,
    );
    reset_vault_dir(&out)?;

    let mut names = NoteNames::default();
    let mut summaries = Vec::with_capacity(member_names.len());
    let mut cross_links = Vec::new();
    let mut count = 0usize;

    for project in &member_names {
        let root = ws.project_root(Some(project))?.ok_or_else(|| {
            anyhow::anyhow!("workspace member `{project}` has no repository root")
        })?;

        // That member's own configuration, not the invoking directory's.
        let member_cfg = config::load(&root).map_err(|e| {
            anyhow::anyhow!(
                "reading the configuration of workspace member `{project}` at {}: {e}",
                root.display()
            )
        })?;
        let ingest = member_cfg.effective.ingest.resolve();
        let debt_ignore = member_cfg.effective.debt.ignore.clone().unwrap_or_default();

        let (repo, mut store, cache) = open_graph_at(&root)?;
        build_graph(&repo, &mut store, &cache, ingest, GraphSource::Committed)?;

        let commit = repo.head_commit_id().ok();
        let remote = repo.origin_url();
        let source_base = member_source_base(remote.as_deref(), commit.as_deref());
        let scope = rto_render::VaultScope {
            project: Some(project),
            members: &members,
        };

        count += write_member_notes(
            &repo,
            &store,
            ingest,
            &out,
            &scope,
            source_base.as_deref(),
            &mut names,
        )?;
        collect_cross_links(&store, project, &members, &mut cross_links)?;

        let repo_url = remote.as_deref().and_then(repo_web_root);
        summaries.push(vault_summary(
            &repo,
            &store,
            repo_url,
            commit,
            &debt_ignore,
        )?);
    }

    // Stable, and stable for a reason: the vault is regenerated over itself, so a
    // `_Home` whose member order moved would show a diff on every render.
    cross_links.sort_by(|a: &rto_render::CrossLink, b: &rto_render::CrossLink| {
        (&a.from_project, &a.from_key, &a.to_qualified).cmp(&(
            &b.from_project,
            &b.from_key,
            &b.to_qualified,
        ))
    });
    let cross_links_total = cross_links.len();
    cross_links.truncate(WORKSPACE_CROSS_LINK_ROWS);

    let home = rto_render::render_workspace_home(&rto_render::WorkspaceSummary {
        name: workspace_name.to_owned(),
        members: summaries,
        cross_links,
        cross_links_total,
    });
    std::fs::write(out.join(&home.filename), &home.content)?;

    println!(
        "rendered obsidian vault for workspace `{workspace_name}` → {} \
         ({count} note(s) across {} member(s) + {})",
        out.display(),
        member_names.len(),
        rto_render::HOME_NOTE
    );
    names.report();
    Ok(())
}

/// Cross-repo links shown in the workspace `_Home`. An overview, not the report:
/// the full one is `roteiro links --matrix`, and `_Home` says so when it truncates.
const WORKSPACE_CROSS_LINK_ROWS: usize = 25;

/// The member repository paths of the workspace named `workspace_name`.
///
/// Deliberately **not** [`links_scope_paths`]: that unions in the current repo (so
/// a link check run inside a spoke resolves against its siblings) and falls back
/// to the workspace containing the cwd. Both are right for `links` and wrong for a
/// vault, where the members are the artifact's contents — rendering `-w Thalweg`
/// from an unrelated directory must produce Thalweg's three members and nothing
/// else.
fn workspace_member_paths(
    cfg: &config::Config,
    workspace_name: &str,
) -> anyhow::Result<Vec<std::path::PathBuf>> {
    use std::collections::BTreeSet;
    let resolved = cfg.resolved_workspaces()?;
    let chosen = select_resolved_workspace(&resolved, workspace_name)?;
    let mut paths: BTreeSet<std::path::PathBuf> = BTreeSet::new();
    for root in &chosen.roots {
        paths.extend(rto_graph::discover_repos_under(std::path::Path::new(root))?);
    }
    for repo in &chosen.repos {
        paths.insert(std::path::PathBuf::from(repo));
    }
    if paths.is_empty() {
        anyhow::bail!("workspace `{workspace_name}` resolves to no repositories");
    }
    Ok(paths.into_iter().collect())
}

/// The configured workspace named `name`, or a clear error listing the known ones.
/// Shared by `links -w` and `render obsidian -w` so one name fails the same way in
/// both.
fn select_resolved_workspace<'a>(
    resolved: &'a [rto_graph::ResolvedWorkspace],
    name: &str,
) -> anyhow::Result<&'a rto_graph::ResolvedWorkspace> {
    resolved.iter().find(|r| r.name == name).ok_or_else(|| {
        let known = resolved
            .iter()
            .map(|r| r.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::anyhow!("no workspace named `{name}` (known: {known})")
    })
}

/// Empty `out`, then create it. A vault is a build output that is regenerated over
/// itself; the alternative is stale notes for symbols that have since been renamed
/// accumulating for ever. The contract this rests on is stated in the issue: the
/// vault directory is Roteiro's, and a user's own notes belong outside it.
///
/// **This is a data-loss surface, so it is stated where a user meets it** — in
/// `render`'s `--help`, in `docs/OBSIDIAN_VAULT.md`, and in the README — not only
/// here. Nothing placed inside the vault survives a render, which is exactly what
/// makes the note *names* the only stable interface it has: the sole thing that
/// persists is a note outside the vault linking in by name. That is why issue
/// #574's renaming was worth a deliberate break rather than a compatibility
/// shim, and why it does not get a second one.
fn reset_vault_dir(out: &std::path::Path) -> anyhow::Result<()> {
    if out.exists() {
        std::fs::remove_dir_all(out)?;
    }
    std::fs::create_dir_all(out)?;
    Ok(())
}

/// A web "blob" base for clickable Source links, from the origin remote and the
/// rendered commit (an absolute URL, so it works in a downloaded vault too).
/// `None` when there is no mappable remote — notes then omit the link.
fn member_source_base(remote: Option<&str>, commit: Option<&str>) -> Option<String> {
    match (remote, commit) {
        (Some(r), Some(c)) => source_blob_base(r, c),
        _ => None,
    }
}

/// The note names a vault has already claimed, so a name written twice is
/// **reported** instead of one note silently overwriting the other.
///
/// Keyed case-insensitively, because that is what a reader gets either way:
/// Obsidian resolves `[[links]]` without regard to case, and macOS and Windows
/// fold it in the filesystem too, so two notes whose names differ only in case are
/// one note however they were written.
///
/// This began as instrumentation for the question workspace mode had to answer —
/// do members collide? — and answering it turned up that they already did *within*
/// a single project: measured on this repository before any workspace support
/// existed, 8,144 nodes rendered to 8,040 distinct notes. Nine were lost to the
/// slug and 95 to filename case folding. The count printed said 8,144.
///
/// Issue #574 fixed the naming rather than only reporting it, so this is now a
/// **guard rather than a census**: [`rto_render::note_name`] appends a 64-bit
/// hash of the whole key to every name, which makes a collision a hash collision.
/// One is not expected — the birthday bound over this repository's keys is about
/// 2e-12 — but "not expected" is exactly the claim that loses a note quietly, so
/// the check stays and the report says which two keys did it. Silence here is now
/// the assertion that the count printed is the number of files written.
#[derive(Default)]
struct NoteNames {
    /// Lowercased note filename → the node key that claimed it first.
    claimed: std::collections::HashMap<String, String>,
    /// `(filename, first key, overwriting key)` for each collision.
    collisions: Vec<(String, String, String)>,
}

impl NoteNames {
    /// Record that `key` wrote `filename`, noting a collision if it was taken.
    fn claim(&mut self, filename: &str, key: &str) {
        match self.claimed.entry(filename.to_lowercase()) {
            std::collections::hash_map::Entry::Occupied(e) => {
                self.collisions
                    .push((filename.to_owned(), e.get().clone(), key.to_owned()));
            }
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(key.to_owned());
            }
        }
    }

    /// Warn about collisions, naming a few. Silent when there are none.
    fn report(&self) {
        if self.collisions.is_empty() {
            return;
        }
        eprintln!(
            "warning: {} note(s) share a name with another and are one note in the \
             vault, so the count above is larger than the number of files written. \
             Since issue #574 every name carries a hash of its whole key, so this \
             is a hash collision and a bug worth reporting — not the old lossy \
             slug. The graph is complete; the vault is not:",
            self.collisions.len()
        );
        for (name, first, second) in self.collisions.iter().take(5) {
            eprintln!("  {name}: `{first}` and `{second}`");
        }
        if self.collisions.len() > 5 {
            eprintln!("  … and {} more", self.collisions.len() - 5);
        }
    }
}

/// Write one member's notes into `out` under `scope`, returning how many were
/// rendered. Shared by the single-project and workspace paths, so both read prose
/// and ADR bodies by exactly the same rules.
fn write_member_notes(
    repo: &rto_graph::Repo,
    store: &rto_graph::Store,
    ingest: rto_graph::IngestConfig,
    out: &std::path::Path,
    scope: &rto_render::VaultScope<'_>,
    source_base: Option<&str>,
    names: &mut NoteNames,
) -> anyhow::Result<usize> {
    // Where a prose file's full text lives in the rendered commit, keyed by path.
    //
    // A node's `meta.content` is an *embedding* budget (`MAX_CONTENT`, whitespace
    // collapsed), so a note built from it alone is a few percent of its source on
    // a single line — findable, and close to unreadable. `render_note` is a pure
    // function of the `Explanation` and cannot reach the source; this call site
    // has the repository, so it supplies the text.
    //
    // Only the *tree walk* is eager, and it reads no blob content: a body is read
    // on demand, one prose node at a time, so this repository reads the 74 blobs
    // that are prose rather than all 7,969 of its nodes. Empty when prose ingest
    // is off, which is a project saying it does not want prose bodies — the same
    // setting that leaves `meta.content` unset for these files.
    //
    // The ADR files are collected alongside and *not* gated on `ingest.prose`: an
    // ADR's text is the authored layer, which `apply_authored_layer` re-parses from
    // blobs on every sync whatever the derived layer chooses to ingest. Which paths
    // are ADRs is read off the graph rather than re-decided here, so the
    // classification in `rto_spec::authored_docs_from` stays the only one.
    let adr_paths: std::collections::HashSet<String> = store
        .all_nodes()?
        .into_iter()
        .filter(|n| n.kind == rto_graph::NodeKind::Adr)
        .filter_map(|n| n.path)
        .collect();
    let mut prose_blobs: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut adr_blobs: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    if ingest.prose || !adr_paths.is_empty() {
        for blob in repo.walk_blobs()? {
            if adr_paths.contains(&blob.path) {
                adr_blobs.insert(blob.path.clone(), blob.oid.clone());
            }
            if ingest.prose && rto_graph::is_prose(&blob.path) {
                prose_blobs.insert(blob.path, blob.oid);
            }
        }
    }

    // Each ADR parsed at most once, however many notes it feeds — this repository
    // is 20 ADRs behind 199 notes. Keyed by blob oid, so the cache is
    // content-addressed like everything else that reads a tree. `None` records a
    // blob that could not be read or parsed, so a broken ADR is not re-read once
    // per section.
    let mut adr_docs: std::collections::HashMap<String, Option<rto_spec::AdrDoc>> =
        std::collections::HashMap::new();

    let mut count = 0usize;
    for key in store.all_keys()? {
        // A cross-repo placeholder whose target is a member of this vault is not
        // rendered: every edge to it was pointed at the real note instead, so a
        // note here would be an unreachable stand-in for a node this vault holds.
        if scope.redirects_external_ref(&key) {
            continue;
        }
        if let Some(ex) = rto_graph::explain(store, &key)? {
            // A non-UTF-8 blob falls back to `meta.content`: extraction papered
            // over it with `from_utf8_lossy`, and a note of replacement
            // characters is worse than the capped text.
            let body = if let Some(oid) = adr_blob_oid(&adr_blobs, &ex) {
                let path = ex.node.path.clone().unwrap_or_default();
                adr_docs
                    .entry(oid.to_owned())
                    .or_insert_with(|| {
                        let text = repo
                            .read_blob(oid)
                            .ok()
                            .and_then(|bytes| String::from_utf8(bytes).ok())?;
                        rto_spec::parse_adr(&path, &text).ok()
                    })
                    .as_ref()
                    .and_then(|doc| doc.text_for_key(&ex.node.key))
                    .map(ToOwned::to_owned)
            } else {
                prose_blob_oid(&prose_blobs, &ex)
                    .and_then(|oid| repo.read_blob(oid).ok())
                    .and_then(|bytes| String::from_utf8(bytes).ok())
            };
            let note = rto_render::render_note_scoped(&ex, source_base, body.as_deref(), scope);
            names.claim(&note.filename, &ex.node.key);
            std::fs::write(out.join(&note.filename), &note.content)?;
            count += 1;
        }
    }
    Ok(count)
}

/// Collect one member's cross-repo links: the edges pointing at its external-ref
/// placeholders (ADR-0009), which is where a spoke records that one of its config
/// keys corresponds to a hub's.
///
/// These are read off the store, never inferred here. `roteiro links --infer
/// --write` is what puts them there; a workspace whose members have never been
/// inferred over simply has none.
fn collect_cross_links(
    store: &rto_graph::Store,
    project: &str,
    members: &std::collections::BTreeSet<String>,
    out: &mut Vec<rto_render::CrossLink>,
) -> anyhow::Result<()> {
    let kind = rto_graph::NodeKind::Other(rto_graph::EXTERNAL_REF_KIND.to_owned());
    for placeholder in store.nodes_by_kind(&kind)? {
        let Some(qualified) = rto_graph::external_ref_target(&placeholder) else {
            continue;
        };
        let resolves = rto_graph::parse_qualified(&qualified)
            .is_some_and(|(target, _)| members.contains(target));
        let Some(ex) = rto_graph::explain(store, &placeholder.key)? else {
            continue;
        };
        for edge in ex.incoming {
            // The source node's display name, for a row that reads as something
            // other than a key. Its absence is not worth failing a render over.
            let name = rto_graph::explain(store, &edge.node)?
                .map_or_else(|| edge.node.clone(), |src| src.node.name);
            out.push(rto_render::CrossLink {
                from_project: project.to_owned(),
                from_key: edge.node,
                from_name: name,
                kind: edge.kind,
                confidence: edge.confidence,
                to_qualified: qualified.clone(),
                resolves,
            });
        }
    }
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

/// The [`rto_render::SourceBase`] for the document at `path`: the commit's blob
/// base plus the document's own directory, repository-relative.
///
/// The directory is what makes a link resolvable — `../crates/x.rs` means
/// `crates/x.rs` from `docs/` and `docs/crates/x.rs` from `docs/adr/` — and it is
/// read from the path being rendered rather than assumed, because the three
/// kinds of source document (`docs/`, `docs/blueprint/`, `website/pages/`) sit in
/// three different places on purpose.
///
/// `None` when there is no blob base, or when `path` is somehow not under the
/// repository root; links are then left exactly as authored.
fn doc_source_base(
    blob_base: Option<&str>,
    root: &std::path::Path,
    path: &std::path::Path,
) -> Option<rto_render::SourceBase> {
    let dir = path.parent()?.strip_prefix(root).ok()?;
    rto_render::SourceBase::new(blob_base, &dir.to_string_lossy().replace('\\', "/"))
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
mod vault_body_tests {
    use super::{adr_blob_oid, prose_blob_oid};

    fn blobs() -> std::collections::HashMap<String, String> {
        // Only prose paths, exactly as `render_obsidian` builds it.
        [
            ("docs/adr/0015-media.md", "oid-adr"),
            ("README.md", "oid-readme"),
        ]
        .into_iter()
        .map(|(p, o)| (p.to_owned(), o.to_owned()))
        .collect()
    }

    fn node(kind: &str, path: Option<&str>) -> rto_graph::Explanation {
        rto_graph::Explanation {
            schema: rto_graph::SCHEMA,
            node: rto_graph::NodeSummary {
                key: "k".into(),
                kind: kind.into(),
                name: "n".into(),
                path: path.map(ToOwned::to_owned),
                lang: None,
            },
            meta: serde_json::Value::Null,
            outgoing: vec![],
            incoming: vec![],
        }
    }

    #[test]
    fn a_prose_file_node_resolves_to_its_blob() {
        assert_eq!(
            prose_blob_oid(&blobs(), &node("file", Some("README.md"))),
            Some("oid-readme")
        );
    }

    /// The kind check, which is the whole of the rule. An ADR and each of its
    /// sections carry the *ADR file's* path, so matching on the path alone would
    /// paste the entire document into every one of them — beside the `file:` note
    /// that already holds it once. Twenty ADRs and 179 section notes on this
    /// repository, so the wrong answer here is not a small one.
    #[test]
    fn a_node_sharing_a_prose_path_does_not_get_the_document() {
        for kind in ["adr", "adr_section", "site_page", "marker"] {
            assert_eq!(
                prose_blob_oid(&blobs(), &node(kind, Some("docs/adr/0015-media.md"))),
                None,
                "{kind} shares the path but is not the document"
            );
        }
    }

    /// The two rules are complements, not overlaps: exactly the kinds
    /// `prose_blob_oid` refuses are the ones `adr_blob_oid` claims. Asserted
    /// together because the bug they guard against is a node being served by
    /// *both* — which is how an ADR note would end up holding the whole file the
    /// `file:` note already holds.
    #[test]
    fn the_adr_rule_claims_exactly_what_the_prose_rule_refuses() {
        let path = "docs/adr/0015-media.md";
        for kind in ["adr", "adr_section"] {
            let ex = node(kind, Some(path));
            assert_eq!(
                adr_blob_oid(&blobs(), &ex),
                Some("oid-adr"),
                "{kind} is entitled to a slice of its ADR"
            );
            assert_eq!(
                prose_blob_oid(&blobs(), &ex),
                None,
                "{kind} is never given the whole document"
            );
        }

        // A `file` node on the very same path is the document, and stays the
        // prose rule's. No node is claimed by both rules.
        let file = node("file", Some(path));
        assert_eq!(prose_blob_oid(&blobs(), &file), Some("oid-adr"));
        assert_eq!(adr_blob_oid(&blobs(), &file), None);
    }

    /// Everything else on an ADR's path — and every node without one — is refused.
    #[test]
    fn the_adr_rule_refuses_any_other_kind() {
        for kind in ["site_page", "marker", "fn", "struct"] {
            assert_eq!(
                adr_blob_oid(&blobs(), &node(kind, Some("docs/adr/0015-media.md"))),
                None,
                "{kind} is not an ADR-layer node"
            );
        }
        assert_eq!(adr_blob_oid(&blobs(), &node("adr", None)), None);
        assert_eq!(
            adr_blob_oid(&blobs(), &node("adr", Some("docs/adr/9999-absent.md"))),
            None,
            "a path with no blob in the rendered tree"
        );
    }

    #[test]
    fn a_non_prose_file_node_and_a_pathless_node_are_left_alone() {
        // Not in the map: `render_obsidian` filters it to prose paths, so source
        // code, config and images keep whatever extraction captured.
        assert_eq!(
            prose_blob_oid(&blobs(), &node("file", Some("src/main.rs"))),
            None
        );
        assert_eq!(prose_blob_oid(&blobs(), &node("file", None)), None);
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

// The `roteiro lint` surface: the flags, and the feature-set resolution that
// decides one of the two axes a lint count moves on. The behaviour underneath —
// parsing cargo's stream, refusing an absent linter, storing nothing — is tested
// where it lives, in `rto-exec` and in `tests/lint_cli.rs`.
#[cfg(all(test, feature = "execution"))]
mod lint_cli {
    use super::{Cli, Command, config, lint_features, lint_host_decision};
    use clap::Parser as _;

    // The caveats exist only where the linter that prints them does; the rest of
    // this module tests the flags and the ADR-0020 §6 table, which an
    // `execution`-only build still has.
    #[cfg(feature = "exec-subprocess")]
    use super::LINT_CAVEATS;

    /// A [`config::Loaded`] whose two layers say exactly what a test asks them
    /// to, so the ADR-0020 §6 table can be exercised without touching a disk.
    fn layers(project: Option<bool>, user: Option<bool>) -> config::Loaded {
        let mut loaded = config::Loaded::default();
        loaded.project.lint.allow_unsandboxed = project;
        loaded.user.lint.allow_unsandboxed = user;
        loaded
    }

    fn parse<const N: usize>(args: [&str; N]) -> Command {
        Cli::try_parse_from(args).expect("parse").command
    }

    /// Linting is the one thing a build without `exec-subprocess` must not be
    /// able to do — and, like the host path of `security run`, it must say so by
    /// name rather than as `unrecognized subcommand`.
    ///
    /// This is the coverage `tests/lint_cli.rs` cannot carry: that file drives a
    /// linter that ran, so it is gated on the opposite arm. Exactly one of the
    /// two is compiled in any build, and this is the arm a
    /// `--no-default-features --features execution` build gets (issue #445).
    #[cfg(not(feature = "exec-subprocess"))]
    #[test]
    fn the_lint_path_is_absent_but_names_the_feature() {
        // Parsing is unconditional on purpose — the clap variant is never gated,
        // for the reason `run_lint`'s refusal arm records — so the refusal has
        // to come from the dispatch, not from the parser.
        assert!(matches!(
            parse(["roteiro", "lint", "clippy"]),
            Command::Lint { .. }
        ));
        let message = super::run_lint(
            "clippy",
            &lint_features(false, None).expect("the default feature set resolves"),
            lint_host_decision(&layers(None, None), false, false),
            false,
            None,
        )
        .expect_err("a build without exec-subprocess must refuse to lint")
        .to_string();
        assert!(message.contains("exec-subprocess"), "{message}");
        // And it must not offer ingest as a way out: a lint is reported and
        // never stored, so there is no artifact another machine could hand over.
        assert!(message.contains("no ingest path"), "{message}");
    }

    #[test]
    fn lint_takes_one_analyzer_and_reports_by_default() {
        let Command::Lint {
            analyzer,
            sandboxed,
            allow_unsandboxed,
            image,
            all_features,
            features,
            json,
        } = parse(["roteiro", "lint", "clippy"])
        else {
            panic!("expected Lint");
        };
        assert_eq!(analyzer, "clippy");
        assert!(!all_features);
        assert_eq!(features, None);
        assert!(!json);
        // Roteiro supplies no image, and the flag defaults to none rather than
        // to something: an image nobody chose would be a boundary nobody chose.
        assert_eq!(image, None);
        // Saying nothing asks for neither: the *default* is decided by
        // `lint_host_decision`, not smuggled in as a flag default here.
        assert!(!sandboxed);
        assert!(!allow_unsandboxed);
    }

    /// The two isolation flags are alternatives. Accepting both would leave the
    /// gate deciding which of two contradictory instructions the user meant.
    #[test]
    fn the_two_isolation_flags_are_mutually_exclusive() {
        assert!(
            Cli::try_parse_from([
                "roteiro",
                "lint",
                "clippy",
                "--sandboxed",
                "--allow-unsandboxed",
            ])
            .is_err()
        );
        for flag in ["--sandboxed", "--allow-unsandboxed"] {
            assert!(
                Cli::try_parse_from(["roteiro", "lint", "clippy", flag]).is_ok(),
                "{flag} alone must parse"
            );
        }
    }

    #[test]
    fn lint_requires_an_analyzer() {
        assert!(Cli::try_parse_from(["roteiro", "lint"]).is_err());
    }

    /// The two feature flags are alternatives, not layers: `--all-features` with
    /// `--features x` would leave the report unable to say which one decided the
    /// build, so the parser refuses the pair rather than picking one.
    #[test]
    fn the_two_feature_flags_are_mutually_exclusive() {
        assert!(
            Cli::try_parse_from([
                "roteiro",
                "lint",
                "clippy",
                "--all-features",
                "--features",
                "x"
            ])
            .is_err()
        );
    }

    #[test]
    fn feature_flags_resolve_to_the_set_the_build_will_use() {
        assert_eq!(
            lint_features(false, None).expect("default"),
            rto_exec::FeatureSet::Defaults
        );
        assert_eq!(
            lint_features(true, None).expect("all"),
            rto_exec::FeatureSet::All
        );
        // Comma- or space-separated, because both are what a user types and
        // neither is worth a second flag.
        for list in ["serve,mcp", "serve mcp", " serve , mcp "] {
            assert_eq!(
                lint_features(false, Some(list)).expect("explicit"),
                rto_exec::FeatureSet::Explicit(vec!["serve".to_owned(), "mcp".to_owned()]),
                "{list:?}"
            );
        }
    }

    /// A `--features` that names nothing is a shell that ate the list, not a
    /// request for the default set — and quietly linting a different feature set
    /// than the one asked for is exactly the kind of unstated input that makes a
    /// count incomparable.
    #[test]
    fn an_empty_feature_list_is_refused_rather_than_silently_defaulted() {
        let err = lint_features(false, Some(" , ")).expect_err("must be refused");
        assert!(err.to_string().contains("--all-features"), "{err}");
    }

    /// The default: `roteiro lint` with no config and no flag runs **sandboxed**.
    ///
    /// This is the inversion in one assertion. Before ADR-0020 v1.3 this case
    /// compiled the tree on the host; between v1.3 and conditions 1-2 it
    /// refused, because the sandbox it selected did not exist. It selects the
    /// same thing now and there is something there.
    #[test]
    fn with_nothing_configured_and_no_flag_the_sandbox_is_selected() {
        let decision = lint_host_decision(&layers(None, None), false, false);
        assert!(!decision.granted());
        assert_eq!(decision.backend(), rto_exec::LintBackend::Sandbox);
        assert_eq!(decision.reason, rto_exec::LintReason::SandboxByDefault);
    }

    /// The row that makes the inversion real rather than documented: a
    /// **project** grant must not enable host execution. `roteiro.toml` is
    /// committed, so a merged line would otherwise start running builds on every
    /// teammate's machine.
    #[test]
    fn a_project_grant_does_not_enable_host_execution() {
        let decision = lint_host_decision(&layers(Some(true), None), false, false);
        assert!(
            !decision.granted(),
            "a committed file may never grant host execution"
        );
        // …and it is reported rather than swallowed, so a team is not left
        // wondering why their committed setting does nothing.
        assert!(decision.project_grant_ignored);
        assert!(decision.ignored_project_grant_note().is_some());
    }

    /// The other row that matters: a **project deny** overrides a user grant,
    /// and the flag too. A locked-down repository stays locked down.
    #[test]
    fn a_project_deny_overrides_a_user_grant_and_the_flag() {
        for (sandboxed, allow) in [(false, false), (false, true), (true, false)] {
            let decision = lint_host_decision(&layers(Some(false), Some(true)), sandboxed, allow);
            assert!(
                !decision.granted(),
                "project denial must hold with sandboxed={sandboxed} allow={allow}"
            );
            assert_eq!(
                decision.reason,
                rto_exec::LintReason::SandboxByProjectDenial
            );
            assert_eq!(decision.backend(), rto_exec::LintBackend::Sandbox);
        }
    }

    /// Either layer suffices — the asymmetry with ADR-0019 that ADR-0020 §6
    /// takes deliberately, because requiring both would make the key useless.
    #[test]
    fn either_the_user_config_or_the_flag_grants_on_its_own() {
        assert_eq!(
            lint_host_decision(&layers(None, Some(true)), false, false).reason,
            rto_exec::LintReason::GrantedByUserLayer,
            "the standing preference needs no flag"
        );
        assert_eq!(
            lint_host_decision(&layers(None, None), false, true).reason,
            rto_exec::LintReason::GrantedByInvocation,
            "the flag needs no standing preference"
        );
    }

    /// `--sandboxed` is how somebody with a standing grant opts one run back
    /// out: it selects the boundary, and if the boundary cannot be had it
    /// refuses rather than falling back.
    #[test]
    fn asking_for_the_sandbox_overrides_a_standing_grant() {
        let decision = lint_host_decision(&layers(None, Some(true)), true, false);
        assert!(!decision.granted());
        assert_eq!(decision.reason, rto_exec::LintReason::SandboxByInvocation);
    }

    /// With no flag, the gate must say exactly what the config layers say — so
    /// the value `roteiro config` prints and the value that decides a run cannot
    /// drift apart.
    ///
    /// Anchored to `LintConfigGrant` rather than to the config merge directly,
    /// because that type is the workspace's single implementation of the rule
    /// and the merge is pinned to it separately in `config.rs`. Two tests, one
    /// source of truth, no private access.
    #[test]
    fn with_no_flag_the_gate_says_what_the_config_layers_say() {
        for project in [None, Some(true), Some(false)] {
            for user in [None, Some(true), Some(false)] {
                assert_eq!(
                    lint_host_decision(&layers(project, user), false, false).granted(),
                    rto_exec::LintConfigGrant::from_layers(project, user).as_effective()
                        == Some(true),
                    "project={project:?} user={user:?}: the gate and the echoed value disagree"
                );
            }
        }
    }

    /// The two inverted keys in this workspace implement one rule twice, because
    /// `rto-remote` is optional and off by default while `lint` ships in the
    /// default set — so the rule could not be shared and has to be *checked*
    /// instead. This pins the **config half**, where they agree exactly.
    ///
    /// They deliberately differ on the invocation half (ADR-0019 needs the flag
    /// as well; ADR-0020 §6 does not), which is why this compares
    /// `as_effective` and not the decisions.
    #[cfg(feature = "remote")]
    #[test]
    fn the_two_inverted_keys_apply_the_same_rule_to_their_config_layers() {
        for project in [None, Some(true), Some(false)] {
            for user in [None, Some(true), Some(false)] {
                assert_eq!(
                    rto_exec::LintConfigGrant::from_layers(project, user).as_effective(),
                    rto_remote::ConfigGrant::from_layers(project, user).as_effective(),
                    "ADR-0020 §6 and ADR-0019 §3 disagree at project={project:?} user={user:?}"
                );
            }
        }
    }

    /// The readings ADR-0020 condition 5 requires be surfaced where a user meets
    /// them. One list feeds both the human report and the JSON one, so a
    /// scripted consumer cannot be told less than a person.
    ///
    /// Four now rather than three. Conditions 1-2 add one that did not exist
    /// while every run was a host run: a sandboxed run reports what the
    /// **image's** rustc said, which is not this machine's, so the number moves
    /// with the image as well as with the toolchain. It belongs in this list
    /// rather than only beside the isolation line, because a `--json` consumer
    /// reads this list and would otherwise be told less than a person is.
    #[cfg(feature = "exec-subprocess")]
    #[test]
    fn the_caveats_name_every_reading_that_moves_a_count() {
        assert_eq!(LINT_CAVEATS.len(), 4);
        let all = LINT_CAVEATS.join(" ");
        for reading in ["renamed", "removed", "[workspace.lints]", "image"] {
            assert!(all.contains(reading), "no caveat covers {reading}: {all}");
        }
        // Each is a way the number moves **without the code changing**, which is
        // the property that makes them one list rather than assorted notes.
        for caveat in LINT_CAVEATS {
            assert!(!caveat.trim().is_empty());
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
        Cli, Command, SecurityAction, SecurityIngestReport, SecurityListing, analyzer_state_line,
        report_analyzer, security_cross_reference,
    };
    use clap::Parser as _;

    /// The `analyzers` column names the missing program, because Roteiro is not
    /// going to install it and the reader has to know which one to go and get
    /// (issue #464).
    ///
    /// Built from a `coverage_matrix_with` row rather than a hand-made struct, so
    /// the display and the verdict cannot disagree about the same host.
    #[test]
    fn the_analyzer_column_names_the_missing_program() {
        let root = std::path::Path::new("/nonexistent-asset-root");

        // Assets missing: the remedy is `prefetch`, which Roteiro performs, and the
        // column says so without naming a program.
        let rows = rto_exec::coverage_matrix_with(root, Some("semgrep"), |_| true);
        assert_eq!(analyzer_state_line(&rows[0]), "assets not provisioned");

        // A binary missing is the state the old `ready: bool` could not express, and
        // the only one whose column carries a name.
        let coverage = rto_exec::AnalyzerCoverage {
            host_readiness: rto_exec::Readiness::BinaryNotFound,
            assets_provisioned: true,
            missing_programs: vec!["cargo-audit"],
            ..rows.into_iter().next().expect("one row")
        };
        assert_eq!(
            analyzer_state_line(&coverage),
            "binary not found: cargo-audit"
        );
    }

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
    /// rather than a default — a build with that backend must not be able to
    /// execute a third-party binary because somebody forgot to say no.
    #[test]
    fn run_requires_the_unsandboxed_flag_to_be_given_explicitly() {
        let SecurityAction::Run {
            analyzer,
            sandboxed,
            allow_unsandboxed,
            json,
        } = action(["roteiro", "security", "run", "semgrep"])
        else {
            panic!("expected Run");
        };
        assert_eq!(analyzer, "semgrep");
        assert!(
            !allow_unsandboxed,
            "the flag must default to off; the host backend refuses without it"
        );
        assert!(!sandboxed, "and neither flag is set by default");
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

    /// Saying nothing asks for the sandbox; only `--allow-unsandboxed` asks for
    /// the host, and it asks for it outright.
    ///
    /// This is the whole behavioural change of this command, and it is asserted
    /// on the pure selector so it holds in a build with **no backend compiled
    /// in** — the build where a regression that quietly re-defaulted to the host
    /// would be least likely to be noticed, because nothing there can run to
    /// contradict it.
    ///
    /// The missing case is the point: there is deliberately no input that maps
    /// to "sandbox, or the host if that fails". If one is ever added, this test
    /// still passes and the constraint is gone — so the refusal tests below
    /// exist to catch it from the other side.
    #[test]
    fn the_sandbox_is_what_no_flag_means() {
        assert_eq!(
            super::select_backend(false, false),
            super::RunBackend::Sandboxed,
            "a bare `security run` must ask for the sandbox, not the host"
        );
        assert_eq!(
            super::select_backend(true, false),
            super::RunBackend::Sandboxed,
            "`--sandboxed` must mean the same as saying nothing"
        );
        assert_eq!(
            super::select_backend(false, true),
            super::RunBackend::Subprocess,
            "`--allow-unsandboxed` selects the host outright"
        );
    }

    /// The two flags cannot be given together, so no invocation is ambiguous
    /// about which machine it runs on.
    #[test]
    fn sandboxed_and_allow_unsandboxed_are_mutually_exclusive() {
        assert!(
            Cli::try_parse_from([
                "roteiro",
                "security",
                "run",
                "semgrep",
                "--sandboxed",
                "--allow-unsandboxed",
            ])
            .is_err(),
            "asking for both an isolation boundary and no isolation boundary must be refused \
             at the parser rather than resolved by precedence"
        );
        // Each alone still parses, so the conflict was declared between them
        // and did not disable one of them.
        for flag in ["--sandboxed", "--allow-unsandboxed"] {
            assert!(
                Cli::try_parse_from(["roteiro", "security", "run", "semgrep", flag]).is_ok(),
                "`{flag}` alone must parse"
            );
        }
    }

    /// `run` and both of its flags parse in **every** build, whatever backends
    /// are compiled in.
    ///
    /// Not a redundant parse check: the alternative — `cfg`-ing the clap variant
    /// or the flag on the backend feature — is what shipped `roteiro model rm`
    /// invisible to every crates.io user, answering a documented command with
    /// `unrecognized subcommand`. This asserts the surface is constant and only
    /// the capability is conditional; the refusal tests below assert the
    /// capability then says so in a sentence.
    #[test]
    fn the_run_surface_is_the_same_in_every_build() {
        for argv in [
            &["roteiro", "security", "run", "semgrep"][..],
            &["roteiro", "security", "run", "semgrep", "--sandboxed"][..],
            &[
                "roteiro",
                "security",
                "run",
                "semgrep",
                "--allow-unsandboxed",
            ][..],
            &["roteiro", "security", "run", "semgrep", "--json"][..],
        ] {
            assert!(
                Cli::try_parse_from(argv).is_ok(),
                "`{}` must parse in every build",
                argv.join(" ")
            );
        }
    }

    #[test]
    fn run_needs_an_analyzer_named() {
        assert!(Cli::try_parse_from(["roteiro", "security", "run"]).is_err());
    }

    /// A build with no sandbox refuses the default path by name, and names the
    /// whole bootstrap rather than only the feature.
    ///
    /// The provisioning order matters and is not guessable: the image pull is
    /// itself behind `exec-boxlite`, so it can only be done by a binary that
    /// already has the feature — which is why the rebuild sits between the two
    /// `prefetch` runs. A message that named the feature alone would send a user
    /// to a build that then fails on a missing image.
    #[cfg(not(feature = "exec-boxlite"))]
    #[test]
    fn a_build_with_no_sandbox_refuses_and_names_the_bootstrap() {
        let message = crate::run_security(
            SecurityAction::Run {
                analyzer: "semgrep".to_owned(),
                sandboxed: false,
                allow_unsandboxed: false,
                json: false,
            },
            // `[lint] image` is a linter's, and this is `security run`. `None`
            // is the honest value rather than a placeholder: a reader-class
            // analyzer's image is pinned in `SANDBOX_IMAGES` and never supplied.
            None,
            // And nothing declared, which is the state this refusal must hold
            // in: a build with no sandbox backend cannot honour
            // `[security.images]` either, so the message names the feature
            // rather than the key.
            &crate::config::SecurityConfig::default(),
        )
        .expect_err("a build with no sandbox must refuse the sandboxed path")
        .to_string();

        assert!(message.contains("exec-boxlite"), "{message}");
        assert!(message.contains("prefetch --analyzer sandbox"), "{message}");
        assert!(message.contains("BOXLITE_RUNTIME_URL"), "{message}");
        assert!(
            message.contains("prefetch --analyzer semgrep"),
            "the image pull is a separate step and must be named: {message}"
        );
        assert!(message.contains("security ingest"), "{message}");
        assert!(
            message.contains("sandbox-unavailable"),
            "the refusal must be greppable, like `assets-unavailable-offline`: {message}"
        );

        // And it must not have run anything instead. `--allow-unsandboxed` is
        // named as a weaker thing the user may choose, with its consequence
        // attached — never as something this command would substitute.
        assert!(
            message.contains("isolation=none"),
            "naming the host path without naming what it costs is how a downgrade \
             gets taken by accident: {message}"
        );
        assert!(
            message.contains("will not make that substitution"),
            "{message}"
        );
    }

    /// `--sandboxed` is refused by the same build for the same reason: the flag
    /// is not a different code path, it is the default said out loud.
    #[cfg(not(feature = "exec-boxlite"))]
    #[test]
    fn asking_for_the_sandbox_explicitly_refuses_identically() {
        let refuse = |sandboxed| {
            crate::run_security(
                SecurityAction::Run {
                    analyzer: "semgrep".to_owned(),
                    sandboxed,
                    allow_unsandboxed: false,
                    json: false,
                },
                None,
                &crate::config::SecurityConfig::default(),
            )
            .expect_err("no sandbox in this build")
            .to_string()
        };
        assert_eq!(refuse(true), refuse(false));
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

    /// Executing on the host is the one thing a build without `exec-subprocess`
    /// must not be able to do — and it must say so by name rather than as
    /// `unrecognized subcommand`.
    ///
    /// Reached via `--allow-unsandboxed`, because that is now the only thing
    /// that asks for the host: without the flag this build is refused by the
    /// sandbox arm instead, for an entirely different missing feature.
    #[cfg(not(feature = "exec-subprocess"))]
    #[test]
    fn the_host_path_is_absent_but_names_the_feature() {
        let SecurityAction::Run { analyzer, .. } =
            action(["roteiro", "security", "run", "cargo-audit"])
        else {
            panic!("expected Run to parse in a build with no host backend");
        };
        assert_eq!(analyzer, "cargo-audit");
        // `None` is the `[lint] image` the dispatch forwards to `prefetch`; the
        // host path never reads it. `run_security` grew that second argument
        // without this call site following, and nothing caught it: the test is
        // gated on `not(exec-subprocess)`, which is precisely the configuration
        // no job compiled (issue #445). It grew a **third** for
        // `[security.images]` and this call site again did not follow — the
        // `no-default-features` job ADR-0014 v1.7 added is what caught it that
        // time, which is the job doing exactly what it was added for.
        let message = crate::run_security(
            SecurityAction::Run {
                analyzer: "cargo-audit".to_owned(),
                sandboxed: false,
                allow_unsandboxed: true,
                json: false,
            },
            None,
            // Nothing declared: the host path has no image, so this argument is
            // not consulted on the way to the refusal below.
            &crate::config::SecurityConfig::default(),
        )
        .expect_err("a build without exec-subprocess must refuse the host path")
        .to_string();
        assert!(message.contains("exec-subprocess"), "{message}");
        assert!(message.contains("security ingest"), "{message}");
    }

    /// The sentence a user reads about isolation is read back out of the stored
    /// run, so it cannot describe a boundary the evidence does not record.
    ///
    /// Asserted on the record rather than through a run because that is the
    /// property worth protecting: the note is a function of `AnalysisRun`, with
    /// no argument saying which backend called it, so there is no input by which
    /// a host run can be described as isolated.
    ///
    /// Gated with the function: a build with no execution backend files no run,
    /// so the sentence has no record to be read out of and `isolation_note` is
    /// not compiled there (issue #445).
    #[cfg(any(feature = "exec-boxlite", feature = "exec-subprocess"))]
    #[test]
    fn the_isolation_note_is_read_out_of_the_stored_run() {
        let base = rto_graph::AnalysisRun {
            layer: "security:semgrep:w".to_owned(),
            analyzer: "semgrep".to_owned(),
            analyzer_version: "1.173.0".to_owned(),
            runner: rto_graph::RunnerKind::Subprocess,
            isolation: rto_graph::Isolation::None,
            image_digest: None,
            rules_digest: None,
            advisory_db: None,
            command_policy: rto_graph::CommandPolicy::default(),
            source: rto_graph::SourceIdentity::default(),
            started_at: "2026-08-18T00:00:00Z".to_owned(),
            ended_at: "2026-08-18T00:00:01Z".to_owned(),
            exit_status: 0,
            report_digest: "sha256:0".to_owned(),
        };

        let host = super::isolation_note(&base);
        assert!(host.contains("isolation none"), "{host}");
        assert!(
            host.contains("nothing enforced that"),
            "a host run must keep saying what it did not have: {host}"
        );

        let sandboxed = super::isolation_note(&rto_graph::AnalysisRun {
            runner: rto_graph::RunnerKind::Sandboxed,
            isolation: rto_graph::Isolation::MicroVm,
            image_digest: Some("sha256:abc".to_owned()),
            ..base
        });
        assert!(sandboxed.contains("isolation microvm"), "{sandboxed}");
        assert!(
            sandboxed.contains("sha256:abc"),
            "a sandboxed run must name the image it ran in — the digest is what makes \
             `isolation=microvm` checkable rather than merely claimed: {sandboxed}"
        );
        assert_ne!(host, sandboxed);
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
            image,
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
        // And no image is assumed either — `prefetch` pulls what `[lint] image`
        // names, or what `--image` names, and invents nothing.
        assert_eq!(image, None);

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

    /// `render obsidian -w <name>` selects a workspace; **bare `render obsidian`
    /// carries none**, which is the compatibility promise of issue #442 stated at
    /// the parse layer. Workspace mode renames every note, and a user's own notes
    /// link into the vault by name, so it must never be entered by inference.
    #[test]
    fn render_takes_a_workspace_name_and_defaults_to_none() {
        let Command::Render {
            target,
            workspace_name,
            ..
        } = parse(["roteiro", "render", "obsidian", "-w", "platform"])
        else {
            panic!("expected Render");
        };
        assert_eq!(target, "obsidian");
        assert_eq!(workspace_name.as_deref(), Some("platform"));

        let Command::Render { workspace_name, .. } = parse(["roteiro", "render", "obsidian"])
        else {
            panic!("expected Render");
        };
        assert_eq!(
            workspace_name, None,
            "no `-w` must mean today's per-project render, never the workspace \
             containing the cwd"
        );
    }

    /// A vault spans a workspace; the docs site is one repository's published
    /// website. Refused rather than ignored, so the flag never looks like it took
    /// effect.
    #[test]
    fn render_docs_refuses_a_workspace_name_instead_of_ignoring_it() {
        let err = crate::run_render(
            &crate::config::Config::default(),
            rto_graph::IngestConfig::default(),
            "docs",
            None,
            &[],
            Some("platform"),
        )
        .expect_err("`render docs -w` must not silently render the plain site")
        .to_string();
        assert!(err.contains("--workspace-name"), "{err}");
        assert!(err.contains("render obsidian"), "{err}");
    }

    /// An unknown workspace name fails fast and lists the known ones — the same
    /// message `links -w` gives, because it is now literally the same code.
    #[test]
    fn an_unknown_workspace_name_for_render_lists_the_known_ones() {
        let resolved = vec![
            rto_graph::ResolvedWorkspace {
                name: "platform".to_owned(),
                roots: vec![],
                repos: vec!["/repos/api".to_owned()],
                linked: true,
            },
            rto_graph::ResolvedWorkspace {
                name: "tools".to_owned(),
                roots: vec![],
                repos: vec!["/repos/cli".to_owned()],
                linked: true,
            },
        ];
        let err = crate::select_resolved_workspace(&resolved, "platfrom")
            .expect_err("a typo must not silently render something else")
            .to_string();
        assert!(err.contains("no workspace named `platfrom`"), "{err}");
        assert!(err.contains("platform, tools"), "{err}");
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

    /// The advertised contract on this surface, matching
    /// `rto_render::mcp::tests::every_limit_tool_advertises_the_bound_it_enforces`
    /// (issue #393). A model reads the parameter schema and the description, and
    /// both must state the bound `model_limit` enforces — including that there is
    /// no unlimited setting, which is what makes this surface's `limit` differ
    /// from the library's without contradicting it.
    #[test]
    fn every_limit_tool_advertises_the_bound_it_enforces() {
        /// The one bounded tool this build gains under `execution`. Empty otherwise,
        /// so the table below needs no `mut` in a build with nothing to add — and
        /// the rule is the same either way: a tool that pages must advertise the
        /// page it enforces. `security_list` bounds findings per layer.
        #[cfg(feature = "execution")]
        const GATED: &[(&str, u64)] = &[("security_list", 100)];
        #[cfg(not(feature = "execution"))]
        const GATED: &[(&str, u64)] = &[];
        use rto_serve::ToolRegistry as _;
        let (set, _flat) = two_workspace_set();
        let tools = registry_for(&set, "api").tools();
        let bounded: Vec<(&str, u64)> = [
            ("search", 25u64),
            ("debt_density", 100),
            ("config_secrets", 200),
            ("coupling", 100),
        ]
        .into_iter()
        .chain(GATED.iter().copied())
        .collect();
        for (name, max) in bounded {
            let tool = tools
                .iter()
                .find(|t| t.name == name)
                .unwrap_or_else(|| panic!("`{name}` advertised"));
            let limit = tool
                .parameters
                .get("properties")
                .and_then(|p| p.get("limit"))
                .unwrap_or_else(|| panic!("`{name}` declares a `limit` parameter"));
            assert_eq!(
                limit.get("minimum").and_then(serde_json::Value::as_u64),
                Some(1),
                "`{name}` must not advertise `0` as a legal limit",
            );
            assert_eq!(
                limit.get("maximum").and_then(serde_json::Value::as_u64),
                Some(max),
                "`{name}` must advertise the ceiling it clamps to",
            );
            assert!(
                tool.description.contains(&format!("1-{max}")),
                "`{name}` description must state its range: {}",
                tool.description,
            );
            assert!(
                tool.description.contains("no unlimited setting"),
                "`{name}` description must say `0`/unlimited is not offered: {}",
                tool.description,
            );
        }
    }

    #[test]
    fn list_projects_returns_only_the_projects_in_the_selected_workspace() {
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

    /// The served-chat registry is a **separate** registry from the MCP server's,
    /// so a test reaching MCP proves nothing here. `context` and `check` were
    /// added to both surfaces in one change; these are the chat side of that.
    #[test]
    fn context_is_advertised_and_returns_the_bounded_bundle() {
        use rto_serve::ToolRegistry as _;
        let reg = called_registry();

        let advertised = reg.tools();
        let def = advertised
            .iter()
            .find(|t| t.name == "context")
            .expect("`context` advertised to the served model");
        let props = def.parameters["properties"]
            .as_object()
            .expect("object schema");
        // A key and nothing else. `--refresh` prunes and `limit` would be a bound
        // this tool does not negotiate; neither may become reachable from a model.
        assert!(props.contains_key("key"), "{props:?}");
        assert!(props.get("refresh").is_none(), "{props:?}");
        assert!(props.get("limit").is_none(), "{props:?}");
        // The advertised cap is the enforced one, interpolated rather than typed.
        assert!(
            def.description.contains(&format!(
                "at most {} edges",
                rto_graph::TOOL_CONTEXT_EDGE_CAP
            )),
            "the description must state the cap it enforces: {}",
            def.description
        );
        assert!(def.description.contains("`omitted`"), "{}", def.description);

        let out = reg
            .call(
                "context",
                &serde_json::json!({ "key": "sym:rust:a.rs#main" }),
            )
            .expect("context");
        let json: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(json["node"]["key"], "sym:rust:a.rs#main");
        assert_eq!(json["edge_cap"], rto_graph::TOOL_CONTEXT_EDGE_CAP);
        assert_eq!(json["truncated"], false);
        assert_eq!(json["outgoing"]["total"], 1, "{json}");
        assert_eq!(json["outgoing"]["edges"][0]["node"], "sym:rust:a.rs#helper");
    }

    /// The read-only contract on this surface too: a call must neither populate
    /// nor prune the context cache (`--refresh`'s maintenance, ADR-0013).
    #[test]
    fn context_never_writes_to_the_store() {
        use rto_serve::ToolRegistry as _;
        let reg = called_registry();
        reg.workspace
            .with_store(None, |store| {
                store
                    .context_cache_put("sym:rust:a.rs#ghost", "stale", "{}")
                    .expect("put");
            })
            .expect("store");

        reg.call(
            "context",
            &serde_json::json!({ "key": "sym:rust:a.rs#main" }),
        )
        .expect("context");
        // A key with no node is where the *cached* read would prune.
        reg.call(
            "context",
            &serde_json::json!({ "key": "sym:rust:a.rs#ghost" }),
        )
        .expect("context");

        let keys = reg
            .workspace
            .with_store(None, |store| store.context_cache_keys().expect("keys"))
            .expect("store");
        assert_eq!(
            keys,
            vec!["sym:rust:a.rs#ghost".to_owned()],
            "a tool read must neither populate nor prune the context cache",
        );
    }

    /// `check` over a pre-opened store has no repository to read an authored
    /// layer from, and the document must be unmistakable about that: `not-run`,
    /// and no `report` for a model to read `violations: []` out of.
    #[test]
    fn check_is_advertised_and_reports_not_run_rather_than_a_clean_repository() {
        use rto_serve::ToolRegistry as _;
        let reg = called_registry();

        let advertised = reg.tools();
        let def = advertised
            .iter()
            .find(|t| t.name == "check")
            .expect("`check` advertised to the served model");
        for claim in [
            "READ `gate` FIRST",
            "`not-run` is a real outcome",
            "carries NO `report`",
            "rather than report a clean repository",
        ] {
            assert!(
                def.description.contains(claim),
                "missing `{claim}` from: {}",
                def.description
            );
        }

        let out = reg.call("check", &serde_json::json!({})).expect("check");
        let json: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(json["schema"], rto_spec::TOOL_CHECK_SCHEMA);
        assert_eq!(json["gate"], "not-run");
        assert!(
            json.get("report").is_none(),
            "a not-run check must carry no report at all: {json}"
        );
        assert!(
            json.pointer("/report/violations").is_none(),
            "`0 violations` must be unreachable when nothing ran: {json}"
        );
        assert!(
            json["not_run_reason"]
                .as_str()
                .is_some_and(|r| !r.is_empty())
        );
    }

    /// The `check` tool end to end on this surface: the registry must resolve the
    /// **hosted project's own** repository, not the directory the server was
    /// started in.
    ///
    /// Without this the only coverage is the `not-run` path, and `not-run` is what
    /// a wrong root produces too — so a `check` that consulted the invoking
    /// repository would look identical. (Fault injection found exactly that gap:
    /// replacing the lookup with `Some(".")` left every other test green.)
    #[test]
    fn check_runs_against_the_repository_of_the_hosted_project_not_the_invoking_one() {
        use rto_serve::ToolRegistry as _;

        let base = std::env::temp_dir().join(format!("rto-chat-check-{}", std::process::id()));
        std::fs::remove_dir_all(&base).ok();
        let dir = base.join("app");
        std::fs::create_dir_all(dir.join("docs/adr")).unwrap();
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args([
                    "-c",
                    "init.defaultBranch=main",
                    "-c",
                    "user.email=t@example.com",
                    "-c",
                    "user.name=T",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .current_dir(&dir)
                .status()
                .expect("run git");
            assert!(status.success(), "git {args:?}");
        };
        git(&["init", "-q"]);
        std::fs::write(dir.join("a.rs"), "pub struct Store;\n").unwrap();
        std::fs::write(
            dir.join("docs/adr/0001.md"),
            "---\nadr-id: \"0001\"\nstatus: Accepted\n---\n\n# ADR-0001\n\n\
             ## Design\n\nUses [[a.rs#Ghost]].\n",
        )
        .unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "seed"]);

        // A graph recorded as synced from that exact `HEAD` tree — `check`'s
        // precondition, and the state a committed `roteiro sync` leaves behind.
        let tree = rto_graph::Repo::discover(&dir)
            .unwrap()
            .head_tree_id()
            .unwrap();
        let store_dir = dir.join(".git").join("roteiro");
        std::fs::create_dir_all(&store_dir).unwrap();
        let mut store = rto_graph::Store::open(&store_dir.join("graph.db")).unwrap();
        store
            .rebuild(
                &rto_graph::FactSet::new()
                    .with_node(rto_graph::Node::new(
                        "file:a.rs",
                        rto_graph::NodeKind::File,
                        "a.rs",
                    ))
                    .with_node(rto_graph::Node::new(
                        "sym:rust:a.rs#Store",
                        rto_graph::NodeKind::Struct,
                        "Store",
                    )),
                Some(&tree),
            )
            .unwrap();
        drop(store);

        let ws = rto_graph::Workspace::from_repo_paths([dir.clone()]).unwrap();
        let reg = GraphToolRegistry::new(std::sync::Arc::new(ws));
        let out = reg.call("check", &serde_json::json!({})).expect("check");
        let json: serde_json::Value = serde_json::from_str(&out).unwrap();

        assert_eq!(
            json["gate"], "fail",
            "the seeded ADR link does not resolve: {json}"
        );
        assert_eq!(json["report"]["adrs"], 1, "{json}");
        assert_eq!(
            json["report"]["violations"][0]["kind"], "broken-link",
            "{json}"
        );
        assert_eq!(json["checked_against"]["tree"], tree, "{json}");
        assert!(json.get("not_run_reason").is_none(), "{json}");

        std::fs::remove_dir_all(&base).ok();
    }

    /// `review` is absent from this surface by decision, not by oversight — see
    /// [`GraphToolRegistry`]'s documentation. A tool added by reflex would pass
    /// every other test here; this is the one that notices.
    #[test]
    fn review_is_not_advertised_to_the_served_model() {
        use rto_serve::ToolRegistry as _;
        assert!(
            !called_registry().tools().iter().any(|t| t.name == "review"),
            "`review` must not be a served-chat tool: ~435 KB for a three-commit \
             range, and its per-file `debt` would report a figure the project's \
             `[debt] ignore` never touched (issue #321)",
        );
    }

    /// The chat side of the same refusal (see `rto_render::mcp`'s tests). The two
    /// registries are separate, so a guard on one proves nothing about the other.
    ///
    /// The `security*` set must be **exactly** the two read-only subcommands.
    /// `ingest` and `run` write a findings layer — `run` through
    /// `execute_and_file`, which both of its backends share, so ADR-0019's
    /// sandboxed default did not make it read-only — and `run` additionally
    /// executes an analyzer, which a tool call is never consent for. `prefetch`
    /// opens the network under an explicit human consent. Issue #435 added `list`
    /// and `status` and deliberately kept this test rather than deleting it: the set
    /// equality catches any unexpected `security*` tool whatever it is called, and
    /// the named loop then fails with the specific refusal that was broken even if
    /// the equality is later loosened.
    #[test]
    fn the_three_mutating_security_subcommands_are_never_advertised() {
        use rto_serve::ToolRegistry as _;
        use std::collections::BTreeSet;

        let security: BTreeSet<String> = called_registry()
            .tools()
            .into_iter()
            .map(|t| t.name)
            .filter(|n| n.starts_with("security"))
            .collect();

        // Feature-gated on `execution`, so the expected set is too: a build without
        // it advertises no `security*` tool at all. That is the same property at a
        // different feature set, not a weaker one.
        #[cfg(feature = "execution")]
        let allowed: BTreeSet<String> = ["security_list", "security_status"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        #[cfg(not(feature = "execution"))]
        let allowed: BTreeSet<String> = BTreeSet::new();

        assert_eq!(
            security, allowed,
            "the `security*` tools must be exactly the read-only pair: `ingest` and \
             `run` mutate (both of `run`'s backends end in `replace_findings_layer`) \
             and `run` executes; `prefetch` opens the network under a human consent. \
             All three are permanent refusals, not gaps",
        );
        for refused in ["ingest", "run", "prefetch"] {
            assert!(
                !security.iter().any(|n| n.contains(refused)),
                "`security {refused}` is a permanent refusal and must never be a \
                 served-chat tool. Found in: {security:?}",
            );
        }
    }

    /// The chat side of `security_status_description_says_what_ready_has_checked`
    /// (issue #464). A served model sees only this string, and the two registries
    /// declare their descriptions separately.
    #[cfg(feature = "execution")]
    #[test]
    fn served_security_status_description_says_what_ready_has_checked() {
        use rto_serve::ToolRegistry as _;
        let tools = called_registry().tools();
        let tool = tools
            .iter()
            .find(|t| t.name == "security_status")
            .expect("`security_status` advertised");
        for claim in [
            "THREE states",
            "assets provisioned AND the analyzer's program on PATH",
            "assets-not-provisioned",
            "binary-not-found",
            "ROTEIRO NEVER INSTALLS ANALYZERS",
            "are ALWAYS present",
            "ON THIS HOST",
            "does not inspect the image store",
        ] {
            assert!(
                tool.description.contains(claim),
                "missing `{claim}` from: {}",
                tool.description
            );
        }
    }

    /// The two tool surfaces must not drift apart in *which* tools they offer.
    /// The MCP server declares its own schemas, so nothing but a test keeps the
    /// sets level — and `[debt] ignore` across three surfaces (#321) and
    /// `limit == 0` across five (#393) are what happens when nothing does.
    ///
    /// # The one tool that is not on both, and why it is not being fixed here
    ///
    /// `list_kind` is on MCP and not on the served-chat surface. That divergence
    /// **predates** this test — it is not something a change introduced and left
    /// — and it is named rather than closed, because closing it in either
    /// direction is a decision:
    ///
    /// - Adding it to the chat surface propagates an unbounded tool. `list_kind`
    ///   on `fn` returns 1,064,414 bytes in this repository — around 265k tokens,
    ///   more than one call can spend — and giving a second surface that hazard is
    ///   worse than the asymmetry.
    /// - Removing it from MCP changes a contract an existing client may depend on.
    ///
    /// Both are worth doing; neither is a cleanup. So the exception is written
    /// down, and the test fails if it stops being *exactly* this one tool — which
    /// is what would happen if somebody quietly added another.
    #[cfg(feature = "mcp")]
    #[test]
    fn both_tool_surfaces_offer_the_same_tools() {
        use rto_serve::ToolRegistry as _;
        use std::collections::BTreeSet;

        /// The known, recorded asymmetry. Not a suppression list to grow: an
        /// addition here needs the reasoning above, written out.
        const MCP_ONLY: [&str; 1] = ["list_kind"];

        let chat: BTreeSet<String> = called_registry()
            .tools()
            .into_iter()
            .map(|t| t.name)
            .collect();
        let mcp: BTreeSet<String> = rto_render::mcp::tool_names().into_iter().collect();

        let mcp_only: BTreeSet<&str> = mcp
            .difference(&chat)
            .map(std::string::String::as_str)
            .collect();
        assert_eq!(
            mcp_only,
            MCP_ONLY.into_iter().collect::<BTreeSet<&str>>(),
            "the MCP-only tools must be exactly the recorded exception — a new one \
             needs the decision written down, not a longer list",
        );
        assert!(
            chat.difference(&mcp).next().is_none(),
            "the served-chat surface must offer nothing MCP does not: {:?}",
            chat.difference(&mcp).collect::<Vec<_>>(),
        );
        // And the tools this change added are on both, which is the property the
        // whole test exists for.
        for name in ["check", "context"] {
            assert!(chat.contains(name) && mcp.contains(name), "`{name}`");
        }
        // Issue #435's pair, for the same reason and with one extra edge: they are
        // feature-gated, and a *gate* that differs between the surfaces is a
        // divergence the name check above would miss under some feature sets and
        // catch under others. This build has `execution` (it is in `default`, and
        // `--all-features` has it too), so they must be on both here.
        #[cfg(feature = "execution")]
        for name in [
            "security_list",
            "security_status",
            "sandbox_status",
            "sandbox_clear",
        ] {
            assert!(
                chat.contains(name) && mcp.contains(name),
                "`{name}` must be on both surfaces: `execution` gates it here and \
                 `rto-render/execution` gates it on MCP, forwarded from the same \
                 feature so they cannot come apart",
            );
        }
    }

    /// Neither surface may advertise a `limit` on `security_status`, because
    /// neither honours one.
    ///
    /// The document is one row per shipped analyzer, one per pinned asset and one
    /// per live findings layer — counts, never findings — so its size is fixed by
    /// what is installed rather than by how much was found. The obvious
    /// "add a `limit` for consistency with the other tools" change would advertise a
    /// bound nothing clamps to, and a schema that disagrees with the clamp is
    /// exactly what issue #402 fixed. MCP's half of this is
    /// `security_status_states_why_it_needs_no_bound`; this is the served-chat half,
    /// plus the cross-check that `security_list` — which *does* page — declares the
    /// same ceiling on both.
    #[cfg(all(feature = "mcp", feature = "execution"))]
    #[test]
    fn security_status_advertises_no_bound_on_either_surface() {
        use rto_serve::ToolRegistry as _;

        let tools = called_registry().tools();
        let status = tools
            .iter()
            .find(|t| t.name == "security_status")
            .expect("`security_status` advertised on the chat surface");
        let props = status.parameters["properties"]
            .as_object()
            .expect("object schema");
        assert!(
            !props.contains_key("limit"),
            "`security_status` must not advertise a `limit` it does not honour: \
             {props:?}",
        );
        for claim in ["needs no `limit`", "COUNTS, NEVER FINDINGS"] {
            assert!(
                status.description.contains(claim),
                "missing `{claim}` from: {}",
                status.description,
            );
        }

        // `security_list` pages, so it declares the ceiling — and declares the same
        // one MCP does, with `"minimum": 1` because `0` means unlimited on the
        // `rto_graph` surfaces and neither model-facing surface offers that.
        let list = tools
            .iter()
            .find(|t| t.name == "security_list")
            .expect("`security_list` advertised on the chat surface");
        let limit = &list.parameters["properties"]["limit"];
        assert_eq!(limit["minimum"], 1, "{limit}");
        assert_eq!(limit["maximum"], 100, "{limit}");
        assert!(
            list.description.contains("1-100 (default 20)")
                && list.description.contains("no unlimited setting"),
            "{}",
            list.description,
        );
    }

    /// The served-chat `security_list` tool distinguishes "nothing analyzed" from
    /// "nothing found", and an unknown analyzer from either.
    ///
    /// `called_registry`'s store has a graph and no findings layers — the state a
    /// first call against a fresh project actually meets, and the one whose CLI
    /// `--json` is `{"layers": [], "findings": 0}`.
    #[cfg(feature = "execution")]
    #[test]
    fn served_security_list_never_reports_an_unanalyzed_repo_as_clean() {
        use rto_serve::ToolRegistry as _;

        let reg = called_registry();
        let out = reg
            .call("security_list", &serde_json::json!({}))
            .expect("security_list");
        let json: serde_json::Value = serde_json::from_str(&out).expect("json");
        assert_eq!(json["coverage"], "no-analyzer-on-record", "{json}");
        assert!(json.get("report").is_none(), "{json}");
        // The field a model would reach for is absent, not zero — `0` is the good
        // answer, which is why it may not appear in this document.
        assert!(!out.contains("\"findings\""), "{out}");

        // A misspelled analyzer is an error, not a document that reads as "semgrep
        // has never run here".
        let err = reg
            .call(
                "security_list",
                &serde_json::json!({ "analyzer": "semgrepp" }),
            )
            .expect_err("unknown analyzer must be an error");
        assert!(err.contains("unknown analyzer `semgrepp`"), "{err}");
    }

    /// The served-chat half of the sandbox pair: offered together, no `project`,
    /// and the destructive one refuses a scope it was not given.
    ///
    /// The MCP surface has the same three assertions
    /// (`the_sandbox_pair_is_offered_together_and_takes_no_project`,
    /// `sandbox_clear_refuses_a_scope_it_was_not_given`). They are separate
    /// registries with separate schemas, so a guard on one proves nothing about
    /// the other — which is the lesson of `[debt] ignore` across three surfaces
    /// (#321) and `limit == 0` across five (#393).
    #[cfg(feature = "execution")]
    #[test]
    fn the_served_sandbox_pair_takes_no_project_and_refuses_an_ungiven_scope() {
        use rto_serve::ToolRegistry as _;

        // A disposable asset root, for the one tool here that deletes. Every test
        // that can reach `sandbox_clear` goes through this, including the ones
        // that only expect a refusal: a refusal is one edit away from not being
        // one, and finding that out with the ambient root costs the developer's
        // whole image cache — see `GraphToolRegistry::asset_root`.
        let root =
            std::env::temp_dir().join(format!("roteiro-chat-sandbox-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("boxlite-home").join("images").join("layers"))
            .expect("a store root");
        std::fs::write(
            root.join("boxlite-home/images/layers/sha256-spare.tar.gz"),
            vec![b'x'; 4096],
        )
        .expect("a spare blob");
        let registry = called_registry().with_asset_root(root.clone());
        let tools = registry.tools();
        for name in ["sandbox_status", "sandbox_clear"] {
            let tool = tools
                .iter()
                .find(|t| t.name == name)
                .unwrap_or_else(|| panic!("`{name}` offered"));
            assert!(
                tool.parameters["properties"].get("project").is_none(),
                "`{name}` must not offer a `project` selector: the sandbox store is \
                 machine-global and the argument would imply an answer that changes with \
                 it. Schema: {}",
                tool.parameters
            );
        }

        // Neither selector is not a request, and must not resolve to the
        // destructive one. Both reach the refusal before the store is opened, so
        // this touches no filesystem.
        let refusal = registry
            .call("sandbox_clear", &serde_json::json!({}))
            .expect_err("silence must not be a scope");
        assert!(
            refusal.contains("does not") && refusal.contains("everything"),
            "the refusal must say that supplying neither is not a request to clear \
             everything: {refusal}"
        );
        assert!(
            registry
                .call(
                    "sandbox_clear",
                    &serde_json::json!({ "image": "registry/a:1", "everything": true })
                )
                .is_err(),
            "`image` and `everything` are different requests and must not combine"
        );

        // And when it is given a scope it does act, reports what it freed, and
        // stays inside the root it was handed. That last one is what fails if the
        // registry ever goes back to resolving the asset root from the process
        // environment — the arrangement that let a fault injection clear a real
        // 8.7 GB cache.
        let out = registry
            .call("sandbox_clear", &serde_json::json!({ "everything": true }))
            .expect("a named scope must be acted on");
        let document: serde_json::Value = serde_json::from_str(&out).expect("a clear document");
        assert_eq!(document["scope"], "machine", "{document}");
        assert_eq!(document["freed_bytes"], 4096, "{document}");
        assert!(
            document["store"]
                .as_str()
                .expect("a store path")
                .starts_with(root.to_str().expect("a utf-8 root")),
            "the tool cleared a store outside the root it was given: {document}"
        );
    }

    /// The served-chat `sandbox_clear` description carries what a model has to do
    /// with the only tool on either surface that changes anything.
    ///
    /// The chat half of `the_mutating_tool_states_its_obligations_where_a_model_reads_them`.
    /// A served model sees only this string.
    #[cfg(feature = "execution")]
    #[test]
    fn the_served_mutating_tool_states_its_obligations() {
        use rto_serve::ToolRegistry as _;

        let tools = called_registry().tools();
        let tool = tools
            .iter()
            .find(|t| t.name == "sandbox_clear")
            .expect("`sandbox_clear` offered");
        for obligation in [
            "sandbox_status",
            "freed_bytes",
            "DIFFERENT REQUESTS",
            "MACHINE-GLOBAL",
            "re-obtainable",
            "retained",
        ] {
            assert!(
                tool.description.contains(obligation),
                "`sandbox_clear`'s description must carry `{obligation}`: {}",
                tool.description
            );
        }
    }

    /// The served-chat `security_status` tool labels its two scopes, and names the
    /// resolved project inside the half that project governs.
    #[cfg(feature = "execution")]
    #[test]
    fn served_security_status_separates_machine_from_repository() {
        use rto_serve::ToolRegistry as _;

        let out = called_registry()
            .call("security_status", &serde_json::json!({}))
            .expect("security_status");
        let json: serde_json::Value = serde_json::from_str(&out).expect("json");
        assert_eq!(json["machine"]["scope"], "machine", "{json}");
        assert_eq!(json["repository"]["scope"], "repository", "{json}");
        assert!(json["machine"]["asset_root"].is_string(), "{json}");
        // The RESOLVED name, not merely *a* string: this registry hosts `api` and
        // the call omitted `project`, so an empty or echoed-argument value here
        // would leave the half unlabelled — which is the whole defect.
        assert_eq!(
            json["repository"]["project"], "api",
            "the resolved project belongs inside the half it governs: {json}"
        );
        assert!(json["machine"].get("project").is_none(), "{json}");
        assert!(json["repository"].get("asset_root").is_none(), "{json}");
        // The machine half's readiness names what it has actually checked (issue
        // #464), on this surface too — the two registries are separate, so a guard
        // on MCP proves nothing here.
        let analyzer = &json["machine"]["analyzers"][0];
        assert!(analyzer["host_readiness"].is_string(), "{json}");
        assert!(analyzer["assets_provisioned"].is_boolean(), "{json}");
        assert!(analyzer["missing_programs"].is_array(), "{json}");
        assert!(analyzer.get("ready").is_none(), "{json}");

        // No findings ingested into this fixture, so the repository half says so
        // rather than showing an empty layer list a reader could call clean.
        assert_eq!(json["repository"]["coverage"], "no-analyzer-on-record");
        assert!(json["repository"].get("layers").is_none(), "{json}");
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

// The `roteiro sandbox` surface: the argument shapes, and the one refusal that
// carries an ADR obligation. The store behaviour these wrap — the set
// difference, the byte accounting, the post-clear verification — is tested where
// it lives, in `rto_exec::sandbox_store`.
#[cfg(all(test, feature = "execution"))]
mod sandbox_cli {
    use super::{Cli, Command, SandboxAction, sandbox_scope};
    use clap::Parser as _;

    fn action<const N: usize>(args: [&str; N]) -> SandboxAction {
        let Command::Sandbox { action } = Cli::try_parse_from(args).expect("parse").command else {
            panic!("expected Sandbox");
        };
        action
    }

    /// `status` and `clear`, and `clear` takes three selectors plus a dry run.
    #[test]
    fn the_store_has_exactly_two_verbs() {
        assert!(matches!(
            action(["roteiro", "sandbox", "status"]),
            SandboxAction::Status { json: false }
        ));
        let SandboxAction::Clear {
            image,
            analyzer,
            everything,
            dry_run,
            json,
        } = action([
            "roteiro",
            "sandbox",
            "clear",
            "--image",
            "registry/a@sha256:abc",
            "--dry-run",
            "--json",
        ])
        else {
            panic!("expected Clear");
        };
        assert_eq!(image.as_deref(), Some("registry/a@sha256:abc"));
        assert_eq!(analyzer, None);
        assert!(!everything);
        assert!(dry_run);
        assert!(json);
    }

    /// Being told nothing must not fall through to clearing everything.
    ///
    /// ADR-0014 v1.6's second obligation for this verb: "clear this image" and
    /// "clear everything" are different requests and must be different arguments,
    /// so a caller who supplied neither has to be sent back rather than served
    /// the destructive reading of silence. The refusal names the way forward,
    /// which for a destructive verb is the *non*-destructive one first.
    #[test]
    fn clearing_nothing_in_particular_is_a_refusal_and_never_everything() {
        let refusal = sandbox_scope(None, None, false)
            .expect_err("no selector must not resolve to a scope")
            .to_string();
        for named in ["--image", "--everything", "roteiro sandbox status"] {
            assert!(
                refusal.contains(named),
                "the refusal does not name `{named}`: {refusal}"
            );
        }
    }

    /// Two selectors is also a refusal, in every pairing.
    #[test]
    fn two_selectors_are_a_refusal_whichever_two_they_are() {
        let pairs = [
            (Some("registry/a:1".to_owned()), None, true),
            (None, Some("semgrep"), true),
            (Some("registry/a:1".to_owned()), Some("semgrep"), false),
        ];
        for (image, analyzer, everything) in pairs {
            assert!(
                sandbox_scope(image.clone(), analyzer, everything).is_err(),
                "accepted two selectors at once: {image:?} {analyzer:?} {everything}"
            );
        }
    }

    /// One selector each resolves to the scope it names.
    #[test]
    fn one_selector_resolves_to_the_scope_it_names() {
        assert_eq!(
            sandbox_scope(None, None, true).expect("everything"),
            rto_exec::Scope::Everything
        );
        assert_eq!(
            sandbox_scope(Some("registry/a:1".to_owned()), None, false).expect("image"),
            rto_exec::Scope::Image("registry/a:1".to_owned())
        );
    }

    /// `--analyzer` is the same request as `--image`, spelled the other way.
    #[cfg(feature = "exec-boxlite")]
    #[test]
    fn an_analyzer_resolves_to_the_reference_the_index_holds_it_under() {
        let image = rto_exec::boxlite::image_for("semgrep").expect("a pinned semgrep image");
        assert_eq!(
            sandbox_scope(None, Some("semgrep"), false).expect("analyzer"),
            rto_exec::Scope::Image(image.reference.to_owned())
        );
        // A reference the table does not carry is a refusal that says what to run
        // to find one that it does.
        let refusal = sandbox_scope(None, Some("cargo-audit"), false)
            .expect_err("no pinned image")
            .to_string();
        assert!(refusal.contains("roteiro sandbox status"), "{refusal}");
    }

    /// Without the pinned image table, `--analyzer` refuses **by name**.
    ///
    /// The store is readable and clearable in this build and the mapping is not,
    /// so the honest answer is a refusal that says how to spell the same request
    /// — not an argument that silently is not there, and not a clear that guessed.
    #[cfg(not(feature = "exec-boxlite"))]
    #[test]
    fn without_the_pinned_image_table_the_analyzer_selector_says_so() {
        let refusal = sandbox_scope(None, Some("semgrep"), false)
            .expect_err("no image table in this build")
            .to_string();
        assert!(refusal.contains("exec-boxlite"), "{refusal}");
        assert!(refusal.contains("--image"), "{refusal}");
    }
}

/// What a refusal **renders as**, which is the only thing a reader ever sees.
///
/// The bug class these guard (#455, #522) is invisible in source: the message
/// reads correctly where it is written, compiles, and passes any test that greps
/// it for a phrase — because the phrase was always right. Only the rendered shape
/// is wrong. So these assert on the shape.
#[cfg(all(test, feature = "execution"))]
mod refusal_text_tests {
    use super::report_analyzer;

    /// The longest run of consecutive spaces in `text` (0 if it has none).
    fn longest_space_run(text: &str) -> usize {
        text.split(|c| c != ' ').map(str::len).max().unwrap_or(0)
    }

    /// The threshold is **two**, not four, and deliberately so. These refusals are
    /// prose sentences, and prose never wants two adjacent spaces; the ~30
    /// legitimate multi-space runs elsewhere in this file are column alignment in
    /// status tables, which is content, on a different surface, and none of it is
    /// here. A rule is only cheap to keep where it is true by construction.
    fn assert_no_leaked_indentation(text: &str) {
        assert!(
            longest_space_run(text) <= 1,
            "a prose refusal carries a run of spaces — leaked source indentation \
             (#522), not content: {text:?}"
        );
    }

    /// #522: this refusal shipped **14 columns of source indentation** into
    /// user-visible text, from a `\` continuation lost in an edit. It is the third
    /// instance of the class `rto_exec::guidance` documents, and it reached a
    /// first-contact path — the message is the entire help available there.
    #[test]
    fn the_missing_analyzer_refusal_renders_no_leaked_indentation() {
        let err = report_analyzer(b"{}", "report.json", None).expect_err("must refuse");
        let text = err.to_string();
        assert_no_leaked_indentation(&text);
        // The way forward is still named — the half that was never broken, kept so
        // a future rewrite cannot satisfy the shape rule by deleting the content.
        assert!(
            text.contains("--analyzer"),
            "the refusal must still name the way forward: {text}"
        );
    }

    /// The sibling refusal on the same path, held to the same rule — so the guard
    /// is about how this function writes to a user, not about one literal.
    #[test]
    fn the_unnamed_analyzer_refusal_renders_no_leaked_indentation() {
        let body = format!(r#"{{"schema":"{}"}}"#, rto_exec::REPORT_SCHEMA);
        let err = report_analyzer(body.as_bytes(), "report.json", None).expect_err("must refuse");
        assert_no_leaked_indentation(&err.to_string());
    }
}

/// What `--limit` means, checked across every surface that offers it.
#[cfg(test)]
mod limit_contract_tests {
    use clap::CommandFactory as _;

    /// The rendered help of the `--limit` argument at a subcommand path.
    fn limit_help(path: &[&str]) -> String {
        let mut cmd = super::Cli::command();
        for name in path {
            let next = cmd
                .find_subcommand(name)
                .unwrap_or_else(|| panic!("no subcommand `{name}` under {path:?}"))
                .clone();
            cmd = next;
        }
        let arg = cmd
            .get_arguments()
            .find(|a| a.get_id() == "limit")
            .unwrap_or_else(|| panic!("no `--limit` argument on `{}`", path.join(" ")));
        arg.get_long_help()
            .or_else(|| arg.get_help())
            .map(ToString::to_string)
            .unwrap_or_default()
    }

    /// #453: `memory list` and `memory recall` have read `0` as unlimited since
    /// #452 and said nothing about it, while `search` documented the same
    /// contract. Behaving consistently while documenting inconsistently leaves a
    /// user unable to tell, which was most of the original complaint in #375 — so
    /// the surfaces are asserted **together, against one clause**, rather than
    /// each being checked for a mention of its own.
    #[test]
    fn every_surface_reading_zero_as_unlimited_says_so_in_the_same_words() {
        // The words `search --limit` has carried since #393. Not "some mention of
        // unlimited": a third phrasing of one contract is the next form of the
        // defect, and it would pass a looser assertion.
        const CLAUSE: &str = "`0` is unlimited";
        for path in [
            &["search"][..],
            &["memory", "list"][..],
            &["memory", "recall"][..],
        ] {
            let help = limit_help(path);
            assert!(
                help.contains(CLAUSE),
                "`{}` --limit reads `0` as unlimited, so its help must say \
                 {CLAUSE:?} — in those words (#453): {help}",
                path.join(" ")
            );
        }
    }
}

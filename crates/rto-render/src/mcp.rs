// roteiro:ignore-file — the `debt` tool's own description and tests name the
// intent-debt vocabulary (todo/fixme/stub/deferred); not real debt here.
//! Model Context Protocol server exposing the query surface to agents, behind
//! the `mcp` feature, built on the official [`rmcp`] SDK.
//!
//! Two transports are offered (see [`serve_stdio`] / [`serve_http`]): stdio for
//! a local agent-spawned subprocess, and streamable-HTTP for networked,
//! multi-client serving (terminate TLS at a reverse proxy). Both expose the
//! same tools — `search`, `explain`, `context`, `check`, `list_kind`, `path`,
//! `debt`, `debt_density`, `coupling`, `config_secrets`, and `list_projects` — as
//! thin wrappers over the
//! matching [`rto_graph`] query primitives, so agents and the CLI see the same
//! graph. Each tool takes an optional `project` selector for a multi-repo
//! workspace (ADR-0008). See ADR-0002 for the decision to adopt `rmcp`.
//!
//! # Every tool here is READ-ONLY
//!
//! A tool call answers from the graph; it never rebuilds or mutates it. That is
//! why `context` here takes a node key and nothing else — `roteiro context
//! --refresh` rebuilds stale cached bundles and *prunes* entries for deleted
//! nodes, and the CLI keeps that on a maintenance seam precisely so ordinary
//! reads do not mutate the store (ADR-0013). It is also why `check` refuses,
//! rather than rebuilds, when the graph does not match `HEAD`.
//!
//! # `roteiro security`, subcommand by subcommand
//!
//! The read-only rule decides most of this surface on its own, and the security
//! subcommands are where it has the sharpest teeth. Written out so the set cannot
//! be quietly completed later by someone who only sees that two of them are here:
//!
//! | subcommand | on this surface | why |
//! | --- | --- | --- |
//! | `security list` | **eligible** — see the note below | read-only over stored findings; the analogue of `debt` |
//! | `security status` | **eligible** — see the note below | read-only; asset digests, advisory-DB age, analyzer coverage |
//! | `security ingest` | **never** | mutating: `run_security_ingest` calls [`rto_graph::Store::replace_findings_layer`] |
//! | `security run` | **never** | mutating *and* executing, on **either** backend — see below |
//! | `security prefetch` | **never** | opens the network under an explicit human consent, and writes the asset cache |
//!
//! `security run` is the one worth spelling out, and its shape changed under
//! ADR-0019 (PR #407) — so this is written against what it does now, not what it
//! used to.
//!
//! It no longer files its own findings: `run_security_run` picks a backend
//! (`select_backend` — sandboxed unless `--allow-unsandboxed`, which selects the
//! host outright rather than as a fallback), and **both** backends end in
//! `execute_and_file`, which calls
//! [`rto_graph::Store::replace_findings_layer`]. That is worth stating precisely,
//! because it is the stronger claim: the write is on the shared path, so
//! **sandboxing does not make the command read-only**. It fails this surface's
//! rule whichever backend runs.
//!
//! And the decisive objection is still the other half: it *executes an
//! analyzer*. **A model asking for a tool is not a human consenting to
//! execution.** That the default is now a microVM makes the command safer for the
//! person who typed it; it does not make a tool call into a person typing it.
//! `--allow-unsandboxed` exists precisely so that choosing the weaker isolation
//! is an explicit act — ADR-0019 §6 refuses even a silent *downgrade* from
//! sandbox to host — and a tool call is exactly the shape that would route around
//! a gate whose whole purpose is to be typed by someone. Nothing on a tool
//! surface may run an analyzer, whatever it is isolated in.
//!
//! `list` and `status` are eligible and are **not implemented yet** — they are a
//! follow-up, not an oversight. Two things have to be settled first, and both
//! were established rather than assumed:
//!
//! - **An empty listing states neither fact.** `security list --json` is 36 bytes
//!   on a repository no analyzer has ever run against: `{"layers": [], "findings":
//!   0}`. "No analyzer has ever run" and "an analyzer ran and found nothing" are
//!   opposite facts, and `findings: 0` reads as the second while meaning the
//!   first. The data does distinguish them (a clean run leaves a layer with no
//!   findings; never running leaves no layer), but the *document* does not say so.
//!   The fix is the shape `check` already uses here: a discriminator that is
//!   always present, and the payload omitted entirely in the case that has no
//!   answer.
//! - **`security status` does not take a project.** `run_security_status` reads
//!   the store via `open_graph()`, which discovers a repository from the
//!   *process's* working directory, while every tool on this surface selects one
//!   with `project` (ADR-0008). Its asset half is machine-global
//!   (`rto_exec::asset_root()`) and its `layers` half is per-repository, so the
//!   tool has to say which of the two each half describes before it can be
//!   correct in a multi-project workspace.
//!
//! Both also need `rto-exec`'s types, which currently live in the `roteiro`
//! binary behind `#[cfg(feature = "execution")]` — so this crate would gain an
//! `execution` notion it has no other use for, and the two tool surfaces would
//! diverge under feature combinations that do not exist today. That is a
//! deliberate design step, not a wiring job.
//!
//! # Why `review` is NOT a tool here
//!
//! `roteiro review` is the third thing the review surface computes, and it is
//! deliberately absent. Two reasons, and the second is the substantive one.
//!
//! **It is enormous.** `review --base HEAD~3` on this repository emits 435,208
//! bytes of JSON — roughly 109k tokens, most of a context window spent on one
//! call. It is the same hazard as `list_kind` on `fn` (1,064,414 bytes), and
//! unlike a ranking there is no `limit` that would make it a page: a review is
//! *the whole change*, and a truncated one is a review that quietly did not look
//! at some of the diff.
//!
//! **It would report a fourth, different debt figure.** `review`'s per-file
//! output carries `debt`, and this crate cannot read the target project's
//! `roteiro.toml` (see the note on the `debt` tool below), so it could not apply
//! that project's `[debt] ignore`. Issue #321 was exactly this defect — one
//! concept reporting different numbers on different surfaces — and it had already
//! recurred across three of them before it was fixed, then again on a fourth
//! (`_Home`, issue #372) that the fix had missed. Adding a surface that reports a
//! *fifth* number for the same repository, on the strength of it being convenient,
//! is how that happens a third time. Exposing `review` means first giving this
//! crate a way to reach the project's config; until then, the honest answer is
//! that the review surface is CLI-first (`roteiro review [--json]`), needs no
//! server, and works in any agent.
//!
//! Separately, and worth knowing before anyone wires this up: **`roteiro review`
//! does not apply `[debt] ignore` today either.** `Command::Review` is never
//! handed the list, and `review::build` collects every marker node in a changed
//! file unconditionally. That is a defect in the CLI, not a reason to duplicate
//! it here.

use std::net::SocketAddr;
use std::sync::Arc;

use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ContentBlock, Implementation, ProtocolVersion, ServerCapabilities,
        ServerInfo,
    },
    tool, tool_handler, tool_router,
    transport::{
        stdio,
        streamable_http_server::{
            StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
        },
    },
};
use rto_graph::{NodeKind, Store, StoreError, Workspace, debt, explain, list_kind, path, search};
use schemars::JsonSchema;
use serde::Deserialize;

/// Errors from running the MCP server.
type McpError = Box<dyn std::error::Error + Send + Sync>;

/// The workspace shared across sessions. A [`Workspace`] is `Send + Sync` (it
/// serialises its own store access internally), so it is shared directly; each
/// stdio session or HTTP connection queries the same registry.
type SharedWorkspace = Arc<Workspace>;

/// Arguments for the `explain` tool.
#[derive(Debug, Deserialize, JsonSchema)]
struct ExplainArgs {
    /// Node key, e.g. `sym:rust:<path>#<Name>`, `file:<path>`, or `adr:<id>`.
    key: String,
    /// Which hosted project to query, when this server hosts several (ADR-0008);
    /// omit for a single-project server. See `list_projects`.
    #[serde(default)]
    project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SearchArgs {
    /// Free-text query; matches node names, keys, paths, and captured content.
    query: String,
    /// Max hits to return (default 10, clamped to 1..=25 — this surface has no
    /// "unlimited": see `model_limit`).
    #[serde(default)]
    #[schemars(range(min = 1, max = 25))]
    limit: Option<u32>,
    /// Which hosted project to query (see `list_projects`); omit if single.
    #[serde(default)]
    project: Option<String>,
}

/// Arguments for the `list_kind` tool.
#[derive(Debug, Deserialize, JsonSchema)]
struct ListKindArgs {
    /// Node kind token, e.g. `fn`, `struct`, `adr`, `file`.
    kind: String,
    /// Which hosted project to query (see `list_projects`); omit if single.
    #[serde(default)]
    project: Option<String>,
}

/// Arguments for the `path` tool.
#[derive(Debug, Deserialize, JsonSchema)]
struct PathArgs {
    /// Start node key.
    from: String,
    /// Goal node key.
    to: String,
    /// Which hosted project to query (see `list_projects`); omit if single.
    #[serde(default)]
    project: Option<String>,
}

/// Arguments for the `context` tool.
///
/// A node key and the project selector every tool carries — and **nothing
/// else**. In particular there is no `refresh`: `roteiro context --refresh`
/// rebuilds every stale cached bundle and *prunes* entries for deleted nodes,
/// which is a write, and this surface is read-only (see `GraphServer::context`).
/// There is no `limit` either; the bound is fixed, and `every_context_tool_states_its_fixed_bound`
/// is what keeps the description honest about it.
#[derive(Debug, Deserialize, JsonSchema)]
struct ContextArgs {
    /// Node key, e.g. `sym:rust:<path>#<Name>`, `file:<path>`, or `adr:<id>`.
    key: String,
    /// Which hosted project to query (see `list_projects`); omit if single.
    #[serde(default)]
    project: Option<String>,
}

/// Arguments for the `check` tool.
#[derive(Debug, Default, Deserialize, JsonSchema)]
struct CheckArgs {
    /// Which hosted project to check (see `list_projects`); omit if single.
    #[serde(default)]
    project: Option<String>,
}

/// Arguments for the `debt` tool.
#[derive(Debug, Default, Deserialize, JsonSchema)]
struct DebtArgs {
    /// Restrict to these categories (empty = all): todo, fixme, hack, stub,
    /// deferred.
    #[serde(default)]
    kind: Vec<String>,
    /// Which hosted project to query (see `list_projects`); omit if single.
    #[serde(default)]
    project: Option<String>,
}

/// Arguments for the `debt_density` tool.
#[derive(Debug, Default, Deserialize, JsonSchema)]
struct DensityArgs {
    /// Restrict to these categories (empty = all): todo, fixme, hack, stub,
    /// deferred.
    #[serde(default)]
    kind: Vec<String>,
    /// Rank by `density` (default), `markers` (the raw count) or `lines`.
    #[serde(default)]
    order: Option<String>,
    /// Max files to return (default 20, clamped to 1..=100 — this surface has no
    /// "unlimited": see `model_limit`).
    #[serde(default)]
    #[schemars(range(min = 1, max = 100))]
    limit: Option<u32>,
    /// Shortest file that may be ranked (default 50; `0` ranks every file).
    #[serde(default)]
    min_lines: Option<u32>,
    /// Which hosted project to query (see `list_projects`); omit if single.
    #[serde(default)]
    project: Option<String>,
}

/// Arguments for the `config_secrets` tool.
#[derive(Debug, Default, Deserialize, JsonSchema)]
struct ConfigSecretArgs {
    /// Max keys to return (default 50, clamped to 1..=200 — this surface has no
    /// "unlimited": see `model_limit`).
    #[serde(default)]
    #[schemars(range(min = 1, max = 200))]
    limit: Option<u32>,
    /// Which hosted project to query (see `list_projects`); omit if single.
    #[serde(default)]
    project: Option<String>,
}

/// Arguments for the `coupling` tool.
#[derive(Debug, Default, Deserialize, JsonSchema)]
struct CouplingArgs {
    /// Rank by `total` (default), `fan_in` (most depended-on) or `fan_out`
    /// (reaches furthest).
    #[serde(default)]
    order: Option<String>,
    /// Max nodes to return (default 20, clamped to 1..=100 — this surface has no
    /// "unlimited": see `model_limit`).
    #[serde(default)]
    #[schemars(range(min = 1, max = 100))]
    limit: Option<u32>,
    /// Which hosted project to query (see `list_projects`); omit if single.
    #[serde(default)]
    project: Option<String>,
}

/// The MCP server handler over a [`Workspace`] of one or more project graphs
/// (ADR-0008). Every tool takes an optional `project` selector and a
/// `list_projects` tool enumerates the hosted projects; a single-project
/// workspace resolves that sole project for a bare call, so it serves as before.
#[derive(Clone)]
struct GraphServer {
    workspace: SharedWorkspace,
    // Populated by the `#[tool_router]` macro and consumed by the
    // `#[tool_handler]`-generated routing; not read by hand.
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl GraphServer {
    fn new(workspace: SharedWorkspace) -> Self {
        Self {
            workspace,
            tool_router: Self::tool_router(),
        }
    }

    /// Run `f` against the selected project's store. Returns the inner query
    /// result, or a project-resolution error (unknown/ambiguous `project`) as a
    /// message string for the caller to surface as a tool error.
    fn with_project<R>(
        &self,
        project: Option<&str>,
        f: impl FnOnce(&Store) -> R,
    ) -> Result<R, String> {
        self.workspace
            .with_store(project, f)
            .map_err(|e| e.to_string())
    }
}

/// Collapse the `Result<Result<T, StoreError>, String>` a `with_project` query
/// produces into a tool result: the JSON value, a query error, or a
/// project-resolution error.
fn query_result<T: serde::Serialize>(r: Result<Result<T, StoreError>, String>) -> CallToolResult {
    match r {
        Ok(Ok(value)) => json_result(&value),
        Ok(Err(e)) => tool_error(&format!("query error: {e}")),
        Err(e) => tool_error(&e),
    }
}

/// The page size for one model-facing tool call: the model's `limit` if it gave
/// one, else `default`, clamped into `1..=max`.
///
/// # Why the floor is `1` here when the library reads `0` as unlimited
///
/// [`rto_graph::window`] — and, since issue #393, the search channels too — read
/// `limit == 0` as *unlimited*. **These tools deliberately do not offer that
/// reading**, and the floor is where they decline it.
///
/// The reason is the same one the ceiling exists for: a tool result is spent
/// against a model's context window, so every tool here advertises a maximum
/// (`25`, `100`, `200`). If `0` meant unlimited it would be the single value
/// that escaped that maximum — the ceiling would hold for `1_000_000` and not
/// for `0`, which is the worst possible place for an exception. Each tool's
/// JSON schema says `"minimum": 1`, so `0` is not part of the advertised
/// contract; the clamp is what happens to a client that sends it anyway, and it
/// yields the *smallest expressible page*, never an empty result. That matters:
/// the defect #393 is about is a caller asking for something and being handed
/// silence, and no value a model can send produces silence here.
///
/// That `"minimum": 1` is **declared**, on each args struct, and has to be:
/// `#[schemars(range(min = 1, max = …))]`. Without it the derive advertises
/// `"minimum": 0` for an unsigned field — the schema would tell a model that `0`
/// is a legal value while this function refused to honour it, which is the
/// contract drift #393 exists to remove, one layer further out. The bound is
/// also stated in each tool's *description*, because a model reads that even
/// when it does not validate against the schema.
///
/// So the library and the tools do not disagree about what `0` means. The
/// library defines it; this surface does not accept it, and says so in its
/// schema. The matching note lives on [`rto_graph::window`] and on the
/// served-chat tool registry in the `roteiro` binary — if this rule changes, all
/// three change together.
fn model_limit(given: Option<u32>, default: u32, max: u32) -> usize {
    // The clamp bounds the value by `max` — 200 at the widest — before the
    // conversion, so `try_from` cannot fail on any target this builds for. The
    // fallback is the floor again rather than a third reading of `limit`.
    usize::try_from(given.unwrap_or(default).clamp(1, max)).unwrap_or(1)
}

/// Resolve a tool key against a `project`: a project-qualified key
/// (`<project>::<key>`) follows a cross-repo link into that project (ADR-0009),
/// overriding `project`; a bare key uses `project`. Owned parts so the query
/// closure can capture them.
fn qualified_or(key: &str, project: Option<&str>) -> (Option<String>, String) {
    rto_graph::parse_qualified(key).map_or_else(
        || (project.map(str::to_owned), key.to_owned()),
        |(p, bare)| (Some(p.to_owned()), bare.to_owned()),
    )
}

#[tool_router]
impl GraphServer {
    /// Explain a node: its record and provenance-labelled incoming/outgoing edges.
    #[tool(description = "Explain a graph node: its record and its \
                          provenance-labelled incoming/outgoing edges. \
                          Keys: sym:<lang>:<path>#<Name>, file:<path>, adr:<id>. \
                          A key may be project-qualified (<project>::<key>) to follow a \
                          cross-repo link into another hosted project (see list_projects).")]
    async fn explain(&self, Parameters(args): Parameters<ExplainArgs>) -> CallToolResult {
        // A project-qualified key follows a cross-repo link into that project.
        let (proj, bare) = qualified_or(&args.key, args.project.as_deref());
        let result = self.with_project(proj.as_deref(), |store| explain(store, &bare));
        match result {
            Ok(Ok(Some(ex))) => json_result(&ex),
            Ok(Ok(None)) => CallToolResult::success(vec![ContentBlock::text(format!(
                "no node with key `{}`",
                args.key
            ))]),
            Ok(Err(e)) => tool_error(&format!("query error: {e}")),
            Err(e) => tool_error(&e),
        }
    }

    /// Search the graph by text, ranked — the entry point for "what/why" questions.
    #[tool(
        description = "Search graph nodes by text — names, keys, paths, and captured \
                          content (doc comments, README/ADR/blueprint prose). Returns \
                          ranked hits with keys; curated ADRs/blueprints and READMEs rank \
                          first, so it's the entry point for \"what is X / why\" questions. \
                          Then `explain` a returned key. Args: query, optional limit \
                          (1-25, default 10 — there is no unlimited setting; narrow the \
                          query instead of asking for more)."
    )]
    async fn search(&self, Parameters(args): Parameters<SearchArgs>) -> CallToolResult {
        let limit = model_limit(args.limit, 10, 25);
        query_result(self.with_project(args.project.as_deref(), |store| {
            search(store, &args.query, limit)
        }))
    }

    /// A node's bounded, read-only context bundle.
    #[tool(
        description = "Fetch a node's CONTEXT BUNDLE: the node, its metadata, and its \
                          one-hop provenance-labelled neighbourhood, with a validity \
                          `fingerprint` that moves when the node or any neighbour changes. \
                          The grounding to answer \"what is this and what is it wired to\" \
                          from. Args: key (the only argument). \
                          BOUNDED, and it tells you when it bound something: each direction \
                          carries at most 50 edges. When more exist, `truncated` is true, \
                          `outgoing.total`/`incoming.total` give the real counts, and \
                          `omitted` names each edge kind and how many of it are missing — \
                          so an absent `imports` edge means there are none, and a large \
                          file's missing definitions are counted rather than silently \
                          dropped. Read `omitted` before concluding anything from an \
                          absence, and use `explain` or `search` to reach what was left \
                          out."
    )]
    async fn context(&self, Parameters(args): Parameters<ContextArgs>) -> CallToolResult {
        // A project-qualified key follows a cross-repo link into that project.
        let (proj, bare) = qualified_or(&args.key, args.project.as_deref());
        // `rto_graph::tool_context` builds on `build_context`, not on the cached
        // `context`: the cached read writes an entry on a miss and *prunes* one
        // for a deleted node. Pruning is `roteiro context --refresh`'s
        // maintenance, which exists precisely so a read never mutates the store
        // (ADR-0013) — and `--refresh` is why this tool takes a key and nothing
        // else. There is no argument here that could reach a write.
        let result = self.with_project(proj.as_deref(), |store| {
            rto_graph::tool_context(store, &bare)
        });
        match result {
            Ok(Ok(Some(ctx))) => json_result(&ctx),
            Ok(Ok(None)) => CallToolResult::success(vec![ContentBlock::text(format!(
                "no node with key `{}`",
                args.key
            ))]),
            Ok(Err(e)) => tool_error(&format!("query error: {e}")),
            Err(e) => tool_error(&e),
        }
    }

    /// Run `roteiro check`'s drift gate, read-only, as data.
    #[tool(
        description = "Run the AUTHORED-LAYER DRIFT CHECK — the same gate `roteiro check` \
                          exits non-zero on and the pre-commit hook reads — and return its \
                          verdict as data: ADR `[[path#Symbol]]` links that no longer \
                          resolve, `@rto:` annotations pointing at unknown or superseded \
                          ADRs, malformed ADRs, and duplicate `adr-id`s. \
                          READ `gate` FIRST. It is `pass`, `fail`, or `not-run`, and \
                          `not-run` is a real outcome: a check needs the project's \
                          repository on disk and a graph synced from the current HEAD, and \
                          when it cannot have both it refuses rather than answering about a \
                          tree that is nobody's. A `not-run` result carries NO `report` at \
                          all — so if you are looking for `violations` and there is no \
                          `report`, nothing was checked and you must say so rather than \
                          report a clean repository. `not_run_reason` says what to fix \
                          (usually: run `roteiro sync`). \
                          This is read-only: it does not rebuild the graph, which is the \
                          one thing the CLI gate does that this cannot."
    )]
    async fn check(&self, Parameters(args): Parameters<CheckArgs>) -> CallToolResult {
        let project = args.project.as_deref();
        // The project's OWN repository, never the one this server was started in
        // — the same rule the `[debt] ignore` lookup follows, for the same reason:
        // answering confidently about the wrong repository is the defect, not a
        // graceful degradation. `None` here is the `not-run` case, not a fallback.
        let root = match self.workspace.project_root(project) {
            Ok(root) => root,
            Err(e) => return tool_error(&e.to_string()),
        };
        query_result(self.with_project(project, |store| {
            rto_spec::tool_check(store, root.as_deref())
        }))
    }

    /// List all nodes of a given kind.
    #[tool(description = "List all nodes of a given kind (fn, struct, enum, \
                          trait, module, file, adr, …).")]
    async fn list_kind(&self, Parameters(args): Parameters<ListKindArgs>) -> CallToolResult {
        query_result(self.with_project(args.project.as_deref(), |store| {
            list_kind(store, &NodeKind::from_token(&args.kind))
        }))
    }

    /// Find a shortest path between two nodes.
    #[tool(
        description = "Find a shortest path between two graph nodes, following \
                          edges in either direction. Each hop records the edge kind, \
                          provenance, and traversal direction (outgoing/incoming). \
                          Args: from, to (node keys). A path lives within one project: \
                          a project-qualified `from` (<project>::<key>) selects that \
                          project (see list_projects)."
    )]
    async fn path(&self, Parameters(args): Parameters<PathArgs>) -> CallToolResult {
        // A path lives within one graph: a qualified `from` selects the project,
        // and a qualifier on either endpoint is stripped to a bare, in-store key.
        let (proj, from_bare) = qualified_or(&args.from, args.project.as_deref());
        let to_bare = rto_graph::parse_qualified(&args.to)
            .map_or_else(|| args.to.clone(), |(_, b)| b.to_owned());
        query_result(self.with_project(proj.as_deref(), |store| path(store, &from_bare, &to_bare)))
    }

    /// List intent-debt markers (TODOs, stubs, deferred work).
    #[tool(
        description = "List intent-debt markers found in the codebase — TODO/FIXME/HACK \
                          comments, todo!()/unimplemented!() stubs, and deferred-work notes — \
                          grouped by category (todo, fixme, hack, stub, deferred). Optional \
                          `kind` restricts to given categories. Each marker links to its \
                          enclosing symbol or file via a `contains` edge."
    )]
    async fn debt(&self, Parameters(args): Parameters<DebtArgs>) -> CallToolResult {
        // `ignore` is empty by necessity, not by oversight: this crate has no
        // access to the target project's `roteiro.toml`, so there is no list to
        // apply. Every surface that *can* reach the config does — the
        // enumeration is on `debt_density` below.
        query_result(self.with_project(args.project.as_deref(), |store| {
            debt(store, &args.kind, &[])
        }))
    }

    /// Rank files by intent-debt density (markers per 1,000 lines).
    #[tool(
        description = "Rank FILES by intent-debt DENSITY — markers per 1,000 lines — rather \
                          than by raw marker count, which ranks the biggest file first by \
                          construction. Each row carries `markers`, `lines`, `per_kloc` and a \
                          per-category split; `overall_per_kloc` is the repository baseline to \
                          read a file's figure against. Args: kind, order (density|markers|\
                          lines), limit (1-100, default 20 — no unlimited setting), \
                          min_lines. \
                          Two limits worth passing on to the user rather than reporting a \
                          number as a finding. The denominator is FILE LENGTH — every line, \
                          blanks and comments included — not source lines of code, so figures \
                          run lower than an SLOC tool's and flatter verbose or generated \
                          files. And the markers beneath it include prose matches (`for now`, \
                          `placeholder`, `tbd`), so a design document can rank as dense debt. \
                          This is a measurement, not a gate."
    )]
    async fn debt_density(&self, Parameters(args): Parameters<DensityArgs>) -> CallToolResult {
        let limit = model_limit(args.limit, 20, 100);
        let min_lines = args.min_lines.unwrap_or(rto_graph::DEFAULT_MIN_LINES);
        // An unrecognised `order` is an error, not a silent fall back to
        // `density`: a model told it ranked by `markers` when it did not will
        // report that as fact.
        let order = match args.order.as_deref() {
            None => rto_graph::DensityOrder::default(),
            Some(token) => match rto_graph::DensityOrder::from_token(token) {
                Some(order) => order,
                None => {
                    return tool_error(&format!(
                        "unknown order `{token}` (expected {})",
                        rto_graph::DensityOrder::tokens().join("|")
                    ));
                }
            },
        };
        // `ignore` is empty here for the same reason `debt` above passes none:
        // this crate has no access to the target project's `roteiro.toml`. The
        // CLI (`debt`, `debt-density`, `check`), the graph API, the served-chat
        // tool registry and the Obsidian `_Home` overview all apply the project's
        // own `[debt] ignore`. That is the complete list, written out rather than
        // summarised because `_Home` was missed when the same defect was fixed on
        // the surfaces that happened to be reported (issues #321, #372). An MCP
        // client sees the unfiltered inventory.
        query_result(self.with_project(args.project.as_deref(), |store| {
            rto_graph::debt_density(store, &args.kind, &[], order, limit, min_lines)
        }))
    }

    /// Inventory secret-named config keys and their redaction state.
    #[tool(
        description = "Inventory the SECRET-NAMED config keys in the graph: their file \
                          paths, their key names, and whether each value was redacted \
                          before being stored (`state` = redacted | declared | present). \
                          Answers \"which of this repo's config surfaces deal in \
                          credentials\" and \"did anything unredacted get into this \
                          graph\". Args: limit (1-200, default 50 — no unlimited \
                          setting). \
                          THIS IS NOT A SECRET SCANNER — state the limits when you \
                          report it, and never imply a security guarantee. It CANNOT \
                          find a hardcoded credential in source code: it reads config-key \
                          nodes, so a token in a Rust or Python string literal produces \
                          nothing here and is invisible. It CANNOT judge whether a value \
                          is valid, because it never sees one — values are redacted \
                          before they reach the store. It CANNOT tell a real secret from \
                          a placeholder: `API_TOKEN=changeme` in a committed \
                          `.env.example` and a live token are the same row. And an EMPTY \
                          RESULT DOES NOT MEAN THERE ARE NO SECRETS — it means no config \
                          key is secret-NAMED; a credential under an innocuous key like \
                          `dsn` or `endpoint` never appears. If asked to scan for \
                          secrets, say plainly that this tool cannot do it."
    )]
    async fn config_secrets(
        &self,
        Parameters(args): Parameters<ConfigSecretArgs>,
    ) -> CallToolResult {
        let limit = model_limit(args.limit, 50, 200);
        query_result(self.with_project(args.project.as_deref(), |store| {
            rto_graph::config_secrets(store, limit)
        }))
    }

    /// Rank nodes by directed call coupling (fan-in / fan-out).
    #[tool(
        description = "Rank symbols by DIRECTED call coupling over `calls` edges: `fan_in` \
                          (how many distinct symbols call this one), `fan_out` (how many it \
                          calls), and `instability` = fan_out/(fan_in+fan_out). Use \
                          `order`=fan_in to find what the codebase most depends on, \
                          `order`=fan_out for the symbols that reach furthest, `total` \
                          (default) for overall coupling. Args: order, limit (1-100, \
                          default 20 — no unlimited setting). \
                          Caveat worth passing on to the user: call edges are resolved by \
                          simple name, so a short generically-named function can absorb \
                          every call to that name and show an inflated `fan_in`. Treat a \
                          high figure on such a symbol as a question, not a finding."
    )]
    async fn coupling(&self, Parameters(args): Parameters<CouplingArgs>) -> CallToolResult {
        let limit = model_limit(args.limit, 20, 100);
        // An unrecognised `order` is an error, not a silent fall back to `total`:
        // a model told it ranked by `fan_in` when it did not will report that.
        let order = match args.order.as_deref() {
            None => rto_graph::CouplingOrder::default(),
            Some(token) => match rto_graph::CouplingOrder::from_token(token) {
                Some(order) => order,
                None => {
                    return tool_error(&format!(
                        "unknown order `{token}` (expected {})",
                        rto_graph::CouplingOrder::tokens().join("|")
                    ));
                }
            },
        };
        query_result(self.with_project(args.project.as_deref(), |store| {
            rto_graph::coupling(store, order, limit)
        }))
    }

    /// List the projects this server hosts (ADR-0008).
    #[tool(
        description = "List the projects this server hosts. Pass one as `project` to the \
                          other tools to query it. A single-project server needs no `project`."
    )]
    async fn list_projects(&self) -> CallToolResult {
        json_result(&serde_json::json!({ "projects": self.workspace.names() }))
    }
}

#[tool_handler]
impl ServerHandler for GraphServer {
    fn get_info(&self) -> ServerInfo {
        // `ServerInfo` is `#[non_exhaustive]`; build from default then set fields.
        let mut info = ServerInfo::default();
        info.protocol_version = ProtocolVersion::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.server_info = Implementation::new("roteiro", env!("CARGO_PKG_VERSION"));
        info.instructions = Some(
            "Roteiro codebase knowledge graph. Start with `search` to find nodes by \
             text (it searches captured content too — README/ADR/blueprint prose — \
             and ranks curated docs first, so it answers \"what is X / why\"); then \
             `explain` a key for its provenance-labelled neighbourhood, or `context` \
             for the same neighbourhood bounded and fingerprinted. `list_kind` \
             enumerates a kind, `path` finds how two nodes connect, `debt` lists \
             intent-debt markers, `debt_density` ranks files by markers per 1,000 \
             lines, `coupling` ranks symbols by directed call fan-in/fan-out, \
             `config_secrets` inventories secret-named config keys (an inventory, \
             not a secret scan — see its description), and `check` runs the \
             authored-layer drift gate and returns its verdict as data (read its \
             `gate` field: `not-run` is a real outcome and is not a clean \
             repository). Every tool here is read-only. There is no `review` tool — \
             `roteiro review` is CLI-first and needs no server; see this module's \
             documentation for why it is not exposed."
                .into(),
        );
        info
    }
}

/// The names of every tool this server advertises, sorted.
///
/// Exposed so the **other** tool surface — the served-chat `GraphToolRegistry` in
/// the `roteiro` binary — can assert the two offer the same set. The `rmcp` macro
/// generates this surface's schemas statically from the argument structs, so
/// there is no shared declaration to keep them level; without a test comparing
/// the sets, a tool added to one and forgotten on the other is invisible until
/// somebody notices an agent can do a thing over one transport and not the other.
/// `[debt] ignore` across three surfaces (issue #321) and `limit == 0` across five
/// (#393) are what that costs.
#[must_use]
pub fn tool_names() -> Vec<String> {
    let mut names: Vec<String> = GraphServer::tool_router()
        .list_all()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    names.sort();
    names
}

/// An error `tools/call` result carrying `message`.
fn tool_error(message: &str) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message.to_owned())])
}

/// A successful `tools/call` result carrying `value` as pretty JSON, or a tool
/// error if serialization fails. Shared by every tool handler.
fn json_result<T: serde::Serialize>(value: &T) -> CallToolResult {
    match serde_json::to_string_pretty(value) {
        Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
        Err(e) => tool_error(&format!("serialize error: {e}")),
    }
}

/// Build a current-thread-safe multi-thread tokio runtime.
fn runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
}

/// Serve the graph over stdio (for a local, agent-spawned server), blocking
/// until stdin closes. Takes ownership of `workspace` (one project or many,
/// ADR-0008).
///
/// # Errors
/// Returns an error if the runtime cannot start or the transport fails.
pub fn serve_stdio(workspace: Arc<Workspace>) -> Result<(), McpError> {
    let shared: SharedWorkspace = workspace;
    runtime()?.block_on(async move {
        let service = GraphServer::new(shared).serve(stdio()).await?;
        service.waiting().await?;
        Ok(())
    })
}

/// Build the axum [`Router`](axum::Router) serving the MCP streamable-HTTP
/// transport at the `/mcp` path, for mounting **standalone or merged into
/// another app** — e.g. alongside the `/v1` model endpoint on one port
/// (ADR-0008), so a single process serves both surfaces over one Workspace.
/// Takes ownership of `workspace`.
pub fn mcp_router(workspace: Arc<Workspace>) -> axum::Router {
    let shared: SharedWorkspace = workspace;
    let service = StreamableHttpService::new(
        move || Ok(GraphServer::new(shared.clone())),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    axum::Router::new().nest_service("/mcp", service)
}

/// Serve the graph over the streamable-HTTP transport at `addr`, on the `/mcp`
/// path (for networked, multi-client access; terminate TLS at a reverse proxy).
/// Takes ownership of `workspace`.
///
/// # Errors
/// Returns an error if the runtime cannot start, the address cannot be bound, or
/// the server fails.
pub fn serve_http(workspace: Arc<Workspace>, addr: SocketAddr) -> Result<(), McpError> {
    let router = mcp_router(workspace);
    runtime()?.block_on(async move {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, router).await?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::{
        CheckArgs, ConfigSecretArgs, ContextArgs, CouplingArgs, DebtArgs, DensityArgs, ExplainArgs,
        GraphServer, ListKindArgs, PathArgs, SearchArgs, model_limit,
    };
    use rmcp::ServerHandler;
    use rmcp::handler::server::wrapper::Parameters;
    use std::sync::Arc;

    use rto_graph::{Edge, EdgeKind, FactSet, Node, NodeKind, Store, Workspace};

    fn seeded() -> GraphServer {
        let mut store = Store::open_in_memory().expect("store");
        let mut marker = Node::new("marker:a.rs#7", NodeKind::Marker, "TODO wire this up");
        marker.meta =
            serde_json::json!({ "category": "todo", "text": "TODO wire this up", "line": 7 });
        marker.path = Some("a.rs".into());
        // The `file` node carries the `meta.lines` that `debt_density` divides
        // by, exactly as `extract::file_node` emits it.
        let mut file = Node::new("file:a.rs", NodeKind::File, "a.rs");
        file.path = Some("a.rs".into());
        file.meta = serde_json::json!({ "bytes": 2000, "lines": 100 });
        // A secret-named config key, redacted by extraction, plus one that is not
        // secret-named — the `config_secrets` inventory's two cases.
        let cfg = |dotted: &str, value: &str| {
            let mut n = Node::new(
                format!("cfgkey:.env#{dotted}"),
                NodeKind::Other("config_key".to_owned()),
                dotted,
            );
            n.path = Some(".env".into());
            n.meta = serde_json::json!({ "key": dotted, "value": value });
            n
        };
        let facts = FactSet::new()
            .with_node(file)
            .with_node(cfg("API_TOKEN", "<redacted>"))
            .with_node(cfg("PORT", "8017"))
            .with_node(Node::new("sym:rust:a.rs#main", NodeKind::Fn, "main"))
            .with_node(Node::new("sym:rust:a.rs#helper", NodeKind::Fn, "helper"))
            .with_node(marker)
            .with_edge(Edge::derived(
                "sym:rust:a.rs#main",
                "sym:rust:a.rs#helper",
                EdgeKind::Calls,
            ))
            .with_edge(Edge::derived(
                "sym:rust:a.rs#main",
                "marker:a.rs#7",
                EdgeKind::Contains,
            ));
        store.apply_factset(&facts).expect("apply");
        GraphServer::new(Arc::new(Workspace::single("test", store)))
    }

    fn text_of(result: &rmcp::model::CallToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.clone()))
            .collect()
    }

    #[tokio::test]
    async fn explain_tool_returns_graph_json() {
        let server = seeded();
        let out = server
            .explain(Parameters(ExplainArgs {
                key: "sym:rust:a.rs#main".into(),
                project: None,
            }))
            .await;
        let text = text_of(&out);
        let json: serde_json::Value = serde_json::from_str(&text).expect("json");
        assert_eq!(json["node"]["key"], "sym:rust:a.rs#main");
        assert_eq!(json["outgoing"][0]["node"], "sym:rust:a.rs#helper");
        assert_eq!(json["outgoing"][0]["provenance"], "derived");
    }

    #[tokio::test]
    async fn list_kind_tool_lists_nodes() {
        let server = seeded();
        let out = server
            .list_kind(Parameters(ListKindArgs {
                kind: "fn".into(),
                project: None,
            }))
            .await;
        let text = text_of(&out);
        assert!(text.contains("sym:rust:a.rs#helper"));
        assert!(text.contains("sym:rust:a.rs#main"));
    }

    #[tokio::test]
    async fn search_tool_finds_nodes_by_text() {
        let server = seeded();
        let out = server
            .search(Parameters(SearchArgs {
                query: "helper".into(),
                limit: None,
                project: None,
            }))
            .await;
        let text = text_of(&out);
        assert!(text.contains("sym:rust:a.rs#helper"), "{text}");
    }

    #[tokio::test]
    async fn explain_missing_node_is_not_an_error() {
        let server = seeded();
        let out = server
            .explain(Parameters(ExplainArgs {
                key: "sym:rust:a.rs#ghost".into(),
                project: None,
            }))
            .await;
        assert!(text_of(&out).contains("no node with key"));
    }

    #[tokio::test]
    async fn path_tool_returns_connecting_path() {
        let server = seeded();
        let out = server
            .path(Parameters(PathArgs {
                from: "sym:rust:a.rs#main".into(),
                to: "sym:rust:a.rs#helper".into(),
                project: None,
            }))
            .await;
        let json: serde_json::Value = serde_json::from_str(&text_of(&out)).expect("json");
        assert_eq!(json["found"], true);
        assert_eq!(json["length"], 1);
        assert_eq!(json["hops"][0]["node"], "sym:rust:a.rs#helper");
        assert_eq!(json["hops"][0]["provenance"], "derived");
    }

    #[tokio::test]
    async fn debt_tool_lists_and_filters_markers() {
        let server = seeded();
        // No filter: the seeded marker is reported and counted.
        let all = text_of(&server.debt(Parameters(DebtArgs::default())).await);
        let json: serde_json::Value = serde_json::from_str(&all).expect("json");
        assert_eq!(json["total"], 1);
        assert_eq!(json["by_category"]["todo"], 1);
        assert_eq!(json["items"][0]["key"], "marker:a.rs#7");
        assert_eq!(json["items"][0]["line"], 7);

        // A non-matching category filter yields nothing.
        let none = text_of(
            &server
                .debt(Parameters(DebtArgs {
                    kind: vec!["stub".into()],
                    project: None,
                }))
                .await,
        );
        let json: serde_json::Value = serde_json::from_str(&none).expect("json");
        assert_eq!(json["total"], 0);
    }

    #[tokio::test]
    async fn debt_density_tool_normalises_by_file_length() {
        let server = seeded();
        let out = text_of(
            &server
                .debt_density(Parameters(DensityArgs::default()))
                .await,
        );
        let json: serde_json::Value = serde_json::from_str(&out).expect("json");
        assert_eq!(json["order"], "density");
        assert_eq!(json["items"][0]["path"], "a.rs");
        assert_eq!(json["items"][0]["markers"], 1);
        assert_eq!(json["items"][0]["lines"], 100, "from the file node: {json}");
        assert_eq!(
            json["items"][0]["per_kloc"], 10.0,
            "1 marker in 100 lines is 10 per 1,000: {json}"
        );
        assert_eq!(json["items"][0]["by_category"]["todo"], 1); // roteiro:ignore

        // The `min_lines` floor is reachable from the tool, and excluding the
        // only file is reported rather than served as an empty ranking.
        let out = text_of(
            &server
                .debt_density(Parameters(DensityArgs {
                    min_lines: Some(500),
                    ..DensityArgs::default()
                }))
                .await,
        );
        let json: serde_json::Value = serde_json::from_str(&out).expect("json");
        assert_eq!(json["short_files"], 1);
        assert_eq!(json["files_with_markers"], 1);
        assert_eq!(json["items"].as_array().map(Vec::len), Some(0));
    }

    #[tokio::test]
    async fn debt_density_tool_errors_rather_than_silently_reordering() {
        // A model told it got `markers` when it got `density` would report that
        // as fact, so an unknown order must surface as a tool error.
        let out = seeded()
            .debt_density(Parameters(DensityArgs {
                order: Some("count".into()),
                ..DensityArgs::default()
            }))
            .await;
        assert_eq!(out.is_error, Some(true), "{out:?}");
        assert!(text_of(&out).contains("unknown order `count`"), "{out:?}");
    }

    #[tokio::test]
    async fn config_secrets_tool_reports_presence_and_state_never_a_value() {
        let out = text_of(
            &seeded()
                .config_secrets(Parameters(ConfigSecretArgs::default()))
                .await,
        );
        let json: serde_json::Value = serde_json::from_str(&out).expect("json");
        assert_eq!(json["config_keys"], 2, "the population: {json}");
        assert_eq!(json["secret_named"], 1, "only `API_TOKEN` is: {json}");
        assert_eq!(json["redacted"], 1);
        assert_eq!(json["unredacted"], 0);
        assert_eq!(json["items"][0]["name"], "API_TOKEN");
        assert_eq!(json["items"][0]["path"], ".env");
        assert_eq!(json["items"][0]["state"], "redacted");

        // Nothing a model could mistake for a value reaches the tool result.
        assert!(
            json["items"][0].get("value").is_none() && !out.contains("<redacted>"),
            "the tool reports presence and state, never a value: {out}"
        );
    }

    #[test]
    fn config_secrets_tool_description_refuses_the_scanner_reading() {
        // The rename is load-bearing, and a model only sees the description. Each
        // limitation must be stated where the model will read it, not only in the
        // Rust doc comment.
        let server = seeded();
        let tool = server
            .tool_router
            .list_all()
            .into_iter()
            .find(|t| t.name == "config_secrets")
            .expect("`config_secrets` advertised");
        let desc = tool.description.as_deref().unwrap_or_default();
        for claim in [
            "NOT A SECRET SCANNER",
            "CANNOT find a hardcoded credential in source code",
            "never sees one",
            "real secret from a placeholder",
            "EMPTY RESULT DOES NOT MEAN THERE ARE NO SECRETS",
        ] {
            assert!(desc.contains(claim), "missing `{claim}` from: {desc}");
        }
    }

    #[tokio::test]
    async fn coupling_tool_separates_the_two_directions() {
        let server = seeded();
        // The seed is `main` → `helper`: identical degree, opposite direction.
        let by_in = text_of(
            &server
                .coupling(Parameters(CouplingArgs {
                    order: Some("fan_in".into()),
                    limit: Some(1),
                    project: None,
                }))
                .await,
        );
        let json: serde_json::Value = serde_json::from_str(&by_in).expect("json");
        assert_eq!(json["order"], "fan_in");
        assert_eq!(json["items"][0]["key"], "sym:rust:a.rs#helper");

        let by_out = text_of(
            &server
                .coupling(Parameters(CouplingArgs {
                    order: Some("fan_out".into()),
                    limit: Some(1),
                    project: None,
                }))
                .await,
        );
        let json: serde_json::Value = serde_json::from_str(&by_out).expect("json");
        assert_eq!(json["items"][0]["key"], "sym:rust:a.rs#main");
    }

    #[tokio::test]
    async fn coupling_tool_errors_rather_than_silently_reordering() {
        // A model told it got `fan_in` when it got `total` would report that as
        // fact, so an unknown order must surface as a tool error.
        let out = seeded()
            .coupling(Parameters(CouplingArgs {
                order: Some("degree".into()),
                limit: None,
                project: None,
            }))
            .await;
        assert_eq!(out.is_error, Some(true), "{out:?}");
        assert!(text_of(&out).contains("unknown order `degree`"), "{out:?}");
    }

    /// The floor decision from issue #393, pinned: `rto_graph` reads `limit == 0`
    /// as unlimited, and **this surface does not offer that reading**. `0` is
    /// clamped to the smallest expressible page, never to an empty result — the
    /// silence #393 is about is not reachable from a tool call.
    #[test]
    fn a_model_limit_of_zero_floors_to_one_page_and_never_to_nothing() {
        // Every (default, max) pair the tools declare, with the two bounds
        // written out as the sizes they must produce.
        for (default, max, largest, page) in
            [(10, 25, 25, 10), (20, 100, 100, 20), (50, 200, 200, 50)]
        {
            assert_eq!(
                model_limit(Some(0), default, max),
                1,
                "0 is the smallest page, not unlimited and not nothing",
            );
            // The ceiling is the reason the floor exists: if `0` meant unlimited
            // it would be the one value that escaped this.
            assert_eq!(model_limit(Some(u32::MAX), default, max), largest);
            assert_eq!(model_limit(None, default, max), page);
            assert_eq!(model_limit(Some(3), default, max), 3);
        }
    }

    /// The advertised contract, not just the clamp behind it (issue #393). A
    /// model reads two things — the parameter schema and the description — and
    /// both must say the same bound the code enforces.
    ///
    /// The schema half is load-bearing and easy to lose: `limit: Option<u32>`
    /// derives `"minimum": 0`, which tells a model `0` is legal while
    /// `model_limit` refuses to honour it. `#[schemars(range(...))]` is what
    /// makes the declaration true, and this test is what keeps it there.
    #[test]
    fn every_limit_tool_advertises_the_bound_it_enforces() {
        let server = seeded();
        let tools = server.tool_router.list_all();
        for (name, max) in [
            ("search", 25u64),
            ("debt_density", 100),
            ("config_secrets", 200),
            ("coupling", 100),
        ] {
            let tool = tools
                .iter()
                .find(|t| t.name == name)
                .unwrap_or_else(|| panic!("`{name}` advertised"));
            let limit = tool
                .input_schema
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
            // And in the prose, because a model reads the description even when
            // it does not validate against the schema.
            let desc = tool.description.as_deref().unwrap_or_default();
            assert!(
                desc.contains(&format!("1-{max}")),
                "`{name}` description must state its range: {desc}",
            );
            assert!(
                desc.contains("no unlimited setting"),
                "`{name}` description must say `0`/unlimited is not offered: {desc}",
            );
        }
    }

    #[tokio::test]
    async fn context_tool_returns_the_bounded_bundle() {
        let server = seeded();
        let out = server
            .context(Parameters(ContextArgs {
                key: "sym:rust:a.rs#main".into(),
                project: None,
            }))
            .await;
        let json: serde_json::Value = serde_json::from_str(&text_of(&out)).expect("json");
        assert_eq!(json["node"]["key"], "sym:rust:a.rs#main");
        assert_eq!(json["edge_cap"], rto_graph::TOOL_CONTEXT_EDGE_CAP);
        assert_eq!(json["truncated"], false);
        // The bounded shape, not the raw one: each direction is an object that
        // states its total, not a bare array a caller could mistake for complete.
        assert_eq!(json["outgoing"]["total"], 2, "{json}");
        assert_eq!(json["outgoing"]["truncated"], false);
        assert!(
            json["outgoing"]["omitted"]
                .as_array()
                .is_some_and(Vec::is_empty)
        );
        assert!(
            json["outgoing"]["edges"]
                .as_array()
                .is_some_and(|a| a.iter().any(|e| e["node"] == "sym:rust:a.rs#helper")),
            "{json}"
        );
        assert!(json["fingerprint"].as_str().is_some_and(|f| !f.is_empty()));
    }

    #[tokio::test]
    async fn context_missing_node_is_not_an_error() {
        let out = seeded()
            .context(Parameters(ContextArgs {
                key: "sym:rust:a.rs#ghost".into(),
                project: None,
            }))
            .await;
        assert_eq!(out.is_error, Some(false), "{out:?}");
        assert!(text_of(&out).contains("no node with key"));
    }

    /// The read-only contract, at the tool boundary. `roteiro context --refresh`
    /// prunes cache entries for deleted nodes; nothing reachable from this tool
    /// may. A miss is the case where the CLI's cached read *would* prune.
    #[tokio::test]
    async fn context_tool_never_writes_to_the_store() {
        let server = seeded();
        server
            .workspace
            .with_store(None, |store| {
                store
                    .context_cache_put("sym:rust:a.rs#ghost", "stale", "{}")
                    .expect("put");
            })
            .expect("store");

        for key in ["sym:rust:a.rs#main", "sym:rust:a.rs#ghost"] {
            server
                .context(Parameters(ContextArgs {
                    key: key.into(),
                    project: None,
                }))
                .await;
        }

        let keys = server
            .workspace
            .with_store(None, |store| store.context_cache_keys().expect("keys"))
            .expect("store");
        assert_eq!(
            keys,
            vec!["sym:rust:a.rs#ghost".to_owned()],
            "a tool read must neither populate nor prune the context cache",
        );
    }

    /// A store with no repository behind it cannot be checked, and the document
    /// must make that unmistakable — `gate: not-run`, and no `report` for a
    /// caller to read `violations: []` out of.
    #[tokio::test]
    async fn check_tool_reports_not_run_rather_than_a_clean_repository() {
        let out = seeded().check(Parameters(CheckArgs::default())).await;
        assert_eq!(
            out.is_error,
            Some(false),
            "not-run is data, not a tool error"
        );
        let json: serde_json::Value = serde_json::from_str(&text_of(&out)).expect("json");
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
                .is_some_and(|r| !r.is_empty()),
            "{json}"
        );
    }

    /// The `check` tool's description is where a model learns that `not-run` is
    /// not a pass. The tool is useless — worse, misleading — if that is only in
    /// the Rust doc comment.
    #[test]
    fn check_tool_description_refuses_the_advisory_reading() {
        let server = seeded();
        let tool = server
            .tool_router
            .list_all()
            .into_iter()
            .find(|t| t.name == "check")
            .expect("`check` advertised");
        let desc = tool.description.as_deref().unwrap_or_default();
        for claim in [
            "READ `gate` FIRST",
            "`not-run` is a real outcome",
            "carries NO `report`",
            "rather than report a clean repository",
        ] {
            assert!(desc.contains(claim), "missing `{claim}` from: {desc}");
        }
    }

    /// `review` is not on this surface, and its absence is a decision rather than
    /// an omission (see the module documentation). A tool added by reflex would
    /// pass every other test in this file; this is the one that notices.
    #[test]
    fn review_is_not_exposed_and_the_reason_is_recorded_here() {
        let server = seeded();
        assert!(
            !server
                .tool_router
                .list_all()
                .iter()
                .any(|t| t.name == "review"),
            "`review` must not be an MCP tool: it is ~435 KB for a three-commit \
             range, and its per-file `debt` cannot apply the target project's \
             `[debt] ignore` from this crate (issue #321). See the module docs.",
        );
    }

    /// No `security` subcommand is a tool, and two of them never can be.
    ///
    /// `ingest` and `run` both call `Store::replace_findings_layer`, and `run`
    /// additionally executes an analyzer — a model asking for a tool is not a
    /// human consenting to execution. `prefetch` opens the network under a human
    /// consent. Completing the set is the failure mode this test exists to
    /// prevent, so it fails on *any* `security*` tool: the two read-only ones
    /// (`list`, `status`) are eligible but need the design step recorded in this
    /// module's documentation, and adding them should mean reading that first.
    #[test]
    fn no_security_subcommand_is_exposed() {
        let server = seeded();
        let security: Vec<String> = server
            .tool_router
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .filter(|n| n.starts_with("security"))
            .collect();
        assert!(
            security.is_empty(),
            "`security ingest`/`run`/`prefetch` are permanent refusals (mutating, \
             executing, network-consented); `list`/`status` are eligible but need \
             the never-run-vs-clean discriminator and a `project` selector first. \
             Found: {security:?}",
        );
    }

    /// The sibling of `every_limit_tool_advertises_the_bound_it_enforces`, for the
    /// tool that bounds its answer **without** a `limit` parameter.
    ///
    /// `context` takes a node key and nothing else — a context bundle is one
    /// node's neighbourhood, and `--refresh` is why it must not grow arguments —
    /// so there is no schema field for the cap to be declared in. The declaration
    /// therefore lives in the description, and that is the only place a model can
    /// read it. This test is what keeps the number there equal to the number the
    /// code enforces; without it the two drift the moment the constant moves.
    #[test]
    fn every_context_tool_states_its_fixed_bound() {
        let server = seeded();
        let tools = server.tool_router.list_all();
        let tool = tools
            .iter()
            .find(|t| t.name == "context")
            .expect("`context` advertised");

        // No negotiable bound: the argument that does not exist cannot disagree
        // with the one enforced.
        let props = tool
            .input_schema
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .expect("`context` declares properties");
        assert!(
            props.get("limit").is_none(),
            "`context` must not advertise a `limit` it does not honour: {props:?}",
        );
        assert!(
            props.get("refresh").is_none(),
            "`context` must never offer `--refresh`: it prunes, and this surface is \
             read-only",
        );
        assert!(props.contains_key("key"), "{props:?}");

        // The enforced bound, stated where a model will read it.
        let desc = tool.description.as_deref().unwrap_or_default();
        assert!(
            desc.contains(&format!(
                "at most {} edges",
                rto_graph::TOOL_CONTEXT_EDGE_CAP
            )),
            "`context` description must state the cap it enforces: {desc}",
        );
        for claim in ["BOUNDED", "`truncated` is true", "`omitted`"] {
            assert!(
                desc.contains(claim),
                "`context` must say it reports its truncation (`{claim}`): {desc}",
            );
        }
    }

    #[test]
    fn get_info_advertises_tools() {
        let server = seeded();
        let info = server.get_info();
        assert_eq!(info.server_info.name, "roteiro");
        assert!(info.capabilities.tools.is_some());
    }

    /// Create a git repo at `dir` whose graph holds a single struct node `key`.
    fn repo_with_node(dir: &std::path::Path, key: &str) {
        std::fs::create_dir_all(dir).unwrap();
        let status = std::process::Command::new("git")
            .args(["-c", "init.defaultBranch=main", "init", "-q"])
            .current_dir(dir)
            .status()
            .expect("run git");
        assert!(status.success(), "git init failed in {}", dir.display());
        let store_dir = dir.join(".git").join("roteiro");
        std::fs::create_dir_all(&store_dir).unwrap();
        let mut store = Store::open(&store_dir.join("graph.db")).unwrap();
        store
            .apply_factset(&FactSet::new().with_node(Node::new(key, NodeKind::Struct, key)))
            .unwrap();
    }

    /// The `check` tool end to end over a real hosted project: the workspace
    /// resolves the project's own repository, the authored layer is read from its
    /// `HEAD` tree, and the verdict is a full report rather than a refusal.
    ///
    /// The in-memory `seeded()` workspace can only ever produce `not-run`, so
    /// without this the plumbing from `project_root` to the drift rule is untested
    /// and a `check` that always refused would look correct.
    #[tokio::test]
    async fn check_tool_runs_against_a_hosted_projects_own_repository() {
        let base = std::env::temp_dir().join(format!("rto-mcp-check-{}", std::process::id()));
        std::fs::remove_dir_all(&base).ok();
        let dir = base.join("app");
        std::fs::create_dir_all(&dir).unwrap();
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
        std::fs::create_dir_all(dir.join("docs/adr")).unwrap();
        std::fs::write(dir.join("a.rs"), "pub struct Store;\n").unwrap();
        // One ADR whose `[[…]]` link resolves, and one whose link does not.
        let adr = |id: &str, target: &str| {
            format!(
                "---\nadr-id: \"{id}\"\nstatus: Accepted\n---\n\n# ADR-{id}\n\n\
                 ## Design\n\nUses [[{target}]].\n"
            )
        };
        std::fs::write(dir.join("docs/adr/0001.md"), adr("0001", "a.rs#Store")).unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "seed"]);

        // A graph recorded as synced from that exact `HEAD` tree — the state a
        // committed `roteiro sync` leaves, and `check`'s precondition.
        let tree = rto_graph::Repo::discover(&dir)
            .unwrap()
            .head_tree_id()
            .unwrap();
        let store_dir = dir.join(".git").join("roteiro");
        std::fs::create_dir_all(&store_dir).unwrap();
        let mut store = Store::open(&store_dir.join("graph.db")).unwrap();
        store
            .rebuild(
                &FactSet::new()
                    .with_node(Node::new("file:a.rs", NodeKind::File, "a.rs"))
                    .with_node(Node::new("sym:rust:a.rs#Store", NodeKind::Struct, "Store")),
                Some(&tree),
            )
            .unwrap();
        drop(store);

        let ws = Workspace::from_repo_paths([dir.clone()]).unwrap();
        let server = GraphServer::new(Arc::new(ws));

        let out = server.check(Parameters(CheckArgs::default())).await;
        let json: serde_json::Value = serde_json::from_str(&text_of(&out)).expect("json");
        assert_eq!(json["gate"], "pass", "{json}");
        assert_eq!(json["report"]["adrs"], 1, "{json}");
        assert_eq!(json["report"]["links_ok"], 1, "{json}");
        assert_eq!(json["checked_against"]["source"], "committed");
        assert_eq!(json["checked_against"]["tree"], tree);
        assert!(json.get("not_run_reason").is_none(), "{json}");

        // Break the link on disk and commit: the same call must now fail the gate
        // and name the drift, not merely flip a boolean.
        std::fs::write(dir.join("docs/adr/0001.md"), adr("0001", "a.rs#Ghost")).unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "drift"]);
        // The graph must still describe HEAD, or `check` would (rightly) refuse.
        let tree = rto_graph::Repo::discover(&dir)
            .unwrap()
            .head_tree_id()
            .unwrap();
        let mut store = Store::open(&store_dir.join("graph.db")).unwrap();
        store
            .rebuild(
                &FactSet::new()
                    .with_node(Node::new("file:a.rs", NodeKind::File, "a.rs"))
                    .with_node(Node::new("sym:rust:a.rs#Store", NodeKind::Struct, "Store")),
                Some(&tree),
            )
            .unwrap();
        drop(store);

        let ws = Workspace::from_repo_paths([dir.clone()]).unwrap();
        let out = GraphServer::new(Arc::new(ws))
            .check(Parameters(CheckArgs::default()))
            .await;
        let json: serde_json::Value = serde_json::from_str(&text_of(&out)).expect("json");
        assert_eq!(json["gate"], "fail", "{json}");
        assert_eq!(
            json["report"]["violations"][0]["kind"], "broken-link",
            "{json}"
        );
        assert!(
            json["report"]["violations"][0]["message"]
                .as_str()
                .is_some_and(|m| m.contains("Ghost")),
            "{json}"
        );

        std::fs::remove_dir_all(&base).ok();
    }

    #[tokio::test]
    async fn explain_follows_a_project_qualified_key() {
        // A two-project workspace; each node exists only in its own repo.
        let base = std::env::temp_dir().join(format!("rto-mcp-xrepo-{}", std::process::id()));
        std::fs::remove_dir_all(&base).ok();
        repo_with_node(&base.join("app"), "sym:rust:a.rs#OnlyInApp");
        repo_with_node(&base.join("deploy"), "sym:rust:b.rs#OnlyInDeploy");
        let ws = Workspace::from_repo_paths([base.join("app"), base.join("deploy")]).unwrap();
        let server = GraphServer::new(Arc::new(ws));

        // A project-qualified key follows the link into `app` — even though the
        // `project` argument names `deploy`, the qualifier wins.
        let out = server
            .explain(Parameters(ExplainArgs {
                key: "app::sym:rust:a.rs#OnlyInApp".into(),
                project: Some("deploy".into()),
            }))
            .await;
        let json: serde_json::Value = serde_json::from_str(&text_of(&out)).expect("json");
        assert_eq!(json["node"]["key"], "sym:rust:a.rs#OnlyInApp");

        // A bare key still honours the `project` argument.
        let out = server
            .explain(Parameters(ExplainArgs {
                key: "sym:rust:b.rs#OnlyInDeploy".into(),
                project: Some("deploy".into()),
            }))
            .await;
        let json: serde_json::Value = serde_json::from_str(&text_of(&out)).expect("json");
        assert_eq!(json["node"]["key"], "sym:rust:b.rs#OnlyInDeploy");

        std::fs::remove_dir_all(&base).ok();
    }
}

// roteiro:ignore-file — the `debt` tool's own description and tests name the
// intent-debt vocabulary (todo/fixme/stub/deferred); not real debt here.
//! Model Context Protocol server exposing the query surface to agents, behind
//! the `mcp` feature, built on the official [`rmcp`] SDK.
//!
//! Two transports are offered (see [`serve_stdio`] / [`serve_http`]): stdio for
//! a local agent-spawned subprocess, and streamable-HTTP for networked,
//! multi-client serving (terminate TLS at a reverse proxy). Both expose the
//! same tools — `search`, `explain`, `context`, `check`, `list_kind`, `path`,
//! `debt`, `debt_density`, `coupling`, `config_secrets`, `list_projects`,
//! `list_tool_classes`, and (with
//! the `execution` feature) `security_list` / `security_status` — as thin wrappers
//! over the
//! matching [`rto_graph`] query primitives, so agents and the CLI see the same
//! graph. Each tool takes an optional `project` selector for a multi-repo
//! workspace (ADR-0008). See ADR-0002 for the decision to adopt `rmcp`.
//!
//! # Every tool here answers from the graph and none of them changes it
//!
//! A tool call answers from the graph; it never rebuilds or mutates it. That is
//! why `context` here takes a node key and nothing else — `roteiro context
//! --refresh` rebuilds stale cached bundles and *prunes* entries for deleted
//! nodes, and the CLI keeps that on a maintenance seam precisely so ordinary
//! reads do not mutate the store (ADR-0013). It is also why `check` refuses,
//! rather than rebuilds, when the graph does not match `HEAD`.
//!
//! This heading used to read *every tool here is READ-ONLY*, and `sandbox_clear`
//! is why it no longer can. **The rule the read-only stance was protecting is
//! that a model must not change what the graph says**, and that rule is
//! unweakened: `sandbox_clear` deletes cached container images, changes nothing
//! the graph says, and makes the next sandboxed run slower and nothing else.
//! ADR-0014 v1.6 admits it as this surface's first mutating tool and states the
//! permission positively so it cannot become a precedent by extension: **a tool
//! may drop state that is re-obtainable from a pinned digest, and may drop
//! nothing else.** That is the same test that makes the CLI verb safe, applied
//! unchanged — so the next candidate has to pass it rather than cite this one.
//!
//! Two obligations come with it, and both are checked by test rather than
//! trusted. It **reports what it freed** (`freed_bytes`, and the store's size
//! before and after), so an 8.5 GB re-pull appears in the transcript rather than
//! turning up later as an unexplained slow run. And it is **not offered a scope
//! it cannot justify**: `image` and `everything` are different arguments, neither
//! defaults, and supplying nothing is a refusal rather than the destructive
//! reading of silence.
//!
//! `security run` remains refused, and the contrast is the point: it writes a
//! findings layer — a change to what Roteiro reports about your code — *and* it
//! executes an analyzer. Sandboxing does not make it read-only, because
//! `execute_and_file` is on the shared path of both its backends.
//!
//! # `roteiro sandbox`, subcommand by subcommand
//!
//! | subcommand | on this surface | why |
//! | --- | --- | --- |
//! | `sandbox status` | **`sandbox_status`** | read-only; what the machine-global image store is holding, per image, with sizes |
//! | `sandbox clear` | **`sandbox_clear`** | mutating, and admitted: everything it drops is re-obtainable from a pinned digest (ADR-0014 v1.6) |
//!
//! Neither takes a `project`. The sandbox store is **machine-global** — one per
//! asset root, shared by every repository the server hosts — and unlike
//! `security_status` there is no per-repository half for a selector to choose
//! between. Both documents carry `scope: "machine"` anyway, because a document
//! that gets quoted has to bring its scope with it.
//!
//! # `roteiro security`, subcommand by subcommand
//!
//! The read-only rule decides most of this surface on its own, and the security
//! subcommands are where it has the sharpest teeth. Written out so the set cannot
//! be quietly completed later by someone who only sees that two of them are here:
//!
//! | subcommand | on this surface | why |
//! | --- | --- | --- |
//! | `security list` | **`security_list`** — see the note below | read-only over stored findings; the analogue of `debt` |
//! | `security status` | **`security_status`** — see the note below | read-only; asset digests, advisory-DB age, analyzer coverage |
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
//! `list` and `status` are here, as `security_list` and `security_status`, behind
//! `all(feature = "mcp", feature = "execution")` (issue #435). Three things had to
//! be settled before either could be correct rather than merely available, and the
//! record of how is kept because each one is a way to get this wrong again:
//!
//! - **An empty listing states neither fact.** `security list --json` is 36 bytes
//!   on a repository no analyzer has ever run against: `{"layers": [], "findings":
//!   0}`. "No analyzer has ever run" and "an analyzer ran and found nothing" are
//!   opposite facts, and `findings: 0` reads as the second while meaning the
//!   first. The data does distinguish them (a clean run leaves a layer with no
//!   findings; never running leaves no layer), but the *document* does not say so.
//!   The fix is the shape `check` already uses here, and it is now
//!   [`rto_exec::Coverage`]: a discriminator that is always present, and the
//!   payload omitted entirely in the case that has no answer. So `coverage:
//!   "no-analyzer-on-record"` carries **no `report` at all**, while a clean run is
//!   `coverage: "analyzed"` with `findings: 0`.
//! - **`security status` does not take a project.** `run_security_status` reads
//!   the store via `open_graph()`, which discovers a repository from the
//!   *process's* working directory, while every tool on this surface selects one
//!   with `project` (ADR-0008). Its asset half is machine-global
//!   (`rto_exec::asset_root()`) and its `layers` half is per-repository, so the
//!   tool has to say which of the two each half describes before it can be
//!   correct in a multi-project workspace. `security_status` therefore returns
//!   **two named sections** — `machine` and `repository` — each carrying an
//!   explicit `scope`, with the asset root inside the first and the resolved
//!   project name inside the second. A model that quotes one section still carries
//!   its scope; a doc comment could not have achieved that, because a model does
//!   not read this file.
//! - **`security list` is unbounded, and every other tool here is not.** A
//!   listing is every finding in every layer, and a tool result is spent against a
//!   context window — so it takes `limit`, clamped by [`model_limit`] like every
//!   ranking here, with `"minimum": 1` declared in the schema because `0` means
//!   *unlimited* on the `rto_graph` surfaces and this one must not offer that. The
//!   bound is **per layer** rather than per document: a document-wide bound spends
//!   its budget on the first layer in key order and reports the second as empty,
//!   which reads as "that analyzer found nothing" — the first bullet's defect,
//!   arrived at from the other direction. See [`rto_exec::security_list`] for why
//!   the page is ordered by severity when the store's listing is not.
//!
//! Both are thin wrappers, as every tool here is: the documents are built by
//! [`rto_exec::tool_security`], which the CLI's `security status` also draws its
//! coverage matrix and staleness rows from. That shared home is the point rather
//! than a convenience — `possibly_stale` and `ready` are judgements about
//! evidence, and issue #321 is what three copies of one judgement costs.
//!
//! This crate gains its own `execution` feature to reach those types, and the
//! `roteiro` binary's `execution` feature forwards it. That forwarding is
//! load-bearing: the served-chat registry's two `security_*` tools are gated on
//! `all(serve, execution)` and these on `all(mcp, execution)`, so the pair appears
//! and disappears on both surfaces together. `both_tool_surfaces_offer_the_same_tools`
//! in the `roteiro` binary is what fails if that ever comes apart.
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
//! **It would report a different debt figure.** `review`'s per-file output
//! carries `debt`, and this crate cannot read the target project's
//! `roteiro.toml` (see the note on the `debt` tool below), so it could not apply
//! that project's `[debt] ignore`. Issue #321 was exactly this defect — one
//! concept reporting different numbers on different surfaces — and it had already
//! recurred across three of them before it was fixed, then again on a fourth
//! (`_Home`, issue #372) that the fix had missed, and a fifth (`roteiro review`,
//! issue #409) that both fixes had missed. Adding a surface that reports another
//! number for the same repository, on the strength of it being convenient, is how
//! that recurs. Exposing `review` means first giving this crate a way to reach
//! the project's config; until then, the honest answer is that the review surface
//! is CLI-first (`roteiro review [--json]`), needs no server, and works in any
//! agent.
//!
//! Worth knowing before anyone wires this up: `roteiro review` **does** apply
//! `[debt] ignore`, as of issue #409 — `Command::Review` is handed the list and
//! `review::build` keeps only the markers `rto_graph::debt` retains under it. The
//! gap here is this crate's missing config access, not a defect in the CLI that
//! would excuse duplicating it.

use std::collections::BTreeSet;
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

// ------------------------------------------------- tool arguments ---
// **Every argument struct below is `#[serde(deny_unknown_fields)]`, and that
// is a property of the surface rather than a preference about strictness.**
//
// An argument key nothing recognises used to be dropped, which is not a
// harmless leniency: this surface spelled `debt`'s category filter `kind` while
// the served `/v1` registry spelled it `categories`, so a model reaching `debt`
// here with the other surface's spelling was handed **every marker in the
// repository, presented as the filtered set it asked for** — a wrong answer
// wearing a correct answer's clothes, with nothing in it to tell them apart.
// The same is true of any typo or invented key: the model asked a narrower
// question than the one it got an answer to.
//
// Refusing closes the class rather than that one instance, and serde's own
// message is close to ideal for it because it names the keys that *are*
// accepted ("unknown field `kind`, expected one of `categories`, `project`").
// rmcp reaches the caller with it as a JSON-RPC `invalid_params` error —
// `Parameters<P>` deserialises the call's `arguments` object and nothing else,
// so this is safe against the protocol: `_meta`, `input_responses` and
// `request_state` are siblings of `arguments` in `CallToolRequestParams`, never
// members of it, and rmcp inserts nothing of its own into the object it hands
// over.
//
// schemars renders the attribute as `"additionalProperties": false` in each
// advertised `inputSchema`, so a client sees the same rule the server enforces;
// `every_tool_closes_its_argument_object` is what keeps a new struct from
// arriving without it. The `/v1` surface states and enforces the same rule
// through `rto_serve::tools::unknown_argument`.
//
// The two surfaces now spell that argument the same way — `categories`, on both
// — and the refusal above is what made the rename safe to take. While unknown
// keys were dropped, renaming `kind` here would have left every existing caller
// parsing fine and quietly receiving *unfiltered* results, which is the worst
// available migration. Refusing first turns the same call into a message that
// names the new key, so the break is loud and self-explaining rather than
// silent. `both_surfaces_name_a_shared_tools_arguments_the_same_way` in
// `roteiro` is what keeps them from drifting apart again.

/// Arguments for the `explain` tool.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ExplainArgs {
    /// Node key, e.g. `sym:rust:<path>#<Name>`, `file:<path>`, or `adr:<id>`.
    key: String,
    /// Which hosted project to query, when this server hosts several (ADR-0008);
    /// omit for a single-project server. See `list_projects`.
    #[serde(default)]
    project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
struct ListKindArgs {
    /// Node kind token, e.g. `fn`, `struct`, `adr`, `file`.
    kind: String,
    /// Which hosted project to query (see `list_projects`); omit if single.
    #[serde(default)]
    project: Option<String>,
}

/// Arguments for the `path` tool.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
struct ContextArgs {
    /// Node key, e.g. `sym:rust:<path>#<Name>`, `file:<path>`, or `adr:<id>`.
    key: String,
    /// Which hosted project to query (see `list_projects`); omit if single.
    #[serde(default)]
    project: Option<String>,
}

/// Arguments for the `check` tool.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CheckArgs {
    /// Which hosted project to check (see `list_projects`); omit if single.
    #[serde(default)]
    project: Option<String>,
}

/// Arguments for the `debt` tool.
///
/// `categories`, not `kind`: the served `/v1` registry already spelled it that
/// way, the values are marker *categories* rather than node kinds, and `kind` is
/// taken on this very surface for something else — [`ListKindArgs::kind`] is a
/// node-kind token (`fn`, `struct`, `adr`, `file`). One word meaning two things
/// across neighbouring tools is a real ambiguity for a model choosing arguments,
/// and this doc comment used to say "categories" directly above a field called
/// `kind`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DebtArgs {
    /// Restrict to these categories (empty = all): todo, fixme, hack, stub,
    /// deferred.
    #[serde(default)]
    categories: Vec<String>,
    /// Which hosted project to query (see `list_projects`); omit if single.
    #[serde(default)]
    project: Option<String>,
}

/// Arguments for the `debt_density` tool.
///
/// `categories` for the reason [`DebtArgs`] gives — it is the same filter, and
/// it reaches the same `rto_graph` parameter, which has always been called
/// `categories` there.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DensityArgs {
    /// Restrict to these categories (empty = all): todo, fixme, hack, stub,
    /// deferred.
    #[serde(default)]
    categories: Vec<String>,
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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

/// Arguments for the `security_list` tool.
#[cfg(feature = "execution")]
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SecurityListArgs {
    /// Restrict to one analyzer's layer (`cargo-audit`, `osv-scanner`,
    /// `semgrep`). An unknown name is an error, not an empty listing.
    #[serde(default)]
    analyzer: Option<String>,
    /// Max findings **per layer** (default 20, clamped to 1..=100 — this surface
    /// has no "unlimited": see `model_limit`).
    #[serde(default)]
    #[schemars(range(min = 1, max = 100))]
    limit: Option<u32>,
    /// Which hosted project to query (see `list_projects`); omit if single.
    #[serde(default)]
    project: Option<String>,
}

/// Arguments for the `security_status` tool.
///
/// No `limit`, and that is a property of the answer rather than an omission: a
/// status document carries one row per shipped analyzer, one per pinned asset, and
/// one per live findings layer — **counts, never findings** — so its size is fixed
/// by what is installed rather than by how much was found. There is no schema
/// field for a bound to be declared in, so
/// `security_status_states_why_it_needs_no_bound` is what keeps the description
/// honest about it, exactly as `every_context_tool_states_its_fixed_bound` does for
/// `context`.
#[cfg(feature = "execution")]
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SecurityStatusArgs {
    /// Restrict both halves to one analyzer (`cargo-audit`, `osv-scanner`,
    /// `semgrep`). An unknown name is an error.
    #[serde(default)]
    analyzer: Option<String>,
    /// Which hosted project's layers to report (see `list_projects`); omit if
    /// single. It selects the `repository` half only — the `machine` half is the
    /// same whichever project is named.
    #[serde(default)]
    project: Option<String>,
}

/// Arguments for the `list_projects` tool: none.
///
/// Declared rather than left implicit. A `#[tool]` handler that takes no
/// [`Parameters`] gets rmcp's `schema_for_empty_input`, which says `properties:
/// {}` and stops there — an argument object with nothing in it and nothing
/// forbidden — so a call carrying a key would have been read as a bare one. That
/// is the same silent drop the rest of this surface now refuses, and this is
/// what puts the tool under the rule: no argument means the empty set, not any
/// set. `every_tool_closes_its_argument_object` found both of these.
///
/// No `project`, in particular: this **is** the tool that answers what the
/// project names are, so a selector would be circular.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListProjectsArgs {}

/// Arguments for the `list_tool_classes` tool: none. Declared for the reason
/// [`ListProjectsArgs`] gives.
///
/// No `project` either: a class index is a property of how this **server** was
/// started, and is the same document whichever hosted project is asked about.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListToolClassesArgs {}

/// Arguments for the `sandbox_status` tool: none.
///
/// No `project`, because the sandbox image store is machine-global — one per
/// asset root, shared by every repository this server hosts — and there is no
/// per-repository half for a selector to choose between. No `limit` either: this
/// is one row per cached image, so its size is fixed by what has been pulled
/// rather than by how much was found.
#[cfg(feature = "execution")]
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SandboxStatusArgs {}

/// Arguments for the `sandbox_clear` tool: **two selectors, and neither has a
/// default**.
///
/// ADR-0014 v1.6's second obligation for the first mutating tool on this surface.
/// "Clear this image" and "clear everything" are different requests, so they are
/// different arguments — a model asking for one must not be able to receive the
/// other, and *supplying neither must not resolve to either*, least of all to the
/// destructive one. Both are therefore `Option`/`Option<bool>` with an explicit
/// refusal behind them rather than a `bool` that reads `false` as "the other one".
#[cfg(feature = "execution")]
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SandboxClearArgs {
    /// Drop this image, by the `reference` `sandbox_status` lists it under.
    #[serde(default)]
    image: Option<String>,
    /// Drop every cached image, and the bytes under the store root no image
    /// claims. Mutually exclusive with `image`; supplying neither is an error.
    #[serde(default)]
    everything: Option<bool>,
    /// Report what would be removed and remove nothing. The result's `applied`
    /// field says which of the two happened.
    #[serde(default)]
    dry_run: Option<bool>,
}

/// Validate a model-supplied `analyzer` against the shipped adapter set.
///
/// An unknown name is a **tool error**, not a listing with nothing in it. That is
/// the difference between the CLI and this surface: on the CLI an unrecognised
/// `--analyzer` prints "no findings ingested for `<name>`" to the person who typed
/// it and can see their own typo, whereas a document saying no result is on record
/// for a name a model invented reads as "that analyzer has never been run here".
/// It is the same rule the `order` arguments follow — a model told something that
/// is not so will state it as fact — and here the fact in question is a security
/// result.
#[cfg(feature = "execution")]
fn checked_analyzer(given: Option<&str>) -> Result<Option<&str>, String> {
    match given {
        None => Ok(None),
        Some(name) if rto_exec::known_analyzers().contains(&name) => Ok(Some(name)),
        Some(name) => Err(format!(
            "unknown analyzer `{name}` (expected {})",
            rto_exec::known_analyzers().join("|")
        )),
    }
}

/// The MCP server handler over a [`Workspace`] of one or more project graphs
/// (ADR-0008). Every tool takes an optional `project` selector and a
/// `list_projects` tool enumerates the hosted projects; a single-project
/// workspace resolves that sole project for a bare call, so it serves as before.
#[derive(Clone)]
struct GraphServer {
    workspace: SharedWorkspace,
    /// The pinned-asset cache this server reports on and, for `sandbox_clear`,
    /// deletes from — **held rather than resolved at the call**.
    ///
    /// It used to be `rto_exec::asset_root()` read inside each handler, which was
    /// harmless while every tool was read-only and is not any more. A test that
    /// exercises `sandbox_clear` cannot redirect an ambient read: `asset_root`
    /// resolves from the process environment and `unsafe_code = "forbid"` rules
    /// out `std::env::set_var`, so the only asset root such a test could ever
    /// reach is the developer's own. **This is not hypothetical** — fault-injecting
    /// `sandbox_clear_refuses_a_scope_it_was_not_given`, to prove that test
    /// catches a missing refusal, cleared the 8.7 GB store on the machine this
    /// was written on. The bytes were re-obtainable, which is the whole ADR-0014
    /// v1.6 argument; a test reaching outside its fixture at all is the defect.
    ///
    /// So the root is a field, `new` fills it from the environment exactly as
    /// before, and the tests point it somewhere disposable.
    #[cfg(feature = "execution")]
    asset_root: std::path::PathBuf,
    /// The advertised **and** dispatchable set, built once by
    /// [`GraphServer::routes`] and read by the `#[tool_handler]`-generated
    /// `list_tools`, `call_tool` and `get_tool`. One object, so a restriction
    /// cannot reach the listing and miss the dispatch (issue #584).
    tool_router: ToolRouter<Self>,
}

impl GraphServer {
    fn new(workspace: SharedWorkspace, advertised: &Advertised) -> Self {
        Self {
            workspace,
            #[cfg(feature = "execution")]
            asset_root: rto_exec::asset_root(),
            tool_router: Self::routes(advertised),
        }
    }

    /// Point this server's asset-cache reads and removals at `root`.
    ///
    /// Test-only, and it is what lets a test exercise the one tool here that
    /// deletes without deleting the developer's cache — see
    /// [`GraphServer::asset_root`].
    #[cfg(all(test, feature = "execution"))]
    fn with_asset_root(mut self, root: std::path::PathBuf) -> Self {
        self.asset_root = root;
        self
    }

    /// Every route this server advertises: the always-present graph tools, plus
    /// the `security_*` and `sandbox_*` tools when this build has `execution`.
    ///
    /// The `security_*` pair lives in a **second** `#[tool_router]` block because
    /// the macro emits one `.with_route(Self::<handler>)` per `#[tool]` fn
    /// unconditionally — a `#[cfg]` on the handler would leave the generated
    /// router referring to a method that does not exist. Two routers merged here
    /// is the composition rmcp offers for exactly this, and it keeps the cfg on
    /// the whole feature-gated block rather than sprinkled through one.
    ///
    /// This is also the single definition of the advertised set: `new` stores it,
    /// `tool_names` reads it, and `#[tool_handler(router = self.tool_router)]`
    /// dispatches against **that stored value** — so a tool cannot be listed and
    /// unroutable, or routable and unlisted.
    ///
    /// # Why the handler reads the field rather than calling this again
    ///
    /// It used to be `#[tool_handler(router = Self::routes())]`, which rebuilt an
    /// unrestricted router on every `tools/list` and every `tools/call` and never
    /// looked at the field at all. That was harmless while the two could not
    /// differ. With `advertised` (issue #584) they can, and the difference would
    /// have been exactly the failure this feature exists to prevent: a restricted
    /// `tools/list` over a router that still dispatched all fifteen. Advertisement
    /// is not authority, so both halves now read one object.
    fn routes(advertised: &Advertised) -> ToolRouter<Self> {
        let routes = Self::tool_router();
        #[cfg(feature = "execution")]
        let routes = routes + Self::security_tool_router();
        #[cfg(feature = "execution")]
        let routes = routes + Self::sandbox_tool_router();

        // Descriptions come from [`crate::tool_text`], which is the only place
        // they are written. They cannot come from the `#[tool]` attribute: that
        // is parsed by `darling` into a `String`, so it takes a string literal
        // and rejects a path — `description = SEARCH` does not compile. Setting
        // them here instead is what lets the literals be deleted rather than
        // duplicated and policed by a test.
        //
        // This is the one funnel: the three `*_tool_router()` calls above are
        // combined nowhere else, so nothing reaches a client without passing
        // through here.
        let mut routes = routes;
        for route in routes.map.values_mut() {
            if let Some(text) = crate::tool_text::for_tool(&route.attr.name) {
                route.attr.description = Some(text.into());
            }
        }
        let Advertised::Only(allowed) = advertised else {
            return routes;
        };
        // `remove_route` rather than `disable_route`: a disabled route stays in
        // the map and its name stays in the disabled set, so a later `merge`
        // could resurrect it. A removed one is gone from both `list_all` and
        // `call`, which is the guarantee the operator asked for.
        let mut routes = routes;
        for name in routes
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .filter(|name| !allowed.contains(name))
            .collect::<Vec<_>>()
        {
            routes.remove_route(&name);
        }
        routes
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
    #[tool]
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
    #[tool]
    async fn search(&self, Parameters(args): Parameters<SearchArgs>) -> CallToolResult {
        let limit = model_limit(args.limit, 10, 25);
        query_result(self.with_project(args.project.as_deref(), |store| {
            search(store, &args.query, limit)
        }))
    }

    /// A node's bounded, read-only context bundle.
    #[tool]
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
    #[tool]
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
    #[tool]
    async fn path(&self, Parameters(args): Parameters<PathArgs>) -> CallToolResult {
        // A path lives within one graph: a qualified `from` selects the project,
        // and a qualifier on either endpoint is stripped to a bare, in-store key.
        let (proj, from_bare) = qualified_or(&args.from, args.project.as_deref());
        let to_bare = rto_graph::parse_qualified(&args.to)
            .map_or_else(|| args.to.clone(), |(_, b)| b.to_owned());
        query_result(self.with_project(proj.as_deref(), |store| path(store, &from_bare, &to_bare)))
    }

    /// List intent-debt markers (TODOs, stubs, deferred work).
    #[tool]
    async fn debt(&self, Parameters(args): Parameters<DebtArgs>) -> CallToolResult {
        // `ignore` is empty by necessity, not by oversight: this crate has no
        // access to the target project's `roteiro.toml`, so there is no list to
        // apply. Every surface that *can* reach the config does — the
        // enumeration is on `debt_density` below.
        query_result(self.with_project(args.project.as_deref(), |store| {
            debt(store, &args.categories, &[])
        }))
    }

    /// Rank files by intent-debt density (markers per 1,000 lines).
    #[tool]
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
        // CLI (`debt`, `debt-density`, `check`, `review`), the graph API, the
        // served-chat tool registry and the Obsidian `_Home` overview all apply
        // the project's own `[debt] ignore`. That is the complete list, written
        // out rather than summarised because summarising is how it goes stale:
        // `_Home` was missed when the defect was fixed on the surfaces that
        // happened to be reported (#321, #372), and `review` was then missed by
        // both — while this very list, omitting it, read as though the set were
        // settled (#409). Adding a surface means adding it here. An MCP client
        // sees the unfiltered inventory.
        query_result(self.with_project(args.project.as_deref(), |store| {
            rto_graph::debt_density(store, &args.categories, &[], order, limit, min_lines)
        }))
    }

    /// Inventory secret-named config keys and their redaction state.
    #[tool]
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
    #[tool]
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
    #[tool]
    async fn list_projects(
        &self,
        Parameters(_args): Parameters<ListProjectsArgs>,
    ) -> CallToolResult {
        json_result(&serde_json::json!({ "projects": self.workspace.names() }))
    }

    /// Name the tool classes, and say which of them this server loaded (#664).
    ///
    /// The one tool [`restrict`] never withholds, and the reason it exists: a
    /// class an operator did not load is invisible from the client side, and an
    /// invisible tool and an impossible one look identical to a model. Without
    /// this, a `query`-only server answers "Roteiro cannot report analyzer
    /// findings" — which is false, and unrecoverable, because nothing on the wire
    /// says the class was withheld rather than absent.
    ///
    /// Both predicates come from **this server's own router**, not from `#[cfg]`,
    /// for the reason [`GraphServer::instructions`] records: an operator can now
    /// remove a tool a build does have, so the announcement and the listing have
    /// to be two views of one object.
    #[tool]
    async fn list_tool_classes(
        &self,
        Parameters(_args): Parameters<ListToolClassesArgs>,
    ) -> CallToolResult {
        // Resolved once. `tool_names` builds an unrestricted router to answer, and
        // the predicate is asked once per tool row — calling it inside the closure
        // would rebuild that router fifteen times to learn something that cannot
        // change while the process runs.
        let in_build: BTreeSet<String> = tool_names().into_iter().collect();
        json_result(&crate::tool_class::report(
            |name| in_build.contains(name),
            |name| self.tool_router.has_route(name),
        ))
    }
}

/// The two `security` subcommands that read and never write (issue #435).
///
/// A separate `#[tool_router]` block so the whole pair can be `#[cfg]`-gated — see
/// [`GraphServer::routes`] for why one cfg per handler would not compile. The other
/// three subcommands are permanent refusals, with their reasons in this module's
/// disposition table; `the_three_mutating_security_subcommands_are_never_exposed`
/// is what fails if one of them is ever added here.
#[cfg(feature = "execution")]
#[tool_router(router = security_tool_router)]
impl GraphServer {
    /// List stored security findings, bounded, with the never-run case named.
    #[tool]
    async fn security_list(
        &self,
        Parameters(args): Parameters<SecurityListArgs>,
    ) -> CallToolResult {
        let analyzer = match checked_analyzer(args.analyzer.as_deref()) {
            Ok(analyzer) => analyzer,
            Err(e) => return tool_error(&e),
        };
        // No `[debt] ignore`-shaped hazard here, and that is what makes this one
        // genuinely eligible rather than eligible-looking: a listing is a pure
        // read over stored findings and a pure cross-reference over them, with no
        // project configuration in the path at all. `debt` has one and passes an
        // empty list because this crate cannot reach `roteiro.toml`; there is
        // nothing equivalent for this tool to be missing.
        let limit = model_limit(args.limit, 20, 100);
        query_result(self.with_project(args.project.as_deref(), |store| {
            store
                .findings_layers(analyzer)
                .map(|layers| rto_exec::security_list(layers, limit))
        }))
    }

    /// What this machine has provisioned, and what this repository has analyzed —
    /// as two labelled scopes.
    #[tool]
    async fn security_status(
        &self,
        Parameters(args): Parameters<SecurityStatusArgs>,
    ) -> CallToolResult {
        let analyzer = match checked_analyzer(args.analyzer.as_deref()) {
            Ok(analyzer) => analyzer,
            Err(e) => return tool_error(&e),
        };
        let project = args.project.as_deref();
        // The **resolved** name, not the argument: a bare call omits `project`, and
        // a `repository` half labelled `null` would be a section whose scope the
        // reader has to guess — which is the defect this shape exists to remove.
        // Resolving here also fails an unknown/ambiguous `project` before any
        // machine-global work is reported, so a caller never gets a document whose
        // two halves came from different questions.
        let project_name = match self.workspace.resolve(project) {
            Ok(name) => name,
            Err(e) => return tool_error(&e.to_string()),
        };
        // Machine-global, and read outside `with_project` because it is not the
        // project's to answer: the asset root describes this host, whichever
        // project was selected. The document is what says so.
        let root = self.asset_root.clone();
        let now = rto_exec::rfc3339_utc(std::time::SystemTime::now());
        query_result(self.with_project(project, |store| {
            store.findings_layers(analyzer).map(|layers| {
                rto_exec::security_status(&root, analyzer, &project_name, &layers, &now)
            })
        }))
    }
}

/// The sandbox image store's two tools, in a **third** `#[tool_router]` block for
/// the same reason the `security_*` pair has a second one — see
/// [`GraphServer::routes`].
///
/// Their own block rather than the `security_*` one because of what they are, not
/// only how they are gated: `sandbox_clear` is the first tool on this surface that
/// **mutates**, and ADR-0014 v1.6 admits it by a rule (*a tool may drop state
/// re-obtainable from a pinned digest, and may drop nothing else*) rather than as
/// an exception. Keeping it visibly apart is what stops the next mutating tool
/// arriving by proximity. `the_only_mutating_tool_is_the_one_the_adr_admits` is
/// what fails if one does.
#[cfg(feature = "execution")]
#[tool_router(router = sandbox_tool_router)]
impl GraphServer {
    /// What the machine-global sandbox image store is holding.
    #[tool]
    async fn sandbox_status(
        &self,
        Parameters(_args): Parameters<SandboxStatusArgs>,
    ) -> CallToolResult {
        // Machine-global, and read from this host's environment rather than from a
        // project's store — there is no `with_project` here because there is no
        // project in the question.
        match rto_exec::sandbox_status(&self.asset_root) {
            Ok(report) => json_result(&report),
            Err(e) => tool_error(&e.to_string()),
        }
    }

    /// Drop cached images, and say what that freed.
    #[tool]
    async fn sandbox_clear(
        &self,
        Parameters(args): Parameters<SandboxClearArgs>,
    ) -> CallToolResult {
        // Neither argument defaults to the other, and `None`/`false` is not a
        // request — it is the absence of one. ADR-0014 v1.6: a model asking for one
        // scope must not receive the other, and silence is not a scope at all.
        let scope = match (args.image, args.everything.unwrap_or(false)) {
            (Some(_), true) => {
                return tool_error(
                    "`image` and `everything` are different requests; pass exactly one.",
                );
            }
            (None, true) => rto_exec::Scope::Everything,
            (Some(reference), false) => rto_exec::Scope::Image(reference),
            (None, false) => {
                return tool_error(
                    "nothing was named to drop. Pass `image` with a reference from \
                     `sandbox_status`, or `everything: true`. Supplying neither does not \
                     mean everything.",
                );
            }
        };
        let outcome = if args.dry_run.unwrap_or(false) {
            rto_exec::sandbox_plan(&self.asset_root, &scope).map(|(report, _doomed)| report)
        } else {
            rto_exec::sandbox_clear(&self.asset_root, &scope)
        };
        match outcome {
            Ok(report) => json_result(&report),
            Err(e) => tool_error(&e.to_string()),
        }
    }
}

/// The read-only rule a server states about itself, in the one of its two
/// spellings that is true here.
///
/// Two spellings of one rule, chosen by whether the mutating tool is on this
/// server: with `sandbox_clear` advertised the flat one is false, and stating a
/// rule with its exception named is the difference between a rule and a claim a
/// model can catch the server out on. The rule the read-only stance protects — a
/// model must not change what the graph says — is unweakened either way.
fn read_only_rule_clause(has_mutating_tool: bool) -> &'static str {
    if has_mutating_tool {
        " Every tool here answers from the graph and none of them changes it, with \
         exactly one exception: `sandbox_clear` deletes cached container images — bytes \
         a pinned digest re-obtains — and changes nothing the graph says."
    } else {
        " Every tool here is read-only."
    }
}

/// The clause a **restricted** server adds to its `instructions`, or `None` when
/// every class is loaded (#664).
///
/// Conditional because the instructions are paid for on every turn exactly as a
/// description is. On a full server the sentence would be true and useless —
/// nothing is withheld, so there is no wrong answer for it to prevent — and a
/// feature whose whole point is fewer tokens must not add unconditional ones.
///
/// It exists at all because withholding a class is otherwise invisible: a model
/// on a `query`-only server has no way to tell a tool that was left out from a
/// capability Roteiro does not have, and will report the second.
///
/// # Why it takes two predicates rather than reading the router alone
///
/// A route can be missing for two unrelated reasons, and only one of them is a
/// withholding. `has` alone cannot tell them apart: `security_*` and `sandbox_*`
/// are `#[cfg(feature = "execution")]`, so on a build without that feature they
/// are absent from **every** server, restricted or not. Deciding from `has` alone
/// made an unrestricted server on such a build announce that it "was started with
/// only SOME of its tool classes" — false, and paid for on every turn in exactly
/// the configuration that withheld nothing to save anything.
///
/// So the signal is the **difference from this build's own unrestricted set**: a
/// tool this build does not carry was not withheld from anyone, and only a tool
/// `in_build` carries and `has` does not is an operator's choice. The claim and
/// the tokens then both follow the thing the sentence is actually about.
fn withheld_class_clause(
    in_build: impl Fn(&str) -> bool,
    has: impl Fn(&str) -> bool,
) -> Option<&'static str> {
    let anything_withheld = crate::tool_class::CLASSES
        .iter()
        .any(|(_, tools)| tools.iter().any(|t| in_build(t) && !has(t)));
    (has(crate::tool_class::CLASS_INDEX_TOOL) && anything_withheld).then_some(
        " This server was started with only SOME of its tool classes. Before telling a \
         user Roteiro cannot do something, call `list_tool_classes`: a class marked \
         `not-loaded-here` was left out at startup to save prompt tokens and is not a \
         missing capability — name the class so the user can restart the server with it.",
    )
}

impl GraphServer {
    /// The `instructions` string a client is handed on `initialize`: one clause
    /// per tool **this server actually advertises**.
    ///
    /// Assembled rather than written as one literal, and assembled from
    /// [`GraphServer::tool_router`] rather than from `#[cfg]` alone. The reason is
    /// the old one with a wider scope: a server that does not offer a tool must
    /// not announce it, because a model told a tool exists and then handed
    /// `unknown tool` has been misinformed by its own server. Until `--tools`
    /// (issue #584) a feature gate was the only way that could happen, so `#[cfg]`
    /// was enough; an operator can now remove a tool a build does have. Reading
    /// the router makes the announcement, the listing and the dispatch three views
    /// of one object.
    fn instructions(&self) -> String {
        let has = |name: &str| self.tool_router.has_route(name);
        let mut out = String::from("Roteiro codebase knowledge graph.");
        // One clause per tool, each independently omissible. The prose is the
        // prose that was here, cut at the joints rather than rewritten, so a
        // restricted server says less and never says something different.
        if has("search") {
            out.push_str(
                " Start with `search` to find nodes by text (it searches captured \
                 content too — README/ADR/blueprint prose — and ranks curated docs \
                 first, so it answers \"what is X / why\").",
            );
            // #675 took this sentence out of `SEARCH`'s description because the
            // served surface's system turn already states it for every tool at
            // once. This surface has no such turn, so the statement lands here
            // instead of being lost — and here it costs once per session, in
            // `initialize`, rather than once per `tools/list`.
            //
            // Deliberately not put back in the description: two copies of one
            // instruction is what #675 is about, and the served copy is the older
            // and more general of the two.
            out.push_str(
                " Read each hit's `snippet`, or call `explain` on its key, to read \
                 a node's actual content before describing it — never answer from \
                 a node's name alone.",
            );
        }
        if has("explain") {
            out.push_str(" `explain` gives a key's provenance-labelled neighbourhood.");
        }
        if has("context") {
            out.push_str(" `context` returns that neighbourhood bounded and fingerprinted.");
        }
        if has("list_kind") {
            out.push_str(" `list_kind` enumerates a kind.");
        }
        if has("path") {
            out.push_str(" `path` finds how two nodes connect.");
        }
        if has("debt") {
            out.push_str(" `debt` lists intent-debt markers.");
        }
        if has("debt_density") {
            out.push_str(" `debt_density` ranks files by markers per 1,000 lines.");
        }
        if has("coupling") {
            out.push_str(" `coupling` ranks symbols by directed call fan-in/fan-out.");
        }
        if has("config_secrets") {
            out.push_str(
                " `config_secrets` inventories secret-named config keys (an inventory, \
                 not a secret scan — see its description).",
            );
        }
        if has("check") {
            out.push_str(
                " `check` runs the authored-layer drift gate and returns its verdict as \
                 data (read its `gate` field: `not-run` is a real outcome and is not a \
                 clean repository).",
            );
        }
        if has("sandbox_status") {
            out.push_str(
                " `sandbox_status` reports what the MACHINE-GLOBAL container-image cache \
                 is holding and what it costs.",
            );
        }
        if has("sandbox_clear") {
            out.push_str(
                " `sandbox_clear` deletes from that cache and is the ONE tool here that \
                 changes anything — everything it drops is re-obtainable from a pinned \
                 digest, so it costs a re-download and never information. Show the user \
                 `sandbox_status` before calling it, quote the bytes it reports freeing, \
                 and pass exactly one of `image` and `everything` — neither has a default \
                 and supplying neither is an error rather than a request to clear \
                 everything.",
            );
        }
        if has("security_list") {
            out.push_str(" `security_list` lists stored analyzer findings.");
        }
        if has("security_status") {
            out.push_str(
                " `security_status` reports readiness in two separately scoped halves \
                 (`machine` = what this host has provisioned AND installed, `repository` \
                 = one project's layers).",
            );
        }
        if has("security_list") || has("security_status") {
            out.push_str(
                " Read `coverage` before concluding anything from either, because \
                 `no-analyzer-on-record` means nothing has been analyzed and is not a \
                 clean repository. Neither can run an analyzer, ingest a report or \
                 prefetch an asset; ask the user to run those.",
            );
        }
        out.push_str(read_only_rule_clause(has("sandbox_clear")));
        // `tool_names` is this build's unrestricted set and is what separates a
        // withheld tool from one the build never had — see `withheld_class_clause`.
        // Resolved once: it builds a router to answer, and the predicate is asked
        // per tool.
        let in_build: BTreeSet<String> = tool_names().into_iter().collect();
        if let Some(clause) = withheld_class_clause(|name| in_build.contains(name), has) {
            out.push_str(clause);
        }
        out.push_str(
            " There is no `review` tool — `roteiro review` is CLI-first and needs no \
             server; see this module's documentation for why it is not exposed.",
        );
        // Stated once, for every tool, rather than per description: the surface's
        // mass is already description prose (#590), and this is one fact about the
        // server rather than a property of any single tool.
        //
        // It is stated at all because of #599. The CLI's report commands read the
        // **working tree** — `roteiro debt` answers about the edit in front of you
        // — while this server answers from the graph it synced, which is `HEAD`. A
        // model that reads `roteiro debt` in a terminal and calls `debt` here can
        // legitimately be told two different numbers; what it must never be is
        // told them without being told why.
        out.push_str(
            " These tools answer from the committed (`HEAD`) graph this server \
             synced, not from anyone's uncommitted working-tree edits — so a count \
             here can differ from the same command run in a terminal, which reads \
             the working tree by default (its `--committed` flag matches this).",
        );
        out
    }
}

// The async methods this trait needs are generated by `#[tool_handler]`;
// whether a given expansion awaits anything is rmcp's business, and
// `ServerHandler` fixes the signatures as `async` either way. The lint is
// aimed at a hand-written `async fn` that could drop the keyword — there is
// no keyword here to drop and no generated body this file can restructure.
// `get_info` below is the only method written by hand, and it is not async.
//
// `unknown_lints` rides along because the clippy lint above landed in 1.98:
// without it, this attribute is itself a `-D warnings` error on any older
// clippy, which would break the checklist command for a contributor whose
// `stable` has not rolled over yet.
#[allow(unknown_lints, clippy::unused_async_trait_impl)]
#[tool_handler(router = self.tool_router)]
impl ServerHandler for GraphServer {
    fn get_info(&self) -> ServerInfo {
        // `ServerInfo` is `#[non_exhaustive]`; build from default then set fields.
        let mut info = ServerInfo::default();
        info.protocol_version = ProtocolVersion::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.server_info = Implementation::new("roteiro", env!("CARGO_PKG_VERSION"));
        info.instructions = Some(self.instructions());
        info
    }
}

/// Each advertised tool's **argument names**, by tool name.
///
/// Read off the `inputSchema` a client is actually handed — the one `schemars`
/// derives from the argument structs — rather than off the source text, so it
/// cannot be satisfied by a comment and cannot drift from what is advertised.
///
/// Exists for the same reason [`tool_names`] does, one level down. The two tool
/// surfaces declare their schemas from separate definitions — this one from the
/// `#[derive(JsonSchema)]` structs above, the served-chat `GraphToolRegistry`
/// from hand-written `json!` literals — so nothing but a test holds their
/// *argument* names level, and the names are what a model has to get right to
/// ask the question it means. Offering the same tools under different argument
/// spellings is the failure this exposes: `debt`'s category filter was `kind`
/// here and `categories` there, one word that also means *node kind* in
/// `list_kind`, and a model that guessed from the wrong surface got every marker
/// in the repository back as the filtered set it asked for.
/// `both_surfaces_name_a_shared_tools_arguments_the_same_way` in `roteiro` is the
/// consumer.
#[must_use]
pub fn tool_argument_names()
-> std::collections::BTreeMap<String, std::collections::BTreeSet<String>> {
    GraphServer::routes(&Advertised::All)
        .list_all()
        .into_iter()
        .map(|t| {
            let names = t
                .input_schema
                .get("properties")
                .and_then(serde_json::Value::as_object)
                .map(|props| props.keys().cloned().collect())
                .unwrap_or_default();
            (t.name.to_string(), names)
        })
        .collect()
}

/// Each advertised tool's **description**, by name.
///
/// Exists so the copy `rmcp` forces can be compared against its source. The macro
/// parses `description` into a `String` via `darling`, so it takes a string
/// literal and rejects a path — `description = SANDBOX_STATUS` fails to compile —
/// and `serve` does not imply `mcp`, so the served side cannot simply read this
/// module either. [`crate::tool_text`] is therefore the authority and the literal
/// below is a checked copy; this is what makes the check possible.
#[must_use]
pub fn tool_descriptions() -> std::collections::BTreeMap<String, String> {
    GraphServer::routes(&Advertised::All)
        .list_all()
        .into_iter()
        .map(|t| {
            (
                t.name.to_string(),
                t.description.unwrap_or_default().to_string(),
            )
        })
        .collect()
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
    advertised_names(&Advertised::All)
}

/// The names a server built for `advertised` would list, sorted.
///
/// [`tool_names`] answers the same question for the unrestricted surface and is
/// defined in terms of this. Exposed separately (#664) so the **other** surface
/// can be compared against a *restricted* MCP server rather than only against a
/// full one: the two registries are separate objects, and a selection applied to
/// one and not the other is invisible to a comparison of their unrestricted sets.
#[must_use]
pub fn advertised_names(advertised: &Advertised) -> Vec<String> {
    let mut names: Vec<String> = GraphServer::routes(advertised)
        .list_all()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    names.sort();
    names
}

/// The bytes a client is handed for a tool surface: for each advertised tool, its
/// name plus its description plus its `inputSchema` as compact JSON.
///
/// The same sum issue #584 measured by driving `roteiro mcp` over stdio and
/// reading `tools/list`, brought in-tree so the number is a property of the build
/// rather than of a script somebody has to still have. It matters because it is
/// not tidiness: a byte-bounded provider refuses a surface over its limit, and at
/// the 3.13 ms/token prefill measured in issue #578 these bytes are seconds on
/// every turn.
///
/// Excludes the server `instructions`, which are handed over once at `initialize`
/// rather than per tool — a different line item, counted separately by anything
/// that wants the total.
#[must_use]
pub fn advertised_bytes(advertised: &Advertised) -> usize {
    GraphServer::routes(advertised)
        .list_all()
        .iter()
        .map(|t| {
            t.name.len()
                + t.description.as_deref().map_or(0, str::len)
                + serde_json::to_string(&t.input_schema).map_or(0, |s| s.len())
        })
        .sum()
}

/// Every tool name this module declares, **in every feature configuration** —
/// unlike [`tool_names`], which answers what *this build* offers.
///
/// The two questions are different and only one of them can validate an
/// operator's `--tools` list. A build without `execution` has no `sandbox_clear`;
/// rejecting the name there would make one restriction file mean different things
/// on different machines, and the operator would have no way to tell a typo from
/// a feature gate. So a name is **unknown** only when it is not a Roteiro tool at
/// all, and a real name this build lacks restricts nothing and is reported as
/// such — which is ADR-0007's feature-availability rule (warn, do not error)
/// applied to a name instead of a key.
///
/// Hand-written because `#[cfg]`-ed-out routes leave no trace to enumerate, and
/// kept honest by `every_tool_covers_this_build`: it fails if a router offers a
/// name this list does not carry, in whatever feature set the test is run under.
pub const EVERY_TOOL: [&str; 16] = [
    "check",
    "config_secrets",
    "context",
    "coupling",
    "debt",
    "debt_density",
    "explain",
    "list_kind",
    "list_projects",
    "list_tool_classes",
    "path",
    "sandbox_clear",
    "sandbox_status",
    "search",
    "security_list",
    "security_status",
];

/// The tools that change something — the complement of what [`READ_ONLY`] selects.
///
/// Exactly one, and this server's own `instructions` already say so in those
/// words ("none of them changes it, with exactly one exception: `sandbox_clear`").
/// Naming it here rather than leaving the claim in prose is what lets
/// `--tools read-only` stay correct when a second mutating tool is added:
/// `read_only_is_every_tool_but_the_mutating_ones` fails if this list and that
/// sentence come apart.
pub const MUTATING_TOOLS: [&str; 1] = ["sandbox_clear"];

/// The `--tools` / `[mcp] tools` alias for "everything except [`MUTATING_TOOLS`]".
///
/// Worth having because the alternative is enumerating fourteen names that grow
/// stale the moment a tool is added — an allow-list written by hand in a client
/// config is exactly how issue #584's consumer ended up with a restriction that
/// did not restrict.
pub const READ_ONLY: &str = "read-only";

/// What an MCP server advertises **and** dispatches: everything this build
/// offers, or the subset an operator named (issue #584).
///
/// One value covers both halves on purpose. The restriction the server was asked
/// for has to bind `tools/call` as well as `tools/list` — a tool that is not
/// advertised must also not be callable, because a client that already knows the
/// name is precisely the case a restriction is for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Advertised {
    /// Every tool this build offers.
    #[default]
    All,
    /// Only these — already validated and resolved against this build by
    /// [`restrict`].
    Only(BTreeSet<String>),
}

impl Advertised {
    /// Whether `name` is advertised (and therefore callable).
    #[must_use]
    pub fn allows(&self, name: &str) -> bool {
        match self {
            Self::All => tool_names().iter().any(|t| t == name),
            Self::Only(allowed) => allowed.contains(name),
        }
    }
}

/// Why an operator's tool list was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestrictError {
    /// A name that is not a Roteiro tool in any build — a typo.
    Unknown(String),
    /// The list resolved to nothing this build can serve.
    ///
    /// **This is the failure the whole feature exists to prevent**, seen from our
    /// own side of the wire. In [omnigent#5178] a sidecar allow-list parsed to
    /// zero entries and the client read that as "no restriction", so a server the
    /// operator believed restricted advertised everything. An empty result is a
    /// refusal to start, never a licence to serve the full surface.
    ///
    /// [omnigent#5178]: https://github.com/omnigent-ai/omnigent/issues/5178
    Empty,
}

impl std::fmt::Display for RestrictError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown(name) => write!(
                f,
                "unknown tool `{name}` (expected one of {}, a class ({}), or `{READ_ONLY}`)",
                EVERY_TOOL.join(", "),
                crate::tool_class::class_names().join(", "),
            ),
            Self::Empty => write!(
                f,
                "the tool restriction leaves nothing to serve — refusing to start \
                 rather than advertising the full surface. Name at least one of {}",
                tool_names().join(", ")
            ),
        }
    }
}

impl std::error::Error for RestrictError {}

/// An operator's tool list, resolved against this build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restriction {
    /// What the server should advertise and dispatch.
    pub advertised: Advertised,
    /// Names that are real Roteiro tools but are not compiled into this binary,
    /// so they restrict nothing here. Reported, never fatal — see [`EVERY_TOOL`].
    pub absent: Vec<String>,
}

/// Resolve an operator's `--tools` / `[mcp] tools` list into a [`Restriction`].
///
/// `names` may hold tool names, a **class** name from [`crate::tool_class`], and
/// the [`READ_ONLY`] alias, in any order. The alias contributes every tool this
/// build offers except [`MUTATING_TOOLS`]; a class contributes the tools it names.
///
/// [`crate::tool_class::CLASS_INDEX_TOOL`] is added to every non-empty result and
/// cannot be withheld — see the note at the end of this function for why a
/// restriction that could hide it would make a model misinform its user.
///
/// # Errors
/// [`RestrictError::Unknown`] for a name that is not a Roteiro tool at all, and
/// [`RestrictError::Empty`] when nothing survives — both loud, because a
/// restriction that quietly restricts nothing is the defect this feature is a
/// response to.
pub fn restrict(names: &[String]) -> Result<Restriction, RestrictError> {
    let in_build = tool_names();
    let mut allowed = BTreeSet::new();
    let mut absent = Vec::new();
    for raw in names {
        let name = raw.trim();
        if name.is_empty() {
            continue;
        }
        if name == READ_ONLY {
            allowed.extend(
                in_build
                    .iter()
                    .filter(|t| !MUTATING_TOOLS.contains(&t.as_str()))
                    .cloned(),
            );
            continue;
        }
        // A class (#664) resolves to its tools before the name check, so
        // `--tools query` is not read as a typo. Tools of the class this build
        // lacks are simply not selected: a class is a shorthand for a group, not
        // an assertion that every member is present, so it has nothing to report
        // as `absent` — the same reading `read-only` above already takes.
        if let Some(tools) = crate::tool_class::tools_in(name) {
            allowed.extend(
                tools
                    .iter()
                    .filter(|t| in_build.iter().any(|b| b == *t))
                    .map(|t| (*t).to_owned()),
            );
            continue;
        }
        if !EVERY_TOOL.contains(&name) {
            return Err(RestrictError::Unknown(name.to_owned()));
        }
        if in_build.iter().any(|t| t == name) {
            allowed.insert(name.to_owned());
        } else if !absent.iter().any(|a| a == name) {
            absent.push(name.to_owned());
        }
    }
    // Emptiness is judged on what the operator *named*, before the class index is
    // added below. Judging it afterwards would make `--tools ""` resolve to a
    // one-tool server instead of the refusal `RestrictError::Empty` documents —
    // a restriction that quietly restricted nothing is the defect #584 exists to
    // prevent, and a restriction that quietly served something nobody asked for
    // is the same mistake pointed the other way.
    if allowed.is_empty() {
        return Err(RestrictError::Empty);
    }
    // The class index is never withheld (#664). A class an operator did not load
    // is otherwise indistinguishable, from the client side, from a capability
    // Roteiro does not have — so a restricted server would make a model tell a
    // user something false. One short description is what that costs; it is
    // dwarfed by any single class it stands in for.
    allowed.insert(crate::tool_class::CLASS_INDEX_TOOL.to_owned());
    Ok(Restriction {
        advertised: Advertised::Only(allowed),
        absent,
    })
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
pub fn serve_stdio(workspace: Arc<Workspace>, advertised: &Advertised) -> Result<(), McpError> {
    let shared: SharedWorkspace = workspace;
    let advertised = advertised.clone();
    runtime()?.block_on(async move {
        let service = GraphServer::new(shared, &advertised).serve(stdio()).await?;
        service.waiting().await?;
        Ok(())
    })
}

/// Build the axum [`Router`](axum::Router) serving the MCP streamable-HTTP
/// transport at the `/mcp` path, for mounting **standalone or merged into
/// another app** — e.g. alongside the `/v1` model endpoint on one port
/// (ADR-0008), so a single process serves both surfaces over one Workspace.
/// Takes ownership of `workspace`.
pub fn mcp_router(workspace: Arc<Workspace>, advertised: &Advertised) -> axum::Router {
    let shared: SharedWorkspace = workspace;
    let advertised = advertised.clone();
    let service = StreamableHttpService::new(
        move || Ok(GraphServer::new(shared.clone(), &advertised)),
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
pub fn serve_http(
    workspace: Arc<Workspace>,
    addr: SocketAddr,
    advertised: &Advertised,
) -> Result<(), McpError> {
    let router = mcp_router(workspace, advertised);
    runtime()?.block_on(async move {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, router).await?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{
        Advertised, CheckArgs, ConfigSecretArgs, ContextArgs, CouplingArgs, DebtArgs, DensityArgs,
        ExplainArgs, GraphServer, ListKindArgs, PathArgs, SearchArgs, model_limit, tool_names,
    };
    #[cfg(feature = "execution")]
    use super::{SandboxClearArgs, SecurityListArgs, SecurityStatusArgs};
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
        GraphServer::new(Arc::new(Workspace::single("test", store)), &Advertised::All)
    }

    /// [`seeded`]'s graph behind a chosen tool surface, for the `--tools` tests.
    #[cfg(feature = "execution")]
    fn seeded_with(advertised: &Advertised) -> GraphServer {
        GraphServer::new(seeded().workspace, advertised)
    }

    fn text_of(result: &rmcp::model::CallToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.clone()))
            .collect()
    }

    /// **The migration this rename was sequenced to buy.** `kind` was this
    /// surface's old name for `debt`'s filter; a client still sending it must be
    /// *told*, by name, rather than quietly answered.
    ///
    /// This is the whole reason the refusal landed first. While an unrecognised
    /// key was dropped, the same call would have deserialised to `categories:
    /// []` — and an empty filter means *every* category — so a caller that had
    /// been asking for `todo` since before the rename would have gone on parsing
    /// fine and started receiving the entire repository's debt, presented as the
    /// filtered set it asked for. Refusing converts that into a message naming
    /// the key that replaced it, which a model can act on in one turn.
    ///
    /// It has to be exercised through deserialisation rather than by building a
    /// `DebtArgs`, because that is where the key would be dropped: the typed
    /// struct every other test here constructs cannot express the defect.
    ///
    /// This is what rmcp deserialises: `Parameters<DebtArgs>` runs
    /// `serde_json::from_value` over the call's `arguments` object and reports a
    /// failure as a JSON-RPC `invalid_params` error ("failed to deserialize
    /// parameters: …"), so the message asserted here is the one that reaches the
    /// client.
    #[test]
    fn the_old_argument_name_is_refused_rather_than_silently_dropped() {
        let sent = serde_json::json!({ "kind": ["todo"] });
        let err = serde_json::from_value::<DebtArgs>(sent).expect_err("refused");
        let message = err.to_string();
        assert!(
            message.contains("unknown field `kind`"),
            "the refusal must name the key that was sent: {message}",
        );
        assert!(
            message.contains("`categories`") && message.contains("`project`"),
            "and the keys that would have worked — this is the entire migration \
             path for a caller written against the old name, so the replacement \
             has to be *in the message*: {message}",
        );

        // The same for `debt_density`, which carried the same filter under the
        // same old name.
        let err = serde_json::from_value::<DensityArgs>(
            serde_json::json!({ "kind": ["todo"], "order": "markers" }),
        )
        .expect_err("refused");
        let message = err.to_string();
        assert!(
            message.contains("unknown field `kind`") && message.contains("`categories`"),
            "{message}",
        );
    }

    /// The other direction, so the rule is not satisfied by refusing everything:
    /// the shared spelling parses on this surface, and still filters.
    #[test]
    fn this_surfaces_own_argument_names_still_parse() {
        let args: DebtArgs = serde_json::from_value(
            serde_json::json!({ "categories": ["todo"], "project": "roteiro" }),
        )
        .expect("accepted");
        assert_eq!(args.categories, ["todo"]);
        assert_eq!(args.project.as_deref(), Some("roteiro"));
        // And an omitted argument is still omitted rather than required.
        let bare: DebtArgs = serde_json::from_value(serde_json::json!({})).expect("accepted");
        assert!(bare.categories.is_empty() && bare.project.is_none());
    }

    /// The class, not the instance: **every** advertised tool's argument object
    /// is closed, in whatever feature configuration this test runs under.
    ///
    /// `deny_unknown_fields` is what schemars renders as `additionalProperties:
    /// false`, so this fails for an argument struct that arrives without the
    /// attribute — which is the drift that would let a second `kind`/`categories`
    /// in. It reads the schema a client is actually handed rather than the source
    /// text, so it cannot be satisfied by a comment.
    #[test]
    fn every_tool_closes_its_argument_object() {
        let open: Vec<String> = GraphServer::routes(&Advertised::All)
            .list_all()
            .into_iter()
            .filter(|t| {
                t.input_schema.get("additionalProperties") != Some(&serde_json::Value::Bool(false))
            })
            .map(|t| t.name.to_string())
            .collect();
        assert!(
            open.is_empty(),
            "these tools accept an argument key nobody reads, so a model sending one \
             gets an answer to a question it did not ask: {open:?}. Add \
             `#[serde(deny_unknown_fields)]` to the argument struct.",
        );
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
                    categories: vec!["stub".into()],
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
        /// The one bounded tool this build gains under `execution`. Empty otherwise,
        /// so the table below needs no `mut` in a build with nothing to add — and
        /// the rule is the same either way: a tool that pages must advertise the
        /// page it enforces. `security_list` bounds findings per layer.
        #[cfg(feature = "execution")]
        const GATED: &[(&str, u64)] = &[("security_list", 100)];
        #[cfg(not(feature = "execution"))]
        const GATED: &[(&str, u64)] = &[];
        let server = seeded();
        let tools = server.tool_router.list_all();
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

    /// The `security*` tool set is **exactly** the two read-only subcommands, and
    /// the other three can never join it.
    ///
    /// `ingest` and `run` both call `Store::replace_findings_layer` — `run` through
    /// `execute_and_file`, which **both** of its backends share, so ADR-0019's
    /// sandboxed default did not make it read-only — and `run` additionally
    /// executes an analyzer: a model asking for a tool is not a human consenting to
    /// execution. `prefetch` opens the network under an explicit human consent.
    ///
    /// Completing the set is the failure mode this test exists to prevent, and it
    /// checks that two ways round. The set equality fails on *any* unexpected
    /// `security*` tool, whatever it is called; the named loop then fails with the
    /// specific refusal that was broken, so a future change that loosens the first
    /// assertion still trips over the second. Issue #435 added `list` and `status`
    /// here and deliberately left this test in place rather than deleting it.
    #[test]
    fn the_three_mutating_security_subcommands_are_never_exposed() {
        use std::collections::BTreeSet;

        let server = seeded();
        let security: BTreeSet<String> = server
            .tool_router
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .filter(|n| n.starts_with("security"))
            .collect();

        // The pair is feature-gated on `execution`, so the expected set is too — a
        // build without it offers no `security*` tool at all, which is the same
        // property stated at a different feature set rather than a weaker one.
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
                 tool. Found in: {security:?}",
            );
        }
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

    /// A store holding one live layer per analyzer, so the `analyzed` path can be
    /// exercised against a real `findings_layers` read rather than a hand-built
    /// document.
    ///
    /// Two layers on purpose: the per-layer page bound is the thing most likely to
    /// be quietly changed to a per-document one, and a single layer cannot tell the
    /// two apart.
    #[cfg(feature = "execution")]
    fn seeded_with_findings() -> GraphServer {
        use rto_graph::{
            AnalysisRun, CommandPolicy, Finding, FindingKey, Isolation, RunnerKind, Severity,
            SourceIdentity,
        };

        let mut store = Store::open_in_memory().expect("store");
        let run = |analyzer: &str| AnalysisRun {
            layer: format!("security:{analyzer}:wt"),
            analyzer: analyzer.to_owned(),
            analyzer_version: "1.0.0".to_owned(),
            runner: RunnerKind::Ingested,
            isolation: Isolation::Ingested,
            image_digest: None,
            rules_digest: None,
            advisory_db: None,
            command_policy: CommandPolicy::default(),
            source: SourceIdentity::default(),
            started_at: "2026-08-01T00:00:00Z".to_owned(),
            ended_at: "2026-08-01T00:00:01Z".to_owned(),
            exit_status: 1,
            report_digest: "deadbeef".to_owned(),
        };
        let finding = |analyzer: &str, rule: &str, severity: Severity| Finding {
            key: FindingKey::new(analyzer, &[rule, "no-snippet"]).expect("key"),
            rule: rule.to_owned(),
            severity,
            title: format!("{rule} title"),
            message: format!("{rule} message"),
            path: None,
            span: None,
            meta: serde_json::Value::Null,
        };
        store
            .replace_findings_layer(
                &run("cargo-audit"),
                &[
                    finding("cargo-audit", "RUSTSEC-2024-0001", Severity::Critical),
                    finding("cargo-audit", "RUSTSEC-2024-0002", Severity::Low),
                ],
            )
            .expect("cargo-audit layer");
        store
            .replace_findings_layer(
                &run("semgrep"),
                &[finding("semgrep", "rules.taint", Severity::High)],
            )
            .expect("semgrep layer");
        GraphServer::new(Arc::new(Workspace::single("test", store)), &Advertised::All)
    }

    /// The trap: `security_list` on a repository no analyzer has run against must
    /// not produce a document a model can read as "no security findings".
    ///
    /// `seeded()` is exactly that repository — it has a graph and no findings — so
    /// this is the shape a first call against a fresh project actually returns.
    #[cfg(feature = "execution")]
    #[tokio::test]
    async fn security_list_distinguishes_nothing_ran_from_nothing_found() {
        let out = text_of(
            &seeded()
                .security_list(Parameters(SecurityListArgs::default()))
                .await,
        );
        let json: serde_json::Value = serde_json::from_str(&out).expect("json");
        assert_eq!(json["coverage"], "no-analyzer-on-record", "{json}");
        // The two fields a model would reach for are ABSENT, not zero: `0` is the
        // good answer for both, which is why neither may appear in this document.
        assert!(json.get("report").is_none(), "{json}");
        assert!(!out.contains("\"findings\""), "{out}");
        assert!(
            json["no_result_reason"]
                .as_str()
                .expect("reason")
                .contains("NOT a clean result"),
            "{json}"
        );

        // And the opposite fact reads as the opposite document, which is what makes
        // the discriminator worth having.
        let out = text_of(
            &seeded_with_findings()
                .security_list(Parameters(SecurityListArgs::default()))
                .await,
        );
        let json: serde_json::Value = serde_json::from_str(&out).expect("json");
        assert_eq!(json["coverage"], "analyzed", "{json}");
        assert_eq!(json["report"]["findings"], 3, "{json}");
        assert_eq!(json["report"]["truncated"], false, "{json}");
    }

    /// The page bound is per layer, so a bound of 1 still reaches every layer.
    ///
    /// A document-wide bound would return the first layer's single finding and
    /// report `semgrep` as absent — "that analyzer found nothing", which is the
    /// same defect as the empty listing above, one level down.
    #[cfg(feature = "execution")]
    #[tokio::test]
    async fn security_list_bounds_per_layer_and_reports_what_it_cut() {
        let out = text_of(
            &seeded_with_findings()
                .security_list(Parameters(SecurityListArgs {
                    limit: Some(1),
                    ..SecurityListArgs::default()
                }))
                .await,
        );
        let json: serde_json::Value = serde_json::from_str(&out).expect("json");
        let report = &json["report"];
        assert_eq!(report["findings"], 3, "the true total survives: {json}");
        assert_eq!(report["returned"], 2, "one from each of two layers: {json}");
        assert_eq!(report["truncated"], true);
        let layers = report["layers"].as_array().expect("layers");
        assert_eq!(layers.len(), 2, "no layer is dropped by the bound: {json}");
        // The truncated layer keeps its worst finding, not its alphabetically
        // luckiest: `RUSTSEC-2024-0001` is critical and `…0002` is low, so key
        // order and severity order agree on this pair only if severity wins.
        let audit = layers
            .iter()
            .find(|l| l["run"]["analyzer"] == "cargo-audit")
            .expect("cargo-audit layer");
        assert_eq!(audit["findings"], 2, "true per-layer count: {audit}");
        assert_eq!(audit["omitted"], 1);
        assert_eq!(audit["truncated"], true);
        assert_eq!(audit["page"][0]["severity"], "critical", "{audit}");
    }

    /// The one tool on this surface that changes anything is the one ADR-0014
    /// v1.6 admits, and it stays the only one.
    ///
    /// The read-only stance was never a rule about the word *read*; it was a rule
    /// that **a model must not change what the graph says**, and `sandbox_clear`
    /// crosses it deliberately without weakening it. A test cannot decide from a
    /// tool's name whether it mutates — so this reads the vocabulary a mutating
    /// tool would have to be named in, and fails on any second one. A tool that
    /// genuinely belongs here has to pass the ADR's test and then be added to this
    /// list, which is exactly the deliberation the ADR asks for.
    #[test]
    fn the_only_mutating_tool_is_the_one_the_adr_admits() {
        use std::collections::BTreeSet;

        const REMOVES: [&str; 7] = [
            "clear", "delete", "remove", "prune", "evict", "purge", "reset",
        ];

        let server = seeded();
        let mutating: BTreeSet<String> = server
            .tool_router
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .filter(|name| REMOVES.iter().any(|verb| name.contains(verb)))
            .collect();

        #[cfg(feature = "execution")]
        let allowed: BTreeSet<String> = ["sandbox_clear"].into_iter().map(str::to_owned).collect();
        #[cfg(not(feature = "execution"))]
        let allowed: BTreeSet<String> = BTreeSet::new();

        assert_eq!(
            mutating, allowed,
            "ADR-0014 v1.6 admits exactly one mutating tool, by a rule and not as an \
             exception: a tool may drop state re-obtainable from a pinned digest and may \
             drop nothing else. A second one is a decision, not a cleanup",
        );
    }

    /// `sandbox_status` is offered beside the verb that destroys, and neither
    /// takes a `project`.
    ///
    /// Two ADR obligations in one place. A destructive verb with no way to see
    /// what it will destroy is invoked blind (v1.6's third rule), and the store is
    /// machine-global — offering a `project` selector would imply an answer that
    /// changes with it, which is the confusion `security_status`'s two scopes
    /// exist to prevent, arrived at from the other direction.
    #[cfg(feature = "execution")]
    #[test]
    fn the_sandbox_pair_is_offered_together_and_takes_no_project() {
        let server = seeded();
        let tools = server.tool_router.list_all();
        for name in ["sandbox_status", "sandbox_clear"] {
            let tool = tools
                .iter()
                .find(|t| t.name == name)
                .unwrap_or_else(|| panic!("`{name}` advertised"));
            let props = tool
                .input_schema
                .get("properties")
                .and_then(serde_json::Value::as_object);
            assert!(
                props.is_none_or(|props| !props.contains_key("project")),
                "`{name}` must not offer a `project` selector: the sandbox store is \
                 machine-global and the argument would imply an answer that changes with \
                 it. Schema: {:?}",
                tool.input_schema,
            );
        }
    }

    /// `image` and `everything` are different arguments, and neither defaults.
    ///
    /// ADR-0014 v1.6's second obligation for a mutating tool: a model asking for
    /// one scope must not be able to receive the other. The schema declares both
    /// and requires neither, so the enforcement is at the call — which is what
    /// this checks, both ways round, and it reaches the refusal without touching
    /// the filesystem because the scope is resolved before the store is opened.
    /// A disposable asset root, for the one tool here that deletes.
    ///
    /// Every test that can reach [`GraphServer::sandbox_clear`] goes through this,
    /// including the ones that only expect a refusal: a refusal is one edit away
    /// from not being one, and the cost of finding that out with the ambient root
    /// is the developer's whole image cache. See [`GraphServer::asset_root`].
    #[cfg(feature = "execution")]
    fn disposable_asset_root(name: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("rto-render-sandbox-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("boxlite-home").join("images").join("layers"))
            .expect("a store root");
        root
    }

    #[cfg(feature = "execution")]
    #[tokio::test]
    async fn sandbox_clear_refuses_a_scope_it_was_not_given() {
        let server = seeded().with_asset_root(disposable_asset_root("refusal"));

        let neither = server
            .sandbox_clear(Parameters(SandboxClearArgs::default()))
            .await;
        assert_eq!(neither.is_error, Some(true), "{neither:?}");
        let message = format!("{:?}", neither.content);
        assert!(
            message.contains("does not") && message.contains("everything"),
            "silence must be refused, and the refusal must say it is not a request to \
             clear everything: {message}"
        );

        let both = server
            .sandbox_clear(Parameters(SandboxClearArgs {
                image: Some("registry/a:1".to_owned()),
                everything: Some(true),
                dry_run: None,
            }))
            .await;
        assert_eq!(both.is_error, Some(true), "{both:?}");
    }

    /// It reports what it freed, and it never leaves the root it was given.
    ///
    /// Two assertions that belong together. ADR-0014 v1.6's first obligation for a
    /// mutating tool is that it **reports what it freed**, so the cost appears in
    /// the transcript rather than turning up later as an unexplained re-pull. The
    /// second is this test's own safety: `store` must be under the fixture root,
    /// which is what fails if the handler ever goes back to resolving the asset
    /// root from the process environment — the arrangement that let a fault
    /// injection clear a real 8.7 GB cache.
    #[cfg(feature = "execution")]
    #[tokio::test]
    async fn sandbox_clear_reports_what_it_freed_and_stays_inside_the_root_it_was_given() {
        let root = disposable_asset_root("freed");
        std::fs::write(
            root.join("boxlite-home/images/layers/sha256-spare.tar.gz"),
            vec![b'x'; 4096],
        )
        .expect("a spare blob");
        let server = seeded().with_asset_root(root.clone());

        let result = server
            .sandbox_clear(Parameters(SandboxClearArgs {
                image: None,
                everything: Some(true),
                dry_run: None,
            }))
            .await;
        assert_ne!(result.is_error, Some(true), "{result:?}");
        let document: serde_json::Value =
            serde_json::from_str(&text_of(&result)).expect("a clear document");

        assert_eq!(document["scope"], "machine", "{document}");
        assert_eq!(document["requested"], "everything", "{document}");
        assert_eq!(document["applied"], true, "{document}");
        assert_eq!(document["freed_bytes"], 4096, "{document}");
        assert!(
            document["store"]
                .as_str()
                .expect("a store path")
                .starts_with(root.to_str().expect("a utf-8 root")),
            "the tool cleared a store outside the root it was given: {document}"
        );
    }

    /// The mutating tool's description carries what a model has to do with it.
    ///
    /// A model reads only this string. The three obligations that do not survive
    /// living in a doc comment are: look before you destroy, pass exactly one
    /// scope, and quote what was freed — plus the scope label, since the store is
    /// shared by every repository the server hosts.
    #[cfg(feature = "execution")]
    #[test]
    fn the_mutating_tool_states_its_obligations_where_a_model_reads_them() {
        let server = seeded();
        let tools = server.tool_router.list_all();
        let tool = tools
            .iter()
            .find(|t| t.name == "sandbox_clear")
            .expect("`sandbox_clear` advertised");
        let description = tool.description.as_deref().unwrap_or_default();
        for obligation in [
            "sandbox_status",
            "freed_bytes",
            "DIFFERENT REQUESTS",
            "MACHINE-GLOBAL",
            "re-obtainable",
            "retained",
        ] {
            assert!(
                description.contains(obligation),
                "`sandbox_clear`'s description must carry `{obligation}` — it is the only \
                 thing a model reads: {description}"
            );
        }
    }

    /// The decision this issue was filed for: the two scopes are labelled in the
    /// document, not merely explained in a doc comment a model cannot read.
    #[cfg(feature = "execution")]
    #[tokio::test]
    async fn security_status_labels_which_half_describes_what() {
        let out = text_of(
            &seeded_with_findings()
                .security_status(Parameters(SecurityStatusArgs::default()))
                .await,
        );
        let json: serde_json::Value = serde_json::from_str(&out).expect("json");
        assert_eq!(json["machine"]["scope"], "machine", "{json}");
        assert_eq!(json["repository"]["scope"], "repository", "{json}");
        // Each scope's identifying value is inside its own half, so a model that
        // quotes one section cannot lose the scope it belongs to.
        assert!(json["machine"]["asset_root"].is_string(), "{json}");
        assert_eq!(
            json["repository"]["project"], "test",
            "the RESOLVED project, not the omitted argument: {json}"
        );
        assert!(json["repository"].get("asset_root").is_none(), "{json}");
        assert!(json["machine"].get("project").is_none(), "{json}");
        // The machine half's readiness names what it has actually checked (issue
        // #464): the verdict plus both facts under it, and no `ready` boolean that
        // was named for running and computed from provisioning.
        let analyzer = &json["machine"]["analyzers"][0];
        assert!(analyzer["host_readiness"].is_string(), "{json}");
        assert!(analyzer["assets_provisioned"].is_boolean(), "{json}");
        assert!(analyzer["host_programs"].is_array(), "{json}");
        assert!(analyzer["missing_programs"].is_array(), "{json}");
        assert!(analyzer.get("ready").is_none(), "{json}");

        // The layer half follows the project, and carries counts rather than
        // findings — which is why this tool needs no page bound.
        assert_eq!(json["repository"]["coverage"], "analyzed", "{json}");
        let layers = json["repository"]["layers"].as_array().expect("layers");
        assert_eq!(layers.len(), 2, "{json}");
        assert!(
            layers.iter().all(|l| l.get("page").is_none()),
            "a status row must not carry findings: {json}"
        );

        // And the repository half carries the same discriminator as the listing, so
        // an unanalyzed project is not a clean one here either.
        let out = text_of(
            &seeded()
                .security_status(Parameters(SecurityStatusArgs::default()))
                .await,
        );
        let json: serde_json::Value = serde_json::from_str(&out).expect("json");
        assert_eq!(json["repository"]["coverage"], "no-analyzer-on-record");
        assert!(json["repository"].get("layers").is_none(), "{json}");
        // The machine half is still answered: it never depended on the project.
        assert!(json["machine"]["asset_root"].is_string(), "{json}");
    }

    /// An unknown `analyzer` is a tool error, never a document saying no result is
    /// on record — which would read as "that analyzer has never run here".
    #[cfg(feature = "execution")]
    #[tokio::test]
    async fn an_unknown_analyzer_is_an_error_on_both_security_tools() {
        let server = seeded_with_findings();
        let list = server
            .security_list(Parameters(SecurityListArgs {
                analyzer: Some("semgrepp".into()),
                ..SecurityListArgs::default()
            }))
            .await;
        assert_eq!(list.is_error, Some(true), "{list:?}");
        assert!(
            text_of(&list).contains("unknown analyzer `semgrepp`"),
            "{list:?}"
        );

        let status = server
            .security_status(Parameters(SecurityStatusArgs {
                analyzer: Some("semgrepp".into()),
                ..SecurityStatusArgs::default()
            }))
            .await;
        assert_eq!(status.is_error, Some(true), "{status:?}");
        assert!(
            text_of(&status).contains("unknown analyzer `semgrepp`"),
            "{status:?}"
        );

        // A known one narrows rather than erroring, so the check is a spelling gate
        // and not a refusal to filter.
        let ok = server
            .security_list(Parameters(SecurityListArgs {
                analyzer: Some("semgrep".into()),
                ..SecurityListArgs::default()
            }))
            .await;
        assert_ne!(ok.is_error, Some(true), "{ok:?}");
        let json: serde_json::Value = serde_json::from_str(&text_of(&ok)).expect("json");
        assert_eq!(json["report"]["layers"].as_array().map(Vec::len), Some(1));
        assert_eq!(json["report"]["layers"][0]["run"]["analyzer"], "semgrep");
    }

    /// The sibling of `check_tool_description_refuses_the_advisory_reading`: the
    /// never-run reading has to be refused where a model will read it.
    #[cfg(feature = "execution")]
    #[test]
    fn security_list_description_refuses_the_clean_reading() {
        let server = seeded();
        let tools = server.tool_router.list_all();
        let tool = tools
            .iter()
            .find(|t| t.name == "security_list")
            .expect("`security_list` advertised");
        let desc = tool.description.as_deref().unwrap_or_default();
        for claim in [
            "READ `coverage` FIRST",
            "NOT a clean repository",
            "carries NO `report`",
            "rather than report zero findings",
            "PER LAYER",
            "most severe findings first",
        ] {
            assert!(desc.contains(claim), "missing `{claim}` from: {desc}");
        }
    }

    /// `security_status`'s two halves have to be separated in its description as
    /// well as in its output: a model reads the description once and may act on it
    /// without re-reading the document's `scope` fields.
    #[cfg(feature = "execution")]
    #[test]
    fn security_status_description_separates_its_two_scopes() {
        let server = seeded();
        let tools = server.tool_router.list_all();
        let tool = tools
            .iter()
            .find(|t| t.name == "security_status")
            .expect("`security_status` advertised");
        let desc = tool.description.as_deref().unwrap_or_default();
        for claim in [
            "TWO SEPARATELY SCOPED SECTIONS",
            "THIS HOST",
            "ONE PROJECT",
            // The machine half is not a has-run claim. The phrase carrying that
            // moved when issue #464 replaced `ready: bool`; the property did not.
            "whether anything has been run",
            "NEVER means current",
            "NOT a clean repository",
        ] {
            assert!(desc.contains(claim), "missing `{claim}` from: {desc}");
        }
    }

    /// A readiness claim has to name what it has actually checked, and the model
    /// only ever sees this string (issue #464).
    ///
    /// `ready` used to be computed from asset provisioning alone, so a host with the
    /// rules installed and `semgrep` absent read as `ready` and the run then failed.
    /// The three states exist because the remedy differs, and the one Roteiro
    /// refuses to perform has to be stated as a refusal rather than left as a gap —
    /// otherwise a model reads "not ready" and offers to install it.
    #[cfg(feature = "execution")]
    #[test]
    fn security_status_description_says_what_ready_has_checked() {
        let server = seeded();
        let tools = server.tool_router.list_all();
        let tool = tools
            .iter()
            .find(|t| t.name == "security_status")
            .expect("`security_status` advertised");
        let desc = tool.description.as_deref().unwrap_or_default();
        for claim in [
            "THREE states",
            "assets provisioned AND the analyzer's program on PATH",
            "assets-not-provisioned",
            "binary-not-found",
            "ROTEIRO NEVER INSTALLS ANALYZERS",
            // Both facts, so a host missing both is not two round trips.
            "are ALWAYS present",
            // And the bound on the claim: this is the host, not the sandbox.
            "ON THIS HOST",
            "does not inspect the image store",
        ] {
            assert!(desc.contains(claim), "missing `{claim}` from: {desc}");
        }
    }

    /// The sibling of `every_context_tool_states_its_fixed_bound`, for the other
    /// tool that answers without a `limit`.
    ///
    /// `security_status` reports counts rather than findings, so its size is fixed
    /// by what is installed and what has run rather than by how much was found.
    /// There is no schema field for that to be declared in, so the declaration
    /// lives in the description — and this is what keeps it there. Without it, the
    /// obvious "add a `limit` for consistency" change would advertise a bound
    /// nothing honours, which is the drift #402 fixed once already.
    #[cfg(feature = "execution")]
    #[test]
    fn security_status_states_why_it_needs_no_bound() {
        let server = seeded();
        let tools = server.tool_router.list_all();
        let tool = tools
            .iter()
            .find(|t| t.name == "security_status")
            .expect("`security_status` advertised");
        let props = tool
            .input_schema
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .expect("`security_status` declares properties");
        assert!(
            props.get("limit").is_none(),
            "`security_status` must not advertise a `limit` it does not honour: \
             {props:?}",
        );
        let desc = tool.description.as_deref().unwrap_or_default();
        for claim in ["needs no `limit`", "COUNTS, NEVER FINDINGS"] {
            assert!(desc.contains(claim), "missing `{claim}` from: {desc}");
        }
    }

    /// **Read the content, not the name — and this surface is the only one that
    /// says so here.**
    ///
    /// The served-chat surface states this in the system turn
    /// `rto_serve::advertised_system_prompt` builds, which is why #675 could take
    /// it out of `tool_text::SEARCH` without losing it *there*. MCP has no system
    /// turn: a client gets `instructions` and a `tools/list`, so if this sentence
    /// is in neither, nothing tells an MCP client to open a hit before describing
    /// it — Copilot caught exactly that on the #675 PR.
    ///
    /// Asserted against `instructions` rather than against the description on
    /// purpose. The point of the move was to state it **once per session** instead
    /// of once per tool listing, and a test that would also pass with the sentence
    /// back in the description would not hold that distinction. It is scoped to a
    /// server that actually offers `search`, because a restricted one has no hits
    /// to open.
    #[test]
    fn an_mcp_client_is_told_to_open_a_hit_rather_than_read_its_name() {
        let instructions = seeded().get_info().instructions.unwrap_or_default();
        assert!(
            instructions.contains("`search`"),
            "this fixture must advertise `search`, or the assertion below is \
             vacuous: {instructions}",
        );
        for clause in [
            "Read each hit's `snippet`",
            "call `explain` on its key",
            "never answer from a node's name alone",
        ] {
            assert!(
                instructions.contains(clause),
                "MCP has no system turn, so `instructions` is the only place this \
                 surface can carry `{clause}` — #675 moved it here out of \
                 `tool_text::SEARCH` and it must not go missing: {instructions}",
            );
        }
    }

    /// A server that does not offer the `security_*` pair must not announce it.
    ///
    /// The instructions string is assembled rather than a single literal precisely
    /// so this can hold, and it is the kind of thing that goes wrong silently: a
    /// model told a tool exists and then handed `unknown tool` has been misinformed
    /// by its own server.
    #[test]
    fn the_instructions_announce_the_security_tools_exactly_when_they_exist() {
        let info = seeded().get_info();
        let instructions = info.instructions.unwrap_or_default();
        let announced = instructions.contains("`security_list`");
        assert_eq!(
            announced,
            cfg!(feature = "execution"),
            "the instructions must name `security_list` exactly when the build \
             offers it: {instructions}",
        );
        // The read-only rule is stated whichever way that went — and under
        // `execution` it is stated *with its exception named*, because a flat "every
        // tool here is read-only" beside a `sandbox_clear` that deletes is a claim a
        // model can catch its own server out on.
        if cfg!(feature = "execution") {
            assert!(
                instructions.contains("none of them changes it, with exactly one exception")
                    && instructions.contains("`sandbox_clear`")
                    && instructions.contains("changes nothing the graph says"),
                "{instructions}"
            );
        } else {
            assert!(
                instructions.contains("Every tool here is read-only"),
                "{instructions}"
            );
        }
    }

    /// [`EVERY_TOOL`] is hand-written because a `#[cfg]`-ed-out route leaves
    /// nothing to enumerate. This is what stops it going stale: whatever feature
    /// set the suite runs under, every route this build offers must be in the
    /// list, and under `--all-features` the two are the same set.
    #[test]
    fn every_tool_covers_this_build() {
        let in_build: BTreeSet<String> = tool_names().into_iter().collect();
        let declared: BTreeSet<String> =
            super::EVERY_TOOL.iter().map(|s| (*s).to_owned()).collect();
        let missing: Vec<&String> = in_build.difference(&declared).collect();
        assert!(
            missing.is_empty(),
            "`EVERY_TOOL` is missing routes this build offers: {missing:?} — a new \
             tool has to be added there or `--tools` will refuse its name as a typo",
        );
        #[cfg(feature = "execution")]
        assert_eq!(
            in_build, declared,
            "with every feature on, `EVERY_TOOL` must be exactly what is routed",
        );
    }

    /// `--tools read-only` has to stay correct when a second mutating tool lands,
    /// so it is defined as the complement of [`MUTATING_TOOLS`] rather than as a
    /// list somebody remembers to edit. This pins the two ends together: the
    /// alias's result, and the sentence `get_info` puts in front of a model.
    #[test]
    fn read_only_is_every_tool_but_the_mutating_ones() {
        let restriction = super::restrict(&[super::READ_ONLY.to_owned()]).expect("resolves");
        let Advertised::Only(allowed) = restriction.advertised else {
            panic!("`read-only` must restrict");
        };
        for name in tool_names() {
            assert_eq!(
                allowed.contains(&name),
                !super::MUTATING_TOOLS.contains(&name.as_str()),
                "`read-only` and `MUTATING_TOOLS` disagree about `{name}`",
            );
        }
        // And the server says the same thing in the prose a model actually reads.
        let server = GraphServer::new(
            Arc::new(rto_graph::Workspace::single(
                "test",
                rto_graph::Store::open_in_memory().expect("store"),
            )),
            &Advertised::Only(allowed),
        );
        let instructions = server.get_info().instructions.unwrap_or_default();
        assert!(
            instructions.contains("Every tool here is read-only"),
            "a read-only surface must claim to be one: {instructions}",
        );
    }

    /// **Advertisement is not authority.** A tool the operator withheld must be
    /// absent from `tools/list` *and* unreachable by `tools/call` — a client that
    /// already knows the name is exactly the case a restriction is for.
    ///
    /// `list_tools` and `get_tool` are both generated from
    /// `#[tool_handler(router = self.tool_router)]`, and `call_tool` is generated
    /// from the same expression, so asserting the first two holds the third: they
    /// are three reads of one object rather than three lookups that could differ.
    #[cfg(feature = "execution")]
    #[test]
    fn a_withheld_tool_is_neither_listed_nor_dispatchable() {
        use rmcp::ServerHandler as _;

        let restriction =
            super::restrict(&["search".to_owned(), "explain".to_owned()]).expect("resolves");
        let server = seeded_with(&restriction.advertised);
        let listed: BTreeSet<String> = server
            .tool_router
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        assert_eq!(
            listed,
            // The class index rides along with every restriction and is the only
            // addition (#664) — a restricted server that could not say what it
            // withheld would leave a model reporting a missing capability.
            ["explain", "list_tool_classes", "search"]
                .into_iter()
                .map(str::to_owned)
                .collect::<BTreeSet<String>>(),
        );
        assert!(
            server.get_tool("sandbox_clear").is_none(),
            "a withheld tool must not be dispatchable",
        );
        assert!(
            server.get_tool("search").is_some(),
            "an allowed tool must be"
        );
        // The announcement has to follow the surface too, or a model is told a
        // tool exists and then handed `unknown tool` by its own server.
        let instructions = server.get_info().instructions.unwrap_or_default();
        assert!(
            !instructions.contains("`sandbox_clear`") && instructions.contains("`search`"),
            "{instructions}",
        );
    }

    /// A typo must not pass. It would otherwise silently drop the tool it meant
    /// to keep, and the operator would have no way to tell that from a tool that
    /// was never there.
    #[test]
    fn an_unknown_tool_name_is_refused_rather_than_ignored() {
        let err = super::restrict(&["explian".to_owned()]).expect_err("must refuse");
        assert_eq!(err, super::RestrictError::Unknown("explian".to_owned()));
        assert!(
            err.to_string().contains("explain"),
            "the refusal must list the real names: {err}",
        );
    }

    /// **The failure this whole feature is a response to**, reproduced on our side
    /// of the wire and refused. In omnigent#5178 an allow-list parsed to zero
    /// entries and the client read that as "no restriction", so a server the
    /// operator believed restricted advertised everything. An empty result here is
    /// a refusal to start.
    #[test]
    fn a_restriction_that_resolves_to_nothing_refuses_rather_than_serving_everything() {
        for names in [vec![], vec![String::new()], vec!["  ".to_owned()]] {
            assert_eq!(
                super::restrict(&names),
                Err(super::RestrictError::Empty),
                "{names:?} must not be read as `advertise everything`",
            );
        }
    }

    /// The byte figure `--tools` exists to move, measured the way a client
    /// experiences it. `polly-local`'s ten-tool surface is the case from #584.
    #[cfg(feature = "execution")]
    #[test]
    fn restricting_the_surface_removes_advertised_bytes() {
        let all = super::advertised_bytes(&Advertised::All);
        let restriction = super::restrict(&[super::READ_ONLY.to_owned()]).expect("resolves");
        let read_only = super::advertised_bytes(&restriction.advertised);
        assert!(
            read_only < all,
            "dropping the mutating tool must drop bytes: {read_only} vs {all}",
        );
    }

    /// The longest prefix of `text` that appears **verbatim** inside a JSON
    /// string, capped so it stays a needle rather than the haystack.
    ///
    /// Needed because the surface a client receives is JSON: a description
    /// containing `"` or `\` is escaped on the wire, so searching the raw constant
    /// would fail to match text that *is* there — a test that passes for the wrong
    /// reason. Cutting at the first character JSON would rewrite gives a needle
    /// that matches if and only if the prose shipped. Capped by `char_indices` and
    /// not by byte slicing: these descriptions contain em dashes.
    fn wire_needle(text: &str) -> &str {
        let stop = text.find(['"', '\\', '\n']).unwrap_or(text.len());
        let cap = text
            .char_indices()
            .nth(60)
            .map_or(text.len(), |(byte, _)| byte);
        &text[..stop.min(cap)]
    }

    /// **A class an operator did not load contributes no tokens.**
    ///
    /// The property, measured on the thing that actually costs: the serialized
    /// `tools/list` payload, not the route count. A test that counted routes would
    /// pass while the descriptions still shipped — and the descriptions are 65% of
    /// this surface's bytes, so they are the whole point of withholding a class.
    ///
    /// Two assertions per withheld tool, because they fail for different reasons.
    /// The **name** check catches a route that survived the restriction. The
    /// **prose** check catches the subtler one: a description reaching the wire
    /// under some other tool's route, or a listing assembled from anywhere other
    /// than the restricted router. The name is checked structurally rather than as
    /// a substring, because `path` is a substring of "file paths" in another
    /// tool's description and a substring test would fail on prose that is
    /// legitimately there.
    #[test]
    fn a_withheld_class_contributes_no_bytes_to_the_serialized_surface() {
        let full = super::tool_descriptions();
        for (class, tools) in crate::tool_class::CLASSES {
            let others: Vec<String> = crate::tool_class::class_names()
                .into_iter()
                .filter(|c| *c != class)
                .map(str::to_owned)
                .collect();
            let restriction = super::restrict(&others).expect("three classes resolve");
            let listed = GraphServer::routes(&restriction.advertised).list_all();
            let names: BTreeSet<String> = listed.iter().map(|t| t.name.to_string()).collect();
            let wire = serde_json::to_string(&listed).expect("the JSON a client receives");
            for tool in tools {
                assert!(
                    !names.contains(*tool),
                    "withholding `{class}` left `{tool}` advertised",
                );
                let Some(text) = full.get(*tool) else {
                    continue;
                };
                let needle = wire_needle(text);
                assert!(
                    needle.len() > 20,
                    "`{tool}`'s needle is too short to prove anything: {needle:?}",
                );
                assert!(
                    !wire.contains(needle),
                    "withholding `{class}` still shipped `{tool}`'s description to the \
                     client. The route may be gone while its prose is not, and the prose \
                     is what costs: {needle:?}",
                );
            }
        }
    }

    /// **A class that is loaded advertises every tool it names**, with the same
    /// description an unrestricted server would have sent.
    ///
    /// The other half of the property, and the one that catches an over-eager
    /// filter. A restriction that removed too much would satisfy the withholding
    /// test perfectly — the strongest possible "no tokens" result is a server that
    /// advertises nothing — so the saving has to be paid for against a surface
    /// that is still complete.
    ///
    /// # The class this build carries nothing of
    ///
    /// `security` and `sandbox` are `execution`-gated, so on a build without that
    /// feature `--tools security` names a real class and selects nothing. That is
    /// [`RestrictError::Empty`] — the refusal-to-start that exists so a
    /// restriction resolving to nothing can never be read as "no restriction"
    /// (omnigent#5178). It is **asserted** here rather than skipped: a branch that
    /// quietly `continue`d would report `ok` on a build where this test checked
    /// nothing at all, which is the shape of a test that passes without exercising
    /// its subject.
    #[test]
    fn a_loaded_class_advertises_every_tool_it_names() {
        let full = super::tool_descriptions();
        let in_build: BTreeSet<String> = tool_names().into_iter().collect();
        for (class, tools) in crate::tool_class::CLASSES {
            if !tools.iter().any(|t| in_build.contains(*t)) {
                assert_eq!(
                    super::restrict(&[class.to_owned()]),
                    Err(super::RestrictError::Empty),
                    "`--tools {class}` selects nothing this build carries, and a \
                     restriction that resolves to nothing must refuse to start rather \
                     than serve the full surface",
                );
                continue;
            }
            let restriction = super::restrict(&[class.to_owned()]).expect("a class resolves");
            let listed = GraphServer::routes(&restriction.advertised).list_all();
            let described: std::collections::BTreeMap<String, String> = listed
                .iter()
                .map(|t| {
                    (
                        t.name.to_string(),
                        t.description.as_deref().unwrap_or_default().to_owned(),
                    )
                })
                .collect();
            for tool in tools.iter().filter(|t| in_build.contains(**t)) {
                let got = described.get(*tool).unwrap_or_else(|| {
                    panic!("`--tools {class}` must advertise its own member `{tool}`")
                });
                assert_eq!(
                    Some(got),
                    full.get(*tool),
                    "`{tool}` is advertised under `{class}` with a different description \
                     than an unrestricted server sends",
                );
            }
        }
    }

    /// Naming every class must be indistinguishable from naming no restriction at
    /// all — which is what makes the taxonomy **total**.
    ///
    /// A tool in no class would be dropped here and nowhere else: reachable by
    /// name, unreachable through the aliases, and invisible to
    /// `list_tool_classes`. That is a surface with two halves that disagree, and
    /// it is the failure a route-counting test would miss.
    #[test]
    fn selecting_every_class_is_the_unrestricted_surface() {
        let names: Vec<String> = crate::tool_class::class_names()
            .into_iter()
            .map(str::to_owned)
            .collect();
        let restriction = super::restrict(&names).expect("every class resolves");
        assert_eq!(
            GraphServer::routes(&restriction.advertised)
                .list_all()
                .into_iter()
                .map(|t| t.name.to_string())
                .collect::<BTreeSet<String>>(),
            tool_names().into_iter().collect::<BTreeSet<String>>(),
            "every class together must be the whole surface — a tool in no class \
             would be silently unreachable through `--tools`",
        );
        assert_eq!(
            super::advertised_bytes(&restriction.advertised),
            super::advertised_bytes(&Advertised::All),
            "and cost exactly the same bytes",
        );
    }

    /// Every routed tool has exactly one class, or is the class index itself.
    ///
    /// The compile-time half is impossible — the table is data — so this is what
    /// fails when a tool is added to the router and not to
    /// [`crate::tool_class::CLASSES`].
    #[test]
    fn every_tool_has_exactly_one_class() {
        for name in tool_names() {
            if name == crate::tool_class::CLASS_INDEX_TOOL {
                assert!(
                    crate::tool_class::class_of(&name).is_none(),
                    "the class index must belong to no class",
                );
                continue;
            }
            assert!(
                crate::tool_class::class_of(&name).is_some(),
                "`{name}` is routed but is in no class — `--tools <class>` cannot \
                 reach it and `list_tool_classes` will not mention it",
            );
        }
    }

    /// The class index survives every restriction, including one that names no
    /// class at all — and is dispatchable, not merely listed.
    ///
    /// This is the discoverability requirement itself. Without it a `query`-only
    /// server is indistinguishable, from the client side, from a Roteiro that
    /// cannot read analyzer findings, and a model asked about them answers
    /// "Roteiro cannot do that" — which is false and which nothing on the wire
    /// could correct.
    #[cfg(feature = "execution")]
    #[test]
    fn the_class_index_is_advertised_whatever_was_withheld() {
        use rmcp::ServerHandler as _;

        for names in [
            vec!["query".to_owned()],
            vec!["search".to_owned()],
            vec![super::READ_ONLY.to_owned()],
            vec!["security".to_owned(), "sandbox".to_owned()],
        ] {
            let restriction = super::restrict(&names).expect("resolves");
            let server = seeded_with(&restriction.advertised);
            assert!(
                server
                    .tool_router
                    .list_all()
                    .iter()
                    .any(|t| t.name == crate::tool_class::CLASS_INDEX_TOOL),
                "`--tools {names:?}` withheld the class index",
            );
            assert!(
                server
                    .get_tool(crate::tool_class::CLASS_INDEX_TOOL)
                    .is_some(),
                "listed but not dispatchable — advertisement is not authority, and \
                 neither is its absence",
            );
        }
    }

    /// A restricted server tells a model to ask, and an unrestricted one does not.
    ///
    /// The instructions are paid for on every turn exactly as a description is, so
    /// the pointer is only worth its bytes where it changes an answer. On a full
    /// server no class is withheld and there is nothing for a model to recover.
    #[cfg(feature = "execution")]
    #[test]
    fn only_a_restricted_server_points_a_model_at_the_class_index() {
        use rmcp::ServerHandler as _;

        let restriction = super::restrict(&["query".to_owned()]).expect("resolves");
        let narrow = seeded_with(&restriction.advertised)
            .get_info()
            .instructions
            .unwrap_or_default();
        assert!(
            narrow.contains("list_tool_classes") && narrow.contains("not a missing capability"),
            "a restricted server must say a class was withheld: {narrow}",
        );
        let full = seeded().get_info().instructions.unwrap_or_default();
        assert!(
            !full.contains("not a missing capability"),
            "an unrestricted server has nothing to recover, and the sentence costs \
             tokens on every turn: {full}",
        );
    }

    /// The same property through a **real** server, and deliberately ungated so it
    /// runs in every feature set the suite is built under.
    ///
    /// `only_a_restricted_server_points_a_model_at_the_class_index` asserts this
    /// too and is `#[cfg(feature = "execution")]`, because its other half needs the
    /// `execution` tools to withhold — and that gate is exactly why the defect
    /// survived: under `--all-features` and under `default` the clause was correct,
    /// and it was wrong only in the build where no test ran. An ungated assertion
    /// over `Advertised::All` costs one line and covers the configuration the gated
    /// one cannot reach.
    #[test]
    fn no_feature_set_makes_an_unrestricted_server_claim_a_restriction() {
        use rmcp::ServerHandler as _;

        let instructions = seeded().get_info().instructions.unwrap_or_default();
        assert!(
            !instructions.contains("not a missing capability"),
            "an unrestricted server withheld nothing and must not say it did — the \
             `security_*`/`sandbox_*` tools are absent from an `execution`-less build \
             for a reason no `--tools` flag can undo, and calling that a withholding \
             both misinforms the model and spends tokens on every turn: {instructions}",
        );
    }

    /// **An unrestricted server says nothing about withholding — whatever feature
    /// set it was built with.**
    ///
    /// The sentence is only worth its tokens where it changes an answer, and on an
    /// unrestricted server there is no withheld class for a model to recover. That
    /// is easy to get right for the build the suite happens to run under and easy
    /// to get wrong for the others: `security_*` and `sandbox_*` are
    /// `#[cfg(feature = "execution")]`, so a version of this that decided from the
    /// router alone emitted the clause on **every** server built without that
    /// feature — claiming a restriction nobody applied, in exactly the
    /// configuration that saved nothing by it.
    ///
    /// Driven through synthetic predicates rather than a real server, because the
    /// property is about the *relationship* between two sets and a test built from
    /// one router can only ever exercise the feature set it was compiled under. A
    /// `#[cfg(feature = "execution")]` test here would have passed under
    /// `--all-features` and under `default` while the defect sat in the build CI
    /// runs as `no-default-features`.
    #[test]
    fn an_unrestricted_server_never_claims_a_class_was_withheld() {
        let index = crate::tool_class::CLASS_INDEX_TOOL;
        let execution_tool = |name: &str| {
            crate::tool_class::class_of(name).is_some_and(|c| c == "security" || c == "sandbox")
        };

        // Every tool present: nothing withheld, nothing to say.
        assert_eq!(
            super::withheld_class_clause(|_| true, |_| true),
            None,
            "a full server has no withheld class and must not spend tokens saying so",
        );

        // The build without `execution`. Its `security_*`/`sandbox_*` routes are
        // absent from an unrestricted server too, and absent is not withheld.
        let carried = |name: &str| !execution_tool(name);
        assert_eq!(
            super::withheld_class_clause(carried, carried),
            None,
            "a build that does not carry the `execution` tools withheld nothing — \
             saying otherwise tells a model a restriction was applied that was not, \
             and costs tokens on every turn to do it",
        );

        // And the case the clause exists for is still detected: this build carries
        // everything and the operator kept only `query`.
        let kept = |name: &str| {
            name == index || crate::tool_class::class_of(name).is_some_and(|c| c == "query")
        };
        assert!(
            super::withheld_class_clause(|_| true, kept).is_some(),
            "a genuinely restricted server must still point a model at the index",
        );
        // Including on the feature-poor build, where `quality` is still withheld
        // by the operator even though `security`/`sandbox` are merely absent.
        assert!(
            super::withheld_class_clause(carried, |n| carried(n) && kept(n)).is_some(),
            "a restriction and a feature gate can coexist, and the restriction is \
             still worth telling a model about",
        );

        // A server with no index to point at says nothing, even with a class
        // genuinely withheld: telling a model to call a tool that is not
        // advertised is worse than silence. `restrict` cannot produce this, and
        // that is the point — the clause must not depend on it having been used.
        let no_index = |name: &str| {
            crate::tool_class::class_of(name).is_some_and(|c| c == "query") && name != index
        };
        assert!(
            crate::tool_class::CLASSES
                .iter()
                .any(|(_, tools)| tools.iter().any(|t| !no_index(t))),
            "the fixture must actually withhold something, or the next assertion \
             passes for the wrong reason",
        );
        assert_eq!(super::withheld_class_clause(|_| true, no_index), None);
    }

    /// The saving the issue was filed for, asserted as a floor on the **serialized
    /// surface** rather than as the exact figure.
    ///
    /// Measured here: withholding `security` and `sandbox` drops 7,936 of 19,155
    /// bytes — 41% — which is the ~1,984 tokens a code-navigation session pays
    /// every turn to advertise tools it will never call. The assertion is a floor
    /// because the exact number moves whenever anyone edits a description, and a
    /// test that failed on prose edits would be deleted rather than fixed.
    #[cfg(feature = "execution")]
    #[test]
    fn withholding_security_and_sandbox_removes_a_third_of_the_surface() {
        let all = super::advertised_bytes(&Advertised::All);
        let restriction = super::restrict(&["query".to_owned(), "quality".to_owned()])
            .expect("two classes resolve");
        let narrowed = super::advertised_bytes(&restriction.advertised);
        assert!(
            narrowed * 10 <= all * 7,
            "`--tools query,quality` must remove at least 30% of the advertised \
             bytes; it removed {} of {all}",
            all - narrowed,
        );
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
    async fn check_tool_runs_against_the_repository_of_a_hosted_project() {
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
        let server = GraphServer::new(Arc::new(ws), &Advertised::All);

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
        let out = GraphServer::new(Arc::new(ws), &Advertised::All)
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
        let server = GraphServer::new(Arc::new(ws), &Advertised::All);

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

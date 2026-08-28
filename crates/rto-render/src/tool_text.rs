//! The **one** place each shared tool description is written.
//!
//! `roteiro:ignore-file` — this file is prose *about* the tools, and two of them
//! are about intent debt: `debt`'s description has to name TODO/FIXME/HACK and
//! `todo!()`/`unimplemented!()` stubs, and `debt_density`'s has to name the prose
//! matches ("for now", "deferred", "tbd") a reader might otherwise be surprised
//! by. Scanned, those descriptions register as three markers the repository does
//! not have. The same opt-out is on `markers.rs`, `check_cli.rs` and the
//! tool-choice fixture, for the same reason: a file that documents a scanner is
//! not a file that owes work.
//!
//! # Why a module of string constants
//!
//! Four tools are advertised on two surfaces: the served-chat registry in
//! `roteiro`, and this crate's MCP server. The prose was written twice, and
//! nothing compared the copies — so they drifted. Measured when this module was
//! introduced: `sandbox_status` differed by 363 bytes, `sandbox_clear` by 197,
//! `security_list` by 55. A served model and an MCP client were told materially
//! different things about the same tool, including `sandbox_clear`, the one tool
//! on either surface that destroys anything.
//!
//! A description is not decoration here. It carries the warnings that prevent the
//! likeliest misuses — that `no-analyzer-on-record` is not a clean repository,
//! that `bytes.exclusive` and not `bytes.total` is what clearing an image frees —
//! so one surface quietly holding an older draft is a real divergence.
//!
//! # Why the MCP side still repeats the text
//!
//! It should not have to, and this is the closest the framework allows. `rmcp`'s
//! `#[tool(description = …)]` is parsed by `darling` into a `String`, so it takes
//! a **string literal** and rejects a path: `description = SANDBOX_STATUS` fails
//! to compile with *"Unexpected type `path`"*. Nor can the served side simply read
//! the MCP surface, because `serve` does not imply `mcp` — a serve-only build has
//! no MCP module to ask.
//!
//! So the constant here is the **source**, `roteiro` uses it directly, and the
//! literal `mcp.rs` is forced to carry is compared against it by
//! `both_tool_surfaces_describe_a_tool_the_same_way`. One authority, one
//! mechanically-checked copy — rather than two copies and a hope.
//!
//! Ungated on purpose: a `serve` build without `mcp` needs these too.

/// `sandbox_status` — what the machine-global sandbox image store holds.
pub const SANDBOX_STATUS: &str = "Report what the machine-global SANDBOX IMAGE STORE holds: one row per cached container \
    image with its reference, digests, layer count, objects on disk, and size split into \
    layers, extracted trees, the derived ext4 disk image and the guest base. MACHINE-GLOBAL, \
    and `scope` says so: one store per asset root, shared by every repository here, so never \
    attribute a size to the project under discussion. No `project` argument — \
    `security_status` is the tool with two scopes. `bytes.total` is what an image references; \
    `bytes.exclusive` is what dropping that image alone would free. They differ when images \
    share a layer, so quote `exclusive` when saying what clearing one would give back. \
    `objects` counts pulled content (manifest, config, one per distinct layer). Extracted \
    trees and disk images are a cache below this one, built on first run, so a pulled-only \
    image is complete without them; `disk_image_built`/`base_disk_built` say whether it has \
    run. `unattributed` is bytes no image claims; `preserved` is state no pinned digest \
    re-obtains, which `sandbox_clear` never removes. Read this before `sandbox_clear` and show \
    the user the numbers: a destructive verb with no way to see what it will destroy is \
    invoked blind. Every `reference` here is a value `sandbox_clear` accepts as `image`. No \
    `limit`: one row per image, counts and sizes, never findings. Read-only.";

/// `sandbox_clear` — delete cached images; the one tool that changes anything.
pub const SANDBOX_CLEAR: &str = "DELETE cached container images from the machine-global sandbox image store, and report \
    what that freed. The one tool here that changes anything; everything it drops is \
    re-obtainable from a pinned digest, so it costs a re-download and never information. It \
    cannot reach findings, memory or the graph. MACHINE-GLOBAL: one store per asset root, \
    shared by every repository this server hosts, so clearing for one project slows the next \
    sandboxed run for all. No `project` argument; `scope` is `machine`. Call `sandbox_status` \
    first and show the user what is cached and what it costs — a re-pull is minutes to tens of \
    minutes and gigabytes. `image` and `everything` are DIFFERENT REQUESTS with no default: \
    pass `image` with a reference from `sandbox_status`, or `everything: true`. Neither is an \
    error and does not mean everything; both is an error. `dry_run: true` removes nothing and \
    `applied` says which happened. Report what it freed: quote `freed_bytes`, with \
    `store_bytes_before`/`store_bytes_after` either side, rather than saying it worked. \
    `retained` re-checks every surviving image against the disk afterwards; if any `complete` \
    is false say so prominently — that is a damaged store, not a successful clear, and \
    `roteiro security prefetch` is the repair. It refuses rather than guessing: a registered \
    box, an unrecognised entry under the store root, or an index row pointing outside it each \
    stop it with nothing removed.";

/// `security_list` — stored findings, with the run evidence behind them.
pub const SECURITY_LIST: &str = "List the SECURITY FINDINGS stored for this repository: every live findings layer with its \
    run evidence (analyzer, version, backend, isolation, advisory database, report digest) and \
    a page of findings. READ `coverage` FIRST. `no-analyzer-on-record` is a real outcome and \
    NOT a clean repository — it carries NO `report` at all, so if there is no `report`, \
    nothing was checked and you must say so rather than report zero findings. An analyzer that \
    ran and found nothing is the other case: `coverage` is `analyzed` and `findings` is 0. \
    Bounded, and it says when it bound something. `limit` is 1-100 (default 20) — no unlimited \
    setting — and is findings PER LAYER; each layer carries its true `findings` count, the \
    `page` returned, `truncated`, and how many were `omitted`. A page keeps the most severe \
    findings first, so what is omitted is the least severe — never conclude a severity is \
    absent from a truncated page. `cross_reference` is a view over those findings, not a \
    replacement: it groups dependency advisories both analyzers reported, `confirmed_by` \
    counts how many, `1` is normal rather than a discrepancy, and the `findings` total is \
    unchanged by it. Read-only: it cannot run an analyzer or ingest a report. Ask the user to \
    run `roteiro security run` or `roteiro security ingest` — a tool call is not a person \
    consenting to execution.";

/// `security_status` — readiness, in two separately scoped halves.
pub const SECURITY_STATUS: &str = "Report SECURITY READINESS in TWO SEPARATELY SCOPED SECTIONS; report them separately, never \
    merged. `machine`: this HOST — the pinned-asset cache under `asset_root`, and each \
    analyzer's coverage matrix with `host_readiness`. Identical for every project here, and \
    says nothing whatsoever about whether anything has been run. `host_readiness` has THREE \
    states with different remedies: `ready` (assets provisioned AND the analyzer's program on \
    PATH); `assets-not-provisioned` (ask the user to run `roteiro security prefetch`); \
    `binary-not-found` (`missing_programs` names it, and ROTEIRO NEVER INSTALLS ANALYZERS — \
    ask the user to install it or to `roteiro security ingest` a report from elsewhere). Both \
    underlying facts (`assets_provisioned`, `missing_programs`) are ALWAYS present, so when \
    the state is not `ready` read both: a host can lack both and `host_readiness` names only \
    the first remedy. Do not read `ready` as more than it says: it is readiness to run ON THIS \
    HOST. The sandboxed backend supplies analyzers from a digest-pinned image, so \
    `binary-not-found` does not block it, and this tool does not inspect the image store, so \
    it reports no sandbox verdict. `repository` describes ONE PROJECT — the one in its \
    `project` field, chosen by the `project` argument — which findings layers are live, how \
    many findings each holds, and the age of the advisory database behind each. \
    `possibly_stale: true` whenever advisory data is involved and NEVER means current; `false` \
    means only that there is no advisory axis. Read `repository.coverage` before concluding \
    anything: `no-analyzer-on-record` carries no layers and means nothing has been analyzed — \
    NOT a clean repository. COUNTS, NEVER FINDINGS; use `security_list` for those. It needs no \
    `limit`. Read-only: it cannot provision, and `roteiro security prefetch` needs human \
    consent, so ask the user to run it.";

/// `list_tool_classes` — the index that keeps a withheld class discoverable.
///
/// Deliberately the shortest description here. It stands in for whole classes of
/// prose an operator chose not to advertise, and a long stand-in would spend the
/// saving it exists to protect: at ~90 tokens it replaces the `security` class's
/// ~819 or the `sandbox` class's ~713.
pub const LIST_TOOL_CLASSES: &str = "Name this server's tool CLASSES — `query`, `quality`, `security`, `sandbox` — the tools in \
    each, and which are LOADED here. Call it before telling a user Roteiro cannot do \
    something: a class can be left out at startup to keep its descriptions out of every \
    turn's prompt, and `not-loaded-here` means not advertised to this session, NOT a missing \
    capability. Report the class name so the user can restart the server with it. Takes no \
    arguments. Read-only.";

/// `check`.
pub const CHECK: &str = "Run the AUTHORED-LAYER DRIFT CHECK — the same gate `roteiro check` exits non-zero on and \
    the pre-commit hook reads — and return its verdict as data: ADR `[[path#Symbol]]` links \
    that no longer resolve, `@rto:` annotations pointing at unknown or superseded ADRs, \
    malformed ADRs, and duplicate `adr-id`s. READ `gate` FIRST. It is `pass`, `fail`, or \
    `not-run`, and `not-run` is a real outcome: a check needs the project's repository on disk \
    and a graph synced from the current HEAD, and when it cannot have both it refuses rather \
    than answering about a tree that is nobody's. A `not-run` result carries NO `report` at \
    all — so if you are looking for `violations` and there is no `report`, nothing was checked \
    and you must say so rather than report a clean repository. `not_run_reason` says what to \
    fix (usually: run `roteiro sync`). Read-only: it does not rebuild the graph, which is the \
    one thing the CLI gate does that this cannot.";

/// `config_secrets`.
pub const CONFIG_SECRETS: &str = "Inventory the SECRET-NAMED config keys in the graph: their file paths, their key names, \
    and whether each value was redacted before being stored (`state` = redacted | declared | \
    present). Answers \"which of this repo's config surfaces deal in credentials\" and \"did \
    anything unredacted get into this graph\". THIS IS NOT A SECRET SCANNER — state the limits \
    when you report it, and never imply a security guarantee. It CANNOT find a hardcoded \
    credential in source code: it reads config-key nodes, so a token in a Rust or Python \
    string literal produces nothing here and is invisible. It CANNOT judge whether a value is \
    valid, because it never sees one — values are redacted before they reach the store. It \
    CANNOT tell a real secret from a placeholder: `API_TOKEN=changeme` in a committed \
    `.env.example` and a live token are the same row. And an EMPTY RESULT DOES NOT MEAN THERE \
    ARE NO SECRETS — it means no config key is secret-NAMED; a credential under an innocuous \
    key like `dsn` or `endpoint` never appears. If asked to scan for secrets, say plainly that \
    this tool cannot do it. `limit` is 1-200 (default 50) — no unlimited setting.";

/// `context`.
pub const CONTEXT: &str = "Fetch a node's CONTEXT BUNDLE: the node, its metadata, and its one-hop provenance-labelled \
    neighbourhood, with a validity `fingerprint` that moves when the node or any neighbour \
    changes. The grounding to answer “what is this and what is it wired to” from. Takes `key` \
    and nothing else. BOUNDED, and it tells you when it bound something: each direction \
    carries at most {cap} edges. When more exist, `truncated` is true, \
    `outgoing.total`/`incoming.total` give the real counts, and `omitted` names each edge kind \
    and how many of it are missing — so an absent `imports` edge means there are none, and a \
    large file's missing definitions are counted rather than silently dropped. Read `omitted` \
    before concluding anything from an absence, and use `explain` or `search` to reach what \
    was left out.";

/// `coupling`.
pub const COUPLING: &str = "Rank symbols by DIRECTED call coupling over `calls` edges: `fan_in` (how many distinct \
    symbols call this one), `fan_out` (how many it calls), `instability` = \
    fan_out/(fan_in+fan_out). `order`=fan_in finds what the codebase most depends on, \
    `order`=fan_out the symbols that reach furthest, `total` (the default) overall coupling. \
    Call edges are resolved by simple name, so a short generically-named function can absorb \
    every call to that name — say so if you report a high `fan_in` on one. `limit` is 1-100 \
    (default 20) — no unlimited setting.";

/// `debt`.
pub const DEBT: &str = "List intent-debt markers found in the codebase — TODO/FIXME/HACK comments, \
    todo!()/unimplemented!() stubs, and deferred-work notes — grouped by category (todo, \
    fixme, hack, stub, deferred). Optional `kind` restricts to given categories. Each marker \
    links to its enclosing symbol or file via a `contains` edge.";

/// `debt_density`.
pub const DEBT_DENSITY: &str = "Rank FILES by intent-debt DENSITY — markers per 1,000 lines — rather than by raw marker \
    count, which ranks the biggest file first by construction. Each row carries `markers`, \
    `lines`, `per_kloc` and a per-category split; `overall_per_kloc` is the repository \
    baseline to read a file's figure against. Use `debt` instead when the question is which \
    markers exist, not where they are concentrated. Two limits to pass on rather than \
    reporting a number as a finding: the denominator is FILE LENGTH — every line, blanks and \
    comments included — not source lines of code, so figures run lower than an SLOC tool's and \
    flatter verbose or generated files; and the markers beneath it include prose matches (`for \
    now`, `deferred`, `tbd`), so a design document can rank as dense debt. This is a \
    measurement, not a gate. `limit` is 1-100 (default 20) — no unlimited setting.";

/// `explain`.
pub const EXPLAIN: &str = "Explain a graph node: its record and its provenance-labelled incoming/outgoing edges. Keys \
    look like `sym:<lang>:<path>#<Name>`, `file:<path>`, `adr:<id>`. A key may be \
    project-qualified (`<project>::<key>`) to follow a cross-repo link into another hosted \
    project (see `list_projects`).";

/// `list_projects`.
pub const LIST_PROJECTS: &str = "List the projects this server hosts (often just one). Pass one as `project` to the other \
    tools to query it (ADR-0008). A single-project server needs no `project`.";

/// `path`.
pub const PATH: &str = "Find a shortest path between two graph nodes, following edges in either direction. Each \
    hop records the edge kind, provenance, and traversal direction (outgoing/incoming). A path \
    lives within one project: a project-qualified `from` (<project>::<key>) selects that \
    project (see list_projects).";

/// `search`.
pub const SEARCH: &str = "Search graph nodes by text — names, keys, paths, and captured content (doc comments, \
    README/ADR/blueprint prose). Returns the top matches with keys and, for content-bearing \
    nodes, a short `snippet` of the node's actual content to ground your answer; curated \
    ADRs/blueprints and READMEs rank first, so this is the entry point for \"what is X / why\" \
    questions. Read the `snippet`, and call `explain` on a returned key for the full content. \
    `limit` is 1-25 (default 10) — there is no unlimited setting; narrow the query instead of \
    asking for more.";

/// The description for `name`, or `None` for a tool this module does not own.
///
/// The lookup exists so [`crate::mcp`] can set descriptions on its routes at
/// build time instead of repeating the prose in a `#[tool(description = …)]`
/// literal. That is what makes this module the **only** definition rather than an
/// authority with a copy beside it.
#[must_use]
pub fn for_tool(name: &str) -> Option<String> {
    let raw = match name {
        "check" => CHECK,
        "config_secrets" => CONFIG_SECRETS,
        "context" => CONTEXT,
        "coupling" => COUPLING,
        "debt" => DEBT,
        "debt_density" => DEBT_DENSITY,
        "explain" => EXPLAIN,
        "list_projects" => LIST_PROJECTS,
        "list_tool_classes" => LIST_TOOL_CLASSES,
        "path" => PATH,
        "sandbox_clear" => SANDBOX_CLEAR,
        "sandbox_status" => SANDBOX_STATUS,
        "search" => SEARCH,
        "security_list" => SECURITY_LIST,
        "security_status" => SECURITY_STATUS,
        _ => return None,
    };
    // Every description goes through the substitution, not just the one that
    // needs it: `CONTEXT` is the only const carrying a `{cap}` placeholder
    // today, and for the other thirteen this is a no-op. An early return for
    // `context` beside a `context` arm in the match would leave a second path
    // that returns the placeholder unreplaced — dead until somebody reorders
    // the function, and then wrong in the output rather than at compile time.
    Some(raw.replace("{cap}", &rto_graph::TOOL_CONTEXT_EDGE_CAP.to_string()))
}

#[cfg(test)]
mod tests {
    use super::for_tool;

    /// No advertised description may still carry a `{…}` placeholder.
    ///
    /// `CONTEXT` holds `{cap}` so the edge cap has one source rather than a
    /// hardcoded `50` on one surface and an interpolation on the other. The risk
    /// that creates is a path returning the raw constant — which is exactly what a
    /// special case beside a `match` arm for the same name would give, dead until
    /// somebody reorders the function and then wrong in a model's prompt rather
    /// than at compile time.
    ///
    /// Asserted over **every** tool, not just `context`: a placeholder added to
    /// another constant later is the same defect, and naming only the one that has
    /// it today would not catch it.
    #[test]
    fn no_description_reaches_a_caller_with_a_placeholder_in_it() {
        for name in [
            "check",
            "config_secrets",
            "context",
            "coupling",
            "debt",
            "debt_density",
            "explain",
            "list_projects",
            "list_tool_classes",
            "path",
            "sandbox_clear",
            "sandbox_status",
            "search",
            "security_list",
            "security_status",
        ] {
            let text = for_tool(name).expect("this module owns every name above");
            assert!(
                !text.contains('{'),
                "`{name}` still carries a placeholder: {text}"
            );
        }
        assert!(for_tool("list_kind").is_none(), "MCP-only, not owned here");
        assert!(for_tool("nope").is_none());
    }

    /// The cap really is substituted, rather than the placeholder merely being
    /// absent because somebody deleted it from the prose.
    #[test]
    fn context_states_the_edge_cap_the_code_enforces() {
        let text = for_tool("context").expect("context");
        assert!(
            text.contains(&format!(
                "at most {} edges",
                rto_graph::TOOL_CONTEXT_EDGE_CAP
            )),
            "the cap in the prose must be the one `bound_edges` applies: {text}"
        );
    }
}

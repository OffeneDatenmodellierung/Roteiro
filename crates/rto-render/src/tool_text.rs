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
//! # What #675 removed, and the rule it used
//!
//! This prose is the **dominant cost of every tooled turn**: measured on the
//! 15-tool surface before the trim, `rto_serve::advertised_system_prompt` rendered
//! 14,586 bytes of which 12,354 — 85% — were these constants, and they reach a
//! local model a second time through its own chat template's tool slot (#681), so
//! this is the one lever that pays on both paths.
//!
//! What went was the prose that **restates a guarantee something else already
//! upholds**, on the surface where it is upheld:
//!
//! * **The schema states it.** `sandbox_clear`'s `dry_run` semantics and the
//!   consequence of naming neither scope are in its `everything`/`dry_run`
//!   argument descriptions, which go out beside this text on both surfaces. Two
//!   tools also declared the *absence* of an argument (*"No `project` argument"*,
//!   *"Takes `key` and nothing else"*) beside a rendered signature that already
//!   showed what they take — and one of those two had gone stale, because
//!   `context` had since gained `project`.
//! * **The code refuses it.** `sandbox_clear`'s three store-integrity refusals
//!   (a registered box, an unrecognised store entry, an index row pointing
//!   outside the root) are `rto_exec::sandbox_store`'s, each with its own test,
//!   and each reaches the caller as an **error** rather than as prose read
//!   beforehand.
//! * **The result body says it.** `no-analyzer-on-record` carries
//!   `rto_exec`'s `NO_RESULT_REASON`, and `check`'s `not-run` carries
//!   `not_run_reason` with no `report` at all — read every time, where a
//!   description is read once.
//! * **The system turn says it.** `search`'s *"read the `snippet`, then `explain`"*
//!   is `rto_serve::advertised_system_prompt`'s grounding rule, stated once for
//!   every tool.
//!
//! What stayed is what **nothing but the words upholds**: look before you delete,
//! quote `freed_bytes`, escalate a `complete: false` retention, `config_secrets`
//! is not a secret scanner, a truncated findings page hides only the least severe.
//! Those were left even where they are long, because no measurement in this
//! repository can yet say what a model loses without them — see #675.
//!
//! # Why the cut is 14% and not the 59% #675 hoped for
//!
//! Because most of what the rule above marks cuttable is **already pinned by a
//! test that was written on purpose**, and each pin encodes a decision this issue
//! has no standing to reverse:
//!
//! | phrase | pinned by | the decision behind it |
//! | --- | --- | --- |
//! | `limit` is 1-`n` … no unlimited setting | `every_limit_tool_advertises_the_bound_it_enforces`, both surfaces | #393: *"a model reads the description even when it does not validate against the schema"* |
//! | `It needs no limit` / `COUNTS, NEVER FINDINGS` | `security_status_advertises_no_bound_on_either_surface`, `security_status_states_why_it_needs_no_bound` | #402: a schema that disagrees with the clamp |
//! | `DIFFERENT REQUESTS` | `the_mutating_tool_states_its_obligations_where_a_model_reads_them`, and its served twin | ADR-0014 v1.6: the obligations that *"do not survive living in a doc comment"* |
//! | `THREE states` … `are ALWAYS present` | `security_status_description_says_what_ready_has_checked`, both surfaces | #464: a host missing both assets and binary must not be two round trips |
//! | `carries NO report` … `rather than report zero findings` | `security_list_description_refuses_the_clean_reading` | the never-run reading, refused where a model reads it |
//!
//! Every one of those is duplication with a schema, a refusal in code, or a
//! result field — and every one was put there **knowing that**, on the stated
//! ground that a model reads this string and may act before it reads anything
//! else. So the honest reading of #675 is not *"the prose is bloated"* but *"the
//! prose is the same fact stated in three places, and the repository has already
//! decided it wants it stated in all three."* Recovering those bytes is a
//! different piece of work from shortening: it means moving the shared statements
//! to the one-per-server places that already exist — `crate::mcp`'s `instructions`
//! and `rto_serve`'s system turn, where #599's working-tree caveat already lives —
//! and re-pointing five tests at the new home. That is a design change, and it
//! needs the interpretation measurement #675 describes, because it trades *"every
//! tool says it"* for *"the server says it once"*.
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
///
/// Trimmed by #675 to the sentences nothing else carries. What went: the absence
/// of `project` and of `limit` (the rendered signature is `sandbox_status()`, so
/// the prose was restating an empty argument list — and unlike the other tools'
/// `limit` clauses this one states no bound, so #393's contract does not reach
/// it), `objects`' definition, and the `reference`↔`image` correspondence
/// (`sandbox_clear`'s `image` argument description states it on both surfaces).
pub const SANDBOX_STATUS: &str = "Report what the machine-global SANDBOX IMAGE STORE holds: one row per cached container \
    image, with its reference, digests, object counts and a size breakdown. MACHINE-GLOBAL, \
    and `scope` says so: one store per asset root, shared by every repository here, so never \
    attribute a size to the project under discussion. `bytes.total` is what an image \
    references; `bytes.exclusive` is what dropping that image alone would free — they differ \
    when images share a layer, so quote `exclusive` when saying what clearing one would give \
    back. Extracted trees and disk images are a cache below this one, built on first run, so \
    a pulled-only image is complete without them. `preserved` is state no pinned digest \
    re-obtains, which `sandbox_clear` never removes. Read this before `sandbox_clear` and show \
    the user the numbers: a destructive verb with no way to see what it will destroy is \
    invoked blind. Read-only.";

/// `sandbox_clear` — delete cached images; the one tool that changes anything.
///
/// # What #675 cut, and why none of it was a safety change
///
/// The scope rule is **enforced, not merely described**, and on both surfaces:
/// `crate::mcp`'s `sandbox_clear` and `roteiro`'s `GraphToolRegistry::sandbox_clear`
/// each refuse `image` with `everything` (*"different requests; pass exactly
/// one"*) and each refuse neither (*"Supplying neither does not mean
/// everything"*), pinned by `sandbox_clear_refuses_a_scope_it_was_not_given` on
/// each. It is stated again in the **argument** descriptions, which go out beside
/// this text on both surfaces (*"Mutually exclusive with `image`; supplying
/// neither is an error"*, *"Report what would be removed and remove nothing"*).
/// So the description's own *"Neither is an error and does not mean everything;
/// both is an error"* was the third statement of one rule, and the second one to
/// be unenforceable prose — it went, along with the `dry_run`/`applied` sentence
/// the `dry_run` argument description already carries.
///
/// The three refusals at the end — a registered box, an unrecognised entry under
/// the store root, an index row pointing outside it — went for a different
/// reason: they are `rto_exec::sandbox_store`'s, each with its own test, and each
/// reaches the caller as an **error**. Prose read beforehand cannot improve on a
/// refusal that already happened.
///
/// **`DIFFERENT REQUESTS` stayed**, not because it is unenforced but because
/// `the_mutating_tool_states_its_obligations_where_a_model_reads_them` and its
/// served twin require it: ADR-0014 v1.6's obligations were deliberately put in
/// the one string a model reads. What also stayed is the part nothing else
/// upholds at all: look before you delete, quote what it freed, and escalate a
/// `complete: false` retention.
pub const SANDBOX_CLEAR: &str = "DELETE cached container images from the machine-global sandbox image store, and report \
    what that freed. The one tool here that changes anything, and it cannot reach findings, \
    memory or the graph: everything it drops is re-obtainable from a pinned digest, so it \
    costs a re-download and never information. MACHINE-GLOBAL: one store per asset root, \
    shared by every repository this server hosts, so clearing for one project slows the next \
    sandboxed run for all. Call `sandbox_status` first and show the user what is cached and \
    what it costs — a re-pull is minutes to tens of minutes and gigabytes. `image` and \
    `everything` are DIFFERENT REQUESTS with no default: pass exactly one, `image` with a \
    reference from `sandbox_status` or `everything: true`. Report what it \
    freed: quote `freed_bytes`, with `store_bytes_before`/`store_bytes_after` either side, \
    rather than saying it worked. `retained` re-checks every surviving image against the disk \
    afterwards; if any `complete` is false say so prominently — that is a damaged store, not \
    a successful clear, and `roteiro security prefetch` is the repair.";

/// `security_list` — stored findings, with the run evidence behind them.
///
/// The long restatement of what `no-analyzer-on-record` means went in #675, and
/// the pointer to it stayed. `rto_exec::tool_security::NO_RESULT_REASON` is
/// emitted in the **result body** for exactly that coverage — *"This is NOT a
/// clean result and must not be reported as one"* — and its own comment gives the
/// argument: *"the description of a tool is read once and the body of its result
/// is read every time."*
pub const SECURITY_LIST: &str = "List the SECURITY FINDINGS stored for this repository: every live findings layer, the run \
    evidence behind it, and a page of its findings. READ `coverage` FIRST: \
    `no-analyzer-on-record` means nothing was \
    analyzed and is NOT a clean repository — it carries NO `report` at all, so say so rather \
    than report zero findings; `analyzed` with `findings` 0 is the other case, an analyzer \
    that ran and found nothing. `limit` is 1-100 (default 20) — no unlimited setting — and is findings PER LAYER; \
    each layer carries its true `findings` count beside the `page` returned. A page keeps the most severe findings first, so what is \
    omitted is the least severe — never conclude a severity is absent from a truncated page. \
    `cross_reference` is a view over those findings, not a replacement: it groups dependency \
    advisories both analyzers reported, `confirmed_by` counts how many, `1` is normal rather \
    than a discrepancy, and the `findings` total is unchanged by it. Read-only: it cannot run \
    an analyzer or ingest a report — ask the user to run `roteiro security run` or `roteiro \
    security ingest`, because a tool call is not a person consenting to execution.";

/// `security_status` — readiness, in two separately scoped halves.
pub const SECURITY_STATUS: &str = "Report SECURITY READINESS in TWO SEPARATELY SCOPED SECTIONS; report them separately, never \
    merged. `machine`: this HOST — its pinned-asset cache and each analyzer's coverage matrix \
    with `host_readiness`. Identical for every project here, and \
    says nothing whatsoever about whether anything has been run; `ready` is readiness to run \
    ON THIS HOST and no more. `host_readiness` has THREE states with different remedies: \
    `ready` (assets provisioned AND the analyzer's program on PATH); `assets-not-provisioned` \
    (ask the user to run `roteiro security prefetch`); `binary-not-found` (ROTEIRO NEVER \
    INSTALLS ANALYZERS — ask the user to install it or to `roteiro security ingest` a report \
    from elsewhere). `assets_provisioned` and `missing_programs` are ALWAYS present, so when \
    the state is not `ready` read both: a host can lack both, and `host_readiness` names only \
    the first remedy. The sandboxed backend supplies analyzers from a digest-pinned image, so \
    `binary-not-found` \
    does not block it, and this tool does not inspect the image store, so it reports no \
    sandbox verdict. `repository` describes ONE PROJECT: which findings layers are live, how \
    many findings each holds, how old the advisory data behind each is. \
    `possibly_stale: true` whenever advisory data is involved and NEVER means current; `false` \
    means only that there is no advisory axis. Read `repository.coverage` first: \
    `no-analyzer-on-record` means nothing has been analyzed and is NOT a clean repository. \
    COUNTS, NEVER FINDINGS; use `security_list` for those, and it needs no `limit`. Read-only: \
    it cannot provision, and \
    `roteiro security prefetch` needs human consent, so ask the user to run it.";

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
    capability. Report the class name so the user can restart the server with it. Read-only.";

/// `check`.
pub const CHECK: &str = "Run the AUTHORED-LAYER DRIFT CHECK — the same gate `roteiro check` exits non-zero on and \
    the pre-commit hook reads — and return its verdict as data: unresolvable ADR \
    `[[path#Symbol]]` links, `@rto:` annotations naming an unknown or superseded ADR, \
    malformed ADRs, duplicate `adr-id`s. READ `gate` FIRST: `pass`, `fail`, or `not-run`. \
    `not-run` is a real outcome — the check refuses rather than answer about a tree that is \
    nobody's — and carries NO `report` at all, so if you are looking for `violations` and \
    there is no `report`, nothing was checked and you must say so rather than report a clean \
    repository; `not_run_reason` says what to fix (usually: run `roteiro sync`). Read-only: it \
    does not rebuild the graph, which is the one thing the CLI gate does that this cannot.";

/// `config_secrets`.
///
/// **#675 left the four `CANNOT` sentences alone**, and this is the record of why.
/// They are not enforced anywhere: the result carries no caveat field, and an
/// empty report is byte-identical whether the repository has no credentials or
/// simply names them innocuously. [`rto_graph::config_secrets`] states the
/// intent — *"Every surface carries this limitation in its own words, so a model
/// calling the tool passes it on rather than reporting a security guarantee that
/// was never offered"* — so cutting them would be cutting the only thing that
/// upholds it. Whether a shorter phrasing would carry as far is a question about
/// what a model does with the text, and nothing in this repository can answer it
/// yet (#675).
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
///
/// *"Takes `key` and nothing else"* went in #675 because it had stopped being
/// true: every graph tool gained the `project` selector, and the rendered
/// signature is `context(key: str, project?: str)`. The `{cap}` clause stayed —
/// see `context_states_the_edge_cap_the_code_enforces`.
pub const CONTEXT: &str = "Fetch a node's CONTEXT BUNDLE: the node, its metadata, and its one-hop provenance-labelled \
    neighbourhood, with a validity `fingerprint` that moves when the node or any neighbour \
    changes. The grounding to answer “what is this and what is it wired to” from. BOUNDED, \
    and it tells you when it bound something: each direction carries at most {cap} edges, and \
    beyond that `truncated` is true, `outgoing.total`/`incoming.total` give the real counts, \
    and `omitted` names each edge kind and how many of it are missing. Read `omitted` before \
    concluding anything from an absence — a large file's missing definitions are counted \
    rather than silently dropped — and use `explain` or `search` to reach what was left out.";

/// `coupling`.
pub const COUPLING: &str = "Rank symbols by DIRECTED call coupling over `calls` edges: `fan_in` (how many distinct \
    symbols call this one), `fan_out` (how many it calls), `instability` = \
    fan_out/(fan_in+fan_out). `order`=fan_in finds what the codebase most depends on, \
    `order`=fan_out the symbols that reach furthest, `total` (the default) overall coupling. \
    Call edges are resolved by simple name, so a short generically-named function can absorb \
    every call to that name — say so if you report a high `fan_in` on one. `limit` is 1-100 \
    (default 20) — no unlimited setting.";

/// `debt`.
///
/// # The sentence #675 removed was false on one of the two surfaces
///
/// *"Optional `kind` restricts to given categories"* named an argument the served
/// registry does not have: `rto_render::mcp`'s `DebtArgs` calls it `kind`, and
/// `roteiro`'s served schema calls it `categories`. One shared description cannot
/// name both, and naming either is wrong half the time — so it names neither, and
/// each surface's own schema carries the argument. The surfaces disagreeing at all
/// is a separate defect from this one and is not fixed here; renaming an argument
/// is a wire change on whichever surface loses.
pub const DEBT: &str = "List intent-debt markers found in the codebase — TODO/FIXME/HACK comments, \
    todo!()/unimplemented!() stubs, and deferred-work notes — grouped by category (todo, \
    fixme, hack, stub, deferred), optionally restricted to some of them. Each marker links to \
    its enclosing symbol or file via a `contains` edge.";

/// `debt_density`.
pub const DEBT_DENSITY: &str = "Rank FILES by intent-debt DENSITY — markers per 1,000 lines — rather than by raw marker \
    count, which ranks the biggest file first by construction. `overall_per_kloc` is the \
    repository baseline to read a file's `per_kloc` against. Use `debt` instead when the \
    question is which markers exist, not where they are concentrated. Two limits to pass on \
    rather than reporting a number as a finding: the denominator is FILE LENGTH — every line, \
    blanks and comments included — not source lines of code, so figures run lower than an SLOC \
    tool's and flatter verbose or generated files; and the markers beneath it include prose \
    matches (`for now`, `deferred`, `tbd`), so a design document can rank as dense debt. This \
    is a measurement, not a gate. `limit` is 1-100 (default 20) — no unlimited setting.";

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
    hop records the edge kind, provenance and direction. A path lives within one project: a \
    project-qualified `from` (<project>::<key>) selects that project (see list_projects).";

/// `search`.
///
/// *"Read the `snippet`, and call `explain` on a returned key for the full
/// content"* went in #675: the system turn the served surface wraps this listing
/// in already says it, in more words and to every tool at once — *"read each hit's
/// `snippet` or call `explain` on its key to read the node's actual content BEFORE
/// describing it"* (`rto_serve::advertised_system_prompt`). Two statements of one
/// instruction is what the prompt was paying for twice.
pub const SEARCH: &str = "Search graph nodes by text — names, keys, paths, and captured content (doc comments, \
    README/ADR/blueprint prose). Returns the top matches with keys and, for content-bearing \
    nodes, a short `snippet` of the node's actual content to ground your answer; curated \
    ADRs/blueprints and READMEs rank first, so this is the entry point for \"what is X / why\" \
    questions. `limit` is 1-25 (default 10) — no unlimited setting.";

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

    /// Every name this module owns.
    const OWNED: [&str; 15] = [
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
    ];

    /// The ceiling on the sum of every description this module owns.
    ///
    /// #675 cut the total from 12,354 bytes to 10,645, and this is set at 10,700 —
    /// **55 bytes of slack, which is not room for a sentence.** A budget with
    /// comfortable headroom would be worse than none: the failure to guard against
    /// is not one careless paragraph but the slow return of the 12 KB, where every
    /// individual addition looked justified on its own. That is how the surface got
    /// there the first time. Set this tight, a change that genuinely needs the room
    /// raises the number in the same commit, and the diff shows the cost being
    /// accepted rather than absorbed.
    ///
    /// Proven non-vacuous by restoring one cut sentence — `sandbox_clear`'s three
    /// store-integrity refusals, 166 bytes — which takes the total past this
    /// ceiling and fails naming `sandbox_clear` as the tool that grew.
    const DESCRIPTION_BYTE_BUDGET: usize = 10_700;

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
        for name in OWNED {
            let text = for_tool(name).expect("this module owns every name above");
            assert!(
                !text.contains('{'),
                "`{name}` still carries a placeholder: {text}"
            );
        }
        assert!(for_tool("list_kind").is_none(), "MCP-only, not owned here");
        assert!(for_tool("nope").is_none());
    }

    /// **No advertised description may contain a run of spaces.**
    ///
    /// Every constant here is one long chain of Rust's string-continuation
    /// escape, and that escape is unforgiving in both directions. A trailing
    /// `\` eats the newline *and* the continued line's indentation, so the
    /// single separating space has to be written **before** the backslash:
    /// forget it and two words weld together, put one on each side and a double
    /// space reaches the model.
    ///
    /// Worth a guard rather than an eyeball, for the reason #675 exists: the
    /// defect is invisible in review — the source reads the same either way and
    /// nothing fails to compile — and its only symptom is bytes, in a surface
    /// this module now budgets to tens of bytes of slack.
    ///
    /// It also settles the question mechanically. Copilot read
    /// `SANDBOX_CLEAR`'s *"Report what it \ freed:"* break as producing several
    /// spaces on the #675 PR. It does not, because the escape swallows the
    /// indentation — but "I read the reference and disagree" is a weaker answer
    /// than a test that runs the real `for_tool` and would fail if it ever did.
    #[test]
    fn no_advertised_description_carries_a_run_of_spaces() {
        for name in OWNED {
            let text = for_tool(name).expect("owned");
            let Some(at) = text.find("  ") else { continue };
            panic!(
                "`{name}` has a run of spaces at byte {at}. It reaches the model and \
                 costs bytes against `DESCRIPTION_BYTE_BUDGET`. A continued line \
                 carries its separating space BEFORE the backslash, never after it \
                 as well: …{}…",
                window_around(&text, at, 60),
            );
        }
    }

    /// `radius` bytes either side of `at`, snapped out to character boundaries.
    ///
    /// # Why this is not `&text[at - radius..at + radius]`
    ///
    /// **That form panics, and it panics only when the guard above finally
    /// fires** — the one moment the guard exists for. These descriptions are far
    /// from ASCII: 74 em dashes at three bytes each, plus ellipses, `↔` and curly
    /// quotes. A window whose end lands inside one of those aborts with `byte
    /// index N is not a char boundary` *before* the assertion message is built, so
    /// the diagnostic that justifies the whole test is the part that breaks.
    /// Caught by Copilot on the #675 PR, and reproduced by injecting a double
    /// space beside an em dash: the guard fired, and reported the em dash instead
    /// of the defect.
    ///
    /// Snapping outward rather than clamping, because a window that silently
    /// shrank could hide the very characters that caused the trouble. `0` and
    /// `len()` are always boundaries, so both loops terminate, and no input can
    /// panic here — not merely no input this repository holds today.
    fn window_around(text: &str, at: usize, radius: usize) -> &str {
        let mut from = at.saturating_sub(radius);
        while !text.is_char_boundary(from) {
            from -= 1;
        }
        let mut to = at.saturating_add(radius).min(text.len());
        while !text.is_char_boundary(to) {
            to += 1;
        }
        &text[from..to]
    }

    /// [`window_around`] survives the multi-byte characters this prose is full of.
    ///
    /// The fix for a panic needs a test that would fail without it, and the guard
    /// above cannot be that test: it only builds a window when it is already
    /// failing, so on a healthy tree it never exercises this at all. Every case
    /// here puts a boundary demand inside an em dash, which is what byte
    /// arithmetic gets wrong.
    #[test]
    fn a_diagnostic_window_never_splits_a_character() {
        // `—` is three bytes, so every offset inside it is a trap.
        let text = "alpha — bravo — charlie — delta";
        let dash = text.find('—').expect("an em dash");
        for radius in 0..text.len() + 4 {
            for at in [0, dash, dash + 3, text.len()] {
                let got = window_around(text, at, radius);
                assert!(
                    text.contains(got),
                    "window must be a real substring: {got:?}"
                );
            }
        }
        // And it really does widen past a character rather than truncating it.
        assert!(
            window_around(text, dash + 3, 1).contains('—'),
            "a window abutting a multi-byte character must include it whole",
        );
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

    /// **The advertised prose has a budget, and this is it.**
    ///
    /// Nothing measured this before #675, which is the whole reason there was
    /// 12,354 bytes of it: every sentence was added deliberately, none was ever
    /// weighed against the ones already there, and the cost lands on **every
    /// tooled turn** — in the served system listing and, since #681, a second time
    /// through a local model's own chat-template tool slot.
    ///
    /// Measured on `for_tool` rather than on the constants, because that is what a
    /// caller receives: the `{cap}` substitution makes the rendered text longer
    /// than the literal, and a budget the substitution can silently exceed is not
    /// a budget. The per-tool figure is in the failure message so a breach names
    /// the tool that grew rather than only the total.
    #[test]
    fn the_advertised_description_prose_stays_within_its_budget() {
        let mut rows: Vec<(usize, &str)> = OWNED
            .iter()
            .map(|name| {
                let text = for_tool(name).expect("owned");
                (text.len(), *name)
            })
            .collect();
        rows.sort_unstable_by(|a, b| b.cmp(a));
        let total: usize = rows.iter().map(|(bytes, _)| bytes).sum();
        assert!(
            total <= DESCRIPTION_BYTE_BUDGET,
            "advertised description prose is {total} bytes, over the \
             {DESCRIPTION_BYTE_BUDGET}-byte budget by {}. Every byte here is \
             prefilled on every tooled turn, on both the served listing and a \
             local model's chat template. Either cut the sentence something else \
             already upholds — the schema, a refusal in code, or the result body \
             — or raise the budget in this commit and say what it bought. \
             Largest first: {rows:?}",
            total - DESCRIPTION_BYTE_BUDGET,
        );
    }
}

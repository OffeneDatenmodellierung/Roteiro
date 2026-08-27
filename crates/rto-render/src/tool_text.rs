//! The **one** place each shared tool description is written.
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

//! The Obsidian-vault renderer: each graph node becomes a markdown note whose
//! edges are `[[wikilinks]]`, so the provenance-tagged graph is browsable in
//! Obsidian's graph view. Notes carry frontmatter `tags` (`roteiro/kind/*`,
//! `roteiro/lang/*`, `roteiro/status/*`) so the graph is colourable/filterable —
//! edge provenance is shown per-link in the body — surface the node's text as the
//! knowledge base, show an ADR's status, and (when the repository's web host is
//! known) a clickable **Source** link to the file.
//!
//! That text is the node's captured `meta.content` (a doc comment, PDF or image
//! text) *except* where the caller supplies a full `body` — which it does for
//! prose documents, because `meta.content` is an embedding budget and a note
//! rendered from it is the document capped at 1500 characters and collapsed onto
//! one line. See [`note_body`].
//!
//! A generated `_Home` note is the overview: what was
//! scanned, counts by kind, provenance breakdown, ADR statuses, intent-debt (with
//! the files it is densest in), an inventory of secret-**named** config keys and
//! their redaction state, and the most depended-on symbols by directed call
//! fan-in.
//! Built from the same [`Explanation`] the query surface returns, so the vault
//! and the CLI agree.

use std::fmt::Write as _;

use rto_graph::Explanation;

/// Filename of the generated overview note (sorts first in the file list).
pub const HOME_NOTE: &str = "_Home.md";

/// A rendered vault note: its filename (with `.md`) and markdown content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultNote {
    /// Filename including the `.md` extension.
    pub filename: String,
    /// Markdown content.
    pub content: String,
}

/// Map a node key to a filesystem- and wikilink-safe note stem. Characters that
/// are awkward in filenames or Obsidian links (`:` `/` `#` whitespace) collapse
/// to `-`; alphanumerics, `.`, `_` and `-` are kept. The result is **bounded**
/// in length (a grouped Rust `use` can key a 300+ char import node) by truncating
/// and appending a short hash of the full key, so notes stay under filesystem
/// limits while remaining unique and deterministic.
#[must_use]
pub fn note_name(key: &str) -> String {
    // Keep the stem well under the 255-byte filename limit (leaving room for
    // ".md"). The slug is ASCII, so byte length equals char count and slicing is
    // safe. A hash of the full key preserves uniqueness after truncation.
    const MAX: usize = 200;
    let mut out = String::with_capacity(key.len());
    let mut prev_dash = false;
    for c in key.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            out.push(c);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let out = out.trim_matches('-');
    if out.len() <= MAX {
        out.to_owned()
    } else {
        format!("{}-{:016x}", &out[..MAX - 17], fnv1a64(key.as_bytes()))
    }
}

/// FNV-1a (64-bit) — a dependency-free, deterministic hash to disambiguate a
/// truncated note stem. No cryptographic properties needed.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Which vault a note is being rendered into: a single project's, or one member
/// of a **workspace** vault spanning several repositories.
///
/// This is the whole of the workspace-vault naming rule, in one place, because
/// the rule has a hard compatibility half. Node keys are **repository-relative**
/// (`file:README.md` names no repo), so every member of a workspace produces the
/// same note name for its `README.md` and one would silently overwrite the rest.
/// Qualifying the key with its project fixes that — but a single-project vault's
/// note names **must not move**: Obsidian resolves `[[links]]` by name, and a
/// user's own notes live outside the vault and link *into* it (issue #442), so a
/// rename breaks every such link silently, with no error and nothing to grep for.
///
/// Hence [`VaultScope::PROJECT`] (`project: None`) is not a degenerate case but
/// the contract: it makes every name in this module reduce to exactly
/// [`note_name`] of the bare key, byte for byte.
#[derive(Debug, Clone, Copy)]
pub struct VaultScope<'a> {
    /// The member project this note belongs to, qualifying its name as
    /// `<project>::<key>` — the same form ADR-0009's cross-repo links already use.
    /// `None` ⇒ a single-project vault, and names are unqualified exactly as
    /// before.
    pub project: Option<&'a str>,
    /// The workspace's member project names. An external-ref placeholder whose
    /// target names one of these is a cross-repo edge the vault can actually
    /// follow, so it is rendered as a link straight to that member's note. Empty
    /// for a single-project vault.
    pub members: &'a std::collections::BTreeSet<String>,
}

/// The empty member set backing [`VaultScope::PROJECT`] — a single-project vault
/// has no other members to resolve a cross-repo reference against.
static NO_MEMBERS: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

impl VaultScope<'_> {
    /// A single-project vault: names are unqualified, and no cross-repo reference
    /// resolves. Every name this produces is byte-identical to [`note_name`] of
    /// the bare key — see the type's documentation for why that is load-bearing.
    pub const PROJECT: Self = Self {
        project: None,
        members: &NO_MEMBERS,
    };
}

impl Default for VaultScope<'_> {
    fn default() -> Self {
        Self::PROJECT
    }
}

impl VaultScope<'_> {
    /// Whether an external-ref placeholder `key` is one this vault resolves for
    /// itself — its target names a member, so every edge to it points at the real
    /// note and the placeholder need not be rendered at all.
    ///
    /// The single rule behind both halves of that: [`link_target`] redirects
    /// exactly the keys this accepts, and the caller skips writing exactly the
    /// notes this accepts. They cannot disagree.
    #[must_use]
    pub fn redirects_external_ref(&self, key: &str) -> bool {
        key.strip_prefix("extref:")
            .and_then(rto_graph::parse_qualified)
            .is_some_and(|(project, _)| self.members.contains(project))
    }
}

/// The note name for a node `key` owned by `scope`'s project.
///
/// In a single-project vault (`scope.project == None`) this *is* [`note_name`].
/// In a workspace vault it is [`note_name`] of the project-qualified key
/// `<project>::<key>` — reusing ADR-0009's qualified form rather than inventing a
/// second one, which is what lets a cross-repo external-ref target (already
/// stored qualified) map to its note by the very same call.
#[must_use]
pub fn scoped_note_name(scope: &VaultScope<'_>, key: &str) -> String {
    match scope.project {
        None => note_name(key),
        Some(project) => note_name(&format!("{project}::{key}")),
    }
}

/// The note an edge pointing at `key` should link to.
///
/// Almost always [`scoped_note_name`]. The exception is the one cross-repo edge
/// the graph already models: a spoke's inferred link to a hub is stored as an
/// edge to a **local external-ref placeholder** (`extref:<project>::<key>`,
/// [`rto_graph::external_ref_key`]) because store integrity requires both ends of
/// an edge in one store. A workspace vault holds both repos' notes, so when the
/// placeholder's target names a member the link is pointed at the **real** note
/// instead of the stand-in.
///
/// This invents no edge. It renders the edge that is there, following the
/// placeholder exactly as [`rto_graph::Workspace::follow_external_ref`] does at
/// query time — the cross-repo graph has only ever been *rendered* one repo at a
/// time.
fn link_target(scope: &VaultScope<'_>, key: &str) -> String {
    if scope.redirects_external_ref(key) {
        // `note_name(qualified)` is by construction the same string
        // `scoped_note_name` produces for that member's own copy of the node.
        // `strip_prefix`, not `trim_start_matches`: the latter strips the prefix
        // repeatedly, which would mangle a target that legitimately starts with it.
        return note_name(key.strip_prefix("extref:").unwrap_or(key));
    }
    scoped_note_name(scope, key)
}

/// Render a node's [`Explanation`] into an Obsidian note: YAML frontmatter (with
/// `tags` for the graph view and an ADR's `status`), a clickable **Source** link
/// (when `source_base` — a web "blob" base like
/// `https://github.com/org/repo/blob/<sha>` — is known and the node has a path),
/// the content as the knowledge base, and its edges as provenance-labelled
/// wikilinks.
///
/// `body` is the node's **full source text**, which only the caller can fetch:
/// this function is a pure function of the `Explanation`, and an `Explanation`
/// carries no repository, store or blob. When it is `Some`, it replaces
/// `meta.content` in the note's `## Content` section — see [`note_body`] for why
/// replacing is the only correct combination of the two.
#[must_use]
pub fn render_note(ex: &Explanation, source_base: Option<&str>, body: Option<&str>) -> VaultNote {
    render_note_scoped(ex, source_base, body, &VaultScope::PROJECT)
}

/// [`render_note`], for one member of a **workspace** vault: identical except
/// that the note's own name and every link it emits are resolved through `scope`
/// (see [`VaultScope`]).
///
/// With [`VaultScope::PROJECT`] this is [`render_note`] byte for byte, which is
/// how the single-project vault's compatibility promise is kept by construction
/// rather than by a parallel code path that has to be kept in step.
#[must_use]
pub fn render_note_scoped(
    ex: &Explanation,
    source_base: Option<&str>,
    body: Option<&str>,
    scope: &VaultScope<'_>,
) -> VaultNote {
    let meta = &ex.meta;
    let status = meta.get("status").and_then(|v| v.as_str());
    let content = note_body(meta.get("content").and_then(|v| v.as_str()), body);

    let mut c = String::new();
    c.push_str("---\n");
    let _ = writeln!(c, "key: \"{}\"", ex.node.key.replace('"', "'"));
    let _ = writeln!(c, "kind: {}", ex.node.kind);
    // Which member this note came from. Absent in a single-project vault, where
    // it would be one constant repeated on every note — and where adding it would
    // change every note's bytes.
    if let Some(project) = scope.project {
        let _ = writeln!(c, "project: \"{}\"", project.replace('"', "'"));
    }
    if let Some(path) = &ex.node.path {
        let _ = writeln!(c, "path: \"{path}\"");
    }
    if let Some(lang) = &ex.node.lang {
        let _ = writeln!(c, "lang: {lang}");
    }
    if let Some(status) = status {
        let _ = writeln!(c, "status: {status}");
    }
    // Nested tags group in Obsidian's tag pane and colour the graph view.
    c.push_str("tags:\n");
    let _ = writeln!(c, "  - roteiro/kind/{}", tag_slug(&ex.node.kind));
    // Colours the graph view by member, which is the one thing a workspace vault
    // is for and a per-project vault has no use for.
    if let Some(project) = scope.project {
        let _ = writeln!(c, "  - roteiro/project/{}", tag_slug(project));
    }
    if let Some(lang) = &ex.node.lang {
        let _ = writeln!(c, "  - roteiro/lang/{}", tag_slug(lang));
    }
    if let Some(status) = status {
        let _ = writeln!(c, "  - roteiro/status/{}", tag_slug(status));
    }
    c.push_str("---\n\n");

    let _ = writeln!(c, "# {}", ex.node.name);
    if let Some(status) = status {
        let _ = writeln!(c, "\n> **Status:** {status}");
    }

    // A clickable link to the file this node comes from. An absolute URL, so it
    // works from the downloaded vault too (which has no repo files beside it).
    if let (Some(base), Some(path)) = (source_base, ex.node.path.as_deref()) {
        let _ = writeln!(
            c,
            "\n**Source:** [`{path}`]({}/{path})",
            base.trim_end_matches('/')
        );
    }

    // The knowledge base: the full source text, or the captured doc comment /
    // prose / PDF / image text.
    if let Some(content) = content.map(str::trim).filter(|s| !s.is_empty()) {
        c.push_str("\n## Content\n\n");
        c.push_str(content);
        c.push('\n');
    }

    if !ex.outgoing.is_empty() {
        c.push_str("\n## Outgoing\n\n");
        for e in &ex.outgoing {
            let _ = writeln!(
                c,
                "- {} ({}){} → [[{}]]",
                e.kind,
                e.provenance,
                confidence(e.confidence),
                link_target(scope, &e.node)
            );
        }
    }
    if !ex.incoming.is_empty() {
        c.push_str("\n## Incoming\n\n");
        for e in &ex.incoming {
            let _ = writeln!(
                c,
                "- [[{}]] {} ({}){} →",
                link_target(scope, &e.node),
                e.kind,
                e.provenance,
                confidence(e.confidence)
            );
        }
    }

    VaultNote {
        filename: format!("{}.md", scoped_note_name(scope, &ex.node.key)),
        content: c,
    }
}

/// Choose the text a note shows: the caller's full `body` when it has one, else
/// the node's stored `content`.
///
/// The two are **not** complementary, they are the same text at two fidelities,
/// so a note shows one of them and never both. `meta.content` is an embedding
/// budget — extraction caps it (1500 chars) and collapses every whitespace run to
/// a single space, which is right for a store that ships with the graph and wrong
/// for a note: a 23 KB document arrives as one 1500-character line with every
/// heading, table and code fence flattened into it. Where the caller can supply
/// the source, that is what a reader wants; appending the capped rendering
/// underneath it would only restate its first 6% badly.
fn note_body<'a>(content: Option<&'a str>, body: Option<&'a str>) -> Option<&'a str> {
    body.or(content)
}

/// `" (0.82)"` for an inferred edge's confidence, else empty.
fn confidence(c: Option<f64>) -> String {
    c.map_or_else(String::new, |c| format!(" ({c:.2})"))
}

/// A tag-safe slug: lowercase, non-alphanumeric runs → `-`. Keeps Obsidian tags
/// (`roteiro/kind/adr-section`) valid and stable.
fn tag_slug(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = false;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_owned()
}

/// One ADR in the overview, with its lifecycle status.
#[derive(Debug, Clone)]
pub struct AdrEntry {
    /// The ADR node key (`adr:<id>`).
    pub key: String,
    /// The ADR title.
    pub name: String,
    /// Lifecycle status (`Accepted`, …), if recorded.
    pub status: Option<String>,
}

/// The `_Home` overview's config-secret inventory figures.
///
/// Counts and file paths only — deliberately not the key names, which belong in
/// `roteiro config-secrets` where the caveat can be stated at length. A vault note
/// is read casually and out of context, which is exactly the wrong place for a
/// list that looks like a secret scan's output.
#[derive(Debug, Clone, Default)]
pub struct ConfigSecretSummary {
    /// Config keys whose **name** matched the secret-name heuristic.
    pub secret_named: usize,
    /// Of those, how many had their value redacted before persistence.
    pub redacted: usize,
    /// Of those, how many are declared in code with no literal value.
    pub declared: usize,
    /// Of those, how many carry an unredacted value. Expected to be zero.
    pub unredacted: usize,
    /// Distinct files carrying at least one secret-named key, ordered and capped
    /// by the caller.
    pub files: Vec<String>,
}

/// One file in the `_Home` overview's intent-debt density table.
#[derive(Debug, Clone)]
pub struct DensityEntry {
    /// Repository-relative path, used for both the wikilink and the label.
    pub path: String,
    /// Retained markers in the file.
    pub markers: u32,
    /// The file's length in lines — the denominator.
    pub lines: u32,
    /// Markers per 1,000 lines.
    pub per_kloc: f64,
}

/// One node in the `_Home` overview's directed-coupling table.
#[derive(Debug, Clone)]
pub struct CouplingEntry {
    /// The node key, for the wikilink.
    pub key: String,
    /// The symbol name.
    pub name: String,
    /// Distinct callers.
    pub fan_in: u32,
    /// Distinct callees.
    pub fan_out: u32,
}

/// Aggregate figures for the vault's `_Home` overview note.
#[derive(Debug, Clone, Default)]
pub struct VaultSummary {
    /// Name of the scanned project (repository directory).
    pub project: String,
    /// Total node and edge counts.
    pub total_nodes: usize,
    /// Total edge count.
    pub total_edges: usize,
    /// `(kind, count)` for each node kind, most-frequent first.
    pub node_counts: Vec<(String, usize)>,
    /// `(provenance, edge count)` — `derived` / `authored` / `inferred`.
    pub edge_provenance: Vec<(String, usize)>,
    /// The ADRs, with status.
    pub adrs: Vec<AdrEntry>,
    /// `(category, count)` of intent-debt markers.
    pub debt: Vec<(String, usize)>,
    /// The files where that debt is most **concentrated**, already ranked and
    /// capped by the caller. Empty when the graph has no markers, or when no
    /// file carrying one has a recorded length.
    pub densest_files: Vec<DensityEntry>,
    /// Secret-named config keys and their redaction state. `None` when the graph
    /// holds no secret-named config key — the section is then absent rather than
    /// rendering a row of zeroes, which would read as a clean bill of health this
    /// lens cannot give.
    pub config_secrets: Option<ConfigSecretSummary>,
    /// The most depended-on symbols by **directed** call fan-in, already ranked
    /// and capped by the caller. Empty when the graph has no `calls` edges.
    pub most_called: Vec<CouplingEntry>,
    /// Web root of the repository (`https://host/owner/repo`), if derivable from
    /// the git remote — for a "Repository" link in the overview.
    pub repo_url: Option<String>,
    /// Hex commit the graph was rendered from, for a permalink note.
    pub commit: Option<String>,
}

/// Render the vault's overview note: what was scanned, the structure by kind,
/// the provenance breakdown, the decisions (ADRs) and their status, the
/// intent-debt summary, and how to navigate. The entry point for the vault.
#[must_use]
pub fn render_home(s: &VaultSummary) -> VaultNote {
    let mut c = String::new();
    c.push_str("---\ntags:\n  - roteiro/home\n---\n\n");
    let _ = writeln!(c, "# {} — knowledge graph", s.project);
    c.push_str(
        "\n*A browsable snapshot of this codebase as one **knowledge graph**, \
         generated by [Roteiro](https://roteiro.dev). Every symbol, document and \
         decision is a note, linked to the things it relates to.*\n",
    );
    c.push_str(HOW_TO_READ);
    let _ = writeln!(
        c,
        "\n**{} nodes**, **{} edges** across the project.",
        s.total_nodes, s.total_edges
    );
    write_repo_line(&mut c, s);
    write_summary_sections(&mut c, s, &VaultScope::PROJECT, 2);
    c.push_str(NAVIGATING);

    VaultNote {
        filename: HOME_NOTE.to_owned(),
        content: c,
    }
}

/// The "how to read a note" paragraph. Shared verbatim by the single-project and
/// workspace overviews — the notes themselves are identical in both, so a reader
/// who learns the format once has learned it for either.
const HOW_TO_READ: &str = "\n**How to read it.** Open any note to see what a thing is, the intent or \
     docs behind it (its **Content**), where it lives (its **Source** link), \
     and how it connects (**Outgoing**/**Incoming** links). Each link is \
     labelled with how the fact was established — `derived` (extracted from \
     code), `authored` (human intent: ADRs, blueprints, annotations), or \
     `inferred` (a scored suggestion). Open Obsidian's **graph view** to see \
     the whole thing at once.\n";

/// The closing navigation section.
const NAVIGATING: &str = "\n## Navigating this vault\n\n\
     - Open the **graph view** to see the whole codebase; notes are coloured/\
     filterable by their `roteiro/kind/*`, `roteiro/lang/*` and \
     `roteiro/status/*` tags.\n\
     - Each note carries its captured **content** (doc comments, prose, PDF/\
     image text) and its provenance-labelled incoming/outgoing links.\n\
     - Start from an ADR above, or search the tag pane for a kind.\n";

/// `**Repository:** …` — the web root and the commit the graph was rendered from.
fn write_repo_line(c: &mut String, s: &VaultSummary) {
    if let Some(repo) = &s.repo_url {
        let _ = write!(c, "\n**Repository:** [{repo}]({repo})");
        if let Some(commit) = &s.commit {
            let short = &commit[..commit.len().min(12)];
            let _ = write!(c, " · rendered at commit `{short}`");
        }
        c.push('\n');
    }
}

/// Every aggregate the overview carries for **one project**: structure by kind,
/// provenance, ADRs, intent debt (and where it is densest), the config-secret
/// inventory and directed call coupling.
///
/// Factored out of [`render_home`] so a workspace vault's per-member section is
/// *the same code*, not a reimplementation that can drift: the promise in issue
/// #442 is that today's per-project view stays a **subset** of the workspace one
/// rather than a casualty of it. `level` is the markdown heading depth — 2 for a
/// single-project `_Home`, 3 inside a member's section — and `scope` decides
/// whether the wikilinks point at bare or project-qualified notes.
fn write_summary_sections(c: &mut String, s: &VaultSummary, scope: &VaultScope<'_>, level: usize) {
    let hd = &"#".repeat(level);
    let sub = &"#".repeat(level + 1);
    write_structure(c, s, hd);
    write_decisions(c, s, scope, hd);
    write_debt(c, s, scope, hd, sub);
    write_config_secrets(c, s, scope, hd);
    write_coupling(c, s, scope, hd);
}

/// `Structure` (nodes by kind) and `Provenance` (edges by how they were established).
fn write_structure(c: &mut String, s: &VaultSummary, hd: &str) {
    let _ = write!(c, "\n{hd} Structure\n\n| Kind | Count |\n| --- | --- |\n");
    for (kind, n) in &s.node_counts {
        let _ = writeln!(c, "| {kind} | {n} |");
    }

    if !s.edge_provenance.is_empty() {
        let _ = write!(
            c,
            "\n{hd} Provenance\n\n| Provenance | Edges |\n| --- | --- |\n"
        );
        for (prov, n) in &s.edge_provenance {
            let _ = writeln!(c, "| {prov} | {n} |");
        }
    }
}

/// `Decisions (ADRs)` — the recorded decisions and their lifecycle status.
fn write_decisions(c: &mut String, s: &VaultSummary, scope: &VaultScope<'_>, hd: &str) {
    let _ = write!(c, "\n{hd} Decisions (ADRs)\n\n");
    if s.adrs.is_empty() {
        c.push_str("*No ADRs found.*\n");
    } else {
        for adr in &s.adrs {
            let status = adr.status.as_deref().unwrap_or("—");
            let _ = writeln!(
                c,
                "- **{status}** — [[{}|{}]]",
                scoped_note_name(scope, &adr.key),
                adr.name
            );
        }
    }
}

/// `Intent debt` — the marker categories, and the files the debt is densest in.
fn write_debt(c: &mut String, s: &VaultSummary, scope: &VaultScope<'_>, hd: &str, sub: &str) {
    let _ = write!(c, "\n{hd} Intent debt\n\n");
    if s.debt.is_empty() {
        c.push_str("*None recorded.*\n");
    } else {
        c.push_str("| Category | Count |\n| --- | --- |\n");
        for (cat, n) in &s.debt {
            let _ = writeln!(c, "| {cat} | {n} |");
        }
    }

    if !s.densest_files.is_empty() {
        let _ = write!(
            c,
            "\n{sub} Densest files (markers per 1,000 lines)\n\n\
             *Where the debt above is concentrated, rather than where there is \
             most of it — a raw count ranks the biggest file first by \
             construction. The denominator is file length: every line, blanks and \
             comments included, not source lines of code. Prose matches (`for \
             now`, `tbd`) count too, so a design document can rank high.*\n\n"
        );
        c.push_str("| File | Markers | Lines | Per 1k |\n| --- | --- | --- | --- |\n");
        for e in &s.densest_files {
            let _ = writeln!(
                c,
                "| [[{}\\|{}]] | {} | {} | {:.2} |",
                scoped_note_name(scope, &format!("file:{}", e.path)),
                e.path,
                e.markers,
                e.lines,
                e.per_kloc
            );
        }
    }
}

/// `Config keys named like secrets` — an inventory and its unconditional caveat.
fn write_config_secrets(c: &mut String, s: &VaultSummary, scope: &VaultScope<'_>, hd: &str) {
    if let Some(cs) = &s.config_secrets {
        let _ = write!(c, "\n{hd} Config keys named like secrets\n\n");
        let _ = writeln!(
            c,
            "**{}** secret-named config key(s): {} redacted before storage, {} \
             declared in code without a value, {} unredacted.",
            cs.secret_named, cs.redacted, cs.declared, cs.unredacted
        );
        if cs.unredacted > 0 {
            let _ = writeln!(
                c,
                "\n> [!warning] {} key(s) carry an **unredacted** value. Extraction \
                 always redacts, so these came from an import layer — inspect the \
                 importing tool, not this repository.",
                cs.unredacted
            );
        }
        if !cs.files.is_empty() {
            c.push_str("\nIn:\n");
            for path in &cs.files {
                let _ = writeln!(
                    c,
                    "- [[{}\\|{path}]]",
                    scoped_note_name(scope, &format!("file:{path}"))
                );
            }
        }
        // The caveat is unconditional and comes last, so it is the final thing read
        // in this section. A vault note is browsed out of context; this is exactly
        // where "config keys named like secrets" would otherwise be misread as a
        // secret scan that came back clean.
        c.push_str(
            "\n*An inventory of config keys whose **names** look secret, not a secret \
             scan. Values are redacted before they are stored, so this reports that \
             such keys exist and were redacted — never a value. It cannot see a \
             hardcoded credential in source code, cannot judge whether a value is \
             valid, and cannot tell a real secret from a placeholder. A credential \
             under an innocuous key name (`dsn`, `endpoint`) does not appear here at \
             all, so this section being small says nothing about whether this \
             repository leaks secrets.*\n",
        );
    }
}

/// `Most depended-on (call fan-in)` — directed call coupling, capped.
fn write_coupling(c: &mut String, s: &VaultSummary, scope: &VaultScope<'_>, hd: &str) {
    if !s.most_called.is_empty() {
        let _ = write!(
            c,
            "\n{hd} Most depended-on (call fan-in)\n\n\
             *Distinct callers and callees over `calls` edges — direction kept, so \
             \"everything calls this\" and \"this calls everything\" are not the same \
             row. Call targets are resolved by simple name, so a short, generically-\
             named function can absorb every call to that name: read a large fan-in on \
             one as a question, not a finding.*\n\n"
        );
        c.push_str("| Symbol | Called by | Calls |\n| --- | --- | --- |\n");
        for e in &s.most_called {
            let _ = writeln!(
                c,
                "| [[{}\\|{}]] | {} | {} |",
                scoped_note_name(scope, &e.key),
                e.name,
                e.fan_in,
                e.fan_out
            );
        }
    }
}

/// One cross-repo edge the workspace vault can actually follow: a spoke's node
/// linking to a hub's, through the external-ref placeholder ADR-0009 persists.
///
/// Collected by the caller, which has every member's store open; the renderer
/// only lays them out. Nothing here is a new edge — these are the `inferred`
/// links `roteiro links` already reports, rendered for the first time.
#[derive(Debug, Clone)]
pub struct CrossLink {
    /// The member the edge starts in.
    pub from_project: String,
    /// The source node's key, within `from_project`.
    pub from_key: String,
    /// The source node's display name.
    pub from_name: String,
    /// The edge kind (`links`, …).
    pub kind: String,
    /// Confidence, for an `inferred` edge.
    pub confidence: Option<f64>,
    /// The project-qualified target, `<project>::<key>` (ADR-0009).
    pub to_qualified: String,
    /// Whether `to_qualified`'s project is a member of this workspace — and so
    /// whether the link resolves to a note in this vault, or dangles because the
    /// target repository is outside it.
    pub resolves: bool,
}

/// Aggregate figures for a **workspace** vault's `_Home` overview: the members,
/// each with exactly the aggregates a single-project `_Home` carries, plus the
/// cross-repo links between them.
#[derive(Debug, Clone, Default)]
pub struct WorkspaceSummary {
    /// The workspace name (`--workspace-name`).
    pub name: String,
    /// One entry per member repository, in stable name order. Each is the very
    /// same [`VaultSummary`] a per-project vault would render.
    pub members: Vec<VaultSummary>,
    /// Cross-repo links between members, already ordered and capped by the caller.
    pub cross_links: Vec<CrossLink>,
    /// Cross-repo links found in total, which `cross_links` may be a capped view
    /// of — so the section can say what it is not showing.
    pub cross_links_total: usize,
}

/// Render a **workspace** vault's overview: the members and their scale, the
/// cross-repo links between them, and then each member's own aggregates —
/// structure, provenance, ADRs, intent debt, config-secret inventory and call
/// coupling — under its own heading.
///
/// The per-member sections are rendered by the same [`write_summary_sections`]
/// the single-project `_Home` uses, so the existing view is a **subset** of this
/// one: someone who came for their repository's coupling and debt tables finds
/// them, rather than a workspace total that averages them away.
#[must_use]
pub fn render_workspace_home(ws: &WorkspaceSummary) -> VaultNote {
    let members: std::collections::BTreeSet<String> =
        ws.members.iter().map(|m| m.project.clone()).collect();

    let mut c = String::new();
    c.push_str("---\ntags:\n  - roteiro/home\n  - roteiro/workspace\n---\n\n");
    let _ = writeln!(c, "# {} — workspace knowledge graph", ws.name);
    c.push_str(
        "\n*A browsable snapshot of a whole **workspace** as one **knowledge \
         graph**, generated by [Roteiro](https://roteiro.dev). Every symbol, \
         document and decision in every member repository is a note, linked to the \
         things it relates to — including across repositories.*\n",
    );
    c.push_str(HOW_TO_READ);
    c.push_str(
        "\n**Notes are named `<project>-<key>`**, because a node key is \
         repository-relative: every member has a `README.md`, and without the \
         project each would overwrite the last. Filter the graph view by a \
         member's `roteiro/project/*` tag to see one repository at a time.\n",
    );

    let total_nodes: usize = ws.members.iter().map(|m| m.total_nodes).sum();
    let total_edges: usize = ws.members.iter().map(|m| m.total_edges).sum();
    let _ = writeln!(
        c,
        "\n**{total_nodes} nodes**, **{total_edges} edges** across **{}** member \
         repositor{}.",
        ws.members.len(),
        if ws.members.len() == 1 { "y" } else { "ies" }
    );

    c.push_str("\n## Members\n\n| Project | Nodes | Edges | Repository | Commit |\n| --- | --- | --- | --- | --- |\n");
    for m in &ws.members {
        let repo = m
            .repo_url
            .as_ref()
            .map_or_else(|| "—".to_owned(), |u| format!("[{u}]({u})"));
        let commit = m.commit.as_ref().map_or_else(
            || "—".to_owned(),
            |c| format!("`{}`", &c[..c.len().min(12)]),
        );
        let _ = writeln!(
            c,
            "| [[#{}\\|{}]] | {} | {} | {repo} | {commit} |",
            m.project, m.project, m.total_nodes, m.total_edges
        );
    }
    c.push_str(
        "\n*The `Repository` and `Commit` columns say where each member came from \
         and what was read. They are **not** a replication manifest — reconstructing \
         a workspace from a vault is issue #442 part 2, and nothing here is designed \
         to be handed to someone else.*\n",
    );

    write_cross_links(&mut c, ws);

    for m in &ws.members {
        let _ = writeln!(c, "\n## {}", m.project);
        let _ = writeln!(
            c,
            "\n**{} nodes**, **{} edges** in this member.",
            m.total_nodes, m.total_edges
        );
        write_repo_line(&mut c, m);
        let scope = VaultScope {
            project: Some(&m.project),
            members: &members,
        };
        write_summary_sections(&mut c, m, &scope, 3);
    }

    c.push_str(NAVIGATING);

    VaultNote {
        filename: HOME_NOTE.to_owned(),
        content: c,
    }
}

/// The `## Cross-repo links` section: the edges that only a workspace vault can
/// show, and the honest statement of what is missing from them.
fn write_cross_links(c: &mut String, ws: &WorkspaceSummary) {
    c.push_str("\n## Cross-repo links\n\n");
    if ws.cross_links.is_empty() {
        c.push_str(
            "*None. These are the `inferred` cross-repo links `roteiro links \
             --infer --write` persists (ADR-0009); a workspace whose members have \
             never been inferred over has none recorded yet.*\n",
        );
        return;
    }
    c.push_str(
        "*A spoke's config key and the hub key it corresponds to, across \
         repositories — the one thing a per-project vault structurally cannot show. \
         These are `inferred` matches persisted by `roteiro links --infer --write` \
         (ADR-0009), not authored facts: read a row as a candidate correspondence.*\n\n",
    );
    c.push_str("| From | | To | Kind |\n| --- | --- | --- | --- |\n");
    for l in &ws.cross_links {
        let from_scope = VaultScope {
            project: Some(&l.from_project),
            members: &NO_MEMBERS,
        };
        let to = if l.resolves {
            format!("[[{}\\|{}]]", note_name(&l.to_qualified), l.to_qualified)
        } else {
            // Outside this workspace: there is no note to link to, and a wikilink
            // to a note that does not exist reads in Obsidian as one that is
            // merely unwritten.
            format!("`{}` *(outside this workspace)*", l.to_qualified)
        };
        let _ = writeln!(
            c,
            "| [[{}\\|{}]] | {} | {to} | {}{} |",
            scoped_note_name(&from_scope, &l.from_key),
            l.from_name,
            l.from_project,
            l.kind,
            confidence(l.confidence)
        );
    }
    if ws.cross_links_total > ws.cross_links.len() {
        let _ = writeln!(
            c,
            "\n*Showing {} of {} — the full report is `roteiro links --matrix`.*",
            ws.cross_links.len(),
            ws.cross_links_total
        );
    }
    c.push_str(
        "\n*Shown in one direction only. The edge lives in the spoke's store, \
         pointing at a local placeholder for the hub's node, so the hub's own note \
         carries no matching **Incoming** entry — Obsidian's **Backlinks** pane \
         still shows it, because the link is in the vault.*\n",
    );
}

#[cfg(test)]
mod tests {
    use super::{
        AdrEntry, ConfigSecretSummary, CouplingEntry, CrossLink, DensityEntry, HOME_NOTE,
        VaultScope, VaultSummary, WorkspaceSummary, note_name, render_home, render_note,
        render_note_scoped, render_workspace_home, scoped_note_name,
    };
    use rto_graph::{EdgeRef, Explanation, NodeSummary};

    #[test]
    fn note_name_is_safe_and_stable() {
        assert_eq!(
            note_name("sym:rust:src/a.rs#Store"),
            "sym-rust-src-a.rs-Store"
        );
        assert_eq!(note_name("adr:0001"), "adr-0001");
        assert_eq!(note_name("file:src/main.rs"), "file-src-main.rs");
    }

    #[test]
    fn render_note_emits_frontmatter_and_wikilinks() {
        let ex = Explanation {
            schema: rto_graph::SCHEMA,
            node: NodeSummary {
                key: "sym:rust:a.rs#main".into(),
                kind: "fn".into(),
                name: "main".into(),
                path: Some("a.rs".into()),
                lang: Some("rust".into()),
            },
            meta: serde_json::Value::Null,
            outgoing: vec![EdgeRef {
                kind: "calls".into(),
                provenance: "derived",
                confidence: None,
                node: "sym:rust:a.rs#helper".into(),
            }],
            incoming: vec![EdgeRef {
                kind: "references".into(),
                provenance: "authored",
                confidence: None,
                node: "adr:0001".into(),
            }],
        };
        let note = render_note(&ex, None, None);
        assert_eq!(note.filename, "sym-rust-a.rs-main.md");
        assert!(note.content.contains("kind: fn"));
        // No source base → no Source link.
        assert!(!note.content.contains("**Source:**"));
        assert!(note.content.contains("# main"));
        assert!(
            note.content
                .contains("- calls (derived) → [[sym-rust-a.rs-helper]]")
        );
        assert!(
            note.content
                .contains("- [[adr-0001]] references (authored) →")
        );
        // Tags for the graph view.
        assert!(note.content.contains("- roteiro/kind/fn"));
        assert!(note.content.contains("- roteiro/lang/rust"));
    }

    #[test]
    fn note_name_bounds_long_keys_deterministically() {
        let long = format!("import:rust:{}", "a::b::c,".repeat(60));
        let a = note_name(&long);
        let b = note_name(&long);
        assert_eq!(a, b, "deterministic");
        assert!(
            a.len() <= 205,
            "bounded under the filename limit: {}",
            a.len()
        );
        assert_ne!(
            note_name(&format!("{long}x")),
            a,
            "different keys stay distinct after truncation"
        );
    }

    #[test]
    fn render_note_surfaces_content_and_status() {
        let ex = Explanation {
            schema: rto_graph::SCHEMA,
            node: NodeSummary {
                key: "adr:0001".into(),
                kind: "adr".into(),
                name: "Build Roteiro".into(),
                path: Some("docs/adr/0001.md".into()),
                lang: None,
            },
            meta: serde_json::json!({ "status": "Accepted", "content": "The decision text." }),
            outgoing: vec![],
            incoming: vec![],
        };
        let note = render_note(&ex, Some("https://github.com/org/repo/blob/abc123"), None);
        assert!(note.content.contains("status: Accepted"));
        assert!(note.content.contains("- roteiro/status/accepted"));
        assert!(note.content.contains("> **Status:** Accepted"));
        assert!(note.content.contains("## Content\n\nThe decision text."));
        // A clickable link to the actual ADR file on the repository host.
        assert!(
            note.content.contains(
                "**Source:** [`docs/adr/0001.md`](https://github.com/org/repo/blob/abc123/docs/adr/0001.md)"
            ),
            "{}",
            note.content
        );
    }

    /// The structured document a prose note is supposed to reproduce: headings, a
    /// table and a fenced code block, none of which survive whitespace collapse.
    const DOC: &str = "# Working offline\n\nRoteiro is **offline-capable**.\n\n| Host | What |\n| --- | --- |\n| `example.com` | models |\n\n```sh\nroteiro model pull\n```\n";

    fn prose_note(content: Option<&str>) -> Explanation {
        Explanation {
            schema: rto_graph::SCHEMA,
            node: NodeSummary {
                key: "file:docs/OFFLINE_SETUP.md".into(),
                kind: "file".into(),
                name: "OFFLINE_SETUP.md".into(),
                path: Some("docs/OFFLINE_SETUP.md".into()),
                lang: None,
            },
            meta: content.map_or(
                serde_json::Value::Null,
                |c| serde_json::json!({ "content": c }),
            ),
            outgoing: vec![],
            incoming: vec![],
        }
    }

    /// The whole readability defect, in one assertion pair: a note built from
    /// `meta.content` alone is the document whitespace-collapsed onto one line,
    /// and a note built from the source is the document.
    ///
    /// The newline count is the claim. A character count alone would pass on a
    /// note that had merely grown longer while staying flat, which is exactly the
    /// failure being fixed — `meta.content` is capped *and* collapsed, and only
    /// the collapse is what makes it unreadable.
    #[test]
    fn a_supplied_body_supersedes_the_collapsed_stored_content() {
        // What extraction stores: the same text, whitespace-collapsed.
        let collapsed = DOC.split_whitespace().collect::<Vec<_>>().join(" ");
        let ex = prose_note(Some(&collapsed));

        let note = render_note(&ex, None, Some(DOC));
        assert!(
            note.content.contains(DOC.trim()),
            "the source document is reproduced verbatim: {}",
            note.content
        );
        assert!(
            !note.content.contains(&collapsed),
            "the collapsed rendering is replaced, not appended: {}",
            note.content
        );
        assert!(
            note.content.contains("\n| Host | What |\n"),
            "a table needs its own lines to be a table: {}",
            note.content
        );
        assert!(
            note.content.contains("\n```sh\n"),
            "a fenced block needs its own lines to be a fence: {}",
            note.content
        );

        // The flat control: the same node with no body is the one-line note.
        let flat = render_note(&ex, None, None);
        assert!(
            flat.content.contains(&collapsed),
            "without a body the stored content is still shown: {}",
            flat.content
        );
        assert!(
            content_lines(&note.content) > content_lines(&flat.content),
            "structure restored: {} line(s) with a body vs {} without",
            content_lines(&note.content),
            content_lines(&flat.content)
        );
        assert_eq!(
            content_lines(&flat.content),
            1,
            "the defect: the stored content is a single line"
        );
    }

    /// A doc comment is a summary of a definition, not a document, and its note is
    /// correct as it stands. The caller supplies no body for these, so this pins
    /// the unchanged path — the fix must not depend on every node gaining one.
    #[test]
    fn a_note_with_no_body_is_unchanged() {
        let ex = Explanation {
            schema: rto_graph::SCHEMA,
            node: NodeSummary {
                key: "sym:rust:a.rs#main".into(),
                kind: "fn".into(),
                name: "main".into(),
                path: Some("a.rs".into()),
                lang: Some("rust".into()),
            },
            meta: serde_json::json!({ "content": "Entry point." }),
            outgoing: vec![],
            incoming: vec![],
        };
        assert!(
            render_note(&ex, None, None)
                .content
                .contains("## Content\n\nEntry point.")
        );
    }

    /// Lines in the note's `## Content` section.
    fn content_lines(note: &str) -> usize {
        let body = note
            .split_once("## Content\n\n")
            .map_or("", |(_, rest)| rest);
        let body = body.split_once("\n## ").map_or(body, |(head, _)| head);
        body.trim_end().lines().count()
    }

    #[test]
    fn render_note_shows_inferred_confidence() {
        let ex = Explanation {
            schema: rto_graph::SCHEMA,
            node: NodeSummary {
                key: "file:a.md".into(),
                kind: "file".into(),
                name: "a.md".into(),
                path: Some("a.md".into()),
                lang: None,
            },
            meta: serde_json::Value::Null,
            outgoing: vec![EdgeRef {
                kind: "related".into(),
                provenance: "inferred",
                confidence: Some(0.82),
                node: "file:b.md".into(),
            }],
            incoming: vec![],
        };
        let note = render_note(&ex, None, None);
        assert!(
            note.content
                .contains("related (inferred) (0.82) → [[file-b.md]]"),
            "{}",
            note.content
        );
    }

    #[test]
    fn render_home_summarises_the_graph() {
        let summary = VaultSummary {
            project: "demo".into(),
            total_nodes: 3,
            total_edges: 2,
            node_counts: vec![("fn".into(), 2), ("adr".into(), 1)],
            edge_provenance: vec![("derived".into(), 1), ("authored".into(), 1)],
            adrs: vec![AdrEntry {
                key: "adr:0001".into(),
                name: "First".into(),
                status: Some("Accepted".into()),
            }],
            debt: vec![("todo".into(), 4)], // roteiro:ignore
            densest_files: vec![DensityEntry {
                path: "src/small.rs".into(),
                markers: 3,
                lines: 120,
                per_kloc: 25.0,
            }],
            config_secrets: Some(ConfigSecretSummary {
                secret_named: 4,
                redacted: 3,
                declared: 1,
                unredacted: 0,
                files: vec![".env".into()],
            }),
            most_called: vec![CouplingEntry {
                key: "sym:rust:a.rs#helper".into(),
                name: "helper".into(),
                fan_in: 7,
                fan_out: 1,
            }],
            repo_url: Some("https://github.com/org/repo".into()),
            commit: Some("abcdef0123456789".into()),
        };
        let note = render_home(&summary);
        assert_eq!(note.filename, HOME_NOTE);
        assert!(note.content.contains("# demo — knowledge graph"));
        assert!(note.content.contains("**3 nodes**, **2 edges**"));
        assert!(note.content.contains("| fn | 2 |"));
        assert!(note.content.contains("| derived | 1 |"));
        assert!(note.content.contains("**Accepted** — [[adr-0001|First]]"));
        assert!(note.content.contains("| todo | 4 |")); // roteiro:ignore
        // Directed coupling: the two fans are separate columns, and the wikilink's
        // own `|` is escaped so it cannot break the table it sits in.
        assert!(
            note.content
                .contains("| [[sym-rust-a.rs-helper\\|helper]] | 7 | 1 |"),
            "{}",
            note.content
        );
        assert!(
            note.content.contains("resolved by simple name"),
            "the precision caveat travels with the figures"
        );
        // Density: the count and the denominator are both shown, so the ratio can
        // be checked rather than taken on trust, and the wikilink's own `|` is
        // escaped so it cannot break the table it sits in.
        assert!(
            note.content
                .contains("| [[file-src-small.rs\\|src/small.rs]] | 3 | 120 | 25.00 |"),
            "{}",
            note.content
        );
        assert!(
            note.content.contains("not source lines of code"),
            "the denominator caveat travels with the figures"
        );
        // Config secrets: counts and files, and no key names — a vault note is
        // browsed out of context, which is the wrong place for a list that would
        // read as a secret scan's output.
        assert!(
            note.content.contains(
                "**4** secret-named config key(s): 3 redacted before storage, 1 \
                 declared in code without a value, 0 unredacted."
            ),
            "{}",
            note.content
        );
        assert!(
            note.content.contains("- [[file-.env\\|.env]]"),
            "{}",
            note.content
        );
        assert!(
            note.content.contains("not a secret scan")
                && note.content.contains("cannot see a hardcoded credential"),
            "the limitation travels with the figures: {}",
            note.content
        );
        assert!(
            !note.content.contains("[!warning]"),
            "no warning when nothing is unredacted: {}",
            note.content
        );
        // A repository link + short-commit permalink note.
        assert!(
            note.content
                .contains("**Repository:** [https://github.com/org/repo](https://github.com/org/repo) · rendered at commit `abcdef012345`"),
            "{}",
            note.content
        );
    }

    #[test]
    fn render_home_omits_density_for_a_graph_with_no_markers() {
        // A clean repository has no markers, so there is no density to rank. An
        // empty table under a heading reads as "measured, and there is nothing";
        // the section is absent instead. Same rule as the coupling table below.
        let note = render_home(&VaultSummary {
            project: "clean".into(),
            total_nodes: 1,
            ..VaultSummary::default()
        });
        assert!(
            !note.content.contains("Densest files"),
            "no heading without rows: {}",
            note.content
        );
        // The intent-debt section itself still renders — density is an addition
        // to it, not a replacement.
        assert!(note.content.contains("## Intent debt"));
        assert!(note.content.contains("*None recorded.*"));
    }

    #[test]
    fn render_home_omits_config_secrets_rather_than_rendering_zeroes() {
        // A row of zeroes under this heading would read as "scanned, and clean" —
        // a conclusion the lens cannot support, since a credential under an
        // innocuous key name never appears in it. The section is absent instead.
        let note = render_home(&VaultSummary {
            project: "clean".into(),
            total_nodes: 1,
            ..VaultSummary::default()
        });
        assert!(
            !note.content.contains("named like secrets"),
            "no heading without figures: {}",
            note.content
        );
    }

    #[test]
    fn render_home_warns_loudly_about_an_unredacted_value() {
        // Extraction cannot produce this state, so if it appears something else
        // put an unredacted value in the store — and the note must say where to
        // look rather than implicating the repository.
        let note = render_home(&VaultSummary {
            project: "imported".into(),
            total_nodes: 1,
            config_secrets: Some(ConfigSecretSummary {
                secret_named: 1,
                redacted: 0,
                declared: 0,
                unredacted: 1,
                files: vec!["imported.env".into()],
            }),
            ..VaultSummary::default()
        });
        assert!(
            note.content.contains("[!warning]") && note.content.contains("**unredacted**"),
            "{}",
            note.content
        );
        assert!(
            note.content.contains("came from an import layer"),
            "and it points at the importing tool, not the repository: {}",
            note.content
        );
    }

    #[test]
    fn render_home_omits_coupling_for_a_graph_with_no_calls() {
        // A prose-only vault has no `calls` edges. An empty table under a heading
        // reads as "measured, and there is nothing" — the section is absent instead.
        let note = render_home(&VaultSummary {
            project: "docs".into(),
            total_nodes: 1,
            ..VaultSummary::default()
        });
        assert!(
            !note.content.contains("Most depended-on"),
            "no heading without rows: {}",
            note.content
        );
        // The rest of the overview is unaffected.
        assert!(note.content.contains("# docs — knowledge graph"));
    }

    // ---- Workspace vaults (issue #442 part 1) --------------------------------

    /// A `Explanation` for `key`, with one outgoing edge to `to`.
    fn node_linking_to(key: &str, name: &str, to: &str) -> Explanation {
        Explanation {
            schema: rto_graph::SCHEMA,
            node: NodeSummary {
                key: key.into(),
                kind: "config_key".into(),
                name: name.into(),
                path: Some("config.toml".into()),
                lang: None,
            },
            meta: serde_json::Value::Null,
            outgoing: vec![EdgeRef {
                kind: "links".into(),
                provenance: "inferred",
                confidence: Some(0.91),
                node: to.into(),
            }],
            incoming: vec![],
        }
    }

    fn members(names: &[&str]) -> std::collections::BTreeSet<String> {
        names.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn a_project_scope_leaves_every_note_name_exactly_as_it_was() {
        // The compatibility promise of issue #442, as a test rather than a claim:
        // a user's own notes live outside the vault and link into it *by name*, so
        // a name that moves breaks those links silently. Whatever workspace mode
        // does, `VaultScope::PROJECT` must reduce to `note_name` of the bare key.
        for key in [
            "file:README.md",
            "adr:0001",
            "sym:rust:src/a.rs#Store",
            "extref:other::file:README.md",
            "cfgkey:config.toml#serve.addr",
        ] {
            assert_eq!(
                scoped_note_name(&VaultScope::PROJECT, key),
                note_name(key),
                "single-project name moved for `{key}`"
            );
        }
    }

    #[test]
    fn render_note_is_the_project_scoped_render_byte_for_byte() {
        let ex = node_linking_to("cfgkey:config.toml#addr", "addr", "sym:rust:a.rs#A");
        assert_eq!(
            render_note(&ex, Some("https://h/b"), Some("body")),
            render_note_scoped(&ex, Some("https://h/b"), Some("body"), &VaultScope::PROJECT),
            "the unscoped entry point must stay the scoped one at PROJECT, so the \
             two cannot drift apart"
        );
    }

    #[test]
    fn each_member_gets_its_own_note_for_the_same_key() {
        // The collision the whole feature exists for: node keys are
        // repository-relative, so every member's `README.md` is `file:README.md`.
        let ms = members(&["api", "sdk"]);
        let names: Vec<String> = ["api", "sdk"]
            .iter()
            .map(|p| {
                scoped_note_name(
                    &VaultScope {
                        project: Some(p),
                        members: &ms,
                    },
                    "file:README.md",
                )
            })
            .collect();
        assert_eq!(names, ["api-file-README.md", "sdk-file-README.md"]);
        assert_ne!(names[0], names[1], "two members must not share one note");
    }

    #[test]
    fn a_member_note_declares_which_member_it_came_from() {
        let ms = members(&["api"]);
        let ex = node_linking_to("cfgkey:config.toml#addr", "addr", "sym:rust:a.rs#A");
        let note = render_note_scoped(
            &ex,
            None,
            None,
            &VaultScope {
                project: Some("api"),
                members: &ms,
            },
        );
        assert_eq!(note.filename, "api-cfgkey-config.toml-addr.md");
        assert!(
            note.content.contains("project: \"api\""),
            "{}",
            note.content
        );
        assert!(
            note.content.contains("- roteiro/project/api"),
            "the tag is what filters the graph view to one repository: {}",
            note.content
        );
        // A within-member edge is qualified to the same member, not left bare.
        assert!(
            note.content.contains("→ [[api-sym-rust-a.rs-A]]"),
            "{}",
            note.content
        );
    }

    #[test]
    fn a_project_note_declares_no_project() {
        let ex = node_linking_to("cfgkey:config.toml#addr", "addr", "sym:rust:a.rs#A");
        let note = render_note(&ex, None, None);
        assert!(!note.content.contains("project:"), "{}", note.content);
        assert!(
            !note.content.contains("roteiro/project/"),
            "a per-project vault would carry one constant on every note — and \
             adding it would change every note's bytes: {}",
            note.content
        );
    }

    #[test]
    fn a_cross_repo_edge_links_straight_to_the_other_members_note() {
        // ADR-0009: the spoke's edge points at a *local placeholder* for the hub's
        // node, because store integrity needs both ends in one store. A workspace
        // vault holds both, so the link goes to the real note. No new edge — the
        // resolver already follows this placeholder at query time.
        let ms = members(&["spoke", "hub"]);
        let scope = VaultScope {
            project: Some("spoke"),
            members: &ms,
        };
        let ex = node_linking_to(
            "cfgkey:config.toml#addr",
            "addr",
            &rto_graph::external_ref_key("hub::cfgkey:config.toml#addr"),
        );
        let note = render_note_scoped(&ex, None, None, &scope);
        assert!(
            note.content.contains("→ [[hub-cfgkey-config.toml-addr]]"),
            "the edge must land on the hub's own note: {}",
            note.content
        );
        assert!(
            !note.content.contains("extref"),
            "and never on the placeholder: {}",
            note.content
        );
        // The same rule decides that the placeholder is not written as a note, so
        // the two halves cannot disagree.
        assert!(
            scope.redirects_external_ref(&rto_graph::external_ref_key(
                "hub::cfgkey:config.toml#addr"
            ))
        );
    }

    #[test]
    fn a_cross_repo_edge_out_of_the_workspace_keeps_its_placeholder() {
        // The target repo is not in this vault, so there is no note to point at.
        // Redirecting anyway would produce a link that resolves to nothing —
        // Obsidian shows that as merely unwritten, which is a worse lie than a
        // placeholder that honestly says "elsewhere".
        let ms = members(&["spoke"]);
        let scope = VaultScope {
            project: Some("spoke"),
            members: &ms,
        };
        let key = rto_graph::external_ref_key("elsewhere::cfgkey:config.toml#addr");
        assert!(!scope.redirects_external_ref(&key));
        let ex = node_linking_to("cfgkey:config.toml#addr", "addr", &key);
        let note = render_note_scoped(&ex, None, None, &scope);
        assert!(
            note.content
                .contains("→ [[spoke-extref-elsewhere-cfgkey-config.toml-addr]]"),
            "{}",
            note.content
        );
    }

    #[test]
    fn a_single_project_vault_never_redirects_an_external_ref() {
        // No members ⇒ nothing to resolve against, so today's vault keeps rendering
        // the placeholder exactly as it does now.
        let key = rto_graph::external_ref_key("hub::cfgkey:config.toml#addr");
        assert!(!VaultScope::PROJECT.redirects_external_ref(&key));
        assert_eq!(
            scoped_note_name(&VaultScope::PROJECT, &key),
            note_name(&key)
        );
    }

    fn member_summary(project: &str, fan_in: u32) -> VaultSummary {
        VaultSummary {
            project: project.to_owned(),
            total_nodes: 3,
            total_edges: 2,
            node_counts: vec![("fn".into(), 2)],
            edge_provenance: vec![("derived".into(), 2)],
            adrs: vec![AdrEntry {
                key: "adr:0001".into(),
                name: "First".into(),
                status: Some("Accepted".into()),
            }],
            debt: vec![("todo".into(), 4)], // roteiro:ignore
            densest_files: vec![DensityEntry {
                path: "src/small.rs".into(),
                markers: 3,
                lines: 120,
                per_kloc: 25.0,
            }],
            config_secrets: None,
            most_called: vec![CouplingEntry {
                key: "sym:rust:a.rs#helper".into(),
                name: "helper".into(),
                fan_in,
                fan_out: 1,
            }],
            repo_url: Some(format!("https://github.com/org/{project}")),
            commit: Some("abcdef0123456789".into()),
        }
    }

    #[test]
    fn the_workspace_home_keeps_every_members_own_aggregates() {
        // The promise in issue #442: the existing per-project `_Home` view is a
        // *subset* of the workspace one, not a casualty of it. Someone who came for
        // their repository's coupling and debt tables must still find them —
        // not a workspace total that averages them away.
        let ws = WorkspaceSummary {
            name: "platform".into(),
            members: vec![member_summary("api", 7), member_summary("sdk", 4)],
            cross_links: vec![],
            cross_links_total: 0,
        };
        let note = render_workspace_home(&ws);
        assert_eq!(note.filename, HOME_NOTE);
        assert!(
            note.content
                .contains("# platform — workspace knowledge graph")
        );
        // Summed, and the members listed.
        assert!(
            note.content
                .contains("**6 nodes**, **4 edges** across **2** member")
        );
        assert!(note.content.contains("| [[#api\\|api]] | 3 | 2 |"));

        for project in ["api", "sdk"] {
            assert!(
                note.content.contains(&format!("\n## {project}\n")),
                "each member gets its own section"
            );
        }
        // Today's sections, one level deeper, once per member.
        for section in [
            "### Structure",
            "### Provenance",
            "### Decisions (ADRs)",
            "### Intent debt",
            "#### Densest files",
            "### Most depended-on",
        ] {
            assert_eq!(
                note.content.matches(section).count(),
                2,
                "`{section}` must appear once per member: {}",
                note.content
            );
        }
        // And every link inside a member's section resolves within that member.
        assert!(
            note.content
                .contains("**Accepted** — [[api-adr-0001|First]]")
        );
        assert!(
            note.content
                .contains("**Accepted** — [[sdk-adr-0001|First]]")
        );
        assert!(
            note.content
                .contains("[[api-sym-rust-a.rs-helper\\|helper]] | 7 |")
        );
        assert!(
            note.content
                .contains("[[sdk-file-src-small.rs\\|src/small.rs]]")
        );
    }

    #[test]
    fn the_workspace_home_renders_cross_repo_links_and_marks_the_ones_it_cannot_follow() {
        let ws = WorkspaceSummary {
            name: "platform".into(),
            members: vec![member_summary("api", 7), member_summary("sdk", 4)],
            cross_links: vec![
                CrossLink {
                    from_project: "sdk".into(),
                    from_key: "cfgkey:config.toml#addr".into(),
                    from_name: "addr".into(),
                    kind: "links".into(),
                    confidence: Some(0.91),
                    to_qualified: "api::cfgkey:config.toml#addr".into(),
                    resolves: true,
                },
                CrossLink {
                    from_project: "sdk".into(),
                    from_key: "cfgkey:config.toml#other".into(),
                    from_name: "other".into(),
                    kind: "links".into(),
                    confidence: None,
                    to_qualified: "absent::cfgkey:config.toml#other".into(),
                    resolves: false,
                },
            ],
            cross_links_total: 2,
        };
        let note = render_workspace_home(&ws);
        // Resolvable: a link to the other member's note, with its confidence.
        assert!(
            note.content.contains(
                "| [[sdk-cfgkey-config.toml-addr\\|addr]] | sdk | \
                 [[api-cfgkey-config.toml-addr\\|api::cfgkey:config.toml#addr]] | links (0.91) |"
            ),
            "{}",
            note.content
        );
        // Outside the workspace: stated as such, never as a wikilink — Obsidian
        // renders a link to a missing note as one that is merely unwritten.
        assert!(
            note.content
                .contains("`absent::cfgkey:config.toml#other` *(outside this workspace)*"),
            "{}",
            note.content
        );
        assert!(
            !note.content.contains("[[absent-"),
            "a dangling wikilink would read as a note someone forgot to write: {}",
            note.content
        );
    }

    #[test]
    fn the_workspace_home_says_when_it_has_truncated_the_cross_links() {
        // A capped table that does not say it is capped reads as the whole set.
        let ws = WorkspaceSummary {
            name: "platform".into(),
            members: vec![member_summary("api", 7)],
            cross_links: vec![CrossLink {
                from_project: "api".into(),
                from_key: "cfgkey:config.toml#addr".into(),
                from_name: "addr".into(),
                kind: "links".into(),
                confidence: None,
                to_qualified: "api::cfgkey:config.toml#addr".into(),
                resolves: true,
            }],
            cross_links_total: 40,
        };
        let note = render_workspace_home(&ws);
        assert!(note.content.contains("Showing 1 of 40"), "{}", note.content);
        assert!(note.content.contains("roteiro links --matrix"));
    }

    #[test]
    fn a_workspace_with_no_cross_repo_links_says_why_rather_than_showing_nothing() {
        let ws = WorkspaceSummary {
            name: "platform".into(),
            members: vec![member_summary("api", 7)],
            cross_links: vec![],
            cross_links_total: 0,
        };
        let note = render_workspace_home(&ws);
        assert!(note.content.contains("## Cross-repo links"));
        assert!(
            note.content.contains("links --infer --write"),
            "an empty section must name what would fill it, or it reads as \
             \"these repos are unrelated\": {}",
            note.content
        );
        // Singular, because getting this wrong on a one-member workspace is the
        // kind of thing nobody notices until it ships.
        assert!(note.content.contains("**1** member repository."));
    }
}

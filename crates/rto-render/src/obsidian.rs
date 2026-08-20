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
    let meta = &ex.meta;
    let status = meta.get("status").and_then(|v| v.as_str());
    let content = note_body(meta.get("content").and_then(|v| v.as_str()), body);

    let mut c = String::new();
    c.push_str("---\n");
    let _ = writeln!(c, "key: \"{}\"", ex.node.key.replace('"', "'"));
    let _ = writeln!(c, "kind: {}", ex.node.kind);
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
                note_name(&e.node)
            );
        }
    }
    if !ex.incoming.is_empty() {
        c.push_str("\n## Incoming\n\n");
        for e in &ex.incoming {
            let _ = writeln!(
                c,
                "- [[{}]] {} ({}){} →",
                note_name(&e.node),
                e.kind,
                e.provenance,
                confidence(e.confidence)
            );
        }
    }

    VaultNote {
        filename: format!("{}.md", note_name(&ex.node.key)),
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
    c.push_str(
        "\n**How to read it.** Open any note to see what a thing is, the intent or \
         docs behind it (its **Content**), where it lives (its **Source** link), \
         and how it connects (**Outgoing**/**Incoming** links). Each link is \
         labelled with how the fact was established — `derived` (extracted from \
         code), `authored` (human intent: ADRs, blueprints, annotations), or \
         `inferred` (a scored suggestion). Open Obsidian's **graph view** to see \
         the whole thing at once.\n",
    );
    let _ = writeln!(
        c,
        "\n**{} nodes**, **{} edges** across the project.",
        s.total_nodes, s.total_edges
    );
    if let Some(repo) = &s.repo_url {
        let _ = write!(c, "\n**Repository:** [{repo}]({repo})");
        if let Some(commit) = &s.commit {
            let short = &commit[..commit.len().min(12)];
            let _ = write!(c, " · rendered at commit `{short}`");
        }
        c.push('\n');
    }

    c.push_str("\n## Structure\n\n| Kind | Count |\n| --- | --- |\n");
    for (kind, n) in &s.node_counts {
        let _ = writeln!(c, "| {kind} | {n} |");
    }

    if !s.edge_provenance.is_empty() {
        c.push_str("\n## Provenance\n\n| Provenance | Edges |\n| --- | --- |\n");
        for (prov, n) in &s.edge_provenance {
            let _ = writeln!(c, "| {prov} | {n} |");
        }
    }

    c.push_str("\n## Decisions (ADRs)\n\n");
    if s.adrs.is_empty() {
        c.push_str("*No ADRs found.*\n");
    } else {
        for adr in &s.adrs {
            let status = adr.status.as_deref().unwrap_or("—");
            let _ = writeln!(
                c,
                "- **{status}** — [[{}|{}]]",
                note_name(&adr.key),
                adr.name
            );
        }
    }

    c.push_str("\n## Intent debt\n\n");
    if s.debt.is_empty() {
        c.push_str("*None recorded.*\n");
    } else {
        c.push_str("| Category | Count |\n| --- | --- |\n");
        for (cat, n) in &s.debt {
            let _ = writeln!(c, "| {cat} | {n} |");
        }
    }

    if !s.densest_files.is_empty() {
        c.push_str(
            "\n### Densest files (markers per 1,000 lines)\n\n\
             *Where the debt above is concentrated, rather than where there is \
             most of it — a raw count ranks the biggest file first by \
             construction. The denominator is file length: every line, blanks and \
             comments included, not source lines of code. Prose matches (`for \
             now`, `tbd`) count too, so a design document can rank high.*\n\n",
        );
        c.push_str("| File | Markers | Lines | Per 1k |\n| --- | --- | --- | --- |\n");
        for e in &s.densest_files {
            let _ = writeln!(
                c,
                "| [[{}\\|{}]] | {} | {} | {:.2} |",
                note_name(&format!("file:{}", e.path)),
                e.path,
                e.markers,
                e.lines,
                e.per_kloc
            );
        }
    }

    if let Some(cs) = &s.config_secrets {
        c.push_str("\n## Config keys named like secrets\n\n");
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
                let _ = writeln!(c, "- [[{}\\|{path}]]", note_name(&format!("file:{path}")));
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

    if !s.most_called.is_empty() {
        c.push_str(
            "\n## Most depended-on (call fan-in)\n\n\
             *Distinct callers and callees over `calls` edges — direction kept, so \
             \"everything calls this\" and \"this calls everything\" are not the same \
             row. Call targets are resolved by simple name, so a short, generically-\
             named function can absorb every call to that name: read a large fan-in on \
             one as a question, not a finding.*\n\n",
        );
        c.push_str("| Symbol | Called by | Calls |\n| --- | --- | --- |\n");
        for e in &s.most_called {
            let _ = writeln!(
                c,
                "| [[{}\\|{}]] | {} | {} |",
                note_name(&e.key),
                e.name,
                e.fan_in,
                e.fan_out
            );
        }
    }

    c.push_str(
        "\n## Navigating this vault\n\n\
         - Open the **graph view** to see the whole codebase; notes are coloured/\
         filterable by their `roteiro/kind/*`, `roteiro/lang/*` and \
         `roteiro/status/*` tags.\n\
         - Each note carries its captured **content** (doc comments, prose, PDF/\
         image text) and its provenance-labelled incoming/outgoing links.\n\
         - Start from an ADR above, or search the tag pane for a kind.\n",
    );

    VaultNote {
        filename: HOME_NOTE.to_owned(),
        content: c,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AdrEntry, ConfigSecretSummary, CouplingEntry, DensityEntry, HOME_NOTE, VaultSummary,
        note_name, render_home, render_note,
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
}

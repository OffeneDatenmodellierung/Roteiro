//! The Obsidian-vault renderer: each graph node becomes a markdown note whose
//! edges are `[[wikilinks]]`, so the provenance-tagged graph is browsable in
//! Obsidian's graph view. Notes carry frontmatter `tags` (`roteiro/kind/*`,
//! `roteiro/lang/*`, `roteiro/status/*`) so the graph is colourable/filterable —
//! edge provenance is shown per-link in the body — surface the captured
//! `meta.content` (doc comments, prose, PDF/image text) as the knowledge base,
//! show an ADR's status, and (when the repository's web host is known) a
//! clickable **Source** link to the file. A generated `_Home` note is the overview: what was
//! scanned, counts by kind, provenance breakdown, ADR statuses, and intent-debt.
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
/// the captured content as the knowledge base, and its edges as provenance-
/// labelled wikilinks.
#[must_use]
pub fn render_note(ex: &Explanation, source_base: Option<&str>) -> VaultNote {
    let meta = &ex.meta;
    let status = meta.get("status").and_then(|v| v.as_str());
    let content = meta.get("content").and_then(|v| v.as_str());

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

    // The knowledge base: the captured doc comment / prose / PDF / image text.
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
    use super::{AdrEntry, HOME_NOTE, VaultSummary, note_name, render_home, render_note};
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
        let note = render_note(&ex, None);
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
        let note = render_note(&ex, Some("https://github.com/org/repo/blob/abc123"));
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
        let note = render_note(&ex, None);
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
            debt: vec![("todo".into(), 4)],
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
        assert!(note.content.contains("| todo | 4 |"));
        // A repository link + short-commit permalink note.
        assert!(
            note.content
                .contains("**Repository:** [https://github.com/org/repo](https://github.com/org/repo) · rendered at commit `abcdef012345`"),
            "{}",
            note.content
        );
    }
}

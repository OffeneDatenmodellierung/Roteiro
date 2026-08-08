//! The Obsidian-vault renderer: each graph node becomes a markdown note whose
//! edges are `[[wikilinks]]`, so the provenance-tagged graph is browsable in
//! Obsidian. Built from the same [`Explanation`] the query surface returns, so
//! the vault and the CLI agree.

use std::fmt::Write as _;

use rto_graph::Explanation;

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
/// to `-`; alphanumerics, `.`, `_` and `-` are kept.
#[must_use]
pub fn note_name(key: &str) -> String {
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
    out.trim_matches('-').to_owned()
}

/// Render a node's [`Explanation`] into an Obsidian note: YAML frontmatter,
/// a heading, and its outgoing/incoming edges as provenance-labelled wikilinks.
#[must_use]
pub fn render_note(ex: &Explanation) -> VaultNote {
    let mut c = String::new();
    c.push_str("---\n");
    let _ = writeln!(c, "key: \"{}\"", ex.node.key.replace('"', "'"));
    let _ = writeln!(c, "kind: {}", ex.node.kind);
    if let Some(path) = &ex.node.path {
        let _ = writeln!(c, "path: \"{path}\"");
    }
    c.push_str("---\n\n");
    let _ = writeln!(c, "# {}", ex.node.name);

    if !ex.outgoing.is_empty() {
        c.push_str("\n## Outgoing\n\n");
        for e in &ex.outgoing {
            let _ = writeln!(
                c,
                "- {} ({}) → [[{}]]",
                e.kind,
                e.provenance,
                note_name(&e.node)
            );
        }
    }
    if !ex.incoming.is_empty() {
        c.push_str("\n## Incoming\n\n");
        for e in &ex.incoming {
            let _ = writeln!(
                c,
                "- [[{}]] {} ({}) →",
                note_name(&e.node),
                e.kind,
                e.provenance
            );
        }
    }

    VaultNote {
        filename: format!("{}.md", note_name(&ex.node.key)),
        content: c,
    }
}

#[cfg(test)]
mod tests {
    use super::{note_name, render_note};
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
        let note = render_note(&ex);
        assert_eq!(note.filename, "sym-rust-a.rs-main.md");
        assert!(note.content.contains("kind: fn"));
        assert!(note.content.contains("# main"));
        assert!(
            note.content
                .contains("- calls (derived) → [[sym-rust-a.rs-helper]]")
        );
        assert!(
            note.content
                .contains("- [[adr-0001]] references (authored) →")
        );
    }
}

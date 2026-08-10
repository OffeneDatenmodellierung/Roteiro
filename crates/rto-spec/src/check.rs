//! Drift checking: validate the authored layer (ADR wiki-links and `@rto:`
//! annotations) against the derived code graph, and weave the valid links in as
//! `authored` edges.
//!
//! [`run`] expects `store` to already hold the derived graph (symbols, files).
//! It applies each ADR's structural nodes, then for every authored link checks
//! that its target exists — reporting a [`Violation`] when it does not — and
//! adds an `authored` edge when it does.

use rto_graph::{Edge, EdgeKind, Store, StoreError};
use serde::Serialize;

use crate::adr::{AdrDoc, AdrStatus};
use crate::annotate::Annotation;
use crate::blueprint::BlueprintDoc;

/// The category of an authored-layer drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ViolationKind {
    /// An ADR under `docs/adr` could not be parsed.
    MalformedAdr,
    /// An ADR `[[…]]` link points at a symbol or file not in the graph.
    BrokenLink,
    /// A `@rto:` annotation references an ADR that does not exist.
    UnknownAdr,
    /// A `@rto:` annotation references a rejected or superseded ADR.
    InactiveAdr,
}

impl ViolationKind {
    /// A short stable label for this kind.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::MalformedAdr => "malformed-adr",
            Self::BrokenLink => "broken-link",
            Self::UnknownAdr => "unknown-adr",
            Self::InactiveAdr => "inactive-adr",
        }
    }
}

/// A single authored-layer drift finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Violation {
    /// What kind of drift this is.
    pub kind: ViolationKind,
    /// A human-readable, location-prefixed message.
    pub message: String,
}

/// The outcome of a [`run`]: how much authored content was checked and any
/// drift found.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CheckReport {
    /// Number of ADRs parsed and applied.
    pub adrs: usize,
    /// Number of blueprints parsed and applied.
    pub blueprints: usize,
    /// Authored `[[…]]` links that resolved and became edges.
    pub links_ok: usize,
    /// `@rto:` annotations that resolved to an active ADR.
    pub annotations_ok: usize,
    /// Drift findings; the check fails if this is non-empty.
    pub violations: Vec<Violation>,
}

impl CheckReport {
    /// Whether any drift was found.
    #[must_use]
    pub fn has_violations(&self) -> bool {
        !self.violations.is_empty()
    }
}

/// Apply the authored layer to `store` and validate it against the derived
/// graph, returning a [`CheckReport`].
///
/// # Errors
/// Returns [`StoreError`] if applying ADR facts or edges, or querying the
/// store, fails.
pub fn run(
    store: &mut Store,
    docs: &[AdrDoc],
    blueprints: &[BlueprintDoc],
    annotations: &[Annotation],
) -> Result<CheckReport, StoreError> {
    // 1. Materialise ADR/blueprint section nodes so links and annotations can
    //    reference them (and so `@rto:` targets can be looked up by key).
    for doc in docs {
        store.apply_factset(&doc.facts())?;
    }
    for bp in blueprints {
        store.apply_factset(&bp.facts())?;
    }

    let mut report = CheckReport {
        adrs: docs.len(),
        blueprints: blueprints.len(),
        ..CheckReport::default()
    };

    // 2. Validate ADR and blueprint `[[…]]` links against the code graph. Both
    //    author `references` edges into real symbols/files and drift the same way.
    let links = docs
        .iter()
        .flat_map(|d| &d.links)
        .chain(blueprints.iter().flat_map(|b| &b.links));
    for link in links {
        if store.get_node(&link.target_key)?.is_some() {
            store.insert_edge(&Edge::authored(
                link.from.clone(),
                link.target_key.clone(),
                EdgeKind::References,
            ))?;
            report.links_ok += 1;
        } else {
            report.violations.push(Violation {
                kind: ViolationKind::BrokenLink,
                message: format!(
                    "{}: authored link [[{}]] does not resolve ({} not found in graph)",
                    link.from, link.raw, link.target_key
                ),
            });
        }
    }

    // 3. Validate `@rto:` annotations against ADR state.
    for ann in annotations {
        let key = ann.target_key();
        let Some(adr) = store.get_node(&key)? else {
            report.violations.push(Violation {
                kind: ViolationKind::UnknownAdr,
                message: format!(
                    "{}:{}: @rto:{} references unknown ADR",
                    ann.path, ann.line, ann.adr_id
                ),
            });
            continue;
        };
        let status = adr
            .meta
            .get("status")
            .and_then(|s| s.as_str())
            .and_then(|s| s.parse::<AdrStatus>().ok());
        if status.is_some_and(|s| !s.is_active()) {
            report.violations.push(Violation {
                kind: ViolationKind::InactiveAdr,
                message: format!(
                    "{}:{}: @rto:{} references non-active ADR ({})",
                    ann.path,
                    ann.line,
                    ann.adr_id,
                    adr.meta
                        .get("status")
                        .and_then(|s| s.as_str())
                        .unwrap_or("?")
                ),
            });
            continue;
        }
        // Link the annotated file to the ADR when the file is in the graph.
        let file_key = format!("file:{}", ann.path);
        if store.get_node(&file_key)?.is_some() {
            store.insert_edge(&Edge::authored(file_key, key, EdgeKind::References))?;
        }
        report.annotations_ok += 1;
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::{ViolationKind, run};
    use crate::adr::parse_adr;
    use crate::annotate::scan_annotations;
    use rto_graph::{Node, NodeKind, Store};

    fn seed_graph(store: &Store) {
        // A tiny derived graph: one file and one symbol.
        store
            .upsert_node(&Node::new("file:src/store.rs", NodeKind::File, "store.rs"))
            .expect("file");
        store
            .upsert_node(&Node::new(
                "sym:rust:src/store.rs#Store",
                NodeKind::Struct,
                "Store",
            ))
            .expect("sym");
    }

    #[test]
    fn resolvable_links_and_annotations_pass() {
        let mut store = Store::open_in_memory().expect("store");
        seed_graph(&store);

        let adr = "---\nadr-id: \"0001\"\nstatus: Accepted\n---\n\n# ADR-0001\n\n## Design\n\nUses [[src/store.rs#Store]].\n";
        let doc = parse_adr("docs/adr/0001.md", adr).expect("parse");
        let anns = scan_annotations("src/store.rs", "//! @rto:0001\n");

        let report = run(&mut store, &[doc], &[], &anns).expect("run");
        assert!(!report.has_violations(), "{:?}", report.violations);
        assert_eq!(report.links_ok, 1);
        assert_eq!(report.annotations_ok, 1);
        // The authored edge is now in the graph.
        let edges = store.edges_from("adr:0001#design").expect("edges");
        assert!(edges.iter().any(|e| e.dst == "sym:rust:src/store.rs#Store"));
    }

    #[test]
    fn broken_link_is_a_violation() {
        let mut store = Store::open_in_memory().expect("store");
        seed_graph(&store);
        let adr =
            "---\nadr-id: \"0001\"\nstatus: Accepted\n---\n\n## Design\n\n[[src/store.rs#Ghost]]\n";
        let doc = parse_adr("docs/adr/0001.md", adr).expect("parse");

        let report = run(&mut store, &[doc], &[], &[]).expect("run");
        assert_eq!(report.violations.len(), 1);
        assert_eq!(report.violations[0].kind, ViolationKind::BrokenLink);
    }

    #[test]
    fn annotation_to_unknown_and_superseded_adrs() {
        let mut store = Store::open_in_memory().expect("store");
        seed_graph(&store);
        let superseded =
            "---\nadr-id: \"0002\"\nstatus: Superseded\n---\n\n# Old\n\n## X\n\nbody\n";
        let doc = parse_adr("docs/adr/0002.md", superseded).expect("parse");
        let anns = scan_annotations("src/store.rs", "// @rto:0002\n// @rto:9999\n");

        let report = run(&mut store, &[doc], &[], &anns).expect("run");
        let kinds: Vec<_> = report.violations.iter().map(|v| v.kind).collect();
        assert!(kinds.contains(&ViolationKind::InactiveAdr));
        assert!(kinds.contains(&ViolationKind::UnknownAdr));
        assert_eq!(report.annotations_ok, 0);
    }
}

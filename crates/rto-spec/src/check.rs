//! Drift checking: validate the authored layer (ADR wiki-links and `@rto:`
//! annotations) against the derived code graph, and weave the valid links in as
//! `authored` edges.
//!
//! [`run`] expects `store` to already hold the derived graph (symbols, files).
//! It applies each ADR's structural nodes, then for every authored link checks
//! that its target exists — reporting a [`Violation`] when it does not — and
//! adds an `authored` edge when it does.
//!
//! It also guards the authored layer's own integrity: ADR ids are node keys, so
//! two ADRs claiming one id silently discard a decision. See
//! [`duplicate_adr_ids`].

use std::collections::BTreeMap;

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
    /// Two or more ADR files declare the same `adr-id`.
    DuplicateAdrId,
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
            Self::DuplicateAdrId => "duplicate-adr-id",
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

/// Find `adr-id` values claimed by more than one ADR file.
///
/// An ADR's node key is `adr:<id>` ([`AdrDoc::key`]), so this collision is not
/// cosmetic — it is *lossy*. Two files sharing an id produce one node key, the
/// later [`Store::apply_factset`] overwrites the earlier, and from then on
/// `query adr:NNNN` answers for one decision while the other is invisible, every
/// `@rto:NNNN` annotation binds to whichever won, and the published artifact
/// carries the survivor alone. Nothing else in the pipeline notices: the two
/// files merge cleanly in git (they touch no common line) and every other check
/// passes. That is exactly how ADR-0016 came to be authored twice on two
/// parallel branches in this repository.
///
/// The message names **both** paths and the id: an id alone leaves the reader to
/// hunt for the partner file, which is the work this check exists to save.
///
/// The same collision class does *not* exist for the other keyed documents,
/// because their ids are their paths, and a tree cannot hold two files at one
/// path: blueprints are `blueprint:<path>` ([`BlueprintDoc::key`]), `lat.md`
/// nodes are `lat:<path>`, files are `file:<path>` and symbols are
/// `sym:<lang>:<path>#<symbol>`. Imported Graphify nodes (`graphify:<id>`) do
/// carry an author-chosen id, but importing is an explicit, single-document act
/// whose merge semantics are deliberate rather than accidental, and hyperedges
/// are already namespaced away from nodes to prevent exactly this clobber.
/// Multi-repo workspaces hold one [`Store`] per project, so ids collide only
/// within a repository, never across one.
fn duplicate_adr_ids(docs: &[AdrDoc]) -> Vec<Violation> {
    let mut by_id: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for doc in docs {
        by_id
            .entry(doc.meta.id.as_str())
            .or_default()
            .push(doc.path.as_str());
    }
    by_id
        .into_iter()
        .filter(|(_, paths)| paths.len() > 1)
        .map(|(id, mut paths)| {
            // Sort so the message is stable whatever order the tree walk yielded.
            paths.sort_unstable();
            Violation {
                kind: ViolationKind::DuplicateAdrId,
                message: format!(
                    "adr-id {id} is declared by {} files: {} — all of them collapse \
                     into the single node `adr:{id}`, so only one decision survives \
                     and every @rto:{id} annotation binds to it",
                    paths.len(),
                    paths.join(", "),
                ),
            }
        })
        .collect()
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
    // 1. Detect colliding ADR ids *before* applying anything, so the report
    //    describes the authored file set rather than what survived the merge.
    let mut duplicates = duplicate_adr_ids(docs);

    // 2. Materialise ADR/blueprint section nodes so links and annotations can
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
    report.violations.append(&mut duplicates);

    // 3. Validate ADR and blueprint `[[…]]` links against the code graph. Both
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

    // 4. Validate `@rto:` annotations against ADR state.
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
    fn two_adrs_sharing_an_id_are_a_violation_naming_both_files() {
        // The regression from issue #324: two branches each author ADR-0016.
        // Both files merge cleanly, both parse, and both apply to the *same*
        // node key — so without this check the report is 0 violations.
        let mut store = Store::open_in_memory().expect("store");
        seed_graph(&store);
        let one = parse_adr(
            "docs/adr/0016-audio-metadata.md",
            "---\nadr-id: \"0016\"\nstatus: Accepted\n---\n\n# Audio metadata\n\n## Decision\n\nbody\n",
        )
        .expect("parse one");
        let two = parse_adr(
            "docs/adr/0016-speculative-decoding.md",
            "---\nadr-id: \"0016\"\nstatus: Accepted\n---\n\n# Speculative decoding\n\n## Decision\n\nbody\n",
        )
        .expect("parse two");

        let report = run(&mut store, &[one, two], &[], &[]).expect("run");
        let dupes: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.kind == ViolationKind::DuplicateAdrId)
            .collect();
        assert_eq!(dupes.len(), 1, "one finding for the one colliding id");
        // Both paths and the id must be named — an id alone makes the reader hunt.
        let msg = &dupes[0].message;
        assert!(msg.contains("0016"), "names the shared id: {msg}");
        assert!(
            msg.contains("docs/adr/0016-audio-metadata.md"),
            "names the first file: {msg}"
        );
        assert!(
            msg.contains("docs/adr/0016-speculative-decoding.md"),
            "names the second file: {msg}"
        );
        assert!(report.has_violations(), "the gate must fail");
    }

    #[test]
    fn distinct_adr_ids_are_not_a_duplicate_violation() {
        let mut store = Store::open_in_memory().expect("store");
        seed_graph(&store);
        let one = parse_adr(
            "docs/adr/0001-a.md",
            "---\nadr-id: \"0001\"\nstatus: Accepted\n---\n\n# A\n\n## Decision\n\nbody\n",
        )
        .expect("parse one");
        let two = parse_adr(
            "docs/adr/0002-b.md",
            "---\nadr-id: \"0002\"\nstatus: Accepted\n---\n\n# B\n\n## Decision\n\nbody\n",
        )
        .expect("parse two");

        let report = run(&mut store, &[one, two], &[], &[]).expect("run");
        assert!(!report.has_violations(), "{:?}", report.violations);
    }

    #[test]
    fn three_files_on_one_id_report_once_and_name_all_three() {
        let mut store = Store::open_in_memory().expect("store");
        seed_graph(&store);
        let docs: Vec<_> = ["c.md", "a.md", "b.md"]
            .iter()
            .map(|name| {
                parse_adr(
                    &format!("docs/adr/{name}"),
                    "---\nadr-id: \"0007\"\nstatus: Accepted\n---\n\n# X\n\n## Decision\n\nbody\n",
                )
                .expect("parse")
            })
            .collect();

        let report = run(&mut store, &docs, &[], &[]).expect("run");
        assert_eq!(report.violations.len(), 1, "one finding, not one per file");
        let msg = &report.violations[0].message;
        // Paths are sorted, so the message does not depend on tree-walk order.
        assert!(
            msg.contains("docs/adr/a.md, docs/adr/b.md, docs/adr/c.md"),
            "names all three in a stable order: {msg}"
        );
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

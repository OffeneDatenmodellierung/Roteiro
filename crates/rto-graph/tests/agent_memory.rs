//! The invariants that make episodic agent memory a *separate* artifact store
//! rather than a graph fact (ADR-0013, issue #288).
//!
//! Every assertion here exists because the alternative is mechanically possible.
//! Writing a lesson into a `NodeKind::Other("memory")` node compiles, passes,
//! inherits `nodes.provenance DEFAULT 'derived'`, is swept into `export_factset`,
//! and — if it were tagged `authored` instead — would collect the +40 relevance
//! boost that `search` reserves for reviewed, deliberately written intent. These
//! tests are what stop an unreviewed, accumulated, unredacted store of prose from
//! quietly acquiring the graph's trust.
//!
//! **None of them needs a model, a GPU or a network.** Memory is `SQLite` and
//! serde; these run everywhere CI does and self-skip nowhere.

use rto_graph::{
    AnchorState, DEFAULT_MEMORY_SCOPE, Edge, EdgeKind, FactSet, GraphArtifact, MemoryFilter,
    MemoryKind, MemoryWrite, Node, NodeKind, Provenance, SearchOptions, Store, search,
    search_channels,
};

/// A lesson with a phrase in it that appears nowhere in the seeded graph, so a
/// leak into `nodes`/`edges`/`search` is unambiguous when it happens.
const LESSON: &str = "The pelican migration failed because the retry loop double-counted \
     partial batches; do not reintroduce the batch cursor without a dedup key.";

/// A small graph standing in for a real repository's derived + authored layers.
fn seed_graph(store: &mut Store) {
    let mut facts = FactSet::new();
    let mut adr =
        Node::new("adr:0013", NodeKind::Adr, "Agent memory").with_provenance(Provenance::Authored);
    adr.path = Some("docs/adr/0013.md".into());
    adr.meta = serde_json::json!({
        "content": "Durable agent-learned knowledge lives in its own artifact store and \
                    never borrows the graph's trust.",
    });
    let mut migrate = Node::new("sym:rust:src/migrate.rs#run", NodeKind::Fn, "run");
    migrate.path = Some("src/migrate.rs".into());
    migrate.blob_hash = Some("blob-migrate-v1".into());
    facts.nodes = vec![
        adr,
        migrate,
        Node::new("sym:rust:src/lib.rs#main", NodeKind::Fn, "main"),
    ];
    facts.edges = vec![Edge::authored(
        "adr:0013",
        "sym:rust:src/lib.rs#main",
        EdgeKind::References,
    )];
    store.rebuild(&facts, Some("treeabc")).expect("rebuild");
}

/// The same graph with `run` recompiled to a different blob — the "code changed
/// underneath the record" case, with the node still present under its key.
fn seed_graph_with_changed_blob(store: &mut Store) {
    let mut facts = store.export_factset().expect("export");
    for node in &mut facts.nodes {
        if node.key == "sym:rust:src/migrate.rs#run" {
            node.blob_hash = Some("blob-migrate-v2".into());
        }
    }
    store.rebuild(&facts, Some("treedef")).expect("rebuild");
}

/// A default write: unanchored, `lesson`, default scope.
fn lesson(body: &str) -> MemoryWrite<'_> {
    MemoryWrite {
        scope: DEFAULT_MEMORY_SCOPE,
        kind: MemoryKind::Lesson,
        anchor: None,
        body,
        confidence: None,
        supersedes: None,
    }
}

/// One record's anchor verdict against the graph as it stands right now.
fn state(store: &Store, id: i64) -> AnchorState {
    store
        .memory_record(id)
        .expect("get")
        .expect("present")
        .anchor_state
}

/// Every live record, newest generation first.
fn live(store: &Store) -> Vec<rto_graph::MemoryRecord> {
    store
        .memory_records(&MemoryFilter::default())
        .expect("records")
}

// --- The graph stays a pure function of the tree -----------------------------

/// **`export_factset` is byte-identical across every memory write.**
///
/// The published artifact is a pure function of the tree; a memory record is not
/// a function of the tree at all. The two can only coexist if writing one has no
/// effect whatsoever on the other, and "no effect" is checked on the serialised
/// bytes rather than field by field — a change that reordered a `meta` key would
/// satisfy a structural comparison and still break the published artifact.
#[test]
fn memory_writes_leave_the_exported_artifact_byte_identical() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);

    let before_bytes =
        serde_json::to_vec(&store.export_factset().expect("export")).expect("serialize");
    let before_artifact =
        serde_json::to_vec(&GraphArtifact::from_store(&store).expect("artifact")).expect("bytes");
    let (before_nodes, before_edges) = (
        store.node_count().expect("nodes"),
        store.edge_count().expect("edges"),
    );

    // A full spread of writes: anchored, unanchored, superseding, and a forget.
    let anchored = store
        .record_memory(&MemoryWrite {
            anchor: Some("sym:rust:src/migrate.rs#run"),
            kind: MemoryKind::Attempt,
            confidence: Some(0.9),
            ..lesson(LESSON)
        })
        .expect("anchored write");
    store
        .record_memory(&MemoryWrite {
            supersedes: Some(anchored),
            ..lesson("Superseded: the dedup key alone was not enough.")
        })
        .expect("superseding write");
    let doomed = store
        .record_memory(&lesson("A record that will be forgotten."))
        .expect("write");
    store.forget_memory(doomed).expect("forget");

    assert_eq!(
        store.memory_counts().expect("counts"),
        (1, 1),
        "the writes must actually have done work",
    );
    assert_eq!(
        serde_json::to_vec(&store.export_factset().expect("export")).expect("serialize"),
        before_bytes,
        "export_factset must be byte-identical across memory writes",
    );
    assert_eq!(
        serde_json::to_vec(&GraphArtifact::from_store(&store).expect("artifact")).expect("bytes"),
        before_artifact,
        "the published GraphArtifact must be byte-identical across memory writes",
    );
    assert_eq!(store.node_count().expect("nodes"), before_nodes);
    assert_eq!(store.edge_count().expect("edges"), before_edges);

    // And nothing leaked sideways into a node's meta on the way.
    for node in store.all_nodes().expect("nodes") {
        assert!(
            !node.meta.to_string().contains("pelican migration"),
            "{}: a memory body leaked into a node's meta",
            node.key,
        );
    }
}

/// **Nothing enters `nodes`/`edges`, and no provenance class is borrowed.**
///
/// The rejected option that would have "worked" is `NodeKind::Other("memory")`:
/// it compiles, and it inherits `provenance DEFAULT 'derived'` — a fact about
/// source, asserted about something that was never in the source. This is the
/// test that a memory write produces no node under any provenance.
#[test]
fn a_memory_record_is_not_a_node_under_any_provenance() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);
    let before: Vec<(Provenance, usize)> = [
        Provenance::Derived,
        Provenance::Authored,
        Provenance::Inferred,
    ]
    .into_iter()
    .map(|p| (p, store.nodes_by_provenance(p).expect("nodes").len()))
    .collect();

    store
        .record_memory(&MemoryWrite {
            anchor: Some("sym:rust:src/migrate.rs#run"),
            ..lesson(LESSON)
        })
        .expect("write");

    for (provenance, count) in before {
        assert_eq!(
            store.nodes_by_provenance(provenance).expect("nodes").len(),
            count,
            "a memory write must not add a {provenance:?} node",
        );
    }
    assert!(
        store
            .all_keys()
            .expect("keys")
            .iter()
            .all(|k| !k.contains("memory") && !k.contains("mem:")),
        "no node key may be minted for a memory record",
    );
    // The anchor is a *reference* to a node, not an edge to one.
    assert!(
        store
            .all_edges()
            .expect("edges")
            .iter()
            .all(|e| e.kind != EdgeKind::Supersedes),
        "supersession stays inside the artifact store and never becomes an edge",
    );
}

/// **Memory does not enter `search` at all, through any channel.**
///
/// Later stages may give recall its own visually distinct channel and its own
/// score. What must never happen — and what this test pins for this stage — is
/// unreviewed accumulated prose riding the `authored` +40 boost that `search`
/// reserves for intent someone deliberately wrote into a reviewed file. The
/// strongest form of that guarantee is total absence, so that is what is checked:
/// both channels, with the opt-in for generated content turned *on*, so a memory
/// record cannot quietly arrive through the door ADR-0015 opened for transcripts.
#[test]
fn memory_never_reaches_search_or_the_authored_boost() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);
    store
        .record_memory(&MemoryWrite {
            anchor: Some("sym:rust:src/migrate.rs#run"),
            ..lesson(LESSON)
        })
        .expect("write");

    // Words unmistakably from the memory body, not from the graph.
    for query in [
        "pelican migration",
        "retry loop double-counted",
        "dedup key",
    ] {
        assert!(
            search(&store, query, 10).expect("search").is_empty(),
            "a memory body reached default search via {query:?}",
        );
        let channels = search_channels(
            &store,
            query,
            SearchOptions {
                limit: 10,
                // Opted in to *generated* content, which memory is not: a record
                // must not arrive through another store's channel either.
                include_generated: true,
            },
        )
        .expect("search");
        assert!(
            channels.hits.is_empty(),
            "{query:?} reached the graph channel"
        );
        assert!(
            channels.generated.is_empty(),
            "{query:?} reached the generated-content channel",
        );
    }

    // The graph itself still searches normally — the absence above is memory's,
    // not a broken index.
    assert!(
        !search(&store, "agent memory", 10)
            .expect("search")
            .is_empty(),
        "the authored ADR must still be findable",
    );
}

// --- Durability: the `imports` property ---------------------------------------

/// **Memory survives `rebuild`.** The whole reason `imports` is persisted is that
/// `sync`'s rebuild would otherwise destroy knowledge with no generating
/// function; memory is the same case, and stronger — an import can at least be
/// re-imported from its source, while a lesson cannot be re-learned from
/// anything. A rebuild that wiped it would be an irreversible loss triggered by
/// an ordinary code change.
#[test]
fn memory_survives_a_graph_rebuild_and_a_reconcile() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);
    let id = store
        .record_memory(&MemoryWrite {
            anchor: Some("sym:rust:src/migrate.rs#run"),
            ..lesson(LESSON)
        })
        .expect("write");

    // A code-changing sync: the whole graph is replaced.
    let mut facts = FactSet::new();
    facts.nodes = vec![Node::new(
        "sym:rust:src/other.rs#helper",
        NodeKind::Fn,
        "helper",
    )];
    store.rebuild(&facts, Some("treexyz")).expect("rebuild");

    let record = store
        .memory_record(id)
        .expect("get")
        .expect("the record must survive a rebuild");
    assert_eq!(record.body, LESSON);
    assert_eq!(
        record.anchor.as_ref().map(|a| a.key.as_str()),
        Some("sym:rust:src/migrate.rs#run"),
        "the anchor is preserved verbatim, even though its node is gone",
    );

    // …and the incremental path too, which writes only the delta.
    store.reconcile(&facts, Some("treexyz")).expect("reconcile");
    assert_eq!(store.memory_counts().expect("counts"), (1, 0));
}

// --- Anchoring and drift ------------------------------------------------------

/// **The three anchor verdicts, on one record each.** Valid, drifted and
/// vanished are decided from `(anchor_key, anchor_blob)` against the graph as it
/// stands, on every read — nothing is stored, so nothing can go stale.
#[test]
fn anchor_state_distinguishes_valid_drifted_and_vanished() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);

    let anchored = store
        .record_memory(&MemoryWrite {
            anchor: Some("sym:rust:src/migrate.rs#run"),
            ..lesson(LESSON)
        })
        .expect("write");
    // An anchor to a node that is not in the graph at all: accepted, not refused.
    let ghost = store
        .record_memory(&MemoryWrite {
            anchor: Some("sym:rust:src/gone.rs#removed"),
            ..lesson("This function was deleted for a reason; do not resurrect it.")
        })
        .expect("write");
    let free = store
        .record_memory(&lesson("A general lesson"))
        .expect("write");

    assert_eq!(state(&store, anchored), AnchorState::Valid);
    assert_eq!(
        state(&store, ghost),
        AnchorState::Vanished,
        "an anchor key naming no node is recorded and reads as vanished",
    );
    assert_eq!(state(&store, free), AnchorState::Unanchored);
    assert!(!state(&store, anchored).is_stale() && state(&store, ghost).is_stale());

    // Recompile the anchored symbol: same key, different blob.
    seed_graph_with_changed_blob(&mut store);
    assert_eq!(
        state(&store, anchored),
        AnchorState::Drifted,
        "a differing blob means the code changed underneath the record",
    );
    assert!(state(&store, anchored).is_stale());
    // The captured evidence is unchanged — it is what makes the comparison mean
    // something, so a read must not quietly refresh it.
    let record = store
        .memory_record(anchored)
        .expect("get")
        .expect("present");
    assert_eq!(
        record.anchor.expect("anchored").blob.as_deref(),
        Some("blob-migrate-v1"),
    );
}

/// **An unanchored record is kept and marked, never pruned.** The authored layer
/// drops links to vanished symbols. Memory must not: a lesson about deleted code
/// is often the most valuable thing in the store, and this is the deliberate
/// departure from the house rule.
///
/// Checked through the operations that *would* be the pruning seams — a rebuild
/// that removes the node, and a context refresh, which prunes cached context for
/// deleted nodes and must leave memory alone.
#[test]
fn a_record_whose_anchor_vanished_is_kept_and_marked_not_pruned() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);
    let id = store
        .record_memory(&MemoryWrite {
            anchor: Some("sym:rust:src/migrate.rs#run"),
            kind: MemoryKind::Attempt,
            ..lesson(LESSON)
        })
        .expect("write");
    assert_eq!(
        store
            .memory_record(id)
            .expect("get")
            .expect("present")
            .anchor_state,
        AnchorState::Valid,
    );

    // Delete the anchored symbol from the graph entirely.
    let mut facts = store.export_factset().expect("export");
    facts
        .nodes
        .retain(|n| n.key != "sym:rust:src/migrate.rs#run");
    store.rebuild(&facts, Some("treedel")).expect("rebuild");
    // The seam that prunes cached context for deleted nodes must not touch this.
    rto_graph::refresh_contexts(&store).expect("refresh");

    let record = store
        .memory_record(id)
        .expect("get")
        .expect("a record about deleted code is the point, not the problem");
    assert_eq!(record.anchor_state, AnchorState::Vanished);
    assert_eq!(record.body, LESSON);
    assert_eq!(
        live(&store).len(),
        1,
        "and it is still returned by a live listing, marked rather than hidden",
    );
    assert_eq!(store.memory_counts().expect("counts"), (1, 0));
}

// --- The scope rule: the anchor decides applicability -------------------------

/// **Anchor resolution — and nothing else — decides where a record applies.**
///
/// The rule is: *a lesson learned on a feature branch is valid on `main` only if
/// the relevant association is merged to `main` in the same format*. This is the
/// test that the implementation really is that, and not a branch label wearing a
/// disguise.
///
/// **One row, unchanged, read against two trees.** Nothing about the record moves
/// between the two reads — same id, same body, same captured anchor, same
/// `created_at`, same scope, same insertion order. Only the tree changes, and the
/// verdict flips with it. So the verdict cannot be a property of the record, of
/// when it was written, or of where; it can only be a property of the tree it was
/// resolved against, which is exactly the claim.
///
/// The tree it is *not* applicable to is reached by changing the anchored blob and
/// nothing else — the "merged in a different format" case, which the rule says is
/// not merged at all.
#[test]
fn applicability_is_decided_by_the_anchor_and_by_nothing_about_the_record() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);
    let id = store
        .record_memory(&MemoryWrite {
            anchor: Some("sym:rust:src/migrate.rs#run"),
            ..lesson(LESSON)
        })
        .expect("write");

    // Tree A — the association is present, in the same format.
    let in_tree_a = store.memory_record(id).expect("get").expect("present");
    assert!(
        in_tree_a.applies,
        "the anchor resolves here with the same blob"
    );
    assert_eq!(in_tree_a.anchor_state, AnchorState::Valid);

    // Tree B — the same node, recompiled to a different blob. "Merged in a
    // different format" is not merged, and the rule is deliberately this strict:
    // even a pure reformat breaks the association, so the failure is toward
    // *marked*, never toward silently applying a lesson to code that has moved.
    seed_graph_with_changed_blob(&mut store);
    let in_tree_b = store.memory_record(id).expect("get").expect("present");
    assert!(
        !in_tree_b.applies,
        "the association is not in this tree in the same format",
    );
    assert_eq!(in_tree_b.anchor_state, AnchorState::Drifted);

    // Everything the record itself carries is identical across the two reads.
    // This is the load-bearing half: it is what proves the flip came from the
    // tree and not from anything about the record — not its age, not its scope,
    // not its position in the sequence.
    assert_eq!(in_tree_a.id, in_tree_b.id);
    assert_eq!(in_tree_a.body, in_tree_b.body);
    assert_eq!(in_tree_a.scope, in_tree_b.scope);
    assert_eq!(in_tree_a.created_at, in_tree_b.created_at);
    assert_eq!(
        in_tree_a.anchor, in_tree_b.anchor,
        "the captured evidence is untouched"
    );
    assert_eq!(in_tree_a.superseded_by, in_tree_b.superseded_by);

    // And it is still stored, still live, still listed. Not applying to a tree is
    // not a reason to lose it: put the old blob back and it applies again, which
    // is the "merged to main" direction of the same rule.
    assert_eq!(store.memory_counts().expect("counts"), (1, 0));
    assert_eq!(live(&store).len(), 1);
    seed_graph(&mut store);
    assert!(
        store
            .memory_record(id)
            .expect("get")
            .expect("present")
            .applies,
        "the record applies again as soon as the association is back in this form",
    );
}

/// **A record with no anchor is repo-wide; a record whose anchor failed to
/// resolve is not.** Two states that both lack a usable anchor, with *opposite*
/// answers — conflating them is the mistake that would make the scope rule
/// meaningless, so they are separate values and this is the test that keeps them
/// apart.
#[test]
fn no_anchor_applies_everywhere_and_is_never_confused_with_a_failed_one() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);

    let general = store
        .record_memory(&lesson("CI is Ubuntu-only; do not assume a macOS runner."))
        .expect("write");
    let ghost = store
        .record_memory(&MemoryWrite {
            anchor: Some("sym:rust:src/gone.rs#removed"),
            ..lesson("This function was deleted on purpose.")
        })
        .expect("write");

    let general_record = store.memory_record(general).expect("get").expect("present");
    let ghost_record = store.memory_record(ghost).expect("get").expect("present");

    assert_eq!(general_record.anchor_state, AnchorState::Unanchored);
    assert!(
        general_record.applies,
        "a general lesson never claimed to be about particular code, so no tree \
         can disagree with it",
    );
    assert!(general_record.anchor.is_none());

    assert_eq!(ghost_record.anchor_state, AnchorState::Vanished);
    assert!(
        !ghost_record.applies,
        "an anchor that failed to resolve is the opposite case and must not be \
         rounded up to repo-wide",
    );
    assert!(
        ghost_record.anchor.is_some(),
        "the failed anchor is still on record"
    );

    // The states are distinct in the serialised contract too, so a consumer
    // reading only JSON cannot merge them either.
    let json = serde_json::to_value(&general_record).expect("json");
    assert_eq!(json["anchor_state"], "unanchored");
    assert_eq!(json["applies"], true);
    assert!(json.get("anchor").is_none(), "no anchor key at all");
    let json = serde_json::to_value(&ghost_record).expect("json");
    assert_eq!(json["anchor_state"], "vanished");
    assert_eq!(json["applies"], false);
    assert_eq!(json["anchor"]["key"], "sym:rust:src/gone.rs#removed");

    // A general lesson survives the tree being replaced wholesale — there is no
    // tree it can fail against.
    store
        .rebuild(&FactSet::new(), Some("treeempty"))
        .expect("rebuild");
    assert!(
        store
            .memory_record(general)
            .expect("get")
            .expect("present")
            .applies,
        "a repo-wide lesson applies even to an empty tree",
    );
}

/// The applicability verdict per state, said once and directly, so the rule is
/// pinned independently of any record or store.
#[test]
fn the_applicability_rule_is_exactly_unanchored_or_valid() {
    for state in [AnchorState::Unanchored, AnchorState::Valid] {
        assert!(state.applies(), "{state} must apply");
    }
    for state in [
        AnchorState::Drifted,
        AnchorState::Vanished,
        // Fails closed: "the same format" cannot be demonstrated when the blob
        // cannot be compared, and silently applying beats being marked only if
        // you would rather be wrong quietly.
        AnchorState::Unverifiable,
    ] {
        assert!(!state.applies(), "{state} must not apply");
    }
    // Staleness is narrower than not-applying: `Unverifiable` makes no claim that
    // the code moved, because nothing was measured.
    assert!(!AnchorState::Unverifiable.is_stale());
    assert!(AnchorState::Drifted.is_stale() && AnchorState::Vanished.is_stale());
}

/// A node present but carrying no blob hash cannot be checked for drift, and says
/// so. Rounding this up to `Valid` would claim a comparison that never happened —
/// the same species of lie as reporting an ungenerated media record as empty text.
#[test]
fn an_anchor_with_no_blob_is_unverifiable_rather_than_valid() {
    let mut store = Store::open_in_memory().expect("store");
    let mut facts = FactSet::new();
    // No `blob_hash` on this one.
    facts.nodes = vec![Node::new("sym:rust:src/lib.rs#main", NodeKind::Fn, "main")];
    store.rebuild(&facts, Some("tree0")).expect("rebuild");

    let id = store
        .record_memory(&MemoryWrite {
            anchor: Some("sym:rust:src/lib.rs#main"),
            ..lesson(LESSON)
        })
        .expect("write");
    let record = store.memory_record(id).expect("get").expect("present");
    assert_eq!(record.anchor_state, AnchorState::Unverifiable);
    assert!(
        !record.anchor_state.is_stale(),
        "unverifiable is not drift: nothing was measured either way",
    );
    // …and it does not apply, because "present in the same format" cannot be
    // shown when the blob cannot be compared. Distinct from a *repo-wide* record,
    // which does apply: this one anchored to something and the check came back
    // inconclusive, which is not the same as never having anchored at all.
    assert!(
        !record.applies,
        "an unmeasurable anchor cannot establish that the association is here",
    );
    assert!(record.anchor.is_some());
}

// --- Supersession -------------------------------------------------------------

/// **A superseded record leaves live listing immediately, and the chain remains
/// auditable.** Immediately means *on the strength of the recorded pointer* — the
/// superseded record here is written in the same test-second as its successor, so
/// no clock could separate them and none is consulted.
#[test]
fn a_superseded_record_leaves_live_listing_but_stays_on_record() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);

    let old = store
        .record_memory(&lesson("Batch size 500 is safe."))
        .expect("write");
    let new = store
        .record_memory(&MemoryWrite {
            supersedes: Some(old),
            ..lesson("Batch size 500 deadlocks under contention; 100 is safe.")
        })
        .expect("write");

    let live_ids: Vec<i64> = live(&store).iter().map(|r| r.id).collect();
    assert_eq!(live_ids, vec![new], "only the successor is live");
    assert_eq!(store.memory_counts().expect("counts"), (1, 1));

    // The chain is auditable: the superseded record is still there, still says
    // what it said, and names its successor and when.
    let audited = store
        .memory_records(&MemoryFilter {
            include_superseded: true,
            ..MemoryFilter::default()
        })
        .expect("records");
    assert_eq!(
        audited.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![new, old],
        "newest generation first, by id and not by clock",
    );
    let overruled = audited.iter().find(|r| r.id == old).expect("still stored");
    assert_eq!(overruled.superseded_by, Some(new));
    assert!(
        overruled.superseded_at.is_some(),
        "the moment is recorded too"
    );
    assert!(!overruled.is_live());
    assert_eq!(overruled.body, "Batch size 500 is safe.");

    // Re-pointing an already-superseded record is refused: that would orphan the
    // successor already on record and turn a chain into a guess.
    let err = store
        .record_memory(&MemoryWrite {
            supersedes: Some(old),
            ..lesson("A third opinion.")
        })
        .expect_err("already superseded");
    assert!(
        matches!(err, rto_graph::MemoryError::AlreadySuperseded { id, by } if id == old && by == new),
        "{err}",
    );
    // A supersession target that does not exist is refused, and writes nothing.
    let before = store.memory_counts().expect("counts");
    assert!(matches!(
        store
            .record_memory(&MemoryWrite {
                supersedes: Some(9999),
                ..lesson("Overruling a record that is not there.")
            })
            .expect_err("no such record"),
        rto_graph::MemoryError::NotFound(9999),
    ));
    assert_eq!(store.memory_counts().expect("counts"), before);
}

/// Forgetting a successor restores what it had superseded. Leaving the
/// predecessor hidden would be supersession by a record that no longer exists —
/// exactly the inferred-not-recorded failure the explicit pointer prevents.
#[test]
fn forgetting_a_successor_restores_what_it_superseded() {
    let mut store = Store::open_in_memory().expect("store");
    let old = store.record_memory(&lesson("The original")).expect("write");
    let new = store
        .record_memory(&MemoryWrite {
            supersedes: Some(old),
            ..lesson("The correction")
        })
        .expect("write");

    let forgotten = store
        .forget_memory(new)
        .expect("forget")
        .expect("the record was there");
    assert_eq!(forgotten.id, new);
    assert_eq!(forgotten.restored, vec![old]);
    assert_eq!(
        live(&store).iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![old],
        "the predecessor is live again",
    );
    let record = store.memory_record(old).expect("get").expect("present");
    assert_eq!(record.superseded_by, None);
    assert_eq!(record.superseded_at, None, "both columns clear together");

    // Forgetting is the only removal path, and it reports an unknown id honestly.
    assert!(store.forget_memory(new).expect("forget").is_none());
}

// --- Ordering -----------------------------------------------------------------

/// **The ordering key is the generation, and a forgotten id is never reused.**
///
/// This is why `AUTOINCREMENT` and not the plain rowid: without it, forgetting
/// the newest record hands its number to the next write, so `ORDER BY id DESC`
/// stops being newest-first and a surviving `superseded_by` silently re-points at
/// an unrelated record. No clock could substitute — these writes all land in the
/// same `datetime('now')` second, which is exactly the tie ADR-0013 refuses to
/// rank on.
#[test]
fn ordering_is_a_monotonic_generation_that_never_reuses_an_id() {
    let mut store = Store::open_in_memory().expect("store");
    let first = store.record_memory(&lesson("first")).expect("write");
    let second = store.record_memory(&lesson("second")).expect("write");
    assert!(second > first, "ids are monotonic");

    store.forget_memory(second).expect("forget");
    let third = store.record_memory(&lesson("third")).expect("write");
    assert!(
        third > second,
        "a forgotten id must never be handed out again: {third} <= {second}",
    );

    assert_eq!(
        live(&store)
            .iter()
            .map(|r| r.body.clone())
            .collect::<Vec<_>>(),
        vec!["third".to_owned(), "first".to_owned()],
        "listing is newest generation first",
    );
    // All three were written within the same second, so the timestamps tie — the
    // ordering above cannot have come from them.
    let stamps: Vec<String> = live(&store).iter().map(|r| r.created_at.clone()).collect();
    assert!(
        stamps.iter().all(|s| !s.is_empty()),
        "created_at is written for humans",
    );
}

// --- Filtering, scope and the listing contract --------------------------------

/// **`scope` is a namespace, and it decides nothing about applicability.**
///
/// It names which repo or project a record belongs to in a multi-repo workspace,
/// and it is matched exactly — no isolation, no inheritance, no merging. The
/// question it does *not* answer is whether a record applies to the tree in front
/// of you: that is the anchor's job (see
/// `applicability_is_decided_by_the_anchor_and_by_nothing_about_the_record`), and
/// this test pins the negative half, so nobody later repurposes the column as a
/// branch label and gets two answers to one question.
#[test]
fn scope_is_a_namespace_and_never_decides_applicability() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);
    store
        .record_memory(&lesson("a repo-wide lesson"))
        .expect("write");
    // A scope that *looks* exactly like a branch name, anchored to code that is
    // present in this tree. If scope were a branch label this record would be
    // out of scope here; it is not, because scope is not that.
    let branchy = store
        .record_memory(&MemoryWrite {
            scope: "feat/some-other-branch",
            kind: MemoryKind::Decision,
            anchor: Some("sym:rust:src/migrate.rs#run"),
            ..lesson("a decision taken on a branch, about code that is here")
        })
        .expect("write");
    assert!(
        store
            .memory_record(branchy)
            .expect("get")
            .expect("present")
            .applies,
        "a branch-shaped scope must not stop a resolving anchor from applying",
    );

    let scoped = store
        .memory_records(&MemoryFilter {
            scope: Some("feat/some-other-branch"),
            ..MemoryFilter::default()
        })
        .expect("records");
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].scope, "feat/some-other-branch");
    assert_eq!(scoped[0].kind, MemoryKind::Decision);

    // And the scope does not hide it from an unfiltered listing either: the store
    // is shared, and sharing is the point.
    assert_eq!(live(&store).len(), 2);
    assert_eq!(
        store
            .memory_records(&MemoryFilter {
                kind: Some(MemoryKind::Lesson),
                ..MemoryFilter::default()
            })
            .expect("records")
            .len(),
        1,
    );
    // An unknown scope matches nothing rather than falling back to everything.
    assert!(
        store
            .memory_records(&MemoryFilter {
                scope: Some("nope"),
                ..MemoryFilter::default()
            })
            .expect("records")
            .is_empty(),
    );
}

/// A listing carries the whole store's counts, so an empty filtered result is
/// legible as *nothing matched* rather than *nothing is stored* — the same
/// distinction `media status` exists to make.
#[test]
fn a_listing_reports_the_counts_that_make_an_empty_result_legible() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);
    let old = store.record_memory(&lesson("old")).expect("write");
    store
        .record_memory(&MemoryWrite {
            supersedes: Some(old),
            ..lesson("new")
        })
        .expect("write");
    store
        .record_memory(&MemoryWrite {
            anchor: Some("sym:rust:src/migrate.rs#run"),
            ..lesson(LESSON)
        })
        .expect("write");

    let listing = store
        .memory_listing(&MemoryFilter {
            scope: Some("nothing-here"),
            ..MemoryFilter::default()
        })
        .expect("listing");
    assert!(listing.records.is_empty());
    assert_eq!((listing.live, listing.superseded), (2, 1));
    assert_eq!(listing.schema, rto_graph::MEMORY_SCHEMA);

    // The JSON contract a programmatic consumer depends on.
    let listing = store
        .memory_listing(&MemoryFilter::default())
        .expect("listing");
    let json = serde_json::to_value(&listing).expect("json");
    assert_eq!(json["schema"], "roteiro.memory/v1");
    assert_eq!(json["live"], 2);
    assert_eq!(json["superseded"], 1);
    let newest = &json["records"][0];
    assert_eq!(newest["kind"], "lesson");
    assert_eq!(newest["anchor_state"], "valid");
    assert_eq!(
        newest["applies"], true,
        "the scope rule is in the JSON, so a consumer need not re-derive it",
    );
    assert_eq!(newest["anchor"]["key"], "sym:rust:src/migrate.rs#run");
    assert_eq!(newest["anchor"]["blob"], "blob-migrate-v1");
    assert_eq!(newest["superseded_by"], serde_json::Value::Null);
    assert_eq!(
        newest["tree"], "treeabc",
        "the repo-state witness is recorded"
    );
}

/// `limit` returns the newest generations, not an arbitrary slice.
#[test]
fn a_limit_returns_the_newest_generations() {
    let mut store = Store::open_in_memory().expect("store");
    for i in 0..5 {
        store
            .record_memory(&lesson(&format!("lesson {i}")))
            .expect("write");
    }
    let limited = store
        .memory_records(&MemoryFilter {
            limit: Some(2),
            ..MemoryFilter::default()
        })
        .expect("records");
    assert_eq!(
        limited.iter().map(|r| r.body.as_str()).collect::<Vec<_>>(),
        vec!["lesson 4", "lesson 3"],
    );
}

/// A record survives a round trip through `SQLite` with every field intact —
/// including the ones nothing reads, because "written for humans" is still a
/// promise about what is stored.
#[test]
fn a_record_round_trips_every_field() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);
    let id = store
        .record_memory(&MemoryWrite {
            scope: "feat/stage23",
            kind: MemoryKind::Pattern,
            anchor: Some("sym:rust:src/migrate.rs#run"),
            body: LESSON,
            confidence: Some(0.75),
            supersedes: None,
        })
        .expect("write");

    let record = store.memory_record(id).expect("get").expect("present");
    assert_eq!(record.id, id);
    assert_eq!(record.scope, "feat/stage23");
    assert_eq!(record.kind, MemoryKind::Pattern);
    assert_eq!(record.body, LESSON);
    assert_eq!(record.confidence, Some(0.75));
    assert_eq!(record.tree.as_deref(), Some("treeabc"));
    assert!(!record.created_at.is_empty());
    let anchor = record.anchor.expect("anchored");
    assert_eq!(anchor.key, "sym:rust:src/migrate.rs#run");
    assert_eq!(anchor.blob.as_deref(), Some("blob-migrate-v1"));
    assert_eq!(anchor.path.as_deref(), Some("src/migrate.rs"));
}

/// A record can be written before anything has ever been synced — there is no
/// tree to witness, and that is a legitimate state rather than an error.
#[test]
fn a_memory_can_be_recorded_before_the_first_sync() {
    let mut store = Store::open_in_memory().expect("store");
    let id = store.record_memory(&lesson(LESSON)).expect("write");
    let record = store.memory_record(id).expect("get").expect("present");
    assert_eq!(record.tree, None);
    assert_eq!(record.anchor_state, AnchorState::Unanchored);
}

/// Validation refuses what could never be recalled, and nothing is written on the
/// way out.
#[test]
fn an_invalid_write_stores_nothing() {
    let mut store = Store::open_in_memory().expect("store");
    for bad in [
        MemoryWrite { ..lesson("") },
        MemoryWrite {
            scope: "",
            ..lesson("body")
        },
        MemoryWrite {
            confidence: Some(1.5),
            ..lesson("body")
        },
    ] {
        assert!(store.record_memory(&bad).is_err());
    }
    assert_eq!(store.memory_counts().expect("counts"), (0, 0));
}

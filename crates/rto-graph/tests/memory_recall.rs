//! Ranked recall over episodic memory: the retrieval-time score, decay,
//! supersession, and anchor drift (ADR-0013; build plan Stage 25).
//!
//! Stage 23 shipped the write path and asserted that nothing an agent remembers
//! reaches the graph. This file asserts the other half — that what is remembered
//! can be *found again*, on terms that do not quietly change underneath the
//! reader:
//!
//! - **`decay = none` is byte-identical across runs.** The score is computed at
//!   retrieval and stored nowhere, so recall over an unchanged store and an
//!   unchanged tree is the same answer every time. A stored decaying score would
//!   rewrite the store on every read and make recall depend on when you last
//!   looked.
//! - **Supersession beats age, immediately.** A superseded record leaves recall
//!   the moment its successor is written — newest, highest-confidence, perfectly
//!   anchored, and still absent.
//! - **Drift demotes and never deletes.** A record whose anchor vanished still
//!   comes back, ranked lower and labelled.
//!
//! **None of them needs a model, a GPU or a network.**

use rto_graph::{
    AnchorState, DEFAULT_BASE_CONFIDENCE, DEFAULT_MEMORY_SCOPE, Decay, FactSet, MemoryKind,
    MemoryWrite, Node, NodeKind, Recall, RecallOptions, Store, anchor_penalty,
};

/// A graph with one anchorable symbol carrying a blob hash.
fn seed_graph(store: &mut Store) {
    let mut facts = FactSet::new();
    let mut migrate = Node::new("sym:rust:src/migrate.rs#run", NodeKind::Fn, "run");
    migrate.path = Some("src/migrate.rs".into());
    migrate.blob_hash = Some("blob-migrate-v1".into());
    let mut other = Node::new("sym:rust:src/lib.rs#main", NodeKind::Fn, "main");
    other.path = Some("src/lib.rs".into());
    other.blob_hash = Some("blob-main-v1".into());
    facts.nodes = vec![migrate, other];
    store.rebuild(&facts, Some("treeabc")).expect("rebuild");
}

/// The same graph with `run` recompiled — the drift case, node still present.
fn drift_the_anchor(store: &mut Store) {
    let mut facts = store.export_factset().expect("export");
    for node in &mut facts.nodes {
        if node.key == "sym:rust:src/migrate.rs#run" {
            node.blob_hash = Some("blob-migrate-v2".into());
        }
    }
    store.rebuild(&facts, Some("treedef")).expect("rebuild");
}

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

/// A record with a stated confidence, so that the only thing left to order a set
/// of them by is the anchor.
fn confident<'a>(anchor: Option<&'a str>, body: &'a str) -> MemoryWrite<'a> {
    MemoryWrite {
        anchor,
        confidence: Some(0.8),
        ..lesson(body)
    }
}

/// Every live record, ranked, with no age term — the reproducible default.
fn recall(store: &Store) -> Recall {
    store
        .recall_memory(&RecallOptions::default())
        .expect("recall")
}

/// The bodies a recall returned, best first.
fn bodies(recall: &Recall) -> Vec<&str> {
    recall
        .results
        .iter()
        .map(|r| r.record.body.as_str())
        .collect()
}

// --- Reproducibility: the property `decay = none` exists to give --------------

/// **`decay = none` gives byte-identical recall for a fixed repo state across
/// runs.** The build plan's first definition-of-done item, checked the only way that means
/// anything: serialise the whole result, close the store, reopen it from disk in
/// a separate session, recall again, and compare the *bytes*.
///
/// Field-by-field comparison would not do. A score that had been persisted and
/// re-read as a slightly different float, or a result order that depended on a
/// hash map's iteration, would both survive a structural check and both break the
/// promise a consumer actually depends on: that the same question over the same
/// store gets the same answer, byte for byte.
#[test]
fn decay_none_recalls_byte_identically_across_runs() {
    let path = std::env::temp_dir().join(format!(
        "roteiro-recall-repro-{}-{:?}.db",
        std::process::id(),
        std::thread::current().id(),
    ));
    std::fs::remove_file(&path).ok();

    let first = {
        let mut store = Store::open(&path).expect("open");
        seed_graph(&mut store);
        store
            .record_memory(&MemoryWrite {
                anchor: Some("sym:rust:src/migrate.rs#run"),
                confidence: Some(0.9),
                ..lesson("The retry loop double-counted partial batches.")
            })
            .expect("write");
        store
            .record_memory(&lesson("CI is Ubuntu-only."))
            .expect("write");
        store
            .record_memory(&MemoryWrite {
                anchor: Some("sym:rust:src/gone.rs#dropped"),
                ..lesson("Removed because the batch cursor had no dedup key.")
            })
            .expect("write");
        // Twice within one session, first of all. Compared as `String` rather
        // than `Vec<u8>` — the same bytes either way, since a Rust `String` is
        // UTF-8 and compares bytewise, but a failure prints the JSON instead of
        // two thousand byte codes.
        let once = serde_json::to_string(&recall(&store)).expect("serialize");
        let twice = serde_json::to_string(&recall(&store)).expect("serialize");
        assert_eq!(once, twice, "recall is not even stable within one session");
        once
    };

    // A separate session over the same store on disk: a new connection, a new
    // process-worth of state, nothing carried over but the file.
    let second = {
        let store = Store::open(&path).expect("reopen");
        serde_json::to_string(&recall(&store)).expect("serialize")
    };
    assert_eq!(
        first, second,
        "decay = none must recall byte-identically across runs",
    );

    // And the guarantee is *claimed* in the payload, not only observed here.
    let store = Store::open(&path).expect("reopen");
    let result = recall(&store);
    assert!(result.reproducible, "decay = none reports itself so");
    assert_eq!(result.decay, Decay::None, "and it is the default");
    drop(store);
    std::fs::remove_file(&path).expect("cleanup");
}

/// **Recall mutates nothing.** The reproducibility above is only worth having if
/// reading does not move the thing being read: no hit counter on the episodic
/// tier, no touched timestamp, no stored score. Checked by recalling repeatedly
/// — with every decay mode, since a mode that wrote a score would do it on the
/// way past — and finding the store's own listing byte-identical afterwards.
#[test]
fn recall_never_writes_to_the_store() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);
    for body in ["first lesson", "second lesson", "third lesson"] {
        store.record_memory(&lesson(body)).expect("write");
    }
    let listing_before = serde_json::to_vec(
        &store
            .memory_listing(&rto_graph::MemoryFilter::default())
            .expect("listing"),
    )
    .expect("serialize");
    let counts_before = store.memory_counts().expect("counts");

    for decay in [
        Decay::None,
        Decay::Linear { span: 2 },
        Decay::Exponential { half_life: 1 },
    ] {
        for _ in 0..3 {
            store
                .recall_memory(&RecallOptions {
                    decay,
                    ..RecallOptions::default()
                })
                .expect("recall");
        }
    }

    assert_eq!(
        serde_json::to_vec(
            &store
                .memory_listing(&rto_graph::MemoryFilter::default())
                .expect("listing")
        )
        .expect("serialize"),
        listing_before,
        "a read moved the store",
    );
    assert_eq!(store.memory_counts().expect("counts"), counts_before);
}

// --- Supersession: evidence beats age, immediately ---------------------------

/// **A superseded memory drops out of recall immediately, regardless of age.**
///
/// The record is rigged to win on every other term there is: it is perfectly
/// anchored (`anchor_penalty` 1.0), states the maximum confidence, and — under
/// `decay = none` — carries no age discount at all. If supersession were priced
/// as a demotion rather than a departure, it would still be first. It is absent,
/// under every decay mode, because the test is a recorded pointer with no clock
/// in it.
#[test]
fn a_superseded_memory_leaves_recall_immediately_regardless_of_age() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);
    let overruled = store
        .record_memory(&MemoryWrite {
            anchor: Some("sym:rust:src/migrate.rs#run"),
            confidence: Some(1.0),
            ..lesson("Batch the writes: it is measurably faster.")
        })
        .expect("write");
    // Deliberately weaker on every term: unanchored, and no stated confidence.
    store
        .record_memory(&MemoryWrite {
            supersedes: Some(overruled),
            ..lesson("Batching the writes lost the ordering guarantee. Do not.")
        })
        .expect("write");

    for decay in [
        Decay::None,
        Decay::Linear { span: 1_000 },
        Decay::Exponential { half_life: 1 },
    ] {
        let result = store
            .recall_memory(&RecallOptions {
                decay,
                ..RecallOptions::default()
            })
            .expect("recall");
        assert!(
            !bodies(&result)
                .iter()
                .any(|b| b.contains("measurably faster")),
            "the superseded record came back under {decay}",
        );
        assert_eq!(
            bodies(&result),
            vec!["Batching the writes lost the ordering guarantee. Do not."],
            "the successor is what recall returns under {decay}",
        );
        assert_eq!(result.superseded, 1, "and the record is still stored");
    }

    // Not a filter that could be turned off: `RecallOptions` has no
    // include-superseded switch at all. The audit view is `memory list
    // --include-superseded`, which still has it.
    let audited = store
        .memory_records(&rto_graph::MemoryFilter {
            include_superseded: true,
            ..rto_graph::MemoryFilter::default()
        })
        .expect("records");
    assert_eq!(audited.len(), 2, "nothing was deleted, only dropped");
}

/// Forgetting a successor puts what it superseded **back into recall** — the
/// mirror of the write path's restore rule. A record hidden on the authority of
/// one that no longer exists is supersession by ghost.
#[test]
fn forgetting_a_successor_returns_its_predecessor_to_recall() {
    let mut store = Store::open_in_memory().expect("store");
    let overruled = store.record_memory(&lesson("the old finding")).expect("w");
    let successor = store
        .record_memory(&MemoryWrite {
            supersedes: Some(overruled),
            ..lesson("the new finding")
        })
        .expect("write");
    assert_eq!(bodies(&recall(&store)), vec!["the new finding"]);

    store.forget_memory(successor).expect("forget");
    assert_eq!(
        bodies(&recall(&store)),
        vec!["the old finding"],
        "the predecessor must return once its successor is gone",
    );
}

// --- Anchor drift: demote, never delete --------------------------------------

/// **An unanchored memory is still retrievable and clearly labelled.** The build
/// plan's fourth definition-of-done item. A record with no anchor is a general lesson about the
/// repository — it applies everywhere, and must never be confused with an anchor
/// that failed to resolve, which applies nowhere.
#[test]
fn an_unanchored_memory_is_recalled_and_clearly_labelled() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);
    store
        .record_memory(&lesson("CI is Ubuntu-only."))
        .expect("write");
    store
        .record_memory(&MemoryWrite {
            anchor: Some("sym:rust:src/never-existed.rs#nope"),
            ..lesson("A lesson about code that is not here.")
        })
        .expect("write");

    let result = recall(&store);
    assert_eq!(result.results.len(), 2, "both are retrievable");

    let general = result
        .results
        .iter()
        .find(|r| r.record.body.starts_with("CI is"))
        .expect("the unanchored record is recalled");
    assert_eq!(
        general.record.anchor_state,
        AnchorState::Unanchored,
        "labelled as never anchored",
    );
    assert!(
        general.record.applies,
        "a general lesson applies everywhere"
    );
    assert!(general.record.anchor.is_none());

    let failed = result
        .results
        .iter()
        .find(|r| r.record.body.starts_with("A lesson about"))
        .expect("the failed-anchor record is recalled too");
    assert_eq!(
        failed.record.anchor_state,
        AnchorState::Vanished,
        "an anchor that did not resolve is a different label entirely",
    );
    assert!(!failed.record.applies);

    // The two states that both lack a usable anchor rank on opposite sides, so
    // no consumer can round one into the other.
    assert!(
        general.score > failed.score,
        "a repo-wide lesson must outrank one whose anchor did not resolve",
    );
}

/// **Anchor drift demotes, never deletes.** The authored layer prunes links to
/// vanished symbols; memory must not, because a lesson about deleted code is
/// often the most valuable thing in the store.
///
/// All four anchor states are seeded from records that are otherwise *identical*
/// — same kind, same scope, same stated confidence, adjacent generations — so the
/// only thing that can order them is the anchor.
#[test]
fn anchor_drift_demotes_but_never_removes() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);
    store
        .record_memory(&confident(Some("sym:rust:src/migrate.rs#run"), "about run"))
        .expect("write");
    store
        .record_memory(&confident(Some("sym:rust:src/lib.rs#main"), "about main"))
        .expect("write");
    store
        .record_memory(&confident(Some("sym:rust:src/gone.rs#gone"), "about gone"))
        .expect("write");
    store
        .record_memory(&confident(None, "about nothing"))
        .expect("w");

    // `run` is recompiled; `main` is untouched.
    drift_the_anchor(&mut store);

    let result = recall(&store);
    assert_eq!(result.results.len(), 4, "nothing was pruned by the rebuild");
    let by_body = |body: &str| {
        result
            .results
            .iter()
            .find(|r| r.record.body == body)
            .unwrap_or_else(|| panic!("{body} must still be recallable"))
            .clone()
    };
    assert_eq!(
        by_body("about run").record.anchor_state,
        AnchorState::Drifted
    );
    assert_eq!(
        by_body("about main").record.anchor_state,
        AnchorState::Valid
    );
    assert_eq!(
        by_body("about gone").record.anchor_state,
        AnchorState::Vanished
    );
    assert_eq!(
        by_body("about nothing").record.anchor_state,
        AnchorState::Unanchored
    );

    // Ranked, in the order the penalties say — and every one of them present.
    assert_eq!(
        bodies(&result),
        vec!["about main", "about nothing", "about gone", "about run"],
        "valid, then repo-wide, then vanished, then drifted",
    );
    for recalled in &result.results {
        assert!(
            recalled.score > 0.0,
            "{} was silenced rather than demoted",
            recalled.record.body,
        );
    }
}

/// Drift is recomputed on **every** read against the tree in front of you, so the
/// same store recalls differently on two trees without anything being written.
/// That is the scope rule: applicability is a question about the tree, not about
/// the branch a record was written on.
#[test]
fn the_same_record_ranks_differently_on_a_tree_where_its_anchor_resolves() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);
    store
        .record_memory(&MemoryWrite {
            anchor: Some("sym:rust:src/migrate.rs#run"),
            ..lesson("about run")
        })
        .expect("write");
    let before = recall(&store).results[0].clone();
    assert_eq!(before.record.anchor_state, AnchorState::Valid);
    assert!(before.record.applies);

    drift_the_anchor(&mut store);
    let after = recall(&store).results[0].clone();

    assert_eq!(after.record.anchor_state, AnchorState::Drifted);
    assert!(!after.record.applies);
    assert!(after.score < before.score, "drift must cost it something");
    assert_eq!(
        after.record.id, before.record.id,
        "and it is the same record: nothing was rewritten",
    );
}

/// `applicable_only` is **off by default** and is a caller's choice, never the
/// store's. Off, a drifted record is demoted and labelled; on, it is withheld.
#[test]
fn withholding_inapplicable_records_is_opt_in() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);
    store
        .record_memory(&MemoryWrite {
            anchor: Some("sym:rust:src/gone.rs#gone"),
            ..lesson("about gone")
        })
        .expect("write");
    store
        .record_memory(&lesson("about nothing"))
        .expect("write");

    assert!(!RecallOptions::default().applicable_only);
    assert_eq!(recall(&store).results.len(), 2);

    let strict = store
        .recall_memory(&RecallOptions {
            applicable_only: true,
            ..RecallOptions::default()
        })
        .expect("recall");
    assert_eq!(bodies(&strict), vec!["about nothing"]);
}

// --- The score itself ---------------------------------------------------------

/// The score **is** the product of its three reported terms — not a number
/// alongside them. A ranking an agent cannot take apart is one it has to trust,
/// and the point of depreciating by evidence is that the evidence is inspectable.
#[test]
fn the_score_is_exactly_the_product_of_the_terms_it_reports() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);
    store
        .record_memory(&MemoryWrite {
            anchor: Some("sym:rust:src/migrate.rs#run"),
            confidence: Some(0.75),
            ..lesson("stated confidence, valid anchor")
        })
        .expect("write");
    store
        .record_memory(&MemoryWrite {
            anchor: Some("sym:rust:src/gone.rs#gone"),
            ..lesson("no stated confidence, vanished anchor")
        })
        .expect("write");

    for decay in [
        Decay::None,
        Decay::Linear { span: 4 },
        Decay::Exponential { half_life: 2 },
    ] {
        let result = store
            .recall_memory(&RecallOptions {
                decay,
                ..RecallOptions::default()
            })
            .expect("recall");
        for recalled in &result.results {
            let product =
                recalled.base_confidence * recalled.anchor_penalty * recalled.decay_factor;
            assert!(
                (recalled.score - product).abs() < 1e-12,
                "{decay}: {} != product of its terms",
                recalled.record.body,
            );
            assert!((0.0..=1.0).contains(&recalled.score));
            assert!(
                (recalled.anchor_penalty - anchor_penalty(recalled.record.anchor_state)).abs()
                    < 1e-12,
                "the reported penalty is the one the public function gives",
            );
            assert!(
                (recalled.decay_factor - decay.factor(recalled.age)).abs() < 1e-12,
                "the reported factor is the one the public decay gives",
            );
        }
    }

    // A record whose writer stated nothing sits at the midpoint of the range one
    // *could* state — so stating a high confidence promotes and a low one demotes,
    // both relative to silence.
    let result = recall(&store);
    let unstated = result
        .results
        .iter()
        .find(|r| r.record.confidence.is_none())
        .expect("present");
    assert!((unstated.base_confidence - DEFAULT_BASE_CONFIDENCE).abs() < 1e-12);
}

// --- Age is a generation, not a clock -----------------------------------------

/// Age is counted in **generations** — records written since — and the newest
/// record is always age `0`. Nothing here reads a timestamp, which is what makes
/// the ordering skew-proof across the worktrees this store is shared between.
#[test]
fn age_is_measured_in_generations_and_the_newest_record_is_age_zero() {
    let mut store = Store::open_in_memory().expect("store");
    let mut ids = Vec::new();
    for body in ["oldest", "middle", "newest"] {
        ids.push(store.record_memory(&lesson(body)).expect("write"));
    }
    let result = recall(&store);
    assert_eq!(
        result.generation,
        *ids.last().expect("ids"),
        "the generation recall ran at is the newest record's id",
    );
    for recalled in &result.results {
        let expected = u64::try_from(result.generation - recalled.record.id).expect("age");
        assert_eq!(recalled.age, expected, "{}", recalled.record.body);
    }
    let newest = result
        .results
        .iter()
        .find(|r| r.record.body == "newest")
        .expect("present");
    assert_eq!(newest.age, 0);
}

/// Under an age term, a newer record outranks an otherwise identical older one —
/// and under `none` they tie and fall back to newest-generation-first. Same
/// store, same tree, two orderings, nothing written either way.
#[test]
fn decay_reorders_otherwise_identical_records_by_generation() {
    let mut store = Store::open_in_memory().expect("store");
    for body in ["older", "newer"] {
        store
            .record_memory(&MemoryWrite {
                confidence: Some(0.9),
                ..lesson(body)
            })
            .expect("write");
    }

    let flat = recall(&store);
    assert!(
        (flat.results[0].score - flat.results[1].score).abs() < f64::EPSILON,
        "with no age term, identical records score identically",
    );
    assert_eq!(
        bodies(&flat),
        vec!["newer", "older"],
        "and the tie breaks by newest generation, so the order is still total",
    );

    let decayed = store
        .recall_memory(&RecallOptions {
            decay: Decay::Exponential { half_life: 1 },
            ..RecallOptions::default()
        })
        .expect("recall");
    assert!(
        decayed.results[0].score > decayed.results[1].score,
        "an age term must actually separate them",
    );
    assert_eq!(bodies(&decayed), vec!["newer", "older"]);
    assert!(!decayed.reproducible, "and it says it is not reproducible");
}

/// **Decay ranks; it never filters.** A record past a linear span scores zero in
/// the age term and therefore sorts last — and is still returned, still labelled,
/// still readable. Zero relevance is not deletion, and this is the test that
/// stops the two being conflated.
#[test]
fn a_record_decayed_to_zero_is_ranked_last_and_still_returned() {
    let mut store = Store::open_in_memory().expect("store");
    for body in ["ancient", "recent"] {
        store.record_memory(&lesson(body)).expect("write");
    }
    let result = store
        .recall_memory(&RecallOptions {
            decay: Decay::Linear { span: 1 },
            ..RecallOptions::default()
        })
        .expect("recall");

    let ancient = result
        .results
        .iter()
        .find(|r| r.record.body == "ancient")
        .expect("a fully decayed record is still returned");
    assert!(ancient.decay_factor.abs() < f64::EPSILON);
    assert!(ancient.score.abs() < f64::EPSILON);
    assert_eq!(
        bodies(&result),
        vec!["recent", "ancient"],
        "ranked last, not withheld",
    );
}

// --- Filtering ----------------------------------------------------------------

/// A query is a **filter, not a scorer**: the ranking formula has no lexical
/// term, so narrowing the query changes which records come back and never how the
/// ones that do are ranked. The alternative — folding relevance into the score —
/// would make the three reported terms stop explaining it.
#[test]
fn a_query_narrows_the_set_without_touching_the_ranking() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);
    store
        .record_memory(&MemoryWrite {
            anchor: Some("sym:rust:src/migrate.rs#run"),
            ..lesson("The retry loop double-counted partial batches.")
        })
        .expect("write");
    store
        .record_memory(&lesson("CI is Ubuntu-only."))
        .expect("write");

    let all = recall(&store);
    let narrowed = store
        .recall_memory(&RecallOptions {
            query: Some("retry loop"),
            ..RecallOptions::default()
        })
        .expect("recall");
    assert_eq!(narrowed.results.len(), 1);
    let matched = &narrowed.results[0];
    let same_record = all
        .results
        .iter()
        .find(|r| r.record.id == matched.record.id)
        .expect("present in both");
    assert!(
        (matched.score - same_record.score).abs() < f64::EPSILON,
        "a query must not move a score",
    );

    // The anchor is searchable too, so a symbol name recalls what was learned
    // about it — but every token must match, so a query still narrows.
    assert_eq!(
        store
            .recall_memory(&RecallOptions {
                query: Some("migrate.rs"),
                ..RecallOptions::default()
            })
            .expect("recall")
            .results
            .len(),
        1,
    );
    assert!(
        store
            .recall_memory(&RecallOptions {
                query: Some("retry loop pelican"),
                ..RecallOptions::default()
            })
            .expect("recall")
            .results
            .is_empty(),
        "every token must appear",
    );
}

/// A limit is applied **after** ranking, so it returns the best matches rather
/// than the newest ones. Applied in SQL it would return the newest — which is
/// exactly the clock-first ordering ADR-0013 rejects.
#[test]
fn a_limit_returns_the_best_matches_not_the_newest() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);
    // Written first, but the strongest evidence there is: stated confidence, and
    // an anchor that resolves.
    store
        .record_memory(&MemoryWrite {
            anchor: Some("sym:rust:src/lib.rs#main"),
            confidence: Some(1.0),
            ..lesson("the best record")
        })
        .expect("write");
    for body in ["newer junk", "newest junk"] {
        store
            .record_memory(&MemoryWrite {
                anchor: Some("sym:rust:src/gone.rs#gone"),
                confidence: Some(0.1),
                ..lesson(body)
            })
            .expect("write");
    }

    let top = store
        .recall_memory(&RecallOptions {
            limit: Some(1),
            ..RecallOptions::default()
        })
        .expect("recall");
    assert_eq!(
        bodies(&top),
        vec!["the best record"],
        "the oldest record wins on evidence, and a limit must not hide that",
    );
    assert_eq!(top.live, 3, "the counts still describe the whole store");
}

/// Scope stays a **namespace with an exact-match filter** — it narrows, and it
/// never decides applicability or ranking. Giving it a second, branch-shaped job
/// is the thing ADR-0013 §Scope rules out.
#[test]
fn scope_narrows_recall_and_decides_nothing_else() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);
    for scope in ["repo", "other-project"] {
        store
            .record_memory(&MemoryWrite {
                scope,
                anchor: Some("sym:rust:src/lib.rs#main"),
                confidence: Some(0.6),
                ..lesson("same lesson, different namespace")
            })
            .expect("write");
    }

    let all = recall(&store);
    assert_eq!(all.results.len(), 2);
    assert!(
        (all.results[0].score - all.results[1].score).abs() < f64::EPSILON,
        "scope must not move a score",
    );
    for recalled in &all.results {
        assert!(recalled.record.applies, "nor decide applicability");
    }

    let narrowed = store
        .recall_memory(&RecallOptions {
            scope: Some("other-project"),
            ..RecallOptions::default()
        })
        .expect("recall");
    assert_eq!(narrowed.results.len(), 1);
    assert_eq!(narrowed.results[0].record.scope, "other-project");
}

/// An empty store recalls nothing, at generation `0`, without erroring — the
/// state every fresh clone is in.
#[test]
fn an_empty_store_recalls_nothing_at_generation_zero() {
    let store = Store::open_in_memory().expect("store");
    let result = recall(&store);
    assert!(result.results.is_empty());
    assert_eq!(result.generation, 0);
    assert_eq!((result.live, result.superseded), (0, 0));
    assert_eq!(result.schema, rto_graph::RECALL_SCHEMA);
}

//! The bounded cache tier and its eviction policy (ADR-0013 Tier 2; build plan
//! Stage 25).
//!
//! The two-tier split is the whole design, and it is a claim about **recovery
//! cost**: everything in this tier is re-derivable, and `build_context` is proven
//! to reconstruct identically, so eviction here costs cycles. Nothing in the
//! episodic tier is re-derivable at all, so eviction there costs the knowledge.
//! The tests that matter are therefore the ones that pin the boundary:
//!
//! - a sweep that evicts **everything it can** leaves every episodic record where
//!   it was — at a budget of zero, which is the harshest instruction the policy
//!   can be given;
//! - the sweep runs at the maintenance seam and **never on a read**;
//! - eviction is oldest-first on `(anchor_valid ASC, last_used ASC)`, by bytes,
//!   and always keeps the most-recently-used entry.
//!
//! **None of them needs a model, a GPU or a network.**

use rto_graph::{
    AnchorState, CacheWrite, DEFAULT_CACHE_BUDGET_BYTES, DEFAULT_MEMORY_SCOPE, FactSet, MemoryKind,
    MemoryWrite, Node, NodeKind, RecallOptions, Store,
};

/// A graph with two anchorable symbols carrying blob hashes.
fn seed_graph(store: &mut Store) {
    let mut facts = FactSet::new();
    for (key, blob) in [
        ("sym:rust:src/a.rs#alpha", "blob-alpha-v1"),
        ("sym:rust:src/b.rs#beta", "blob-beta-v1"),
    ] {
        let mut node = Node::new(key, NodeKind::Fn, "sym");
        node.blob_hash = Some(blob.into());
        facts.nodes.push(node);
    }
    store.rebuild(&facts, Some("treeabc")).expect("rebuild");
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

/// An entry with a payload padded to roughly `bytes`, so budgets in these tests
/// are about sizes rather than counts.
fn put(store: &Store, key: &str, anchor: Option<&str>, bytes: usize) {
    let json = format!("\"{}\"", "x".repeat(bytes));
    store
        .agent_cache_put(&CacheWrite {
            key,
            fingerprint: "fp",
            json: &json,
            anchor,
        })
        .expect("put");
}

/// The keys still in the tier, ordered.
fn keys(store: &Store) -> Vec<String> {
    store
        .agent_cache_entries()
        .expect("entries")
        .into_iter()
        .map(|e| e.key)
        .collect()
}

// --- The boundary: eviction never reaches episodic memory ---------------------

/// **Eviction never removes an episodic row.** The build plan's second
/// definition-of-done item, checked with the harshest instruction the policy can
/// be given — a budget of zero, swept repeatedly, with the cache tier emptied down
/// to its last pinned entry.
///
/// Episodic memory is deliberately seeded with the records a capacity policy would
/// find most tempting if it could see them at all: the oldest, an unanchored one,
/// one whose anchor vanished, and one already superseded.
#[test]
fn a_sweep_at_zero_budget_never_touches_an_episodic_record() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);

    let oldest = store
        .record_memory(&lesson("the oldest lesson"))
        .expect("w");
    store
        .record_memory(&MemoryWrite {
            anchor: Some("sym:rust:src/gone.rs#gone"),
            ..lesson("a lesson about code that is gone")
        })
        .expect("write");
    let overruled = store
        .record_memory(&lesson("an overruled finding"))
        .expect("w");
    store
        .record_memory(&MemoryWrite {
            supersedes: Some(overruled),
            ..lesson("the finding that overruled it")
        })
        .expect("write");
    let before = serde_json::to_vec(
        &store
            .memory_listing(&rto_graph::MemoryFilter {
                include_superseded: true,
                ..rto_graph::MemoryFilter::default()
            })
            .expect("listing"),
    )
    .expect("serialize");

    for key in ["ctx:a", "ctx:b", "ctx:c"] {
        put(&store, key, Some("sym:rust:src/a.rs#alpha"), 1024);
    }

    for _ in 0..3 {
        let swept = store.sweep_agent_cache(0).expect("sweep");
        assert!(swept.evicted <= 3);
    }
    assert_eq!(
        store.agent_cache_entries().expect("entries").len(),
        1,
        "the cache tier is swept down to the one entry that is always kept",
    );

    assert_eq!(
        serde_json::to_vec(
            &store
                .memory_listing(&rto_graph::MemoryFilter {
                    include_superseded: true,
                    ..rto_graph::MemoryFilter::default()
                })
                .expect("listing")
        )
        .expect("serialize"),
        before,
        "a sweep moved episodic memory",
    );
    assert_eq!(store.memory_counts().expect("counts"), (3, 1));
    assert!(
        store.memory_record(oldest).expect("get").is_some(),
        "the oldest record is exactly what an LRU would have taken first",
    );
    // And it is still recallable, which is the property the count alone does not
    // establish.
    assert_eq!(
        store
            .recall_memory(&RecallOptions::default())
            .expect("recall")
            .results
            .len(),
        3,
    );
}

// --- The policy: bytes, oldest-first, and always keep the MRU -----------------

/// Eviction is a **byte budget**, not a row cap: one large entry can cost several
/// small ones their place, and a tier of many small entries under the budget loses
/// nothing at all.
#[test]
fn the_budget_is_bytes_and_not_a_row_count() {
    let mut store = Store::open_in_memory().expect("store");
    for key in ["small:1", "small:2", "small:3", "small:4"] {
        put(&store, key, None, 10);
    }
    let swept = store.sweep_agent_cache(10_000).expect("sweep");
    assert_eq!(swept.evicted, 0, "four rows well under the byte budget");
    assert!(!swept.over_budget);
    assert_eq!(keys(&store).len(), 4);

    // One entry two orders of magnitude larger, and a budget that fits it and
    // very little else. A row-count cap would have seen five equal entries here.
    put(&store, "large", None, 4_000);
    let held = store.agent_cache_stats(u64::MAX).expect("stats").bytes;
    let large = store
        .agent_cache_entries()
        .expect("entries")
        .into_iter()
        .find(|e| e.key == "large")
        .expect("present")
        .bytes;
    assert!(held > large, "the small entries do occupy space");

    let swept = store.sweep_agent_cache(large + 20).expect("sweep");
    assert!(
        swept.evicted > 0,
        "a single large entry must be able to cost small ones their place",
    );
    assert!(keys(&store).contains(&"large".to_owned()));
}

/// **Oldest-first on `(anchor_valid ASC, last_used ASC)`.** An entry whose anchor
/// no longer applies goes before one that still describes this tree, however
/// recently it was used — evidence first, recency second, which is the same order
/// the recall side ranks by.
#[test]
fn eviction_takes_invalid_anchors_before_merely_old_ones() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);
    // Written first, so it is the least recently used of the two valid entries.
    put(&store, "old-valid", Some("sym:rust:src/a.rs#alpha"), 100);
    put(
        &store,
        "new-invalid",
        Some("sym:rust:src/gone.rs#gone"),
        100,
    );
    // A third, most recently used, so neither of the above is pinned as the MRU.
    put(&store, "newest", Some("sym:rust:src/b.rs#beta"), 100);

    let entries = store.agent_cache_entries().expect("entries");
    let state = |key: &str| {
        entries
            .iter()
            .find(|e| e.key == key)
            .expect("present")
            .anchor_state
    };
    assert_eq!(state("new-invalid"), AnchorState::Vanished);
    assert_eq!(state("old-valid"), AnchorState::Valid);

    // Advance the generation so nothing is pinned as this generation's own work,
    // then leave room for exactly two entries.
    store.sweep_agent_cache(u64::MAX).expect("warm-up sweep");
    let swept = store.sweep_agent_cache(230).expect("sweep");

    assert_eq!(swept.evicted, 1);
    assert!(
        !keys(&store).contains(&"new-invalid".to_owned()),
        "the entry whose anchor stopped applying goes first, though it is newer",
    );
    assert!(keys(&store).contains(&"old-valid".to_owned()));
}

/// **The most-recently-used entry is always kept**, even when it alone exceeds the
/// budget — `ModelCache`'s rule, for `ModelCache`'s reason: what was just asked
/// for has to be there. A budget of zero therefore keeps exactly one entry, never
/// none.
#[test]
fn the_most_recently_used_entry_survives_any_budget() {
    let mut store = Store::open_in_memory().expect("store");
    put(&store, "only", None, 5_000);
    let swept = store.sweep_agent_cache(0).expect("sweep");
    assert_eq!(swept.evicted, 0);
    assert_eq!(keys(&store), vec!["only"]);
    assert!(
        swept.over_budget,
        "and the tier says it is still over budget rather than pretending",
    );

    put(&store, "newer", None, 10);
    // Advance past the generation both were written in, so neither is pinned as
    // this session's own work and recency is the only thing left deciding.
    store.sweep_agent_cache(u64::MAX).expect("warm-up sweep");
    // A read is what makes an entry the most recently used, so read the *older*
    // one back and watch it become the one that survives.
    store
        .agent_cache_get("only")
        .expect("get")
        .expect("present");
    store.sweep_agent_cache(0).expect("sweep");
    assert_eq!(
        keys(&store),
        vec!["only"],
        "recency is what a read records, and it is what the sweep honours",
    );
}

/// An entry written in the **current generation** with a valid anchor is the
/// session's own work, and the maintenance pass behind it must not undo it. The
/// pin lasts one generation: the sweep advances the counter on its way out, so the
/// next sweep can take it.
#[test]
fn work_from_this_generation_survives_one_sweep_and_not_two() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);
    put(&store, "mine:1", Some("sym:rust:src/a.rs#alpha"), 1_000);
    put(&store, "mine:2", Some("sym:rust:src/b.rs#beta"), 1_000);

    let first = store.sweep_agent_cache(0).expect("sweep");
    assert_eq!(first.evicted, 0, "a session's own work survives its sweep");
    assert_eq!(first.pinned, 2);
    assert!(first.over_budget, "and the overage is reported, not hidden");
    assert_eq!(keys(&store).len(), 2);

    let second = store.sweep_agent_cache(0).expect("sweep");
    assert_eq!(
        second.evicted, 1,
        "by the next generation the pin has lapsed and the budget binds",
    );
    assert_eq!(keys(&store).len(), 1, "all but the always-kept entry");
}

/// The pin is `generation` **and** a valid anchor, not generation alone: an entry
/// written this generation whose anchor does not apply is ordinary eviction
/// fodder. Otherwise a session could pin the tier full of entries about code that
/// is not there.
#[test]
fn the_current_generation_pin_requires_a_valid_anchor() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);
    put(&store, "keeper", Some("sym:rust:src/a.rs#alpha"), 1_000);
    put(&store, "stale", Some("sym:rust:src/gone.rs#gone"), 1_000);
    put(&store, "newest", Some("sym:rust:src/b.rs#beta"), 10);

    let swept = store.sweep_agent_cache(0).expect("sweep");
    assert_eq!(swept.evicted, 1);
    assert!(
        !keys(&store).contains(&"stale".to_owned()),
        "a fresh entry about vanished code is not this session's useful work",
    );
    assert!(keys(&store).contains(&"keeper".to_owned()));
}

// --- Reads, writes, and what each of them moves -------------------------------

/// A cache read records the access — that is what `hits` and `last_used` are for,
/// and a hit counter nothing increments is a column that lies. What matters is
/// the blast radius: it moves `agent_cache` and nothing else, so the *ranked* read
/// stays reproducible.
#[test]
fn a_cache_read_records_the_access_and_nothing_else() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);
    store
        .record_memory(&lesson("an episodic record"))
        .expect("w");
    let memory_before = serde_json::to_vec(
        &store
            .memory_listing(&rto_graph::MemoryFilter::default())
            .expect("listing"),
    )
    .expect("serialize");
    let recall_before = serde_json::to_vec(
        &store
            .recall_memory(&RecallOptions::default())
            .expect("recall"),
    )
    .expect("serialize");

    put(&store, "ctx:a", None, 10);
    let first = store
        .agent_cache_get("ctx:a")
        .expect("get")
        .expect("present");
    assert_eq!(first.hits, 0, "the hit is recorded for the *next* reader");
    let second = store
        .agent_cache_get("ctx:a")
        .expect("get")
        .expect("present");
    assert_eq!(second.hits, 1);
    assert!(
        second.last_used > first.last_used,
        "and recency advances on every read",
    );

    // Inspection is not use: listing the tier must not move it.
    let listed = store.agent_cache_entries().expect("entries");
    assert_eq!(listed[0].hits, 2);
    assert_eq!(
        store.agent_cache_entries().expect("entries")[0].hits,
        2,
        "listing the tier twice must not count as using it",
    );

    assert_eq!(
        serde_json::to_vec(
            &store
                .memory_listing(&rto_graph::MemoryFilter::default())
                .expect("listing")
        )
        .expect("serialize"),
        memory_before,
        "a cache read reached the episodic tier",
    );
    assert_eq!(
        serde_json::to_vec(
            &store
                .recall_memory(&RecallOptions::default())
                .expect("recall")
        )
        .expect("serialize"),
        recall_before,
        "a cache read changed what recall answers",
    );
}

/// A missing key is a miss, not an error, and reads nothing back. Recording an
/// access for a key that is not there would invent an entry's worth of history.
#[test]
fn a_miss_is_a_miss() {
    let store = Store::open_in_memory().expect("store");
    assert!(
        store
            .agent_cache_get("nothing:here")
            .expect("get")
            .is_none()
    );
    assert!(store.agent_cache_entries().expect("entries").is_empty());
    assert!(!store.agent_cache_forget("nothing:here").expect("forget"));
}

/// A replacement is a new value under the same key: the payload, the size, the
/// generation and the anchor evidence are all replaced, and `hits` survives —
/// the counter is about how often the *key* is worth having, and the value under
/// it is re-derivable by definition.
#[test]
fn a_replacement_refreshes_the_entry_and_keeps_its_hit_count() {
    let store = Store::open_in_memory().expect("store");
    put(&store, "ctx:a", None, 10);
    store.agent_cache_get("ctx:a").expect("get");
    store.agent_cache_get("ctx:a").expect("get");

    put(&store, "ctx:a", None, 500);
    let entry = &store.agent_cache_entries().expect("entries")[0];
    assert_eq!(entry.hits, 2, "the key's history survives a new value");
    assert!(entry.bytes > 400, "and the size is the new payload's");
    assert_eq!(
        store.agent_cache_entries().expect("entries").len(),
        1,
        "a replacement replaces rather than accumulating",
    );
}

/// Anchor evidence is captured **from the graph at write time**, exactly as the
/// episodic tier's is, and resolved on every read — so the same entry reports a
/// different anchor state once the code moves, with nothing rewritten.
#[test]
fn cache_anchors_are_captured_at_write_and_resolved_on_read() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);
    put(&store, "ctx:alpha", Some("sym:rust:src/a.rs#alpha"), 10);
    put(&store, "ctx:none", None, 10);
    put(&store, "ctx:ghost", Some("sym:rust:src/ghost.rs#ghost"), 10);

    let state = |store: &Store, key: &str| {
        store
            .agent_cache_entries()
            .expect("entries")
            .into_iter()
            .find(|e| e.key == key)
            .expect("present")
            .anchor_state
    };
    assert_eq!(state(&store, "ctx:alpha"), AnchorState::Valid);
    assert_eq!(state(&store, "ctx:none"), AnchorState::Unanchored);
    assert_eq!(state(&store, "ctx:ghost"), AnchorState::Vanished);

    // Recompile `alpha`: same key, different blob.
    let mut facts = store.export_factset().expect("export");
    for node in &mut facts.nodes {
        if node.key == "sym:rust:src/a.rs#alpha" {
            node.blob_hash = Some("blob-alpha-v2".into());
        }
    }
    store.rebuild(&facts, Some("treedef")).expect("rebuild");

    assert_eq!(
        state(&store, "ctx:alpha"),
        AnchorState::Drifted,
        "resolved against the tree in front of you, on every read",
    );
}

// --- Stats and the budget -----------------------------------------------------

/// The tier reports what it holds against what it is allowed to hold, so an
/// operator can see a bound approaching rather than discovering it as an eviction.
#[test]
fn stats_report_the_tier_against_its_budget() {
    let store = Store::open_in_memory().expect("store");
    let empty = store
        .agent_cache_stats(DEFAULT_CACHE_BUDGET_BYTES)
        .expect("stats");
    assert_eq!((empty.entries, empty.bytes), (0, 0));
    assert_eq!(empty.budget_bytes, DEFAULT_CACHE_BUDGET_BYTES);

    put(&store, "ctx:a", None, 100);
    put(&store, "ctx:b", None, 100);
    let stats = store
        .agent_cache_stats(DEFAULT_CACHE_BUDGET_BYTES)
        .expect("stats");
    assert_eq!(stats.entries, 2);
    assert!(stats.bytes >= 200, "sizes are the payloads', not a count");
    assert_eq!(
        stats.bytes,
        store
            .agent_cache_entries()
            .expect("entries")
            .iter()
            .map(|e| e.bytes)
            .sum::<u64>(),
    );
}

/// The default budget is **256 MB**, and it is a default rather than a cap: the
/// sweep enforces whatever it is handed.
#[test]
fn the_default_budget_is_the_documented_number() {
    assert_eq!(DEFAULT_CACHE_BUDGET_BYTES, 256 * 1024 * 1024);
    assert_eq!(
        DEFAULT_CACHE_BUDGET_BYTES / (1024 * 1024),
        256,
        "and it is expressed in whole megabytes, which is how it is configured",
    );
}

/// An explicit forget removes one entry and leaves the rest — the same
/// reclamation shape the episodic tier has, for the operator who wants a specific
/// thing gone rather than a budget enforced.
#[test]
fn forgetting_one_entry_leaves_the_others() {
    let store = Store::open_in_memory().expect("store");
    put(&store, "ctx:a", None, 10);
    put(&store, "ctx:b", None, 10);
    assert!(store.agent_cache_forget("ctx:a").expect("forget"));
    assert_eq!(keys(&store), vec!["ctx:b"]);
    assert!(
        !store.agent_cache_forget("ctx:a").expect("forget"),
        "forgetting it twice is not an error, and does nothing",
    );
}

/// A sweep of an empty tier is a no-op that still advances the generation, so a
/// maintenance pass on a cold store is not a special case anywhere.
#[test]
fn sweeping_an_empty_tier_is_a_no_op_that_still_advances() {
    let mut store = Store::open_in_memory().expect("store");
    let swept = store
        .sweep_agent_cache(DEFAULT_CACHE_BUDGET_BYTES)
        .expect("s");
    assert_eq!((swept.scanned, swept.evicted, swept.freed_bytes), (0, 0, 0));
    assert!(!swept.over_budget);
    let again = store
        .sweep_agent_cache(DEFAULT_CACHE_BUDGET_BYTES)
        .expect("s");
    assert!(
        again.generation > swept.generation,
        "the generation advances even with nothing to do",
    );
}

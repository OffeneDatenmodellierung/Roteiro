//! The invariants that make generated media content a *separate* artifact store
//! rather than a `derived` fact (ADR-0015, issue #300).
//!
//! Every assertion here exists because the alternative is mechanically possible.
//! Writing a transcript into `nodes.meta.content` compiles, passes, inherits
//! `nodes.provenance DEFAULT 'derived'`, is swept into `export_factset`, and
//! ranks in `search` — which is exactly what happened: a fixture of digital
//! silence was transcribed into two kilobytes of fluent prose about world
//! government and stored as a derived fact. These tests are what stop that from
//! happening quietly again.
//!
//! **None of them needs a model or a GPU.** The generation seam is a trait, so
//! incrementality, idempotence, `--force`, producer identity and the search
//! channels are all exercised by a stub producer that counts its own
//! invocations — which is what CI actually runs.

use std::sync::atomic::{AtomicUsize, Ordering};

use rto_graph::{
    Edge, EdgeKind, FactSet, GeneratedContent, GraphArtifact, MediaBlob, MediaBuildOptions,
    MediaFilter, MediaKind, MediaProducer, MediaWrite, Node, NodeKind, Producer, Provenance,
    SearchOptions, Store, build_media, search, search_channels,
};

/// The confabulation from issue #300, shortened. A clip of digital silence, and a
/// model that produced this about it.
const CONFABULATION: &str = "Good evening. Tonight I want to talk about the prospects for \
     world government, and why the twentieth century's institutions were never designed \
     for the problem we now face.";

/// A small graph standing in for a real repository's derived + authored layers.
fn seed_graph(store: &mut Store) {
    let mut facts = FactSet::new();
    let mut adr = Node::new("adr:0015", NodeKind::Adr, "Generated media content")
        .with_provenance(Provenance::Authored);
    adr.path = Some("docs/adr/0015.md".into());
    adr.meta = serde_json::json!({
        "content": "Generative output stops being derived. Prospects for world government \
                    are not a fact about a blob.",
    });
    let mut clip = Node::new("file:assets/silence.wav", NodeKind::File, "silence.wav");
    clip.path = Some("assets/silence.wav".into());
    clip.blob_hash = Some("blob-silence".into());
    facts.nodes = vec![
        adr,
        clip,
        Node::new("sym:rust:src/lib.rs#main", NodeKind::Fn, "main"),
    ];
    facts.edges = vec![Edge::authored(
        "adr:0015",
        "sym:rust:src/lib.rs#main",
        EdgeKind::References,
    )];
    store.rebuild(&facts, Some("treeabc")).expect("rebuild");
}

/// The ASR producer identity, as `media build` would construct it.
fn voxtral() -> Producer {
    Producer {
        kind: MediaKind::Audio,
        model: "voxtral-mini-3b".to_owned(),
        model_digest: "4705be8e".to_owned(),
        quantisation: "Q4_K_M".to_owned(),
        mmproj_digest: "4f24c4ef".to_owned(),
        prompt: "Transcribe this audio recording.".to_owned(),
        temperature: 0.0,
        max_tokens: 512,
    }
}

/// A second, better model — a *different* producer for the same modality.
fn successor() -> Producer {
    Producer {
        model: "voxtral-small-24b".to_owned(),
        model_digest: "deadbeef".to_owned(),
        ..voxtral()
    }
}

/// A generator that produces fixed text and counts how often it was asked to.
///
/// The counter is the whole point: "incremental" and "idempotent" are claims
/// about *work not done*, and only an invocation count can distinguish "skipped"
/// from "regenerated the same string".
struct StubProducer {
    identity: Producer,
    text: String,
    calls: AtomicUsize,
}

impl StubProducer {
    fn new(identity: Producer, text: &str) -> Self {
        Self {
            identity,
            text: text.to_owned(),
            calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

impl MediaProducer for StubProducer {
    fn producer(&self) -> &Producer {
        &self.identity
    }

    fn generate(&self, _path: &str, _bytes: &[u8]) -> Option<GeneratedContent> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Some(GeneratedContent {
            text: self.text.clone(),
            confidence: None,
        })
    }
}

/// The one silent clip every test builds against.
fn silence() -> Vec<MediaBlob> {
    vec![MediaBlob {
        blob_id: "blob-silence".to_owned(),
        path: "assets/silence.wav".to_owned(),
        kind: MediaKind::Audio,
    }]
}

/// Audio only, incremental — the default shape of a `media build --audio`.
fn audio_only() -> MediaBuildOptions {
    MediaBuildOptions {
        audio: true,
        vision: false,
        force: false,
    }
}

/// Run a build with one producer over `blobs`, feeding it a byte or two.
fn run_build(
    store: &mut Store,
    blobs: &[MediaBlob],
    producer: &StubProducer,
    opts: MediaBuildOptions,
) -> rto_graph::MediaBuildReport {
    build_media(store, blobs, &[producer], opts, |_| Some(vec![0u8; 4])).expect("build")
}

/// **The headline invariant.** A `media build` must not change the graph by one
/// byte: not the nodes, not the edges, not the exported artifact.
///
/// `export_factset` is compared as its serialised bytes rather than field by
/// field, because "byte-identical function of the tree" is the actual promise —
/// a change that reordered a `meta` key would satisfy a structural comparison and
/// break the published artifact.
#[test]
fn a_media_build_leaves_the_exported_artifact_byte_identical() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);

    let before_facts = store.export_factset().expect("export");
    let before_bytes = serde_json::to_vec(&before_facts).expect("serialize");
    let before_artifact =
        serde_json::to_vec(&GraphArtifact::from_store(&store).expect("artifact")).expect("bytes");
    let (before_nodes, before_edges) = (
        store.node_count().expect("nodes"),
        store.edge_count().expect("edges"),
    );

    let producer = StubProducer::new(voxtral(), CONFABULATION);
    let report = run_build(&mut store, &silence(), &producer, audio_only());
    assert_eq!(
        report.generated, 1,
        "the build must actually have done work"
    );
    assert_eq!(
        store.media_content_count().expect("count"),
        1,
        "…and stored it somewhere"
    );

    assert_eq!(
        serde_json::to_vec(&store.export_factset().expect("export")).expect("serialize"),
        before_bytes,
        "export_factset must be byte-identical across a media build",
    );
    assert_eq!(
        serde_json::to_vec(&GraphArtifact::from_store(&store).expect("artifact")).expect("bytes"),
        before_artifact,
        "the published GraphArtifact must be byte-identical across a media build",
    );
    assert_eq!(store.node_count().expect("nodes"), before_nodes);
    assert_eq!(store.edge_count().expect("edges"), before_edges);

    // And nothing acquired a node kind or provenance on the way: the generated
    // text appears in no node's meta anywhere in the graph. The marker is a
    // phrase only the transcript uses — the seeded ADR deliberately shares the
    // *topic*, so a looser check would catch the wrong thing.
    for node in store.all_nodes().expect("nodes") {
        let meta = node.meta.to_string();
        assert!(
            !meta.contains("Tonight I want to talk about"),
            "{}: generated text leaked into a node's meta",
            node.key,
        );
    }
}

/// **A silent clip cannot put prose into default `search` results.**
///
/// The whole of #300 in one assertion: the confabulation is stored, it is
/// findable when asked for, and a plain `search` — the command every agent and
/// human runs — does not return it.
#[test]
fn generated_content_is_absent_from_default_search_and_marked_when_opted_in() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);
    let producer = StubProducer::new(voxtral(), CONFABULATION);
    run_build(&mut store, &silence(), &producer, audio_only());

    // The words are unmistakably from the transcript, not from the graph.
    let hits = search(&store, "twentieth century institutions", 10).expect("search");
    assert!(
        hits.is_empty(),
        "a silent clip's confabulation reached default search: {:?}",
        hits.iter().map(|h| &h.node.key).collect::<Vec<_>>(),
    );

    // The same query through the default options — the shape the CLI uses without
    // `--include-generated` — is likewise empty in *both* channels.
    let default = search_channels(
        &store,
        "twentieth century institutions",
        SearchOptions::default(),
    )
    .expect("search");
    assert!(default.hits.is_empty());
    assert!(
        default.generated.is_empty(),
        "generated content must be opt-in, not merely de-ranked",
    );

    // Opted in, it is returned — and every hit is marked.
    let opted = search_channels(
        &store,
        "twentieth century institutions",
        SearchOptions {
            limit: 10,
            include_generated: true,
        },
    )
    .expect("search");
    assert_eq!(opted.generated.len(), 1);
    let hit = &opted.generated[0];
    assert!(hit.generated, "every generated hit carries the marker");
    assert_eq!(hit.kind, "audio");
    assert_eq!(hit.model, "voxtral-mini-3b");
    assert_eq!(hit.blob, "blob-silence");
    assert_eq!(hit.path, "assets/silence.wav");
    assert_eq!(hit.producer, voxtral().id().to_string());
    assert!(
        hit.snippet
            .as_deref()
            .is_some_and(|s| s.contains("world government")),
        "a generated hit carries grounding text: {:?}",
        hit.snippet,
    );

    // The serialised form marks it too, for a consumer that reads only JSON.
    let json = serde_json::to_value(&opted).expect("json");
    assert_eq!(json["generated"][0]["generated"], true);
    assert!(json["hits"].as_array().is_some_and(Vec::is_empty));
}

/// **Generated content never acquires the `authored` relevance boost.**
///
/// The two are scored by different functions in different channels, so the boost
/// is structurally unreachable — but "structurally unreachable" is a claim, and
/// this is the test that makes it one you can check. An authored ADR and a
/// transcript are given text that matches the same query; the ADR keeps its +40
/// and the transcript is not merely ranked below it but is in a different list.
#[test]
fn generated_content_never_gets_the_authored_boost() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);
    let producer = StubProducer::new(voxtral(), CONFABULATION);
    run_build(&mut store, &silence(), &producer, audio_only());

    let results = search_channels(
        &store,
        "world government",
        SearchOptions {
            limit: 10,
            include_generated: true,
        },
    )
    .expect("search");

    // Both channels match the query…
    let adr = results
        .hits
        .iter()
        .find(|h| h.node.key == "adr:0015")
        .expect("the authored ADR matches by content");
    let generated = results
        .generated
        .first()
        .expect("the transcript matches too");

    // …and the authored node outscores the transcript by more than the difference
    // in their content terms alone: the +40 lands on the ADR and nowhere else.
    assert!(
        adr.score > generated.score,
        "authored intent must outrank generated text: {} vs {}",
        adr.score,
        generated.score,
    );
    assert!(
        adr.score - generated.score >= 40,
        "the authored boost must be the ADR's and only the ADR's: {} vs {}",
        adr.score,
        generated.score,
    );

    // The structural half: no generated hit is ever a graph hit, whatever it says.
    assert!(
        results
            .hits
            .iter()
            .all(|h| !h.node.key.contains("blob-silence")),
        "a generated record must never appear as a node",
    );
}

/// `media build` is incremental and idempotent: a second run with the same
/// producer invokes no model and writes no row.
///
/// The invocation counter is what gives this teeth. Without it, a build that
/// regenerated the identical string on every run would satisfy a row count and
/// still cost a 715 MB projector load per blob (#301).
#[test]
fn a_second_build_with_the_same_producer_does_no_work() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);
    let producer = StubProducer::new(voxtral(), CONFABULATION);

    let first = run_build(&mut store, &silence(), &producer, audio_only());
    assert_eq!(first.candidates, 1);
    assert_eq!(first.generated, 1);
    assert_eq!(first.skipped_existing, 0);
    assert_eq!(producer.calls(), 1);

    let second = run_build(&mut store, &silence(), &producer, audio_only());
    assert_eq!(second.candidates, 1);
    assert_eq!(second.generated, 0, "nothing new to write");
    assert_eq!(second.skipped_existing, 1, "the existing record is kept");
    assert_eq!(
        producer.calls(),
        1,
        "the model must not be invoked for a blob that already has a record",
    );
    assert_eq!(store.media_content_count().expect("count"), 1);

    // `--force` is the only way to redo the work, and it replaces in place rather
    // than accumulating duplicates for one identity.
    let forced = run_build(
        &mut store,
        &silence(),
        &producer,
        MediaBuildOptions {
            force: true,
            ..audio_only()
        },
    );
    assert_eq!(forced.generated, 1);
    assert_eq!(producer.calls(), 2);
    assert_eq!(
        store.media_content_count().expect("count"),
        1,
        "--force replaces this producer's record, it does not add a second",
    );
}

/// A **different producer is a new record, not a mutation**. This is the
/// property the whole keying decision exists to give: you can compare what two
/// models said about the same blob, and you can drop one of them.
#[test]
fn a_different_producer_writes_a_new_record_beside_the_old_one() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);

    let old = StubProducer::new(voxtral(), CONFABULATION);
    let new = StubProducer::new(successor(), "Four seconds of near-silence, no speech.");
    run_build(&mut store, &silence(), &old, audio_only());
    run_build(&mut store, &silence(), &new, audio_only());

    let records = store
        .media_records(&MediaFilter::default())
        .expect("records");
    assert_eq!(records.len(), 2, "both descriptions are kept");
    assert_eq!(
        records
            .iter()
            .map(|r| r.blob_id.as_str())
            .collect::<std::collections::BTreeSet<_>>(),
        ["blob-silence"].into_iter().collect(),
        "…of the same blob",
    );
    let texts: Vec<&str> = records.iter().map(|r| r.content.text.as_str()).collect();
    assert!(texts.contains(&CONFABULATION));
    assert!(texts.iter().any(|t| t.contains("no speech")));

    // The generation counter distinguishes a first description from a re-describe.
    let mut generations: Vec<u32> = records.iter().map(|r| r.generation).collect();
    generations.sort_unstable();
    assert_eq!(generations, vec![1, 2]);

    // Every record's evidence chain is intact and attributable.
    for record in &records {
        assert_eq!(record.producer_id, record.producer.id());
        assert!(!record.tool_version.is_empty());
        assert!(!record.produced_at.is_empty());
        assert_eq!(record.producer.kind, MediaKind::Audio);
    }
}

/// **`media clear --producer X` removes exactly that producer's records and
/// leaves the graph untouched.** Dropping a model you no longer trust must not
/// cost a re-sync.
#[test]
fn clearing_one_producer_leaves_the_other_and_the_graph_alone() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);
    let old = StubProducer::new(voxtral(), CONFABULATION);
    let new = StubProducer::new(successor(), "Four seconds of near-silence, no speech.");
    run_build(&mut store, &silence(), &old, audio_only());
    run_build(&mut store, &silence(), &new, audio_only());

    let graph_before =
        serde_json::to_vec(&store.export_factset().expect("export")).expect("serialize");

    let removed = store
        .clear_media_content(Some(voxtral().id().as_str()))
        .expect("clear");
    assert_eq!(removed, 1, "exactly one producer's records went");

    let left = store
        .media_records(&MediaFilter::default())
        .expect("records");
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].producer_id, successor().id());
    assert_eq!(
        serde_json::to_vec(&store.export_factset().expect("export")).expect("serialize"),
        graph_before,
        "clearing a producer must not touch the graph",
    );

    // Clearing an unknown producer removes nothing rather than everything.
    assert_eq!(
        store
            .clear_media_content(Some("media:audio:nobody:0000000000000000"))
            .expect("clear"),
        0,
    );
    assert_eq!(store.media_content_count().expect("count"), 1);

    // And the unqualified clear takes the rest.
    assert_eq!(store.clear_media_content(None).expect("clear"), 1);
    assert_eq!(store.media_content_count().expect("count"), 0);
    assert_eq!(
        serde_json::to_vec(&store.export_factset().expect("export")).expect("serialize"),
        graph_before,
    );
}

/// **Records survive `rebuild`**, following the `imports` precedent — they are
/// expensive to reproduce (a 715 MB projector load per blob, #301) and are not
/// derivable from source alone.
///
/// A `sync` that rebuilds the graph from a changed tree must not silently throw
/// away hours of generation.
#[test]
fn records_survive_a_graph_rebuild() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);
    let producer = StubProducer::new(voxtral(), CONFABULATION);
    run_build(&mut store, &silence(), &producer, audio_only());
    let before = store
        .media_records(&MediaFilter::default())
        .expect("records");
    assert_eq!(before.len(), 1);

    // A full rebuild at a different tree — what a code-changing `sync` does.
    let mut next = FactSet::new();
    next.nodes = vec![Node::new(
        "sym:rust:src/lib.rs#other",
        NodeKind::Fn,
        "other",
    )];
    store.rebuild(&next, Some("treedef")).expect("rebuild");

    assert_eq!(
        store
            .media_records(&MediaFilter::default())
            .expect("records"),
        before,
        "generated content must survive a rebuild, exactly as imports do",
    );
    // Including the incrementality decision: the next build still skips.
    let after = run_build(&mut store, &silence(), &producer, audio_only());
    assert_eq!(after.skipped_existing, 1);
    assert_eq!(producer.calls(), 1);
}

/// The modality filter is honoured: `media build --audio` must not run the vision
/// producer over an image, and vice versa.
#[test]
fn a_build_runs_only_the_modalities_it_was_asked_for() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);
    let blobs = vec![
        MediaBlob {
            blob_id: "blob-silence".to_owned(),
            path: "assets/silence.wav".to_owned(),
            kind: MediaKind::Audio,
        },
        MediaBlob {
            blob_id: "blob-blank".to_owned(),
            path: "assets/blank.png".to_owned(),
            kind: MediaKind::Vision,
        },
    ];
    let audio = StubProducer::new(voxtral(), CONFABULATION);
    let vision = StubProducer::new(
        Producer {
            kind: MediaKind::Vision,
            model: "smolvlm-500m-gguf".to_owned(),
            prompt: "Describe this image.".to_owned(),
            max_tokens: 128,
            ..voxtral()
        },
        "A plain white square.",
    );

    // Audio only: the vision producer is never consulted, and the image gets no
    // record even though a producer for it exists.
    let report = build_media(&mut store, &blobs, &[&audio, &vision], audio_only(), |_| {
        Some(vec![0u8; 4])
    })
    .expect("build");
    assert_eq!(report.candidates, 1, "only the audio blob was a candidate");
    assert_eq!(report.generated, 1);
    assert_eq!(vision.calls(), 0);
    assert!(
        store
            .media_records(&MediaFilter {
                kind: Some(MediaKind::Vision),
                ..MediaFilter::default()
            })
            .expect("records")
            .is_empty(),
    );

    // Both: the image is described too, under its own producer.
    let report = build_media(
        &mut store,
        &blobs,
        &[&audio, &vision],
        MediaBuildOptions::default(),
        |_| Some(vec![0u8; 4]),
    )
    .expect("build");
    assert_eq!(report.candidates, 2);
    assert_eq!(report.generated, 1, "the audio blob was already described");
    assert_eq!(report.skipped_existing, 1);
    assert_eq!(vision.calls(), 1);
    assert_eq!(store.media_content_count().expect("count"), 2);
}

/// A producer that returns nothing (or empty text) is recorded as *nothing
/// produced*, not as an empty record. An empty transcript would be a claim about
/// the blob; the absence of a record is an honest silence.
#[test]
fn a_producer_that_says_nothing_writes_nothing() {
    struct Mute(Producer);
    impl MediaProducer for Mute {
        fn producer(&self) -> &Producer {
            &self.0
        }
        fn generate(&self, _path: &str, _bytes: &[u8]) -> Option<GeneratedContent> {
            None
        }
    }
    struct Blank(Producer);
    impl MediaProducer for Blank {
        fn producer(&self) -> &Producer {
            &self.0
        }
        fn generate(&self, _path: &str, _bytes: &[u8]) -> Option<GeneratedContent> {
            Some(GeneratedContent {
                text: "   \n ".to_owned(),
                confidence: None,
            })
        }
    }

    let mut store = Store::open_in_memory().expect("store");
    for producer in [
        Box::new(Mute(voxtral())) as Box<dyn MediaProducer>,
        Box::new(Blank(successor())),
    ] {
        let report = build_media(
            &mut store,
            &silence(),
            &[producer.as_ref()],
            audio_only(),
            |_| Some(vec![0u8; 4]),
        )
        .expect("build");
        assert_eq!(report.generated, 0);
        assert_eq!(report.empty, 1);
    }
    assert_eq!(store.media_content_count().expect("count"), 0);
}

/// The store's own write path, exercised directly: the same `(blob, producer)`
/// twice is refused without `replace`, and the row round-trips every field it was
/// given.
#[test]
fn a_record_round_trips_its_whole_evidence_chain() {
    let mut store = Store::open_in_memory().expect("store");
    let identity = voxtral();
    let content = GeneratedContent {
        text: CONFABULATION.to_owned(),
        confidence: Some(0.5),
    };
    let write = MediaWrite {
        blob_id: "blob-silence",
        path: "assets/silence.wav",
        producer: &identity,
        tool_version: "9.9.9",
        content: &content,
        replace: false,
    };
    assert!(store.record_media_content(&write).expect("write"));
    assert!(
        !store.record_media_content(&write).expect("write"),
        "the same producer must not describe one blob twice",
    );

    let records = store
        .media_records(&MediaFilter::default())
        .expect("records");
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.blob_id, "blob-silence");
    assert_eq!(record.path, "assets/silence.wav");
    assert_eq!(record.tool_version, "9.9.9");
    assert_eq!(record.generation, 1);
    assert_eq!(record.content, content);
    assert_eq!(record.producer, identity);
    assert_eq!(record.producer_id, identity.id());

    // The filters narrow rather than merely decorate.
    assert_eq!(
        store
            .media_records(&MediaFilter {
                producer: Some(identity.id().as_str()),
                ..MediaFilter::default()
            })
            .expect("records")
            .len(),
        1,
    );
    assert!(
        store
            .media_records(&MediaFilter {
                blob_id: Some("blob-other"),
                ..MediaFilter::default()
            })
            .expect("records")
            .is_empty(),
    );
    assert!(
        store
            .has_media_record("blob-silence", identity.id().as_str())
            .expect("exists")
    );
    assert!(
        !store
            .has_media_record("blob-silence", successor().id().as_str())
            .expect("exists"),
    );
}

/// `media status` distinguishes *nothing to generate* from *cannot generate*, and
/// counts against the tree rather than against itself.
#[test]
fn status_reports_producers_candidates_and_coverage() {
    let mut store = Store::open_in_memory().expect("store");
    seed_graph(&mut store);

    // Empty store, one candidate in the tree.
    let status = rto_graph::media_status(&store, &silence()).expect("status");
    assert_eq!(status.records, 0);
    assert!(status.producers.is_empty());
    let audio = status
        .candidates
        .iter()
        .find(|c| c.kind == MediaKind::Audio)
        .expect("audio row");
    assert_eq!(audio.blobs, 1);
    assert_eq!(audio.described, 0);

    let producer = StubProducer::new(voxtral(), CONFABULATION);
    run_build(&mut store, &silence(), &producer, audio_only());

    let status = rto_graph::media_status(&store, &silence()).expect("status");
    assert_eq!(status.records, 1);
    assert_eq!(status.producers.len(), 1);
    let summary = &status.producers[0];
    assert_eq!(summary.producer_id, voxtral().id());
    assert_eq!(summary.model, "voxtral-mini-3b");
    assert_eq!(summary.quantisation, "Q4_K_M");
    assert_eq!(summary.records, 1);
    assert!(!summary.latest.is_empty());
    let audio = status
        .candidates
        .iter()
        .find(|c| c.kind == MediaKind::Audio)
        .expect("audio row");
    assert_eq!(audio.described, 1, "coverage is counted against the tree");

    // A record whose blob has left the tree is reported as orphaned rather than
    // deleted behind the operator's back — a blob can come back, and the record
    // is expensive.
    let orphans = store
        .orphan_media_records(&std::collections::BTreeSet::new())
        .expect("orphans");
    assert_eq!(orphans.len(), 1);
    assert_eq!(orphans[0].blob_id, "blob-silence");
}

#![cfg(feature = "audio-metadata")]
//! ADR-0016: audio metadata as `derived` facts.
//!
//! Everything here runs from the committed fixtures — **no model, no network, no
//! C/C++ toolchain, no platform audio library**. That is the point of a format
//! read: unlike `audio_ingest.rs`'s `generation` module, none of these tests is
//! `#[ignore]`d, so they run wherever CI does.
//!
//! The suite is organised around the three obligations ADR-0016 places on a
//! deterministic-but-inexact fact:
//!
//! * **exactness** — a duration the container states is `exact`; one inferred is
//!   `estimated`; and one it does not state at all is *absent*, never a guess.
//!   [`exactness`] covers all three from real bytes.
//! * **determinism** — same bytes, same facts, byte for byte, and independent of
//!   the order a container happens to store its tags in ([`determinism`]).
//! * **placement** — the facts are ordinary graph facts: searchable through the
//!   ordinary scorer, unranked, and never mixed with the generated-content store
//!   ADR-0015 created ([`placement`]).
//!
//! # Where the non-committed fixtures come from
//!
//! Two cases need bytes the repository does not carry, and both are **built from
//! the committed fixtures rather than downloaded or committed anew**, keeping the
//! "no third-party audio, no encoding dependency" property of
//! `tests/audio_fixtures.rs`:
//!
//! * a CBR MP3 long enough for symphonia to estimate a duration from — the
//!   committed 10-frame MP3, repeated (see [`long_cbr_mp3`]);
//! * WAV and MP3 files carrying tags — the committed clips with a RIFF `LIST`
//!   `INFO` chunk or an `ID3v2` tag spliced in (see [`tagged`]).

use rto_graph::{Extractor, NodeKind, Registry};

/// A fixture's bytes, by file name.
fn fixture(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("audio")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()))
}

/// Read one fixture's audio facts, failing loudly if the reader declines it.
fn facts_of(name: &str) -> rto_graph::AudioFacts {
    let ext = name.rsplit('.').next();
    rto_graph::audio::read(&fixture(name), ext)
        .unwrap_or_else(|| panic!("{name}: the reader returned no facts"))
}

/// A **CBR MP3 with no Xing/VBRI header**, long enough that symphonia will
/// estimate a duration for it: the committed `silence-44khz-mono-261ms.mp3`
/// repeated `n` times.
///
/// Repetition is exact, not approximate. That fixture is `mp3::silence(10)` from
/// `tests/audio_fixtures.rs` — ten *identical*, self-contained 104-byte MPEG-1
/// Layer III frames, each with `part2_3_length == 0`, so no frame depends on any
/// other's main-data pool. Concatenating the file `n` times is therefore
/// byte-identical to `mp3::silence(10 * n)`, and this needs no encoder, no new
/// dependency and no new committed binary. (The encoder itself cannot be called
/// from here: `audio_fixtures.rs` is its own test crate, so nothing in it is
/// nameable from this one.)
///
/// It carries no Xing or Info header — the generator writes none — which is
/// exactly the case ADR-0016 calls out: symphonia must fall back to inferring the
/// length from the bitrate and the stream size, and the result must be marked
/// `estimated`.
fn long_cbr_mp3(n: usize) -> Vec<u8> {
    fixture("silence-44khz-mono-261ms.mp3").repeat(n)
}

mod exactness {
    use super::{facts_of, fixture, long_cbr_mp3};
    use rto_graph::Exactness;

    /// **Exact.** WAV's `data` chunk length and FLAC's `STREAMINFO` total-samples
    /// field are statements the container makes about itself, so the duration
    /// derived from them is exact — and it is the *right* number, not merely a
    /// marked one: every fixture's name states its own length.
    #[test]
    fn a_container_that_states_its_length_yields_an_exact_duration() {
        for (name, ms) in [
            ("silence-16khz-mono-256ms.wav", 256),
            ("tone-500hz-16khz-mono-256ms.wav", 256),
            ("syllables-16khz-mono-512ms.wav", 512),
            ("silence-16khz-mono-256ms.flac", 256),
            ("tone-500hz-16khz-mono-256ms.flac", 256),
        ] {
            let facts = facts_of(name);
            let duration = facts
                .duration
                .unwrap_or_else(|| panic!("{name}: no duration"));
            assert_eq!(
                duration.exactness,
                Exactness::Exact,
                "{name}: a stated sample count is not an estimate",
            );
            assert_eq!(duration.ms, ms, "{name}: wrong duration");
            // The name is the claim; the container is the evidence. Check the rest
            // of the shape matches it too, so a decoder regression that returned a
            // plausible-but-wrong rate would fail here.
            assert_eq!(facts.sample_rate_hz, Some(16_000), "{name}");
            assert_eq!(facts.channels, Some(1), "{name}");
            assert_eq!(facts.bit_depth, Some(16), "{name}");
        }
    }

    /// **Estimated.** A CBR MP3 with no Xing/VBRI header gives symphonia nothing
    /// to read a length from, so it infers one from the bitrate and the stream
    /// size. The number is close — but it is an inference, and it must say so.
    ///
    /// 100 copies of the 10-frame fixture is 1000 frames of 1152 samples at
    /// 44.1 kHz = 26.122 s. The assertion is deliberately two-sided: the marker
    /// must be `Estimated` **and** the number must be about right, because a
    /// reader that returned a wildly wrong length while marking it honestly would
    /// still be broken.
    #[test]
    fn a_cbr_mp3_without_a_xing_header_yields_an_estimated_duration() {
        let bytes = long_cbr_mp3(100);
        assert!(
            !bytes
                .windows(4)
                .any(|w| w == b"Xing" || w == b"Info" || w == b"VBRI"),
            "the fixture must carry no VBR header, or this tests the wrong path",
        );
        let facts = rto_graph::audio::read(&bytes, Some("mp3")).expect("facts");
        let duration = facts.duration.expect("an estimated duration");
        assert_eq!(
            duration.exactness,
            Exactness::Estimated,
            "an inferred length must never be labelled exact",
        );
        assert_eq!(
            duration.ms, 26_122,
            "1000 frames of 1152 samples at 44.1 kHz"
        );
        assert_eq!(facts.container, "mp3");
        assert_eq!(facts.codec, "mp3");
        assert_eq!(facts.sample_rate_hz, Some(44_100));
    }

    /// **Absent.** The repository's degenerate 1 040-byte MP3 is too short for
    /// symphonia to infer anything from: it reports the codec, the rate and the
    /// channel count that the frame headers state outright, and **no frame count
    /// at all**.
    ///
    /// So there is no duration — and the correct record of that is *nothing*. Not
    /// zero, not a null, not a nearby guess from the file size. This is the
    /// assertion ADR-0016 exists for.
    ///
    /// **The whole struct is pinned**, field by field, not just the absent
    /// duration. `read` returning `Some` here — rather than `None` — is the
    /// distinction its documentation now turns on, and an equality over every
    /// field is what stops that documentation drifting away from the behaviour
    /// again: a change that started discarding this blob wholesale, or started
    /// inventing a `frames` for it, fails here.
    #[test]
    fn a_container_that_states_no_length_yields_no_duration_at_all() {
        let facts = facts_of("silence-44khz-mono-261ms.mp3");
        assert_eq!(
            facts,
            rto_graph::AudioFacts {
                container: "mp3".to_owned(),
                codec: "mp3".to_owned(),
                sample_rate_hz: Some(44_100),
                // The frame headers state no bit depth or sample format, and MPEG
                // audio has neither until it is decoded.
                bit_depth: None,
                channels: Some(1),
                channel_layout: Some("front_left".to_owned()),
                sample_format: None,
                // The two that matter: nothing to count, so nothing to record.
                frames: None,
                duration: None,
                tags: Vec::new(),
            },
            "the degenerate fixture must yield facts with an absent duration, not \
             be discarded whole",
        );

        // …and the JSON has no `duration` key whatsoever, rather than a null.
        let json = serde_json::to_value(&facts).expect("serialize");
        assert!(
            json.get("duration").is_none(),
            "an absent duration must not serialise at all: {json}",
        );
    }

    /// A blob the reader cannot make sense of yields **no facts at all** — the
    /// other, harder absence, and the *only* thing `read`'s `None` means.
    ///
    /// The distinction from the test above is the whole point: that fixture is a
    /// readable container missing one fact, this is not a container at all.
    #[test]
    fn a_blob_the_reader_rejects_yields_no_facts() {
        assert_eq!(rto_graph::audio::read(b"", Some("wav")), None);
        assert_eq!(
            rto_graph::audio::read(b"not audio at all", Some("wav")),
            None
        );
        // A truncated header is not guessed at either.
        let truncated = &fixture("silence-16khz-mono-256ms.flac")[..8];
        assert_eq!(rto_graph::audio::read(truncated, Some("flac")), None);
    }

    /// …and `None` really does mean **no node**, which is the half of the contract
    /// only the extraction path can demonstrate.
    ///
    /// The `file` node is still emitted, with the blob's true size: a `.wav` whose
    /// bytes are nonsense is still a file in the tree. It just has nothing to say
    /// about its stream, and says nothing rather than saying it emptily.
    #[test]
    fn an_unreadable_audio_blob_emits_no_stream_node() {
        use rto_graph::{Extractor, NodeKind, Registry};

        let bytes = b"this is not a wav file, whatever the extension claims";
        let facts = Registry::default().extract("assets/broken.wav", "blob-id", bytes);
        assert_eq!(
            facts.nodes.len(),
            1,
            "only the file node: {:?}",
            facts.nodes.iter().map(|n| &n.key).collect::<Vec<_>>(),
        );
        assert_eq!(facts.nodes[0].kind, NodeKind::File);
        assert!(
            facts.edges.is_empty(),
            "no stream node means no contains edge either",
        );
        assert_eq!(
            facts.nodes[0].meta["bytes"].as_u64(),
            u64::try_from(bytes.len()).ok(),
        );
    }

    /// Every rendering of a duration names its exactness — the property that
    /// makes "no surface shows an estimate as exact" structural rather than a
    /// standing review obligation.
    #[test]
    fn every_surface_that_shows_a_duration_shows_its_exactness() {
        let exact = facts_of("silence-16khz-mono-256ms.wav");
        assert!(
            exact.summary().contains("256 ms (exact)"),
            "{}",
            exact.summary()
        );

        let estimated = rto_graph::audio::read(&long_cbr_mp3(100), Some("mp3")).expect("facts");
        assert!(
            estimated.summary().contains("26122 ms (estimated)"),
            "{}",
            estimated.summary(),
        );

        // The node the graph stores says it too, in both its rendered content and
        // its structured meta — a consumer reading either one is told.
        let facts = Registry::default().extract(
            "assets/clip.mp3",
            "blob-id",
            &rto_graph::audio::read(&long_cbr_mp3(100), Some("mp3"))
                .map(|_| long_cbr_mp3(100))
                .expect("facts"),
        );
        let stream = super::stream_node(&facts);
        assert_eq!(stream.meta["duration"]["exactness"], "estimated");
        assert!(
            stream.meta["content"]
                .as_str()
                .expect("content")
                .contains("(estimated)"),
            "the rendered content must carry the marker too",
        );
        // And there is no bare number a consumer could read without it.
        assert!(stream.meta.get("duration_ms").is_none());
    }

    use rto_graph::{Extractor, Registry};
}

mod determinism {
    use super::{facts_of, fixture, long_cbr_mp3, tagged};
    use rto_graph::{Extractor, Registry};

    /// The core `derived` invariant: identical bytes yield identical facts, byte
    /// for byte, across runs. Compared as serialised JSON rather than as structs,
    /// because it is the serialised form the content-addressed cache and the
    /// exported factset actually persist.
    #[test]
    fn identical_bytes_yield_byte_identical_facts() {
        for name in [
            "silence-16khz-mono-256ms.wav",
            "syllables-16khz-mono-512ms.wav",
            "tone-500hz-16khz-mono-256ms.flac",
            "silence-44khz-mono-261ms.mp3",
        ] {
            let once = serde_json::to_string(&facts_of(name)).expect("serialize");
            let twice = serde_json::to_string(&facts_of(name)).expect("serialize");
            assert!(!once.is_empty(), "{name}: nothing to compare");
            assert_eq!(once, twice, "{name}: the format read is not deterministic");
        }
    }

    /// The whole fact set — nodes, edges, meta and all — is stable across runs,
    /// and the audio node is really in it (so the equality is not vacuous).
    #[test]
    fn the_emitted_fact_set_is_stable_across_runs() {
        for name in [
            "tone-500hz-16khz-mono-256ms.flac",
            "silence-44khz-mono-261ms.mp3",
        ] {
            let bytes = fixture(name);
            let path = format!("assets/{name}");
            let a = Registry::default().extract(&path, "blob-id", &bytes);
            let b = Registry::default().extract(&path, "blob-id", &bytes);
            assert!(
                a.nodes.iter().any(|n| n.key.starts_with("audio:")),
                "{name}: no audio node, so the comparison below proves nothing",
            );
            assert_eq!(
                serde_json::to_string(&a).expect("serialize"),
                serde_json::to_string(&b).expect("serialize"),
                "{name}: fact sets differ between runs",
            );
        }
    }

    /// **Tag order must come from the content, not the container.** The same tags
    /// written in a different order in the file must produce the same fact set —
    /// otherwise a re-tagged file that says exactly the same things would look
    /// like a changed fact to every consumer downstream.
    #[test]
    fn tag_order_in_the_container_does_not_reach_the_facts() {
        let pairs = [
            ("TITLE", "A Title"),
            ("ARTIST", "An Artist"),
            ("DESCRIPTION", "A Comment"),
        ];
        let forwards = tagged::flac_with_vorbis_comments(&pairs);
        let mut reversed_pairs = pairs;
        reversed_pairs.reverse();
        let backwards = tagged::flac_with_vorbis_comments(&reversed_pairs);
        assert_ne!(
            forwards, backwards,
            "the two files must differ in their bytes"
        );

        let a = rto_graph::audio::read(&forwards, Some("flac")).expect("facts");
        let b = rto_graph::audio::read(&backwards, Some("flac")).expect("facts");
        assert_eq!(a.tags.len(), 3, "all three tags must be read: {:?}", a.tags);
        assert_eq!(a, b, "tag order in the file must not reach the facts");
        // Sorted by name, so the order is a function of the content.
        let names: Vec<&str> = a.tags.iter().map(|t| t.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "tags must be emitted in sorted order");
    }

    /// A duplicated tag is recorded once. Two identical `ARTIST` comments say one
    /// thing about the file, and the graph should say it once.
    #[test]
    fn identical_tags_are_de_duplicated() {
        let bytes = tagged::flac_with_vorbis_comments(&[("ARTIST", "Twice"), ("ARTIST", "Twice")]);
        let facts = rto_graph::audio::read(&bytes, Some("flac")).expect("facts");
        assert_eq!(facts.tags.len(), 1, "{:?}", facts.tags);
    }

    /// **The same logical tag from two sources collapses to one row.**
    ///
    /// An MP3 carrying `ID3v2` at the head and `ID3v1` at the tail states its
    /// artist twice, under two different container keys (`TPE1` and `ARTIST`).
    /// They normalise to the same `name` and the same `value`, so they are one
    /// fact — and de-duplicating on the whole struct, `source_key` included, would
    /// keep both and make `summary()` print `artist: …` twice.
    ///
    /// Which key survives is fixed by the sort, not by which revision happened to
    /// be drained first: rows are ordered by `(name, value, source_key)` and the
    /// first of each `(name, value)` run is kept, so the lowest `source_key` in
    /// byte order wins — `ARTIST` here. That is deliberately a rule about bytes
    /// rather than about formats; see `read_tags` for why.
    #[test]
    fn the_same_tag_from_two_sources_is_one_row() {
        let bytes = tagged::mp3_with_id3v2_and_id3v1("Hildegard");
        let facts = rto_graph::audio::read(&bytes, Some("mp3")).expect("facts");

        let artists: Vec<&super::AudioTag> =
            facts.tags.iter().filter(|t| t.name == "artist").collect();
        assert_eq!(
            artists.len(),
            1,
            "one artist, stated twice in the file, must be one row: {:?}",
            facts.tags,
        );
        assert_eq!(artists[0].value, "Hildegard");
        assert_eq!(
            artists[0].source_key, "ARTIST",
            "the surviving provenance key is the lowest in byte order, not the \
             first drained: {:?}",
            facts.tags,
        );

        // The user-visible consequence, asserted where the user would see it.
        let summary = facts.summary();
        assert_eq!(
            summary.matches("artist: Hildegard").count(),
            1,
            "the rendered summary must not repeat the line: {summary}",
        );

        // Both tags really did arrive — otherwise this would pass by reading only
        // one of the two sources, which is a different bug wearing the same green.
        assert!(
            facts.tags.iter().any(|t| t.source_key == "TRACK"),
            "the ID3v1 block must have been read at all: {:?}",
            facts.tags,
        );
    }

    /// The merge is a pure function of the bytes, `source_key` included: repeated
    /// reads of a blob whose tags come from two sources are byte-identical.
    ///
    /// This is the determinism half of the de-duplication rule. A merge that kept
    /// "whichever revision was drained first" would still pass the single-read
    /// assertions above while making the exported factset depend on symphonia's
    /// revision ordering — something an upstream release could change without a
    /// byte of the file moving.
    #[test]
    fn a_two_source_merge_is_deterministic() {
        let bytes = tagged::mp3_with_id3v2_and_id3v1("Hildegard");
        let once = rto_graph::audio::read(&bytes, Some("mp3")).expect("facts");
        let twice = rto_graph::audio::read(&bytes, Some("mp3")).expect("facts");
        assert!(!once.tags.is_empty(), "nothing to compare");
        assert_eq!(
            serde_json::to_string(&once).expect("serialize"),
            serde_json::to_string(&twice).expect("serialize"),
        );
    }

    /// Tag ordering is **byte order, not a locale collation**. `sort` on a
    /// `String` compares UTF-8 bytes, which is the same everywhere; a
    /// locale-sensitive collation orders `Ä` relative to `Z` differently
    /// depending on where the machine thinks it is, and the exported factset would
    /// stop being a function of the tree alone.
    ///
    /// Pinned by a case the two orders disagree about, rather than by asserting
    /// the absence of a `setlocale` call.
    #[test]
    fn tags_sort_by_bytes_rather_than_by_locale() {
        // Same tag name, so the *values* decide: "Zebra" sorts before "Ärtist" by
        // bytes (0x5A < 0xC3) and after it in most locale collations.
        let bytes = tagged::flac_with_vorbis_comments(&[("ARTIST", "Zebra"), ("ARTIST", "Ärtist")]);
        let facts = rto_graph::audio::read(&bytes, Some("flac")).expect("facts");
        let values: Vec<&str> = facts.tags.iter().map(|t| t.value.as_str()).collect();
        assert_eq!(
            values,
            vec!["Zebra", "Ärtist"],
            "tags must be in UTF-8 byte order, not a locale collation",
        );
    }

    /// The estimated case is deterministic too — an inference is still a pure
    /// function of the bytes, which is what keeps it inside `derived` at all.
    #[test]
    fn an_estimated_duration_is_still_deterministic() {
        let bytes = long_cbr_mp3(37);
        let a = rto_graph::audio::read(&bytes, Some("mp3")).expect("facts");
        let b = rto_graph::audio::read(&bytes, Some("mp3")).expect("facts");
        assert_eq!(a, b);
        assert!(a.duration.is_some());
    }
}

mod placement {
    use super::{fixture, stream_node};
    use rto_graph::{EdgeKind, Extractor, NodeKind, Provenance, Registry};

    /// The facts hang off the `file` node as their own node, under a `contains`
    /// edge — the shape `config_key` and `image_ref` already use — and they are
    /// `derived`, like every other extracted fact.
    #[test]
    fn the_facts_hang_off_the_file_node_as_a_derived_contains_edge() {
        let bytes = fixture("tone-500hz-16khz-mono-256ms.flac");
        let facts = Registry::default().extract("assets/clip.flac", "blob-id", &bytes);

        let stream = stream_node(&facts);
        assert_eq!(stream.key, "audio:assets/clip.flac");
        assert_eq!(
            stream.kind,
            NodeKind::Other(rto_graph::AUDIO_STREAM_KIND.to_owned())
        );
        assert_eq!(stream.name, "clip.flac");
        assert_eq!(stream.path.as_deref(), Some("assets/clip.flac"));
        assert_eq!(stream.blob_hash.as_deref(), Some("blob-id"));
        assert_eq!(
            stream.provenance,
            Provenance::Derived,
            "extracted facts are derived; nothing here is authored or inferred",
        );

        let edge = facts.edges.first().expect("a contains edge");
        assert_eq!(edge.src, "file:assets/clip.flac");
        assert_eq!(edge.dst, stream.key);
        assert_eq!(edge.kind, EdgeKind::Contains);
        assert_eq!(edge.provenance, Provenance::Derived);
        assert_eq!(
            edge.confidence, None,
            "a derived edge carries no confidence score",
        );
    }

    /// The stream node carries the whole shape the ADR promises, under names a
    /// consumer can rely on.
    #[test]
    fn the_stream_node_records_the_shape_the_adr_lists() {
        let bytes = fixture("silence-16khz-mono-256ms.wav");
        let facts = Registry::default().extract("assets/clip.wav", "blob-id", &bytes);
        let meta = &stream_node(&facts).meta;
        assert_eq!(meta["container"], "wave");
        assert_eq!(meta["codec"], "pcm_s16le");
        assert_eq!(meta["sample_rate_hz"], 16_000);
        assert_eq!(meta["bit_depth"], 16);
        assert_eq!(meta["channels"], 1);
        assert!(meta["channel_layout"].is_string());
        assert_eq!(meta["frames"], 4096);
        assert_eq!(meta["duration"]["ms"], 256);
        assert_eq!(meta["duration"]["exactness"], "exact");
    }

    /// Searchable **in the ordinary way**: the rendered summary lands in
    /// `meta.content`, which is the field [`rto_graph::search`] already matches
    /// on, so codec, rate and channel words are all findable with no new branch in
    /// the scorer.
    #[test]
    fn the_facts_are_searchable_through_the_ordinary_scorer() {
        let store = super::store_with(&[
            ("assets/clip.flac", "tone-500hz-16khz-mono-256ms.flac"),
            ("assets/voice.wav", "syllables-16khz-mono-512ms.wav"),
        ]);
        for query in ["flac", "16000", "mono", "pcm_s16le"] {
            let hits = rto_graph::search(&store, query, 10).expect("search");
            assert!(
                hits.iter().any(|h| h.node.key.starts_with("audio:")),
                "`{query}` found no audio node: {:?}",
                hits.iter().map(|h| &h.node.key).collect::<Vec<_>>(),
            );
        }
    }

    /// …and with **no special ranking**. The node is `derived`, so it takes none
    /// of the `authored` boost curated intent gets; an ADR that mentions the same
    /// word still outranks it.
    #[test]
    fn the_facts_take_no_ranking_privilege() {
        let mut store =
            super::store_with(&[("assets/clip.flac", "tone-500hz-16khz-mono-256ms.flac")]);
        let mut adr = rto_graph::Node::new("adr:0016", NodeKind::Adr, "ADR-0016")
            .with_provenance(Provenance::Authored);
        adr.meta = serde_json::json!({ "content": "Audio metadata: flac and wav duration." });
        store
            .apply_factset(&rto_graph::FactSet::new().with_node(adr))
            .expect("apply");

        let hits = rto_graph::search(&store, "flac", 10).expect("search");
        let rank = |prefix: &str| {
            hits.iter()
                .position(|h| h.node.key.starts_with(prefix))
                .unwrap_or_else(|| panic!("no hit starting {prefix}: {hits:?}"))
        };
        assert!(
            rank("adr:") < rank("audio:"),
            "an extracted audio fact must not outrank curated intent: {:?}",
            hits.iter()
                .map(|h| (&h.node.key, h.score))
                .collect::<Vec<_>>(),
        );
    }

    /// `export_factset` — and therefore the published `GraphArtifact` — stays a
    /// **byte-identical function of the tree**.
    ///
    /// The audio facts are new nodes and edges in the export, so this is the check
    /// that they carry no ordering incidental: two stores built from the same
    /// blobs, in *different insertion orders*, must export the same bytes. Order
    /// matters here because the previous test only proves one build is repeatable;
    /// this one proves the export does not remember how the store was filled.
    #[test]
    fn the_exported_factset_is_a_byte_identical_function_of_the_tree() {
        let forwards = super::store_with(&[
            ("assets/a.flac", "tone-500hz-16khz-mono-256ms.flac"),
            ("assets/b.wav", "syllables-16khz-mono-512ms.wav"),
            ("assets/c.mp3", "silence-44khz-mono-261ms.mp3"),
        ]);
        let backwards = super::store_with(&[
            ("assets/c.mp3", "silence-44khz-mono-261ms.mp3"),
            ("assets/b.wav", "syllables-16khz-mono-512ms.wav"),
            ("assets/a.flac", "tone-500hz-16khz-mono-256ms.flac"),
        ]);
        let export = |s: &rto_graph::Store| {
            serde_json::to_string(&s.export_factset().expect("export")).expect("serialize")
        };
        let a = export(&forwards);
        assert!(
            a.contains("audio:assets/a.flac"),
            "the export must actually contain the audio facts, or this is vacuous",
        );
        assert_eq!(
            a,
            export(&backwards),
            "the export moved with insertion order"
        );
    }

    /// The two stores stay disjoint. ADR-0016's facts are nodes and edges;
    /// ADR-0015's generated content is neither, and lives in `media_content`.
    /// Extracting audio metadata must not write a media record, and the generated
    /// channel must stay empty.
    #[test]
    fn extraction_writes_no_generated_media_record() {
        let store = super::store_with(&[("assets/clip.flac", "tone-500hz-16khz-mono-256ms.flac")]);
        let records = store
            .media_records(&rto_graph::MediaFilter::default())
            .expect("media records");
        assert!(
            records.is_empty(),
            "a format read must not touch the generated-content store: {records:?}",
        );
        let results = rto_graph::search_channels(
            &store,
            "flac",
            rto_graph::SearchOptions {
                limit: 10,
                include_generated: true,
            },
        )
        .expect("search");
        assert!(
            !results.hits.is_empty(),
            "the graph channel must hold the extracted facts",
        );
        assert!(
            results.generated.is_empty(),
            "nothing extracted may appear in the generated channel: {:?}",
            results.generated,
        );
    }
}

mod tags {
    use super::tagged;

    /// A FLAC's Vorbis comments are normalised onto symphonia's standard tag
    /// vocabulary — `TITLE` becomes `track_title`, not `title` — and the
    /// container's own key is kept beside it so the mapping stays auditable
    /// rather than lossy.
    #[test]
    fn vorbis_comments_are_normalised_and_keep_their_source_key() {
        let bytes =
            tagged::flac_with_vorbis_comments(&[("TITLE", "A Title"), ("ARTIST", "An Artist")]);
        let facts = rto_graph::audio::read(&bytes, Some("flac")).expect("facts");
        let by_name: std::collections::BTreeMap<&str, &super::AudioTag> =
            facts.tags.iter().map(|t| (t.name.as_str(), t)).collect();
        let title = by_name
            .get("track_title")
            .unwrap_or_else(|| panic!("no normalised title in {:?}", facts.tags));
        assert_eq!(title.value, "A Title");
        assert_eq!(title.source_key, "TITLE");
        assert_eq!(
            by_name.get("artist").map(|t| t.value.as_str()),
            Some("An Artist"),
            "{:?}",
            facts.tags,
        );
    }

    /// **A recorded upstream limitation, not an accepted silence.**
    ///
    /// ADR-0016 lists RIFF INFO among the tag formats extracted, and symphonia
    /// 0.6.1 does parse a WAV's `LIST`/`INFO` chunk — and then **drops it on the
    /// floor**. `symphonia-format-riff`'s `WavReader::try_new` accumulates the
    /// parsed revision into a local `metadata` binding and then constructs itself
    /// with `opts.external_data.metadata.unwrap_or_default()` instead, so the
    /// local is discarded. (`symphonia-bundle-flac` gets this right: it pushes its
    /// revision onto the external log, which is why the test above passes.)
    ///
    /// Nothing on our side can recover them short of re-parsing the container,
    /// which is the hand-rolling ADR-0016 declined. So the limitation is pinned
    /// here rather than left invisible: **this test fails the moment a symphonia
    /// bump fixes it**, which is exactly when the ADR's claim should be revisited
    /// and this test inverted.
    #[test]
    fn wav_riff_info_tags_are_lost_upstream_in_symphonia_0_6_1() {
        let bytes = tagged::wav_with_riff_info(&[("INAM", "A Title"), ("IART", "An Artist")]);
        let facts = rto_graph::audio::read(&bytes, Some("wav")).expect("facts");
        // The stream itself still reads correctly — only the tags are lost.
        assert_eq!(facts.sample_rate_hz, Some(16_000));
        assert_eq!(facts.duration.map(|d| d.ms), Some(256));
        assert!(
            facts.tags.is_empty(),
            "symphonia now surfaces WAV RIFF INFO tags — invert this test, and revisit \
             ADR-0016's tag list: {:?}",
            facts.tags,
        );
    }

    /// **The check on the feature-flag decision.** `ID3v2` is a *standalone* tag
    /// reader in symphonia: with `id3v2` off it is never registered on the probe,
    /// and every tag on an MP3 is silently invisible while ADR-0016 promises to
    /// extract them. This asserts the flag is on and the tags arrive.
    #[test]
    fn id3v2_tags_on_an_mp3_are_read() {
        let bytes = tagged::mp3_with_id3v2(&[("TIT2", "A Title"), ("TPE1", "An Artist")]);
        let facts = rto_graph::audio::read(&bytes, Some("mp3")).expect("facts");
        let names: Vec<&str> = facts.tags.iter().map(|t| t.name.as_str()).collect();
        assert!(
            names.contains(&"track_title") && names.contains(&"artist"),
            "ID3v2 tags did not reach the facts (is the `id3v2` feature on?): {:?}",
            facts.tags,
        );
        assert!(
            facts.tags.iter().any(|t| t.value == "An Artist"),
            "{:?}",
            facts.tags,
        );
    }

    /// Tags reach `meta.content`, so they are searchable like any other captured
    /// text — which is what makes "find the clip by its artist" work offline.
    #[test]
    fn tags_reach_the_searchable_summary() {
        let bytes = tagged::flac_with_vorbis_comments(&[("ARTIST", "Hildegard")]);
        let facts = rto_graph::audio::read(&bytes, Some("flac")).expect("facts");
        assert!(
            facts.summary().contains("artist: Hildegard"),
            "{}",
            facts.summary()
        );
    }
}

/// The one `audio_stream` node in a fact set.
fn stream_node(facts: &rto_graph::FactSet) -> &rto_graph::Node {
    facts
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Other(rto_graph::AUDIO_STREAM_KIND.to_owned()))
        .unwrap_or_else(|| {
            panic!(
                "no audio node in {:?}",
                facts.nodes.iter().map(|n| &n.key).collect::<Vec<_>>()
            )
        })
}

/// An in-memory store holding the extracted facts for `(path, fixture)` pairs.
fn store_with(blobs: &[(&str, &str)]) -> rto_graph::Store {
    let mut store = rto_graph::Store::open_in_memory().expect("open store");
    for (path, name) in blobs {
        let facts = Registry::default().extract(path, "blob-id", &fixture(name));
        store.apply_factset(&facts).expect("apply");
    }
    store
}

/// Re-export for [`tags`], which names the type in a map.
use rto_graph::AudioTag;

/// Splice tags into the committed fixtures, so tag handling is tested against
/// real container bytes without committing a new binary or taking an encoding
/// dependency. Both writers are a few lines of chunk/frame layout, in the spirit
/// of `tests/audio_fixtures.rs`'s hand-written encoders.
mod tagged {
    use super::fixture;

    /// The committed 16-bit mono WAV with a RIFF `LIST`/`INFO` chunk spliced in
    /// **before** the `data` chunk, and the outer `RIFF` size fixed up.
    ///
    /// Before, not after: a WAVE reader walks the chunk list until it reaches
    /// `data`, and from there the rest of the file *is* the audio stream — so a
    /// `LIST` appended past it is never parsed, by symphonia or by anything else.
    /// The committed fixture's canonical 44-byte header makes the splice point
    /// exact: 12 bytes of `RIFF….WAVE`, then a 24-byte `fmt ` chunk, then `data`.
    pub fn wav_with_riff_info(pairs: &[(&str, &str)]) -> Vec<u8> {
        /// Offset of the `data` chunk in the committed fixture's canonical header.
        const DATA_AT: usize = 36;

        let mut info = Vec::new();
        info.extend_from_slice(b"INFO");
        for (key, value) in pairs {
            assert_eq!(key.len(), 4, "a RIFF INFO key is a FourCC");
            let mut text = value.as_bytes().to_vec();
            text.push(0); // NUL-terminated
            info.extend_from_slice(key.as_bytes());
            info.extend_from_slice(&u32::try_from(text.len()).expect("fits u32").to_le_bytes());
            info.extend_from_slice(&text);
            if text.len() % 2 == 1 {
                info.push(0); // chunks are word-aligned
            }
        }

        let mut chunk = Vec::with_capacity(8 + info.len());
        chunk.extend_from_slice(b"LIST");
        chunk.extend_from_slice(&u32::try_from(info.len()).expect("fits u32").to_le_bytes());
        chunk.extend_from_slice(&info);

        let source = fixture("silence-16khz-mono-256ms.wav");
        assert_eq!(
            &source[DATA_AT..DATA_AT + 4],
            b"data",
            "the fixture's header is no longer the canonical 44-byte layout",
        );
        let mut out = Vec::with_capacity(source.len() + chunk.len());
        out.extend_from_slice(&source[..DATA_AT]);
        out.extend_from_slice(&chunk);
        out.extend_from_slice(&source[DATA_AT..]);
        // Fix the outer RIFF size: everything after the first 8 bytes.
        let riff_size = u32::try_from(out.len() - 8).expect("fits u32");
        out[4..8].copy_from_slice(&riff_size.to_le_bytes());
        out
    }

    /// The committed FLAC with a `VORBIS_COMMENT` metadata block spliced in
    /// between `STREAMINFO` and the audio frame.
    ///
    /// FLAC's metadata blocks are a chain: each header's top bit says "I am the
    /// last". The committed fixture has exactly one block (`STREAMINFO`, header
    /// `0x80` = last + type 0), so inserting a second means clearing that bit and
    /// marking the new block last instead (`0x84` = last + type 4). The audio
    /// frame that follows is untouched, so the samples stay byte-identical to the
    /// committed fixture's.
    pub fn flac_with_vorbis_comments(pairs: &[(&str, &str)]) -> Vec<u8> {
        /// A `VORBIS_COMMENT` block's lengths are little-endian, unlike the rest
        /// of FLAC — the block is Vorbis's format, embedded verbatim.
        fn le_len(bytes: &[u8]) -> [u8; 4] {
            u32::try_from(bytes.len()).expect("fits u32").to_le_bytes()
        }

        /// Offset of the first audio frame: `fLaC` (4) + block header (4) +
        /// `STREAMINFO` (34).
        const FRAME_AT: usize = 42;

        // Vendor string, then a count, then `KEY=value` entries.
        let vendor = b"roteiro-test";
        let mut payload = Vec::new();
        payload.extend_from_slice(&le_len(vendor));
        payload.extend_from_slice(vendor);
        payload.extend_from_slice(&u32::try_from(pairs.len()).expect("fits u32").to_le_bytes());
        for (key, value) in pairs {
            let entry = format!("{key}={value}").into_bytes();
            payload.extend_from_slice(&le_len(&entry));
            payload.extend_from_slice(&entry);
        }

        let source = fixture("silence-16khz-mono-256ms.flac");
        assert_eq!(
            source[4], 0x80,
            "the fixture's STREAMINFO is no longer the last block"
        );
        let mut out = Vec::with_capacity(source.len() + payload.len() + 4);
        out.extend_from_slice(&source[..FRAME_AT]);
        out[4] = 0x00; // STREAMINFO: no longer the last block
        // METADATA_BLOCK_HEADER: last-block = 1, type = 4 (VORBIS_COMMENT), and a
        // 24-bit big-endian length.
        out.push(0x84);
        out.extend_from_slice(
            &u32::try_from(payload.len())
                .expect("fits u32")
                .to_be_bytes()[1..],
        );
        out.extend_from_slice(&payload);
        out.extend_from_slice(&source[FRAME_AT..]);
        out
    }

    /// The committed MP3 with an ID3v2.3 tag prepended.
    ///
    /// An `ID3v2` tag sits in front of the first MPEG sync word, which is exactly
    /// how real MP3s carry one — and exactly the layout that needs symphonia's
    /// standalone `id3v2` reader to be registered on the probe.
    pub fn mp3_with_id3v2(frames: &[(&str, &str)]) -> Vec<u8> {
        let mut body = Vec::new();
        for (id, value) in frames {
            assert_eq!(id.len(), 4, "an ID3v2.3 frame id is four characters");
            // Frame payload: one encoding byte (0 = ISO-8859-1) then the text.
            let mut payload = vec![0u8];
            payload.extend_from_slice(value.as_bytes());
            body.extend_from_slice(id.as_bytes());
            body.extend_from_slice(
                &u32::try_from(payload.len())
                    .expect("fits u32")
                    .to_be_bytes(),
            );
            body.extend_from_slice(&[0, 0]); // frame flags
            body.extend_from_slice(&payload);
        }

        let mut out = Vec::with_capacity(10 + body.len());
        out.extend_from_slice(b"ID3");
        out.extend_from_slice(&[3, 0]); // version 2.3.0
        out.push(0); // flags
        out.extend_from_slice(&synchsafe(u32::try_from(body.len()).expect("fits u32")));
        out.extend_from_slice(&body);
        out.extend_from_slice(&fixture("silence-44khz-mono-261ms.mp3"));
        out
    }

    /// The committed MP3 carrying an `ID3v2` tag at the head **and** an `ID3v1`
    /// tag at the tail — the layout that produces the same logical tag from two
    /// sources, which is what a merged, de-duplicated view has to cope with.
    ///
    /// `artist` is deliberately the same string in both, so the two rows differ
    /// only in the container's own key: `TPE1` from `ID3v2`, `ARTIST` from
    /// `ID3v1`. Real files get here routinely — a tagger writes `ID3v2` and leaves
    /// a legacy `ID3v1` block for old players.
    ///
    /// The `ID3v1` block is the format's fixed 128 bytes at end-of-file: `TAG`,
    /// then 30-byte title/artist/album, a 4-byte year, a 30-byte comment and a
    /// one-byte genre index. Only the artist field is filled; `0xFF` is the "no
    /// genre" index, so symphonia emits no genre tag for it.
    pub fn mp3_with_id3v2_and_id3v1(artist: &str) -> Vec<u8> {
        /// A fixed-width, NUL-padded `ID3v1` text field.
        fn field(text: &str, width: usize) -> Vec<u8> {
            let mut out = text.as_bytes().to_vec();
            assert!(
                out.len() <= width,
                "`{text}` overflows a {width}-byte field"
            );
            out.resize(width, 0);
            out
        }

        let mut out = mp3_with_id3v2(&[("TPE1", artist)]);
        out.extend_from_slice(b"TAG");
        out.extend_from_slice(&field("", 30)); // title
        out.extend_from_slice(&field(artist, 30)); // artist — the duplicate
        out.extend_from_slice(&field("", 30)); // album
        out.extend_from_slice(&field("", 4)); // year
        out.extend_from_slice(&field("", 30)); // comment
        out.push(0xFF); // genre: none
        out
    }

    /// `ID3v2`'s synchsafe integer: 28 bits spread over four bytes, seven bits
    /// apiece, so the size field can never contain a false MPEG sync word.
    fn synchsafe(value: u32) -> [u8; 4] {
        assert!(value < 1 << 28, "an ID3v2 tag size is 28 bits");
        [
            u8::try_from((value >> 21) & 0x7f).expect("7 bits"),
            u8::try_from((value >> 14) & 0x7f).expect("7 bits"),
            u8::try_from((value >> 7) & 0x7f).expect("7 bits"),
            u8::try_from(value & 0x7f).expect("7 bits"),
        ]
    }
}

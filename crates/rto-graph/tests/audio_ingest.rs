//! Behavioural cover for what an audio blob does to the **derived graph**, using
//! the committed fixtures.
//!
//! # What changed, and why this file got stronger
//!
//! This suite used to be split in two tiers because audio ingestion was inert
//! without a model: `asr_content` returned `None` when the Voxtral GGUF was
//! absent, exactly as it did when the clip was over the size cap or the `audio`
//! toggle was off. On a model-less machine — every CI runner — the *presence* of
//! a transcript could not distinguish those cases, so the always-on tier had to
//! settle for "no content **while the toggle is off**".
//!
//! Since ADR-0015 that hedge is gone. A transcript is generated, not decoded, so
//! it is not a `derived` fact and extraction does not produce one **in any build,
//! with any toggle, with any model installed**. The assertion is therefore
//! absence, unconditionally — and it holds on a developer machine with three
//! gigabytes of Voxtral on disk just as it holds on a bare runner. That is a
//! materially better guard than the one it replaces: the failure #300 describes
//! would now be caught by CI, not only by a machine that happened to have the
//! model.
//!
//! Real transcription still exists; it moved to [`generation`], which drives
//! `roteiro media build`'s path into the artifact store rather than the graph.

use rto_graph::{Extractor, IngestConfig, MediaKind, NodeKind, Provenance, Registry};

/// A fixture's bytes, by file name.
fn fixture(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("audio")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()))
}

/// Every committed fixture, in the order they are asserted over.
const FIXTURES: [&str; 6] = [
    "silence-16khz-mono-256ms.wav",
    "tone-500hz-16khz-mono-256ms.wav",
    "syllables-16khz-mono-512ms.wav",
    "silence-16khz-mono-256ms.flac",
    "tone-500hz-16khz-mono-256ms.flac",
    "silence-44khz-mono-261ms.mp3",
];

/// The extensions the audio modality accepts, and nothing else.
const ACCEPTED: [&str; 3] = ["wav", "mp3", "flac"];

/// The audio modality accepts exactly `wav`, `mp3` and `flac`, so the fixture set
/// must cover all three — otherwise a format could quietly stop decoding and
/// nothing here would notice — and must contain nothing else, or a later test
/// would be asserting over a file no modality ever classifies as audio.
///
/// The match is on `".{ext}"`, not `ext`: a suffix check would count
/// `clip.notwav` as covering `wav`, and this same file deliberately treats
/// `.wav.bak` as a *rejected* extension.
#[test]
fn the_fixture_set_covers_every_accepted_extension() {
    for ext in ACCEPTED {
        assert!(
            FIXTURES.iter().any(|f| f.ends_with(&format!(".{ext}"))),
            "no fixture covers the `{ext}` branch of the audio modality",
        );
        assert!(
            MediaKind::Audio.accepts_path(&format!("a/clip.{ext}")),
            "`{ext}` must be an accepted audio extension",
        );
    }
    for name in FIXTURES {
        assert!(
            ACCEPTED
                .iter()
                .any(|ext| name.ends_with(&format!(".{ext}"))),
            "{name} is not an extension the audio modality accepts",
        );
    }
}

/// How many nodes one audio blob yields, and how many edges.
///
/// Without `audio-metadata` an audio blob is one bare `file` node and nothing
/// else. With it (ADR-0016) the container's own account of the stream becomes a
/// second node — `audio:<path>` — under a `contains` edge from the file. Pinning
/// the exact counts either way is what stops extraction quietly growing spurious
/// facts for a binary blob; asserting "at least a file node" would not.
///
/// Every committed fixture, the degenerate MP3 included, yields a stream node:
/// that fixture has no *duration* the reader will state, but it does have a codec,
/// a rate and a channel count. ADR-0016's **absent** case is therefore about the
/// duration field, not about the node — see `audio_metadata.rs`, which asserts
/// both that and the harder case of a blob the reader rejects outright.
#[cfg(feature = "audio-metadata")]
fn expected_shape(_name: &str) -> (usize, usize) {
    (2, 1)
}

/// Without the feature, every audio blob is exactly one `file` node.
#[cfg(not(feature = "audio-metadata"))]
fn expected_shape(_name: &str) -> (usize, usize) {
    (1, 0)
}

/// An audio blob is a first-class `file` node whatever the build: correct key,
/// kind, name, span and byte count, derived provenance. This is what a `sync`
/// stores for every clip, model or no model.
#[test]
fn audio_blobs_become_well_formed_file_nodes() {
    for name in FIXTURES {
        let bytes = fixture(name);
        let path = format!("assets/{name}");
        let facts = Registry::default().extract(&path, "blob-id", &bytes);

        let (nodes, edges) = expected_shape(name);
        assert_eq!(
            facts.nodes.len(),
            nodes,
            "{name}: unexpected node count, got {:?}",
            facts.nodes.iter().map(|n| &n.key).collect::<Vec<_>>(),
        );
        assert_eq!(
            facts.edges.len(),
            edges,
            "{name}: unexpected edge count, got {:?}",
            facts
                .edges
                .iter()
                .map(|e| (&e.src, &e.dst))
                .collect::<Vec<_>>(),
        );

        // The `file` node is emitted first, whatever else joins it.
        let node = &facts.nodes[0];
        assert_eq!(node.kind, NodeKind::File);
        assert_eq!(node.key, format!("file:{path}"));
        assert_eq!(node.name, name);
        assert_eq!(node.path.as_deref(), Some(path.as_str()));
        assert_eq!(node.lang, None, "{name}: audio carries no language");
        assert_eq!(node.provenance, Provenance::Derived);
        assert_eq!(
            node.meta["bytes"].as_u64(),
            u64::try_from(bytes.len()).ok(),
            "{name}: byte count must be the real size",
        );
        let span = node.span.expect("audio file node carries a span");
        assert_eq!(
            u64::from(span.end),
            u64::try_from(bytes.len()).expect("fixture fits u64")
        );
    }
}

/// **The regression test for #300.** No `meta.content` is embedded for an audio
/// blob — with the `audio` toggle *on*, which is the default, in whatever build
/// this is compiled as, on a machine with or without the model installed.
///
/// The old form of this test could only assert absence while the toggle was off,
/// because with it on a model-less machine and a confabulating one were
/// indistinguishable. The move to an artifact store removes the ambiguity: a
/// transcript is not a derived fact, so extraction must not produce one, ever.
/// A regression that reconnected the ASR path to `file_node` would fail here on
/// CI rather than only on a developer's laptop.
///
/// The assertion is absence, not "content that contains no mojibake": the weaker
/// form would pass while a transcript was being embedded, which is precisely the
/// failure being guarded.
///
/// **It is specifically the `file` node that stays empty**, and it stays empty in
/// an `audio-metadata` build too. ADR-0016 adds container facts on a *separate*
/// `audio_stream` node precisely so this slot — the one ADR-0015 emptied — is not
/// reoccupied by anything, and so "an audio file node with content" remains an
/// unambiguous bug rather than a thing that now has two possible causes.
#[test]
fn extraction_never_embeds_content_for_the_audio_file_node() {
    for name in FIXTURES {
        let bytes = fixture(name);
        for ingest in [
            IngestConfig::default(),
            IngestConfig {
                audio: false,
                ..IngestConfig::default()
            },
        ] {
            let facts = Registry::new(ingest).extract(&format!("assets/{name}"), "blob-id", &bytes);
            let file = facts
                .nodes
                .iter()
                .find(|n| n.kind == NodeKind::File)
                .expect("every audio blob yields a file node");
            let content = file.meta.get("content");
            assert!(
                content.is_none(),
                "{name}: extraction embedded content on the audio *file* node (audio toggle = \
                 {}): {content:?}",
                ingest.audio,
            );
        }
    }
}

/// Extraction is a deterministic pure function of `(path, blob id, bytes)` — the
/// core provenance invariant. Binary blobs are the interesting case: nothing
/// about a clip may vary run to run.
///
/// The comparison is over the whole `FactSet`, not just its nodes: determinism is
/// claimed for everything extraction emits, so a change that perturbed edge order
/// would otherwise slip through. And the set is asserted non-empty first, because
/// two empty fact sets compare equal — a regression that stopped emitting facts
/// for audio entirely would make this test *pass*.
#[test]
fn audio_extraction_is_deterministic() {
    for name in FIXTURES {
        let bytes = fixture(name);
        let path = format!("assets/{name}");
        let once = Registry::default().extract(&path, "blob-id", &bytes);
        let twice = Registry::default().extract(&path, "blob-id", &bytes);
        assert!(
            !once.nodes.is_empty(),
            "{name}: extraction emitted nothing, so equality below would be vacuous",
        );
        assert_eq!(once, twice, "{name}: extraction is not deterministic");
    }
}

/// The `audio` toggle must **no longer** move the extraction cache key.
///
/// It used to have to, because it changed what went into `meta.content`. It no
/// longer changes any derived fact — it gates whether `roteiro media build` may
/// invoke a model — so folding it into the key would force a full, pointless
/// re-extraction on every repository that sets `[ingest] audio = false`,
/// including this one. The inversion of the old assertion is deliberate and is
/// recorded here rather than left as a silent behaviour change.
#[test]
fn the_audio_toggle_no_longer_moves_the_extraction_cache_key() {
    let bytes = fixture("tone-500hz-16khz-mono-256ms.wav");
    let off = Registry::new(IngestConfig {
        audio: false,
        ..IngestConfig::default()
    });
    assert!(
        off.extract("assets/clip.wav", "blob-id", &bytes).nodes[0]
            .meta
            .get("content")
            .is_none(),
        "no transcript may be embedded",
    );
    assert_eq!(
        off.env_tag(),
        Registry::default().env_tag(),
        "`audio` gates generation, not extraction, so it must not move the cache key",
    );
}

/// An oversized clip must be **refused**, not truncated to the cap and
/// transcribed in part: a partial transcript presented as a whole one is exactly
/// the kind of quietly-wrong claim this store exists to prevent.
///
/// The cap moved with the generation: it is now applied while `media build`
/// *enumerates* candidates, which is strictly better — the blob is excluded
/// before the projector loads, in every build, rather than inside a
/// feature-gated function. The blob is built here rather than committed (50 MiB
/// of fixture would dwarf the repository), and the node must still record its
/// true size.
#[test]
fn an_oversized_clip_is_refused_rather_than_truncated() {
    /// One byte over the cap `media build` applies while enumerating candidates.
    /// Derived from the constant rather than restated, so the two cannot drift.
    const OVER_CAP: usize = rto_graph::media::MAX_AUDIO_BYTES + 1;

    let mut bytes = fixture("silence-16khz-mono-256ms.wav");
    bytes.resize(OVER_CAP, 0);
    let facts = Registry::default().extract("assets/huge.wav", "blob-id", &bytes);

    let node = &facts.nodes[0];
    assert!(
        node.meta.get("content").is_none(),
        "no transcript at all, whole or partial",
    );
    assert_eq!(
        node.meta["bytes"].as_u64(),
        u64::try_from(OVER_CAP).ok(),
        "the node must record the clip's real size",
    );
    // "Not truncated" literally: the span still covers the whole blob rather than
    // stopping at the cap.
    let span = node.span.expect("the node carries a span");
    assert_eq!(
        u64::from(span.end),
        u64::try_from(OVER_CAP).expect("fits u64"),
        "the span must cover the whole clip, not stop at the cap",
    );
}

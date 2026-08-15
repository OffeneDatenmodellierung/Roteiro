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

/// An audio blob is a first-class `file` node whatever the build: correct key,
/// kind, name, span and byte count, derived provenance. This is what a `sync`
/// stores for every clip, model or no model.
#[test]
fn audio_blobs_become_well_formed_file_nodes() {
    for name in FIXTURES {
        let bytes = fixture(name);
        let path = format!("assets/{name}");
        let facts = Registry::default().extract(&path, "blob-id", &bytes);

        // Exactly one node and no edges — not "at least one file node somewhere
        // in the set", which would also hold if extraction started emitting
        // spurious facts for a binary blob.
        assert_eq!(
            facts.nodes.len(),
            1,
            "{name}: an audio blob yields exactly one node, got {:?}",
            facts.nodes.iter().map(|n| &n.key).collect::<Vec<_>>(),
        );
        assert!(facts.edges.is_empty(), "{name}: an audio blob has no edges");

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
#[test]
fn extraction_never_embeds_content_for_audio() {
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
            let content = facts.nodes[0].meta.get("content");
            assert!(
                content.is_none(),
                "{name}: extraction embedded content for audio (audio toggle = {}): {content:?}",
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

/// Real generation. Needs the `audio-transcribe` feature, the Voxtral GGUF on
/// disk, and several gigabytes of RAM, so these are `#[ignore]`d — CI compiles
/// them under `--all-features` but never runs them, and a developer who runs them
/// without the model gets a skip message rather than a failure.
///
/// ```text
/// roteiro model pull voxtral-mini-3b
/// cargo test -p rto-graph --features audio-transcribe --test audio_ingest \
///   -- --ignored --nocapture --test-threads=1
/// ```
///
/// `--test-threads=1` matters: each test builds and releases its own engine, and
/// running them concurrently holds two copies of a 3 GB model in memory at once.
#[cfg(feature = "audio-transcribe")]
mod generation {
    use super::fixture;
    use rto_graph::media::producers;
    use rto_graph::{MediaBuildOptions, MediaKind, model_dir, release_media_engines};

    /// Registry name of the audio model (`~/.roteiro/models/<name>/`).
    const MODEL: &str = "voxtral-mini-3b";

    /// `true` when the model is installed; prints the skip line and returns
    /// `false` otherwise.
    fn model_present() -> bool {
        let dir = model_dir(MODEL);
        if dir.join("model.gguf").exists() && dir.join("mmproj.gguf").exists() {
            return true;
        }
        eprintln!("SKIP: `{MODEL}` not installed (run `roteiro model pull {MODEL}`)");
        false
    }

    /// The audio-only build options these tests drive.
    fn audio_only() -> MediaBuildOptions {
        MediaBuildOptions {
            audio: true,
            vision: false,
            ..MediaBuildOptions::default()
        }
    }

    /// End-to-end: every committed fixture goes through the real projector, in
    /// all three formats, and comes back with text — but into the artifact store's
    /// producer, never into `meta.content`.
    ///
    /// The assertions are about the *path*, not the words. What a 3B model hears
    /// in a 500 Hz tone is not a contract — and, as the printed output shows, what
    /// it hears is confident invented prose, which is the entire reason ADR-0015
    /// moved it out of the graph. What *is* a contract is that a WAV and a FLAC of
    /// the same samples decode to the same PCM and so, at temperature 0,
    /// transcribe identically. That check is what proves the hand-written FLAC
    /// encoder is right rather than merely well-formed, and it would catch a
    /// decoder regression in either format.
    #[test]
    #[ignore = "needs the Voxtral GGUF on disk; slow and several GB of RAM"]
    fn every_fixture_format_reaches_the_projector() {
        if !model_present() {
            return;
        }
        let built = producers::installed(audio_only()).expect("the model is installed");
        let producer = built.first().expect("one audio producer");
        assert_eq!(producer.producer().kind, MediaKind::Audio);

        let mut transcripts = std::collections::BTreeMap::new();
        for name in super::FIXTURES {
            let out = producer
                .generate(&format!("assets/{name}"), &fixture(name))
                .map(|c| c.text);
            eprintln!("{name}: {out:?}");
            assert!(
                out.is_some(),
                "{name}: reached the projector but yielded no content",
            );
            transcripts.insert(name, out);
        }

        // The two stems that exist in both containers, carrying identical samples.
        for stem in ["silence-16khz-mono-256ms", "tone-500hz-16khz-mono-256ms"] {
            assert_eq!(
                transcripts[&*format!("{stem}.wav")],
                transcripts[&*format!("{stem}.flac")],
                "{stem}: the WAV and FLAC hold the same samples, so they must decode — and \
                 therefore transcribe — identically",
            );
        }

        // Equality alone is satisfiable by a decoder that returns silence for
        // everything — every clip would then transcribe the same, and every pair
        // above would match. Different signals must reach the model *differently*.
        assert_ne!(
            transcripts["silence-16khz-mono-256ms.wav"],
            transcripts["tone-500hz-16khz-mono-256ms.wav"],
            "silence and a tone transcribed identically, which means the samples are not \
             reaching the model — a decode that yields silence for everything would pass \
             every other assertion here",
        );

        drop(built);
        assert!(
            release_media_engines(),
            "the ASR engine must be releasable, not leaked to exit (#291)",
        );
    }

    /// The installed producer's identity must name what actually produced the
    /// text: the registry model, its pinned digest, and the quantisation read
    /// from the pin. Only observable with the model on disk, which is why it
    /// lives behind the gate.
    #[test]
    #[ignore = "needs the Voxtral GGUF on disk"]
    fn the_producer_identity_names_the_installed_model() {
        if !model_present() {
            return;
        }
        let built = producers::installed(audio_only()).expect("installed");
        let identity = built.first().expect("one producer").producer();
        assert_eq!(identity.model, MODEL);
        assert_eq!(identity.quantisation, "Q4_K_M");
        assert!(!identity.model_digest.is_empty());
        assert!(!identity.mmproj_digest.is_empty());
        assert_ne!(
            identity.model_digest, identity.mmproj_digest,
            "the weights and the projector are different files",
        );
        identity.validate().expect("a well-formed identity");
        eprintln!("producer: {}", identity.id());

        drop(built);
        assert!(release_media_engines(), "the ASR engine must be releasable");
    }
}

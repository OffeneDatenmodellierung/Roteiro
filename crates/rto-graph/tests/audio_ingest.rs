//! Behavioural cover for the audio ingestion path (`extract.rs`'s `is_audio` /
//! `audio_content` / `asr_content`) using the committed fixtures.
//!
//! # Why this splits into two tiers
//!
//! Audio ingestion is inert without a model: `asr_content` returns `None` when
//! the Voxtral GGUF is not installed, exactly as it does when the clip is over
//! the size cap or the `audio` toggle is off. So on a machine with no model —
//! which is every CI runner, since the model store is not provisioned there —
//! the *presence* of a transcript cannot distinguish those cases.
//!
//! The suite is therefore split:
//!
//! * **Always-on tests** assert what holds in any build: an audio blob yields a
//!   well-formed `file` node, binary bytes are never smuggled into `content` as
//!   lossy UTF-8, an oversized clip is refused rather than truncated, and
//!   flipping the `audio` toggle moves the extraction cache key so stale
//!   content-free facts cannot be served after the toggle changes. These need
//!   no feature and no model, and are the coverage that actually protects the
//!   path on every run. They also never *invoke* a model — see [`no_audio`] —
//!   so `cargo test --all-features` stays fast even where one is installed.
//! * **Model-gated tests** ([`transcription`]) exercise real transcription and
//!   the extension classification that only becomes observable once a model can
//!   produce content. They are `#[ignore]`d and additionally self-skip with a
//!   visible message when the model is absent, following the pattern #292
//!   established for the vision teardown test.

use rto_graph::{Extractor, IngestConfig, NodeKind, Provenance, Registry};

/// A fixture's bytes, by file name.
fn fixture(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("audio")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()))
}

/// A registry with the `audio` toggle off, used by every always-on test.
///
/// This matters for more than tidiness. CI runs `--all-features`, so
/// `audio-transcribe` is compiled in; on a *developer* machine that also has the
/// model installed, a `Registry::default()` here would load three gigabytes and
/// transcribe six clips on every `cargo test --workspace --all-features`. These
/// tests are about the model-independent shape of the facts, so they say so, and
/// the behaviour that genuinely needs a model lives in [`transcription`].
fn no_audio() -> Registry {
    Registry::new(IngestConfig {
        audio: false,
        ..IngestConfig::default()
    })
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

/// `is_audio` accepts exactly `wav`, `mp3` and `flac`, so the fixture set must
/// cover all three — otherwise a format could quietly stop decoding and nothing
/// here would notice.
#[test]
fn the_fixture_set_covers_every_accepted_extension() {
    for ext in ["wav", "mp3", "flac"] {
        assert!(
            FIXTURES.iter().any(|f| f.ends_with(ext)),
            "no fixture covers the `{ext}` branch of `is_audio`",
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
        let facts = no_audio().extract(&path, "blob-id", &bytes);

        let node = facts
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::File)
            .unwrap_or_else(|| panic!("{name}: no file node"));
        assert_eq!(node.key, format!("file:{path}"));
        assert_eq!(node.name, name);
        assert_eq!(node.path.as_deref(), Some(path.as_str()));
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

/// Audio must never reach `meta.content` as text. Without a model there is no
/// transcript, and the fallback must be *no content* — not the blob decoded as
/// lossy UTF-8, which would poison the embedding with mojibake and, for the
/// silent fixtures, with kilobytes of NUL.
#[test]
fn audio_bytes_are_never_embedded_as_lossy_text() {
    for name in FIXTURES {
        let bytes = fixture(name);
        let facts = no_audio().extract(&format!("assets/{name}"), "blob-id", &bytes);
        let node = &facts.nodes[0];
        if let Some(content) = node.meta.get("content").and_then(serde_json::Value::as_str) {
            assert!(
                !content.contains('\u{fffd}') && !content.contains('\0'),
                "{name}: raw audio bytes leaked into meta.content",
            );
        }
    }
}

/// Extraction is a deterministic pure function of `(path, blob id, bytes)` — the
/// core provenance invariant. Binary blobs are the interesting case: nothing
/// about a clip may vary run to run.
#[test]
fn audio_extraction_is_deterministic() {
    for name in FIXTURES {
        let bytes = fixture(name);
        let path = format!("assets/{name}");
        let once = no_audio().extract(&path, "blob-id", &bytes);
        let twice = no_audio().extract(&path, "blob-id", &bytes);
        assert_eq!(
            serde_json::to_string(&once.nodes).expect("serialize"),
            serde_json::to_string(&twice.nodes).expect("serialize"),
            "{name}: extraction is not deterministic",
        );
    }
}

/// Turning the `audio` ingest toggle off must produce no content — and, just as
/// importantly, must move the extraction cache key, so a sync after the toggle
/// change re-extracts instead of serving the transcript it cached while the
/// toggle was on.
///
/// The cache key is the assertion with teeth here: "no content" is also what a
/// model-less machine produces, but a *changed* `env_tag` is observable
/// everywhere and is what actually keeps stale content from being served.
#[test]
fn disabling_the_audio_toggle_yields_no_content_and_moves_the_cache_key() {
    let bytes = fixture("tone-500hz-16khz-mono-256ms.wav");
    let off = no_audio();
    let facts = off.extract("assets/clip.wav", "blob-id", &bytes);
    assert!(
        facts.nodes[0].meta.get("content").is_none(),
        "the `audio` toggle is off, so no transcript may be embedded",
    );
    assert_ne!(
        off.env_tag(),
        Registry::default().env_tag(),
        "toggling `audio` off must change the cache key so cached transcripts are not reused",
    );
}

/// A clip over `MAX_AUDIO_BYTES` (50 MiB) must be **refused**, not truncated to
/// the cap and transcribed in part: a partial transcript presented as a whole
/// one is a derived fact that is quietly wrong.
///
/// The blob is built here rather than committed — 50 MiB of fixture would dwarf
/// the repository — and the node must still record its true size.
///
/// This is the one always-on test that deliberately runs with the `audio` toggle
/// **on**, because the cap is what has to do the refusing: it costs nothing on a
/// model-less machine, and on one with the model installed it is the assertion's
/// whole point — a 3 GB transcription must *not* start.
#[test]
fn an_oversized_clip_is_refused_rather_than_truncated() {
    /// `MAX_AUDIO_BYTES` in `extract.rs`, plus one byte.
    const OVER_CAP: usize = 50 * 1024 * 1024 + 1;

    let mut bytes = fixture("silence-16khz-mono-256ms.wav");
    bytes.resize(OVER_CAP, 0);
    let facts = Registry::default().extract("assets/huge.wav", "blob-id", &bytes);

    let node = &facts.nodes[0];
    assert!(
        node.meta.get("content").is_none(),
        "a clip over the cap must yield no transcript at all, not a partial one",
    );
    assert_eq!(
        node.meta["bytes"].as_u64(),
        u64::try_from(OVER_CAP).ok(),
        "the node must record the clip's real size even though it was not transcribed",
    );
}

/// Real transcription. Needs the `audio-transcribe` feature, the Voxtral GGUF on
/// disk, and several gigabytes of RAM, so these are `#[ignore]`d — CI compiles
/// them under `--all-features` but never runs them, and a developer who runs
/// them without the model gets a skip message rather than a failure.
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
mod transcription {
    use super::fixture;
    use rto_graph::{Extractor, IngestConfig, Registry, model_dir, release_media_engines};

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

    /// The transcript a fixture extracts to, or `None` when it embedded no
    /// content. Drives the production path — this is exactly what `sync` does
    /// for an audio blob.
    fn transcribe(path: &str, name: &str) -> Option<String> {
        let facts = Registry::new(IngestConfig::default()).extract(path, "blob-id", &fixture(name));
        facts.nodes[0]
            .meta
            .get("content")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned)
    }

    /// End-to-end: every committed fixture goes through the real projector, in
    /// all three formats, and comes back with content.
    ///
    /// The assertions are about the *path*, not the words. What a 3B model hears
    /// in a 500 Hz tone is not a contract — and, as the printed transcripts show,
    /// what it hears is confident invented prose, which is why this repository
    /// sets `[ingest] audio = false` on itself. What *is* a contract is that a
    /// WAV and a FLAC of the same samples decode to the same PCM and so, at
    /// temperature 0, transcribe identically. That check is what proves the
    /// hand-written FLAC encoder is right rather than merely well-formed, and it
    /// would catch a decoder regression in either format.
    #[test]
    #[ignore = "needs the Voxtral GGUF on disk; slow and several GB of RAM"]
    fn every_fixture_format_reaches_the_projector() {
        if !model_present() {
            return;
        }
        let mut transcripts = std::collections::BTreeMap::new();
        for name in super::FIXTURES {
            let out = transcribe(&format!("assets/{name}"), name);
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

        assert!(
            release_media_engines(),
            "the ASR engine must be releasable, not leaked to exit (#291)",
        );
    }

    /// The classification `is_audio` performs is only observable once a model can
    /// produce content: identical bytes under an accepted extension transcribe,
    /// and under a rejected one do not. Without a model both are `None` and the
    /// distinction is invisible, which is why this test lives behind the gate.
    #[test]
    #[ignore = "needs the Voxtral GGUF on disk; slow and several GB of RAM"]
    fn only_accepted_extensions_are_transcribed() {
        if !model_present() {
            return;
        }
        let name = "syllables-16khz-mono-512ms.wav";
        let accepted = transcribe("assets/clip.wav", name);
        eprintln!("as .wav: {accepted:?}");
        assert!(
            accepted.is_some(),
            "a `.wav` clip must transcribe when the model is installed",
        );
        for rejected in ["assets/clip.ogg", "assets/clip.m4a", "assets/clip.wav.bak"] {
            assert!(
                transcribe(rejected, name).is_none(),
                "{rejected}: `is_audio` accepts only wav/mp3/flac, so this must not transcribe",
            );
        }
        assert!(release_media_engines(), "the ASR engine must be releasable");
    }
}

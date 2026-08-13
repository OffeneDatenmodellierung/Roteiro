//! Extraction: turning the bytes of a source blob into a [`FactSet`].
//!
//! Extraction must be a deterministic pure function of `(path, blob_id, bytes)`
//! so its output can be cached; because the facts are path-dependent (node keys
//! are path-scoped), the cache is keyed by both path and blob id (see
//! [`crate::sync`]). [`Registry`] dispatches by file extension to a
//! language-aware extractor ([`RustExtractor`]), falling back to
//! [`FileNodeExtractor`] for files with no registered language.
//!
//! Language extractors emit `defines`/`contains`/`imports` edges directly, and
//! record each function's callee names in the caller node's `meta.calls`. Call
//! *edges* are resolved later, at assembly time, once every file's symbols are
//! known (see [`crate::sync`]) — a single blob cannot resolve cross-file calls.

use crate::{Edge, EdgeKind, FactSet, Node, NodeKind, Provenance, Span};

/// Version of the extraction *output* (node/edge shape and captured `meta`).
/// Bump whenever extraction changes what it produces, so the content-addressed
/// cache (keyed by blob oid + path) does not serve stale facts for an unchanged
/// blob — the version is folded into the cache key. See [`crate::sync`].
///
/// The `pdf-text`, `image-ocr`, `image-vision`, and `audio-transcribe` features
/// change what PDFs/images/audio extract to, so each occupies a distinct version
/// namespace: a feature build and a default build never serve each other stale
/// (content-bearing vs content-free) facts from a shared cache. (Image/audio
/// output also depends on *which* models are installed; that runtime state is
/// folded into the cache key separately — see [`media_env_tag`] and
/// [`crate::sync`].)
// Bumped 5 → 6 for config-key nodes (ADR-0009): config files now emit
// `config_key` nodes, so cached extraction facts must be regenerated. Bumped
// 6 → 7 for YAML config keys + Dockerfile `image_ref` nodes (ADR-0009 derived
// deploy-artifact extraction). Bumped 7 → 8 for struct `meta.fields` (the named
// field list a struct declares) — the signal the config_key→struct follow bridge
// joins on, so cached struct facts must be regenerated to carry it. Bumped 8 → 9
// for struct `meta.field_types` / `meta.config_root` and the `config_key` nodes
// synthesized from a `@rto:config`-marked config-root struct's declared fields
// (see [`RustWalk::synthesize_config_keys`]), so cached facts regenerate to carry
// these new nodes/meta.
pub(crate) const EXTRACT_VERSION: u32 = 9
    + if cfg!(feature = "pdf-text") { 100 } else { 0 }
    + if cfg!(feature = "image-ocr") { 200 } else { 0 }
    + if cfg!(feature = "image-vision") {
        400
    } else {
        0
    }
    + if cfg!(feature = "audio-transcribe") {
        800
    } else {
        0
    };

/// Max characters of embeddable content (markdown body / doc-comment / PDF text)
/// captured into a node's `meta.content`, to keep the store small while giving
/// inference real text to embed.
const MAX_CONTENT: usize = 1500;

/// PDFs larger than this are not text-extracted — `pdf-extract` builds the full
/// document text in memory, so cap the work a pathological file can impose.
#[cfg(feature = "pdf-text")]
const MAX_PDF_BYTES: usize = 20 * 1024 * 1024;

/// Images larger than this (compressed bytes) are not processed (OCR/VLM).
#[cfg(any(feature = "image-ocr", feature = "image-vision"))]
const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;

/// Images with more pixels than this are not processed — OCR/VLM time scales with
/// pixel count, and this also guards against decompression bombs (the dimension is
/// read from the header before the pixels are decoded).
#[cfg(any(feature = "image-ocr", feature = "image-vision"))]
const MAX_IMAGE_PIXELS: u64 = 4096 * 4096;

/// When OCR yields fewer than this many words, the image is treated as
/// text-sparse (a diagram/photo rather than a text screenshot), so the vision
/// model is run to describe it (only when `image-vision` is also enabled).
#[cfg(feature = "image-vision")]
const MIN_OCR_WORDS: usize = 8;

/// Audio files larger than this (compressed bytes) are not transcribed — decode
/// + inference time scales with duration, so cap the work a single clip imposes.
#[cfg(feature = "audio-transcribe")]
const MAX_AUDIO_BYTES: usize = 50 * 1024 * 1024;

/// Turns one source blob into the nodes and edges derived from it.
pub trait Extractor {
    /// Extract a [`FactSet`] from a blob's `path`, git `blob_id`, and `bytes`.
    ///
    /// Implementations must be deterministic: identical inputs must always
    /// produce an identical fact set.
    fn extract(&self, path: &str, blob_id: &str, bytes: &[u8]) -> FactSet;

    /// Runtime inputs — beyond `(path, bytes)` — that change extraction output
    /// and so must be folded into the sync cache key: the installed media-model
    /// identity (OCR + vision + audio) and any [`IngestConfig`] toggles. The
    /// default is the media-model tag alone; [`Registry`] additionally folds in
    /// its ingestion config so toggling content off re-extracts affected blobs
    /// instead of serving stale, content-bearing facts.
    fn env_tag(&self) -> u64 {
        media_env_tag()
    }
}

/// Runtime ingestion toggles (ADR-0007 `[ingest]`): which blob content is
/// extracted for embedding. Every toggle defaults to **on**, and a toggle only
/// gates content *within a build that supports it* — turning `pdf` on cannot
/// extract PDF text in a binary built without the `pdf-text` feature, but
/// turning it off suppresses that content in a binary that has it.
// Four independent content toggles: a flat bool-per-class struct is the clearest
// representation (a state enum or bitflags would obscure, not clarify).
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngestConfig {
    /// Embed the UTF-8 body of prose files (Markdown, plain text).
    pub prose: bool,
    /// Extract text from PDF documents (needs the `pdf-text` feature).
    pub pdf: bool,
    /// OCR literal text from images (needs the `image-ocr` feature).
    pub ocr: bool,
    /// Describe images with a vision model (needs the `image-vision` feature).
    pub vision: bool,
    /// Transcribe spoken-word audio (needs the `audio-transcribe` feature).
    pub audio: bool,
}

impl Default for IngestConfig {
    fn default() -> Self {
        Self {
            prose: true,
            pdf: true,
            ocr: true,
            vision: true,
            audio: true,
        }
    }
}

impl IngestConfig {
    /// A cache-key contribution that is **`0` when every toggle is on** (the
    /// default), so the common case leaves existing cache keys untouched. Each
    /// disabled toggle sets a distinct bit, so turning content off changes the
    /// key and re-extracts affected blobs.
    fn disabled_bits(self) -> u64 {
        u64::from(!self.prose)
            | (u64::from(!self.pdf) << 1)
            | (u64::from(!self.ocr) << 2)
            | (u64::from(!self.vision) << 3)
            | (u64::from(!self.audio) << 4)
    }
}

/// Dispatches extraction to a language-aware extractor by file extension,
/// falling back to a plain file node when no language is registered. After the
/// language extractor runs, [`crate::markers`] appends any intent-debt markers
/// (intent-debt markers) found in the blob. Carries the runtime
/// [`IngestConfig`] applied to content extraction.
#[derive(Debug, Clone, Copy, Default)]
pub struct Registry {
    /// Which blob content to extract for embedding.
    pub ingest: IngestConfig,
}

impl Registry {
    /// A registry with the given ingestion toggles.
    #[must_use]
    pub fn new(ingest: IngestConfig) -> Self {
        Self { ingest }
    }
}

impl Extractor for Registry {
    fn extract(&self, path: &str, blob_id: &str, bytes: &[u8]) -> FactSet {
        let mut facts = extract_facts(path, blob_id, bytes, self.ingest);
        crate::markers::augment(&mut facts, path, blob_id, bytes);
        facts
    }

    fn env_tag(&self) -> u64 {
        let media = media_env_tag();
        let disabled = self.ingest.disabled_bits();
        if disabled == 0 {
            // All-on default: preserve existing cache keys exactly.
            media
        } else {
            // FNV-1a fold of both components — deterministic and stable. As with
            // any 64-bit hash a collision with the all-on key is possible but
            // vanishingly unlikely, and a collision only costs a spurious cache
            // hit/miss, never incorrect facts.
            let mut h = 0xcbf2_9ce4_8422_2325u64;
            for b in media
                .to_le_bytes()
                .into_iter()
                .chain(disabled.to_le_bytes())
            {
                h ^= u64::from(b);
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
            h
        }
    }
}

/// Shared extraction dispatch used by [`Registry`] and the standalone
/// extractors: pick the language extractor by extension, applying `ingest` to
/// content extraction.
fn extract_facts(path: &str, blob_id: &str, bytes: &[u8], ingest: IngestConfig) -> FactSet {
    // Config files (TOML / JSON / .env) get config-key nodes rather than a plain
    // file node, so their keys are first-class graph nodes (ADR-0009).
    if crate::config_keys::is_config_path(path) {
        return config_facts(path, blob_id, bytes, ingest);
    }
    // Dockerfiles yield `image_ref` nodes (the base-image version pin a spoke
    // deploys) rather than a plain file node (ADR-0009 derived facts).
    if is_dockerfile(path) {
        return dockerfile_facts(path, blob_id, bytes, ingest);
    }
    let ext = extension(path);
    match ext.as_deref() {
        // Rust keeps its dedicated AST walker (imports, impl scoping, richer calls).
        Some("rs") => rust_facts(path, blob_id, bytes, ingest),
        // Every other supported language goes through the generic tags extractor;
        // an unhandled extension (or a query that fails to compile) falls back to
        // a plain file node.
        Some(ext) => tag_facts(path, blob_id, bytes, ext, ingest).unwrap_or_else(|| {
            FactSet::new().with_node(file_node(path, blob_id, bytes, None, ingest))
        }),
        None => FactSet::new().with_node(file_node(path, blob_id, bytes, None, ingest)),
    }
}

/// Lowercase file extension of `path`, if any. Lowercasing makes extension
/// dispatch case-insensitive, so `Guide.PDF` and `README.MD` are recognised.
fn extension(path: &str) -> Option<String> {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
}

/// The natural key of the `file` node for `path`.
fn file_key(path: &str) -> String {
    format!("file:{path}")
}

/// Build the shared `file` node for a source blob. `ingest` gates which content
/// is embedded (ADR-0007 `[ingest]`): a disabled class yields no content, as if
/// the file carried none.
fn file_node(
    path: &str,
    blob_id: &str,
    bytes: &[u8],
    lang: Option<&str>,
    ingest: IngestConfig,
) -> Node {
    let name = path.rsplit('/').next().unwrap_or(path).to_owned();
    let lines = bytes
        .iter()
        .fold(0usize, |n, &b| n + usize::from(b == b'\n'));
    let end = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    let mut meta = serde_json::json!({ "bytes": bytes.len(), "lines": lines });
    // Capture the (capped) body so inference embeds *meaning*, not just the
    // filename: prose files decode as UTF-8; PDFs go through `pdf_content` (only
    // when the `pdf-text` feature is on, otherwise it is a no-op). Each class is
    // gated by its `ingest` toggle so a project can suppress it without a rebuild.
    let content = if ingest.prose && is_prose(path) {
        cap_content(&String::from_utf8_lossy(bytes))
    } else if let Some(text) = ingest.pdf.then(|| pdf_content(path, bytes)).flatten() {
        cap_content(&text)
    } else if let Some(text) = image_content(path, bytes, ingest) {
        cap_content(&text)
    } else if let Some(text) = audio_content(path, bytes, ingest) {
        cap_content(&text)
    } else {
        String::new()
    };
    if !content.is_empty() {
        meta["content"] = serde_json::Value::from(content);
    }
    Node {
        key: file_key(path),
        kind: NodeKind::File,
        name,
        path: Some(path.to_owned()),
        lang: lang.map(ToOwned::to_owned),
        blob_hash: Some(blob_id.to_owned()),
        span: Some(Span::new(0, end)),
        provenance: Provenance::Derived,
        meta,
    }
}

/// Emit config-key facts for a config file (ADR-0009): the `file` node, plus a
/// `config_key` node per flattened leaf — key `cfgkey:<path>#<dotted>`, name the
/// dotted path, `meta` carrying the key and value — with a `contains` edge from
/// the file. Deterministic: keys are de-duplicated (dotenv "last one wins") into
/// a sorted map. Secret-looking values are redacted before they reach the store.
fn config_facts(path: &str, blob_id: &str, bytes: &[u8], ingest: IngestConfig) -> FactSet {
    let mut facts = FactSet::new().with_node(file_node(path, blob_id, bytes, None, ingest));
    let file = file_key(path);
    // A config file that repeats a key yields one node with the final value, and
    // the emission order is deterministic regardless of parse order.
    let mut by_key: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for ck in crate::config_keys::flatten(path, bytes) {
        by_key.insert(ck.key, ck.value);
    }
    for (key, value) in by_key {
        let node_key = format!("cfgkey:{path}#{key}");
        // Redact the value of secret-looking keys so tokens/passwords from
        // `.env`/config files are never persisted into the (exportable) store.
        let value = if crate::config_keys::is_secret_key(&key) {
            "<redacted>".to_owned()
        } else {
            value
        };
        let mut node = Node::new(
            node_key.clone(),
            NodeKind::Other(crate::config_keys::KIND.into()),
            key.clone(),
        );
        node.path = Some(path.to_owned());
        node.blob_hash = Some(blob_id.to_owned());
        node.meta = serde_json::json!({ "key": key, "value": value });
        facts = facts.with_node(node).with_edge(Edge::derived(
            file.clone(),
            node_key,
            EdgeKind::Contains,
        ));
    }
    facts
}

/// The `NodeKind::Other` token for a container base-image reference extracted from
/// a Dockerfile `FROM` (ADR-0009 derived deploy-artifact facts). Its `meta` carries
/// `{image, tag, digest}` — the version pin a spoke deploys.
pub(crate) const IMAGE_REF_KIND: &str = "image_ref";

/// Whether `path` is a Dockerfile/Containerfile (by conventional name):
/// `Dockerfile`, `Containerfile`, `Dockerfile.<x>`, or `*.dockerfile`.
fn is_dockerfile(path: &str) -> bool {
    let base = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    base == "dockerfile"
        || base == "containerfile"
        || base.starts_with("dockerfile.")
        || base.ends_with(".dockerfile")
}

/// Extract each Dockerfile `FROM` external base image into an `image_ref` node
/// (`imageref:<file>#<n>`, `meta {image, tag, digest}`) with a `references` edge
/// from the file — the version pin a deployment spoke ships. Internal multi-stage
/// references (`FROM <prior-stage>`) and `FROM scratch` are skipped.
fn dockerfile_facts(path: &str, blob_id: &str, bytes: &[u8], ingest: IngestConfig) -> FactSet {
    let mut facts = FactSet::new().with_node(file_node(path, blob_id, bytes, None, ingest));
    let file = file_key(path);
    let text = String::from_utf8_lossy(bytes);
    let mut stages: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut idx = 0usize;
    for line in text.lines() {
        let Some(rest) = strip_from_prefix(line.trim()) else {
            continue;
        };
        let (image, stage) = parse_from(rest);
        // Decide whether the image is an earlier stage against the stages seen *so
        // far*, before recording this line's own alias — otherwise `FROM x AS x`
        // would wrongly treat the external image `x` as an internal stage.
        let is_internal_stage = stages.contains(&image.to_ascii_lowercase());
        if let Some(s) = stage {
            stages.insert(s.to_ascii_lowercase());
        }
        // Skip `scratch` and references to an earlier build stage — neither is an
        // external image to pin.
        if image.is_empty() || image.eq_ignore_ascii_case("scratch") || is_internal_stage {
            continue;
        }
        let (name, tag, digest) = split_image(image);
        let node_key = format!("imageref:{path}#{idx}");
        idx += 1;
        let mut node = Node::new(
            node_key.clone(),
            NodeKind::Other(IMAGE_REF_KIND.into()),
            image.to_owned(),
        );
        node.path = Some(path.to_owned());
        node.blob_hash = Some(blob_id.to_owned());
        node.meta = serde_json::json!({ "image": name, "tag": tag, "digest": digest });
        facts = facts.with_node(node).with_edge(Edge::derived(
            file.clone(),
            node_key,
            EdgeKind::References,
        ));
    }
    facts
}

/// The remainder of a `FROM ` line (case-insensitive prefix), or `None`.
fn strip_from_prefix(line: &str) -> Option<&str> {
    let b = line.as_bytes();
    (b.len() >= 5 && b[..4].eq_ignore_ascii_case(b"from") && b[4].is_ascii_whitespace())
        .then(|| line[5..].trim_start())
}

/// Parse a `FROM` argument list into `(image, stage-alias)`: the first non-flag
/// token is the image (leading `--platform=…` flags skipped), and an `AS <name>`
/// suffix names the build stage.
fn parse_from(rest: &str) -> (&str, Option<&str>) {
    let image = rest
        .split_whitespace()
        .find(|t| !t.starts_with("--"))
        .unwrap_or("");
    let mut toks = rest.split_whitespace();
    let mut stage = None;
    while let Some(t) = toks.next() {
        if t.eq_ignore_ascii_case("as") {
            stage = toks.next();
            break;
        }
    }
    (image, stage)
}

/// Split an image reference into `(name, tag, digest)`. A `@sha256:…` digest wins;
/// otherwise a tag is the `:`-suffix *after the last path segment* (so a registry
/// `host:port/` prefix is never mistaken for a tag).
fn split_image(image: &str) -> (String, Option<String>, Option<String>) {
    if let Some((name, digest)) = image.split_once('@') {
        return (name.to_owned(), None, Some(digest.to_owned()));
    }
    let seg = image.rfind('/').map_or(0, |i| i + 1);
    if let Some(colon) = image[seg..].find(':') {
        let at = seg + colon;
        return (
            image[..at].to_owned(),
            Some(image[at + 1..].to_owned()),
            None,
        );
    }
    (image.to_owned(), None, None)
}

/// Strip doc-comment markers from a comment, returning its body — or `None` if
/// it is not a doc comment. Recognises `///` (but not `////`), `//!`, `/** */`,
/// and `/*! */`; a plain `//` or `/* */` comment returns `None`.
fn doc_comment_body(raw: &str) -> Option<String> {
    let t = raw.trim();
    if t.starts_with("//!") || (t.starts_with("///") && !t.starts_with("////")) {
        return Some(t[3..].trim().to_owned());
    }
    if (t.starts_with("/**") || t.starts_with("/*!")) && t.ends_with("*/") {
        // Content lies between the 3-char opener (`/**`/`/*!`) and the 2-char
        // closer (`*/`). Guard the overlap on tiny comments like `/**/`, where
        // the opener and closer share a `*` — those have no body.
        let end = t.len() - 2;
        let inner = if end >= 3 { &t[3..end] } else { "" };
        let cleaned: Vec<&str> = inner
            .lines()
            .map(|l| l.trim().trim_start_matches('*').trim())
            .filter(|l| !l.is_empty())
            .collect();
        return Some(cleaned.join(" "));
    }
    None
}

/// Extract the text of a PDF blob for embedding, or `None` when `path` is not a
/// PDF, the `pdf-text` feature is off, the file is too large, or extraction
/// yields no usable text.
///
/// `pdf-extract` handles fonts/CMaps internally but can panic on some malformed
/// documents; the call is panic-guarded so a bad PDF degrades to a plain file
/// node rather than aborting the whole sync.
#[cfg(feature = "pdf-text")]
fn pdf_content(path: &str, bytes: &[u8]) -> Option<String> {
    if extension(path).as_deref() != Some("pdf") || bytes.len() > MAX_PDF_BYTES {
        return None;
    }
    let owned = bytes.to_vec();
    let text = std::panic::catch_unwind(move || pdf_extract::extract_text_from_mem(&owned).ok())
        .ok()
        .flatten()?;
    (!text.trim().is_empty()).then_some(text)
}

/// No-op when the `pdf-text` feature is off: PDFs become plain file nodes.
#[cfg(not(feature = "pdf-text"))]
fn pdf_content(_path: &str, _bytes: &[u8]) -> Option<String> {
    None
}

/// Embeddable content for an image blob, composing OCR text and an optional
/// vision-model description (see [`ocr_content`]/[`vlm_content`]), or `None` when
/// `path` is not an image, the image is too large, no image model is installed,
/// or nothing is produced.
///
/// Both extractors read the *installed* image models — that runtime dependency is
/// reflected in the cache key via [`media_env_tag`], so installing/upgrading a
/// model re-extracts affected images instead of serving stale (content-free)
/// facts.
#[cfg(any(feature = "image-ocr", feature = "image-vision"))]
fn image_content(path: &str, bytes: &[u8], ingest: IngestConfig) -> Option<String> {
    if !is_image(path) || bytes.len() > MAX_IMAGE_BYTES {
        return None;
    }
    // OCR reads literal text (cheap, accurate); the vision model *describes* the
    // image (slow). Smart composition (ADR-0005): always OCR; run the VLM only
    // when OCR text is sparse — a diagram/photo rather than a text screenshot —
    // and store both when both fire. Each stage is additionally gated by its
    // `ingest` toggle so a project can disable OCR and/or vision at runtime.
    let ocr = if ingest.ocr { ocr_content(bytes) } else { None };
    let sparse = ocr
        .as_deref()
        .is_none_or(|t| t.split_whitespace().count() < min_ocr_words());
    let vision = if ingest.vision && sparse {
        vlm_content(bytes)
    } else {
        None
    };
    match (ocr, vision) {
        (Some(o), Some(v)) => Some(format!("{o}\n\n{v}")),
        (Some(o), None) => Some(o),
        (None, Some(v)) => Some(v),
        (None, None) => None,
    }
}

/// No-op when neither image feature is on: images become plain file nodes.
#[cfg(not(any(feature = "image-ocr", feature = "image-vision")))]
fn image_content(_path: &str, _bytes: &[u8], _ingest: IngestConfig) -> Option<String> {
    None
}

/// The word count below which OCR output is "sparse" enough to invoke the VLM.
/// `usize::MAX` when `image-vision` is off, so the VLM is never triggered.
#[cfg(any(feature = "image-ocr", feature = "image-vision"))]
fn min_ocr_words() -> usize {
    #[cfg(feature = "image-vision")]
    {
        MIN_OCR_WORDS
    }
    #[cfg(not(feature = "image-vision"))]
    {
        usize::MAX
    }
}

/// Whether `path` is an image OCR/vision can read.
#[cfg(any(feature = "image-ocr", feature = "image-vision"))]
fn is_image(path: &str) -> bool {
    matches!(extension(path).as_deref(), Some("png" | "jpg" | "jpeg"))
}

/// Embeddable content for an audio blob: a transcript of its spoken words, or
/// `None` when `path` is not audio, the clip is too large, the `audio` toggle is
/// off, the `audio-transcribe` feature is off, or no model is installed.
///
/// Like the image extractors, this reads the *installed* audio model — that
/// runtime dependency is reflected in the cache key via [`media_env_tag`], so
/// installing/upgrading the model re-transcribes affected clips instead of serving
/// stale (content-free) facts.
#[cfg(feature = "audio-transcribe")]
fn audio_content(path: &str, bytes: &[u8], ingest: IngestConfig) -> Option<String> {
    if !ingest.audio || !is_audio(path) || bytes.len() > MAX_AUDIO_BYTES {
        return None;
    }
    asr_content(bytes)
}

/// No-op when the `audio-transcribe` feature is off: audio files become plain
/// file nodes.
#[cfg(not(feature = "audio-transcribe"))]
fn audio_content(_path: &str, _bytes: &[u8], _ingest: IngestConfig) -> Option<String> {
    None
}

/// Whether `path` is an audio file the projector's miniaudio decoder can read
/// (WAV/MP3/FLAC — the formats llama.cpp bundles support for).
#[cfg(feature = "audio-transcribe")]
fn is_audio(path: &str) -> bool {
    matches!(extension(path).as_deref(), Some("wav" | "mp3" | "flac"))
}

/// Transcribe spoken-word audio with the GGUF audio model (`ASR_MODEL`) through
/// the shared llama.cpp engine (`rto-llama`) — the raw file bytes are decoded and
/// resampled by llama.cpp's bundled miniaudio, so no separate audio-decoding crate
/// is needed. `None` when the model is not installed or generation yields nothing.
#[cfg(feature = "audio-transcribe")]
fn asr_content(bytes: &[u8]) -> Option<String> {
    use rto_llama::Engine as _;

    let engine = asr_engine()?;
    let completion = engine
        .chat(&rto_llama::ChatRequest {
            model: ASR_MODEL.to_owned(),
            messages: vec![rto_llama::Message {
                role: "user".to_owned(),
                content: "Transcribe this audio recording. Output only the spoken words, verbatim."
                    .to_owned(),
            }],
            images: Vec::new(),
            audio: vec![bytes.to_vec()],
            temperature: 0.0,
            max_tokens: 512,
        })
        .ok()?;
    let text = completion.content.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// The GGUF audio model backing `audio-transcribe`.
#[cfg(feature = "audio-transcribe")]
const ASR_MODEL: &str = "voxtral-mini-3b";

/// The process-wide audio engine, built lazily from the installed `ASR_MODEL`
/// (`model.gguf` + audio `mmproj.gguf`). `None` when the model is not installed —
/// transcription is then inert (run `roteiro model pull voxtral-mini-3b`).
#[cfg(feature = "audio-transcribe")]
fn asr_engine() -> Option<&'static rto_llama::llama::LlamaEngine> {
    use std::sync::OnceLock;
    static ENGINE: OnceLock<Option<rto_llama::llama::LlamaEngine>> = OnceLock::new();
    ENGINE
        .get_or_init(|| {
            let dir = crate::models::model_dir(ASR_MODEL);
            let (gguf, mmproj) = (dir.join("model.gguf"), dir.join("mmproj.gguf"));
            if !gguf.exists() || !mmproj.exists() {
                return None;
            }
            rto_llama::llama::LlamaEngine::new(
                vec![rto_llama::llama::Served {
                    name: ASR_MODEL.to_owned(),
                    path: gguf,
                    mmproj: Some(mmproj),
                }],
                0,
            )
            .ok()
        })
        .as_ref()
}

/// Whether the image's pixel dimensions (read from its header, without decoding
/// the pixels — so a decompression bomb is rejected cheaply) are within
/// [`MAX_IMAGE_PIXELS`]. `false` if the header cannot be parsed or the limit is
/// exceeded.
#[cfg(any(feature = "image-ocr", feature = "image-vision"))]
fn image_dimensions_ok(bytes: &[u8]) -> bool {
    let Ok(reader) = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()
    else {
        return false;
    };
    match reader.into_dimensions() {
        Ok((w, h)) => u64::from(w) * u64::from(h) <= MAX_IMAGE_PIXELS,
        Err(_) => false,
    }
}

/// OCR an image's text (or `None` when `image-ocr` is off, the models are not
/// installed, the image is too large, or extraction yields nothing). The `ocrs`
/// engine can panic on some inputs, so the call is panic-guarded.
#[cfg(feature = "image-ocr")]
fn ocr_content(bytes: &[u8]) -> Option<String> {
    let dir = crate::models::model_dir("ocrs-text");
    let detection = dir.join("text-detection.rten");
    let recognition = dir.join("text-recognition.rten");
    if !detection.exists() || !recognition.exists() || !image_dimensions_ok(bytes) {
        // Models not installed → OCR is inert (run `roteiro model pull ocrs-text`).
        return None;
    }
    // Borrow `bytes` into the guarded closure — no need to clone the (up to
    // 20 MiB) image. `&[u8]`/`&Path` are unwind-safe, so no `AssertUnwindSafe`.
    let text = std::panic::catch_unwind(|| run_ocr(&detection, &recognition, bytes))
        .ok()
        .flatten()?;
    (!text.trim().is_empty()).then_some(text)
}

// Only the `any(image-ocr, image-vision)` version of `image_content` calls this,
// so the no-op stub is needed only when that caller is compiled with image-ocr
// off — i.e. image-vision on. Without this narrower gate it would be dead code in
// an audio-only (no image feature) build.
#[cfg(all(feature = "image-vision", not(feature = "image-ocr")))]
fn ocr_content(_bytes: &[u8]) -> Option<String> {
    None
}

/// Run detection + recognition over an image's bytes, returning its text.
/// Fallible steps collapse to `None` (a bad image yields no content).
#[cfg(feature = "image-ocr")]
fn run_ocr(
    detection: &std::path::Path,
    recognition: &std::path::Path,
    bytes: &[u8],
) -> Option<String> {
    use ocrs::{ImageSource, OcrEngine, OcrEngineParams};

    let detection_model = rten::Model::load_file(detection).ok()?;
    let recognition_model = rten::Model::load_file(recognition).ok()?;
    let engine = OcrEngine::new(OcrEngineParams {
        detection_model: Some(detection_model),
        recognition_model: Some(recognition_model),
        ..Default::default()
    })
    .ok()?;

    let img = image::load_from_memory(bytes).ok()?.into_rgb8();
    let source = ImageSource::from_bytes(img.as_raw(), img.dimensions()).ok()?;
    let input = engine.prepare_input(source).ok()?;
    engine.get_text(&input).ok()
}

/// Describe an image with the GGUF vision-language model (`smolvlm-500m-gguf`)
/// through the shared llama.cpp engine (`rto-llama`, ADR-0003 v1.2) — no candle.
/// Returns `None` when `image-vision` is off, the model is not installed, the
/// image is too large, or generation yields nothing. The engine (model +
/// `mmproj`) is loaded once per process and reused across images (a fresh
/// context per call keeps KV cache from carrying over).
#[cfg(feature = "image-vision")]
fn vlm_content(bytes: &[u8]) -> Option<String> {
    use rto_llama::Engine as _;

    if !image_dimensions_ok(bytes) {
        return None;
    }
    let engine = vlm_engine()?;
    let completion = engine
        .chat(&rto_llama::ChatRequest {
            model: VLM_MODEL.to_owned(),
            messages: vec![rto_llama::Message {
                role: "user".to_owned(),
                content: "Describe this image in one or two sentences.".to_owned(),
            }],
            images: vec![bytes.to_vec()],
            audio: Vec::new(),
            temperature: 0.0,
            max_tokens: 128,
        })
        .ok()?;
    let text = completion.content.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// The GGUF vision-language model backing `image-vision`.
#[cfg(feature = "image-vision")]
const VLM_MODEL: &str = "smolvlm-500m-gguf";

/// The process-wide vision engine, built lazily from the installed
/// `smolvlm-500m-gguf` (`model.gguf` + `mmproj.gguf`). `None` when the model is
/// not installed — vision is then inert (run `roteiro model pull smolvlm-500m-gguf`).
#[cfg(feature = "image-vision")]
fn vlm_engine() -> Option<&'static rto_llama::llama::LlamaEngine> {
    use std::sync::OnceLock;
    static ENGINE: OnceLock<Option<rto_llama::llama::LlamaEngine>> = OnceLock::new();
    ENGINE
        .get_or_init(|| {
            let dir = crate::models::model_dir(VLM_MODEL);
            let (gguf, mmproj) = (dir.join("model.gguf"), dir.join("mmproj.gguf"));
            if !gguf.exists() || !mmproj.exists() {
                return None;
            }
            rto_llama::llama::LlamaEngine::new(
                vec![rto_llama::llama::Served {
                    name: VLM_MODEL.to_owned(),
                    path: gguf,
                    mmproj: Some(mmproj),
                }],
                0,
            )
            .ok()
        })
        .as_ref()
}

// Mirror of the `ocr_content` stub: needed only when the image path is compiled
// (image-ocr on) with image-vision off, not in an audio-only build.
#[cfg(all(feature = "image-ocr", not(feature = "image-vision")))]
fn vlm_content(_bytes: &[u8]) -> Option<String> {
    None
}

/// A cache-key component reflecting the media extractors' runtime environment:
/// `0` when no media feature is on or no models are installed, else a hash of the
/// installed OCR/vision/audio model identities. Folded into the sync cache key so
/// installing/upgrading a model re-extracts affected images/audio instead of
/// serving stale facts (media output is not a pure function of the blob alone).
/// See [`crate::sync`].
///
/// The audio fold is `#[cfg]`-gated on `audio-transcribe`, so an image-only build
/// produces exactly the same tag it did before audio existed — no cache churn.
#[cfg(any(
    feature = "image-ocr",
    feature = "image-vision",
    feature = "audio-transcribe"
))]
pub(crate) fn media_env_tag() -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut any = false;
    #[cfg(feature = "image-ocr")]
    {
        any |= fold_installed_model(&mut hash, "ocrs-text");
    }
    #[cfg(feature = "image-vision")]
    {
        any |= fold_installed_model(&mut hash, "smolvlm-500m-gguf");
    }
    #[cfg(feature = "audio-transcribe")]
    {
        any |= fold_installed_model(&mut hash, "voxtral-mini-3b");
    }
    if any { hash | 1 } else { 0 }
}

/// If model `name` is fully installed, fold its host-variant checksums into
/// `hash` and return `true`. Only the host-selected variant is hashed, so an
/// unrelated platform variant does not perturb this host's tag.
#[cfg(any(
    feature = "image-ocr",
    feature = "image-vision",
    feature = "audio-transcribe"
))]
fn fold_installed_model(hash: &mut u64, name: &str) -> bool {
    let Some(variant) = crate::models::find(name)
        .and_then(|spec| spec.variant_for(crate::models::Platform::host()))
    else {
        return false;
    };
    let dir = crate::models::model_dir(name);
    if !variant.files.iter().all(|f| dir.join(f.name).exists()) {
        return false;
    }
    for file in variant.files {
        for b in file.sha256.bytes() {
            *hash ^= u64::from(b);
            *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    true
}

/// `0` whenever no media feature is compiled in.
#[cfg(not(any(
    feature = "image-ocr",
    feature = "image-vision",
    feature = "audio-transcribe"
)))]
pub(crate) fn media_env_tag() -> u64 {
    0
}

/// Whether `path` is a prose file whose body is worth embedding.
fn is_prose(path: &str) -> bool {
    matches!(
        extension(path).as_deref(),
        Some("md" | "markdown" | "txt" | "rst" | "adoc")
    )
}

/// Trim and cap `text` to [`MAX_CONTENT`] characters (whitespace-collapsed), so
/// stored content stays small and deterministic.
fn cap_content(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(MAX_CONTENT));
    // Track the character count incrementally — `out.chars().count()` per
    // iteration would make this O(n²) on long inputs.
    let mut chars = 0usize;
    let mut last_was_space = true;
    for c in text.chars() {
        if chars >= MAX_CONTENT {
            break;
        }
        if c.is_whitespace() {
            if !last_was_space {
                out.push(' ');
                chars += 1;
                last_was_space = true;
            }
        } else {
            out.push(c);
            chars += 1;
            last_was_space = false;
        }
    }
    out.trim().to_owned()
}

/// Fallback extractor: emits a single `file` node per blob, tagged with its blob
/// hash and basic size metadata. Produces no edges. Used for files with no
/// registered language.
#[derive(Debug, Clone, Copy, Default)]
pub struct FileNodeExtractor;

impl Extractor for FileNodeExtractor {
    fn extract(&self, path: &str, blob_id: &str, bytes: &[u8]) -> FactSet {
        FactSet::new().with_node(file_node(
            path,
            blob_id,
            bytes,
            None,
            IngestConfig::default(),
        ))
    }
}

/// Derived extractor for Rust source, backed by tree-sitter. Emits a `file`
/// node, one symbol node per `fn`/`struct`/`enum`/`trait`/`mod` (and a few
/// others) with `defines`/`contains` edges reflecting lexical nesting, and
/// `imports` edges for `use` declarations. Each function records the (optionally
/// scope-qualified) names it calls in `meta.calls` for later cross-file
/// resolution — see [`RustWalk::callee_name`].
#[derive(Debug, Clone, Copy, Default)]
pub struct RustExtractor;

impl Extractor for RustExtractor {
    fn extract(&self, path: &str, blob_id: &str, bytes: &[u8]) -> FactSet {
        rust_facts(path, blob_id, bytes, IngestConfig::default())
    }
}

/// Extract Rust facts, applying `ingest` to the file node's embedded content.
/// Shared by [`RustExtractor`] (default toggles) and [`Registry`] (its config).
fn rust_facts(path: &str, blob_id: &str, bytes: &[u8], ingest: IngestConfig) -> FactSet {
    let mut parser = tree_sitter::Parser::new();
    // The Rust grammar is compiled in, so this only fails on a version
    // mismatch — a build-time invariant, not a runtime input error.
    if parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .is_err()
    {
        return FactSet::new().with_node(file_node(path, blob_id, bytes, None, ingest));
    }
    let Some(tree) = parser.parse(bytes, None) else {
        return FactSet::new().with_node(file_node(path, blob_id, bytes, None, ingest));
    };

    let mut walk = RustWalk {
        path,
        blob_id,
        src: bytes,
        nodes: vec![file_node(path, blob_id, bytes, Some("rust"), ingest)],
        edges: Vec::new(),
    };
    let root = tree.root_node();
    let mut cursor = root.walk();
    let children: Vec<_> = root.children(&mut cursor).collect();
    for child in children {
        walk.visit(child, &[]);
    }
    // Synthesize `config_key` nodes from any `@rto:config`-marked config-root struct
    // (ADR-0009): a code-defined config becomes matchable dotted keys without a
    // committed `*-example.toml` mirror. Runs after the walk so every struct in the
    // file is available to resolve nested field types.
    walk.synthesize_config_keys(root);

    // Deterministic ordering so the cached fact set is byte-stable regardless of
    // traversal incidentals.
    walk.nodes.sort_by(|a, b| a.key.cmp(&b.key));
    walk.edges
        .sort_by(|a, b| (a.kind.as_str(), &a.src, &a.dst).cmp(&(b.kind.as_str(), &b.src, &b.dst)));
    FactSet {
        nodes: walk.nodes,
        edges: walk.edges,
    }
}

/// One entry on the lexical scope stack: a name segment and, when the scope is
/// itself an emitted symbol, that symbol's key (impl blocks contribute a segment
/// but no node, so their `key` is `None`).
struct Scope {
    seg: String,
    key: Option<String>,
}

/// One declared struct field: its name and the `type_identifier` tokens of its
/// type (outermost first). See [`RustWalk::struct_fields`].
struct FieldDef {
    name: String,
    type_idents: Vec<String>,
}

/// Single-value **transparent** wrappers whose inner type is the "real" field type
/// for config purposes — a `zerobus: Option<ZerobusConfig>` still nests into
/// `ZerobusConfig`. Peeled by [`core_type_name`] / [`recursion_target`].
const TRANSPARENT_WRAPPERS: &[&str] = &[
    "Option", "Box", "Arc", "Rc", "Cow", "RefCell", "Cell", "Mutex", "RwLock",
];

/// **Collection** wrappers: a `Vec<ItemConfig>` / `HashMap<_, _>` field serialises
/// to an array/table keyed by *runtime* index/key, not by nested struct fields, so
/// synthesis stops at the field itself (one leaf key) rather than inventing dotted
/// paths under it. Detecting one anywhere in a field's type makes it a leaf.
const COLLECTION_WRAPPERS: &[&str] = &[
    "Vec", "VecDeque", "HashMap", "BTreeMap", "HashSet", "BTreeSet", "IndexMap",
];

/// The field's **core type name** for `meta.field_types`: the first type token that
/// is not a [`TRANSPARENT_WRAPPERS`] wrapper (so `Option<ZerobusConfig>` →
/// `ZerobusConfig`, `String` → `String`), or the outermost token if a wrapper is
/// all there is. `None` for a type with no identifier (a bare reference, tuple, …).
fn core_type_name(type_idents: &[String]) -> Option<String> {
    type_idents
        .iter()
        .find(|t| !TRANSPARENT_WRAPPERS.contains(&t.as_str()))
        .or_else(|| type_idents.first())
        .cloned()
}

/// The struct name a field should **recurse into**, given the structs known in this
/// file (`known`), or `None` when the field is a config leaf. A collection wrapper
/// anywhere short-circuits to a leaf; transparent wrappers are peeled; the first
/// remaining token nests only if it names a known struct.
fn recursion_target<'a>(
    type_idents: &'a [String],
    known: &std::collections::BTreeMap<String, StructDef>,
) -> Option<&'a str> {
    for t in type_idents {
        if COLLECTION_WRAPPERS.contains(&t.as_str()) {
            return None;
        }
        if TRANSPARENT_WRAPPERS.contains(&t.as_str()) {
            continue;
        }
        return known.contains_key(t).then_some(t.as_str());
    }
    None
}

/// A struct discovered in the file for config synthesis: its fields and whether it
/// carries the `@rto:config` root marker.
struct StructDef {
    fields: Vec<FieldDef>,
    is_root: bool,
}

/// Guard against a pathological or cyclic type graph producing unbounded keys.
const MAX_CONFIG_DEPTH: usize = 16;

/// Recursively expand a config struct into its dotted **leaf** keys. A field that
/// resolves to another known struct ([`recursion_target`]) descends with the field
/// name appended to `prefix`; every other field is a leaf recorded in `out`
/// (first-writer wins, tagged with the originating `root` for provenance). `visited`
/// tracks the current descent path so a cyclic type graph terminates (the cyclic
/// field falls back to a leaf) rather than recursing forever.
fn expand_config_keys(
    table: &std::collections::BTreeMap<String, StructDef>,
    struct_name: &str,
    prefix: &str,
    root: &str,
    visited: &mut std::collections::BTreeSet<String>,
    depth: usize,
    out: &mut std::collections::BTreeMap<String, String>,
) {
    let Some(def) = table.get(struct_name) else {
        return;
    };
    for f in &def.fields {
        let key = if prefix.is_empty() {
            f.name.clone()
        } else {
            format!("{prefix}.{}", f.name)
        };
        match recursion_target(&f.type_idents, table) {
            Some(inner) if depth < MAX_CONFIG_DEPTH && !visited.contains(inner) => {
                visited.insert(inner.to_owned());
                expand_config_keys(table, inner, &key, root, visited, depth + 1, out);
                visited.remove(inner);
            }
            _ => {
                out.entry(key).or_insert_with(|| root.to_owned());
            }
        }
    }
}

/// Accumulating state for a single Rust file walk.
struct RustWalk<'a> {
    path: &'a str,
    blob_id: &'a str,
    src: &'a [u8],
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}

impl RustWalk<'_> {
    /// Visit one AST node under the given lexical scope stack.
    fn visit(&mut self, node: tree_sitter::Node, scope: &[Scope]) {
        match node.kind() {
            "function_item" => self.visit_symbol(node, scope, NodeKind::Fn, true),
            "struct_item" | "union_item" => self.visit_symbol(node, scope, NodeKind::Struct, false),
            "enum_item" => self.visit_symbol(node, scope, NodeKind::Enum, false),
            "trait_item" => self.visit_symbol(node, scope, NodeKind::Trait, false),
            "mod_item" => self.visit_symbol(node, scope, NodeKind::Module, false),
            "type_item" => self.visit_symbol(node, scope, NodeKind::Other("type".into()), false),
            "macro_definition" => {
                self.visit_symbol(node, scope, NodeKind::Other("macro".into()), false);
            }
            "impl_item" => self.visit_impl(node, scope),
            "use_declaration" => self.visit_use(node),
            // Recurse through unnamed structural wrappers (e.g. the top-level
            // `declaration_list` of a module handled in `visit_symbol`).
            _ => self.visit_children(node, scope),
        }
    }

    /// Visit every named child of `node` under the same scope.
    fn visit_children(&mut self, node: tree_sitter::Node, scope: &[Scope]) {
        let mut cursor = node.walk();
        let children: Vec<_> = node.named_children(&mut cursor).collect();
        for child in children {
            self.visit(child, scope);
        }
    }

    /// Emit a symbol node for a named definition, link it to its containing
    /// scope, and recurse into its body for nested definitions.
    fn visit_symbol(
        &mut self,
        node: tree_sitter::Node,
        scope: &[Scope],
        kind: NodeKind,
        collect_calls: bool,
    ) {
        let Some(name) = self.field_text(node, "name") else {
            return self.visit_children(node, scope);
        };
        let qualified = qualify(scope, &name);
        let key = format!("sym:rust:{}#{qualified}", self.path);

        let mut meta = serde_json::Map::new();
        if collect_calls {
            let mut calls = Vec::new();
            self.collect_calls(node, &mut calls);
            calls.sort();
            calls.dedup();
            if !calls.is_empty() {
                meta.insert("calls".into(), serde_json::Value::from(calls));
            }
        }
        // Capture the item's doc-comment so inference embeds what it *means*.
        if let Some(doc) = self.doc_comment(node) {
            meta.insert("content".into(), serde_json::Value::from(doc));
        }
        // A struct/union records its NAMED field identifiers in `meta.fields` — the
        // signal the config_key→struct follow bridge joins on (a dotted config key's
        // leaf, e.g. `serve.addr`'s `addr`, must be a real field of the matched
        // struct before we bridge to it). Tuple/unit structs have no named fields
        // and add nothing; the key is omitted rather than emitted empty. Alongside,
        // `meta.field_types` maps each named field to its **core type name** (wrapper
        // types like `Option`/`Box` peeled — see [`core_type_name`]) so a later,
        // cross-file synthesizer can descend into nested config structs from the
        // stored graph alone; `meta.config_root` marks a struct authored with the
        // `@rto:config` signal as the root of a config tree (see
        // [`RustWalk::synthesize_config_keys`]).
        if matches!(node.kind(), "struct_item" | "union_item") {
            let defs = self.struct_fields(node);
            if !defs.is_empty() {
                let names: Vec<&str> = defs.iter().map(|f| f.name.as_str()).collect();
                meta.insert("fields".into(), serde_json::Value::from(names));
                let types: serde_json::Map<String, serde_json::Value> = defs
                    .iter()
                    .filter_map(|f| {
                        core_type_name(&f.type_idents).map(|t| (f.name.clone(), t.into()))
                    })
                    .collect();
                if !types.is_empty() {
                    meta.insert("field_types".into(), serde_json::Value::Object(types));
                }
            }
            if self.has_config_marker(node) {
                meta.insert("config_root".into(), serde_json::Value::Bool(true));
            }
        }

        self.nodes.push(Node {
            key: key.clone(),
            kind,
            name,
            path: Some(self.path.to_owned()),
            lang: Some("rust".to_owned()),
            blob_hash: Some(self.blob_id.to_owned()),
            span: Some(span(node)),
            provenance: Provenance::Derived,
            meta: serde_json::Value::Object(meta),
        });
        self.link_parent(&key, scope);

        // Recurse into the body so nested items (a fn in a mod, etc.) are found,
        // pushing this symbol onto the scope stack.
        let child_scope = extend(scope, &self.simple(node, "name"), Some(key));
        self.recurse_body(node, &child_scope);
    }

    /// The doc-comment (`///` / `//!` / `/** … */`) immediately preceding `node`,
    /// concatenated, or `None`. Attributes between the comment and the item are
    /// skipped; a non-doc comment (or any other node) ends the block.
    fn doc_comment(&self, node: tree_sitter::Node) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        let mut prev = node.prev_sibling();
        while let Some(n) = prev {
            match n.kind() {
                "line_comment" | "block_comment" => match doc_comment_body(self.text(n)) {
                    Some(body) => {
                        parts.push(body);
                        prev = n.prev_sibling();
                    }
                    None => break,
                },
                "attribute_item" => prev = n.prev_sibling(),
                _ => break,
            }
        }
        if parts.is_empty() {
            return None;
        }
        parts.reverse();
        let joined = cap_content(&parts.join(" "));
        (!joined.is_empty()).then_some(joined)
    }

    /// An `impl` block emits no node but contributes its type name as a scope
    /// segment, so methods qualify as `Type::method`.
    fn visit_impl(&mut self, node: tree_sitter::Node, scope: &[Scope]) {
        let type_name = self
            .field_text(node, "type")
            .unwrap_or_else(|| "impl".to_owned());
        let child_scope = extend(scope, &type_name, None);
        self.recurse_body(node, &child_scope);
    }

    /// Record a `use` declaration as an `imports` edge from the file to an
    /// import-target node keyed by the (whitespace-normalised) import path.
    fn visit_use(&mut self, node: tree_sitter::Node) {
        let Some(arg) = node.child_by_field_name("argument") else {
            return;
        };
        let text: String = self
            .text(arg)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        if text.is_empty() {
            return;
        }
        let key = format!("import:rust:{text}");
        self.nodes.push(Node {
            key: key.clone(),
            kind: NodeKind::Other("import".into()),
            name: text,
            path: None,
            lang: Some("rust".to_owned()),
            blob_hash: None,
            span: None,
            provenance: Provenance::Derived,
            meta: serde_json::Value::Null,
        });
        self.edges
            .push(Edge::derived(file_key(self.path), key, EdgeKind::Imports));
    }

    /// Link a freshly-emitted symbol to its nearest enclosing emitted scope:
    /// `contains` from that symbol, or `defines` from the file at top level.
    fn link_parent(&mut self, key: &str, scope: &[Scope]) {
        if let Some(parent) = scope.iter().rev().find_map(|s| s.key.as_deref()) {
            self.edges.push(Edge::derived(
                parent.to_owned(),
                key.to_owned(),
                EdgeKind::Contains,
            ));
        } else {
            self.edges.push(Edge::derived(
                file_key(self.path),
                key.to_owned(),
                EdgeKind::Defines,
            ));
        }
    }

    /// The NAMED field identifiers a struct/union declares, in source order — its
    /// `field_declaration_list`'s `field_declaration` names. Tuple structs use an
    /// ordered (positional) field list whose entries carry no `name`, so they
    /// contribute nothing; a unit struct has no field list at all.
    /// The named fields a struct/union declares, each with its declared name and
    /// the type-identifier tokens of its type (outermost first, e.g.
    /// `Option<ZerobusConfig>` → `["Option", "ZerobusConfig"]`). Source order is
    /// preserved. Tuple/unit structs contribute nothing.
    fn struct_fields(&self, node: tree_sitter::Node) -> Vec<FieldDef> {
        let mut out = Vec::new();
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.kind() == "field_declaration_list" {
                let mut inner = child.walk();
                for field in child.named_children(&mut inner) {
                    if field.kind() == "field_declaration"
                        && let Some(name) = field.child_by_field_name("name")
                    {
                        let type_idents = field
                            .child_by_field_name("type")
                            .map(|t| self.type_idents(t))
                            .unwrap_or_default();
                        out.push(FieldDef {
                            name: self.text(name).to_owned(),
                            type_idents,
                        });
                    }
                }
            }
        }
        out
    }

    /// Every `type_identifier` token in a type subtree, outermost first — so a
    /// generic like `Option<Vec<Inner>>` yields `["Option", "Vec", "Inner"]`. The
    /// order lets [`core_type_name`] / [`recursion_target`] peel transparent
    /// wrappers and stop at a collection.
    fn type_idents(&self, ty: tree_sitter::Node) -> Vec<String> {
        let mut out = Vec::new();
        self.collect_type_idents(ty, &mut out);
        out
    }

    fn collect_type_idents(&self, node: tree_sitter::Node, out: &mut Vec<String>) {
        // A named type (`ZerobusConfig`, `String`) or a primitive (`u32`, `bool`) —
        // both are field-type tokens; primitives never name a struct, so they only
        // ever resolve to a leaf, but they make `meta.field_types` complete.
        if matches!(node.kind(), "type_identifier" | "primitive_type") {
            out.push(self.text(node).to_owned());
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.collect_type_idents(child, out);
        }
    }

    /// Whether an authored **`@rto:config`** marker immediately precedes `node` —
    /// the explicit, opt-in signal that a struct is the root of a config tree
    /// [`RustWalk::synthesize_config_keys`] may expand. Accepts the token in a
    /// leading comment (`// @rto:config`, `/// @rto:config`) or attribute, scanning
    /// past intervening attributes/comments exactly as [`doc_comment`] does; any
    /// other node ends the run. Requiring an authored marker keeps synthesis
    /// conservative — a struct is never guessed to be config.
    fn has_config_marker(&self, node: tree_sitter::Node) -> bool {
        const MARKER: &str = "@rto:config";
        let mut prev = node.prev_sibling();
        while let Some(n) = prev {
            match n.kind() {
                "line_comment" | "block_comment" | "attribute_item" => {
                    if self.text(n).contains(MARKER) {
                        return true;
                    }
                    prev = n.prev_sibling();
                }
                _ => break,
            }
        }
        false
    }

    /// Recurse into the `declaration_list` / body of a definition.
    fn recurse_body(&mut self, node: tree_sitter::Node, scope: &[Scope]) {
        let mut cursor = node.walk();
        let children: Vec<_> = node.named_children(&mut cursor).collect();
        for child in children {
            match child.kind() {
                "declaration_list" | "field_declaration_list" | "trait_body" => {
                    self.visit_children(child, scope);
                }
                _ => {}
            }
        }
    }

    /// Collect the simple names of functions called anywhere within `node`'s
    /// subtree (used for later call resolution).
    fn collect_calls(&self, node: tree_sitter::Node, out: &mut Vec<String>) {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.kind() == "call_expression"
                && let Some(func) = child.child_by_field_name("function")
                && let Some(name) = self.callee_name(func)
            {
                out.push(name);
            }
            self.collect_calls(child, out);
        }
    }

    /// A callee descriptor for a `call_expression`'s function child, keeping the
    /// *immediate* qualifier when the syntax supplies one so [`crate::sync`] can
    /// resolve scope-aware (not just by unique simple name):
    /// - `foo()` → `foo` (unqualified)
    /// - `a::b::foo()` → `b::foo` (immediate module/type qualifier)
    /// - `Type::assoc()` → `Type::assoc`
    /// - `self.foo()` / `Self::foo()` → `Self::foo` (a same-impl method call,
    ///   resolved via the caller's own type)
    /// - `x.foo()` on a non-`self` receiver → `foo` (the receiver's type is
    ///   unknown without type inference, so no qualifier is claimed)
    fn callee_name(&self, func: tree_sitter::Node) -> Option<String> {
        match func.kind() {
            "identifier" => Some(self.text(func).to_owned()),
            "scoped_identifier" => {
                let name = func.child_by_field_name("name")?;
                // The immediate qualifier is the last segment of the `path` child
                // (`a::b` → `b`), which most closely scopes the call.
                let qualifier = func
                    .child_by_field_name("path")
                    .and_then(|p| self.text(p).rsplit("::").next().map(str::to_owned));
                Some(qualify_callee(qualifier.as_deref(), self.text(name)))
            }
            "field_expression" => {
                let name = func.child_by_field_name("field")?;
                // A call on the `self` receiver targets a method of the caller's
                // own impl type; mark it `Self` so the resolver can bind it.
                let on_self = func
                    .child_by_field_name("value")
                    .is_some_and(|v| self.text(v) == "self");
                Some(qualify_callee(on_self.then_some("Self"), self.text(name)))
            }
            _ => None,
        }
    }

    /// Synthesize `config_key` nodes from any **config-root** struct in this file —
    /// a struct authored with the `@rto:config` marker (see [`has_config_marker`]).
    /// Its declared fields are walked recursively, descending into nested
    /// struct-typed fields (resolved by name against the other structs in *this
    /// file*), and each config **leaf** becomes a `config_key` node keyed
    /// `cfgkey:<path>#<dotted>` — so a code-defined config (`zerobus: ZerobusConfig`
    /// with `server_endpoint: String`) yields `zerobus.server_endpoint` **without** a
    /// committed `*-example.toml` mirror. The nodes carry `meta.source = "struct"`
    /// (and `meta.struct = <root>`) so they stay distinguishable from file-derived
    /// keys, while sharing the `config_key` kind so they flow through
    /// `Store::config_keys` → `links --infer`/`--matrix`/the explorer unchanged.
    ///
    /// Deliberately conservative and additive: nothing is emitted unless a root is
    /// explicitly marked. Field names are used verbatim as dotted segments; the
    /// cross-convention matcher ([`crate::canonicalize_config_key`]) already bridges
    /// a `snake_case` field to a `camelCase`/`kebab` infra key, so `serde`
    /// `rename_all` conventions match without being parsed here.
    ///
    /// Known limits (documented, deferred): recursion resolves nested structs by
    /// name **within this file only** (a config struct split across modules/files is
    /// not descended — those leaves simply stay unsynthesized, as today); an explicit
    /// `#[serde(rename = "...")]` to an unrelated spelling is not applied; and
    /// collection-typed fields (`Vec`/`Map`) are one leaf, not indexed paths.
    fn synthesize_config_keys(&mut self, root: tree_sitter::Node) {
        let table = self.collect_struct_defs(root);
        // key → the root struct name that produced it (first root wins; deterministic
        // because `table` iterates roots by name).
        let mut keys: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        for (name, def) in &table {
            if !def.is_root {
                continue;
            }
            let mut visited = std::collections::BTreeSet::new();
            visited.insert(name.clone());
            expand_config_keys(&table, name, "", name, &mut visited, 0, &mut keys);
        }
        let file = file_key(self.path);
        for (dotted, root_name) in keys {
            let node_key = format!("cfgkey:{}#{dotted}", self.path);
            let mut node = Node::new(
                node_key.clone(),
                NodeKind::Other(crate::config_keys::KIND.into()),
                dotted.clone(),
            );
            node.path = Some(self.path.to_owned());
            node.blob_hash = Some(self.blob_id.to_owned());
            // No literal value in source; `source`/`struct` mark the provenance and
            // keep these distinguishable from file-derived config keys.
            node.meta = serde_json::json!({
                "key": dotted,
                "value": "",
                "source": "struct",
                "struct": root_name,
            });
            self.edges.push(Edge::derived(
                file.clone(),
                node_key.clone(),
                EdgeKind::Contains,
            ));
            self.nodes.push(node);
        }
    }

    /// Index every struct/union in the file by its **simple name** (first
    /// declaration wins on a collision) for config synthesis — recording its fields,
    /// its node key, and whether it is a `@rto:config` root.
    fn collect_struct_defs(
        &self,
        root: tree_sitter::Node,
    ) -> std::collections::BTreeMap<String, StructDef> {
        let mut out = std::collections::BTreeMap::new();
        self.collect_struct_defs_into(root, &mut out);
        out
    }

    fn collect_struct_defs_into(
        &self,
        node: tree_sitter::Node,
        out: &mut std::collections::BTreeMap<String, StructDef>,
    ) {
        if matches!(node.kind(), "struct_item" | "union_item")
            && let Some(name) = self.field_text(node, "name")
        {
            out.entry(name.clone()).or_insert_with(|| StructDef {
                fields: self.struct_fields(node),
                is_root: self.has_config_marker(node),
            });
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.collect_struct_defs_into(child, out);
        }
    }

    fn text(&self, node: tree_sitter::Node) -> &str {
        node.utf8_text(self.src).unwrap_or("")
    }

    fn field_text(&self, node: tree_sitter::Node, field: &str) -> Option<String> {
        node.child_by_field_name(field)
            .map(|n| self.text(n).to_owned())
    }

    fn simple(&self, node: tree_sitter::Node, field: &str) -> String {
        self.field_text(node, field).unwrap_or_default()
    }
}

// ======================= Generic tags-query extraction =======================
//
// One extractor drives every non-Rust language through its tree-sitter `tags.scm`
// query (the `@definition.*` / `@reference.*` capture convention). It emits the
// same fact shape as the Rust walker — a `file` node, one symbol node per
// definition with `defines`/`contains` edges reflecting byte-range nesting, and
// each function's callee simple-names in `meta.calls` — so cross-file (and
// cross-language) call resolution in `crate::sync` works uniformly. Where the
// language has an import query (`import_query_for`), it also emits `imports`
// edges (`file → import` target), as the Rust walker does for `use`. A new
// language is a row in `tag_lang_for` (and optionally `import_query_for`), not
// new code.

/// A language dispatched to the generic tags extractor: its label, grammar, and
/// `tags.scm` source (from the grammar crate, or vendored under `src/queries/`).
struct TagLang {
    /// Canonical label — the node `lang` and the `sym:<lang>:` key namespace.
    lang: &'static str,
    /// Cache key identifying the *grammar* (not just the label): one `lang` can
    /// map to more than one grammar — OCaml `.ml` and `.mli` are both `"ocaml"`
    /// but use distinct grammars — so the config cache must key on this, not
    /// `lang`, to avoid parsing one grammar's blobs with another's parser.
    grammar_key: &'static str,
    /// The tree-sitter grammar.
    language: tree_sitter::Language,
    /// The `tags.scm` query source. Usually borrowed from the grammar crate's
    /// const; owned when it is assembled (TypeScript's query `inherits` the
    /// JavaScript one, which the crate's `TAGS_QUERY` const does not concatenate).
    query: std::borrow::Cow<'static, str>,
}

/// Resolve a lowercase file extension to its tags-extractor language, or `None`
/// when no generic extractor handles it (the caller then falls back to a plain
/// file node). Rust is intentionally absent — it keeps its richer AST walker.
// A flat extension→grammar dispatch table; length is inherent to the breadth.
#[allow(clippy::too_many_lines)]
fn tag_lang_for(ext: &str) -> Option<TagLang> {
    use std::borrow::Cow;
    // TypeScript's tags query `inherits` JavaScript's; the crate const ships only
    // the TS-specific supplement, so concatenate the two. The JavaScript patterns
    // match against the TypeScript superset grammar.
    let ts_query = || -> Cow<'static, str> {
        Cow::Owned(format!(
            "{}\n{}",
            tree_sitter_javascript::TAGS_QUERY,
            tree_sitter_typescript::TAGS_QUERY
        ))
    };
    let borrowed = |q: &'static str| -> Cow<'static, str> { Cow::Borrowed(q) };

    let (lang, language, query): (&str, tree_sitter::Language, Cow<'static, str>) = match ext {
        "py" | "pyi" => (
            "python",
            tree_sitter_python::LANGUAGE.into(),
            borrowed(tree_sitter_python::TAGS_QUERY),
        ),
        "js" | "jsx" | "mjs" | "cjs" => (
            "javascript",
            tree_sitter_javascript::LANGUAGE.into(),
            borrowed(tree_sitter_javascript::TAGS_QUERY),
        ),
        "ts" | "mts" | "cts" => (
            "typescript",
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            ts_query(),
        ),
        "tsx" => (
            "tsx",
            tree_sitter_typescript::LANGUAGE_TSX.into(),
            ts_query(),
        ),
        "go" => (
            "go",
            tree_sitter_go::LANGUAGE.into(),
            borrowed(tree_sitter_go::TAGS_QUERY),
        ),
        "rb" => (
            "ruby",
            tree_sitter_ruby::LANGUAGE.into(),
            borrowed(tree_sitter_ruby::TAGS_QUERY),
        ),
        "java" => (
            "java",
            tree_sitter_java::LANGUAGE.into(),
            borrowed(tree_sitter_java::TAGS_QUERY),
        ),
        "c" | "h" => (
            "c",
            tree_sitter_c::LANGUAGE.into(),
            borrowed(tree_sitter_c::TAGS_QUERY),
        ),
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => (
            "cpp",
            tree_sitter_cpp::LANGUAGE.into(),
            borrowed(tree_sitter_cpp::TAGS_QUERY),
        ),
        // The crate's TAGS_QUERY has a stray `@module` capture that
        // `tree-sitter-tags` rejects, so a corrected copy is vendored.
        "cs" => (
            "csharp",
            tree_sitter_c_sharp::LANGUAGE.into(),
            borrowed(include_str!("queries/csharp/tags.scm")),
        ),
        "php" => (
            "php",
            tree_sitter_php::LANGUAGE_PHP.into(),
            borrowed(tree_sitter_php::TAGS_QUERY),
        ),
        // Scala's crate bundles a tags.scm but exposes no const, so it is vendored.
        "scala" | "sc" => (
            "scala",
            tree_sitter_scala::LANGUAGE.into(),
            borrowed(include_str!("queries/scala/tags.scm")),
        ),
        "ml" => (
            "ocaml",
            tree_sitter_ocaml::LANGUAGE_OCAML.into(),
            borrowed(tree_sitter_ocaml::TAGS_QUERY),
        ),
        "mli" => (
            "ocaml",
            tree_sitter_ocaml::LANGUAGE_OCAML_INTERFACE.into(),
            borrowed(tree_sitter_ocaml::TAGS_QUERY),
        ),
        "ex" | "exs" => (
            "elixir",
            tree_sitter_elixir::LANGUAGE.into(),
            borrowed(tree_sitter_elixir::TAGS_QUERY),
        ),
        // Bash ships no tags query at all, so one is vendored.
        "sh" | "bash" => (
            "bash",
            tree_sitter_bash::LANGUAGE.into(),
            borrowed(include_str!("queries/bash/tags.scm")),
        ),
        // SQL (tree-sitter-sequel) ships no tags query, so one is vendored.
        "sql" => (
            "sql",
            tree_sitter_sequel::LANGUAGE.into(),
            borrowed(include_str!("queries/sql/tags.scm")),
        ),
        _ => return None,
    };
    // Distinguish grammars that share a `lang` label: `.ml` and `.mli` are both
    // "ocaml" but parse with different grammars, so they must cache separately.
    let grammar_key = match ext {
        "mli" => "ocaml-interface",
        _ => lang,
    };
    Some(TagLang {
        lang,
        grammar_key,
        language,
        query,
    })
}

/// A compiled tags configuration, shared across the blobs of one language.
type TagConfig = std::sync::Arc<tree_sitter_tags::TagsConfiguration>;

/// Cache of compiled tags configurations, keyed by [`TagLang::grammar_key`] (not
/// the `lang` label, since one label can back multiple grammars). Compiling a
/// `tags.scm` query is not free, and `sync` extracts many blobs, so each
/// grammar's configuration is built once. A grammar whose query fails to compile
/// (a grammar/query mismatch — a build-time invariant, not a runtime input)
/// caches `None` so it is not retried per file.
static TAG_CONFIGS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<&'static str, Option<TagConfig>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// The compiled tags configuration for a language, building and caching it on
/// first use. `None` if the query does not compile against the grammar.
fn tag_config(def: &TagLang) -> Option<TagConfig> {
    let mut cache = TAG_CONFIGS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache
        .entry(def.grammar_key)
        .or_insert_with(|| {
            tree_sitter_tags::TagsConfiguration::new(def.language.clone(), &def.query, "")
                .ok()
                .map(std::sync::Arc::new)
        })
        .clone()
}

/// A per-language tree-sitter query capturing import/include targets as `@path`.
/// Run alongside the tags extraction so the generic languages emit `imports`
/// edges (`file → import` node) the way the Rust walker does for `use`. `None`
/// for a language whose imports we do not yet capture (it simply emits none).
///
/// Node names are grammar-specific; a query that fails to compile against its
/// grammar is cached as absent (see [`import_query`]) rather than retried.
fn import_query_for(lang: &str) -> Option<&'static str> {
    Some(match lang {
        // `import a.b.c`, `import a.b as d`, `from a.b import x`, `from . import x`.
        "python" => {
            "(import_statement name: (dotted_name) @path)\n\
             (import_statement name: (aliased_import name: (dotted_name) @path))\n\
             (import_from_statement module_name: (dotted_name) @path)\n\
             (import_from_statement module_name: (relative_import) @path)"
        }
        // `import x from \"mod\"`, `export … from \"mod\"` — the module string.
        "javascript" | "typescript" | "tsx" => {
            "(import_statement source: (string (string_fragment) @path))\n\
             (export_statement source: (string (string_fragment) @path))"
        }
        // Each spec's quoted path inside an `import ( … )` block or single import.
        "go" => "(import_spec path: (interpreted_string_literal) @path)",
        // `import a.b.C;` / `import static a.b.C;`.
        "java" => {
            "(import_declaration (scoped_identifier) @path)\n\
             (import_declaration (identifier) @path)"
        }
        // `#include \"x.h\"` and `#include <x>` (C and, by inheritance, C++).
        "c" | "cpp" => {
            "(preproc_include path: (string_literal) @path)\n\
             (preproc_include path: (system_lib_string) @path)"
        }
        _ => return None,
    })
}

/// A compiled import query, shared across the blobs of one grammar.
type ImportQuery = std::sync::Arc<tree_sitter::Query>;

/// Cache of compiled import queries, keyed by [`TagLang::grammar_key`] (as with
/// [`TAG_CONFIGS`]). `None` when the language has no import query or it does not
/// compile against the grammar, so it is not retried per file.
static IMPORT_QUERIES: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<&'static str, Option<ImportQuery>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// The compiled import query for a language, building and caching it on first use.
fn import_query(def: &TagLang) -> Option<ImportQuery> {
    let mut cache = IMPORT_QUERIES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache
        .entry(def.grammar_key)
        .or_insert_with(|| {
            let src = import_query_for(def.lang)?;
            tree_sitter::Query::new(&def.language, src)
                .ok()
                .map(std::sync::Arc::new)
        })
        .clone()
}

/// Normalise a captured import target to a bare module string: strip surrounding
/// quotes (`"…"`), C system-header brackets (`<…>`), and whitespace.
fn normalize_import(raw: &str) -> String {
    raw.trim()
        .trim_matches(|c| c == '"' || c == '\'' || c == '<' || c == '>')
        .trim()
        .to_owned()
}

/// Append `imports` edges for a blob by running its language's import query.
/// Emits one `import:<lang>:<module>` node (deduped) and a `file → import`
/// `Imports` edge per distinct target, mirroring the Rust walker's `use` handling.
fn append_import_facts(
    path: &str,
    def: &TagLang,
    bytes: &[u8],
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    use streaming_iterator::StreamingIterator as _;

    let Some(query) = import_query(def) else {
        return;
    };
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&def.language).is_err() {
        return;
    }
    let Some(tree) = parser.parse(bytes, None) else {
        return;
    };
    let mut cursor = tree_sitter::QueryCursor::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut matches = cursor.matches(&query, tree.root_node(), bytes);
    while let Some(m) = matches.next() {
        for cap in m.captures {
            let Ok(raw) = cap.node.utf8_text(bytes) else {
                continue;
            };
            let module = normalize_import(raw);
            if module.is_empty() {
                continue;
            }
            let key = format!("import:{}:{module}", def.lang);
            if seen.insert(key.clone()) {
                nodes.push(Node {
                    key: key.clone(),
                    kind: NodeKind::Other("import".into()),
                    name: module,
                    // The import *target* is not owned by any one file (its key is
                    // global): leave `path` unset, as the Rust walker does, so two
                    // files importing the same module dedup to one stable node.
                    path: None,
                    lang: Some(def.lang.to_owned()),
                    blob_hash: None,
                    span: None,
                    provenance: Provenance::Derived,
                    meta: serde_json::Value::Null,
                });
                edges.push(Edge::derived(file_key(path), key, EdgeKind::Imports));
            }
        }
    }
}

/// Map a `tags.scm` syntax type (the tail of a `@definition.X` capture) to a
/// graph node kind. Unrecognised kinds are kept verbatim under `Other`.
fn tag_node_kind(syntax_type: &str) -> NodeKind {
    match syntax_type {
        "function" | "method" | "constructor" => NodeKind::Fn,
        "class" | "struct" => NodeKind::Struct,
        "interface" | "trait" | "protocol" => NodeKind::Trait,
        "enum" => NodeKind::Enum,
        // A Scala/Kotlin `object` is a singleton namespace; group it with modules.
        "module" | "namespace" | "object" => NodeKind::Module,
        other => NodeKind::Other(other.to_owned()),
    }
}

/// A definition captured from a `tags.scm` run, before nesting is resolved.
struct TagDef {
    name: String,
    kind: NodeKind,
    range: std::ops::Range<usize>,
    docs: Option<String>,
}

/// Extract facts from a source blob via its language's tags query. Returns `None`
/// when the extension has no generic extractor or the query cannot compile, so
/// the caller falls back to a plain file node.
fn tag_facts(
    path: &str,
    blob_id: &str,
    bytes: &[u8],
    ext: &str,
    ingest: IngestConfig,
) -> Option<FactSet> {
    let def = tag_lang_for(ext)?;
    let lang = def.lang;
    let config = tag_config(&def)?;

    let mut ctx = tree_sitter_tags::TagsContext::new();
    let (tags, _had_error) = ctx.generate_tags(&config, bytes, None).ok()?;

    let mut defs: Vec<TagDef> = Vec::new();
    // Call references, as (byte offset of the call, callee simple-name), attached
    // later to whichever function definition encloses them.
    let mut calls: Vec<(usize, String)> = Vec::new();
    for tag in tags {
        let Ok(tag) = tag else { continue };
        let Some(name) = bytes
            .get(tag.name_range.clone())
            .and_then(|b| std::str::from_utf8(b).ok())
        else {
            continue;
        };
        let syntax = config.syntax_type_name(tag.syntax_type_id);
        if tag.is_definition {
            defs.push(TagDef {
                name: name.to_owned(),
                kind: tag_node_kind(syntax),
                range: tag.range.clone(),
                // The tags machinery already resolves a definition's doc comment.
                docs: tag.docs.clone(),
            });
        } else if syntax == "call" || syntax == "send" {
            // `send` is Ruby's message-send; both mean "invokes a name".
            calls.push((tag.range.start, name.to_owned()));
        }
    }

    // Resolve nesting purely by byte-range containment: a definition's parent is
    // the smallest other definition whose range strictly encloses it. This yields
    // `contains` edges (parent→child) and qualified, collision-resistant keys
    // without any language-specific scope rules.
    let parents: Vec<Option<usize>> = (0..defs.len())
        .map(|i| smallest_enclosing(&defs, defs[i].range.clone(), Some(i)))
        .collect();

    let keys: Vec<String> = (0..defs.len())
        .map(|i| {
            let qualified = qualified_name(&defs, &parents, i);
            format!("sym:{lang}:{path}#{qualified}")
        })
        .collect();

    let mut nodes = vec![file_node(path, blob_id, bytes, Some(lang), ingest)];
    let mut edges: Vec<Edge> = Vec::new();

    for (i, d) in defs.iter().enumerate() {
        let mut meta = serde_json::Map::new();
        if let Some(doc) = &d.docs {
            let content = cap_content(doc);
            if !content.is_empty() {
                meta.insert("content".into(), serde_json::Value::from(content));
            }
        }
        // Attach the calls this definition encloses — but only for functions, the
        // only kind `crate::sync::resolve_calls` links.
        if d.kind == NodeKind::Fn {
            let mut names: Vec<String> = calls
                .iter()
                .filter(|(off, _)| d.range.contains(off))
                .filter(|(off, _)| smallest_enclosing_off(&defs, *off) == Some(i))
                .map(|(_, name)| name.clone())
                .collect();
            names.sort();
            names.dedup();
            if !names.is_empty() {
                meta.insert("calls".into(), serde_json::Value::from(names));
            }
        }

        let start = u32::try_from(d.range.start).unwrap_or(u32::MAX);
        let end = u32::try_from(d.range.end).unwrap_or(u32::MAX);
        nodes.push(Node {
            key: keys[i].clone(),
            kind: d.kind.clone(),
            name: d.name.clone(),
            path: Some(path.to_owned()),
            lang: Some(lang.to_owned()),
            blob_hash: Some(blob_id.to_owned()),
            span: Some(Span::new(start, end)),
            provenance: Provenance::Derived,
            meta: serde_json::Value::Object(meta),
        });

        match parents[i] {
            Some(p) => edges.push(Edge::derived(
                keys[p].clone(),
                keys[i].clone(),
                EdgeKind::Contains,
            )),
            None => edges.push(Edge::derived(
                file_key(path),
                keys[i].clone(),
                EdgeKind::Defines,
            )),
        }
    }

    // Import/include edges (file → import target), where the language has a query.
    append_import_facts(path, &def, bytes, &mut nodes, &mut edges);

    // Deterministic, duplicate-free output (two query patterns can capture the
    // same definition, and distinct symbols can share a qualified name).
    nodes.sort_by(|a, b| a.key.cmp(&b.key));
    nodes.dedup_by(|a, b| a.key == b.key);
    edges.sort_by(|a, b| (a.kind.as_str(), &a.src, &a.dst).cmp(&(b.kind.as_str(), &b.src, &b.dst)));
    edges.dedup();
    Some(FactSet { nodes, edges })
}

/// Index of the smallest definition (other than `skip`) whose range strictly
/// encloses `range`, or `None` if `range` is top-level.
fn smallest_enclosing(
    defs: &[TagDef],
    range: std::ops::Range<usize>,
    skip: Option<usize>,
) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (j, c) in defs.iter().enumerate() {
        if Some(j) == skip {
            continue;
        }
        // Strictly encloses: contains both ends and is a larger span.
        let encloses = c.range.start <= range.start
            && c.range.end >= range.end
            && (c.range.end - c.range.start) > (range.end - range.start);
        if encloses
            && best.is_none_or(|b| {
                defs[b].range.end - defs[b].range.start > c.range.end - c.range.start
            })
        {
            best = Some(j);
        }
    }
    best
}

/// Index of the smallest definition enclosing byte offset `off`.
fn smallest_enclosing_off(defs: &[TagDef], off: usize) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (j, c) in defs.iter().enumerate() {
        if c.range.contains(&off)
            && best.is_none_or(|b| {
                defs[b].range.end - defs[b].range.start > c.range.end - c.range.start
            })
        {
            best = Some(j);
        }
    }
    best
}

/// A definition's qualified name: its ancestors' names (root→leaf) joined to its
/// own by `::`, so nested symbols get distinct, stable keys.
fn qualified_name(defs: &[TagDef], parents: &[Option<usize>], i: usize) -> String {
    let mut chain: Vec<&str> = vec![defs[i].name.as_str()];
    let mut cur = parents[i];
    // Bound the walk by the number of definitions — parents form a DAG toward
    // smaller-or-equal spans, but guard against any pathological cycle.
    let mut guard = defs.len();
    while let Some(p) = cur {
        if guard == 0 {
            break;
        }
        guard -= 1;
        chain.push(defs[p].name.as_str());
        cur = parents[p];
    }
    chain.reverse();
    chain.join("::")
}

/// Byte span of an AST node, clamped to `u32`.
fn span(node: tree_sitter::Node) -> Span {
    let start = u32::try_from(node.start_byte()).unwrap_or(u32::MAX);
    let end = u32::try_from(node.end_byte()).unwrap_or(u32::MAX);
    Span::new(start, end)
}

/// Qualified name for a new symbol: all enclosing scope segments plus `name`.
fn qualify(scope: &[Scope], name: &str) -> String {
    let mut parts: Vec<&str> = scope.iter().map(|s| s.seg.as_str()).collect();
    parts.push(name);
    parts.join("::")
}

/// Combine an optional immediate qualifier with a callee `name` into the stored
/// `meta.calls` descriptor. Path-relative qualifiers (`self`/`crate`/`super`) and
/// an empty qualifier collapse to the bare name, since they don't scope a
/// cross-file target; `Self` is preserved as the marker for a same-impl call.
fn qualify_callee(qualifier: Option<&str>, name: &str) -> String {
    match qualifier {
        Some(q) if !q.is_empty() && !matches!(q, "self" | "crate" | "super") => {
            format!("{q}::{name}")
        }
        _ => name.to_owned(),
    }
}

/// Push a scope entry, returning the extended stack.
fn extend(scope: &[Scope], seg: &str, key: Option<String>) -> Vec<Scope> {
    let mut next: Vec<Scope> = scope
        .iter()
        .map(|s| Scope {
            seg: s.seg.clone(),
            key: s.key.clone(),
        })
        .collect();
    next.push(Scope {
        seg: seg.to_owned(),
        key,
    });
    next
}

#[cfg(test)]
mod tests {
    use super::{Extractor, FileNodeExtractor, Registry, RustExtractor};
    use crate::{EdgeKind, Node, NodeKind};

    #[test]
    fn file_node_extractor_is_deterministic_and_tagged() {
        let ex = FileNodeExtractor;
        let a = ex.extract("src/lib.rs", "abc123", b"one\ntwo\n");
        let b = ex.extract("src/lib.rs", "abc123", b"one\ntwo\n");
        assert_eq!(a, b, "extraction must be deterministic");

        assert_eq!(a.nodes.len(), 1);
        assert!(a.edges.is_empty());
        let node = &a.nodes[0];
        assert_eq!(node.key, "file:src/lib.rs");
        assert_eq!(node.kind, NodeKind::File);
        assert_eq!(node.name, "lib.rs");
        assert_eq!(node.blob_hash.as_deref(), Some("abc123"));
        assert_eq!(node.meta["lines"], 2);
        assert_eq!(node.meta["bytes"], 8);
    }

    #[test]
    fn config_files_emit_config_key_nodes() {
        let reg = Registry::new(crate::IngestConfig::default());
        let toml = b"[serve]\naddr = \"0.0.0.0:8443\"\ntools = false\n";
        let a = reg.extract("config.toml", "cfg1", toml);
        let b = reg.extract("config.toml", "cfg1", toml);
        assert_eq!(a, b, "config extraction must be deterministic");

        // The file node plus a config_key node per leaf.
        assert!(a.nodes.iter().any(|n| n.key == "file:config.toml"));
        let addr = a
            .nodes
            .iter()
            .find(|n| n.key == "cfgkey:config.toml#serve.addr")
            .expect("serve.addr config_key node");
        assert_eq!(addr.kind, NodeKind::Other("config_key".into()));
        assert_eq!(addr.name, "serve.addr");
        assert_eq!(addr.meta["value"], "0.0.0.0:8443"); // unquoted
        // A `contains` edge from the file to each config key.
        assert!(a.edges.iter().any(|e| {
            e.src == "file:config.toml"
                && e.dst == "cfgkey:config.toml#serve.addr"
                && e.kind == EdgeKind::Contains
        }));

        // A `.env` (no extension) is recognised by name; a repeated key yields one
        // node with the last value; a secret value is redacted.
        let env = reg.extract(".env", "env1", b"PORT=8080\nPORT=9090\nAPI_TOKEN=s3cr3t\n");
        let port = env
            .nodes
            .iter()
            .find(|n| n.key == "cfgkey:.env#PORT")
            .expect("PORT node");
        assert_eq!(port.meta["value"], "9090", "dotenv last-one-wins");
        assert_eq!(
            env.nodes
                .iter()
                .filter(|n| n.key == "cfgkey:.env#PORT")
                .count(),
            1
        );
        let token = env
            .nodes
            .iter()
            .find(|n| n.key == "cfgkey:.env#API_TOKEN")
            .expect("API_TOKEN node");
        assert_eq!(token.meta["value"], "<redacted>", "secret not persisted");
        // A source file is unaffected.
        let rs = reg.extract("src/lib.rs", "x", b"pub fn f() {}\n");
        assert!(
            rs.nodes
                .iter()
                .all(|n| n.kind != NodeKind::Other("config_key".into()))
        );
    }

    #[test]
    fn dockerfile_emits_image_ref_nodes_and_skips_internal_stages() {
        let reg = Registry::new(crate::IngestConfig::default());
        // Multi-stage: a builder stage (external), an internal `FROM builder`
        // (skipped), and a runtime external base pinned by digest.
        let df = b"FROM --platform=linux/amd64 rust:1.90 AS builder\nRUN cargo build\n\
                   FROM builder AS test\nFROM registry.io/app:1.2@sha256:abc AS run\nFROM scratch\n";
        let a = reg.extract("Dockerfile", "d1", df);
        let b = reg.extract("Dockerfile", "d1", df);
        assert_eq!(a, b, "dockerfile extraction must be deterministic");

        let refs: Vec<&Node> = a
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Other("image_ref".into()))
            .collect();
        // Two external images: rust:1.90 and the app digest. `FROM builder` and
        // `FROM scratch` are not pins.
        assert_eq!(refs.len(), 2, "got: {refs:?}");
        let rust = refs
            .iter()
            .find(|n| n.meta["image"] == "rust")
            .expect("rust");
        assert_eq!(rust.meta["tag"], "1.90");
        let app = refs
            .iter()
            .find(|n| n.meta["image"] == "registry.io/app:1.2")
            .expect("app digest");
        assert_eq!(app.meta["digest"], "sha256:abc");
        // A `references` edge from the file to each image_ref.
        assert!(
            a.edges
                .iter()
                .any(|e| { e.src == "file:Dockerfile" && e.kind == EdgeKind::References })
        );
        // `Dockerfile.prod` is recognised too; a plain source file is not.
        assert!(
            reg.extract("Dockerfile.prod", "d2", b"FROM alpine:3\n")
                .nodes
                .iter()
                .any(|n| n.kind == NodeKind::Other("image_ref".into()))
        );

        // A stage alias equal to the image name (`FROM alpine AS alpine`) must not
        // make the external `alpine` look like an internal stage — it is still a pin.
        let c = reg.extract("Dockerfile", "d3", b"FROM alpine AS alpine\n");
        assert!(
            c.nodes
                .iter()
                .any(|n| n.kind == NodeKind::Other("image_ref".into())
                    && n.meta["image"] == "alpine"),
            "FROM x AS x is an external pin, got: {:?}",
            c.nodes
        );
    }

    const SAMPLE: &str = r"
use std::path::Path;

pub struct Store;

impl Store {
    pub fn open() -> Store {
        helper();
        Store
    }
}

fn helper() {}

mod inner {
    pub fn nested() {}
}
";

    fn keys(fs: &crate::FactSet) -> Vec<String> {
        let mut k: Vec<_> = fs.nodes.iter().map(|n| n.key.clone()).collect();
        k.sort();
        k
    }

    #[test]
    fn rust_extractor_emits_symbols_and_edges() {
        let fs = RustExtractor.extract("src/lib.rs", "blob1", SAMPLE.as_bytes());
        let ks = keys(&fs);
        assert!(ks.contains(&"file:src/lib.rs".to_owned()));
        assert!(ks.contains(&"sym:rust:src/lib.rs#Store".to_owned()));
        assert!(ks.contains(&"sym:rust:src/lib.rs#Store::open".to_owned()));
        assert!(ks.contains(&"sym:rust:src/lib.rs#helper".to_owned()));
        assert!(ks.contains(&"sym:rust:src/lib.rs#inner".to_owned()));
        assert!(ks.contains(&"sym:rust:src/lib.rs#inner::nested".to_owned()));

        // `open` records that it calls `helper`.
        let open = fs
            .nodes
            .iter()
            .find(|n| n.key == "sym:rust:src/lib.rs#Store::open")
            .expect("open node");
        assert_eq!(open.meta["calls"], serde_json::json!(["helper"]));

        // file defines top-level items; a module contains its nested fn.
        let defines: Vec<_> = fs
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Defines && e.dst == "sym:rust:src/lib.rs#helper")
            .collect();
        assert_eq!(defines.len(), 1);
        assert!(fs.edges.iter().any(|e| e.kind == EdgeKind::Contains
            && e.src == "sym:rust:src/lib.rs#inner"
            && e.dst == "sym:rust:src/lib.rs#inner::nested"));

        // the `use` becomes an imports edge.
        assert!(fs.edges.iter().any(|e| e.kind == EdgeKind::Imports
            && e.src == "file:src/lib.rs"
            && e.dst == "import:rust:std::path::Path"));
    }

    #[test]
    fn rust_extractor_records_struct_field_names() {
        // A struct with named fields records them in `meta.fields` (the follow
        // bridge's join signal); a tuple struct and a unit struct carry none.
        let src = "pub struct ServeConfig {\n\
                   \x20   pub addr: Option<String>,\n\
                   \x20   pub tls_cert: Option<String>,\n\
                   }\n\
                   pub struct Pair(u8, u8);\n\
                   pub struct Marker;\n";
        let fs = RustExtractor.extract("src/config.rs", "b", src.as_bytes());
        let fields = |key: &str| {
            fs.nodes
                .iter()
                .find(|n| n.key == key)
                .and_then(|n| n.meta.get("fields").cloned())
        };
        assert_eq!(
            fields("sym:rust:src/config.rs#ServeConfig"),
            Some(serde_json::json!(["addr", "tls_cert"])),
            "named fields captured in source order"
        );
        // Positional (tuple) and unit structs declare no named fields → no key.
        assert_eq!(fields("sym:rust:src/config.rs#Pair"), None);
        assert_eq!(fields("sym:rust:src/config.rs#Marker"), None);
    }

    #[test]
    fn struct_records_field_types_and_config_root_marker() {
        // Field types land in `meta.field_types` (transparent wrappers peeled), and
        // the `@rto:config` marker sets `meta.config_root`.
        let src = "// @rto:config\n\
                   pub struct Config {\n\
                   \x20   pub zerobus: ZerobusConfig,\n\
                   \x20   pub replicas: Option<u32>,\n\
                   }\n\
                   pub struct ZerobusConfig {\n\
                   \x20   pub server_endpoint: String,\n\
                   }\n";
        let fs = RustExtractor.extract("src/config.rs", "b", src.as_bytes());
        let node = |key: &str| fs.nodes.iter().find(|n| n.key == key).expect("node");
        let root = node("sym:rust:src/config.rs#Config");
        assert_eq!(root.meta.get("config_root"), Some(&serde_json::json!(true)));
        assert_eq!(
            root.meta.get("field_types"),
            Some(&serde_json::json!({ "zerobus": "ZerobusConfig", "replicas": "u32" })),
            "transparent wrappers peeled (Option<u32> → u32)"
        );
        // An unmarked struct carries no `config_root` flag.
        assert_eq!(
            node("sym:rust:src/config.rs#ZerobusConfig")
                .meta
                .get("config_root"),
            None
        );
    }

    #[test]
    fn config_root_struct_synthesizes_recursive_dotted_config_keys() {
        // A `@rto:config` root with a nested struct field yields dotted `config_key`
        // nodes for its leaves — no committed `*-example.toml` needed. The nested
        // field descends by name into a struct defined in the same file.
        let src = "// @rto:config\n\
                   pub struct Config {\n\
                   \x20   pub zerobus: ZerobusConfig,\n\
                   \x20   pub log_level: String,\n\
                   }\n\
                   pub struct ZerobusConfig {\n\
                   \x20   pub server_endpoint: String,\n\
                   \x20   pub workspace_url: String,\n\
                   }\n";
        let fs = RustExtractor.extract("src/config.rs", "b", src.as_bytes());
        let cfg = |dotted: &str| {
            fs.nodes
                .iter()
                .find(|n| n.key == format!("cfgkey:src/config.rs#{dotted}"))
        };
        for dotted in [
            "zerobus.server_endpoint",
            "zerobus.workspace_url",
            "log_level",
        ] {
            let n = cfg(dotted).unwrap_or_else(|| panic!("missing {dotted}: {:?}", fs.nodes));
            assert_eq!(n.kind, NodeKind::Other("config_key".into()));
            assert_eq!(n.meta.get("key").and_then(|v| v.as_str()), Some(dotted));
            // Provenance marks it struct-derived, distinguishable from file keys.
            assert_eq!(
                n.meta.get("source").and_then(|v| v.as_str()),
                Some("struct")
            );
            assert_eq!(
                n.meta.get("struct").and_then(|v| v.as_str()),
                Some("Config")
            );
        }
        // The nested struct's own container name is NOT a leaf (only leaves emit).
        assert!(
            cfg("zerobus").is_none(),
            "intermediate section is not a leaf"
        );
        // A `contains` edge runs from the file node to each synthesized key.
        assert!(fs.edges.iter().any(|e| e.src == "file:src/config.rs"
            && e.dst == "cfgkey:src/config.rs#zerobus.server_endpoint"
            && e.kind == EdgeKind::Contains));
    }

    #[test]
    fn struct_without_config_marker_synthesizes_no_config_keys() {
        // The safety property: an ordinary struct (no `@rto:config`) never produces
        // synthetic config keys, so the feature is strictly opt-in and additive.
        let src = "pub struct Config {\n\
                   \x20   pub zerobus: ZerobusConfig,\n\
                   }\n\
                   pub struct ZerobusConfig {\n\
                   \x20   pub server_endpoint: String,\n\
                   }\n";
        let fs = RustExtractor.extract("src/config.rs", "b", src.as_bytes());
        assert!(
            fs.nodes
                .iter()
                .all(|n| n.kind != NodeKind::Other("config_key".into())),
            "no synthetic config_key nodes without the marker: {:?}",
            fs.nodes
        );
    }

    #[test]
    fn config_root_recursion_terminates_on_a_type_cycle() {
        // A self-referential config type must not loop forever: the cyclic field
        // falls back to a leaf and synthesis terminates.
        let src = "// @rto:config\n\
                   pub struct Config {\n\
                   \x20   pub addr: String,\n\
                   \x20   pub next: Box<Config>,\n\
                   }\n";
        let fs = RustExtractor.extract("src/config.rs", "b", src.as_bytes());
        let has = |dotted: &str| {
            fs.nodes
                .iter()
                .any(|n| n.key == format!("cfgkey:src/config.rs#{dotted}"))
        };
        assert!(has("addr"));
        // The descent path already holds `Config`, so the self-referential `next`
        // field is a leaf rather than recursing — synthesis terminates.
        assert!(has("next"), "cyclic field falls back to a leaf");
        assert!(!has("next.addr"), "no unbounded expansion");
    }

    #[test]
    fn rust_extraction_is_deterministic() {
        let a = RustExtractor.extract("src/lib.rs", "blob1", SAMPLE.as_bytes());
        let b = RustExtractor.extract("src/lib.rs", "blob1", SAMPLE.as_bytes());
        assert_eq!(a, b);
    }

    #[test]
    fn rust_extractor_captures_doc_comments() {
        let src = "/// The central store.\n\
                   pub struct Store;\n\n\
                   /// Opens it.\n\
                   /// Reads the config.\n\
                   pub fn open() {}\n\n\
                   // not a doc comment\n\
                   pub fn plain() {}\n";
        let fs = RustExtractor.extract("src/lib.rs", "b", src.as_bytes());
        let content = |key: &str| {
            fs.nodes
                .iter()
                .find(|n| n.key == key)
                .and_then(|n| n.meta.get("content"))
                .and_then(|v| v.as_str())
                .map(ToOwned::to_owned)
        };
        assert_eq!(
            content("sym:rust:src/lib.rs#Store").as_deref(),
            Some("The central store.")
        );
        assert_eq!(
            content("sym:rust:src/lib.rs#open").as_deref(),
            Some("Opens it. Reads the config.")
        );
        // A plain `//` comment is not captured.
        assert_eq!(content("sym:rust:src/lib.rs#plain"), None);
    }

    #[test]
    fn prose_file_captures_capped_body() {
        let md = FileNodeExtractor.extract("docs/x.md", "b", b"# Title\n\nSome prose   here.\n");
        assert_eq!(md.nodes[0].meta["content"], "# Title Some prose here.");
        // A non-prose file gets no content.
        let rs = FileNodeExtractor.extract("notes.bin", "b", b"\x00\x01binary");
        assert!(rs.nodes[0].meta.get("content").is_none());
        // Extension matching is case-insensitive: `README.MD` is prose too.
        let upper = FileNodeExtractor.extract("README.MD", "b", b"# Hi\n");
        assert_eq!(upper.nodes[0].meta["content"], "# Hi");
    }

    /// Build a one-page PDF with a single Helvetica text run, computing exact
    /// byte offsets for the xref table so `pdf-extract` can parse it.
    #[cfg(feature = "pdf-text")]
    fn minimal_pdf(text: &str) -> Vec<u8> {
        let content = format!("BT /F1 24 Tf 72 720 Td ({text}) Tj ET");
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_owned(),
            format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        ];
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let mut offsets = Vec::new();
        for (i, obj) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n{obj}\nendobj\n", i + 1).as_bytes());
        }
        let xref_start = pdf.len();
        pdf.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for off in &offsets {
            pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        pdf
    }

    #[cfg(feature = "pdf-text")]
    #[test]
    fn pdf_file_captures_text_content() {
        let pdf = minimal_pdf("Hello Roteiro");
        let facts = FileNodeExtractor.extract("docs/guide.pdf", "b", &pdf);
        let content = facts.nodes[0].meta["content"].as_str().unwrap();
        assert!(content.contains("Hello Roteiro"), "got: {content:?}");
        // Extension matching is case-insensitive: `Guide.PDF` extracts too.
        let upper = FileNodeExtractor.extract("docs/Guide.PDF", "b", &pdf);
        assert!(upper.nodes[0].meta.get("content").is_some());
        // A malformed PDF degrades to a plain file node — no panic, no content.
        let bad = FileNodeExtractor.extract("docs/bad.pdf", "b", b"%PDF-1.4\ngarbage");
        assert!(bad.nodes[0].meta.get("content").is_none());
    }

    #[cfg(any(feature = "image-ocr", feature = "image-vision"))]
    #[test]
    fn image_content_guards_before_touching_models() {
        // Case-insensitive image detection.
        assert!(super::is_image("shot.PNG"));
        assert!(super::is_image("b.jpeg"));
        assert!(super::is_image("c.jpg"));
        assert!(!super::is_image("d.gif"));
        // A non-image path returns None without ever looking for models.
        assert!(
            super::image_content("notes.txt", b"hello", super::IngestConfig::default()).is_none()
        );
        // An oversized image is rejected by the size guard, before model lookup.
        let big = vec![0u8; super::MAX_IMAGE_BYTES + 1];
        assert!(super::image_content("shot.png", &big, super::IngestConfig::default()).is_none());
    }

    #[test]
    fn doc_comment_body_recognises_doc_markers() {
        assert_eq!(super::doc_comment_body("/// hi").as_deref(), Some("hi"));
        assert_eq!(
            super::doc_comment_body("//! mod doc").as_deref(),
            Some("mod doc")
        );
        assert_eq!(
            super::doc_comment_body("/** block */").as_deref(),
            Some("block")
        );
        // Plain and `////` comments are not docs.
        assert_eq!(super::doc_comment_body("// plain"), None);
        assert_eq!(super::doc_comment_body("//// header"), None);
        // Degenerate block comments have an empty body, never garbage like "/".
        assert_eq!(super::doc_comment_body("/**/").as_deref(), Some(""));
        assert_eq!(super::doc_comment_body("/*!*/").as_deref(), Some(""));
    }

    #[test]
    fn registry_dispatches_by_extension() {
        let rs = Registry::default().extract("src/lib.rs", "b", SAMPLE.as_bytes());
        assert!(rs.nodes.len() > 1, "rust file yields symbols");
        let txt = Registry::default().extract("notes.txt", "b", b"hello\n");
        assert_eq!(
            txt.nodes.len(),
            1,
            "non-code file falls back to a file node"
        );
        assert_eq!(txt.nodes[0].kind, NodeKind::File);
    }

    #[test]
    fn tags_extracts_python_symbols_calls_and_nesting() {
        let src = "def helper():\n    pass\n\nclass Thing:\n    def run(self):\n        helper()\n";
        let fs = Registry::default().extract("app.py", "b", src.as_bytes());

        let names: Vec<&str> = fs.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"helper"), "top-level function");
        assert!(names.contains(&"Thing"), "class");
        assert!(names.contains(&"run"), "method");

        // Every symbol is language-tagged.
        assert_eq!(
            fs.nodes
                .iter()
                .find(|n| n.name == "helper")
                .and_then(|n| n.lang.as_deref()),
            Some("python")
        );

        // The method is nested in the class: a `contains` edge to `Thing::run`.
        assert!(
            fs.edges
                .iter()
                .any(|e| e.kind == EdgeKind::Contains && e.dst.ends_with("#Thing::run")),
            "method nested under class via containment"
        );

        // The method's body calls `helper`, recorded for later resolution.
        let run = fs.nodes.iter().find(|n| n.name == "run").unwrap();
        let calls = run.meta.get("calls").and_then(|v| v.as_array()).unwrap();
        assert!(
            calls.iter().any(|c| c.as_str() == Some("helper")),
            "enclosed call captured in meta.calls"
        );
    }

    #[test]
    fn tags_extraction_is_deterministic() {
        let src = b"package main\nfunc Add(a int) int { return a }\n";
        let a = Registry::default().extract("m.go", "b", src);
        let b = Registry::default().extract("m.go", "b", src);
        assert_eq!(a, b, "tags extraction must be deterministic");
        assert!(
            a.nodes
                .iter()
                .any(|n| n.name == "Add" && n.kind == NodeKind::Fn)
        );
    }

    #[test]
    fn tags_extracts_typescript() {
        let ts = Registry::default().extract("svc.ts", "b", b"export class Svc {\n  run() {}\n}\n");
        assert!(ts.nodes.iter().any(|n| n.name == "Svc"), "class");
        assert!(ts.nodes.iter().any(|n| n.name == "run"), "method");
        assert_eq!(
            ts.nodes
                .iter()
                .find(|n| n.name == "Svc")
                .and_then(|n| n.lang.as_deref()),
            Some("typescript")
        );
    }

    // Extract `src` as `path` and collect the `import:<…>` targets it emits.
    // Every import node's key is global, so — like the Rust walker's — it must
    // carry no `path`, keeping the node stable when several files import it.
    fn import_targets(path: &str, src: &[u8]) -> Vec<String> {
        Registry::default()
            .extract(path, "b", src)
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Other("import".into()))
            .inspect(|n| {
                assert!(
                    n.path.is_none(),
                    "import node must not be file-scoped: {}",
                    n.key
                );
            })
            .map(|n| n.key.clone())
            .collect()
    }

    #[test]
    fn extracts_imports_edges_per_language() {
        // Each case: a file with import statements → the expected `import:` nodes,
        // plus a `file → import` Imports edge.
        let cases: &[(&str, &[u8], &[&str])] = &[
            (
                "app.py",
                b"import os\nfrom a.b import c\nimport x.y as z\n",
                &["import:python:os", "import:python:a.b", "import:python:x.y"],
            ),
            (
                "m.js",
                b"import foo from \"./mod.js\";\nexport { y } from \"./y.js\";\n",
                &["import:javascript:./mod.js", "import:javascript:./y.js"],
            ),
            (
                "svc.ts",
                b"import { A } from \"./a\";\n",
                &["import:typescript:./a"],
            ),
            (
                "m.go",
                b"package main\nimport (\n\t\"fmt\"\n\t\"os\"\n)\n",
                &["import:go:fmt", "import:go:os"],
            ),
            (
                "M.java",
                b"import java.util.List;\nimport static a.B.c;\n",
                &["import:java:java.util.List", "import:java:a.B.c"],
            ),
            (
                "m.c",
                b"#include <stdio.h>\n#include \"local.h\"\n",
                &["import:c:stdio.h", "import:c:local.h"],
            ),
            ("m.cpp", b"#include <vector>\n", &["import:cpp:vector"]),
        ];
        for (path, src, expected) in cases {
            let got = import_targets(path, src);
            for want in *expected {
                assert!(
                    got.iter().any(|k| k == want),
                    "{path}: expected import node {want}, got {got:?}"
                );
            }
            // The corresponding file → import edge is derived.
            let fs = Registry::default().extract(path, "b", src);
            for want in *expected {
                assert!(
                    fs.edges.iter().any(|e| e.kind == EdgeKind::Imports
                        && e.src == format!("file:{path}")
                        && &e.dst == want),
                    "{path}: expected Imports edge to {want}"
                );
            }
        }
    }

    #[test]
    fn every_registered_language_query_compiles() {
        // A grammar/query mismatch (e.g. a future grammar bump) would make a
        // language silently fall back to a plain file node; assert each query
        // compiles against its grammar so that regression surfaces here instead.
        for ext in [
            "py", "js", "ts", "tsx", "go", "rb", "java", "c", "cpp", "cs", "php", "scala", "ml",
            "mli", "ex", "sh", "sql",
        ] {
            let def = super::tag_lang_for(ext).unwrap_or_else(|| panic!("no language for .{ext}"));
            let lang = def.lang;
            assert!(
                super::tag_config(&def).is_some(),
                "tags query for .{ext} ({lang}) must compile against its grammar"
            );
        }
    }

    #[test]
    fn ocaml_impl_and_interface_cache_under_distinct_grammars() {
        // `.ml` and `.mli` share the `ocaml` label but use different grammars, so
        // their config-cache keys must differ or one would parse with the other's
        // grammar (see the config cache keyed on `grammar_key`, not `lang`).
        let ml = super::tag_lang_for("ml").unwrap();
        let mli = super::tag_lang_for("mli").unwrap();
        assert_eq!(ml.lang, "ocaml");
        assert_eq!(mli.lang, "ocaml");
        assert_ne!(
            ml.grammar_key, mli.grammar_key,
            "distinct grammars must cache separately"
        );
    }

    #[test]
    fn tags_extracts_vendored_bash_query() {
        let src = "greet() {\n  echo hi\n}\nmain() {\n  greet\n}\n";
        let fs = Registry::default().extract("run.sh", "b", src.as_bytes());
        let names: Vec<&str> = fs.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"greet"), "shell function greet");
        assert!(names.contains(&"main"), "shell function main");

        // `main` invokes `greet` — a command reference captured as a call.
        let main = fs.nodes.iter().find(|n| n.name == "main").unwrap();
        assert!(
            main.meta
                .get("calls")
                .and_then(|v| v.as_array())
                .is_some_and(|c| c.iter().any(|x| x.as_str() == Some("greet"))),
            "internal command invocation captured"
        );
    }

    #[test]
    fn tags_extracts_vendored_sql_query() {
        let src = "CREATE TABLE users (id int);\n\
                   CREATE FUNCTION recent() RETURNS int AS $$ SELECT total(id) FROM users $$ LANGUAGE sql;\n";
        let fs = Registry::default().extract("schema.sql", "b", src.as_bytes());
        let names: Vec<&str> = fs.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"users"), "table definition");
        assert!(names.contains(&"recent"), "function definition");

        // The table maps to a non-function kind; the function to `Fn`.
        assert_eq!(
            fs.nodes.iter().find(|n| n.name == "users").map(|n| &n.kind),
            Some(&NodeKind::Other("table".to_owned()))
        );
        // The function body invokes `total`, captured for resolution.
        let f = fs.nodes.iter().find(|n| n.name == "recent").unwrap();
        assert!(
            f.meta
                .get("calls")
                .and_then(|v| v.as_array())
                .is_some_and(|c| c.iter().any(|x| x.as_str() == Some("total"))),
            "invocation inside function captured in meta.calls"
        );
        assert_eq!(
            fs.nodes
                .iter()
                .find(|n| n.name == "users")
                .and_then(|n| n.lang.as_deref()),
            Some("sql")
        );
    }

    #[test]
    fn ingest_prose_toggle_gates_embedded_content() {
        use super::IngestConfig;

        let content = |ingest: IngestConfig| {
            Registry::new(ingest)
                .extract("notes.md", "b", b"# Title\n\nBody text.\n")
                .nodes[0]
                .meta
                .get("content")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        };

        // Default (prose on) embeds the markdown body; disabling prose drops it.
        assert!(
            content(IngestConfig::default()).is_some_and(|c| c.contains("Body text")),
            "prose content embedded by default"
        );
        assert_eq!(
            content(IngestConfig {
                prose: false,
                ..IngestConfig::default()
            }),
            None,
            "disabling prose suppresses the embedded body"
        );
    }

    #[test]
    fn env_tag_stable_by_default_and_shifts_when_gated() {
        use super::IngestConfig;

        // All-on is the default: its tag must equal a plain `Registry` so existing
        // caches are untouched.
        let all_on = Registry::new(IngestConfig::default()).env_tag();
        assert_eq!(all_on, Registry::default().env_tag());

        // Each disabled toggle changes the tag (forcing re-extraction), and
        // distinct disabled sets produce distinct tags.
        let no_prose = Registry::new(IngestConfig {
            prose: false,
            ..IngestConfig::default()
        })
        .env_tag();
        let no_pdf = Registry::new(IngestConfig {
            pdf: false,
            ..IngestConfig::default()
        })
        .env_tag();
        let no_audio = Registry::new(IngestConfig {
            audio: false,
            ..IngestConfig::default()
        })
        .env_tag();
        assert_ne!(no_prose, all_on);
        assert_ne!(no_pdf, all_on);
        assert_ne!(no_audio, all_on);
        assert_ne!(no_prose, no_pdf);
        assert_ne!(no_audio, no_prose);
        assert_ne!(no_audio, no_pdf);
    }
}

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

use crate::{Edge, EdgeKind, FactSet, Node, NodeKind, Span};

/// Version of the extraction *output* (node/edge shape and captured `meta`).
/// Bump whenever extraction changes what it produces, so the content-addressed
/// cache (keyed by blob oid + path) does not serve stale facts for an unchanged
/// blob — the version is folded into the cache key. See [`crate::sync`].
///
/// The `pdf-text` and `image-ocr` features change what PDFs/images extract to, so
/// each occupies a distinct version namespace: a feature build and a default
/// build never serve each other stale (content-bearing vs content-free) facts
/// from a shared cache. (OCR output also depends on *which* models are installed;
/// that runtime state is folded into the cache key separately — see
/// [`ocr_env_tag`] and [`crate::sync`].)
pub(crate) const EXTRACT_VERSION: u32 = 3
    + if cfg!(feature = "pdf-text") { 100 } else { 0 }
    + if cfg!(feature = "image-ocr") { 200 } else { 0 };

/// Max characters of embeddable content (markdown body / doc-comment / PDF text)
/// captured into a node's `meta.content`, to keep the store small while giving
/// inference real text to embed.
const MAX_CONTENT: usize = 1500;

/// PDFs larger than this are not text-extracted — `pdf-extract` builds the full
/// document text in memory, so cap the work a pathological file can impose.
#[cfg(feature = "pdf-text")]
const MAX_PDF_BYTES: usize = 20 * 1024 * 1024;

/// Images larger than this (compressed bytes) are not OCR'd.
#[cfg(feature = "image-ocr")]
const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;

/// Images with more pixels than this are not OCR'd — OCR time scales with pixel
/// count, and this also guards against decompression bombs (the dimension is read
/// from the header before the pixels are decoded).
#[cfg(feature = "image-ocr")]
const MAX_IMAGE_PIXELS: u64 = 4096 * 4096;

/// Turns one source blob into the nodes and edges derived from it.
pub trait Extractor {
    /// Extract a [`FactSet`] from a blob's `path`, git `blob_id`, and `bytes`.
    ///
    /// Implementations must be deterministic: identical inputs must always
    /// produce an identical fact set.
    fn extract(&self, path: &str, blob_id: &str, bytes: &[u8]) -> FactSet;
}

/// Dispatches extraction to a language-aware extractor by file extension,
/// falling back to [`FileNodeExtractor`] when no language is registered. After
/// the language extractor runs, [`crate::markers`] appends any intent-debt
/// markers (TODOs, stubs, deferred-work notes) found in the blob.
#[derive(Debug, Clone, Copy, Default)]
pub struct Registry;

impl Extractor for Registry {
    fn extract(&self, path: &str, blob_id: &str, bytes: &[u8]) -> FactSet {
        let mut facts = match extension(path).as_deref() {
            Some("rs") => RustExtractor.extract(path, blob_id, bytes),
            _ => FileNodeExtractor.extract(path, blob_id, bytes),
        };
        crate::markers::augment(&mut facts, path, blob_id, bytes);
        facts
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

/// Build the shared `file` node for a source blob.
fn file_node(path: &str, blob_id: &str, bytes: &[u8], lang: Option<&str>) -> Node {
    let name = path.rsplit('/').next().unwrap_or(path).to_owned();
    let lines = bytes
        .iter()
        .fold(0usize, |n, &b| n + usize::from(b == b'\n'));
    let end = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    let mut meta = serde_json::json!({ "bytes": bytes.len(), "lines": lines });
    // Capture the (capped) body so inference embeds *meaning*, not just the
    // filename: prose files decode as UTF-8; PDFs go through `pdf_content` (only
    // when the `pdf-text` feature is on, otherwise it is a no-op).
    let content = if is_prose(path) {
        cap_content(&String::from_utf8_lossy(bytes))
    } else if let Some(text) = pdf_content(path, bytes) {
        cap_content(&text)
    } else if let Some(text) = image_content(path, bytes) {
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
        meta,
    }
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

/// OCR the text of an image blob for embedding, or `None` when `path` is not an
/// image, the `image-ocr` feature is off, the image is too large, the OCR models
/// are not installed, or OCR yields no text.
///
/// The `ocrs` engine can panic on some inputs; the call is panic-guarded so a
/// bad image degrades to a plain file node rather than aborting the whole sync.
/// OCR reads the installed OCR models — that runtime dependency is reflected in
/// the cache key via [`ocr_env_tag`], so installing/upgrading the models
/// re-extracts affected images instead of serving stale (content-free) facts.
#[cfg(feature = "image-ocr")]
fn image_content(path: &str, bytes: &[u8]) -> Option<String> {
    if !is_image(path) || bytes.len() > MAX_IMAGE_BYTES {
        return None;
    }
    let dir = crate::models::model_dir("ocrs-text");
    let detection = dir.join("text-detection.rten");
    let recognition = dir.join("text-recognition.rten");
    if !detection.exists() || !recognition.exists() {
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

/// Whether `path` is an image OCR can read.
#[cfg(feature = "image-ocr")]
fn is_image(path: &str) -> bool {
    matches!(extension(path).as_deref(), Some("png" | "jpg" | "jpeg"))
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

    // Read the dimensions from the header first, before decoding pixels, so a
    // decompression bomb is rejected cheaply.
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let (w, h) = reader.into_dimensions().ok()?;
    if u64::from(w) * u64::from(h) > MAX_IMAGE_PIXELS {
        return None;
    }

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

/// No-op when the `image-ocr` feature is off: images become plain file nodes.
#[cfg(not(feature = "image-ocr"))]
fn image_content(_path: &str, _bytes: &[u8]) -> Option<String> {
    None
}

/// A cache-key component reflecting the OCR extractor's runtime environment: `0`
/// when the `image-ocr` feature is off or the models are not installed, else a
/// hash of the pinned model identities. Folded into the sync cache key so that
/// installing or upgrading the OCR models invalidates cached image facts (OCR
/// output is not a pure function of the blob alone). See [`crate::sync`].
#[cfg(feature = "image-ocr")]
pub(crate) fn ocr_env_tag() -> u64 {
    let dir = crate::models::model_dir("ocrs-text");
    if !dir.join("text-detection.rten").exists() || !dir.join("text-recognition.rten").exists() {
        return 0;
    }
    // Fold the pinned checksums of the *host-selected* variant so a model change
    // (new registry sha) re-extracts — but adding an unrelated platform variant
    // does not perturb this host's tag.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    if let Some(variant) = crate::models::find("ocrs-text")
        .and_then(|spec| spec.variant_for(crate::models::Platform::host()))
    {
        for file in variant.files {
            for b in file.sha256.bytes() {
                hash ^= u64::from(b);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
    }
    hash | 1 // never 0 (which means "no OCR")
}

/// `0` whenever OCR is not compiled in.
#[cfg(not(feature = "image-ocr"))]
pub(crate) fn ocr_env_tag() -> u64 {
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
        FactSet::new().with_node(file_node(path, blob_id, bytes, None))
    }
}

/// Derived extractor for Rust source, backed by tree-sitter. Emits a `file`
/// node, one symbol node per `fn`/`struct`/`enum`/`trait`/`mod` (and a few
/// others) with `defines`/`contains` edges reflecting lexical nesting, and
/// `imports` edges for `use` declarations. Each function records the simple
/// names it calls in `meta.calls` for later cross-file resolution.
#[derive(Debug, Clone, Copy, Default)]
pub struct RustExtractor;

impl Extractor for RustExtractor {
    fn extract(&self, path: &str, blob_id: &str, bytes: &[u8]) -> FactSet {
        let mut parser = tree_sitter::Parser::new();
        // The Rust grammar is compiled in, so this only fails on a version
        // mismatch — a build-time invariant, not a runtime input error.
        if parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .is_err()
        {
            return FileNodeExtractor.extract(path, blob_id, bytes);
        }
        let Some(tree) = parser.parse(bytes, None) else {
            return FileNodeExtractor.extract(path, blob_id, bytes);
        };

        let mut walk = RustWalk {
            path,
            blob_id,
            src: bytes,
            nodes: vec![file_node(path, blob_id, bytes, Some("rust"))],
            edges: Vec::new(),
        };
        let root = tree.root_node();
        let mut cursor = root.walk();
        let children: Vec<_> = root.children(&mut cursor).collect();
        for child in children {
            walk.visit(child, &[]);
        }

        // Deterministic ordering so the cached fact set is byte-stable
        // regardless of traversal incidentals.
        walk.nodes.sort_by(|a, b| a.key.cmp(&b.key));
        walk.edges.sort_by(|a, b| {
            (a.kind.as_str(), &a.src, &a.dst).cmp(&(b.kind.as_str(), &b.src, &b.dst))
        });
        FactSet {
            nodes: walk.nodes,
            edges: walk.edges,
        }
    }
}

/// One entry on the lexical scope stack: a name segment and, when the scope is
/// itself an emitted symbol, that symbol's key (impl blocks contribute a segment
/// but no node, so their `key` is `None`).
struct Scope {
    seg: String,
    key: Option<String>,
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

        self.nodes.push(Node {
            key: key.clone(),
            kind,
            name,
            path: Some(self.path.to_owned()),
            lang: Some("rust".to_owned()),
            blob_hash: Some(self.blob_id.to_owned()),
            span: Some(span(node)),
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

    /// The simple callee name for a `call_expression`'s function child:
    /// `foo()` → `foo`, `a::b::foo()` → `foo`, `x.foo()` → `foo`.
    fn callee_name(&self, func: tree_sitter::Node) -> Option<String> {
        match func.kind() {
            "identifier" => Some(self.text(func).to_owned()),
            "scoped_identifier" => func
                .child_by_field_name("name")
                .map(|n| self.text(n).to_owned()),
            "field_expression" => func
                .child_by_field_name("field")
                .map(|n| self.text(n).to_owned()),
            _ => None,
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
    use crate::{EdgeKind, NodeKind};

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

    #[cfg(feature = "image-ocr")]
    #[test]
    fn image_content_guards_before_touching_models() {
        // Case-insensitive image detection.
        assert!(super::is_image("shot.PNG"));
        assert!(super::is_image("b.jpeg"));
        assert!(super::is_image("c.jpg"));
        assert!(!super::is_image("d.gif"));
        // A non-image path returns None without ever looking for OCR models.
        assert!(super::image_content("notes.txt", b"hello").is_none());
        // An oversized image is rejected by the size guard, before model lookup.
        let big = vec![0u8; super::MAX_IMAGE_BYTES + 1];
        assert!(super::image_content("shot.png", &big).is_none());
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
        let rs = Registry.extract("src/lib.rs", "b", SAMPLE.as_bytes());
        assert!(rs.nodes.len() > 1, "rust file yields symbols");
        let txt = Registry.extract("notes.txt", "b", b"hello\n");
        assert_eq!(
            txt.nodes.len(),
            1,
            "non-code file falls back to a file node"
        );
        assert_eq!(txt.nodes[0].kind, NodeKind::File);
    }
}

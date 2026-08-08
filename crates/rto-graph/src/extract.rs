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

/// Turns one source blob into the nodes and edges derived from it.
pub trait Extractor {
    /// Extract a [`FactSet`] from a blob's `path`, git `blob_id`, and `bytes`.
    ///
    /// Implementations must be deterministic: identical inputs must always
    /// produce an identical fact set.
    fn extract(&self, path: &str, blob_id: &str, bytes: &[u8]) -> FactSet;
}

/// Dispatches extraction to a language-aware extractor by file extension,
/// falling back to [`FileNodeExtractor`] when no language is registered.
#[derive(Debug, Clone, Copy, Default)]
pub struct Registry;

impl Extractor for Registry {
    fn extract(&self, path: &str, blob_id: &str, bytes: &[u8]) -> FactSet {
        match extension(path) {
            Some("rs") => RustExtractor.extract(path, blob_id, bytes),
            _ => FileNodeExtractor.extract(path, blob_id, bytes),
        }
    }
}

/// Lowercase file extension of `path`, if any.
fn extension(path: &str) -> Option<&str> {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').map(|(_, ext)| ext)
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
    Node {
        key: file_key(path),
        kind: NodeKind::File,
        name,
        path: Some(path.to_owned()),
        lang: lang.map(ToOwned::to_owned),
        blob_hash: Some(blob_id.to_owned()),
        span: Some(Span::new(0, end)),
        meta: serde_json::json!({ "bytes": bytes.len(), "lines": lines }),
    }
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

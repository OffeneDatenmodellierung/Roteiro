//! Extraction: turning the bytes of a source blob into a [`FactSet`].
//!
//! Extraction must be a deterministic pure function of `(path, blob_id, bytes)`
//! so its output can be cached; because the facts are path-dependent (node keys
//! are path-scoped), the cache is keyed by both path and blob id (see
//! [`crate::sync`]). Language-aware extraction (tree-sitter) arrives in a later
//! stage; [`FileNodeExtractor`] is the minimal placeholder that lets the sync
//! pipeline run end-to-end today.

use crate::{FactSet, Node, NodeKind, Span};

/// Turns one source blob into the nodes and edges derived from it.
pub trait Extractor {
    /// Extract a [`FactSet`] from a blob's `path`, git `blob_id`, and `bytes`.
    ///
    /// Implementations must be deterministic: identical inputs must always
    /// produce an identical fact set.
    fn extract(&self, path: &str, blob_id: &str, bytes: &[u8]) -> FactSet;
}

/// Placeholder extractor: emits a single `file` node per blob, tagged with its
/// blob hash and basic size metadata. Produces no edges. Superseded by the
/// tree-sitter extractor in a later stage.
#[derive(Debug, Clone, Copy, Default)]
pub struct FileNodeExtractor;

impl Extractor for FileNodeExtractor {
    fn extract(&self, path: &str, blob_id: &str, bytes: &[u8]) -> FactSet {
        let name = path.rsplit('/').next().unwrap_or(path).to_owned();
        let lines = bytes
            .iter()
            .fold(0usize, |n, &b| n + usize::from(b == b'\n'));
        let end = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
        let node = Node {
            key: format!("file:{path}"),
            kind: NodeKind::File,
            name,
            path: Some(path.to_owned()),
            lang: None,
            blob_hash: Some(blob_id.to_owned()),
            span: Some(Span::new(0, end)),
            meta: serde_json::json!({ "bytes": bytes.len(), "lines": lines }),
        };
        FactSet::new().with_node(node)
    }
}

#[cfg(test)]
mod tests {
    use super::{Extractor, FileNodeExtractor};
    use crate::NodeKind;

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
}

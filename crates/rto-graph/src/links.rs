//! Cross-repo external references (ADR-0009, the persisted `inferred` links).
//!
//! An inferred cross-repo link connects a config key in one repo (the *spoke*)
//! to its counterpart in another (the *hub*). The two endpoints live in
//! **different** graph stores, but the store's integrity rule requires both ends
//! of an edge to resolve to a node in the *same* store. So the spoke store gets
//! an **external-ref node** — a local placeholder standing in for the hub's node,
//! carrying the project-qualified target key — and the inferred edge points at
//! that placeholder. Store integrity holds locally, while the [`crate::Workspace`]
//! resolver still follows the placeholder across repos to the real node (see
//! [`crate::Workspace::follow_external_ref`]).
//!
//! These facts are not derivable from the spoke's own blobs (they need the hub),
//! so they are persisted as an **import layer** under [`LINKS_REF`] and re-applied
//! after every sync — dangling edges pruned when a config key is removed — reusing
//! the same durability machinery lat.md and Graphify imports rely on.

use crate::Provenance;
use crate::model::{Node, NodeKind};

/// The import-layer `src_ref` under which inferred cross-repo links are persisted
/// (see [`crate::Store::apply_import_layer`]). Its own producer, so re-inferring
/// can re-derive it authoritatively without touching other import layers.
pub const LINKS_REF: &str = "import:links";

/// The import-layer `src_ref` for **authored** cross-repo links — a repo's
/// `[[links]]` declarations (ADR-0009), as opposed to [`LINKS_REF`]'s inferred
/// matches.
///
/// A **separate** ref, and that is the load-bearing part. `apply_import_layer`
/// is authoritative per ref: it clears the ref's prior edges before re-applying.
/// Sharing one ref would therefore make `links --write` delete every inferred
/// edge and `links --infer --write` delete every authored one — each command
/// silently reclassifying the other's work on every run.
///
/// It is also what lets "authored → gold, inferred → slate" mean anything: the
/// two provenances have to be independently replaceable, or re-running one
/// changes the colour of the other.
pub const LINKS_AUTHORED_REF: &str = "import:links/authored";

/// The node-kind token for an external-ref placeholder — a stand-in, in one
/// repo's store, for a node that actually lives in another repo's graph.
pub const EXTERNAL_REF_KIND: &str = "external_ref";

/// Build an external-ref placeholder node for a **project-qualified** target key
/// (`<project>::<key>`, ADR-0009). The node lives in the *referring* repo's store
/// so an inferred edge to the (foreign) target satisfies store integrity; its
/// qualified target is recorded in `meta` so [`crate::Workspace::follow_external_ref`]
/// can resolve it across the workspace. Tagged [`Provenance::Inferred`].
#[must_use]
pub fn external_ref_node(qualified: &str) -> Node {
    external_ref_node_with(qualified, Provenance::Inferred)
}

/// [`external_ref_node`], with the placeholder's provenance chosen by the caller.
///
/// The placeholder carries the provenance of the *claim that the target exists*:
/// [`Provenance::Inferred`] for a confidence-scored match, [`Provenance::Authored`]
/// for a `[[links]]` declaration someone wrote.
///
/// # The placeholder's provenance is not the link's
///
/// Both flavours share a key, so a repo that both declares *and* infers the same
/// target has **one** placeholder — and since each layer upserts it, the node's
/// own provenance is whichever layer was applied last. Do not read it as the
/// link's provenance. The **edges** carry that, one per ref, and a consumer
/// asking "is this link authored?" must look there:
///
/// ```text
/// "incoming": [
///   { "provenance": "authored", "confidence": null },
///   { "provenance": "inferred", "confidence": 0.9   }
/// ]
/// ```
#[must_use]
pub fn external_ref_node_with(qualified: &str, provenance: Provenance) -> Node {
    let mut node = Node::new(
        external_ref_key(qualified),
        NodeKind::Other(EXTERNAL_REF_KIND.to_owned()),
        qualified.to_owned(),
    )
    .with_provenance(provenance);
    node.meta = serde_json::json!({ "qualified": qualified });
    node
}

/// The store key of the external-ref node for `qualified` — the qualified target
/// under an `extref:` namespace, so it never collides with a real node key.
#[must_use]
pub fn external_ref_key(qualified: &str) -> String {
    format!("extref:{qualified}")
}

/// The project-qualified target of an external-ref `node`, or `None` if `node` is
/// not one. Read from `meta.qualified`, falling back to the `extref:` key prefix
/// so a node written by an older layer still resolves.
#[must_use]
pub fn external_ref_target(node: &Node) -> Option<String> {
    // Compare by token so a hot resolver path never allocates a `NodeKind::Other`
    // just to check the kind.
    if node.kind.as_str() != EXTERNAL_REF_KIND {
        return None;
    }
    node.meta
        .get("qualified")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .or_else(|| node.key.strip_prefix("extref:").map(str::to_owned))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_ref_round_trips_its_qualified_target() {
        let q = "app::cfgkey:config.toml#serve.addr";
        let node = external_ref_node(q);
        assert_eq!(node.key, "extref:app::cfgkey:config.toml#serve.addr");
        assert_eq!(node.kind, NodeKind::Other("external_ref".to_owned()));
        assert_eq!(node.provenance, Provenance::Inferred);
        assert_eq!(external_ref_target(&node).as_deref(), Some(q));
    }

    #[test]
    fn target_falls_back_to_the_key_prefix_when_meta_is_missing() {
        let mut node = external_ref_node("app::file:x");
        node.meta = serde_json::Value::Null;
        assert_eq!(external_ref_target(&node).as_deref(), Some("app::file:x"));
    }

    #[test]
    fn non_external_ref_has_no_target() {
        let node = Node::new("file:x", NodeKind::File, "x");
        assert_eq!(external_ref_target(&node), None);
    }
}

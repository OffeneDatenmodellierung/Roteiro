//! The external works a node rests on, attached to the `explain` and `context`
//! answers that return it (ADR-0029, issue #801).
//!
//! One module so the CLI, the MCP server and the served tools attach them the
//! same way: a `references` field holding the APA 7 [`ReferenceList`] — rendered
//! entries, and every record **refused** with the fields that stopped it. The
//! field is additive under `roteiro.query/v1` and omitted when a node cites
//! nothing, so such a node serialises byte-for-byte as it did before.
//!
//! [`ReferenceList`]: crate::apa::ReferenceList

use rto_graph::{Reference, Store, StoreError};
use serde::Serialize;

/// `value` serialised, with `references` set to the reference list of `works`
/// when there are any.
///
/// # Errors
/// Returns the serialiser's error if `value` does not serialise.
pub fn with_references<T: Serialize>(
    value: &T,
    works: &[Reference],
) -> serde_json::Result<serde_json::Value> {
    let mut out = serde_json::to_value(value)?;
    if !works.is_empty()
        && let Some(object) = out.as_object_mut()
    {
        object.insert(
            "references".to_owned(),
            serde_json::to_value(crate::apa::reference_list(works))?,
        );
    }
    Ok(out)
}

/// [`rto_graph::explain`] for `key`, with the works it rests on attached.
///
/// # Errors
/// Returns [`StoreError`] on query failure.
pub fn explain(store: &Store, key: &str) -> Result<Option<serde_json::Value>, StoreError> {
    let Some(ex) = rto_graph::explain(store, key)? else {
        return Ok(None);
    };
    Ok(Some(with_references(
        &ex,
        &rto_graph::works_cited(store, key)?,
    )?))
}

/// [`rto_graph::tool_context`] for `key`, with the works it rests on attached.
///
/// # Errors
/// Returns [`StoreError`] on query failure.
pub fn tool_context(store: &Store, key: &str) -> Result<Option<serde_json::Value>, StoreError> {
    let Some(ctx) = rto_graph::tool_context(store, key)? else {
        return Ok(None);
    };
    Ok(Some(with_references(
        &ctx,
        &rto_graph::works_cited(store, key)?,
    )?))
}

#[cfg(test)]
mod tests {
    use super::with_references;
    use rto_graph::{Reference, Stability, WorkKind};

    #[test]
    fn nothing_cited_serialises_unchanged_and_a_citation_adds_references() {
        let value = serde_json::json!({ "schema": "roteiro.query/v1" });
        assert_eq!(with_references(&value, &[]).expect("json"), value);

        let work = Reference::new("w", WorkKind::Document, "T", Stability::FixedOrArchived);
        let out = with_references(&value, &[work]).expect("json");
        assert_eq!(out["schema"], "roteiro.query/v1");
        assert_eq!(
            out["references"]["refused"][0]["reference_id"], "w",
            "an incomplete record is listed as refused, never dropped"
        );
    }
}

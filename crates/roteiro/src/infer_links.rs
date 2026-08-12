//! Inferred cross-repo config-key **matching** (ADR-0009, the `inferred` path).
//!
//! `roteiro links --infer` reads each workspace repo's `config_key` nodes **from
//! its graph** (via [`rto_graph::Store::config_keys`] — the nodes a `sync`
//! extracts from its TOML / JSON / `.env` files) and matches every spoke repo's
//! keys against a hub repo's by name — surfacing correspondences with no
//! hand-authored `[[links]]` and flagging **orphans** (a spoke key with no hub
//! counterpart, the drift signal). Reading from the graph, not re-parsing files,
//! keeps the matcher and the stored config-key nodes in lock-step.
//!
//! With `--write`, [`link_facts`] turns the matches into a durable `inferred`
//! import layer per spoke: an **external-ref node** standing in for the hub's key
//! (so store integrity holds), plus a `references` edge from the spoke's own
//! `config_key` node to it — the cross-repo edge, followable via
//! [`rto_graph::Workspace::follow_external_ref`] and re-applied after every sync.

use rto_graph::{Edge, EdgeKind, FactSet, LINKS_REF, external_ref_node};
// Re-exported so callers name one `ConfigKey`; the graph and the matcher share it.
pub use rto_graph::ConfigKey;

/// One inferred correspondence between a spoke key and a hub key.
#[derive(Debug, Clone, serde::Serialize)]
pub struct KeyMatch {
    /// The spoke's key.
    pub spoke_key: String,
    /// The spoke source file.
    pub spoke_file: String,
    /// The matched hub key.
    pub hub_key: String,
    /// The hub source file (with `hub_key`, addresses the hub's `config_key` node).
    pub hub_file: String,
    /// Match confidence in `0.0..=1.0` (name + value agreement).
    pub confidence: f64,
}

fn last_token(norm: &str) -> &str {
    norm.rsplit('.').next().unwrap_or(norm)
}

/// Match every `spoke` key against the `hub` keys. Returns the correspondences
/// (best hub match per spoke key, by confidence) and the **orphans** — spoke keys
/// with no hub counterpart (the drift candidates).
#[must_use]
pub fn match_against_hub(
    spoke: &[ConfigKey],
    hub: &[ConfigKey],
) -> (Vec<KeyMatch>, Vec<ConfigKey>) {
    use rto_graph::normalize_config_key as normalize;
    use std::collections::HashMap;
    // Index the hub by full normalised key, and by leaf token → *all* candidates
    // (so an ambiguous leaf isn't silently resolved to an arbitrary one).
    let mut by_full: HashMap<String, &ConfigKey> = HashMap::new();
    let mut by_leaf: HashMap<String, Vec<&ConfigKey>> = HashMap::new();
    for h in hub {
        let n = normalize(&h.key);
        by_full.entry(n.clone()).or_insert(h);
        by_leaf
            .entry(last_token(&n).to_owned())
            .or_default()
            .push(h);
    }
    let mut matches = Vec::new();
    let mut orphans = Vec::new();
    for s in spoke {
        let n = normalize(&s.key);
        if let Some(h) = by_full.get(&n) {
            // Full-path name match; value agreement nudges confidence up.
            let conf = if h.value == s.value { 0.98 } else { 0.9 };
            matches.push(KeyMatch {
                spoke_key: s.key.clone(),
                spoke_file: s.file.clone(),
                hub_key: h.key.clone(),
                hub_file: h.file.clone(),
                confidence: conf,
            });
        } else if let Some([h]) = by_leaf.get(last_token(&n)).map(Vec::as_slice) {
            // Same leaf name under a different path — a weaker, still-useful hint,
            // but only when it's *unambiguous* (exactly one hub key with that leaf).
            matches.push(KeyMatch {
                spoke_key: s.key.clone(),
                spoke_file: s.file.clone(),
                hub_key: h.key.clone(),
                hub_file: h.file.clone(),
                confidence: 0.55,
            });
        } else {
            // No full match, and either no leaf or an ambiguous one → orphan.
            orphans.push(s.clone());
        }
    }
    (matches, orphans)
}

/// Build the import-layer facts persisting one spoke's inferred cross-repo links
/// (ADR-0009, feature 2b). For each `KeyMatch`, an **external-ref node** stands in
/// for the hub's `config_key` node (`<hub_project>::cfgkey:<hub_file>#<hub_key>`)
/// so store integrity holds, and an inferred `references` edge runs from the
/// spoke's own `config_key` node to it, stamped [`LINKS_REF`]. Applied to the
/// spoke store as an import layer, so it survives sync (dangling edges pruned when
/// a config key is removed).
#[must_use]
pub fn link_facts(hub_project: &str, matches: &[KeyMatch]) -> FactSet {
    let mut facts = FactSet::new();
    for m in matches {
        let spoke_node = format!("cfgkey:{}#{}", m.spoke_file, m.spoke_key);
        let qualified = format!("{hub_project}::cfgkey:{}#{}", m.hub_file, m.hub_key);
        let target = external_ref_node(&qualified);
        let mut edge = Edge::inferred(
            spoke_node,
            target.key.clone(),
            EdgeKind::References,
            m.confidence,
        );
        edge.src_ref = Some(LINKS_REF.to_owned());
        facts = facts.with_node(target).with_edge(edge);
    }
    facts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ck(key: &str, value: &str) -> ConfigKey {
        ConfigKey {
            file: "f".into(),
            key: key.into(),
            value: value.into(),
        }
    }

    #[test]
    fn value_agreement_lifts_confidence_and_ambiguous_leaf_is_skipped() {
        let hub = vec![ck("serve.addr", "0.0.0.0:8443"), ck("db.addr", "x")];
        let spoke = vec![
            ck("SERVE_ADDR", "0.0.0.0:8443"), // full match + equal value → 0.98
            ck("addr", "y"),                  // ambiguous leaf (serve.addr/db.addr) → orphan
        ];
        let (m, orphans) = match_against_hub(&spoke, &hub);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].hub_key, "serve.addr");
        assert_eq!(m[0].hub_file, "f");
        assert!(m[0].confidence >= 0.95);
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].key, "addr");
    }

    #[test]
    fn link_facts_builds_an_external_ref_and_inferred_edge_per_match() {
        let m = KeyMatch {
            spoke_key: "SERVE_ADDR".into(),
            spoke_file: "prod.env".into(),
            hub_key: "serve.addr".into(),
            hub_file: "config.toml".into(),
            confidence: 0.9,
        };
        let facts = link_facts("app", std::slice::from_ref(&m));
        // One external-ref node standing in for the hub key.
        assert_eq!(facts.nodes.len(), 1);
        assert_eq!(
            facts.nodes[0].key,
            "extref:app::cfgkey:config.toml#serve.addr"
        );
        // One inferred edge from the spoke's config-key node to it, LINKS_REF-stamped.
        assert_eq!(facts.edges.len(), 1);
        let e = &facts.edges[0];
        assert_eq!(e.src, "cfgkey:prod.env#SERVE_ADDR");
        assert_eq!(e.dst, "extref:app::cfgkey:config.toml#serve.addr");
        assert_eq!(e.confidence, Some(0.9));
        assert_eq!(e.src_ref.as_deref(), Some(LINKS_REF));
    }
}

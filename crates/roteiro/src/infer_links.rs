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

/// The sole hub candidate among `cands`, or `None` if they cover **two or more
/// distinct hub keys** — an ambiguous collision we must not resolve arbitrarily
/// (skip, so the spoke key falls through to an orphan rather than a wrong link).
/// Entries that share one original key name (the same setting listed in several
/// files) count as one, first-wins, matching the `by_full` index.
fn unambiguous<'a>(cands: &[&'a ConfigKey]) -> Option<&'a ConfigKey> {
    let first = *cands.first()?;
    cands.iter().all(|c| c.key == first.key).then_some(first)
}

/// Match every `spoke` key against the `hub` keys. Returns the correspondences
/// (best hub match per spoke key, by confidence) and the **orphans** — spoke keys
/// with no hub counterpart (the drift candidates).
///
/// Matching runs in precedence tiers, strongest first, so an exact hit always wins
/// over a looser one:
/// 1. **Exact** — the full [`normalize`](rto_graph::normalize_config_key)d key
///    (split on any run of non-ASCII-alphanumeric chars — `_`, `-`, `.`, etc. — so
///    every such run is a segment boundary), bridging `SERVE_ADDR` ↔ `serve.addr`.
/// 2. **Canonical** — the [`canonicalize`](rto_graph::canonicalize_config_key)d key
///    (separators collapsed *within* a segment), bridging **naming conventions**:
///    a k8s YAML `zerobus.serverEndpoint` (`camelCase`) ↔ an app TOML
///    `zerobus.server_endpoint` (`snake_case`) ↔ `zerobus.server-endpoint` (`kebab`).
///    Consulted only when the exact tier misses, and only when the hub side is
///    *unambiguous* — a still-inferred link, never a forced one.
/// 3. **Leaf** — same trailing token under a different path, the weakest hint,
///    likewise only when unambiguous.
///
/// Normalisation is for *matching* only: the original spoke/hub key names are
/// preserved verbatim in every [`KeyMatch`] and orphan for display and persistence.
#[must_use]
pub fn match_against_hub(
    spoke: &[ConfigKey],
    hub: &[ConfigKey],
) -> (Vec<KeyMatch>, Vec<ConfigKey>) {
    use rto_graph::{canonicalize_config_key as canonicalize, normalize_config_key as normalize};
    use std::collections::HashMap;
    // Index the hub three ways. `by_full` is first-wins (the exact tier is
    // unambiguous by construction). `by_canon`/`by_leaf` keep *all* candidates so a
    // collision is detected and skipped rather than silently resolved to an
    // arbitrary one.
    let mut by_full: HashMap<String, &ConfigKey> = HashMap::new();
    let mut by_canon: HashMap<String, Vec<&ConfigKey>> = HashMap::new();
    let mut by_leaf: HashMap<String, Vec<&ConfigKey>> = HashMap::new();
    for h in hub {
        let n = normalize(&h.key);
        by_full.entry(n.clone()).or_insert(h);
        by_canon.entry(canonicalize(&h.key)).or_default().push(h);
        by_leaf
            .entry(last_token(&n).to_owned())
            .or_default()
            .push(h);
    }
    let mut matches = Vec::new();
    let mut orphans = Vec::new();
    let hit = |s: &ConfigKey, h: &ConfigKey, confidence: f64| KeyMatch {
        spoke_key: s.key.clone(),
        spoke_file: s.file.clone(),
        hub_key: h.key.clone(),
        hub_file: h.file.clone(),
        confidence,
    };
    for s in spoke {
        let n = normalize(&s.key);
        if let Some(h) = by_full.get(&n) {
            // Tier 1 — exact full-path name match; value agreement nudges up.
            matches.push(hit(s, h, if h.value == s.value { 0.98 } else { 0.9 }));
        } else if let Some(h) = by_canon
            .get(&canonicalize(&s.key))
            .and_then(|c| unambiguous(c))
        {
            // Tier 2 — same key across a naming-convention gap (camel/snake/kebab),
            // only when the hub side is unambiguous. Still an inferred link, scored
            // just under the exact tier.
            matches.push(hit(s, h, if h.value == s.value { 0.95 } else { 0.85 }));
        } else if let Some([h]) = by_leaf.get(last_token(&n)).map(Vec::as_slice) {
            // Tier 3 — same leaf name under a different path — a weaker, still-useful
            // hint, but only when it's *unambiguous* (exactly one hub key that leaf).
            matches.push(hit(s, h, 0.55));
        } else {
            // No confident match at any tier → orphan (the drift signal).
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
    fn camel_snake_kebab_bridge_the_naming_convention_gap() {
        // The real k8s-YAML ↔ app-TOML case: a hub app defines snake_case keys in
        // TOML; its infra repos set the camelCase (and kebab) spellings in YAML.
        // These are the *same* settings and must match, not read as drift.
        let hub = vec![
            ck("zerobus.server_endpoint", "grpc://z:443"),
            ck("zerobus.workspace_url", "https://w"),
        ];
        let spoke = vec![
            ck("zerobus.serverEndpoint", "grpc://z:443"), // camelCase, value agrees
            ck("zerobus.workspace-url", "https://w"),     // kebab-case, value agrees
        ];
        let (m, orphans) = match_against_hub(&spoke, &hub);
        assert!(orphans.is_empty(), "both bridge the gap: {orphans:?}");
        assert_eq!(m.len(), 2);
        // Original key names are preserved verbatim (normalisation is match-only).
        let camel = m
            .iter()
            .find(|k| k.spoke_key == "zerobus.serverEndpoint")
            .unwrap();
        assert_eq!(camel.hub_key, "zerobus.server_endpoint");
        assert!(
            camel.confidence >= 0.9,
            "value agreement lifts it: {camel:?}"
        );
        let kebab = m
            .iter()
            .find(|k| k.spoke_key == "zerobus.workspace-url")
            .unwrap();
        assert_eq!(kebab.hub_key, "zerobus.workspace_url");
    }

    #[test]
    fn exact_match_takes_precedence_over_a_canonical_one() {
        // When both an exact and a canonical hub key exist, the exact spelling wins
        // (tier 1) — and scores higher than a cross-convention (tier 2) match.
        let hub = vec![
            ck("zerobus.server_endpoint", "grpc://z:443"),
            ck("zerobus.serverEndpoint", "grpc://other:443"),
        ];
        let spoke = vec![ck("zerobus.server_endpoint", "grpc://z:443")];
        let (m, orphans) = match_against_hub(&spoke, &hub);
        assert!(orphans.is_empty());
        assert_eq!(m.len(), 1);
        assert_eq!(
            m[0].hub_key, "zerobus.server_endpoint",
            "exact spelling wins"
        );
        assert!(
            m[0].confidence >= 0.98,
            "exact + value agreement: {:?}",
            m[0]
        );
    }

    #[test]
    fn a_hub_absent_key_is_still_reported_as_drift() {
        // Normalisation must not *invent* matches: a key with no hub counterpart
        // stays an orphan (the drift signal the user relies on).
        let hub = vec![ck("zerobus.server_endpoint", "grpc://z:443")];
        let spoke = vec![ck(
            "zerobus.delta_table_properties.delta.enableChangeDataFeed",
            "true",
        )];
        let (m, orphans) = match_against_hub(&spoke, &hub);
        assert!(m.is_empty(), "no fabricated match: {m:?}");
        assert_eq!(orphans.len(), 1);
        assert_eq!(
            orphans[0].key,
            "zerobus.delta_table_properties.delta.enableChangeDataFeed"
        );
    }

    #[test]
    fn an_ambiguous_canonical_collision_produces_no_wrong_link() {
        // Two distinct hub keys canonicalise the same way; a third spelling on the
        // spoke could map to either — so we skip it (orphan) rather than guess.
        let hub = vec![
            ck("zerobus.server_endpoint", "grpc://snake"),
            ck("zerobus.server-endpoint", "grpc://kebab"),
        ];
        // camelCase spelling: no exact hub hit, canonical collides on two distinct
        // hub keys → must NOT force a link.
        let spoke = vec![ck("zerobus.serverEndpoint", "grpc://camel")];
        let (m, orphans) = match_against_hub(&spoke, &hub);
        assert!(m.is_empty(), "ambiguous collision must not link: {m:?}");
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].key, "zerobus.serverEndpoint");
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

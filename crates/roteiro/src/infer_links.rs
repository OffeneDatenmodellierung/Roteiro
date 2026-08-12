//! Inferred cross-repo config-key **matching** (ADR-0009, the `inferred` path).
//!
//! `roteiro links --infer` reads each workspace repo's config files, flattens
//! them to dotted keys, and matches every spoke repo's keys against a hub repo's
//! by name — surfacing correspondences with no hand-authored `[[links]]` and
//! flagging **orphans** (a spoke key with no hub counterpart, the drift signal).
//!
//! Parsing lives in [`rto_graph::config_keys`] (the same code the extraction
//! pipeline turns into `config_key` graph nodes), so the matcher and the graph
//! never disagree. This module is just the cross-repo matching over those keys.

use rto_graph::Repo;
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
                confidence: conf,
            });
        } else if let Some([h]) = by_leaf.get(last_token(&n)).map(Vec::as_slice) {
            // Same leaf name under a different path — a weaker, still-useful hint,
            // but only when it's *unambiguous* (exactly one hub key with that leaf).
            matches.push(KeyMatch {
                spoke_key: s.key.clone(),
                spoke_file: s.file.clone(),
                hub_key: h.key.clone(),
                confidence: 0.55,
            });
        } else {
            // No full match, and either no leaf or an ambiguous one → orphan.
            orphans.push(s.clone());
        }
    }
    (matches, orphans)
}

/// Collect every config-key leaf from a repo's tracked config files (committed
/// content, so the result is deterministic).
///
/// # Errors
/// Propagates git errors from walking or reading blobs.
pub fn collect(repo: &Repo) -> anyhow::Result<Vec<ConfigKey>> {
    let mut keys = Vec::new();
    for blob in repo.walk_blobs()? {
        if rto_graph::is_config_path(&blob.path) {
            let bytes = repo.read_blob(&blob.oid)?;
            keys.extend(rto_graph::flatten_config(&blob.path, &bytes));
        }
    }
    Ok(keys)
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
        assert!(m[0].confidence >= 0.95);
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].key, "addr");
    }
}

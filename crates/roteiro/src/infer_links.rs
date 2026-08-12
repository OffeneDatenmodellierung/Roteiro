//! Inferred cross-repo config-key matching (ADR-0009, the `inferred` path).
//!
//! `roteiro links --infer` reads each workspace repo's **config files** (TOML,
//! JSON, `.env`), flattens them to dotted keys, and matches every spoke repo's
//! keys against a hub repo's by name — surfacing the correspondences
//! automatically (no hand-authored `[[links]]`) and flagging **orphans**: a spoke
//! key with no hub counterpart, the likely-drift signal.
//!
//! Deliberately dependency-free — TOML and JSON parse with crates already in the
//! tree, `.env` is a trivial line format. YAML is intentionally out of scope for
//! now (it would need a new parser dependency; revisit if a Helm/k8s target
//! arrives).

use rto_graph::Repo;

/// A single leaf config setting: its dotted key, source file, and value.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ConfigKey {
    /// Repo-relative file the key was read from.
    pub file: String,
    /// Dotted key path (e.g. `serve.addr`), verbatim from the source.
    pub key: String,
    /// The scalar (or compact list/object) value, as a string.
    pub value: String,
}

/// One inferred correspondence between a spoke key and a hub key.
#[derive(Debug, Clone, serde::Serialize)]
pub struct KeyMatch {
    /// The spoke's key and where it lives.
    pub spoke_key: String,
    /// The spoke source file.
    pub spoke_file: String,
    /// The matched hub key.
    pub hub_key: String,
    /// Match confidence in `0.0..=1.0` (name + value agreement).
    pub confidence: f64,
}

/// The lowercased file extension, if any (`config.TOML` → `toml`).
fn ext_lower(path: &str) -> Option<String> {
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
}

/// Whether a repo-relative path is a config file this matcher understands:
/// `*.toml`, `*.json`, `*.env`, or a dotenv name (`.env`, `.env.<x>`).
#[must_use]
pub fn is_config_path(path: &str) -> bool {
    let base = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    matches!(ext_lower(path).as_deref(), Some("toml" | "json" | "env"))
        || base == ".env"
        || base.starts_with(".env.")
}

/// Flatten a config file's bytes into leaf keys, dispatched by extension.
/// Unparseable files yield nothing (a config we can't read is not an error here).
#[must_use]
pub fn flatten(path: &str, bytes: &[u8]) -> Vec<ConfigKey> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    match ext_lower(path).as_deref() {
        Some("toml") => {
            if let Ok(v) = toml::from_str::<toml::Value>(text) {
                flatten_toml(&v, "", path, &mut out);
            }
        }
        Some("json") => {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(text) {
                flatten_json(&v, "", path, &mut out);
            }
        }
        // `.env`, `.env.<x>`, `*.env`, or anything else we treat as line-format.
        _ => flatten_env(text, path, &mut out),
    }
    out
}

fn push(out: &mut Vec<ConfigKey>, file: &str, key: &str, value: String) {
    if !key.is_empty() {
        out.push(ConfigKey {
            file: file.to_owned(),
            key: key.to_owned(),
            value,
        });
    }
}

fn join(prefix: &str, seg: &str) -> String {
    if prefix.is_empty() {
        seg.to_owned()
    } else {
        format!("{prefix}.{seg}")
    }
}

/// A TOML leaf value as a plain string. Strings are emitted **unquoted** (so
/// `addr = "127.0.0.1"` matches an env `ADDR=127.0.0.1` by value); other scalars
/// and arrays keep their canonical rendering.
fn toml_scalar(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// A JSON leaf value as a plain string — strings unquoted, matching [`toml_scalar`]
/// so the same setting agrees across formats.
fn json_scalar(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Recurse into TOML tables; every non-table (scalar, array, inline) is a leaf
/// value keyed by its dotted path — so `serve.models = ["a"]` is one key.
fn flatten_toml(v: &toml::Value, prefix: &str, file: &str, out: &mut Vec<ConfigKey>) {
    match v {
        toml::Value::Table(t) => {
            for (k, val) in t {
                flatten_toml(val, &join(prefix, k), file, out);
            }
        }
        other => push(out, file, prefix, toml_scalar(other)),
    }
}

/// Recurse into JSON objects; arrays and scalars are leaves.
fn flatten_json(v: &serde_json::Value, prefix: &str, file: &str, out: &mut Vec<ConfigKey>) {
    match v {
        serde_json::Value::Object(m) => {
            for (k, val) in m {
                flatten_json(val, &join(prefix, k), file, out);
            }
        }
        other => push(out, file, prefix, json_scalar(other)),
    }
}

/// Parse `KEY=VALUE` lines (skipping blanks / `#` comments), trimming a single
/// layer of surrounding quotes from the value.
fn flatten_env(text: &str, file: &str, out: &mut Vec<ConfigKey>) {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, val)) = line.strip_prefix("export ").unwrap_or(line).split_once('=') {
            let key = k.trim();
            let val = val.trim().trim_matches('"').trim_matches('\'').to_owned();
            push(out, file, key, val);
        }
    }
}

/// Normalise a dotted key for matching: lowercase, split on any non-alphanumeric
/// run, join with `.`. So `SERVE_ADDR`, `serve.addr`, and `serve-addr` all become
/// `serve.addr` and match across TOML / env / JSON conventions.
#[must_use]
pub fn normalize(key: &str) -> String {
    key.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join(".")
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
        if is_config_path(&blob.path) {
            let bytes = repo.read_blob(&blob.oid)?;
            keys.extend(flatten(&blob.path, &bytes));
        }
    }
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flatten_toml_json_and_env_to_dotted_leaves() {
        let toml_keys = flatten(
            "a.toml",
            b"[serve]\naddr = \"0.0.0.0:8443\"\nmodels = [\"q8\"]\n",
        );
        // TOML string scalars are unquoted, so they compare with env/JSON values.
        assert!(
            toml_keys
                .iter()
                .any(|k| k.key == "serve.addr" && k.value == "0.0.0.0:8443")
        );
        // An array is one leaf key, not indexed noise.
        assert!(toml_keys.iter().any(|k| k.key == "serve.models"));

        let json_keys = flatten(
            "a.json",
            br#"{"serve":{"tools":false,"addr":"0.0.0.0:8443"}}"#,
        );
        assert!(
            json_keys
                .iter()
                .any(|k| k.key == "serve.tools" && k.value == "false")
        );
        // JSON string scalars are unquoted too — same setting agrees across formats.
        assert!(
            json_keys
                .iter()
                .any(|k| k.key == "serve.addr" && k.value == "0.0.0.0:8443")
        );

        let env_keys = flatten(".env", b"# c\nexport SERVE_ADDR=127.0.0.1:8017\nEMPTY=\n");
        assert!(
            env_keys
                .iter()
                .any(|k| k.key == "SERVE_ADDR" && k.value == "127.0.0.1:8017")
        );
    }

    #[test]
    fn value_agreement_across_formats_lifts_confidence() {
        // TOML hub and env spoke set the same address → unquoted values agree.
        let hub = flatten("app.toml", b"[serve]\naddr = \"0.0.0.0:8443\"\n");
        let spoke = flatten("prod.env", b"SERVE_ADDR=0.0.0.0:8443\n");
        let (m, _) = match_against_hub(&spoke, &hub);
        assert_eq!(m.len(), 1);
        assert!(
            m[0].confidence >= 0.95,
            "matching values → high confidence: {:?}",
            m[0]
        );
    }

    #[test]
    fn ambiguous_leaf_is_not_matched() {
        // Two hub keys share the leaf `addr`; a spoke `bind` normalises to a
        // different leaf and finds no full match — the ambiguous leaf is skipped,
        // so it's reported as an orphan rather than an arbitrary pick.
        let hub = vec![
            ConfigKey {
                file: "h".into(),
                key: "serve.addr".into(),
                value: "a".into(),
            },
            ConfigKey {
                file: "h".into(),
                key: "db.addr".into(),
                value: "b".into(),
            },
        ];
        let spoke = vec![ConfigKey {
            file: "s".into(),
            key: "addr".into(),
            value: "x".into(),
        }];
        let (m, orphans) = match_against_hub(&spoke, &hub);
        assert!(m.is_empty(), "ambiguous leaf must not match: {m:?}");
        assert_eq!(orphans.len(), 1);
    }

    #[test]
    fn normalize_bridges_naming_conventions() {
        assert_eq!(normalize("SERVE_ADDR"), "serve.addr");
        assert_eq!(normalize("serve.addr"), "serve.addr");
        assert_eq!(normalize("serve-addr"), "serve.addr");
    }

    #[test]
    fn matching_finds_correspondences_and_orphans() {
        let hub = vec![
            ConfigKey {
                file: "h".into(),
                key: "serve.addr".into(),
                value: "127.0.0.1:8017".into(),
            },
            ConfigKey {
                file: "h".into(),
                key: "models.generative".into(),
                value: "qwen3-0.6b".into(),
            },
        ];
        let spoke = vec![
            // exact name, same value → highest confidence
            ConfigKey {
                file: "s".into(),
                key: "SERVE_ADDR".into(),
                value: "127.0.0.1:8017".into(),
            },
            // exact name, different value → strong but not top
            ConfigKey {
                file: "s".into(),
                key: "models.generative".into(),
                value: "qwen3-8b".into(),
            },
            // no counterpart → orphan (drift candidate)
            ConfigKey {
                file: "s".into(),
                key: "serve.max_connections".into(),
                value: "512".into(),
            },
        ];
        let (matches, orphans) = match_against_hub(&spoke, &hub);
        assert_eq!(matches.len(), 2);
        let addr = matches
            .iter()
            .find(|m| m.spoke_key == "SERVE_ADDR")
            .unwrap();
        assert_eq!(addr.hub_key, "serve.addr");
        assert!(addr.confidence >= 0.95, "same value → high confidence");
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].key, "serve.max_connections");
    }
}

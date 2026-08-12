//! Config-file → flat config-key parsing (ADR-0009).
//!
//! A deployment/config repo is mostly key/value files. This module flattens
//! TOML, JSON, and `.env` into dotted **leaf keys**, used two ways: the
//! extraction pipeline turns them into `config_key` graph nodes (so config keys
//! are queryable and visible in the graph), and `roteiro links --infer` matches
//! them across repos. One parser, so the graph and the matcher never disagree.
//!
//! Deterministic — TOML/JSON object iteration is sorted (both back onto ordered
//! maps here), and the caller sorts emitted nodes; `.env` preserves file order.
//! Dependency-free beyond `toml`/`serde_json` (already in the tree); YAML is out
//! of scope (it would need a new parser).

/// The `NodeKind::Other` token for a config-key node (`cfgkey:<file>#<dotted>`).
/// Shared by the extractor that emits them and the store reader that finds them.
pub(crate) const KIND: &str = "config_key";

/// A single leaf config setting: its dotted key, source file, and value.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ConfigKey {
    /// Repo-relative file the key was read from.
    pub file: String,
    /// Dotted key path (e.g. `serve.addr`), verbatim from the source.
    pub key: String,
    /// The scalar (or compact list/object) value, as a string. String scalars are
    /// **unquoted** so the same setting compares across TOML / JSON / `.env`.
    pub value: String,
}

/// The lowercased file extension, if any (`config.TOML` → `toml`).
fn ext_lower(path: &str) -> Option<String> {
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
}

/// Whether a repo-relative path is a config file this module understands:
/// `*.toml`, `*.json`, `*.env`, or a dotenv name (`.env`, `.env.<x>`).
#[must_use]
pub fn is_config_path(path: &str) -> bool {
    let base = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    matches!(ext_lower(path).as_deref(), Some("toml" | "json" | "env"))
        || base == ".env"
        || base.starts_with(".env.")
}

/// Flatten a config file's bytes into leaf keys, dispatched by extension. An
/// unparseable file yields nothing (a config we can't read is not an error here).
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

/// A TOML leaf value as a plain string — strings unquoted, so they compare with
/// env/JSON; other scalars and arrays keep their canonical rendering.
fn toml_scalar(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// A JSON leaf value as a plain string — strings unquoted, matching [`toml_scalar`].
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

/// Parse `KEY=VALUE` lines (skipping blanks / `#` comments), stripping surrounding
/// single/double quote characters from the value.
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

/// Whether a config key's *name* looks like it holds a secret (token, password,
/// credential, …). Extraction **redacts the value** of such keys so secrets from
/// `.env`/config files are never persisted into the graph store (which is
/// queryable and exportable). Matched against the key with separators removed, so
/// `API_KEY`, `apiKey`, and `api-key` all count.
#[must_use]
pub fn is_secret_key(key: &str) -> bool {
    const NEEDLES: &[&str] = &[
        "secret",
        "password",
        "passwd",
        "passphrase",
        "token",
        "apikey",
        "credential",
        "privatekey",
        "accesskey",
        "pwd",
    ];
    let flat: String = key
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    NEEDLES.iter().any(|n| flat.contains(n))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flatten_unquotes_strings_and_treats_arrays_as_one_leaf() {
        let toml = flatten(
            "a.toml",
            b"[serve]\naddr = \"0.0.0.0:8443\"\nmodels = [\"q8\"]\n",
        );
        assert!(
            toml.iter()
                .any(|k| k.key == "serve.addr" && k.value == "0.0.0.0:8443")
        );
        assert!(toml.iter().any(|k| k.key == "serve.models"));
        let json = flatten(
            "a.json",
            br#"{"serve":{"addr":"0.0.0.0:8443","tools":false}}"#,
        );
        assert!(
            json.iter()
                .any(|k| k.key == "serve.addr" && k.value == "0.0.0.0:8443")
        );
        assert!(
            json.iter()
                .any(|k| k.key == "serve.tools" && k.value == "false")
        );
        let env = flatten(".env", b"# c\nexport SERVE_ADDR=127.0.0.1:8017\n");
        assert!(
            env.iter()
                .any(|k| k.key == "SERVE_ADDR" && k.value == "127.0.0.1:8017")
        );
    }

    #[test]
    fn is_config_path_matches_toml_json_env() {
        assert!(!is_config_path("values.prod.yaml")); // YAML is out of scope
        assert!(is_config_path("config.toml"));
        assert!(is_config_path("a/b.json"));
        assert!(is_config_path(".env"));
        assert!(is_config_path(".env.local"));
        assert!(is_config_path("prod.env"));
        assert!(!is_config_path("src/main.rs"));
    }

    #[test]
    fn normalize_bridges_conventions() {
        assert_eq!(normalize("SERVE_ADDR"), "serve.addr");
        assert_eq!(normalize("serve-addr"), "serve.addr");
    }

    #[test]
    fn secret_keys_are_flagged_across_conventions() {
        for k in [
            "API_TOKEN",
            "apiKey",
            "db.password",
            "AWS_SECRET_ACCESS_KEY",
            "PWD",
        ] {
            assert!(is_secret_key(k), "{k} should be secret");
        }
        for k in ["serve.addr", "models.generative", "port", "workspace.roots"] {
            assert!(!is_secret_key(k), "{k} should not be secret");
        }
    }
}

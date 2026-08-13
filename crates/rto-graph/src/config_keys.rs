//! Config-file → flat config-key parsing (ADR-0009).
//!
//! A deployment/config repo is mostly key/value files. This module flattens
//! TOML, JSON, `.env`, and **YAML** into dotted **leaf keys**, used two ways: the
//! extraction pipeline turns them into `config_key` graph nodes (so config keys
//! are queryable and visible in the graph), and `roteiro links --infer` matches
//! them across repos. One parser, so the graph and the matcher never disagree.
//!
//! YAML gets special handling because a Kubernetes spoke repo is mostly YAML: a
//! **k8s manifest** (a document with `apiVersion` + `kind`) is *not* flattened
//! wholesale — that would bury real config under `apiVersion`/`metadata` noise —
//! but mined for the settings a deployment actually overrides: ConfigMap/Secret
//! `data`, and each container's `image` and literal `env` vars (Secret values
//! and secret-looking keys redacted). Any other YAML — a Helm `values.yaml`, a
//! kustomization, a plain config — is flattened like TOML/JSON.
//!
//! Deterministic — TOML/JSON/YAML object iteration is sorted (the caller sorts
//! emitted nodes); `.env` preserves file order. Parsers (`toml`, `serde_json`,
//! `yaml-rust2`) are all permissive and `cargo deny`-clean.

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
/// `*.toml`, `*.json`, `*.yaml`, `*.yml`, `*.env`, or a dotenv name (`.env`,
/// `.env.<x>`).
///
/// `.github/` is excluded: CI workflows and repo metadata are YAML but not *app*
/// config, so mining them would bury a spoke's real overrides under `jobs`/`steps`
/// noise (and add nothing to a hub repo's graph).
#[must_use]
pub fn is_config_path(path: &str) -> bool {
    if path == ".github" || path.starts_with(".github/") {
        return false;
    }
    let base = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    matches!(
        ext_lower(path).as_deref(),
        Some("toml" | "json" | "yaml" | "yml" | "env")
    ) || base == ".env"
        || base.starts_with(".env.")
}

/// Whether a repo-relative config path is **build / tooling / CI** config rather
/// than an application's own config — a `Cargo.toml`, a `rustfmt.toml`, a CI
/// workflow, and so on. Used only by *opt-in* filters (`--app-config-only`, the
/// explorer's "hide tooling config" toggle): the default everywhere is to show
/// every config key, so this classifier never changes what is extracted or stored.
///
/// **Conservative by design** — it returns `true` only for a curated allow-list of
/// well-known tooling names and directories, so real app config is never hidden by
/// mistake. A file it doesn't recognise (e.g. `config/app.toml`, `values.yaml`,
/// `prod.env`) is treated as app config. The list is meant to grow; add new
/// well-known tooling files to the `match` (basename) or the directory checks.
///
/// Matches on the **file-path component** of a `cfgkey:<file>#<dotted>` node, so
/// callers extract that path (see the CLI's `--app-config-only`) before calling.
///
/// Covered today:
/// - Rust build/tooling: `Cargo.toml`, `Cargo.lock`, `rust-toolchain[.toml]`,
///   `rustfmt.toml` / `.rustfmt.toml`, `clippy.toml`, `deny.toml`, `release-plz.toml`.
/// - Cargo's own config: `.cargo/config` / `.cargo/config.toml`.
/// - Anything under `.config/` (e.g. `.config/nextest.toml`).
/// - Anything under `.github/` (CI workflows, `dependabot.yml`).
/// - `.gitlab-ci.yml`.
#[must_use]
pub fn is_tooling_config_path(path: &str) -> bool {
    // Path components, ignoring any leading `./` or empty segments. Repo-relative
    // paths use `/`, matching the `cfgkey:<file>` ids these are checked against.
    let segments: Vec<&str> = path
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    let base = segments.last().copied().unwrap_or(path);
    let base_lower = base.to_ascii_lowercase();

    // Directory-scoped: a whole directory that is tooling/CI, not app config.
    // `.github/` — CI workflows + repo metadata. `.config/` — nextest & friends.
    if segments.iter().any(|s| *s == ".github" || *s == ".config") {
        return true;
    }

    // `.cargo/config` or `.cargo/config.toml` — cargo's own build config. Scoped
    // to that exact file inside `.cargo/`, so an unrelated `.cargo/app.toml` isn't
    // swept up.
    if segments.len() >= 2
        && segments[segments.len() - 2] == ".cargo"
        && matches!(base_lower.as_str(), "config" | "config.toml")
    {
        return true;
    }

    // Well-known tooling files by basename, anywhere in the tree (a vendored crate
    // carries its own `Cargo.toml`, and it's tooling there too).
    matches!(
        base_lower.as_str(),
        "cargo.toml"
            | "cargo.lock"
            | "rust-toolchain"
            | "rust-toolchain.toml"
            | "rustfmt.toml"
            | ".rustfmt.toml"
            | "clippy.toml"
            | "deny.toml"
            | "release-plz.toml"
            | ".gitlab-ci.yml"
    )
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
        Some("yaml" | "yml") => flatten_yaml(text, path, &mut out),
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

use yaml_rust2::Yaml;

/// A YAML scalar as a plain string (strings unquoted, like [`toml_scalar`]);
/// `None` for containers/aliases/bad values (handled by recursion, not as leaves).
fn yaml_scalar(v: &Yaml) -> Option<String> {
    match v {
        // `Real` already holds its source text, so it renders like a string scalar.
        Yaml::String(s) | Yaml::Real(s) => Some(s.clone()),
        Yaml::Integer(i) => Some(i.to_string()),
        Yaml::Boolean(b) => Some(b.to_string()),
        Yaml::Null => Some("null".to_owned()),
        _ => None,
    }
}

/// The string value at `key` in a YAML mapping, if present and scalar-stringy.
fn yaml_get_str<'a>(doc: &'a Yaml, key: &str) -> Option<&'a str> {
    doc.as_hash()?.get(&Yaml::String(key.to_owned()))?.as_str()
}

/// The array at `key` in a YAML mapping, if present.
fn yaml_get_vec<'a>(doc: &'a Yaml, key: &str) -> Option<&'a Vec<Yaml>> {
    doc.as_hash()?.get(&Yaml::String(key.to_owned()))?.as_vec()
}

/// Parse every YAML document in `text` (multi-document `---` streams included),
/// dispatching each to k8s-aware mining or a plain flatten. An unparseable stream
/// yields nothing.
fn flatten_yaml(text: &str, file: &str, out: &mut Vec<ConfigKey>) {
    let Ok(docs) = yaml_rust2::YamlLoader::load_from_str(text) else {
        return;
    };
    for doc in &docs {
        match k8s_kind(doc) {
            Some(kind) => flatten_k8s(doc, &kind, file, out),
            None => flatten_yaml_node(doc, "", file, out),
        }
    }
}

/// Flatten an arbitrary YAML document like TOML/JSON: recurse mappings, treat
/// scalars and arrays as leaves (an array renders as one compact leaf).
fn flatten_yaml_node(v: &Yaml, prefix: &str, file: &str, out: &mut Vec<ConfigKey>) {
    match v {
        Yaml::Hash(h) => {
            for (k, val) in h {
                if let Some(k) = k.as_str() {
                    flatten_yaml_node(val, &join(prefix, k), file, out);
                }
            }
        }
        Yaml::Array(items) => {
            // An all-scalar array renders as one compact leaf; if any element is a
            // map/array, emit a sentinel rather than silently dropping it to `[]`
            // (which would mislead the matcher/diff into a false equality).
            let parts: Vec<Option<String>> = items.iter().map(yaml_scalar).collect();
            let value = if parts.iter().all(Option::is_some) {
                let scalars: Vec<String> = parts.into_iter().flatten().collect();
                format!("[{}]", scalars.join(", "))
            } else {
                format!("[<{} items>]", items.len())
            };
            push(out, file, prefix, value);
        }
        other => {
            if let Some(s) = yaml_scalar(other) {
                push(out, file, prefix, s);
            }
        }
    }
}

/// The `kind` of a document that is a Kubernetes resource (a mapping carrying
/// both `apiVersion` and `kind`), else `None`.
fn k8s_kind(doc: &Yaml) -> Option<String> {
    let h = doc.as_hash()?;
    let has = |k: &str| h.contains_key(&Yaml::String(k.to_owned()));
    (has("apiVersion") && has("kind"))
        .then(|| yaml_get_str(doc, "kind").map(str::to_owned))
        .flatten()
}

/// Mine a k8s resource for the settings a deployment actually overrides, rather
/// than flattening its structural noise.
fn flatten_k8s(doc: &Yaml, kind: &str, file: &str, out: &mut Vec<ConfigKey>) {
    match kind {
        // ConfigMap `data` is literally the app's config; keys stand alone.
        "ConfigMap" => k8s_data(doc, "data", file, out, false),
        // A Secret's `data`/`stringData` are secret by definition — always redacted.
        "Secret" => {
            k8s_data(doc, "data", file, out, true);
            k8s_data(doc, "stringData", file, out, true);
        }
        // Workload kinds carry a pod template — mine its containers.
        _ => {
            if let Some(pod) = k8s_pod_spec(doc, kind) {
                k8s_containers(pod, file, out);
            }
        }
    }
}

/// Emit each entry of a k8s `data`/`stringData` mapping as a config key. When
/// `redact`, the value is replaced with `<redacted>` (a Secret's data is secret
/// even when the key name isn't); otherwise the caller's secret-key redaction
/// still applies to secret-looking names.
fn k8s_data(doc: &Yaml, field: &str, file: &str, out: &mut Vec<ConfigKey>, redact: bool) {
    let Some(map) = doc
        .as_hash()
        .and_then(|h| h.get(&Yaml::String(field.to_owned())))
        .and_then(Yaml::as_hash)
    else {
        return;
    };
    for (k, v) in map {
        let Some(k) = k.as_str() else { continue };
        if redact {
            // A Secret's value is secret whatever its shape — always redact.
            push(out, file, k, "<redacted>".to_owned());
        } else if let Some(value) = yaml_scalar(v) {
            push(out, file, k, value);
        }
        // A non-scalar ConfigMap value is skipped rather than emitted as `""`,
        // which would mislead the matcher/diff.
    }
}

/// Navigate to the pod spec (the mapping that holds `containers`) for a workload
/// `kind`, or `None` for kinds that carry no pod template.
fn k8s_pod_spec<'a>(doc: &'a Yaml, kind: &str) -> Option<&'a Yaml> {
    let path: &[&str] = match kind {
        "Pod" => &["spec"],
        "Deployment" | "StatefulSet" | "DaemonSet" | "ReplicaSet" | "Job" => {
            &["spec", "template", "spec"]
        }
        "CronJob" => &["spec", "jobTemplate", "spec", "template", "spec"],
        _ => return None,
    };
    let mut cur = doc;
    for seg in path {
        cur = cur.as_hash()?.get(&Yaml::String((*seg).to_owned()))?;
    }
    Some(cur)
}

/// Mine each container (and init container) in a pod spec for its `image` (keyed
/// `container.<name>.image`) and each literal `env` var (keyed by the env name,
/// so it matches a hub `.env`/config setting). Env vars sourced from `valueFrom`
/// carry no literal value here and are skipped.
fn k8s_containers(pod: &Yaml, file: &str, out: &mut Vec<ConfigKey>) {
    for field in ["containers", "initContainers"] {
        let Some(list) = yaml_get_vec(pod, field) else {
            continue;
        };
        for c in list {
            let cname = yaml_get_str(c, "name").unwrap_or("container");
            if let Some(image) = yaml_get_str(c, "image") {
                push(
                    out,
                    file,
                    &format!("container.{cname}.image"),
                    image.to_owned(),
                );
            }
            if let Some(env) = yaml_get_vec(c, "env") {
                for e in env {
                    if let (Some(name), Some(value)) =
                        (yaml_get_str(e, "name"), yaml_get_str(e, "value"))
                    {
                        push(out, file, name, value.to_owned());
                    }
                }
            }
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

/// Canonicalise a dotted key for cross-**naming-convention** matching: keep the
/// dotted structure, but collapse each `.`-delimited segment to its lowercased
/// ASCII-alphanumerics only — dropping `_`, `-`, and any other punctuation within
/// the segment. So within a segment `serverEndpoint`, `server_endpoint`, and
/// `server-endpoint` all become `serverendpoint`, letting a Kubernetes YAML
/// `zerobus.serverEndpoint` (`camelCase`) match an app TOML `zerobus.server_endpoint`
/// (`snake_case`) that [`normalize`] keeps apart — `normalize` splits on *any* run
/// of non-ASCII-alphanumeric chars, so `_` becomes a boundary
/// (`zerobus.server.endpoint`) and a compound leaf never lines up with its
/// `camelCase` spelling. The dotted structure is preserved here (segments are split
/// on `.` only) so `a.b` and `ab` stay distinct.
#[must_use]
pub fn canonicalize(key: &str) -> String {
    key.split('.')
        .map(|seg| {
            seg.chars()
                .filter(char::is_ascii_alphanumeric)
                .map(|c| c.to_ascii_lowercase())
                .collect::<String>()
        })
        .filter(|s| !s.is_empty())
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
    fn is_config_path_matches_toml_json_yaml_env() {
        assert!(is_config_path("values.prod.yaml")); // YAML is in scope (k8s spokes)
        assert!(is_config_path("deploy.yml"));
        assert!(is_config_path("config.toml"));
        assert!(is_config_path("a/b.json"));
        assert!(is_config_path(".env"));
        assert!(is_config_path(".env.local"));
        assert!(is_config_path("prod.env"));
        assert!(!is_config_path("src/main.rs"));
        // CI workflows are YAML but not app config — excluded.
        assert!(!is_config_path(".github/workflows/ci.yml"));
        assert!(!is_config_path(".github/dependabot.yml"));
    }

    #[test]
    fn tooling_config_paths_are_flagged_conservatively() {
        // Well-known build/tooling/CI files → tooling (hidden by the opt-in filter).
        for p in [
            "Cargo.toml",
            "Cargo.lock",
            "crates/rto-graph/Cargo.toml", // a workspace member's manifest, too
            "vendor/some-crate/Cargo.toml", // a vendored crate's manifest
            "rust-toolchain",
            "rust-toolchain.toml",
            "rustfmt.toml",
            ".rustfmt.toml",
            "clippy.toml",
            "deny.toml",
            "release-plz.toml",
            ".config/nextest.toml",
            ".cargo/config",
            ".cargo/config.toml",
            ".github/workflows/ci.yml",
            ".github/dependabot.yml",
            ".gitlab-ci.yml",
        ] {
            assert!(is_tooling_config_path(p), "{p} should be tooling config");
        }

        // Ordinary application config → NOT tooling (always shown; never misclassified).
        for p in [
            "config/app.toml",
            "values.yaml",
            "values.prod.yaml",
            "prod.env",
            ".env",
            ".env.local",
            "zerobus-example.toml",
            "deploy/service.json",
            "settings/config.toml", // a plain `config.toml`, not under `.cargo/`
            ".cargo/app.toml",      // an unrelated file that merely lives under `.cargo/`
        ] {
            assert!(!is_tooling_config_path(p), "{p} should be app config");
        }
    }

    #[test]
    fn yaml_helm_values_flatten_like_toml() {
        let ks = flatten(
            "values.yaml",
            b"service:\n  addr: 0.0.0.0:8443\n  tools: false\nreplicas: 3\nmodels:\n  - a\n  - b\n",
        );
        assert!(
            ks.iter()
                .any(|k| k.key == "service.addr" && k.value == "0.0.0.0:8443"),
            "{ks:?}"
        );
        assert!(
            ks.iter()
                .any(|k| k.key == "service.tools" && k.value == "false")
        );
        assert!(ks.iter().any(|k| k.key == "replicas" && k.value == "3"));
        // An array is one compact leaf (as for TOML/JSON).
        assert!(ks.iter().any(|k| k.key == "models" && k.value == "[a, b]"));
    }

    #[test]
    fn yaml_array_of_objects_emits_a_sentinel_not_empty() {
        // An array whose elements are maps must not silently render as `[]` (which
        // would false-match another empty list) — a sentinel signals the structure.
        let ks = flatten(
            "values.yaml",
            b"ingress:\n  hosts:\n    - host: a.example\n      paths: [/]\n    - host: b.example\n",
        );
        let hosts = ks
            .iter()
            .find(|k| k.key == "ingress.hosts")
            .expect("hosts leaf");
        assert_eq!(hosts.value, "[<2 items>]", "non-scalar array is a sentinel");
    }

    #[test]
    fn k8s_configmap_skips_non_scalar_values() {
        // A ConfigMap whose value is itself a map must be skipped, not emitted as "".
        let cm = b"apiVersion: v1\nkind: ConfigMap\ndata:\n  flat: ok\n  nested:\n    a: 1\n";
        let ks = flatten("cm.yaml", cm);
        assert!(ks.iter().any(|k| k.key == "flat" && k.value == "ok"));
        assert!(
            !ks.iter().any(|k| k.key == "nested"),
            "non-scalar ConfigMap value skipped, not emitted empty: {ks:?}"
        );
    }

    #[test]
    fn k8s_manifest_mines_config_not_structural_noise() {
        // A Deployment: env + image are mined; apiVersion/metadata are not.
        let dep = b"apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: api\nspec:\n  template:\n    spec:\n      containers:\n        - name: api\n          image: registry/app:1.2\n          env:\n            - name: SERVE_ADDR\n              value: 0.0.0.0:8443\n            - name: DB_HOST\n              valueFrom:\n                secretKeyRef:\n                  name: db\n";
        let ks = flatten("deploy.yaml", dep);
        assert!(
            ks.iter()
                .any(|k| k.key == "SERVE_ADDR" && k.value == "0.0.0.0:8443"),
            "env var mined as a bare key so it matches a hub .env: {ks:?}"
        );
        assert!(
            ks.iter()
                .any(|k| k.key == "container.api.image" && k.value == "registry/app:1.2"),
            "{ks:?}"
        );
        // valueFrom env has no literal value → skipped; structural noise absent.
        assert!(!ks.iter().any(|k| k.key == "DB_HOST"));
        assert!(
            !ks.iter()
                .any(|k| k.key.starts_with("apiVersion") || k.key.contains("metadata"))
        );
    }

    #[test]
    fn k8s_configmap_and_secret_data_with_redaction() {
        let cm = b"apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: c\ndata:\n  serve.addr: 127.0.0.1:8017\n  log_level: info\n";
        let ks = flatten("cm.yaml", cm);
        assert!(
            ks.iter()
                .any(|k| k.key == "serve.addr" && k.value == "127.0.0.1:8017")
        );
        assert!(ks.iter().any(|k| k.key == "log_level" && k.value == "info"));

        // A Secret's data is always redacted — even a non-secret-looking key name.
        let sec = b"apiVersion: v1\nkind: Secret\ndata:\n  database-url: aHR0cA==\n";
        let ks = flatten("secret.yaml", sec);
        assert!(
            ks.iter()
                .any(|k| k.key == "database-url" && k.value == "<redacted>"),
            "secret data must be redacted regardless of key name: {ks:?}"
        );
    }

    #[test]
    fn yaml_multi_document_stream_mines_each_doc() {
        // One file, two docs (`---`): a ConfigMap and a Deployment.
        let stream = b"apiVersion: v1\nkind: ConfigMap\ndata:\n  port: \"8443\"\n---\napiVersion: apps/v1\nkind: Deployment\nspec:\n  template:\n    spec:\n      containers:\n        - name: web\n          image: app:2.0\n";
        let ks = flatten("bundle.yaml", stream);
        assert!(ks.iter().any(|k| k.key == "port" && k.value == "8443"));
        assert!(
            ks.iter()
                .any(|k| k.key == "container.web.image" && k.value == "app:2.0")
        );
    }

    #[test]
    fn normalize_bridges_conventions() {
        assert_eq!(normalize("SERVE_ADDR"), "serve.addr");
        assert_eq!(normalize("serve-addr"), "serve.addr");
    }

    #[test]
    fn canonicalize_bridges_camel_snake_kebab_within_a_segment() {
        // The three spellings of a compound leaf collapse to one canonical form,
        // which `normalize` (separator-as-boundary) keeps apart.
        assert_eq!(
            canonicalize("zerobus.serverEndpoint"),
            "zerobus.serverendpoint"
        );
        assert_eq!(
            canonicalize("zerobus.server_endpoint"),
            "zerobus.serverendpoint"
        );
        assert_eq!(
            canonicalize("zerobus.server-endpoint"),
            "zerobus.serverendpoint"
        );
        assert_ne!(
            normalize("zerobus.server_endpoint"),
            normalize("zerobus.serverEndpoint"),
            "normalize splits snake_case on `_`, so it cannot bridge camelCase"
        );
        // Dotted structure is preserved: `a.b` must not collapse into `ab`.
        assert_ne!(canonicalize("a.b"), canonicalize("ab"));
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

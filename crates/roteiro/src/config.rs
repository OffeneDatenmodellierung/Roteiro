//! Layered project/user configuration (ADR-0007).
//!
//! Reads an optional **project** `roteiro.toml` (at the repository root — the
//! nearest ancestor holding a `.git` entry) and an optional **user**
//! `~/.roteiro/config.toml`, then
//! merges them so that, per value, the precedence is
//! **CLI flag > project > user > built-in default**. This module resolves the
//! *config* layers (project over user); the CLI-vs-config precedence is applied
//! at each call site (a flag, when present, wins over the resolved config).
//!
//! Every field is optional and every consumer has a built-in default, so **no
//! config is a working default**. TOML only (YAML is intentionally unsupported —
//! `serde_yaml` is unmaintained). Unknown keys are ignored (forward-compatible);
//! a malformed file is a hard error, never a silent partial parse.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The merged configuration plus the layers it came from (for provenance).
#[derive(Debug, Default)]
pub struct Loaded {
    /// The effective, merged config used at run time (project over user).
    pub effective: Config,
    /// The user-layer config (`~/.roteiro/config.toml`), for provenance.
    pub user: Config,
    /// The project-layer config (`roteiro.toml`), for provenance.
    pub project: Config,
    /// Path the user config was read from, if present.
    pub user_path: Option<PathBuf>,
    /// Path the project config was read from, if present.
    pub project_path: Option<PathBuf>,
}

/// Roteiro configuration. All fields optional; see the module docs.
#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    /// Per-project model picks (override the tier defaults).
    pub models: ModelsConfig,
    /// `roteiro infer` tuning.
    pub infer: InferConfig,
    /// `roteiro duplicates` tuning.
    pub duplicates: DuplicatesConfig,
    /// `roteiro sync` content-ingestion toggles.
    pub ingest: IngestConfig,
    /// `roteiro serve --models` local endpoint settings.
    pub serve: ServeConfig,
    /// `roteiro debt` tuning (paths excluded from the intent-debt scan).
    pub debt: DebtConfig,
    /// Filesystem locations (the model store).
    pub paths: PathsConfig,
}

/// `[debt]` — intent-debt reporting.
#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct DebtConfig {
    /// Glob patterns whose matching files are excluded from the intent-debt
    /// report (e.g. `["vendor/**", "**/generated/*"]`). Patterns are matched
    /// anchored end-to-end against the whole repo-relative path (not a substring
    /// match): `*`/`?` match within a path segment, `**` matches across segments.
    pub ignore: Option<Vec<String>>,
}

/// `[paths]` — filesystem locations.
#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct PathsConfig {
    /// The model store directory (default `~/.roteiro/models`, or
    /// `$ROTEIRO_HOME/models`). A leading `~/` is expanded to the home directory.
    pub model_store: Option<String>,
}

/// `[models]` — override the registry tier defaults for this project.
#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct ModelsConfig {
    /// Registry name of the embedding model `infer --model` defaults to.
    pub embedding: Option<String>,
    /// Registry name of the generative model `spec draft` defaults to.
    pub generative: Option<String>,
}

/// `[ingest]` — which blob content `roteiro sync` extracts for embedding. Each
/// toggle is `Some(false)` to disable a content class the binary supports; unset
/// (or `true`) leaves it on. A toggle cannot enable a class the binary was not
/// built with (the `pdf-text`/`image-ocr`/`image-vision` features).
#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct IngestConfig {
    /// Embed the UTF-8 body of prose files (Markdown, plain text).
    pub prose: Option<bool>,
    /// Extract text from PDF documents.
    pub pdf: Option<bool>,
    /// OCR literal text from images.
    pub ocr: Option<bool>,
    /// Describe images with a vision model.
    pub vision: Option<bool>,
}

impl IngestConfig {
    /// Resolve to the graph-layer [`rto_graph::IngestConfig`], defaulting each
    /// unset toggle to on.
    #[must_use]
    pub fn resolve(&self) -> rto_graph::IngestConfig {
        let default = rto_graph::IngestConfig::default();
        rto_graph::IngestConfig {
            prose: self.prose.unwrap_or(default.prose),
            pdf: self.pdf.unwrap_or(default.pdf),
            ocr: self.ocr.unwrap_or(default.ocr),
            vision: self.vision.unwrap_or(default.vision),
        }
    }
}

/// `[serve]` — the opt-in local OpenAI-compatible model endpoint (ADR-0006).
#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct ServeConfig {
    /// Bind address for `roteiro serve --models` (default `127.0.0.1:8017`).
    pub addr: Option<String>,
    /// Restrict which installed generative models to serve (default: all
    /// installed generative models).
    pub models: Option<Vec<String>>,
    /// Auto-register the graph tools so the served model can query the codebase
    /// (ADR-0006). Default `true`.
    pub tools: Option<bool>,
    /// Approximate memory budget (MiB) for models kept resident at once. The
    /// engine loads models on demand and unloads the least-recently-used once the
    /// resident set (proxied by GGUF size) exceeds this. Unset/`0` keeps a single
    /// model resident — set it higher to keep several warm and swap in real time.
    pub memory_budget_mb: Option<u64>,
    /// PEM certificate-chain file for in-app TLS. When both this and `tls_key`
    /// are set, `serve --models` terminates HTTPS itself (needs `--features
    /// serve`); otherwise it serves plain HTTP (front with a proxy for TLS).
    pub tls_cert: Option<String>,
    /// PEM private-key file paired with `tls_cert` (PKCS#8 or RSA).
    pub tls_key: Option<String>,
}

/// `[infer]` — defaults for the similarity-inference command.
#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct InferConfig {
    /// Minimum cosine similarity for a suggested edge (`0.0..=1.0`).
    pub min_confidence: Option<f64>,
    /// Maximum suggestions per node.
    pub top_k: Option<usize>,
}

/// `[duplicates]` — defaults for the duplicate-detection command.
#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct DuplicatesConfig {
    /// Minimum cosine similarity for a near-duplicate pair (`0.0..=1.0`).
    pub min_similarity: Option<f64>,
    /// Maximum pairs to report.
    pub limit: Option<usize>,
}

impl Config {
    /// Overlay `over` on top of `self`, taking `over`'s value wherever set — used
    /// to apply the project layer on top of the user layer.
    fn overlaid_with(&self, over: &Config) -> Config {
        Config {
            models: ModelsConfig {
                embedding: over
                    .models
                    .embedding
                    .clone()
                    .or(self.models.embedding.clone()),
                generative: over
                    .models
                    .generative
                    .clone()
                    .or(self.models.generative.clone()),
            },
            infer: InferConfig {
                min_confidence: over.infer.min_confidence.or(self.infer.min_confidence),
                top_k: over.infer.top_k.or(self.infer.top_k),
            },
            duplicates: DuplicatesConfig {
                min_similarity: over
                    .duplicates
                    .min_similarity
                    .or(self.duplicates.min_similarity),
                limit: over.duplicates.limit.or(self.duplicates.limit),
            },
            ingest: IngestConfig {
                prose: over.ingest.prose.or(self.ingest.prose),
                pdf: over.ingest.pdf.or(self.ingest.pdf),
                ocr: over.ingest.ocr.or(self.ingest.ocr),
                vision: over.ingest.vision.or(self.ingest.vision),
            },
            serve: ServeConfig {
                addr: over.serve.addr.clone().or(self.serve.addr.clone()),
                models: over.serve.models.clone().or(self.serve.models.clone()),
                tools: over.serve.tools.or(self.serve.tools),
                memory_budget_mb: over.serve.memory_budget_mb.or(self.serve.memory_budget_mb),
                tls_cert: over.serve.tls_cert.clone().or(self.serve.tls_cert.clone()),
                tls_key: over.serve.tls_key.clone().or(self.serve.tls_key.clone()),
            },
            debt: DebtConfig {
                ignore: over.debt.ignore.clone().or(self.debt.ignore.clone()),
            },
            paths: PathsConfig {
                model_store: over
                    .paths
                    .model_store
                    .clone()
                    .or(self.paths.model_store.clone()),
            },
        }
    }
}

/// Load and merge the user and project config layers, starting the project
/// search from `cwd`.
///
/// # Errors
/// Returns an error if a config file exists but cannot be read or parsed
/// (malformed TOML is a hard error, never a silent partial parse).
pub fn load(cwd: &Path) -> anyhow::Result<Loaded> {
    let user_path = user_config_path().filter(|p| p.is_file());
    let project_path = find_project_config(cwd);
    load_from(user_path, project_path)
}

/// Read and merge explicit user/project config paths — the env-free core of
/// [`load`], so tests can exercise layering without mutating global state.
fn load_from(user_path: Option<PathBuf>, project_path: Option<PathBuf>) -> anyhow::Result<Loaded> {
    let user = read_config(user_path.as_deref())?;
    let project = read_config(project_path.as_deref())?;
    let effective = user.overlaid_with(&project);
    Ok(Loaded {
        effective,
        user,
        project,
        user_path,
        project_path,
    })
}

/// Parse a config file, or the default config if `path` is `None`.
fn read_config(path: Option<&Path>) -> anyhow::Result<Config> {
    let Some(path) = path else {
        return Ok(Config::default());
    };
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("reading config {}: {e}", path.display()))?;
    toml::from_str(&text).map_err(|e| anyhow::anyhow!("parsing config {}: {e}", path.display()))
}

/// The user config path: `$ROTEIRO_HOME/config.toml`, else `~/.roteiro/config.toml`
/// (mirrors the model store's home resolution).
fn user_config_path() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("ROTEIRO_HOME") {
        return Some(PathBuf::from(home).join("config.toml"));
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".roteiro").join("config.toml"))
}

/// Find the project `roteiro.toml` at the **repository root** — the nearest
/// ancestor of `start` that contains a `.git` entry (per ADR-0007, the project
/// config lives alongside the git dir). Bounding discovery to the repo root
/// keeps it from ascending into parent directories *outside* the repo and stops
/// a `roteiro.toml` in a nested subdirectory from shadowing the repo-level one.
/// Returns `None` when `start` is not inside a git repository, or the root has
/// no `roteiro.toml`.
fn find_project_config(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        // `.git` is a directory in a normal clone but a file in worktrees and
        // submodules, so test existence rather than `is_dir`.
        if d.join(".git").exists() {
            let candidate = d.join("roteiro.toml");
            return candidate.is_file().then_some(candidate);
        }
        dir = d.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{Config, find_project_config, load_from};

    #[test]
    fn project_config_is_repo_root_bounded() {
        let root = std::env::temp_dir().join(format!("roteiro-disc-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let repo = root.join("repo");
        let sub = repo.join("crate").join("src");
        std::fs::create_dir_all(&sub).expect("mkdir");
        std::fs::create_dir_all(repo.join(".git")).expect("mkdir .git");

        // A `roteiro.toml` above the repo (in `root`) must NOT be picked up when
        // running from inside the repo — discovery stops at the repo root.
        std::fs::write(root.join("roteiro.toml"), "[infer]\ntop_k = 1\n").expect("write outside");
        assert_eq!(
            find_project_config(&sub),
            None,
            "no repo-root config → None, never the parent-dir one"
        );

        // With a config at the repo root, running from a nested subdir finds it.
        let at_root = repo.join("roteiro.toml");
        std::fs::write(&at_root, "[infer]\ntop_k = 2\n").expect("write root");
        assert_eq!(find_project_config(&sub), Some(at_root));

        // Not inside a git repo at all → None (the outside config is ignored).
        assert_eq!(find_project_config(&root), None);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn config_layering_precedence_and_errors() {
        let dir = std::env::temp_dir().join(format!("roteiro-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let user = dir.join("config.toml");
        let project = dir.join("roteiro.toml");

        // No config files → all defaults.
        let loaded = load_from(None, None).expect("load");
        assert_eq!(loaded.effective, Config::default());
        assert!(loaded.project_path.is_none());

        // User layer sets both infer knobs and an embedding model; project layer
        // overrides min_confidence only.
        std::fs::write(
            &user,
            "[infer]\nmin_confidence = 0.3\ntop_k = 9\n[models]\nembedding = \"bge-base-en-v1.5\"\n",
        )
        .expect("write user");
        std::fs::write(&project, "[infer]\nmin_confidence = 0.7\n").expect("write project");

        let loaded = load_from(Some(user.clone()), Some(project.clone())).expect("load");
        // project wins for min_confidence; user's top_k + embedding survive.
        assert_eq!(loaded.effective.infer.min_confidence, Some(0.7));
        assert_eq!(loaded.effective.infer.top_k, Some(9));
        assert_eq!(
            loaded.effective.models.embedding.as_deref(),
            Some("bge-base-en-v1.5")
        );
        assert_eq!(loaded.user.infer.min_confidence, Some(0.3));
        assert_eq!(loaded.project.infer.min_confidence, Some(0.7));

        // Unknown keys are ignored (forward-compatible).
        std::fs::write(&project, "[future]\nwhatever = true\n[infer]\ntop_k = 2\n").expect("write");
        let loaded = load_from(None, Some(project.clone())).expect("unknown keys ignored");
        assert_eq!(loaded.effective.infer.top_k, Some(2));

        // A malformed file is a hard error.
        std::fs::write(&project, "[infer]\nmin_confidence = = =\n").expect("write");
        assert!(
            load_from(None, Some(project)).is_err(),
            "malformed TOML must error"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}

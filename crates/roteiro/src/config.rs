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

use std::borrow::Cow;
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
    /// `roteiro media build`'s pre-generation gate.
    pub media: MediaConfig,
    /// `roteiro serve --models` local endpoint settings.
    pub serve: ServeConfig,
    /// `roteiro debt` tuning (paths excluded from the intent-debt scan).
    pub debt: DebtConfig,
    /// Filesystem locations (the model store).
    pub paths: PathsConfig,
    /// `[telemetry]` — opt-in structured file logging (ADR-0011). Unset ⇒ stdout
    /// only, unchanged.
    pub telemetry: TelemetryConfig,
    /// `roteiro serve --workspace` — repos a single server can host (ADR-0008).
    /// The legacy **single** workspace; still fully supported and, when it names
    /// any repos, folded in as the `default` linked workspace (see
    /// [`Config::resolved_workspaces`]).
    pub workspace: WorkspaceConfig,
    /// `[[workspaces]]` — additional **named**, linked workspaces (each a multi-repo
    /// graph whose cross-repo links resolve), the named form of the legacy single
    /// `[workspace]` (ADR-0008 multi-workspace).
    pub workspaces: Vec<NamedWorkspace>,
    /// `[standalone]` — repos each served as their **own** single-repo graph, with
    /// no cross-repo links. Discovered like `[workspace]` (roots scanned + explicit
    /// repos) but partitioned one-workspace-per-repo (ADR-0008 multi-workspace).
    pub standalone: WorkspaceConfig,
    /// `[[links]]` — authored cross-repo links to other workspace repos (ADR-0009).
    pub links: Vec<LinkDecl>,
    /// `[pins]` — how to map a deployed artifact to a hub git ref when the default
    /// tag guess doesn't fit this project's scheme (ADR-0009 step 8c). Keyed by the
    /// hub/image name; value is a ref template with a `{tag}` placeholder, e.g.
    /// `app = "release-{tag}"` maps image `app:1.2` → git ref `release-1.2`.
    pub pins: std::collections::BTreeMap<String, String>,
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

/// `[telemetry]` — opt-in structured file logging, the groundwork for a future
/// OpenTelemetry exporter (ADR-0011). Every field is optional; with the whole
/// table absent, Roteiro logs exactly as it does today — human-readable text on
/// stdout, nothing written to disk. Setting `file` (or the `--log-file` flag /
/// `ROTEIRO_LOG_FILE` env var, or the `--log` flag for the default path) turns on
/// a **second**, structured sink to a rotating file, leaving stdout untouched.
///
/// The name is `telemetry`, not `log`, deliberately: this table is the seam for
/// the deferred OTLP logs **and** metrics/traces exporter (ADR-0011), so it
/// should read as "observability config", not "the log file". Precedence is the
/// usual CLI flag / env var > project > user > built-in default (ADR-0007).
#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct TelemetryConfig {
    /// Path to the rotating log file. **Unset ⇒ file logging is off** (stdout
    /// only). A leading `~/` is expanded to the home directory; a relative path is
    /// resolved under `$ROTEIRO_HOME` (else `~/.roteiro`). When file logging is
    /// enabled without an explicit path (the `--log` flag), the default is
    /// `$ROTEIRO_HOME/logs/roteiro.log`.
    pub file: Option<String>,
    /// Rotation cadence for the file appender: `daily` (default), `hourly`,
    /// `minutely`, or `never` (a single, unrotated file). Time-based only —
    /// size-based rotation is out of scope for `tracing-appender` and deferred to
    /// the OTLP step (ADR-0011).
    pub rotation: Option<String>,
    /// On-disk record format: `otel` (default) / `json` — one
    /// OpenTelemetry-shaped JSON object per line (see [`crate::telemetry`] for the
    /// field mapping) — or `text`, the same human-readable format stdout uses.
    pub format: Option<String>,
}

impl TelemetryConfig {
    /// Overlay `over` on top of `self` per field (the project layer over the user
    /// layer), taking `over`'s value wherever set.
    fn overlaid_with(&self, over: &Self) -> Self {
        Self {
            file: over.file.clone().or_else(|| self.file.clone()),
            rotation: over.rotation.clone().or_else(|| self.rotation.clone()),
            format: over.format.clone().or_else(|| self.format.clone()),
        }
    }
}

/// `[workspace]` — the repos one `roteiro serve` can host (ADR-0008). Naturally a
/// user-layer setting (machine-specific), but merged like any table. Combined
/// with `serve --workspace <root>`; empty ⇒ single-repo serve (the cwd's repo).
#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct WorkspaceConfig {
    /// Directories to scan for git repos (each immediate subdirectory that is a
    /// repo becomes a project, plus the root itself if it is one).
    pub roots: Option<Vec<String>>,
    /// Explicit repo paths to host, in addition to anything found under `roots`.
    pub repos: Option<Vec<String>>,
}

impl WorkspaceConfig {
    /// Whether this table names nothing to host (no roots and no repos) — used to
    /// decide whether the legacy `[workspace]` should fold in as `default`.
    fn is_empty(&self) -> bool {
        self.roots.as_ref().is_none_or(Vec::is_empty)
            && self.repos.as_ref().is_none_or(Vec::is_empty)
    }
}

/// One `[[workspaces]]` entry: a **named** linked workspace — a set of repos served
/// as a single multi-repo graph (their cross-repo links resolve), the named form of
/// the legacy single `[workspace]` (ADR-0008). Reuses the `roots`/`repos` discovery
/// rules of [`WorkspaceConfig`], plus a `name` (the `--workspace-name` selector).
#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct NamedWorkspace {
    /// The workspace name — the selector passed to `--workspace-name`.
    pub name: String,
    /// Directories to scan for member git repos (each immediate subdirectory that
    /// is a repo, plus the root itself if it is one), as `[workspace] roots`.
    pub roots: Option<Vec<String>>,
    /// Explicit member repo paths, in addition to anything found under `roots`.
    pub repos: Option<Vec<String>>,
}

/// One authored cross-repo link (ADR-0009): a `[[links]]` entry in a spoke repo's
/// `roteiro.toml` declaring that this repo references a project-qualified key in
/// another workspace repo. `roteiro links` resolves each against the workspace and
/// flags drift (targets that no longer exist).
#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
pub struct LinkDecl {
    /// The project-qualified target: `<project>::<key>`, e.g.
    /// `app::sym:rust:crates/roteiro/src/config.rs#ServeConfig`.
    pub to: String,
    /// Optional local anchor in *this* repo the link originates from, e.g.
    /// `file:values.prod.yaml`. Recorded for provenance and shown in the report;
    /// not currently resolved against this repo's graph.
    #[serde(default)]
    pub from: Option<String>,
    /// Relationship label for display (default `references`).
    #[serde(default)]
    pub kind: Option<String>,
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
/// built with (the `pdf-text`/`image-ocr`/`image-vision`/`audio-transcribe`
/// features).
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
    /// Transcribe spoken-word audio.
    pub audio: Option<bool>,
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
            audio: self.audio.unwrap_or(default.audio),
        }
    }
}

/// `[media]` — the **pre-generation gate** `roteiro media build` applies before
/// loading a model (ADR-0015).
///
/// The gate is a cheap, deterministic refusal of blobs with nothing to read:
/// digital silence, flat-colour images. It is on by default at thresholds that
/// sit at the digital noise floor, because **a false skip is worse than a false
/// pass** now that generated output is clearly labelled — so raising a threshold
/// is a deliberate act, taken here, and never something the tool does for you.
///
/// Every setting is `Option`, and unset means the built-in default
/// ([`rto_graph::GateThresholds::default`]).
#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct MediaConfig {
    /// Apply the gate at all. `false` sends every blob to the model, which is
    /// what `--force` does for a single run.
    pub gate: Option<bool>,
    /// RMS amplitude (full scale `1.0`) at or below which an audio clip counts
    /// as silent. Default `0.0001` (≈ -80 dBFS).
    pub silence_rms: Option<f64>,
    /// Luma variance (a pixel is `0.0`…`1.0`) at or below which an image counts
    /// as uniform. Default `0.00001`.
    pub image_variance: Option<f64>,
}

impl MediaConfig {
    /// Resolve to the graph-layer thresholds, defaulting each unset value.
    ///
    /// `gate = false` resolves to [`rto_graph::GateThresholds::disabled`] rather
    /// than to a separate flag: the two settings would otherwise have to be read
    /// together everywhere, and a disabled gate is exactly a gate nothing can
    /// fall below.
    #[must_use]
    pub fn resolve(&self) -> rto_graph::GateThresholds {
        if self.gate == Some(false) {
            return rto_graph::GateThresholds::disabled();
        }
        let default = rto_graph::GateThresholds::default();
        rto_graph::GateThresholds {
            silence_rms: self.silence_rms.unwrap_or(default.silence_rms),
            image_variance: self.image_variance.unwrap_or(default.image_variance),
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
    /// PEM certificate-chain file for in-app TLS. Set **both** this and `tls_key`
    /// and `serve --models` terminates HTTPS itself (needs `--features serve`);
    /// set **neither** and it serves plain HTTP (front with a proxy for TLS).
    /// Setting exactly one is a startup error.
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
                audio: over.ingest.audio.or(self.ingest.audio),
            },
            media: MediaConfig {
                gate: over.media.gate.or(self.media.gate),
                silence_rms: over.media.silence_rms.or(self.media.silence_rms),
                image_variance: over.media.image_variance.or(self.media.image_variance),
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
            telemetry: self.telemetry.overlaid_with(&over.telemetry),
            workspace: WorkspaceConfig {
                roots: over
                    .workspace
                    .roots
                    .clone()
                    .or(self.workspace.roots.clone()),
                repos: over
                    .workspace
                    .repos
                    .clone()
                    .or(self.workspace.repos.clone()),
            },
            // `[[workspaces]]` overlay like `links`: the project layer wins outright
            // when it declares any, else the user layer's survive.
            workspaces: if over.workspaces.is_empty() {
                self.workspaces.clone()
            } else {
                over.workspaces.clone()
            },
            // `[standalone]` merges per field (project over user), like `workspace`.
            standalone: WorkspaceConfig {
                roots: over
                    .standalone
                    .roots
                    .clone()
                    .or(self.standalone.roots.clone()),
                repos: over
                    .standalone
                    .repos
                    .clone()
                    .or(self.standalone.repos.clone()),
            },
            // Links are per-repo (a spoke declares its own); the project layer wins
            // outright when it has any, else the user layer's (rare).
            links: if over.links.is_empty() {
                self.links.clone()
            } else {
                over.links.clone()
            },
            // Pins merge per key: the project layer overrides the user layer.
            pins: {
                let mut m = self.pins.clone();
                m.extend(over.pins.clone());
                m
            },
        }
    }

    /// Normalise the workspace configuration into a flat list of resolved groups
    /// (ADR-0008 multi-workspace), the input to [`rto_graph::WorkspaceSet`]:
    ///
    /// - the legacy `[workspace]` table, **when it names any roots/repos**, folds in
    ///   as a linked group named `default`;
    /// - each `[[workspaces]]` entry is a linked group under its own name;
    /// - `[standalone]` expands — by discovering the repos under its roots plus its
    ///   explicit repos — into one **unlinked**, single-repo group per repo, named
    ///   after the repo directory (deduped `-2`/`-3` on collision, as the workspace
    ///   registry does).
    ///
    /// **Linked** workspace names (`default` and every `[[workspaces]]`) must be
    /// **unique** — a collision is a config error, never a silent rename, because
    /// renaming a user-named linked workspace would resolve its cross-repo links
    /// against the wrong repos. Only the auto-generated **standalone** names (derived
    /// from repo directory names) take a `-2`/`-3` suffix on collision, including
    /// against a linked name.
    ///
    /// Fully backward-compatible: a config with only `[workspace]` yields exactly one
    /// `default` linked group with the same membership as before; a config naming no
    /// workspaces at all yields an empty list.
    ///
    /// # Errors
    /// A duplicate **linked** workspace name, or a discovery failure (an unreadable
    /// `[standalone]` root).
    pub fn resolved_workspaces(&self) -> anyhow::Result<Vec<rto_graph::ResolvedWorkspace>> {
        use std::collections::HashSet;

        let mut out: Vec<rto_graph::ResolvedWorkspace> = Vec::new();
        let mut used: HashSet<String> = HashSet::new();

        // Legacy `[workspace]` → the `default` linked group (only if it names any
        // repos), so today's single-workspace configs keep working unchanged.
        if !self.workspace.is_empty() {
            used.insert("default".to_owned());
            out.push(rto_graph::ResolvedWorkspace {
                name: "default".to_owned(),
                roots: expand_tilde_all(self.workspace.roots.clone().unwrap_or_default()),
                repos: expand_tilde_all(self.workspace.repos.clone().unwrap_or_default()),
                linked: true,
            });
        }

        // Each `[[workspaces]]` → a named linked group. A collision with `default`
        // or another `[[workspaces]]` is a config error (never a silent rename).
        for nw in &self.workspaces {
            if !used.insert(nw.name.clone()) {
                anyhow::bail!(
                    "duplicate workspace name `{}` — `[[workspaces]]` names (and the legacy \
                     `[workspace]`, which folds in as `default`) must each be unique",
                    nw.name
                );
            }
            out.push(rto_graph::ResolvedWorkspace {
                name: nw.name.clone(),
                roots: expand_tilde_all(nw.roots.clone().unwrap_or_default()),
                repos: expand_tilde_all(nw.repos.clone().unwrap_or_default()),
                linked: true,
            });
        }

        // `[standalone]` → one unlinked, single-repo group per discovered repo.
        for repo in self.standalone_repo_paths()? {
            let base = repo
                .file_name()
                .map_or_else(|| "repo".to_owned(), |s| s.to_string_lossy().into_owned());
            let name = dedupe_workspace_name(&mut used, base);
            out.push(rto_graph::ResolvedWorkspace {
                name,
                roots: Vec::new(),
                repos: vec![repo.to_string_lossy().into_owned()],
                linked: false,
            });
        }

        Ok(out)
    }

    /// Every standalone repo: those discovered under each `[standalone] roots` entry
    /// plus each explicit `[standalone] repos` path, order-stable and de-duplicated
    /// by path.
    fn standalone_repo_paths(&self) -> anyhow::Result<Vec<PathBuf>> {
        use std::collections::HashSet;
        let mut seen: HashSet<PathBuf> = HashSet::new();
        let mut out: Vec<PathBuf> = Vec::new();
        for root in self.standalone.roots.iter().flatten() {
            for repo in rto_graph::discover_repos_under(&expand_tilde(root))? {
                if seen.insert(repo.clone()) {
                    out.push(repo);
                }
            }
        }
        for repo in self.standalone.repos.iter().flatten() {
            let p = expand_tilde(repo).into_owned();
            if seen.insert(p.clone()) {
                out.push(p);
            }
        }
        Ok(out)
    }
}

/// Expand a leading `~/` (or a bare `~`) to the user's home directory in every
/// path string, returning owned strings. The workspace-resolution boundary
/// ([`Config::resolved_workspaces`]): `rto_graph` receives real paths, never a
/// literal `~` it would hand straight to git. A path without a leading `~` is
/// unchanged. Env-var expansion (`$HOME`) is intentionally out of scope.
fn expand_tilde_all(paths: Vec<String>) -> Vec<String> {
    paths
        .into_iter()
        .map(|p| expand_tilde(&p).to_string_lossy().into_owned())
        .collect()
}

/// Expand a leading `~/` (or a bare `~`) to the user's home directory; any other
/// path is borrowed back unchanged. The one home-relative expansion shared by
/// every config path — the model store (`[paths] model_store`) and workspace
/// `roots`/`repos` (new `[[workspaces]]`/`[standalone]` and legacy `[workspace]`).
/// Env-var expansion (`$HOME`) is intentionally out of scope — only `~`.
///
/// Fast path: a string that isn't exactly `~` and doesn't start with `~/` is
/// borrowed as-is — no `HOME`/`USERPROFILE` lookup and no allocation — so
/// resolving a large `roots`/`repos` list (serve startup, SIGHUP reload) costs
/// nothing beyond the borrow, as it did before tilde handling was added.
pub(crate) fn expand_tilde(path: &str) -> Cow<'_, Path> {
    if path != "~" && !path.starts_with("~/") {
        return Cow::Borrowed(Path::new(path));
    }
    let home = home_dir();
    Cow::Owned(expand_tilde_with(
        path,
        home.as_deref().map(std::path::Path::as_os_str),
    ))
}

/// The user's home directory: `$HOME`, else `$USERPROFILE` (Windows). The one home
/// lookup shared across the codebase — [`expand_tilde`]'s `~` expansion and
/// [`roteiro_home`]'s `~/.roteiro` fallback both resolve home through it. Returns
/// `None` when neither is set. Env-var expansion beyond `~` is out of scope.
pub(crate) fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Core of [`expand_tilde`] with the home directory injected, so tests drive it
/// deterministically without mutating process-global env.
fn expand_tilde_with(path: &str, home: Option<&std::ffi::OsStr>) -> PathBuf {
    if path == "~"
        && let Some(h) = home
    {
        return PathBuf::from(h);
    }
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(h) = home
    {
        return Path::new(h).join(rest);
    }
    PathBuf::from(path)
}

/// Make `base` unique against the names already handed out, appending `-2`, `-3`, …
/// on collision (mirrors the workspace registry's `dedupe_name`). Records the
/// chosen name in `used`.
fn dedupe_workspace_name(used: &mut std::collections::HashSet<String>, base: String) -> String {
    if used.insert(base.clone()) {
        return base;
    }
    let mut n = 2u32;
    loop {
        let candidate = format!("{base}-{n}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
        n += 1;
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

/// Roteiro's home directory: `$ROTEIRO_HOME` when set, else `~/.roteiro`. The one
/// place the model store, the user config, and the default log path
/// ([`default_log_path`]) resolve their base, so they always agree. Returns
/// `None` only when neither `ROTEIRO_HOME` nor a home directory is discoverable.
pub(crate) fn roteiro_home() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("ROTEIRO_HOME") {
        return Some(PathBuf::from(home));
    }
    Some(home_dir()?.join(".roteiro"))
}

/// The default rotating-log path used when file logging is enabled without an
/// explicit path (`roteiro --log`): `$ROTEIRO_HOME/logs/roteiro.log`, else
/// `~/.roteiro/logs/roteiro.log`.
pub(crate) fn default_log_path() -> Option<PathBuf> {
    Some(roteiro_home()?.join("logs").join("roteiro.log"))
}

/// The user config path: `$ROTEIRO_HOME/config.toml`, else `~/.roteiro/config.toml`
/// (mirrors the model store's home resolution).
fn user_config_path() -> Option<PathBuf> {
    Some(roteiro_home()?.join("config.toml"))
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
    use std::path::{Path, PathBuf};

    use super::{
        Config, NamedWorkspace, WorkspaceConfig, expand_tilde_with, find_project_config, load_from,
    };

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
            load_from(None, Some(project.clone())).is_err(),
            "malformed TOML must error"
        );

        // `[pins]` parses, and merges per key (project over user).
        std::fs::write(&user, "[pins]\napp = \"v{tag}\"\nother = \"user-{tag}\"\n").expect("write");
        std::fs::write(&project, "[pins]\napp = \"release-{tag}\"\n").expect("write");
        let loaded = load_from(Some(user), Some(project)).expect("load");
        assert_eq!(
            loaded.effective.pins.get("app").map(String::as_str),
            Some("release-{tag}")
        );
        assert_eq!(
            loaded.effective.pins.get("other").map(String::as_str),
            Some("user-{tag}")
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The `[telemetry]` block parses, and overlays per field (project over user)
    /// like the other tables: an unset field falls back to the user layer.
    #[test]
    fn telemetry_block_parses_and_overlays_per_field() {
        // Absent table ⇒ all fields None (file logging off by default).
        let none: Config = toml::from_str("[infer]\ntop_k = 1\n").expect("parse");
        assert_eq!(none.telemetry, super::TelemetryConfig::default());

        let user: Config = toml::from_str(
            "[telemetry]\nfile = \"~/.roteiro/logs/roteiro.log\"\nrotation = \"daily\"\nformat = \"otel\"\n",
        )
        .expect("parse user");
        assert_eq!(
            user.telemetry.file.as_deref(),
            Some("~/.roteiro/logs/roteiro.log")
        );
        assert_eq!(user.telemetry.rotation.as_deref(), Some("daily"));

        // Project overrides only `rotation`; user's `file`/`format` survive.
        let project: Config =
            toml::from_str("[telemetry]\nrotation = \"hourly\"\n").expect("parse project");
        let merged = user.overlaid_with(&project);
        assert_eq!(merged.telemetry.rotation.as_deref(), Some("hourly"));
        assert_eq!(
            merged.telemetry.file.as_deref(),
            Some("~/.roteiro/logs/roteiro.log")
        );
        assert_eq!(merged.telemetry.format.as_deref(), Some("otel"));
    }

    /// Backward compat: a legacy `[workspace]`-only config parses, merges, and
    /// resolves to exactly one `default` linked workspace — as before the
    /// multi-workspace fields existed.
    #[test]
    fn legacy_workspace_only_resolves_to_one_default_linked_group() {
        let cfg = Config {
            workspace: WorkspaceConfig {
                roots: Some(vec!["/code".to_owned()]),
                repos: Some(vec!["/code/extra".to_owned()]),
            },
            ..Default::default()
        };
        let resolved = cfg.resolved_workspaces().expect("resolve");
        assert_eq!(resolved.len(), 1);
        let d = &resolved[0];
        assert_eq!(d.name, "default");
        assert!(d.linked);
        assert_eq!(d.roots, vec!["/code".to_owned()]);
        assert_eq!(d.repos, vec!["/code/extra".to_owned()]);

        // A config naming no workspaces at all yields no groups.
        assert!(
            Config::default()
                .resolved_workspaces()
                .expect("resolve default")
                .is_empty()
        );

        // A user-layer `[workspace]` still survives an empty project overlay, and
        // the new fields default to empty (they don't perturb a legacy config).
        let user: Config = toml::from_str("[workspace]\nroots = [\"/code\"]\n").expect("parse");
        let merged = user.overlaid_with(&Config::default());
        assert_eq!(
            merged.workspace.roots.as_deref(),
            Some(&["/code".to_owned()][..])
        );
        assert!(merged.workspaces.is_empty());
        assert!(merged.standalone.is_empty());
    }

    /// A config with `[[workspaces]]` + `[standalone]` (and a legacy `[workspace]`)
    /// partitions into linked named groups first, then one unlinked single-repo
    /// group per discovered standalone repo, with the right names and flags.
    #[test]
    fn named_workspaces_and_standalone_partition_correctly() {
        let base = std::env::temp_dir().join(format!("roteiro-rw-{}", std::process::id()));
        std::fs::remove_dir_all(&base).ok();
        // Two synthetic standalone repos (a `.git` entry marks a repo) under a root.
        for name in ["docs", "tools"] {
            std::fs::create_dir_all(base.join("solo").join(name).join(".git")).expect("mkrepo");
        }

        let cfg = Config {
            workspace: WorkspaceConfig {
                roots: Some(vec!["/legacy".to_owned()]),
                repos: None,
            },
            workspaces: vec![
                NamedWorkspace {
                    name: "api".to_owned(),
                    roots: Some(vec!["/api".to_owned()]),
                    repos: None,
                },
                NamedWorkspace {
                    name: "web".to_owned(),
                    roots: None,
                    repos: Some(vec!["/web/app".to_owned()]),
                },
            ],
            standalone: WorkspaceConfig {
                roots: Some(vec![base.join("solo").to_string_lossy().into_owned()]),
                repos: None,
            },
            ..Default::default()
        };
        let resolved = cfg.resolved_workspaces().expect("resolve");
        let names: Vec<&str> = resolved.iter().map(|r| r.name.as_str()).collect();

        // Legacy `[workspace]` folds in as `default` first, then the named linked
        // groups, then the standalone singletons (discovered in sorted order).
        assert_eq!(names, vec!["default", "api", "web", "docs", "tools"]);

        let by_name = |n: &str| resolved.iter().find(|r| r.name == n).expect("group");
        assert!(by_name("default").linked);
        assert!(by_name("api").linked);
        assert!(by_name("web").linked);

        let docs = by_name("docs");
        assert!(!docs.linked, "standalone repos are unlinked");
        assert!(docs.roots.is_empty());
        assert_eq!(docs.repos.len(), 1, "a standalone group is a singleton");
        assert!(docs.repos[0].ends_with("docs"));
        assert!(!by_name("tools").linked);

        std::fs::remove_dir_all(&base).ok();
    }

    /// A standalone repo whose directory name collides with a linked workspace name
    /// takes a `-2` suffix (dedupe like the workspace registry); the linked group
    /// keeps its slot.
    #[test]
    fn standalone_names_dedupe_against_linked_names() {
        let base = std::env::temp_dir().join(format!("roteiro-rw-dedupe-{}", std::process::id()));
        std::fs::remove_dir_all(&base).ok();
        let repo = base.join("default");
        std::fs::create_dir_all(repo.join(".git")).expect("mkrepo");

        let cfg = Config {
            workspace: WorkspaceConfig {
                roots: Some(vec!["/legacy".to_owned()]),
                repos: None,
            },
            standalone: WorkspaceConfig {
                roots: None,
                repos: Some(vec![repo.to_string_lossy().into_owned()]),
            },
            ..Default::default()
        };
        let resolved = cfg.resolved_workspaces().expect("resolve");
        let names: Vec<&str> = resolved.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["default", "default-2"]);
        // The linked `default` keeps its slot; the standalone takes `default-2`.
        assert!(
            resolved
                .iter()
                .find(|r| r.name == "default")
                .unwrap()
                .linked
        );
        assert!(
            !resolved
                .iter()
                .find(|r| r.name == "default-2")
                .unwrap()
                .linked
        );

        std::fs::remove_dir_all(&base).ok();
    }

    /// The new fields parse from TOML and overlay like `links`: a project-layer
    /// `[[workspaces]]` wins outright, while an unset `[standalone]` falls back to
    /// the user layer.
    #[test]
    fn multi_workspace_fields_parse_and_overlay() {
        let user: Config = toml::from_str(
            "[[workspaces]]\nname = \"api\"\nroots = [\"/api\"]\n\
             [standalone]\nrepos = [\"/solo/x\"]\n",
        )
        .expect("parse user");
        assert_eq!(user.workspaces.len(), 1);
        assert_eq!(user.workspaces[0].name, "api");
        assert_eq!(
            user.standalone.repos.as_deref(),
            Some(&["/solo/x".to_owned()][..])
        );

        let project: Config =
            toml::from_str("[[workspaces]]\nname = \"web\"\n").expect("parse project");
        let merged = user.overlaid_with(&project);
        // `[[workspaces]]` from the project layer wins outright.
        assert_eq!(merged.workspaces.len(), 1);
        assert_eq!(merged.workspaces[0].name, "web");
        // `[standalone]` unset in the project layer ⇒ the user layer's survives.
        assert_eq!(
            merged.standalone.repos.as_deref(),
            Some(&["/solo/x".to_owned()][..])
        );
    }

    /// A **linked** workspace name collision is a config error (never a silent
    /// rename that would resolve links against the wrong repos) — both between two
    /// `[[workspaces]]` and between a `[[workspaces]]` and the folded-in `default`.
    #[test]
    fn duplicate_linked_workspace_names_are_a_config_error() {
        // Two `[[workspaces]]` sharing a name → error.
        let cfg = Config {
            workspaces: vec![
                NamedWorkspace {
                    name: "api".to_owned(),
                    roots: Some(vec!["/a".to_owned()]),
                    repos: None,
                },
                NamedWorkspace {
                    name: "api".to_owned(),
                    roots: Some(vec!["/b".to_owned()]),
                    repos: None,
                },
            ],
            ..Default::default()
        };
        let err = cfg
            .resolved_workspaces()
            .expect_err("duplicate [[workspaces]] name must error")
            .to_string();
        assert!(
            err.contains("duplicate workspace name") && err.contains("api"),
            "{err}"
        );

        // A `[[workspaces]]` named `default` collides with the legacy `[workspace]`
        // that folds in as `default` → error.
        let cfg = Config {
            workspace: WorkspaceConfig {
                roots: Some(vec!["/legacy".to_owned()]),
                repos: None,
            },
            workspaces: vec![NamedWorkspace {
                name: "default".to_owned(),
                roots: Some(vec!["/x".to_owned()]),
                repos: None,
            }],
            ..Default::default()
        };
        let err = cfg
            .resolved_workspaces()
            .expect_err("[[workspaces]] named `default` must collide with the legacy fold-in")
            .to_string();
        assert!(err.contains("default"), "{err}");
    }

    /// A `[standalone]` root holding several repos expands to one **single-repo**,
    /// unlinked group per repo — never one multi-repo unlinked group.
    #[test]
    fn standalone_root_yields_one_single_repo_group_per_repo() {
        let base = std::env::temp_dir().join(format!("roteiro-rw-solo-{}", std::process::id()));
        std::fs::remove_dir_all(&base).ok();
        for name in ["svc-a", "svc-b"] {
            std::fs::create_dir_all(base.join("pool").join(name).join(".git")).expect("mkrepo");
        }

        let cfg = Config {
            standalone: WorkspaceConfig {
                roots: Some(vec![base.join("pool").to_string_lossy().into_owned()]),
                repos: None,
            },
            ..Default::default()
        };
        let resolved = cfg.resolved_workspaces().expect("resolve");
        assert_eq!(resolved.len(), 2, "one group per repo: {resolved:?}");
        for rw in &resolved {
            assert!(!rw.linked, "standalone repos are unlinked: {rw:?}");
            assert_eq!(rw.repos.len(), 1, "each is a singleton: {rw:?}");
            assert!(rw.roots.is_empty(), "expanded, not a roots scan: {rw:?}");
        }
        let names: Vec<&str> = resolved.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["svc-a", "svc-b"],
            "named after repo dirs, sorted"
        );

        std::fs::remove_dir_all(&base).ok();
    }

    /// The shared `~` expansion (the seam every workspace path flows through):
    /// bare `~` and `~/rest` map onto the injected home; anything else is verbatim.
    #[test]
    fn expand_tilde_handles_bare_and_prefixed_and_passes_others_through() {
        let home = std::ffi::OsString::from("/home/alice");
        let h = Some(home.as_os_str());
        assert_eq!(expand_tilde_with("~", h), PathBuf::from("/home/alice"));
        assert_eq!(
            expand_tilde_with("~/foo/bar", h),
            PathBuf::from("/home/alice/foo/bar")
        );
        // No leading `~` → unchanged (absolute and relative alike).
        assert_eq!(
            expand_tilde_with("/abs/path", h),
            PathBuf::from("/abs/path")
        );
        assert_eq!(expand_tilde_with("rel/path", h), PathBuf::from("rel/path"));
        // `~user` (neither `~` nor `~/`) is not home-expansion — left verbatim.
        assert_eq!(expand_tilde_with("~bob/x", h), PathBuf::from("~bob/x"));
        // No home available → the `~` forms are left as-is rather than panicking.
        assert_eq!(expand_tilde_with("~/foo", None), PathBuf::from("~/foo"));
        assert_eq!(expand_tilde_with("~", None), PathBuf::from("~"));
    }

    /// A leading `~` in every workspace path input — legacy `[workspace]`,
    /// `[[workspaces]]`, and `[standalone]`, in both `roots` and `repos` — expands
    /// to the user's home at the resolution boundary, so `rto_graph` (and git)
    /// never see a literal `~`. A path without a leading `~` is passed through.
    #[test]
    fn resolved_workspaces_expand_leading_tilde_in_all_path_inputs() {
        // Home, from the same source `expand_tilde` reads, so the expectation is
        // deterministic wherever the test runs. With no home set, `expand_tilde`
        // documents that it leaves `~` unchanged — there is nothing to expand
        // against, so skip rather than assert against a home that doesn't exist
        // (keeps a sanitized-env run green, matching production behaviour).
        let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
        else {
            return;
        };
        let home = Path::new(&home);
        let joined = |rest: &str| home.join(rest).to_string_lossy().into_owned();

        let cfg = Config {
            workspace: WorkspaceConfig {
                roots: Some(vec!["~/legacy/root".to_owned()]),
                repos: Some(vec!["~/legacy/repo".to_owned()]),
            },
            workspaces: vec![NamedWorkspace {
                name: "api".to_owned(),
                roots: Some(vec!["~/api/root".to_owned()]),
                repos: Some(vec!["~/api/repo".to_owned()]),
            }],
            standalone: WorkspaceConfig {
                roots: None,
                // Explicit standalone repos become their own single-repo groups
                // with no filesystem discovery, so the expanded path is directly
                // observable. `~` roots share the identical `expand_tilde` call
                // (covered by the pure-function test above).
                repos: Some(vec!["~/solo/repo".to_owned(), "/abs/solo".to_owned()]),
            },
            ..Default::default()
        };
        let resolved = cfg.resolved_workspaces().expect("resolve");
        let by_name = |n: &str| resolved.iter().find(|r| r.name == n).expect("group");

        // Legacy `[workspace]` → `default`: roots and repos expanded.
        let d = by_name("default");
        assert_eq!(d.roots, vec![joined("legacy/root")]);
        assert_eq!(d.repos, vec![joined("legacy/repo")]);

        // `[[workspaces]]`: roots and repos expanded.
        let api = by_name("api");
        assert_eq!(api.roots, vec![joined("api/root")]);
        assert_eq!(api.repos, vec![joined("api/repo")]);

        // `[standalone]` explicit repo: expanded, named after the repo dir; a
        // non-`~` absolute path is passed through unchanged.
        assert_eq!(by_name("repo").repos, vec![joined("solo/repo")]);
        assert_eq!(by_name("solo").repos, vec!["/abs/solo".to_owned()]);

        // No expanded path still carries a literal leading `~`.
        for rw in &resolved {
            for p in rw.roots.iter().chain(&rw.repos) {
                assert!(!p.starts_with('~'), "unexpanded tilde survived: {p}");
            }
        }
    }
}

//! Which model serves a task, and **why** — the one resolver (Stage 33).
//!
//! Before this module, seven surfaces each picked a model by their own rule and
//! none knew the others existed: `spec draft` searched the registry, `infer
//! --model` validated by hand, `serve` filtered the served set, and the media and
//! OCR paths held **hard-coded string constants** — so a project could pin its
//! embedding and generative models but *could not pin its ASR model at all*.
//! [`resolve`] is now the single answer, and it returns the chosen model **plus
//! the rule that chose it**, because `roteiro config` has to answer *why did it
//! use that model?* and cannot do that from a bare string.
//!
//! # It lives in `rto-graph` on purpose
//!
//! This crate structurally cannot reach the network: its `gix` dependency is
//! pinned `default-features = false` precisely to exclude the transports. A
//! resolver that decides which model runs is exactly the kind of code that
//! acquires a "just check the hub for a newer one" call later, and the cheapest
//! way to make that impossible is to put it where the call cannot be written.
//!
//! # Deterministic rules over categorical signals — not a classifier
//!
//! Every signal here is low-cardinality: the task (seven), the model's kind (five),
//! whether it is installed (two). A table over categoricals *is* the correct
//! model for that, and it has the property a learned selector cannot have: the
//! same inputs give the same model on every machine, forever, which is what
//! producer identity (ADR-0015) depends on.
//!
//! The seed was [`ModelTask::capable`]'s ancestor, `chat_capable_model_ids`,
//! which filters models that **cannot do the job** rather than ranking models by
//! how well they might. It exists because routing a BERT encoder through
//! `/v1/chat/completions` aborts llama.cpp's decode path with a `GGML_ASSERT` — a
//! process abort, not an error. That is the generalisation this table carries:
//! capability, not preference.
//!
//! # A wrong pin fails loudly
//!
//! [`resolve`] never falls back to the default when a pin is wrong. Given the
//! `GGML_ASSERT` precedent, a pin of the wrong modality may not merely
//! mis-answer — it may abort the process — and a silent fallback would leave the
//! configuration *appearing* honoured while something else ran. Unknown names and
//! wrong modalities are [`ModelChoiceError`]s naming the offending key.
//!
//! Whether the chosen model is **installed** is a separate question, reported on
//! the choice rather than raised as an error, because the right response differs
//! per surface: `media build` refuses, `spec draft` emits the plain scaffold, OCR
//! goes inert. Surfaces that need the model call
//! [`ModelChoice::require_installed`], which names the pinning key when there is
//! one.
//!
//! # Unset changes nothing
//!
//! With no `[models]` key set, every task resolves to the constant its call site
//! used before this module existed — see `resolve_unset_matches_the_previous_hard_coded_models`.

use crate::models::{self, ModelKind, Platform};

/// A job a model is chosen for.
///
/// One variant per surface that used to decide for itself. `Draft` and `Chat` are
/// separate tasks sharing one config key: they want the same *kind* of model but
/// answer different questions, and `roteiro config` reports them separately
/// because an operator asking "why did Ask use that model?" is not asking about
/// `spec draft`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ModelTask {
    /// `roteiro infer` — embed nodes to suggest `inferred` similarity edges.
    Embed,
    /// `roteiro spec draft` — write the scaffold's unfilled sections.
    Draft,
    /// `roteiro serve` / the explorer's Ask panel — answer a question.
    Chat,
    /// `roteiro review --llm` — read one changed file and report defects in it
    /// (Stage 35b).
    ///
    /// A third task on the `generative` key rather than a fourth bespoke
    /// selection rule in the reviewer: this module exists because seven surfaces
    /// each picked a model their own way, and a reviewer that searched the
    /// registry itself would be the next one.
    ///
    /// Separate from [`ModelTask::Draft`] even though the two want the same kind
    /// of model and the same key, for the reason `Draft` and `Chat` are separate:
    /// an operator asking *why did review use that model?* is not asking about
    /// `spec draft`, and `roteiro config` answers per task.
    Review,
    /// `roteiro media build` (audio) — transcribe a spoken-word clip.
    Transcribe,
    /// `roteiro media build` (vision) — describe an image.
    Describe,
    /// `roteiro sync` — read literal text out of an image.
    Ocr,
}

/// Every task, in the order `roteiro config` prints them.
pub const TASKS: [ModelTask; 7] = [
    ModelTask::Embed,
    ModelTask::Draft,
    ModelTask::Chat,
    ModelTask::Review,
    ModelTask::Transcribe,
    ModelTask::Describe,
    ModelTask::Ocr,
];

/// The OCR model `sync` uses when `[models] ocr` is unset.
pub const DEFAULT_OCR: &str = "ocrs-text";

impl ModelTask {
    /// Stable token for this task (`embed`, `draft`, …) — used in `roteiro
    /// config` output and in error messages.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Embed => "embed",
            Self::Draft => "draft",
            Self::Chat => "chat",
            Self::Review => "review",
            Self::Transcribe => "transcribe",
            Self::Describe => "describe",
            Self::Ocr => "ocr",
        }
    }

    /// The command surface this task serves, for a human reading `roteiro
    /// config`. A task token alone does not tell an operator where the model is
    /// used, and "where" is half of "why did it use that model?".
    #[must_use]
    pub fn surface(self) -> &'static str {
        match self {
            Self::Embed => "roteiro infer",
            Self::Draft => "roteiro spec draft",
            Self::Chat => "roteiro serve / Ask",
            Self::Review => "roteiro review --llm",
            Self::Transcribe => "roteiro media build (audio)",
            Self::Describe => "roteiro media build (vision)",
            Self::Ocr => "roteiro sync (image OCR)",
        }
    }

    /// The `[models]` key that pins this task's model.
    ///
    /// Not injective: `draft`, `chat` and `review` share `generative`, which is
    /// why [`pin_accepts`] validates a pin against **every** task the key governs
    /// rather than against one.
    #[must_use]
    pub fn config_key(self) -> &'static str {
        match self {
            Self::Embed => "embedding",
            Self::Draft | Self::Chat | Self::Review => "generative",
            Self::Transcribe => "audio",
            Self::Describe => "vision",
            Self::Ocr => "ocr",
        }
    }

    /// Whether a model of `kind` **can do this job at all** — the capability
    /// table, and the whole of the selection logic.
    ///
    /// This is the generalisation of `chat_capable_model_ids`: it filters models
    /// that cannot do the job, never ranks models that can. `Chat` admits vision
    /// models because a multimodal GGUF served over `/v1/chat/completions`
    /// generates text like any other; it excludes embedding models because a BERT
    /// encoder on that path aborts llama.cpp with a `GGML_ASSERT`.
    #[must_use]
    pub fn capable(self, kind: ModelKind) -> bool {
        match self {
            Self::Embed => kind == ModelKind::Embedding,
            // `Review` is `Draft`'s shape, not `Chat`'s: it has to read code and
            // emit a fixed line format, which is a text job. Admitting a vision
            // model would not widen `generative` anyway — `pin_accepts`
            // intersects over every task on the key, and `Draft` already
            // excludes it — so it would only mean a `review` that claimed a
            // capability the key can never hand it.
            Self::Draft | Self::Review => kind == ModelKind::Generative,
            Self::Chat => matches!(kind, ModelKind::Generative | ModelKind::Vision),
            Self::Transcribe => kind == ModelKind::Audio,
            Self::Describe => kind == ModelKind::Vision,
            Self::Ocr => kind == ModelKind::Ocr,
        }
    }

    /// Registry name of the model this task uses when nothing pins one, or `None`
    /// when the built-in fallback is not a registry model.
    ///
    /// Only [`ModelTask::Embed`] returns `None`: `infer` without a model embeds
    /// with the compiled-in hashing embedder (ADR-0003's "tiny static default"),
    /// which has no registry entry and needs no download.
    ///
    /// The audio and vision defaults are read from [`crate::MediaKind::model`]
    /// rather than repeated here, so the modality and the resolver cannot
    /// disagree about which model a media record was produced by.
    #[must_use]
    pub fn default_model(self) -> Option<&'static str> {
        match self {
            Self::Ocr => Some(DEFAULT_OCR),
            _ => None,
        }
    }
}

impl std::fmt::Display for ModelTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether `[models] <key>` may name a model of `kind`.
///
/// The intersection of [`ModelTask::capable`] over every task the key governs —
/// derived from the one table rather than written down a second time. It matters
/// for `generative`, which governs `draft`, `chat` and `review`: `chat` alone
/// would admit a vision model, but the same key also has to satisfy `spec draft`,
/// which cannot use one. So the key accepts generative models only — and a task
/// added to the key later tightens this automatically rather than needing to
/// remember to.
#[must_use]
pub fn pin_accepts(key: &str, kind: ModelKind) -> bool {
    let mut governs = TASKS.iter().filter(|t| t.config_key() == key).peekable();
    governs.peek().is_some() && governs.all(|t| t.capable(kind))
}

/// The model kinds `[models] <key>` accepts, for an error message that says what
/// *would* have been valid rather than only what was not.
fn pin_kinds(key: &str) -> Vec<ModelKind> {
    ModelKind::ALL
        .iter()
        .copied()
        .filter(|k| pin_accepts(key, *k))
        .collect()
}

/// Render a list of kinds as `an embedding` / `a generative or vision`.
fn kinds_phrase(kinds: &[ModelKind]) -> String {
    let names: Vec<&str> = kinds.iter().map(|k| k.as_str()).collect();
    let joined = match names.as_slice() {
        [] => return "no".to_owned(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
    };
    let article = if joined.starts_with(['a', 'e', 'i', 'o', 'u']) {
        "an"
    } else {
        "a"
    };
    format!("{article} {joined}")
}

/// The `[models]` table as the resolver reads it: one optional registry name per
/// key, with no interpretation applied yet.
///
/// The graph-layer mirror of the binary's `[models]` config section, in the same
/// shape as [`crate::IngestConfig`] mirrors `[ingest]`: the binary owns layering
/// and precedence, this crate owns what the resulting names *mean*.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ModelPins {
    /// `[models] embedding` — the model `roteiro infer` embeds with.
    pub embedding: Option<String>,
    /// `[models] generative` — the model `spec draft` and Ask generate with.
    pub generative: Option<String>,
    /// `[models] vision` — the model `media build` describes images with.
    pub vision: Option<String>,
    /// `[models] audio` — the model `media build` transcribes speech with.
    pub audio: Option<String>,
    /// `[models] ocr` — the model `sync` reads image text with.
    pub ocr: Option<String>,
}

impl ModelPins {
    /// The pin for `key`, treating an all-whitespace value as unset: a stray
    /// `generative = ""` is a mistake, and reporting it as an unknown model whose
    /// name is the empty string would be a worse answer than the default.
    #[must_use]
    pub fn by_key(&self, key: &str) -> Option<&str> {
        let raw = match key {
            "embedding" => &self.embedding,
            "generative" => &self.generative,
            "vision" => &self.vision,
            "audio" => &self.audio,
            "ocr" => &self.ocr,
            _ => &None,
        };
        raw.as_deref().map(str::trim).filter(|v| !v.is_empty())
    }

    /// The pin governing `task`, if any.
    #[must_use]
    pub fn for_task(&self, task: ModelTask) -> Option<&str> {
        self.by_key(task.config_key())
    }
}

/// Which rule chose a model — the half of a resolution that a bare model name
/// throws away, and the half `roteiro config` needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelSource {
    /// A `[models]` key named it. The key is [`ModelTask::config_key`].
    Pinned,
    /// Nothing named it, so the task's built-in default applies.
    Default,
    /// A `[models]` key named a **provider** model id (ADR-0033), passed through
    /// unchecked: the provider owns its list, and refuses an unknown id itself.
    Provider,
}

impl ModelSource {
    /// Stable token (`pinned` | `default`) for `--json` output.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pinned => "pinned",
            Self::Default => "default",
            Self::Provider => "provider",
        }
    }
}

/// A resolved task: the model, the rule that chose it, and whether it is on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelChoice {
    /// The task this answers.
    pub task: ModelTask,
    /// Registry name of the chosen model, or `None` for [`ModelTask::Embed`]'s
    /// compiled-in hashing embedder (see [`ModelTask::default_model`]).
    pub model: Option<&'static str>,
    /// Whether a `[models]` key chose it or the default did.
    pub source: ModelSource,
    /// Whether every file of the host variant is present in the model store.
    ///
    /// `Some(true)` when there is nothing to install — [`ModelChoice::model`] is
    /// `None` for the compiled-in hashing embedder, which is in the binary. `None`
    /// means the question does not apply: a provider model, which is not on disk
    /// (ADR-0033).
    pub installed: Option<bool>,
    /// The provider model id a `[models]` key named, for every task but OCR
    /// ([`ModelSource::Provider`]).
    pub provider_id: Option<String>,
}

impl ModelChoice {
    /// The chosen model's name, or the label of the built-in fallback.
    #[must_use]
    pub fn label(&self) -> String {
        match (&self.provider_id, self.model) {
            (Some(id), _) => id.clone(),
            (None, Some(name)) => name.to_owned(),
            (None, None) if self.task == ModelTask::Embed => {
                "hashing embedder (offline default)".to_owned()
            }
            (None, None) => "none (no provider model configured)".to_owned(),
        }
    }

    /// One sentence saying **why** this model — the string `roteiro config`
    /// prints beside the name.
    #[must_use]
    pub fn why(&self) -> String {
        let key = self.task.config_key();
        match self.source {
            ModelSource::Pinned => format!("pinned by `[models] {key}`"),
            ModelSource::Provider => format!("provider model, named by `[models] {key}`"),
            ModelSource::Default if self.task == ModelTask::Embed => {
                format!("built-in default, no model needed (`[models] {key}` unset)")
            }
            ModelSource::Default if self.model.is_none() => {
                format!("served by the provider: set `[models] {key}` (ADR-0033)")
            }
            ModelSource::Default => format!("built-in default (`[models] {key}` unset)"),
        }
    }

    /// The chosen model's name, or the error a surface should refuse with when it
    /// cannot proceed without the weights.
    ///
    /// Names the pinning key when there is one, because "model `x` is not
    /// installed" leaves an operator to work out for themselves why `x` was
    /// wanted at all.
    ///
    /// # Errors
    /// [`ModelChoiceError::NotInstalled`] or [`ModelChoiceError::PinNotInstalled`]
    /// when the model's files are not in the store,
    /// and [`ModelChoiceError::NoModel`] for a task whose default is not a model.
    pub fn require_installed(&self) -> Result<&'static str, ModelChoiceError> {
        let Some(name) = self.model else {
            return Err(ModelChoiceError::NoModel { task: self.task });
        };
        if self.installed == Some(true) {
            return Ok(name);
        }
        Err(match self.source {
            ModelSource::Pinned => ModelChoiceError::PinNotInstalled {
                key: self.task.config_key(),
                name: name.to_owned(),
            },
            // Matched rather than caught by a wildcard so that a later variant
            // has to decide what it means here instead of inheriting "not
            // installed" by accident.
            ModelSource::Default | ModelSource::Provider => ModelChoiceError::NotInstalled {
                name: name.to_owned(),
            },
        })
    }
}

/// Why a `[models]` key could not be honoured.
///
/// Every variant names the key and says what to do about it. None of them is
/// recoverable by falling back to the default: that would leave the
/// configuration appearing honoured while a different model ran, which is the
/// failure this type exists to prevent.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelChoiceError {
    /// The pinned name is in no registry entry.
    #[error(
        "`[models] {key} = {name:?}` names no model this build knows — \
         run `roteiro model list` for the names it does"
    )]
    Unknown {
        /// The `[models]` key that named it.
        key: &'static str,
        /// The name as configured.
        name: String,
    },
    /// The pinned model exists but is the wrong modality for the key.
    ///
    /// Loud rather than ignored on purpose: routing a model through a path its
    /// architecture cannot serve does not reliably produce a bad answer — on the
    /// llama.cpp chat path it aborts the process with a `GGML_ASSERT`.
    #[error(
        "`[models] {key} = {name:?}` is {got} model, but that key needs {want} model \
         ({key} governs {surfaces}) — run `roteiro model list` to see each model's kind"
    )]
    WrongKind {
        /// The `[models]` key that named it.
        key: &'static str,
        /// The name as configured.
        name: String,
        /// What the named model actually is, as `a vision`/`an embedding`.
        got: String,
        /// What the key accepts, as `a generative`.
        want: String,
        /// The surfaces the key governs, for an operator who set it for one of
        /// them and did not know about the others.
        surfaces: String,
    },
    /// A `[models]`-pinned model is not in the model store.
    #[error("`[models] {key} = {name:?}` is not installed: run `roteiro model pull {name}`")]
    PinNotInstalled {
        /// The `[models]` key that named it.
        key: &'static str,
        /// The pinned name.
        name: String,
    },
    /// The default model for a task is not in the model store. Worded exactly as
    /// the media path worded it before this module existed.
    #[error("model `{name}` is not installed: run `roteiro model pull {name}`")]
    NotInstalled {
        /// The default model's name.
        name: String,
    },
    /// A caller demanded weights for a task whose default is not a model at all.
    #[error("`{task}` has no model to install — it uses the built-in offline default")]
    NoModel {
        /// The task asked about.
        task: ModelTask,
    },
}

/// Resolve `task` against `pins` — the pure core, with no process state.
///
/// # Errors
/// [`ModelChoiceError::Unknown`] when a pin names nothing in the registry, and
/// [`ModelChoiceError::WrongKind`] when it names a model of a modality the key
/// cannot use. Never falls back to the default on either.
pub fn resolve_with(task: ModelTask, pins: &ModelPins) -> Result<ModelChoice, ModelChoiceError> {
    let key = task.config_key();
    // Every task but OCR is served by the provider (ADR-0033): its pin is a
    // provider id, which the registry cannot check and does not try to.
    if task != ModelTask::Ocr {
        return Ok(match pins.for_task(task) {
            Some(id) => ModelChoice {
                task,
                model: None,
                source: ModelSource::Provider,
                installed: None,
                provider_id: Some(id.to_owned()),
            },
            None => ModelChoice {
                task,
                model: None,
                source: ModelSource::Default,
                installed: (task == ModelTask::Embed).then_some(true),
                provider_id: None,
            },
        });
    }
    let Some(name) = pins.for_task(task) else {
        let model = task.default_model();
        return Ok(ModelChoice {
            task,
            installed: Some(model.is_none_or(is_installed_now)),
            model,
            source: ModelSource::Default,
            provider_id: None,
        });
    };
    let spec = models::find(name).ok_or_else(|| ModelChoiceError::Unknown {
        key,
        name: name.to_owned(),
    })?;
    if !pin_accepts(key, spec.kind) {
        return Err(ModelChoiceError::WrongKind {
            key,
            name: name.to_owned(),
            got: kinds_phrase(&[spec.kind]),
            want: kinds_phrase(&pin_kinds(key)),
            surfaces: TASKS
                .iter()
                .filter(|t| t.config_key() == key)
                .map(|t| t.surface())
                .collect::<Vec<_>>()
                .join(" and "),
        });
    }
    Ok(ModelChoice {
        task,
        model: Some(spec.name),
        source: ModelSource::Pinned,
        installed: Some(is_installed_now(spec.name)),
        provider_id: None,
    })
}

/// Whether every file of `name`'s host variant is in the model store.
fn is_installed_now(name: &str) -> bool {
    models::find(name)
        .and_then(|spec| spec.variant_for(Platform::host()))
        .is_some_and(|variant| models::is_installed(name, variant))
}

/// The process-wide `[models]` pins, set once at startup from the layered config.
///
/// A process-wide slot rather than a threaded parameter for the same reason
/// [`crate::set_model_store`] is one: the OCR pin has to reach `ocr_content`,
/// which runs per blob five layers below the command that read the config, and
/// threading a config through the extraction path to deliver one string would
/// cost more than it explains. [`resolve_with`] is the pure function; everything
/// testable is tested through it.
static PINS: std::sync::OnceLock<ModelPins> = std::sync::OnceLock::new();

/// Set the `[models]` pins for this process. First call wins; later calls are
/// ignored. Call once at startup, before any command resolves a model.
pub fn set_model_pins(pins: ModelPins) {
    let _ = PINS.set(pins);
}

/// The pins set for this process, or an empty set (every key unset) if none were.
#[must_use]
pub fn model_pins() -> &'static ModelPins {
    /// The all-unset pins, so an unconfigured process needs no allocation and —
    /// more to the point — cannot accidentally *initialise* the slot by reading
    /// it, which `OnceLock::get_or_init` would.
    static UNSET: ModelPins = ModelPins {
        embedding: None,
        generative: None,
        vision: None,
        audio: None,
        ocr: None,
    };
    PINS.get().unwrap_or(&UNSET)
}

/// Resolve `task` against this process's pins.
///
/// # Errors
/// As [`resolve_with`].
pub fn resolve(task: ModelTask) -> Result<ModelChoice, ModelChoiceError> {
    resolve_with(task, model_pins())
}

/// Resolve every task against `pins`, in [`TASKS`] order — what `roteiro config`
/// prints, errors included, since a key that cannot be honoured is precisely what
/// that command exists to surface.
#[must_use]
pub fn resolve_all_with(
    pins: &ModelPins,
) -> Vec<(ModelTask, Result<ModelChoice, ModelChoiceError>)> {
    TASKS
        .iter()
        .map(|&task| (task, resolve_with(task, pins)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_OCR, ModelChoiceError, ModelPins, ModelSource, ModelTask, TASKS, pin_accepts,
        resolve_all_with, resolve_with,
    };
    use crate::models::ModelKind;

    fn pins(key: &str, value: &str) -> ModelPins {
        let mut p = ModelPins::default();
        let slot = match key {
            "embedding" => &mut p.embedding,
            "generative" => &mut p.generative,
            "vision" => &mut p.vision,
            "audio" => &mut p.audio,
            "ocr" => &mut p.ocr,
            other => panic!("no such key {other}"),
        };
        *slot = Some(value.to_owned());
        p
    }

    /// Unset, only OCR names a registry model; Embed falls back to the built-in
    /// hashing embedder; every other task waits for a provider model (ADR-0033).
    #[test]
    fn unset_tasks_resolve_to_ocr_the_hashing_embedder_or_nothing() {
        let unset = ModelPins::default();
        for task in TASKS {
            let choice = resolve_with(task, &unset).expect("unset resolves");
            assert_eq!(choice.source, ModelSource::Default, "{task}");
            assert_eq!(choice.provider_id, None, "{task}");
            let expected = (task == ModelTask::Ocr).then_some(DEFAULT_OCR);
            assert_eq!(choice.model, expected, "{task}");
        }
        let embed = resolve_with(ModelTask::Embed, &unset).expect("resolves");
        assert!(embed.label().contains("hashing"), "{}", embed.label());
        let draft = resolve_with(ModelTask::Draft, &unset).expect("resolves");
        assert!(
            draft.why().contains("[models] generative"),
            "{}",
            draft.why()
        );
    }

    /// A non-OCR pin is a provider id, passed through verbatim — including ids
    /// the registry never held, and ids with `:` in them.
    #[test]
    fn a_non_ocr_pin_is_a_provider_id_passed_through() {
        for (task, key, id) in [
            (ModelTask::Draft, "generative", "qwen3:8b"),
            (ModelTask::Chat, "generative", "claude-sonnet-5-5"),
            (ModelTask::Review, "generative", "qwen3:8b"),
            (ModelTask::Embed, "embedding", "nomic-embed-text"),
            (ModelTask::Transcribe, "audio", "voxtral"),
            (ModelTask::Describe, "vision", "gemma3:4b"),
        ] {
            let choice = resolve_with(task, &pins(key, id)).expect("a provider id resolves");
            assert_eq!(choice.source, ModelSource::Provider, "{task}");
            assert_eq!(choice.provider_id.as_deref(), Some(id), "{task}");
            assert_eq!(choice.model, None, "{task}");
            assert_eq!(choice.label(), id);
            assert!(choice.why().contains(key), "{}", choice.why());
        }
    }

    /// One pin moves exactly the tasks its key governs and no others.
    #[test]
    fn a_pin_moves_only_its_own_tasks() {
        let pinned = pins("audio", "voxtral");
        for (task, result) in resolve_all_with(&pinned) {
            let choice = result.expect("valid");
            let expected = if task == ModelTask::Transcribe {
                ModelSource::Provider
            } else {
                ModelSource::Default
            };
            assert_eq!(choice.source, expected, "{task}");
        }
    }

    /// OCR still resolves against the registry: a wrong name is an error naming
    /// the key, never a fallback.
    #[test]
    fn an_ocr_pin_is_checked_against_the_registry() {
        let err =
            resolve_with(ModelTask::Ocr, &pins("ocr", "no-such-ocr")).expect_err("no such model");
        assert!(
            matches!(err, ModelChoiceError::Unknown { key: "ocr", .. }),
            "{err:?}"
        );
        let ok = resolve_with(ModelTask::Ocr, &pins("ocr", DEFAULT_OCR)).expect("ocr resolves");
        assert_eq!(ok.source, ModelSource::Pinned);
        assert!(pin_accepts("ocr", ModelKind::Ocr));
    }

    /// A blank value reads as unset.
    #[test]
    fn a_blank_pin_reads_as_unset() {
        let choice = resolve_with(ModelTask::Transcribe, &pins("audio", "   "))
            .expect("blank is not a name");
        assert_eq!(choice.source, ModelSource::Default);
        assert_eq!(choice.provider_id, None);
    }

    /// `require_installed` on a task with no model says so instead of inventing
    /// a name to fail on.
    #[test]
    fn require_installed_on_a_modelless_task_is_named() {
        let choice = resolve_with(ModelTask::Embed, &ModelPins::default()).expect("resolves");
        let err = choice.require_installed().expect_err("no model to install");
        assert!(
            matches!(
                err,
                ModelChoiceError::NoModel {
                    task: ModelTask::Embed
                }
            ),
            "{err:?}"
        );
    }

    /// `Review` stays a task on the existing `generative` key.
    #[test]
    fn review_is_a_task_on_the_existing_generative_key() {
        assert_eq!(ModelTask::Review.config_key(), "generative");
        assert_eq!(ModelTask::Review.as_str(), "review");
        assert_eq!(ModelTask::Review.surface(), "roteiro review --llm");
        assert_eq!(ModelTask::Review.default_model(), None);
    }

    /// Every task is listed in [`TASKS`] exactly once. `TASKS` is what
    /// `resolve_all_with` — and so `roteiro config` — iterates, so a task missing
    /// from it resolves correctly and is invisible to the command whose whole job
    /// is answering *why that model*.
    #[test]
    fn every_task_is_listed_exactly_once() {
        let tokens: Vec<&str> = TASKS.iter().map(|t| t.as_str()).collect();
        let mut deduped = tokens.clone();
        deduped.sort_unstable();
        deduped.dedup();
        assert_eq!(deduped.len(), tokens.len(), "TASKS repeats a task");
        assert!(tokens.contains(&"review"), "{tokens:?}");
        assert_eq!(
            resolve_all_with(&ModelPins::default()).len(),
            TASKS.len(),
            "`roteiro config` reports every task"
        );
    }

    /// Every task's key is one of the five `[models]` keys, and every key is
    /// reachable from some task — so a key can neither be unresolvable nor
    /// resolve to nothing.
    #[test]
    fn every_key_and_task_are_connected() {
        let keys = ["embedding", "generative", "vision", "audio", "ocr"];
        for task in TASKS {
            assert!(keys.contains(&task.config_key()), "{task}");
        }
        for key in keys {
            assert!(
                TASKS.iter().any(|t| t.config_key() == key),
                "{key} governs something"
            );
            assert!(
                !super::pin_kinds(key).is_empty(),
                "{key} accepts some model kind"
            );
        }
        assert!(!pin_accepts("nonsense", ModelKind::Generative));
    }
}

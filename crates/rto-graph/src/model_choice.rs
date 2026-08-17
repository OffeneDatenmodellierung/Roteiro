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
//! Every signal here is low-cardinality: the task (six), the model's kind (five),
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
//!
//! # The remote tier is a *variant*, not a transport
//!
//! [`ModelSource::Remote`] is the one resolution that does not name a registry
//! model, and it is here because ADR-0019's remote tier reaches the same two
//! surfaces this resolver already serves ([`ModelTask::Draft`] and
//! [`ModelTask::Chat`]). What arrives here is a **decision already taken
//! elsewhere**: [`RemoteTier`] is the consent gate's answer, computed by
//! `rto-remote` from the config layers and the invocation, and this module has
//! no way to compute it, ask for it, or change it. Nothing here opens a socket,
//! reads a credential or knows an endpoint's URL — the crate still has no
//! transport, and its `gix` is still pinned without one.
//!
//! Two consequences are deliberate and worth stating, because both look like
//! omissions:
//!
//! * **A remote choice carries no model name.** [`ModelChoice::model`] is a
//!   *registry* name, and a hosted model has no registry entry. The vendor model
//!   string lives on the endpoint (`[remote] model`), where the trust grade that
//!   qualifies it lives too. Putting it in this field would let a mutable
//!   pointer sit in the slot reserved for digest-pinned names.
//! * **A remote choice reports [`ModelChoice::installed`] as `None`.** Not
//!   `false`: there are no weights to install, so "is it installed?" has no
//!   answer rather than a negative one, and `Some(false)` would send a reader to
//!   `roteiro model pull` for a model no registry lists.

use crate::models::{self, ModelKind, Platform};
use crate::trust::ProducerTrust;

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
    /// `roteiro media build` (audio) — transcribe a spoken-word clip.
    Transcribe,
    /// `roteiro media build` (vision) — describe an image.
    Describe,
    /// `roteiro sync` — read literal text out of an image.
    Ocr,
}

/// Every task, in the order `roteiro config` prints them.
pub const TASKS: [ModelTask; 6] = [
    ModelTask::Embed,
    ModelTask::Draft,
    ModelTask::Chat,
    ModelTask::Transcribe,
    ModelTask::Describe,
    ModelTask::Ocr,
];

/// The generative model `spec draft` and Ask use when `[models] generative` is
/// unset: the low-tier **instruct** pick, which runs anywhere.
///
/// A literal rather than a registry search, so that adding a registry entry
/// cannot silently move a project's default;
/// `default_generative_is_still_the_low_tier_instruct_pick` proves the literal
/// and the curation have not drifted apart.
pub const DEFAULT_GENERATIVE: &str = "qwen3-0.6b";

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
            Self::Transcribe => "roteiro media build (audio)",
            Self::Describe => "roteiro media build (vision)",
            Self::Ocr => "roteiro sync (image OCR)",
        }
    }

    /// The `[models]` key that pins this task's model.
    ///
    /// Not injective: `draft` and `chat` share `generative`, which is why
    /// [`pin_accepts`] validates a pin against **every** task the key governs
    /// rather than against one.
    #[must_use]
    pub fn config_key(self) -> &'static str {
        match self {
            Self::Embed => "embedding",
            Self::Draft | Self::Chat => "generative",
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
            Self::Draft => kind == ModelKind::Generative,
            Self::Chat => matches!(kind, ModelKind::Generative | ModelKind::Vision),
            Self::Transcribe => kind == ModelKind::Audio,
            Self::Describe => kind == ModelKind::Vision,
            Self::Ocr => kind == ModelKind::Ocr,
        }
    }

    /// Whether the remote model tier (ADR-0019) can serve this task at all.
    ///
    /// Only [`ModelTask::Draft`] and [`ModelTask::Chat`] — the two generative
    /// surfaces, which share `[models] generative` and are the two ADR-0019
    /// names. The other four are refused **structurally rather than by policy**,
    /// and for two separate reasons:
    ///
    /// * [`ModelTask::Embed`] needs vectors, not text, and the remote tier
    ///   speaks one chat-completion shape.
    /// * [`ModelTask::Transcribe`], [`ModelTask::Describe`] and
    ///   [`ModelTask::Ocr`] run **inside extraction**, per blob, below the
    ///   command that read the config. There is no invocation there to grant
    ///   anything, so a remote call on those paths could only be consented to by
    ///   a config value — which is the user layer again, and ADR-0019 §3 is
    ///   explicit that the user layer alone never suffices. A gate that cannot
    ///   be operated is not a stricter gate; leaving these four local is.
    ///
    /// Their outputs are also *stored* (media records, OCR text in `meta`),
    /// where ADR-0019 §5's producer identity would have to become
    /// vendor-asserted — a far larger change than a resolution, and not one this
    /// stage makes.
    #[must_use]
    pub fn goes_remote(self) -> bool {
        matches!(self, Self::Draft | Self::Chat)
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
            Self::Embed => None,
            Self::Draft | Self::Chat => Some(DEFAULT_GENERATIVE),
            Self::Transcribe => Some(crate::media::MediaKind::Audio.model()),
            Self::Describe => Some(crate::media::MediaKind::Vision.model()),
            Self::Ocr => Some(DEFAULT_OCR),
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
/// for `generative`, which governs both `draft` and `chat`: `chat` alone would
/// admit a vision model, but the same key also has to satisfy `spec draft`, which
/// cannot use one. So the key accepts generative models only.
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
    /// The remote model tier serves this task, because the consent gate opened
    /// for this run (ADR-0019).
    ///
    /// **The variant carries the trust grade rather than a model name, and that
    /// is the point of it existing.** A hosted model has no registry entry and no
    /// digest: a vendor model string is a *mutable pointer*, so the weights
    /// behind it can change while the name does not, and Roteiro cannot detect
    /// it. `trust` is always [`ProducerTrust::VendorAsserted`] today, and it is a
    /// field rather than an implication so that a resolution says **on its face**
    /// that its identity is a claim — the same honesty ADR-0019 §5 requires of a
    /// stored record. A future endpoint that could prove which weights answered
    /// would set [`ProducerTrust::PinnedDigest`] here and change nothing else.
    Remote {
        /// How far the endpoint's model string can be trusted to identify
        /// particular weights.
        trust: ProducerTrust,
    },
}

impl ModelSource {
    /// Stable token (`pinned` | `default` | `remote`) for `--json` output.
    ///
    /// `remote` does not encode the trust grade, which is reported on its own —
    /// a consumer that reads `source == "remote"` and wants to know how much the
    /// model string means asks [`ModelSource::trust`] rather than parsing a
    /// compound token.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pinned => "pinned",
            Self::Default => "default",
            Self::Remote { .. } => "remote",
        }
    }

    /// The trust grade of a remote resolution, or `None` for a local one.
    ///
    /// A local model's identity is a digest computed on this machine, so it has
    /// no grade to report here; [`crate::Producer`] carries it where it matters.
    #[must_use]
    pub fn trust(self) -> Option<ProducerTrust> {
        match self {
            Self::Pinned | Self::Default => None,
            Self::Remote { trust } => Some(trust),
        }
    }

    /// Whether this resolution sends anything off the machine.
    #[must_use]
    pub fn is_remote(self) -> bool {
        matches!(self, Self::Remote { .. })
    }
}

/// Whether the remote model tier is available to a resolution — **the consent
/// gate's answer, arriving as an input.**
///
/// This crate cannot compute it and must not try. Deciding it means reading the
/// project file, the user file and the invocation under ADR-0019 §3's inverted
/// precedence, and that logic has exactly one implementation, in `rto-remote`.
/// What crosses into this module is the result: a boolean the user opened, plus
/// the trust grade the endpoint carries.
///
/// Deliberately **not** an `Option<ProducerTrust>`, so a call site cannot pass
/// `None` meaning "I did not check". [`RemoteTier::Unavailable`] is a stated
/// answer, and it is the [`Default`], so every existing caller keeps the
/// local-only behaviour it had by construction rather than by remembering to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RemoteTier {
    /// The tier is off, not built, not consented, or not configured. The
    /// resolver behaves exactly as it did before ADR-0019.
    #[default]
    Unavailable,
    /// The gate is open for this invocation, and the endpoint's model string
    /// carries this trust grade.
    Granted {
        /// How far that model string can be trusted to identify weights.
        trust: ProducerTrust,
    },
}

/// A resolved task: the model, the rule that chose it, and whether it is on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelChoice {
    /// The task this answers.
    pub task: ModelTask,
    /// Registry name of the chosen model, or `None` for [`ModelTask::Embed`]'s
    /// compiled-in hashing embedder (see [`ModelTask::default_model`]) and for
    /// every [`ModelSource::Remote`] resolution — a hosted model has no registry
    /// entry, and its vendor model string lives on the endpoint rather than here.
    pub model: Option<&'static str>,
    /// Whether a `[models]` key chose it, the default did, or the remote tier
    /// did.
    pub source: ModelSource,
    /// Whether every file of the host variant is present in the model store.
    ///
    /// `Some(true)` when there is nothing to install — [`ModelChoice::model`] is
    /// `None` for the compiled-in hashing embedder, which is in the binary. `None`
    /// is the *third* answer, and it means the question does not apply: a
    /// [`ModelSource::Remote`] choice has no weights on any disk, so reporting
    /// `Some(false)` would be a false negative pointing at `roteiro model pull`
    /// for a model no registry lists.
    pub installed: Option<bool>,
}

impl ModelChoice {
    /// The chosen model's name, or the label of the built-in fallback.
    #[must_use]
    pub fn label(&self) -> &'static str {
        if self.source.is_remote() {
            // Not the vendor model string: this returns a `&'static str`, and
            // more to the point the endpoint owns that string together with the
            // trust grade that qualifies it. A label that quoted it here would
            // read like a registry name.
            return "the remote model tier (`[remote] model`)";
        }
        self.model.unwrap_or("hashing embedder (offline default)")
    }

    /// One sentence saying **why** this model — the string `roteiro config`
    /// prints beside the name.
    #[must_use]
    pub fn why(&self) -> String {
        let key = self.task.config_key();
        match self.source {
            ModelSource::Pinned => format!("pinned by `[models] {key}`"),
            ModelSource::Default if self.model.is_none() => {
                format!("built-in default, no model needed (`[models] {key}` unset)")
            }
            ModelSource::Default => format!("built-in default (`[models] {key}` unset)"),
            // Says both halves, because either alone misleads: *this run* was
            // granted (so the reader knows it is not a standing setting), and the
            // identity is asserted rather than measured (so they know what the
            // model string is worth). `[models] <key>` is named as not applying,
            // since a pin that resolved and then did not run is exactly the
            // "configuration appearing honoured" this module refuses elsewhere.
            ModelSource::Remote { trust } => format!(
                "the remote model tier, granted for this run (ADR-0019) — `[remote] model`, \
                 not `[models] {key}`; identity is {trust}, a claim rather than a measurement"
            ),
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
    /// [`ModelChoiceError::NoModel`] for a task whose default is not a model, and
    /// [`ModelChoiceError::RemoteHasNoWeights`] for a remote resolution.
    pub fn require_installed(&self) -> Result<&'static str, ModelChoiceError> {
        // Checked before `model`, so a remote choice is reported as remote rather
        // than as a task with no model — the two are both `model: None` and mean
        // entirely different things to whoever reads the error.
        if self.source.is_remote() {
            return Err(ModelChoiceError::RemoteHasNoWeights { task: self.task });
        }
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
            // `Remote` is unreachable — it returned above — and `Default` is the
            // only case left. Matched rather than caught by a wildcard so that a
            // later variant has to decide what it means here instead of
            // inheriting "not installed" by accident.
            ModelSource::Default | ModelSource::Remote { .. } => ModelChoiceError::NotInstalled {
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
    /// A caller demanded local weights for a task the remote tier resolved.
    ///
    /// Not a fallback point. A surface reaching this has a remote resolution in
    /// hand and asked for a file path anyway, which means it has a local branch
    /// it forgot to guard — and quietly answering with the local default there is
    /// exactly the unannounced downgrade ADR-0019 exists to prevent.
    #[error(
        "`{task}` resolved to the remote model tier for this run, which has no weights on \
         this machine to install or load — a hosted model has no registry entry and no \
         digest. Send the request through the remote tier, or re-run without \
         `--allow-remote` to use the local model deliberately"
    )]
    RemoteHasNoWeights {
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
    let Some(name) = pins.for_task(task) else {
        let model = task.default_model();
        return Ok(ModelChoice {
            task,
            installed: Some(model.is_none_or(is_installed_now)),
            model,
            source: ModelSource::Default,
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
    })
}

/// Resolve `task` against `pins`, letting the remote tier take the two surfaces
/// it serves when the consent gate opened for this run (ADR-0019).
///
/// # The order, and why it is this order
///
/// The pin is resolved **first, and its errors still fail**, even when the tier
/// is granted and the local answer will be discarded. That is deliberate: an
/// unknown name or a wrong modality in `[models] generative` is a broken
/// configuration whether or not this particular run reads it, and a
/// `--allow-remote` that made a config error stop being reported would be a flag
/// that hides bugs as a side effect. Fixing the pin is cheap; discovering it
/// three weeks later when the flag comes off is not.
///
/// Then the tier wins, for [`ModelTask::goes_remote`] tasks only. It wins over a
/// pin because the two are not the same kind of statement: `[models] generative`
/// is a standing project default, and the invocation grant is a deliberate,
/// per-run act by a person who typed it. The specific overrides the standing —
/// and it does so **out loud**: [`ModelChoice::why`] names `[models] <key>` as
/// not applying, so a displaced pin is reported rather than silently skipped.
///
/// The four non-generative tasks are untouched by any [`RemoteTier`], so a
/// granted run still transcribes, describes, embeds and OCRs locally. See
/// [`ModelTask::goes_remote`] for why that is structural rather than a policy
/// this function could relax.
///
/// # Errors
/// As [`resolve_with`] — this adds none of its own.
pub fn resolve_with_remote(
    task: ModelTask,
    pins: &ModelPins,
    remote: RemoteTier,
) -> Result<ModelChoice, ModelChoiceError> {
    let local = resolve_with(task, pins)?;
    let RemoteTier::Granted { trust } = remote else {
        return Ok(local);
    };
    if !task.goes_remote() {
        return Ok(local);
    }
    Ok(ModelChoice {
        task,
        // No registry name and no weights: both are `None` because a hosted
        // model has neither, not because they were not looked up.
        model: None,
        source: ModelSource::Remote { trust },
        installed: None,
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
        DEFAULT_GENERATIVE, DEFAULT_OCR, ModelChoiceError, ModelPins, ModelSource, ModelTask,
        RemoteTier, TASKS, pin_accepts, resolve_all_with, resolve_with, resolve_with_remote,
    };
    use crate::models::{ModelKind, ModelRole, REGISTRY, ResourceTier};
    use crate::trust::ProducerTrust;

    /// The gate's answer as it actually arrives: open, with a model string
    /// nothing on this machine can verify.
    fn granted() -> RemoteTier {
        RemoteTier::Granted {
            trust: ProducerTrust::VendorAsserted,
        }
    }

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

    /// **The byte-identical-when-unset proof.** Every task with no pin must
    /// resolve to exactly the constant its call site held before this module
    /// existed — `voxtral-mini-3b`, `smolvlm-500m-gguf`, `ocrs-text`,
    /// `qwen3-0.6b`, and no model at all for `infer`. Asserted against literals
    /// rather than against the implementation, so a change to the table cannot
    /// quietly agree with itself.
    #[test]
    fn resolve_unset_matches_the_previous_hard_coded_models() {
        let unset = ModelPins::default();
        let expected = [
            (ModelTask::Embed, None),
            (ModelTask::Draft, Some("qwen3-0.6b")),
            (ModelTask::Chat, Some("qwen3-0.6b")),
            (ModelTask::Transcribe, Some("voxtral-mini-3b")),
            (ModelTask::Describe, Some("smolvlm-500m-gguf")),
            (ModelTask::Ocr, Some("ocrs-text")),
        ];
        for (task, model) in expected {
            let choice = resolve_with(task, &unset).expect("unset resolves");
            assert_eq!(choice.model, model, "{task} default");
            assert_eq!(choice.source, ModelSource::Default, "{task} source");
            assert!(
                choice.why().contains("unset"),
                "{task} explains itself: {}",
                choice.why()
            );
        }
    }

    /// The default generative model is a literal so the registry cannot move it
    /// silently — but it must still *be* the low-tier instruct pick `spec draft`
    /// used to search for. If a smaller instruct model is curated in, this fails
    /// and the choice becomes a decision rather than an accident.
    #[test]
    fn default_generative_is_still_the_low_tier_instruct_pick() {
        let searched = REGISTRY
            .iter()
            .find(|m| {
                m.kind == ModelKind::Generative
                    && m.role == ModelRole::Instruct
                    && m.tier == ResourceTier::Low
            })
            .expect("a low-tier instruct model is curated");
        assert_eq!(searched.name, DEFAULT_GENERATIVE);
    }

    /// Every default names a real registry entry of a kind the task can use.
    #[test]
    fn every_default_is_a_registry_model_the_task_can_use() {
        for task in TASKS {
            let Some(name) = task.default_model() else {
                continue;
            };
            let spec = crate::models::find(name).unwrap_or_else(|| panic!("{task}: {name} exists"));
            assert!(task.capable(spec.kind), "{task} can use {name}");
            assert!(
                pin_accepts(task.config_key(), spec.kind),
                "{task}'s own default would be accepted by its key"
            );
        }
        assert_eq!(DEFAULT_OCR, "ocrs-text");
    }

    /// A pin of the wrong modality is an error naming the key, **not** a silent
    /// fallback. This is the `GGML_ASSERT` case: a vision model pinned for audio
    /// would be handed to the ASR path, and a config that appears honoured while
    /// something else runs is the worst available outcome.
    #[test]
    fn a_wrong_modality_pin_errors_and_names_the_key() {
        let err = resolve_with(ModelTask::Transcribe, &pins("audio", "smolvlm-500m-gguf"))
            .expect_err("a vision model cannot transcribe");
        let ModelChoiceError::WrongKind { key, ref name, .. } = err else {
            panic!("expected WrongKind, got {err:?}");
        };
        assert_eq!(key, "audio");
        assert_eq!(name, "smolvlm-500m-gguf");
        let text = err.to_string();
        assert!(text.contains("[models] audio"), "names the key: {text}");
        assert!(text.contains("vision model"), "names what it is: {text}");
        assert!(
            text.contains("an audio model"),
            "names what it needs: {text}"
        );
    }

    /// The BERT-encoder case the capability filter was born from: an embedding
    /// model pinned as the generative one.
    #[test]
    fn an_embedding_model_is_refused_as_the_generative_pin() {
        for task in [ModelTask::Draft, ModelTask::Chat] {
            let err = resolve_with(task, &pins("generative", "bge-small-en-v1.5-gguf"))
                .expect_err("an encoder cannot generate");
            assert!(
                matches!(
                    err,
                    ModelChoiceError::WrongKind {
                        key: "generative",
                        ..
                    }
                ),
                "{task}: {err:?}"
            );
        }
    }

    /// `generative` governs both `draft` and `chat`. `chat` alone would accept a
    /// vision model — a multimodal GGUF does generate text — but `spec draft`
    /// cannot use one, so the shared key accepts generative models only. The
    /// alternative is a key that works for one of its two surfaces.
    #[test]
    fn a_shared_key_accepts_only_what_all_its_surfaces_accept() {
        assert!(ModelTask::Chat.capable(ModelKind::Vision));
        assert!(!ModelTask::Draft.capable(ModelKind::Vision));
        assert!(!pin_accepts("generative", ModelKind::Vision));
        assert!(pin_accepts("generative", ModelKind::Generative));
        let err = resolve_with(ModelTask::Chat, &pins("generative", "smolvlm-500m-gguf"))
            .expect_err("the key is shared with draft");
        let text = err.to_string();
        assert!(
            text.contains("roteiro spec draft") && text.contains("roteiro serve / Ask"),
            "says which surfaces the key governs: {text}"
        );
    }

    /// An unknown name is an error, not a fallback — a typo'd model must not
    /// resolve to the default and look honoured.
    #[test]
    fn an_unknown_pin_errors_rather_than_falling_back() {
        let err = resolve_with(ModelTask::Describe, &pins("vision", "smolvlm-500m"))
            .expect_err("no such model");
        assert!(
            matches!(err, ModelChoiceError::Unknown { key: "vision", .. }),
            "{err:?}"
        );
        assert!(
            err.to_string().contains("roteiro model list"),
            "actionable: {err}"
        );
    }

    /// A blank value is a mistake, and the useful reading of it is "unset" — an
    /// `unknown model ""` error would be a worse answer than the default.
    #[test]
    fn a_blank_pin_reads_as_unset() {
        let choice = resolve_with(ModelTask::Transcribe, &pins("audio", "   "))
            .expect("blank is not a name");
        assert_eq!(choice.source, ModelSource::Default);
        assert_eq!(choice.model, Some("voxtral-mini-3b"));
    }

    /// A valid pin is honoured, and says so.
    #[test]
    fn a_valid_pin_is_honoured_and_reports_its_key() {
        let choice = resolve_with(ModelTask::Draft, &pins("generative", "qwen3-8b"))
            .expect("a generative model is valid for draft");
        assert_eq!(choice.model, Some("qwen3-8b"));
        assert_eq!(choice.source, ModelSource::Pinned);
        assert_eq!(choice.why(), "pinned by `[models] generative`");
    }

    /// One pin moves exactly the tasks its key governs and no others.
    #[test]
    fn a_pin_moves_only_its_own_tasks() {
        let pinned = pins("audio", "voxtral-mini-3b");
        for (task, result) in resolve_all_with(&pinned) {
            let choice = result.expect("valid");
            let expected = if task == ModelTask::Transcribe {
                ModelSource::Pinned
            } else {
                ModelSource::Default
            };
            assert_eq!(choice.source, expected, "{task}");
        }
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

    /// **A granted tier changes exactly two surfaces.** `spec draft` and Ask are
    /// what ADR-0019 names; the other four are refused structurally rather than
    /// by a policy check, because there is no invocation inside extraction to
    /// grant anything with — see [`ModelTask::goes_remote`].
    ///
    /// The negative half is the half worth having: a run that granted egress must
    /// not thereby start sending audio, images or OCR blobs it was never asked
    /// about.
    #[test]
    fn a_granted_tier_takes_the_two_generative_surfaces_and_no_others() {
        let unset = ModelPins::default();
        for task in TASKS {
            let choice = resolve_with_remote(task, &unset, granted()).expect("resolves");
            if task.goes_remote() {
                assert_eq!(
                    choice.source,
                    ModelSource::Remote {
                        trust: ProducerTrust::VendorAsserted
                    },
                    "{task} is one of ADR-0019's two surfaces"
                );
                assert!(choice.model.is_none(), "{task}: no registry name exists");
                assert!(
                    choice.installed.is_none(),
                    "{task}: `installed` has no answer, not a negative one"
                );
            } else {
                assert_eq!(
                    choice,
                    resolve_with(task, &unset).expect("resolves"),
                    "{task} is untouched by a granted tier"
                );
                assert!(choice.installed.is_some(), "{task}: locally answerable");
            }
        }
        assert!(ModelTask::Draft.goes_remote() && ModelTask::Chat.goes_remote());
        assert!(
            !ModelTask::Embed.goes_remote()
                && !ModelTask::Transcribe.goes_remote()
                && !ModelTask::Describe.goes_remote()
                && !ModelTask::Ocr.goes_remote()
        );
    }

    /// **A remote resolution says on its face that its identity is a claim.**
    /// ADR-0019 §5: a vendor model string is a mutable pointer, so the weights
    /// behind it can change while the name does not. The variant carries the
    /// trust grade precisely so that nothing downstream has to remember to add
    /// the caveat.
    #[test]
    fn a_remote_resolution_declares_its_identity_a_claim() {
        let choice = resolve_with_remote(ModelTask::Chat, &ModelPins::default(), granted())
            .expect("resolves");
        assert_eq!(choice.source.as_str(), "remote");
        assert!(choice.source.is_remote());
        assert_eq!(choice.source.trust(), Some(ProducerTrust::VendorAsserted));

        let why = choice.why();
        assert!(why.contains("vendor_asserted"), "{why}");
        assert!(why.contains("claim rather than a measurement"), "{why}");
        assert!(why.contains("granted for this run"), "{why}");
        // The label points at the endpoint's key rather than quoting a model
        // name, so nothing reads a mutable pointer as a registry entry.
        assert_eq!(choice.label(), "the remote model tier (`[remote] model`)");

        // A local resolution has no grade to report, and must not acquire one:
        // `pinned`/`default` identities are digests measured on this machine.
        let local = resolve_with(ModelTask::Chat, &ModelPins::default()).expect("resolves");
        assert_eq!(local.source.trust(), None);
        assert!(!local.source.is_remote());
    }

    /// **A displaced pin is reported, never silently skipped.** `--allow-remote`
    /// is a deliberate per-run act and outranks a standing `[models] generative`,
    /// but a configuration that appears honoured while something else ran is the
    /// exact failure this module exists to prevent — so `why()` names the key it
    /// did not use.
    #[test]
    fn a_granted_tier_displaces_a_pin_and_says_which_key_it_displaced() {
        let pinned = pins("generative", "qwen3-8b");
        let local = resolve_with(ModelTask::Draft, &pinned).expect("a valid pin");
        assert_eq!(local.model, Some("qwen3-8b"));
        assert_eq!(local.source, ModelSource::Pinned);

        let remote = resolve_with_remote(ModelTask::Draft, &pinned, granted()).expect("resolves");
        assert!(remote.source.is_remote(), "the invocation outranks the pin");
        let why = remote.why();
        assert!(
            why.contains("not `[models] generative`"),
            "the displaced key is named: {why}"
        );
    }

    /// **A broken pin still fails under a granted tier.** The local answer is
    /// about to be discarded, and the error is raised anyway: an unknown name or
    /// a wrong modality is a broken configuration whether or not this run reads
    /// it, and a flag that made config errors stop being reported would hide bugs
    /// as a side effect of granting egress.
    #[test]
    fn a_broken_pin_still_fails_under_a_granted_tier() {
        let err = resolve_with_remote(
            ModelTask::Draft,
            &pins("generative", "bge-small-en-v1.5-gguf"),
            granted(),
        )
        .expect_err("an encoder cannot generate, tier or no tier");
        assert!(
            matches!(
                err,
                ModelChoiceError::WrongKind {
                    key: "generative",
                    ..
                }
            ),
            "{err:?}"
        );
        let err = resolve_with_remote(
            ModelTask::Chat,
            &pins("generative", "no-such-model"),
            granted(),
        )
        .expect_err("an unknown name is still unknown");
        assert!(matches!(err, ModelChoiceError::Unknown { .. }), "{err:?}");
    }

    /// **The default is off, structurally.** [`RemoteTier`]'s `Default` is
    /// `Unavailable`, and an unavailable tier resolves every task to exactly what
    /// `resolve_with` returns — so a caller that forgets the tier gets the local
    /// answer rather than an accidental grant.
    #[test]
    fn an_unavailable_tier_resolves_exactly_as_the_local_resolver_does() {
        assert_eq!(RemoteTier::default(), RemoteTier::Unavailable);
        for pins in [
            ModelPins::default(),
            pins("generative", "qwen3-8b"),
            pins("audio", "voxtral-mini-3b"),
        ] {
            for task in TASKS {
                assert_eq!(
                    resolve_with_remote(task, &pins, RemoteTier::default()).ok(),
                    resolve_with(task, &pins).ok(),
                    "{task}: an unavailable tier changes nothing"
                );
            }
        }
    }

    /// **`require_installed` on a remote choice refuses instead of falling back.**
    /// A surface holding a remote resolution that asks for a file path has a
    /// local branch it forgot to guard, and answering it with the local default
    /// would be the unannounced downgrade ADR-0019 most needs to prevent. The
    /// error says so and offers the deliberate alternative.
    #[test]
    fn require_installed_on_a_remote_choice_refuses_rather_than_falling_back() {
        let choice = resolve_with_remote(ModelTask::Draft, &ModelPins::default(), granted())
            .expect("resolves");
        let err = choice
            .require_installed()
            .expect_err("there are no weights to require");
        assert!(
            matches!(
                err,
                ModelChoiceError::RemoteHasNoWeights {
                    task: ModelTask::Draft
                }
            ),
            "{err:?}"
        );
        let text = err.to_string();
        assert!(text.contains("no registry entry and no digest"), "{text}");
        assert!(text.contains("--allow-remote"), "actionable: {text}");
        // …and it is *not* reported as the modelless-task case, which is also
        // `model: None` and means something entirely different to a reader.
        assert!(!matches!(err, ModelChoiceError::NoModel { .. }));
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

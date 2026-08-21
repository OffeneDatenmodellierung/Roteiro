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

impl Loaded {
    /// The effective `[debt] ignore` patterns, each tagged with the layer it came
    /// from (`"project"`, `"user"`, or `"project, user"` when both name it).
    ///
    /// Because this list **merges** across layers ([`merge_ignore`]), one label
    /// per key would be a lie — the effective list can hold patterns from both
    /// layers at once — so provenance is reported per *pattern*. `roteiro config`
    /// prints this, which is what makes the merge legible instead of magic.
    #[must_use]
    pub fn debt_ignore_sources(&self) -> Vec<(&str, &'static str)> {
        let contains = |c: &Config, pattern: &str| {
            c.debt
                .ignore
                .as_deref()
                .is_some_and(|ps| ps.iter().any(|p| p == pattern))
        };
        self.effective
            .debt
            .ignore
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|pattern| {
                let layer = match (
                    contains(&self.project, pattern),
                    contains(&self.user, pattern),
                ) {
                    (true, true) => "project, user",
                    (true, false) => "project",
                    (false, true) => "user",
                    // Unreachable while the effective list is built from the two
                    // layers; reported honestly rather than asserted away.
                    (false, false) => "unknown",
                };
                (pattern.as_str(), layer)
            })
            .collect()
    }

    /// Whether the **user** layer asked for an `ignore_reset` that did nothing.
    ///
    /// A reset drops what a layer inherits, and the user layer is the bottom of
    /// the two ([`DebtConfig::ignore_reset`]), so its flag never reaches
    /// [`merge_ignore`]. Reporting that is the point: a key whose whole argument
    /// is "a reset cannot fail quietly" must not itself fail quietly, and
    /// `roteiro config` is where a reader goes to find out what their
    /// configuration did.
    ///
    /// False when a reset *is* in force, even though the user's own flag was
    /// still individually inert: the project layer reset, patterns really were
    /// dropped, and `roteiro config` says so. Adding "…and your reset did
    /// nothing" beside that would be true but unreadable, and the user got the
    /// behaviour they asked for either way.
    #[must_use]
    pub fn debt_ignore_reset_was_inert(&self) -> bool {
        self.user.debt.ignore_reset == Some(true) && self.effective.debt.ignore_reset != Some(true)
    }

    /// The user-layer `[debt] ignore` patterns that `ignore_reset` discarded, so
    /// the reset is *visible* rather than merely effective. Empty when no reset is
    /// in force.
    ///
    /// Empty in practice unless the project layer both reset **and** declined to
    /// restate a user pattern, since the kept list is otherwise a superset of the
    /// user's. That is why this could never have exposed the inherited-flag defect
    /// on its own — with no reset in force the kept list is the union, so every
    /// user pattern is present and the filter yields nothing. The false claim was
    /// the unconditional headline in `roteiro config`, not this list.
    #[must_use]
    pub fn debt_ignore_discarded(&self) -> Vec<&str> {
        if self.effective.debt.ignore_reset != Some(true) {
            return Vec::new();
        }
        let kept = self.effective.debt.ignore.as_deref().unwrap_or_default();
        self.user
            .debt
            .ignore
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|p| !kept.iter().any(|k| k == *p))
            .map(String::as_str)
            .collect()
    }
}

#[cfg(feature = "remote")]
impl Loaded {
    /// What the two **config layers** say about the remote model tier, as the
    /// consent gate reads them (ADR-0019 §3).
    ///
    /// Built from the *layers*, not from [`Loaded::effective`], because the
    /// merge is lossy on purpose: a project-layer grant is discarded on the way
    /// to the effective value, and `rto_remote::ConfigGrant` is what remembers
    /// there was one to report. The invocation is the caller's to supply.
    #[must_use]
    pub fn remote_config_grant(&self) -> rto_remote::ConfigGrant {
        rto_remote::ConfigGrant::from_layers(self.project.remote.enabled, self.user.remote.enabled)
    }
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
    /// `[remote]` — the optional, default-off remote model tier (ADR-0019).
    /// **One of the two tables whose grant key does not follow this file's
    /// precedence**; see [`RemoteConfig`], and [`LintConfig`] for the other.
    pub remote: RemoteConfig,
    /// `[lint]` — whether `roteiro lint` may run a linter on this host
    /// (ADR-0020 §6). **The other inverted key**; see [`LintConfig`].
    pub lint: LintConfig,
    /// `[security]` — the images `roteiro security run` sandboxes analyzers in
    /// (ADR-0014). Ordinary precedence: these are locators, not permissions —
    /// see [`SecurityConfig`] for the ADR-0007 v1.4 classification.
    pub security: SecurityConfig,
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
    ///
    /// **This list is additive across layers**: the project's patterns are
    /// appended to the user's rather than replacing them (see
    /// [`Config::overlaid_with`]). To start from nothing instead, set
    /// [`DebtConfig::ignore_reset`].
    pub ignore: Option<Vec<String>>,
    /// Drop every [`DebtConfig::ignore`] pattern inherited from a lower layer,
    /// so this layer's list stands alone.
    ///
    /// This is the deliberate answer to "if lists merge, how does a user *remove*
    /// an inherited pattern?" — an explicit, all-or-nothing reset rather than a
    /// per-pattern negation prefix such as `!vendor/**`. Two reasons, both about
    /// keeping failure loud:
    ///
    /// 1. **A reset cannot fail quietly; a negation can.** Mistype
    ///    `!vendour/**` and it matches no inherited pattern, removes nothing, and
    ///    says nothing — a wrong-but-quiet answer, the exact failure this key
    ///    exists to prevent. A reset is present or absent, with no third silent
    ///    state, and `roteiro config` prints the patterns it discarded.
    /// 2. **`!` already means something else.** In `.gitignore` — the syntax
    ///    every reader of a glob-exclusion list has in mind — a leading `!`
    ///    *re-includes a matching file*; it does not delete an inherited rule.
    ///    Borrowing a familiar sigil for an unfamiliar operation is worse than an
    ///    unfamiliar key that means exactly what it says.
    ///
    /// The cost is coarseness: you cannot drop one inherited pattern and keep the
    /// rest — you restate the ones you want. Exclusion lists are short, so that is
    /// cheap, and restating them is visible in review, which subtracting one
    /// invisibly would not be.
    ///
    /// # Effective in the project layer only (today)
    ///
    /// A reset drops what a layer **inherits**, so it means something only where
    /// there is a layer beneath it. There are exactly two layers — user
    /// (`~/.roteiro/config.toml`) then project (`roteiro.toml`), applied as
    /// `user.overlaid_with(&project)` — so the user layer is the bottom, and
    /// [`merge_ignore`] consults the *nearer* layer's flag and only that one.
    /// Setting this in the user config therefore resets nothing.
    ///
    /// That is an uncomfortable shape for a key whose entire argument is "a reset
    /// cannot fail quietly", so it is **not** left to this doc comment to carry.
    /// `roteiro config` reports an inert user-layer reset in so many words
    /// ([`Loaded::debt_ignore_reset_was_inert`]) — the same principle as the
    /// merge itself: the surface whose job is explaining the configuration says
    /// what happened, rather than expecting the reader to have found this
    /// paragraph.
    ///
    /// It is deliberately **not** a hard error. The key is inert because of
    /// today's layer *arrangement*, not because it is meaningless: add a built-in
    /// defaults layer beneath `user` — a plausible future — and a user-layer reset
    /// starts doing exactly what it says, with no config to migrate. Rejecting it
    /// permanently would encode a temporary fact as a rule.
    ///
    /// Accepted as `ignore_reset` (canonical, matching every other key in this
    /// file) or `ignore-reset`.
    #[serde(alias = "ignore-reset")]
    pub ignore_reset: Option<bool>,
}

/// `[remote]` — the optional, **default-off** remote model tier (ADR-0019), and
/// the one table in this file whose precedence is not the one the module docs
/// describe.
///
/// # `enabled` inverts the layering, and only `enabled`
///
/// ADR-0007's order is **CLI flag > project > user > built-in default**. For
/// [`RemoteConfig::enabled`] it is inverted, because `roteiro.toml` is
/// *committed and shared by design* — this file's own reason for existing — so a
/// merged line authorising egress on every teammate's machine would be consent
/// granted by someone else and noticed by nobody:
///
/// | Layer | May deny | May grant |
/// |---|---|---|
/// | Built-in default | denied by default | — |
/// | Project `roteiro.toml` | **yes** | **no** |
/// | User `~/.roteiro/config.toml` | yes | yes — necessary, not sufficient |
/// | Invocation (`--allow-remote`) | yes | yes — necessary, not sufficient |
///
/// [`RemoteConfig::endpoint`] and [`RemoteConfig::model`] are **ordinary keys**
/// and layer ordinarily, project over user. A project may point the tier at its
/// own gateway; it still cannot turn the tier on, and the endpoint it chose is
/// printed by `roteiro remote status` and by every dry-run before anything is
/// sent.
#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct RemoteConfig {
    /// Whether *this layer* opts into the remote model tier.
    ///
    /// Meaningful per layer rather than merged like a scalar — see the type's
    /// docs, and [`remote_enabled_effective`] for the merge.
    pub enabled: Option<bool>,
    /// The URL a request goes to. Must be `https://`, or `http://` to a loopback
    /// address for a gateway that terminates TLS itself.
    pub endpoint: Option<String>,
    /// The vendor's model string. It is a **mutable pointer**: the weights behind
    /// it can change while the name does not, which is why anything recorded from
    /// this tier is `vendor_asserted` rather than digest-pinned.
    pub model: Option<String>,
}

impl ModelsConfig {
    /// Overlay the project layer (`over`) on the user layer (`self`).
    ///
    /// Five ordinary keys, ordinary precedence. It became its own function when
    /// `[lint]` was added and [`Config::overlaid_with`] crossed the length limit
    /// — but it belongs here regardless: `[remote]`, `[lint]` and `[telemetry]`
    /// already overlay themselves, and the largest table in the file was the odd
    /// one out for no reason beyond the order things were written in.
    fn overlaid_with(&self, over: &Self) -> Self {
        let pick = |over: &Option<String>, own: &Option<String>| over.clone().or(own.clone());
        Self {
            embedding: pick(&over.embedding, &self.embedding),
            generative: pick(&over.generative, &self.generative),
            vision: pick(&over.vision, &self.vision),
            audio: pick(&over.audio, &self.audio),
            ocr: pick(&over.ocr, &self.ocr),
        }
    }
}

impl RemoteConfig {
    /// Overlay the project layer (`over`) on the user layer (`self`).
    ///
    /// Its own function, unlike the tables that inline their overlay in
    /// [`Config::overlaid_with`], because **one of these three fields does not
    /// follow the rule the other two do** and that difference deserves to be
    /// visible at the point it is applied rather than inferred from a comment
    /// twenty lines into a hundred-line merge.
    fn overlaid_with(&self, over: &Self) -> Self {
        Self {
            // ADR-0019 §3. `over.enabled.or(self.enabled)` — the shape every
            // other scalar in this file uses — would let a merged line in a
            // committed `roteiro.toml` authorise egress on every teammate's
            // machine, which is the failure the inversion exists to prevent.
            enabled: remote_enabled_effective(self.enabled, over.enabled),
            // Ordinary keys, ordinary precedence: a project may choose *where*
            // its own gateway is without being able to turn the tier on.
            endpoint: over.endpoint.clone().or_else(|| self.endpoint.clone()),
            model: over.model.clone().or_else(|| self.model.clone()),
        }
    }
}

/// `[lint]` — whether `roteiro lint` may run a linter **on this host**
/// (ADR-0020 §6), and the second table in this file whose key does not follow
/// the module's precedence.
///
/// # `allow_unsandboxed` inverts the layering, for [`RemoteConfig`]'s reason
///
/// `roteiro lint` runs the linter sandboxed by default. `cargo clippy` has
/// `cargo check` semantics, so running it on the host executes the tree's build
/// scripts and loads its proc macros with the invoking user's filesystem and
/// credentials — and that the *toolchain* is yours does not make the *code*
/// yours when you are linting a branch you are reviewing. So the host is
/// permitted rather than assumed, and the permission layers as ADR-0019 §3
/// layers egress, because `roteiro.toml` is committed and shared:
///
/// | Layer | May deny | May grant |
/// |---|---|---|
/// | Built-in default | sandboxed by default | — |
/// | Project `roteiro.toml` | **yes** | **no** |
/// | User `~/.roteiro/config.toml` | yes | **yes** |
/// | Invocation (`--allow-unsandboxed` / `--sandboxed`) | yes | **yes** |
///
/// A merged line that starts running builds on every teammate's machine is
/// consent granted by someone else and noticed by nobody.
///
/// # It differs from `[remote] enabled` in one respect, deliberately
///
/// There, the user layer and the invocation must **both** grant. Here **either
/// suffices** (ADR-0020 §6). Remote egress sends your source elsewhere and is
/// worth re-consenting to per run; building on your own machine with your own
/// toolchain is a standing preference a person may reasonably express once —
/// and requiring both would make this key useless, since you would still type
/// the flag on every run. Do not reconcile the two.
///
/// The whole difference lives in `rto_exec::lint::decide`, not here: this table
/// contributes the *config* half, which is the half the two tiers agree on.
#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct LintConfig {
    /// Whether *this layer* permits `roteiro lint` to run a linter on this host.
    ///
    /// Meaningful per layer rather than merged like a scalar — see the type's
    /// docs, and [`lint_allow_unsandboxed_effective`] for the merge. Named after
    /// `security run --allow-unsandboxed` because it is the same concept and a
    /// reader should not have to learn a second word for it; scoped to *this*
    /// command by the table it sits in, which is what stops it reading as a
    /// setting that governs both.
    #[serde(alias = "allow-unsandboxed")]
    pub allow_unsandboxed: Option<bool>,
    /// The digest-pinned OCI image the sandboxed linter runs in.
    ///
    /// **Roteiro ships no default, and that is a decision rather than an
    /// omission.** A builder's image has to carry the linter — for `clippy`, a
    /// Rust toolchain *with the `clippy` component* — and no first-party image
    /// does: `rust-lang/docker-rust` builds every stable and nightly variant
    /// with `rustup-init --profile minimal`, which installs `rustc`, `cargo` and
    /// `rust-std` and stops. Picking a third party's image on the user's behalf
    /// would make somebody else's container the boundary in which somebody
    /// else's build scripts execute, chosen by Roteiro and noticed by nobody;
    /// building one would make Roteiro the publisher of it. Neither is a job
    /// this project takes on, so the image is **supplied**, and an image without
    /// the linter in it is a named refusal that says how to build one.
    ///
    /// # Ordinary precedence, unlike the key above it
    ///
    /// Project over user, and `--image` over both — `[remote] gateway`'s rule
    /// and for the same reason: a project may choose *where* its team's boundary
    /// comes from without being able to decide *whether* there is one. The
    /// inversion belongs to `allow_unsandboxed`, which is a permission; this is
    /// a locator.
    ///
    /// A tag is refused. An image is where somebody else's build scripts
    /// execute, and a tag is a mutable pointer to it.
    pub image: Option<String>,
}

impl LintConfig {
    /// Overlay the project layer (`over`) on the user layer (`self`).
    ///
    /// Its own function for [`RemoteConfig::overlaid_with`]'s reason: the rule
    /// this applies is not the rule the rest of the merge applies, and that
    /// belongs where it is applied rather than in a comment far away.
    fn overlaid_with(&self, over: &Self) -> Self {
        Self {
            // ADR-0020 §6. `over.allow_unsandboxed.or(self.allow_unsandboxed)` —
            // the shape every ordinary scalar uses — would let a merged line in
            // a committed `roteiro.toml` start running builds on every
            // teammate's machine.
            allow_unsandboxed: lint_allow_unsandboxed_effective(
                self.allow_unsandboxed,
                over.allow_unsandboxed,
            ),
            // Ordinary key, ordinary precedence — `[remote] gateway`'s rule: a
            // project may choose *where* its team's boundary comes from without
            // being able to decide *whether* there is one.
            image: over.image.clone().or(self.image.clone()),
        }
    }
}

/// The `[lint] allow_unsandboxed` the config layers jointly produce, before the
/// invocation is consulted.
///
/// Delegates to `rto_exec::lint::ConfigGrant`, so the value `roteiro config`
/// echoes and the value the gate consults cannot drift apart — the same
/// arrangement [`remote_enabled_effective`] has with `rto_remote`.
#[cfg(feature = "exec-subprocess")]
fn lint_allow_unsandboxed_effective(user: Option<bool>, project: Option<bool>) -> Option<bool> {
    rto_exec::LintConfigGrant::from_layers(project, user).as_effective()
}

/// Without a linter to run, the effective value is unset whatever the layers
/// say.
///
/// Reported as unset rather than echoed back, for the reason
/// [`remote_enabled_effective`]'s counterpart gives: echoing
/// `allow_unsandboxed = true` from a build that cannot run a linter would
/// describe a permission that permits nothing. The key still **parses**, so a
/// config shared with a fuller build is never rejected by a leaner one.
#[cfg(not(feature = "exec-subprocess"))]
fn lint_allow_unsandboxed_effective(_user: Option<bool>, _project: Option<bool>) -> Option<bool> {
    None
}

/// The `[remote] enabled` the config layers jointly produce, before the
/// invocation is consulted.
///
/// Delegates to `rto_remote::ConfigGrant`, which is the workspace's single
/// implementation of "a project may deny but never grant" — so the value
/// `roteiro config` echoes and the value the gate consults cannot drift apart.
#[cfg(feature = "remote")]
fn remote_enabled_effective(user: Option<bool>, project: Option<bool>) -> Option<bool> {
    rto_remote::ConfigGrant::from_layers(project, user).as_effective()
}

/// Without the `remote` feature there is no tier for a key to enable, so the
/// effective value is unset whatever the layers say.
///
/// Reported as unset rather than echoed back, because echoing `enabled = true`
/// from a build that cannot send anything would describe a capability that is not
/// there. The key still **parses** in this build, so a config shared with a
/// fuller one is never rejected by a leaner one (ADR-0007's forward-compatibility
/// rule); `roteiro config` says the build has no remote tier.
#[cfg(not(feature = "remote"))]
fn remote_enabled_effective(_user: Option<bool>, _project: Option<bool>) -> Option<bool> {
    None
}

/// `[security]` — how `roteiro security run` sandboxes an analyzer.
///
/// # Why this table is a map where `[lint]`'s is a scalar
///
/// A builder has one image, so `[lint] image` is one key. The analyzer path has
/// **N**: one per analyzer, each a different tool with a different published
/// container, and `roteiro security run <analyzer>` picks between them by name.
/// So the shape follows the surface rather than the precedent — a scalar here
/// could only mean "the image every analyzer runs in", which is not a thing that
/// exists.
///
/// It **composes with** the built-in table rather than replacing it. With this
/// table absent, `security run` behaves exactly as it did: `semgrep` runs in the
/// image Roteiro pinned, and an analyzer with no pin refuses. See
/// `rto_exec::boxlite::resolve_image` for what an entry does when there is
/// already a pin — it wins, and that decision is argued where it is implemented.
///
/// # Ordinary precedence: these are values, not capabilities
///
/// Classified against [[docs/adr/0007-configuration-file.md]] v1.4's five tests
/// rather than assumed, because getting the layering direction wrong here is how
/// a committed `roteiro.toml` starts choosing the boundary somebody else's
/// analyzer runs in:
///
/// 1. **Sends repository content off the machine?** No. A run never opens a
///    socket — the guest has no network interface — and the pull is
///    `roteiro security prefetch --allow-download`, an invocation rather than
///    this key.
/// 2. **Executes code the repository supplies?** No, twice over. The analyzers
///    reachable here *parse* the tree rather than building it (ADR-0014's own
///    reading of the security argument), and the image is registry content named
///    by a locator, not something the repository carries.
/// 3. **Writes outside the repository and Roteiro's caches?** No. Provisioning
///    writes into `~/.roteiro/security/boxlite-home`, which is written regardless;
///    this changes *what* is in it, never *whether* — test 3 as v1.4 sharpened it,
///    the same reading that made `[paths] model_store` a value.
/// 4. **Spends materially more of the machine?** The pull is large, and it is
///    not this key that spends it: `prefetch --allow-download` does, having
///    printed the reference first, and a run refuses rather than fetching.
/// 5. **Removes a guard?** It is the opposite, and this is the decisive one. The
///    key's *direction* is to put an analyzer that had no sandboxed path at all
///    **inside** one. Unset, `cargo-audit` and `osv-scanner` are host-only or
///    refused; set, they run in a microVM. A key whose default grants nothing and
///    whose effect is to add a boundary is not a permission.
///
/// So: **value**, ordinary precedence — project over user, exactly `[lint] image`
/// and `[remote] endpoint`. A project may choose *where* its team's boundary
/// comes from without deciding *whether* there is one; and for a locator the
/// inversion is not merely unnecessary but inexpressible, since there is no
/// "deny" for a locator, only a different one.
///
/// The residual is real and is answered by disclosure rather than by precedence:
/// a committed file *does* choose the container somebody else's analyzer runs in,
/// so `prefetch` names the reference before opening a socket, `security status`
/// says which images are built-in and which are declared, and the run records the
/// digest that actually ran.
#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct SecurityConfig {
    /// Digest-pinned OCI images for `roteiro security run`, keyed by analyzer.
    ///
    /// ```toml
    /// [security.images]
    /// osv-scanner = "registry.example/you/osv-scanner@sha256:…"
    /// ```
    ///
    /// **A tag is refused.** An image is where somebody else's code executes,
    /// and a tag is a mutable pointer to it — the same rule, the same message
    /// and the same function as `[lint] image`, because the difference between a
    /// pinned entry and a declared one is *who chose*, never *how strong the pin
    /// is*.
    ///
    /// Nothing is validated in this module, which is deliberate and is
    /// [`ModelsConfig::resolve`]'s rule: a value that is wrong is refused where
    /// it is *consumed*, so `roteiro config` can **report** a bad entry rather
    /// than being the one command a bad entry stops. Every path that resolves an
    /// image goes through `rto_exec::boxlite::resolve_image`, so there is one
    /// refusal and it fires before a guest boots — including at `prefetch` and
    /// `status`, so a typo in a committed file does not lie dormant until
    /// somebody runs that analyzer. [`Self::problems`] is the reporting half.
    ///
    /// **Public registries only, for now, and that is a conflict rather than an
    /// unimplemented nicety.** A private registry needs credentials at pull
    /// time, and they would have to come from the ambient environment — inside a
    /// feature whose whole `EnvironmentPolicy::Scrubbed` posture exists to keep
    /// ambient credentials *out*, and whose guest never receives an environment
    /// at all. Resolving that needs a credential story, not a code path, so
    /// until there is one an image must be pullable without authentication.
    pub images: std::collections::BTreeMap<String, String>,
}

impl SecurityConfig {
    /// The declared images as the resolver takes them: `(analyzer, reference)`.
    ///
    /// A `Vec` rather than the map, because `rto-exec` owns what a reference
    /// *means* and owns none of the layering that produced it — the same seam
    /// `[lint] image` crosses as a plain `&str`.
    ///
    /// Gated on the backend that resolves images, because in a build without one
    /// its only caller is compiled out — and an item whose callers are all cfg'd
    /// away is precisely the `-D warnings` rejection ADR-0014 v1.7 records the
    /// `no-default-features` job existing to catch. [`Self::image_for`] and
    /// [`Self::problems`] are ungated: reporting a key is not a property of which
    /// backends were compiled in.
    #[cfg(feature = "exec-boxlite")]
    #[must_use]
    pub fn declared(&self) -> Vec<(String, String)> {
        self.images
            .iter()
            .map(|(analyzer, reference)| (analyzer.clone(), reference.clone()))
            .collect()
    }

    /// The declared reference for one analyzer, if this configuration names one.
    #[must_use]
    pub fn image_for(&self, analyzer: &str) -> Option<&str> {
        self.images.get(analyzer).map(String::as_str)
    }

    /// Everything wrong with this table, as sentences, for `roteiro config` to
    /// **print** rather than refuse over.
    ///
    /// The report-don't-refuse half of ADR-0007 v1.3: `roteiro config` is the
    /// command an operator reaches for precisely because a key is not doing what
    /// they expected, so it must not be the one command that key stops. The
    /// refusal itself lives at every consuming site, in one function.
    ///
    /// Checks the pin through `rto_exec::image_pinned_digest`, which is the same
    /// function the refusal uses — a second implementation written for reporting
    /// is how one of the two ends up laxer than the other.
    #[cfg(feature = "execution")]
    #[must_use]
    pub fn problems(&self) -> Vec<String> {
        self.images
            .iter()
            .filter_map(|(analyzer, reference)| {
                if rto_exec::adapter_for(analyzer).is_none() {
                    return Some(format!(
                        "`{analyzer}` is not an analyzer this build can read the output of \
                         (it can read: {}) — an image can only serve an analyzer Roteiro already \
                         has an adapter for, because the parser is Rust and cannot be supplied \
                         alongside the image",
                        rto_exec::known_analyzers().join(", ")
                    ));
                }
                rto_exec::image_pinned_digest(&format!("`[security.images] {analyzer}`"), reference)
                    .err()
                    .map(|e| e.to_string())
            })
            .collect()
    }

    /// Without `execution` there is no analyzer surface for a key to name, so
    /// there is nothing this build can say is wrong with one.
    ///
    /// Empty rather than absent, and never a parse failure: ADR-0007's
    /// forward-compatibility rule is that a config shared with a fuller build is
    /// not rejected by a leaner one, and `roteiro config` says the build has no
    /// analyzer surface where it prints the section.
    #[cfg(not(feature = "execution"))]
    #[must_use]
    pub fn problems(&self) -> Vec<String> {
        Vec::new()
    }
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
    /// Names of other **named** workspaces whose members fold into this one,
    /// transitively (ADR-0008 v1.3 nested workspaces): every `roots`/`repos` entry
    /// of each included workspace — and of everything *it* includes — joins this
    /// workspace's own.
    ///
    /// Nesting adds no expressiveness: the same paths could be listed under both
    /// names, since a repo may belong to any number of workspaces. What it adds is
    /// that the two lists cannot drift. Resolution therefore **flattens** — a
    /// composed workspace is an ordinary flat [`rto_graph::ResolvedWorkspace`] with
    /// more members, so no consumer of [`Config::resolved_workspaces`] learns a new
    /// concept.
    ///
    /// Only names that exist are includable: every `[[workspaces]]` entry, plus the
    /// legacy `[workspace]` under the name it folds in as (`default`). A
    /// `[standalone]` repo cannot be included — the table is unnamed, so there is
    /// nothing to reference, which also removes by construction the incoherence of a
    /// linked workspace absorbing repos declared to have no cross-repo links.
    ///
    /// Shaped as `Option<Vec<String>>` to match its `roots`/`repos` siblings: the
    /// three are read, merged and serialised by the same paths, and an unset key
    /// means the same "not declared here" for all of them.
    pub includes: Option<Vec<String>>,
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
///
/// One key per model **kind**, not per command: `generative` governs both `spec
/// draft` and the Ask panel, because they want the same kind of model and a
/// project that pins one and not the other has almost certainly made a mistake.
/// Which key governs which surface is [`rto_graph::ModelTask::config_key`], and
/// `roteiro config` prints the whole mapping resolved.
///
/// `vision`, `audio` and `ocr` arrived in Stage 33. Until then those three models
/// were compiled-in string constants, so **a project could not pin its ASR model
/// at all** — the setting simply did not exist. Every key is still optional and
/// unset still means exactly what it meant before: the same model, chosen the
/// same way.
#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct ModelsConfig {
    /// Registry name of the embedding model `roteiro infer` embeds nodes with.
    pub embedding: Option<String>,
    /// Registry name of the generative model `spec draft` and the Ask panel
    /// generate with.
    pub generative: Option<String>,
    /// Registry name of the vision-language model `media build` describes images
    /// with.
    pub vision: Option<String>,
    /// Registry name of the audio model `media build` transcribes speech with.
    pub audio: Option<String>,
    /// Registry name of the OCR model `sync` reads literal image text with.
    pub ocr: Option<String>,
}

/// The one reading of a `[models]` value: surrounding whitespace is not part of a
/// registry name, and a value with nothing left after trimming names no model, so
/// the key is unset.
///
/// **The resolver's semantics, adopted here rather than the other way round**,
/// because the resolver's are the ones that decide what actually runs
/// ([`rto_graph::ModelPins::by_key`]). A reporting surface that disagreed with
/// them would be describing a run that did not happen.
fn model_pin(raw: Option<&str>) -> Option<&str> {
    raw.map(str::trim).filter(|value| !value.is_empty())
}

impl ModelsConfig {
    /// Drop values that name no model, so every downstream surface sees one
    /// answer for "is this key set?".
    ///
    /// Applied once, where a layer is parsed ([`read_config`]), rather than at
    /// each point of use — there are four of those (`roteiro config`'s key list,
    /// its per-surface resolution table, the `--json` echo of the effective
    /// config, and the resolver), and normalising at the point of use would have
    /// meant four implementations of one rule with nothing holding them level.
    ///
    /// That is not hypothetical: `[models] audio = "   "` was reported as **set**
    /// and attributed to a layer by two of those surfaces while the other two
    /// resolved it as **unset** (PR #379 review). Normalising the parsed value
    /// makes the four agree by construction instead of by coincidence.
    fn normalize(&mut self) {
        for slot in [
            &mut self.embedding,
            &mut self.generative,
            &mut self.vision,
            &mut self.audio,
            &mut self.ocr,
        ] {
            *slot = slot
                .take()
                .and_then(|value| model_pin(Some(&value)).map(ToOwned::to_owned));
        }
    }

    /// The value of one `[models]` key by name, so a caller iterating the
    /// resolver's tasks can report *which layer* set the key governing each one
    /// without a match arm per key at every call site.
    ///
    /// Reads through [`model_pin`], so it gives the same answer as the resolver
    /// even for a `ModelsConfig` that never went through [`ModelsConfig::normalize`]
    /// — one built in a test, say. Belt and braces on the parsed path, and the
    /// only guard on every other path.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        model_pin(match key {
            "embedding" => self.embedding.as_deref(),
            "generative" => self.generative.as_deref(),
            "vision" => self.vision.as_deref(),
            "audio" => self.audio.as_deref(),
            "ocr" => self.ocr.as_deref(),
            _ => None,
        })
    }

    /// Resolve to the graph-layer pins the resolver reads, in the same shape as
    /// [`IngestConfig::resolve`]: this layer owns precedence between config
    /// files, `rto-graph` owns what the resulting names *mean*.
    ///
    /// Nothing is validated here. A name is checked against the registry when a
    /// task is resolved, so `roteiro config` can *report* a bad pin instead of
    /// refusing to run — which is the command an operator reaches for when a pin
    /// is not doing what they expected.
    ///
    /// Gated on `models` because the resolver is: without the registry there is
    /// nothing for a name to resolve *to*. The keys themselves still parse, and
    /// `roteiro config` still shows them, so a config shared with a fuller build
    /// is never rejected by a leaner one (ADR-0007's forward-compatibility rule).
    #[cfg(feature = "models")]
    #[must_use]
    pub fn resolve(&self) -> rto_graph::ModelPins {
        rto_graph::ModelPins {
            embedding: self.embedding.clone(),
            generative: self.generative.clone(),
            vision: self.vision.clone(),
            audio: self.audio.clone(),
            ocr: self.ocr.clone(),
        }
    }
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
    /// Upper bound, in tokens, on the context window any one request may be
    /// given (issue #486). Unset/`0` — the default — means **each model's own
    /// trained window**, so a request may grow to as much as the model actually
    /// supports and no further.
    ///
    /// This is a *ceiling*, not the window every request gets: contexts are
    /// built per generation and sized to the request, so a short question costs
    /// a short context whatever this is set to. Lower it to bound the KV cache
    /// on a machine with less memory to spare — measured on `qwen3.8-27b`, a
    /// context costs about 64 KiB per token (429 MiB at 4,096 tokens, 16,466 MiB
    /// at its trained 262,144), so this is the key that decides the worst case a
    /// single request can reach.
    ///
    /// **A value, not a capability, under ADR-0007 v1.4** — and by the ADR's own
    /// default rule rather than by assertion. The built-in default already
    /// grants the largest window each model supports, so setting this in a
    /// committed project file cannot cause anything that would not otherwise
    /// happen; it can only ever spend *less* of the machine. It therefore fails
    /// the capability test at its first clause and takes the ordinary
    /// CLI > project > user > default precedence. Had the key been defined the
    /// other way round — a low default that this raises — clause 4 ("spends
    /// materially more of the machine") would have made it a capability, and the
    /// project layer could not have set it at all.
    ///
    /// Above a model's `n_ctx_train` it is **clamped, with a warning**, not
    /// refused: one number is set across models whose trained windows span 512×,
    /// so exceeding the smallest is ordinary rather than an error. A *request*
    /// that does not fit the resulting window is refused as a 400.
    ///
    /// **This is a backstop against caller-influenced prompts, not only a memory
    /// setting.** Where a client may supply its own tool definitions, the prompt
    /// — and therefore the window, and therefore the allocation — is partly an
    /// outside party's input. The bound on an oversized tool surface belongs at
    /// the serving edge, which refuses it with a 400; this key is what decides
    /// the worst case a single request can reach regardless. Raising it raises
    /// that worst case.
    pub max_context_tokens: Option<u32>,
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

/// The `[debt] ignore` exclusion globs that govern `project` within `ws` — read
/// from **that project's own repository**, not from the repo the process was
/// started in.
///
/// This is ADR-0009's per-repo config resolution applied to scanning: *a
/// repository's own configuration governs how it is scanned, whoever is asking.*
/// `roteiro links` already reads each spoke's config with a per-repo
/// [`load`]; a multi-repo server that skips it scans repo B from repo A under
/// A's exclusions, so B's own `[debt] ignore` never applies and B's operators
/// cannot explain the number they are shown.
///
/// Returns no exclusions when the project has no repository on disk to consult —
/// a pre-opened store ([`rto_graph::Workspace::from_stores`], the in-memory
/// tests). Substituting the invoking repo's config there would be precisely the
/// mix-up this function exists to prevent.
///
/// # Errors
/// The project name not resolving in `ws`, or that repository's `roteiro.toml`
/// being unreadable or malformed. Both are surfaced rather than swallowed: a
/// fallback to "no exclusions" answers with a silently different number, which is
/// the defect, not a graceful degradation.
// The two callers are the graph API (`explorer`) and the MCP `debt` tool
// (`serve`); a default build has neither, so gate it or dead-code warns.
#[cfg(any(feature = "explorer", feature = "serve"))]
pub fn debt_ignore_for(
    ws: &rto_graph::Workspace,
    project: Option<&str>,
) -> anyhow::Result<Vec<String>> {
    let Some(root) = ws.project_root(project)? else {
        return Ok(Vec::new());
    };
    let loaded = load(&root).map_err(|e| {
        anyhow::anyhow!(
            "reading the configuration of the repository at {}: {e}",
            root.display()
        )
    })?;
    Ok(loaded.effective.debt.ignore.unwrap_or_default())
}

/// Overlay the `[debt] ignore` **exclusion list**: `over`'s patterns are
/// *appended* to `base`'s (de-duplicated, inherited first), unless `over` sets
/// `ignore_reset`, in which case `base` is dropped entirely.
///
/// Replace is right for a scalar — one `model`, one `addr`, and the nearer layer
/// names it. It is wrong for an exclusion list, where the intent is nearly always
/// additive: a user who globally ignores `vendor/**` and then adds one
/// project-specific `thirdparty/**` wants both, and getting the union silently
/// narrowed to one pattern is a trap that reports debt the user believed excluded
/// — or, worse, hides it.
///
/// Only this list merges. The other list-valued keys stay replace-wins, and that
/// is a decision rather than an oversight:
///
/// - `[workspace]`/`[standalone]` `roots`/`repos` are **discovery** lists: a
///   merge would silently serve repos the project never named, which is a
///   surprise with a security surface, not a convenience.
/// - `[serve] models` is a **selection** ("serve exactly these"), where the
///   nearer layer narrowing the set is the whole point.
/// - `[[links]]`/`[[workspaces]]` are already whole-entry overlays with their own
///   documented rule, and `[pins]` already merges per key.
///
/// The distinguishing question is whether adding an entry *widens* something
/// harmless (an exclusion) or *changes what is reached* (discovery, selection).
fn merge_ignore(base: Option<&[String]>, over: &DebtConfig) -> Option<Vec<String>> {
    if over.ignore_reset == Some(true) {
        return over.ignore.clone();
    }
    match (base, over.ignore.as_deref()) {
        (None, None) => None,
        (Some(only), None) | (None, Some(only)) => Some(only.to_vec()),
        (Some(base), Some(over)) => {
            // Inherited first, then the nearer layer's, each pattern once — the
            // order `roteiro config` prints them in.
            let mut merged: Vec<String> = base.to_vec();
            for pattern in over {
                if !merged.iter().any(|p| p == pattern) {
                    merged.push(pattern.clone());
                }
            }
            Some(merged)
        }
    }
}

impl Config {
    /// Overlay `over` on top of `self`, taking `over`'s value wherever set — used
    /// to apply the project layer on top of the user layer.
    ///
    /// Scalars replace; the `[debt] ignore` exclusion list **merges** (see
    /// [`merge_ignore`]).
    fn overlaid_with(&self, over: &Config) -> Config {
        Config {
            models: self.models.overlaid_with(&over.models),
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
                // Ordinary precedence — a value, not a capability (ADR-0007
                // v1.4): the default already grants each model's full trained
                // window, so this key can only lower it.
                max_context_tokens: over
                    .serve
                    .max_context_tokens
                    .or(self.serve.max_context_tokens),
                tls_cert: over.serve.tls_cert.clone().or(self.serve.tls_cert.clone()),
                tls_key: over.serve.tls_key.clone().or(self.serve.tls_key.clone()),
            },
            // `[debt] ignore` is the one list-valued key that **merges** rather
            // than replaces; see `merge_ignore` for why, and why the other lists
            // deliberately do not.
            debt: DebtConfig {
                ignore: merge_ignore(self.debt.ignore.as_deref(), &over.debt),
                // NOT inherited with `.or(self.debt.ignore_reset)`, unlike every
                // scalar above. Those are *values* the run consumes afterwards, so
                // falling back to the lower layer is right. This is a **directive
                // to the merge**, already consumed by `merge_ignore` — which reads
                // `over`'s flag and only `over`'s. After the overlay it is no
                // longer an input to anything; it is the *record* of what the
                // merge did. Inheriting it made the record disagree with the
                // event: a user-layer reset (inert, since `merge_ignore` never
                // consults it) surfaced in `effective`, and `roteiro config` — the
                // command whose whole job is explaining what the config did —
                // announced "inherited patterns dropped" over a list where every
                // inherited pattern was plainly still present.
                ignore_reset: over.debt.ignore_reset,
            },
            // The two tables in this merge whose grant key does **not** follow
            // the project-over-user rule every line above uses; see
            // [`RemoteConfig::overlaid_with`] (ADR-0019 §3) and
            // [`LintConfig::overlaid_with`] (ADR-0020 §6).
            remote: self.remote.overlaid_with(&over.remote),
            lint: self.lint.overlaid_with(&over.lint),
            // **Per key, project over user** — `[pins]`'s rule, not
            // `[debt] ignore`'s. Each entry is an independent locator for one
            // analyzer, so a project naming an image for `osv-scanner` must not
            // discard a user's entry for `cargo-audit`: the two answer different
            // questions and neither is a narrowing of the other. It is not the
            // merging *exclusion* list either, which is additive because its
            // intent is a union; here the project's entry for the same analyzer
            // replaces the user's outright, because two images for one analyzer
            // is not a union, it is a choice.
            security: SecurityConfig {
                images: {
                    let mut merged = self.security.images.clone();
                    merged.extend(over.security.images.clone());
                    merged
                },
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
    /// - each `[[workspaces]]` entry is a linked group under its own name, with the
    ///   members of everything it `includes` folded in transitively (ADR-0008 v1.3);
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
    /// **Nesting flattens.** A workspace that `includes` others is a workspace with
    /// more members and nothing else — the same flat [`rto_graph::ResolvedWorkspace`]
    /// every surface already consumes, so `links`, `serve`, the explorer and the vault
    /// see a longer member list and no new concept. See [`NamedWorkspace::includes`].
    ///
    /// Fully backward-compatible: a config with only `[workspace]` yields exactly one
    /// `default` linked group with the same membership as before; a config naming no
    /// workspaces at all yields an empty list. A config declaring no `includes`
    /// anywhere is untouched by the fold — [`Self::fold_includes`] appends to a
    /// workspace only what its `includes` reach, and reads (never rewrites) each
    /// entry's own declarations, so such a config resolves exactly as it did before
    /// nesting existed.
    ///
    /// # Errors
    /// A duplicate **linked** workspace name, an `includes` cycle, an `includes` entry
    /// naming no known workspace, or a discovery failure (an unreadable `[standalone]`
    /// root).
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

        // Fold each composed workspace's `includes` in, transitively (ADR-0008 v1.3).
        // After the loop above, so a name can be included before it is declared and
        // so a duplicate name is refused before anything tries to reference it.
        self.fold_includes(&mut out)?;

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

    /// Fold every composed workspace's `includes` into its resolved membership,
    /// transitively (ADR-0008 v1.3 nested workspaces).
    ///
    /// Each `[[workspaces]]` entry that declares `includes` gains, appended after its
    /// own declarations and in `includes` order, every `roots`/`repos` entry of each
    /// included workspace and of everything *those* include. The included workspaces'
    /// own resolved groups are left alone: including a workspace composes it, it does
    /// not consume it.
    ///
    /// **Declarations are read, never rewritten**, so this is a pure append and a
    /// config with no `includes` anywhere is untouched — every loop below is entered
    /// zero times, and the resolved list is exactly what it was before nesting
    /// existed. Members are read from the *declared* tables ([`Self::declared_members`])
    /// rather than from the partially-folded groups in `out`, so the result does not
    /// depend on the order the entries were written in: `a` may include `b` whether
    /// `b` is declared above or below it.
    ///
    /// # Errors
    /// A cycle (naming the path) or an `includes` entry that names no known workspace
    /// (listing the ones that do exist).
    fn fold_includes(&self, out: &mut [rto_graph::ResolvedWorkspace]) -> anyhow::Result<()> {
        for nw in &self.workspaces {
            let includes = nw.includes.as_deref().unwrap_or_default();
            if includes.is_empty() {
                continue;
            }
            // The group pushed for this entry above. Absent only if a future caller
            // folds a partial list; skipping is then the honest no-op.
            let Some(group) = out.iter_mut().find(|r| r.name == nw.name) else {
                continue;
            };
            let mut fold = IncludeFold::starting_at(&nw.name, group);
            for name in includes {
                self.fold_one(name, &mut fold)?;
            }
        }
        Ok(())
    }

    /// Fold the workspace `name` — and, depth-first, everything it includes — into
    /// the group being composed.
    ///
    /// Cycle detection is the include *path* (the chain currently being expanded),
    /// which is also what the error prints; self-inclusion is the degenerate case of
    /// the same check, with a one-hop path. `expanded` is the separate "already
    /// folded in" set that makes a diamond cost one visit rather than two, and makes
    /// a wide graph linear rather than exponential. A name reached a second time is
    /// therefore skipped, not re-appended — but it is skipped only *after* the path
    /// check, so a cycle is still reported rather than silently absorbed.
    ///
    /// # Errors
    /// A cycle, or a name no declared workspace answers to.
    fn fold_one(&self, name: &str, fold: &mut IncludeFold<'_>) -> anyhow::Result<()> {
        if fold.path.iter().any(|step| step == name) {
            anyhow::bail!(
                "workspace include cycle: {} — a `[[workspaces]]` entry cannot include \
                 itself, directly or through the workspaces it includes",
                fold.cycle_path(name)
            );
        }
        let Some(member) = self.declared_members(name) else {
            anyhow::bail!(
                "`includes` in workspace `{}`: no workspace named `{name}` (known: {}) — only \
                 named `[[workspaces]]` (and the legacy `[workspace]`, which folds in as \
                 `default`) can be included; `[standalone]` repos are unnamed, so there is no \
                 name to reference",
                fold.referrer(),
                self.includable_names().join(", ")
            );
        };
        if !fold.expanded.insert(name.to_owned()) {
            // Already folded in by another branch (a diamond), and its subtree with
            // it — so each member lands exactly once.
            return Ok(());
        }
        for root in member.roots {
            fold.push_root(expand_tilde(root).to_string_lossy().into_owned());
        }
        for repo in member.repos {
            fold.push_repo(expand_tilde(repo).to_string_lossy().into_owned());
        }
        fold.path.push(name.to_owned());
        for next in member.includes {
            self.fold_one(next, fold)?;
        }
        fold.path.pop();
        Ok(())
    }

    /// The declared membership of the named workspace `name`, or `None` if no
    /// declaration answers to that name.
    ///
    /// The legacy `[workspace]` answers to `default` — the name it folds in as — when
    /// it names anything, so a composed workspace can include it like any other named
    /// linked workspace. `[standalone]` answers to nothing: it is one unnamed table,
    /// and the per-repo names its members resolve to are derived from directory names
    /// rather than declared, so there is no name in the config to reference.
    fn declared_members(&self, name: &str) -> Option<DeclaredMembers<'_>> {
        const NONE: &[String] = &[];
        if name == "default" && !self.workspace.is_empty() {
            return Some(DeclaredMembers {
                roots: self.workspace.roots.as_deref().unwrap_or(NONE),
                repos: self.workspace.repos.as_deref().unwrap_or(NONE),
                includes: NONE,
            });
        }
        self.workspaces
            .iter()
            .find(|w| w.name == name)
            .map(|w| DeclaredMembers {
                roots: w.roots.as_deref().unwrap_or(NONE),
                repos: w.repos.as_deref().unwrap_or(NONE),
                includes: w.includes.as_deref().unwrap_or(NONE),
            })
    }

    /// Every name an `includes` entry may reference, in resolution order — the
    /// `known:` list an unknown name is refused with. Exactly the names
    /// [`Self::declared_members`] answers to, derived from the same two sources, so
    /// the list cannot come to disagree with what is actually includable.
    fn includable_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = Vec::new();
        if !self.workspace.is_empty() {
            names.push("default");
        }
        names.extend(self.workspaces.iter().map(|w| w.name.as_str()));
        names
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

/// One workspace's declarations as written, as [`Config::declared_members`] reads
/// them: the three lists an `includes` fold needs, borrowed from the config rather
/// than cloned, with an unset key read as an empty list.
struct DeclaredMembers<'a> {
    /// `roots` as declared (before `~` expansion or any scanning).
    roots: &'a [String],
    /// `repos` as declared (before `~` expansion).
    repos: &'a [String],
    /// The workspaces this one includes — the next hop of the fold. Always empty for
    /// the legacy `[workspace]`, which has no `includes` key.
    includes: &'a [String],
}

/// The accumulating state of one composed workspace's `includes` fold
/// ([`Config::fold_one`]): the group being composed, the include path currently
/// being expanded (for cycle detection and for the error message), and the three
/// sets that keep the result exactly-once.
struct IncludeFold<'a> {
    /// The resolved group being composed — appended to, never rewritten.
    group: &'a mut rto_graph::ResolvedWorkspace,
    /// The chain of workspace names currently being expanded, starting with the
    /// composed workspace itself. A name that reappears here closes a cycle.
    path: Vec<String>,
    /// Names already folded in, so a diamond folds each member once and a wide
    /// include graph costs one visit per node rather than one per path to it.
    expanded: std::collections::HashSet<String>,
    /// Expanded root paths already present in `group.roots`.
    seen_roots: std::collections::HashSet<String>,
    /// Expanded repo paths already present in `group.repos`.
    seen_repos: std::collections::HashSet<String>,
}

impl<'a> IncludeFold<'a> {
    /// Begin folding into `group`, the resolved group of the workspace `name`.
    ///
    /// The seen-sets are seeded from what the group already holds — its own declared
    /// `roots`/`repos` — so an included workspace re-listing a path the composing one
    /// already names adds nothing. They are seeded from the *expanded* strings the
    /// group holds, the same form [`Config::fold_one`] pushes, so the two agree on
    /// what "the same path" means: `~/git/api` and the expanded spelling of it are
    /// one member, not two.
    fn starting_at(name: &str, group: &'a mut rto_graph::ResolvedWorkspace) -> Self {
        let seen_roots = group.roots.iter().cloned().collect();
        let seen_repos = group.repos.iter().cloned().collect();
        Self {
            group,
            path: vec![name.to_owned()],
            expanded: std::collections::HashSet::new(),
            seen_roots,
            seen_repos,
        }
    }

    /// Append an expanded root, unless this group already has it.
    fn push_root(&mut self, root: String) {
        if self.seen_roots.insert(root.clone()) {
            self.group.roots.push(root);
        }
    }

    /// Append an expanded repo path, unless this group already has it.
    fn push_repo(&mut self, repo: String) {
        if self.seen_repos.insert(repo.clone()) {
            self.group.repos.push(repo);
        }
    }

    /// The include path closed by reaching `name` again, rendered for the cycle
    /// error: `` `platform` → `backend` → `platform` ``. Naming the path is the whole
    /// point of refusing here — a cycle is a mistake in a file a person wrote, and
    /// the useful answer is which chain of includes closed it.
    fn cycle_path(&self, name: &str) -> String {
        self.path
            .iter()
            .map(String::as_str)
            .chain(std::iter::once(name))
            .map(|step| format!("`{step}`"))
            .collect::<Vec<_>>()
            .join(" → ")
    }

    /// The workspace whose `includes` list is being read right now — the one to blame
    /// for an unknown name. Never empty: the path starts with the composed workspace.
    fn referrer(&self) -> &str {
        self.path.last().map_or("", String::as_str)
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
    let mut config: Config = toml::from_str(&text)
        .map_err(|e| anyhow::anyhow!("parsing config {}: {e}", path.display()))?;
    // The one place a `[models]` value is interpreted. Both provenance layers and
    // the effective config are built from here, so normalising once means the key
    // list, the resolution table, the `--json` echo and the resolver cannot
    // disagree about whether a key is set (see [`ModelsConfig::normalize`]).
    config.models.normalize();
    Ok(config)
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
        Config, LintConfig, NamedWorkspace, WorkspaceConfig, expand_tilde_with,
        find_project_config, load_from,
    };

    /// **The two sides of a `[models]` key must agree about whether it is set.**
    ///
    /// One side reports (`roteiro config`'s key list and its layer labels), the
    /// other decides (`rto_graph`'s resolver). They read the same string through
    /// different code, and they diverged: `[models] audio = "   "` was reported as
    /// set and attributed to a layer while resolution treated it as unset (PR
    /// #379 review). A fix without this assertion only resets the clock — the two
    /// implementations are still separate, and nothing else notices when one
    /// moves.
    ///
    /// So this drives *both* over the same table and asserts they answer
    /// identically: set-or-not, and — when set — the same name, since trimming
    /// changes the value as well as its presence.
    #[cfg(feature = "models")]
    #[test]
    fn a_models_key_is_reported_exactly_as_it_is_resolved() {
        use rto_graph::{ModelSource, ModelTask};

        // `(key, task)` for the three keys whose model is a real registry entry
        // this test can name. `embedding` is deliberately included: its *default*
        // is not a model, but a *pin* on it resolves like any other.
        let keys = [
            ("generative", ModelTask::Draft, "qwen3-8b"),
            ("audio", ModelTask::Transcribe, "voxtral-mini-3b"),
            ("embedding", ModelTask::Embed, "bge-base-en-v1.5"),
        ];
        for (key, task, model) in keys {
            for raw in [
                "",
                "   ",
                "\t\n ",
                model,
                &format!("  {model}  "),
                &format!("{model}\t"),
            ] {
                let mut cfg = super::ModelsConfig::default();
                let slot = match key {
                    "generative" => &mut cfg.generative,
                    "audio" => &mut cfg.audio,
                    _ => &mut cfg.embedding,
                };
                *slot = Some(raw.to_owned());

                let reported = cfg.get(key);
                let resolved = rto_graph::resolve_model_with(task, &cfg.resolve())
                    .expect("every value here is either blank or a valid model");

                assert_eq!(
                    reported.is_some(),
                    resolved.source == ModelSource::Pinned,
                    "{key} = {raw:?}: reported set={:?} but resolved source={:?}",
                    reported.is_some(),
                    resolved.source,
                );
                if let Some(reported) = reported {
                    assert_eq!(
                        Some(reported),
                        resolved.model,
                        "{key} = {raw:?}: reported and resolved names differ",
                    );
                }
            }
        }
    }

    /// The normalisation happens where a layer is **parsed**, so it reaches the
    /// surfaces that never call `get` — the `--json` echo of the effective config
    /// serialises the struct directly, and would otherwise print `"   "` for a key
    /// the resolution table in the same document calls unset.
    #[test]
    fn a_blank_pin_is_dropped_when_the_layer_is_parsed() {
        let dir = std::env::temp_dir().join(format!("roteiro-blank-pin-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("mkdir");
        let project = dir.join("roteiro.toml");
        std::fs::write(
            &project,
            "[models]\naudio = \"   \"\ngenerative = \"  qwen3-8b  \"\n",
        )
        .expect("write");

        let loaded = load_from(None, Some(project)).expect("load");
        // Not `Some("   ")`: the value reached the struct itself, so serde sees
        // the same thing every other surface does.
        assert_eq!(loaded.effective.models.audio, None);
        assert_eq!(loaded.project.models.audio, None);
        // A real name survives, trimmed — the same string the resolver looks up.
        assert_eq!(
            loaded.effective.models.generative.as_deref(),
            Some("qwen3-8b")
        );

        std::fs::remove_dir_all(&dir).ok();
    }

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

    /// `[debt] ignore` is the one list-valued key that **merges** across layers
    /// (issue #321c): a project pattern must not silently discard every global
    /// one, because the user then sees debt they believed excluded — or misses
    /// debt they believed counted — with nothing to indicate why.
    #[test]
    fn debt_ignore_merges_across_layers_instead_of_replacing() {
        let dir = std::env::temp_dir().join(format!("roteiro-cfg-debt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let user = dir.join("config.toml");
        let project = dir.join("roteiro.toml");

        std::fs::write(&user, "[debt]\nignore = [\"vendor/**\", \"target/**\"]\n").expect("user");
        std::fs::write(&project, "[debt]\nignore = [\"thirdparty/**\"]\n").expect("project");
        let loaded = load_from(Some(user.clone()), Some(project.clone())).expect("load");

        // The union, inherited first — NOT just the project's one pattern.
        assert_eq!(
            loaded.effective.debt.ignore.as_deref(),
            Some(
                [
                    "vendor/**".to_owned(),
                    "target/**".to_owned(),
                    "thirdparty/**".to_owned()
                ]
                .as_slice()
            ),
        );

        // Every pattern reports the layer it came from, so the merge is legible.
        assert_eq!(
            loaded.debt_ignore_sources(),
            vec![
                ("vendor/**", "user"),
                ("target/**", "user"),
                ("thirdparty/**", "project"),
            ]
        );
        assert!(loaded.debt_ignore_discarded().is_empty(), "nothing dropped");

        // A pattern named by both layers appears once, tagged with both.
        std::fs::write(
            &project,
            "[debt]\nignore = [\"vendor/**\", \"thirdparty/**\"]\n",
        )
        .expect("project");
        let loaded = load_from(Some(user.clone()), Some(project.clone())).expect("load");
        assert_eq!(
            loaded.effective.debt.ignore.as_deref(),
            Some(
                [
                    "vendor/**".to_owned(),
                    "target/**".to_owned(),
                    "thirdparty/**".to_owned()
                ]
                .as_slice()
            ),
            "a duplicate pattern is not listed twice"
        );
        assert_eq!(
            loaded.debt_ignore_sources()[0],
            ("vendor/**", "project, user")
        );

        // One layer alone still works, in both directions.
        let only_user = load_from(Some(user.clone()), None).expect("load");
        assert_eq!(
            only_user.effective.debt.ignore.as_deref(),
            Some(["vendor/**".to_owned(), "target/**".to_owned()].as_slice())
        );
        let only_project = load_from(None, Some(project.clone())).expect("load");
        assert_eq!(
            only_project.effective.debt.ignore.as_deref(),
            Some(["vendor/**".to_owned(), "thirdparty/**".to_owned()].as_slice())
        );
        // Neither layer ⇒ unset, as before.
        assert!(
            load_from(None, None)
                .expect("load")
                .effective
                .debt
                .ignore
                .is_none()
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The answer to "how do I remove an inherited pattern?": `ignore_reset`
    /// drops the inherited list wholesale, and `roteiro config` can name what it
    /// dropped — an explicit reset rather than a `!pattern` negation that would
    /// fail silently on a typo (see [`super::DebtConfig::ignore_reset`]).
    #[test]
    fn debt_ignore_reset_drops_the_inherited_list_visibly() {
        let dir = std::env::temp_dir().join(format!("roteiro-cfg-reset-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let user = dir.join("config.toml");
        let project = dir.join("roteiro.toml");
        std::fs::write(&user, "[debt]\nignore = [\"vendor/**\", \"target/**\"]\n").expect("user");

        // With the reset, only the project's own patterns remain…
        std::fs::write(
            &project,
            "[debt]\nignore_reset = true\nignore = [\"thirdparty/**\"]\n",
        )
        .expect("project");
        let loaded = load_from(Some(user.clone()), Some(project.clone())).expect("load");
        assert_eq!(
            loaded.effective.debt.ignore.as_deref(),
            Some(["thirdparty/**".to_owned()].as_slice())
        );
        // …and the drop is reportable, not merely effective.
        assert_eq!(
            loaded.debt_ignore_discarded(),
            vec!["vendor/**", "target/**"]
        );

        // The kebab-case spelling is accepted too.
        std::fs::write(
            &project,
            "[debt]\nignore-reset = true\nignore = [\"thirdparty/**\"]\n",
        )
        .expect("project");
        let kebab = load_from(Some(user.clone()), Some(project.clone())).expect("load");
        assert_eq!(kebab.effective.debt.ignore_reset, Some(true));
        assert_eq!(
            kebab.effective.debt.ignore.as_deref(),
            Some(["thirdparty/**".to_owned()].as_slice())
        );

        // A reset with no list of its own excludes nothing at all.
        std::fs::write(&project, "[debt]\nignore_reset = true\n").expect("project");
        let bare = load_from(Some(user.clone()), Some(project.clone())).expect("load");
        assert!(bare.effective.debt.ignore.is_none(), "inherits nothing");
        assert_eq!(bare.debt_ignore_discarded(), vec!["vendor/**", "target/**"]);

        // `ignore_reset = false` is not a reset: the merge stands.
        std::fs::write(
            &project,
            "[debt]\nignore_reset = false\nignore = [\"thirdparty/**\"]\n",
        )
        .expect("project");
        let off = load_from(Some(user), Some(project)).expect("load");
        assert_eq!(
            off.effective.debt.ignore.as_deref(),
            Some(
                [
                    "vendor/**".to_owned(),
                    "target/**".to_owned(),
                    "thirdparty/**".to_owned()
                ]
                .as_slice()
            )
        );
        assert!(off.debt_ignore_discarded().is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A **user-layer** `ignore_reset` must not surface in the effective config
    /// (PR #343 review). It governs nothing — [`super::merge_ignore`] reads only
    /// the nearer (project) layer's flag, and the user layer is the bottom — so
    /// inheriting it made `effective` claim a reset that never happened, and
    /// `roteiro config` announce "inherited patterns dropped" over a list where
    /// every inherited pattern was still present.
    #[test]
    fn an_inert_user_layer_reset_does_not_claim_a_reset_that_never_happened() {
        let dir = std::env::temp_dir().join(format!("roteiro-cfg-inert-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let user = dir.join("config.toml");
        let project = dir.join("roteiro.toml");

        // The scenario: the user asks for a reset, the project does not.
        std::fs::write(
            &user,
            "[debt]\nignore_reset = true\nignore = [\"vendor/**\", \"target/**\"]\n",
        )
        .expect("user");
        std::fs::write(&project, "[debt]\nignore = [\"thirdparty/**\"]\n").expect("project");
        let loaded = load_from(Some(user.clone()), Some(project.clone())).expect("load");

        // The effective flag records the merge that ran, which reset nothing.
        assert_ne!(
            loaded.effective.debt.ignore_reset,
            Some(true),
            "a user-layer reset governs nothing, so it must not appear as one in \
             the effective config — that is the claim `roteiro config` prints"
        );
        // The inert request is still *visible*, since a silent no-op is exactly
        // what this key was introduced to avoid.
        assert!(
            loaded.debt_ignore_reset_was_inert(),
            "the user asked for a reset that did nothing; that must be reportable"
        );

        // Nothing was discarded, and nothing claims to have been. (This list could
        // not have caught the defect on its own: with no reset in force the kept
        // list is the union, so it is a superset of the user's and filters empty.)
        assert!(
            loaded.debt_ignore_discarded().is_empty(),
            "no pattern was dropped, so none may be reported as dropped: {:?}",
            loaded.debt_ignore_discarded()
        );

        // And the inert flag did not accidentally start *resetting* anything:
        // both layers' patterns survive, in merge order.
        assert_eq!(
            loaded.effective.debt.ignore.as_deref(),
            Some(
                [
                    "vendor/**".to_owned(),
                    "target/**".to_owned(),
                    "thirdparty/**".to_owned()
                ]
                .as_slice()
            ),
            "the merge is unaffected — this was only ever a reporting defect"
        );

        // The project layer's reset is unaffected by the change: it governs, so it
        // is reported, and the user's flag is not what put it there.
        std::fs::write(
            &project,
            "[debt]\nignore_reset = true\nignore = [\"thirdparty/**\"]\n",
        )
        .expect("project");
        let both = load_from(Some(user.clone()), Some(project)).expect("load");
        assert_eq!(both.effective.debt.ignore_reset, Some(true));
        assert_eq!(both.debt_ignore_discarded(), vec!["vendor/**", "target/**"]);
        assert!(
            !both.debt_ignore_reset_was_inert(),
            "a reset really happened, so `roteiro config` reports the drop rather \
             than a second note saying the user's own flag was individually inert"
        );

        // With no project config at all, the user's reset is still inert.
        let alone = load_from(Some(user), None).expect("load");
        assert_ne!(alone.effective.debt.ignore_reset, Some(true));
        assert!(alone.debt_ignore_reset_was_inert());
        assert_eq!(
            alone.effective.debt.ignore.as_deref(),
            Some(["vendor/**".to_owned(), "target/**".to_owned()].as_slice()),
            "and it resets none of the user's own patterns"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Merging is scoped to the **exclusion** list. Discovery and selection lists
    /// stay replace-wins, so a global `[workspace] roots` cannot silently add
    /// repos a project never named.
    #[test]
    fn other_list_keys_still_replace_rather_than_merge() {
        let dir = std::env::temp_dir().join(format!("roteiro-cfg-lists-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let user = dir.join("config.toml");
        let project = dir.join("roteiro.toml");
        std::fs::write(
            &user,
            "[workspace]\nroots = [\"/u/one\"]\n[serve]\nmodels = [\"a\"]\n",
        )
        .expect("user");
        std::fs::write(
            &project,
            "[workspace]\nroots = [\"/p/two\"]\n[serve]\nmodels = [\"b\"]\n",
        )
        .expect("project");
        let loaded = load_from(Some(user), Some(project)).expect("load");
        assert_eq!(
            loaded.effective.workspace.roots.as_deref(),
            Some(["/p/two".to_owned()].as_slice()),
            "discovery roots replace"
        );
        assert_eq!(
            loaded.effective.serve.models.as_deref(),
            Some(["b".to_owned()].as_slice()),
            "model selection replaces"
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
                    includes: None,
                },
                NamedWorkspace {
                    name: "web".to_owned(),
                    roots: None,
                    repos: Some(vec!["/web/app".to_owned()]),
                    includes: None,
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

    /// The ADR-0008 v1.3 example config, verbatim: `includes` parses off the wire as
    /// a sibling of `roots`/`repos`, and the workspace it composes resolves to the
    /// union of its own repos and its members'.
    ///
    /// The end-to-end case — every other `includes` test builds the `Config` by hand,
    /// which would pass just as well if the key were spelled differently in TOML or
    /// were not read at all.
    #[test]
    fn the_adr_example_parses_and_composes() {
        let cfg: Config = toml::from_str(
            "[[workspaces]]\nname = \"backend\"\nrepos = [\"~/git/api\", \"~/git/worker\"]\n\n\
             [[workspaces]]\nname = \"frontend\"\nrepos = [\"~/git/web\"]\n\n\
             [[workspaces]]\nname = \"platform\"\nincludes = [\"backend\", \"frontend\"]\n\
             repos = [\"~/git/shared-infra\"]\n",
        )
        .expect("parse");
        assert_eq!(
            cfg.workspaces[2].includes.as_deref(),
            Some(&["backend".to_owned(), "frontend".to_owned()][..]),
            "`includes` must be read from the config file, not merely constructible"
        );
        assert_eq!(
            cfg.workspaces[0].includes, None,
            "an entry that declares no `includes` reads as unset"
        );

        let resolved = cfg.resolved_workspaces().expect("resolve");
        let expect = |p: &str| super::expand_tilde(p).to_string_lossy().into_owned();
        assert_eq!(
            members(&resolved, "platform").1,
            vec![
                expect("~/git/shared-infra"),
                expect("~/git/api"),
                expect("~/git/worker"),
                expect("~/git/web"),
            ],
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
                    includes: None,
                },
                NamedWorkspace {
                    name: "api".to_owned(),
                    roots: Some(vec!["/b".to_owned()]),
                    repos: None,
                    includes: None,
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
                includes: None,
            }],
            ..Default::default()
        };
        let err = cfg
            .resolved_workspaces()
            .expect_err("[[workspaces]] named `default` must collide with the legacy fold-in")
            .to_string();
        assert!(err.contains("default"), "{err}");
    }

    /// A `[[workspaces]]` entry with `roots`/`repos` and nothing else — the shape
    /// every existing config has.
    fn ws(name: &str, roots: &[&str], repos: &[&str]) -> NamedWorkspace {
        NamedWorkspace {
            name: name.to_owned(),
            roots: (!roots.is_empty()).then(|| roots.iter().map(|r| (*r).to_owned()).collect()),
            repos: (!repos.is_empty()).then(|| repos.iter().map(|r| (*r).to_owned()).collect()),
            includes: None,
        }
    }

    /// The same, composed: `includes` on top of its own declarations.
    fn composed(name: &str, includes: &[&str], roots: &[&str], repos: &[&str]) -> NamedWorkspace {
        NamedWorkspace {
            includes: Some(includes.iter().map(|i| (*i).to_owned()).collect()),
            ..ws(name, roots, repos)
        }
    }

    /// One resolved group's member lists, for comparing a fold against an expectation.
    fn members(
        resolved: &[rto_graph::ResolvedWorkspace],
        name: &str,
    ) -> (Vec<String>, Vec<String>) {
        let g = resolved
            .iter()
            .find(|r| r.name == name)
            .unwrap_or_else(|| panic!("no resolved workspace `{name}` in {resolved:?}"));
        (g.roots.clone(), g.repos.clone())
    }

    /// **The compatibility case, and the one that matters most**: a config that
    /// declares no `includes` anywhere resolves to exactly what it resolved to before
    /// nesting existed — every existing config is this case.
    ///
    /// Asserted against the whole resolved list written out literally, not against a
    /// property of it, so a fold that appended, reordered or de-duplicated anything
    /// fails here. (`[standalone]` is left out only because its expansion touches the
    /// filesystem; the other tests cover it.)
    #[test]
    fn config_without_includes_resolves_exactly_as_before() {
        let cfg = Config {
            workspace: WorkspaceConfig {
                roots: Some(vec!["/legacy/root".to_owned()]),
                repos: Some(vec!["/legacy/repo".to_owned()]),
            },
            workspaces: vec![
                ws("api", &["/api/root"], &["/api/one", "/api/two"]),
                // A repo listed twice in one entry, and a repo also named by another
                // workspace: neither is de-duplicated today, and the fold must not
                // start de-duplicating them.
                ws("web", &[], &["/web/app", "/web/app", "/api/one"]),
            ],
            ..Default::default()
        };

        let expected = vec![
            rto_graph::ResolvedWorkspace {
                name: "default".to_owned(),
                roots: vec!["/legacy/root".to_owned()],
                repos: vec!["/legacy/repo".to_owned()],
                linked: true,
            },
            rto_graph::ResolvedWorkspace {
                name: "api".to_owned(),
                roots: vec!["/api/root".to_owned()],
                repos: vec!["/api/one".to_owned(), "/api/two".to_owned()],
                linked: true,
            },
            rto_graph::ResolvedWorkspace {
                name: "web".to_owned(),
                roots: Vec::new(),
                repos: vec![
                    "/web/app".to_owned(),
                    "/web/app".to_owned(),
                    "/api/one".to_owned(),
                ],
                linked: true,
            },
        ];
        assert_eq!(
            cfg.resolved_workspaces().expect("resolve"),
            expected,
            "a config with no `includes` must resolve exactly as it did before nesting"
        );
    }

    /// A two-level compose resolves to the **union**: the composing workspace's own
    /// declarations first, then each included workspace's, in `includes` order — and
    /// the included workspaces are unchanged, because including a workspace composes
    /// it rather than consuming it.
    #[test]
    fn includes_resolve_to_the_union_of_members() {
        let cfg = Config {
            workspaces: vec![
                ws("backend", &["/backend/root"], &["/api", "/worker"]),
                ws("frontend", &[], &["/web"]),
                composed(
                    "platform",
                    &["backend", "frontend"],
                    &[],
                    &["/shared-infra"],
                ),
            ],
            ..Default::default()
        };
        let resolved = cfg.resolved_workspaces().expect("resolve");

        assert_eq!(
            members(&resolved, "platform"),
            (
                vec!["/backend/root".to_owned()],
                vec![
                    "/shared-infra".to_owned(),
                    "/api".to_owned(),
                    "/worker".to_owned(),
                    "/web".to_owned(),
                ]
            ),
            "own declarations first, then each included workspace's, in `includes` order"
        );
        assert_eq!(
            members(&resolved, "backend"),
            (
                vec!["/backend/root".to_owned()],
                vec!["/api".to_owned(), "/worker".to_owned()]
            ),
            "an included workspace is composed, not consumed"
        );
        assert_eq!(members(&resolved, "frontend").1, vec!["/web".to_owned()]);
        assert!(
            resolved.iter().all(|r| r.linked),
            "a composed workspace is an ordinary linked group: {resolved:?}"
        );
    }

    /// Three levels fold transitively — `everything` includes `platform`, which
    /// includes `backend` — and the depth is not a special case: the fold walks
    /// declarations, so it does not matter that `platform` is itself composed.
    #[test]
    fn includes_fold_transitively_through_three_levels() {
        let cfg = Config {
            workspaces: vec![
                composed("everything", &["platform"], &[], &["/top"]),
                composed("platform", &["backend"], &[], &["/shared-infra"]),
                ws("backend", &[], &["/api"]),
            ],
            ..Default::default()
        };
        let resolved = cfg.resolved_workspaces().expect("resolve");

        assert_eq!(
            members(&resolved, "everything").1,
            vec![
                "/top".to_owned(),
                "/shared-infra".to_owned(),
                "/api".to_owned()
            ],
            "a member three levels down folds in"
        );
        // Declared above the workspaces it includes, and resolved just the same.
        assert_eq!(
            members(&resolved, "platform").1,
            vec!["/shared-infra".to_owned(), "/api".to_owned()]
        );
    }

    /// A diamond — `top` includes `left` and `right`, both of which include `base` —
    /// yields each member **once**, and in first-reached order.
    ///
    /// This needs no special handling beyond folding each name once: the fold appends
    /// a member only if the composed group does not already have it, keyed by the
    /// expanded path string.
    #[test]
    fn includes_diamond_yields_each_member_once() {
        let cfg = Config {
            workspaces: vec![
                composed("top", &["left", "right"], &[], &["/top"]),
                composed("left", &["base"], &[], &["/left"]),
                composed("right", &["base"], &[], &["/right"]),
                ws("base", &["/base/root"], &["/base"]),
            ],
            ..Default::default()
        };
        let resolved = cfg.resolved_workspaces().expect("resolve");

        assert_eq!(
            members(&resolved, "top"),
            (
                vec!["/base/root".to_owned()],
                vec![
                    "/top".to_owned(),
                    "/left".to_owned(),
                    "/base".to_owned(),
                    "/right".to_owned(),
                ]
            ),
            "the shared member appears once, at the point it was first reached"
        );
    }

    /// The same repo declared under two spellings — a literal path and the `~` form
    /// of it — folds in **once**: the fold compares members after `~` expansion, the
    /// same normalisation resolution already applies on the way in, so the dedupe is
    /// by resolved path rather than by the string somebody happened to type.
    #[test]
    fn includes_dedupe_by_expanded_path_not_by_declared_string() {
        let Some(home) = super::home_dir() else {
            return; // No `HOME`: `~` does not expand, so there is nothing to compare.
        };
        let literal = home.join("git/api").to_string_lossy().into_owned();
        let cfg = Config {
            workspaces: vec![
                composed("platform", &["backend"], &[], &[&literal]),
                ws("backend", &[], &["~/git/api"]),
            ],
            ..Default::default()
        };
        let resolved = cfg.resolved_workspaces().expect("resolve");
        assert_eq!(
            members(&resolved, "platform").1,
            vec![literal],
            "`~/git/api` and its expansion are one member, not two"
        );
    }

    /// A cycle is a config error **naming the path** — never a silent flatten and
    /// never a hang.
    #[test]
    fn includes_cycle_is_refused_naming_the_path() {
        let cfg = Config {
            workspaces: vec![
                composed("a", &["b"], &[], &["/a"]),
                composed("b", &["c"], &[], &["/b"]),
                composed("c", &["a"], &[], &["/c"]),
            ],
            ..Default::default()
        };
        let err = cfg
            .resolved_workspaces()
            .expect_err("a cycle must be refused")
            .to_string();
        assert!(err.contains("workspace include cycle"), "{err}");
        assert!(
            err.contains("`a` → `b` → `c` → `a`"),
            "the error must name the path that closed the cycle; was: {err}"
        );
    }

    /// Self-inclusion is the degenerate cycle, and is refused by the same check with
    /// a one-hop path.
    #[test]
    fn includes_self_reference_is_refused() {
        let cfg = Config {
            workspaces: vec![composed("solo", &["solo"], &[], &["/solo"])],
            ..Default::default()
        };
        let err = cfg
            .resolved_workspaces()
            .expect_err("self-inclusion must be refused")
            .to_string();
        assert!(err.contains("workspace include cycle"), "{err}");
        assert!(err.contains("`solo` → `solo`"), "{err}");
    }

    /// An unknown name in `includes` is refused **listing the workspaces that do
    /// exist**, in the same phrasing `--workspace-name` refuses with, plus which
    /// workspace referred to it.
    #[test]
    fn unknown_include_is_refused_listing_the_known_workspaces() {
        let cfg = Config {
            workspaces: vec![
                ws("one", &[], &["/one"]),
                ws("two", &[], &["/two"]),
                composed("gamma", &["nope"], &[], &["/gamma"]),
            ],
            ..Default::default()
        };
        let err = cfg
            .resolved_workspaces()
            .expect_err("an unknown include must be refused")
            .to_string();
        assert!(
            err.contains("no workspace named `nope` (known: one, two, gamma)"),
            "the refusal must match how `--workspace-name` refuses; was: {err}"
        );
        assert!(
            err.contains("`gamma`"),
            "and name the workspace that referred to it; was: {err}"
        );
    }

    /// A `[standalone]` repo **cannot be included**: the table is unnamed, so the
    /// per-repo names its members resolve to are not names anything in the config can
    /// reference. The refusal is the ordinary unknown-name one, whose tail says why.
    #[test]
    fn standalone_repos_cannot_be_included() {
        let cfg = Config {
            workspaces: vec![composed("platform", &["docs"], &[], &["/shared"])],
            standalone: WorkspaceConfig {
                roots: None,
                // Resolves to a standalone workspace *named* `docs` — and still not
                // includable, which is the point.
                repos: Some(vec!["/solo/docs".to_owned()]),
            },
            ..Default::default()
        };
        let err = cfg
            .resolved_workspaces()
            .expect_err("a `[standalone]` repo must not be includable")
            .to_string();
        assert!(err.contains("no workspace named `docs`"), "{err}");
        assert!(
            err.contains("`[standalone]` repos are unnamed"),
            "the refusal must say why a standalone repo is not includable; was: {err}"
        );
    }

    /// The legacy `[workspace]` is includable under the name it folds in as
    /// (`default`) — it is a named linked workspace like any other once resolved, and
    /// `[standalone]` is excluded for being unnamed, not for being legacy.
    #[test]
    fn legacy_workspace_is_includable_as_default() {
        let cfg = Config {
            workspace: WorkspaceConfig {
                roots: None,
                repos: Some(vec!["/legacy".to_owned()]),
            },
            workspaces: vec![composed("platform", &["default"], &[], &["/shared"])],
            ..Default::default()
        };
        let resolved = cfg.resolved_workspaces().expect("resolve");
        assert_eq!(
            members(&resolved, "platform").1,
            vec!["/shared".to_owned(), "/legacy".to_owned()]
        );
        assert_eq!(
            members(&resolved, "default").1,
            vec!["/legacy".to_owned()],
            "including the legacy table composes it, it does not consume it"
        );
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
                includes: None,
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
    /// The `[lint]` merge must apply ADR-0020 §6's inversion, and must apply it
    /// by **delegating** rather than by re-deriving it: `rto_exec` owns the rule
    /// and this file owns the keys.
    ///
    /// Its counterpart in `main.rs` pins the *gate* to the same type, so the
    /// value this report echoes and the value that decides a run are the same
    /// value without either test needing the other's internals.
    #[cfg(feature = "exec-subprocess")]
    #[test]
    fn the_lint_merge_delegates_the_inversion_rather_than_repeating_it() {
        for project in [None, Some(true), Some(false)] {
            for user in [None, Some(true), Some(false)] {
                let merged = LintConfig {
                    allow_unsandboxed: user,
                    ..LintConfig::default()
                }
                .overlaid_with(&LintConfig {
                    allow_unsandboxed: project,
                    ..LintConfig::default()
                });
                assert_eq!(
                    merged.allow_unsandboxed,
                    rto_exec::LintConfigGrant::from_layers(project, user).as_effective(),
                    "project={project:?} user={user:?}"
                );
            }
        }
        // Spelled out at the two cells that matter, so a reader sees the rule
        // and not only that two functions agree about it.
        let project_grant = LintConfig::default().overlaid_with(&LintConfig {
            allow_unsandboxed: Some(true),
            ..LintConfig::default()
        });
        assert_eq!(
            project_grant.allow_unsandboxed, None,
            "a committed grant must not become effective"
        );
        let project_deny = LintConfig {
            allow_unsandboxed: Some(true),
            ..LintConfig::default()
        }
        .overlaid_with(&LintConfig {
            allow_unsandboxed: Some(false),
            ..LintConfig::default()
        });
        assert_eq!(
            project_deny.allow_unsandboxed,
            Some(false),
            "a committed denial must override a user grant"
        );
    }

    /// `[lint] image` follows the **ordinary** precedence, and this is the test
    /// that stops it being "fixed" to match its neighbour.
    ///
    /// The two keys sit in one table under two rules, which is unusual enough to
    /// be worth pinning: `allow_unsandboxed` is a *permission* and inverts, so a
    /// committed file may deny and never grant; `image` is a *locator* and does
    /// not, so a project may choose where its team's boundary comes from — which
    /// is `[remote] endpoint`'s rule beside `[remote] enabled`'s.
    #[test]
    fn the_lint_image_takes_ordinary_precedence_unlike_the_key_beside_it() {
        let image = |user: Option<&str>, project: Option<&str>| {
            LintConfig {
                image: user.map(str::to_owned),
                ..LintConfig::default()
            }
            .overlaid_with(&LintConfig {
                image: project.map(str::to_owned),
                ..LintConfig::default()
            })
            .image
        };
        assert_eq!(
            image(Some("user@sha256:a"), Some("project@sha256:b")),
            Some("project@sha256:b".to_owned()),
            "a project may choose where its team's boundary comes from"
        );
        assert_eq!(
            image(Some("user@sha256:a"), None),
            Some("user@sha256:a".to_owned())
        );
        assert_eq!(
            image(None, Some("project@sha256:b")),
            Some("project@sha256:b".to_owned())
        );
        assert_eq!(image(None, None), None);

        // And the contrast, in one assertion, because the point is the
        // *difference*: the same project layer that supplies an image cannot
        // supply a grant.
        let both = LintConfig {
            allow_unsandboxed: None,
            image: None,
        }
        .overlaid_with(&LintConfig {
            allow_unsandboxed: Some(true),
            image: Some("project@sha256:b".to_owned()),
        });
        assert_eq!(both.image, Some("project@sha256:b".to_owned()));
        assert_eq!(
            both.allow_unsandboxed, None,
            "the locator carries from the project layer; the permission does not"
        );
    }

    /// `[security.images]` is a **map**, and it layers per entry — `[pins]`'s
    /// rule, not `[lint] image`'s scalar one and not `[debt] ignore`'s merge.
    ///
    /// The property that matters is the third assertion: a project naming an
    /// image for one analyzer must not silently un-declare a user's image for a
    /// *different* one. They answer different questions, and neither is a
    /// narrowing of the other — so a table-level replace would leave someone's
    /// `cargo-audit` sandbox quietly gone the day a teammate committed an
    /// `osv-scanner` entry.
    #[test]
    fn security_images_layer_per_analyzer_rather_than_per_table() {
        let dir = std::env::temp_dir().join(format!("roteiro-cfg-sec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let user = dir.join("config.toml");
        let project = dir.join("roteiro.toml");

        let hex = "a".repeat(64);
        std::fs::write(
            &user,
            format!(
                "[security.images]\nsemgrep = \"mine/semgrep@sha256:{hex}\"\n\
                 cargo-audit = \"mine/audit@sha256:{hex}\"\n"
            ),
        )
        .expect("write");
        std::fs::write(
            &project,
            format!("[security.images]\nsemgrep = \"ours/semgrep@sha256:{hex}\"\n"),
        )
        .expect("write");

        let loaded = load_from(Some(user), Some(project)).expect("load");
        let images = &loaded.effective.security.images;
        assert_eq!(
            images.get("semgrep").map(String::as_str),
            Some(format!("ours/semgrep@sha256:{hex}").as_str()),
            "project over user, per analyzer"
        );
        assert_eq!(
            images.get("cargo-audit").map(String::as_str),
            Some(format!("mine/audit@sha256:{hex}").as_str()),
            "a project entry for one analyzer must not discard a user entry for another"
        );
        assert_eq!(images.len(), 2);
        assert_eq!(
            loaded.effective.security.image_for("cargo-audit"),
            Some(format!("mine/audit@sha256:{hex}").as_str())
        );
        assert_eq!(loaded.effective.security.image_for("osv-scanner"), None);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// With no `[security]` table at all, nothing changes — which is the
    /// forward-compatibility rule and also the whole promise of the feature: the
    /// built-in pins are what `security run` uses, exactly as before.
    #[test]
    fn no_security_table_declares_nothing_and_reports_nothing() {
        let empty = Config::default();
        assert!(empty.security.images.is_empty());
        assert!(empty.security.image_for("semgrep").is_none());
        assert!(
            empty.security.problems().is_empty(),
            "an absent table has nothing wrong with it"
        );
    }

    /// A bad entry is **reported, never a parse failure** — ADR-0007 v1.3's rule
    /// that `roteiro config` is the command an operator runs *because* a key is
    /// misbehaving, so it must not be the one command that key stops.
    ///
    /// The refusal itself lives at every consuming site (`security run`,
    /// `prefetch`, `status`), which is what stops this being a softening: the tag
    /// never reaches a guest, it just does not take `roteiro config` down on the
    /// way.
    #[cfg(feature = "execution")]
    #[test]
    fn a_bad_security_image_is_reported_rather_than_refused_at_load() {
        let dir = std::env::temp_dir().join(format!("roteiro-cfg-secbad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let project = dir.join("roteiro.toml");
        std::fs::write(
            &project,
            "[security.images]\nsemgrep = \"registry.example/you/semgrep:1.173.0\"\n\
             my-favourite-linter = \"registry.example/you/lint@sha256:\"\n",
        )
        .expect("write");

        let loaded = load_from(None, Some(project)).expect("a bad value must still load");
        let problems = loaded.effective.security.problems();
        assert_eq!(problems.len(), 2, "{problems:?}");

        // Matched on the *key* rather than on the analyzer name, because the
        // adapterless message lists every readable analyzer — `semgrep` among
        // them — so "contains semgrep" finds the wrong sentence. A test that
        // reads a refusal has to select it the way a reader would.
        let tag = problems
            .iter()
            .find(|p| p.contains("`[security.images] semgrep`"))
            .expect("the tag is reported");
        assert!(tag.contains("`[security.images] semgrep`"), "{tag}");
        assert!(tag.contains("tag rather than a digest"), "{tag}");
        assert!(
            tag.contains("registry.example/you/semgrep:1.173.0"),
            "{tag}"
        );

        let adapter = problems
            .iter()
            .find(|p| p.starts_with("`my-favourite-linter`"))
            .expect("the adapterless analyzer is reported");
        assert!(adapter.contains("adapter"), "{adapter}");
        assert!(
            !adapter.contains("tag rather than a digest"),
            "the adapter is the reason, and it is reported instead of the pin rather than \
             after it — fixing the digest would not have helped: {adapter}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}

//! Per-analyzer adapters: native analyzer output in, a [`NormalizedReport`] out.
//!
//! An adapter is the **only** analyzer-specific code in this crate. It knows one
//! tool's native JSON, how to name that tool's findings so they are recognisable
//! across runs, and which argv produces that JSON. Everything downstream — the
//! validation, the identity keys, the ordering, the store — is shared.
//!
//! # Why this is the seam, and not the runner
//!
//! ADR-0012 requires that "a finding is the same artifact whether it was produced
//! locally in a sandbox or ingested from a CI report". The cheap way to satisfy
//! that is to write the conversion twice and add a test comparing the two. This
//! crate does the other thing: **there is one conversion**, and both paths call
//! it. A subprocess run captures the analyzer's stdout and hands those bytes to
//! the adapter; `roteiro security ingest` reads a file of the same native bytes
//! and hands them to the same adapter. Equality of the resulting [`Finding`]s is
//! therefore a property of the code, and the tests that assert it are guarding
//! against a future refactor rather than establishing the invariant.
//!
//! [`Finding`]: rto_graph::Finding
//!
//! # Adding an analyzer needs no migration
//!
//! [`rto_graph::FindingKey`] is `finding:<analyzer>:<that analyzer's own ordered
//! identity components>`. An adapter chooses the recipe; the schema never learns
//! what the components mean. So a new analyzer is a new file in `adapters/`, an
//! entry in [`ADAPTERS`], and nothing else — no schema change, no migration.
//!
//! @rto:0012
//! @rto:0014
//! @rto:0018

use rto_graph::SourceIdentity;

use crate::guidance::{Guidance, Line};
use crate::ingest::NormalizedReport;
use crate::runner::ExecError;
use crate::snippet::SnippetSource;

pub mod cargo_audit;
pub mod clippy;
pub mod osv_scanner;
pub mod semgrep;

/// Everything an adapter may need that is *not* in the analyzer's own output.
///
/// Native analyzer output is missing things the evidence chain requires — no
/// mainstream analyzer stamps its report with the wall-clock window it ran in,
/// and `cargo audit` does not even record its own version. Rather than let an
/// adapter invent them, the caller supplies what it actually knows, and an
/// adapter that has nothing better says so ([`UNKNOWN_VERSION`]).
#[derive(Clone)]
pub struct NativeContext<'a> {
    /// When the run started, RFC 3339 UTC. A subprocess run measures it; an
    /// ingest of a report file uses the file's modification time, which is the
    /// only timestamp evidence a bare report carries.
    pub started_at: String,
    /// When the run ended, RFC 3339 UTC.
    pub ended_at: String,
    /// The analyzer's version, where the caller learned it out of band (a
    /// subprocess run asks the binary). `None` leaves the adapter to use
    /// whatever the report itself carries.
    pub analyzer_version: Option<String>,
    /// The analyzer's process exit status, where the caller observed it.
    pub exit_status: i32,
    /// The source identity the run was against. Some identity recipes need it —
    /// `cargo-audit` keys findings by lockfile blob, so a finding stays distinct
    /// when the lockfile changes underneath the same advisory.
    pub source: &'a SourceIdentity,
    /// Digest of the rule set the analyzer ran with, where one applies.
    pub rules_digest: Option<String>,
    /// The pinned advisory database the caller provisioned, where one applies.
    ///
    /// A fallback, not an override: an adapter prefers what the analyzer's own
    /// report says about the database it consulted, and uses this only when the
    /// report says nothing. `cargo audit` says nothing whenever it is pointed at
    /// a database with `--db`, which is every pinned run — so without this, the
    /// reproducible configuration would be the one with no staleness evidence.
    pub advisory_db: Option<rto_graph::AdvisoryDb>,
    /// The checkout the report describes, where the caller knows it.
    ///
    /// Only an adapter whose analyzer reports **absolute** paths needs this, and
    /// `osv-scanner` is that adapter: it returns a full filesystem path for every
    /// manifest even when it is told to scan `.`. Without the worktree there is
    /// nothing to relativise against, so an absolute path would be stored
    /// verbatim — user-identifying data in a persisted finding key, and a key
    /// that differs between two machines running the identical scan.
    ///
    /// `None` is the honest answer for a report about a tree this checkout does
    /// not have; an adapter must then say the location is unknown rather than
    /// guess at one.
    pub worktree: Option<&'a std::path::Path>,
    /// Where to read the source a finding points at, for identity recipes that
    /// include a snippet hash.
    ///
    /// It is here rather than inside an adapter because the *caller* knows which
    /// checkout the report describes, and because both execution paths must read
    /// the same one — that is what makes a subprocess run and an ingest of its
    /// output produce identical finding keys.
    pub snippets: &'a dyn SnippetSource,
}

// Hand-written because `&dyn SnippetSource` is not `Debug` and does not need to
// be: what a debug print of a context should show is the evidence it carries,
// not the identity of the thing that reads files.
impl std::fmt::Debug for NativeContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeContext")
            .field("started_at", &self.started_at)
            .field("ended_at", &self.ended_at)
            .field("analyzer_version", &self.analyzer_version)
            .field("exit_status", &self.exit_status)
            .field("source", self.source)
            .field("rules_digest", &self.rules_digest)
            .field("advisory_db", &self.advisory_db)
            .field("worktree", &self.worktree)
            .finish_non_exhaustive()
    }
}

impl NativeContext<'_> {
    /// The version to record: what the caller learned, else what the report
    /// carried, else [`UNKNOWN_VERSION`].
    ///
    /// Never empty — [`crate::IngestRunner`] refuses a report that cannot say
    /// what version produced it, and "unknown" is a truthful answer where an
    /// empty string is a missing one.
    #[must_use]
    pub fn version_or(&self, from_report: Option<&str>) -> String {
        self.analyzer_version
            .as_deref()
            .or(from_report)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or(UNKNOWN_VERSION)
            .to_owned()
    }
}

/// Recorded as an analyzer's version when neither the caller nor the report
/// knows it — which is the ordinary case for a `cargo audit` report ingested
/// from CI, since its JSON has no version field.
pub const UNKNOWN_VERSION: &str = "unknown";

/// How an analyzer is invoked as a child process.
///
/// Returned by [`Adapter::command`] and consumed by the subprocess runner, so
/// the argv lives beside the parser that understands its output rather than in
/// the runner, which knows no analyzer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// The program to execute, looked up on `PATH` unless it is a path.
    pub program: String,
    /// Its arguments, in order.
    pub args: Vec<String>,
    /// Exit statuses that mean "the analyzer ran and produced a report".
    ///
    /// Analyzers overload the exit status: `semgrep` exits `1` when it found
    /// something, `cargo audit` exits `1` on a vulnerability. Treating non-zero
    /// as failure would discard exactly the runs that matter, so each adapter
    /// declares which statuses carry a usable report and every other status is a
    /// hard failure.
    pub success_statuses: Vec<i32>,
}

/// How to obtain one program from [`Adapter::host_programs`], for the refusal
/// that discovers it is absent.
///
/// The counterpart of the refusal rule in `docs/REVIEW_CHECKLIST.md`: a refusal
/// names the way forward, and *this* is the way forward for the one obstacle
/// Roteiro will never clear on the reader's behalf. It is keyed **by program and
/// not by analyzer** because a single analyzer's programs are obtained
/// differently — `cargo-audit` needs `cargo` from rustup *and* `cargo-audit`
/// from crates.io, and one hint covering both would be right about at most one
/// of them. That is the "right *kind* of way forward" check, which is the one
/// that has shipped wrong here before.
///
/// # What may go in one, and what may not
///
/// - **The command is upstream's, verbatim, or there is none.** Every command
///   below was read off the tool's own install page at the time it was written,
///   not recalled. Where upstream documents no single command — `osv-scanner`
///   offers eight platform-specific ones and ranks none — the hint says so and
///   gives the page. A plausible command that fails is worse than a URL.
/// - **Never the reader's package manager.** No `brew`, no `apt`, unless
///   upstream itself names one as *the* way. A canonical ecosystem command is
///   portable and checkable; a package-manager guess is wrong for most readers.
/// - **Always the upstream page.** A URL ages better than a command line, so
///   even a hint with a good command carries the page that would correct it.
/// - **Saying how is not doing it.** Nothing reads a hint and runs it. Roteiro
///   installs no analyzer (ADR-0014), and a refusal that quietly installed one
///   is the silent downgrade ADR-0019 §6 and ADR-0020 §6 forbid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstallHint {
    /// The program this obtains — an entry in the same adapter's
    /// [`Adapter::host_programs`], which is what
    /// `tests::every_host_program_has_an_install_hint` pairs them by.
    pub program: &'static str,
    /// What to tell the reader, rendered by [`Guidance`] so a message built from
    /// it cannot lose its own indentation. See [`crate::guidance`].
    pub guidance: Guidance,
}

/// Obtaining `cargo` or `rustc`: the toolchain itself, not a tool installed with
/// it.
///
/// Shared by the two adapters that shell out to cargo, so the answer to "how do
/// I get cargo" cannot drift into two answers. **Deliberately no command:**
/// rustup's installer is a different shell line on every host, and printing one
/// of them is exactly the platform guess the refusals checklist forbids. Its
/// front page picks the right one, which is why the page *is* the answer here.
///
/// So this hint has a `Note` and no [`Line::Command`], which is what "no
/// command" has to mean if the sentence above is to be true of the code under
/// it. It read `Line::Command("https://rustup.rs")` for one revision — a URL
/// promoted into the slot a command would have occupied, three lines under a
/// comment denying there was one. See [`URL_PREFIX`].
pub const RUST_TOOLCHAIN: Guidance = Guidance::new(&[
    Line::Note(&[
        "Roteiro does not install toolchains. Install Rust — rustup's front page",
        "selects the right installer for this host, so there is nothing to paste",
        "here.",
    ]),
    Line::Note(&["Upstream: https://rustup.rs"]),
]);

/// How every install hint introduces its upstream page.
///
/// One convention, named once, because there were two. Three hints carried the
/// URL as a `Note` reading `Upstream: …` and two promoted it into a
/// [`Line::Command`] — and both of the two were the hints whose prose said they
/// had *no* command, so the odd rendering and the contradicted comment were the
/// same mistake seen from either end.
///
/// The `Note` is the right side of that split. [`Line::Command`] renders one
/// step further in, as the thing to copy and run; a page is a thing to *read*,
/// and the label is what says which of the two a reader is looking at. Reserving
/// the command slot for commands is also what lets a hint say "there is nothing
/// to paste here" and be visibly telling the truth.
///
/// `tests::every_hint_renders_its_upstream_page_the_same_way` holds it, so a
/// fifth adapter cannot introduce a third convention.
pub const URL_PREFIX: &str = "Upstream: ";

/// One analyzer's native output format and invocation.
pub trait Adapter: Sync + std::fmt::Debug {
    /// The analyzer id — the value that appears in every layer key and finding
    /// key this adapter produces.
    fn analyzer(&self) -> &'static str;

    /// A one-line description of what it looks for, for `roteiro security
    /// status` and `--help`.
    fn summary(&self) -> &'static str;

    /// The languages this adapter produces findings for, as the coverage matrix
    /// in ADR-0018 states them. Reported by the CLI so the claim is inspectable
    /// rather than only documented.
    fn languages(&self) -> &'static [&'static str];

    /// Which pinned assets the analyzer needs before it can run offline (see
    /// [`crate::assets`]). An empty slice means it needs none.
    fn asset_ids(&self) -> &'static [&'static str];

    /// Which programs must be on `PATH` for this analyzer to run **on this host**,
    /// in the order a reader would install them.
    ///
    /// The counterpart of [`Adapter::asset_ids`], and the reason both exist:
    /// asset ids are what Roteiro *provisions*, and these are what it
    /// deliberately **never installs** (ADR-0014). `roteiro security status`
    /// reports the two separately because their remedies differ — `prefetch` for
    /// the first, an install the host owner performs for the second — and
    /// collapsing them into one word is issue #464.
    ///
    /// # Why this is declared and not read off [`Adapter::command`]
    ///
    /// Because [`Invocation::program`] is not always the thing to look for.
    /// `cargo-audit`'s program is `cargo`, and `cargo audit` dispatches to a
    /// separate `cargo-audit` binary on `PATH` — so probing `Invocation::program`
    /// would find `cargo` on any Rust developer's machine and report *ready* in
    /// precisely the commonest failure, `cargo` installed and `cargo-audit` not.
    /// That is the defect #464 is about, reintroduced one level down. An adapter
    /// therefore states its own requirement.
    ///
    /// Empty means the analyzer needs nothing on `PATH`.
    fn host_programs(&self) -> &'static [&'static str];

    /// How to obtain each of [`Adapter::host_programs`], one hint per program.
    ///
    /// Required rather than defaulted, and that is the whole point of it being
    /// on the trait: a fifth analyzer cannot compile without answering, so it
    /// cannot ship a refusal that names the obstacle and trails off. The
    /// *pairing* is what
    /// `tests::every_host_program_has_an_install_hint` checks — a hint for
    /// some other program would satisfy the compiler and not the reader.
    ///
    /// Order and duplication do not matter; [`install_hint`] looks up by
    /// program. Empty is correct only for an analyzer that needs nothing on
    /// `PATH`, which no shipped adapter is.
    fn install_hints(&self) -> &'static [InstallHint];

    /// The argv that makes the analyzer emit the native format
    /// [`Adapter::normalize`] parses, with egress configured off.
    ///
    /// `assets` maps an id from [`Adapter::asset_ids`] to the verified local
    /// path it was provisioned to.
    fn command(&self, assets: &AssetPaths<'_>) -> Invocation;

    /// Parse native output into a normalized report.
    ///
    /// # Errors
    /// Returns [`ExecError::MalformedReport`] when the bytes are not this
    /// analyzer's format, or [`ExecError::Json`] when they are not JSON at all.
    /// A partially-parsed report is never returned: either the whole thing
    /// converts or the run fails.
    fn normalize(
        &self,
        native: &[u8],
        ctx: &NativeContext<'_>,
    ) -> Result<NormalizedReport, ExecError>;
}

/// Verified local paths of an analyzer's provisioned assets, keyed by asset id.
#[derive(Debug, Clone, Copy, Default)]
pub struct AssetPaths<'a> {
    entries: &'a [(&'a str, std::path::PathBuf)],
}

impl<'a> AssetPaths<'a> {
    /// Wrap a resolved id → path list.
    #[must_use]
    pub fn new(entries: &'a [(&'a str, std::path::PathBuf)]) -> Self {
        Self { entries }
    }

    /// The path provisioned for `id`, or `None` if it was not resolved.
    ///
    /// An adapter that asked for an asset in [`Adapter::asset_ids`] will always
    /// find it here, because the runner refuses to start otherwise — see
    /// [`ExecError::AssetsUnavailableOffline`].
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&std::path::Path> {
        self.entries
            .iter()
            .find(|(key, _)| *key == id)
            .map(|(_, path)| path.as_path())
    }

    /// The path provisioned for `id` as a string, or an empty string. Adapters
    /// build argv from this; an unresolved asset cannot reach here.
    #[must_use]
    pub fn arg(&self, id: &str) -> String {
        self.get(id)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

/// Every analyzer whose findings this build can **store**.
///
/// Ingest consults this table, so a report from any of them can be read in from
/// CI whether or not this build can *execute* the analyzer — which is the whole
/// point of ADR-0014's "ingest is always available".
///
/// # `clippy` is an adapter and is deliberately not here
///
/// [`clippy::Clippy`] implements the trait above and is reached only by
/// `roteiro lint`, which reports and stores nothing. Membership of this table is
/// what makes an analyzer storable — it is how `ingest` resolves `--analyzer`,
/// and everything it resolves ends at
/// [`rto_graph::Store::replace_findings_layer`]. Leaving a linter out is
/// therefore the mechanism, not a note: there is no `--analyzer clippy` to
/// accept and no layer key for two runs at different toolchains to collide over.
/// ADR-0020 v1.1 is the decision, and [`clippy`]'s module documentation is the
/// reasoning. **Adding it here would silently make lint output an artifact.**
pub static ADAPTERS: &[&dyn Adapter] = &[
    &semgrep::Semgrep,
    &cargo_audit::CargoAudit,
    &osv_scanner::OsvScanner,
];

/// Every analyzer `roteiro lint` can run.
///
/// A separate list from [`known_analyzers`], which answers a different question
/// — *what can be stored* — and would name `semgrep` and `cargo-audit` here,
/// sending a caller off to ask for a lint from an analyzer that files layers.
/// It sits beside [`ADAPTERS`] rather than in [`crate::lint`] so that the two
/// lists are read together: they are the same shape and deliberately disjoint,
/// and a name that drifted into both would make a lint storable by accident.
///
/// Ungated, unlike the linter itself, for [`crate::lint_grant`]'s reason: what
/// `roteiro lint` *could* run is a question a build that cannot run it still has
/// to answer, and `roteiro security prefetch --analyzer clippy` is one of the
/// callers that asks.
pub const LINT_ANALYZERS: &[&str] = &[clippy::ANALYZER];

/// The adapters behind [`LINT_ANALYZERS`].
///
/// [`LINT_ANALYZERS`] names them and this one *is* them, because the callers
/// differ: `prefetch` and the CLI want ids, and anything asking how to obtain a
/// linter's binary wants the adapter. Kept in step by
/// `tests::the_lint_tables_name_the_same_analyzers` rather than derived from
/// each other, since neither can be `const`-derived from the other and a silent
/// divergence would cost a linter its install hint.
static LINT_ADAPTERS: &[&dyn Adapter] = &[&clippy::Clippy];

/// The adapter for `analyzer`, or `None` if this build has none.
///
/// Storable analyzers only, deliberately: this is what `ingest` resolves
/// `--analyzer` through, so answering for `clippy` here would make lint output
/// storable — see [`ADAPTERS`]. Use [`every_adapter`] to ask a question that is
/// about the tool rather than about the store.
#[must_use]
pub fn adapter_for(analyzer: &str) -> Option<&'static dyn Adapter> {
    ADAPTERS.iter().copied().find(|a| a.analyzer() == analyzer)
}

/// Every adapter this build has, storable or not.
///
/// The set an install hint has to exist for, which is a wider set than
/// [`ADAPTERS`]: `roteiro lint` reaches the same subprocess machinery, so a
/// missing `cargo-clippy` produces the same refusal as a missing `semgrep` and
/// deserves the same answer. Kept distinct from [`adapter_for`] so that widening
/// *this* can never widen what `ingest` accepts.
pub fn every_adapter() -> impl Iterator<Item = &'static dyn Adapter> {
    ADAPTERS.iter().chain(LINT_ADAPTERS).copied()
}

/// How to obtain `program`, as declared by the adapter that needs it.
///
/// `None` for a program no adapter declares — which a refusal must then print
/// without an install clause rather than with a guessed one. It cannot happen
/// for a program reached through [`Adapter::command`], because
/// `tests::every_host_program_has_an_install_hint` pairs the two lists and
/// `tests::every_invoked_program_is_declared_on_path` ties the invocation to
/// them.
#[must_use]
pub fn install_hint(program: &str) -> Option<Guidance> {
    every_adapter()
        .flat_map(Adapter::install_hints)
        .find(|hint| hint.program == program)
        .map(|hint| hint.guidance)
}

/// Every analyzer id this build can normalise, sorted — for error messages that
/// tell a caller what it *could* have asked for.
#[must_use]
pub fn known_analyzers() -> Vec<&'static str> {
    let mut ids: Vec<&'static str> = ADAPTERS.iter().map(|a| a.analyzer()).collect();
    ids.sort_unstable();
    ids
}

/// Recorded in place of a snippet hash when the source could not be read — an
/// ingested report about a tree this checkout does not have.
///
/// A named marker rather than a hash of the empty string, so a reader of a
/// finding key can tell "the code was empty" from "the code was unavailable".
pub const NO_SNIPPET: &str = "no-snippet";

/// Short SHA-256 prefix of a snippet, used by identity recipes that need to
/// notice that the *code* at a location changed even though the location did
/// not.
///
/// Sixteen hex characters is 64 bits — far more than enough to keep two
/// snippets at the same rule and offset distinct, and short enough that a
/// rendered key stays readable in a terminal. Leading and trailing whitespace is
/// stripped first, so a reformat that only moved indentation is not a new
/// finding.
#[must_use]
pub fn snippet_hash(snippet: &str) -> String {
    crate::sha256_hex(snippet.trim().as_bytes())[..16].to_owned()
}

/// [`snippet_hash`] of what `snippets` holds for the span, or [`NO_SNIPPET`].
#[must_use]
pub fn snippet_hash_at(snippets: &dyn SnippetSource, path: &str, start: u32, end: u32) -> String {
    snippets
        .snippet(path, start, end)
        .map_or_else(|| NO_SNIPPET.to_owned(), |text| snippet_hash(&text))
}

#[cfg(test)]
mod tests {
    use super::{
        Adapter as _, AssetPaths, Guidance, LINT_ANALYZERS, Line, NO_SNIPPET, NativeContext,
        UNKNOWN_VERSION, URL_PREFIX, adapter_for, every_adapter, install_hint, known_analyzers,
        snippet_hash, snippet_hash_at,
    };
    use rto_graph::SourceIdentity;

    fn ctx(version: Option<&str>) -> NativeContext<'static> {
        static SOURCE: std::sync::LazyLock<SourceIdentity> =
            std::sync::LazyLock::new(SourceIdentity::default);
        NativeContext {
            started_at: "2026-08-15T09:00:00Z".to_owned(),
            ended_at: "2026-08-15T09:00:04Z".to_owned(),
            analyzer_version: version.map(str::to_owned),
            exit_status: 0,
            source: &SOURCE,
            rules_digest: None,
            advisory_db: None,
            worktree: None,
            snippets: &crate::snippet::NoSnippets,
        }
    }

    #[test]
    fn the_registry_answers_for_every_analyzer_it_lists() {
        for id in known_analyzers() {
            assert_eq!(adapter_for(id).expect("registered").analyzer(), id);
        }
        assert!(adapter_for("no-such-analyzer").is_none());
    }

    /// The guard issue #430 asks for, and the reason the hint lives on the trait
    /// rather than in a table beside it.
    ///
    /// What cannot be tested offline is that a command *works* — this machine
    /// may have no network and certainly should not install anything to find
    /// out. What can be tested is that none is **missing**, and missing is the
    /// failure that shipped: a refusal that names the obstacle and stops. So a
    /// fifth analyzer that declares a program and no hint for it fails here,
    /// rather than reaching a reader as a message that trails off.
    ///
    /// Paired **by program**, not counted: an adapter with two programs and two
    /// hints for one of them would satisfy a count and leave the other reader
    /// with nothing.
    #[test]
    fn every_host_program_has_an_install_hint() {
        for adapter in every_adapter() {
            for program in adapter.host_programs() {
                let hint = adapter
                    .install_hints()
                    .iter()
                    .find(|hint| hint.program == *program);
                assert!(
                    hint.is_some(),
                    "{} needs `{program}` on PATH and says nothing about how to get it — \
                     see `Adapter::install_hints`",
                    adapter.analyzer()
                );
            }
            for hint in adapter.install_hints() {
                assert!(
                    adapter.host_programs().contains(&hint.program),
                    "{} hints at installing `{}`, which it does not need on PATH — a hint \
                     for a program no refusal names is one nobody reads",
                    adapter.analyzer(),
                    hint.program
                );
            }
        }
    }

    /// A hint is a *way forward*, so this asserts the shape that makes it one.
    ///
    /// `Guidance` checks its own prose whenever it renders ([`crate::guidance`]),
    /// which covers the collapsed-continuation defect. What it cannot know is
    /// that a hint about obtaining a program must carry the upstream page —
    /// #430's durability rule, because a URL ages better than a command line —
    /// and must not print a package manager that upstream did not name, which is
    /// the platform guess `docs/REVIEW_CHECKLIST.md` forbids.
    #[test]
    fn every_install_hint_carries_upstream_and_guesses_no_package_manager() {
        for adapter in every_adapter() {
            for hint in adapter.install_hints() {
                let rendered = hint.guidance.to_string();
                assert!(
                    rendered.contains("https://"),
                    "the hint for `{}` names no upstream page",
                    hint.program
                );
                // Not a blanket ban on the words: an analyzer whose upstream
                // *does* name one as canonical would state so here and this
                // would have to be argued with. None of the shipped four does,
                // and every one of them has an ecosystem command or a page
                // instead.
                for guess in ["brew ", "apt ", "apt-get ", "yum ", "dnf ", "choco "] {
                    assert!(
                        !rendered.contains(guess),
                        "the hint for `{}` reaches for `{guess}`, which guesses the \
                         reader's platform",
                        hint.program
                    );
                }
            }
        }
    }

    /// One convention for the upstream page, asserted on the [`Line`]s rather
    /// than on rendered text.
    ///
    /// Two of the five hints used to promote the URL into a [`Line::Command`],
    /// and both were the hints whose prose said they had *no* command — so the
    /// inconsistent rendering and the contradicted comment were one mistake, and
    /// a reviewer met it as the comment. The convention is now: the page is a
    /// `Note` beginning [`URL_PREFIX`], and the command slot holds commands.
    ///
    /// Structural, because that is what a fifth adapter would evade. A test on
    /// rendered text would pass on a hint that put the URL anywhere at all — it
    /// is the *shape* that has drifted here, not the presence of the string, and
    /// `every_install_hint_carries_upstream_and_guesses_no_package_manager`
    /// already checks the presence.
    #[test]
    fn every_hint_renders_its_upstream_page_the_same_way() {
        for adapter in every_adapter() {
            for hint in adapter.install_hints() {
                let pages: Vec<&str> = hint
                    .guidance
                    .lines()
                    .iter()
                    .filter_map(|line| match line {
                        Line::Note(fragments) => fragments
                            .iter()
                            .copied()
                            .find(|f| f.starts_with(URL_PREFIX)),
                        Line::Command(_) => None,
                    })
                    .collect();
                assert_eq!(
                    pages.len(),
                    1,
                    "the hint for `{}` must introduce its upstream page exactly once, as a \
                     note beginning {URL_PREFIX:?} — found {pages:?}",
                    hint.program
                );

                // The other half, and the one that caught the real defect: a URL
                // in the command slot. `Line::Command` renders one step further
                // in as the thing to copy and run, so a page there reads as a
                // command — and in both hints where it happened, the prose three
                // lines above said there was no command at all.
                for line in hint.guidance.lines() {
                    if let Line::Command(command) = line {
                        assert!(
                            !command.contains("://"),
                            "the hint for `{}` puts a URL in the command slot ({command:?}) — \
                             a page is read, not run; introduce it with {URL_PREFIX:?}",
                            hint.program
                        );
                    }
                }
            }
        }
    }

    /// Two adapters needing the same program must answer the same way.
    ///
    /// `cargo` is needed by `cargo-audit` and by `clippy`, and
    /// [`install_hint`] resolves by program alone — so it returns whichever is
    /// found first, and the two disagreeing would make the message depend on
    /// table order. They share [`RUST_TOOLCHAIN`] for that reason, and this is
    /// what says so.
    #[test]
    fn a_program_two_adapters_need_is_obtained_one_way() {
        let mut seen: Vec<(&str, Guidance)> = Vec::new();
        for adapter in every_adapter() {
            for hint in adapter.install_hints() {
                if let Some((_, first)) = seen.iter().find(|(name, _)| *name == hint.program) {
                    assert_eq!(
                        *first, hint.guidance,
                        "`{}` is obtained two different ways depending on which adapter \
                         asked — `install_hint` resolves by program, so one of them would \
                         never be printed",
                        hint.program
                    );
                } else {
                    seen.push((hint.program, hint.guidance));
                }
            }
        }
    }

    /// The program a refusal actually names is the one an invocation runs, so
    /// that is the one that must resolve to a hint.
    ///
    /// [`every_host_program_has_an_install_hint`] pairs the two *declared*
    /// lists; this ties them to the third thing, which is what
    /// `SubprocessError::BinaryNotFound` looks up. `cargo-audit` is why it is a
    /// separate assertion: its invocation is `cargo`, so a hint table covering
    /// only `cargo-audit` would pass the pairing and still leave the commonest
    /// refusal without an answer.
    #[test]
    fn every_invoked_program_is_declared_on_path() {
        let empty = AssetPaths::default();
        for adapter in every_adapter() {
            let program = adapter.command(&empty).program;
            assert!(
                adapter.host_programs().contains(&program.as_str()),
                "{} invokes `{program}`, which it does not declare in `host_programs` — \
                 a refusal naming it would find no install hint",
                adapter.analyzer()
            );
            assert!(
                install_hint(&program).is_some(),
                "`{program}` is invoked and has no install hint"
            );
        }
    }

    /// [`LINT_ANALYZERS`] and `LINT_ADAPTERS` are two spellings of one list, and
    /// this is what keeps them one. A linter present in the first and absent
    /// from the second would lose its install hint silently — the refusal would
    /// still print, just without the half that says what to do.
    #[test]
    fn the_lint_tables_name_the_same_analyzers() {
        let from_adapters: Vec<&str> = super::LINT_ADAPTERS.iter().map(|a| a.analyzer()).collect();
        assert_eq!(from_adapters, LINT_ANALYZERS.to_vec());
    }

    /// The lookup is by program and answers for every shipped one, including the
    /// two that are toolchain components rather than analyzers.
    #[test]
    fn the_lookup_answers_for_a_known_program_and_not_an_unknown_one() {
        for program in [
            "semgrep",
            "cargo-audit",
            "osv-scanner",
            "cargo",
            "cargo-clippy",
        ] {
            assert!(install_hint(program).is_some(), "no hint for `{program}`");
        }
        assert!(install_hint("no-such-binary").is_none());
    }

    /// The two hints that could most easily become the same wrong answer.
    ///
    /// A reader missing `cargo-audit` has cargo already; a reader missing
    /// `cargo` has neither. Collapsing them onto one hint would send the first
    /// to an installer they do not need, which is the "wrong *kind* of way
    /// forward" the refusals checklist says has cost an hour here before. This
    /// asserts the distinction survives, and asserts it on rendered text because
    /// that is what the reader gets.
    #[test]
    fn the_toolchain_and_the_subcommand_are_obtained_differently() {
        let toolchain = install_hint("cargo").expect("cargo").to_string();
        let subcommand = install_hint("cargo-audit")
            .expect("cargo-audit")
            .to_string();
        assert!(toolchain.contains("https://rustup.rs"), "{toolchain}");
        assert!(
            !toolchain.contains("cargo install cargo-audit"),
            "{toolchain}"
        );
        assert!(
            subcommand.contains("cargo install cargo-audit"),
            "{subcommand}"
        );
        assert!(!subcommand.contains("https://rustup.rs"), "{subcommand}");
        // Verified against upstream's README, which documents no `--locked`.
        assert!(!subcommand.contains("--locked"), "{subcommand}");
    }

    /// The analyzer with no honest single command says so, rather than reaching
    /// for one of the eight platform-specific ones upstream lists.
    #[test]
    fn an_analyzer_without_one_canonical_command_says_so() {
        let hint = install_hint("osv-scanner")
            .expect("osv-scanner")
            .to_string();
        assert!(
            hint.contains("https://google.github.io/osv-scanner/installation/"),
            "{hint}"
        );
        assert!(hint.contains("no single install command"), "{hint}");
    }

    /// Every registered analyzer is one whose findings are **stored**, so each
    /// one must have a pinned rule set or database to decide the answer. A
    /// linter has neither — its rules are the toolchain — which is why clippy
    /// has an adapter and no registry entry, and why this asserts the property
    /// rather than the name: a future storable analyzer with no asset would fail
    /// here and have to argue its case.
    #[test]
    fn every_storable_analyzer_pins_what_decides_its_answer() {
        for id in known_analyzers() {
            let adapter = adapter_for(id).expect("registered");
            assert!(
                !adapter.asset_ids().is_empty(),
                "{id} is stored but pins nothing that decides its findings"
            );
        }
        assert!(
            super::clippy::Clippy.asset_ids().is_empty(),
            "a linter has no pinned rule set — that is why it is not stored"
        );
    }

    /// Every shipped adapter claims at least one language and a summary, because
    /// `roteiro security status` prints the coverage matrix from this table —
    /// an adapter that claims nothing would silently shrink the reported
    /// coverage.
    #[test]
    fn every_adapter_states_its_coverage() {
        for id in known_analyzers() {
            let adapter = adapter_for(id).expect("registered");
            assert!(!adapter.languages().is_empty(), "{id} claims no language");
            assert!(!adapter.summary().is_empty(), "{id} has no summary");
        }
    }

    #[test]
    fn a_version_is_taken_from_the_caller_then_the_report_then_unknown() {
        assert_eq!(ctx(Some("1.2.3")).version_or(Some("0.0.1")), "1.2.3");
        assert_eq!(ctx(None).version_or(Some("0.0.1")), "0.0.1");
        assert_eq!(ctx(None).version_or(None), UNKNOWN_VERSION);
        // Whitespace is not a version: an all-blank field would be refused
        // downstream as missing evidence, so it is treated as absent here.
        assert_eq!(ctx(Some("  ")).version_or(None), UNKNOWN_VERSION);
    }

    #[test]
    fn snippet_hashes_are_short_stable_and_whitespace_insensitive() {
        let hash = snippet_hash("eval(user_input)");
        assert_eq!(hash.len(), 16);
        assert_eq!(hash, snippet_hash("  eval(user_input)\n"));
        assert_ne!(hash, snippet_hash("eval(other_input)"));
    }

    /// A report about a tree this checkout does not have still yields a
    /// well-formed identity, and one that says why it is weaker.
    #[test]
    fn an_unavailable_snippet_is_named_not_hashed_as_empty() {
        let hash = snippet_hash_at(&crate::snippet::NoSnippets, "a.py", 0, 4);
        assert_eq!(hash, NO_SNIPPET);
        assert_ne!(hash, snippet_hash(""));
    }

    #[test]
    fn asset_paths_resolve_only_what_was_provisioned() {
        let entries = [("semgrep-rules", std::path::PathBuf::from("/cache/r.yaml"))];
        let paths = AssetPaths::new(&entries);
        assert_eq!(paths.arg("semgrep-rules"), "/cache/r.yaml");
        assert!(paths.get("advisory-db").is_none());
        assert!(paths.arg("advisory-db").is_empty());
    }
}

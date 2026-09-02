//! Conformance and hygiene checking for an OKF bundle, over `okf-core`'s model.
//!
//! # Why this is written here rather than depended on
//!
//! Upstream's `okf-validator` does this job and was adopted for a while. It could
//! not be kept: none of its dependencies is optional and it syntax-checks fenced
//! code blocks in four languages, so taking it means taking `rustpython-parser` —
//! 61 crates, `LGPL-3.0-only` through the `malachite` tree, and six unmaintained
//! advisories whose own text says no safe upgrade exists. `cargo deny` refuses
//! that on both counts and ADR-0017 §3 forbids admitting a licence merely to turn
//! CI green.
//!
//! What is rebuilt here is the **structural** half, which is the half that is
//! about OKF. It costs no dependency: every rule below is expressed over
//! [`okf_core`]'s own model, which is already in the tree.
//!
//! # This is not a re-derivation of the specification
//!
//! `docs/OKF_BUNDLE.md` warns that re-deriving the format is how two readers of
//! one spec end up disagreeing, and that warning is the reason `okf-core` was
//! adopted in the first place. So nothing here parses OKF. Frontmatter, trust
//! tiers, actor classes, links, footnotes, headings, computations and concept ids
//! all come from `okf-core`; these functions only ask questions **about** the
//! model it returns. When the specification changes, the parsing follows upstream
//! and only the questions are ours.
//!
//! # Code syntax is deliberately not checked here
//!
//! Whether a fenced `sql` block is valid SQL says nothing about whether a bundle
//! is valid OKF — a document full of pseudocode is perfectly conformant. Folding
//! the two together is what made upstream's validator expensive, and it is also
//! what makes it noisy: run over the four bundles published with the
//! specification, its SQL arm reports six warnings, every one a documentation
//! *fragment* in `stackoverflow/references/` that was never meant to be a
//! statement.
//!
//! That question has its own command, [`super::inspect::syntax_report`]
//! (`roteiro okf syntax`), and its own crate. The two checks below that upstream
//! spends a Python parser on — `check_code_block_syntax` and
//! `check_computation_script_syntax` — are therefore **absent by design**, and
//! their absence is the only intended behavioural difference from upstream over
//! the published corpus.
//!
//! # Determinism
//!
//! `stale_after` is checked for **syntax** and never against the clock, so a
//! bundle that validates today validates tomorrow. A check whose result depends
//! on when it ran cannot be a gate, and this one is used as one.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use okf_core::{
    Bundle, Concept, Document, Frontmatter, PREFERRED_KEY_ORDER, Status, TrustTier, Value,
};
use serde::Serialize;

use super::inspect::InspectError;

/// One finding from [`validate_report`] or [`lint_report`].
#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    /// `error`, `warning` or `info`.
    pub severity: &'static str,
    /// The hygiene rule that produced this (`L1`..`L12`), when it was one.
    ///
    /// Conformance findings carry no code: they are the specification's rules
    /// rather than this project's opinions, and numbering them here would invent
    /// an identifier scheme OKF does not have.
    pub code: Option<&'static str>,
    /// The concept the finding is about, if it is about one.
    pub concept: Option<String>,
    /// The file the finding is about, relative to the bundle root.
    pub path: Option<String>,
    /// What is wrong.
    pub message: String,
}

/// The findings from one check over a bundle.
#[derive(Debug, Clone, Serialize)]
pub struct CheckReport {
    /// The bundle root, as the caller named it.
    pub root: String,
    /// Which check produced this: `validate` or `lint`.
    pub check: &'static str,
    /// How many concepts were examined. Reported so that "no findings" over an
    /// empty bundle cannot be read as a clean bill of health.
    pub concepts: usize,
    /// Findings: errors first, then warnings, then info. Within one severity,
    /// bundle order is preserved.
    pub findings: Vec<Finding>,
    /// Count of `error` findings. Non-zero means the check failed.
    pub errors: usize,
    /// Count of `warning` findings.
    pub warnings: usize,
}

impl CheckReport {
    /// `true` when nothing rose to `error`.
    ///
    /// Warnings deliberately do not fail: §11 tells a consumer not to reject a
    /// document over a soft-guidance deviation, and a check that failed on one
    /// would be unusable against real third-party bundles. Measured: of the 208
    /// diagnostics upstream reports over the four published bundles, **none** is
    /// an error — so a gate that failed on warnings would reject the
    /// specification's own corpus.
    #[must_use]
    pub const fn passed(&self) -> bool {
        self.errors == 0
    }
}

/// Accumulates findings, remembering which concept is being examined so each
/// rule can emit a diagnostic without restating the path and id.
struct Cx<'a> {
    findings: Vec<Finding>,
    concept: Option<String>,
    path: Option<String>,
    root: &'a Path,
}

impl<'a> Cx<'a> {
    fn new(root: &'a Path) -> Self {
        Self {
            findings: Vec::new(),
            concept: None,
            path: None,
            root,
        }
    }

    /// Point subsequent findings at `concept`.
    fn at(&mut self, concept: &Concept) {
        self.concept = Some(concept.id.to_string());
        self.path = Some(self.relative(&concept.path));
    }

    /// Point subsequent findings at a file that is not a concept — an index, a
    /// log, or a document that failed to parse.
    fn at_file(&mut self, path: &Path) {
        self.concept = None;
        self.path = Some(self.relative(path));
    }

    /// Bundle-relative, so a report does not leak the absolute path it was run
    /// from and two runs of the same bundle compare equal.
    fn relative(&self, path: &Path) -> String {
        path.strip_prefix(self.root)
            .unwrap_or(path)
            .display()
            .to_string()
    }

    fn push(&mut self, severity: &'static str, code: Option<&'static str>, message: String) {
        self.findings.push(Finding {
            severity,
            code,
            concept: self.concept.clone(),
            path: self.path.clone(),
            message,
        });
    }

    fn err(&mut self, message: impl Into<String>) {
        self.push("error", None, message.into());
    }

    fn warn(&mut self, message: impl Into<String>) {
        self.push("warning", None, message.into());
    }

    fn info(&mut self, message: impl Into<String>) {
        self.push("info", None, message.into());
    }

    /// A hygiene finding, which always carries its rule code.
    fn lint(&mut self, severity: &'static str, code: &'static str, message: impl Into<String>) {
        self.push(severity, Some(code), message.into());
    }

    fn finish(self, root: &Path, check: &'static str, concepts: usize) -> CheckReport {
        let mut findings = self.findings;
        // Sorted rather than relied upon: the traversal order is an
        // implementation detail, while this ordering is what a reader sees first
        // and what a CI log diff compares. `sort_by_key` is stable, so within a
        // severity the bundle's own order survives.
        findings.sort_by_key(|f| match f.severity {
            "error" => 0u8,
            "warning" => 1,
            _ => 2,
        });
        CheckReport {
            root: root.display().to_string(),
            check,
            concepts,
            errors: findings.iter().filter(|f| f.severity == "error").count(),
            warnings: findings.iter().filter(|f| f.severity == "warning").count(),
            findings,
        }
    }
}

/// Check a bundle for conformance with the OKF v0.2 specification.
///
/// Deterministic — see the module documentation.
///
/// # Errors
///
/// [`InspectError::Unreadable`] if the path is not a loadable OKF bundle.
pub fn validate_report(root: &Path) -> Result<CheckReport, InspectError> {
    let bundle = super::inspect::load(root)?;
    Ok(validate_bundle(&bundle, root))
}

/// The bundle-in-hand half of [`validate_report`], so a caller that has already
/// loaded a [`Bundle`] pays for the directory walk once.
#[must_use]
pub fn validate_bundle(bundle: &Bundle, root: &Path) -> CheckReport {
    let mut cx = Cx::new(root);

    // A document that did not parse is a conformance error, and it is the only
    // class here that is: everything else is a judgement about a document we
    // could read.
    for (path, error) in bundle.parse_errors() {
        cx.at_file(path);
        cx.err(format!("not a readable OKF document: {error}"));
    }

    for concept in bundle.concepts() {
        cx.at(concept);
        let doc = &concept.document;
        let fm = &doc.frontmatter;

        check_type(&mut cx, concept, fm);
        check_recommended(&mut cx, doc);
        check_empty_body(&mut cx, doc);
        check_tags(&mut cx, fm);
        check_trust(&mut cx, fm);
        check_lifecycle(&mut cx, fm);
        check_usage_window(&mut cx, fm);
        check_attribution(&mut cx, doc);
        check_legacy(&mut cx, doc, fm);
        check_computation(&mut cx, concept);
        check_resources(&mut cx, bundle, concept);
        check_link_targets(&mut cx, bundle, concept);
        check_reserved_filename(&mut cx, concept);
    }

    check_declared_version(&mut cx, bundle);
    check_duplicate_titles(&mut cx, bundle);
    check_circular_derivation(&mut cx, bundle);
    check_stale_indexes(&mut cx, bundle);

    cx.finish(root, "validate", bundle.concepts().len())
}

/// Check a bundle against the hygiene rules (`L1`..`L12`).
///
/// A different question from [`validate_report`]: conformance asks whether the
/// bundle *is* OKF, linting asks whether it is *good* OKF. Nothing here is a
/// conformance failure, which is why every rule reports `warning` or `info` and
/// this check never gates.
///
/// # Errors
///
/// [`InspectError::Unreadable`] if the path is not a loadable OKF bundle.
pub fn lint_report(root: &Path) -> Result<CheckReport, InspectError> {
    let bundle = super::inspect::load(root)?;
    Ok(lint_bundle(&bundle, root))
}

/// The bundle-in-hand half of [`lint_report`].
#[must_use]
pub fn lint_bundle(bundle: &Bundle, root: &Path) -> CheckReport {
    let mut cx = Cx::new(root);
    let indexed = indexed_concepts(bundle);

    for concept in bundle.concepts() {
        cx.at(concept);
        let doc = &concept.document;
        let fm = &doc.frontmatter;

        lint_headings(&mut cx, doc);
        lint_key_order(&mut cx, fm);
        lint_unused_sources(&mut cx, concept, doc);
        lint_actor_convention(&mut cx, concept);
        lint_computation_block(&mut cx, doc);
        lint_whitespace(&mut cx, doc);
        lint_orphan(&mut cx, concept, &indexed);
        lint_portable_id(&mut cx, concept);
        lint_self_link(&mut cx, bundle, concept);
        lint_unverified(&mut cx, concept);
        lint_draft(&mut cx, concept);
    }

    cx.finish(root, "lint", bundle.concepts().len())
}

// ---------------------------------------------------------------------------
// Conformance
// ---------------------------------------------------------------------------

/// §4.1: every concept carries a non-empty `type`.
fn check_type(cx: &mut Cx<'_>, concept: &Concept, fm: &Frontmatter) {
    if concept.type_().is_none_or(|t| t.trim().is_empty()) {
        cx.err("`type` is missing or empty; §4.1 requires one on every concept");
    }
    // An unknown *value* is not an error. §11 tells a consumer to read
    // liberally, and a producer's vocabulary is theirs — this is the line
    // between "not OKF" and "not our OKF".
    if let Some(t) = fm.type_()
        && !t.trim().is_empty()
        && t.trim() != t
    {
        cx.info(format!(
            "`type` has surrounding whitespace (`{t}`); consumers that compare it literally will not match"
        ));
    }
}

/// §4.1's recommended keys. Always a warning — conformance forbids rejecting a
/// concept over an optional field, however much a producer wants it filled in.
fn check_recommended(cx: &mut Cx<'_>, doc: &Document) {
    for key in doc.missing_recommended() {
        cx.warn(format!("recommended key `{key}` is missing"));
    }
}

fn check_empty_body(cx: &mut Cx<'_>, doc: &Document) {
    if doc.body.trim().is_empty() {
        cx.warn("body is empty; a concept should carry at least one line of prose or code");
    }
}

/// §4.1: `tags` is a list of short strings.
///
/// A bare scalar is the shape Google's `stackoverflow` bundle writes in seven
/// documents. Roteiro's *reader* accepts it (§11, read liberally); this reports
/// it, because a producer that wrote one string meant one tag and most consumers
/// will read none.
fn check_tags(cx: &mut Cx<'_>, fm: &Frontmatter) {
    match fm.get("tags") {
        Some(Value::String(_)) => cx.warn(
            "`tags` should be a list of short strings, found a string; \
             a strict consumer reads no tags from it",
        ),
        Some(Value::Sequence(items)) => {
            if let Some(bad) = items.iter().find(|v| !matches!(v, Value::String(_))) {
                cx.warn(format!(
                    "`tags` contains a non-string entry ({}); §4.1 asks for short strings",
                    kind_of(bad)
                ));
            }
        }
        Some(other) => cx.warn(format!(
            "`tags` should be a list of short strings, found {}",
            kind_of(other)
        )),
        None => {}
    }
}

/// §5.2: the `generated` and `verified` trust events.
fn check_trust(cx: &mut Cx<'_>, fm: &Frontmatter) {
    if let Some(generated) = fm.generated() {
        if generated.by.is_none() {
            cx.warn("`generated.by` is required within `generated`");
        }
        match &generated.at {
            None => cx.warn("`generated.at` is required within `generated`"),
            Some(at) if at.datetime.is_none() => cx.warn(format!(
                "`generated.at` is not an ISO-8601 datetime (`{}`)",
                at.raw
            )),
            Some(_) => {}
        }
    }

    // Present-but-empty is the case worth reporting: it asserts that
    // verification happened and then names nobody.
    if fm.contains_key("verified") {
        let events = fm.verified();
        if events.is_empty() {
            cx.warn("`verified` is present but contains no `{ by, at }` events");
        }
        for (i, event) in events.iter().enumerate() {
            if event.by.is_none() {
                cx.warn(format!("`verified[{i}].by` is missing"));
            }
            match &event.at {
                None => cx.warn(format!("`verified[{i}].at` is missing")),
                Some(at) if at.datetime.is_none() => cx.warn(format!(
                    "`verified[{i}].at` is not an ISO-8601 datetime (`{}`)",
                    at.raw
                )),
                Some(_) => {}
            }
        }
    }
}

/// §5.4 lifecycle, and §5.5's `stale_after` — its **syntax**, never its relation to now.
fn check_lifecycle(cx: &mut Cx<'_>, fm: &Frontmatter) {
    if let Some(Status::Other(value)) = Some(Status::parse(fm.get("status").and_then(as_str))) {
        cx.info(format!(
            "`status: {value}` is outside §5.4's `draft | stable | deprecated`; \
             consumers must tolerate it, but few will act on it"
        ));
    }
    if let Some(raw) = fm.get("stale_after").and_then(as_str)
        && okf_core::DateTime::parse(raw).is_none()
    {
        cx.warn(format!(
            "`stale_after` is not an ISO-8601 datetime (`{raw}`), so no consumer can act on it"
        ));
    }
}

/// §5.1: a `usage_window` frames sources, so it needs some to frame.
fn check_usage_window(cx: &mut Cx<'_>, fm: &Frontmatter) {
    if fm.usage_window().is_some() && fm.sources().is_empty() {
        cx.warn("`usage_window` is present without `sources` to frame");
    }
}

/// §5.1: a footnote is the join key between body prose and a `sources` entry.
/// (The footnote syntax itself is §4.2.)
fn check_attribution(cx: &mut Cx<'_>, doc: &Document) {
    for attribution in doc.attributions() {
        if attribution.source.is_none() {
            cx.warn(format!(
                "footnote [^{}] matches no `sources[].id`; the label is the join key for attribution",
                attribution.label
            ));
        }
    }
}

/// §13.1: the keys and body conventions v0.2 superseded.
fn check_legacy(cx: &mut Cx<'_>, doc: &Document, fm: &Frontmatter) {
    if fm.timestamp().is_some() {
        cx.warn("`timestamp` is superseded by `generated.at` (§13.1)");
    }
    if doc.has_legacy_citations() {
        cx.warn("the body `# Citations` list is superseded by `sources` (§13.1)");
    }
}

/// §10: an Attested Computation carries a runnable, checkable contract.
fn check_computation(cx: &mut Cx<'_>, concept: &Concept) {
    let Some(computation) = concept.attested_computation() else {
        return;
    };

    if computation.runtime.as_deref().is_none_or(str::is_empty) {
        cx.warn("`runtime` is missing; without it nothing knows how to run the computation");
    }
    for (i, parameter) in computation.parameters.iter().enumerate() {
        if parameter.name.is_none() {
            cx.warn(format!("`parameters[{i}].name` is missing"));
        }
        if parameter.type_.is_none() {
            cx.warn(format!("`parameters[{i}].type` is missing"));
        }
    }
    match &computation.executor {
        None => cx.warn("missing `executor`: nothing says how to run the computation"),
        Some(e) if e.resource.is_none() => {
            cx.warn("`executor.resource` is missing; it names the run instructions or code");
        }
        Some(_) => {}
    }
    match &computation.attester {
        None => cx.warn("missing `attester`: nothing can check a run's receipt"),
        Some(a) if a.resource.is_none() => {
            cx.warn("`attester.resource` is missing; it names the deterministic check");
        }
        Some(_) => {}
    }
    if computation.computation.is_missing() {
        cx.warn(
            "no computation: neither a `# Computation` block nor a `computation:` path is present, \
             so there is nothing for an executor to run or an attester to check",
        );
    }
    if computation.has_redundant_inline {
        cx.warn(
            "both a `# Computation` block and a `computation:` path are present; \
             §10 asks for one or the other, and two copies can disagree",
        );
    }
}

/// Every `resource:` that names something inside the bundle must be there.
///
/// A URL is left alone: whether `https://…` resolves is a network question, and
/// this whole module is offline by construction.
fn check_resources(cx: &mut Cx<'_>, bundle: &Bundle, concept: &Concept) {
    let check = |label: &str, raw: &str, cx: &mut Cx<'_>| {
        if let Some(rel) = bundle_relative(raw)
            && !bundle.root().join(&rel).exists()
        {
            cx.warn(format!(
                "{label} names `{raw}`, which the bundle does not contain"
            ));
        }
    };

    if let Some(resource) = concept.document.frontmatter.resource() {
        check("`resource`", &resource, cx);
    }
    for source in concept.sources() {
        if let Some(resource) = &source.resource {
            check("a `sources` entry", resource, cx);
        }
    }
    if let Some(computation) = concept.attested_computation() {
        if let Some(e) = computation
            .executor
            .as_ref()
            .and_then(|e| e.resource.clone())
        {
            check("`executor.resource`", &e, cx);
        }
        if let Some(a) = computation
            .attester
            .as_ref()
            .and_then(|a| a.resource.clone())
        {
            check("`attester.resource`", &a, cx);
        }
        if let Some(path) = computation.computation.path() {
            check("`computation`", path, cx);
        }
    }
}

/// Links that resolve, and what they resolve *to*.
///
/// A broken cross-link is `info`, not an error: §11 permits it, and a bundle is
/// often one half of a set. A link to a **deprecated** concept is a warning,
/// because the target is telling the reader to go somewhere else.
fn check_link_targets(cx: &mut Cx<'_>, bundle: &Bundle, concept: &Concept) {
    // Deprecation is reported once per *target*, not once per link. A document
    // that mentions a retired concept twice has one problem, and `gross-margin`
    // in the specification's own `acme_retail` does exactly that.
    let mut deprecated: BTreeSet<String> = BTreeSet::new();
    for link in bundle.links_from(&concept.id) {
        if !link.exists {
            cx.info(format!(
                "link `{}` names `{}`, which the bundle does not contain; \
                 §6 tells a consumer to tolerate this",
                link.raw, link.target
            ));
            continue;
        }
        if let Some(target) = bundle.get(&link.target)
            && target.status().is_deprecated()
        {
            deprecated.insert(link.target.to_string());
        }
    }
    for target in deprecated {
        cx.warn(format!("links to deprecated concept `{target}`"));
    }
}

/// §3.1: the reserved filenames a concept document may not take.
///
/// One of the few plain `MUST NOT`s in the specification, so one of the few
/// errors here.
fn check_reserved_filename(cx: &mut Cx<'_>, concept: &Concept) {
    let name = concept
        .path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if okf_core::RESERVED_FILENAMES.contains(&name) {
        cx.err(format!(
            "`{name}` is a reserved filename and §3.1 forbids using it for a concept document"
        ));
    }
}

/// §12: a declared OKF version this reader does not implement.
///
/// **Absence is not reported**, and that is the specification's decision rather
/// than leniency: §8 and §12 both say a bundle-root `index.md` *MAY* carry
/// `okf_version`. An earlier draft of this module warned when it was missing,
/// and that warning fired on **all four** bundles published with the
/// specification — the same shape as any check that disagrees with an entire
/// corpus, and the same conclusion.
///
/// A version we do not implement is `info` rather than a warning because §12
/// tells a consumer that does not understand the declared version to "attempt
/// best-effort consumption rather than refusing the bundle" — which is what
/// this reader does, so the note is for the reader and not against the bundle.
fn check_declared_version(cx: &mut Cx<'_>, bundle: &Bundle) {
    if let Some(version) = bundle.okf_version()
        && version != super::OKF_VERSION
    {
        cx.at_file(&bundle.root().join("index.md"));
        cx.info(format!(
            "the bundle declares `okf_version: {version}`; this reader implements {}, \
             so it is read best-effort (§12)",
            super::OKF_VERSION
        ));
        cx.concept = None;
        cx.path = None;
    }
}

/// Two concepts with the same title are indistinguishable in any listing.
fn check_duplicate_titles(cx: &mut Cx<'_>, bundle: &Bundle) {
    let mut by_title: BTreeMap<String, Vec<&Concept>> = BTreeMap::new();
    for concept in bundle.concepts() {
        by_title
            .entry(concept.display_title())
            .or_default()
            .push(concept);
    }
    for (title, concepts) in by_title {
        if concepts.len() < 2 {
            continue;
        }
        let others: Vec<String> = concepts.iter().map(|c| c.id.to_string()).collect();
        for concept in &concepts {
            cx.at(concept);
            // Formatted once per concept rather than once per comparison.
            let self_id = concept.id.to_string();
            let siblings: Vec<&String> = others.iter().filter(|id| **id != self_id).collect();
            cx.warn(format!(
                "title `{title}` is shared with {}; \
                 the two are indistinguishable in any listing that shows titles",
                siblings
                    .iter()
                    .map(|s| format!("`{s}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
}

/// A concept derived, through `sources`, from itself.
///
/// An **error**, unlike every other provenance finding: a cycle means no reader
/// can establish where the claim came from, and following it is unbounded.
fn check_circular_derivation(cx: &mut Cx<'_>, bundle: &Bundle) {
    // Edges: concept → the concepts its `sources` name.
    let mut edges: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for concept in bundle.concepts() {
        let from = concept.id.to_string();
        for source in concept.sources() {
            let Some(resource) = &source.resource else {
                continue;
            };
            let Some(rel) = bundle_relative(resource) else {
                continue;
            };
            // A self-edge is kept. A concept whose `sources` names itself is a
            // cycle of length one — the shortest way to make provenance
            // unresolvable — and dropping it as "not really an edge" was the one
            // shape this rule could not see.
            if let Some(id) = okf_core::links::concept_id_for_path(&rel)
                && bundle.contains(&id)
            {
                edges
                    .entry(from.clone())
                    .or_default()
                    .insert(id.to_string());
            }
        }
    }

    // Depth-first, reporting the cycle's members rather than only its existence:
    // "there is a cycle" is not actionable, and a reader needs the ring.
    let mut seen: BTreeSet<String> = BTreeSet::new();
    // Keyed by the ring's *members*, not by where the walk happened to start.
    // `a → b → a` and `b → a → b` are one cycle seen from two ends, and keying
    // on the start reported it once per member — two errors for one ring.
    let mut reported: BTreeSet<Vec<String>> = BTreeSet::new();
    for start in edges.keys() {
        let mut stack = vec![(start.clone(), vec![start.clone()])];
        while let Some((node, trail)) = stack.pop() {
            for next in edges.get(&node).into_iter().flatten() {
                if next == start {
                    let ring = trail.join(" → ");
                    let mut members = trail.clone();
                    members.sort();
                    members.dedup();
                    if reported.insert(members) {
                        if let Some(concept) = bundle
                            .concepts()
                            .iter()
                            .find(|c| c.id.to_string() == *start)
                        {
                            cx.at(concept);
                        }
                        cx.err(format!(
                            "circular derivation: {ring} → {start}; \
                             no reader can establish where this claim came from"
                        ));
                    }
                    continue;
                }
                if seen.insert(format!("{start}\u{0}{next}")) {
                    let mut trail = trail.clone();
                    trail.push(next.clone());
                    stack.push((next.clone(), trail));
                }
            }
        }
    }
}

/// An `index.md` that lists a concept the bundle no longer contains.
///
/// The other direction — a concept no index lists — is hygiene rather than
/// conformance, and is `L9` below.
fn check_stale_indexes(cx: &mut Cx<'_>, bundle: &Bundle) {
    for index in bundle.index_files() {
        cx.at_file(index);
        for (target, resolved) in index_listings(bundle, index) {
            if !resolved.exists() {
                cx.warn(format!(
                    "index lists `{target}`, which no longer exists; \
                     a reader following the listing lands on nothing"
                ));
            }
        }
    }
    cx.concept = None;
    cx.path = None;
}

// ---------------------------------------------------------------------------
// Hygiene (L1..L12)
// ---------------------------------------------------------------------------

/// `L1` (no top-level heading), `L3` (more than one, or a skipped level) and
/// `L4` (a heading with nothing under it).
///
/// One traversal, because all three are questions about the same heading list
/// and splitting them would walk the body three times to no purpose.
fn lint_headings(cx: &mut Cx<'_>, doc: &Document) {
    let headings = okf_core::markdown::extract_headings(&doc.body);
    if headings.is_empty() {
        cx.lint(
            "warning",
            "L1",
            "body has no top-level `#` heading; OKF docs conventionally open with one",
        );
        return;
    }

    let mut top = 0usize;
    let mut previous = 0usize;
    for (i, heading) in headings.iter().enumerate() {
        if heading.level == 1 {
            top += 1;
            if top > 1 {
                cx.lint(
                    "warning",
                    "L3",
                    format!(
                        "multiple top-level `#` headings found (heading `{}` at line {})",
                        heading.text, heading.line_num
                    ),
                );
            }
        }
        if previous > 0 && heading.level > previous + 1 {
            cx.lint(
                "warning",
                "L3",
                format!(
                    "heading level skipped: `{}` jumps from h{previous} to h{}",
                    heading.text, heading.level
                ),
            );
        }
        previous = heading.level;

        // Empty when nothing but blank lines follows, *and* nothing is nested
        // beneath. A heading whose next sibling is deeper is a container — the
        // content is under its subheadings, not missing. `# Common query
        // patterns` in the specification's `ga4` bundle is exactly that shape,
        // and an earlier draft of this rule flagged three of them.
        let starts = heading.line_index + 1;
        let ends = headings
            .get(i + 1)
            .map_or_else(|| doc.body.lines().count(), |h| h.line_index);
        let contains_a_deeper_heading = headings
            .get(i + 1)
            .is_some_and(|next| next.level > heading.level);
        let empty = !contains_a_deeper_heading
            && doc
                .body
                .lines()
                .skip(starts)
                .take(ends.saturating_sub(starts))
                .all(|l| l.trim().is_empty());
        if empty {
            cx.lint(
                "warning",
                "L4",
                format!("heading `{}` has no content", heading.text),
            );
        }
    }

    if top == 0 {
        cx.lint(
            "warning",
            "L1",
            "body has no top-level `#` heading; OKF docs conventionally open with one",
        );
    }
}

/// `L2`: frontmatter keys in the canonical order.
///
/// Only the keys §5 names are ordered. A producer's own keys are theirs and are
/// skipped, so a bundle is not nagged for carrying extra metadata.
fn lint_key_order(cx: &mut Cx<'_>, fm: &Frontmatter) {
    let rank: BTreeMap<&str, usize> = PREFERRED_KEY_ORDER
        .iter()
        .enumerate()
        .map(|(i, k)| (*k, i))
        .collect();
    let ranked: Vec<usize> = fm.keys().filter_map(|k| rank.get(k).copied()).collect();
    if ranked.windows(2).any(|w| w[0] > w[1]) {
        cx.lint(
            "info",
            "L2",
            "frontmatter keys are not in canonical order (§5's reading order)",
        );
    }
}

/// `L5`: a declared source nobody cites.
fn lint_unused_sources(cx: &mut Cx<'_>, concept: &Concept, doc: &Document) {
    let cited: BTreeSet<String> = doc
        .footnote_refs()
        .into_iter()
        .map(|r| r.label)
        .chain(doc.footnote_definitions().into_iter().map(|d| d.label))
        .collect();
    for source in concept.sources() {
        let Some(id) = &source.id else { continue };
        if !cited.contains(id) {
            cx.lint(
                "warning",
                "L5",
                format!(
                    "source `{id}` is declared in frontmatter but never cited with footnote `[^{id}]`"
                ),
            );
        }
    }
}

/// `L6`: §7's actor convention on a source's author.
fn lint_actor_convention(cx: &mut Cx<'_>, concept: &Concept) {
    for source in concept.sources() {
        let Some(author) = &source.author else {
            continue;
        };
        if author.kind() == okf_core::ActorKind::Other {
            cx.lint(
                "info",
                "L6",
                format!(
                    "author `{}` in `sources.author` does not follow §7's `human:<id>`, \
                     `process:<id>` or `<producer>/<version>` convention",
                    author.as_str()
                ),
            );
        }
    }
}

/// `L7`: a `# Computation` block with no language tag.
///
/// Reported, not acted on. `roteiro okf syntax` is what would check the code,
/// and an untagged block is exactly the one it must skip — so this rule is the
/// reason a reader ever sees "skipped" there.
fn lint_computation_block(cx: &mut Cx<'_>, doc: &Document) {
    if let Some(inline) = doc.inline_computation()
        && inline.fenced
        && inline.language.is_none()
    {
        cx.lint(
            "warning",
            "L7",
            "`# Computation` code block carries no language tag \
             (e.g. ```sql), so no syntax check can read it",
        );
    }
}

/// `L8`: trailing whitespace in the body.
fn lint_whitespace(cx: &mut Cx<'_>, doc: &Document) {
    let offending: Vec<usize> = doc
        .body
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.is_empty() && l.trim_end() != *l)
        .map(|(i, _)| i + 1)
        .collect();
    if let Some(first) = offending.first() {
        cx.lint(
            "info",
            "L8",
            format!(
                "trailing whitespace found on {} line(s) in markdown body (first at line {first})",
                offending.len()
            ),
        );
    }
}

/// `L9`: a concept no index lists.
fn lint_orphan(cx: &mut Cx<'_>, concept: &Concept, indexed: &BTreeSet<String>) {
    if !indexed.contains(&concept.id.to_string()) {
        cx.lint(
            "warning",
            "L9",
            "no `index.md` lists this concept, so nothing walking the bundle's \
             listings will reach it",
        );
    }
}

/// `R1`: a concept id that will not survive every checkout.
///
/// **`R` and not `L`, deliberately.** `L1`..`L12` are upstream's hygiene rules
/// and that namespace is theirs; this one is Roteiro's, and the specification
/// states no portability requirement for path segments — §6 constrains what a
/// path *means*, not what characters it may contain. Numbering it `L13` would
/// both claim their vocabulary and imply a conformance basis it does not have.
fn lint_portable_id(cx: &mut Cx<'_>, concept: &Concept) {
    for segment in concept.id.segments() {
        if !okf_core::concept_id::is_portable_segment(segment) {
            cx.lint(
                "warning",
                "R1",
                format!(
                    "concept-id segment `{segment}` may not survive a checkout on every \
                     filesystem; the specification does not forbid it, but a consumer on \
                     a case-insensitive or restricted filesystem cannot read the bundle"
                ),
            );
        }
    }
}

/// `L10`: a concept that links to itself.
fn lint_self_link(cx: &mut Cx<'_>, bundle: &Bundle, concept: &Concept) {
    if bundle
        .links_from(&concept.id)
        .iter()
        .any(|l| l.target == concept.id)
    {
        cx.lint(
            "warning",
            "L10",
            "self-link; a concept that links to itself usually signals a stray reference",
        );
    }
}

/// `L11`: nothing has confirmed this concept.
fn lint_unverified(cx: &mut Cx<'_>, concept: &Concept) {
    if concept.trust_tier() == TrustTier::Unverified {
        cx.lint(
            "info",
            "L11",
            "no `verified` events; trust tier is `unverified`",
        );
    }
}

/// `L12`: a draft concept.
fn lint_draft(cx: &mut Cx<'_>, concept: &Concept) {
    if concept.status() == Status::Draft {
        cx.lint(
            "warning",
            "L12",
            "`status: draft`; a draft concept is not ready for production consumption",
        );
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Every concept id any `index.md` links to.
fn indexed_concepts(bundle: &Bundle) -> BTreeSet<String> {
    let mut listed = BTreeSet::new();
    for index in bundle.index_files() {
        for (_, resolved) in index_listings(bundle, index) {
            if let Ok(id) = okf_core::ConceptId::from_path(bundle.root(), &resolved) {
                listed.insert(id.to_string());
            }
        }
    }
    listed
}

/// Every concept document one `index.md` links to, as `(as written, resolved)`.
///
/// Shared by the two rules that read a listing — "this index names something
/// gone" and "no index names this concept". They are opposite directions of one
/// question, and two copies of the walk would be two chances to resolve a link
/// differently and report a contradiction.
fn index_listings(bundle: &Bundle, index: &Path) -> Vec<(String, std::path::PathBuf)> {
    let Ok(text) = std::fs::read_to_string(index) else {
        return Vec::new();
    };
    let parent = index.parent().unwrap_or_else(|| bundle.root());
    okf_core::links::extract_links(&text)
        .into_iter()
        .filter_map(|link| {
            let target = link.target_without_anchor().to_owned();
            if target.contains("://")
                || !Path::new(&target)
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("md"))
            {
                return None;
            }
            let resolved = target
                .strip_prefix('/')
                .map_or_else(|| parent.join(&target), |rooted| bundle.root().join(rooted));
            Some((target, resolved))
        })
        .collect()
}

/// The bundle-relative path a `resource:` names, or `None` when it names
/// something outside the bundle — a URL, or a path that climbs out of it.
fn bundle_relative(raw: &str) -> Option<String> {
    if raw.contains("://") || raw.starts_with("mailto:") {
        return None;
    }
    let trimmed = raw.trim_start_matches('/');
    if trimmed.is_empty() {
        return None;
    }

    // Nothing that could climb out of the bundle or re-root the join, on
    // **either** platform's rules. A bundle is a portable artefact — one written
    // on Windows is read on Unix — so the separator cannot be left to whichever
    // machine happens to be reading. `..\..` is a single ordinary filename to
    // Unix and a climb to Windows, and `C:\…` re-roots the join outright; the
    // caller does `bundle.root().join(rel)`, so either would have this checker
    // stat a file the bundle does not own.
    if trimmed
        .split(['/', '\\'])
        .any(|segment| segment == ".." || segment == "." || segment.is_empty())
    {
        return None;
    }
    // A drive or UNC prefix. A URL was already excluded above, and no portable
    // filename carries a colon, so this costs nothing that was readable anyway.
    if trimmed.contains(':') {
        return None;
    }
    // The platform's own reading, as a backstop: whatever the two rules above
    // missed, every component must still be an ordinary name.
    if Path::new(trimmed)
        .components()
        .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return None;
    }
    Some(trimmed.to_owned())
}

fn as_str(value: &Value) -> Option<&str> {
    match value {
        Value::String(s) => Some(s.as_str()),
        _ => None,
    }
}

/// A YAML value's shape, for a diagnostic that says what was found rather than
/// only what was wanted.
const fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Int(_) => "an integer",
        Value::Float(_) => "a number",
        Value::String(_) => "a string",
        Value::Sequence(_) => "a list",
        Value::Mapping(_) => "a mapping",
    }
}

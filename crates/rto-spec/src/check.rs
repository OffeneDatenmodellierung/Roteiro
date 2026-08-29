//! Drift checking: validate the authored layer (ADR wiki-links and `@rto:`
//! annotations) against the derived code graph, and weave the valid links in as
//! `authored` edges.
//!
//! [`run`] expects `store` to already hold the derived graph (symbols, files).
//! It applies each ADR's structural nodes, then for every authored link checks
//! that its target exists — reporting a [`Violation`] when it does not — and
//! adds an `authored` edge when it does.
//!
//! It also guards the authored layer's own integrity: ADR ids are node keys, so
//! two ADRs claiming one id silently discard a decision. See
//! [`duplicate_adr_ids`].

use std::collections::{BTreeMap, BTreeSet};

use rto_graph::{Edge, EdgeKind, Store, StoreError};
use serde::Serialize;

use crate::adr::{AdrDoc, AdrStatus};
use crate::annotate::Annotation;
use crate::blueprint::BlueprintDoc;
use crate::layer::AuthoredDocs;
use crate::site::SitePage;

/// The category of an authored-layer drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ViolationKind {
    /// An ADR under `docs/adr` could not be parsed.
    MalformedAdr,
    /// An ADR `[[…]]` link points at a symbol or file not in the graph.
    BrokenLink,
    /// A `@rto:` annotation references an ADR that does not exist.
    UnknownAdr,
    /// A `@rto:` annotation references a rejected or superseded ADR.
    InactiveAdr,
    /// Two or more ADR files declare the same `adr-id`.
    DuplicateAdrId,
    /// An ADR's version metadata disagrees with itself.
    AdrVersionDrift,
    /// A document declared itself a published site page but could not be parsed
    /// as one.
    MalformedSitePage,
    /// Two or more documents declare the same `site-page` slug.
    DuplicateSiteSlug,
    /// A `#[allow(…)]` with no justification comment — the house rule `AGENTS.md`
    /// states and nothing checked (issue #438, corpus row `3789168273`).
    ///
    /// Its **own** variant rather than a message under an existing one. The ADR
    /// version family took the other road deliberately — five checks behind
    /// `AdrVersionDrift`, told apart by their messages — because they are five
    /// readings of one artifact's self-consistency. This is not one of those: it
    /// is a different artifact (Rust source), a different question (a written
    /// convention), and a consumer filtering for it should branch on the kind
    /// rather than match prose.
    UnjustifiedAllow,
    /// A lossy string conversion feeding a value that must be unique.
    ///
    /// `to_string_lossy` replaces every invalid byte sequence with `U+FFFD`, so
    /// two inputs that differ only in those bytes become one string. Harmless in
    /// a message; a defect when the result is hashed or used as an identity,
    /// because two distinct things then collapse into one and the later silently
    /// replaces the earlier.
    LossyIdentity,
}

impl ViolationKind {
    /// A short stable label for this kind.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::MalformedAdr => "malformed-adr",
            Self::BrokenLink => "broken-link",
            Self::UnknownAdr => "unknown-adr",
            Self::InactiveAdr => "inactive-adr",
            Self::DuplicateAdrId => "duplicate-adr-id",
            Self::AdrVersionDrift => "adr-version-drift",
            Self::MalformedSitePage => "malformed-site-page",
            Self::DuplicateSiteSlug => "duplicate-site-slug",
            Self::UnjustifiedAllow => "unjustified-allow",
            Self::LossyIdentity => "lossy-identity",
        }
    }
}

/// A single authored-layer drift finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Violation {
    /// What kind of drift this is.
    pub kind: ViolationKind,
    /// A human-readable, location-prefixed message.
    pub message: String,
}

/// The outcome of a [`run`]: how much authored content was checked and any
/// drift found.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CheckReport {
    /// Number of ADRs parsed and applied.
    pub adrs: usize,
    /// Number of blueprints parsed and applied.
    pub blueprints: usize,
    /// Number of published site pages parsed and applied.
    pub site_pages: usize,
    /// Authored `[[…]]` links that resolved and became edges.
    pub links_ok: usize,
    /// `@rto:` annotations that resolved to an active ADR.
    pub annotations_ok: usize,
    /// Drift findings; the check fails if this is non-empty.
    pub violations: Vec<Violation>,
}

impl CheckReport {
    /// Whether any drift was found.
    #[must_use]
    pub fn has_violations(&self) -> bool {
        !self.violations.is_empty()
    }
}

/// Find `adr-id` values claimed by more than one ADR file.
///
/// An ADR's node key is `adr:<id>` ([`AdrDoc::key`]), so this collision is not
/// cosmetic — it is *lossy*. Two files sharing an id produce one node key, the
/// later [`Store::apply_factset`] overwrites the earlier, and from then on
/// `query adr:NNNN` answers for one decision while the other is invisible, every
/// `@rto:NNNN` annotation binds to whichever won, and the published artifact
/// carries the survivor alone. Nothing else in the pipeline notices: the two
/// files merge cleanly in git (they touch no common line) and every other check
/// passes. That is exactly how ADR-0016 came to be authored twice on two
/// parallel branches in this repository.
///
/// The message names **both** paths and the id: an id alone leaves the reader to
/// hunt for the partner file, which is the work this check exists to save.
///
/// The same collision class does *not* exist for the other keyed documents,
/// because their ids are their paths, and a tree cannot hold two files at one
/// path: blueprints are `blueprint:<path>` ([`BlueprintDoc::key`]), `lat.md`
/// nodes are `lat:<path>`, files are `file:<path>` and symbols are
/// `sym:<lang>:<path>#<symbol>`. Imported Graphify nodes (`graphify:<id>`) do
/// carry an author-chosen id, but importing is an explicit, single-document act
/// whose merge semantics are deliberate rather than accidental, and hyperedges
/// are already namespaced away from nodes to prevent exactly this clobber.
/// Multi-repo workspaces hold one [`Store`] per project, so ids collide only
/// within a repository, never across one.
fn duplicate_adr_ids(docs: &[AdrDoc]) -> Vec<Violation> {
    let mut by_id: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for doc in docs {
        by_id
            .entry(doc.meta.id.as_str())
            .or_default()
            .push(doc.path.as_str());
    }
    by_id
        .into_iter()
        .filter(|(_, paths)| paths.len() > 1)
        .map(|(id, mut paths)| {
            // Sort so the message is stable whatever order the tree walk yielded.
            paths.sort_unstable();
            Violation {
                kind: ViolationKind::DuplicateAdrId,
                message: format!(
                    "adr-id {id} is declared by {} files: {} — all of them collapse \
                     into the single node `adr:{id}`, so only one decision survives \
                     and every @rto:{id} annotation binds to it",
                    paths.len(),
                    paths.join(", "),
                ),
            }
        })
        .collect()
}

/// Find `site-page` slugs claimed by more than one document.
///
/// Exactly [`duplicate_adr_ids`]'s failure, in the one other place this
/// repository lets an author choose a key. A site page's node key is
/// `site:<slug>` and its published filename is `<slug>.html`, so two documents
/// claiming one slug collapse twice over: the later `apply_factset` overwrites
/// the earlier node, and the later write to `<slug>.html` overwrites the earlier
/// page. The site then serves one document at an address the other one also
/// claims, and nothing else notices — the two files merge cleanly in git, and
/// every other check passes. That is the ADR-0016 story with a public URL
/// attached.
///
/// The message names **both** paths and the slug, for the reason
/// [`duplicate_adr_ids`] does: a slug alone leaves the reader to hunt for the
/// partner file, which is the work this check exists to save.
fn duplicate_site_slugs(pages: &[SitePage]) -> Vec<Violation> {
    let mut by_slug: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for page in pages {
        by_slug
            .entry(page.slug.as_str())
            .or_default()
            .push(page.path.as_str());
    }
    by_slug
        .into_iter()
        .filter(|(_, paths)| paths.len() > 1)
        .map(|(slug, mut paths)| {
            // Sort so the message is stable whatever order the tree walk yielded.
            paths.sort_unstable();
            Violation {
                kind: ViolationKind::DuplicateSiteSlug,
                message: format!(
                    "site-page slug `{slug}` is declared by {} files: {} — all of \
                     them collapse into the single node `site:{slug}` and the single \
                     published page `{slug}.html`, so only one document survives",
                    paths.len(),
                    paths.join(", "),
                ),
            }
        })
        .collect()
}

/// Find ADRs whose version metadata contradicts itself.
///
/// An ADR states its version in three places, and nothing until now compared
/// them. Three real defects were found by hand in this repository on
/// 2026-08-18, all of this shape, while `check` reported 0 violations: ADR-0001
/// carried frontmatter `1.2` over a summary row reading `1.0` (#406); ADR-0006
/// listed 1.3 above 1.2 in its history and carried an inline note citing
/// `(Update, v1.5)`, a version it has never had (#413). The third is the worst
/// of them — `git log -S` put the change that note describes at 2026-08-14,
/// when the document was at 1.1, and it never got a history row at all. A
/// version claim nobody checks is a claim that quietly stops being true.
///
/// Five contradictions, reported under one kind because a caller does the same
/// thing with all of them — fail the gate and print the message — and because
/// the message, not the label, is what tells the reader which one fired. This
/// follows [`ViolationKind::MalformedAdr`], which likewise covers every
/// [`crate::adr::ParseError`] behind one label and puts the specifics in prose.
/// It is also why rules 4 and 5 (issue #432) needed no new variant, and so
/// reopened no part of the semver question this enum's exhaustiveness raises.
///
/// Each message names the file and **both** conflicting values, for the reason
/// [`duplicate_adr_ids`] names both paths: a message that reports only what it
/// found leaves the reader to hunt for what it was compared against.
///
/// # Why rules 4 and 5 are separate rules rather than widenings
///
/// Rule 1 compares the frontmatter to the **summary row**, and rule 4 compares
/// it to the **history**. That looks redundant until you see how the defect
/// actually happens: the row and the frontmatter are usually forgotten in the
/// *same* edit, so they agree with each other and both lag the history. Rule 1
/// is silent on exactly the documents rule 4 catches — ADR-0009 sat at 1.10
/// over a history reaching 1.11, and ADR-0014 at 1.4 over 1.5. Both were found
/// by hand while rule 1 was being written, by the rule's own author, with rule 1
/// passing.
///
/// Rule 5 is **one-directional on purpose**, and getting that backwards would be
/// worse than not having it. `last-modified` running *ahead* of the newest
/// history row is legitimate — a typo fix or a link repair need not earn a row —
/// so an equality rule would fire on every small edit and be switched off within
/// a week. Only the impossible direction is checked: a document cannot have been
/// last modified before a change it itself lists. When it was measured, eight of
/// this repository's twenty ADRs lagged, three of them put there by people
/// actively repairing this same family.
///
/// Still deliberately *not* checked, because widening a rule that returns one
/// hit or none is how it becomes a rule nobody reads: that a `version:`, a
/// summary row or a history table exists at all. The contradiction is the
/// finding, and ADR-0011 legitimately has no history.
fn adr_version_drift(docs: &[AdrDoc]) -> Vec<Violation> {
    let mut out = Vec::new();
    for doc in docs {
        let path = &doc.path;
        let facts = &doc.versions;

        // 1. The two places a *current* version is written must agree. This is
        //    the pair ADR-0001 got wrong; a reader trusting frontmatter and a
        //    reader trusting the rendered table came away with different answers.
        if let (Some(front), Some(row)) = (doc.meta.version, facts.summary_row)
            && front != row
        {
            out.push(Violation {
                kind: ViolationKind::AdrVersionDrift,
                message: format!(
                    "{path}: frontmatter says version {front} but the summary \
                     table's **Document version** row says {row}"
                ),
            });
        }

        // 2. The history is a sequence, so it has to read as one. Compared
        //    component-wise: 1.10 follows 1.9, and any ordering that puts it
        //    first would report this repository's longest-running ADR as broken.
        for pair in facts.history.windows(2) {
            let (prev, next) = (pair[0].version, pair[1].version);
            if next > prev {
                continue;
            }
            let why = if next == prev {
                "twice"
            } else {
                "out of order"
            };
            out.push(Violation {
                kind: ViolationKind::AdrVersionDrift,
                message: format!(
                    "{path}: version history lists {next} after {prev} — {why}; the \
                     rows must ascend so the document reads as its own changelog"
                ),
            });
        }

        // 3. A note citing a version the history never recorded describes a
        //    change the document cannot account for. Skipped when there is no
        //    history table: an absent table contradicts nothing, and requiring
        //    one is the fourth rule this deliberately is not.
        if facts.history.is_empty() {
            continue;
        }
        for reference in &facts.inline_refs {
            if facts.history.iter().any(|r| r.version == reference.version) {
                continue;
            }
            let known = facts
                .history
                .iter()
                .map(|r| r.version.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            out.push(Violation {
                kind: ViolationKind::AdrVersionDrift,
                message: format!(
                    "{path}:{}: an inline note cites (Update, v{}), a version this \
                     document has never had — its history records {known}",
                    reference.line, reference.version
                ),
            });
        }

        // 4. The frontmatter must have kept up with the history. A row is added
        //    by the person making the change; `version:` is a second place the
        //    same fact is written, and it is the one that gets forgotten —
        //    ADR-0009 sat at 1.10 over a history reaching 1.11, ADR-0014 at 1.4
        //    over 1.5, both found while rule 1 was being written and neither
        //    caught by it, because rule 1 compares frontmatter to the *summary
        //    row* and the summary row was forgotten in the same edit.
        //
        //    Compared against the highest row rather than the last one so this
        //    still reports honestly on a document rule 2 has already failed:
        //    with the rows out of order, "the last row" is not the version the
        //    document has reached.
        if let (Some(front), Some(highest)) = (
            doc.meta.version,
            facts.history.iter().map(|r| r.version).max(),
        ) && front != highest
        {
            out.push(Violation {
                kind: ViolationKind::AdrVersionDrift,
                message: format!(
                    "{path}: frontmatter says version {front} but the version \
                     history reaches {highest} — the frontmatter was not bumped \
                     with the row that records the change"
                ),
            });
        }

        // 5. `last-modified` must not predate a change the document itself
        //    records. **One-directional on purpose.** Running *ahead* of the
        //    newest row is legitimate — a typo fix or a link repair need not
        //    earn a history row — so an equality rule would fire on every small
        //    edit and be switched off within a week. Only the impossible
        //    direction is a violation: the document cannot have last been
        //    modified before a change it lists.
        //
        //    Rows without a parseable date are skipped rather than guessed at;
        //    a row reading `TBD` makes no claim to contradict.
        if let (Some(modified), Some(newest)) = (
            doc.meta.last_modified,
            facts.history.iter().filter_map(|r| r.date).max(),
        ) && modified < newest
        {
            out.push(Violation {
                kind: ViolationKind::AdrVersionDrift,
                message: format!(
                    "{path}: frontmatter last-modified is {modified} but the \
                     version history records a change on {newest} — a document \
                     cannot have been last modified before a change it lists"
                ),
            });
        }
    }
    out
}

/// The outcome of a read-only [`validate`]: the report, plus the `authored`
/// edges the valid links and annotations *would* weave into the graph.
///
/// Splitting the edges out of the report is what lets one violation definition
/// serve both a gate that writes ([`run`]) and a tool surface that must not
/// ([`crate::tool_check`]). Nothing decides what counts as drift twice.
#[derive(Debug, Clone, Default)]
pub struct Validation {
    /// What was checked and what drifted.
    pub report: CheckReport,
    /// The `authored` `references` edges the resolved links and annotations
    /// imply. [`run`] inserts these; a read-only caller discards them.
    pub edges: Vec<Edge>,
}

/// The nodes the authored layer *would* contribute, and each authored ADR's
/// parsed `status`.
///
/// [`run`] applies these to the store before validating, so its `get_node`
/// lookups see them. [`validate`] must reach the same verdict without writing,
/// so it consults this overlay first and the store second — the keys come from
/// the very same [`AdrDoc::facts`]/[`BlueprintDoc::facts`]/[`SitePage::facts`]
/// sets `run` applies, so the two cannot disagree about what the authored layer
/// contributes.
///
/// Later docs overwrite earlier ones, matching `apply_factset`'s
/// last-writer-wins — which is exactly the lossiness [`duplicate_adr_ids`]
/// reports separately.
#[derive(Debug, Default)]
struct AuthoredOverlay {
    /// Every node key the authored layer contributes (ADRs, ADR sections,
    /// blueprints, site pages, and all of their sections).
    keys: BTreeSet<String>,
    /// Parsed status per ADR node key.
    adr_status: BTreeMap<String, AdrStatus>,
}

fn authored_overlay(
    docs: &[AdrDoc],
    blueprints: &[BlueprintDoc],
    site: &[SitePage],
) -> AuthoredOverlay {
    let mut overlay = AuthoredOverlay::default();
    for doc in docs {
        overlay
            .keys
            .extend(doc.facts().nodes.into_iter().map(|n| n.key));
        overlay.adr_status.insert(doc.key(), doc.meta.status);
    }
    for bp in blueprints {
        overlay
            .keys
            .extend(bp.facts().nodes.into_iter().map(|n| n.key));
    }
    for page in site {
        overlay
            .keys
            .extend(page.facts().nodes.into_iter().map(|n| n.key));
    }
    overlay
}

/// Validate the authored layer against the derived graph **without writing
/// anything**, returning the report and the edges a writing caller should weave.
///
/// This is the whole of the drift rule. [`run`] is this function plus the two
/// writes it deliberately leaves out (applying the ADR/blueprint structure, and
/// inserting the returned edges), so the CLI gate and the read-only tool surfaces
/// cannot drift apart in what they call a violation.
///
/// # Errors
/// Returns [`StoreError`] if querying the store fails.
pub fn validate(
    store: &Store,
    docs: &[AdrDoc],
    blueprints: &[BlueprintDoc],
    annotations: &[Annotation],
) -> Result<Validation, StoreError> {
    validate_all(store, docs, blueprints, &[], annotations)
}

/// [`validate`] over a whole [`AuthoredDocs`] — the same verdict, plus the
/// **site pages** the three-slice form has no parameter for.
///
/// Two entry points rather than one, because the classification that produces
/// site pages ([`crate::authored_layer_from`]) and the CLI gate that consumes
/// them land in separate changes: a caller still passing three slices keeps
/// compiling and keeps getting exactly today's verdict, and moves to this
/// function when it is ready to check the website too. The shared body below is
/// the only copy of the rule, so the two cannot drift into disagreeing about
/// what a violation is.
///
/// # Errors
/// Returns [`StoreError`] if querying the store fails.
pub fn validate_layer(store: &Store, docs: &AuthoredDocs) -> Result<Validation, StoreError> {
    validate_all(
        store,
        &docs.layer.docs,
        &docs.layer.blueprints,
        &docs.site,
        &docs.layer.annotations,
    )
}

/// The whole drift rule, over every authored document class. [`validate`] and
/// [`validate_layer`] are this function with and without site pages.
fn validate_all(
    store: &Store,
    docs: &[AdrDoc],
    blueprints: &[BlueprintDoc],
    site: &[SitePage],
    annotations: &[Annotation],
) -> Result<Validation, StoreError> {
    // 1. Detect colliding ADR ids *before* anything is applied, so the report
    //    describes the authored file set rather than what survived the merge.
    let mut report = CheckReport {
        adrs: docs.len(),
        blueprints: blueprints.len(),
        site_pages: site.len(),
        violations: duplicate_adr_ids(docs),
        ..CheckReport::default()
    };
    // The same collision in the one other place an author picks a key.
    report.violations.extend(duplicate_site_slugs(site));
    // Self-contradiction inside one ADR, checked alongside the collision
    // *between* ADRs above: neither needs the graph, both are read off the
    // authored files exactly as they were parsed.
    report.violations.extend(adr_version_drift(docs));
    let overlay = authored_overlay(docs, blueprints, site);
    let mut edges = Vec::new();

    // 2. Validate ADR, blueprint and site-page `[[…]]` links against the code
    //    graph. All three author `references` edges into real symbols/files and
    //    drift the same way — which is the point of making the website a
    //    document class rather than a pile of hand-written HTML: a page that
    //    describes `security run`'s isolation posture can cite the code that
    //    implements it, and the citation fails the gate when the code moves.
    let links = docs
        .iter()
        .flat_map(|d| &d.links)
        .chain(blueprints.iter().flat_map(|b| &b.links))
        .chain(site.iter().flat_map(|p| &p.links));
    for link in links {
        // A link resolves against the derived graph, or against an ADR the
        // authored layer is contributing in this same pass.
        if store.get_node(&link.target_key)?.is_some() || overlay.keys.contains(&link.target_key) {
            edges.push(Edge::authored(
                link.from.clone(),
                link.target_key.clone(),
                EdgeKind::References,
            ));
            report.links_ok += 1;
        } else {
            report.violations.push(Violation {
                kind: ViolationKind::BrokenLink,
                message: format!(
                    "{}: authored link [[{}]] does not resolve ({} not found in graph)",
                    link.from, link.raw, link.target_key
                ),
            });
        }
    }

    // 3. Validate `@rto:` annotations against ADR state. The overlay is consulted
    //    first: an ADR authored in this pass is the one the annotation means, and
    //    its parsed status is what `run` would have written to the node.
    for ann in annotations {
        let key = ann.target_key();
        let status = match overlay.adr_status.get(&key) {
            Some(status) => Some(*status),
            None => match store.get_node(&key)? {
                Some(adr) => Some(
                    adr.meta
                        .get("status")
                        .and_then(|s| s.as_str())
                        .and_then(|s| s.parse::<AdrStatus>().ok())
                        // A node with an unparseable status still *exists*, so it
                        // is not `unknown-adr`; treat it as active, exactly as the
                        // pre-split code did by leaving `status` at `None`.
                        .unwrap_or(AdrStatus::Accepted),
                ),
                None => None,
            },
        };
        let Some(status) = status else {
            report.violations.push(Violation {
                kind: ViolationKind::UnknownAdr,
                message: format!(
                    "{}:{}: @rto:{} references unknown ADR",
                    ann.path, ann.line, ann.adr_id
                ),
            });
            continue;
        };
        if !status.is_active() {
            report.violations.push(Violation {
                kind: ViolationKind::InactiveAdr,
                message: format!(
                    "{}:{}: @rto:{} references non-active ADR ({})",
                    ann.path,
                    ann.line,
                    ann.adr_id,
                    status.as_str()
                ),
            });
            continue;
        }
        // Link the annotated file to the ADR when the file is in the graph.
        let file_key = format!("file:{}", ann.path);
        if store.get_node(&file_key)?.is_some() {
            edges.push(Edge::authored(file_key, key, EdgeKind::References));
        }
        report.annotations_ok += 1;
    }

    Ok(Validation { report, edges })
}

/// Apply the authored layer to `store` and validate it against the derived
/// graph, returning a [`CheckReport`].
///
/// The verdict itself comes from [`validate`]; this function is the writing half
/// around it — materialising ADR/blueprint structure so links can reference it,
/// and weaving the resolved links in as `authored` edges.
///
/// # Errors
/// Returns [`StoreError`] if applying ADR facts or edges, or querying the
/// store, fails.
pub fn run(
    store: &mut Store,
    docs: &[AdrDoc],
    blueprints: &[BlueprintDoc],
    annotations: &[Annotation],
) -> Result<CheckReport, StoreError> {
    run_all(store, docs, blueprints, &[], annotations)
}

/// [`run`] over a whole [`AuthoredDocs`], including its **site pages**. See
/// [`validate_layer`] for why both entry points exist.
///
/// # Errors
/// Returns [`StoreError`] if applying authored facts or edges, or querying the
/// store, fails.
pub fn run_layer(store: &mut Store, docs: &AuthoredDocs) -> Result<CheckReport, StoreError> {
    run_all(
        store,
        &docs.layer.docs,
        &docs.layer.blueprints,
        &docs.site,
        &docs.layer.annotations,
    )
}

/// The writing half, over every authored document class.
fn run_all(
    store: &mut Store,
    docs: &[AdrDoc],
    blueprints: &[BlueprintDoc],
    site: &[SitePage],
    annotations: &[Annotation],
) -> Result<CheckReport, StoreError> {
    // Materialise ADR/blueprint/site-page section nodes so links and annotations
    // can reference them (and so `@rto:` targets can be looked up by key).
    for doc in docs {
        store.apply_factset(&doc.facts())?;
    }
    for bp in blueprints {
        store.apply_factset(&bp.facts())?;
    }
    for page in site {
        store.apply_factset(&page.facts())?;
    }

    let validation = validate_all(store, docs, blueprints, site, annotations)?;
    for edge in &validation.edges {
        store.insert_edge(edge)?;
    }
    Ok(validation.report)
}

#[cfg(test)]
mod tests {
    use super::{ViolationKind, run, run_layer};
    use crate::adr::parse_adr;
    use crate::annotate::scan_annotations;
    use crate::layer::{AuthoredDocs, AuthoredLayer};
    use crate::site::parse_site_page;
    use rto_graph::{Node, NodeKind, Store};

    /// An [`AuthoredDocs`] holding only site pages — the rest of the authored
    /// layer is exercised by the tests above.
    fn site_layer(pages: Vec<crate::site::SitePage>) -> AuthoredDocs {
        AuthoredDocs {
            site: pages,
            ..AuthoredDocs::default()
        }
    }

    fn seed_graph(store: &Store) {
        // A tiny derived graph: one file and one symbol.
        store
            .upsert_node(&Node::new("file:src/store.rs", NodeKind::File, "store.rs"))
            .expect("file");
        store
            .upsert_node(&Node::new(
                "sym:rust:src/store.rs#Store",
                NodeKind::Struct,
                "Store",
            ))
            .expect("sym");
    }

    #[test]
    fn resolvable_links_and_annotations_pass() {
        let mut store = Store::open_in_memory().expect("store");
        seed_graph(&store);

        let adr = "---\nadr-id: \"0001\"\nstatus: Accepted\n---\n\n# ADR-0001\n\n## Design\n\nUses [[src/store.rs#Store]].\n";
        let doc = parse_adr("docs/adr/0001.md", adr).expect("parse");
        let anns = scan_annotations("src/store.rs", "//! @rto:0001\n");

        let report = run(&mut store, &[doc], &[], &anns).expect("run");
        assert!(!report.has_violations(), "{:?}", report.violations);
        assert_eq!(report.links_ok, 1);
        assert_eq!(report.annotations_ok, 1);
        // The authored edge is now in the graph.
        let edges = store.edges_from("adr:0001#design").expect("edges");
        assert!(edges.iter().any(|e| e.dst == "sym:rust:src/store.rs#Store"));
    }

    #[test]
    fn a_site_page_s_links_are_drift_checked_like_an_adr_s() {
        // The whole point of the document class: the public website's claims are
        // held against the graph, so a page that cites the code it describes
        // fails the gate when that code moves.
        let mut store = Store::open_in_memory().expect("store");
        seed_graph(&store);
        let ok = parse_site_page(
            "docs/site/modes.md",
            "---\nsite-page: modes\n---\n\n# Modes\n\n## Offline\n\nSee [[src/store.rs#Store]].\n",
        )
        .expect("parse");
        let report = run_layer(&mut store, &site_layer(vec![ok])).expect("run");
        assert!(!report.has_violations(), "{:?}", report.violations);
        assert_eq!(report.site_pages, 1);
        assert_eq!(report.links_ok, 1);
        // The authored edge is in the graph, attributed to the page's section.
        let edges = store.edges_from("site:modes#offline").expect("edges");
        assert!(edges.iter().any(|e| e.dst == "sym:rust:src/store.rs#Store"));

        // The failing half — this is what would have caught the stale
        // `--allow-unsandboxed` claim the hand-written page carried.
        let mut store = Store::open_in_memory().expect("store");
        seed_graph(&store);
        let stale = parse_site_page(
            "docs/site/modes.md",
            "---\nsite-page: modes\n---\n\n# Modes\n\nSee [[src/store.rs#Ghost]].\n",
        )
        .expect("parse");
        let report = run_layer(&mut store, &site_layer(vec![stale])).expect("run");
        assert_eq!(report.violations.len(), 1);
        assert_eq!(report.violations[0].kind, ViolationKind::BrokenLink);
    }

    #[test]
    fn two_pages_sharing_a_slug_are_a_violation_naming_both_files() {
        // `duplicate_adr_ids` with a public URL attached: one node key and one
        // published filename, so the later document silently replaces the first.
        let mut store = Store::open_in_memory().expect("store");
        seed_graph(&store);
        let one = parse_site_page(
            "docs/site/config.md",
            "---\nsite-page: config\n---\n\n# Configuration\n",
        )
        .expect("one");
        let two = parse_site_page(
            "docs/OFFLINE_SETUP.md",
            "---\nsite-page: config\n---\n\n# Offline setup\n",
        )
        .expect("two");
        let report = run_layer(&mut store, &site_layer(vec![one, two])).expect("run");
        let dupes: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.kind == ViolationKind::DuplicateSiteSlug)
            .collect();
        assert_eq!(dupes.len(), 1, "one finding for the one colliding slug");
        let msg = &dupes[0].message;
        assert!(msg.contains("config"), "names the slug: {msg}");
        assert!(
            msg.contains("docs/site/config.md"),
            "names the first: {msg}"
        );
        assert!(
            msg.contains("docs/OFFLINE_SETUP.md"),
            "names the second: {msg}"
        );
    }

    #[test]
    fn the_three_slice_entry_point_still_reaches_the_same_verdict_today() {
        // `run` is `run_layer` with no site pages. A caller that has not moved
        // over must see exactly the report it sees today — that is the whole
        // reason both entry points exist.
        let mut a = Store::open_in_memory().expect("store");
        seed_graph(&a);
        let mut b = Store::open_in_memory().expect("store");
        seed_graph(&b);
        let adr = "---\nadr-id: \"0001\"\nstatus: Accepted\n---\n\n# ADR-0001\n\n## Design\n\nUses [[src/store.rs#Store]].\n";
        let doc = parse_adr("docs/adr/0001.md", adr).expect("parse");

        let old = run(&mut a, std::slice::from_ref(&doc), &[], &[]).expect("run");
        let new = run_layer(
            &mut b,
            &AuthoredDocs {
                layer: AuthoredLayer {
                    docs: vec![doc],
                    ..AuthoredLayer::default()
                },
                ..AuthoredDocs::default()
            },
        )
        .expect("run_layer");
        assert_eq!(old.adrs, new.adrs);
        assert_eq!(old.links_ok, new.links_ok);
        assert_eq!(old.violations.len(), new.violations.len());
        assert_eq!(old.site_pages, 0, "no site pages via the three-slice form");
        assert_eq!(new.site_pages, 0);
    }

    #[test]
    fn broken_link_is_a_violation() {
        let mut store = Store::open_in_memory().expect("store");
        seed_graph(&store);
        let adr =
            "---\nadr-id: \"0001\"\nstatus: Accepted\n---\n\n## Design\n\n[[src/store.rs#Ghost]]\n";
        let doc = parse_adr("docs/adr/0001.md", adr).expect("parse");

        let report = run(&mut store, &[doc], &[], &[]).expect("run");
        assert_eq!(report.violations.len(), 1);
        assert_eq!(report.violations[0].kind, ViolationKind::BrokenLink);
    }

    #[test]
    fn two_adrs_sharing_an_id_are_a_violation_naming_both_files() {
        // The regression from issue #324: two branches each author ADR-0016.
        // Both files merge cleanly, both parse, and both apply to the *same*
        // node key — so without this check the report is 0 violations.
        let mut store = Store::open_in_memory().expect("store");
        seed_graph(&store);
        let one = parse_adr(
            "docs/adr/0016-audio-metadata.md",
            "---\nadr-id: \"0016\"\nstatus: Accepted\n---\n\n# Audio metadata\n\n## Decision\n\nbody\n",
        )
        .expect("parse one");
        let two = parse_adr(
            "docs/adr/0016-speculative-decoding.md",
            "---\nadr-id: \"0016\"\nstatus: Accepted\n---\n\n# Speculative decoding\n\n## Decision\n\nbody\n",
        )
        .expect("parse two");

        let report = run(&mut store, &[one, two], &[], &[]).expect("run");
        let dupes: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.kind == ViolationKind::DuplicateAdrId)
            .collect();
        assert_eq!(dupes.len(), 1, "one finding for the one colliding id");
        // Both paths and the id must be named — an id alone makes the reader hunt.
        let msg = &dupes[0].message;
        assert!(msg.contains("0016"), "names the shared id: {msg}");
        assert!(
            msg.contains("docs/adr/0016-audio-metadata.md"),
            "names the first file: {msg}"
        );
        assert!(
            msg.contains("docs/adr/0016-speculative-decoding.md"),
            "names the second file: {msg}"
        );
        assert!(report.has_violations(), "the gate must fail");
    }

    #[test]
    fn distinct_adr_ids_are_not_a_duplicate_violation() {
        let mut store = Store::open_in_memory().expect("store");
        seed_graph(&store);
        let one = parse_adr(
            "docs/adr/0001-a.md",
            "---\nadr-id: \"0001\"\nstatus: Accepted\n---\n\n# A\n\n## Decision\n\nbody\n",
        )
        .expect("parse one");
        let two = parse_adr(
            "docs/adr/0002-b.md",
            "---\nadr-id: \"0002\"\nstatus: Accepted\n---\n\n# B\n\n## Decision\n\nbody\n",
        )
        .expect("parse two");

        let report = run(&mut store, &[one, two], &[], &[]).expect("run");
        assert!(!report.has_violations(), "{:?}", report.violations);
    }

    #[test]
    fn three_files_on_one_id_report_once_and_name_all_three() {
        let mut store = Store::open_in_memory().expect("store");
        seed_graph(&store);
        let docs: Vec<_> = ["c.md", "a.md", "b.md"]
            .iter()
            .map(|name| {
                parse_adr(
                    &format!("docs/adr/{name}"),
                    "---\nadr-id: \"0007\"\nstatus: Accepted\n---\n\n# X\n\n## Decision\n\nbody\n",
                )
                .expect("parse")
            })
            .collect();

        let report = run(&mut store, &docs, &[], &[]).expect("run");
        assert_eq!(report.violations.len(), 1, "one finding, not one per file");
        let msg = &report.violations[0].message;
        // Paths are sorted, so the message does not depend on tree-walk order.
        assert!(
            msg.contains("docs/adr/a.md, docs/adr/b.md, docs/adr/c.md"),
            "names all three in a stable order: {msg}"
        );
    }

    #[test]
    fn annotation_to_unknown_and_superseded_adrs() {
        let mut store = Store::open_in_memory().expect("store");
        seed_graph(&store);
        let superseded =
            "---\nadr-id: \"0002\"\nstatus: Superseded\n---\n\n# Old\n\n## X\n\nbody\n";
        let doc = parse_adr("docs/adr/0002.md", superseded).expect("parse");
        let anns = scan_annotations("src/store.rs", "// @rto:0002\n// @rto:9999\n");

        let report = run(&mut store, &[doc], &[], &anns).expect("run");
        let kinds: Vec<_> = report.violations.iter().map(|v| v.kind).collect();
        assert!(kinds.contains(&ViolationKind::InactiveAdr));
        assert!(kinds.contains(&ViolationKind::UnknownAdr));
        assert_eq!(report.annotations_ok, 0);
    }

    /// A clean ADR carrying all three version claims in agreement, used as the
    /// base each test below injects exactly one defect into.
    const VERSIONED: &str = "\
---
adr-id: \"0006\"
status: Accepted
version: \"1.4\"
---

# ADR-0006

| Field | Value |
|---|---|
| **Document version** | 1.4 |

## Consequences

The server moved. *(Update, v1.2: it moved again.)*

Taken with `axum` v1.13.0, and boxlite v0.9.7 alongside it.

## Document version history

| Version | Date | Notes |
|---------|------|-------|
| 1.0 | 2026-08-09 | Accepted. |
| 1.1 | 2026-08-09 | Revised. |
| 1.2 | 2026-08-15 | Consequence added. |
| 1.4 | 2026-08-18 | HTTP/2 is a non-goal. |
";

    fn drift(adr: &str) -> Vec<String> {
        let mut store = Store::open_in_memory().expect("store");
        seed_graph(&store);
        let doc = parse_adr("docs/adr/0006-local-model-serving.md", adr).expect("parse");
        let report = run(&mut store, &[doc], &[], &[]).expect("run");
        report
            .violations
            .into_iter()
            .inspect(|v| assert_eq!(v.kind, ViolationKind::AdrVersionDrift, "{}", v.message))
            .map(|v| v.message)
            .collect()
    }

    #[test]
    fn a_self_consistent_adr_reports_nothing() {
        assert!(drift(VERSIONED).is_empty());
    }

    #[test]
    fn frontmatter_disagreeing_with_the_summary_row_is_a_violation() {
        // ADR-0001's defect, fixed by #406: frontmatter said 1.2 over a summary
        // row still reading 1.0, so the answer depended on which one you read.
        let msg = &drift(&VERSIONED.replace("version: \"1.4\"", "version: \"1.2\""))[0];
        assert!(msg.contains("0006-local-model-serving.md"), "{msg}");
        assert!(msg.contains("frontmatter says version 1.2"), "{msg}");
        assert!(msg.contains("row says 1.4"), "{msg}");
    }

    #[test]
    fn history_rows_out_of_order_are_a_violation() {
        // ADR-0006's defect, fixed by #413: the table listed 1.3 above 1.2.
        let swapped = VERSIONED.replace(
            "| 1.1 | 2026-08-09 | Revised. |",
            "| 1.3 | 2026-08-09 | Revised. |",
        );
        let msg = &drift(&swapped)[0];
        assert!(msg.contains("lists 1.2 after 1.3"), "{msg}");
        assert!(msg.contains("out of order"), "{msg}");
    }

    #[test]
    fn a_version_listed_twice_is_a_violation() {
        // ADR-0017 carried two different rows both labelled 1.2. Sorting cannot
        // fix that, so it is reported as its own thing rather than as disorder.
        let dup = VERSIONED.replace(
            "| 1.1 | 2026-08-09 | Revised. |",
            "| 1.0 | 2026-08-09 | Revised. |",
        );
        let msg = &drift(&dup)[0];
        assert!(msg.contains("lists 1.0 after 1.0"), "{msg}");
        assert!(msg.contains("twice"), "{msg}");
    }

    #[test]
    fn an_inline_note_citing_an_unrecorded_version_is_a_violation() {
        // ADR-0006's third defect, and the nastiest: a note citing (Update,
        // v1.5) for a change that landed while the document was at 1.1 and was
        // never given a history row at all. ADR-0002 carried the same note.
        let msg = &drift(&VERSIONED.replace("(Update, v1.2:", "(Update, v1.5:"))[0];
        assert!(msg.contains("0006-local-model-serving.md:15"), "{msg}");
        assert!(msg.contains("(Update, v1.5)"), "{msg}");
        assert!(msg.contains("never had"), "{msg}");
        assert!(msg.contains("1.0, 1.1, 1.2, 1.4"), "{msg}");
    }

    #[test]
    fn a_date_that_is_not_exactly_iso_8601_is_not_a_date() {
        use crate::adr::DocDate;

        // Exactly four-two-two, and nothing else.
        assert_eq!(
            DocDate::parse("2026-08-18"),
            Some(DocDate {
                year: 2026,
                month: 8,
                day: 18
            })
        );
        // A table cell arrives padded, so surrounding whitespace is trimmed
        // before the width is counted — that is not leniency about the format.
        assert!(DocDate::parse("  2026-08-18  ").is_some());

        for bad in [
            "2026-8-18",   // month not padded
            "2026-08-1",   // day not padded
            "26-08-18",    // two-digit year
            "20260-08-18", // five-digit year
            "2026-08-188", // three-digit day
            "2026/08/18",  // wrong separator
            "2026-08",     // no day
            "TBD",
            "",
        ] {
            assert_eq!(
                DocDate::parse(bad),
                None,
                "`{bad}` must not parse as a date"
            );
        }

        // Display round-trips exactly what a conforming file contains — which
        // is why the width is fixed. A lenient parser would accept `2026-8-1`
        // and then quote it back as `2026-08-01`, so a violation message would
        // name a date the document does not contain.
        assert_eq!(
            DocDate::parse("2026-08-18").unwrap().to_string(),
            "2026-08-18"
        );
    }

    #[test]
    fn frontmatter_lagging_the_history_is_a_violation() {
        // ADR-0009 (1.10 over a history reaching 1.11) and ADR-0014 (1.4 over
        // 1.5). The summary row is moved down **with** the frontmatter, because
        // that is how the defect really occurs — both are forgotten in one edit,
        // so they agree with each other and rule 1 stays silent. If rule 1 could
        // catch this, rule 4 would not be worth having.
        let lagged = VERSIONED
            .replace("version: \"1.4\"", "version: \"1.2\"")
            .replace(
                "| **Document version** | 1.4 |",
                "| **Document version** | 1.2 |",
            );
        let msgs = drift(&lagged);
        assert_eq!(msgs.len(), 1, "rule 1 must stay silent here: {msgs:?}");
        assert!(
            msgs[0].contains("frontmatter says version 1.2"),
            "{}",
            msgs[0]
        );
        assert!(msgs[0].contains("history reaches 1.4"), "{}", msgs[0]);
    }

    #[test]
    fn rule_four_compares_against_the_highest_row_not_the_last_one() {
        // On a document rule 2 has already failed, "the last row" is not the
        // version the document has reached — so comparing against it would
        // report a second, false contradiction on top of the real one.
        let out_of_order = VERSIONED.replace(
            "| 1.4 | 2026-08-18 | HTTP/2 is a non-goal. |",
            "| 1.4 | 2026-08-18 | HTTP/2 is a non-goal. |\n| 1.3 | 2026-08-19 | Later, lower. |",
        );
        let msgs = drift(&out_of_order);
        assert!(
            msgs.iter().any(|m| m.contains("out of order")),
            "rule 2 still fires: {msgs:?}"
        );
        assert!(
            !msgs.iter().any(|m| m.contains("history reaches")),
            "frontmatter 1.4 *is* the highest row, so rule 4 must not fire: {msgs:?}"
        );
    }

    #[test]
    fn last_modified_older_than_the_newest_history_row_is_a_violation() {
        let stale = VERSIONED.replace(
            "version: \"1.4\"",
            "version: \"1.4\"\nlast-modified: 2026-08-15",
        );
        let msgs = drift(&stale);
        assert_eq!(msgs.len(), 1, "{msgs:?}");
        assert!(
            msgs[0].contains("last-modified is 2026-08-15"),
            "{}",
            msgs[0]
        );
        assert!(msgs[0].contains("change on 2026-08-18"), "{}", msgs[0]);
    }

    #[test]
    fn last_modified_ahead_of_the_history_is_not_a_violation() {
        // The direction that makes this rule survivable. A typo fix or a link
        // repair moves `last-modified` and earns no history row; an equality
        // rule would fire on every one of them and be switched off within a
        // week. Only the impossible direction is a finding.
        let ahead = VERSIONED.replace(
            "version: \"1.4\"",
            "version: \"1.4\"\nlast-modified: 2026-09-30",
        );
        assert!(drift(&ahead).is_empty(), "{:?}", drift(&ahead));
    }

    #[test]
    fn a_history_row_without_a_parseable_date_is_skipped_not_guessed() {
        // The Date column is prose in practice — `TBD`, a range, an empty cell.
        // Such a row still counts for the ordering rules (it has a version), but
        // it makes no date claim, so rule 5 has nothing to contradict. Guessing
        // one would invent the finding.
        let tbd = VERSIONED
            .replace(
                "| 1.4 | 2026-08-18 | HTTP/2 is a non-goal. |",
                "| 1.4 | TBD | HTTP/2 is a non-goal. |",
            )
            .replace(
                "version: \"1.4\"",
                "version: \"1.4\"\nlast-modified: 2026-08-16",
            );
        // 2026-08-16 is older than the *undated* 1.4 row but newer than 1.2's
        // 2026-08-15, which is the newest row that actually states a date.
        assert!(drift(&tbd).is_empty(), "{:?}", drift(&tbd));
    }

    #[test]
    fn an_adr_with_no_last_modified_field_reports_nothing_for_rule_five() {
        // Rule 5 compares two claims. A document that makes only one of them
        // cannot contradict itself, and requiring the field is the rule this
        // deliberately is not.
        assert!(!VERSIONED.contains("last-modified"));
        assert!(drift(VERSIONED).is_empty());
    }

    #[test]
    fn software_versions_in_prose_are_not_document_versions() {
        // `v1.13.0` is a crate release and `v0.9.7` is boxlite's; a scan for a
        // bare `vX.Y` reads both as document versions this ADR has never had.
        // Over the 20 ADRs in this repository that scan matches 40+ times and
        // the `(Update, v` marker matches 4 — this is the whole precision gap.
        assert!(drift(VERSIONED).is_empty());
        let extra = VERSIONED.replace(
            "Taken with",
            "Released in v1.11.0 and v1.12.0, superseding v0.9. Taken with",
        );
        assert!(drift(&extra).is_empty(), "{:?}", drift(&extra));
    }

    #[test]
    fn a_history_row_quoting_a_bad_note_is_not_itself_one() {
        // The false positive this rule had to be built around. #413 recorded
        // its own fix by *quoting* the note it removed, so ADR-0006's history
        // contains the literal `(Update, v1.5)` — inside the history section,
        // which the scan therefore excludes.
        let quoting = VERSIONED.replace(
            "| 1.4 | 2026-08-18 | HTTP/2 is a non-goal. |",
            "| 1.4 | 2026-08-18 | An inline note cited *(Update, v1.5)*, now removed. |",
        );
        assert!(drift(&quoting).is_empty(), "{:?}", drift(&quoting));
    }

    #[test]
    fn ten_is_a_later_revision_than_nine() {
        // ADR-0009 reached 1.11 one row at a time. Lexical or decimal ordering
        // sorts 1.10 below 1.9 and reports the whole table as out of order.
        let long = VERSIONED.replace(
            "| 1.4 | 2026-08-18 | HTTP/2 is a non-goal. |",
            "| 1.9 | 2026-08-12 | Step 8b. |\n| 1.10 | 2026-08-12 | Step 8c. |\n| 1.11 | 2026-08-13 | Config keys. |",
        );
        let long = long.replace("version: \"1.4\"", "version: \"1.11\"");
        let long = long.replace(
            "| **Document version** | 1.4 |",
            "| **Document version** | 1.11 |",
        );
        assert!(drift(&long).is_empty(), "{:?}", drift(&long));
    }

    #[test]
    fn an_adr_with_no_history_table_is_not_a_violation() {
        // ADR-0011 has none. An absent table contradicts nothing.
        let none = VERSIONED
            .split("## Document version history")
            .next()
            .expect("body")
            .to_owned();
        assert!(drift(&none).is_empty(), "{:?}", drift(&none));
    }
}

//! Reading the **authored layer** out of a git tree: which files carry authored
//! intent, and what parsing each of them yields.
//!
//! This is one function ([`authored_layer`]) rather than a rule each caller
//! applies for itself, and that is the whole reason the module exists. The
//! authored file set must match the tree the *derived* layer was built from, and
//! the two disagreeing is issue #330 — a silent wrong answer, not a loud one.
//! [`rto_graph::GraphSource`] names the tree for both halves; this function is
//! the authored half of that pairing.
//!
//! It reads a git tree and parses text. It touches no [`Store`](rto_graph::Store)
//! and writes nothing, so a read-only surface can call it — which is what
//! [`crate::tool_check`] does.

use rto_graph::{BlobRef, GitError, GraphSource, Repo};

use crate::adr::AdrDoc;
use crate::annotate::Annotation;
use crate::blueprint::BlueprintDoc;
use crate::check::{Violation, ViolationKind};
use crate::site::SitePage;

/// Yields a blob's authored bytes, or `None` when the tree has no such file (a
/// worktree deletion, which the caller drops).
///
/// Generic over the error so a caller in a crate with its own error type — the
/// `roteiro` binary's `anyhow`, this crate's [`GitError`] — passes its own
/// closure without converting on the way in.
pub type BlobReader<'a, E> = dyn Fn(&BlobRef) -> Result<Option<Vec<u8>>, E> + 'a;

/// The authored documents found in one tree, ready for [`crate::check::run`] or
/// [`crate::check::validate`].
#[derive(Debug, Default)]
pub struct AuthoredLayer {
    /// ADRs under `docs/adr/` that parsed.
    pub docs: Vec<AdrDoc>,
    /// House-style blueprints (markdown, no frontmatter).
    pub blueprints: Vec<BlueprintDoc>,
    /// `@rto:` annotations scanned from every other file.
    pub annotations: Vec<Annotation>,
    /// ADRs under `docs/adr/` — and site pages anywhere — that did **not**
    /// parse. Carried as violations rather than dropped: a malformed ADR is
    /// drift, not a skippable warning — swallowing it lets the gate pass while
    /// silently discarding authored intent. A site page that declared itself
    /// published and then failed to parse is the same failure with a public
    /// consequence: the page silently does not exist.
    pub malformed: Vec<Violation>,
    /// House-style convention breaches found while reading the same blobs — see
    /// [`crate::convention`].
    ///
    /// Carried beside [`Self::malformed`] rather than inside it because the two
    /// are different claims: `malformed` is *this document does not parse*, and
    /// this is *this source breaks a rule we wrote down*. A caller that wants
    /// only parse failures should not have to filter prose to get them.
    pub conventions: Vec<Violation>,
}

/// Which files in `source`'s tree carry the authored layer.
///
/// The staged files in `Index` mode (so a staged-new ADR is seen), the `HEAD`
/// tree in `Committed` mode, and in `Worktree` mode `HEAD` **plus untracked
/// files** — because that is precisely what [`rto_graph::sync_worktree`] overlaid
/// into the derived layer.
///
/// Getting this wrong is issue #330's observed symptom. `sync_worktree` walks
/// untracked files deliberately, "so the working-tree `sync`/`check`/`review` see
/// new work that isn't staged yet" — but the authored set read only `HEAD`, so a
/// brand-new ADR had its symbols extracted while the file was never parsed as an
/// ADR. `check` then reported 17 ADRs with 18 on disk, `sync` said "up to date",
/// and nothing indicated that the newest decision was missing. The two layers
/// disagreed about which tree they were describing, in one worktree, with no
/// second worktree involved.
///
/// # Errors
/// Returns [`GitError`] if the tree or the index cannot be walked.
pub fn authored_blobs(repo: &Repo, source: GraphSource) -> Result<Vec<BlobRef>, GitError> {
    match source {
        GraphSource::Index => repo.index_files(),
        GraphSource::Committed => repo.walk_blobs(),
        GraphSource::Worktree => {
            let mut blobs = repo.walk_blobs()?;
            // `untracked_files` is defined against the index, so it cannot return
            // a path already in `blobs`. The synthesized oid is unused:
            // `Repo::read_source` reads Worktree content from disk by path, and an
            // untracked file has no git object to read anyway. (A bare repo has no
            // working tree, and `untracked_files` returns nothing there, so the
            // oid-reading fallback is never reached with one of these.)
            blobs.extend(repo.untracked_files()?.into_iter().map(|path| BlobRef {
                path,
                oid: String::new(),
            }));
            Ok(blobs)
        }
    }
}

/// The authored layer **plus the site pages** — everything one tree's classify
/// pass yields.
///
/// A wrapper rather than a fourth field on [`AuthoredLayer`], because that struct
/// is destructured exhaustively by its callers and a new field is a breaking
/// change for every one of them. Wrapping lets a caller adopt site pages when it
/// is ready to render and gate them, and lets the rest keep compiling against
/// exactly the layer they already handle — which matters here because the
/// classification below must stay the *one* copy of the rule either way.
#[derive(Debug, Default)]
pub struct AuthoredDocs {
    /// ADRs, blueprints, annotations, and anything malformed — including a site
    /// page that declared itself published and then failed to parse.
    pub layer: AuthoredLayer,
    /// Documents that declared themselves published (`site-page:` frontmatter).
    pub site: Vec<SitePage>,
}

/// Classify and parse the authored layer out of `blobs`, reading each blob's
/// bytes with `read` — **discarding the site pages**.
///
/// Site pages are classified (they are not ADRs, blueprints or annotation
/// carriers, and misfiling them would put website prose into the annotation
/// scan) and then dropped, because this function's return type has nowhere to
/// put them. A caller that publishes or gates the website wants
/// [`authored_docs_from`], which is this function's whole body with the site
/// pages kept.
///
/// # Errors
/// Returns `E` if `read` fails.
pub fn authored_layer_from<E>(
    blobs: Vec<BlobRef>,
    read: &BlobReader<'_, E>,
) -> Result<AuthoredLayer, E> {
    Ok(authored_docs_from(blobs, read)?.layer)
}

/// Read and parse the authored layer from `source`'s tree, **discarding the site
/// pages** — see [`authored_layer_from`].
///
/// # Errors
/// Returns [`GitError`] if the tree cannot be walked or a source file cannot be
/// read.
pub fn authored_layer(repo: &Repo, source: GraphSource) -> Result<AuthoredLayer, GitError> {
    Ok(authored_docs(repo, source)?.layer)
}

/// Read and parse **everything** the authored classification yields from
/// `source`'s tree: the file set from [`authored_blobs`], the bytes from
/// [`Repo::read_source`], and the classification from [`authored_docs_from`].
///
/// # Errors
/// Returns [`GitError`] if the tree cannot be walked or a source file cannot be
/// read.
pub fn authored_docs(repo: &Repo, source: GraphSource) -> Result<AuthoredDocs, GitError> {
    authored_docs_from(authored_blobs(repo, source)?, &|blob| {
        repo.read_source(blob, source)
    })
}

/// Classify and parse the authored layer out of `blobs`, reading each blob's
/// bytes with `read`.
///
/// # This is the one copy of the classification rule
///
/// Which path is an ADR, which markdown is a blueprint, which markdown declares
/// itself a published site page, which file merely carries `@rto:` annotations,
/// and that a malformed ADR is drift rather than a skippable warning — that is a
/// rule with one correct answer, and it now has three callers
/// that reach it by different routes:
///
/// - [`authored_layer`] below, from a [`GraphSource`] tree (`build_graph`);
/// - `build_graph_at_rev` in the `roteiro` binary, from an arbitrary rev's blobs
///   (the Stage 35b graph arm, which needs the ADRs *of the reviewed commit*);
/// - [`crate::tool_check`], read-only, which cannot use either of the first two
///   because both end in a write.
///
/// Copying the loop would leave them free to drift, which is the shape this
/// repository has closed repeatedly — `[debt] ignore` honoured on three surfaces
/// and not a fourth, `limit == 0` meaning two things across five endpoints. A
/// graph arm whose ADRs were classified by a slightly different rule than
/// `check`'s would be measuring its own reimplementation.
///
/// `read` yields a blob's authored bytes, or `None` when the tree has no such
/// file (a worktree deletion); the caller supplies it because *where* the bytes
/// come from is precisely what differs between a tree, a rev, and a read-only
/// query. It is generic over its error so a caller in a crate with its own error
/// type does not have to convert on the way in.
///
/// # Errors
/// Returns `E` if `read` fails. A file that reads but does not *parse* is not an
/// error: a malformed ADR lands in [`AuthoredLayer::malformed`] as a violation.
pub fn authored_docs_from<E>(
    blobs: Vec<BlobRef>,
    read: &BlobReader<'_, E>,
) -> Result<AuthoredDocs, E> {
    let mut out = AuthoredDocs::default();
    let layer = &mut out.layer;
    for blob in blobs {
        // Parse the authored source from the same tree the derived layer used.
        let Some(bytes) = read(&blob)? else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        let file = std::path::Path::new(&blob.path);
        let is_md = file
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("md"));
        let name = file
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let is_adr = blob.path.starts_with("docs/adr/") && is_md && name != "README.md";
        if is_adr {
            match crate::adr::parse_adr(&blob.path, &text) {
                Ok(doc) => layer.docs.push(doc),
                Err(e) => layer.malformed.push(Violation {
                    kind: ViolationKind::MalformedAdr,
                    message: format!("{}: cannot parse ADR: {e}", blob.path),
                }),
            }
        } else if is_md && crate::site::is_site_page(&text) {
            // A document that declares itself published (`site-page:`) authors
            // `[[…]]` links like an ADR, and is checked the same way — which is
            // the entire reason the class exists. Classified before the blueprint
            // rule so a published document is never demoted by a coincidence of
            // its path or its H1.
            match crate::site::parse_site_page(&blob.path, &text) {
                Ok(page) => out.site.push(page),
                Err(e) => layer.malformed.push(Violation {
                    kind: ViolationKind::MalformedSitePage,
                    message: format!("{}: cannot parse site page: {e}", blob.path),
                }),
            }
        } else if is_md && crate::blueprint::is_blueprint(&blob.path, &text) {
            // House-style blueprints (no frontmatter) author `[[…]]` links like
            // ADRs; their links are drift-checked against the derived graph too.
            layer
                .blueprints
                .push(crate::blueprint::parse_blueprint(&blob.path, &text));
        } else {
            layer
                .annotations
                .extend(crate::annotate::scan_annotations(&blob.path, &text));
            // The same text, read once, for the conventions that are written
            // down and were not enforced. Riding this pass rather than adding a
            // second walk of the tree: the blobs are already open.
            layer
                .conventions
                .extend(crate::convention::scan_unjustified_allows(
                    &blob.path, &text,
                ));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::authored_docs_from;
    use rto_graph::BlobRef;

    /// Classify a set of `(path, text)` pairs through the one classification
    /// rule, reading bytes straight from the fixture.
    fn classify(files: &[(&str, &str)]) -> super::AuthoredDocs {
        let blobs: Vec<BlobRef> = files
            .iter()
            .map(|(path, _)| BlobRef {
                path: (*path).to_owned(),
                oid: String::new(),
            })
            .collect();
        authored_docs_from(blobs, &|blob: &BlobRef| -> Result<Option<Vec<u8>>, ()> {
            Ok(files
                .iter()
                .find(|(p, _)| *p == blob.path)
                .map(|(_, text)| text.as_bytes().to_vec()))
        })
        .expect("classify")
    }

    #[test]
    fn publication_is_a_declaration_and_survives_living_outside_docs_site() {
        // The rule that makes the class worth having: `docs/OFFLINE_SETUP.md`
        // gains a public page *in place*, and the internal working documents
        // beside it stay internal — neither outcome depends on a path.
        let layer = classify(&[
            (
                "docs/OFFLINE_SETUP.md",
                "---\nsite-page: offline-setup\n---\n\n# Offline setup\n",
            ),
            (
                "docs/REVIEW_CHECKLIST.md",
                "# Review checklist\n\nInternal.\n",
            ),
            ("docs/BUILD_PLAN_V2.md", "# Build Plan V2\n\nInternal.\n"),
        ]);
        let published: Vec<&str> = layer.site.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(published, ["docs/OFFLINE_SETUP.md"]);
        assert_eq!(layer.site[0].slug, "offline-setup");
        assert!(
            layer.layer.malformed.is_empty(),
            "{:?}",
            layer.layer.malformed
        );
    }

    #[test]
    fn an_adr_is_still_an_adr_and_a_page_outranks_the_blueprint_rule() {
        // ADRs are recognised first and are published by their own mechanism.
        // A declared page under `docs/blueprint/` must not be demoted to a
        // blueprint by the coincidence of its path.
        let layer = classify(&[
            (
                "docs/adr/0001-x.md",
                "---\nadr-id: \"0001\"\nstatus: Accepted\nsite-page: sneaky\n---\n\n# ADR-0001\n",
            ),
            (
                "docs/blueprint/landing.md",
                "---\nsite-page: index\n---\n\n# Roteiro\n",
            ),
            (
                "docs/blueprint/roteiro.md",
                "# Roteiro — Technical Implementation Plan\n",
            ),
        ]);
        assert_eq!(layer.layer.docs.len(), 1, "the ADR is still an ADR");
        let pages: Vec<&str> = layer.site.iter().map(|p| p.slug.as_str()).collect();
        assert_eq!(pages, ["index"]);
        assert_eq!(layer.layer.blueprints.len(), 1);
        assert_eq!(layer.layer.blueprints[0].path, "docs/blueprint/roteiro.md");
    }

    #[test]
    fn a_page_that_declares_itself_and_fails_to_parse_is_drift_not_silence() {
        // It asked to be published. Dropping it would leave the gate green and
        // the page silently absent from the site.
        let layer = classify(&[("docs/site/x.md", "---\nsite-page: Not A Slug\n---\n\n# X\n")]);
        assert!(layer.site.is_empty());
        assert_eq!(layer.layer.malformed.len(), 1);
        assert_eq!(
            layer.layer.malformed[0].kind,
            crate::check::ViolationKind::MalformedSitePage
        );
        assert!(
            layer.layer.malformed[0].message.contains("docs/site/x.md"),
            "names the file: {}",
            layer.layer.malformed[0].message
        );
    }

    #[test]
    fn the_three_field_entry_point_drops_pages_rather_than_misfiling_them() {
        // `authored_layer_from` has nowhere to put a site page. Dropping it is
        // deliberate and documented; the failure to avoid is the *other* one —
        // a published page falling through to the annotation scan, which would
        // put website prose into the `@rto:` surface.
        let files = [(
            "docs/OFFLINE_SETUP.md",
            "---\nsite-page: offline-setup\n---\n\n# Offline setup\n\n// @rto:0001\n",
        )];
        let blobs = vec![BlobRef {
            path: files[0].0.to_owned(),
            oid: String::new(),
        }];
        let layer =
            super::authored_layer_from(blobs, &|_: &BlobRef| -> Result<Option<Vec<u8>>, ()> {
                Ok(Some(files[0].1.as_bytes().to_vec()))
            })
            .expect("classify");
        assert!(layer.docs.is_empty());
        assert!(layer.blueprints.is_empty());
        assert!(
            layer.annotations.is_empty(),
            "a published page is not an annotation carrier: {:?}",
            layer.annotations
        );
        // The full form keeps it.
        assert_eq!(classify(&files).site.len(), 1);
    }

    #[test]
    fn a_non_page_still_contributes_its_annotations() {
        // Adding a class must not steal files from the annotation scan.
        let layer = classify(&[("src/store.rs", "//! @rto:0001\n")]);
        assert!(layer.site.is_empty());
        assert_eq!(layer.layer.annotations.len(), 1);
    }
}

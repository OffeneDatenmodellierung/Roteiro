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
    /// ADRs under `docs/adr/` that did **not** parse. Carried as violations
    /// rather than dropped: a malformed ADR is drift, not a skippable warning —
    /// swallowing it lets the gate pass while silently discarding authored
    /// intent.
    pub malformed: Vec<Violation>,
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

/// Classify and parse the authored layer out of `blobs`, reading each blob's
/// bytes with `read`.
///
/// # This is the one copy of the classification rule
///
/// Which path is an ADR, which markdown is a blueprint, which file merely carries
/// `@rto:` annotations, and that a malformed ADR is drift rather than a skippable
/// warning — that is a rule with one correct answer, and it now has three callers
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
pub fn authored_layer_from<E>(
    blobs: Vec<BlobRef>,
    read: &BlobReader<'_, E>,
) -> Result<AuthoredLayer, E> {
    let mut layer = AuthoredLayer::default();
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
        }
    }
    Ok(layer)
}

/// Read and parse the authored layer from the tree named by `source`: the file
/// set from [`authored_blobs`], the bytes from [`Repo::read_source`], and the
/// classification from [`authored_layer_from`].
///
/// # Errors
/// Returns [`GitError`] if the tree cannot be walked or a source file cannot be
/// read.
pub fn authored_layer(repo: &Repo, source: GraphSource) -> Result<AuthoredLayer, GitError> {
    authored_layer_from(authored_blobs(repo, source)?, &|blob| {
        repo.read_source(blob, source)
    })
}

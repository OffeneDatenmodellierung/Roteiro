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
fn authored_blobs(repo: &Repo, source: GraphSource) -> Result<Vec<BlobRef>, GitError> {
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

/// Read and parse the authored layer from the tree named by `source`.
///
/// # Errors
/// Returns [`GitError`] if the tree cannot be walked or a source file cannot be
/// read. A file that reads but does not *parse* is not an error: a malformed ADR
/// lands in [`AuthoredLayer::malformed`] as a violation.
pub fn authored_layer(repo: &Repo, source: GraphSource) -> Result<AuthoredLayer, GitError> {
    let mut layer = AuthoredLayer::default();
    for blob in authored_blobs(repo, source)? {
        // Parse the authored source from the same tree the derived layer used.
        let Some(bytes) = repo.read_source(&blob, source)? else {
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

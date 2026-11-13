//! The upload surface: `PUT /v1/raw/{name}` stores one document in `raw/`, and
//! does nothing else (issue #814, ADR-0026).
//!
//! Mounted by `roteiro serve` only, and only when **both** gates are open: the
//! `upload` cargo feature (a build without it has no route to mount) and
//! `--allow-upload` on the command line (a build with it still refuses without
//! the flag). There is deliberately no config key: a write surface should not be
//! opened by a file somebody edited once.
//!
//! The upload surface runs in the process that holds the graph store, so the limits
//! below are the **whole** containment story, not defence in depth:
//!
//! 1. **Path confinement.** A name is one path segment drawn from an allowlist of
//!    characters, so `..`, `/`, `\`, an absolute path and a leading dot cannot be
//!    spelled. Then resolve-then-verify: the destination's parent is resolved
//!    on every request and must equal `raw/`'s real path, resolved at startup
//!    (so `raw/` may itself be a symlink to another volume). The final component
//!    is opened `create_new`, which refuses an existing symlink there (dangling
//!    or not) rather than following it.
//! 2. **Only `raw/`.** The router's state is the `raw/` path and nothing else. It
//!    holds no `Workspace`, no store, no bundle path, no config, so nothing a
//!    handler does can reach them.
//! 3. **A content-type allowlist**, keyed on the extension, with the declared
//!    `Content-Type` required to agree. Archives are not on it: refused, not
//!    stored, and never expanded.
//! 4. **Size and count limits** ([`Limits`]). One file per request.
//! 5. **No overwrite.** An existing name is a `409`, never a replacement.
//! 6. **No pipeline trigger.** Storing the bytes is the whole effect; extraction
//!    stays a separate, explicit run.
//! 7. **Loopback only.** [`resolve_raw_root`] refuses a non-loopback bind: there
//!    is no authentication, and a write endpoint on the network is a remote
//!    file-write, not a disclosure question.
//!
//! None of this screens the *content*. A correctly functioning upload handed a
//! poisoned document is the likelier attack, and that is #813's screen, not this
//! boundary.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path as UrlPath, State};
use axum::http::{HeaderMap, StatusCode, header::CONTENT_TYPE};
use axum::routing::put;

/// The one route. `{name}` is a single path segment.
pub const ROUTE: &str = "/v1/raw/{name}";

/// What may be stored: extension → the `Content-Type` a client must declare.
/// An allowlist, so a type nobody thought about is refused visibly.
const ALLOWED: &[(&str, &str)] = &[
    ("pdf", "application/pdf"),
    ("md", "text/markdown"),
    ("txt", "text/plain"),
];

/// Per-request and per-directory ceilings.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Largest body accepted, in bytes; larger is a `413`.
    pub max_bytes: usize,
    /// Most entries `raw/` may hold before uploads are refused with `507`.
    pub max_files: usize,
}

impl Limits {
    /// 64 MiB a file, 10,000 files.
    pub const DEFAULT: Self = Self {
        max_bytes: 64 * 1024 * 1024,
        max_files: 10_000,
    };
}

#[derive(Clone)]
struct Upload {
    /// `raw/`'s real path, resolved once at startup.
    raw: Arc<PathBuf>,
    /// `raw/`'s identity at startup, so a write can prove it landed in that
    /// directory and not in whatever a swapped path now points at.
    raw_id: Option<FileId>,
    limits: Limits,
    /// Held from the count check to the file's creation, so concurrent uploads
    /// cannot both take the last slot under [`Limits::max_files`].
    // shortcut: one process only; two servers over one raw/ can still race the
    // count by one file each — move the count into a lock file if that is ever supported.
    gate: Arc<std::sync::Mutex<()>>,
    /// One request body buffered at a time. Taken *before* the body is read, so
    /// waiting uploads hold neither memory (their bytes stay in the socket) nor
    /// a blocking-pool thread; at most one [`Limits::max_bytes`] is in memory.
    inflight: Arc<tokio::sync::Semaphore>,
}

/// `(device, inode)` on Unix; `None` elsewhere, where only the path check runs.
type FileId = (u64, u64);

#[cfg_attr(
    unix,
    allow(
        clippy::unnecessary_wraps,
        reason = "always `Some` on Unix; the non-Unix build returns `None`"
    )
)]
fn file_id(meta: &std::fs::Metadata) -> Option<FileId> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        Some((meta.dev(), meta.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        None
    }
}

/// The upload router over `raw`, which must already be the resolved real path
/// (see [`resolve_raw_root`]).
pub fn router(raw: PathBuf, limits: Limits) -> Router {
    router_with(raw, limits, Arc::new(tokio::sync::Semaphore::new(1)))
}

/// [`router`] with the in-flight semaphore supplied, so a test can hold it.
fn router_with(raw: PathBuf, limits: Limits, inflight: Arc<tokio::sync::Semaphore>) -> Router {
    Router::new()
        .route(ROUTE, put(store))
        .layer(DefaultBodyLimit::max(limits.max_bytes))
        .with_state(Upload {
            raw_id: std::fs::metadata(&raw).ok().as_ref().and_then(file_id),
            raw: Arc::new(raw),
            limits,
            gate: Arc::default(),
            inflight,
        })
}

/// [`router`] at the default [`Limits`], announced on stderr so an operator
/// can see that this server accepts writes, and where they land.
pub fn mount(raw: &Path) -> Router {
    eprintln!(
        "upload surface ON: PUT {ROUTE} stores into {} (pdf/md/txt, no overwrite, \
         nothing extracted)",
        raw.display()
    );
    router(raw.to_path_buf(), Limits::DEFAULT)
}

/// Resolve `<repo>/raw` for an upload-enabled server bound at `socket`, or refuse.
///
/// Refuses a non-loopback bind, a cwd outside a repository, and a missing `raw/`.
/// `raw/` is not created here: whether it is committed, ignored or a symlink to
/// shared storage is the user's decision (ADR-0026 resolved question 2), and a
/// directory that appeared on its own would take it for them.
pub fn resolve_raw_root(socket: std::net::SocketAddr) -> anyhow::Result<PathBuf> {
    if !socket.ip().is_loopback() {
        anyhow::bail!(
            "refusing `--allow-upload` on non-loopback address {socket}: the upload \
             surface has no authentication, so binding it wider would accept file \
             writes from the network. Bind 127.0.0.1 and front it with an \
             authenticating reverse proxy on the same host"
        );
    }
    let cwd = std::env::current_dir()?;
    let repo = rto_graph::Repo::discover(&cwd)?;
    let raw = repo.workdir().unwrap_or(&cwd).join("raw");
    if !raw.is_dir() {
        anyhow::bail!(
            "`--allow-upload` needs a `raw/` directory at {} — create it first, and \
             decide whether it is committed, ignored, or a symlink to shared storage \
             (ADR-0026)",
            raw.display()
        );
    }
    Ok(std::fs::canonicalize(&raw)?)
}

async fn store(
    State(up): State<Upload>,
    UrlPath(name): UrlPath<String>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> (StatusCode, String) {
    let Ok(_permit) = up.inflight.clone().acquire_owned().await else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "upload surface closed\n".to_owned(),
        );
    };
    let body: Bytes = match axum::body::to_bytes(body, up.limits.max_bytes).await {
        Ok(bytes) => bytes,
        Err(e) => {
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                format!(
                    "refused: the body exceeds {} bytes ({e})\n",
                    up.limits.max_bytes
                ),
            );
        }
    };
    let declared = headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    // Blocking file I/O off the runtime thread: `serve` may run current-thread.
    tokio::task::spawn_blocking(move || store_blocking(&up, &name, &declared, &body))
        .await
        .unwrap_or_else(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}\n")))
}

fn store_blocking(up: &Upload, name: &str, declared: &str, body: &[u8]) -> (StatusCode, String) {
    if !valid_name(name) {
        return (
            StatusCode::BAD_REQUEST,
            "refused: a name is one segment of [A-Za-z0-9._-], not starting with `.`\n".to_owned(),
        );
    }
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    let Some(&(_, expected)) = ALLOWED.iter().find(|(e, _)| *e == ext) else {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            format!("refused: `.{ext}` is not on the allowlist (pdf, md, txt)\n"),
        );
    };
    let media = declared
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if media != expected {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            format!("refused: `.{ext}` must be sent as `Content-Type: {expected}`\n"),
        );
    }
    // A poisoned lock only means another upload panicked; the count is re-read.
    let _held = up
        .gate
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match std::fs::read_dir(up.raw.as_path()).map(Iterator::count) {
        Ok(held) if held >= up.limits.max_files => {
            return (
                StatusCode::INSUFFICIENT_STORAGE,
                format!(
                    "refused: raw/ already holds {} entries\n",
                    up.limits.max_files
                ),
            );
        }
        Ok(_) => {}
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("raw/: {e}\n")),
    }
    let dest = up.raw.join(name);
    if !contained(&up.raw, &dest) {
        return (
            StatusCode::FORBIDDEN,
            "refused: destination resolves outside raw/\n".to_owned(),
        );
    }
    match write_new(&dest, up.raw_id, body) {
        Ok(()) => (
            StatusCode::CREATED,
            format!(
                "stored raw/{name} ({} bytes). Nothing was extracted: ingest is a \
                 separate, explicit run\n",
                body.len()
            ),
        ),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            (StatusCode::FORBIDDEN, format!("refused: {e}\n"))
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (
            StatusCode::CONFLICT,
            format!("refused: raw/{name} already exists, and uploads never overwrite\n"),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("raw/{name}: {e}\n"),
        ),
    }
}

/// An allowlist of what a name may contain, so traversal cannot be spelled.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Resolve the destination's parent *now* and require it to be `raw/`'s real
/// path. Catches a `raw/` (or an ancestor) swapped for a symlink after startup.
fn contained(raw: &Path, dest: &Path) -> bool {
    dest.parent()
        .and_then(|p| std::fs::canonicalize(p).ok())
        .is_some_and(|p| p == raw)
}

/// `create_new` is `O_CREAT | O_EXCL`: it refuses an existing file and an
/// existing symlink alike, never following it. A failed write removes the
/// partial file it created.
///
/// `create_new` protects only the final component, so before any byte is
/// written the new file must be the entry `dest` names *and* sit in the
/// directory `raw_id` identified at startup. An ancestor swapped for a symlink
/// between [`contained`] and the open leaves an empty file elsewhere, which is
/// removed, and the upload is refused with nothing written.
// shortcut: a swap *and* swap-back inside this window is not caught; closing it
// needs `openat` on a held directory handle, which std does not expose.
fn write_new(dest: &Path, raw_id: Option<FileId>, body: &[u8]) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dest)?;
    let ours = file.metadata().ok().as_ref().and_then(file_id);
    let named = std::fs::symlink_metadata(dest)
        .ok()
        .as_ref()
        .and_then(file_id);
    let parent = dest
        .parent()
        .and_then(|p| std::fs::metadata(p).ok())
        .as_ref()
        .and_then(file_id);
    if raw_id.is_some() && (ours != named || parent != raw_id) {
        if ours.is_some() && ours == named {
            let _ = std::fs::remove_file(dest);
        }
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "the destination moved outside raw/ while it was being created",
        ));
    }
    file.write_all(body)
        .and_then(|()| file.sync_all())
        .inspect_err(|_| {
            let _ = std::fs::remove_file(dest);
        })
}

#[cfg(test)]
mod tests {
    use super::{Limits, router};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use std::path::PathBuf;
    use tower::ServiceExt as _;

    /// A fresh `raw/` (resolved) beside an `outside/` it must never write into.
    fn fixture(label: &str) -> (PathBuf, PathBuf) {
        let root =
            std::env::temp_dir().join(format!("roteiro-upload-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("raw")).expect("mkdir raw");
        std::fs::create_dir_all(root.join("outside")).expect("mkdir outside");
        let root = std::fs::canonicalize(&root).expect("canon");
        (root.join("raw"), root.join("outside"))
    }

    async fn put(
        raw: &std::path::Path,
        limits: Limits,
        uri: &str,
        ctype: &str,
        body: &'static [u8],
    ) -> StatusCode {
        router(raw.to_path_buf(), limits)
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(uri)
                    .header("content-type", ctype)
                    .body(Body::from(body))
                    .expect("request"),
            )
            .await
            .expect("response")
            .status()
    }

    fn entries(dir: &std::path::Path) -> usize {
        std::fs::read_dir(dir).expect("read_dir").count()
    }

    #[tokio::test]
    async fn an_allowed_document_is_stored_byte_for_byte() {
        let (raw, _) = fixture("ok");
        let status = put(
            &raw,
            Limits::DEFAULT,
            "/v1/raw/paper.pdf",
            "application/pdf",
            b"%PDF-1.7\n",
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(
            std::fs::read(raw.join("paper.pdf")).expect("stored"),
            b"%PDF-1.7\n"
        );
    }

    #[tokio::test]
    async fn traversal_is_refused_and_writes_nothing() {
        let (raw, outside) = fixture("traversal");
        // Percent-encoded so the router hands the handler `../outside/x.md`.
        for uri in [
            "/v1/raw/..%2Foutside%2Fx.md",
            "/v1/raw/..",
            "/v1/raw/.hidden.md",
        ] {
            let status = put(&raw, Limits::DEFAULT, uri, "text/markdown", b"x").await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
        }
        assert_eq!(entries(&raw) + entries(&outside), 0);
    }

    #[tokio::test]
    async fn an_absolute_path_is_refused() {
        let (raw, outside) = fixture("absolute");
        let uri = format!("/v1/raw/{}", outside.join("x.md").display()).replace('/', "%2F");
        let uri = uri.replacen("%2Fv1%2Fraw%2F", "/v1/raw/", 1);
        let status = put(&raw, Limits::DEFAULT, &uri, "text/markdown", b"x").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(entries(&outside), 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlink_escaping_raw_is_not_followed() {
        let (raw, outside) = fixture("symlink");
        let target = outside.join("victim.md");
        std::fs::write(&target, "original").expect("victim");
        std::os::unix::fs::symlink(&target, raw.join("evil.md")).expect("link");
        // A dangling one too: `create_new` must not create its target.
        std::os::unix::fs::symlink(outside.join("new.md"), raw.join("dangling.md")).expect("link");
        for name in ["evil.md", "dangling.md"] {
            let uri = format!("/v1/raw/{name}");
            let status = put(&raw, Limits::DEFAULT, &uri, "text/markdown", b"pwned").await;
            assert_eq!(status, StatusCode::CONFLICT, "{name}");
        }
        assert_eq!(
            std::fs::read_to_string(&target).expect("victim"),
            "original"
        );
        assert!(!outside.join("new.md").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_raw_swapped_for_a_symlink_after_startup_is_refused() {
        let (raw, outside) = fixture("swap");
        let app = router(raw.clone(), Limits::DEFAULT);
        std::fs::remove_dir(&raw).expect("rmdir");
        std::os::unix::fs::symlink(&outside, &raw).expect("link");
        let status = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/v1/raw/x.md")
                    .header("content-type", "text/markdown")
                    .body(Body::from("x"))
                    .expect("request"),
            )
            .await
            .expect("response")
            .status();
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(entries(&outside), 0);
    }

    /// The window `contained` cannot close: the path check passed, then the
    /// directory the name lands in is not the one identified at startup. Driven
    /// directly with a startup identity that does not match, so it needs no race.
    /// Unix only: elsewhere there is no `(device, inode)` to compare.
    #[cfg(unix)]
    #[test]
    fn a_write_that_lands_outside_the_startup_raw_is_undone() {
        let (raw, outside) = fixture("landed");
        let raw_id = std::fs::metadata(&raw)
            .ok()
            .as_ref()
            .and_then(super::file_id);
        let err = super::write_new(&outside.join("x.md"), raw_id, b"secret")
            .expect_err("a file outside raw/ is refused");
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(entries(&outside), 0, "the stray empty file is removed");
        super::write_new(&raw.join("x.md"), raw_id, b"ok").expect("inside raw/ is fine");
    }

    /// A request waits for the in-flight permit before its body is read, so
    /// queued uploads are not each buffering up to `max_bytes`. Raised in
    /// review on #970.
    #[tokio::test]
    async fn an_upload_waits_for_the_in_flight_permit() {
        let (raw, _) = fixture("inflight");
        let inflight = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
        let held = inflight.clone().acquire_owned().await.expect("permit");
        let app = super::router_with(raw.clone(), Limits::DEFAULT, inflight);
        let call = tokio::spawn(
            app.oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/v1/raw/w.txt")
                    .header("content-type", "text/plain")
                    .body(Body::from("x"))
                    .expect("request"),
            ),
        );
        tokio::task::yield_now().await;
        std::thread::sleep(std::time::Duration::from_millis(100));
        tokio::task::yield_now().await;
        assert!(!call.is_finished(), "it must wait while the permit is held");
        assert_eq!(entries(&raw), 0);
        drop(held);
        let status = call.await.expect("join").expect("response").status();
        assert_eq!(status, StatusCode::CREATED);
    }

    /// One slot left and sixteen uploads under different names released at
    /// once: exactly one may take it. `raw/` is pre-filled so each count is a
    /// slow directory read, which keeps every thread inside the
    /// check-then-create window together when nothing serialises them.
    #[test]
    fn concurrent_uploads_cannot_overfill_the_count() {
        const SEEDED: usize = 2_000;
        const THREADS: usize = 16;
        let (raw, _) = fixture("race");
        for i in 0..SEEDED {
            std::fs::write(raw.join(format!("seed{i}.txt")), "s").expect("seed");
        }
        let up = super::Upload {
            raw_id: std::fs::metadata(&raw)
                .ok()
                .as_ref()
                .and_then(super::file_id),
            raw: std::sync::Arc::new(raw.clone()),
            limits: Limits {
                max_files: SEEDED + 1,
                ..Limits::DEFAULT
            },
            gate: std::sync::Arc::default(),
            inflight: std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
        };
        let start = std::sync::Barrier::new(THREADS);
        let created = std::thread::scope(|s| {
            let runs: Vec<_> = (0..THREADS)
                .map(|i| {
                    let (up, start) = (&up, &start);
                    s.spawn(move || {
                        start.wait();
                        super::store_blocking(up, &format!("f{i}.txt"), "text/plain", b"x").0
                    })
                })
                .collect();
            runs.into_iter()
                .map(|r| r.join().expect("join"))
                .filter(|status| *status == StatusCode::CREATED)
                .count()
        });
        assert_eq!(created, 1);
        assert_eq!(entries(&raw), SEEDED + 1);
    }

    #[tokio::test]
    async fn an_oversize_body_is_refused_and_leaves_nothing() {
        let (raw, _) = fixture("oversize");
        let small = Limits {
            max_bytes: 4,
            ..Limits::DEFAULT
        };
        let status = put(&raw, small, "/v1/raw/big.txt", "text/plain", b"12345").await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(entries(&raw), 0);
    }

    #[tokio::test]
    async fn the_file_count_limit_holds() {
        let (raw, _) = fixture("count");
        std::fs::write(raw.join("a.txt"), "a").expect("seed");
        let one = Limits {
            max_files: 1,
            ..Limits::DEFAULT
        };
        let status = put(&raw, one, "/v1/raw/b.txt", "text/plain", b"b").await;
        assert_eq!(status, StatusCode::INSUFFICIENT_STORAGE);
        assert!(!raw.join("b.txt").exists());
    }

    #[tokio::test]
    async fn an_existing_file_is_never_overwritten() {
        let (raw, _) = fixture("overwrite");
        std::fs::write(raw.join("paper.md"), "first").expect("seed");
        let status = put(
            &raw,
            Limits::DEFAULT,
            "/v1/raw/paper.md",
            "text/markdown",
            b"second",
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            std::fs::read_to_string(raw.join("paper.md")).expect("kept"),
            "first"
        );
    }

    #[tokio::test]
    async fn types_off_the_allowlist_are_refused_and_archives_are_not_stored() {
        let (raw, _) = fixture("types");
        for (uri, ctype) in [
            ("/v1/raw/corpus.zip", "application/zip"),
            ("/v1/raw/page.html", "text/html"),
            ("/v1/raw/noext", "text/plain"),
            // On the list, but declared as something else.
            ("/v1/raw/paper.pdf", "text/html"),
        ] {
            let status = put(&raw, Limits::DEFAULT, uri, ctype, b"x").await;
            assert_eq!(
                status,
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "{uri} as {ctype}"
            );
        }
        assert_eq!(entries(&raw), 0);
    }

    #[tokio::test]
    async fn the_surface_is_write_only() {
        let (raw, _) = fixture("writeonly");
        std::fs::write(raw.join("paper.md"), "secret").expect("seed");
        let status = router(raw.clone(), Limits::DEFAULT)
            .oneshot(
                Request::builder()
                    .uri("/v1/raw/paper.md")
                    .body(Body::empty())
                    .expect("req"),
            )
            .await
            .expect("response")
            .status();
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    }

    /// The read surface writes no bytes into the filesystem (issue #814).
    ///
    /// `serve`/`explorer` already mutate the *graph store* (`POST …/links/write`),
    /// so "read-only" was never the line. The line is that no handler there
    /// writes a file, so the upload route above stays the only one that can. A
    /// file-writing call added to any of these modules' non-test code fails here
    /// and has to argue its way past #814 rather than arriving unnoticed.
    ///
    /// shortcut: scans only this crate's server modules, not `rto-serve`'s `/v1`
    /// model routes; extend if those ever touch the filesystem.
    #[test]
    fn the_read_surface_has_no_filesystem_write() {
        const WRITES: &[&str] = &[
            "fs::write",
            "File::create",
            "OpenOptions",
            "create_dir",
            "fs::rename",
            "fs::copy",
            "remove_file",
            "remove_dir",
            "tokio::fs",
            "File::options",
            "hard_link",
            "fs::symlink(",
            "symlink_file",
            "symlink_dir",
        ];
        for (module, source) in [
            ("graph_api.rs", include_str!("graph_api.rs")),
            ("explorer_app.rs", include_str!("explorer_app.rs")),
            ("okf_viewer.rs", include_str!("okf_viewer.rs")),
        ] {
            let shipped = source.split("#[cfg(test)]").next().unwrap_or(source);
            for call in WRITES {
                assert!(
                    !shipped.contains(call),
                    "{module} now contains `{call}` outside its tests. The read surface \
                     must not write files: only `upload.rs` may, behind `--allow-upload` \
                     (issue #814, ADR-0026)"
                );
            }
        }
    }
}

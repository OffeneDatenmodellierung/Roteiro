//! The served half of the OKF viewer (ADR-0022).
//!
//! [`rto_render::okf::view`] decides *what* a reader is shown and what is never
//! emitted; this module is the HTTP around it, exactly as `graph_api` is the
//! served half of the explorer.
//!
//! # It reads a bundle, and only reads it
//!
//! Nothing here writes: not to the graph, not to a store, not to the bundle.
//! `roteiro import --from okf` remains the only way a peer's content enters the
//! graph, and it keeps its consent gate. A viewer that could import would make
//! "have a look at this bundle" a trust decision, which is the thing ADR-0021
//! spent a consent prompt avoiding.
//!
//! The bundle is loaded **per request**, which is what makes this dynamic: an
//! author editing a concept sees it on reload. A bundle of a few hundred concepts
//! is a few hundred small files, and correctness under editing is worth more here
//! than a cache that can be stale.
//!
//! # Serving somebody else's directory
//!
//! Every rule about untrusted content lives in `rto_render::okf::view` and is
//! tested there. The one this module owns is [`file`]: a reader can type a URL,
//! so the route re-applies `view::safe_bundle_file` rather than trusting that
//! only hrefs our own renderer produced will arrive. A guard that assumed its
//! input came from us would be a guard on the wrong side of the boundary.
//!
//! Responses carry `Content-Security-Policy: default-src 'self'`, so even if
//! something were to slip past the renderer the page cannot reach the network.
//! That is a second line, not the first: the first is that raw HTML is escaped
//! and never emitted.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Path as UrlPath, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use rto_render::okf::view;

/// The bundle this viewer serves.
#[derive(Clone)]
struct Viewer {
    root: Arc<PathBuf>,
    /// The last bundle read, and the stamp it was read at.
    ///
    /// ADR-0022 renders at request time so an author editing a concept sees it
    /// on reload, and the first implementation took that literally: every HTML
    /// route re-read and re-parsed the whole bundle. Measured against this
    /// repository's own 9,511-concept bundle, `/` took **6.7 s** and
    /// `/api/graph.json` 1.4 s — and on a current-thread runtime eight
    /// concurrent requests did not finish in ten minutes, because each waited
    /// for the one before it.
    ///
    /// Re-reading is now conditional on the bundle having changed, which costs
    /// a walk of its `mtime`s: **58 ms** for those same 9,517 files, 115x less
    /// than the parse it avoids. The promise ADR-0022 makes is kept — an edit
    /// still appears on the next request — while the cost is paid only when
    /// there was an edit.
    cache: Arc<Mutex<Option<Cached>>>,
    /// The path this viewer is mounted under: empty when served alone, `/okf`
    /// when nested into `serve` beside the explorer.
    ///
    /// Every generated href carries it. Relative hrefs are not an option — a
    /// concept id contains slashes, so `/c/a/b` and `/c/a/b/c` sit at different
    /// depths — and a nested router emitting unprefixed absolute paths would
    /// produce a UI whose every link 404s.
    base: Arc<String>,
}

/// The viewer's routes.
///
/// Stateless from the caller's side — it holds only a path — so it merges into
/// the `serve` router the way [`crate::explorer_app::router`] does.
///
/// `base` is the mount path: empty when served alone by `roteiro okf view`, and
/// `/okf` when nested beside the explorer under `serve`.
/// A bundle, what it looked like on disk when it was read, and the whole-bundle
/// views derived from it.
///
/// The derived views are cached because loading is not the only cost. With the
/// bundle cached, `/` still took 5.8 s against 9,511 concepts while
/// `/api/graph.json` took 0.28 s — the difference being that the index runs the
/// content screener over every concept, and the graph only walks links. Caching
/// the parse and then re-deriving that on each request would have fixed the
/// smaller half of the problem and reported it as fixed.
///
/// Both are filled on demand rather than at load, so a bundle nobody asks the
/// index about does not pay for one.
struct Cached {
    stamp: Stamp,
    bundle: Arc<view::Bundle>,
    overview: Option<Arc<view::BundleView>>,
    graph: Option<Arc<view::GraphView>>,
}

/// A cheap fingerprint of a bundle directory: how many files, and the newest
/// modification time among them.
///
/// Not a hash of the contents. Hashing 9,517 files would cost more than the
/// parse it is meant to avoid, and this only has to answer "did anything
/// change since I last looked", which an author editing a concept always makes
/// true. The count is carried alongside the time because a deletion can leave
/// the newest `mtime` untouched.
///
/// It inherits `mtime`'s limits and does not pretend otherwise: a filesystem
/// with coarse timestamps could hide an edit made in the same second as the
/// read, in a bundle whose file count did not change. The failure that would
/// cause is a stale page until the next edit, on a read-only viewer — which is
/// why this is acceptable here and would not be in the import path.
#[derive(PartialEq, Eq, Clone, Copy)]
struct Stamp {
    files: usize,
    newest: Option<std::time::SystemTime>,
}

fn stamp(root: &std::path::Path) -> Stamp {
    fn walk(dir: &std::path::Path, out: &mut Stamp) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            // This walks a directory a peer controls, so a symlink must never
            // be recursed into: `loop -> ..` inside a bundle would send it
            // round for ever, on *every* request, since the stamp is what
            // decides whether to reload.
            //
            // `file_type()` is documented not to follow the link, and that is
            // why it is used. The honest note is that `metadata()` did not
            // follow it either: `DirEntry::metadata` is `lstat` on Unix, so the
            // loop above never actually reproduced here — verified directly,
            // after a reproduction in Python wrongly suggested it did, because
            // Python's `DirEntry.stat()` *does* follow. The std documentation
            // says `metadata` "will traverse symbolic links", so relying on it
            // not to would be relying on the implementation over its contract.
            //
            // So this is the explicit form of a rule that currently holds by
            // platform accident, in the same spirit as refusing an unknown URL
            // scheme in `view.rs` rather than leaving it to a colon check.
            //
            // A symlink is counted as the file it is, by its own `mtime` via
            // `symlink_metadata`. Its target is not consulted, which is right:
            // if the target is inside the bundle it is already being walked, and
            // if it is outside then `safe_bundle_file` refuses to serve it, so
            // its changes cannot reach a reader and should not force a reload.
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() {
                walk(&path, out);
            } else {
                out.files += 1;
                if let Ok(modified) = std::fs::symlink_metadata(&path).and_then(|m| m.modified()) {
                    out.newest = Some(out.newest.map_or(modified, |n| n.max(modified)));
                }
            }
        }
    }
    let mut out = Stamp {
        files: 0,
        newest: None,
    };
    walk(root, &mut out);
    out
}

impl Viewer {
    /// The bundle, re-read only if the directory changed since it was last read.
    ///
    /// Blocking: every caller runs it inside [`blocking`].
    fn bundle(&self) -> Result<Arc<view::Bundle>, rto_render::okf::inspect::InspectError> {
        self.with_cache(|cached| Arc::clone(&cached.bundle))
    }

    /// The index view, derived once per bundle read.
    fn overview(&self) -> Result<Arc<view::BundleView>, rto_render::okf::inspect::InspectError> {
        let root = self.root.display().to_string();
        self.with_cache(|cached| {
            Arc::clone(
                cached
                    .overview
                    .get_or_insert_with(|| Arc::new(view::overview_in(&cached.bundle, &root))),
            )
        })
    }

    /// The concept graph, derived once per bundle read.
    fn graph(&self) -> Result<Arc<view::GraphView>, rto_render::okf::inspect::InspectError> {
        self.with_cache(|cached| {
            Arc::clone(
                cached
                    .graph
                    .get_or_insert_with(|| Arc::new(view::graph_in(&cached.bundle))),
            )
        })
    }

    /// Re-read the bundle if the directory changed, then hand the entry to `f`.
    ///
    /// Blocking, and holds the lock across the load: a burst of concurrent cold
    /// requests reads the bundle once rather than once each. That serialises
    /// them behind one parse — which is the cost being avoided anyway, and far
    /// better than N parses competing for the same page cache.
    fn with_cache<T>(
        &self,
        f: impl FnOnce(&mut Cached) -> T,
    ) -> Result<T, rto_render::okf::inspect::InspectError> {
        let current = stamp(&self.root);
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let stale = cache.as_ref().is_none_or(|cached| cached.stamp != current);
        if stale {
            // The derived views go with it. Keeping them beside a new bundle is
            // how a viewer shows an edited concept in the body and the old title
            // in the sidebar — the two halves of one page disagreeing.
            *cache = Some(Cached {
                stamp: current,
                bundle: Arc::new(view::load(&self.root)?),
                overview: None,
                graph: None,
            });
        }
        Ok(f(cache.as_mut().expect("just populated")))
    }
}

/// Run blocking work off the async executor.
///
/// Every route here touches the filesystem, and the standalone server runs on a
/// current-thread runtime, so doing that work inline stalls every other request
/// for its duration — measured at ten minutes for eight concurrent requests
/// against a 9,511-concept bundle. `spawn_blocking` is the portable fix: it is
/// correct whether the viewer is standalone or nested inside `serve`'s runtime.
async fn blocking<T, F>(work: F) -> Option<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    // `Option` rather than `Result<T, Response>`: the only failure is the task
    // panicking or being cancelled, which carries no information a caller can
    // act on, and a `Response` in the error position makes every `Result` in
    // this module 128 bytes wide for a case that never carries a message.
    tokio::task::spawn_blocking(work).await.ok()
}

/// The response when the blocking pool could not run the work.
fn spawn_failed() -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        [(header::CONTENT_SECURITY_POLICY, CSP)],
        Html("<p>The viewer failed to read the bundle.</p>"),
    )
        .into_response()
}

pub fn router(root: PathBuf, base: &str) -> Router {
    let state = Viewer {
        root: Arc::new(root),
        base: Arc::new(base.to_owned()),
        cache: Arc::new(Mutex::new(None)),
    };
    Router::new()
        .route("/", get(index))
        .route("/graph", get(graph_page))
        .route("/api/graph.json", get(graph_json))
        .route("/c/{*id}", get(concept))
        .route("/f/{*path}", get(file))
        .route("/okf-viewer.css", get(stylesheet))
        .route("/cytoscape.min.js", get(cytoscape))
        .with_state(state)
}

const STYLE: &str = include_str!("assets/okf-viewer.css");
/// The same vendored build the explorer uses (ADR-0010) — one copy in the
/// binary, and no fetch from a CDN.
const CYTOSCAPE: &str = include_str!("assets/cytoscape.min.js");

/// `default-src 'self'` and nothing else: no CDN, no font host, no analytics.
///
/// The renderer already refuses to emit raw HTML, so this is a second line
/// rather than the defence. It is worth having because the two fail
/// independently — a bug in the escaping does not also disable the header.
const CSP: &str = "default-src 'self'; img-src 'self'; object-src 'none'; base-uri 'none'";

/// The policy for `/f/`, which serves **bytes a peer wrote**.
///
/// Tighter than [`CSP`], and deliberately a different constant. The page policy
/// says `default-src 'self'` because the viewer's own pages legitimately load
/// their own stylesheet and script. A file out of the bundle needs neither, and
/// `'self'` is too generous for one: `script-src` falls back to `default-src`,
/// so an SVG served from here and opened directly could have pulled in
/// `/f/anything.js` from the same bundle.
///
/// That was blocked in practice — the mime table serves an unknown extension as
/// `application/octet-stream` and `nosniff` refuses to execute it as script — but
/// that is three unrelated rules happening to line up, not a policy. SVG is
/// active content, unlike every other type in the table, and the bundle chooses
/// its contents.
///
/// `sandbox` puts a directly-opened file in a unique origin with scripting off;
/// `default-src 'none'` stops it fetching anything at all. Neither affects an
/// image embedded with `<img>`, which is how the viewer itself loads them —
/// scripts never run in that context regardless.
const FILE_CSP: &str = "default-src 'none'; sandbox; base-uri 'none'";

/// The most `/f/` will read into memory for one request.
///
/// The route reads a whole file before answering, and the bundle is somebody
/// else's: without a bound, one large file — or a handful of concurrent requests
/// for it — is memory pressure a peer chooses for you. This is a **memory bound,
/// not a policy**: it says nothing about what a bundle may contain, only what
/// this process will hold at once, and it is per request, so concurrency
/// multiplies it.
///
/// 32 MiB is far above any image a concept embeds and far below anything that
/// threatens a server. A bundle carrying something genuinely larger is not
/// refused as a bundle — every other command still reads it — it simply is not
/// served down this route.
const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;

/// `Cache-Control` for the two compiled-in assets.
///
/// They are `include_str!`ed, so they change only when the binary does — the
/// same reasoning and the same hour as `explorer_app`'s `CACHE_JS`. Without it a
/// browser refetches the whole of cytoscape on every navigation, which on a
/// bundle whose pages are otherwise cheap is the largest thing on the wire.
///
/// Deliberately **not** applied to any bundle content. `/f/` serves files an
/// author is editing, and `/`, `/c/` and the graph are rendered from a bundle
/// that changes underneath the server — caching those in the browser would undo
/// the reload guarantee the server-side cache is careful to keep.
const CACHE_ASSET: &str = "public, max-age=3600";

fn page(title: &str, root: &str, base: &str, body: &str) -> Response {
    let mut out = String::with_capacity(body.len() + 2048);
    let _ = write!(
        out,
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <link rel=\"stylesheet\" href=\"{base}/okf-viewer.css\">\
         <title>{} — OKF viewer</title></head><body>\
         <header><span class=\"name\">OKF viewer</span>\
         <span class=\"root\">{}</span>\
         <nav><a href=\"{base}/\">Concepts</a><a href=\"{base}/graph\">Graph</a></nav></header>\
         <main>{body}</main>\
         <footer>Read-only. Nothing here is imported into the graph — \
         <code>roteiro import --from okf</code> is still the only path that does, \
         and it asks first.</footer></body></html>",
        escape(title),
        escape(root),
    );
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CONTENT_SECURITY_POLICY, CSP),
        ],
        Html(out),
    )
        .into_response()
}

/// Every string this module interpolates goes through here.
///
/// The body HTML is the renderer's and is already safe; a title, an id, a
/// screener class and a path are all bundle-controlled text being put into
/// markup here, so they are escaped at the point of use rather than trusted to
/// have been escaped earlier.
fn escape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

fn unreadable(err: &rto_render::okf::inspect::InspectError) -> Response {
    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_SECURITY_POLICY, CSP)],
        Html(format!(
            "<p>Not a readable OKF bundle: {}</p>",
            escape(&err.to_string())
        )),
    )
        .into_response()
}

/// A tier or status pill, which always carries its word as well as its colour.
fn pill(class: &str, text: &str) -> String {
    format!(
        "<span class=\"tier tier-{}\">{}</span>",
        escape(class),
        escape(text)
    )
}

async fn index(State(v): State<Viewer>) -> Response {
    let state = v.clone();
    let built = blocking(move || state.overview()).await;
    let view = match built {
        Some(Ok(view)) => view,
        Some(Err(e)) => return unreadable(&e),
        None => return spawn_failed(),
    };
    let base = v.base.as_str();

    let mut body = String::new();
    body.push_str("<aside>");
    let _ = write!(
        body,
        "<div class=\"group\">{} concept(s)</div><ol>",
        view.concepts.len()
    );
    for c in &view.concepts {
        let _ = write!(
            body,
            "<li><a href=\"{base}/c/{}\">{}</a></li>",
            escape(&c.id),
            escape(&c.title)
        );
    }
    body.push_str("</ol></aside><article>");

    let _ = write!(
        body,
        "<h1>Bundle</h1><ul class=\"counts\">\
         <li><span class=\"n\">{}</span> human-reviewed</li>\
         <li><span class=\"n\">{}</span> machine-confirmed</li>\
         <li><span class=\"n\">{}</span> unverified</li>\
         <li><span class=\"n\">{}</span> unresolved link(s)</li>\
         <li>okf_version {}</li></ul>",
        view.human_reviewed,
        view.machine_confirmed,
        view.unverified,
        view.broken_links,
        view.okf_version
            .as_deref()
            .map_or_else(|| "not declared".to_owned(), escape),
    );

    if !view.flagged.is_empty() {
        let _ = write!(
            body,
            "<div class=\"screened\"><strong>{} concept(s) tripped the content screener.</strong> \
             This is a bundle somebody else wrote, and the screener looks for text shaped to be \
             read as instructions rather than as content. Nothing has been imported.<ul>",
            view.flagged.len()
        );
        for f in &view.flagged {
            let _ = write!(
                body,
                "<li><a href=\"{base}/c/{}\">{}</a> — {} ({})</li>",
                escape(&f.id),
                escape(&f.id),
                escape(&f.verdict),
                f.classes
                    .iter()
                    .map(|c| format!("<code>{}</code>", escape(c)))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        body.push_str("</ul></div>");
    }

    body.push_str("<table><tr><th>Concept</th><th>Type</th><th>Trust</th><th>Status</th></tr>");
    for c in &view.concepts {
        let status_class = if c.status == "deprecated" {
            " class=\"status-deprecated\""
        } else {
            ""
        };
        let _ = write!(
            body,
            "<tr><td><a href=\"{base}/c/{}\">{}</a></td><td>{}</td><td>{}</td><td{status_class}>{}</td></tr>",
            escape(&c.id),
            escape(&c.title),
            c.kind.as_deref().map_or_else(String::new, escape),
            pill(c.trust, c.trust),
            escape(&c.status),
        );
    }
    body.push_str("</table></article>");
    page("Bundle", &view.root, base, &body)
}

async fn concept(State(v): State<Viewer>, UrlPath(id): UrlPath<String>) -> Response {
    let state = v.clone();
    let wanted = id.clone();
    let built = blocking(move || {
        let bundle = state.bundle()?;
        Ok::<_, rto_render::okf::inspect::InspectError>(view::concept_in(
            &bundle,
            &wanted,
            &state.base,
        ))
    })
    .await;
    let base = v.base.as_str();
    let found = match built {
        Some(Ok(found)) => found,
        Some(Err(e)) => return unreadable(&e),
        None => return spawn_failed(),
    };
    let Some(c) = found else {
        return (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_SECURITY_POLICY, CSP)],
            Html(format!(
                "<p>The bundle contains no concept <code>{}</code>. \
                 <a href=\"{base}/\">Back to the bundle</a>.</p>",
                escape(&id)
            )),
        )
            .into_response();
    };

    let mut body = String::from("<article>");
    if !c.screen.is_empty() {
        let _ = write!(
            body,
            "<div class=\"screened\"><strong>The content screener flagged this document.</strong> \
             It is somebody else's text and may be written to be read as instructions. \
             Classes: {}.</div>",
            c.screen
                .iter()
                .map(|s| format!("<code>{}</code>", escape(s)))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let _ = write!(
        body,
        "<h1>{}</h1><p class=\"meta\">{}{}{}<code>{}</code></p>{}",
        escape(&c.title),
        pill(c.trust, c.trust),
        c.kind
            .as_deref()
            .map_or_else(String::new, |k| format!("<span>{}</span>", escape(k))),
        format_args!("<span>{}</span>", escape(&c.status)),
        escape(&c.path),
        c.body_html,
    );

    body.push_str("<div class=\"rel\">");
    if !c.links.is_empty() {
        body.push_str("<h2>Links out</h2><ul>");
        for l in &c.links {
            if l.exists {
                let _ = write!(
                    body,
                    "<li><a href=\"{base}/c/{}\">{}</a></li>",
                    escape(&l.target),
                    escape(&l.target)
                );
            } else {
                // §6 tells a consumer to tolerate a link whose target is not
                // here, so it is shown as absent rather than hidden or errored.
                let _ = write!(
                    body,
                    "<li class=\"absent\">{} — not in this bundle</li>",
                    escape(&l.target)
                );
            }
        }
        body.push_str("</ul>");
    }
    if !c.backlinks.is_empty() {
        body.push_str("<h2>Linked from</h2><ul>");
        for b in &c.backlinks {
            let _ = write!(
                body,
                "<li><a href=\"{base}/c/{}\">{}</a></li>",
                escape(b),
                escape(b)
            );
        }
        body.push_str("</ul>");
    }
    body.push_str("</div></article>");
    page(&c.title, &v.root.display().to_string(), base, &body)
}

async fn graph_page(State(v): State<Viewer>) -> Response {
    // The script is inline and fixed at compile time, so it is content of this
    // binary rather than of the bundle. `'unsafe-inline'` is scoped to
    // `script-src` on this one route and never widens `default-src`.
    // `{BASE}` and a `replace`, not `format!`: this is JavaScript, so it is most
    // of the way to being braces, and every one of them would have to be doubled
    // to survive a format string. A placeholder keeps the script readable as the
    // script it is.
    const SCRIPT: &str = "<article><h1>Concept graph</h1><div id=\"graph\"></div>\
        <script src=\"{BASE}/cytoscape.min.js\"></script>\
        <script>fetch('{BASE}/api/graph.json').then(r=>r.json()).then(g=>{\
        cytoscape({container:document.getElementById('graph'),\
        elements:[...g.nodes.map(n=>({data:{id:n.id,label:n.label,trust:n.trust}})),\
        ...g.edges.map(e=>({data:{source:e.source,target:e.target}}))],\
        layout:{name:'cose'},style:[\
        {selector:'node',style:{'label':'data(label)','font-size':'8px',\
        'background-color':'#6b7684','color':'#1a2733'}},\
        {selector:'node[trust=\"human-reviewed\"]',style:{'background-color':'#0e6e8c'}},\
        {selector:'edge',style:{'width':1,'line-color':'#d8d2c4',\
        'target-arrow-shape':'triangle','target-arrow-color':'#d8d2c4',\
        'curve-style':'bezier'}}]});});</script></article>";

    let base = v.base.as_str();
    let body = &SCRIPT.replace("{BASE}", base);
    let mut res = page("Concept graph", &v.root.display().to_string(), base, body);
    res.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        header::HeaderValue::from_static(
            "default-src 'self'; script-src 'self' 'unsafe-inline'; \
             img-src 'self'; object-src 'none'; base-uri 'none'",
        ),
    );
    res
}

async fn graph_json(State(v): State<Viewer>) -> Response {
    let built = blocking(move || v.graph()).await;
    let graph = match built {
        Some(Ok(graph)) => graph,
        Some(Err(e)) => return unreadable(&e),
        None => return spawn_failed(),
    };
    // `GraphView` is plain owned data, so this does not fail in practice — which
    // is exactly why it must not be swallowed. A `{}` under a 200 would render
    // as a bundle with no concepts: a client cannot tell that from a real empty
    // graph, so the one way this ever goes wrong is also the way that hides it.
    let Ok(body) = serde_json::to_string(graph.as_ref()) else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::CONTENT_SECURITY_POLICY, CSP),
            ],
            r#"{"error":"the graph could not be serialised"}"#,
        )
            .into_response();
    };
    (
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CONTENT_SECURITY_POLICY, CSP),
        ],
        body,
    )
        .into_response()
}

/// A file from inside the bundle — an image a concept embeds, and nothing else.
///
/// The guard is `view::safe_bundle_file`, the same one the renderer applies when
/// it decides whether to emit a `/f/` href at all. Re-applied here because a
/// reader can type a URL: trusting that only our own hrefs arrive would put the
/// check on the wrong side of the boundary.
async fn file(State(v): State<Viewer>, UrlPath(path): UrlPath<String>) -> Response {
    // The refusals carry the policy too. "Every response" has to mean every
    // response, or it is not a rule but a description of the happy path — and a
    // reader cannot tell which from the sentence.
    let refused = || {
        (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_SECURITY_POLICY, FILE_CSP)],
        )
            .into_response()
    };

    // Guard, size check and read happen together on the blocking pool rather
    // than as three hops: they are one decision about one file, and splitting
    // them would widen the window in which it can change underneath us.
    let root = Arc::clone(&v.root);
    let wanted = path.clone();
    let read = blocking(move || {
        let resolved = view::safe_bundle_file(&root, &wanted)?;
        // Checked before the read, not after: `std::fs::read` allocates to the
        // file's length, so a check on `bytes.len()` would already have paid
        // the cost it exists to avoid.
        let meta = std::fs::metadata(&resolved).ok()?;
        if meta.len() > MAX_FILE_BYTES {
            return None;
        }
        let bytes = std::fs::read(&resolved).ok()?;
        Some((resolved, bytes))
    })
    .await;
    let Some(Some((resolved, bytes))) = read else {
        // A file past the bound refuses as 404 like every other refusal here:
        // the size of a file this route declines to serve is not something a
        // stranger's client needs confirmed.
        return refused();
    };

    // Typed from the extension against a closed list. A bundle does not choose
    // the content type: echoing one back from the file would let a bundle serve
    // itself as `text/html` and undo the escaping.
    let mime = match resolved
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        // Anything else is served as bytes to download rather than rendered.
        _ => "application/octet-stream",
    };
    (
        [
            (header::CONTENT_TYPE, mime),
            (header::CONTENT_SECURITY_POLICY, FILE_CSP),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        bytes,
    )
        .into_response()
}

async fn stylesheet() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CONTENT_SECURITY_POLICY, CSP),
            (header::CACHE_CONTROL, CACHE_ASSET),
        ],
        STYLE,
    )
        .into_response()
}

async fn cytoscape() -> Response {
    (
        [
            (
                header::CONTENT_TYPE,
                "application/javascript; charset=utf-8",
            ),
            (header::CONTENT_SECURITY_POLICY, CSP),
            (header::CACHE_CONTROL, CACHE_ASSET),
        ],
        CYTOSCAPE,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The palette is copied from the site, so it is asserted rather than
    /// remembered.
    ///
    /// `include_str!` would be the obvious way to have one copy, and it cannot be
    /// used: `roteiro` publishes to crates.io and `cargo package` takes only
    /// files under the crate directory, so a build-time include reaching up to
    /// `website/` would ship a crate that does not compile. A copy plus a test is
    /// the same trade the vendored OKF fixtures make.
    #[test]
    fn the_viewer_shares_the_sites_palette() {
        let site =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../website/public/style.css");
        let Ok(site_css) = std::fs::read_to_string(&site) else {
            // Not a repository checkout (a packaged crate has no `website/`), so
            // there is nothing to compare against. Returning is right here and
            // would not be in a repo — see the assertion below.
            return;
        };
        let palette = |css: &str| {
            css.lines()
                .find(|l| l.trim_start().starts_with(":root") && l.contains("--ink"))
                .map(|l| l.trim().to_owned())
        };
        let theirs = palette(&site_css).expect("the site declares a palette");
        let ours = palette(STYLE).expect("the viewer declares a palette");
        assert_eq!(
            ours, theirs,
            "the viewer's palette has drifted from the site's. Copy \
             `website/public/style.css`'s `:root` line into \
             `crates/roteiro/src/assets/okf-viewer.css`."
        );
    }

    /// Bundle-controlled text is escaped at the point it enters markup.
    #[test]
    fn interpolated_text_is_escaped() {
        let out = escape(r#"<img src=x onerror="alert(1)">&'"#);
        assert!(!out.contains('<'), "{out}");
        assert!(!out.contains('>'), "{out}");
        assert!(!out.contains('"'), "{out}");
        assert_eq!(
            out,
            "&lt;img src=x onerror=&quot;alert(1)&quot;&gt;&amp;&#39;"
        );
    }

    /// A title carrying markup cannot break out of the page shell.
    #[test]
    fn a_hostile_title_cannot_escape_the_shell() {
        let html = page(
            "</title><script>alert(1)</script>",
            "/tmp/b",
            "",
            "<article/>",
        );
        let body = format!("{html:?}");
        assert!(!body.contains("<script>alert"), "{body}");
    }

    // ---- routes ----------------------------------------------------------
    //
    // Driven in memory with `tower::ServiceExt::oneshot`, the same way
    // `graph_api`'s route tests run: no TCP bind, so they are as fast and as
    // deterministic as any other unit test.

    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt as _;

    fn fixture(tag: &str, files: &[(&str, &str)]) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "roteiro-okf-view-{}-{seq}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        for (rel, content) in files {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(&path, content).expect("write");
        }
        root
    }

    /// `(status, body)` for one GET.
    async fn get_(root: &std::path::Path, base: &str, uri: &str) -> (StatusCode, String) {
        let response = router(root.to_path_buf(), base)
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 22)
            .await
            .expect("body");
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    fn sample() -> PathBuf {
        fixture(
            "routes",
            &[
                ("index.md", "---\nokf_version: \"0.2\"\n---\n\n# Bundle\n"),
                (
                    "metrics/revenue.md",
                    "---\ntype: Metric\ntitle: Revenue\nverified: { by: human:alice, \
                     at: 2026-08-01T10:00:00Z }\n---\n\n# Revenue\n\nSee \
                     [cost](/metrics/cost.md).\n",
                ),
                (
                    "metrics/cost.md",
                    "---\ntype: Metric\ntitle: Cost\n---\n\n# Cost\n",
                ),
            ],
        )
    }

    #[tokio::test]
    async fn the_index_lists_the_bundle_and_counts_its_tiers() {
        let root = sample();
        let (status, body) = get_(&root, "", "/").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Revenue"), "{body}");
        assert!(body.contains("href=\"/c/metrics/revenue\""), "{body}");
        assert!(body.contains("human-reviewed"), "{body}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_concept_renders_with_its_links_and_backlinks() {
        let root = sample();
        let (status, body) = get_(&root, "", "/c/metrics/revenue").await;
        assert_eq!(status, StatusCode::OK);
        // The body link was rewritten to a viewer route by the renderer.
        assert!(body.contains("href=\"/c/metrics/cost\""), "{body}");

        let (_, cost) = get_(&root, "", "/c/metrics/cost").await;
        assert!(cost.contains("Linked from"), "{cost}");
        assert!(cost.contains("metrics/revenue"), "{cost}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An unknown concept is a 404 page, not a 500 and not a blank 200.
    #[tokio::test]
    async fn an_unknown_concept_is_a_404() {
        let root = sample();
        let (status, body) = get_(&root, "", "/c/metrics/nope").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.contains("no concept"), "{body}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **A file out of the bundle is served under a policy of its own.**
    ///
    /// `/f/` serves bytes a peer wrote, and one of the types it will label is
    /// `image/svg+xml` — active content, unlike every other entry in the table.
    /// Under the page policy, `script-src` falls back to `default-src 'self'`, so
    /// an SVG opened directly could have referenced `/f/anything.js` from the same
    /// bundle. The mime table and `nosniff` did stop that, but by coincidence
    /// rather than by policy.
    ///
    /// Asserted as a *difference* from [`CSP`] rather than as a literal string:
    /// the point is that the two are not the same policy, and a future edit that
    /// unified them would be the regression.
    #[tokio::test]
    async fn a_bundle_file_is_served_under_a_stricter_policy_than_a_page() {
        let root = fixture(
            "file-csp",
            &[
                ("index.md", "---\nokf_version: \"0.2\"\n---\n\n# B\n"),
                ("img/logo.svg", "<svg/>"),
            ],
        );
        let policy = |uri: &'static str| {
            let root = root.clone();
            async move {
                let response = router(root, "")
                    .oneshot(
                        Request::builder()
                            .uri(uri)
                            .body(Body::empty())
                            .expect("req"),
                    )
                    .await
                    .expect("response");
                response
                    .headers()
                    .get(header::CONTENT_SECURITY_POLICY)
                    .expect("every response carries a policy")
                    .to_str()
                    .expect("ascii")
                    .to_owned()
            }
        };

        let file = policy("/f/img/logo.svg").await;
        let page = policy("/").await;
        assert_ne!(file, page, "a peer's bytes do not get the page's policy");
        assert!(
            file.contains("sandbox"),
            "a directly-opened file is sandboxed: {file}"
        );
        assert!(
            file.contains("default-src 'none'"),
            "and fetches nothing: {file}"
        );
        assert!(
            !file.contains("'self'"),
            "`'self'` is what let an SVG reach the rest of the bundle: {file}"
        );

        // The refusal path carries it too — "every response" has to mean every.
        let missing = policy("/f/img/absent.png").await;
        assert_eq!(missing, file, "a refusal carries the same policy as a hit");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **A symlinked directory does not send the stamp walk round in circles.**
    ///
    /// The stamp decides whether to re-read the bundle, so it runs on *every*
    /// request over a directory a peer controls, and `loop -> ..` inside one
    /// would send it round for ever.
    ///
    /// **A characterisation test, and it was nearly mislabelled a guard.**
    /// Reverting `file_type()` to `metadata()` leaves it green, because
    /// `DirEntry::metadata` is `lstat` on Unix and does not follow the link —
    /// checked directly, after a reproduction written in Python appeared to
    /// prove the opposite. Python's `DirEntry.stat()` follows; Rust's does not.
    /// The reproduction was measuring itself.
    ///
    /// The property is therefore upheld today by platform behaviour that the std
    /// documentation explicitly disclaims — it says `metadata` "will traverse
    /// symbolic links". `file_type()` makes it hold by contract instead, and
    /// this pins the property so a future walk that does follow gets caught.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_directory_does_not_make_the_stamp_walk_forever() {
        use std::os::unix::fs::symlink;
        let root = fixture(
            "stamp-loop",
            &[("index.md", "---\nokf_version: \"0.2\"\n---\n\n# B\n")],
        );
        symlink("..", root.join("loop")).expect("symlink to the parent");
        symlink("/", root.join("everything")).expect("symlink to the root");

        let first = stamp(&root);
        // Two real entries — `index.md` and the two links, counted as the links
        // they are rather than followed.
        assert_eq!(first.files, 3, "the links count as files, not as trees");

        // Stable across calls, so it does not force a reload on every request.
        assert!(
            first == stamp(&root),
            "the stamp is stable when nothing changed"
        );

        // And it still notices a real edit.
        std::fs::write(root.join("second.md"), "---\ntype: Metric\n---\n\n# S\n").expect("write");
        assert!(first != stamp(&root), "a new file changes the stamp");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **`/f/` will not read an unbounded file into memory.**
    ///
    /// The bundle is somebody else's, and the route reads a whole file before
    /// answering, so without a bound one large file is memory pressure a peer
    /// chooses for this process. Checked at `metadata().len()` rather than on
    /// the bytes, because `std::fs::read` allocates to the file's length and a
    /// check afterwards has already paid the cost.
    ///
    /// The fixture is written just over the bound, so it also pins the bound
    /// being *applied* rather than merely defined — a constant nothing consults
    /// reads exactly like this test passing.
    #[tokio::test]
    async fn the_file_route_refuses_a_file_too_large_to_hold() {
        let root = fixture(
            "big-file",
            &[
                ("index.md", "---\nokf_version: \"0.2\"\n---\n\n# B\n"),
                ("img/logo.svg", "<svg/>"),
            ],
        );
        let big = root.join("img/huge.bin");
        // Sparse where the filesystem allows it, so this costs a few bytes on
        // disk rather than 32 MiB per test run.
        let handle = std::fs::File::create(&big).expect("create");
        handle
            .set_len(MAX_FILE_BYTES + 1)
            .expect("size the file past the bound");
        drop(handle);

        let (status, _) = get_(&root, "", "/f/img/huge.bin").await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "a file past the bound must not be served"
        );

        // And the bound is a bound, not a ban: the small file beside it is fine.
        let (ok, body) = get_(&root, "", "/f/img/logo.svg").await;
        assert_eq!(ok, StatusCode::OK);
        assert!(body.contains("svg"), "{body}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **The file route re-applies the guard**, because a reader can type a URL.
    ///
    /// The renderer only ever emits a `/f/` href for a path it has already
    /// checked, so a route that trusted its input would be a guard on the wrong
    /// side of the boundary — and nothing would fail until someone tried.
    #[tokio::test]
    async fn the_file_route_serves_only_from_inside_the_bundle() {
        let root = fixture(
            "files",
            &[
                ("index.md", "---\nokf_version: \"0.2\"\n---\n\n# B\n"),
                ("img/logo.svg", "<svg/>"),
            ],
        );
        // Outside the bundle entirely, and readable — so a route without the
        // guard would happily return it.
        let outside = root.parent().expect("parent").join("outside.txt");
        std::fs::write(&outside, "secret").expect("write");

        let (ok, body) = get_(&root, "", "/f/img/logo.svg").await;
        assert_eq!(ok, StatusCode::OK);
        assert!(body.contains("svg"), "{body}");

        for hostile in [
            "/f/../outside.txt",
            "/f/img/../../outside.txt",
            "/f/..%2Foutside.txt",
            "/f/img/absent.png",
        ] {
            let (status, body) = get_(&root, "", hostile).await;
            assert_ne!(
                status,
                StatusCode::OK,
                "`{hostile}` must not be served: {body}"
            );
            assert!(!body.contains("secret"), "`{hostile}` leaked: {body}");
        }
        let _ = std::fs::remove_file(&outside);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Nested under `serve`, every generated href carries the mount prefix.
    ///
    /// Without this the viewer would look right standalone and 404 on every link
    /// the moment it was mounted beside the explorer — the failure the `base`
    /// field exists to prevent.
    #[tokio::test]
    async fn a_nested_mount_prefixes_every_href() {
        let root = sample();
        let (status, body) = get_(&root, "/okf", "/").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("href=\"/okf/c/metrics/revenue\""), "{body}");
        assert!(body.contains("href=\"/okf/okf-viewer.css\""), "{body}");
        assert!(body.contains("href=\"/okf/graph\""), "{body}");
        assert!(
            !body.contains("href=\"/c/"),
            "an unprefixed href would 404 when nested: {body}"
        );

        // **And a concept page, whose body links the renderer builds.**
        //
        // This half is the one that mattered: the index carries no rendered
        // markdown, so checking only the chrome let unprefixed body links pass.
        // Nested, every link inside a concept's prose would have 404'd while the
        // page around it looked correct.
        let (status, page) = get_(&root, "/okf", "/c/metrics/revenue").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            page.contains("href=\"/okf/c/metrics/cost\""),
            "a link in the body must carry the prefix: {page}"
        );
        assert!(
            !page.contains("href=\"/c/") && !page.contains("src=\"/f/"),
            "no unprefixed href anywhere on the page: {page}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Every page carries a policy that cannot reach the network.
    /// **Every** response carries the policy, not only the HTML ones.
    ///
    /// The module documentation says "every response". A test that checked only
    /// the two HTML routes would have let that sentence describe the happy path
    /// while the assets, the JSON and the refusals went bare — and a reader could
    /// not tell the difference from the sentence.
    #[tokio::test]
    async fn responses_carry_a_content_security_policy() {
        let root = sample();
        for uri in [
            "/",
            "/c/metrics/revenue",
            "/c/does/not/exist",
            "/graph",
            "/api/graph.json",
            "/okf-viewer.css",
            "/cytoscape.min.js",
            "/f/../escape",
        ] {
            let response = router(root.clone(), "")
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .body(Body::empty())
                        .expect("req"),
                )
                .await
                .expect("response");
            let csp = response
                .headers()
                .get(header::CONTENT_SECURITY_POLICY)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            // Two policies, and which one applies is the point: the viewer's
            // own pages load their own stylesheet and script, so they need
            // `'self'`; bytes out of a peer's bundle need nothing at all. What
            // this asserts of every route is that *some* policy is present and
            // that nothing can execute.
            if uri.starts_with("/f/") {
                assert!(csp.contains("default-src 'none'"), "{uri}: {csp}");
                assert!(csp.contains("sandbox"), "{uri}: {csp}");
            } else {
                assert!(csp.contains("default-src 'self'"), "{uri}: {csp}");
                assert!(csp.contains("object-src 'none'"), "{uri}: {csp}");
            }
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A path that is not a bundle is refused rather than served empty.
    #[tokio::test]
    async fn a_path_that_is_not_a_bundle_is_refused() {
        let root = std::env::temp_dir().join("roteiro-okf-view-not-a-bundle");
        let _ = std::fs::remove_dir_all(&root);
        let (status, body) = get_(&root, "", "/").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.contains("Not a readable OKF bundle"), "{body}");
    }
}

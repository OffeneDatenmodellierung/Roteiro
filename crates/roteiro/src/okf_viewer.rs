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
use std::sync::Arc;

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
pub fn router(root: PathBuf, base: &str) -> Router {
    let state = Viewer {
        root: Arc::new(root),
        base: Arc::new(base.to_owned()),
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
const CSP: &str = "default-src 'self'; img-src 'self' data:; object-src 'none'; base-uri 'none'";

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
    let base = v.base.as_str();
    let view = match view::overview(&v.root) {
        Ok(v) => v,
        Err(e) => return unreadable(&e),
    };

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
    let base = v.base.as_str();
    let found = match view::concept(&v.root, &id) {
        Ok(c) => c,
        Err(e) => return unreadable(&e),
    };
    let Some(c) = found else {
        return (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_SECURITY_POLICY, CSP)],
            Html(format!(
                "<p>The bundle contains no concept <code>{}</code>. \
                 <a href=\"/\">Back to the bundle</a>.</p>",
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
             img-src 'self' data:; object-src 'none'; base-uri 'none'",
        ),
    );
    res
}

async fn graph_json(State(v): State<Viewer>) -> Response {
    match view::graph(&v.root) {
        Ok(g) => (
            [(header::CONTENT_TYPE, "application/json")],
            serde_json::to_string(&g).unwrap_or_else(|_| "{}".to_owned()),
        )
            .into_response(),
        Err(e) => unreadable(&e),
    }
}

/// A file from inside the bundle — an image a concept embeds, and nothing else.
///
/// The guard is `view::safe_bundle_file`, the same one the renderer applies when
/// it decides whether to emit a `/f/` href at all. Re-applied here because a
/// reader can type a URL: trusting that only our own hrefs arrive would put the
/// check on the wrong side of the boundary.
async fn file(State(v): State<Viewer>, UrlPath(path): UrlPath<String>) -> Response {
    let Some(resolved) = view::safe_bundle_file(&v.root, &path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(bytes) = std::fs::read(&resolved) else {
        return StatusCode::NOT_FOUND.into_response();
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
            (header::CONTENT_SECURITY_POLICY, CSP),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        bytes,
    )
        .into_response()
}

async fn stylesheet() -> Response {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], STYLE).into_response()
}

async fn cytoscape() -> Response {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
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
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Every page carries a policy that cannot reach the network.
    #[tokio::test]
    async fn responses_carry_a_content_security_policy() {
        let root = sample();
        for uri in ["/", "/c/metrics/revenue"] {
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
            assert!(csp.contains("default-src 'self'"), "{uri}: {csp}");
            assert!(csp.contains("object-src 'none'"), "{uri}: {csp}");
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

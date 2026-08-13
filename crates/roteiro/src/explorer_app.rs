//! The served **workspace-explorer web app** (ADR-0010): a self-contained,
//! same-origin UI mounted alongside the read-only `/v1/graph/*` data API by the
//! llama-free `roteiro explorer` server ([`crate::main::run_explorer`]).
//!
//! Three static assets, all committed to the repo and embedded at compile time
//! with `include_str!` — no npm, no build step, no external fetch:
//!
//! - `GET /` (and `/explorer`) → the HTML shell;
//! - `GET /app.js` → our hand-written, dependency-free ES app; and
//! - `GET /vendor/cytoscape.min.js` → the **vendored** cytoscape.js UMD bundle
//!   (the one client-side dependency; see ADR-0010 for why a real graph library
//!   is warranted for the interactive topology of ~1,300 nodes).
//!
//! The app is served *only* by the explorer server; a full `serve` build keeps
//! exposing just the JSON API (no bundled UI). It talks to the same origin's
//! `/v1/graph/*` endpoints, so there is no CORS surface. This is distinct from
//! the script-free static `links --matrix --html` export ([`crate::overview`]),
//! which stays a single self-contained file with no JavaScript.

use axum::Router;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::get;

/// The HTML shell: header, workspace switcher, stat tiles, legend, and the
/// topology + matrix panels. References `/app.js` and `/vendor/cytoscape.min.js`.
const SHELL_HTML: &str = include_str!("assets/index.html");

/// Our hand-written ES app: fetches `/v1/graph/*`, renders the tiles/topology/
/// matrix, and emits drill-in navigation intents (the target view is a later PR).
const APP_JS: &str = include_str!("assets/app.js");

/// The vendored cytoscape.js UMD bundle, committed to the repo (ADR-0010). A
/// single prebuilt file served verbatim — no npm and no build step.
const CYTOSCAPE_JS: &str = include_str!("assets/cytoscape.min.js");

/// `text/html` for the shell; both scripts are served as `text/javascript`. All
/// three are UTF-8 and cached briefly (they change only when the binary does).
const HTML: &str = "text/html; charset=utf-8";
const JS: &str = "text/javascript; charset=utf-8";

/// Build the static web-app router: the HTML shell, the app script, and the
/// vendored cytoscape bundle. Stateless (`Router`), so it merges cleanly into the
/// stateful `/v1/graph/*` router the explorer server builds.
pub fn router() -> Router {
    Router::new()
        .route("/", get(|| async { asset(HTML, SHELL_HTML) }))
        .route("/explorer", get(|| async { asset(HTML, SHELL_HTML) }))
        .route("/app.js", get(|| async { asset(JS, APP_JS) }))
        .route(
            "/vendor/cytoscape.min.js",
            get(|| async { asset(JS, CYTOSCAPE_JS) }),
        )
}

/// One embedded asset as a `200 OK` with an explicit content-type. Bodies are
/// `&'static str`, so serving copies nothing but the header.
fn asset(content_type: &'static str, body: &'static str) -> Response {
    ([(header::CONTENT_TYPE, content_type)], body).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt as _;
    use tower::ServiceExt as _; // for `oneshot`

    /// Drive one GET against the app router, returning `(status, content-type,
    /// body)`.
    async fn get(uri: &str) -> (StatusCode, String, String) {
        let resp = router()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let ct = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        (status, ct, String::from_utf8_lossy(&body).into_owned())
    }

    #[tokio::test]
    async fn root_serves_html_shell_referencing_app_and_cytoscape() {
        let (status, ct, body) = get("/").await;
        assert_eq!(status, StatusCode::OK);
        assert!(ct.starts_with("text/html"), "content-type was {ct}");
        assert!(body.contains("<!doctype html>"));
        assert!(body.contains("/app.js"), "shell must load our app script");
        assert!(
            body.contains("/vendor/cytoscape.min.js"),
            "shell must load the vendored graph library"
        );
    }

    #[tokio::test]
    async fn explorer_alias_serves_the_same_shell() {
        let (status, ct, body) = get("/explorer").await;
        assert_eq!(status, StatusCode::OK);
        assert!(ct.starts_with("text/html"));
        assert!(body.contains("<!doctype html>"));
    }

    #[tokio::test]
    async fn app_js_is_served_as_javascript() {
        let (status, ct, body) = get("/app.js").await;
        assert_eq!(status, StatusCode::OK);
        assert!(ct.contains("javascript"), "content-type was {ct}");
        assert!(!body.is_empty());
        // It must talk to the data API it is built against.
        assert!(body.contains("/v1/graph/workspaces"));
    }

    #[tokio::test]
    async fn vendored_cytoscape_is_nonempty_javascript() {
        let (status, ct, body) = get("/vendor/cytoscape.min.js").await;
        assert_eq!(status, StatusCode::OK);
        assert!(ct.contains("javascript"), "content-type was {ct}");
        assert!(
            body.len() > 100_000,
            "the vendored UMD bundle is substantial"
        );
        assert!(body.contains("cytoscape"), "it is the cytoscape library");
    }
}

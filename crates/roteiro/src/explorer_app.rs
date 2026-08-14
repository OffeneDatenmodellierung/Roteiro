//! The served **workspace-explorer web app** (ADR-0010): a self-contained,
//! same-origin UI mounted alongside the read-only `/v1/graph/*` data API by the
//! llama-free `roteiro explorer` server ([`crate::main::run_explorer`]).
//!
//! Three static assets, all committed to the repo and embedded at compile time
//! with `include_str!` — no npm, no build step, no external fetch:
//!
//! - `GET /` (and `/explorer`) → the HTML shell;
//! - `GET /app.js` → our hand-written, dependency-free ES app;
//! - `GET /vendor/cytoscape.min.js` → the **vendored** cytoscape.js UMD bundle
//!   (the one client-side dependency; see ADR-0010 for why a real graph library
//!   is warranted for the interactive topology of ~1,300 nodes); and
//! - `GET /sticker.svg` → the **vendored** Roteiro sticker logo (copied from
//!   `website/public/sticker.svg`), shown in the workspace-selector landing.
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

/// The HTML shell: the workspace view (switcher, stat tiles, legend, topology +
/// matrix panels) and the project drill-in view (dark graph canvas + right-hand
/// hotspots/node/ask panels). References `/app.js` and `/vendor/cytoscape.min.js`.
const SHELL_HTML: &str = include_str!("assets/index.html");

/// Our hand-written ES app: fetches `/v1/graph/*`, renders the workspace view
/// (tiles/topology/matrix) and the hash-routed project graph view (nodes coloured
/// by provenance, hotspots/debt/node panels), and drives drill/back navigation.
const APP_JS: &str = include_str!("assets/app.js");

/// The vendored cytoscape.js UMD bundle, committed to the repo (ADR-0010). A
/// single prebuilt file served verbatim — no npm and no build step.
const CYTOSCAPE_JS: &str = include_str!("assets/cytoscape.min.js");

/// The vendored Roteiro sticker logo, copied from `website/public/sticker.svg`
/// and committed alongside the other assets so the app stays self-contained (no
/// external fetch). Served as the logo in the workspace-selector landing header.
const STICKER_SVG: &str = include_str!("assets/sticker.svg");

/// `text/html` for the shell; both scripts are served as `text/javascript`; the
/// sticker as `image/svg+xml`. All are UTF-8.
const HTML: &str = "text/html; charset=utf-8";
const JS: &str = "text/javascript; charset=utf-8";
const SVG: &str = "image/svg+xml; charset=utf-8";

/// `Cache-Control` for the assets, which change only when the binary does. The
/// shell is the entry point, so it is cached only briefly; the scripts — chiefly
/// the ~365 KB vendored cytoscape bundle — are cached for an hour so a browser
/// re-uses them across page loads instead of re-fetching on every visit.
const CACHE_HTML: &str = "public, max-age=300";
const CACHE_JS: &str = "public, max-age=3600";

/// Build the static web-app router: the HTML shell, the app script, and the
/// vendored cytoscape bundle. Stateless (`Router`), so it merges cleanly into the
/// stateful `/v1/graph/*` router the explorer server builds.
pub fn router() -> Router {
    Router::new()
        .route("/", get(|| async { asset(HTML, CACHE_HTML, SHELL_HTML) }))
        .route(
            "/explorer",
            get(|| async { asset(HTML, CACHE_HTML, SHELL_HTML) }),
        )
        .route("/app.js", get(|| async { asset(JS, CACHE_JS, APP_JS) }))
        .route(
            "/vendor/cytoscape.min.js",
            get(|| async { asset(JS, CACHE_JS, CYTOSCAPE_JS) }),
        )
        // The sticker logo changes only when the binary does, like the scripts, so
        // it takes the same hour-long `Cache-Control` as the vendored bundle.
        .route(
            "/sticker.svg",
            get(|| async { asset(SVG, CACHE_JS, STICKER_SVG) }),
        )
}

/// One embedded asset as a `200 OK` with an explicit content-type and a
/// `Cache-Control` so browsers actually cache it. Bodies are `&'static str`, so
/// serving copies nothing but the headers.
fn asset(content_type: &'static str, cache_control: &'static str, body: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, cache_control),
        ],
        body,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt as _;
    use tower::ServiceExt as _; // for `oneshot`

    /// Drive one GET against the app router, returning `(status, content-type,
    /// cache-control, body)`.
    async fn get(uri: &str) -> (StatusCode, String, String, String) {
        let resp = router()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let headers = resp.headers();
        let header_str = |name: header::HeaderName| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_owned()
        };
        let ct = header_str(header::CONTENT_TYPE);
        let cache = header_str(header::CACHE_CONTROL);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            ct,
            cache,
            String::from_utf8_lossy(&body).into_owned(),
        )
    }

    #[tokio::test]
    async fn root_serves_html_shell_referencing_app_and_cytoscape() {
        let (status, ct, _cache, body) = get("/").await;
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
        let (status, ct, _cache, body) = get("/explorer").await;
        assert_eq!(status, StatusCode::OK);
        assert!(ct.starts_with("text/html"));
        assert!(body.contains("<!doctype html>"));
    }

    #[tokio::test]
    async fn shell_scaffolds_the_project_drill_in_view() {
        // The HTML shell must carry the project graph view's scaffold: the view
        // container, the graph canvas, the provenance legend, the search box, and
        // the right-panel tabs (incl. the disabled Ask tab).
        let (status, _ct, _cache, body) = get("/").await;
        assert_eq!(status, StatusCode::OK);
        for needle in [
            "id=\"view-project\"",
            "id=\"p-graph\"",
            "colour: provenance",
            "find in this repo",
            "data-tab=\"hotspots\"",
            "data-tab=\"node\"",
            "data-tab=\"ask\"",
        ] {
            assert!(body.contains(needle), "shell must contain `{needle}`");
        }
        // The Ask tab is present but disabled (llama is a later PR), conveyed via
        // `aria-disabled` (not a native `disabled`, so it stays perceivable).
        assert!(
            body.contains("requires the model build") || body.contains("roteiro serve --models"),
            "Ask tab must explain it needs the model build"
        );
        // The ARIA tab pattern must be wired: tabs point at their panels, panels
        // back at their tabs, and the disabled tab is aria-disabled.
        for needle in [
            "role=\"tablist\"",
            "aria-controls=\"p-pane-hotspots\"",
            "aria-selected=\"true\"",
            "aria-labelledby=\"p-tab-node\"",
            "aria-disabled=\"true\"",
        ] {
            assert!(body.contains(needle), "shell must contain `{needle}`");
        }
    }

    #[tokio::test]
    async fn served_assets_are_free_of_raw_control_chars() {
        // The assets must stay reviewable/tooling-safe: no stray control bytes
        // (e.g. a `0x01` separator once used in a cytoscape edge id).
        for uri in ["/", "/app.js"] {
            let (_s, _c, _cc, body) = get(uri).await;
            assert!(
                !body
                    .bytes()
                    .any(|b| b < 0x20 && b != b'\t' && b != b'\n' && b != b'\r'),
                "{uri} contains a raw control character"
            );
        }
    }

    #[tokio::test]
    async fn app_js_consumes_the_project_data_endpoints() {
        // The drill-in view reads the per-project endpoints this server exposes; a
        // rename on either side would break the wiring, so pin it here.
        let (status, _ct, _cache, body) = get("/app.js").await;
        assert_eq!(status, StatusCode::OK);
        for needle in [
            "/hotspots",
            "/debt",
            "/node/",
            "loadProject",
            "navigateToProject",
        ] {
            assert!(body.contains(needle), "app.js must reference `{needle}`");
        }
    }

    #[tokio::test]
    async fn app_js_wires_the_ask_tab_to_capabilities_and_chat() {
        // The Ask tab is data-driven: the app reads `/v1/graph/capabilities` to
        // decide whether to enable it, and — when enabled — posts to the
        // project-scoped chat endpoint. Pin both so a rename on either side (the
        // capability route or the chat route) is caught here.
        let (status, _ct, _cache, body) = get("/app.js").await;
        assert_eq!(status, StatusCode::OK);
        for needle in [
            "/v1/graph/capabilities",
            "loadCapabilities",
            "enableAskTab",
            "/chat/completions",
        ] {
            assert!(body.contains(needle), "app.js must reference `{needle}`");
        }
    }

    #[tokio::test]
    async fn app_js_wires_the_workspace_ask_to_capabilities_and_chat() {
        // The WORKSPACE-level Ask is data-driven off the SAME capability signal as
        // the project Ask (`loadCapabilities` enables both), but — answering about the
        // SELECTED workspace and its projects — it posts to the workspace-scoped
        // `/v1/workspaces/{ws}/chat/completions` (built from the selected workspace
        // name), whose tools are confined to that workspace (ADR-0008); the model
        // still ranges over the workspace via `list_projects` + the per-tool `project`
        // argument. Pin the wiring so a rename on either side is caught.
        let (status, _ct, _cache, body) = get("/app.js").await;
        assert_eq!(status, StatusCode::OK);
        for needle in [
            "loadCapabilities",
            "enableWorkspaceAsk",
            "submitWorkspaceAsk",
            // the workspace-scoped chat route, built from the selected workspace name
            "`/v1/workspaces/${encodeURIComponent(workspace)}/chat/completions`",
            "list_projects",
        ] {
            assert!(body.contains(needle), "app.js must reference `{needle}`");
        }
    }

    #[tokio::test]
    async fn app_js_workspace_ask_linkifies_project_qualified_citations() {
        // A workspace Ask answer cites PROJECT-QUALIFIED node keys (`<project>::<key>`,
        // and a project/dir name may contain `-`/`.`, e.g. `stream-sync::sym:rust:…`).
        // The linkifier must recognise the whole qualified token — not mis-split a
        // hyphenated project at an inner `:` — and route the click, WITH its project,
        // into that project's graph. Pin the wiring: a dedicated `WS_KEY_RE` whose
        // optional project segment spans `-`/`.` before the `::` separator, driven
        // through `renderAnswer` with the project group, and a `wsAskGoToProject`
        // that parses the qualifier and drills in.
        let (status, _ct, _cache, body) = get("/app.js").await;
        assert_eq!(status, StatusCode::OK);
        for needle in [
            "WS_KEY_RE",
            // the optional `<project>::` qualifier, project segment allowing `-`/`.`
            "(?:([A-Za-z0-9_.-]+)::)?",
            // the workspace Ask renders with the qualified grammar (project in group 1,
            // prefix in group 2 → pass group 2 as the URL-checkable prefix)
            "renderAnswer(answer, content, wsAskGoToProject, WS_KEY_RE, 2)",
            "function wsAskGoToProject",
            "function parseQualifiedKey",
        ] {
            assert!(body.contains(needle), "app.js must reference `{needle}`");
        }
    }

    #[tokio::test]
    async fn app_js_ask_panels_render_a_model_dropdown_gated_on_multi_model() {
        // Both Ask panels swap their static `model: <name>` label for a `<select>`
        // model chooser WHEN more than one chat-capable model is served — populated
        // from the capabilities `models` list, default-selected to the served
        // default (`askModels[0]`, generative-first). With exactly one model it must
        // stay a static label (no pointless 1-option dropdown), and the PICKED model
        // (`askModel`/`wsAskModel`) — not a hardcoded index — is what each submit
        // sends. Pin the gating + wiring so a regression is caught headlessly.
        let (status, _ct, _cache, body) = get("/app.js").await;
        assert_eq!(status, StatusCode::OK);
        for needle in [
            // the shared control, and its single-model static-label branch (the gate)
            "function askModelControl",
            "models.length === 1",
            // multi-model → a <select> populated from the capabilities model list,
            // with the option matching the current pick pre-selected
            "p-ask-model-select",
            "o.selected = true",
            // both panels resolve their pick (preserve-or-default) and update it live
            "state.askModel = resolveAskModel(state.askModel)",
            "state.wsAskModel = resolveAskModel(state.wsAskModel)",
            // the PICKED model is what goes on the wire (project + workspace Ask)
            "model: state.askModel || state.askModels[0]",
            "model: state.wsAskModel || state.askModels[0]",
        ] {
            assert!(body.contains(needle), "app.js must reference `{needle}`");
        }
    }

    #[tokio::test]
    async fn app_js_ask_model_pick_survives_re_render_and_falls_back_when_unserved() {
        // `enableAskTab`/`enableWorkspaceAsk` are idempotent — re-running them must NOT
        // silently discard the user's dropdown choice. The pick is routed through
        // `resolveAskModel`, which PRESERVES a remembered model when it is still in the
        // served `askModels` list and only falls back to the default (`askModels[0]`)
        // when it is unset or no longer served; the `<select>` then pre-selects the
        // option matching that resolved pick, so `state` and the dropdown stay in sync
        // across re-renders. Pin that contract (both panels) headlessly.
        let (status, _ct, _cache, body) = get("/app.js").await;
        assert_eq!(status, StatusCode::OK);
        for needle in [
            "function resolveAskModel",
            // preserve iff still served, else fall back to the generative-first default
            "if (current && state.askModels.includes(current)) return current;",
            "return state.askModels[0] || null;",
            // both panels resolve (not blindly reset) their remembered pick
            "state.askModel = resolveAskModel(state.askModel)",
            "state.wsAskModel = resolveAskModel(state.wsAskModel)",
            // the rendered <select> mirrors the resolved pick (fallback when unserved)
            "const current = models.includes(selected) ? selected : models[0];",
            "if (m === current) o.selected = true;",
        ] {
            assert!(body.contains(needle), "app.js must reference `{needle}`");
        }
    }

    #[tokio::test]
    async fn app_js_cited_node_click_opens_the_node_detail_with_content() {
        // Clicking a cited node must NAVIGATE TO and OPEN it: the project Ask selects
        // the node (which activates the Node tab + loads its detail); the workspace
        // Ask drills into the cited key's PROJECT and then — via `focusPending` —
        // opens the node's detail even when the graph view doesn't plot that node
        // (e.g. a `file:` citation). The Node detail renders the node's captured
        // `meta.content` (a file/doc's text) so a cited node can be read in place.
        let (status, _ct, _cache, body) = get("/app.js").await;
        assert_eq!(status, StatusCode::OK);
        for needle in [
            // project Ask: a citation click selects → opens the Node tab + detail
            "function askGoToNode",
            // workspace Ask: drill into the cited key's project, then open the node
            "function focusPending",
            // `focusPending` always opens the node detail (not gated on it being plotted)
            "// whether or not the graph happens to plot it.",
            "activateTab(\"node\")",
            "loadNodeDetail(state.projectWs, state.project, key)",
            // Node detail surfaces the node's captured content (file/doc text)
            "exp.meta.content",
            "p-node-content",
        ] {
            assert!(body.contains(needle), "app.js must reference `{needle}`");
        }
    }

    #[tokio::test]
    async fn shell_styles_the_node_content_and_model_dropdown() {
        // The two new UX surfaces need their styles shipped in the shell: the Node
        // detail's captured-content block and the Ask model `<select>`. Pin the class
        // hooks so the CSS isn't dropped in a refactor.
        let (status, _ct, _cache, body) = get("/").await;
        assert_eq!(status, StatusCode::OK);
        for needle in [".p-node-content", "select.p-ask-model-select"] {
            assert!(body.contains(needle), "shell CSS must define `{needle}`");
        }
    }

    #[tokio::test]
    async fn shell_scaffolds_the_workspace_ask_panel_gated_hidden() {
        // The workspace (overview) view carries a graph-grounded Ask panel. It must
        // ship HIDDEN — the llama-free explorer reports `ask:false`, so the panel only
        // appears once `/v1/graph/capabilities` enables Ask (the same gate as the
        // project Ask tab), matching that tab's disabled-in-explorer behaviour.
        let (status, _ct, _cache, body) = get("/").await;
        assert_eq!(status, StatusCode::OK);
        for needle in ["id=\"ws-ask-panel\"", "id=\"ws-ask-body\""] {
            assert!(body.contains(needle), "shell must contain `{needle}`");
        }
        assert!(
            body.contains("id=\"ws-ask-panel\" hidden"),
            "the workspace Ask panel must ship hidden, gated on capabilities"
        );
    }

    #[tokio::test]
    async fn shell_places_the_workspace_ask_panel_under_the_drill_into_row() {
        // Placement (pinned): the workspace Ask panel renders DIRECTLY UNDER the
        // `drill into` project-chip row (`#projects-bar`) and ABOVE the Topology and
        // config-override-matrix panels — the panel a visitor reaches for right after
        // choosing where to drill. Assert the source order in the served shell so a
        // reflow that moves it back below the matrix (or above the chips) is caught.
        let (status, _ct, _cache, body) = get("/").await;
        assert_eq!(status, StatusCode::OK);
        let at = |needle: &str| {
            body.find(needle)
                .unwrap_or_else(|| panic!("shell must contain `{needle}`"))
        };
        let drill = at("id=\"projects-bar\"");
        let ask = at("id=\"ws-ask-panel\"");
        let topology = at("id=\"topology\"");
        let matrix = at("id=\"matrix\"");
        assert!(
            drill < ask,
            "the workspace Ask panel must sit AFTER the `drill into` row"
        );
        assert!(
            ask < topology && ask < matrix,
            "the workspace Ask panel must sit ABOVE the Topology and matrix panels"
        );
    }

    #[tokio::test]
    async fn app_js_is_served_as_javascript() {
        let (status, ct, cache, body) = get("/app.js").await;
        assert_eq!(status, StatusCode::OK);
        assert!(ct.contains("javascript"), "content-type was {ct}");
        assert!(
            cache.contains("max-age="),
            "app.js must be cacheable: {cache}"
        );
        assert!(!body.is_empty());
        // It must talk to the data API it is built against.
        assert!(body.contains("/v1/graph/workspaces"));
    }

    #[tokio::test]
    async fn sticker_is_served_as_cacheable_nonempty_svg() {
        // The workspace-selector landing shows the vendored sticker logo. The route
        // must return a non-empty SVG document with the right content-type and a
        // caching `Cache-Control` consistent with the other static assets.
        let (status, ct, cache, body) = get("/sticker.svg").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            ct.contains("image/svg+xml"),
            "sticker must be served as SVG: {ct}"
        );
        assert!(
            cache.contains("max-age=") && cache.contains("public"),
            "sticker must send a caching Cache-Control: {cache}"
        );
        assert!(!body.is_empty(), "the sticker asset must be non-empty");
        assert!(body.contains("<svg"), "it is an SVG document");
    }

    #[tokio::test]
    async fn shell_scaffolds_the_workspace_selector_landing() {
        // The entry point is the workspace-selector landing: the shell must carry
        // its view container, the card grid the app fills from `/v1/graph/workspaces`,
        // and the sticker logo (pointing at the served route).
        let (status, _ct, _cache, body) = get("/").await;
        assert_eq!(status, StatusCode::OK);
        for needle in ["id=\"view-select\"", "id=\"select-grid\"", "/sticker.svg"] {
            assert!(body.contains(needle), "shell must contain `{needle}`");
        }
    }

    #[tokio::test]
    async fn app_js_routes_the_selector_by_workspace_type() {
        // The landing/routing logic is JS: the app renders the selector and routes
        // by project count (auto-entering a lone workspace). Pin the entry hooks so
        // a rename is caught headlessly (full visual QA needs a browser).
        let (status, _ct, _cache, body) = get("/app.js").await;
        assert_eq!(status, StatusCode::OK);
        for needle in ["renderSelector", "goByType", "showSelectView"] {
            assert!(body.contains(needle), "app.js must reference `{needle}`");
        }
    }

    #[tokio::test]
    async fn vendored_cytoscape_is_cacheable_nonempty_javascript() {
        let (status, ct, cache, body) = get("/vendor/cytoscape.min.js").await;
        assert_eq!(status, StatusCode::OK);
        assert!(ct.contains("javascript"), "content-type was {ct}");
        // The ~365 KB bundle must be cached by the browser, not re-fetched each load.
        assert!(
            cache.contains("max-age=") && cache.contains("public"),
            "vendored bundle must send a caching Cache-Control: {cache}"
        );
        assert!(
            body.len() > 100_000,
            "the vendored UMD bundle is substantial"
        );
        assert!(body.contains("cytoscape"), "it is the cytoscape library");
    }
}

//! The served **workspace-explorer web app** (ADR-0010): a self-contained,
//! same-origin UI mounted alongside the read-only `/v1/graph/*` data API by the
//! llama-free `roteiro explorer` server (`crate::main::run_explorer`).
//!
//! Static assets, all committed to the repo and embedded at compile time with
//! `include_str!` — no npm, no build step, no external fetch:
//!
//! - `GET /` (and `/explorer`) → the HTML shell, with the app's ONE token master
//!   ([`crate::theme`]) spliced into its inline `<style>`, so the palette
//!   arrives in the same round-trip as the markup that uses it;
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
//! the script-free static `links --matrix --html` export (`crate::overview`),
//! which stays a single self-contained file with no JavaScript.

use axum::Router;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::get;

/// The HTML shell *before* the palette is spliced in: the workspace view
/// (switcher, stat tiles, legend, topology + matrix panels) and the project
/// drill-in view (dark graph canvas + right-hand hotspots/node/ask panels).
/// References `/app.js` and `/vendor/cytoscape.min.js`.
const SHELL_TEMPLATE: &str = include_str!("assets/index.html");

/// Where [`crate::theme::TOKENS`] goes in [`SHELL_TEMPLATE`].
///
/// A marker rather than a `<link>`: the shell must stay one self-contained
/// document — a second round-trip for the palette would paint the app unstyled
/// first — and the same master has to be inlinable for the `links --matrix
/// --html` export, which has no server at all.
const TOKENS_MARKER: &str = "/* @tokens */";

/// The served HTML shell: [`SHELL_TEMPLATE`] with the app's ONE token master
/// spliced into its inline `<style>`.
///
/// Built once, on first request. `str::replace` is silent when the needle is
/// absent — it would serve a shell with no palette at all, and every rule in it
/// would fall back to inherited/initial and render *plausibly wrong* rather than
/// erroring, which is precisely the #512 failure mode. So
/// `the_shell_splices_in_the_token_master` asserts the marker was really there.
static SHELL_HTML: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| SHELL_TEMPLATE.replace(TOKENS_MARKER, crate::theme::TOKENS));

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
        .route(
            "/",
            get(|| async { asset(HTML, CACHE_HTML, SHELL_HTML.as_str()) }),
        )
        .route(
            "/explorer",
            get(|| async { asset(HTML, CACHE_HTML, SHELL_HTML.as_str()) }),
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
    use std::collections::{BTreeMap, BTreeSet};
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

    /// The explorer is served from this binary, so it is a **consumer of our own
    /// API** — and nothing else checks that the two agree, because the app is
    /// hand-written ES with no test runner.
    ///
    /// Since #623 the topology's `role` is `root`/`intermediate`/`leaf`/`isolated`
    /// and never `hub`. The app used to select the baseline with
    /// `p.role === "hub"` and count spokes with `p.role !== "hub"`; left alone,
    /// the first would have matched nothing and the second everything, so the
    /// diagram would have marked no hub and the tile would have counted the hub
    /// as a spoke of itself — a silent misrender, since both are valid JS against
    /// a payload whose shape merely changed underneath them.
    #[test]
    fn the_app_does_not_select_the_hub_by_a_role_the_api_no_longer_emits() {
        // Code lines only. The comments at those call sites quote the old
        // comparison in order to explain why it is wrong, and a whole-file
        // substring match cannot tell an explanation from an instruction — it
        // failed on the very comment describing the fix.
        let offending: Vec<&str> = APP_JS
            .lines()
            .map(str::trim)
            .filter(|l| !l.starts_with("//"))
            .filter(|l| l.contains(r#"role === "hub""#) || l.contains(r#"role !== "hub""#))
            .collect();
        assert!(
            offending.is_empty(),
            "app.js still compares the API's `role` against \"hub\"; the baseline \
             is `isMatrixHub` now: {offending:?}"
        );
        assert!(
            APP_JS.contains("isMatrixHub"),
            "…and it must read the field that replaced it"
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
    async fn app_js_ask_survives_a_null_content_tool_call_turn() {
        // `ChatMessageDto.content` is now `Option<String>` (#485): a turn that
        // carries `tool_calls` serialises `content: null`. Neither Ask panel sends
        // `tools`, so neither can receive that turn today — but the field is
        // nullable for every caller from now on, and a `null` must degrade to the
        // empty-answer message rather than throw.
        //
        // Confirmed live against a real `finish_reason: "tool_calls"` body: both
        // read sites evaluate to `"(the model returned an empty answer)"`. What is
        // pinned here is the guard that makes that true — BOTH Ask panels reading
        // `message.content` behind the `||` fallback rather than bare.
        let (status, _ct, _cache, body) = get("/app.js").await;
        assert_eq!(status, StatusCode::OK);
        let guarded = "data.choices[0].message.content) ||";
        assert_eq!(
            body.matches(guarded).count(),
            2,
            "both Ask panels must read `message.content` behind the empty-answer \
             fallback, so a `null` degrades instead of throwing"
        );
        assert!(
            body.contains("\"(the model returned an empty answer)\""),
            "the fallback the guard falls back TO"
        );
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
    async fn app_js_labels_generated_media_content_and_never_as_extracted() {
        // ADR-0015's UI clause: a media node's generated content is surfaced
        // ALWAYS visibly attributed to the producer that made it, with a
        // per-blob rebuild — and it must never render as if it were extracted.
        //
        // Pin the wiring headlessly (full visual QA needs a browser): the
        // section reads the `generated` array the node endpoint adds, renders it
        // under its own heading with the model/producer attribution and the
        // "produced by a model" warning, and offers the per-blob rebuild
        // command. It must NOT be merged into the `p-node-content` block, which
        // is where a node's EXTRACTED text lives.
        let (status, _ct, _cache, body) = get("/app.js").await;
        assert_eq!(status, StatusCode::OK);
        for needle in [
            "function generatedSection",
            // driven off the endpoint's sibling key, not off `meta.content`
            "generatedSection(exp.generated)",
            // its own heading, its own warning, and attribution on the record
            "Generated media content",
            "produced by a model — not extracted from the source",
            "`producer ${r.producer || \"?\"}`",
            "`generated · ${r.kind || \"media\"}`",
            // a gate refusal shows its measurement instead of an empty panel
            "r.skipped",
            "no model was run",
            // the per-blob rebuild affordance
            "p-generated-rebuild",
            "r.rebuild",
        ] {
            assert!(body.contains(needle), "app.js must reference `{needle}`");
        }
        // The generated block must be a DIFFERENT element from the extracted
        // content block. If a refactor ever renders a transcript into
        // `p-node-content`, this is what catches it.
        assert!(
            !body.contains("p-node-content\" }, el(\"code\", { text: r.text"),
            "generated text must never be rendered into the extracted-content block"
        );
    }

    #[tokio::test]
    async fn shell_styles_generated_media_content_distinctly() {
        // Generated content must not LOOK like extracted content either: it has
        // its own class hooks, and they must survive a CSS refactor.
        let (status, _ct, _cache, body) = get("/").await;
        assert_eq!(status, StatusCode::OK);
        for needle in [
            ".p-generated",
            ".p-generated-warning",
            ".p-generated-producer",
            ".p-generated-rebuild",
            ".p-badge.p-generated-badge",
        ] {
            assert!(body.contains(needle), "shell CSS must define `{needle}`");
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

    /// Every custom property a stylesheet declares, as `scope -> name -> value`.
    ///
    /// Scopes are **nesting-aware**: the key is the enclosing at-rules and the
    /// selector joined, so `@media (prefers-color-scheme: dark)`'s `:root` is a
    /// scope of its own rather than folded into the bare `:root`. That
    /// distinction is the whole point since the palette gained a dark mode — a
    /// dark-only token would otherwise look like a `:root` token to rule (2)
    /// below, which is exactly the hole that rule exists to close.
    fn css_scopes(css: &str) -> BTreeMap<String, BTreeMap<String, String>> {
        let css = crate::theme::without_comments(css);
        let mut scopes: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        let mut stack: Vec<String> = Vec::new();
        let mut buf = String::new();
        for ch in css.chars() {
            match ch {
                '{' => {
                    // The prelude's last non-blank line, so a preceding rule's
                    // trailing whitespace isn't swept up with the selector.
                    let prelude = buf
                        .lines()
                        .rfind(|l| !l.trim().is_empty())
                        .unwrap_or_default()
                        .trim()
                        .to_owned();
                    stack.push(prelude);
                    buf.clear();
                }
                '}' => {
                    if !stack.is_empty() {
                        let key = stack.join(" ");
                        for (name, value) in buf
                            .split(';')
                            .map(str::trim)
                            .filter_map(|d| d.strip_prefix("--"))
                            .filter_map(|d| d.split_once(':'))
                        {
                            scopes
                                .entry(key.clone())
                                .or_default()
                                .insert(format!("--{}", name.trim()), value.trim().to_owned());
                        }
                        stack.pop();
                    }
                    buf.clear();
                }
                _ => buf.push(ch),
            }
        }
        scopes
    }

    /// The one inline `<style>` block of a served shell.
    fn style_block(body: &str) -> &str {
        body.split_once("<style>")
            .and_then(|(_, rest)| rest.split_once("</style>"))
            .map(|(css, _)| css)
            .expect("the shell ships one inline <style> block")
    }

    /// The shell's `<style>` block, with its custom-property NAMES grouped by the
    /// scope that owns them (`:root`, `#view-project`, `[data-theme="dark"]`, …).
    fn palette_scopes(body: &str) -> BTreeMap<String, BTreeSet<String>> {
        css_scopes(style_block(body))
            .into_iter()
            .map(|(scope, decls)| (scope, decls.into_keys().collect()))
            .collect()
    }

    /// The shell really is themed by the app's ONE token master.
    ///
    /// `str::replace` is silent when the needle is absent, so a renamed or
    /// deleted marker would serve a shell with **no palette at all** — and
    /// because an undefined custom property is invalid at computed-value time
    /// rather than an error, the app would render plausibly wrong instead of
    /// failing. This is the assertion that makes the splice load-bearing.
    #[tokio::test]
    async fn the_shell_splices_in_the_token_master() {
        let (status, _ct, _cache, body) = get("/").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains(crate::theme::TOKENS),
            "the served shell does not carry `assets/tokens.css` verbatim — has \
             `{TOKENS_MARKER}` been renamed or reformatted in `index.html`?"
        );
        assert!(
            !body.contains(TOKENS_MARKER),
            "the marker survived into the served shell, so nothing was spliced"
        );
    }

    /// The review tool splices the palette the same way the server does.
    ///
    /// `scripts/resolve-explorer-theme.py` resolves the shell's declarations to
    /// literals so two revisions of a palette change can be diffed — it is what
    /// `shell_colour_vars_are_one_namespace_declared_by_both_views` tells a
    /// reader to reach for. Moving the palette behind a marker broke it: it
    /// parsed `index.html` directly, found no variable definitions at all, and
    /// reported `defines vars on []` with `<UNRESOLVED --bg>` for every colour
    /// on the page. It said so out loud and still nobody read it for a round.
    ///
    /// The marker is the seam between the two, so it is the thing to pin.
    #[test]
    fn the_theme_review_script_uses_the_same_splice_marker() {
        let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/resolve-explorer-theme.py");
        let Ok(text) = std::fs::read_to_string(&script) else {
            // A packaged crate has no `scripts/`. In a repository checkout the
            // file is always there, so this returns only where there is nothing
            // to compare.
            return;
        };
        assert!(
            text.contains(TOKENS_MARKER),
            "`resolve-explorer-theme.py` does not know the `{TOKENS_MARKER}` \
             marker, so it will read a shell with no palette and resolve every \
             colour to `<UNRESOLVED>`"
        );
        assert!(
            text.contains("assets/tokens.css"),
            "`resolve-explorer-theme.py` must read the same token master the \
             server splices in"
        );
    }

    /// The shell declares no colour of its own — it only names tokens.
    ///
    /// Scanned against `SHELL_TEMPLATE` (pre-splice), so the master's own values
    /// are not what is being looked at: this is about the ~1,200 lines of rules
    /// *around* it. Both halves fail independently — a stated colour renders fine
    /// and silently forks the identity, while a `var()` naming a token the master
    /// dropped renders plausibly wrong and errors nowhere.
    ///
    /// "Colour" means every syntax, not every syntax somebody listed: this guard
    /// first shipped recognising `#rrggbb` only, and two `rgba()` shadows sat
    /// here while it stayed green. See [`crate::theme::colour_literals`].
    #[test]
    fn the_shell_declares_no_colour_of_its_own() {
        let literals = crate::theme::colour_literals(style_block(SHELL_TEMPLATE));
        assert!(
            literals.is_empty(),
            "`index.html` hard-codes colours {literals:?} — name a token from \
             `assets/tokens.css` instead, or add one there if the role is new"
        );
        let dangling = crate::theme::dangling_tokens(SHELL_TEMPLATE);
        assert!(
            dangling.is_empty(),
            "the shell names {dangling:?}, which `assets/tokens.css` does not \
             declare — they resolve to nothing and fall back to inherited/initial"
        );

        // The stylesheet is not the only consumer. `app.js` resolves graph
        // colours with `getComputedStyle`, so those names are invisible to a
        // `var(--…)` scan — and `--match` and `--fg-strong` are reached from
        // NOWHERE else. Renaming either in the master would have returned `""`
        // to cytoscape, drawing an unstyled border, with every guard green.
        let declared = crate::theme::declared_names();
        let script_refs = crate::theme::script_token_refs(APP_JS);
        assert!(
            script_refs.len() >= 10,
            "only {} token lookups found in `app.js`; the scan has stopped \
             matching the accessor and is measuring nothing",
            script_refs.len()
        );
        let unresolved: Vec<&String> = script_refs
            .iter()
            .filter(|name| !declared.contains(*name))
            .collect();
        assert!(
            unresolved.is_empty(),
            "`app.js` asks `getComputedStyle` for {unresolved:?}, which \
             `assets/tokens.css` does not declare — the property resolves to an \
             empty string and cytoscape draws the element unstyled"
        );

        // The palette lives in ONE file. A rule declaring its own custom property
        // is a second place a colour can live, however local it looks.
        let local: Vec<String> = crate::theme::declarations(style_block(SHELL_TEMPLATE))
            .into_iter()
            .filter(|(prop, _)| prop.starts_with("--"))
            .map(|(prop, _)| prop)
            .collect();
        assert!(
            local.is_empty(),
            "`index.html` declares {local:?} of its own — custom properties \
             belong in `assets/tokens.css`, which every in-app surface reads"
        );
    }

    /// JS with string, template and comment contents blanked out, so a brace or a
    /// key-looking word inside one cannot be read as structure.
    ///
    /// **Length-preserving**: every blanked character is replaced one-for-one, so
    /// an offset into the result indexes the original. That is what lets
    /// `cytoscape_option_keys` report the key a reader can search for rather than
    /// the run of spaces it was blanked to.
    fn js_without_literals(js: &str) -> String {
        // Blank one character to as many spaces as it had BYTES: `js.len()` is a
        // byte count, and this file is full of `—`, `⚠` and `→`. Replacing a
        // 3-byte char with a 1-byte space silently shortens the copy and every
        // offset after it points at the wrong place.
        fn blank(out: &mut String, c: char) {
            if c == '\n' {
                out.push('\n');
            } else {
                for _ in 0..c.len_utf8() {
                    out.push(' ');
                }
            }
        }
        let mut out = String::with_capacity(js.len());
        let mut chars = js.chars().peekable();
        let mut quote: Option<char> = None;
        while let Some(c) = chars.next() {
            match quote {
                Some(q) => {
                    if c == '\\' {
                        blank(&mut out, c);
                        if let Some(next) = chars.next() {
                            blank(&mut out, next);
                        }
                        continue;
                    }
                    if c == q {
                        quote = None;
                        out.push(c);
                    } else {
                        blank(&mut out, c);
                    }
                }
                None => match c {
                    '"' | '\'' | '`' => {
                        quote = Some(c);
                        out.push(c);
                    }
                    '/' if chars.peek() == Some(&'/') => {
                        blank(&mut out, c);
                        while let Some(c) = chars.peek().copied() {
                            if c == '\n' {
                                break;
                            }
                            blank(&mut out, c);
                            chars.next();
                        }
                    }
                    _ => out.push(c),
                },
            }
        }
        out
    }

    /// The top-level keys of every `cytoscape({ … })` options object in `app.js`.
    ///
    /// Structure is read from the blanked copy; the key TEXT is sliced out of the
    /// original at the same offsets, so a quoted key reports as `"text-valign"`
    /// and not as the spaces it was blanked to.
    fn cytoscape_option_keys(js: &str) -> BTreeSet<String> {
        let blanked = js_without_literals(js);
        assert_eq!(
            blanked.len(),
            js.len(),
            "`js_without_literals` must be length-preserving for the offsets below"
        );
        let mut keys = BTreeSet::new();
        for (start, _) in blanked.match_indices("cytoscape({") {
            let base = start + "cytoscape({".len();
            let body = &blanked[base..];
            let mut depth = 0i32;
            let mut key_start: Option<usize> = None;
            for (i, c) in body.char_indices() {
                match c {
                    '{' | '[' | '(' => depth += 1,
                    // The options object's own closing brace, at depth 0, ends
                    // this call; anything else nested just unwinds a level.
                    '}' if depth == 0 => break,
                    ']' | ')' | '}' => depth -= 1,
                    ':' if depth == 0 => {
                        if let Some(from) = key_start.take() {
                            let name = js[base + from..base + i].trim().trim_matches('"');
                            if !name.is_empty() {
                                keys.insert(name.to_owned());
                            }
                        }
                    }
                    ',' if depth == 0 => key_start = None,
                    c if depth == 0 && !c.is_whitespace() => {
                        key_start.get_or_insert(i);
                    }
                    _ => {}
                }
            }
        }
        keys
    }

    /// Graph styling lives in the two `*GraphStyle` builders, and nowhere else.
    ///
    /// Extracting those builders out of the `cytoscape({…})` calls left seven
    /// style properties — `width`, `height`, `padding`, `label`, `text-wrap`,
    /// `text-valign`, `text-halign` — stranded at the TOP LEVEL of the options
    /// object, where cytoscape has no such options and simply ignored them. It
    /// parsed, it rendered correctly (the builder carries the same seven), and
    /// nothing said a word. Only the node stylesheet's own copy was doing any
    /// work.
    ///
    /// An allowlist of cytoscape's core options, so residue from the NEXT
    /// extraction is caught the same way — a key nobody listed is a failure
    /// rather than a silence.
    #[test]
    fn the_graph_options_carry_no_stranded_style_properties() {
        const CORE_OPTIONS: &[&str] = &[
            "autolock",
            "autoungrabify",
            "autounselectify",
            "boxSelectionEnabled",
            "container",
            "elements",
            "headless",
            "hideEdgesOnViewport",
            "layout",
            "maxZoom",
            "minZoom",
            "motionBlur",
            "pan",
            "panningEnabled",
            "pixelRatio",
            "selectionType",
            "style",
            "styleEnabled",
            "textureOnViewport",
            "touchTapThreshold",
            "userPanningEnabled",
            "userZoomingEnabled",
            "wheelSensitivity",
            "zoom",
            "zoomingEnabled",
        ];
        let keys = cytoscape_option_keys(APP_JS);
        assert!(
            keys.contains("style") && keys.contains("layout"),
            "the scan found no cytoscape options at all, so it is measuring \
             nothing: {keys:?}"
        );
        let stray: Vec<&String> = keys
            .iter()
            .filter(|k| !CORE_OPTIONS.contains(&k.as_str()))
            .collect();
        assert!(
            stray.is_empty(),
            "{stray:?} sit at the top level of a `cytoscape({{…}})` call, where \
             they are not options and do nothing. Style properties belong inside \
             a rule in `workspaceGraphStyle`/`projectGraphStyle`."
        );
    }

    /// The document's mode is written in ONE place, and that place re-resolves
    /// every live graph.
    ///
    /// cytoscape holds resolved colours rather than custom properties, and
    /// `#topology` sits OUTSIDE `#view-project` — so while the project view
    /// forces `data-theme="dark"` on `<html>`, the hidden workspace graph
    /// resolves dark too. Flipping the OS to light with the project view open
    /// restyled it dark, and walking back re-renders nothing when the workspace
    /// has not changed (`route` only calls `loadWorkspace` when `state.current`
    /// differs), so the topology sat dark on a light panel. Reproduced over CDP
    /// before the fix: panel `rgb(255,255,255)`, nodes `rgb(22,27,34)`.
    ///
    /// **This guard is structural, and that is a real limit.** This repository
    /// has no JavaScript engine and no browser in CI — nothing runs `app.js` —
    /// so no test here can observe the rendered colour. What it can hold is the
    /// shape that made the bug possible: two write sites, one of which forgot.
    /// It fails if a second write site appears, or if the single one stops
    /// restyling. It would NOT catch `restyleGraphs` itself regressing.
    #[test]
    fn the_theme_is_set_in_one_place_that_restyles() {
        let js = js_without_literals(APP_JS);
        let writes = js.matches("documentElement.dataset.theme").count();
        assert_eq!(
            writes, 2,
            "`documentElement.dataset.theme` is touched {writes} time(s); it must \
             be exactly the set and the delete inside `setDark`, so there is one \
             place that can forget to restyle"
        );
        let body = js
            .split_once("const setDark =")
            .map(|(_, rest)| rest.split_once("};").map_or(rest, |(b, _)| b))
            .expect("`app.js` defines `setDark`");
        assert!(
            body.contains("documentElement.dataset.theme") && body.contains("restyleGraphs()"),
            "`setDark` must both write the mode and re-resolve the live graphs — \
             a graph styled under the old mode keeps it until something restyles \
             it. Body was: {body}"
        );
    }

    /// The master's two dark mappings say the same thing.
    ///
    /// Dark is written twice — once under `prefers-color-scheme`, once under
    /// `[data-theme="dark"]` for the project graph view, which is dark whatever
    /// the OS prefers — because a media query cannot be folded into a selector
    /// list. That is the only duplication in `tokens.css`, and it is the one
    /// place the "one identity" claim could quietly stop being true: the two
    /// blocks drifting would give the app two dark palettes again.
    #[test]
    fn dark_modes_agree() {
        let scopes = css_scopes(crate::theme::TOKENS);
        let media = scopes
            .get("@media (prefers-color-scheme: dark) :root")
            .expect("`tokens.css` answers `prefers-color-scheme: dark`");
        let attr = scopes
            .get("[data-theme=\"dark\"]")
            .expect("`tokens.css` answers an explicit `data-theme=\"dark\"`");
        assert_eq!(
            media, attr,
            "the two dark mappings in `assets/tokens.css` have drifted — a \
             surface that forces dark would render a different palette from one \
             that inherits it from the OS"
        );
        let light = scopes.get(":root").expect("`tokens.css` declares light");
        let missing: Vec<&String> = media.keys().filter(|k| !light.contains_key(*k)).collect();
        assert!(
            missing.is_empty(),
            "dark declares {missing:?}, which light has no value for"
        );
    }

    /// #512: the explorer's surfaces must address ONE set of colour names whose
    /// *values* change per mode, never two parallel sets of names.
    ///
    /// The shape this guards has moved once. #512 fixed two *views* with two
    /// namespaces (`--*` on `:root`, `--p*` under `#view-project`) by giving
    /// them one set of names re-declared per view. The palette is now one master
    /// (`assets/tokens.css`) re-declared per **mode** — light on `:root`, dark
    /// under `prefers-color-scheme` and under `data-theme="dark"`, which is what
    /// the project graph view carries. `#view-project` declares nothing at all
    /// any more. The assertions are unchanged and now cover a case they could
    /// not before: `css_scopes` is nesting-aware, so a token declared only
    /// inside the dark media query no longer reads as a `:root` token.
    ///
    /// The old shape kept a `--p*` set scoped to `#view-project` alongside the
    /// `--*` set on `:root`, so any component reused across the views needed a
    /// hand-written remap — and the one remap that existed was four variables
    /// short. Nothing errored: an undefined custom property is invalid at
    /// computed-value time, so the property silently falls back to inherited or
    /// initial. `.p-toggle` did exactly that on the workspace view, inheriting the
    /// panel's accent indigo instead of the muted grey its own rule asked for.
    ///
    /// Both halves are asserted, because either alone passes while still broken:
    ///   1. every `var(--x)` resolves on `:root`, so no rule can depend on a name
    ///      only one view declares;
    ///   2. every name a view re-declares also exists on `:root`, so a dark-only
    ///      role cannot be added without its light value.
    ///
    /// Deliberately structural: it pins no colour, so a palette re-tune stays free.
    /// To show that a re-tune changed no rendered value, resolve both revisions
    /// with `scripts/resolve-explorer-theme.py` and diff them.
    #[tokio::test]
    async fn shell_colour_vars_are_one_namespace_declared_by_both_views() {
        let (status, _ct, _cache, body) = get("/").await;
        assert_eq!(status, StatusCode::OK);
        let scopes = palette_scopes(&body);
        let root = scopes
            .get(":root")
            .expect("the shell declares its palette on `:root`");

        // (1) No rule may reference a name `:root` does not declare. A `var()`
        // carrying a fallback is fine — there, the fallback IS the declared value.
        let mut dangling: Vec<&str> = body
            .match_indices("var(--")
            .map(|(i, _)| &body[i + "var(".len()..])
            .map(|rest| &rest[..rest.find(')').unwrap_or(rest.len())])
            .filter(|inner| !inner.contains(','))
            .map(str::trim)
            .filter(|name| !root.contains(*name))
            .collect();
        dangling.sort_unstable();
        dangling.dedup();
        assert!(
            dangling.is_empty(),
            "used but not declared on `:root`, so they resolve only inside whichever \
             view declares them and fall back to inherited/initial everywhere else: \
             {dangling:?}"
        );

        // (2) A view that re-declares the palette must not introduce a name the
        // light theme has no value for.
        for (selector, names) in &scopes {
            if selector == ":root" {
                continue;
            }
            let missing: Vec<&String> = names.difference(root).collect();
            assert!(
                missing.is_empty(),
                "`{selector}` declares {missing:?}, which `:root` does not — a \
                 component reused outside `{selector}` would get no value at all"
            );
        }

        // Name the old parallel namespace so a partial revert is caught outright.
        for gone in ["--pbg", "--pfg", "--paccent", "--pmuted"] {
            assert!(
                !body.contains(gone),
                "`{gone}` is back: re-declare the SHARED name under `#view-project` \
                 rather than reintroducing a `--p*` palette"
            );
        }
    }
}

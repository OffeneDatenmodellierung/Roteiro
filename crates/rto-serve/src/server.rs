//! The thin `/v1` OpenAI-compatible HTTP layer (ADR-0006): `GET /v1/models` and
//! `POST /v1/chat/completions` over an [`Engine`]. Loopback-bound by the caller;
//! no auth (a localhost dev tool — TLS/authn terminate at a reverse proxy, as
//! ADR-0002 frames for MCP).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use tokio_stream::StreamExt as _;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::engine::{ChatRequest, CompletionStats, Engine, EngineError, FinishReason};
use crate::responses::{Frame, ResponseWriter, ResponsesRequest};
use crate::tools::{
    ClientToolCall, ToolDef, ToolLoopOutcome, ToolRegistry, chat_with_client_tools,
};
use crate::types::{
    ChatChoice, ChatCompletionChunk, ChatCompletionRequest, ChatCompletionResponse, ChatMessageDto,
    ChunkChoice, Delta, EmbeddingObject, EmbeddingRequest, EmbeddingResponse, ErrorResponse,
    FunctionCallDto, ModelList, ModelObject, ToolCallDelta, ToolCallDto, Usage,
};

/// How many tool round-trips a single request may take (ADR-0006 server-side
/// execute-and-loop). One further generation runs after the budget is spent, so
/// the last tool result informs the answer.
///
/// A model still calling tools in *that* generation has run out of rounds
/// without reaching an answer, and is refused rather than published (#489). The
/// refusal names this constant by name and file — `tools::still_calling_refusal`
/// — because raising it is the way forward for a reader who keeps meeting it, so
/// a rename here must update that message.
///
/// **Why 10 and not 4.** #489's refusals made the old budget visible rather than
/// hiding it in a wrong answer: 2 of 6 Ask questions exhausted it. Measured live
/// against this repo's graph on `qwen3-coder-30b-a3b` (greedy, and the model the
/// Ask panel actually defaults to), those two clear at **6** and **8** rounds.
/// So 4 was below what the questions people ask need, and 10 leaves two rounds
/// of headroom over the hardest one that clears at all.
///
/// **Raising this is free for a request that does not need it — a property of
/// the loop, not a hope.** [`chat_with_client_tools`] returns the moment a
/// generation carries no tool call, so an unused round is never a generation.
/// Measured: a question answering in two rounds produced byte-identical output
/// at 4, 6, 8 and 10 — same prompt *and* completion token counts — and a
/// question answering at 8 was identical again at 10.
///
/// **What it does cost is paid by the requests that spend it, and both halves
/// were measured.** A question that exhausts the budget refuses about 3.5x
/// slower, because it really does run every round before giving up: 22s at 4,
/// 78s at 10. And each round feeds another tool result into the prompt, so the
/// *context* a request allocates grows even though `MAX_TOOL_RESULT` did not
/// change. Worst case — every result at its 4,000-byte cap — that moves a single
/// request from a 9,518-token window costing 784 MiB to an 18,626-token window
/// costing 1,318 MiB on `qwen3.8-27b`. That is the price of this constant, and
/// it is why the number is not larger still.
///
/// **That price is now asserted rather than remembered** (#556). This constant
/// multiplies *both* of the other two — `tools::MAX_TOOL_RESULT` and, through
/// the tool call each round appends to the next prompt,
/// `types::DEFAULT_MAX_TOKENS` — so [`crate::budget`] computes the worst-case
/// window from all three and fails the build if raising this one outgrows it.
pub(crate) const MAX_TOOL_ROUNDS: usize = 10;

/// Shared handler state: the inference engine, optionally the graph tools the
/// served model may call, and — for a multi-workspace serve (ADR-0008) — a
/// per-workspace tool registry each confined to one workspace's projects, keyed
/// by workspace name and addressed via `/v1/workspaces/{ws}/chat/completions`.
struct AppState {
    engine: Arc<dyn Engine>,
    tools: Option<Arc<dyn ToolRegistry>>,
    /// Workspace name → a registry scoped to just that workspace's projects. Empty
    /// unless the caller built one ([`app_with_workspace_tools`]); the unscoped and
    /// `/v1/{project}/…` routes never consult it, so the default paths are untouched.
    workspaces: std::collections::HashMap<String, Arc<dyn ToolRegistry>>,
    /// Operator-set request bounds. [`crate::types::Limits::default`] unless the
    /// caller built the router with [`app_with_workspace_tools_limited`].
    limits: crate::types::Limits,
}
type Shared = Arc<AppState>;

/// Build the `/v1` router over `engine`, with no tools.
pub fn app(engine: Arc<dyn Engine>) -> Router {
    router(Arc::new(AppState {
        engine,
        tools: None,
        workspaces: std::collections::HashMap::new(),
        limits: crate::types::Limits::default(),
    }))
}

/// As [`app`], but with operator-set request bounds rather than the built-in
/// defaults.
///
/// The untooled router still needs these. A server with no graph registry does
/// not stop accepting a **client's** `tools` array — it just returns the tool
/// calls for the client to execute instead of running them itself — so the
/// oversized-`tools` bound applies on this path exactly as on the tooled one.
pub fn app_limited(engine: Arc<dyn Engine>, limits: crate::types::Limits) -> Router {
    router(Arc::new(AppState {
        engine,
        tools: None,
        workspaces: std::collections::HashMap::new(),
        limits,
    }))
}

/// Build the `/v1` router over `engine` with `tools` auto-registered — the model
/// may call them to query the graph while answering (ADR-0006).
pub fn app_with_tools(engine: Arc<dyn Engine>, tools: Arc<dyn ToolRegistry>) -> Router {
    router(Arc::new(AppState {
        engine,
        tools: Some(tools),
        workspaces: std::collections::HashMap::new(),
        limits: crate::types::Limits::default(),
    }))
}

/// Build the `/v1` router over `engine` with a default `tools` registry AND a set
/// of per-**workspace** registries, each confined to one workspace's projects
/// (ADR-0008). The unscoped `/v1/chat/completions` and `/v1/{project}/…` routes
/// behave exactly as with [`app_with_tools`] (they use `tools`); the added
/// `/v1/workspaces/{ws}/chat/completions` route runs its tool loop over the
/// registry for `{ws}` alone, so a workspace-level Ask can never see or answer
/// about a project outside the selected workspace.
// The map is moved straight into `AppState` (which fixes the default hasher), and
// every caller builds it with the std default, so generalising over `BuildHasher`
// would add a type parameter for no benefit.
#[allow(clippy::implicit_hasher)]
pub fn app_with_workspace_tools(
    engine: Arc<dyn Engine>,
    tools: Arc<dyn ToolRegistry>,
    workspaces: std::collections::HashMap<String, Arc<dyn ToolRegistry>>,
) -> Router {
    router(Arc::new(AppState {
        engine,
        tools: Some(tools),
        workspaces,
        limits: crate::types::Limits::default(),
    }))
}

/// As [`app_with_workspace_tools`], but with operator-set request bounds rather
/// than the built-in defaults.
///
/// Separate from [`app_with_workspace_tools`] so that adding a bound cannot
/// silently change what an existing caller enforces: a caller that has not been
/// updated keeps [`crate::types::Limits::default`], which is what it had before
/// the parameter existed.
// Allowed for the reason its unlimited twin above gives: the map is moved
// straight into `AppState`, which fixes the default hasher, and every caller
// builds it with the std default — so generalising over `BuildHasher` would add
// a type parameter for no benefit.
#[allow(clippy::implicit_hasher)]
pub fn app_with_workspace_tools_limited(
    engine: Arc<dyn Engine>,
    tools: Arc<dyn ToolRegistry>,
    workspaces: std::collections::HashMap<String, Arc<dyn ToolRegistry>>,
    limits: crate::types::Limits,
) -> Router {
    router(Arc::new(AppState {
        engine,
        tools: Some(tools),
        workspaces,
        limits,
    }))
}

/// Assemble the router over a fully-built [`AppState`].
///
/// Alongside the plain `/v1/*` routes, a `/v1/{project}/*` prefix pre-binds the
/// graph tools to one hosted project of a multi-repo workspace (ADR-0008): a
/// client points its `base_url` at `…/v1/<project>` and every tool call the
/// served model makes is scoped to that project without the model naming it.
/// `models`/`embeddings` ignore the prefix (they are engine-level).
fn router(state: Shared) -> Router {
    Router::new()
        .route("/v1/models", get(list_models))
        .route("/v1/projects", get(list_projects))
        .route("/v1/chat/completions", post(chat_completions))
        // OpenAI's Responses wire (#809), unscoped and streaming-only. An
        // adapter over the same tool loop, not a second one: see
        // `crate::responses`. The scoped variants are deliberately absent.
        .route("/v1/responses", post(responses))
        .route("/v1/embeddings", post(embeddings))
        .route("/v1/{project}/models", get(list_models_scoped))
        .route(
            "/v1/{project}/chat/completions",
            post(chat_completions_scoped),
        )
        .route("/v1/{project}/embeddings", post(embeddings_scoped))
        // Workspace-scoped chat (ADR-0008): the served model's tool calls are
        // confined to `{ws}`'s projects, so a workspace-level Ask cannot reach a
        // project in another workspace. A 5-segment path, distinct from the
        // 4-segment `/v1/{project}/chat/completions`, so the two never collide.
        .route(
            "/v1/workspaces/{ws}/chat/completions",
            post(chat_completions_workspace_scoped),
        )
        .with_state(state)
}

/// A [`ToolRegistry`] advertising nothing — the stand-in when a server was built
/// with no graph tools but the request still needs the tool loop because the
/// *client* supplied tools of its own.
struct NoTools;

impl ToolRegistry for NoTools {
    fn tools(&self) -> Vec<ToolDef> {
        Vec::new()
    }

    fn call(&self, name: &str, _arguments: &serde_json::Value) -> Result<String, String> {
        Err(format!("no tool `{name}` is registered"))
    }
}

/// A [`ToolRegistry`] that pre-binds a `project` for `/v1/{project}/…` requests:
/// it forwards to the inner registry but fills in `project` on each tool call
/// when the model did not name one (an explicit `project` in the call still
/// wins, allowing a cross-project query).
///
/// This inserts a key the model did not send, which is why the unknown-argument
/// check (`tools::unknown_argument`) runs **before** the registry rather than
/// inside it: what a route pre-binds is a server-side value, not an argument
/// submitted to be judged, and a tool that declares no `project` — the
/// machine-global `sandbox_*` pair — would otherwise be refused on this route
/// for something nobody typed.
struct ScopedTools<'a> {
    inner: &'a dyn ToolRegistry,
    project: String,
}

impl ToolRegistry for ScopedTools<'_> {
    fn tools(&self) -> Vec<crate::tools::ToolDef> {
        self.inner.tools()
    }

    fn projects(&self) -> Vec<String> {
        self.inner.projects()
    }

    fn call(&self, name: &str, arguments: &serde_json::Value) -> Result<String, String> {
        let mut arguments = arguments.clone();
        if let Some(obj) = arguments.as_object_mut() {
            obj.entry("project")
                .or_insert_with(|| serde_json::Value::String(self.project.clone()));
        }
        self.inner.call(name, &arguments)
    }
}

/// Serve the `/v1` app on `addr`, blocking the calling thread until shutdown.
///
/// # Errors
/// Returns an error if the tokio runtime cannot start, the address cannot be
/// bound, or the server exits abnormally.
pub fn serve_blocking(engine: Arc<dyn Engine>, addr: std::net::SocketAddr) -> anyhow::Result<()> {
    serve_router(app(engine), addr)
}

/// Like [`serve_blocking`], with graph tools auto-registered (ADR-0006).
///
/// # Errors
/// As [`serve_blocking`].
pub fn serve_blocking_with_tools(
    engine: Arc<dyn Engine>,
    tools: Arc<dyn ToolRegistry>,
    addr: std::net::SocketAddr,
) -> anyhow::Result<()> {
    serve_router(app_with_tools(engine, tools), addr)
}

/// Serve a caller-composed `router` on `addr`, blocking until shutdown. Lets the
/// caller mount extra paths — e.g. merge an MCP service at `/mcp` alongside
/// `/v1` on one port (ADR-0008) — building the `/v1` half with [`app`] /
/// [`app_with_tools`].
///
/// # Errors
/// As [`serve_blocking`].
pub fn serve_blocking_router(router: Router, addr: std::net::SocketAddr) -> anyhow::Result<()> {
    serve_router(router, addr)
}

/// Run `router` on `addr`, blocking until shutdown.
fn serve_router(router: Router, addr: std::net::SocketAddr) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, router).await?;
        Ok(())
    })
}

/// Serve the `/v1` app over **TLS** on `addr`, blocking until shutdown — an
/// in-process alternative to terminating TLS at a reverse proxy (ADR-0002
/// addendum). `cert` and `key` are paths to PEM files: a certificate chain and
/// its private key (PKCS#8 or RSA). `tools`, when set, auto-registers the graph
/// tools (as [`serve_blocking_with_tools`]).
///
/// # Errors
/// Returns an error if the tokio runtime cannot start, the cert/key cannot be
/// read or parsed, the address cannot be bound, or the server exits abnormally.
#[cfg(feature = "tls")]
pub fn serve_blocking_tls(
    engine: Arc<dyn Engine>,
    tools: Option<Arc<dyn ToolRegistry>>,
    addr: std::net::SocketAddr,
    cert: &std::path::Path,
    key: &std::path::Path,
) -> anyhow::Result<()> {
    let router = match tools {
        Some(tools) => app_with_tools(engine, tools),
        None => app(engine),
    };
    serve_router_tls(router, addr, cert, key)
}

/// Serve a caller-composed `router` on `addr` over TLS, blocking until shutdown
/// — the TLS counterpart to [`serve_blocking_router`].
///
/// # Errors
/// As [`serve_blocking_tls`].
#[cfg(feature = "tls")]
pub fn serve_blocking_router_tls(
    router: Router,
    addr: std::net::SocketAddr,
    cert: &std::path::Path,
    key: &std::path::Path,
) -> anyhow::Result<()> {
    serve_router_tls(router, addr, cert, key)
}

/// Run `router` on `addr` over TLS, blocking until shutdown.
#[cfg(feature = "tls")]
fn serve_router_tls(
    router: Router,
    addr: std::net::SocketAddr,
    cert: &std::path::Path,
    key: &std::path::Path,
) -> anyhow::Result<()> {
    // The `tls-rustls-no-provider` feature ships no crypto provider, so install
    // ring — but only if none is set yet (another component, e.g. `ureq`, may have
    // installed one already). `install_default`'s sole failure mode is "a provider
    // is already installed", so guarding on `get_default` means the only ignored
    // outcome is a benign race where another thread installed one between the check
    // and the call; ring is the only provider this build ever installs.
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let config = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key)
            .await
            .map_err(|e| anyhow::anyhow!("loading TLS certificate/key: {e}"))?;
        axum_server::bind_rustls(addr, config)
            .serve(router.into_make_service())
            .await?;
        Ok(())
    })
}

/// `GET /v1/models` — the installed models this server serves.
async fn list_models(State(state): State<Shared>) -> Json<ModelList> {
    let data = state
        .engine
        .models()
        .into_iter()
        .map(|m| ModelObject {
            id: m.id,
            object: "model",
            owned_by: "roteiro",
        })
        .collect();
    Json(ModelList {
        object: "list",
        data,
    })
}

/// `GET /v1/projects` — the workspace projects this server hosts (ADR-0008), so a
/// client (e.g. an agent router) can discover them without a model round-trip.
/// Empty for a single, unnamed source. Not an OpenAI-standard endpoint; shaped
/// like `/v1/models` (`{ object: "list", data: [{ id, object: "project" }] }`).
async fn list_projects(State(state): State<Shared>) -> Json<serde_json::Value> {
    let data: Vec<serde_json::Value> = state
        .tools
        .as_ref()
        .map(|t| t.projects())
        .unwrap_or_default()
        .into_iter()
        .map(|id| serde_json::json!({ "id": id, "object": "project" }))
        .collect();
    Json(serde_json::json!({ "object": "list", "data": data }))
}

/// `GET /v1/{project}/models` — the engine-level model list; the prefix is
/// accepted (and ignored) so a client can use `…/v1/<project>` as its base URL
/// (ADR-0008).
async fn list_models_scoped(
    State(state): State<Shared>,
    axum::extract::Path(_project): axum::extract::Path<String>,
) -> Json<ModelList> {
    list_models(State(state)).await
}

/// Which tool registry a chat request runs against, and how its tool calls are
/// scoped. Keeps the default paths (`Default`/`Project`) using `state.tools`
/// exactly as before; `Workspace` swaps in a registry already confined to one
/// workspace's projects (ADR-0008).
enum ChatScope {
    /// Unscoped `/v1/chat/completions`: the server's default registry, no pin.
    Default,
    /// `/v1/{project}/…`: the default registry, tool calls pre-bound to `project`.
    Project(String),
    /// `/v1/workspaces/{ws}/…`: a registry confined to one workspace's projects.
    Workspace(Arc<dyn ToolRegistry>),
}

/// `POST /v1/chat/completions` — a full JSON completion, or an SSE stream of
/// `chat.completion.chunk` events when `stream: true`.
async fn chat_completions(
    State(state): State<Shared>,
    Json(body): Json<ChatCompletionRequest>,
) -> Response {
    run_chat(state, body, ChatScope::Default).await
}

/// `POST /v1/{project}/chat/completions` — as [`chat_completions`], but the
/// served model's tool calls are pre-bound to `project` (ADR-0008).
async fn chat_completions_scoped(
    State(state): State<Shared>,
    axum::extract::Path(project): axum::extract::Path<String>,
    Json(body): Json<ChatCompletionRequest>,
) -> Response {
    run_chat(state, body, ChatScope::Project(project)).await
}

/// `POST /v1/workspaces/{ws}/chat/completions` — as [`chat_completions`], but the
/// served model's tools are confined to workspace `{ws}`'s projects (ADR-0008),
/// so a workspace-level Ask never sees a project in another workspace. An unknown
/// workspace is a 404 (the addressed scope does not exist).
async fn chat_completions_workspace_scoped(
    State(state): State<Shared>,
    axum::extract::Path(ws): axum::extract::Path<String>,
    Json(body): Json<ChatCompletionRequest>,
) -> Response {
    let Some(tools) = state.workspaces.get(&ws).cloned() else {
        return error(
            StatusCode::NOT_FOUND,
            format!("unknown workspace `{ws}`"),
            "invalid_request_error",
        );
    };
    run_chat(state, body, ChatScope::Workspace(tools)).await
}

/// Shared chat entry point: validate, then dispatch to the streaming or JSON
/// path, carrying the tool scope for the tool loop.
async fn run_chat(state: Shared, body: ChatCompletionRequest, scope: ChatScope) -> Response {
    let stream = body.stream == Some(true);
    let normalised = match body.normalise(state.limits) {
        Ok(n) => n,
        Err(msg) => return error(StatusCode::BAD_REQUEST, msg, "invalid_request_error"),
    };
    // Destructured field-by-field on purpose: `tool_choice` and
    // `parallel_tool_calls` are parsed and carried but deliberately NOT enforced
    // (see their field docs and `crate::openai_params`), and naming every field
    // here means a future one cannot be added and silently dropped. #488 closed
    // the same hole one layer up, where a parameter with no field at all now has
    // to be declared before it can be served.
    let crate::types::NormalisedChat {
        request: req,
        client_tools,
        tool_choice: _,
        parallel_tool_calls: _,
    } = normalised;
    // Validate the model up front so the streaming and non-streaming paths agree:
    // an unknown model is a 404 either way, not a 200 SSE that fails mid-stream.
    if let Some(refusal) = unknown_model(&state, &req.model) {
        return refusal;
    }
    if stream {
        stream_chat(state, req, scope, client_tools)
    } else {
        chat_json(state, req, scope, client_tools).await
    }
}

/// The `404` an unrecognised model gets — `None` when the model is served.
///
/// Shared by every request-taking surface so that a model this server does not
/// have is refused the same way on each: the alternative is a new wire arriving
/// with its own spelling of the same refusal, which is how `/v1` came to have
/// two content paths in the first place.
fn unknown_model(state: &AppState, model: &str) -> Option<Response> {
    if state.engine.models().iter().any(|m| m.id == model) {
        return None;
    }
    Some(error(
        StatusCode::NOT_FOUND,
        EngineError::UnknownModel(model.to_owned()).to_string(),
        "invalid_request_error",
    ))
}

/// Non-streaming path: run one blocking completion on a worker thread and return
/// a single JSON body. With tools registered, the model may call them first.
async fn chat_json(
    state: Shared,
    req: ChatRequest,
    scope: ChatScope,
    client_tools: Vec<ToolDef>,
) -> Response {
    let model = req.model.clone();
    // Inference blocks (llama.cpp decode loop); keep it off the async runtime.
    let result =
        tokio::task::spawn_blocking(move || complete(&state, &req, &scope, &client_tools)).await;

    match result {
        Ok(Ok(outcome)) => Json(build_response(&model, &outcome)).into_response(),
        Ok(Err(EngineError::UnknownModel(m))) => error(
            StatusCode::NOT_FOUND,
            EngineError::UnknownModel(m).to_string(),
            "invalid_request_error",
        ),
        Ok(Err(e @ EngineError::InvalidRequest(_))) => error(
            StatusCode::BAD_REQUEST,
            e.to_string(),
            "invalid_request_error",
        ),
        Ok(Err(e @ EngineError::Unsupported(_))) => error(
            StatusCode::NOT_IMPLEMENTED,
            e.to_string(),
            "not_implemented",
        ),
        Ok(Err(e @ EngineError::Inference(_))) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            e.to_string(),
            "inference_error",
        ),
        // The worker thread panicked (or was cancelled): report, don't hang.
        Err(e) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("inference task failed: {e}"),
            "inference_error",
        ),
    }
}

/// Run a completion honouring registered tools: the agentic tool loop
/// (ADR-0006) when tools are present, otherwise a plain generation. The `scope`
/// picks the registry and how its calls are bound (ADR-0008): the default
/// registry (optionally project-pinned) or a per-workspace registry. Blocking.
fn complete(
    state: &AppState,
    req: &ChatRequest,
    scope: &ChatScope,
    client_tools: &[ToolDef],
) -> Result<ToolLoopOutcome, EngineError> {
    let engine = state.engine.as_ref();
    // Suppression is decided inside `chat_with_client_tools`, which also falls
    // back to a plain `Engine::chat` when neither tool set has anything in it —
    // so every scope resolves to one call and the empty cases are not special.
    match scope {
        // A workspace-scoped request runs over its own (already-confined)
        // registry; the default and project-scoped requests share `state.tools`.
        ChatScope::Workspace(tools) => {
            chat_with_client_tools(engine, tools.as_ref(), client_tools, req, MAX_TOOL_ROUNDS)
        }
        ChatScope::Project(project) => match &state.tools {
            Some(tools) => {
                let scoped = ScopedTools {
                    inner: tools.as_ref(),
                    project: project.clone(),
                };
                chat_with_client_tools(engine, &scoped, client_tools, req, MAX_TOOL_ROUNDS)
            }
            None => chat_with_client_tools(engine, &NoTools, client_tools, req, MAX_TOOL_ROUNDS),
        },
        ChatScope::Default => match &state.tools {
            Some(tools) => {
                chat_with_client_tools(engine, tools.as_ref(), client_tools, req, MAX_TOOL_ROUNDS)
            }
            None => chat_with_client_tools(engine, &NoTools, client_tools, req, MAX_TOOL_ROUNDS),
        },
    }
}

/// Whether the registry a `scope` resolves to advertises any tools — the
/// streaming path uses this to choose token-incremental generation (no tools)
/// versus the resolve-then-emit tool loop.
fn scope_has_tools(state: &AppState, scope: &ChatScope) -> bool {
    match scope {
        ChatScope::Workspace(tools) => !tools.tools().is_empty(),
        _ => state.tools.as_ref().is_some_and(|t| !t.tools().is_empty()),
    }
}

/// `POST /v1/{project}/embeddings` — embeddings are engine-level; the prefix is
/// accepted (and ignored) so `…/v1/<project>` works as a base URL (ADR-0008).
async fn embeddings_scoped(
    State(state): State<Shared>,
    axum::extract::Path(_project): axum::extract::Path<String>,
    Json(body): Json<EmbeddingRequest>,
) -> Response {
    embeddings(State(state), Json(body)).await
}

/// `POST /v1/embeddings` — one embedding vector per input string.
async fn embeddings(State(state): State<Shared>, Json(body): Json<EmbeddingRequest>) -> Response {
    let model = body.model;
    let inputs = body.input.into_vec();
    if inputs.is_empty() {
        return error(
            StatusCode::BAD_REQUEST,
            "`input` must not be empty",
            "invalid_request_error",
        );
    }
    if !state.engine.models().iter().any(|m| m.id == model) {
        return error(
            StatusCode::NOT_FOUND,
            EngineError::UnknownModel(model).to_string(),
            "invalid_request_error",
        );
    }

    let engine = state.engine.clone();
    let (model_for_task, inputs_for_task) = (model.clone(), inputs);
    let result =
        tokio::task::spawn_blocking(move || engine.embed(&model_for_task, &inputs_for_task)).await;

    match result {
        Ok(Ok(vectors)) => Json(build_embedding_response(&model, vectors)).into_response(),
        Ok(Err(EngineError::UnknownModel(m))) => error(
            StatusCode::NOT_FOUND,
            EngineError::UnknownModel(m).to_string(),
            "invalid_request_error",
        ),
        Ok(Err(e @ EngineError::InvalidRequest(_))) => error(
            StatusCode::BAD_REQUEST,
            e.to_string(),
            "invalid_request_error",
        ),
        Ok(Err(e @ EngineError::Unsupported(_))) => error(
            StatusCode::NOT_IMPLEMENTED,
            e.to_string(),
            "not_implemented",
        ),
        Ok(Err(e @ EngineError::Inference(_))) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            e.to_string(),
            "inference_error",
        ),
        Err(e) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("embedding task failed: {e}"),
            "inference_error",
        ),
    }
}

/// Assemble the OpenAI embeddings response from the per-input vectors.
fn build_embedding_response(model: &str, vectors: Vec<Vec<f32>>) -> EmbeddingResponse {
    let data = vectors
        .into_iter()
        .enumerate()
        .map(|(index, embedding)| EmbeddingObject {
            object: "embedding",
            embedding,
            index,
        })
        .collect();
    EmbeddingResponse {
        object: "list",
        data,
        model: model.to_owned(),
        // Token accounting is not tracked for embeddings on this local endpoint.
        usage: Usage {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
        },
    }
}

/// A message from the blocking generation worker to the SSE stream.
enum StreamMsg {
    /// The first chunk: announce the assistant role.
    Role,
    /// A piece of generated text.
    Delta(String),
    /// The model called the **client's** tools: one chunk carrying every call,
    /// which Roteiro returns without executing. **Divergence:** OpenAI fragments
    /// `arguments` across several chunks; each call here arrives with its
    /// `arguments` complete. Each still carries its positional `index`, so a
    /// client accumulating by index is unaffected (see [`ToolCallDelta`]).
    ToolCalls(Vec<ToolCallDto>),
    /// Generation finished cleanly with this wire reason (`stop` | `length` |
    /// `tool_calls`).
    Done(&'static str),
    /// Generation failed part-way; carry the message **and its kind** for a
    /// final error event. The kind because the status code is already spent by
    /// the time a stream can fail — see [`error_kind`].
    Failed {
        /// What went wrong, as the caller should read it.
        message: String,
        /// The OpenAI error `type`, from [`error_kind`].
        kind: &'static str,
    },
}

/// Stream an **untooled** generation, applying by hand the content rules that a
/// tooled run inherits from `tools::finish`, and hand each publishable piece to
/// `on_text`. Blocking.
///
/// ## Why this is a function and not a block inside one handler
///
/// This is the branch of `/v1` that never reaches `tools::finish`: nothing was
/// advertised, so the tool loop is not used and `engine.chat_stream` is called
/// directly. Its own history is the argument for the shape. The `<think>` rule
/// (#582) and the never-left-the-block refusal (#583) both had to be *ported*
/// here after being written for the loop, because the loop was not on this path
/// — and this file's comments said as much: "the one branch of `/v1` that never
/// reaches `tools::finish`".
///
/// Adding a second protocol (`/v1/responses`, #809) would have made that two
/// branches and the next rule would have to be ported twice. So the branch is
/// named once and both wires stream through it. What each wire supplies is the
/// only thing that differs — a `chat.completion.chunk` delta or a
/// `response.output_text.delta` — and neither may decide what the text *is*.
///
/// `StreamFilter` withholds a reasoning block rather than forwarding it, so a
/// reasoning model costs this path its time-to-first-token and no correctness.
/// When it held every token there is no answer to publish, so the refusal is the
/// whole of the assistant's turn rather than a correction appended to half a
/// reply — the same sentence the non-streaming path produces, from the same
/// function, because a caller must not have to know which transport it used to
/// know what happened (#583).
///
/// # Errors
/// Whatever the engine returns; nothing has been published when it does.
fn stream_untooled(
    state: &AppState,
    req: &ChatRequest,
    on_text: &mut dyn FnMut(String),
) -> Result<CompletionStats, EngineError> {
    let mut filter = crate::thinking::StreamFilter::new();
    let mut on_token = |piece: &str| {
        if let Some(text) = filter.push(piece) {
            on_text(text);
        }
    };
    let usage = state.engine.chat_stream(req, &mut on_token)?;
    match filter.end(usage.finish_reason) {
        Ok(Some(text)) => on_text(text),
        Ok(None) => {}
        Err(why) => on_text(crate::tools::unanswered_refusal(why, req.max_tokens)),
    }
    Ok(usage)
}

/// Streaming path: run generation on a blocking worker that feeds token deltas
/// over a channel, and surface them as OpenAI `chat.completion.chunk` SSE events
/// terminated by `data: [DONE]`.
fn stream_chat(
    state: Shared,
    req: ChatRequest,
    scope: ChatScope,
    client_tools: Vec<ToolDef>,
) -> Response {
    let id = format!("chatcmpl-{}", next_id());
    let created = unix_seconds();
    let model = req.model.clone();

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<StreamMsg>();
    tokio::task::spawn_blocking(move || {
        // A dropped receiver (client disconnected) just makes sends fail — the
        // generation loop then runs to completion harmlessly.
        let _ = tx.send(StreamMsg::Role);
        // Only take the (non-incremental) tool path when the scope's registry
        // actually advertises tools — an empty registry falls back to
        // `engine.chat` in `complete`, so match that here and keep token-by-token
        // streaming.
        // Client tools force the tool loop even on a server with no graph tools:
        // the model has to be told about them, and a call to one must terminate
        // the stream with `tool_calls` rather than stream as prose.
        let use_tools = !client_tools.is_empty() || scope_has_tools(&state, &scope);
        if use_tools {
            // The tool loop runs multiple generations, so it is resolved fully
            // and then the final answer is emitted as one delta (tool-mode
            // streaming is not token-incremental).
            //
            // The same two branches as `build_response`, and markup-free for the
            // same reason: content is emitted only when there are no client
            // calls, and an outcome with no client calls came through
            // `tools::finish`.
            //
            // Here that is *unqualified*, where in `build_response` it is not.
            // `use_tools` is true exactly when something was advertised — the
            // `ScopedTools` wrapper delegates `tools()`, so `scope_has_tools`
            // and the loop's own advertised list agree — so `Ending::Untooled`,
            // the one ending that passes markup through, cannot arise inside
            // this block at all.
            //
            // Resolving the loop first is also what makes any of it possible
            // here — a token-incremental stream has already sent the first half
            // of a call before anything can judge it. Which is why the
            // else-branch below, where nothing is advertised and the loop is not
            // used, streams raw: that is the untooled case, reached by a
            // different route and with the same meaning. Nothing there injected
            // the tool prompt, so nothing there assigned `<tool_call>` a meaning.
            match complete(&state, &req, &scope, &client_tools) {
                Ok(outcome) if !outcome.client_tool_calls.is_empty() => {
                    let _ = tx.send(StreamMsg::ToolCalls(tool_call_dtos(
                        &outcome.client_tool_calls,
                    )));
                    let _ = tx.send(StreamMsg::Done("tool_calls"));
                }
                Ok(outcome) => {
                    let reason = outcome.completion.finish_reason.as_str();
                    let _ = tx.send(StreamMsg::Delta(outcome.completion.content));
                    let _ = tx.send(StreamMsg::Done(reason));
                }
                Err(e) => {
                    let _ = tx.send(StreamMsg::Failed {
                        message: e.to_string(),
                        kind: error_kind(&e),
                    });
                }
            }
        } else {
            // The untooled, token-incremental path. Every rule it has to apply
            // lives in `stream_untooled`, which `/v1/responses` streams through
            // too — see that function for why it is one function and not two
            // copies.
            let mut emit = |text: String| {
                let _ = tx.send(StreamMsg::Delta(text));
            };
            match stream_untooled(&state, &req, &mut emit) {
                Ok(usage) => {
                    let _ = tx.send(StreamMsg::Done(usage.finish_reason.as_str()));
                }
                Err(e) => {
                    let _ = tx.send(StreamMsg::Failed {
                        message: e.to_string(),
                        kind: error_kind(&e),
                    });
                }
            }
        }
    });

    let events = UnboundedReceiverStream::new(rx).map(move |msg| {
        let data = match msg {
            StreamMsg::Role => chunk_json(&id, created, &model, role_delta(), None),
            StreamMsg::Delta(text) => chunk_json(&id, created, &model, content_delta(text), None),
            StreamMsg::ToolCalls(calls) => {
                chunk_json(&id, created, &model, tool_calls_delta(calls), None)
            }
            StreamMsg::Done(reason) => {
                chunk_json(&id, created, &model, Delta::default(), Some(reason))
            }
            StreamMsg::Failed { message, kind } => {
                serde_json::to_string(&ErrorResponse::new(message, kind))
                    .unwrap_or_else(|_| "{\"error\":{\"message\":\"stream failed\"}}".to_owned())
            }
        };
        Ok::<Event, std::convert::Infallible>(Event::default().data(data))
    });
    // OpenAI terminates the stream with a literal `data: [DONE]`.
    let done = tokio_stream::once(Ok(Event::default().data("[DONE]")));
    Sse::new(events.chain(done)).into_response()
}

/// `POST /v1/responses` — OpenAI's Responses wire, streaming-only and unscoped
/// (#809).
///
/// An adapter: [`ResponsesRequest::normalise`] translates the body into the
/// same [`ChatRequest`] the chat wire produces, and the answer comes back out
/// of the same tool loop. See [`crate::responses`] for the mapping and for what
/// is refused.
async fn responses(State(state): State<Shared>, Json(body): Json<ResponsesRequest>) -> Response {
    // Read before `body` is consumed. **Whether the caller sent tools decides
    // the mode, not whether any of them survived translation** — the published
    // rule is that a `tools` array puts this endpoint in general mode and the
    // graph tools are not injected. A request carrying only hosted tools
    // (`codex-cli` sends `web_search` on every turn) filters down to an empty
    // client list, and keying the decision on that list would have put such a
    // request back into Ask mode: graph tools advertised and executed for a
    // client that asked for the opposite, plus the ~3,100 tokens of schemas
    // that behaviour exists to spare it. Raised in review of #825.
    let declared_tools = body.tools.as_ref().is_some_and(|t| !t.is_empty());
    let normalised = match body.normalise(state.limits) {
        Ok(n) => n,
        Err(msg) => return error(StatusCode::BAD_REQUEST, msg, "invalid_request_error"),
    };
    // Destructured field-by-field for the reason `run_chat` gives: `tool_choice`
    // and `parallel_tool_calls` are carried and deliberately not enforced, and
    // naming every field means a future one cannot be added and silently
    // dropped on this wire while the chat wire grows it.
    let crate::types::NormalisedChat {
        request: req,
        client_tools,
        tool_choice: _,
        parallel_tool_calls: _,
    } = normalised;
    if let Some(refusal) = unknown_model(&state, &req.model) {
        return refusal;
    }
    // `ChatScope::Default` is the whole of the scoping story here: the
    // project- and workspace-scoped Responses routes are deferred, and the
    // seam they will reuse is this argument (ADR-0008), not a second
    // confinement mechanism.
    stream_responses(state, req, ChatScope::Default, client_tools, declared_tools)
}

/// Run one Responses turn and surface it as the typed SSE event sequence.
///
/// The same two branches as [`stream_chat`], for the same reasons — a tooled
/// run resolves the loop and then publishes, an untooled one streams through
/// [`stream_untooled`] token by token — differing only in which events carry
/// the result.
fn stream_responses(
    state: Shared,
    req: ChatRequest,
    scope: ChatScope,
    client_tools: Vec<ToolDef>,
    declared_tools: bool,
) -> Response {
    let id = format!("resp_{}", next_id());
    let created = unix_seconds();
    let model = req.model.clone();

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Frame>();
    tokio::task::spawn_blocking(move || {
        let mut writer = ResponseWriter::new(id, model, created);
        // Not required by the one client this was measured against, and emitted
        // anyway: it is what OpenAI's own wire opens with, and a client that
        // reads it learns the response id before the first item. See
        // `ResponseWriter` for the measurement.
        let _ = tx.send(writer.created());
        // The graph tools are consulted only when the caller declared none of
        // its own — see `responses` for why this asks `declared_tools` rather
        // than looking at what survived. A hosted-only request therefore takes
        // the untooled branch: nothing is advertised, which is what general
        // mode means when none of the declared tools can be served, and the
        // answer streams token by token as a bonus.
        let use_tools =
            !client_tools.is_empty() || (!declared_tools && scope_has_tools(&state, &scope));
        if use_tools {
            match complete(&state, &req, &scope, &client_tools) {
                Ok(outcome) if !outcome.client_tool_calls.is_empty() => {
                    // `FinishReason::Stop` regardless of what the engine
                    // reported, and deliberately. The loop returns calls only
                    // through `Ending::ClientCalls`, which the markup reader
                    // reaches only for a call that arrived **intact** — one cut
                    // by the token cap becomes `Ending::Unfinished` and is
                    // refused, never returned. So a turn that gets here carries
                    // a complete, executable call, and `response.incomplete`
                    // would tell the client its call was truncated when it was
                    // not, and stop it running one it could have run. This is
                    // the same override the chat wire makes when it reports
                    // `finish_reason: "tool_calls"` over `length`.
                    for call in tool_call_dtos(&outcome.client_tool_calls) {
                        for frame in writer.function_call(&call) {
                            let _ = tx.send(frame);
                        }
                    }
                    let _ = tx.send(writer.finish(&usage_of(&outcome), FinishReason::Stop));
                }
                Ok(outcome) => {
                    let usage = usage_of(&outcome);
                    let finish = outcome.completion.finish_reason;
                    let text = outcome.completion.content;
                    for frame in writer.message_start() {
                        let _ = tx.send(frame);
                    }
                    // One delta for the whole answer — the declared divergence
                    // this path inherits from the tool loop, which has to run to
                    // completion before any of its output may be published.
                    let _ = tx.send(writer.text_delta(&text));
                    for frame in writer.message_done(&text, finish) {
                        let _ = tx.send(frame);
                    }
                    let _ = tx.send(writer.finish(&usage, finish));
                }
                Err(e) => {
                    let _ = tx.send(writer.failed(&e.to_string(), error_kind(&e)));
                }
            }
        } else {
            let mut text = String::new();
            let mut started = false;
            // The item is opened on the first publishable piece rather than up
            // front, so a generation that fails before saying anything reports
            // `response.failed` without a half-open item ahead of it.
            // Scoped so the closure's borrow of `writer` and `text` ends
            // before the arms below need them back.
            let result = {
                let mut emit = |piece: String| {
                    if !started {
                        started = true;
                        for frame in writer.message_start() {
                            let _ = tx.send(frame);
                        }
                    }
                    text.push_str(&piece);
                    let _ = tx.send(writer.text_delta(&piece));
                };
                stream_untooled(&state, &req, &mut emit)
            };
            match result {
                Ok(usage) => {
                    if !started {
                        for frame in writer.message_start() {
                            let _ = tx.send(frame);
                        }
                    }
                    for frame in writer.message_done(&text, usage.finish_reason) {
                        let _ = tx.send(frame);
                    }
                    let _ = tx.send(writer.finish(
                        &Usage {
                            prompt_tokens: usage.prompt_tokens,
                            completion_tokens: usage.completion_tokens,
                            total_tokens: usage.prompt_tokens + usage.completion_tokens,
                        },
                        usage.finish_reason,
                    ));
                }
                Err(e) => {
                    let _ = tx.send(writer.failed(&e.to_string(), error_kind(&e)));
                }
            }
        }
    });

    let events = UnboundedReceiverStream::new(rx).map(|(name, data)| {
        Ok::<Event, std::convert::Infallible>(Event::default().event(name).data(data))
    });
    // The terminal event — `response.completed`, or `response.incomplete` for a
    // turn the token budget cut short — is what a Responses client waits for;
    // `[DONE]` follows it for symmetry with the chat stream and because the
    // client this was measured against accepts it.
    let done = tokio_stream::once(Ok(Event::default().data("[DONE]")));
    Sse::new(events.chain(done)).into_response()
}

/// The token accounting one tool-loop outcome reports.
fn usage_of(outcome: &ToolLoopOutcome) -> Usage {
    let c = &outcome.completion;
    Usage {
        prompt_tokens: c.prompt_tokens,
        completion_tokens: c.completion_tokens,
        total_tokens: c.prompt_tokens + c.completion_tokens,
    }
}

/// The first-chunk delta announcing the assistant role.
fn role_delta() -> Delta {
    Delta {
        role: Some("assistant"),
        content: None,
        tool_calls: None,
    }
}

/// A content-piece delta.
fn content_delta(text: String) -> Delta {
    Delta {
        role: None,
        content: Some(text),
        tool_calls: None,
    }
}

/// The single, complete `tool_calls` delta — see [`StreamMsg::ToolCalls`] for the
/// declared divergence from OpenAI's fragmented streaming shape.
fn tool_calls_delta(calls: Vec<ToolCallDto>) -> Delta {
    Delta {
        role: None,
        content: None,
        tool_calls: Some(
            calls
                .into_iter()
                .enumerate()
                .map(|(index, c)| ToolCallDelta {
                    index: u32::try_from(index).unwrap_or(u32::MAX),
                    id: c.id,
                    kind: "function",
                    function: c.function,
                })
                .collect(),
        ),
    }
}

/// Render the loop's client tool calls into their wire form. `arguments` is a
/// JSON **string** on the OpenAI wire, not an object.
fn tool_call_dtos(calls: &[ClientToolCall]) -> Vec<ToolCallDto> {
    calls
        .iter()
        .map(|c| ToolCallDto {
            id: c.id.clone(),
            kind: "function".to_owned(),
            function: FunctionCallDto {
                name: c.name.clone(),
                arguments: serde_json::to_string(&c.arguments).unwrap_or_else(|_| "{}".to_owned()),
            },
        })
        .collect()
}

/// Serialise one streamed chunk to its JSON `data:` payload.
fn chunk_json(
    id: &str,
    created: u64,
    model: &str,
    delta: Delta,
    finish_reason: Option<&'static str>,
) -> String {
    let chunk = ChatCompletionChunk {
        id: id.to_owned(),
        object: "chat.completion.chunk",
        created,
        model: model.to_owned(),
        choices: vec![ChunkChoice {
            index: 0,
            delta,
            finish_reason,
        }],
    };
    serde_json::to_string(&chunk).unwrap_or_default()
}

/// Assemble the OpenAI response body from a tool-loop [`ToolLoopOutcome`].
///
/// Neither branch publishes tool-call markup that Roteiro asked a model to write
/// (#489), and between them they cover the outcome:
///
/// * With client calls, this reports `finish_reason: "tool_calls"` and
///   `content: null`, so the raw markup in `completion.content` is not rendered.
/// * Without them, `content` is whatever `tools::finish` passed — and that is
///   the one place a generation is declared to be the user's answer, so it
///   carries no markup to publish, **unless the request advertised no tools at
///   all**. Such a request was never sent a tool system prompt, so its
///   `<tool_call>` is ordinary model text and passing it through is the intended
///   behaviour (`tools::Ending::Untooled`).
///
/// That exception really does reach here, unlike in [`stream_chat`]: this is the
/// non-streaming path and it runs for every request, with no `use_tools` guard
/// above it to exclude the untooled case. It is not a hole — an untooled request
/// has no tool to call, so there is no call to mistake for an answer.
///
/// The requirement on *this* function is therefore only the OpenAI one it
/// already meets: do not render `content` beside `tool_calls`. It is not
/// separately responsible for inspecting the text, and it must not become so —
/// a check at each render site is what let the defect survive in three places.
fn build_response(model: &str, outcome: &ToolLoopOutcome) -> ChatCompletionResponse {
    let completion = &outcome.completion;
    let (content, tool_calls, finish_reason) = if outcome.client_tool_calls.is_empty() {
        (
            Some(completion.content.clone()),
            None,
            completion.finish_reason.as_str(),
        )
    } else {
        (
            None,
            Some(tool_call_dtos(&outcome.client_tool_calls)),
            "tool_calls",
        )
    };
    ChatCompletionResponse {
        id: format!("chatcmpl-{}", next_id()),
        object: "chat.completion",
        created: unix_seconds(),
        model: model.to_owned(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessageDto {
                role: "assistant".to_owned(),
                content,
                tool_calls,
            },
            finish_reason,
        }],
        usage: Usage {
            prompt_tokens: completion.prompt_tokens,
            completion_tokens: completion.completion_tokens,
            total_tokens: completion.prompt_tokens + completion.completion_tokens,
        },
    }
}

/// Build an OpenAI-shaped error response with the given status.
fn error(status: StatusCode, message: impl Into<String>, r#type: &'static str) -> Response {
    (status, Json(ErrorResponse::new(message, r#type))).into_response()
}

/// The OpenAI error `type` an [`EngineError`] carries — the streaming
/// counterpart of the status code the JSON handlers choose (issue #848).
///
/// A stream has already sent `200` and its first event by the time generation
/// can fail, so the status code is spent and the `type` is the only place left
/// to say *whose* fault this was. It was the literal `"inference_error"` at
/// every one of those sites, which is a guess that happened to be right while
/// the only streaming failure was a broken decode. It stopped being right when
/// [`EngineError::InvalidRequest`] gained a way to arise from a chat template's
/// own refusal of the conversation: a client told `inference_error` retries, and
/// a client told `invalid_request_error` reads the message and fixes its
/// request.
///
/// The strings are the ones the non-streaming handlers already pass to
/// [`error`], deliberately — two spellings of one refusal is the thing this
/// server has been bitten by before.
///
/// Exhaustive on purpose: [`EngineError`] is not `#[non_exhaustive]`, so a new
/// variant should stop compiling here and be classified rather than default into
/// somebody's 500.
const fn error_kind(e: &EngineError) -> &'static str {
    match e {
        EngineError::UnknownModel(_) | EngineError::InvalidRequest(_) => "invalid_request_error",
        EngineError::Unsupported(_) => "not_implemented",
        EngineError::Inference(_) => "inference_error",
    }
}

/// Seconds since the Unix epoch (0 if the clock is before the epoch).
fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A process-monotonic counter for completion ids (no randomness needed).
fn next_id() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::{app, app_with_tools};
    use crate::engine::{
        ChatRequest, CompletionStats, Engine, EngineError, FinishReason, ModelInfo,
    };

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt as _;
    use tower::ServiceExt as _; // for `oneshot`

    /// A deterministic engine: serves `echo`, and replies with the last user
    /// message uppercased, streamed one word at a time so tests can assert both
    /// the accumulated and the streamed paths.
    struct MockEngine;

    impl Engine for MockEngine {
        fn models(&self) -> Vec<ModelInfo> {
            vec![ModelInfo {
                id: "echo".to_owned(),
            }]
        }

        fn chat_stream(
            &self,
            req: &ChatRequest,
            on_token: &mut dyn FnMut(&str),
        ) -> Result<CompletionStats, EngineError> {
            if req.model != "echo" {
                return Err(EngineError::UnknownModel(req.model.clone()));
            }
            let last = req
                .messages
                .last()
                .map(|m| m.content.to_uppercase())
                .unwrap_or_default();
            let words: Vec<&str> = last.split_whitespace().collect();
            let mut completion_tokens = 0u32;
            for (i, w) in words.iter().enumerate() {
                let piece = if i == 0 {
                    (*w).to_owned()
                } else {
                    format!(" {w}")
                };
                on_token(&piece);
                completion_tokens += 1;
            }
            Ok(CompletionStats {
                prompt_tokens: 3,
                completion_tokens,
                finish_reason: FinishReason::Stop,
            })
        }

        fn embed(&self, model: &str, inputs: &[String]) -> Result<Vec<Vec<f32>>, EngineError> {
            if model != "echo" {
                return Err(EngineError::UnknownModel(model.to_owned()));
            }
            // A fixed 3-d vector per input so tests can assert count and shape.
            Ok(inputs.iter().map(|_| vec![1.0, 2.0, 3.0]).collect())
        }
    }

    fn test_app() -> axum::Router {
        app(std::sync::Arc::new(MockEngine))
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// `[serve] tools = false` builds the untooled router, and that router still
    /// accepts a *client's* `tools` array — it returns the tool calls instead of
    /// running them. So the operator's bound has to reach this path too. It did
    /// not: `limits` was read from config and then only passed on the tooled arm,
    /// so `max_client_tool_bytes` was silently the built-in whenever graph tools
    /// were off. Raised in review of #769.
    #[tokio::test]
    async fn the_untooled_router_enforces_the_operator_s_bound() {
        let router = super::app_limited(
            std::sync::Arc::new(MockEngine),
            crate::types::Limits {
                max_client_tool_bytes: 512,
            },
        );
        // Comfortably under the 32 KiB built-in, so only the configured bound can
        // refuse it — the assertion is vacuous against `Limits::default()`.
        let body = serde_json::json!({
            "model": "echo",
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{
                "type": "function",
                "function": {"name": "wordy", "description": "z".repeat(1_024)},
            }],
        });
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let msg = body_json(resp).await["error"]["message"]
            .as_str()
            .expect("an error message")
            .to_owned();
        assert!(
            msg.contains("512 byte limit"),
            "the bound in force is the operator's, not the built-in: {msg}"
        );
    }

    #[tokio::test]
    async fn models_lists_served_models() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/v1/models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["object"], "list");
        assert_eq!(json["data"][0]["id"], "echo");
        assert_eq!(json["data"][0]["owned_by"], "roteiro");
    }

    fn chat_request(model: &str, user: &str) -> Request<Body> {
        let body = serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": user}],
        });
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    #[test]
    fn scoped_tools_fills_project_only_when_absent() {
        use super::ScopedTools;
        use crate::tools::{ToolDef, ToolRegistry};

        // A registry that echoes back the arguments it was called with.
        struct Echo;
        impl ToolRegistry for Echo {
            fn tools(&self) -> Vec<ToolDef> {
                Vec::new()
            }
            fn call(&self, _name: &str, args: &serde_json::Value) -> Result<String, String> {
                Ok(args.to_string())
            }
        }
        let scoped = ScopedTools {
            inner: &Echo,
            project: "beta".to_owned(),
        };
        // Omitted → the path project is injected.
        let out = scoped
            .call("search", &serde_json::json!({ "query": "x" }))
            .unwrap();
        assert!(out.contains(r#""project":"beta""#), "{out}");
        // Explicit → the caller's choice wins (cross-project query stays possible).
        let out = scoped
            .call(
                "search",
                &serde_json::json!({ "query": "x", "project": "alpha" }),
            )
            .unwrap();
        assert!(out.contains(r#""project":"alpha""#), "{out}");
        assert!(!out.contains("beta"), "{out}");
    }

    #[tokio::test]
    async fn projects_endpoint_lists_workspace_projects() {
        use super::app_with_tools;
        use crate::tools::{ToolDef, ToolRegistry};

        // A registry that hosts two named projects.
        struct TwoProjects;
        impl ToolRegistry for TwoProjects {
            fn tools(&self) -> Vec<ToolDef> {
                Vec::new()
            }
            fn call(&self, _n: &str, _a: &serde_json::Value) -> Result<String, String> {
                Ok(String::new())
            }
            fn projects(&self) -> Vec<String> {
                vec!["alpha".to_owned(), "beta".to_owned()]
            }
        }
        let app = app_with_tools(
            std::sync::Arc::new(MockEngine),
            std::sync::Arc::new(TwoProjects),
        );
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/v1/projects")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["object"], "list");
        assert_eq!(json["data"][0]["id"], "alpha");
        assert_eq!(json["data"][1]["id"], "beta");
        assert_eq!(json["data"][0]["object"], "project");

        // With no tools registered, the list is simply empty (single source).
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/v1/projects")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(body_json(resp).await["data"].as_array().unwrap().len(), 0);
    }

    /// A registry whose sole tool reports which workspace it belongs to, so the
    /// model's tool result — fed back and echoed as the answer — is DIFFERENT per
    /// workspace. That is what lets the workspace-routing test prove the request ran
    /// over the RIGHT per-workspace registry, not merely that a route exists.
    struct Tagged(&'static str);
    impl crate::tools::ToolRegistry for Tagged {
        fn tools(&self) -> Vec<crate::tools::ToolDef> {
            vec![crate::tools::ToolDef {
                name: "who".to_owned(),
                description: "which workspace am I?".to_owned(),
                parameters: serde_json::json!({"type": "object"}),
            }]
        }
        fn call(&self, _n: &str, _a: &serde_json::Value) -> Result<String, String> {
            Ok(self.0.to_owned())
        }
        fn projects(&self) -> Vec<String> {
            vec![self.0.to_owned()]
        }
    }

    /// A registry that advertises no tools — the flat/default registry in the
    /// workspace-routing test, so the unscoped path never shadows the scoped one.
    struct NoTools;
    impl crate::tools::ToolRegistry for NoTools {
        fn tools(&self) -> Vec<crate::tools::ToolDef> {
            Vec::new()
        }
        fn call(&self, _n: &str, _a: &serde_json::Value) -> Result<String, String> {
            Ok(String::new())
        }
    }

    /// A deterministic engine that actually drives the tool loop: on the first turn
    /// it calls `who`; once it sees the `<tool_response>` fed back, it echoes that
    /// payload as the final answer. So the completion carries whatever the dispatched
    /// registry's `who` returned — the workspace's own tag.
    struct ToolEchoEngine;
    impl Engine for ToolEchoEngine {
        fn models(&self) -> Vec<ModelInfo> {
            vec![ModelInfo {
                id: "echo".to_owned(),
            }]
        }
        fn chat_stream(
            &self,
            req: &ChatRequest,
            on_token: &mut dyn FnMut(&str),
        ) -> Result<CompletionStats, EngineError> {
            if req.model != "echo" {
                return Err(EngineError::UnknownModel(req.model.clone()));
            }
            let last = req.messages.last().map_or("", |m| m.content.as_str());
            let out = match last
                .strip_prefix("<tool_response>")
                .and_then(|s| s.strip_suffix("</tool_response>"))
            {
                // The tool result came back → answer with exactly its payload.
                Some(payload) => payload.to_owned(),
                // No result yet → request the identity tool.
                None => "<tool_call>{\"name\":\"who\",\"arguments\":{}}</tool_call>".to_owned(),
            };
            on_token(&out);
            Ok(CompletionStats {
                prompt_tokens: 1,
                completion_tokens: 1,
                finish_reason: FinishReason::Stop,
            })
        }
    }

    /// The status + answer content a workspace-scoped chat route returns for `ws`,
    /// given the `ToolEchoEngine` tool loop (the answer echoes that workspace's tag).
    async fn ask_workspace(app: &axum::Router, ws: &str) -> (StatusCode, String) {
        let body = serde_json::json!({
            "model": "echo",
            "messages": [{"role": "user", "content": "which workspace is this?"}],
        });
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/workspaces/{ws}/chat/completions"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let content = body_json(resp).await["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        (status, content)
    }

    #[tokio::test]
    async fn workspace_scoped_route_dispatches_to_its_own_registry_and_404s_when_unknown() {
        use super::{MAX_TOOL_ROUNDS, app_with_workspace_tools};
        use crate::engine::Message;
        use crate::tools::{ToolRegistry, chat_with_tools};

        // Sanity-check the engine/registry pair in isolation (no HTTP): the tool loop
        // over each per-workspace registry yields that workspace's own tag.
        let req = ChatRequest {
            tools: None,
            model: "echo".to_owned(),
            messages: vec![Message {
                role: "user".to_owned(),
                content: "which workspace is this?".to_owned(),
            }],
            images: vec![],
            audio: vec![],
            temperature: 0.0,
            max_tokens: 64,
        };
        assert_eq!(
            chat_with_tools(&ToolEchoEngine, &Tagged("api"), &req, MAX_TOOL_ROUNDS)
                .unwrap()
                .content,
            "api"
        );
        assert_eq!(
            chat_with_tools(&ToolEchoEngine, &Tagged("docs"), &req, MAX_TOOL_ROUNDS)
                .unwrap()
                .content,
            "docs"
        );

        let mut workspaces: std::collections::HashMap<String, std::sync::Arc<dyn ToolRegistry>> =
            std::collections::HashMap::new();
        workspaces.insert("api".to_owned(), std::sync::Arc::new(Tagged("api")));
        workspaces.insert("docs".to_owned(), std::sync::Arc::new(Tagged("docs")));
        let app = app_with_workspace_tools(
            std::sync::Arc::new(ToolEchoEngine),
            std::sync::Arc::new(NoTools),
            workspaces,
        );

        // Each workspace resolves to ITS OWN registry: `api` reports `api`, `docs`
        // reports `docs`. Different outputs from the same request prove the handler
        // dispatched to the correct per-workspace registry (not just that a route
        // exists — a 200-only check couldn't tell the two apart).
        let (status_api, content_api) = ask_workspace(&app, "api").await;
        assert_eq!(status_api, StatusCode::OK);
        assert_eq!(
            content_api, "api",
            "the `api` route must use the `api` registry"
        );

        let (status_docs, content_docs) = ask_workspace(&app, "docs").await;
        assert_eq!(status_docs, StatusCode::OK);
        assert_eq!(
            content_docs, "docs",
            "the `docs` route must use the `docs` registry"
        );
        assert_ne!(
            content_api, content_docs,
            "the two workspaces must yield different, workspace-specific results"
        );

        // An unknown workspace is a 404, never answered from another registry.
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/workspaces/nope/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "model": "echo",
                            "messages": [{"role": "user", "content": "hi"}],
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn project_prefixed_routes_work() {
        // `/v1/{project}/chat/completions` round-trips (the prefix is accepted; the
        // scope only matters once tools are registered).
        let body = serde_json::json!({
            "model": "echo",
            "messages": [{"role": "user", "content": "hi there"}],
        });
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/myproj/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            body_json(resp).await["choices"][0]["message"]["content"],
            "HI THERE"
        );

        // `/v1/{project}/models` returns the same engine-level list.
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/v1/myproj/models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_json(resp).await["data"][0]["id"], "echo");
    }

    #[tokio::test]
    async fn chat_completion_round_trips_through_the_engine() {
        let resp = test_app()
            .oneshot(chat_request("echo", "hi there"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["object"], "chat.completion");
        assert_eq!(json["choices"][0]["message"]["content"], "HI THERE");
        assert_eq!(json["choices"][0]["finish_reason"], "stop");
        assert_eq!(json["usage"]["total_tokens"], 5);
    }

    #[tokio::test]
    async fn streaming_emits_chunks_and_done() {
        let body = serde_json::json!({
            "model": "echo",
            "messages": [{"role": "user", "content": "hi there"}],
            "stream": true,
        });
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        // Role chunk first, then per-word content deltas, a finish chunk, [DONE].
        assert!(
            text.contains("chat.completion.chunk"),
            "chunk object: {text}"
        );
        assert!(
            text.contains("\"role\":\"assistant\""),
            "role chunk: {text}"
        );
        assert!(text.contains("\"content\":\"HI\""), "first delta: {text}");
        assert!(
            text.contains("\"content\":\" THERE\""),
            "second delta: {text}"
        );
        assert!(
            text.contains("\"finish_reason\":\"stop\""),
            "finish: {text}"
        );
        assert!(text.contains("data: [DONE]"), "terminator: {text}");
    }

    #[tokio::test]
    async fn streaming_unknown_model_is_404_not_a_stream() {
        // An unknown model must 404 up front, not open a 200 SSE that fails later.
        let body = serde_json::json!({
            "model": "nope",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true,
        });
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn embeddings_return_a_vector_per_input() {
        // Array input → one embedding per element, in order.
        let body = serde_json::json!({ "model": "echo", "input": ["alpha", "beta"] });
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/embeddings")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["object"], "list");
        assert_eq!(json["data"].as_array().unwrap().len(), 2);
        assert_eq!(json["data"][0]["object"], "embedding");
        assert_eq!(json["data"][0]["embedding"].as_array().unwrap().len(), 3);
        assert_eq!(json["data"][1]["index"], 1);
    }

    #[tokio::test]
    async fn embeddings_accept_a_single_string_and_reject_unknown_model() {
        // Single-string input is accepted (OpenAI allows string or array).
        let ok = test_app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/embeddings")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "model": "echo", "input": "hello" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ok.status(), StatusCode::OK);
        let json = body_json(ok).await;
        assert_eq!(json["data"].as_array().unwrap().len(), 1);

        // An unknown model is a 404.
        let bad = test_app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/embeddings")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "model": "nope", "input": "hi" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn multimodal_content_parts_are_parsed() {
        // A user turn with a text part + an image_url (tiny 1x1 PNG data URI):
        // the text is extracted (mock echoes it) and the image decodes without error.
        let png = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
        let body = serde_json::json!({
            "model": "echo",
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": "look"},
                {"type": "image_url", "image_url": {"url": png}},
            ]}],
        });
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["choices"][0]["message"]["content"], "LOOK");
    }

    async fn post_chat(body: serde_json::Value) -> axum::http::StatusCode {
        test_app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    /// Sends `body` and returns the whole response, so a test can read the
    /// refusal a caller would actually be shown rather than only its status.
    async fn post_chat_response(body: serde_json::Value) -> axum::response::Response {
        test_app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    fn chat_plus(extra: &serde_json::Value) -> serde_json::Value {
        let mut body = serde_json::json!({
            "model": "echo",
            "messages": [{"role": "user", "content": "hi"}],
        });
        let map = body.as_object_mut().expect("an object");
        for (k, v) in extra.as_object().expect("an object") {
            map.insert(k.clone(), v.clone());
        }
        body
    }

    /// **Every** parameter declared `400` is refused over HTTP, by name.
    ///
    /// The unit tests in [`crate::openai_params`] prove the check refuses; this
    /// proves the refusal survives the trip through axum's extractor, serde's
    /// `flatten` and the error envelope, and reaches a client as a `400` naming
    /// the parameter. Driven from the declaration itself, so a row added later
    /// is covered the moment it is added rather than when someone remembers to
    /// come back here.
    #[tokio::test]
    async fn every_declared_refusal_is_a_400_over_http() {
        for p in crate::openai_params::OPENAI_CHAT_PARAMS {
            let Some(refusal) = p.refusal() else { continue };
            // A value no client library sends by accident — see
            // `openai_params`' inert defaults for the ones that are.
            let body = chat_plus(&serde_json::json!({ p.name: "roteiro-decided-this" }));
            let resp = post_chat_response(body).await;
            assert_eq!(
                resp.status(),
                StatusCode::BAD_REQUEST,
                "`{}` is declared 400 but the endpoint served it",
                p.name
            );
            let json = body_json(resp).await;
            assert_eq!(json["error"]["type"], "invalid_request_error", "{}", p.name);
            assert_eq!(
                json["error"]["message"], refusal,
                "the refusal on the wire is not the one that is declared and published"
            );
        }
    }

    /// The two the issue calls most likely to be load-bearing, end to end.
    ///
    /// Before this change both returned `200` and a wrong answer: `seed` a
    /// non-reproducible completion, `response_format` prose that does not parse.
    #[tokio::test]
    async fn seed_and_response_format_are_refused_with_somewhere_to_go() {
        let resp = post_chat_response(chat_plus(&serde_json::json!({"seed": 42}))).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let message = body_json(resp).await["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        assert!(message.contains("`seed`"), "{message}");
        assert!(message.contains("temperature: 0"), "{message}");

        let resp = post_chat_response(chat_plus(
            &serde_json::json!({"response_format": {"type": "json_object"}}),
        ))
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let message = body_json(resp).await["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        assert!(message.contains("`response_format`"), "{message}");
        assert!(message.contains("Ask for JSON in the prompt"), "{message}");
    }

    /// The callers `deny_unknown_fields` would have broken still get a `200`.
    ///
    /// This is the other half of the decision and the reason for the narrower
    /// instrument: a request carrying every habitual key **and** the defaults a
    /// client library serialises without being asked is a request that decided
    /// nothing Roteiro cannot do, and it must still be answered.
    #[tokio::test]
    async fn a_habitual_client_is_still_answered() {
        let body = chat_plus(&serde_json::json!({
            // Bookkeeping keys, declared `dropped`.
            "user": "u-42",
            "store": false,
            "metadata": {"run": "nightly"},
            "service_tier": "auto",
            "prompt_cache_key": "bucket-1",
            "safety_identifier": "s-9",
            // Defaults of refused parameters: present, but no decision.
            "n": 1,
            "top_p": 1.0,
            "frequency_penalty": 0,
            "presence_penalty": 0.0,
            "logit_bias": {},
            "logprobs": false,
            "seed": serde_json::Value::Null,
            "stop": serde_json::Value::Null,
            "response_format": {"type": "text"},
            // A key this table has never heard of.
            "x_some_client_extension": true,
        }));
        let resp = post_chat_response(body).await;
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "a caller who decided nothing this endpoint cannot do was refused"
        );
    }

    /// `n: 1` and `n: 3` are the same key and different requests.
    ///
    /// The single sharpest consequence of keying on the value rather than the
    /// presence, and the one that would silently invert if someone later
    /// "simplified" the check to `extra.contains_key(name)`.
    #[tokio::test]
    async fn the_same_key_is_answered_by_what_it_carries() {
        assert_eq!(
            post_chat(chat_plus(&serde_json::json!({"n": 1}))).await,
            StatusCode::OK
        );
        assert_eq!(
            post_chat(chat_plus(&serde_json::json!({"n": 3}))).await,
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn image_in_a_non_user_message_is_400() {
        let img = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
        let body = serde_json::json!({
            "model": "echo",
            "messages": [
                {"role": "system", "content": [{"type": "image_url", "image_url": {"url": img}}]},
                {"role": "user", "content": "hi"},
            ],
        });
        assert_eq!(post_chat(body).await, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn too_many_images_is_400() {
        let img = serde_json::json!({
            "type": "image_url",
            "image_url": {"url": "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="},
        });
        let parts: Vec<serde_json::Value> = (0..9).map(|_| img.clone()).collect();
        let body = serde_json::json!({
            "model": "echo",
            "messages": [{"role": "user", "content": parts}],
        });
        assert_eq!(post_chat(body).await, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn non_image_data_uri_is_400() {
        let body = serde_json::json!({
            "model": "echo",
            "messages": [{"role": "user", "content": [
                {"type": "image_url", "image_url": {"url": "data:text/plain;base64,aGk="}},
            ]}],
        });
        assert_eq!(post_chat(body).await, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn images_to_a_text_only_model_are_400() {
        use crate::engine::{ChatRequest, CompletionStats};

        struct TextOnly;
        impl Engine for TextOnly {
            fn models(&self) -> Vec<ModelInfo> {
                vec![ModelInfo {
                    id: "echo".to_owned(),
                }]
            }
            fn chat_stream(
                &self,
                req: &ChatRequest,
                _on_token: &mut dyn FnMut(&str),
            ) -> Result<CompletionStats, EngineError> {
                if req.images.is_empty() {
                    Ok(CompletionStats {
                        prompt_tokens: 1,
                        completion_tokens: 0,
                        finish_reason: FinishReason::Stop,
                    })
                } else {
                    Err(EngineError::InvalidRequest("text-only model".to_owned()))
                }
            }
        }

        let img = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
        let body = serde_json::json!({
            "model": "echo",
            "messages": [{"role": "user", "content": [
                {"type": "image_url", "image_url": {"url": img}},
            ]}],
        });
        let resp = app(std::sync::Arc::new(TextOnly))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn remote_image_url_is_rejected() {
        let body = serde_json::json!({
            "model": "echo",
            "messages": [{"role": "user", "content": [
                {"type": "image_url", "image_url": {"url": "https://example.com/x.png"}},
            ]}],
        });
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn embeddings_empty_input_is_400() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/embeddings")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "model": "echo", "input": [] }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn an_overlong_embedding_input_is_a_400_not_a_500() {
        // llama.cpp bounds how many tokens one batch may carry and enforces it
        // with a `GGML_ASSERT`, which aborts the *process* — so `rto-llama`
        // refuses an over-long input up front as an `InvalidRequest` (issue
        // #346). That variant reached `/v1/embeddings` for the first time with
        // that guard: before it, `embed` could only fail as `UnknownModel`,
        // `Unsupported` or `Inference`, so this arm of the match was unreachable
        // and untested. It has to be a 400 — the caller sent too much text, which
        // is something they can fix, and a 500 would tell them the opposite.
        struct OverlongEngine;
        impl Engine for OverlongEngine {
            fn models(&self) -> Vec<ModelInfo> {
                vec![ModelInfo {
                    id: "bge-small-en-v1.5-gguf".to_owned(),
                }]
            }
            fn chat_stream(
                &self,
                _req: &ChatRequest,
                _on_token: &mut dyn FnMut(&str),
            ) -> Result<CompletionStats, EngineError> {
                unreachable!("this test only drives the embeddings route")
            }
            fn embed(
                &self,
                _model: &str,
                _inputs: &[String],
            ) -> Result<Vec<Vec<f32>>, EngineError> {
                Err(EngineError::InvalidRequest(
                    "embedding input is 702 tokens, over the 512-token limit this \
                     model accepts in one request — send less text (shorten or split it)"
                        .to_owned(),
                ))
            }
        }

        let body = serde_json::json!({
            "model": "bge-small-en-v1.5-gguf",
            "input": "…a very long document…",
        });
        let resp = app(std::sync::Arc::new(OverlongEngine))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/embeddings")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = body_json(resp).await;
        assert_eq!(json["error"]["type"], "invalid_request_error");
        // The limit and the actual size must survive the trip to the client:
        // they are the only way a caller learns how much to cut.
        let message = json["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("702") && message.contains("512"),
            "{message}"
        );
    }

    #[tokio::test]
    async fn embedding_model_chat_is_a_clean_error_not_a_crash() {
        // The engine's chat guard rejects an encoder-only embedding model with a
        // typed `InvalidRequest` *before* the decode path that would abort the
        // process (GGML_ASSERT). The server must surface that as a 400 — a single
        // bad request never kills the server for everyone.
        struct EmbeddingEngine;
        impl Engine for EmbeddingEngine {
            fn models(&self) -> Vec<ModelInfo> {
                // The model IS served (for `/v1/embeddings`), so the up-front
                // existence check passes and the request reaches the guard.
                vec![ModelInfo {
                    id: "bge-small-en-v1.5-gguf".to_owned(),
                }]
            }
            fn chat_stream(
                &self,
                req: &ChatRequest,
                _on_token: &mut dyn FnMut(&str),
            ) -> Result<CompletionStats, EngineError> {
                Err(EngineError::InvalidRequest(format!(
                    "model `{}` is an embedding model and cannot generate chat completions",
                    req.model,
                )))
            }
        }

        let body = serde_json::json!({
            "model": "bge-small-en-v1.5-gguf",
            "messages": [{"role": "user", "content": "hi"}],
        });
        let resp = app(std::sync::Arc::new(EmbeddingEngine))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = body_json(resp).await;
        assert_eq!(json["error"]["type"], "invalid_request_error");
    }

    #[tokio::test]
    async fn unknown_model_is_404() {
        let resp = test_app()
            .oneshot(chat_request("nope", "hi"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let json = body_json(resp).await;
        assert_eq!(json["error"]["type"], "invalid_request_error");
    }

    #[tokio::test]
    async fn null_content_is_accepted_as_empty() {
        // OpenAI clients may send `content: null`; it must deserialize (as "")
        // rather than 400 before normalisation.
        let body = serde_json::json!({
            "model": "echo",
            "messages": [
                {"role": "system", "content": null},
                {"role": "user", "content": "hey"},
            ],
        });
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["choices"][0]["message"]["content"], "HEY");
    }

    #[tokio::test]
    async fn empty_messages_is_400() {
        let body = serde_json::json!({ "model": "echo", "messages": [] });
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    // The TLS path loads the cert/key *before* binding, so a missing file is a
    // clean error rather than a hang or a bound-but-broken listener. Exercises the
    // ring-provider install + PEM-loading wiring without needing a live handshake
    // (the actual serving is axum-server's, verified end-to-end via `curl -k`).
    #[cfg(feature = "tls")]
    #[test]
    fn serve_tls_reports_a_missing_certificate() {
        use std::sync::Arc;
        let engine: Arc<dyn Engine> = Arc::new(MockEngine);
        let addr = "127.0.0.1:0".parse().expect("addr");
        // A guaranteed-absent path under the platform temp dir (portable; not a
        // hard-coded POSIX path), unique per process so a stray real file can't
        // make this flaky.
        let missing =
            std::env::temp_dir().join(format!("roteiro-no-such-cert-{}.pem", std::process::id()));
        let err = super::serve_blocking_tls(engine, None, addr, &missing, &missing)
            .expect_err("a missing certificate must error, not bind");
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("cert") || msg.contains("tls"),
            "error should name the certificate: {err}"
        );
    }

    // ---------------------------------------------------------------- #485 ---
    // Client-supplied `tools` on the wire. `POST /v1/chat/completions` is
    // OpenAI's path, so a client's `tools` array is honoured — by returning the
    // call, never by executing it.

    /// An engine that emits a fixed script of turns and records the messages it
    /// was handed, so a test can assert both what the model was told and what the
    /// loop did with what it said.
    struct ScriptedServeEngine {
        turns: std::sync::Mutex<Vec<String>>,
        seen: std::sync::Mutex<Vec<Vec<crate::engine::Message>>>,
    }

    impl ScriptedServeEngine {
        fn new(turns: &[&str]) -> std::sync::Arc<Self> {
            std::sync::Arc::new(Self {
                turns: std::sync::Mutex::new(turns.iter().map(|s| (*s).to_owned()).collect()),
                seen: std::sync::Mutex::new(Vec::new()),
            })
        }
        fn first_system_prompt(&self) -> String {
            self.seen.lock().unwrap()[0][0].content.clone()
        }
    }

    impl Engine for ScriptedServeEngine {
        fn models(&self) -> Vec<ModelInfo> {
            vec![ModelInfo {
                id: "echo".to_owned(),
            }]
        }
        fn chat_stream(
            &self,
            req: &ChatRequest,
            on_token: &mut dyn FnMut(&str),
        ) -> Result<CompletionStats, EngineError> {
            self.seen.lock().unwrap().push(req.messages.clone());
            let next = self.turns.lock().unwrap().remove(0);
            on_token(&next);
            Ok(CompletionStats {
                prompt_tokens: 3,
                completion_tokens: 1,
                finish_reason: FinishReason::Stop,
            })
        }
    }

    /// A graph registry advertising `graph_only_tool` that panics if executed —
    /// so "the client's tools suppressed these" is asserted by construction.
    struct PanicRegistry;

    impl crate::tools::ToolRegistry for PanicRegistry {
        fn tools(&self) -> Vec<crate::tools::ToolDef> {
            vec![crate::tools::ToolDef {
                name: "graph_only_tool".to_owned(),
                description: "a graph tool".to_owned(),
                parameters: serde_json::json!({"type": "object"}),
            }]
        }
        fn call(&self, name: &str, _a: &serde_json::Value) -> Result<String, String> {
            panic!("executed `{name}` — a suppressed graph tool must never run");
        }
    }

    /// The client's `tools` array, OpenAI-shaped.
    fn weather_tools() -> serde_json::Value {
        serde_json::json!([{
            "type": "function",
            "function": {
                "name": "get_weather",
                "description": "current weather for a city",
                "parameters": {
                    "type": "object",
                    "properties": {"city": {"type": "string"}},
                    "required": ["city"],
                },
            },
        }])
    }

    fn chat_body(body: &serde_json::Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    const WEATHER_CALL: &str =
        "<tool_call>{\"name\":\"get_weather\",\"arguments\":{\"city\":\"Berlin\"}}</tool_call>";

    #[tokio::test]
    async fn a_client_tool_call_is_returned_with_finish_reason_tool_calls() {
        let engine = ScriptedServeEngine::new(&[WEATHER_CALL]);
        let resp = app(engine)
            .oneshot(chat_body(&serde_json::json!({
                "model": "echo",
                "messages": [{"role": "user", "content": "weather in Berlin?"}],
                "tools": weather_tools(),
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let choice = &json["choices"][0];

        assert_eq!(choice["finish_reason"], "tool_calls");
        // The one wire-visible change reaching existing callers: `content` is now
        // nullable, and a tool-call turn serialises an explicit `null` (OpenAI's
        // shape) rather than omitting the field.
        assert!(choice["message"]["content"].is_null(), "{text}");
        assert!(text.contains("\"content\":null"), "explicit null: {text}");

        let call = &choice["message"]["tool_calls"][0];
        assert_eq!(call["type"], "function");
        assert_eq!(call["function"]["name"], "get_weather");
        assert!(
            call["id"].as_str().is_some_and(|s| !s.is_empty()),
            "a correlation id for `tool_call_id`: {text}"
        );
        // `arguments` is a JSON **string** on the OpenAI wire, not an object.
        let arguments = call["function"]["arguments"].as_str().expect("a string");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(arguments).unwrap(),
            serde_json::json!({"city": "Berlin"})
        );
    }

    #[tokio::test]
    async fn client_tools_suppress_the_graph_tools_over_http() {
        let engine = ScriptedServeEngine::new(&["I cannot help with that."]);
        let router = app_with_tools(engine.clone(), std::sync::Arc::new(PanicRegistry));
        let resp = router
            .oneshot(chat_body(&serde_json::json!({
                "model": "echo",
                "messages": [{"role": "user", "content": "weather in Berlin?"}],
                "tools": weather_tools(),
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let prompt = engine.first_system_prompt();
        assert!(prompt.contains("get_weather"), "{prompt}");
        assert!(
            !prompt.contains("graph_only_tool"),
            "a client sending its own tools does not also get Roteiro's: {prompt}"
        );
    }

    #[tokio::test]
    async fn graph_tools_still_run_when_the_client_sends_no_tools() {
        // Suppression must not leak into the Ask path, which sends no `tools`.
        struct GraphOnly;
        impl crate::tools::ToolRegistry for GraphOnly {
            fn tools(&self) -> Vec<crate::tools::ToolDef> {
                vec![crate::tools::ToolDef {
                    name: "graph_only_tool".to_owned(),
                    description: "a graph tool".to_owned(),
                    parameters: serde_json::json!({"type": "object"}),
                }]
            }
            fn call(&self, _n: &str, _a: &serde_json::Value) -> Result<String, String> {
                Ok("the graph said so".to_owned())
            }
        }
        let engine = ScriptedServeEngine::new(&[
            "<tool_call>{\"name\":\"graph_only_tool\",\"arguments\":{}}</tool_call>",
            "the graph said so",
        ]);
        let router = app_with_tools(engine.clone(), std::sync::Arc::new(GraphOnly));
        let resp = router
            .oneshot(chat_body(&serde_json::json!({
                "model": "echo",
                "messages": [{"role": "user", "content": "why?"}],
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["choices"][0]["finish_reason"], "stop");
        assert_eq!(
            json["choices"][0]["message"]["content"],
            "the graph said so"
        );
        // The tool ran and its result was fed back as a `<tool_response>` user turn.
        let second_round = &engine.seen.lock().unwrap()[1];
        assert!(
            second_round
                .iter()
                .any(|m| m.role == "user" && m.content.contains("<tool_response>")),
            "{second_round:?}"
        );
    }

    #[tokio::test]
    async fn a_tool_role_turn_becomes_a_tool_response_user_turn() {
        // Counter-intuitive and load-bearing: a `role: "tool"` message would emit
        // a role token the served models were never trained on, while a
        // `<tool_response>` user turn is what every Qwen template emits natively.
        let engine = ScriptedServeEngine::new(&["Berlin is 21 degrees."]);
        let resp = app(engine.clone())
            .oneshot(chat_body(&serde_json::json!({
                "model": "echo",
                "messages": [
                    {"role": "user", "content": "weather in Berlin?"},
                    {"role": "assistant", "content": null, "tool_calls": [{
                        "id": "call_0",
                        "type": "function",
                        "function": {"name": "get_weather", "arguments": "{\"city\":\"Berlin\"}"},
                    }]},
                    {"role": "tool", "tool_call_id": "call_0", "name": "get_weather",
                     "content": "{\"temp\":21}"},
                ],
                "tools": weather_tools(),
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let seen = engine.seen.lock().unwrap();
        let turns = &seen[0];
        assert!(
            turns.iter().all(|m| m.role != "tool"),
            "no `tool` role reaches the template: {turns:?}"
        );
        assert!(
            turns
                .iter()
                .any(|m| m.role == "user"
                    && m.content == "<tool_response>{\"temp\":21}</tool_response>"),
            "the result is a `<tool_response>` user turn: {turns:?}"
        );
        // And the assistant's own call is replayed in the in-band form, so the
        // response has an antecedent instead of dangling.
        assert!(
            turns.iter().any(|m| m.role == "assistant"
                && m.content.contains("<tool_call>")
                && m.content.contains("get_weather")
                && m.content.contains("Berlin")),
            "the replayed call is rendered back: {turns:?}"
        );
    }

    #[tokio::test]
    async fn streaming_emits_one_complete_tool_calls_chunk() {
        let engine = ScriptedServeEngine::new(&[WEATHER_CALL]);
        let resp = app(engine)
            .oneshot(chat_body(&serde_json::json!({
                "model": "echo",
                "messages": [{"role": "user", "content": "weather in Berlin?"}],
                "tools": weather_tools(),
                "stream": true,
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);

        assert!(
            text.contains("\"role\":\"assistant\""),
            "role chunk: {text}"
        );
        // Declared divergence: one complete chunk at `index: 0` carrying whole
        // `arguments`, where OpenAI fragments `arguments` across chunks.
        assert!(text.contains("\"index\":0"), "per-call index: {text}");
        assert!(
            text.contains("\"name\":\"get_weather\""),
            "the call: {text}"
        );
        assert!(
            text.contains(r#""arguments":"{\"city\":\"Berlin\"}""#),
            "whole arguments in one chunk: {text}"
        );
        assert!(
            text.contains("\"finish_reason\":\"tool_calls\""),
            "terminating reason: {text}"
        );
        assert!(text.contains("data: [DONE]"), "terminator: {text}");
    }

    #[tokio::test]
    async fn tool_choice_and_parallel_tool_calls_are_accepted_but_not_enforced() {
        // Declared, not half-implemented: both fields are accepted (no 400), and
        // neither changes behaviour. Forcing a named function is grammar work.
        let engine = ScriptedServeEngine::new(&["I would rather just answer."]);
        let resp = app(engine)
            .oneshot(chat_body(&serde_json::json!({
                "model": "echo",
                "messages": [{"role": "user", "content": "weather in Berlin?"}],
                "tools": weather_tools(),
                "tool_choice": {"type": "function", "function": {"name": "get_weather"}},
                "parallel_tool_calls": true,
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "the fields are accepted");
        let json = body_json(resp).await;
        assert_eq!(
            json["choices"][0]["finish_reason"], "stop",
            "`tool_choice` did not force a call — the divergence the README declares"
        );
        assert_eq!(
            json["choices"][0]["message"]["content"],
            "I would rather just answer."
        );
    }

    /// A reasoning model: emits `text` in `pieces` tokens and stops for
    /// `finish_reason`. Records what it was prompted with, so a test can assert
    /// on the history the tool loop fed back.
    struct ReasoningEngine {
        text: String,
        pieces: usize,
        finish_reason: FinishReason,
        seen: std::sync::Mutex<Vec<Vec<crate::engine::Message>>>,
    }

    impl ReasoningEngine {
        fn new(text: &str, finish_reason: FinishReason) -> std::sync::Arc<Self> {
            Self::in_pieces(text, 1, finish_reason)
        }

        /// The same generation split across `pieces` tokens — the streaming
        /// tests care that a tag straddling a token boundary is still a tag.
        fn in_pieces(
            text: &str,
            pieces: usize,
            finish_reason: FinishReason,
        ) -> std::sync::Arc<Self> {
            std::sync::Arc::new(Self {
                text: text.to_owned(),
                pieces,
                finish_reason,
                seen: std::sync::Mutex::new(Vec::new()),
            })
        }
    }

    impl Engine for ReasoningEngine {
        fn models(&self) -> Vec<ModelInfo> {
            vec![ModelInfo {
                id: "echo".to_owned(),
            }]
        }
        fn chat_stream(
            &self,
            req: &ChatRequest,
            on_token: &mut dyn FnMut(&str),
        ) -> Result<CompletionStats, EngineError> {
            self.seen.lock().unwrap().push(req.messages.clone());
            let chars: Vec<char> = self.text.chars().collect();
            let per = chars.len().div_ceil(self.pieces.max(1)).max(1);
            for chunk in chars.chunks(per) {
                on_token(&chunk.iter().collect::<String>());
            }
            Ok(CompletionStats {
                prompt_tokens: 3,
                completion_tokens: 105,
                finish_reason: self.finish_reason,
            })
        }
    }

    /// Reassemble an SSE body into the text a client would have displayed.
    ///
    /// Concatenated rather than substring-matched, because the point of the
    /// streaming path is that the answer arrives in pieces: `"four"` may well
    /// cross a chunk boundary as `"fou"` + `"r"`, and a test that grepped the
    /// raw body for it would be asserting on the tokenisation instead of the
    /// content.
    fn stream_content(sse: &str) -> String {
        sse.lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .filter(|l| *l != "[DONE]")
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter_map(|v| {
                v["choices"][0]["delta"]["content"]
                    .as_str()
                    .map(str::to_owned)
            })
            .collect()
    }

    /// The generation #582 was filed on, near enough verbatim.
    const THINKS_THEN_ANSWERS: &str = "<think>\nOkay, the user is asking \"What is 2+2?\". Just a direct answer.\n</think>\n\nfour";

    /// **The defect #582 is: this is stripped for every CLI consumer and was
    /// not stripped here.** Same model, same block, two answers depending on
    /// which consumer asked. Measured live, ~95 of 105 completion tokens were
    /// reasoning for a one-word answer, and all of them reached the caller.
    #[tokio::test]
    async fn a_reasoning_block_does_not_reach_an_http_caller() {
        let engine = ReasoningEngine::new(THINKS_THEN_ANSWERS, FinishReason::Stop);
        let resp = app(engine)
            .oneshot(chat_body(&serde_json::json!({
                "model": "echo",
                "messages": [{"role": "user", "content": "What is 2+2?"}],
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["choices"][0]["message"]["content"], "four");

        // The counts are the model's, not the answer's: it really did spend them,
        // and a client budgeting against a number Roteiro shrank would be told a
        // comfortable lie.
        assert_eq!(json["usage"]["completion_tokens"], 105);
    }

    /// The same, with tools advertised — the Ask path, which goes through the
    /// tool loop rather than the untooled shortcut. Both routes reach the
    /// caller, so a fix on one of them is half a fix.
    #[tokio::test]
    async fn the_tool_loop_path_strips_it_too() {
        let engine = ReasoningEngine::new(THINKS_THEN_ANSWERS, FinishReason::Stop);
        let resp = app_with_tools(engine, std::sync::Arc::new(PanicRegistry))
            .oneshot(chat_body(&serde_json::json!({
                "model": "echo",
                "messages": [{"role": "user", "content": "What is 2+2?"}],
            })))
            .await
            .unwrap();
        assert_eq!(
            body_json(resp).await["choices"][0]["message"]["content"],
            "four"
        );
    }

    /// **#583 over HTTP.** The block never closes, so there is no answer in this
    /// generation at all — and the old rule returned the text unchanged, which
    /// would publish a model's scratch deliberation as its reply.
    #[tokio::test]
    async fn an_unterminated_block_is_refused_rather_than_published() {
        let cut = "<think>\nOkay, I need to compare B-trees and LSM trees";
        let engine = ReasoningEngine::new(cut, FinishReason::Length);
        let resp = app(engine)
            .oneshot(chat_body(&serde_json::json!({
                "model": "echo",
                "messages": [{"role": "user", "content": "B-trees vs LSM?"}],
                "max_tokens": 512,
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        let content = json["choices"][0]["message"]["content"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(
            !content.contains("B-trees"),
            "the deliberation was published as the answer: {content}"
        );
        assert!(
            content.starts_with("Roteiro: "),
            "a refusal says who is speaking: {content}"
        );
        assert!(
            content.contains("512"),
            "it names the budget the caller can raise: {content}"
        );
        // The stop reason is the model's and is left alone, so a machine client
        // still learns that generation was cut short.
        assert_eq!(json["choices"][0]["finish_reason"], "length");
    }

    /// A model that stops on its own inside the block gets a different sentence,
    /// because it has a different way forward: no budget will fix it.
    #[tokio::test]
    async fn a_model_that_stops_mid_reasoning_is_told_apart_from_one_that_ran_out() {
        let engine = ReasoningEngine::new("<think>\nhmm", FinishReason::Stop);
        let resp = app(engine)
            .oneshot(chat_body(&serde_json::json!({
                "model": "echo",
                "messages": [{"role": "user", "content": "hi"}],
            })))
            .await
            .unwrap();
        let json = body_json(resp).await;
        let content = json["choices"][0]["message"]["content"].as_str().unwrap();
        assert!(content.starts_with("Roteiro: "), "{content}");
        assert!(
            content.contains("roteiro model list"),
            "points at the other installed models: {content}"
        );
        assert!(
            !content.contains("max_tokens"),
            "raising a budget that was not the constraint is the wrong advice: {content}"
        );
    }

    /// **The streaming surface answers the same question the same way.** Left
    /// raw, this would have replaced #582's CLI/HTTP split with a
    /// streaming/non-streaming one inside the same endpoint.
    ///
    /// Split across many tokens so the tags straddle boundaries, which is what a
    /// real tokeniser does.
    #[tokio::test]
    async fn a_reasoning_block_does_not_reach_a_streaming_caller() {
        let engine = ReasoningEngine::in_pieces(THINKS_THEN_ANSWERS, 40, FinishReason::Stop);
        let resp = app(engine)
            .oneshot(chat_body(&serde_json::json!({
                "model": "echo",
                "messages": [{"role": "user", "content": "What is 2+2?"}],
                "stream": true,
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains("Okay") && !text.contains("think"),
            "no delta may carry any part of the block: {text}"
        );
        assert_eq!(
            stream_content(&text),
            "four",
            "the answer still arrives, and only the answer: {text}"
        );
        assert!(text.contains("data: [DONE]"), "terminator: {text}");
    }

    /// And a stream that never leaves the block refuses, rather than ending with
    /// the caller having received nothing at all.
    #[tokio::test]
    async fn a_stream_cut_off_inside_the_block_refuses() {
        let engine = ReasoningEngine::in_pieces("<think>\nstill thinking", 8, FinishReason::Length);
        let resp = app(engine)
            .oneshot(chat_body(&serde_json::json!({
                "model": "echo",
                "messages": [{"role": "user", "content": "hi"}],
                "max_tokens": 512,
                "stream": true,
            })))
            .await
            .unwrap();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains("still thinking"),
            "the deliberation was streamed: {text}"
        );
        let content = stream_content(&text);
        assert!(
            content.starts_with("Roteiro: ") && content.contains("512"),
            "the refusal is the whole turn, and the same sentence the \
             non-streaming path gives: {content}"
        );
        assert!(text.contains("data: [DONE]"), "terminator: {text}");
    }

    /// **The loop stops re-sending the block to itself.** The assistant turn fed
    /// back into round two used to carry the whole of round one's reasoning, so
    /// it was re-prefilled once per round against a prompt budget with no prefix
    /// cache (#578). This is the internal half of the multi-turn argument #582
    /// makes about clients echoing history.
    #[tokio::test]
    async fn reasoning_is_not_fed_back_into_the_next_round() {
        struct GraphTool;
        impl crate::tools::ToolRegistry for GraphTool {
            fn tools(&self) -> Vec<crate::tools::ToolDef> {
                vec![crate::tools::ToolDef {
                    name: "lookup".to_owned(),
                    description: "look something up".to_owned(),
                    parameters: serde_json::json!({"type": "object"}),
                }]
            }
            fn call(&self, _n: &str, _a: &serde_json::Value) -> Result<String, String> {
                Ok("the graph said so".to_owned())
            }
        }

        // Round one deliberates and calls a tool; round two answers.
        let engine = ScriptedServeEngine::new(&[
            "<think>\nI should look this up first.\n</think>\n\n\
             <tool_call>{\"name\": \"lookup\", \"arguments\": {}}</tool_call>",
            "<think>\nNow I can answer.\n</think>\n\nthe graph said so",
        ]);
        let resp = app_with_tools(engine.clone(), std::sync::Arc::new(GraphTool))
            .oneshot(chat_body(&serde_json::json!({
                "model": "echo",
                "messages": [{"role": "user", "content": "look it up"}],
            })))
            .await
            .unwrap();
        assert_eq!(
            body_json(resp).await["choices"][0]["message"]["content"],
            "the graph said so"
        );

        let seen = engine.seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "the loop ran two rounds");
        let round_two: String = seen[1].iter().map(|m| m.content.clone()).collect();
        assert!(
            !round_two.contains("I should look this up first"),
            "round one's deliberation was re-sent in round two's prompt: {round_two}"
        );
        assert!(
            round_two.contains("lookup"),
            "the call it actually made is still there: {round_two}"
        );
    }

    #[tokio::test]
    async fn an_ordinary_answer_still_carries_a_string_content_and_no_tool_calls() {
        // The other side of `content: Option<String>`: an ordinary turn is byte-
        // identical to before, and `tool_calls` is omitted rather than `null`.
        let engine = ScriptedServeEngine::new(&["a plain answer"]);
        let resp = app(engine)
            .oneshot(chat_body(&serde_json::json!({
                "model": "echo",
                "messages": [{"role": "user", "content": "hi"}],
            })))
            .await
            .unwrap();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("\"content\":\"a plain answer\""), "{text}");
        assert!(!text.contains("tool_calls"), "omitted, not null: {text}");
    }

    #[tokio::test]
    async fn an_oversized_or_malformed_tools_array_is_a_400() {
        // The bounds and the `type` check have to be visible on the wire as a
        // refusal, not swallowed into a truncated tool set — a client that sent
        // too much must be told, because the alternative is a model calling
        // tools whose schemas the client no longer recognises.
        let cases: Vec<(&str, serde_json::Value)> = vec![
            (
                "over the byte bound",
                serde_json::json!([{
                    "type": "function",
                    "function": {"name": "big", "description": "z".repeat(64 * 1024)},
                }]),
            ),
            (
                "over the count bound",
                serde_json::Value::Array(
                    (0..200)
                        .map(|i| {
                            serde_json::json!({
                                "type": "function",
                                "function": {"name": format!("t{i}")},
                            })
                        })
                        .collect(),
                ),
            ),
            (
                "an unsupported tool type",
                serde_json::json!([{"type": "retrieval", "function": {"name": "lookup"}}]),
            ),
        ];
        for (what, tools) in cases {
            let engine = ScriptedServeEngine::new(&["unreachable"]);
            let resp = app(engine)
                .oneshot(chat_body(&serde_json::json!({
                    "model": "echo",
                    "messages": [{"role": "user", "content": "hi"}],
                    "tools": tools,
                })))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{what}");
            let json = body_json(resp).await;
            assert_eq!(json["error"]["type"], "invalid_request_error", "{what}");
        }
    }

    /// Two spellings of the budget carrying two numbers must reach the client
    /// as this endpoint's own refusal, on the wire.
    ///
    /// The unit tests prove `generation_budget` returns the right `Err`; only
    /// this one proves what the caller receives. It is here because the obvious
    /// implementation — `#[serde(alias = "max_completion_tokens")]` on
    /// `max_tokens` — was measured returning `422 Unprocessable Entity` with
    /// the plain-text body ``Failed to deserialize the JSON body into the
    /// target type: duplicate field `max_tokens` ``: not the `{"error": …}`
    /// envelope, naming the field the caller did not send, and saying what
    /// broke rather than what to do. Reading the alias out of the catch-all is
    /// what buys the status and the envelope asserted below, so it is asserted
    /// rather than assumed.
    #[tokio::test]
    async fn two_different_generation_budgets_are_a_400_in_the_error_envelope() {
        let resp = test_app()
            .oneshot(chat_body(&serde_json::json!({
                "model": "echo",
                "messages": [{"role": "user", "content": "hi"}],
                "max_tokens": 10,
                "max_completion_tokens": 20,
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = body_json(resp).await;
        assert_eq!(json["error"]["type"], "invalid_request_error");
        let message = json["error"]["message"].as_str().expect("a message");
        assert!(message.contains("`max_completion_tokens`"), "{message}");
        assert!(message.contains("`max_tokens`"), "{message}");
        assert!(message.contains("Send one of them"), "{message}");
    }

    /// The other half: OpenAI's current spelling alone is served, not refused
    /// and not a `422`.
    #[tokio::test]
    async fn the_current_spelling_of_the_budget_is_served() {
        let resp = test_app()
            .oneshot(chat_body(&serde_json::json!({
                "model": "echo",
                "messages": [{"role": "user", "content": "hi there"}],
                "max_completion_tokens": 64,
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["choices"][0]["message"]["content"], "HI THERE");
    }

    // ---------------------------------------------------------------------
    // `POST /v1/responses` — the Responses wire (#809).
    // ---------------------------------------------------------------------

    fn responses_body(body: &serde_json::Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    async fn sse_text(resp: axum::response::Response) -> String {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// Every `data:` payload of a Responses stream, paired with its `type`.
    fn responses_events(sse: &str) -> Vec<(String, serde_json::Value)> {
        sse.lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .filter(|l| *l != "[DONE]")
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .map(|v| (v["type"].as_str().unwrap_or_default().to_owned(), v))
            .collect()
    }

    /// Reassemble the answer a Responses client would have displayed —
    /// concatenated from the deltas, for the reason [`stream_content`] gives.
    fn responses_content(sse: &str) -> String {
        responses_events(sse)
            .into_iter()
            .filter(|(t, _)| t == "response.output_text.delta")
            .filter_map(|(_, v)| v["delta"].as_str().map(str::to_owned))
            .collect()
    }

    /// An engine whose only answer is the failure it was built with.
    ///
    /// Two of these are needed and they must differ only in the `EngineError`
    /// variant, because the claim under test is precisely that the variant is
    /// what decides the reported `code`.
    struct FailingEngine(fn() -> EngineError);
    impl Engine for FailingEngine {
        fn models(&self) -> Vec<ModelInfo> {
            vec![ModelInfo {
                id: "echo".to_owned(),
            }]
        }
        fn chat_stream(
            &self,
            _req: &ChatRequest,
            _on_token: &mut dyn FnMut(&str),
        ) -> Result<CompletionStats, EngineError> {
            Err((self.0)())
        }
    }

    /// The `error.code` a Responses stream reports follows the failure's kind —
    /// and a chat template's refusal is the caller's, not the server's (#848).
    ///
    /// A `/v1/responses` request is streaming-only, so the `200` and the first
    /// event are already sent before generation can fail. There is no status
    /// code left to carry the distinction, and this envelope said
    /// `inference_error` for every failure whatsoever — which is what a client
    /// retries on. #848's real turn is exactly this: `qwen3.8-27b`'s own template
    /// refuses a second `system` message, which cannot be fixed by retrying and
    /// can be fixed by reading.
    ///
    /// Both halves asserted together. `invalid_request_error` alone would pass
    /// just as well if the code were hard-wired to the *other* constant, and the
    /// bug being fixed was a hard-wired constant.
    #[tokio::test]
    async fn a_responses_stream_reports_a_refusal_as_the_callers_error() {
        let refused = app(std::sync::Arc::new(FailingEngine(|| {
            EngineError::InvalidRequest("System message must be at the beginning.".to_owned())
        })));
        let resp = refused
            .oneshot(responses_body(&serde_json::json!({
                "model": "echo", "stream": true,
                "input": [{"type": "message", "role": "user",
                           "content": [{"type": "input_text", "text": "hi"}]}],
            })))
            .await
            .unwrap();
        // Still a `200` with a well-formed terminal event: a client waiting for
        // `response.completed` must not be left holding an open stream.
        assert_eq!(resp.status(), StatusCode::OK);
        let sse = sse_text(resp).await;
        let (_, failed) = responses_events(&sse)
            .into_iter()
            .find(|(t, _)| t == "response.failed")
            .expect("a refused turn must still terminate the stream");
        assert_eq!(failed["response"]["error"]["code"], "invalid_request_error");
        assert_eq!(
            failed["response"]["error"]["message"], "System message must be at the beginning.",
            "the refusal's own words must reach the client: {sse}"
        );

        // The other half: a genuine server failure is still the server's.
        let broken = app(std::sync::Arc::new(FailingEngine(|| {
            EngineError::Inference("decode aborted".to_owned())
        })));
        let resp = broken
            .oneshot(responses_body(&serde_json::json!({
                "model": "echo", "stream": true,
                "input": [{"type": "message", "role": "user",
                           "content": [{"type": "input_text", "text": "hi"}]}],
            })))
            .await
            .unwrap();
        let sse = sse_text(resp).await;
        let (_, failed) = responses_events(&sse)
            .into_iter()
            .find(|(t, _)| t == "response.failed")
            .expect("a failed turn must still terminate the stream");
        assert_eq!(failed["response"]["error"]["code"], "inference_error");
    }

    /// The same split on the **chat** streaming wire, which has its own error
    /// frame and had its own hard-wired `inference_error`.
    ///
    /// Two streaming surfaces reach this decision by different routes — one
    /// through `StreamMsg::Failed`, one through `responses::Envelope::failed` —
    /// and that is the split which has already let a rule reach one and not the
    /// other on this server. Asserting it once would leave the other free.
    #[tokio::test]
    async fn a_chat_stream_reports_a_refusal_as_the_callers_error() {
        for (make, expected) in [
            (
                (|| EngineError::InvalidRequest("Unexpected message role.".to_owned()))
                    as fn() -> EngineError,
                "invalid_request_error",
            ),
            (
                || EngineError::Inference("decode aborted".to_owned()),
                "inference_error",
            ),
        ] {
            let resp = app(std::sync::Arc::new(FailingEngine(make)))
                .oneshot(chat_body(&serde_json::json!({
                    "model": "echo",
                    "messages": [{"role": "user", "content": "hi"}],
                    "stream": true,
                })))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            let sse = sse_text(resp).await;
            let frame = sse
                .lines()
                .filter_map(|l| l.strip_prefix("data: "))
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                .find(|v| v.get("error").is_some())
                .expect("a failed stream must carry an error frame");
            assert_eq!(frame["error"]["type"], expected, "{sse}");
        }
    }

    /// **The guard this surface exists to be held by.**
    ///
    /// `/v1` has three content surfaces now — chat non-streaming, chat
    /// streaming, and Responses — and two of them reach the published-content
    /// rules by different routes: the tooled paths inherit them from
    /// `tools::finish`, the untooled streaming path applies them itself in
    /// `stream_untooled`. That split is how #582's `<think>` rule came to be
    /// applied in one place and not the other, and it was closed by *porting*
    /// the rule rather than by removing the split.
    ///
    /// So the property is asserted directly: **one generation, three surfaces,
    /// one answer.** A rule added to `tools::finish` or to `stream_untooled`
    /// that reaches only some of them fails here rather than shipping.
    ///
    /// The fixture is #582's own generation, split across tokens so the tag
    /// boundaries fall inside pieces and the streaming filter is really doing
    /// the work.
    #[tokio::test]
    async fn all_three_surfaces_answer_a_thinking_model_identically() {
        let question = serde_json::json!([{"role": "user", "content": "What is 2+2?"}]);

        // 1. Chat, non-streaming.
        let engine = ReasoningEngine::in_pieces(THINKS_THEN_ANSWERS, 7, FinishReason::Stop);
        let resp = app(engine)
            .oneshot(chat_body(&serde_json::json!({
                "model": "echo", "messages": question,
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let chat_json = body_json(resp).await["choices"][0]["message"]["content"]
            .as_str()
            .expect("assistant content")
            .to_owned();

        // 2. Chat, streaming.
        let engine = ReasoningEngine::in_pieces(THINKS_THEN_ANSWERS, 7, FinishReason::Stop);
        let resp = app(engine)
            .oneshot(chat_body(&serde_json::json!({
                "model": "echo", "messages": question, "stream": true,
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let chat_stream = stream_content(&sse_text(resp).await);

        // 3. Responses.
        let engine = ReasoningEngine::in_pieces(THINKS_THEN_ANSWERS, 7, FinishReason::Stop);
        let resp = app(engine)
            .oneshot(responses_body(&serde_json::json!({
                "model": "echo", "stream": true,
                "input": [{"type": "message", "role": "user",
                           "content": [{"type": "input_text", "text": "What is 2+2?"}]}],
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let sse = sse_text(resp).await;
        let responses = responses_content(&sse);

        assert_eq!(
            chat_json, chat_stream,
            "chat's two surfaces disagree (#582/#589)"
        );
        assert_eq!(
            chat_json, responses,
            "the Responses surface answers differently from the chat surfaces"
        );
        // Not three empty strings agreeing with each other: the answer is the
        // answer, and the deliberation is gone from all three.
        assert_eq!(chat_json, "four");
        assert!(
            !sse.contains("Okay, the user"),
            "the reasoning block reached the Responses wire: {sse}"
        );

        // The same text also has to be what `response.completed` carries, or a
        // client that reads the final object rather than the deltas sees
        // something else again.
        let completed = responses_events(&sse)
            .into_iter()
            .find(|(t, _)| t == "response.completed")
            .expect("a `response.completed` event")
            .1;
        assert_eq!(
            completed["response"]["output"][0]["content"][0]["text"], "four",
            "the final response object disagrees with its own deltas"
        );
    }

    /// The #583 rule — a generation that never leaves its `<think>` block is a
    /// refusal, not a short answer — on the third surface too.
    #[tokio::test]
    async fn a_responses_turn_that_never_leaves_its_think_block_is_refused() {
        let engine = ReasoningEngine::in_pieces("<think>still deciding", 4, FinishReason::Length);
        let resp = app(engine)
            .oneshot(responses_body(&serde_json::json!({
                "model": "echo", "stream": true, "input": "What is 2+2?",
            })))
            .await
            .unwrap();
        let sse = sse_text(resp).await;
        let answer = responses_content(&sse);
        assert!(answer.starts_with("Roteiro: "), "a refusal: {answer}");
        assert!(answer.contains("max_tokens"), "names the budget: {answer}");
        assert!(
            !answer.contains("still deciding"),
            "the deliberation is not returned as a consolation: {answer}"
        );
    }

    #[tokio::test]
    async fn responses_streams_an_untooled_answer_as_a_message_item() {
        let resp = test_app()
            .oneshot(responses_body(&serde_json::json!({
                "model": "echo", "stream": true, "input": "hi there",
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let sse = sse_text(resp).await;
        let kinds: Vec<String> = responses_events(&sse).into_iter().map(|(t, _)| t).collect();
        assert_eq!(
            kinds,
            vec![
                "response.created",
                "response.output_item.added",
                "response.content_part.added",
                "response.output_text.delta",
                "response.output_text.delta",
                "response.output_text.done",
                "response.content_part.done",
                "response.output_item.done",
                "response.completed",
            ],
            "{sse}"
        );
        // Two deltas for two words: an untooled Responses turn is genuinely
        // token-incremental, unlike the tooled one.
        assert_eq!(responses_content(&sse), "HI THERE");
        assert!(sse.contains("event: response.completed"), "{sse}");
        assert!(sse.contains("data: [DONE]"), "terminator: {sse}");

        let completed = responses_events(&sse)
            .into_iter()
            .find(|(t, _)| t == "response.completed")
            .expect("a `response.completed` event")
            .1;
        assert_eq!(completed["response"]["status"], "completed");
        assert_eq!(completed["response"]["store"], false);
        assert_eq!(completed["response"]["usage"]["output_tokens"], 2);
        assert_eq!(completed["response"]["output"][0]["type"], "message");
        assert_eq!(
            completed["response"]["output"][0]["content"][0]["text"],
            "HI THERE"
        );
        // Sequence numbers are what a client orders by; they must not repeat.
        let seqs: Vec<u64> = responses_events(&sse)
            .into_iter()
            .filter_map(|(_, v)| v["sequence_number"].as_u64())
            .collect();
        assert_eq!(seqs, (0..seqs.len() as u64).collect::<Vec<_>>(), "{sse}");
    }

    /// A call against the **client's** tool comes back as a `function_call`
    /// item with the `call_id` the client will echo as `function_call_output`.
    #[tokio::test]
    async fn responses_returns_a_client_tool_call_as_a_function_call_item() {
        let engine = ReasoningEngine::new(WEATHER_CALL, FinishReason::Stop);
        let resp = app(engine)
            .oneshot(responses_body(&serde_json::json!({
                "model": "echo", "stream": true, "input": "weather in Berlin?",
                "tools": [{
                    "type": "function", "name": "get_weather",
                    "description": "current weather",
                    "parameters": {"type": "object",
                                   "properties": {"city": {"type": "string"}}},
                }],
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let sse = sse_text(resp).await;
        let events = responses_events(&sse);
        let kinds: Vec<&str> = events.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(
            kinds,
            vec![
                "response.created",
                "response.output_item.added",
                "response.function_call_arguments.delta",
                "response.function_call_arguments.done",
                "response.output_item.done",
                "response.completed",
            ],
            "{sse}"
        );
        let item = &events
            .iter()
            .find(|(t, _)| t == "response.output_item.done")
            .expect("a done item")
            .1["item"];
        assert_eq!(item["type"], "function_call");
        assert_eq!(item["name"], "get_weather");
        assert_eq!(item["arguments"], "{\"city\":\"Berlin\"}");
        let call_id = item["call_id"].as_str().expect("a call_id");
        assert!(!call_id.is_empty(), "a call_id the client can echo back");
        // The raw `<tool_call>` markup is never published as text (#489), on
        // this wire as on the others.
        assert!(!sse.contains("<tool_call>"), "markup leaked: {sse}");
        assert!(responses_content(&sse).is_empty(), "no prose beside a call");
    }

    /// The turn after the one above: the client's result comes back as a
    /// `function_call_output` item, and the model sees the transcript it wrote.
    #[tokio::test]
    async fn a_responses_tool_result_reaches_the_model_as_a_tool_response_turn() {
        let engine = ReasoningEngine::new("It is 20C.", FinishReason::Stop);
        let served: std::sync::Arc<dyn Engine> = std::sync::Arc::clone(&engine) as _;
        let resp = app(served)
            .oneshot(responses_body(&serde_json::json!({
                "model": "echo", "stream": true,
                "input": [
                    {"type": "message", "role": "user", "content": "weather in Berlin?"},
                    {"type": "function_call", "call_id": "call_9", "name": "get_weather",
                     "arguments": "{\"city\":\"Berlin\"}"},
                    {"type": "function_call_output", "call_id": "call_9", "output": "20C"},
                ],
                "tools": [{"type": "function", "name": "get_weather",
                           "parameters": {"type": "object"}}],
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        // Drain the stream first: it closes when the blocking worker drops its
        // sender, so consuming it is what makes the generation have happened.
        let sse = sse_text(resp).await;
        assert!(sse.contains("response.completed"), "{sse}");
        let seen = engine.seen.lock().unwrap();
        let turns: Vec<(String, String)> = seen
            .last()
            .expect("one generation")
            .iter()
            .map(|m| (m.role.clone(), m.content.clone()))
            .collect();
        // The system turn the tool loop injects comes first; after it, the
        // conversation the client sent, rendered in-band.
        assert!(
            turns
                .iter()
                .any(|(role, content)| role == "assistant" && content == WEATHER_CALL),
            "the assistant's own call is replayed verbatim: {turns:?}"
        );
        assert!(
            turns
                .iter()
                .any(|(role, content)| role == "user"
                    && content == "<tool_response>20C</tool_response>"),
            "the client's result reaches the model as a `<tool_response>` user turn: {turns:?}"
        );
    }

    /// A generation the token budget cut short is **not** a completed one.
    ///
    /// The chat wire says so with `finish_reason: "length"`; this wire says so
    /// with `response.incomplete` and an `incomplete_details.reason`. Reporting
    /// `response.completed` would hand a client a truncated answer with nothing
    /// on the wire to say it was truncated — the silent contradiction every
    /// other refusal on this surface exists to prevent.
    #[tokio::test]
    async fn a_responses_turn_cut_at_the_token_cap_is_incomplete_not_completed() {
        let engine = ReasoningEngine::in_pieces("a long answer that ran", 4, FinishReason::Length);
        let resp = app(engine)
            .oneshot(responses_body(&serde_json::json!({
                "model": "echo", "stream": true, "input": "go on",
                "max_output_tokens": 4,
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let sse = sse_text(resp).await;
        let events = responses_events(&sse);
        let kinds: Vec<&str> = events.iter().map(|(t, _)| t.as_str()).collect();
        assert!(
            kinds.contains(&"response.incomplete"),
            "the terminal event names the truncation: {sse}"
        );
        assert!(
            !kinds.contains(&"response.completed"),
            "a truncated generation must not also claim to have completed: {sse}"
        );
        let terminal = &events
            .iter()
            .find(|(t, _)| t == "response.incomplete")
            .expect("a terminal event")
            .1["response"];
        assert_eq!(terminal["status"], "incomplete");
        assert_eq!(
            terminal["incomplete_details"]["reason"],
            "max_output_tokens"
        );
        // The item carries the same verdict, not a contradicting one.
        assert_eq!(terminal["output"][0]["status"], "incomplete");
        // And the text produced so far is still delivered — truncated, not lost.
        assert_eq!(responses_content(&sse), "a long answer that ran");
    }

    /// **A `tools` array of only hosted tools is still a `tools` array.**
    ///
    /// The published rule is that sending `tools` puts this endpoint in general
    /// mode and the graph tools are *not* injected. Hosted entries
    /// (`web_search`, which `codex-cli` sends on every turn) are dropped by the
    /// adapter, so keying the mode on the surviving client list put such a
    /// request back into Ask mode — graph tools advertised and executed for a
    /// client that asked for the opposite, plus the ~3,100 tokens of schemas
    /// that behaviour exists to spare it. Raised in review of #825.
    #[tokio::test]
    async fn a_hosted_only_tools_array_does_not_turn_the_graph_tools_back_on() {
        let engine = ScriptedServeEngine::new(&["I cannot search the web."]);
        let router = app_with_tools(engine.clone(), std::sync::Arc::new(PanicRegistry));
        let resp = router
            .oneshot(responses_body(&serde_json::json!({
                "model": "echo", "stream": true, "input": "what is the news?",
                "tools": [{"type": "web_search", "external_web_access": true}],
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let sse = sse_text(resp).await;
        assert!(sse.contains("response.completed"), "{sse}");

        let prompt = engine.first_system_prompt();
        assert!(
            !prompt.contains("graph_only_tool"),
            "a client that sent `tools` does not also get Roteiro's, even when \
             every tool it sent was one this endpoint drops: {prompt}"
        );
    }

    /// The Ask path must keep working: no `tools` key at all still gets the
    /// graph tools. Without this the fix above could be "suppress always", which
    /// would pass the test above and delete Ask mode from this wire.
    #[tokio::test]
    async fn a_responses_request_with_no_tools_still_gets_the_graph_tools() {
        struct GraphOnly;
        impl crate::tools::ToolRegistry for GraphOnly {
            fn tools(&self) -> Vec<crate::tools::ToolDef> {
                vec![crate::tools::ToolDef {
                    name: "graph_only_tool".to_owned(),
                    description: "a graph tool".to_owned(),
                    parameters: serde_json::json!({"type": "object"}),
                }]
            }
            fn call(&self, _n: &str, _a: &serde_json::Value) -> Result<String, String> {
                Ok("the graph said so".to_owned())
            }
        }
        let engine = ScriptedServeEngine::new(&[
            "<tool_call>{\"name\":\"graph_only_tool\",\"arguments\":{}}</tool_call>",
            "the graph said so",
        ]);
        let router = app_with_tools(engine.clone(), std::sync::Arc::new(GraphOnly));
        let resp = router
            .oneshot(responses_body(&serde_json::json!({
                "model": "echo", "stream": true, "input": "why?",
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let sse = sse_text(resp).await;
        assert_eq!(responses_content(&sse), "the graph said so");
        assert!(
            engine.first_system_prompt().contains("graph_only_tool"),
            "Ask mode on this wire still advertises the graph tools"
        );
    }

    /// A client tool call is **complete by construction**, whatever the engine
    /// reported: the loop returns calls only through `Ending::ClientCalls`, and
    /// a call cut by the token cap becomes `Ending::Unfinished` and is refused
    /// instead. So this turn must terminate on `response.completed` — the same
    /// override the chat wire makes when it reports `finish_reason: "tool_calls"`
    /// over `length`. Reporting `response.incomplete` would tell the client its
    /// call was truncated and stop it running one it could have run.
    #[tokio::test]
    async fn a_client_tool_call_completes_even_when_the_engine_reports_length() {
        let engine = ReasoningEngine::new(WEATHER_CALL, FinishReason::Length);
        let resp = app(engine)
            .oneshot(responses_body(&serde_json::json!({
                "model": "echo", "stream": true, "input": "weather in Berlin?",
                "max_output_tokens": 8,
                "tools": [{"type": "function", "name": "get_weather",
                           "parameters": {"type": "object"}}],
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let sse = sse_text(resp).await;
        let events = responses_events(&sse);
        let kinds: Vec<&str> = events.iter().map(|(t, _)| t.as_str()).collect();
        assert!(kinds.contains(&"response.completed"), "{sse}");
        assert!(
            !kinds.contains(&"response.incomplete"),
            "an intact call must not be reported as truncated: {sse}"
        );
        let item = &events
            .iter()
            .find(|(t, _)| t == "response.output_item.done")
            .expect("a done item")
            .1["item"];
        assert_eq!(item["type"], "function_call");
        assert_eq!(item["status"], "completed");
        assert_eq!(item["name"], "get_weather");
    }

    /// **The declaration that stands in for a refusal on `store`.**
    ///
    /// A request that omits `store` is not refused (see `docs/SERVING.md` for
    /// why, and for why the default OpenAI applies could not be verified
    /// offline), so the response itself has to say what happened — in the field
    /// a client reads to find out, on *every* lifecycle event rather than only
    /// the terminal one. A client that reads `response.created` and stops must
    /// learn the same thing as one that waits.
    #[tokio::test]
    async fn every_lifecycle_event_says_nothing_was_stored() {
        // `store` omitted entirely — the case the declaration exists for.
        let resp = test_app()
            .oneshot(responses_body(&serde_json::json!({
                "model": "echo", "stream": true, "input": "hi there",
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let sse = sse_text(resp).await;
        let carriers: Vec<(String, serde_json::Value)> = responses_events(&sse)
            .into_iter()
            .filter(|(_, v)| v.get("response").is_some())
            .collect();
        assert_eq!(
            carriers.len(),
            2,
            "created and completed both carry a `response` object: {sse}"
        );
        for (kind, event) in &carriers {
            assert_eq!(
                event["response"]["store"], false,
                "`{kind}` must say nothing was stored: {sse}"
            );
        }

        // And on the truncated path, whose terminal event is a different one.
        let engine = ReasoningEngine::in_pieces("cut off here", 3, FinishReason::Length);
        let resp = app(engine)
            .oneshot(responses_body(&serde_json::json!({
                "model": "echo", "stream": true, "input": "go on",
                "max_output_tokens": 2,
            })))
            .await
            .unwrap();
        let sse = sse_text(resp).await;
        let incomplete = responses_events(&sse)
            .into_iter()
            .find(|(t, _)| t == "response.incomplete")
            .expect("a `response.incomplete` event")
            .1;
        assert_eq!(incomplete["response"]["store"], false, "{sse}");
    }

    #[tokio::test]
    async fn responses_refuses_a_non_streaming_request_by_name() {
        let resp = test_app()
            .oneshot(responses_body(&serde_json::json!({
                "model": "echo", "input": "hi",
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let msg = body_json(resp).await["error"]["message"]
            .as_str()
            .expect("an error message")
            .to_owned();
        assert!(msg.starts_with("`stream` must be `true`"), "{msg}");
    }

    #[tokio::test]
    async fn responses_refuses_an_unsupported_input_item_by_name() {
        let resp = test_app()
            .oneshot(responses_body(&serde_json::json!({
                "model": "echo", "stream": true,
                "input": [{"type": "reasoning", "id": "rs_1", "summary": []}],
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let msg = body_json(resp).await["error"]["message"]
            .as_str()
            .expect("an error message")
            .to_owned();
        assert!(msg.contains("`reasoning`"), "names the item type: {msg}");
    }

    /// An unknown model is a `404` before a byte of stream, exactly as it is on
    /// the chat wire — not a `200` SSE that fails half-way.
    #[tokio::test]
    async fn responses_unknown_model_is_404_not_a_stream() {
        let resp = test_app()
            .oneshot(responses_body(&serde_json::json!({
                "model": "nope", "stream": true, "input": "hi",
            })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            body_json(resp).await["error"]["type"],
            "invalid_request_error"
        );
    }

    /// Scope parity is deferred, and deferred means **absent**, not
    /// half-present: a client that points at a scoped Responses URL is told so
    /// by the router rather than being quietly served the unscoped answer.
    #[tokio::test]
    async fn the_scoped_responses_routes_are_not_served() {
        for uri in ["/v1/demo/responses", "/v1/workspaces/ws/responses"] {
            let resp = test_app()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(uri)
                        .header("content-type", "application/json")
                        .body(Body::from(
                            "{\"model\":\"echo\",\"stream\":true,\"input\":\"hi\"}",
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{uri}");
        }
    }
}

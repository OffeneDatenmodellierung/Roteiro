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

use crate::engine::{ChatRequest, Engine, EngineError};
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
const MAX_TOOL_ROUNDS: usize = 4;

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
}
type Shared = Arc<AppState>;

/// Build the `/v1` router over `engine`, with no tools.
pub fn app(engine: Arc<dyn Engine>) -> Router {
    router(Arc::new(AppState {
        engine,
        tools: None,
        workspaces: std::collections::HashMap::new(),
    }))
}

/// Build the `/v1` router over `engine` with `tools` auto-registered — the model
/// may call them to query the graph while answering (ADR-0006).
pub fn app_with_tools(engine: Arc<dyn Engine>, tools: Arc<dyn ToolRegistry>) -> Router {
    router(Arc::new(AppState {
        engine,
        tools: Some(tools),
        workspaces: std::collections::HashMap::new(),
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
    let normalised = match body.normalise() {
        Ok(n) => n,
        Err(msg) => return error(StatusCode::BAD_REQUEST, msg, "invalid_request_error"),
    };
    // Destructured field-by-field on purpose: `tool_choice` and
    // `parallel_tool_calls` are parsed and carried but deliberately NOT enforced
    // (see their field docs and the crate README's divergence table), and naming
    // every field here means a future one cannot be added and silently dropped —
    // which is the #488 defect this endpoint is trying to stop repeating.
    let crate::types::NormalisedChat {
        request: req,
        client_tools,
        tool_choice: _,
        parallel_tool_calls: _,
    } = normalised;
    // Validate the model up front so the streaming and non-streaming paths agree:
    // an unknown model is a 404 either way, not a 200 SSE that fails mid-stream.
    if !state.engine.models().iter().any(|m| m.id == req.model) {
        return error(
            StatusCode::NOT_FOUND,
            EngineError::UnknownModel(req.model).to_string(),
            "invalid_request_error",
        );
    }
    if stream {
        stream_chat(state, req, scope, client_tools)
    } else {
        chat_json(state, req, scope, client_tools).await
    }
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
    /// Generation failed part-way; carry the message for a final error event.
    Failed(String),
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
            // `tools::finish`. Resolving the loop first is also what makes that
            // possible here at all — a token-incremental stream has already sent
            // the first half of a call before anything can judge it. Which is
            // why the else-branch below, where no tools are advertised and the
            // loop is not used, streams raw: nothing there injected the tool
            // prompt, so nothing there assigned `<tool_call>` a meaning.
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
                    let _ = tx.send(StreamMsg::Failed(e.to_string()));
                }
            }
        } else {
            let mut on_token = |piece: &str| {
                let _ = tx.send(StreamMsg::Delta(piece.to_owned()));
            };
            match state.engine.chat_stream(&req, &mut on_token) {
                Ok(usage) => {
                    let _ = tx.send(StreamMsg::Done(usage.finish_reason.as_str()));
                }
                Err(e) => {
                    let _ = tx.send(StreamMsg::Failed(e.to_string()));
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
            StreamMsg::Failed(message) => {
                serde_json::to_string(&ErrorResponse::new(message, "inference_error"))
                    .unwrap_or_else(|_| "{\"error\":{\"message\":\"stream failed\"}}".to_owned())
            }
        };
        Ok::<Event, std::convert::Infallible>(Event::default().data(data))
    });
    // OpenAI terminates the stream with a literal `data: [DONE]`.
    let done = tokio_stream::once(Ok(Event::default().data("[DONE]")));
    Sse::new(events.chain(done)).into_response()
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
/// Neither branch can publish tool-call markup as the assistant's answer (#489),
/// and between them they cover the outcome:
///
/// * With client calls, this reports `finish_reason: "tool_calls"` and
///   `content: null`, so the raw markup in `completion.content` is not rendered.
/// * Without them, `content` is whatever `tools::finish` passed — and that is
///   the one place a generation is declared to be the user's answer, so it
///   carries no markup to publish.
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
}

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

use crate::engine::{ChatRequest, Completion, Engine, EngineError, FinishReason};
use crate::tools::{ToolRegistry, chat_with_tools};
use crate::types::{
    ChatChoice, ChatCompletionChunk, ChatCompletionRequest, ChatCompletionResponse, ChatMessageDto,
    ChunkChoice, Delta, EmbeddingObject, EmbeddingRequest, EmbeddingResponse, ErrorResponse,
    ModelList, ModelObject, Usage,
};

/// How many tool round-trips a single request may take before the model's last
/// output is returned regardless (ADR-0006 server-side execute-and-loop).
const MAX_TOOL_ROUNDS: usize = 4;

/// Shared handler state: the inference engine and, optionally, the graph tools
/// the served model may call.
struct AppState {
    engine: Arc<dyn Engine>,
    tools: Option<Arc<dyn ToolRegistry>>,
}
type Shared = Arc<AppState>;

/// Build the `/v1` router over `engine`, with no tools.
pub fn app(engine: Arc<dyn Engine>) -> Router {
    router(Arc::new(AppState {
        engine,
        tools: None,
    }))
}

/// Build the `/v1` router over `engine` with `tools` auto-registered — the model
/// may call them to query the graph while answering (ADR-0006).
pub fn app_with_tools(engine: Arc<dyn Engine>, tools: Arc<dyn ToolRegistry>) -> Router {
    router(Arc::new(AppState {
        engine,
        tools: Some(tools),
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
        .with_state(state)
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

/// `POST /v1/chat/completions` — a full JSON completion, or an SSE stream of
/// `chat.completion.chunk` events when `stream: true`.
async fn chat_completions(
    State(state): State<Shared>,
    Json(body): Json<ChatCompletionRequest>,
) -> Response {
    run_chat(state, body, None).await
}

/// `POST /v1/{project}/chat/completions` — as [`chat_completions`], but the
/// served model's tool calls are pre-bound to `project` (ADR-0008).
async fn chat_completions_scoped(
    State(state): State<Shared>,
    axum::extract::Path(project): axum::extract::Path<String>,
    Json(body): Json<ChatCompletionRequest>,
) -> Response {
    run_chat(state, body, Some(project)).await
}

/// Shared chat entry point: validate, then dispatch to the streaming or JSON
/// path, carrying an optional project scope for the tool loop.
async fn run_chat(state: Shared, body: ChatCompletionRequest, project: Option<String>) -> Response {
    let stream = body.stream == Some(true);
    let req = match body.into_engine_request() {
        Ok(req) => req,
        Err(msg) => return error(StatusCode::BAD_REQUEST, msg, "invalid_request_error"),
    };
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
        stream_chat(state, req, project)
    } else {
        chat_json(state, req, project).await
    }
}

/// Non-streaming path: run one blocking completion on a worker thread and return
/// a single JSON body. With tools registered, the model may call them first.
async fn chat_json(state: Shared, req: ChatRequest, project: Option<String>) -> Response {
    let model = req.model.clone();
    // Inference blocks (llama.cpp decode loop); keep it off the async runtime.
    let result =
        tokio::task::spawn_blocking(move || complete(&state, &req, project.as_deref())).await;

    match result {
        Ok(Ok(completion)) => Json(build_response(&model, &completion)).into_response(),
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
/// (ADR-0006) when tools are present, otherwise a plain generation. When
/// `project` is set (a `/v1/{project}/…` request), the tool calls are scoped to
/// it (ADR-0008). Blocking.
fn complete(
    state: &AppState,
    req: &ChatRequest,
    project: Option<&str>,
) -> Result<Completion, EngineError> {
    let Some(tools) = &state.tools else {
        return state.engine.chat(req);
    };
    match project {
        Some(project) => {
            let scoped = ScopedTools {
                inner: tools.as_ref(),
                project: project.to_owned(),
            };
            chat_with_tools(state.engine.as_ref(), &scoped, req, MAX_TOOL_ROUNDS)
        }
        None => chat_with_tools(state.engine.as_ref(), tools.as_ref(), req, MAX_TOOL_ROUNDS),
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
    /// Generation finished cleanly with this reason.
    Done(FinishReason),
    /// Generation failed part-way; carry the message for a final error event.
    Failed(String),
}

/// Streaming path: run generation on a blocking worker that feeds token deltas
/// over a channel, and surface them as OpenAI `chat.completion.chunk` SSE events
/// terminated by `data: [DONE]`.
fn stream_chat(state: Shared, req: ChatRequest, project: Option<String>) -> Response {
    let id = format!("chatcmpl-{}", next_id());
    let created = unix_seconds();
    let model = req.model.clone();

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<StreamMsg>();
    tokio::task::spawn_blocking(move || {
        // A dropped receiver (client disconnected) just makes sends fail — the
        // generation loop then runs to completion harmlessly.
        let _ = tx.send(StreamMsg::Role);
        // Only take the (non-incremental) tool path when the registry actually
        // advertises tools — an empty registry falls back to `engine.chat` in
        // `complete`, so match that here and keep token-by-token streaming.
        let use_tools = state.tools.as_ref().is_some_and(|t| !t.tools().is_empty());
        if use_tools {
            // The tool loop runs multiple generations, so it is resolved fully
            // and then the final answer is emitted as one delta (tool-mode
            // streaming is not token-incremental).
            match complete(&state, &req, project.as_deref()) {
                Ok(completion) => {
                    let _ = tx.send(StreamMsg::Delta(completion.content));
                    let _ = tx.send(StreamMsg::Done(completion.finish_reason));
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
                    let _ = tx.send(StreamMsg::Done(usage.finish_reason));
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
            StreamMsg::Done(reason) => chunk_json(
                &id,
                created,
                &model,
                Delta::default(),
                Some(reason.as_str()),
            ),
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
    }
}

/// A content-piece delta.
fn content_delta(text: String) -> Delta {
    Delta {
        role: None,
        content: Some(text),
    }
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

/// Assemble the OpenAI response body from an engine [`Completion`].
fn build_response(model: &str, completion: &crate::engine::Completion) -> ChatCompletionResponse {
    ChatCompletionResponse {
        id: format!("chatcmpl-{}", next_id()),
        object: "chat.completion",
        created: unix_seconds(),
        model: model.to_owned(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessageDto {
                role: "assistant".to_owned(),
                content: completion.content.clone(),
            },
            finish_reason: completion.finish_reason.as_str(),
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
    use super::app;
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
}

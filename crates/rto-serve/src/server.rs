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
fn router(state: Shared) -> Router {
    Router::new()
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/embeddings", post(embeddings))
        .with_state(state)
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

/// `POST /v1/chat/completions` — a full JSON completion, or an SSE stream of
/// `chat.completion.chunk` events when `stream: true`.
async fn chat_completions(
    State(state): State<Shared>,
    Json(body): Json<ChatCompletionRequest>,
) -> Response {
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
        stream_chat(state, req)
    } else {
        chat_json(state, req).await
    }
}

/// Non-streaming path: run one blocking completion on a worker thread and return
/// a single JSON body. With tools registered, the model may call them first.
async fn chat_json(state: Shared, req: ChatRequest) -> Response {
    let model = req.model.clone();
    // Inference blocks (llama.cpp decode loop); keep it off the async runtime.
    let result = tokio::task::spawn_blocking(move || complete(&state, &req)).await;

    match result {
        Ok(Ok(completion)) => Json(build_response(&model, &completion)).into_response(),
        Ok(Err(EngineError::UnknownModel(m))) => error(
            StatusCode::NOT_FOUND,
            EngineError::UnknownModel(m).to_string(),
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
/// (ADR-0006) when tools are present, otherwise a plain generation. Blocking.
fn complete(state: &AppState, req: &ChatRequest) -> Result<Completion, EngineError> {
    match &state.tools {
        Some(tools) => chat_with_tools(state.engine.as_ref(), tools.as_ref(), req, MAX_TOOL_ROUNDS),
        None => state.engine.chat(req),
    }
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
fn stream_chat(state: Shared, req: ChatRequest) -> Response {
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
            match complete(&state, &req) {
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
}

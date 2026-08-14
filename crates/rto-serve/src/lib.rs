//! Roteiro local model serving (ADR-0006): an opt-in, loopback,
//! OpenAI-compatible `/v1` endpoint over the models a user has already pulled,
//! so other tools (an Omnigent agent, an editor) can call them offline with no
//! second download.
//!
//! The crate is split so the HTTP surface is testable without a C++ build: the
//! pure-Rust [`server`] is written against the [`engine::Engine`] trait, and the
//! real llama.cpp-backed [`llama::LlamaEngine`] lives behind the `llama` feature.
//! Graph-tool auto-registration and `/v1/embeddings` land in later PRs.

pub mod server;
pub mod tools;
pub mod types;

// The inference core now lives in `rto-llama`; re-export it so `rto_serve::engine`
// / `rto_serve::llama` (and the engine types) keep resolving for this crate's
// modules and existing callers.
pub use rto_llama::engine;
#[cfg(feature = "llama")]
pub use rto_llama::llama;

pub use rto_llama::{
    ChatRequest, Completion, CompletionStats, Engine, EngineError, FinishReason, Message, ModelInfo,
};
pub use server::{
    app, app_with_tools, app_with_workspace_tools, serve_blocking, serve_blocking_router,
    serve_blocking_with_tools,
};
#[cfg(feature = "tls")]
pub use server::{serve_blocking_router_tls, serve_blocking_tls};
pub use tools::{ToolDef, ToolRegistry, chat_with_tools};

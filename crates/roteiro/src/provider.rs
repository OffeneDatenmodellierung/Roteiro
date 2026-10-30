//! The external model provider — models run in a server the user already has
//! ([[docs/adr/0027-external-model-provider.md]]).
//!
//! [`ProviderEngine`] implements [`rto_llama::Engine`] over HTTP against any
//! OpenAI-compatible server: Ollama, `llama-server`, LM Studio. Everything that
//! already talks to a model through that trait — the `/v1` tool loop, the Ask
//! panel — runs against it unchanged.
//!
//! # Loopback only
//!
//! A provider on another machine is egress, and ADR-0019 decides what egress
//! requires. Until that gate is wired here, [`ProviderEngine::new`] refuses any
//! base URL whose host is not this machine, so `[provider] base_url` in a
//! committed `roteiro.toml` cannot send repository content anywhere
//! (ADR-0027 §1).
//!
//! # Tools stay in the prompt
//!
//! [`Engine::carries_tools`] keeps its default `false`, so the tool loop
//! advertises tools in a system turn and parses `<tool_call>` text exactly as it
//! does for the in-process engine. Sending OpenAI `tools` instead would make the
//! protocol depend on whether a given server version parses a given model's
//! tool-call dialect (ADR-0027 §3).
//!
//! # No probe
//!
//! Construction opens no socket. Whether the server answers is discovered by the
//! first call, and a failure names the URL.

use base64::Engine as _;
use rto_llama::{ChatRequest, CompletionStats, Engine, EngineError, FinishReason, ModelInfo};

/// The environment variable holding the provider's credential, if it needs one.
///
/// Not a config key, for the reason `[remote]`'s credential is not one:
/// `roteiro.toml` is committed. A local Ollama or `llama-server` wants none.
pub const API_KEY_ENV: &str = "ROTEIRO_PROVIDER_API_KEY";

/// How long a connection may take to open. Short, because the server is on this
/// machine: a slow connect means it is not running.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// How long one call may take end to end. Generous, because a provider may load
/// a model from disk before answering, and generation is bounded by
/// `max_tokens` anyway.
const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// An [`Engine`] backed by an OpenAI-compatible server on this machine.
pub struct ProviderEngine {
    base: String,
    models: Vec<String>,
    agent: ureq::Agent,
}

impl ProviderEngine {
    /// An engine over `base_url` (ending in `/v1`) offering `models`, the
    /// provider model ids `GET /v1/models` lists — taken from config rather than
    /// asked of the server, so building one opens no socket.
    ///
    /// # Errors
    /// If `base_url` is not an absolute `http(s)` URL naming this machine.
    pub fn new(base_url: &str, models: Vec<String>) -> anyhow::Result<Self> {
        let base =
            loopback_base(base_url).map_err(|e| anyhow::anyhow!("`[provider] base_url`: {e}"))?;
        let agent = ureq::config::Config::builder()
            // A redirect could move the call off this machine after the
            // loopback check passed.
            .max_redirects(0)
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_global(Some(CALL_TIMEOUT))
            .http_status_as_error(false)
            .build()
            .into();
        Ok(Self {
            base,
            models,
            agent,
        })
    }

    /// POST `body` to `path` under the base URL, returning the response once its
    /// status is known to be a success.
    fn post(
        &self,
        path: &str,
        model: &str,
        body: &serde_json::Value,
    ) -> Result<ureq::http::Response<ureq::Body>, EngineError> {
        let url = format!("{}{path}", self.base);
        let mut request = self
            .agent
            .post(&url)
            .header("content-type", "application/json");
        if let Some(key) = api_key() {
            request = request.header("authorization", format!("Bearer {key}"));
        }
        let mut response = request.send(body.to_string()).map_err(|e| {
            EngineError::Inference(format!("the provider at {url} did not answer: {e}"))
        })?;
        let status = response.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(response);
        }
        let said = response
            .body_mut()
            .with_config()
            .limit(64 * 1024)
            .read_to_string()
            .unwrap_or_default();
        // Ollama and llama-server both answer an unknown model with 404.
        if status == 404 {
            return Err(EngineError::UnknownModel(model.to_owned()));
        }
        Err(EngineError::Inference(format!(
            "the provider at {url} answered {status}: {}",
            said.trim()
        )))
    }
}

impl Engine for ProviderEngine {
    fn models(&self) -> Vec<ModelInfo> {
        self.models
            .iter()
            .map(|id| ModelInfo { id: id.clone() })
            .collect()
    }

    fn chat_stream(
        &self,
        req: &ChatRequest,
        on_token: &mut dyn FnMut(&str),
    ) -> Result<CompletionStats, EngineError> {
        let response = self.post("/chat/completions", &req.model, &chat_body(req))?;
        let reader = std::io::BufReader::new(response.into_body().into_reader());
        read_stream(reader, on_token)
    }

    fn embed(&self, model: &str, inputs: &[String]) -> Result<Vec<Vec<f32>>, EngineError> {
        let body = serde_json::json!({ "model": model, "input": inputs });
        let text = self
            .post("/embeddings", model, &body)?
            .body_mut()
            .with_config()
            .limit(64 * 1024 * 1024)
            .read_to_string()
            .map_err(|e| EngineError::Inference(format!("reading embeddings: {e}")))?;
        parse_embeddings(&text, inputs.len())
    }
}

/// The credential, if one is set and non-blank.
fn api_key() -> Option<String> {
    std::env::var(API_KEY_ENV)
        .ok()
        .map(|key| key.trim().to_owned())
        .filter(|key| !key.is_empty())
}

/// `url` without a trailing `/`, if it is an absolute `http(s)` URL whose host is
/// this machine.
fn loopback_base(url: &str) -> Result<String, String> {
    let trimmed = url.trim().trim_end_matches('/');
    let uri: ureq::http::Uri = trimmed
        .parse()
        .map_err(|e| format!("`{url}` is not a URL: {e}"))?;
    if !matches!(uri.scheme_str(), Some("http" | "https")) {
        return Err(format!("`{url}` must start with http:// or https://"));
    }
    let host = uri.host().unwrap_or_default();
    let ip_host = host.trim_start_matches('[').trim_end_matches(']');
    let loopback = host.eq_ignore_ascii_case("localhost")
        || ip_host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    if !loopback {
        return Err(format!(
            "`{url}` is not on this machine. A provider elsewhere is egress, which \
             ADR-0027 §1 refuses until it is routed through the remote tier's consent \
             gate; use a loopback address such as http://127.0.0.1:11434/v1"
        ));
    }
    Ok(trimmed.to_owned())
}

/// The `/v1/chat/completions` body for `req`, streaming.
///
/// Images and audio ride on the **last user turn**, as OpenAI content parts,
/// because that is the turn the in-process engine attached them to. `tools` is
/// deliberately absent: see the module docs.
fn chat_body(req: &ChatRequest) -> serde_json::Value {
    let last_user = req.messages.iter().rposition(|m| m.role == "user");
    let messages: Vec<serde_json::Value> = req
        .messages
        .iter()
        .enumerate()
        .map(|(i, m)| {
            if Some(i) != last_user || (req.images.is_empty() && req.audio.is_empty()) {
                return serde_json::json!({ "role": m.role, "content": m.content });
            }
            let mut parts = vec![serde_json::json!({ "type": "text", "text": m.content })];
            for image in &req.images {
                let url = format!("data:{};base64,{}", image_mime(image), b64(image));
                parts.push(serde_json::json!({ "type": "image_url", "image_url": { "url": url } }));
            }
            for clip in &req.audio {
                parts.push(serde_json::json!({
                    "type": "input_audio",
                    "input_audio": { "data": b64(clip), "format": audio_format(clip) },
                }));
            }
            serde_json::json!({ "role": m.role, "content": parts })
        })
        .collect();
    serde_json::json!({
        "model": req.model,
        "messages": messages,
        "temperature": req.temperature,
        "max_tokens": req.max_tokens,
        "stream": true,
        "stream_options": { "include_usage": true },
    })
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// The image's media type from its magic bytes. The in-process engine accepted
/// PNG and JPEG (`rto-graph`'s `image` features), so those are the two named.
fn image_mime(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"\x89PNG") {
        "image/png"
    } else {
        "image/jpeg"
    }
}

/// The audio container from its magic bytes, as `input_audio.format` names it.
fn audio_format(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"RIFF") {
        "wav"
    } else if bytes.starts_with(b"fLaC") {
        "flac"
    } else {
        "mp3"
    }
}

/// Read an OpenAI server-sent-event stream, passing each content delta to
/// `on_token`, until `data: [DONE]` or the end of the body.
fn read_stream(
    reader: impl std::io::BufRead,
    on_token: &mut dyn FnMut(&str),
) -> Result<CompletionStats, EngineError> {
    let mut stats = CompletionStats {
        prompt_tokens: 0,
        completion_tokens: 0,
        finish_reason: FinishReason::Stop,
    };
    for line in reader.lines() {
        let line = line.map_err(|e| EngineError::Inference(format!("reading the stream: {e}")))?;
        let Some(data) = line.strip_prefix("data:").map(str::trim) else {
            continue;
        };
        if data == "[DONE]" {
            break;
        }
        let event: serde_json::Value = serde_json::from_str(data).map_err(|e| {
            EngineError::Inference(format!("the provider sent an unreadable event: {e}"))
        })?;
        if let Some(error) = event.get("error") {
            return Err(EngineError::Inference(format!(
                "the provider reported: {error}"
            )));
        }
        if let Some(choice) = event.pointer("/choices/0") {
            if let Some(piece) = choice.pointer("/delta/content").and_then(|v| v.as_str())
                && !piece.is_empty()
            {
                on_token(piece);
            }
            if choice.get("finish_reason").and_then(|v| v.as_str()) == Some("length") {
                stats.finish_reason = FinishReason::Length;
            }
        }
        if let Some(usage) = event.get("usage").filter(|u| !u.is_null()) {
            let count = |key: &str| {
                usage
                    .get(key)
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok())
                    .unwrap_or(0)
            };
            stats.prompt_tokens = count("prompt_tokens");
            stats.completion_tokens = count("completion_tokens");
        }
    }
    Ok(stats)
}

/// The vectors of a `/v1/embeddings` response, in input order.
fn parse_embeddings(text: &str, expected: usize) -> Result<Vec<Vec<f32>>, EngineError> {
    #[derive(serde::Deserialize)]
    struct Item {
        index: usize,
        embedding: Vec<f32>,
    }
    #[derive(serde::Deserialize)]
    struct Response {
        data: Vec<Item>,
    }
    let mut response: Response = serde_json::from_str(text).map_err(|e| {
        EngineError::Inference(format!("the provider sent unreadable embeddings: {e}"))
    })?;
    if response.data.len() != expected {
        return Err(EngineError::Inference(format!(
            "asked for {expected} embeddings and the provider returned {}",
            response.data.len()
        )));
    }
    response.data.sort_by_key(|item| item.index);
    Ok(response
        .data
        .into_iter()
        .map(|item| item.embedding)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};

    /// A one-shot HTTP server on loopback that answers with `status` and `body`,
    /// and hands back the request it received.
    fn stub(status: u16, body: &'static str) -> (String, std::thread::JoinHandle<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let base = format!("http://{}/v1", listener.local_addr().expect("addr"));
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = Vec::new();
            let mut buf = [0u8; 8192];
            // Read until the declared body has arrived.
            loop {
                let n = stream.read(&mut buf).expect("read");
                request.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&request);
                if let Some(split) = text.find("\r\n\r\n") {
                    let length = text[..split]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    if request.len() >= split + 4 + length {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            write!(
                stream,
                "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .expect("write");
            String::from_utf8_lossy(&request).into_owned()
        });
        (base, handle)
    }

    fn request(model: &str) -> ChatRequest {
        ChatRequest {
            model: model.to_owned(),
            messages: vec![rto_llama::Message {
                role: "user".to_owned(),
                content: "hello".to_owned(),
            }],
            images: Vec::new(),
            tools: None,
            audio: Vec::new(),
            temperature: 0.0,
            max_tokens: 16,
        }
    }

    #[test]
    fn a_streamed_completion_is_assembled_with_its_usage() {
        let (base, server) = stub(
            200,
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n\
             data: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":\"length\"}]}\n\n\
             data: {\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2}}\n\n\
             data: [DONE]\n\n",
        );
        let engine = ProviderEngine::new(&base, vec!["qwen3:8b".to_owned()]).expect("loopback");
        let completion = engine.chat(&request("qwen3:8b")).expect("completes");
        assert_eq!(completion.content, "Hello");
        assert_eq!(completion.prompt_tokens, 3);
        assert_eq!(completion.completion_tokens, 2);
        assert_eq!(completion.finish_reason, FinishReason::Length);

        let sent = server.join().expect("server");
        assert!(sent.starts_with("POST /v1/chat/completions "), "{sent}");
        assert!(sent.contains("\"model\":\"qwen3:8b\""), "{sent}");
        assert!(sent.contains("\"stream\":true"), "{sent}");
        // Tools stay in the prompt (ADR-0027 §3), so none go on the wire.
        assert!(!sent.contains("\"tools\""), "{sent}");
    }

    #[test]
    fn an_unknown_model_is_named_rather_than_reported_as_a_failure() {
        let (base, _server) = stub(404, "{\"error\":\"model not found\"}");
        let engine = ProviderEngine::new(&base, Vec::new()).expect("loopback");
        let err = engine.chat(&request("nope")).expect_err("404");
        assert!(
            matches!(err, EngineError::UnknownModel(ref m) if m == "nope"),
            "{err}"
        );
    }

    #[test]
    fn embeddings_come_back_in_input_order() {
        let (base, _server) = stub(
            200,
            "{\"data\":[{\"index\":1,\"embedding\":[2.0]},{\"index\":0,\"embedding\":[1.0]}]}",
        );
        let engine = ProviderEngine::new(&base, Vec::new()).expect("loopback");
        let vectors = engine
            .embed("bge-m3", &["a".to_owned(), "b".to_owned()])
            .expect("embeds");
        assert_eq!(vectors, vec![vec![1.0], vec![2.0]]);
    }

    #[test]
    fn images_ride_on_the_last_user_turn_as_data_urls() {
        let mut req = request("qwen2.5vl:3b");
        req.images.push(b"\x89PNG....".to_vec());
        let body = chat_body(&req);
        let url = body
            .pointer("/messages/0/content/1/image_url/url")
            .and_then(|v| v.as_str())
            .expect("an image part");
        assert!(url.starts_with("data:image/png;base64,"), "{url}");
    }

    #[test]
    fn only_this_machine_is_accepted() {
        for ok in [
            "http://127.0.0.1:11434/v1",
            "http://localhost:8080/v1/",
            "http://[::1]:8080/v1",
        ] {
            assert!(loopback_base(ok).is_ok(), "{ok}");
        }
        for refused in [
            "http://192.168.1.5:11434/v1",
            "https://api.example.com/v1",
            "ftp://127.0.0.1/v1",
            "127.0.0.1:11434",
        ] {
            assert!(loopback_base(refused).is_err(), "{refused}");
        }
    }
}

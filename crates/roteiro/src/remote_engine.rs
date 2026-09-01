//! Ask over the remote model tier — the served engine, wrapped (ADR-0019).
//!
//! # Why the wrapper is here and not in `rto-serve`
//!
//! Ask is not a Roteiro function. The explorer's panel POSTs
//! `/v1/chat/completions`, which `rto_serve::server` handles over an
//! `Arc<dyn Engine>`. So there were two places the remote branch could live, and
//! this is the one that keeps two existing guarantees intact:
//!
//! * **`rto-serve` gains nothing.** It carries llama.cpp and its 13 unmonitored
//!   vendored advisories (`docs/VENDORED_DEPENDENCIES.md`); putting Roteiro's
//!   one egress path in the same crate would put the guard and the largest
//!   third-party surface behind one feature flag.
//! * **The socket stays in the binary.** [`crate::remote_transport`] remains the
//!   only module in the workspace that can open one, and this file hands it to
//!   [`rto_remote::call_with`] as a closure exactly as `roteiro remote call`
//!   does. There is still no second path to the wire, and no second consent gate.
//!
//! The cost is real and worth stating: the hosted model becomes a served model
//! id, so it appears in `GET /v1/models` and any client that can reach the port
//! may address it — not only the Ask panel. Three things bound that, and
//! ADR-0019 §"What 'the invocation' means for a long-lived process" names all
//! three: `serve` binds loopback by default, the user layer must have granted
//! independently of the flag, and **every call is on the ledger**, so a session's
//! egress is enumerable afterwards rather than merely bounded in principle.
//!
//! # The grant is the process, and it dies with the process
//!
//! ADR-0019 v1.2 decides this: for a long-lived server the invocation *is* the
//! server process, so `serve --allow-remote` grants every Ask it answers for as
//! long as it runs. Re-asking per request cannot work — there is nobody at an
//! HTTP request to ask, so a per-request grant would be either a config value
//! (the user layer again, which never suffices alone) or a prompt no one is
//! present to answer.
//!
//! What that does **not** license is equally decided, and this type is built so
//! it cannot: the [`rto_remote::Decision`] is a field, taken at construction from
//! the gate. It is never recomputed, never persisted, never read back from
//! anywhere, and it is dropped when the engine is. There is no code here that
//! could infer a grant from a previous session because there is nothing to infer
//! it from.
//!
//! # The transcript is not what leaves
//!
//! This is the part worth reading twice. A chat request is a message array, and
//! with graph tools on, `rto_serve::chat_with_tools` injects **tool results** —
//! arbitrary graph query output — into the conversation before the model sees
//! it. Proxying that array would put bytes on the wire that were assembled by
//! "whatever the local path happened to build", which ADR-0019 §4 forbids by
//! name and which the [`rto_remote::Payload`] allow-list exists to make
//! impossible.
//!
//! So [`RemoteBackedEngine`] **rebuilds** the request:
//!
//! * the instruction is the user's own turns, and only those — assistant turns
//!   and tool results are dropped rather than forwarded;
//! * graph content reaches the endpoint **only** as
//!   [`rto_remote::ContextItem`]s, grounded through the same
//!   `rto_spec::context` the authoring surfaces use, and reduced by
//!   [`rto_remote::ContextItem::from_node`];
//! * the tool loop does not run remotely at all.
//!
//! The consequence is that a remote Ask is **not the same conversation** as a
//! local one, and that is a deliberate trade rather than an oversight. Keeping
//! the allow-list load-bearing is worth more than transcript fidelity on the one
//! path that leaves the machine.

use std::sync::Arc;

use rto_serve::{ChatRequest, CompletionStats, Engine, EngineError, FinishReason, ModelInfo};

/// How many graph nodes a remote Ask may ground itself on.
///
/// Well under [`rto_remote::payload::MAX_CONTEXT_ITEMS`], and lower than the
/// authoring surfaces use, because nobody reads a dry-run before an Ask: this
/// number is the disclosure a person implicitly accepted when they started the
/// server, so it is set where the ledger line stays readable.
const ASK_CONTEXT_NODES: usize = 12;

/// The budget is checked **at compile time**, not in a test: an Ask nobody
/// dry-ran must not be able to become an unbounded disclosure, and a bound that
/// can only be violated by editing a literal should fail the build that edits it
/// rather than a test run that might not be looked at.
const _: () = assert!(ASK_CONTEXT_NODES < rto_remote::payload::MAX_CONTEXT_ITEMS);

/// **Which HTTP status a remote failure deserves** — the `/v1` server derives
/// one from [`EngineError`]'s variant, so choosing the variant is choosing the
/// status.
///
/// The default of "everything is [`EngineError::Inference`]" was wrong in a way
/// that mattered, and not only for the 4xx/5xx contract. A 500 says *the server
/// broke*, so someone reading one goes looking for a fault; a refused consent
/// gate is the server working exactly as designed, and what they actually need to
/// do is grant consent. The status is the first thing they see and it was
/// pointing them away from the answer.
///
/// | Failure | Variant | Status | Why |
/// |---|---|---|---|
/// | [`rto_remote::RemoteError::NotConsented`] | [`EngineError::InvalidRequest`] | 400 | A policy refusal of *this request*. Not a fault, and the message carries the layer that refused and its remedy. |
/// | [`rto_remote::RemoteError::NoTransport`] | [`EngineError::Unsupported`] | 501 | A capability this build does not have — the same shape as asking a chat-only engine for embeddings. |
/// | [`rto_remote::RemoteError::Transport`] | [`EngineError::Inference`] | 500 | The upstream could not be reached. Genuinely server-side; 502 would be better and there is no variant for it. |
/// | [`rto_remote::RemoteError::Ledger`] | [`EngineError::Inference`] | 500 | This machine could not record the call, so it refused to make it. A real fault, on this side. |
///
/// # No new `EngineError` variant, deliberately
///
/// 403 is the honest status for a refused gate, and adding a `Forbidden` variant
/// would express it exactly. `EngineError` is **not** `#[non_exhaustive]`, and
/// `rto-llama`/`rto-serve` are published at 1.x with `rto-serve`'s own dispatch
/// matching it arm by arm — so a variant is a breaking change for every
/// downstream `match`. PR #391 could take the attribute for `rto_remote::Reason`
/// because that crate had been published hours earlier with nine downloads; this
/// is the other case, the one `rto_graph::StoreError` records: the enum shipped
/// without the attribute, so the fact gets expressed within the existing set
/// rather than by widening it. 400 with the gate's own explanation and remedy in
/// the body is a worse status code and a better answer.
///
/// A wildcard arm is required — [`rto_remote::RemoteError`] is
/// `#[non_exhaustive]` as of #391 — and it maps to `Inference`, which is the
/// conservative direction: a new failure this code has never seen is more likely
/// to be a fault than a policy refusal, and over-reporting a 500 is safer than
/// telling a client its request was invalid when nobody knows that it was.
fn classify(err: &rto_remote::RemoteError) -> EngineError {
    let text = err.to_string();
    match err {
        rto_remote::RemoteError::NotConsented(_) => EngineError::InvalidRequest(text),
        rto_remote::RemoteError::NoTransport { .. } => EngineError::Unsupported(text),
        rto_remote::RemoteError::Transport { .. } | rto_remote::RemoteError::Ledger(_) => {
            EngineError::Inference(text)
        }
        _ => EngineError::Inference(text),
    }
}

/// The served engine with the hosted model added beside it.
///
/// Delegates everything it is not asked for. The **only** request it handles
/// itself is one naming the endpoint's model string; every other id — every
/// local GGUF — goes straight to the wrapped engine and never touches this
/// file's code path.
pub struct RemoteBackedEngine {
    /// The llama.cpp engine this wraps. Untouched, and still the answer for
    /// every local model id.
    local: Arc<dyn Engine>,
    /// Where a remote call goes, and the vendor model string that names it. That
    /// string is also the served id, so `/v1/models` and the endpoint cannot
    /// disagree about what was asked for.
    endpoint: rto_remote::Endpoint,
    /// The gate's answer for **this process**, taken once at startup.
    ///
    /// Not an `Option<bool>` and not recomputed per request: ADR-0019 v1.2 scopes
    /// the invocation to the server process, and a decision that could be
    /// re-derived here would be a second implementation of who may send.
    decision: rto_remote::Decision,
    /// The egress ledger. Every call is written before it is sent, so a session's
    /// disclosures are enumerable after the fact.
    ledger: rto_remote::Ledger,
    /// The graph a remote Ask grounds itself against — read to turn a question
    /// into allow-listed context items, never to build a prompt string.
    workspace: Arc<rto_graph::Workspace>,
}

impl RemoteBackedEngine {
    /// Wrap `local`, serving `endpoint`'s model string in addition to it.
    pub fn new(
        local: Arc<dyn Engine>,
        endpoint: rto_remote::Endpoint,
        decision: rto_remote::Decision,
        ledger: rto_remote::Ledger,
        workspace: Arc<rto_graph::Workspace>,
    ) -> Self {
        Self {
            local,
            endpoint,
            decision,
            ledger,
            workspace,
        }
    }

    /// **Reduce a chat request to what may leave the machine.**
    ///
    /// The one place a transcript becomes a payload, and it is a reduction
    /// rather than a translation:
    ///
    /// * `user` turns become the instruction, in order. They are the person's own
    ///   words, which is the one free-text field ADR-0019 §4's allow-list
    ///   carries by design.
    /// * `assistant` and tool turns are **dropped**. An assistant turn is a
    ///   previous model's output about this repository and a tool turn is raw
    ///   graph query output; forwarding either would route graph content past
    ///   [`rto_remote::ContextItem::from_node`], which is the whole guard.
    /// * `system` turns are dropped too: [`rto_remote::Payload`] supplies its
    ///   own, and it is a constant precisely so that a dry-run shows it.
    ///
    /// Grounding then happens *here*, deliberately, through the same
    /// `rto_spec::context` the authoring surfaces use — so a remote Ask is still
    /// grounded, and everything grounding it went through the allow-list.
    fn payload_for(&self, req: &ChatRequest) -> Result<rto_remote::Payload, EngineError> {
        if !req.images.is_empty() || !req.audio.is_empty() {
            return Err(EngineError::InvalidRequest(
                "the remote model tier carries text only: images and audio are not on \
                 ADR-0019's payload allow-list, and Roteiro will not send bytes it cannot \
                 show you in a dry-run"
                    .to_owned(),
            ));
        }
        let instruction = req
            .messages
            .iter()
            .filter(|m| m.role == "user")
            .map(|m| m.content.trim())
            .filter(|c| !c.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        if instruction.is_empty() {
            return Err(EngineError::InvalidRequest(
                "a remote Ask needs a question in a `user` message: assistant turns and tool \
                 results are deliberately not forwarded to the hosted model (ADR-0019 §4)"
                    .to_owned(),
            ));
        }
        let nodes = self.ground(&instruction);
        rto_remote::Payload::new(&instruction, &nodes)
            .map_err(|e| EngineError::InvalidRequest(e.to_string()))
    }

    /// The graph nodes a question grounds against, read back as real
    /// [`rto_graph::Node`]s so [`rto_remote::ContextItem::from_node`] — and not
    /// some second reduction written here — decides what may be sent.
    ///
    /// A grounding failure is **not** fatal: an ungrounded question is a smaller
    /// disclosure, not a broken one, and refusing the Ask because a store read
    /// failed would trade a worse outcome for a stricter-looking one. The nodes
    /// that do resolve are the ones that travel.
    fn ground(&self, question: &str) -> Vec<rto_graph::Node> {
        let inner =
            |store: &rto_graph::Store| -> Result<Vec<rto_graph::Node>, rto_graph::StoreError> {
                let ctx = rto_spec::context(store, question, ASK_CONTEXT_NODES)?;
                let keys: Vec<String> = ctx
                    .symbols
                    .iter()
                    .map(|s| s.node.key.clone())
                    .chain(ctx.docs.iter().map(|d| d.key.clone()))
                    .take(ASK_CONTEXT_NODES)
                    .collect();
                let mut nodes = Vec::with_capacity(keys.len());
                for key in keys {
                    if let Some(node) = store.get_node(&key)? {
                        nodes.push(node);
                    }
                }
                Ok(nodes)
            };
        self.workspace
            .with_store(None, inner)
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default()
    }
}

impl Engine for RemoteBackedEngine {
    /// For the local models, yes; for the hosted one, no.
    ///
    /// `payload_for` builds an allow-listed body under ADR-0019, and that
    /// allow-list has no `tools` field — deliberately, since every key sent to a
    /// third party is a key someone approved. So a request routed to the tier
    /// carries no tools of its own and the caller must advertise them in the
    /// conversation, which is the one place the allow-list does let through.
    fn carries_tools(&self, model: &str) -> bool {
        model != self.endpoint.model() && self.local.carries_tools(model)
    }

    /// The hosted model **first**, then everything the wrapped engine serves.
    ///
    /// First because `chat_capable_model_ids` places the Ask default at the head
    /// of the pool and the UI sends `models[0]`: a server started with
    /// `--allow-remote` was started to use the tier, so the tier is what Ask
    /// reaches for. The local models remain served and addressable by name, so
    /// nothing became unavailable — only the default moved.
    fn models(&self) -> Vec<ModelInfo> {
        let mut out = vec![ModelInfo {
            id: self.endpoint.model().to_owned(),
        }];
        out.extend(self.local.models());
        out
    }

    /// Answer, remotely for the hosted id and locally for everything else.
    ///
    /// # Errors
    /// The wrapped engine's, or — for a remote request — the consent gate's, the
    /// transport's, the ledger's, or the response reader's. **None of them is a
    /// fall back to a local model**: a client that asked for the hosted model and
    /// got a local model's answer would have no signal that anything changed,
    /// and that is the failure ADR-0019 §6 most needs to prevent. The error text
    /// says so in as many words, and it reaches the client as a `/v1` error
    /// rather than as prose from a different model.
    ///
    /// Which `/v1` error, and therefore which HTTP status, is [`classify`]'s
    /// decision — a refused gate is a 400 and not a 500, because the server did
    /// not break.
    fn chat_stream(
        &self,
        req: &ChatRequest,
        on_token: &mut dyn FnMut(&str),
    ) -> Result<CompletionStats, EngineError> {
        if req.model != self.endpoint.model() {
            return self.local.chat_stream(req, on_token);
        }

        let payload = self.payload_for(req)?;
        let raw = rto_remote::call_with(
            &self.endpoint,
            &payload,
            self.decision,
            &self.ledger,
            &|| rto_exec::rfc3339_utc(std::time::SystemTime::now()),
            Some(&crate::remote_transport::call),
        )
        .map_err(|e| classify(&e))?;
        // Left as `Inference`: an unusable body is a failure *of the upstream*,
        // not of the client's request. The nearest honest status would be 502,
        // which `EngineError` has no variant for, and 500 is the closer of the
        // two available answers — the client did nothing wrong and cannot fix it.
        let answer =
            rto_remote::response::parse(&raw).map_err(|e| EngineError::Inference(e.to_string()))?;

        // Emitted as one piece rather than streamed: the request is sent with
        // `stream: false` (the body a dry-run prints), so there is nothing
        // incremental to relay, and inventing token boundaries would make the
        // stream look like a generation this process watched.
        on_token(&answer.text);
        Ok(CompletionStats {
            // Zero rather than a guess. The endpoint's own accounting is not on
            // the payload allow-list's return path, and a fabricated token count
            // is worse than an absent one — a client charging against it would be
            // charging against a number Roteiro made up.
            prompt_tokens: 0,
            completion_tokens: 0,
            // `response::parse` already refuses a generation that stopped at a
            // token limit, so anything that reaches here finished.
            finish_reason: FinishReason::Stop,
        })
    }

    /// Embeddings are always local.
    ///
    /// `ModelTask::Embed` is not one of the two surfaces the tier serves
    /// (`rto_graph::ModelTask::goes_remote`), and the remote tier speaks one
    /// chat-completion shape. So this delegates unconditionally rather than
    /// checking the id: there is no remote embedding to check for.
    fn embed(&self, model: &str, inputs: &[String]) -> Result<Vec<Vec<f32>>, EngineError> {
        self.local.embed(model, inputs)
    }
}

#[cfg(test)]
mod tests {
    use super::{RemoteBackedEngine, classify};
    use rto_serve::{ChatRequest, CompletionStats, Engine, EngineError, FinishReason, Message};
    use std::sync::Arc;

    /// A stand-in for llama.cpp that records what it was asked, so a test can
    /// assert on **delegation** rather than on generation.
    struct LocalSpy {
        seen: std::sync::Mutex<Vec<String>>,
    }

    impl Engine for LocalSpy {
        fn models(&self) -> Vec<rto_serve::ModelInfo> {
            vec![rto_serve::ModelInfo {
                id: "qwen3-0.6b".to_owned(),
            }]
        }

        fn chat_stream(
            &self,
            req: &ChatRequest,
            on_token: &mut dyn FnMut(&str),
        ) -> Result<CompletionStats, EngineError> {
            self.seen.lock().expect("lock").push(req.model.clone());
            on_token("the local answer");
            Ok(CompletionStats {
                prompt_tokens: 1,
                completion_tokens: 1,
                finish_reason: FinishReason::Stop,
            })
        }
    }

    fn engine(
        dir: &std::path::Path,
        invocation: Option<bool>,
    ) -> (RemoteBackedEngine, Arc<LocalSpy>) {
        let local = Arc::new(LocalSpy {
            seen: std::sync::Mutex::new(Vec::new()),
        });
        let endpoint = rto_remote::Endpoint::new(
            // Loopback, port 1, no DNS lookup: structurally unreachable rather
            // than merely unused. Follows part 2a's precedent — a test in this
            // workspace must not be able to reach a network even if it tried.
            "http://127.0.0.1:1/v1/chat/completions",
            "a-vendor-model",
            rto_remote::ProducerTrust::VendorAsserted,
        )
        .expect("a valid endpoint");
        let decision = rto_remote::consent::decide(
            rto_remote::ConfigGrant::from_layers(None, Some(true)),
            invocation,
        );
        let ledger = rto_remote::Ledger::at(dir.join("egress.jsonl"));
        // No projects, so grounding resolves nothing and the payload carries the
        // question alone. That is the point: these tests assert the *reduction*,
        // and a fixture graph would only add content to assert the absence of.
        let workspace = Arc::new(rto_graph::Workspace::from_stores(Vec::<(
            String,
            rto_graph::Store,
        )>::new()));
        (
            RemoteBackedEngine::new(
                Arc::clone(&local) as Arc<dyn Engine>,
                endpoint,
                decision,
                ledger,
                workspace,
            ),
            local,
        )
    }

    fn ask(model: &str, turns: &[(&str, &str)]) -> ChatRequest {
        ChatRequest {
            tools: None,
            model: model.to_owned(),
            messages: turns
                .iter()
                .map(|(role, content)| Message {
                    role: (*role).to_owned(),
                    content: (*content).to_owned(),
                })
                .collect(),
            images: Vec::new(),
            audio: Vec::new(),
            temperature: 0.0,
            max_tokens: 256,
        }
    }

    fn temp_dir(label: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "roteiro-remote-engine-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("create the test directory");
        dir
    }

    /// **A local model id never touches the remote path.** The wrapper adds a
    /// served model; it does not intercept the ones that were already there, and
    /// a build that silently rerouted `qwen3-0.6b` outward would be the worst
    /// possible reading of "the tier is available".
    #[test]
    fn a_local_model_id_is_delegated_untouched() {
        let dir = temp_dir("delegated");
        let (engine, local) = engine(&dir, Some(true));
        let mut text = String::new();
        let stats = engine
            .chat_stream(&ask("qwen3-0.6b", &[("user", "what is this?")]), &mut |t| {
                text.push_str(t);
            })
            .expect("the local engine answered");

        assert_eq!(text, "the local answer");
        assert_eq!(stats.prompt_tokens, 1, "the local engine's own accounting");
        assert_eq!(*local.seen.lock().expect("lock"), vec!["qwen3-0.6b"]);
        assert!(
            !dir.join("egress.jsonl").exists(),
            "a local answer is not an egress and leaves no ledger line"
        );
    }

    /// **A shut gate refuses rather than answering locally.** The client asked
    /// for the hosted model; handing back the local model's prose would be a
    /// different answer with no signal that anything changed — ADR-0019 §6's
    /// named failure, arriving through the consent gate instead of a socket.
    #[test]
    fn a_shut_gate_refuses_and_does_not_answer_from_the_local_model() {
        let dir = temp_dir("shut-gate");
        // The user layer granted; the invocation did not — the tightest case,
        // one flag from open.
        let (engine, local) = engine(&dir, None);
        let mut text = String::new();
        let err = engine
            .chat_stream(
                &ask("a-vendor-model", &[("user", "what is this?")]),
                &mut |t| {
                    text.push_str(t);
                },
            )
            .expect_err("the gate is shut");

        // 400, not 500: a refused gate is the server working as designed, and a
        // 500 would send whoever reads it looking for a fault instead of for the
        // consent they have to grant.
        assert!(matches!(err, EngineError::InvalidRequest(_)), "{err:?}");
        assert!(
            err.to_string().contains("not enabled for this run"),
            "names the gate: {err}"
        );
        assert!(text.is_empty(), "nothing was emitted");
        assert!(
            local.seen.lock().expect("lock").is_empty(),
            "the local engine must not have been asked instead"
        );
        assert!(
            !dir.join("egress.jsonl").exists(),
            "a refusal disclosed nothing, so it records nothing"
        );
    }

    /// **A refused gate is a 4xx, and the other failures keep their classes.**
    ///
    /// `EngineError`'s variant is what `rto-serve` turns into an HTTP status, so
    /// this asserts the whole mapping rather than only the case that was wrong.
    /// A 500 for a refused gate says *the server broke* and sends whoever reads
    /// it hunting for a fault, when what they need is to grant consent — the
    /// status is the first thing they see, and it was pointing away from the
    /// answer.
    #[test]
    fn a_refused_gate_is_a_client_error_and_the_rest_keep_their_classes() {
        use rto_remote::RemoteError;

        let refused = classify(&RemoteError::NotConsented(
            rto_remote::Reason::InvocationUnset,
        ));
        assert!(
            matches!(refused, EngineError::InvalidRequest(_)),
            "{refused:?}"
        );
        // The gate's own words survive the classification: a 400 whose body did
        // not say which layer refused would be a status code and no answer.
        assert!(
            refused.to_string().contains("not enabled for this run"),
            "{refused}"
        );

        // A build with no backend is a capability gap (501), not a fault and not
        // a bad request — the same shape as asking a chat-only engine to embed.
        let no_backend = classify(&RemoteError::NoTransport {
            endpoint: "https://models.example/v1".to_owned(),
        });
        assert!(
            matches!(no_backend, EngineError::Unsupported(_)),
            "{no_backend:?}"
        );

        // An unreachable upstream really is server-side, and stays a 500.
        let unreachable = classify(&RemoteError::Transport {
            endpoint: "https://models.example/v1".to_owned(),
            detail: "connection refused".to_owned(),
        });
        assert!(
            matches!(unreachable, EngineError::Inference(_)),
            "{unreachable:?}"
        );
        assert!(
            unreachable.to_string().contains("did **not** fall back"),
            "and it still refuses to degrade: {unreachable}"
        );
    }

    /// **The hosted model leads the served set, and the local ones stay.** The
    /// Ask UI sends `models[0]`, so a server started with `--allow-remote` must
    /// reach for the tier — without anything it served before becoming
    /// unaddressable.
    #[test]
    fn the_hosted_model_leads_and_the_local_models_remain() {
        let dir = temp_dir("models");
        let (engine, _) = engine(&dir, Some(true));
        let ids: Vec<String> = engine.models().into_iter().map(|m| m.id).collect();
        assert_eq!(ids, vec!["a-vendor-model", "qwen3-0.6b"]);
        assert_eq!(engine.endpoint.model(), "a-vendor-model");
    }

    /// **The transcript is reduced, not forwarded.** Assistant turns and tool
    /// results carry graph content that never passed through
    /// `ContextItem::from_node`; sending them would leave the allow-list in place
    /// with nothing to guard. Asserted on the payload rather than on the wire,
    /// because the wire is what the allow-list exists to keep clean.
    #[test]
    fn only_the_user_turns_reach_the_payload() {
        let dir = temp_dir("reduction");
        let (engine, _) = engine(&dir, Some(true));
        let req = ask(
            "a-vendor-model",
            &[
                ("system", "you are a helpful assistant"),
                ("user", "what does the store do?"),
                (
                    "assistant",
                    "it holds Store::apply_import_layer and friends",
                ),
                ("tool", "{\"secret_from_a_tool\":\"hunter2\"}"),
                ("user", "and how is it tested?"),
            ],
        );
        let payload = engine.payload_for(&req).expect("assembles");
        let instruction = payload.instruction();

        assert_eq!(
            instruction,
            "what does the store do?\n\nand how is it tested?"
        );
        for dropped in [
            "helpful assistant",
            "apply_import_layer",
            "secret_from_a_tool",
            "hunter2",
        ] {
            assert!(
                !instruction.contains(dropped),
                "`{dropped}` is not a user turn but reached the instruction: {instruction}"
            );
        }
        // …and the rendered body carries nothing of them either, which is the
        // assertion that actually protects the wire.
        let body = rto_remote::dry_run(&engine.endpoint, &payload);
        assert!(!body.contains("hunter2"), "{body}");
        assert!(!body.contains("apply_import_layer"), "{body}");
    }

    /// A request with no `user` turn is refused rather than sent empty, and the
    /// refusal says why the other turns were not used — otherwise it reads as a
    /// bug in the client.
    #[test]
    fn a_request_with_no_user_turn_is_refused_with_its_reason() {
        let dir = temp_dir("no-user-turn");
        let (engine, _) = engine(&dir, Some(true));
        let err = engine
            .payload_for(&ask(
                "a-vendor-model",
                &[("assistant", "I already answered that")],
            ))
            .expect_err("no question");
        assert!(matches!(err, EngineError::InvalidRequest(_)), "{err:?}");
        assert!(err.to_string().contains("not forwarded"), "{err}");
    }

    /// Images and audio are refused outright. They are not on ADR-0019 §4's
    /// allow-list, and a dry-run cannot show a person a megabyte of PNG — so
    /// there is no honest way to disclose them before sending.
    #[test]
    fn attached_media_is_refused_rather_than_sent() {
        let dir = temp_dir("media");
        let (engine, _) = engine(&dir, Some(true));
        let mut req = ask("a-vendor-model", &[("user", "describe this")]);
        req.images = vec![vec![0x89, b'P', b'N', b'G']];
        let err = engine
            .payload_for(&req)
            .expect_err("images are not sendable");
        assert!(matches!(err, EngineError::InvalidRequest(_)), "{err:?}");
        assert!(err.to_string().contains("text only"), "{err}");
    }
}

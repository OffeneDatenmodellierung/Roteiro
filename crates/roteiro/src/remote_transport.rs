//! The remote model tier's transport — **the only code in this workspace that
//! can send repository content off the machine** (ADR-0019).
//!
//! # Why it is here and not in `rto-remote`
//!
//! `rto-remote` holds the policy: who consented, what may be sent, what came
//! back, what was recorded. It takes the transport as a caller-supplied closure
//! ([`rto_remote::Transport`]), exactly as `rto-exec` takes its
//! [`rto_exec::Fetcher`], so the code that *decides* whether bytes may leave is
//! not the code that *can* make them leave. That separation is the whole reason
//! the guarantee is checkable from a `Cargo.toml` rather than asserted in prose,
//! and part 2 does not weaken it: this module supplies a closure to
//! [`rto_remote::call_with`], it does not replace it.
//!
//! One function opens a socket — [`call`] — and it is reachable from exactly one
//! command (`roteiro remote call`), only after [`rto_remote::consent::decide`]
//! has granted and only after the egress line is on disk.
//!
//! # What is *not* here, deliberately
//!
//! **No reachability probe.** There is no `ping`, no pre-flight `HEAD`, no DNS
//! lookup outside [`call`] itself. ADR-0019 §2: a probe *is* egress — a DNS
//! query leaks the question to a resolver — and probing to decide whether egress
//! is allowed inverts the gate. Whether the endpoint answers is discovered by the
//! one act that was consented to.
//!
//! **No retry.** A retried call is a second disclosure, and the ledger would be
//! right to record it as one. Deciding to send the same repository content twice
//! belongs to the person who consented, not to a backoff loop.
//!
//! **No redirect.** `max_redirects(0)`, and this is a policy rather than a
//! default worth inheriting: the ledger records the endpoint that was consented
//! to, and a `302` would make that record a lie about where the bytes went. A
//! moved endpoint is a configuration change, made deliberately in
//! `[remote] endpoint`.
//!
//! # The credential is an environment variable, and cannot be anything else
//!
//! [`API_KEY_ENV`] is read from the environment and has **no configuration
//! key**. `roteiro.toml` is committed by design — ADR-0007's own words — so a
//! key that could be set there is a key that gets committed, and the layering
//! that ADR-0019 §3 inverted for `enabled` would carry a credential into every
//! clone. It never reaches the ledger either, structurally: the ledger records
//! [`rto_remote::Payload::body`], and headers are not part of it.

/// The environment variable holding the endpoint's credential, if it needs one.
///
/// Deliberately not a config key — see the module docs. Unset is a supported
/// configuration, not a broken one: a loopback gateway terminating TLS on
/// Roteiro's behalf commonly wants no credential at all.
pub const API_KEY_ENV: &str = "ROTEIRO_REMOTE_API_KEY";

/// How long a single call may take, end to end.
///
/// A bound rather than a preference: a call with no deadline is a CLI that hangs
/// with repository content already disclosed and no way to know whether it
/// arrived. Generous enough for a large hosted model to think, short enough that
/// a black-holed connection fails within a coffee break — and the failure names
/// the endpoint, so it is diagnosable rather than mysterious.
const TIMEOUT_SECS: u64 = 120;

/// [`TIMEOUT_SECS`] as a `Duration`, for the agent's configuration.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(TIMEOUT_SECS);

/// The most of a response body that will be read into memory.
///
/// A completion is text. Anything past this is an endpoint that is not sending a
/// completion, and reading it in full would turn a misconfiguration into a
/// memory problem.
const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;

/// **Send.** The one function in this workspace that can put repository content
/// on a wire.
///
/// Shaped as [`rto_remote::Transport`] expects — `(&Endpoint, &str) -> Result<String, String>`
/// — taking the body as an already-rendered `&str` so it has no opportunity to
/// assemble a different request from the one the dry-run showed.
///
/// The `Err` string is the *detail* of `RemoteError::Transport`, which wraps it
/// with the endpoint and with the sentence saying no local model was
/// substituted. So every string returned here names the specific cause and
/// nothing else.
pub fn call(endpoint: &rto_remote::Endpoint, body: &str) -> Result<String, String> {
    let agent: ureq::Agent = ureq::config::Config::builder()
        // See the module docs: a redirect would move the bytes somewhere the
        // ledger does not say they went.
        .max_redirects(0)
        .timeout_global(Some(TIMEOUT))
        // Handle the status here rather than letting ureq turn it into an
        // opaque error: a 4xx body usually carries the endpoint's own
        // explanation, and that sentence is the actionable half of the failure.
        .http_status_as_error(false)
        .build()
        .into();

    let mut request = agent
        .post(endpoint.url())
        .header("content-type", "application/json");
    if let Some(key) = api_key() {
        request = request.header("authorization", format!("Bearer {key}"));
    }

    let mut response = request.send(body).map_err(|e| send_failure(&e))?;
    let status = response.status().as_u16();
    let text = response
        .body_mut()
        .with_config()
        .limit(MAX_RESPONSE_BYTES)
        .read_to_string()
        // A body that stops early is a body that did not all arrive, and ureq
        // enforces the declared framing itself. Named as a *response* failure
        // rather than passed off as a malformed answer, which is what
        // `rto_remote::response` would otherwise report it as.
        .map_err(|e| format!("the response body did not arrive whole: {e}"))?;

    if let Some(failure) = status_failure(status, &text) {
        return Err(failure);
    }
    Ok(text)
}

/// The endpoint's credential, or `None` when the environment does not set one.
///
/// An all-whitespace value counts as unset: an exported-but-empty variable is a
/// shell accident, and sending `Authorization: Bearer ` would turn it into a
/// puzzling 401 rather than the "no credential configured" it actually is.
pub fn api_key() -> Option<String> {
    std::env::var(API_KEY_ENV)
        .ok()
        .map(|key| key.trim().to_owned())
        .filter(|key| !key.is_empty())
}

/// Whether a credential is configured — the fact `roteiro remote status`
/// reports. **Never the value**, and never its length.
pub fn api_key_is_set() -> bool {
    api_key().is_some()
}

/// Turn a send failure into the detail `RemoteError::Transport` wraps.
///
/// Split out from [`call`] for the same reason [`status_failure`] is: this is
/// the branch that decides what a caller is told when nothing came back, and it
/// must not be the branch only a live server can exercise. Every variant here is
/// constructible in a test, and all four are.
///
/// Three failures get their own sentence because the generic one is misleading:
///
/// * **A timeout** is the only case where the deadline is worth naming. Saying
///   *"after 120s at most"* on a connection refused in a microsecond reads as
///   though something waited, which is worse than saying nothing.
/// * **An unresolvable host** is a configuration error, not an outage, and
///   pointing at `[remote] endpoint` saves a reader looking for a firewall.
/// * **A refused redirect** is this crate's own policy (see the module docs),
///   and left unexplained it reads as a transport bug.
fn send_failure(error: &ureq::Error) -> String {
    match error {
        ureq::Error::Timeout(_) => format!(
            "no response within {TIMEOUT_SECS}s, so the call was abandoned. \
             The request had already been sent and is already in the ledger — \
             a timeout does not un-send it"
        ),
        ureq::Error::HostNotFound => format!(
            "the host named by `[remote] endpoint` could not be resolved ({error}). \
             That is a configuration error rather than an outage"
        ),
        ureq::Error::RedirectFailed => format!(
            "the endpoint answered with a redirect, which is not followed ({error}): \
             the ledger records the endpoint that was consented to, and following the \
             redirect would make that record wrong about where the bytes went. \
             Point `[remote] endpoint` at the new URL deliberately"
        ),
        other => other.to_string(),
    }
}

/// Turn a non-success HTTP status into the failure detail, or `None` when the
/// status is a success.
///
/// Split out from [`call`] so the status handling has tests that need no socket
/// — the same split `download_asset_file` uses for `declared_body_length`, and
/// for the same reason: this is the branch that decides whether an answer
/// happened, and it must not be the branch that only a live server can exercise.
///
/// The endpoint's own body is quoted when there is one, because "HTTP 400" is
/// not an actionable error and *"model `x` does not exist"* is. `401` and `403`
/// additionally name [`API_KEY_ENV`], since a missing credential is the single
/// most likely cause and the variable is not guessable from the config file.
fn status_failure(status: u16, body: &str) -> Option<String> {
    if (200..300).contains(&status) {
        return None;
    }
    let said = body.trim();
    let said = if said.is_empty() {
        "and sent no explanation".to_owned()
    } else {
        format!("and said: {}", excerpt(said))
    };
    let hint = match status {
        401 | 403 => format!(
            ". The credential comes from `${API_KEY_ENV}` — it is not a config key, because \
             `roteiro.toml` is committed by design"
        ),
        // A redirect reaches here only because redirects are refused on purpose;
        // saying so stops it reading as a transport bug.
        300..=399 => ". Redirects are not followed: the ledger records the endpoint that was \
             consented to, and following a redirect would make that record wrong about where \
             the bytes went. Point `[remote] endpoint` at the new URL deliberately"
            .to_owned(),
        _ => String::new(),
    };
    Some(format!("the endpoint answered HTTP {status} {said}{hint}"))
}

/// The first 200 characters of an endpoint's message, marked when clipped.
///
/// Bounded for the same reason `rto_remote::response`'s excerpt is: a gateway
/// answering with an HTML error page should produce a readable first line, not a
/// page.
fn excerpt(text: &str) -> String {
    match text.char_indices().nth(200) {
        None => text.to_owned(),
        Some((cut, _)) => format!("{}…[truncated]", &text[..cut]),
    }
}

/// **Whether this run's consent may be asked for at a prompt**, given what the
/// gate already decided.
///
/// The invocation grant has two forms — a flag, and a TTY prompt (ADR-0019 §3's
/// *"Invocation (flag, or a TTY prompt)"*) — and exactly one gate state may be
/// resolved by asking:
///
/// | Reason | May a prompt open it | Why |
/// |---|---|---|
/// | [`Reason::InvocationUnset`] | **yes** | The human opted in; this run has not. Asking is the invocation. |
/// | [`Reason::UserLayerUnset`] / [`Reason::UserLayerDenied`] | no | The user layer opts *the human* in, and it lives in a file they edited deliberately. A prompt that could stand in for it would collapse two required grants into one, which is the thing ADR-0019 §3 says neither alone may do. |
/// | [`Reason::ProjectDenied`] | no | A repository-wide denial that no flag overrides is not made overridable by asking nicely. |
/// | [`Reason::InvocationDenied`] | no | This run already said no. Re-asking is not consent, it is nagging until yes. |
/// | [`Reason::Granted`] | no | Nothing to ask. |
///
/// [`Reason::InvocationUnset`]: rto_remote::Reason::InvocationUnset
/// [`Reason::UserLayerUnset`]: rto_remote::Reason::UserLayerUnset
/// [`Reason::UserLayerDenied`]: rto_remote::Reason::UserLayerDenied
/// [`Reason::ProjectDenied`]: rto_remote::Reason::ProjectDenied
/// [`Reason::InvocationDenied`]: rto_remote::Reason::InvocationDenied
/// [`Reason::Granted`]: rto_remote::Reason::Granted
pub fn may_prompt(reason: rto_remote::Reason) -> bool {
    matches!(reason, rto_remote::Reason::InvocationUnset)
}

/// What a person is shown before being asked to grant this run.
///
/// **The exact bytes, not a summary of them.** ADR-0019 §4 requires the payload
/// to be inspectable before it is sent, and a prompt that says *"send 3 nodes to
/// the endpoint?"* is not an inspection — it is a description of one, and the
/// description is where a disclosure goes missing. This shows what `dry-run`
/// shows, because it is the same string from the same function.
pub fn prompt_text(endpoint: &rto_remote::Endpoint, body: &str, fields: &[&'static str]) -> String {
    format!(
        "\nroteiro is about to send repository content off this machine.\n\
         \n\
         to:       {url}\n\
         as model: {model}  (trust: {trust})\n\
         carrying: {fields} ({bytes} bytes)\n\
         \n\
         --- the exact body ---\n{body}\n--- end of body ---\n\
         \n{disclosure}\n",
        url = endpoint.url(),
        model = endpoint.model(),
        trust = endpoint.trust().as_str(),
        fields = fields.join(", "),
        bytes = body.len(),
        disclosure = rto_remote::Payload::disclosure(),
    )
}

#[cfg(test)]
mod tests {
    use super::{API_KEY_ENV, TIMEOUT_SECS, may_prompt, prompt_text, send_failure, status_failure};
    use rto_remote::{Endpoint, ProducerTrust, Reason};

    fn endpoint() -> Endpoint {
        Endpoint::new(
            "https://models.example/v1/chat/completions",
            "a-vendor-model",
            ProducerTrust::VendorAsserted,
        )
        .expect("a valid endpoint")
    }

    /// **Only a timeout names the deadline.** Saying *"after 120 seconds at
    /// most"* on a connection refused in a microsecond reads as though something
    /// waited, which misdirects a reader more thoroughly than saying nothing —
    /// and it was the first thing this got wrong.
    #[test]
    fn a_send_failure_says_the_true_thing_about_why_nothing_came_back() {
        let timed_out = send_failure(&ureq::Error::Timeout(ureq::Timeout::Global));
        assert!(timed_out.contains(&TIMEOUT_SECS.to_string()), "{timed_out}");
        assert!(
            timed_out.contains("does not un-send it"),
            "the bytes are gone and the ledger already says so: {timed_out}"
        );

        // Everything else is reported in the transport's own words, with no
        // invented deadline attached.
        let refused = send_failure(&ureq::Error::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "Connection refused",
        )));
        assert!(refused.contains("Connection refused"), "{refused}");
        assert!(
            !refused.contains(&TIMEOUT_SECS.to_string()),
            "an instant refusal did not wait for anything: {refused}"
        );

        // A name that will not resolve is a configuration error, and pointing at
        // the key saves a reader hunting for a firewall.
        let unresolved = send_failure(&ureq::Error::HostNotFound);
        assert!(unresolved.contains("[remote] endpoint"), "{unresolved}");
        assert!(unresolved.contains("rather than an outage"), "{unresolved}");

        // And a refused redirect is explained as the policy it is — see
        // `a_redirect_is_reported_as_a_refusal_with_its_reason` for the status
        // half of the same rule.
        let redirected = send_failure(&ureq::Error::RedirectFailed);
        assert!(redirected.contains("consented to"), "{redirected}");
        assert!(redirected.contains("not followed"), "{redirected}");
    }

    /// A success passes through untouched — the status branch must not be able
    /// to invent a failure for a call that worked — and the boundary is pinned
    /// at both ends, because a range that quietly grew would let a `3xx` be read
    /// as an answer.
    #[test]
    fn a_success_status_is_not_a_failure() {
        for status in [200, 201, 204, 299] {
            assert_eq!(status_failure(status, "{}"), None, "HTTP {status}");
        }
        for status in [199, 300] {
            assert!(
                status_failure(status, "{}").is_some(),
                "HTTP {status} is not a success"
            );
        }
    }

    /// **The endpoint's own words survive.** "HTTP 400" is not actionable;
    /// *"model `x` does not exist"* is, and it is the only sentence in the
    /// exchange that knows what actually went wrong.
    #[test]
    fn a_failure_status_quotes_what_the_endpoint_said() {
        let detail = status_failure(400, r#"{"error":{"message":"model `x` does not exist"}}"#)
            .expect("a failure");
        assert!(detail.contains("HTTP 400"), "{detail}");
        assert!(detail.contains("model `x` does not exist"), "{detail}");

        // An endpoint that explained nothing says so, rather than producing a
        // trailing colon and a blank.
        let silent = status_failure(500, "   ").expect("a failure");
        assert!(silent.contains("sent no explanation"), "{silent}");

        // And a page of HTML is clipped rather than pasted whole.
        let huge = status_failure(502, &"x".repeat(5_000)).expect("a failure");
        assert!(huge.contains("…[truncated]"), "bounded");
        assert!(
            huge.len() < 600,
            "{} chars is not an error message",
            huge.len()
        );
    }

    /// An auth failure names the environment variable, because the credential is
    /// deliberately *not* a config key and nothing in `roteiro.toml` would lead
    /// a reader to it.
    #[test]
    fn an_auth_failure_names_the_environment_variable() {
        for status in [401, 403] {
            let detail = status_failure(status, "unauthorized").expect("a failure");
            assert!(detail.contains(API_KEY_ENV), "HTTP {status}: {detail}");
            assert!(detail.contains("committed by design"), "{detail}");
        }
        // …and an unrelated failure does not, or the hint stops meaning anything.
        let other = status_failure(500, "boom").expect("a failure");
        assert!(!other.contains(API_KEY_ENV), "{other}");
    }

    /// A redirect is reported as the **policy** it is, not as a transport
    /// oddity: the ledger records the endpoint that was consented to, so
    /// following a `302` would make that record wrong about where the bytes
    /// went.
    #[test]
    fn a_redirect_is_reported_as_a_refusal_with_its_reason() {
        let detail = status_failure(302, "").expect("a failure");
        assert!(detail.contains("Redirects are not followed"), "{detail}");
        assert!(detail.contains("consented to"), "{detail}");
    }

    /// **Only one gate state may be opened by asking.** The user layer opts the
    /// human in and the invocation opts the run in; a prompt that could stand in
    /// for the user layer would collapse the two grants ADR-0019 §3 requires
    /// separately into one keystroke.
    #[test]
    fn a_prompt_may_only_supply_the_invocation_half_of_consent() {
        assert!(
            may_prompt(Reason::InvocationUnset),
            "the run, not the human"
        );
        for reason in [
            Reason::Granted,
            Reason::ProjectDenied,
            Reason::InvocationDenied,
            Reason::UserLayerDenied,
            Reason::UserLayerUnset,
        ] {
            assert!(
                !may_prompt(reason),
                "{reason:?} must not be resolvable at a prompt"
            );
        }
    }

    /// **The prompt shows the bytes, not a summary of them.** A description of a
    /// disclosure is where the disclosure goes missing, so what is shown here is
    /// the same string `dry-run` prints, and it carries the same caveats.
    #[test]
    fn the_prompt_shows_the_exact_body_and_the_full_disclosure() {
        let endpoint = endpoint();
        let payload = rto_remote::Payload::new(
            "what changed?",
            &[rto_graph::Node::new(
                "adr:0019",
                rto_graph::NodeKind::Adr,
                "Remote tier",
            )],
        )
        .expect("assembles");
        let body = rto_remote::dry_run(&endpoint, &payload);
        let text = prompt_text(&endpoint, &body, &payload.fields_present());

        assert!(text.contains(&body), "the exact bytes, verbatim");
        assert!(text.contains("https://models.example/v1/chat/completions"));
        assert!(text.contains("vendor_asserted"), "{text}");
        // The disclosure's uncomfortable half travels with the prompt, not just
        // with `status` — this is the moment it is actually being read.
        assert!(text.contains("no redaction chokepoint"), "{text}");
        assert!(text.contains("DATABASE_URL"), "{text}");
    }
}

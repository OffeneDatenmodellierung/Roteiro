//! The remote model tier — Roteiro's one explicitly-consented egress path
//! ([[docs/adr/0019-remote-model-tier.md]]).
//!
//! This is the first capability in Roteiro that sends repository content off the
//! machine. That is the whole of its significance, and every guard in this crate
//! is load-bearing rather than ceremonial.
//!
//! # What this crate is, and what it structurally cannot do
//!
//! It holds the **policy**: who consented ([`consent`]), what may be sent
//! ([`payload`]), where to ([`endpoint`]), what came back ([`response`]), what
//! was recorded ([`record`]), and whether a *local* attempt fell short
//! ([`escalation`]).
//!
//! It holds **no transport**, and must never hold one. [`call_with`] takes the
//! transport as a caller-supplied closure — the same shape `rto-exec` uses for
//! its `Fetcher`, and for the same reason: the code that decides whether bytes
//! may leave is not the code that can make them leave, so the guarantee is
//! checkable rather than promised. It also means every test in this crate
//! exercises the whole path with no network, so a test cannot accidentally become
//! the first thing that sends data.
//!
//! `rto-graph` — extraction and the graph — gains nothing from this. Its `gix` is
//! still pinned `default-features = false` to exclude the transports, and this
//! crate depends *on* it rather than the other way round.
//!
//! # The four rules, and where each one lives
//!
//! | ADR-0019 | Rule | Here |
//! |---|---|---|
//! | §1 | The local→remote edge is a gate, not a routing decision. No learned router. | [`consent::decide`] reads only layers; [`escalation`] measures only finished attempts |
//! | §2 | Reachability must not be probed — a probe *is* egress | there is no probe; [`endpoint`] validates shape, never the world |
//! | §4 | An explicit allow-list, inspectable before it is sent | [`payload::ContextItem::from_node`], [`dry_run`] |
//! | §5 | Remote output is not a graph fact | nothing here writes a node, an edge or a `Provenance`; [`record::Ledger`] is a file beside the graph, not in it |
//!
//! # Fail loudly, never degrade silently
//!
//! Principle 10's first half is *exempted* for this capability — a remote call is
//! fetching by definition and can be neither digest-pinned nor prefetched. Its
//! second half binds harder: with the tier enabled and no network, [`call_with`]
//! fails with a named error that quotes the endpoint and says, in as many words,
//! that it did **not** fall back to a local model. An unannounced downgrade
//! produces a different answer with no signal that anything changed, and it is
//! the failure mode ADR-0019 most needs to prevent.
//!
//! [`response::parse`] carries the same rule onto the receive side, where it is
//! easier to miss: a completion that stopped at a token limit *reads* as
//! finished, so returning it would be an unannounced downgrade with the
//! endpoint's own name on it. It is refused instead.
//!
//! # Default-off, and it stays default-off
//!
//! Nothing in this crate has a default that permits a call. [`consent::decide`]
//! denies unless the user's own config *and* the invocation both grant, and
//! [`call_with`] additionally refuses when there is no transport to hand it to.

pub mod consent;
pub mod endpoint;
pub mod escalation;
pub mod payload;
pub mod record;
pub mod response;
pub mod trust;

pub use consent::{ConfigGrant, Decision, Invocation, Reason};
pub use endpoint::{Endpoint, EndpointError};
pub use escalation::{Check, LocalAttempt, Policy, Trigger};
pub use payload::{ContextItem, Payload, PayloadError};
pub use record::{Egress, Entry, Ledger, LedgerError, Outcome};
pub use response::{Answer, ResponseError};
pub use trust::ProducerTrust;

/// The transport, supplied by the caller.
///
/// Given an endpoint and the exact body to send, it returns the response text or
/// a reason it failed. **This crate never implements one**; see the module docs.
///
/// The signature takes the body as a `&str` already rendered by
/// [`Payload::body`], rather than taking the payload, so that an implementation
/// has no opportunity to assemble a *different* request from the one the
/// dry-run showed.
pub type Transport<'a> = dyn Fn(&Endpoint, &str) -> Result<String, String> + 'a;

/// The clock, supplied by the caller — the ledger's timestamps.
///
/// Injected for the same reason the transport is: this crate stays a pure
/// function of its inputs, and a test can assert on an exact recorded timestamp
/// instead of on the fact that one exists. The binary passes
/// `rto_exec::rfc3339_utc` over `SystemTime::now()`.
pub type Clock<'a> = dyn Fn() -> String + 'a;

/// Why a remote call did not produce an answer.
/// Marked `#[non_exhaustive]` for the reason recorded on
/// [`crate::Reason`]: this crate is published at 1.x, and error sets grow.
/// Taken while the crate had no consumer that could exist; it will not be
/// taken again.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RemoteError {
    /// The gate is shut. Nothing was assembled, nothing was recorded, nothing
    /// was sent.
    #[error("the remote model tier is not enabled for this run: {0}")]
    NotConsented(Reason),
    /// Consent was given, but this build has no backend to give the request to.
    ///
    /// Reported as loudly as a network failure and never as a fall back, because
    /// the two are the same thing from the caller's point of view: the remote
    /// answer they consented to does not exist.
    #[error(
        "the remote model tier is enabled and consented, but this build has no backend to \
         reach {endpoint} with — so no request was made. Roteiro did **not** fall back to a \
         local model: a different model is a different answer, and it will not give you one \
         without saying so"
    )]
    NoTransport {
        /// The endpoint that would have been called.
        endpoint: String,
    },
    /// The transport failed — unreachable, refused, timed out, anything.
    #[error(
        "the remote model tier could not reach {endpoint}: {detail}. Roteiro did **not** fall \
         back to a local model — a different model is a different answer, and an unannounced \
         downgrade gives you one with no signal that anything changed. Re-run without \
         `--allow-remote` to get the local answer deliberately"
    )]
    Transport {
        /// The endpoint that was called.
        endpoint: String,
        /// The transport's own words.
        detail: String,
    },
    /// The call could not be recorded.
    ///
    /// Fatal by design. An egress path whose record cannot be written is an
    /// egress path with no answer to *"what left this machine?"*, and ADR-0019
    /// requires that question to be answerable after the fact rather than
    /// reconstructed.
    #[error(transparent)]
    Ledger(#[from] LedgerError),
}

/// **What would be sent, exactly.** Assembles nothing new and sends nothing: it
/// returns the same string [`call_with`] hands the transport.
///
/// ADR-0019 §4 requires the payload to be inspectable *before* it is sent, and
/// the only way a preview is worth reading is if it is the same bytes. One
/// function produces them; `the_dry_run_body_is_what_the_transport_receives`
/// holds the two call sites level.
///
/// Deliberately **not** recorded: nothing left the machine, and a ledger that
/// logged inspections alongside disclosures would make the disclosures harder to
/// find.
#[must_use]
pub fn dry_run(endpoint: &Endpoint, payload: &Payload) -> String {
    payload.body(endpoint)
}

/// Make a remote call, if every guard permits it.
///
/// The order below is the contract, and each step is a refusal point:
///
/// 1. **Consent.** A shut gate returns [`RemoteError::NotConsented`] having
///    assembled nothing, recorded nothing and sent nothing.
/// 2. **A backend exists.** Checked *before* anything is recorded, so a build
///    with no transport never writes an egress line for a call that could not
///    happen.
/// 3. **The record is written.** The [`Entry::Egress`] line lands on disk, with
///    the body, before the transport is invoked. An unwritable ledger
///    ([`RemoteError::Ledger`]) **refuses the call** rather than sending
///    unrecorded — the one ordering decision in this function that is a policy
///    rather than a convenience.
/// 4. **The call happens**, and its outcome is recorded whether it succeeded or
///    failed.
///
/// # Errors
/// See [`RemoteError`]. Every variant is a refusal or a failure; none of them is
/// a fall back to a local model.
pub fn call_with(
    endpoint: &Endpoint,
    payload: &Payload,
    decision: Decision,
    ledger: &Ledger,
    now: &Clock<'_>,
    transport: Option<&Transport<'_>>,
) -> Result<String, RemoteError> {
    if !decision.granted() {
        return Err(RemoteError::NotConsented(decision.reason));
    }
    let Some(transport) = transport else {
        return Err(RemoteError::NoTransport {
            endpoint: endpoint.url().to_owned(),
        });
    };

    let body = payload.body(endpoint);
    let at = now();
    let call = Ledger::next_call_id(&at);
    ledger.append(&Entry::Egress(Egress {
        call: call.clone(),
        at,
        endpoint: endpoint.url().to_owned(),
        model: endpoint.model().to_owned(),
        trust: endpoint.trust(),
        fields: payload
            .fields_present()
            .into_iter()
            .map(ToOwned::to_owned)
            .collect(),
        bytes: body.len(),
        // The recorded copy and the sent bytes are one `String`, not two calls
        // to `body()` that happen to agree: the record is of *these* bytes.
        body: body.clone(),
    }))?;

    let result = transport(endpoint, &body);
    ledger.append(&Entry::Outcome(Outcome {
        call,
        at: now(),
        ok: result.is_ok(),
        detail: result.as_ref().err().cloned().unwrap_or_default(),
        response_bytes: result.as_ref().map_or(0, String::len),
    }))?;

    result.map_err(|detail| RemoteError::Transport {
        endpoint: endpoint.url().to_owned(),
        detail,
    })
}

/// **Every public enum here has had the `#[non_exhaustive]` question answered.**
///
/// Asserted against this crate's own source, not against behaviour, because
/// "somebody thought about semver" is a property of the text rather than of any
/// runtime value — the same reason `remote_is_not_a_default_feature` reads a
/// `Cargo.toml`.
///
/// This exists because the decision was expensive to make once and would be
/// silently unmade by the next `pub enum` added without it. `rto-remote` is
/// published at 1.x, so a variant added to a bare public enum is a breaking
/// change; [`Reason`] carries the full reasoning, and `rto_graph::StoreError`
/// carries the cautionary case. A new enum passes here by taking the attribute
/// **or** by saying in its own docs why its set is closed — either is a decision,
/// and only silence is not.
#[cfg(test)]
mod semver_posture {
    /// Every `src/*.rs` in this crate, as `(file name, contents)`.
    fn sources() -> Vec<(String, String)> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .expect("read this crate's src/")
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "rs"))
            .map(|p| {
                let name = p
                    .file_name()
                    .expect("a file name")
                    .to_string_lossy()
                    .into_owned();
                (
                    name,
                    std::fs::read_to_string(&p).expect("read a source file"),
                )
            })
            .collect();
        files.sort();
        files
    }

    #[test]
    fn every_public_enum_either_is_non_exhaustive_or_says_why_not() {
        let mut seen = 0;
        for (file, text) in sources() {
            let lines: Vec<&str> = text.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                let Some(rest) = line.strip_prefix("pub enum ") else {
                    continue;
                };
                seen += 1;
                let name = rest.split_whitespace().next().unwrap_or(rest);

                // Walk back over the item's attributes and doc comment, which is
                // everything contiguous above it that is one or the other.
                //
                // Attributes and docs are kept **apart** on purpose. The first
                // version of this test joined them and asked whether the block
                // contained `#[non_exhaustive]` — which every deliberately-
                // exhaustive enum's doc comment does, because saying "not
                // `#[non_exhaustive]`" names the attribute. It reported `Trigger`
                // as both marked and unmarked. A check for an attribute has to
                // look at attribute lines.
                let (mut attrs, mut docs) = (Vec::new(), Vec::new());
                for above in lines[..i].iter().rev() {
                    let t = above.trim_start();
                    if t.starts_with("///") {
                        docs.push(t);
                    } else if t.starts_with("#[") || t.starts_with(')') {
                        attrs.push(t);
                    } else if !t.starts_with("//") || t.starts_with("//!") {
                        // An ordinary `//` comment sits inside the block and is
                        // skipped; anything else — including a module doc, which
                        // means we have run off the top of the file — ends it.
                        break;
                    }
                }

                let marked = attrs.contains(&"#[non_exhaustive]");
                // The opt-out has to *name* the attribute, so it reads as a
                // refusal rather than as prose that happens to be nearby.
                let justified = docs.iter().any(|d| d.contains("not `#[non_exhaustive]`"));
                assert!(
                    marked || justified,
                    "{file}: `pub enum {name}` is neither `#[non_exhaustive]` nor documented as \
                     deliberately exhaustive. This crate is published at 1.x, so a variant added \
                     to it later is a breaking change — see `Reason`'s doc comment for what that \
                     cost the last time, and `rto_graph::StoreError` for what it costs when the \
                     decision is not made at all. Take the attribute, or say in the enum's own \
                     docs why its set is closed."
                );
                assert!(
                    !(marked && justified),
                    "{file}: `pub enum {name}` both carries `#[non_exhaustive]` and documents \
                     itself as deliberately not; one of the two is stale"
                );
            }
        }
        assert!(
            seen >= 9,
            "only found {seen} public enums — the scan stopped matching, which would let this \
             test pass by finding nothing"
        );
    }
}

/// Temp directories for this crate's tests. No network, no fixtures, no shared
/// state — every test that touches a ledger gets its own directory.
#[cfg(test)]
pub(crate) mod testing {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A fresh, empty directory named for `label`. Unique per call as well as per
    /// process, so two tests in one binary cannot collide on a ledger path.
    pub(crate) fn temp_dir(label: &str) -> PathBuf {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "rto-remote-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("create the test directory");
        dir
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Ledger, LocalAttempt, Payload, Policy, RemoteError, Transport, call_with, dry_run,
        testing::temp_dir,
    };
    use crate::consent::{ConfigGrant, Decision, Reason, decide};
    use crate::endpoint::Endpoint;
    use crate::trust::ProducerTrust;
    use rto_graph::{Node, NodeKind};

    const AT: &str = "2026-08-17T09:00:00Z";

    fn clock() -> impl Fn() -> String {
        || AT.to_owned()
    }

    fn endpoint() -> Endpoint {
        Endpoint::new(
            "https://models.example/v1/chat/completions",
            "a-vendor-model",
            ProducerTrust::VendorAsserted,
        )
        .expect("a valid endpoint")
    }

    fn payload() -> Payload {
        Payload::new(
            "what changed?",
            &[Node::new("adr:0019", NodeKind::Adr, "Remote tier")],
        )
        .expect("assembles")
    }

    /// Consent as it is actually reached in production: user config plus the
    /// invocation, project silent.
    fn granted() -> Decision {
        decide(ConfigGrant::from_layers(None, Some(true)), Some(true))
    }

    /// **A shut gate stops before anything happens.** The transport must not be
    /// reached, and — as much to the point — no ledger line may be written: the
    /// egress record is a record of disclosures, and a refusal disclosed nothing.
    #[test]
    fn a_denied_call_never_reaches_the_transport_and_records_nothing() {
        let dir = temp_dir("denied");
        let ledger = Ledger::at(dir.join("egress.jsonl"));
        let called = std::cell::Cell::new(false);
        let transport = |_: &Endpoint, _: &str| -> Result<String, String> {
            called.set(true);
            Ok("should never happen".to_owned())
        };

        // Everything grants except the invocation — the tightest case, since the
        // gate is one flag away from opening.
        let decision = decide(ConfigGrant::from_layers(None, Some(true)), None);
        let err = call_with(
            &endpoint(),
            &payload(),
            decision,
            &ledger,
            &clock(),
            Some(&transport),
        )
        .expect_err("the invocation did not grant");

        assert!(matches!(
            err,
            RemoteError::NotConsented(Reason::InvocationUnset)
        ));
        assert!(!called.get(), "the transport was reached");
        assert!(ledger.read().expect("read").is_empty(), "nothing recorded");
        assert!(
            !ledger.path().exists(),
            "a refusal does not even create the ledger"
        );
    }

    /// **A build with no backend fails as loudly as a network failure**, names
    /// the endpoint, and says in as many words that it did not fall back. This is
    /// Principle 10's second half, which ADR-0019 says binds harder for this
    /// capability than for any other.
    #[test]
    fn no_backend_fails_loudly_naming_the_endpoint_and_refuses_to_fall_back() {
        let dir = temp_dir("no-backend");
        let ledger = Ledger::at(dir.join("egress.jsonl"));
        let err = call_with(&endpoint(), &payload(), granted(), &ledger, &clock(), None)
            .expect_err("no transport");

        let text = err.to_string();
        assert!(matches!(err, RemoteError::NoTransport { .. }), "{err:?}");
        assert!(
            text.contains("https://models.example/v1/chat/completions"),
            "{text}"
        );
        assert!(text.contains("did **not** fall back"), "{text}");
        assert!(
            ledger.read().expect("read").is_empty(),
            "a call that could not be made is not an egress"
        );
    }

    /// A transport failure is reported the same way, for the same reason: the
    /// caller consented to a remote answer and did not get one, and the worst
    /// available outcome is quietly handing them a different model's.
    #[test]
    fn a_transport_failure_names_the_endpoint_and_refuses_to_fall_back() {
        let dir = temp_dir("transport-failure");
        let ledger = Ledger::at(dir.join("egress.jsonl"));
        let transport =
            |_: &Endpoint, _: &str| -> Result<String, String> { Err("connection refused".into()) };
        let err = call_with(
            &endpoint(),
            &payload(),
            granted(),
            &ledger,
            &clock(),
            Some(&transport),
        )
        .expect_err("the transport failed");

        let text = err.to_string();
        assert!(
            text.contains("https://models.example/v1/chat/completions"),
            "{text}"
        );
        assert!(text.contains("connection refused"), "{text}");
        assert!(text.contains("did **not** fall back"), "{text}");

        // The attempt is still in the record: bytes left the machine, and the
        // fact that nothing came back does not un-send them.
        let entries = ledger.read().expect("read");
        assert_eq!(entries.len(), 2, "the egress and its outcome");
        let super::Entry::Outcome(outcome) = &entries[1] else {
            panic!("expected an outcome line, got {:?}", entries[1]);
        };
        assert!(!outcome.ok);
        assert_eq!(outcome.detail, "connection refused");
    }

    /// **A call that fails leaves an honest record, not a missing one.**
    ///
    /// The ledger is written first *by design*, and the reason only shows up in
    /// the failure case: `a_transport_failure_names_the_endpoint_and_refuses_to_fall_back`
    /// reads the ledger after the call returns, which a single line written at the
    /// end would also satisfy. This reads it **from inside the failing transport**,
    /// which only the write-first ordering can satisfy — and it is the ordering
    /// that matters, because the calls worth knowing about are exactly the ones
    /// that never returned. "We have no record, so presumably nothing left" is the
    /// wrong default for an egress log.
    #[test]
    fn a_failing_call_finds_its_egress_already_recorded_before_it_fails() {
        let dir = temp_dir("failure-ordering");
        let ledger = Ledger::at(dir.join("egress.jsonl"));
        let payload = payload();
        let endpoint = endpoint();

        let transport = |_: &Endpoint, _: &str| -> Result<String, String> {
            // The bytes are gone by now: whatever happens next, the record of
            // their leaving is already on disk.
            let seen = ledger.read().expect("read from inside the call");
            assert_eq!(seen.len(), 1, "the egress line is on disk before sending");
            let super::Entry::Egress(egress) = &seen[0] else {
                panic!("expected an egress line, got {:?}", seen[0]);
            };
            assert_eq!(
                egress.body,
                dry_run(&endpoint, &payload),
                "and it is these bytes"
            );
            Err("the peer hung up before answering".to_owned())
        };

        let err = call_with(
            &endpoint,
            &payload,
            granted(),
            &ledger,
            &clock(),
            Some(&transport as &Transport<'_>),
        )
        .expect_err("the transport failed");
        assert!(matches!(err, RemoteError::Transport { .. }), "{err:?}");

        // Two lines, not one: a call that disclosed bytes and then failed is a
        // disclosure with a failure attached, never an absence.
        let entries = ledger.read().expect("read");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].call(), entries[1].call());
        let super::Entry::Outcome(outcome) = &entries[1] else {
            panic!("expected an outcome line, got {:?}", entries[1]);
        };
        assert!(!outcome.ok);
        assert_eq!(outcome.response_bytes, 0);
        assert_eq!(outcome.detail, "the peer hung up before answering");
    }

    /// **A response is not an answer until it has been read**, and reading it is
    /// [`response::parse`]'s job rather than the transport's. Held level here
    /// because the seam is where the two could drift: `call_with` returns the
    /// bytes verbatim, so a body the endpoint filled with its own error arrives
    /// as `Ok` — and is still refused, with the endpoint's words rather than a
    /// substituted local answer.
    #[test]
    fn a_transport_success_carrying_no_answer_is_still_a_refusal() {
        let dir = temp_dir("unusable-response");
        let ledger = Ledger::at(dir.join("egress.jsonl"));
        let refusal = r#"{"error":{"message":"quota exhausted"}}"#;
        let transport =
            |_: &Endpoint, _: &str| -> Result<String, String> { Ok(refusal.to_owned()) };

        let raw = call_with(
            &endpoint(),
            &payload(),
            granted(),
            &ledger,
            &clock(),
            Some(&transport as &Transport<'_>),
        )
        .expect("bytes came back, which is all `call_with` claims");
        assert_eq!(raw, refusal, "the transport's bytes are returned verbatim");

        let err = crate::response::parse(&raw).expect_err("those bytes are not an answer");
        assert_eq!(
            err,
            crate::response::ResponseError::EndpointReported {
                message: "quota exhausted".to_owned()
            }
        );

        // The ledger is right either way: bytes did leave, and something did come
        // back. The record answers "what left this machine?", not "was the reply
        // any good?" — and conflating the two would make a refused call look like
        // one that never happened.
        let entries = ledger.read().expect("read");
        assert_eq!(entries.len(), 2);
        let super::Entry::Outcome(outcome) = &entries[1] else {
            panic!("expected an outcome line, got {:?}", entries[1]);
        };
        assert!(outcome.ok, "the transport did succeed");
        assert_eq!(outcome.response_bytes, refusal.len());
    }

    /// **An unrecordable call does not happen.** If the egress line cannot be
    /// written there is no answer to "what left this machine?", so the call is
    /// refused rather than made — the ordering in `call_with` is a policy, not a
    /// convenience, and this is what asserts it.
    #[test]
    fn an_unwritable_ledger_refuses_the_call_rather_than_sending_unrecorded() {
        let dir = temp_dir("unwritable");
        // A *file* where the ledger's parent directory must be, so `create_dir_all`
        // fails without needing to depend on permissions.
        let blocker = dir.join("remote");
        std::fs::write(&blocker, "not a directory").expect("write");
        let ledger = Ledger::at(blocker.join("egress.jsonl"));

        let called = std::cell::Cell::new(false);
        let transport = |_: &Endpoint, _: &str| -> Result<String, String> {
            called.set(true);
            Ok("should never happen".to_owned())
        };
        let err = call_with(
            &endpoint(),
            &payload(),
            granted(),
            &ledger,
            &clock(),
            Some(&transport),
        )
        .expect_err("the ledger is unwritable");

        assert!(matches!(err, RemoteError::Ledger(_)), "{err:?}");
        assert!(!called.get(), "bytes must not leave unrecorded");
    }

    /// The happy path, and the shape of the record it leaves: the egress line is
    /// written **before** the transport runs and carries the body; the outcome
    /// line closes it.
    #[test]
    fn a_successful_call_records_the_egress_before_the_outcome() {
        let dir = temp_dir("success");
        let ledger = Ledger::at(dir.join("egress.jsonl"));
        let endpoint = endpoint();
        let payload = payload();

        // The transport reads the ledger *as it is called*, which is how the
        // ordering is asserted rather than assumed: the egress line has to be on
        // disk already.
        let transport = |_: &Endpoint, _: &str| -> Result<String, String> {
            let seen = ledger.read().expect("read from inside the call");
            assert_eq!(seen.len(), 1, "the egress line is recorded before sending");
            assert!(matches!(seen[0], super::Entry::Egress(_)));
            Ok("the remote answer".to_owned())
        };
        let answer = call_with(
            &endpoint,
            &payload,
            granted(),
            &ledger,
            &clock(),
            Some(&transport as &Transport<'_>),
        )
        .expect("the gate is open and the transport succeeded");
        assert_eq!(answer, "the remote answer");

        let entries = ledger.read().expect("read");
        assert_eq!(entries.len(), 2);
        let super::Entry::Egress(egress) = &entries[0] else {
            panic!("expected an egress line");
        };
        assert_eq!(egress.at, AT);
        assert_eq!(
            egress.endpoint,
            "https://models.example/v1/chat/completions"
        );
        assert_eq!(egress.model, "a-vendor-model");
        assert_eq!(egress.trust, ProducerTrust::VendorAsserted);
        assert_eq!(egress.body, dry_run(&endpoint, &payload));
        assert_eq!(egress.bytes, egress.body.len());
        assert!(egress.fields.contains(&"node keys".to_owned()));
        assert_eq!(entries[1].call(), egress.call, "one call, two lines");
    }

    /// A dry-run is an inspection, not a disclosure: it sends nothing and — the
    /// part worth asserting — records nothing, so the ledger stays a list of
    /// things that actually left.
    #[test]
    fn a_dry_run_sends_nothing_and_records_nothing() {
        let dir = temp_dir("dry-run");
        let ledger = Ledger::at(dir.join("egress.jsonl"));
        let body = dry_run(&endpoint(), &payload());
        assert!(body.contains("adr:0019"), "{body}");
        assert!(ledger.read().expect("read").is_empty());
        assert!(!ledger.path().exists());
    }

    /// **The escalation check cannot open the gate.** A local attempt that fell
    /// short is an input to consent, never a substitute for it — ADR-0019 §1's
    /// whole point, asserted at the seam where the two would be confused.
    #[test]
    fn an_escalation_trigger_does_not_open_the_gate() {
        let fell_short = crate::escalation::check(
            LocalAttempt {
                output_chars: 0,
                tool_calls: 0,
                rounds: 4,
                max_rounds: 4,
            },
            Policy::default(),
        );
        assert!(
            fell_short.fell_short(),
            "the local attempt produced nothing"
        );

        // …and the gate is entirely unmoved by it: same denial, same reason.
        let dir = temp_dir("escalation");
        let ledger = Ledger::at(dir.join("egress.jsonl"));
        let err = call_with(
            &endpoint(),
            &payload(),
            decide(ConfigGrant::from_layers(None, None), Some(true)),
            &ledger,
            &clock(),
            None,
        )
        .expect_err("the user layer never granted");
        assert!(matches!(
            err,
            RemoteError::NotConsented(Reason::UserLayerUnset)
        ));
    }
}

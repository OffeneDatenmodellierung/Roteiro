//! What may be sent — an explicit allow-list, and the exact bytes it produces.
//!
//! ADR-0019 §4 decides this rather than deferring it, "because deferring this is
//! how an egress path ships before its guard". Three rules, all implemented here:
//!
//! 1. A request is assembled from an **explicit allow-list of fields**, never
//!    "whatever the local path happened to build".
//! 2. The **exact payload is inspectable before it is sent** — [`Payload::body`]
//!    is what a dry-run prints *and* what the transport receives, one function,
//!    so the preview cannot drift from the act.
//! 3. The documentation says what actually leaves, including the parts that are
//!    identifying — see [`Payload::disclosure`].
//!
//! # The allow-list is a type, not a review habit
//!
//! [`ContextItem::from_node`] is the only way graph content enters a payload, and
//! it reads **five named fields** off a [`Node`]: `key`, `kind`, `name`, `path`,
//! and `meta.content` (capped). `Node::meta` is a `serde_json::Value` that
//! extraction may put anything into; every other key in it is unreachable from
//! here, and `only_the_allow_listed_fields_of_a_node_reach_the_payload` is the
//! test that keeps it that way.
//!
//! # What the graph holds is narrower than it looks — but not narrow enough
//!
//! Function bodies are not stored. `meta.content` is capped at 1,500 characters
//! and is populated only from prose files, doc-comments, PDF/OCR text and audio
//! summaries; extraction asserts that a `.rs` file node carries no
//! `meta.content`. So a graph-derived prompt carries **symbol names, headings and
//! topics** — not source text.
//!
//! **That is not the same as carrying nothing identifying, and the difference is
//! the disclosure this module owes its reader.** There is no redaction chokepoint
//! on a prompt: extraction redacts secret-*looking* config values before
//! persistence precisely because the store is exportable, but that mechanism does
//! not apply here, and it is weaker than it sounds even where it does —
//! `is_secret_key` matches **key names only**, against ten needles, with no
//! inspection of values, so `DATABASE_URL=postgres://user:hunter2@host` matches
//! none of them. And symbol names alone are commercially sensitive for some
//! users. [`Payload::disclosure`] says all of that in the surface's own words.

use rto_graph::Node;
use serde::{Deserialize, Serialize};

use crate::endpoint::Endpoint;

/// The cap on a single prose excerpt, matching extraction's own `meta.content`
/// cap — so a payload can never carry more prose per node than the graph holds.
pub const MAX_PROSE_EXCERPT: usize = 1_500;

/// The cap on the instruction. Generous, because an instruction is written by
/// the operator and is the one field they can read back in full before sending;
/// bounded, because a payload of unbounded size is a payload nobody inspects.
pub const MAX_INSTRUCTION: usize = 8_000;

/// The cap on context items. A dry-run that nobody reads is not an inspection,
/// and 64 nodes is already more than fits on a screen.
pub const MAX_CONTEXT_ITEMS: usize = 64;

/// One graph node, reduced to the fields that may leave the machine.
///
/// Constructed **only** by [`ContextItem::from_node`]. The fields are public for
/// reading and reporting; there is no public constructor that takes them
/// individually, so no caller can assemble a context item out of something that
/// was not a node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextItem {
    /// The node's key (`sym:rust:…#Store`, `adr:0019`, `file:…`).
    pub key: String,
    /// The node's kind, as its stable token.
    pub kind: String,
    /// The node's human-facing name — a symbol name, a heading, a file name.
    pub name: String,
    /// Repository-relative path, when the node has one.
    pub path: Option<String>,
    /// Up to [`MAX_PROSE_EXCERPT`] characters of `meta.content` — captured prose
    /// from docs, ADRs, blueprints, PDF/OCR text and audio summaries. `None` for
    /// a node with no captured prose, which is every code symbol.
    pub prose: Option<String>,
}

impl ContextItem {
    /// Reduce a node to what may be sent.
    ///
    /// **This is the allow-list.** Five fields, read by name. Anything else
    /// extraction placed in [`Node::meta`] — and it is a free-form
    /// `serde_json::Value`, so that is genuinely anything — is not reachable from
    /// a payload, because there is no code path here that reads it.
    #[must_use]
    pub fn from_node(node: &Node) -> Self {
        Self {
            key: node.key.clone(),
            kind: node.kind.as_str().to_owned(),
            name: node.name.clone(),
            path: node.path.clone(),
            prose: node
                .meta
                .get("content")
                .and_then(serde_json::Value::as_str)
                .map(|text| truncate_chars(text, MAX_PROSE_EXCERPT)),
        }
    }
}

/// Take at most `max` characters, on a character boundary, appending an explicit
/// marker when anything was dropped so a reader of a dry-run can tell a complete
/// excerpt from a clipped one.
fn truncate_chars(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        None => text.to_owned(),
        Some((cut, _)) => format!("{}…[truncated]", &text[..cut]),
    }
}

/// Why a payload could not be assembled.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PayloadError {
    /// An instruction with nothing in it.
    #[error("a remote request needs an instruction: there is nothing to ask")]
    EmptyInstruction,
    /// An instruction past [`MAX_INSTRUCTION`].
    #[error(
        "the instruction is {len} bytes, over the {MAX_INSTRUCTION}-byte limit — \
         a payload too large to read before sending is a payload nobody inspected"
    )]
    InstructionTooLong {
        /// The instruction's length in bytes.
        len: usize,
    },
    /// More than [`MAX_CONTEXT_ITEMS`] nodes.
    #[error(
        "{count} context nodes is over the {MAX_CONTEXT_ITEMS}-node limit — \
         narrow the selection so the dry-run is readable before you send it"
    )]
    TooMuchContext {
        /// How many were offered.
        count: usize,
    },
}

/// Everything a remote call would carry, and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Payload {
    instruction: String,
    context: Vec<ContextItem>,
}

/// The system message every request carries. A constant, so it is part of what a
/// dry-run shows rather than something assembled out of sight.
const SYSTEM_MESSAGE: &str = "You are answering a question about a software repository. \
     You are given node identities from its knowledge graph — keys, kinds, names, paths, and \
     captured prose. You are not given source code. Answer from what you are given, and say so \
     when it is not enough.";

impl Payload {
    /// Assemble a payload from an instruction and the nodes to describe.
    ///
    /// # Errors
    /// [`PayloadError::EmptyInstruction`], [`PayloadError::InstructionTooLong`]
    /// or [`PayloadError::TooMuchContext`].
    pub fn new(instruction: &str, nodes: &[Node]) -> Result<Self, PayloadError> {
        let instruction = instruction.trim();
        if instruction.is_empty() {
            return Err(PayloadError::EmptyInstruction);
        }
        if instruction.len() > MAX_INSTRUCTION {
            return Err(PayloadError::InstructionTooLong {
                len: instruction.len(),
            });
        }
        if nodes.len() > MAX_CONTEXT_ITEMS {
            return Err(PayloadError::TooMuchContext { count: nodes.len() });
        }
        Ok(Self {
            instruction: instruction.to_owned(),
            context: nodes.iter().map(ContextItem::from_node).collect(),
        })
    }

    /// The instruction, verbatim as it will be sent.
    #[must_use]
    pub fn instruction(&self) -> &str {
        &self.instruction
    }

    /// The allow-listed context, in the order it will be sent.
    #[must_use]
    pub fn context(&self) -> &[ContextItem] {
        &self.context
    }

    /// Which classes of information this particular payload actually carries —
    /// the concrete answer for one request, where [`Payload::disclosure`] is the
    /// general one.
    ///
    /// Derived from the content, not declared: a payload with no prose excerpt in
    /// it does not claim to carry prose.
    #[must_use]
    pub fn fields_present(&self) -> Vec<&'static str> {
        let mut fields = vec!["instruction"];
        if !self.context.is_empty() {
            fields.push("node keys");
            fields.push("node names");
        }
        if self.context.iter().any(|c| c.path.is_some()) {
            fields.push("repository-relative paths");
        }
        if self.context.iter().any(|c| c.prose.is_some()) {
            fields.push("captured prose excerpts");
        }
        fields
    }

    /// **The exact bytes that would leave this machine.**
    ///
    /// One function, called by the dry-run and by the call path, so a preview
    /// cannot drift from the act it previews — `the_dry_run_body_is_what_the_transport_receives`
    /// holds them level. The shape is an OpenAI-compatible chat completion, which
    /// is what the endpoints this tier can address accept.
    ///
    /// Deterministic: `serde_json`'s map is a `BTreeMap`, so the key order is
    /// fixed, and `temperature` is `0` because a record of what was sent is worth
    /// less if the same payload cannot be sent again.
    #[must_use]
    pub fn body(&self, endpoint: &Endpoint) -> String {
        let request = serde_json::json!({
            "model": endpoint.model(),
            "messages": [
                { "role": "system", "content": SYSTEM_MESSAGE },
                { "role": "user", "content": self.user_message() },
            ],
            "stream": false,
            "temperature": 0,
        });
        // Serializing a `serde_json::Value` built from owned data cannot fail.
        serde_json::to_string(&request).unwrap_or_default()
    }

    /// The user turn: the instruction, then one block per context node.
    fn user_message(&self) -> String {
        use std::fmt::Write as _;

        let mut text = self.instruction.clone();
        for item in &self.context {
            // Writing to a `String` is infallible.
            let _ = write!(
                text,
                "\n\n--- {} ({})\nname: {}",
                item.key, item.kind, item.name
            );
            if let Some(path) = &item.path {
                let _ = write!(text, "\npath: {path}");
            }
            if let Some(prose) = &item.prose {
                let _ = write!(text, "\n{prose}");
            }
        }
        text
    }

    /// What a person needs to have read before granting this — the general
    /// disclosure, printed beside every dry-run and by `roteiro remote status`.
    ///
    /// It says the reassuring thing and then refuses to stop there, because
    /// ADR-0019 §4 requires exactly that: *"the documentation must say so rather
    /// than implying that 'no source is sent' means 'nothing identifying is
    /// sent'."*
    #[must_use]
    pub fn disclosure() -> &'static str {
        "What leaves this machine when the remote tier sends a request:\n\
         \n\
         \x20 * your instruction, verbatim;\n\
         \x20 * for each node you selected: its graph key, kind, name, and\n\
         \x20   repository-relative path;\n\
         \x20 * up to 1,500 characters of captured prose per node — doc comments,\n\
         \x20   ADR and blueprint text, PDF/OCR text, audio summaries.\n\
         \n\
         Source code is not sent: function bodies are not in the graph, and a\n\
         `.rs` file node carries no captured prose at all.\n\
         \n\
         That is not the same as nothing identifying being sent, and you should\n\
         not read it that way:\n\
         \n\
         \x20 * **Symbol names, file paths and headings identify a codebase.** For\n\
         \x20   some users they are commercially sensitive on their own, without a\n\
         \x20   single line of a function body.\n\
         \x20 * **There is no redaction chokepoint on a prompt.** Extraction\n\
         \x20   redacts secret-*named* config keys before persistence, but that is\n\
         \x20   name-matching over ten needles with no inspection of values — so\n\
         \x20   `DATABASE_URL=postgres://user:hunter2@host` is not redacted, and\n\
         \x20   captured prose containing it would be sent as it stands.\n\
         \n\
         Read the dry-run. It prints the exact bytes."
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ContextItem, MAX_CONTEXT_ITEMS, MAX_INSTRUCTION, MAX_PROSE_EXCERPT, Payload, PayloadError,
    };
    use crate::endpoint::Endpoint;
    use crate::trust::ProducerTrust;
    use rto_graph::{Node, NodeKind, Provenance};

    fn endpoint() -> Endpoint {
        Endpoint::new(
            "https://models.example/v1/chat/completions",
            "a-vendor-model",
            ProducerTrust::VendorAsserted,
        )
        .expect("a valid endpoint")
    }

    /// A node with prose in `meta.content` and a secret in a *different* meta
    /// key — the shape the allow-list exists for.
    fn node(key: &str, meta: serde_json::Value) -> Node {
        Node {
            key: key.to_owned(),
            kind: NodeKind::Doc,
            name: "a heading".to_owned(),
            path: Some("docs/thing.md".to_owned()),
            lang: None,
            blob_hash: Some("cafebabe".to_owned()),
            span: None,
            provenance: Provenance::Authored,
            meta,
        }
    }

    /// **The allow-list, proved by the thing it excludes.** `Node::meta` is a
    /// free-form JSON value, so extraction — now or later — may put anything in
    /// it. Only `meta.content` is reachable from a payload; a sibling key
    /// carrying a credential must not appear in the bytes that leave.
    #[test]
    fn only_the_allow_listed_fields_of_a_node_reach_the_payload() {
        let node = node(
            "doc:docs/thing.md#a-heading",
            serde_json::json!({
                "content": "the prose that may be sent",
                "raw_env": "DATABASE_URL=postgres://user:hunter2@db.internal",
                "sha": "not-sent-either",
            }),
        );
        let payload =
            Payload::new("what is this?", std::slice::from_ref(&node)).expect("assembles");
        let body = payload.body(&endpoint());

        assert!(body.contains("the prose that may be sent"), "{body}");
        assert!(body.contains("doc:docs/thing.md#a-heading"), "{body}");
        assert!(body.contains("docs/thing.md"), "{body}");
        for excluded in ["hunter2", "raw_env", "not-sent-either", "cafebabe"] {
            assert!(
                !body.contains(excluded),
                "`{excluded}` is not on the allow-list but reached the wire: {body}"
            );
        }

        // …and the item itself holds exactly the five allow-listed fields.
        let item = ContextItem::from_node(&node);
        assert_eq!(item.key, "doc:docs/thing.md#a-heading");
        assert_eq!(item.name, "a heading");
        assert_eq!(item.path.as_deref(), Some("docs/thing.md"));
        assert_eq!(item.prose.as_deref(), Some("the prose that may be sent"));
    }

    /// A node with no captured prose contributes no prose — the code-symbol case,
    /// which is most of the graph.
    #[test]
    fn a_node_without_captured_prose_contributes_none() {
        let item = ContextItem::from_node(&node("sym:rust:x#Foo", serde_json::json!({})));
        assert!(item.prose.is_none());
        let payload =
            Payload::new("q", &[node("sym:rust:x#Foo", serde_json::json!({}))]).expect("assembles");
        assert!(
            !payload
                .fields_present()
                .contains(&"captured prose excerpts"),
            "a payload does not claim to carry prose it has none of: {:?}",
            payload.fields_present()
        );
    }

    /// **The dry-run shows the act, not a rendering of it.** The preview and the
    /// sent bytes come from one function, and this is the assertion that keeps
    /// them from drifting: whatever `body` returns is what a transport is handed.
    #[test]
    fn the_dry_run_body_is_what_the_transport_receives() {
        let endpoint = endpoint();
        let payload =
            Payload::new("explain", &[node("adr:0019", serde_json::json!({}))]).expect("assembles");
        let previewed = crate::dry_run(&endpoint, &payload);

        let seen = std::cell::RefCell::new(String::new());
        let transport = |_: &Endpoint, body: &str| -> Result<String, String> {
            seen.borrow_mut().push_str(body);
            Ok("ok".to_owned())
        };
        let dir = crate::testing::temp_dir("dry-run-parity");
        let ledger = crate::Ledger::at(dir.join("egress.jsonl"));
        crate::call_with(
            &endpoint,
            &payload,
            crate::consent::decide(
                crate::ConfigGrant::from_layers(None, Some(true)),
                Some(true),
            ),
            &ledger,
            &|| "2026-08-17T00:00:00Z".to_owned(),
            Some(&transport),
        )
        .expect("the gate is open and a transport was supplied");

        assert_eq!(
            previewed,
            *seen.borrow(),
            "the dry-run must be byte-identical to what was sent"
        );
    }

    /// Prose is capped at the same 1,500 characters extraction caps
    /// `meta.content` at, and a clipped excerpt says it was clipped — a reader
    /// inspecting a dry-run must be able to tell a whole excerpt from a piece of
    /// one.
    #[test]
    fn a_prose_excerpt_is_capped_and_says_when_it_was_clipped() {
        let long = "é".repeat(MAX_PROSE_EXCERPT + 500);
        let item = ContextItem::from_node(&node("doc:x", serde_json::json!({ "content": long })));
        let prose = item.prose.expect("prose");
        assert!(prose.ends_with("…[truncated]"), "clipping is declared");
        assert_eq!(
            prose.chars().count(),
            MAX_PROSE_EXCERPT + "…[truncated]".chars().count(),
            "cut on a character boundary at exactly the cap"
        );

        // Exactly at the cap is *not* clipped, so the marker means what it says.
        let exact = "a".repeat(MAX_PROSE_EXCERPT);
        let item = ContextItem::from_node(&node("doc:y", serde_json::json!({ "content": exact })));
        assert_eq!(item.prose.as_deref(), Some(exact.as_str()));
    }

    /// Every bound refuses rather than trimming: a payload silently shortened is
    /// a payload whose dry-run described something else.
    #[test]
    fn the_payload_bounds_refuse_rather_than_trim() {
        assert_eq!(
            Payload::new("   ", &[]).expect_err("empty"),
            PayloadError::EmptyInstruction
        );
        let long = "x".repeat(MAX_INSTRUCTION + 1);
        assert!(matches!(
            Payload::new(&long, &[]),
            Err(PayloadError::InstructionTooLong { .. })
        ));
        let many: Vec<Node> = (0..=MAX_CONTEXT_ITEMS)
            .map(|i| node(&format!("doc:{i}"), serde_json::json!({})))
            .collect();
        assert!(matches!(
            Payload::new("q", &many),
            Err(PayloadError::TooMuchContext { .. })
        ));
        // The boundary itself is allowed, so the limit is the limit.
        assert!(Payload::new("q", &many[..MAX_CONTEXT_ITEMS]).is_ok());
    }

    /// The disclosure must not stop at the reassuring half. ADR-0019 §4 requires
    /// it to say that symbol names identify a codebase and that there is no
    /// redaction chokepoint on a prompt — including the `DATABASE_URL` case,
    /// which matches none of `is_secret_key`'s ten needles.
    #[test]
    fn the_disclosure_names_the_absent_redaction_chokepoint() {
        let text = Payload::disclosure();
        assert!(text.contains("Source code is not sent"), "{text}");
        assert!(text.contains("no redaction chokepoint"), "{text}");
        assert!(text.contains("DATABASE_URL"), "{text}");
        assert!(text.contains("commercially sensitive"), "{text}");
    }

    /// The declared fields describe this payload, so a reader is told what *this*
    /// request carries rather than what the tier can carry in general.
    #[test]
    fn the_declared_fields_describe_this_payload() {
        let bare = Payload::new("q", &[]).expect("assembles");
        assert_eq!(bare.fields_present(), vec!["instruction"]);

        let rich = Payload::new(
            "q",
            &[node("doc:x", serde_json::json!({ "content": "prose" }))],
        )
        .expect("assembles");
        assert_eq!(
            rich.fields_present(),
            vec![
                "instruction",
                "node keys",
                "node names",
                "repository-relative paths",
                "captured prose excerpts",
            ]
        );
        assert_eq!(rich.instruction(), "q");
        assert_eq!(rich.context().len(), 1);
    }
}

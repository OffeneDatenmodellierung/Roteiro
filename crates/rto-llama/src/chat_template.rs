//! Render a model's own chat template, in Rust (issue #492).
//!
//! # Why this exists
//!
//! `LlamaModel::apply_chat_template` wraps `llama_chat_apply_template`, and
//! `llama.h` states the limitation plainly:
//!
//! > NOTE: This function does not use a jinja parser. It only support a
//! > pre-defined list of template.
//!
//! `llm_chat_detect_template` substring-matches: anything containing
//! `<|im_start|>` becomes `CHATML`, rendered by eight lines of C++. Measured on
//! this repository's registry — qwen3-32b's own **4,100-byte** template comes
//! back as **153 bytes**. The Jinja never runs.
//!
//! The second problem is larger and independent of the first: that function's
//! signature is `(template, messages, add_assistant)`. There is **no `tools`
//! parameter**, so a tool definition cannot reach a model's template through it
//! however the Jinja is handled — and a model trained to receive tools in its
//! template is instead told about them somewhere else, in a shape it was not
//! trained on.
//!
//! # Why in Rust rather than through a binding
//!
//! Asked upstream; declined, as out of scope by design
//! (`utilityai/llama-cpp-rs#1048`). The maintainers' argument was not "not yet":
//!
//! > Handling chat templates, json parsing, tool call parsing and stuff like
//! > that is in my opinion best done in rust, outside of llama.cpp. It's all very
//! > high-level features, and I think this lib is best served by … operating on
//! > the level of logits and tokens, rather than on the level of messages.
//!
//! They also offered evidence that llama.cpp's own implementation is *slower*
//! than minijinja rather than merely out of scope. So this is the recommended
//! architecture, not a workaround for a missing binding.
//!
//! # What the registry actually needs
//!
//! Established by rendering every template in this repository's model registry
//! before choosing the feature set, rather than by reading minijinja's docs:
//!
//! | template | bytes | needs |
//! | --- | --- | --- |
//! | `qwen3-32b` | 4,100 | `tojson` |
//! | `qwen3-coder-30b-a3b` | 6,896 | — |
//! | `qwen3.8-27b` | 8,952 | `tojson`, `startswith`, `raise_exception` |
//! | `smolvlm-500m-gguf` | 403 | — |
//! | `voxtral-mini-3b` | 9,838 | `raise_exception` |
//!
//! `tojson` comes from minijinja's `json` feature; `startswith` is a Python
//! string method that Jinja2 exposes and minijinja does not, supplied by
//! `minijinja-contrib`'s `pycompat` callback. Both were added *because a real
//! template failed without them*, which is why the set is exactly this and not
//! larger.
//!
//! # The callables `transformers` injects (issue #848)
//!
//! `raise_exception` is in neither of those groups: it is not Jinja, and it is
//! not a Python string method. `transformers` puts it into the template
//! environment itself, and a template calls it to **reject** a conversation
//! shape the model was not trained on — `qwen3.8-27b` has nine such branches.
//! Unregistered, minijinja answers `unknown function: raise_exception`, which
//! replaces the template's own explanation at exactly the moment the template
//! was explaining itself. See [`TemplateError::Rejected`].
//!
//! That set is **closed and short**, which is why this is an allowlist rather
//! than a name added each time a model surprises us.
//! `transformers.utils.chat_template_utils._compile_jinja_template` injects
//! precisely three things — the `tojson` filter, and the `raise_exception` and
//! `strftime_now` globals — plus `jinja2.ext.loopcontrols`, which is minijinja's
//! `loop_controls` feature and is already on. [`TRANSFORMERS_CALLABLES`] is that
//! list. A name outside it is one `transformers` never supplied either, so a
//! template calling it is broken against its own renderer and not only against
//! this one — and minijinja's `unknown function` is then the right answer rather
//! than a gap.
//!
//! Surveyed by reading `tokenizer.chat_template` out of every GGUF installed for
//! this registry — the five models the server advertises plus `voxtral-mini-3b`
//! — and extracting every `name(` from each. Two call `raise_exception`;
//! **none** calls `strftime_now`.

use minijinja::{Environment, Error as JinjaError, ErrorKind, Value, context};
use std::sync::{Arc, Mutex, PoisonError};

/// A chat message as a template sees it.
///
/// Deliberately not [`crate::engine::Message`]: a template reads whatever
/// fields the model was trained to expect — `reasoning_content` and `tool_calls`
/// among them — and coupling this to the engine's type would mean every new
/// template field became an engine change.
pub type Message = serde_json::Value;

/// Why a template could not be rendered.
///
/// `#[non_exhaustive]` because the failure modes are genuinely open: a template
/// reaching for a filter this engine does not provide is a new variant waiting
/// to happen, and the registry has already produced two such surprises
/// (`tojson`, `startswith`). That is the opposite of
/// [`crate::chat_template`]'s sibling decision elsewhere in this workspace,
/// where a set closed *by a specification* is left exhaustive on purpose — the
/// question is always who owns the set, not which attribute is safer.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TemplateError {
    /// The template is not valid Jinja.
    #[error("chat template does not parse as Jinja: {0}")]
    Parse(String),
    /// The template parsed but failed while rendering.
    #[error("chat template failed to render: {0}")]
    Render(String),
    /// The template **refused the conversation**, in its own words.
    ///
    /// Not a failure of this renderer. A chat template is a statement of the
    /// shapes its model was trained on, and `raise_exception` is how it says
    /// that a request is outside them — `System message must be at the
    /// beginning.`, `Unexpected message role.` Sibling to
    /// [`Self::Render`] in the type and the opposite of it in meaning: that one
    /// is about this server, this one is about the request, and the caller can
    /// act on this one. `llama::template_failure` is where that becomes a 4xx
    /// rather than a 5xx — named rather than linked, because that module is
    /// behind the `llama` feature and an intra-doc link to it fails
    /// `cargo doc` without it. CI only documents `--all-features`, so the link
    /// compiled everywhere it was checked and nowhere else.
    ///
    /// The payload is the template's argument **verbatim** and nothing else. It
    /// is the whole value of the variant: prefixing it, truncating it or folding
    /// it into a sentence of ours would put this back where it started, with
    /// Roteiro's words in place of the model's.
    #[error("{0}")]
    Rejected(String),
}

/// What this renderer does with one `transformers`-injected callable.
///
/// A typed verdict rather than a prose note, so that [`TRANSFORMERS_CALLABLES`]
/// can *drive* registration instead of merely describing it. The list was
/// documentation before: the registrations repeated the same names separately,
/// so a callable added to the environment without a row here was invisible to
/// both the list and the test that reads it.
///
/// `#[non_exhaustive]` because the set is genuinely open in the one direction
/// that matters: a third verdict — *implemented*, supplying a real value rather
/// than honouring or refusing — is exactly what `strftime_now` becomes the day
/// someone settles which clock and which `strftime` dialect. That is a variant
/// waiting to happen, not a hypothetical, so downstream code must not be written
/// as though these two were all there could be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Support {
    /// Registered and honoured; a call surfaces as [`TemplateError::Rejected`].
    Rejects,
    /// Registered **only to refuse by name**, with this explanation.
    Refused(&'static str),
}

/// The complete set of callables `transformers` injects into a chat template's
/// environment, and what this renderer does with each.
///
/// An allowlist, and an exhaustive one: see the module header for where the set
/// comes from and why it can be closed. Registering a name here — even to refuse
/// it — is what turns minijinja's bare `unknown function` into a sentence that
/// says which of the two things went wrong.
///
/// **This list is the only way a callable gets registered.**
/// `register_transformers_callables` (private) iterates it and matches on
/// [`Support`];
/// there is no `env.add_function` anywhere else in this module. So the two
/// directions are closed by construction rather than by a test: a name in the
/// list that nothing registers cannot exist (the loop registers every row), and
/// a registration absent from the list cannot exist either (the loop is the only
/// registrar). It used to be a `&str` note beside a separate pair of hard-coded
/// `add_function` calls, which made this doc comment a claim rather than a
/// mechanism.
///
/// `tojson` is absent because it is a *filter* rather than a callable and
/// minijinja's `json` feature already supplies it.
pub const TRANSFORMERS_CALLABLES: [(&str, Support); 2] = [
    ("raise_exception", Support::Rejects),
    (
        "strftime_now",
        Support::Refused(
            "strftime_now is a `transformers` template callable Roteiro does not \
             supply: it would stamp the wall clock into the prompt, and Roteiro \
             renders a chat template deterministically. No model in the served \
             registry calls it. Supporting it is a decision about which clock and \
             which strftime dialect, not a missing line",
        ),
    ),
];
/// Why `strftime_now` is registered only to refuse.
///
/// It is the one injected callable this renderer declines, and declining it is a
/// decision rather than an omission. Supplying it means choosing a clock and a
/// `strftime` dialect — Python's has some forty directives, and a partial
/// implementation would put a *wrong date* into a prompt without failing, which
/// is precisely the silent-wrongness this module exists to refuse (see
/// `render_shaped`). It also makes the rendered prompt a function of the wall
/// clock, so two identical requests stop producing identical prefills.
///
/// No template in the surveyed registry calls it, so this costs nothing today.
/// When one does, it needs that decision taken deliberately — not guessed at
/// here — and the message on its [`TRANSFORMERS_CALLABLES`] row is what will say
/// so at the time.
const _WHY_STRFTIME_NOW_IS_REFUSED: () = ();

/// Register the [`TRANSFORMERS_CALLABLES`] on `env`, recording any rejection in
/// `rejection`.
///
/// The side channel is not decoration. minijinja wraps a returned error in its
/// own context — kind, template name, line — so `Error::to_string()` gives
/// `invalid operation: System message must be at the beginning. (in
/// <string>:106)`, and recovering the argument from that means parsing our way
/// back out of a message format we do not own. Writing it down on the way past
/// keeps [`TemplateError::Rejected`]'s promise of *verbatim* exact rather than
/// approximately kept.
///
/// `Arc<Mutex<…>>` rather than a cell because minijinja requires a registered
/// function to be `Send + Sync`. It is uncontended: one environment, one render,
/// one thread.
fn register_transformers_callables(
    env: &mut Environment<'_>,
    rejection: &Arc<Mutex<Option<String>>>,
) {
    for (name, support) in TRANSFORMERS_CALLABLES {
        match support {
            Support::Rejects => {
                let sink = Arc::clone(rejection);
                env.add_function(name, move |message: Value| -> Result<Value, JinjaError> {
                    // `as_str` first so a string argument keeps its exact bytes;
                    // `to_string` only for the template that hands this something
                    // else, which is still better answered with the value than
                    // with nothing.
                    let message = message
                        .as_str()
                        .map_or_else(|| message.to_string(), std::borrow::ToOwned::to_owned);
                    // Recorded *before* returning, because the error below is the
                    // last this code sees of the message.
                    *sink.lock().unwrap_or_else(PoisonError::into_inner) = Some(message.clone());
                    Err(JinjaError::new(ErrorKind::InvalidOperation, message))
                });
            }
            Support::Refused(why) => {
                env.add_function(name, move |_: Value| -> Result<Value, JinjaError> {
                    Err(JinjaError::new(ErrorKind::InvalidOperation, why))
                });
            }
        }
    }
}

/// Whether `template` is Jinja rather than one of llama.cpp's builtin names.
///
/// `resolve_chat_template` falls back to a *name* (`"chatml"`) for a model that
/// embeds no template of its own, and a name is not something to render — it is
/// a key into llama.cpp's table. Rendering it would produce the literal string
/// `chatml` as the entire prompt, which is the kind of failure that looks like a
/// model behaving oddly rather than like a bug.
#[must_use]
pub fn is_jinja(template: &str) -> bool {
    template.contains("{%") || template.contains("{{")
}

#[cfg(test)]
thread_local! {
    /// Renders performed on this thread, so a test can pin how many a tooled
    /// turn costs.
    ///
    /// Thread-local rather than a global counter because the harness runs tests
    /// in parallel and a shared count would race. Test-only: nothing outside the
    /// guard reads it.
    pub(crate) static RENDER_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Render `template` with the model's own Jinja engine semantics.
///
/// A template that uses `tools` receives exactly the JSON the model was trained
/// on — which is the whole point, and the thing `apply_chat_template` cannot do
/// at any setting, because it takes no such argument.
///
/// `None` is **not** passed through as `none`: it is rendered as an empty list,
/// for the reason given at the `tools` binding below. So a template testing
/// `tools is none` never sees a true, and one testing `{% if tools %}` or
/// `tools | length` behaves as it would with no tools at all. The distinction
/// matters only to a template written to branch on the difference, and none in
/// the registry does.
///
/// # Errors
/// [`TemplateError::Parse`] if the template is not valid Jinja,
/// [`TemplateError::Rejected`] if the template called `raise_exception` to
/// refuse this conversation, and [`TemplateError::Render`] if it fails while
/// rendering for any other reason — a missing variable the template
/// dereferences, for instance.
pub fn render(
    template: &str,
    messages: &[Message],
    tools: Option<&serde_json::Value>,
    add_generation_prompt: bool,
) -> Result<String, TemplateError> {
    #[cfg(test)]
    RENDER_CALLS.with(|c| c.set(c.get() + 1));
    let mut env = Environment::new();
    // Set by `raise_exception` on its way past; read only if the render fails.
    // A template that somehow calls it and still renders — no Jinja construct
    // does, but the variable does not depend on that being true — leaves this
    // set and unread, so a successful render is unaffected by its presence.
    let rejection: Arc<Mutex<Option<String>>> = Arc::default();
    register_transformers_callables(&mut env, &rejection);
    // Python string methods (`startswith`, `endswith`, …). Jinja2 exposes them
    // because it runs on Python; minijinja does not, and templates written
    // against Jinja2 use them regardless — `qwen3.8-27b` does.
    env.set_unknown_method_callback(minijinja_contrib::pycompat::unknown_method_callback);
    // Match Jinja2's whitespace handling. Chat templates are whitespace-exact:
    // a stray newline before `<|im_start|>` is a token the model was not trained
    // to see there.
    env.set_lstrip_blocks(true);
    env.set_trim_blocks(true);

    let tmpl = env
        .template_from_str(template)
        .map_err(|e| TemplateError::Parse(e.to_string()))?;
    // An **empty list**, never `none`, when there are no tools. `qwen3-coder-30b-a3b`
    // guards with `tools is iterable and tools | length > 0`; Jinja2 short-circuits
    // that because `none is iterable` is false, and minijinja does not — so `none`
    // reaches `| length` and the render fails outright.
    //
    // An empty list is correct for both engines rather than a workaround for one:
    // `{% if tools %}` is false for `[]` exactly as it is for `none`, and every
    // template that iterates gets something iterable. Found by rendering the real
    // templates *without* tools — the scratch experiment that chose minijinja
    // always passed some, and so never reached this line.
    let empty = serde_json::Value::Array(Vec::new());
    let tools = tools.unwrap_or(&empty);
    tmpl.render(context! {
        messages => messages,
        tools => tools,
        add_generation_prompt => add_generation_prompt,
        // Off unless a caller asks for it. A template that branches on this
        // (qwen3 does) otherwise emits an empty `<think></think>` block the
        // model then has to continue from.
        enable_thinking => false,
    })
    .map_err(|e| {
        // The recorded message wins over minijinja's rendering of the same
        // error, which is the point: one is the template's sentence and the
        // other is that sentence wrapped in `invalid operation: … (in
        // <string>:106)`.
        match rejection
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            Some(message) => TemplateError::Rejected(message),
            None => TemplateError::Render(e.to_string()),
        }
    })
}

/// The plain tool advertisement, used wherever the template will not carry the
/// tools itself.
///
/// Deliberately plain, and deliberately the *only* wording: a model whose
/// template says nothing about tools was not trained on a tool-use format
/// either, so there is no house style to match — but two different wordings for
/// one set of tools would be a contradiction the model has to resolve.
#[must_use]
pub fn tool_advertisement(tools: &serde_json::Value) -> String {
    format!(
        "You may call these tools. Reply with only a tool call, as \
         `<tool_call>{{\"name\": \"<tool>\", \"arguments\": {{ \u{2026} }}}}</tool_call>`.\n{tools}"
    )
}

/// The conversation's operative text — the last message carrying non-empty
/// string content.
///
/// The last one because that is the turn a template cannot discard: a template
/// may legitimately drop or merge *system* messages (several do), but none
/// renders a conversation without its final turn. Checking every message would
/// call those templates broken.
fn operative_text(messages: &[Message]) -> Option<&str> {
    messages
        .iter()
        .rev()
        .find_map(|m| m.get("content")?.as_str())
        .map(str::trim)
        .filter(|t| !t.is_empty())
}

/// The same conversation with each string `content` wrapped as a single text
/// part, `[{"type": "text", "text": …}]`.
///
/// The other convention a chat template may be written against. A template for a
/// multimodal model typically indexes `content[0]['type']` and iterates the
/// parts, because a turn can hold an image beside its text; one for a text model
/// just interpolates `content`. Nothing in a GGUF says which a template expects.
fn as_content_parts(messages: &[Message]) -> Vec<Message> {
    messages
        .iter()
        .map(|m| {
            let Some(text) = m.get("content").and_then(|c| c.as_str()) else {
                return m.clone();
            };
            let mut out = m.clone();
            out["content"] = serde_json::json!([{"type": "text", "text": text}]);
            out
        })
        .collect()
}

/// Render `template`, in whichever content shape it was written for.
///
/// A template that expects parts and is handed a string does not fail: Jinja
/// iterates the string's *characters*, none of which has a `['type']`, so every
/// branch falls through and the turn renders as nothing at all. `smolvlm-500m`
/// does exactly this — the whole user message disappears and the prompt comes
/// back as `<|im_start|>User: <end_of_utterance>\nAssistant:`. Nothing errors,
/// which is why this has to be detected rather than caught.
///
/// So the shape is settled by trying: whichever rendering keeps the operative
/// text is the one the template was written for. `apply_chat_template` never met
/// this problem because llama.cpp's C++ renderer concatenates strings and never
/// looks at a part.
///
/// # Errors
/// [`TemplateError::Render`] if neither shape keeps the conversation. That is a
/// loud failure in place of a prompt with the user's question missing from it,
/// which is the trade this module exists to make.
fn render_shaped(
    template: &str,
    messages: &[Message],
    tools: Option<&serde_json::Value>,
    add_generation_prompt: bool,
) -> Result<String, TemplateError> {
    let plain = render(template, messages, tools, add_generation_prompt)?;
    let Some(text) = operative_text(messages) else {
        return Ok(plain);
    };
    if plain.contains(text) {
        return Ok(plain);
    }
    let parts = as_content_parts(messages);
    match render(template, &parts, tools, add_generation_prompt) {
        Ok(shaped) if shaped.contains(text) => return Ok(shaped),
        // A refusal is an **answer**, and the retry is the only place it is
        // spoken. This arm used to be `if let Ok(shaped) = …`, which discarded
        // every error the retry produced and reported the generic message below
        // — so a template saying `parts shape is unsupported by this model` was
        // replaced by Roteiro guessing that the template "rendered the
        // conversation away". Measured: it does happen, and the sentence the
        // template wrote reached nobody.
        //
        // Only a rejection is promoted. A `Parse`/`Render` from the retry is a
        // second failure of a shape we chose to try, and the message below —
        // which reports that *both* shapes failed — describes that better than
        // either failure alone does.
        Err(e @ TemplateError::Rejected(_)) => return Err(e),
        Ok(_) | Err(_) => {}
    }
    Err(TemplateError::Render(
        "the template rendered the conversation away: the last turn's text is \
         absent from the prompt in both content shapes (plain string and \
         `[{type: text}]` parts). Sending this would ask the model to answer a \
         question it was never given."
            .to_owned(),
    ))
}

/// The prompt with `tools` applied, plus whether the template ignored them.
///
/// Settled by rendering twice and comparing, which is exact: if passing the
/// tools changes no byte of the output, they reached nothing. Searching the
/// prompt for the tool names cannot say this — a tool called `search` may appear
/// in the conversation by coincidence and would read as an advertisement the
/// template never made.
///
/// Returns the tooled rendering rather than only the verdict, because the caller
/// needs exactly that string when the template did use the tools. Computing it
/// and throwing it away cost a third full render on every tooled turn — the
/// common path — which at issue #578's prefill economics is not a rounding
/// error.
///
/// A template that *fails* to render without tools is plainly using them, so an
/// error there answers "not ignored" rather than propagating a second failure.
fn render_and_test_tools(
    template: &str,
    messages: &[Message],
    tools: &serde_json::Value,
    add_generation_prompt: bool,
) -> Result<(String, bool), TemplateError> {
    let with = render_shaped(template, messages, Some(tools), add_generation_prompt)?;
    let ignored = matches!(
        render_shaped(template, messages, None, add_generation_prompt),
        Ok(without) if without == with
    );
    Ok((with, ignored))
}

/// Render `template`, guaranteeing that advertised `tools` reach the prompt.
///
/// A chat template is free to ignore `tools`. Nothing in the format obliges a
/// model to have been trained with them, and a model Roteiro has never seen may
/// simply never reference the variable — in which case the tools reach nothing
/// and the caller has no way to tell: the render succeeds and returns a prompt
/// that looks complete.
///
/// So the tools are advertised here, once, only when the template declines to.
/// That is what lets every other layer stop advertising them defensively: the
/// prompt states each tool exactly once, in the model's own trained format where
/// it has one and in [`tool_advertisement`]'s plain form where it does not.
///
/// # Errors
/// [`TemplateError`] if the template does not parse or does not render — the
/// same failures as [`render`], since this is that call with at most one extra
/// message in front of it.
pub fn render_advertising(
    template: &str,
    messages: &[Message],
    tools: Option<&serde_json::Value>,
    add_generation_prompt: bool,
) -> Result<String, TemplateError> {
    // An empty array is "no tools", not "an empty list of tools". Advertising it
    // would hand the model `You may call these tools.` followed by `[]`, and
    // `ignores_tools` would compare two renderings that differ in nothing.
    let tools = tools.filter(|t| !t.as_array().is_some_and(Vec::is_empty));
    let Some(t) = tools else {
        return render_shaped(template, messages, None, add_generation_prompt);
    };
    let (rendered, ignored) = render_and_test_tools(template, messages, t, add_generation_prompt)?;
    if !ignored {
        return Ok(rendered);
    }

    // Folded into the caller's own system turn where there is one, rather than
    // added as a second. A chat template is entitled to reject a system message
    // that is not the first, or to allow only one, and several do — so inserting
    // a turn to describe the tools could make a template that renders perfectly
    // well stop rendering at all. Prepended within that turn so the tools are
    // stated before the instructions that refer to them.
    let mut announced = messages.to_vec();
    match announced
        .first_mut()
        .filter(|m| m.get("role").and_then(serde_json::Value::as_str) == Some("system"))
        .and_then(|m| Some((m.get("content")?.as_str()?.to_owned(), m)))
    {
        Some((existing, first)) => {
            first["content"] =
                serde_json::Value::String(format!("{}\n\n{existing}", tool_advertisement(t)));
        }
        // No system turn to fold into — or one whose content is not a plain
        // string, which this must not flatten.
        None => announced.insert(
            0,
            serde_json::json!({"role": "system", "content": tool_advertisement(t)}),
        ),
    }
    // `announced` is no longer the caller's conversation: the turn above is ours.
    // So a refusal of it is **not** evidence that the caller sent something the
    // model will not accept — and we can prove that here rather than guess it,
    // because `rendered` above is the *same* conversation without our turn and it
    // rendered. Exactly the two-renders-and-compare argument `render_and_test_tools`
    // already uses to settle whether a template read the tools, applied to blame
    // instead of to content.
    //
    // Reported as a `Render` — a fault of this renderer, a 5xx — because that is
    // whose it is. Letting the `Rejected` through would send the template's
    // sentence to a client as a `400` about a turn the client never wrote, and
    // "System message must be at the beginning." is unanswerable advice when the
    // misplaced message is Roteiro's own.
    render_shaped(template, &announced, Some(t), add_generation_prompt).map_err(|e| match e {
        TemplateError::Rejected(message) => TemplateError::Render(format!(
            "the template accepted this conversation and then refused it once \
             Roteiro added its own `system` turn advertising the tools, saying: \
             {message} The template does not read `tools`, so the tools can only \
             reach this model as a conversation turn, and this template will not \
             take one there. Nothing was sent to the model. This is Roteiro's to \
             fix, not the caller's — the turn it objects to is not theirs."
        )),
        other => other,
    })
}

#[cfg(test)]
mod render_cost {
    use super::{RENDER_CALLS, render_advertising};

    /// A tooled turn renders the template **twice**, not three times.
    ///
    /// Two is the floor: deciding whether a template uses `tools` means
    /// rendering with them and without them and comparing. What is avoidable is
    /// a third render to produce the prompt, when the first render already is
    /// that prompt. It was three until review pointed it out — on the common
    /// path of every tooled chat, where #578 measures prefill in seconds.
    #[test]
    fn a_tooled_turn_renders_twice_not_three_times() {
        let uses_tools = "{%- if tools %}<tools>{{ tools|length }}</tools>{%- endif %}\
                          {%- for m in messages %}{{ m.content }}{%- endfor %}";
        let tools = serde_json::json!([{
            "type": "function",
            "function": {"name": "roteiro_search", "description": "d", "parameters": {}}
        }]);
        let msgs = vec![serde_json::json!({"role": "user", "content": "what is fn foo?"})];

        RENDER_CALLS.with(|c| c.set(0));
        let out = render_advertising(uses_tools, &msgs, Some(&tools), true).expect("renders");
        let calls = RENDER_CALLS.with(std::cell::Cell::get);

        assert!(out.contains("<tools>"), "the template did advertise: {out}");
        assert_eq!(
            calls, 2,
            "a tooled turn cost {calls} renders; two is the floor, and three means \
             the prompt was rendered again after the comparison already produced it"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msgs() -> Vec<Message> {
        vec![
            serde_json::json!({"role": "system", "content": "You are helpful."}),
            serde_json::json!({"role": "user", "content": "Where is Lisbon?"}),
        ]
    }

    /// A ChatML-shaped template renders its messages, in order, with its own
    /// delimiters.
    #[test]
    fn a_jinja_template_renders_its_messages() {
        let t = "{%- for m in messages %}<|im_start|>{{ m.role }}\n{{ m.content }}<|im_end|>\n{%- endfor %}";
        let out = render(t, &msgs(), None, false).expect("render");
        assert!(
            out.contains("<|im_start|>system\nYou are helpful."),
            "{out}"
        );
        assert!(out.contains("<|im_start|>user\nWhere is Lisbon?"), "{out}");
        assert!(
            out.find("system") < out.find("user"),
            "message order must be preserved: {out}"
        );
    }

    /// The reason this module exists: a tool definition reaches the template.
    ///
    /// `apply_chat_template` takes no `tools` argument, so this assertion cannot
    /// be satisfied through it at any setting.
    #[test]
    fn tools_reach_the_template() {
        let t = "{%- if tools %}TOOLS:{% for x in tools %} {{ x.function.name }}{% endfor %}{%- endif %}";
        let tools = serde_json::json!([
            {"type": "function", "function": {"name": "search"}},
            {"type": "function", "function": {"name": "explain"}}
        ]);
        let out = render(t, &msgs(), Some(&tools), false).expect("render");
        assert_eq!(out, "TOOLS: search explain", "{out}");

        // …and a template that is given none renders as though tools were absent,
        // rather than emitting an empty tools block.
        assert_eq!(render(t, &msgs(), None, false).expect("render"), "");
    }

    /// `tojson` — needed by two of the three templates in the registry.
    #[test]
    fn the_tojson_filter_is_available() {
        let t = "{{ tools | tojson }}";
        let tools = serde_json::json!([{"name": "search"}]);
        let out = render(t, &[], Some(&tools), false).expect("render");
        assert!(out.contains("\"search\""), "{out}");
    }

    /// `startswith` — a Python string method `qwen3.8-27b` uses, which minijinja
    /// does not provide without the pycompat callback.
    ///
    /// The result renders as `True`, not `true`: pycompat gives Python's
    /// semantics throughout, which is what a template authored against Jinja2
    /// was written to expect. Pinned rather than adjusted, because a later
    /// change to plain minijinja would silently flip every boolean a template
    /// prints.
    #[test]
    fn python_string_methods_are_available() {
        let t = "{{ messages[0].content.startswith('You') }}";
        let out = render(t, &msgs(), None, false).expect("render");
        assert_eq!(out, "True", "{out}");

        // The method is genuinely evaluated, not merely accepted.
        let f = "{{ messages[0].content.startswith('zzz') }}";
        assert_eq!(render(f, &msgs(), None, false).expect("render"), "False");
    }

    /// A builtin *name* is not a template, and must not be rendered as one.
    #[test]
    fn a_builtin_template_name_is_not_jinja() {
        assert!(!is_jinja("chatml"));
        assert!(!is_jinja("llama3"));
        assert!(is_jinja("{%- if tools %}"));
        assert!(is_jinja("{{ messages }}"));
    }

    /// A template that does not parse says so, rather than producing a prompt
    /// that happens to be its own source.
    #[test]
    fn an_unparseable_template_is_an_error() {
        let e = render("{% for m in messages %}unclosed", &msgs(), None, false)
            .expect_err("must not render");
        assert!(matches!(e, TemplateError::Parse(_)), "{e:?}");
    }
}

/// The callables `transformers` injects, and the rejection/failure split they
/// make possible (issue #848).
#[cfg(test)]
mod transformers_callables {
    use super::{
        Message, Support, TRANSFORMERS_CALLABLES, TemplateError, render, render_advertising,
    };

    /// The sentence a template is trying to say when it calls `raise_exception`.
    ///
    /// Deliberately long, deliberately punctuated, and deliberately not a word
    /// this module uses elsewhere: every one of those is a way a message can be
    /// quietly truncated, re-cased or swallowed, and an assertion on `"nope"`
    /// would notice none of them.
    const REFUSAL: &str =
        "this model does not support a tool response without a preceding tool call";

    fn msgs() -> Vec<Message> {
        vec![serde_json::json!({"role": "user", "content": "Where is Lisbon?"})]
    }

    /// The template's own message reaches the caller **verbatim**.
    ///
    /// The heart of #848. Unregistered, `raise_exception` produced
    /// `unknown function: raise_exception is unknown (in <string>:106)` — the
    /// template's explanation replaced by a complaint about the template, at
    /// precisely the moment the template was explaining itself.
    ///
    /// Byte equality rather than `contains`, and then three explicit absences:
    /// `contains` would pass just as happily on minijinja's
    /// `invalid operation: <message> (in <string>:1)`, which is the wrapped form
    /// this variant exists to avoid and the reason the message is captured on
    /// its way past rather than parsed back out of an error.
    #[test]
    fn a_rejection_carries_the_templates_own_message_verbatim() {
        let t = format!("{{{{ raise_exception('{REFUSAL}') }}}}");
        let e = render(&t, &msgs(), None, false).expect_err("must not render");
        let TemplateError::Rejected(message) = &e else {
            panic!("a raise_exception call must be a rejection, not {e:?}");
        };
        assert_eq!(message, REFUSAL);
        for decoration in ["invalid operation", "<string>", "unknown function"] {
            assert!(
                !message.contains(decoration),
                "the template's message must arrive undecorated, but it carries \
                 `{decoration}`: {message}"
            );
        }
        // And through `Display`, which is what the caller formats.
        assert_eq!(e.to_string(), REFUSAL);
    }

    /// The message survives the tooled path too.
    ///
    /// `render_advertising` is what `rto-llama` actually calls, and it renders up
    /// to three times through two more functions. A rejection that is verbatim in
    /// `render` and lost one layer up would be verbatim where nothing reads it.
    #[test]
    fn a_rejection_survives_the_tooled_path() {
        let t = format!("{{{{ tools | length }}}}{{{{ raise_exception('{REFUSAL}') }}}}");
        let tools = serde_json::json!([{"type": "function", "function": {"name": "search"}}]);
        let e = render_advertising(&t, &msgs(), Some(&tools), true).expect_err("must not render");
        assert!(
            matches!(&e, TemplateError::Rejected(m) if m == REFUSAL),
            "{e:?}"
        );
    }

    /// The negative control: a template that never calls `raise_exception`
    /// renders exactly as it did before the callables were registered.
    ///
    /// Exact output, not a `contains`: registering functions on the environment
    /// is a change to every render in the process, and the risk worth guarding is
    /// not that it errors but that it perturbs — an extra byte, a lost newline. A
    /// chat template is whitespace-exact, so a byte is a token.
    ///
    /// The fixture puts its block tags on their own lines and **indents** them,
    /// the way every template in `tests/fixtures/templates` does, because that is
    /// the only arrangement in which either whitespace setting is observable at
    /// all: `trim_blocks` needs a newline after a tag to eat, and `lstrip_blocks`
    /// needs leading indentation to strip. A flush-left one-liner renders
    /// identically with both settings on or off, so it would hold the output
    /// still while holding none of the engine configuration that produces it —
    /// and that configuration is what registration now sits beside. Measured:
    /// turning off `lstrip_blocks` alone left the flush-left version green.
    #[test]
    fn a_template_that_does_not_raise_renders_unchanged() {
        let t = "{% for m in messages %}\n  {% if m.content %}\n<|im_start|>{{ m.role }}\n{{ m.content }}<|im_end|>\n  {% endif %}\n{% endfor %}";
        let out = render(t, &msgs(), None, false).expect("render");
        assert_eq!(out, "<|im_start|>user\nWhere is Lisbon?<|im_end|>\n");
    }

    /// A genuine rendering failure is still a rendering failure — even in a
    /// template that *contains* a `raise_exception` it never reaches.
    ///
    /// The untaken branch is the whole test. The rejection is recorded by a side
    /// channel, and the failure mode a side channel has is being consulted when
    /// nothing wrote to it: a template holding both a rejection branch and a real
    /// bug would then report the branch it did not take. Every registry template
    /// that raises has eight more branches that do not.
    #[test]
    fn an_internal_failure_in_a_raising_template_is_still_a_render_error() {
        let t = "{%- if false %}{{ raise_exception('not this one') }}{%- endif %}\
                 {{ no_such_helper('x') }}";
        let e = render(t, &msgs(), None, false).expect_err("must not render");
        let TemplateError::Render(message) = &e else {
            panic!("an unreached rejection branch must not become one: {e:?}");
        };
        assert!(
            message.contains("no_such_helper"),
            "the failure must name what was missing: {message}"
        );
        assert!(
            !message.contains("not this one"),
            "the untaken branch's message must not be reported: {message}"
        );
    }

    /// A name outside the allowlist is refused as an unknown function, and that
    /// is the intended answer rather than a gap.
    ///
    /// `transformers` injects three things and no more, so a template calling a
    /// fourth is broken against its own renderer too. This is the "clear refusal
    /// for the rest" half of the allowlist, and it stays a `Render` — a statement
    /// about this server — rather than a `Rejected`.
    #[test]
    fn a_callable_outside_the_allowlist_is_an_unknown_function() {
        let e = render("{{ chat_template_kwargs('x') }}", &msgs(), None, false)
            .expect_err("must not render");
        assert!(
            matches!(&e, TemplateError::Render(m) if m.contains("unknown")),
            "{e:?}"
        );
    }

    /// `strftime_now` is refused **by name**, not by silence.
    ///
    /// It is the second of the three injected names and the one this renderer
    /// declines; see its [`super::TRANSFORMERS_CALLABLES`] row for why declining is the
    /// decision. A refusal that says which callable and why is the difference
    /// between this and the `unknown function` that started #848 — and it is a
    /// `Render`, because a callable Roteiro does not supply is a fact about
    /// Roteiro.
    #[test]
    fn strftime_now_is_refused_by_name() {
        let e = render("{{ strftime_now('%Y-%m-%d') }}", &msgs(), None, false)
            .expect_err("must not render");
        let TemplateError::Render(message) = &e else {
            panic!("a callable this server declines is not a rejection: {e:?}");
        };
        assert!(message.contains("strftime_now"), "{message}");
        assert!(
            message.contains("which clock"),
            "the refusal must say what the decision is, not only that there was \
             one: {message}"
        );
        assert!(!message.contains("unknown function"), "{message}");
    }

    /// A refusal raised by the **parts retry** reaches the caller, instead of
    /// being replaced by Roteiro's guess.
    ///
    /// `render_shaped` retries in `[{type: text}]` shape when the plain render
    /// drops the operative text. That retry's error used to be discarded wholesale
    /// by an `if let Ok(…)`, so a template that refused the parts shape *in
    /// words* had those words replaced by the generic "rendered the conversation
    /// away" sentence — the precise failure #848 exists to stop, surviving inside
    /// the fix for it.
    ///
    /// The fixture renders each turn to a fixed token when content is a string —
    /// so the plain render succeeds and drops the text, forcing the retry — and
    /// raises when it is a list.
    #[test]
    fn a_refusal_from_the_parts_retry_is_not_swallowed() {
        let t = "{%- for m in messages %}{%- if m.content is string %}<turn/>\
                 {%- else %}{{- raise_exception('parts shape unsupported here') }}\
                 {%- endif %}{%- endfor %}";
        let e = render_advertising(t, &msgs(), None, false).expect_err("must not render");
        assert!(
            matches!(&e, TemplateError::Rejected(m) if m == "parts shape unsupported here"),
            "the retry's refusal must survive, but got: {e:?}"
        );
    }

    /// A **non-refusal** failure in the parts retry does *not* displace the
    /// "rendered the conversation away" message.
    ///
    /// The negative control for the test above, and it took two attempts. The
    /// first fixture's retry *succeeded* (it rendered, just without the text), so
    /// promoting every retry error instead of only a rejection left it green —
    /// the control pinned nothing about which errors are promoted. Measured: the
    /// injection ran clean.
    ///
    /// This fixture's retry fails for an ordinary reason — an unknown function,
    /// reachable only in the parts shape — so the two behaviours give different
    /// answers and the fixture *contains the difference*. Promoting it would
    /// report `unknown function` to a caller whose real problem is that neither
    /// shape carried their question.
    #[test]
    fn an_ordinary_retry_failure_does_not_displace_the_both_shapes_message() {
        let t = "{%- for m in messages %}{%- if m.content is string %}<turn/>\
                 {%- else %}{{- no_such_helper(m) }}{%- endif %}{%- endfor %}";
        let e = render_advertising(t, &msgs(), None, false).expect_err("must not render");
        let TemplateError::Render(message) = &e else {
            panic!("an unknown function is not a refusal: {e:?}");
        };
        assert!(
            message.contains("rendered the conversation away"),
            "the caller's real problem is the missing question, not the retry's \
             own failure: {message}"
        );
        assert!(!message.contains("no_such_helper"), "{message}");
    }

    /// A refusal of **Roteiro's own** advertisement turn is reported as Roteiro's
    /// fault, not the caller's (issues #848, #853).
    ///
    /// This is the one injection point where causation is *exactly* known rather
    /// than guessed: the same conversation without our turn rendered a moment
    /// earlier, so the turn is the only thing that changed. The fixture is a
    /// template that ignores `tools` — which is what makes Roteiro splice an
    /// advertisement turn in at all — and refuses a `system` message, as
    /// `qwen3.8-27b` does.
    ///
    /// `Render` rather than `Rejected`, because the status code follows the
    /// variant one layer up: a `Rejected` here would become a `400` telling the
    /// caller to fix a turn Roteiro wrote.
    #[test]
    fn a_refusal_of_our_own_advertisement_turn_is_ours_not_the_callers() {
        let refuses_system = "{%- for m in messages %}{%- if m.role == 'system' %}\
             {{- raise_exception('this model takes no system messages') }}{%- endif %}\
             {{- m.content }}{%- endfor %}";
        let tools = serde_json::json!([{
            "type": "function",
            "function": {"name": "roteiro_search", "description": "d", "parameters": {}}
        }]);
        let user_only = vec![serde_json::json!({"role": "user", "content": "Where is Lisbon?"})];

        // Without tools the very same conversation renders — so the difference is
        // ours, and the test is not merely observing a broken template.
        render(refuses_system, &user_only, None, false).expect("renders without our turn");

        let e = render_advertising(refuses_system, &user_only, Some(&tools), false)
            .expect_err("our injected system turn is refused");
        let TemplateError::Render(message) = &e else {
            panic!("a refusal of Roteiro's own turn must not be billed to the caller: {e:?}");
        };
        assert!(
            message.contains("this model takes no system messages"),
            "the template's own words are still the answer: {message}"
        );
        assert!(
            message.contains("Roteiro"),
            "the message must name whose turn was refused: {message}"
        );
    }

    /// Every row of [`super::TRANSFORMERS_CALLABLES`] reaches the environment.
    ///
    /// The list *drives* registration now, so the two failure modes a hand-kept
    /// allowlist has — a name listed but not registered, a name registered but
    /// not listed — are both unrepresentable, and this test cannot be the thing
    /// that catches them because nothing can: the loop is the only registrar.
    ///
    /// What is left for a test is narrower than it looks, and worth being exact
    /// about. Measured by injection: mis-typing a row (`Rejects` where `Refused`
    /// was meant) does **not** fail here, because registration follows the row —
    /// both sides move together and the test agrees with itself. That case is
    /// caught by `strftime_now_is_refused_by_name`, which pins the refusal's
    /// words rather than its shape.
    ///
    /// So this guards the one thing construction cannot: that the loop is
    /// *reached at all*. Skipping the `register_transformers_callables` call is
    /// the whole of #848 in one line, and it fails here.
    #[test]
    fn every_allowlisted_callable_behaves_as_its_row_says() {
        for (name, support) in TRANSFORMERS_CALLABLES {
            let e = render(&format!("{{{{ {name}('x') }}}}"), &msgs(), None, false)
                .expect_err("each of these refuses by design");
            assert!(
                !e.to_string().contains("unknown"),
                "`{name}` is allow-listed but not registered: {e}"
            );
            match support {
                Support::Rejects => assert!(
                    matches!(&e, TemplateError::Rejected(m) if m == "x"),
                    "`{name}` is listed as honouring the template's message, but \
                     answered: {e:?}"
                ),
                Support::Refused(why) => assert!(
                    matches!(&e, TemplateError::Render(m) if m.contains(why)),
                    "`{name}` is listed as refused, but answered: {e:?}"
                ),
            }
        }
    }
}

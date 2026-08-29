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
//! | `qwen3.8-27b` | 8,952 | `tojson`, `startswith` |
//!
//! `tojson` comes from minijinja's `json` feature; `startswith` is a Python
//! string method that Jinja2 exposes and minijinja does not, supplied by
//! `minijinja-contrib`'s `pycompat` callback. Both were added *because a real
//! template failed without them*, which is why the set is exactly this and not
//! larger.

use minijinja::{Environment, context};

/// A chat message as a template sees it.
///
/// Deliberately not [`crate::engine::ChatMessage`]: a template reads whatever
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

/// Render `template` with the model's own Jinja engine semantics.
///
/// `tools` is passed through untouched. A template that ignores it renders as
/// though it were absent, and one that uses it receives exactly the JSON the
/// model was trained on — which is the whole point, and the thing
/// `apply_chat_template` cannot do at any setting because it takes no such
/// argument.
///
/// # Errors
/// [`TemplateError::Parse`] if the template is not valid Jinja, and
/// [`TemplateError::Render`] if it fails while rendering — a missing variable
/// the template dereferences, for instance.
pub fn render(
    template: &str,
    messages: &[Message],
    tools: Option<&serde_json::Value>,
    add_generation_prompt: bool,
) -> Result<String, TemplateError> {
    let mut env = Environment::new();
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
    .map_err(|e| TemplateError::Render(e.to_string()))
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
    if let Ok(shaped) = render(template, &parts, tools, add_generation_prompt)
        && shaped.contains(text)
    {
        return Ok(shaped);
    }
    Err(TemplateError::Render(
        "the template rendered the conversation away: the last turn's text is \
         absent from the prompt in both content shapes (plain string and \
         `[{type: text}]` parts). Sending this would ask the model to answer a \
         question it was never given."
            .to_owned(),
    ))
}

/// True when `template` makes no use of `tools` at all.
///
/// Settled by rendering twice and comparing, which is exact: if passing the
/// tools changes no byte of the output, they reached nothing. Searching the
/// prompt for the tool names cannot say this — a tool called `search` may appear
/// in the conversation by coincidence and would read as an advertisement the
/// template never made.
///
/// A template that *fails* to render without tools is plainly using them, so an
/// error here answers "not ignored" rather than propagating a second failure.
fn ignores_tools(
    template: &str,
    messages: &[Message],
    tools: &serde_json::Value,
    add_generation_prompt: bool,
) -> bool {
    let Ok(with) = render_shaped(template, messages, Some(tools), add_generation_prompt) else {
        return false;
    };
    matches!(
        render_shaped(template, messages, None, add_generation_prompt),
        Ok(without) if without == with
    )
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
    if !ignores_tools(template, messages, t, add_generation_prompt) {
        return render_shaped(template, messages, Some(t), add_generation_prompt);
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
    render_shaped(template, &announced, Some(t), add_generation_prompt)
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

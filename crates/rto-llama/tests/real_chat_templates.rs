//! Guard: every chat template this repository's registry ships actually renders
//! (issue #492).
//!
//! # Why the real templates and not fixtures
//!
//! The unit tests beside [`rto_llama::chat_template`] use small hand-written
//! templates, and they pass against a renderer that cannot handle any real one.
//! `qwen3-32b` needs the `tojson` filter; `qwen3.8-27b` additionally calls
//! `startswith`, a Python string method minijinja does not provide. Both were
//! discovered by rendering the actual templates and watching them fail — not by
//! reading documentation, and not by any fixture small enough to write by hand.
//!
//! So these are the genuine articles, extracted from the GGUFs' embedded
//! `tokenizer.chat_template` metadata and vendored verbatim — each one's source
//! model and GGUF recorded in `tests/fixtures/templates/PROVENANCE.md`, which
//! also says why they are not rows in the native-dependency register. They are the
//! artefact the renderer must handle, and shrinking them to something
//! comfortable would be shrinking the test's subject.
//!
//! # What this replaces
//!
//! `LlamaModel::apply_chat_template` runs no Jinja: it substring-matches for
//! `<|im_start|>` and renders eight lines of C++, so qwen3-32b's 4,100 bytes of
//! template come back as 153. It also takes **no `tools` argument**, so the
//! `tools_reach_every_template` case below cannot be satisfied through it at any
//! setting — which is the substantive reason this module exists rather than the
//! Jinja one.

use std::path::{Path, PathBuf};

/// Where the vendored templates live.
fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/templates")
}

/// Every vendored template, as `(name, source)`.
///
/// Reads the directory rather than listing names: a template added to the
/// registry and vendored here is then covered without anyone remembering to
/// extend a list, which is the failure mode a hard-coded array has.
fn templates() -> Vec<(String, String)> {
    let dir = fixtures();
    let mut out: Vec<(String, String)> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jinja"))
        .map(|p| {
            let name = p
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let src = std::fs::read_to_string(&p)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()));
            (name, src)
        })
        .collect();
    out.sort();
    assert!(
        !out.is_empty(),
        "no vendored templates in {} — this test would otherwise pass over nothing",
        dir.display()
    );
    out
}

fn messages() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({"role": "system", "content": "You answer from the graph."}),
        serde_json::json!({"role": "user", "content": "Which ADR governs cross-repo links?"}),
    ]
}

fn tools() -> serde_json::Value {
    serde_json::json!([{
        "type": "function",
        "function": {
            "name": "search",
            "description": "Find nodes by text.",
            "parameters": {
                "type": "object",
                "properties": {"q": {"type": "string"}},
                "required": ["q"]
            }
        }
    }])
}

#[test]
fn every_registry_template_renders() {
    for (name, src) in templates() {
        let out = rto_llama::chat_template::render(&src, &messages(), None, true)
            .unwrap_or_else(|e| panic!("{name} failed to render: {e}"));
        assert!(
            out.len() > 40,
            "{name} rendered {} bytes — a template that collapses to almost \
             nothing is the `apply_chat_template` failure this replaces, not a \
             successful render",
            out.len()
        );
        assert!(
            out.contains("Which ADR governs cross-repo links?"),
            "{name} dropped the user's message: {out}"
        );
    }
}

/// The one `apply_chat_template` cannot satisfy at any setting.
#[test]
fn tools_reach_every_template() {
    let tools = tools();
    for (name, src) in templates() {
        let with = rto_llama::chat_template::render(&src, &messages(), Some(&tools), true)
            .unwrap_or_else(|e| panic!("{name} failed to render with tools: {e}"));
        assert!(
            with.contains("search"),
            "{name} did not name the tool it was given — a model trained to \
             receive tools in its template got none: {with}"
        );

        // And the same template without tools must not carry the tool's name,
        // or the assertion above proves nothing about `tools` reaching it.
        let without = rto_llama::chat_template::render(&src, &messages(), None, true)
            .unwrap_or_else(|e| panic!("{name} failed to render without tools: {e}"));
        assert!(
            !without.contains("search"),
            "{name} names the tool even when given none, so the positive case \
             above is not evidence: {without}"
        );
    }
}

/// A template renders tools in **its own** shape, and the shapes differ.
///
/// This is why the template must render them rather than the caller composing a
/// system turn: the registry does **not** agree on how a tool is written, and a
/// caller cannot know which shape a given model was trained on.
///
/// All three share the `<tools>` wrapper; what differs is what goes inside it.
/// An earlier version of this test asserted on the wrapper — the one part they
/// agree about — and so passed while proving nothing. It is the *inner* shape
/// that a hand-rolled system turn would get wrong:
///
/// | template | inside `<tools>` |
/// | --- | --- |
/// | `qwen3-32b` | a JSON object |
/// | `qwen3-coder-30b-a3b` | XML — `<function><name>search</name>…` |
/// | `qwen3.8-27b` | a JSON object |
#[test]
fn each_template_renders_tools_in_its_own_shape() {
    let tools = tools();
    let mut shapes: Vec<(String, bool)> = Vec::new();
    for (name, src) in templates() {
        let out = rto_llama::chat_template::render(&src, &messages(), Some(&tools), true)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        // The tool declared as an XML element rather than a JSON object.
        shapes.push((name, out.contains("<name>search</name>")));
    }
    assert!(
        shapes.iter().any(|(_, xml)| *xml),
        "expected a template declaring tools as XML elements: {shapes:?}"
    );
    assert!(
        shapes.iter().any(|(_, xml)| !*xml),
        "expected a template declaring them as JSON — if every template agreed, \
         one hand-rolled shape would be adequate and this whole path would be \
         unnecessary: {shapes:?}"
    );
}

/// Rendering is deterministic: the same inputs give the same prompt.
///
/// A prompt that varies between identical requests would defeat any prefix cache
/// downstream, and would do it silently.
#[test]
fn rendering_is_deterministic() {
    let tools = tools();
    for (name, src) in templates() {
        let a = rto_llama::chat_template::render(&src, &messages(), Some(&tools), true)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let b = rto_llama::chat_template::render(&src, &messages(), Some(&tools), true)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(a, b, "{name} rendered differently on identical input");
    }
}

/// `add_generation_prompt` actually changes the output.
///
/// It is the flag that tells the model to start answering. A renderer that
/// ignored it would produce a prompt the model continues as though it were still
/// reading the conversation.
#[test]
fn the_generation_prompt_flag_is_honoured() {
    for (name, src) in templates() {
        let with = rto_llama::chat_template::render(&src, &messages(), None, true)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let without = rto_llama::chat_template::render(&src, &messages(), None, false)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_ne!(
            with, without,
            "{name} renders identically with and without the generation prompt, \
             so the flag reaches nothing"
        );
    }
}

/// Every real template advertises the tools itself, so nothing else has to.
///
/// This is the licence for rto-serve's system turn to stop listing them: if a
/// template both renders the tools *and* a system prompt names them again, the
/// model is told the same set twice in two shapes. The assertion is that
/// `render_advertising` adds nothing here — the plain fallback must stay out of
/// the way of a template that has done the job.
#[test]
fn a_real_template_advertises_tools_without_help() {
    let tools = serde_json::json!([{
        "type": "function",
        "function": {
            "name": "roteiro_search",
            "description": "Find nodes by text.",
            "parameters": {"type": "object", "properties": {}},
        }
    }]);
    let msgs = vec![serde_json::json!({"role": "user", "content": "hello"})];
    for (name, src) in templates() {
        let out = rto_llama::chat_template::render_advertising(&src, &msgs, Some(&tools), true)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(
            out.contains("roteiro_search"),
            "{name}: the template did not carry the tool into the prompt"
        );
        assert!(
            !out.contains("You may call these tools"),
            "{name}: the plain fallback was added on top of a template that had \
             already advertised the tools — that is the duplication this exists \
             to avoid"
        );
    }
}

/// A template that never mentions `tools` still gets them.
///
/// The case an unknown model brings: nothing in the format obliges a template to
/// reference the variable, and one that does not would otherwise leave the model
/// with instructions about tools it was never shown. Deliberately not a real
/// registry template, because every template in the registry *does* use tools —
/// the gap only appears with a model Roteiro has not seen.
#[test]
fn a_template_that_ignores_tools_still_gets_them() {
    let ignores = "{%- for m in messages %}<|{{ m.role }}|>{{ m.content }}{%- endfor %}";
    let tools = serde_json::json!([{
        "type": "function",
        "function": {"name": "roteiro_explain", "description": "d", "parameters": {}}
    }]);
    let msgs = vec![serde_json::json!({"role": "user", "content": "hello"})];

    let plain =
        rto_llama::chat_template::render(ignores, &msgs, Some(&tools), true).expect("renders");
    assert!(
        !plain.contains("roteiro_explain"),
        "the fixture must actually drop the tools, or this proves nothing"
    );

    let out = rto_llama::chat_template::render_advertising(ignores, &msgs, Some(&tools), true)
        .expect("renders");
    assert!(
        out.contains("roteiro_explain"),
        "the tools never reached the prompt: {out}"
    );
    assert!(out.contains("You may call these tools"), "{out}");
}

/// No tools offered means no advertisement — the fallback must not invent a tool
/// section for a plain conversation.
#[test]
fn no_tools_means_no_advertisement() {
    let ignores = "{%- for m in messages %}<|{{ m.role }}|>{{ m.content }}{%- endfor %}";
    let msgs = vec![serde_json::json!({"role": "user", "content": "hello"})];
    let out =
        rto_llama::chat_template::render_advertising(ignores, &msgs, None, true).expect("renders");
    assert!(!out.contains("You may call these tools"), "{out}");
}

/// A template written for structured content still receives the conversation.
///
/// `smolvlm-500m`'s own template, vendored verbatim. It indexes
/// `content[0]['type']` and iterates the parts, so handed a plain string it
/// iterates that string's *characters* — none of which has a `['type']` — and
/// every branch falls through. Nothing errors; the turn simply renders as
/// nothing, and the prompt comes back with the user's question missing.
///
/// This is not a vision curiosity. It cost a real failure: `media_prompt` puts
/// mtmd's `<__media__>` marker in the message text, the marker vanished with the
/// rest of the turn, and mtmd then reported `Image preprocessing error` — an
/// error naming neither the template nor the missing text.
#[test]
fn a_structured_content_template_still_gets_the_conversation() {
    let src = std::fs::read_to_string(
        fixtures()
            .parent()
            .unwrap()
            .join("smolvlm-500m-structured-content.jinja"),
    )
    .expect("fixture");
    let msgs = vec![serde_json::json!({
        "role": "user",
        "content": "<__media__>\nDescribe what you perceive in one short sentence.",
    })];

    let naive = rto_llama::chat_template::render(&src, &msgs, None, true).expect("renders");
    assert!(
        !naive.contains("<__media__>"),
        "the fixture must actually drop the turn, or this proves nothing: {naive}"
    );

    let out =
        rto_llama::chat_template::render_advertising(&src, &msgs, None, true).expect("renders");
    assert!(
        out.contains("<__media__>"),
        "mtmd's marker did not survive templating: {out}"
    );
    assert!(
        out.contains("Describe what you perceive"),
        "the user's question did not survive templating: {out}"
    );
}

/// A template that keeps the conversation in neither shape is an error, not a
/// prompt with the question missing from it.
#[test]
fn a_template_that_renders_the_conversation_away_is_an_error() {
    let discards = "<|start|>{% if add_generation_prompt %}<|assistant|>{% endif %}";
    let msgs = vec![serde_json::json!({"role": "user", "content": "what is fn foo?"})];
    let err = rto_llama::chat_template::render_advertising(discards, &msgs, None, true)
        .expect_err("a prompt without the question is not a prompt");
    let text = err.to_string();
    assert!(text.contains("rendered the conversation away"), "{text}");
}

/// An empty tool array is "no tools", not "an empty list of tools".
#[test]
fn an_empty_tool_list_is_not_advertised() {
    let ignores = "{%- for m in messages %}<|{{ m.role }}|>{{ m.content }}{%- endfor %}";
    let msgs = vec![serde_json::json!({"role": "user", "content": "hello"})];
    let empty = serde_json::json!([]);
    let out = rto_llama::chat_template::render_advertising(ignores, &msgs, Some(&empty), true)
        .expect("renders");
    assert!(!out.contains("You may call these tools"), "{out}");
}

/// The fallback folds into the caller's system turn instead of adding a second.
///
/// A chat template may reject a system message that is not the first, or allow
/// only one. Inserting a turn to describe the tools could therefore stop a
/// perfectly good template from rendering at all.
#[test]
fn the_fallback_folds_into_an_existing_system_turn() {
    let ignores = "{%- for m in messages %}<|{{ m.role }}|>{{ m.content }}\n{%- endfor %}";
    let msgs = vec![
        serde_json::json!({"role": "system", "content": "GROUNDING RULES HERE"}),
        serde_json::json!({"role": "user", "content": "hello"}),
    ];
    let tools = serde_json::json!([{
        "type": "function",
        "function": {"name": "roteiro_path", "description": "d", "parameters": {}}
    }]);
    let out = rto_llama::chat_template::render_advertising(ignores, &msgs, Some(&tools), true)
        .expect("renders");

    assert_eq!(
        out.matches("<|system|>").count(),
        1,
        "a second system turn was added rather than folded in: {out}"
    );
    assert!(out.contains("roteiro_path"), "{out}");
    assert!(out.contains("GROUNDING RULES HERE"), "{out}");
}

/// No tools is an empty list, not `none` — the documented behaviour, pinned.
///
/// `qwen3-coder-30b-a3b` guards with `tools is iterable and tools | length > 0`.
/// Jinja2 short-circuits that because `none is iterable` is false; minijinja does
/// not, so `none` reaches `| length` and the render fails outright. The
/// substitution is therefore load-bearing, and a reader who believed the header
/// comment's earlier claim that `tools` was "passed through untouched" would
/// mispredict what a template testing `tools is none` sees.
#[test]
fn absent_tools_render_as_an_empty_list_not_none() {
    let probe = "{% if tools is none %}NONE{% elif tools %}SOME{% else %}EMPTY{% endif %}";
    let msgs = vec![serde_json::json!({"role": "user", "content": "hello"})];
    let out = rto_llama::chat_template::render(probe, &msgs, None, false).expect("renders");
    assert_eq!(
        out.trim(),
        "EMPTY",
        "absent tools reached the template as: {out}"
    );
}

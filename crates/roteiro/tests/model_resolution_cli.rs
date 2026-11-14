//! End-to-end tests for **local model resolution** (Stage 33): the `[models]`
//! table pins a model per task, `roteiro config` answers *why did it use that
//! model?* for every surface, and a pin that cannot be honoured is refused by
//! name rather than replaced by the default.
//!
//! The `vision`, `audio` and `ocr` keys are the point of the stage. Before it,
//! those three models were compiled-in string constants — so a project could not
//! pin its ASR model at all, and these tests are the first place that setting
//! does anything.
#![cfg(feature = "models")]

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_roteiro");

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

/// Run `roteiro` in `dir`, isolated from the developer's real `~/.roteiro`:
/// `ROTEIRO_HOME` is `dir`, so `dir/config.toml` is the **user** layer and
/// `dir/roteiro.toml` the **project** layer — which is what lets one temp dir
/// exercise both layers of provenance.
fn roteiro(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("ROTEIRO_HOME", dir)
        .env("HOME", dir)
        .env("USERPROFILE", dir)
        .output()
        .expect("run roteiro")
}

fn fresh_repo(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("roteiro-models-{label}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("mkdir");
    git(&dir, &["init", "-q"]);
    std::fs::write(dir.join("README.md"), "# fixture\n").expect("write");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    dir
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// **The unset baseline** (ADR-0033). OCR is the one registry model; `infer`
/// falls back to the hashing embedder; every other surface waits for a provider
/// model and says which key to set.
#[test]
fn unset_resolves_to_ocr_the_hashing_embedder_or_a_key_to_set() {
    let dir = fresh_repo("unset");

    let out = roteiro(&dir, &["config"]);
    assert!(out.status.success(), "config failed: {out:?}");
    let text = stdout(&out);

    let ocr = resolution_line(&text, "ocr");
    assert!(
        ocr.contains("ocrs-text") && ocr.contains("built-in default"),
        "{ocr}"
    );
    let embed = resolution_line(&text, "embed");
    assert!(embed.contains("hashing embedder"), "{embed}");
    for (task, key) in [
        ("draft", "generative"),
        ("chat", "generative"),
        ("review", "generative"),
        ("transcribe", "audio"),
        ("describe", "vision"),
    ] {
        let line = resolution_line(&text, task);
        assert!(
            line.contains(&format!("set `[models] {key}`")),
            "unset `{task}` names its key: {line}"
        );
    }

    let json = roteiro(&dir, &["config", "--json"]);
    let cfg: serde_json::Value = serde_json::from_slice(&json.stdout).expect("valid JSON");
    let entries = cfg["model_resolution"]
        .as_array()
        .expect("model_resolution is an array");
    assert_eq!(entries.len(), 7, "one entry per surface: {entries:?}");
    let review = entry(entries, "review");
    assert_eq!(review["config_key"], "generative", "no bespoke key");
    assert_eq!(review["surface"], "roteiro review --llm");
    assert!(review["model"].is_null(), "no model until one is named");
    let transcribe = entry(entries, "transcribe");
    assert_eq!(transcribe["source"], "default");
    assert_eq!(transcribe["config_key"], "audio");
    assert!(transcribe["layer"].is_null(), "nothing pinned it");
    assert_eq!(entry(entries, "ocr")["model"], "ocrs-text");
    assert!(cfg["infer"].is_object(), "existing sections intact: {cfg}");

    std::fs::remove_dir_all(&dir).ok();
}

/// A provider id is honoured verbatim, reported **per surface**, and labelled
/// with the layer it came from; an OCR pin is still a registry pin.
#[test]
fn a_pin_is_reported_per_surface_with_the_layer_it_came_from() {
    let dir = fresh_repo("pinned");
    // `ROTEIRO_HOME` is `dir`, so this is the *user* layer…
    std::fs::write(dir.join("config.toml"), "[models]\nocr = \"ocrs-text\"\n").expect("write user");
    // …and this the *project* layer.
    std::fs::write(dir.join("roteiro.toml"), "[models]\naudio = \"voxtral\"\n")
        .expect("write project");

    let text = stdout(&roteiro(&dir, &["config"]));

    let transcribe = resolution_line(&text, "transcribe");
    assert!(
        transcribe.contains("voxtral")
            && transcribe.contains("[models] audio")
            && transcribe.contains("in project config"),
        "the provider id, its key and its layer are named: {transcribe}"
    );
    let ocr = resolution_line(&text, "ocr");
    assert!(
        ocr.contains("pinned by `[models] ocr` in user config"),
        "the user layer is distinguished from the project one: {ocr}"
    );
    let describe = resolution_line(&text, "describe");
    assert!(
        describe.contains("set `[models] vision`"),
        "an unrelated surface is untouched: {describe}"
    );

    let json = roteiro(&dir, &["config", "--json"]);
    let cfg: serde_json::Value = serde_json::from_slice(&json.stdout).expect("valid JSON");
    let entries = cfg["model_resolution"].as_array().expect("array");
    assert_eq!(entry(entries, "transcribe")["source"], "provider");
    assert_eq!(entry(entries, "transcribe")["model"], "voxtral");
    assert_eq!(entry(entries, "transcribe")["layer"], "project");
    assert_eq!(entry(entries, "ocr")["layer"], "user");
    assert_eq!(entry(entries, "describe")["source"], "default");

    std::fs::remove_dir_all(&dir).ok();
}

/// A blank value reads as unset on every surface of `roteiro config`, and a
/// padded name is trimmed to what the resolver uses.
#[test]
fn a_blank_pin_reads_as_unset_on_every_surface() {
    let dir = fresh_repo("blank-pin");
    std::fs::write(dir.join("roteiro.toml"), "[models]\naudio = \"   \"\n").expect("write project");

    let text = stdout(&roteiro(&dir, &["config"]));
    let key_line = text
        .lines()
        .find(|l| l.starts_with("  audio "))
        .unwrap_or_else(|| panic!("no `[models] audio` line in:\n{text}"));
    assert!(
        key_line.contains("None") && key_line.contains("(default)"),
        "a value that names no model is not a set key: {key_line}"
    );
    let line = resolution_line(&text, "transcribe");
    assert!(
        line.contains("set `[models] audio`"),
        "resolution agrees: {line}"
    );

    let json = roteiro(&dir, &["config", "--json"]);
    let cfg: serde_json::Value = serde_json::from_slice(&json.stdout).expect("valid JSON");
    assert!(cfg["models"]["audio"].is_null(), "{}", cfg["models"]);
    let transcribe = entry(
        cfg["model_resolution"].as_array().expect("array"),
        "transcribe",
    );
    assert_eq!(transcribe["source"], "default");
    assert!(transcribe["layer"].is_null(), "no layer pinned it");

    std::fs::write(
        dir.join("roteiro.toml"),
        "[models]\naudio = \"  voxtral  \"\n",
    )
    .expect("write project");
    let json = roteiro(&dir, &["config", "--json"]);
    let cfg: serde_json::Value = serde_json::from_slice(&json.stdout).expect("valid JSON");
    assert_eq!(
        cfg["models"]["audio"], "voxtral",
        "trimmed to the resolved name"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// An unknown OCR name is refused rather than defaulted — OCR is the one key
/// still checked against the registry.
#[test]
fn an_unknown_ocr_pin_is_refused_rather_than_defaulted() {
    let dir = fresh_repo("unknown");
    std::fs::write(dir.join("roteiro.toml"), "[models]\nocr = \"ocrs-txt\"\n")
        .expect("write project");

    let out = roteiro(&dir, &["config"]);
    assert!(
        out.status.success(),
        "`config` diagnoses rather than refuses: {out:?}"
    );
    let line = resolution_line(&stdout(&out), "ocr");
    assert!(line.contains("UNRESOLVED"), "{line}");
    assert!(line.contains("roteiro model list"), "actionable: {line}");

    std::fs::remove_dir_all(&dir).ok();
}

/// `media status` names the key to set when no provider model is configured,
/// rather than a model to pull (ADR-0033).
#[test]
fn media_status_names_the_key_to_set() {
    let dir = fresh_repo("media-status");
    let text = stdout(&roteiro(&dir, &["media", "status"]));
    assert!(
        !text.contains("model pull"),
        "no GGUF to pull any more: {text}"
    );
    assert!(
        text.contains("[models] audio") || text.contains("--features audio-transcribe"),
        "says what to configure or rebuild: {text}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A loopback stand-in for Ollama: answers every `/v1/chat/completions` with one
/// streamed sentence, and sends each request body it receives down the channel.
#[cfg(feature = "provider")]
fn stub_provider() -> (String, std::sync::mpsc::Receiver<String>) {
    stub_provider_with(|_| "Drafted by the provider.")
}

/// [`stub_provider`], answering each request body with `reply(body)`.
#[cfg(feature = "provider")]
fn stub_provider_with(
    reply: fn(&str) -> &'static str,
) -> (String, std::sync::mpsc::Receiver<String>) {
    use std::io::{BufRead as _, BufReader, Read as _, Write as _};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://{}/v1", listener.local_addr().expect("addr"));
    let (sent, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).expect("header");
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = v.trim().parse().expect("length");
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).expect("body");
            let body = String::from_utf8_lossy(&body).into_owned();
            let content = serde_json::to_string(reply(&body)).expect("encode");
            sent.send(body).ok();
            let events = format!(
                "data: {{\"choices\":[{{\"delta\":{{\"content\":{content}}}}}]}}\n\ndata: [DONE]\n\n"
            );
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\
                 connection: close\r\n\r\n{events}",
                events.len()
            )
            .ok();
        }
    });
    (base, received)
}

/// With `[provider]` set, `spec draft` drafts through it — in a stock build with
/// no llama.cpp — using the provider's own model id, which the registry has
/// never heard of (ADR-0027).
#[test]
#[cfg(feature = "provider")]
fn spec_draft_drafts_through_the_configured_provider() {
    let (base, requests) = stub_provider();
    let dir = fresh_repo("spec-draft-provider");
    std::fs::write(
        dir.join("roteiro.toml"),
        format!("[provider]\nbase_url = \"{base}\"\n\n[models]\ngenerative = \"qwen3:8b\"\n"),
    )
    .expect("write project");

    let out = roteiro(&dir, &["spec", "draft", "fixture"]);
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "drafts: {err}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("Drafted by the provider."),
        "the provider's prose lands in the document: {out:?}"
    );
    assert!(err.contains("with qwen3:8b"), "names the model: {err}");

    let first = requests.try_recv().expect("the provider was called");
    assert!(first.contains("\"model\":\"qwen3:8b\""), "{first}");

    std::fs::remove_dir_all(&dir).ok();
}

/// `spec draft` reports, on stderr, which supplied symbols the draft never
/// mentions (ADR-0031 §1) — and the document on stdout is unchanged by it.
#[test]
#[cfg(feature = "provider")]
fn spec_draft_reports_what_the_draft_never_mentions() {
    let (base, _requests) = stub_provider();
    let dir = fresh_repo("spec-draft-coverage");
    std::fs::create_dir_all(dir.join("src")).expect("mkdir");
    std::fs::write(dir.join("src/lib.rs"), "pub fn fixture_parser() {}\n").expect("write");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "code"]);
    std::fs::write(
        dir.join("roteiro.toml"),
        format!("[provider]\nbase_url = \"{base}\"\n\n[models]\ngenerative = \"qwen3:8b\"\n"),
    )
    .expect("write project");

    let out = roteiro(&dir, &["spec", "draft", "fixture_parser"]);
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "drafts: {err}");
    let line = err
        .lines()
        .find(|l| l.starts_with("coverage: "))
        .unwrap_or_else(|| panic!("a coverage line: {err}"));
    assert!(line.contains("never mentioned: fixture_parser"), "{line}");
    assert!(
        err.lines()
            .any(|l| l.starts_with("style: ") && l.contains("not correctness")),
        "a style line (ADR-0031 §2): {err}"
    );
    assert!(
        !stdout(&out).contains("coverage:"),
        "stdout stays the document"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// A repo whose only symbol is `fixture_parser`, configured for `base`.
#[cfg(feature = "provider")]
fn parser_repo(label: &str, base: &str) -> PathBuf {
    let dir = fresh_repo(label);
    std::fs::create_dir_all(dir.join("src")).expect("mkdir");
    std::fs::write(dir.join("src/lib.rs"), "pub fn fixture_parser() {}\n").expect("write");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "code"]);
    std::fs::write(
        dir.join("roteiro.toml"),
        format!("[provider]\nbase_url = \"{base}\"\n\n[models]\ngenerative = \"qwen3:8b\"\n"),
    )
    .expect("write project");
    dir
}

/// The first draft every section gets: poor style, mentions the symbol.
const POOR_DRAFT: &str = "We utilize `fixture_parser` in order to parse.";

/// `--repair` keeps a rewrite that scores better and drops nothing
/// (ADR-0031 §3).
#[test]
#[cfg(feature = "provider")]
fn spec_draft_repair_keeps_a_better_rewrite() {
    let (base, _requests) = stub_provider_with(|body| {
        if body.contains("Your previous draft was") {
            "`fixture_parser` parses the input."
        } else {
            POOR_DRAFT
        }
    });
    let dir = parser_repo("spec-draft-repair-keep", &base);
    let out = roteiro(&dir, &["spec", "draft", "fixture_parser", "--repair", "1"]);
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "drafts: {err}");
    let doc = stdout(&out);
    assert!(doc.contains("`fixture_parser` parses the input."), "{doc}");
    assert!(
        !doc.contains(POOR_DRAFT),
        "the rewrite replaced the draft: {doc}"
    );
    assert!(err.contains("style 0 → 100 after 1 pass(es)"), "{err}");
    std::fs::remove_dir_all(&dir).ok();
}

/// `--repair` discards a rewrite that scores better by dropping a symbol the
/// draft mentioned — the loophole a style score alone leaves open.
#[test]
#[cfg(feature = "provider")]
fn spec_draft_repair_refuses_a_rewrite_that_drops_content() {
    let (base, _requests) = stub_provider_with(|body| {
        if body.contains("Your previous draft was") {
            "It parses the input."
        } else {
            POOR_DRAFT
        }
    });
    let dir = parser_repo("spec-draft-repair-refuse", &base);
    let out = roteiro(&dir, &["spec", "draft", "fixture_parser", "--repair", "1"]);
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "drafts: {err}");
    let doc = stdout(&out);
    assert!(
        doc.contains(POOR_DRAFT),
        "the original draft survives: {doc}"
    );
    assert!(!doc.contains("It parses the input."), "{doc}");
    assert!(err.contains("style 0 → 0 after 1 pass(es)"), "{err}");
    std::fs::remove_dir_all(&dir).ok();
}

/// Without `--repair`, no repair request is ever sent.
#[test]
#[cfg(feature = "provider")]
fn spec_draft_without_repair_sends_no_repair_request() {
    let (base, requests) = stub_provider_with(|_| POOR_DRAFT);
    let dir = parser_repo("spec-draft-no-repair", &base);
    let out = roteiro(&dir, &["spec", "draft", "fixture_parser"]);
    assert!(out.status.success(), "{out:?}");
    let sent: Vec<String> = requests.try_iter().collect();
    assert!(!sent.is_empty(), "the provider was called");
    assert!(
        sent.iter().all(|b| !b.contains("Your previous draft was")),
        "{sent:?}"
    );
    assert!(!String::from_utf8_lossy(&out.stderr).contains("repair `"));
    std::fs::remove_dir_all(&dir).ok();
}

/// `--repair` is bounded: four passes are refused at the parser.
#[test]
#[cfg(feature = "provider")]
fn spec_draft_repair_is_at_most_three_passes() {
    let dir = fresh_repo("spec-draft-repair-bound");
    let out = roteiro(&dir, &["spec", "draft", "x", "--repair", "4"]);
    assert!(!out.status.success(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--repair"),
        "{out:?}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// `[provider]` with no `[models] generative` is refused by key — there is no
/// provider id a default could safely name.
#[test]
#[cfg(feature = "provider")]
fn spec_draft_under_a_provider_needs_a_generative_model() {
    let dir = fresh_repo("spec-draft-provider-unset");
    std::fs::write(
        dir.join("roteiro.toml"),
        "[provider]\nbase_url = \"http://127.0.0.1:9/v1\"\n",
    )
    .expect("write project");

    let out = roteiro(&dir, &["spec", "draft", "fixture"]);
    assert!(!out.status.success(), "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    // The provider's refusal, not the no-backend fallback, which names the same
    // key for a different reason.
    assert!(
        err.contains("`[provider] base_url` is set but `[models] generative` is not"),
        "names the key: {err}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// Under `[provider]`, `roteiro config` reports the surfaces that moved to it
/// (Ask, `spec draft` and `review --llm`) as using the provider id they accept,
/// rather than calling a working pin unresolved because the registry does not
/// list it (ADR-0027).
#[test]
#[cfg(feature = "provider")]
fn config_reports_the_provider_id_for_the_surfaces_that_use_it() {
    let dir = fresh_repo("config-provider");
    std::fs::write(
        dir.join("roteiro.toml"),
        "[provider]\nbase_url = \"http://127.0.0.1:11434/v1\"\n\n[models]\ngenerative = \"qwen3:8b\"\n",
    )
    .expect("write project");

    let out = roteiro(&dir, &["config"]);
    assert!(out.status.success(), "{out:?}");
    let text = stdout(&out);
    for task in ["chat", "draft", "review"] {
        let line = resolution_line(&text, task);
        assert!(
            line.contains("qwen3:8b") && line.contains("provider") && !line.contains("UNRESOLVED"),
            "{task}: {line}"
        );
    }

    let json = roteiro(&dir, &["config", "--json"]);
    assert!(json.status.success(), "{json:?}");
    let cfg: serde_json::Value = serde_json::from_slice(&json.stdout).expect("json");
    let entries = cfg["model_resolution"].as_array().expect("array");
    for task in ["chat", "draft", "review"] {
        let e = entry(entries, task);
        assert_eq!(e["model"], "qwen3:8b", "{e}");
        assert_eq!(e["source"], "provider", "{e}");
    }

    std::fs::remove_dir_all(&dir).ok();
}

/// The resolution line for `task`, or a panic naming what was printed instead —
/// an assertion against a missing line is far less useful than one that shows the
/// table it looked in.
fn resolution_line(text: &str, task: &str) -> String {
    // Four spaces, not `trim_start`: the resolution rows are indented one level
    // deeper than the `[models]` key lines above them, and `ocr` is both a key
    // and a task — so a looser match reads the wrong line and the test passes or
    // fails for the wrong reason.
    let want = format!("    {task} ");
    text.lines()
        .find(|l| l.starts_with(&want))
        .unwrap_or_else(|| panic!("no resolution line for `{task}` in:\n{text}"))
        .to_owned()
}

/// The `model_resolution` entry for `task`.
fn entry<'a>(entries: &'a [serde_json::Value], task: &str) -> &'a serde_json::Value {
    entries
        .iter()
        .find(|e| e["task"] == task)
        .unwrap_or_else(|| panic!("no `{task}` entry in {entries:?}"))
}

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

/// **The unset baseline.** With no `[models]` key set anywhere, every surface
/// resolves to exactly the model it used before this stage existed. Asserted
/// against the literal names — `voxtral-mini-3b`, `smolvlm-500m-gguf`,
/// `ocrs-text`, `qwen3-0.6b` — because the claim being tested is that *these
/// specific* constants survived being moved into a table.
#[test]
fn unset_resolves_to_the_models_used_before_the_table_existed() {
    let dir = fresh_repo("unset");

    let out = roteiro(&dir, &["config"]);
    assert!(out.status.success(), "config failed: {out:?}");
    let text = stdout(&out);

    for (task, model) in [
        ("draft", "qwen3-0.6b"),
        ("chat", "qwen3-0.6b"),
        // Stage 35b's `review --llm`: a third task on the `generative` key, so it
        // inherits that key's default rather than acquiring one of its own.
        ("review", "qwen3-0.6b"),
        ("transcribe", "voxtral-mini-3b"),
        ("describe", "smolvlm-500m-gguf"),
        ("ocr", "ocrs-text"),
    ] {
        let line = resolution_line(&text, task);
        assert!(
            line.contains(model),
            "unset `{task}` still resolves to {model}: {line}"
        );
        assert!(
            line.contains("built-in default"),
            "and says nothing pinned it: {line}"
        );
    }
    // `infer` has no registry default: unset means the compiled-in hashing
    // embedder, which is not a model and needs no download.
    let embed = resolution_line(&text, "embed");
    assert!(
        embed.contains("hashing embedder"),
        "embed falls back to the offline default: {embed}"
    );

    // The same answers via `--json`, for a consumer that should not have to parse
    // prose to learn why a model was used.
    let json = roteiro(&dir, &["config", "--json"]);
    let cfg: serde_json::Value = serde_json::from_slice(&json.stdout).expect("valid JSON");
    let entries = cfg["model_resolution"]
        .as_array()
        .expect("model_resolution is an array");
    assert_eq!(entries.len(), 7, "one entry per surface: {entries:?}");
    // `review` is a surface `roteiro config` answers for, on the shared key. A
    // task the resolver knows but this command does not report would leave an
    // operator asking "why that model?" about a surface with no row.
    let review = entry(entries, "review");
    assert_eq!(review["config_key"], "generative", "no bespoke key");
    assert_eq!(review["surface"], "roteiro review --llm");
    assert_eq!(review["model"], "qwen3-0.6b");
    let transcribe = entry(entries, "transcribe");
    assert_eq!(transcribe["model"], "voxtral-mini-3b");
    assert_eq!(transcribe["source"], "default");
    assert_eq!(transcribe["config_key"], "audio");
    assert!(transcribe["layer"].is_null(), "nothing pinned it");
    assert!(
        entry(entries, "embed")["model"].is_null(),
        "no model needed"
    );

    // Adding `[models]` keys the previous shape never had must not disturb the
    // rest of the document — every path a consumer already reads is unmoved.
    assert!(cfg["infer"].is_object(), "existing sections intact: {cfg}");
    assert!(
        cfg["models"]["audio"].is_null(),
        "the new key exists: {cfg}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// A pin is honoured, reported **per surface**, and labelled with the layer it
/// came from — the same per-value provenance `[debt] ignore` gets, for the same
/// reason: a key that governs several surfaces cannot be explained by one label
/// on the key.
#[test]
fn a_pin_is_reported_per_surface_with_the_layer_it_came_from() {
    let dir = fresh_repo("pinned");
    // `ROTEIRO_HOME` is `dir`, so this is the *user* layer…
    std::fs::write(dir.join("config.toml"), "[models]\nocr = \"ocrs-text\"\n").expect("write user");
    // …and this the *project* layer.
    std::fs::write(
        dir.join("roteiro.toml"),
        "[models]\naudio = \"voxtral-mini-3b\"\n",
    )
    .expect("write project");

    let text = stdout(&roteiro(&dir, &["config"]));

    let transcribe = resolution_line(&text, "transcribe");
    assert!(
        transcribe.contains("pinned by `[models] audio` in project config"),
        "the pin and its layer are named: {transcribe}"
    );
    let ocr = resolution_line(&text, "ocr");
    assert!(
        ocr.contains("pinned by `[models] ocr` in user config"),
        "the user layer is distinguished from the project one: {ocr}"
    );
    // One key moves one surface. `[models] audio` must not silently become the
    // answer for `describe` as well.
    let describe = resolution_line(&text, "describe");
    assert!(
        describe.contains("built-in default") && describe.contains("smolvlm-500m-gguf"),
        "an unrelated surface is untouched: {describe}"
    );

    let json = roteiro(&dir, &["config", "--json"]);
    let cfg: serde_json::Value = serde_json::from_slice(&json.stdout).expect("valid JSON");
    let entries = cfg["model_resolution"].as_array().expect("array");
    assert_eq!(entry(entries, "transcribe")["source"], "pinned");
    assert_eq!(entry(entries, "transcribe")["layer"], "project");
    assert_eq!(entry(entries, "ocr")["layer"], "user");
    assert_eq!(entry(entries, "describe")["source"], "default");

    std::fs::remove_dir_all(&dir).ok();
}

/// **The trap.** A vision model pinned as the audio model is refused by name.
/// Not a warning, not a fallback: llama.cpp aborts the process on a `GGML_ASSERT`
/// when handed a model whose architecture the path cannot serve, so a config that
/// *appears* honoured while something else runs is the worst outcome available.
///
/// `roteiro config` is the exception that proves it — it reports the error rather
/// than refusing to run, because it is the command someone reaches for when a pin
/// is not doing what they expected.
#[test]
fn a_wrong_modality_pin_is_refused_by_name_and_never_falls_back() {
    let dir = fresh_repo("wrong-kind");
    std::fs::write(
        dir.join("roteiro.toml"),
        "[models]\naudio = \"smolvlm-500m-gguf\"\n",
    )
    .expect("write project");

    let out = roteiro(&dir, &["config"]);
    assert!(
        out.status.success(),
        "`config` must still run — it is how you diagnose this: {out:?}"
    );
    let text = stdout(&out);
    let line = resolution_line(&text, "transcribe");
    assert!(
        line.contains("UNRESOLVED"),
        "not silently defaulted: {line}"
    );
    assert!(line.contains("[models] audio"), "names the key: {line}");
    assert!(
        line.contains("vision model") && line.contains("an audio model"),
        "says what it is and what was needed: {line}"
    );
    assert!(
        !line.contains("voxtral-mini-3b"),
        "the default must not appear as the answer: {line}"
    );

    // Every other surface is unaffected — one bad key does not poison the table.
    assert!(resolution_line(&text, "describe").contains("smolvlm-500m-gguf"));
    assert!(resolution_line(&text, "ocr").contains("ocrs-text"));

    // And the machine-readable form carries the error rather than a model.
    let json = roteiro(&dir, &["config", "--json"]);
    let cfg: serde_json::Value = serde_json::from_slice(&json.stdout).expect("valid JSON");
    let transcribe = entry(
        cfg["model_resolution"].as_array().expect("array"),
        "transcribe",
    );
    assert!(transcribe["model"].is_null(), "no model was chosen");
    assert!(
        transcribe["error"]
            .as_str()
            .is_some_and(|e| e.contains("[models] audio")),
        "the error names the key: {transcribe}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// **A key that names no model is reported exactly as it is resolved** — on every
/// surface of `roteiro config`, not just the one that happens to call the shared
/// accessor (PR #379 review).
///
/// `[models] audio = "   "` used to be printed as set and attributed to the
/// project layer, in the same document whose resolution table called it unset,
/// and `--json` echoed the raw `"   "` beside a `"source": "default"`. Three
/// surfaces, two answers. For the stage whose whole claim is that one place
/// decides and can say why, a report that contradicts the decision is the defect
/// it exists to end, not a cosmetic one — so this asserts all three at once.
#[test]
fn a_blank_pin_reads_as_unset_on_every_surface() {
    let dir = fresh_repo("blank-pin");
    std::fs::write(dir.join("roteiro.toml"), "[models]\naudio = \"   \"\n").expect("write project");

    let text = stdout(&roteiro(&dir, &["config"]));

    // Surface 1: the key list. Not `Some("   ")  (project)`.
    let key_line = text
        .lines()
        .find(|l| l.starts_with("  audio "))
        .unwrap_or_else(|| panic!("no `[models] audio` line in:\n{text}"));
    assert!(
        key_line.contains("None") && key_line.contains("(default)"),
        "a value that names no model is not a set key: {key_line}"
    );

    // Surface 2: the resolution table, which always said this.
    let line = resolution_line(&text, "transcribe");
    assert!(
        line.contains("built-in default") && line.contains("voxtral-mini-3b"),
        "resolution is unchanged: {line}"
    );

    // Surface 3: `--json`, which serialises the effective config directly and so
    // never went through the accessor at all — the reason the fix belongs at the
    // parse, not at the point of use.
    let json = roteiro(&dir, &["config", "--json"]);
    let cfg: serde_json::Value = serde_json::from_slice(&json.stdout).expect("valid JSON");
    assert!(
        cfg["models"]["audio"].is_null(),
        "the echoed config agrees with the resolution beside it: {}",
        cfg["models"]
    );
    let transcribe = entry(
        cfg["model_resolution"].as_array().expect("array"),
        "transcribe",
    );
    assert_eq!(transcribe["source"], "default");
    assert!(transcribe["layer"].is_null(), "no layer pinned it");

    // A real name still survives the same normalisation, trimmed to what the
    // resolver looks up — so this is not "blank keys are dropped", it is "the
    // report and the resolution read the value the same way".
    std::fs::write(
        dir.join("roteiro.toml"),
        "[models]\naudio = \"  voxtral-mini-3b  \"\n",
    )
    .expect("write project");
    let text = stdout(&roteiro(&dir, &["config"]));
    assert!(
        resolution_line(&text, "transcribe").contains("pinned by `[models] audio`"),
        "a padded name is still a pin: {text}"
    );
    let json = roteiro(&dir, &["config", "--json"]);
    let cfg: serde_json::Value = serde_json::from_slice(&json.stdout).expect("valid JSON");
    assert_eq!(
        cfg["models"]["audio"], "voxtral-mini-3b",
        "reported as the name that was resolved, not as it was typed"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// An unknown name is refused too — the typo case. A fallback here would be the
/// quietest possible failure: the config file says one model, the tool uses
/// another, and nothing says so.
#[test]
fn an_unknown_pin_is_refused_rather_than_defaulted() {
    let dir = fresh_repo("unknown");
    std::fs::write(
        dir.join("roteiro.toml"),
        "[models]\ngenerative = \"qwen3-0.6\"\n",
    )
    .expect("write project");

    let text = stdout(&roteiro(&dir, &["config"]));
    for task in ["draft", "chat"] {
        let line = resolution_line(&text, task);
        assert!(line.contains("UNRESOLVED"), "{task}: {line}");
        assert!(line.contains("roteiro model list"), "actionable: {line}");
        assert!(
            !line.contains("qwen3-0.6b"),
            "the near-miss default is not substituted: {line}"
        );
    }

    std::fs::remove_dir_all(&dir).ok();
}

/// The pin reaches the **media path**, not just the report: `media status` tells
/// an operator to pull the model this repository actually uses.
///
/// This is the assertion that distinguishes a config key from a decorative one.
/// Told to pull the built-in default, a project that pinned another model would
/// download the wrong weights and still be unable to build.
#[test]
fn media_status_names_the_pinned_model_to_pull() {
    let dir = fresh_repo("media-status");

    // Unset: the built-in default, exactly as before this stage.
    let before = stdout(&roteiro(&dir, &["media", "status"]));
    assert!(
        before.contains("roteiro model pull voxtral-mini-3b"),
        "unset names the default: {before}"
    );

    // Pinned: the pinned model, and why.
    std::fs::write(
        dir.join("roteiro.toml"),
        "[models]\naudio = \"qwen3-0.6b\"\n",
    )
    .expect("write project");
    let bad = stdout(&roteiro(&dir, &["media", "status"]));
    assert!(
        bad.contains("[models] audio") && !bad.contains("model pull voxtral-mini-3b"),
        "a generative model pinned for audio is refused, not defaulted: {bad}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// `spec draft` refuses a `[models] generative` it cannot use, instead of the
/// pre-Stage-33 behaviour: filtering the pin out and silently drafting with the
/// default. Needs a generation backend to reach the model pick at all.
#[test]
#[cfg(any(feature = "serve", feature = "inference-local-models"))]
fn spec_draft_refuses_a_generative_pin_of_the_wrong_kind() {
    let dir = fresh_repo("spec-draft");
    std::fs::write(
        dir.join("roteiro.toml"),
        "[models]\ngenerative = \"bge-small-en-v1.5-gguf\"\n",
    )
    .expect("write project");

    let out = roteiro(&dir, &["spec", "draft", "fixture"]);
    assert!(!out.status.success(), "a bad pin must fail: {out:?}");
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(err.contains("[models] generative"), "names the key: {err}");
    // The *reason*, not merely the key. Without this the test would also pass on
    // a machine where the pinned model simply is not installed, which is a
    // different refusal and would leave the modality check unproven.
    assert!(
        err.contains("embedding model") && err.contains("a generative model"),
        "says what is wrong with it: {err}"
    );
    assert!(
        !err.contains("drafted"),
        "and nothing was drafted with a substitute model: {err}"
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("# "),
        "not even a scaffold: a refused pin produces no artifact"
    );

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

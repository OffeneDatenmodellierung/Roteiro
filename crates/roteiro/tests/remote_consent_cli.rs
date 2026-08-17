//! End-to-end tests for the **remote model tier's consent gate** (ADR-0019) —
//! the first capability in Roteiro that could send repository content off the
//! machine, and the reason every guard here is load-bearing rather than
//! ceremonial.
//!
//! The unit tests in `rto-remote` prove the rule. These prove the *wiring*: that
//! a real `roteiro.toml` on disk, a real `~/.roteiro/config.toml`, and a real
//! flag reach that rule in the shape it expects. A consent model that is correct
//! in a library and mis-plumbed in a binary has the same effect as no consent
//! model.
//!
//! **No test here makes a network call, and none can**: this build compiles no
//! backend, and `rto-remote` holds no transport — it takes one as a
//! caller-supplied closure, so the code that could open a socket does not exist
//! in the binary under test.
#![cfg(feature = "remote")]

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
/// exercise both halves of the inversion.
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
    let dir = std::env::temp_dir().join(format!("roteiro-remote-{label}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("mkdir");
    git(&dir, &["init", "-q"]);
    std::fs::write(dir.join("README.md"), "# fixture\n\nsome captured prose.\n").expect("write");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    dir
}

/// A destination, so the gate is never the only thing that could stop a run.
const ENDPOINT: &str = "[remote]\nendpoint = \"https://models.example/v1/chat/completions\"\n\
                        model = \"some-vendor/model-2026-05\"\n";

/// Write the **project** layer: `roteiro.toml`, the committed, shared file.
fn project_layer(dir: &Path, body: &str) {
    std::fs::write(dir.join("roteiro.toml"), format!("{ENDPOINT}{body}")).expect("write");
}

/// Write the **user** layer: `$ROTEIRO_HOME/config.toml`, this human's own file.
fn user_layer(dir: &Path, body: &str) {
    std::fs::write(dir.join("config.toml"), body).expect("write");
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn status_json(dir: &Path, flags: &[&str]) -> serde_json::Value {
    let mut args = vec!["remote", "status", "--json"];
    args.extend_from_slice(flags);
    let out = roteiro(dir, &args);
    assert!(out.status.success(), "remote status failed: {out:?}");
    serde_json::from_str(&stdout(&out)).expect("status emits JSON")
}

/// **A project-layer grant does not enable egress.** ADR-0019 §3, and the reason
/// this key's precedence is inverted at all: `roteiro.toml` is committed and
/// shared by design, so a merged line authorising egress on every teammate's
/// machine is consent by pull request — granted by someone else, noticed by
/// nobody.
///
/// Asserted with the **invocation granting**, so the only thing that can be
/// stopping it is the missing user layer.
#[test]
fn a_project_layer_grant_does_not_enable_egress() {
    let dir = fresh_repo("project-grant");
    project_layer(&dir, "enabled = true\n");

    let gate = status_json(&dir, &["--allow-remote"]);
    assert_eq!(
        gate["granted"], false,
        "a committed file cannot grant egress"
    );
    assert_eq!(gate["reason"], "user_layer_unset");
    assert_eq!(
        gate["project_grant_ignored"], true,
        "the discarded grant is reported, not swallowed"
    );
    assert_eq!(
        gate["layers"]["project"], true,
        "the file really did say yes"
    );

    // …and the human-readable surface says so too, in the terms a reader of that
    // file would need: not "denied", but "your committed setting was ignored,
    // and here is why it can never work".
    let out = roteiro(&dir, &["remote", "status", "--allow-remote"]);
    let text = stdout(&out);
    assert!(text.contains("DENIED"), "{text}");
    assert!(text.contains("read and ignored"), "{text}");
    assert!(text.contains("never grant"), "{text}");
    assert!(
        text.contains("~/.roteiro/config.toml"),
        "the remedy names the only file that could grant it: {text}"
    );
}

/// **A project-layer denial disables the tier even when the user layer and the
/// invocation both grant.** Denial has none of the problems of grant: a
/// locked-down repository is a legitimate thing to express, and no flag
/// overrides it.
#[test]
fn a_project_layer_denial_wins_over_the_user_layer_and_the_invocation() {
    let dir = fresh_repo("project-denial");
    project_layer(&dir, "enabled = false\n");
    user_layer(&dir, "[remote]\nenabled = true\n");

    let gate = status_json(&dir, &["--allow-remote"]);
    assert_eq!(gate["granted"], false, "the project denied it for everyone");
    assert_eq!(gate["reason"], "project_denied");
    assert_eq!(gate["layers"]["user"], true, "the user really did grant");
    assert_eq!(gate["layers"]["invocation"], true, "so did the invocation");

    // The remedy must not send the reader to edit a file that would not help.
    let remedy = gate["remedy"].as_str().expect("a remedy");
    assert!(remedy.contains("no flag overrides"), "{remedy}");

    // And `roteiro config` — where someone looks *before* running anything —
    // reports the same effective value.
    let text = stdout(&roteiro(&dir, &["config"]));
    assert!(text.contains("enabled  = Some(false)"), "{text}");
}

/// **Both halves of the grant are necessary, and neither is sufficient.** The
/// user layer opts *the human* in; the invocation opts *the run* in. Walked as a
/// table, because the interesting property is that three of the four cells deny.
#[test]
fn the_user_layer_and_the_invocation_are_both_required() {
    let dir = fresh_repo("both-required");
    project_layer(&dir, "");

    for (user_grants, flag, granted, reason) in [
        (false, None, false, "user_layer_unset"),
        (false, Some("--allow-remote"), false, "user_layer_unset"),
        (true, None, false, "invocation_unset"),
        (true, Some("--allow-remote"), true, "granted"),
    ] {
        if user_grants {
            user_layer(&dir, "[remote]\nenabled = true\n");
        } else {
            std::fs::remove_file(dir.join("config.toml")).ok();
        }
        let gate = status_json(&dir, flag.as_slice());
        assert_eq!(
            gate["granted"], granted,
            "user={user_grants} flag={flag:?} -> {gate}"
        );
        assert_eq!(gate["reason"], reason, "user={user_grants} flag={flag:?}");
    }

    // `--no-remote` denies a run that every other layer allowed — the invocation
    // may deny as well as grant.
    user_layer(&dir, "[remote]\nenabled = true\n");
    let gate = status_json(&dir, &["--no-remote"]);
    assert_eq!(gate["granted"], false);
    assert_eq!(gate["reason"], "invocation_denied");
}

/// The tier is default-off end to end: a repository with no `[remote]` key
/// anywhere reports denied, and says which file would change that.
#[test]
fn with_nothing_configured_the_tier_is_off_and_says_what_would_change_it() {
    let dir = fresh_repo("default-off");

    let gate = status_json(&dir, &[]);
    assert_eq!(gate["granted"], false);
    assert_eq!(gate["reason"], "user_layer_unset");
    assert_eq!(gate["project_grant_ignored"], false);
    assert!(
        gate["endpoint"].is_null(),
        "nothing is configured, so there is nowhere to send: {gate}"
    );
    assert!(
        gate["endpoint_error"]
            .as_str()
            .is_some_and(|e| e.contains("`[remote] endpoint` is not set")),
        "and it says so rather than inventing a destination: {gate}"
    );
}

/// **A dry-run prints the exact bytes and sends nothing**, which is what makes
/// the payload inspectable *before* it is sent (ADR-0019 §4).
///
/// It also takes no consent flag: an inspection is not a disclosure, so it has to
/// be available to someone deciding whether to grant one.
#[test]
fn a_dry_run_prints_the_exact_payload_and_records_nothing() {
    let dir = fresh_repo("dry-run");
    project_layer(&dir, "");
    assert!(
        roteiro(&dir, &["sync"]).status.success(),
        "the fixture graph must build"
    );

    let out = roteiro(
        &dir,
        &[
            "remote",
            "dry-run",
            "--json",
            "what is this repository about?",
            "--key",
            "file:README.md",
        ],
    );
    assert!(out.status.success(), "dry-run failed: {out:?}");
    let report: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("JSON");

    assert_eq!(report["sent"], false, "a dry-run sends nothing");
    assert_eq!(
        report["endpoint"], "https://models.example/v1/chat/completions",
        "it names where the bytes would have gone"
    );
    assert_eq!(report["trust"], "vendor_asserted");

    let body = report["body"].as_str().expect("a body");
    assert!(body.contains("what is this repository about?"), "{body}");
    assert!(body.contains("file:README.md"), "{body}");
    assert!(
        body.contains("some captured prose"),
        "captured prose is disclosed, and the dry-run shows it: {body}"
    );
    assert_eq!(
        report["bytes"].as_u64(),
        Some(body.len() as u64),
        "the byte count describes the body it is printed beside"
    );

    // The disclosure travels with the preview, including the half that is not
    // reassuring.
    let disclosure = report["disclosure"].as_str().expect("a disclosure");
    assert!(
        disclosure.contains("no redaction chokepoint"),
        "{disclosure}"
    );
    assert!(
        disclosure.contains("commercially sensitive"),
        "{disclosure}"
    );

    // Nothing left, so nothing is in the ledger.
    let log = stdout(&roteiro(&dir, &["remote", "log"]));
    assert!(log.contains("nothing has left this machine"), "{log}");
    assert!(
        !dir.join("remote").join("egress.jsonl").exists(),
        "a dry-run does not even create the ledger"
    );
}

/// `roteiro config` reports `[remote]` as **layers**, not as one merged value,
/// because a reader applying the general project-over-user rule to this key
/// would be wrong about the one setting where being wrong means believing egress
/// is off when it is on.
#[test]
fn config_reports_the_remote_layers_and_the_inversion() {
    let dir = fresh_repo("config-report");
    project_layer(&dir, "enabled = true\n");
    user_layer(&dir, "[remote]\nenabled = false\n");

    let text = stdout(&roteiro(&dir, &["config"]));
    assert!(text.contains("[remote]"), "{text}");
    assert!(text.contains("may deny, never grant"), "{text}");
    assert!(
        text.contains("read and ignored"),
        "the project's grant is called out where it was written: {text}"
    );
    assert!(
        text.contains("--allow-remote"),
        "and the reader is told the invocation is still required: {text}"
    );
    // The user layer's `false` is what survives the merge; the project's `true`
    // is not.
    assert!(text.contains("enabled  = Some(false)"), "{text}");
    // `endpoint` and `model` are ordinary keys and layer ordinarily — the
    // project set them, so the project is credited.
    assert!(
        text.contains("endpoint = Some(\"https://models.example/v1/chat/completions\")  (project)"),
        "{text}"
    );
}

/// **The tier is not in the default feature set, and no release may put it
/// there.** Asserted against the manifest rather than against behaviour, because
/// "off by default" is a property of the build, and a behavioural test would pass
/// unchanged the day someone flipped it.
#[test]
fn remote_is_not_a_default_feature() {
    let manifest =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .expect("read this crate's manifest");
    let default = manifest
        .lines()
        .find(|line| line.starts_with("default = ["))
        .expect("the manifest declares a default feature set");
    assert!(
        !default.contains("remote"),
        "`remote` reached the default feature set: {default}. ADR-0019 makes this capability \
         the project's one exemption from Principle 10, and the exemption is only tolerable \
         because the capability is absent unless someone asked for it at build time, again in \
         their own user config, and again per invocation."
    );
}

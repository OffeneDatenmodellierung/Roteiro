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
//! # No test here makes a network call, and none can
//!
//! **This paragraph changed in part 2, and the change is the point.** Part 1
//! could say the binary under test compiled no backend, so nothing in it could
//! open a socket. That is no longer true — `remote_transport::call` is compiled
//! in, and it is exactly what these tests exercise. So the guarantee is
//! re-established on different ground rather than quietly dropped:
//!
//! * The tests that need a *granted* call point `[remote] endpoint` at
//!   [`UNREACHABLE_LOOPBACK`] — port 1 on `127.0.0.1`. Loopback is not a network:
//!   the kernel cannot route those bytes to another host, there is no DNS lookup
//!   to leak the query to a resolver (ADR-0019 §2's reason a probe *is* egress),
//!   and nothing is listening, so the call is refused in microseconds. It is
//!   structurally incapable of reaching anything, not merely pointed somewhere
//!   that happens not to answer.
//! * The tests that need a *refused* call never reach the transport at all,
//!   which is asserted rather than assumed: an empty ledger is proof, because
//!   `rto_remote::call_with` writes the egress line **before** it sends.
//! * Everything about a *successful* response — the shapes, the truncations, the
//!   malformed bodies — is tested in `rto_remote::response`, over string
//!   literals, where there is no socket to be tempted by.
//!
//! No test sets a real endpoint, and none should. A test that could accidentally
//! become the first thing that sends data is the worst regression this file has.
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

/// **The one endpoint a test is allowed to actually call.**
///
/// Port 1 on the loopback interface. Three properties, all of them structural
/// rather than circumstantial:
///
/// * **It cannot leave this machine.** Loopback is not a network; the kernel has
///   nowhere else to put the bytes.
/// * **It resolves nothing.** A hostname would mean a DNS query, and ADR-0019 §2
///   is explicit that a DNS lookup leaks the question to a resolver — so a test
///   that "just" used an unresolvable hostname would be performing the very act
///   the ADR calls egress.
/// * **Nothing listens on it.** Port 1 is privileged and unassigned in practice,
///   so `connect` is refused immediately and the test is fast as well as inert.
///
/// `rto_remote::Endpoint::new` permits plaintext here for exactly this reason —
/// a loopback gateway is the one case where clear text never leaves the host.
const UNREACHABLE_LOOPBACK: &str = "http://127.0.0.1:1/v1/chat/completions";

/// Write the **project** layer: `roteiro.toml`, the committed, shared file.
fn project_layer(dir: &Path, body: &str) {
    std::fs::write(dir.join("roteiro.toml"), format!("{ENDPOINT}{body}")).expect("write");
}

/// Write the project layer with a different `[remote] endpoint` — the only
/// caller points it at [`UNREACHABLE_LOOPBACK`].
fn project_layer_at(dir: &Path, url: &str, body: &str) {
    std::fs::write(
        dir.join("roteiro.toml"),
        format!("[remote]\nendpoint = \"{url}\"\nmodel = \"some-vendor/model-2026-05\"\n{body}"),
    )
    .expect("write");
}

/// The egress ledger's entries, oldest first. A missing ledger reads as empty —
/// which is the assertion several tests below are actually making.
fn ledger(dir: &Path) -> Vec<serde_json::Value> {
    let path = dir.join("remote").join("egress.jsonl");
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("a ledger line is JSON"))
        .collect()
}

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
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

/// **The whole call path, end to end, with a granted gate and no network.**
///
/// This is the test part 1 could not write, and it asserts the two things that
/// distinguish this capability from an ordinary HTTP client:
///
/// 1. **The record is written before the bytes leave.** The endpoint refuses
///    instantly, so the call never succeeds — and the ledger still holds a full
///    egress line carrying the body, closed by a failure outcome. A ledger that
///    only recorded *completed* calls would be silent about exactly the calls
///    worth knowing about.
/// 2. **The preview is the act.** The body recorded in the ledger is
///    byte-identical to what `remote dry-run` printed for the same request.
///    Both go through one assembly and one rendering, and this is what holds
///    the two call sites level through the binary rather than in a unit test.
///
/// And the failure itself: named, quoting the endpoint, saying in as many words
/// that no local model was substituted (ADR-0019 §6).
#[test]
fn a_granted_call_records_its_egress_before_failing_loudly_at_the_endpoint() {
    let dir = fresh_repo("call-unreachable");
    project_layer_at(&dir, UNREACHABLE_LOOPBACK, "");
    user_layer(&dir, "[remote]\nenabled = true\n");
    assert!(
        roteiro(&dir, &["sync"]).status.success(),
        "the fixture graph must build"
    );

    let request = ["what is this repository about?", "--key", "file:README.md"];

    // What a dry-run says would be sent, before anything is.
    let mut preview_args = vec!["remote", "dry-run", "--json"];
    preview_args.extend_from_slice(&request);
    let preview = roteiro(&dir, &preview_args);
    assert!(preview.status.success(), "dry-run failed: {preview:?}");
    let preview: serde_json::Value = serde_json::from_str(&stdout(&preview)).expect("JSON");
    let previewed = preview["body"].as_str().expect("a body").to_owned();
    assert!(
        ledger(&dir).is_empty(),
        "a dry-run is an inspection, not a disclosure"
    );

    // And now the call, which is granted, reaches the transport, and fails.
    let mut call_args = vec!["remote", "call", "--allow-remote"];
    call_args.extend_from_slice(&request);
    let out = roteiro(&dir, &call_args);
    assert!(!out.status.success(), "the endpoint cannot be reached");

    let text = stderr(&out);
    assert!(
        text.contains(UNREACHABLE_LOOPBACK),
        "names where it went: {text}"
    );
    assert!(
        text.contains("did **not** fall back"),
        "an unannounced downgrade is the failure ADR-0019 most needs to prevent: {text}"
    );
    assert!(
        text.contains("--allow-remote"),
        "and says how to get the local answer deliberately instead: {text}"
    );

    // The record: two lines, one call, the egress first — and it carries the
    // bytes, because "a call happened" answers a much weaker question than
    // "this is what it carried".
    let entries = ledger(&dir);
    assert_eq!(entries.len(), 2, "an egress and its outcome: {entries:?}");
    assert_eq!(entries[0]["event"], "egress");
    assert_eq!(entries[1]["event"], "outcome");
    assert_eq!(
        entries[0]["call"], entries[1]["call"],
        "one call, two lines"
    );
    assert_eq!(entries[0]["endpoint"], UNREACHABLE_LOOPBACK);
    assert_eq!(entries[0]["trust"], "vendor_asserted");
    assert_eq!(
        entries[1]["ok"], false,
        "and it is recorded as having failed"
    );
    assert_eq!(
        entries[0]["body"].as_str(),
        Some(previewed.as_str()),
        "what was recorded is what the dry-run showed, byte for byte"
    );

    // `remote log` stops saying nothing has left this machine, because something
    // did — an attempt that failed is still a disclosure, and the log is a log of
    // disclosures rather than of successes.
    let log = stdout(&roteiro(&dir, &["remote", "log"]));
    assert!(!log.contains("nothing has left this machine"), "{log}");
    assert!(log.contains("SENT"), "{log}");
    assert!(log.contains("FAILED"), "{log}");
}

/// **`status` and `dry-run` never prompt.** They are the commands you run to
/// find out what would happen, and a command that asks permission in order to
/// tell you is useless — so the gate state a prompt *could* resolve
/// (`invocation_unset`: the human opted in, this run has not) must leave both of
/// them entirely unmoved.
///
/// Run with stdin at `/dev/null`, which is what `Command::output` gives them —
/// so a prompt would not merely be unwanted here, it would hang or fail.
#[test]
fn status_and_dry_run_never_prompt_even_when_a_prompt_could_open_the_gate() {
    let dir = fresh_repo("never-prompt");
    project_layer_at(&dir, UNREACHABLE_LOOPBACK, "");
    user_layer(&dir, "[remote]\nenabled = true\n");

    // The gate is one prompt away from opening. Neither command may take it.
    let gate = status_json(&dir, &[]);
    assert_eq!(gate["reason"], "invocation_unset", "the promptable state");

    for args in [
        vec!["remote", "status"],
        vec!["remote", "dry-run", "anything at all"],
    ] {
        let out = roteiro(&dir, &args);
        assert!(out.status.success(), "{args:?} failed: {out:?}");
        let text = format!("{}{}", stdout(&out), stderr(&out));
        assert!(
            !text.contains("[y/N]"),
            "{args:?} asked for consent it does not need: {text}"
        );
        assert!(
            !text.contains("Send this now?"),
            "{args:?} asked to send: {text}"
        );
    }
    assert!(
        ledger(&dir).is_empty(),
        "and neither of them disclosed anything"
    );
}

/// **A pipe cannot consent.** With the user layer granting and no
/// `--allow-remote`, an interactive terminal is asked; a non-interactive one is
/// *refused*, because treating an unattended run as a yes is precisely the
/// consent-by-default this ADR exists to prevent.
///
/// The refusal names the flag that would work, so the answer to "how do I run
/// this in CI?" is a deliberate flag rather than a discovered loophole.
#[test]
fn a_non_interactive_run_is_refused_rather_than_assumed_to_consent() {
    let dir = fresh_repo("non-interactive");
    project_layer_at(&dir, UNREACHABLE_LOOPBACK, "");
    user_layer(&dir, "[remote]\nenabled = true\n");

    let out = roteiro(&dir, &["remote", "call", "what is this?"]);
    assert!(!out.status.success(), "nobody granted this run");
    let text = stderr(&out);
    assert!(text.contains("not interactive"), "{text}");
    assert!(text.contains("Nothing was sent"), "{text}");
    assert!(text.contains("--allow-remote"), "{text}");
    assert!(
        ledger(&dir).is_empty(),
        "a refusal disclosed nothing, so it records nothing"
    );
}

/// **A denied call stops before the transport exists to it.** `--no-remote` with
/// every other layer granting: nothing assembled, nothing recorded, nothing
/// sent — and the empty ledger is the proof, since a call that reached the
/// transport would have had to write its egress line first.
#[test]
fn a_denied_call_reaches_no_transport_and_leaves_no_record() {
    let dir = fresh_repo("call-denied");
    project_layer_at(&dir, UNREACHABLE_LOOPBACK, "");
    user_layer(&dir, "[remote]\nenabled = true\n");

    let out = roteiro(&dir, &["remote", "call", "--no-remote", "what is this?"]);
    assert!(!out.status.success(), "this run denied itself");
    let text = stderr(&out);
    assert!(text.contains("not enabled for this run"), "{text}");
    assert!(ledger(&dir).is_empty(), "nothing left, so nothing recorded");

    // A project-wide denial is the same story with a remedy nobody local can
    // act on, and it too must never reach the transport.
    project_layer_at(&dir, UNREACHABLE_LOOPBACK, "enabled = false\n");
    let out = roteiro(&dir, &["remote", "call", "--allow-remote", "what is this?"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("no flag overrides that"),
        "{}",
        stderr(&out)
    );
    assert!(ledger(&dir).is_empty());
}

/// **The credential is reported as a fact, never as a value.** It lives in an
/// environment variable rather than a config key because `roteiro.toml` is
/// committed by design — and a `status` command that echoed it would put it in
/// every terminal scrollback and CI log that ran one.
#[test]
fn status_reports_the_backend_and_that_a_credential_is_set_but_never_its_value() {
    const SECRET: &str = "sk-do-not-print-this-anywhere";

    let dir = fresh_repo("credential");
    project_layer_at(&dir, UNREACHABLE_LOOPBACK, "");

    let out = Command::new(BIN)
        .args(["remote", "status", "--json"])
        .current_dir(&dir)
        .env("ROTEIRO_HOME", &dir)
        .env("HOME", &dir)
        .env("USERPROFILE", &dir)
        .env("ROTEIRO_REMOTE_API_KEY", SECRET)
        .output()
        .expect("run roteiro");
    let text = stdout(&out);
    assert!(!text.contains(SECRET), "the credential was printed: {text}");

    let gate: serde_json::Value = serde_json::from_str(&text).expect("JSON");
    assert_eq!(gate["credential_set"], true);
    assert_eq!(gate["credential_env"], "ROTEIRO_REMOTE_API_KEY");
    assert_eq!(
        gate["backend"], "ureq",
        "this build can send, and says so rather than leaving it to be discovered"
    );

    // Without the variable, the same command says so — an absent credential is a
    // supported configuration (a loopback gateway often needs none), not an error.
    let gate = status_json(&dir, &[]);
    assert_eq!(gate["credential_set"], false);
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

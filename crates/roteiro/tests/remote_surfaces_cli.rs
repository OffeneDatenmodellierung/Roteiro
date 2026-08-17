//! End-to-end tests for the **surfaces** the remote model tier reaches
//! (ADR-0019, Stage 34 part 2b) — `spec draft` and Ask, as distinct from
//! `roteiro remote call`.
//!
//! `remote_consent_cli.rs` proves the gate and the transport on the command that
//! exists to send. This file proves the harder half: that the two surfaces where
//! a person could send repository content **without typing `remote`** cannot do
//! so unaware.
//!
//! Three claims, and each one is a failure mode rather than a feature:
//!
//! 1. **No flag, no egress** — even with the user layer granting, and even on a
//!    TTY. There is no prompt on these paths, because a prompt on a path whose
//!    default is local turns a habituated "y" into consent-by-default.
//! 2. **A refused `--allow-remote` stops the run.** Someone who typed the flag
//!    asked for the hosted model; drafting locally instead is a different answer
//!    with no signal that anything changed — ADR-0019 §6's named failure,
//!    arriving through the consent gate rather than through a socket.
//! 3. **The allow-list still assembles the request.** The local path interpolates
//!    grounded symbol names into a prompt *string*; the remote path rebuilds
//!    through `rto_remote::Payload`, so what leaves is decided by
//!    `ContextItem::from_node` and nothing else. Asserted against the recorded
//!    body, which is the bytes.
//!
//! # No test here makes a network call, and none can
//!
//! Same ground as part 2a's, and for the same reason. The only endpoint any test
//! grants a call to is [`UNREACHABLE_LOOPBACK`] — port 1 on `127.0.0.1`. Loopback
//! is not a network, so the kernel has nowhere else to put the bytes; it is a
//! literal address, so there is no DNS query to leak the question to a resolver
//! (ADR-0019 §2's reason a probe *is* egress); and nothing listens there, so
//! `connect` is refused in microseconds. Every other test is refused **before**
//! the transport, which is asserted rather than assumed: the ledger is written
//! before the send, so an empty ledger proves nothing left.
#![cfg(feature = "remote")]

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_roteiro");

/// See the module docs. The one address a test may grant a call to.
const UNREACHABLE_LOOPBACK: &str = "http://127.0.0.1:1/v1/chat/completions";

/// The vendor model string these tests configure — deliberately shaped like a
/// dated vendor pointer, since that mutability is what `VendorAsserted` exists
/// to declare.
const MODEL: &str = "some-vendor/model-2026-05";

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
/// `dir/roteiro.toml` the **project** layer.
///
/// Stdin is closed, so no test can be answered by an interactive prompt even if
/// one appeared — which is itself one of the things under test.
fn roteiro(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("ROTEIRO_HOME", dir)
        .env("HOME", dir)
        .env("USERPROFILE", dir)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run roteiro")
}

/// A repository with **both** prose and a real code symbol, so `spec context`
/// grounds against something.
///
/// The code symbol is load-bearing rather than decoration:
/// `a_granted_draft_records_its_egress_and_still_refuses_to_fall_back` asserts
/// that the *local* prompt string did not reach the wire, and the local prompt
/// only names symbols when it has some. Without a symbol here that assertion
/// would hold vacuously and would catch nothing.
fn fresh_repo(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "roteiro-remote-surface-{label}-{}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("src")).expect("mkdir");
    git(&dir, &["init", "-q"]);
    std::fs::write(
        dir.join("README.md"),
        "# fixture\n\n## Store\n\nThe store holds nodes and edges.\n",
    )
    .expect("write");
    std::fs::write(
        dir.join("src/store.rs"),
        "/// The store of nodes and edges.\npub struct Store;\n\n         pub fn store_nodes(store: &Store) {\n    let _ = store;\n}\n",
    )
    .expect("write");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    dir
}

/// Write the **project** layer (`roteiro.toml`) — committed and shared, so it may
/// deny but never grant.
fn project_layer(dir: &Path, endpoint: &str, body: &str) {
    std::fs::write(
        dir.join("roteiro.toml"),
        format!("[remote]\nendpoint = \"{endpoint}\"\nmodel = \"{MODEL}\"\n{body}"),
    )
    .expect("write");
}

/// Write the **user** layer (`$ROTEIRO_HOME/config.toml`) — this human's own file,
/// the only one that may grant.
fn user_layer(dir: &Path, body: &str) {
    std::fs::write(dir.join("config.toml"), body).expect("write");
}

/// The egress ledger's entries, oldest first. A missing ledger reads as empty,
/// which is exactly the assertion most tests here are making.
fn ledger(dir: &Path) -> Vec<serde_json::Value> {
    let Ok(text) = std::fs::read_to_string(dir.join("remote").join("egress.jsonl")) else {
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

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// **A `spec draft --allow-remote` the gate refuses stops the run.**
///
/// The core claim of this file. The person typed the flag, so they asked for the
/// hosted model's answer; producing a local draft or a bare scaffold instead
/// would hand them a *different answer with no signal that anything changed*.
/// ADR-0019 calls the unannounced downgrade the failure mode it most needs to
/// prevent, and a consent refusal is one of its two shapes — the other being the
/// network failure `remote_consent_cli.rs` covers.
///
/// The user layer is unset here, which is the default state on every machine, so
/// this is also what a `--allow-remote` in a script hits on a colleague's laptop.
#[test]
fn spec_draft_allow_remote_refused_stops_the_run_rather_than_drafting_locally() {
    let dir = fresh_repo("draft-refused");
    project_layer(&dir, UNREACHABLE_LOOPBACK, "");
    // No user layer at all — nobody on this machine opted in.

    let out = roteiro(&dir, &["spec", "draft", "store", "--allow-remote"]);
    assert!(!out.status.success(), "a refused grant must not succeed");

    let err = stderr(&out);
    assert!(
        err.contains("did **not** fall back"),
        "says it did not degrade: {err}"
    );
    assert!(
        err.contains("~/.roteiro/config.toml"),
        "names the layer that would change it: {err}"
    );
    assert!(
        err.contains("--allow-remote"),
        "and the deliberate local alternative: {err}"
    );
    assert!(
        !stdout(&out).contains("_TODO"),
        "no scaffold was emitted as a consolation: {}",
        stdout(&out)
    );
    assert!(
        ledger(&dir).is_empty(),
        "a refusal disclosed nothing, so it records nothing"
    );
}

/// **A project-layer denial cannot be overridden, and says so.** A locked-down
/// repository is a legitimate thing to express (ADR-0019 §3), and the remedy must
/// not send the reader to edit a file that would not help.
#[test]
fn a_project_denial_refuses_spec_draft_with_the_repository_remedy() {
    let dir = fresh_repo("draft-project-denied");
    project_layer(&dir, UNREACHABLE_LOOPBACK, "enabled = false\n");
    // Both granting layers grant, and it still loses.
    user_layer(&dir, "[remote]\nenabled = true\n");

    let out = roteiro(&dir, &["spec", "draft", "store", "--allow-remote"]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(
        err.contains("no flag overrides"),
        "does not advise editing a file that would not help: {err}"
    );
    assert!(ledger(&dir).is_empty(), "nothing left the machine");
}

/// **A committed `enabled = true` is reported, never swallowed.** It is doing
/// something reasonable and wrong; leaving a team to wonder why their setting
/// does nothing is worse than refusing it out loud.
#[test]
fn an_ignored_project_grant_is_reported_on_the_draft_surface_too() {
    let dir = fresh_repo("draft-ignored-grant");
    project_layer(&dir, UNREACHABLE_LOOPBACK, "enabled = true\n");

    let out = roteiro(&dir, &["spec", "draft", "store", "--allow-remote"]);
    assert!(!out.status.success(), "a committed grant is not a grant");
    let err = stderr(&out);
    assert!(err.contains("read and ignored"), "{err}");
    assert!(ledger(&dir).is_empty());
}

/// **The user layer granting is not enough, and there is no prompt.**
///
/// This is the case the surface exists to make safe: the human opted in once, in
/// their own file, and every `spec draft` from then on must still be local unless
/// this run says otherwise. `roteiro remote call` shows a TTY the bytes and asks;
/// here the default *is* local, and a prompt on a default path is how consent
/// becomes a reflex.
///
/// Stdin is closed, so a prompt would hang or fail rather than pass — the test
/// asserts the run completed without one and disclosed nothing.
#[test]
fn a_granting_user_layer_without_the_flag_sends_nothing_and_never_asks() {
    let dir = fresh_repo("draft-no-flag");
    project_layer(&dir, UNREACHABLE_LOOPBACK, "");
    user_layer(&dir, "[remote]\nenabled = true\n");

    let out = roteiro(&dir, &["spec", "draft", "store"]);
    let err = stderr(&out);
    assert!(
        !err.contains("Send this now?"),
        "this surface must never prompt: {err}"
    );
    assert!(
        ledger(&dir).is_empty(),
        "the user layer alone never sends: {:?}",
        ledger(&dir)
    );
}

/// **`--no-remote` is a deliberate local run**, and it is not the refusal case:
/// the person said no, so nothing is announced and nothing is refused.
#[test]
fn no_remote_is_a_quiet_local_run() {
    let dir = fresh_repo("draft-no-remote");
    project_layer(&dir, UNREACHABLE_LOOPBACK, "");
    user_layer(&dir, "[remote]\nenabled = true\n");

    let out = roteiro(&dir, &["spec", "draft", "store", "--no-remote"]);
    let err = stderr(&out);
    assert!(
        !err.contains("did **not** fall back"),
        "nobody asked for the tier, so nothing was downgraded: {err}"
    );
    assert!(ledger(&dir).is_empty());
}

/// **The whole granted path, end to end — and it still refuses to degrade.**
///
/// Both layers grant and the endpoint is [`UNREACHABLE_LOOPBACK`], so the call is
/// assembled, recorded, attempted and refused. Four things are proved at once,
/// and only this test can prove them together:
///
/// * the surface reaches `rto_remote::call_with` at all (the ledger line exists);
/// * the record is written **before** the send (it survives a call that failed);
/// * the failure names the endpoint and says it did not fall back to a local
///   model — with a llama.cpp build present under `--all-features`, so falling
///   back was a real option that was not taken;
/// * **the payload was rebuilt through the allow-list**, not lifted from the
///   local prompt. `rto_spec::draft_prompt` interpolates grounded symbol names
///   into a string with the phrase "Relevant code symbols:"; the recorded body
///   must carry the rebuilt instruction instead, with graph content present only
///   as allow-listed context items.
#[test]
fn a_granted_draft_records_its_egress_and_still_refuses_to_fall_back() {
    let dir = fresh_repo("draft-granted");
    project_layer(&dir, UNREACHABLE_LOOPBACK, "");
    user_layer(&dir, "[remote]\nenabled = true\n");

    let out = roteiro(&dir, &["spec", "draft", "store", "--allow-remote"]);
    assert!(!out.status.success(), "nothing is listening on port 1");
    let err = stderr(&out);
    assert!(
        err.contains(UNREACHABLE_LOOPBACK),
        "names the endpoint: {err}"
    );
    assert!(
        err.contains("did **not** fall back"),
        "refuses to degrade even with a local model available: {err}"
    );

    let entries = ledger(&dir);
    assert!(
        !entries.is_empty(),
        "the egress is recorded before it is attempted"
    );
    let egress = &entries[0];
    assert_eq!(egress["endpoint"], UNREACHABLE_LOOPBACK);
    assert_eq!(egress["model"], MODEL);
    assert_eq!(
        egress["trust"], "vendor_asserted",
        "a hosted model's identity is a claim, and the record says so"
    );

    let body = egress["body"].as_str().expect("the recorded body");
    assert!(
        body.contains("You are drafting"),
        "the rebuilt instruction is what was sent: {body}"
    );
    assert!(
        body.contains("sym:") || body.contains("doc:"),
        "and the graph reached it as context items, so grounding did happen: {body}"
    );
    // The two phrases `rto_spec::draft_prompt` emits and the rebuild does not.
    // The first is unconditional in that function; the second appears only when
    // it has symbols to name, which `fresh_repo` guarantees it does — so neither
    // assertion can pass vacuously.
    for lifted in ["Reference the real symbols above", "Relevant code symbols:"] {
        assert!(
            !body.contains(lifted),
            "the local prompt string must not be lifted onto the wire — graph content \
             reaches the endpoint only through the allow-list, and `{lifted}` is how it \
             would arrive if it did not: {body}"
        );
    }
}

/// **`spec draft --allow-remote` with no `[remote] endpoint` fails at the
/// configuration, not at the wire.** The gate opened, so the run is entitled to a
/// destination; refusing here means nothing was assembled and nothing recorded.
#[test]
fn a_granted_draft_without_an_endpoint_names_the_missing_key() {
    let dir = fresh_repo("draft-no-endpoint");
    user_layer(&dir, "[remote]\nenabled = true\n");
    // No project layer at all, so `[remote] endpoint` is unset.

    let out = roteiro(&dir, &["spec", "draft", "store", "--allow-remote"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("`[remote] endpoint` is not set"),
        "{}",
        stderr(&out)
    );
    assert!(ledger(&dir).is_empty());
}

/// **Default-off, with everything but the consent in place.**
///
/// A destination is configured — so nothing but the gate can be what stops this
/// — and no layer grants and no flag is passed. That is the state of every
/// machine whose owner has not opted in, and it must send nothing.
///
/// Configured deliberately rather than left bare: an unconfigured repository
/// would also send nothing because there is nowhere to send *to*, which would
/// make this pass for the wrong reason and detect nothing if the gate itself
/// ever defaulted open.
///
/// Asserted on the receipt phrase rather than on the words "remote model tier",
/// because a build carrying the tier and no local generator legitimately *names*
/// it when explaining what a draft would need — offering a capability is not
/// using one.
#[test]
fn a_configured_but_unconsented_repository_drafts_nothing_remotely() {
    let dir = fresh_repo("draft-default-off");
    project_layer(&dir, UNREACHABLE_LOOPBACK, "");

    let out = roteiro(&dir, &["spec", "draft", "store"]);
    let err = stderr(&out);
    assert!(
        !err.contains("via the remote model tier"),
        "nothing was drafted remotely: {err}"
    );
    assert!(
        !err.contains("bytes total"),
        "no receipt was printed: {err}"
    );
    assert!(
        ledger(&dir).is_empty(),
        "the tier is off until both layers grant: {:?}",
        ledger(&dir)
    );
}

/// **A `[remote] model` that squats on a local model id is refused, loudly.**
///
/// The failure it prevents is not a duplicate listing. Under
/// `serve --allow-remote` a `[remote] model = "qwen3-0.6b"` makes that id a
/// served model, so every request naming it is answered by the hosted endpoint —
/// and `qwen3-0.6b` reads, to anyone looking, as the name of a local GGUF. The
/// user layer granted, so the egress is authorised; it is *unrecognisable*, and
/// ADR-0019 treats that as the same harm one step removed, because §1's gate is
/// one somebody opened knowingly and §4's payload is one they can inspect.
///
/// Asserted on the **rendered message**, not the variant: an operator who cannot
/// see which local id was collided with has been told their configuration is
/// wrong and not which part.
///
/// Refused at `remote_endpoint`, the single constructor, so it reaches every
/// surface — checked here through both a sending one (`spec draft`) and an
/// inspecting one (`remote status`), since someone deciding whether to grant the
/// tier should find out before they do rather than after.
#[test]
fn a_remote_model_colliding_with_a_local_id_is_refused_naming_both() {
    let dir = fresh_repo("model-collision");
    std::fs::write(
        dir.join("roteiro.toml"),
        format!("[remote]\nendpoint = \"{UNREACHABLE_LOOPBACK}\"\nmodel = \"qwen3-0.6b\"\n"),
    )
    .expect("write");
    user_layer(&dir, "[remote]\nenabled = true\n");

    let out = roteiro(&dir, &["spec", "draft", "store", "--allow-remote"]);
    assert!(!out.status.success(), "a colliding id must not be usable");
    let err = stderr(&out);
    assert!(
        err.contains("[remote] model"),
        "names the key that is wrong: {err}"
    );
    assert!(
        err.contains("qwen3-0.6b"),
        "and the local id it collides with: {err}"
    );
    assert!(
        err.contains("roteiro model list"),
        "actionable — the names that are taken: {err}"
    );
    assert!(
        ledger(&dir).is_empty(),
        "nothing left the machine under a borrowed name: {:?}",
        ledger(&dir)
    );

    // The inspecting surface reports it too, so the refusal is discoverable
    // before a grant rather than only at the moment of one.
    let status = roteiro(&dir, &["remote", "status", "--json"]);
    assert!(status.status.success(), "status still reports: {status:?}");
    let gate: serde_json::Value =
        serde_json::from_str(&stdout(&status)).expect("status emits JSON");
    let reported = gate["endpoint_error"]
        .as_str()
        .expect("an endpoint error is reported");
    assert!(reported.contains("[remote] model"), "{reported}");
    assert!(reported.contains("qwen3-0.6b"), "{reported}");
}

/// A vendor string that is *not* a registry name passes untouched — the guard
/// refuses a collision, not a hosted model.
#[test]
fn a_vendor_model_string_that_is_not_a_registry_name_is_accepted() {
    let dir = fresh_repo("model-no-collision");
    project_layer(&dir, UNREACHABLE_LOOPBACK, "");
    user_layer(&dir, "[remote]\nenabled = true\n");

    let status = roteiro(&dir, &["remote", "status", "--json"]);
    let gate: serde_json::Value =
        serde_json::from_str(&stdout(&status)).expect("status emits JSON");
    assert!(
        gate["endpoint_error"].is_null(),
        "`{MODEL}` is nobody's registry entry: {gate}"
    );
    assert_eq!(gate["model"], MODEL);
}

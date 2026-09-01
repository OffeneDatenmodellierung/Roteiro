//! SIGHUP must never kill a Roteiro server, and a reload must move **every**
//! surface the server offers.
//!
//! # Why this is an end-to-end test and not a unit test
//!
//! The defect was that SIGHUP's *default disposition* terminated the process:
//! `roteiro explorer` and single-repo `roteiro serve`/`mcp` installed no handler
//! at all, and died with exit 129. Nothing about that is visible from inside the
//! process — a unit test calling `Workspace::reload_from` passes on a tree whose
//! servers all die on the signal, which is exactly what happened. The only
//! assertion that catches it is: spawn the real binary, send it the real signal,
//! and look for a live process afterwards.
//!
//! # Why the server list is read out of `main.rs`
//!
//! A hard-coded list of three is how the drift happened in the first place: the
//! handler lived at the tail of one builder that two of the three server paths
//! never reached. So [`server_commands_declared_in_main`] parses the `=> true`
//! arms of `is_long_lived_server` — the exhaustive match `main` installs the
//! handler from — and [`every_long_lived_server_survives_sighup`] fails if the
//! table below does not cover every one of them. Adding a fourth server means
//! classifying it there (the match has no `_` arm, so it will not compile
//! otherwise) and signalling it here.

#![cfg(all(unix, any(feature = "serve", feature = "explorer", feature = "mcp")))]

#[cfg(feature = "explorer")]
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

mod common;
use common::{IsolatedHome, repo_file, scratch_dir};

const BIN: &str = env!("CARGO_BIN_EXE_roteiro");

/// How each long-lived server is invoked for the signal test, keyed by its
/// `Command` variant name in `main.rs`.
///
/// **Every case binds a port**, deliberately: an accepted TCP connection is a
/// readiness signal the test can wait *for*, and a `sleep` is a readiness signal
/// it can only hope for — which on a loaded runner is how an end-to-end signal
/// test turns into a flake. `mcp` is therefore signalled over `--http` rather
/// than stdio (which prints nothing at startup and so offers nothing to wait
/// for); the handler under test is installed in `main` before either transport
/// is chosen, so the two are the same code path from this guard's point of view.
///
/// `runnable` is narrower than "the subcommand parses": some of these exist as
/// clap variants in feature sets that cannot actually serve, and refuse with an
/// error instead of staying up (`roteiro mcp` in a build without `mcp` says so
/// and exits 1). A build that cannot run one skips its **spawn** — never the
/// coverage assertion, which is what makes a newly added server fail here rather
/// than slip through. CI's `--all-features` cell runs all three.
struct ServerCase {
    /// The `Command` variant name, matching `is_long_lived_server`.
    variant: &'static str,
    /// Arguments after the binary; `{addr}` is substituted with a free address.
    args: &'static [&'static str],
    /// Whether this build can actually run this server (see above).
    runnable: bool,
}

fn cases() -> Vec<ServerCase> {
    vec![
        ServerCase {
            variant: "Serve",
            args: &["serve", "--addr", "{addr}"],
            // With no model installed — these tests use an isolated home, so
            // there is none — `serve` degrades to the llama-free graph API + UI,
            // which is the `explorer` feature. Without it the command reports
            // that this build has no network server and exits.
            runnable: cfg!(feature = "explorer"),
        },
        ServerCase {
            variant: "Mcp",
            args: &["mcp", "--http", "{addr}"],
            // The variant exists under `serve` too, but only `mcp` can serve it.
            runnable: cfg!(feature = "mcp"),
        },
        ServerCase {
            variant: "Explorer",
            args: &["explorer", "--addr", "{addr}"],
            runnable: cfg!(feature = "explorer"),
        },
    ]
}

// ---------------------------------------------------------------------------
// The guards
// ---------------------------------------------------------------------------

/// Every server `main` will install the SIGHUP handler for must survive the
/// signal. Driven from `is_long_lived_server`'s `=> true` arms, so a fourth
/// server is covered or this test is red.
#[test]
fn every_long_lived_server_survives_sighup() {
    let declared = server_commands_declared_in_main();
    let cases = cases();
    let covered: Vec<&str> = cases.iter().map(|c| c.variant).collect();
    for variant in &declared {
        assert!(
            covered.contains(&variant.as_str()),
            "`{variant}` is classified as a long-lived server in \
             `is_long_lived_server`, but this test never signals it. SIGHUP's \
             default disposition kills a process, so an unsignalled server is an \
             unverified one — add a `ServerCase` for it."
        );
    }
    for variant in &covered {
        assert!(
            declared.contains(&(*variant).to_owned()),
            "this test signals `{variant}`, but `is_long_lived_server` no longer \
             classifies it as a server — the two lists have drifted, which is the \
             defect this guard exists for."
        );
    }

    let base = scratch_dir("sighup-survives");
    std::fs::create_dir_all(&base).expect("mkdir");
    let repo = base.join("solo");
    make_repo(&repo);

    for case in cases.iter().filter(|c| c.runnable) {
        let addr = free_addr();
        let args: Vec<String> = case
            .args
            .iter()
            .map(|a| a.replace("{addr}", &addr))
            .collect();
        let home = IsolatedHome::new("sighup-survives");
        let mut server = Server::spawn(&args, &repo, &home);

        wait_for_port(&addr, &mut server.child, case.variant);

        sighup(&server.child);
        std::thread::sleep(Duration::from_millis(1000));

        let status = server.child.try_wait().expect("try_wait");
        assert!(
            status.is_none(),
            "`roteiro {}` died on SIGHUP ({status:?}) — the signal's default \
             disposition terminates the process, so this server registered no \
             handler. Exit 129 is 128 + SIGHUP.",
            args.join(" "),
        );
    }
    std::fs::remove_dir_all(&base).ok();
}

/// A workspace-mode `serve` must reload **both** of its surfaces on SIGHUP, and
/// must say so truthfully.
///
/// The original defect: the handler reloaded only the flattened workspace behind
/// the model tools and MCP router, never the `WorkspaceSet` behind `/v1/graph/*`
/// and the UI. So the server logged `workspace reloaded: 3 project(s) — one,
/// three, two` and went on serving two. The assertion that catches that is the
/// **comparison**, not either half: the reload line's project list is produced
/// from the flat view, `/v1/graph/projects` is produced from the set, and they
/// have to be equal. Asserting only that the log said three passes on the broken
/// tree.
#[test]
#[cfg(feature = "explorer")]
fn sighup_reloads_the_graph_api_and_the_flat_view_together() {
    let base = scratch_dir("sighup-reload");
    let root = base.join("ws");
    std::fs::create_dir_all(&root).expect("mkdir");
    for name in ["one", "two"] {
        make_repo(&root.join(name));
    }

    let addr = free_addr();
    let home = IsolatedHome::new("sighup-reload");
    let args = [
        "serve".to_owned(),
        "--workspace".to_owned(),
        root.to_str().expect("utf-8 root").to_owned(),
        "--addr".to_owned(),
        addr.clone(),
    ];
    let mut server = Server::spawn(&args, &root.join("one"), &home);
    // Collect stderr on a thread: the reload line is the flat view's own report,
    // and reading it inline would deadlock on the pipe.
    let stderr = server.child.stderr.take().expect("piped stderr");
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
        {
            let _ = tx.send(line);
        }
    });

    wait_for_port(&addr, &mut server.child, "Serve");
    assert_eq!(
        projects(&addr),
        vec!["one".to_owned(), "two".to_owned()],
        "the graph API should host the two repos under the workspace root"
    );

    // A third repo appears under the already-configured root — the case SIGHUP
    // exists for.
    make_repo(&root.join("three"));
    sighup(&server.child);

    // Wait for the reload line, which is emitted after both swaps.
    let reported = wait_for_reload_line(&rx, &mut server.child);
    let expected = vec!["one".to_owned(), "three".to_owned(), "two".to_owned()];
    assert_eq!(
        reported, expected,
        "the reload line should name the three hosted projects"
    );

    // The assertion that would have caught the defect: what the server *says* it
    // reloaded and what the graph API *serves* must be the same list.
    let via_graph_api = wait_for_projects(&addr, &expected);
    assert_eq!(
        via_graph_api, reported,
        "`/v1/graph/*` (the WorkspaceSet, which also backs the explorer UI) and \
         the flattened workspace the reload line reports from must host the same \
         projects after a SIGHUP. Disagreeing is the defect: the server printed a \
         message saying it reloaded three projects and then served two."
    );

    // `server` kills the child on drop, on this path and on a panicking one.
    std::fs::remove_dir_all(&base).ok();
}

// ---------------------------------------------------------------------------
// Reading the server list out of `main.rs`
// ---------------------------------------------------------------------------

/// The `Command` variant names `is_long_lived_server` classifies as servers.
///
/// Parses the function body rather than trusting a copy of the list, so this
/// test's coverage is defined by the code that installs the handler. Panics if
/// the function cannot be found or yields nothing — a guard that silently scans
/// an empty text is a green meaning "could not look".
fn server_commands_declared_in_main() -> Vec<String> {
    let Some(source) = repo_file("crates/roteiro/src/main.rs") else {
        // Not a repository checkout (a packaged crate has no sibling sources).
        return cases().iter().map(|c| c.variant.to_owned()).collect();
    };
    let marker = "fn is_long_lived_server(cmd: &Command) -> bool {";
    let start = source.find(marker).unwrap_or_else(|| {
        panic!(
            "`{marker}` not found in crates/roteiro/src/main.rs. This guard's \
             coverage is defined by that function; if it was renamed, rename it \
             here too rather than letting the scan go vacuous."
        )
    });
    let body = &source[start..];
    let end = body
        .find("\n}\n")
        .expect("unterminated `is_long_lived_server` body");
    let body = &body[..end];

    let mut out: Vec<String> = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        // `Command::Serve { .. } => true,`
        let Some(rest) = line.strip_prefix("Command::") else {
            continue;
        };
        if !rest.ends_with("=> true,") {
            continue;
        }
        let name: String = rest
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect();
        assert!(!name.is_empty(), "unparsable server arm: {line}");
        out.push(name);
    }
    assert!(
        !out.is_empty(),
        "no `=> true` arms parsed out of `is_long_lived_server`. Either every \
         server stopped being long-lived (it did not) or the arm shape changed \
         and this scan is now vacuous."
    );
    out
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A spawned server that is killed when it goes out of scope — **including on a
/// panic**.
///
/// Not tidiness. Every assertion in this file is about a process that is still
/// running, so every failure here leaves one behind; a plain `child.kill()` after
/// the assertion is skipped by the very unwind that matters. Left alone the
/// orphan keeps its port, and the next run of this test (or another one) meets a
/// bind failure that looks nothing like the original defect. Found the honest
/// way: a fault-injection run leaked a listener.
struct Server {
    child: Child,
}

impl Server {
    /// Spawn `args` in `cwd` with an isolated config home, holding both so they
    /// outlive the child.
    fn spawn(args: &[String], cwd: &Path, home: &IsolatedHome) -> Self {
        let mut command = Command::new(BIN);
        command
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        home.apply(&mut command);
        let child = command
            .spawn()
            .unwrap_or_else(|e| panic!("spawn roteiro {args:?}: {e}"));
        Self { child }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

/// A fresh git repo with one commit, so `serve`/`mcp` can build a graph for it.
fn make_repo(dir: &Path) {
    std::fs::create_dir_all(dir).expect("mkdir repo");
    git(dir, &["init", "-q", "."]);
    std::fs::write(dir.join("README.md"), "# fixture\n").expect("write README");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "init"]);
}

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
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

/// A loopback address nothing is listening on, by binding port 0 and releasing
/// it. Racy in principle; in practice the kernel does not immediately re-hand
/// the same ephemeral port, and the alternative (a fixed port) collides between
/// concurrent test binaries for certain.
fn free_addr() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    drop(listener);
    format!("127.0.0.1:{}", addr.port())
}

/// Send SIGHUP to `child` via `kill(1)` — no `libc` dependency, and this file is
/// `#![cfg(unix)]`.
fn sighup(child: &Child) {
    let status = Command::new("kill")
        .args(["-HUP", &child.id().to_string()])
        .status()
        .expect("run kill");
    assert!(status.success(), "kill -HUP {} failed", child.id());
}

/// Block until the server answers on `addr`, failing loudly if it exits first.
fn wait_for_port(addr: &str, child: &mut Child, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().expect("try_wait") {
            panic!("`{what}` exited before binding {addr}: {status}");
        }
        if TcpStream::connect(addr).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("`{what}` never bound {addr}");
}

/// `GET /v1/graph/projects` → the hosted project names, in served order.
// Only the reload test below uses these: a build without `explorer` serves no
// `/v1/graph/*`, so there is nothing to read them from.
#[cfg(feature = "explorer")]
fn projects(addr: &str) -> Vec<String> {
    let body = http_get(addr, "/v1/graph/projects");
    // Deliberately a hand-rolled read of `{"isMulti":…,"projects":["a","b"]}`:
    // this test binary carries no JSON dependency, and the shape is fixed by
    // `graph_api`'s own tests.
    let start = body
        .find("\"projects\":[")
        .unwrap_or_else(|| panic!("no `projects` array in {body}"))
        + "\"projects\":[".len();
    let end = start
        + body[start..]
            .find(']')
            .unwrap_or_else(|| panic!("unterminated `projects` array in {body}"));
    body[start..end]
        .split(',')
        .map(|s| s.trim().trim_matches('"').to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Poll `/v1/graph/projects` until it reports `expected`, or return whatever it
/// last reported so the assertion can print the difference.
// Only the reload test below uses these: a build without `explorer` serves no
// `/v1/graph/*`, so there is nothing to read them from.
#[cfg(feature = "explorer")]
fn wait_for_projects(addr: &str, expected: &[String]) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut last = Vec::new();
    while Instant::now() < deadline {
        last = projects(addr);
        if last == expected {
            return last;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    last
}

/// Wait for the `workspace reloaded: …` stderr line and return the project names
/// it reports.
// Only the reload test below uses these: a build without `explorer` serves no
// `/v1/graph/*`, so there is nothing to read them from.
#[cfg(feature = "explorer")]
fn wait_for_reload_line(rx: &std::sync::mpsc::Receiver<String>, child: &mut Child) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(line) => {
                if let Some(rest) = line.strip_prefix("workspace reloaded: ") {
                    // `N workspace(s) [..] — M project(s) — a, b, c`
                    let tail = rest
                        .rsplit_once(" — ")
                        .unwrap_or_else(|| panic!("unexpected reload line: {line}"))
                        .1;
                    return tail.split(", ").map(str::to_owned).collect();
                }
                assert!(
                    !line.starts_with("workspace reload failed"),
                    "the reload failed: {line}"
                );
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if let Some(status) = child.try_wait().expect("try_wait") {
                    panic!("server died while waiting for the reload line: {status}");
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    panic!("no `workspace reloaded:` line within the deadline");
}

/// A minimal blocking HTTP/1.1 GET — this test binary has no HTTP client, and
/// pulling one in for two requests is not worth a dependency.
// Only the reload test below uses these: a build without `explorer` serves no
// `/v1/graph/*`, so there is nothing to read them from.
#[cfg(feature = "explorer")]
fn http_get(addr: &str, path: &str) -> String {
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("read timeout");
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
    )
    .expect("write request");
    let mut raw = String::new();
    stream.read_to_string(&mut raw).expect("read response");
    raw.split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("malformed HTTP response: {raw}"))
        .1
        .to_owned()
}

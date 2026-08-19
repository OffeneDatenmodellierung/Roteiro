//! ADR-0020 conditions 1 and 2, executed rather than argued.
//!
//! > A writable build directory that does not relax the read-only preflight for
//! > readers, and a demonstrated refusal path that never falls back to the host.
//!
//! This runs a **real `cargo clippy`** inside a real microVM against a real
//! worktree, and then asks the only question that matters afterwards: is the
//! tree exactly as it was? A builder that reported beautifully and left a
//! `target/` behind would have failed at the thing this ADR is about.
//!
//! # Why the tree is compared by content, not by listing
//!
//! `Cargo.lock` is rewritten **in place**. A listing of file names cannot see
//! that, and an earlier test of the host path was green while the linter was
//! modifying the tree it was reporting on (ADR-0020 v1.4). So every file is
//! digested, and the digest of the digests is compared.
//!
//! # Skipping
//!
//! Every precondition prints why it skipped. A silent skip is how a test that
//! covers nothing gets mistaken for a test that passed. The image in particular
//! is **supplied by the operator** — roteiro ships no default, because no
//! first-party Rust image carries `clippy` — so this test cannot provision its
//! own way to green, and says so rather than quietly passing.

#![cfg(all(feature = "exec-subprocess", feature = "exec-boxlite"))]

use std::path::Path;
use std::process::Command;

use rto_exec::{FeatureSet, LintConfigGrant, LintRequested, decide_lint_host};
use rto_graph::Isolation;

/// The environment variable that supplies the image, and the sentence a skip
/// prints when it is unset.
///
/// A variable rather than the config file because this crate has no config
/// loader — `[lint] image` is read by the binary — and inventing a second reader
/// for it here would be a second implementation of a rule that has one.
const IMAGE_VAR: &str = "ROTEIRO_TEST_LINT_IMAGE";

/// One diagnostic clippy has fired on since long before this test, in a crate
/// with **no dependencies**.
///
/// No dependencies on purpose: this test is about the boundary, not about the
/// package cache, and a fixture that needed crates from the host's cache would
/// fail for a reason that has its own test.
const MAIN: &str = r#"fn main() {
    let v: Vec<i32> = vec![1, 2, 3];
    for i in 0..v.len() {
        println!("{}", v[i]);
    }
}
"#;

const MANIFEST: &str = r#"[package]
name = "sandboxed-builder-fixture"
version = "0.1.0"
edition = "2021"
"#;

#[test]
fn a_sandboxed_lint_reports_from_inside_the_boundary_and_leaves_the_tree_alone() {
    let Some(image) = preconditions() else {
        return;
    };
    let tree = fixture();
    let root = tree.path();

    let before = digest_tree(root);
    let outcome = rto_exec::run_lint(
        "clippy",
        root,
        &FeatureSet::Defaults,
        // The default decision, so this exercises the path a person gets for
        // saying nothing rather than a path only a test can reach.
        decide_lint_host(LintConfigGrant::default(), LintRequested::Unset),
        Some(&image),
    )
    .expect("a sandboxed lint");

    // Condition 3: the boundary is **recorded**, and recorded by the code that
    // obtained it rather than by whoever prints it.
    assert_eq!(outcome.isolation, Isolation::MicroVm);
    assert_eq!(
        outcome.image.as_deref(),
        Some(image.as_str()),
        "the run must name the image it was actually put inside"
    );
    // And the toolchain is the **guest's**, asked for rather than assumed from
    // the reference — which is what makes the count comparable to anything.
    assert!(
        outcome.toolchain.rustc.starts_with("rustc "),
        "the guest's rustc was not read: {:?}",
        outcome.toolchain
    );
    assert!(
        outcome.toolchain.host.contains("linux"),
        "the guest is a Linux microVM, whatever this host is: {:?}",
        outcome.toolchain
    );
    assert!(
        outcome.command.iter().any(|a| a == "--offline"),
        "a guest with no interface must be told so: {:?}",
        outcome.command
    );
    assert!(
        outcome.summary.build_succeeded,
        "the fixture must actually compile, or the assertions below prove nothing"
    );
    assert!(
        outcome
            .report
            .findings
            .iter()
            .any(|f| f.rule.contains("needless_range_loop")),
        "the fixture's diagnostic was not reported: {:?}",
        outcome.report.findings
    );

    // Condition 1, after the fact. The scratch is outside the tree, and the
    // tree is byte-for-byte what it was.
    assert!(
        !outcome.scratch.starts_with(root),
        "the build wrote inside the tree it was reviewing: {}",
        outcome.scratch.display()
    );
    assert!(
        !root.join("target").exists(),
        "a `target/` was left in the tree under review"
    );
    assert_eq!(
        before,
        digest_tree(root),
        "the sandboxed lint modified the tree it was reporting on"
    );

    // And the guest really built something, in the scratch rather than anywhere
    // else: build scripts are what `cargo check` semantics execute, and this is
    // where they landed.
    assert!(
        outcome.scratch.join("debug").is_dir(),
        "nothing was built in {}",
        outcome.scratch.display()
    );
}

/// Every precondition, each printing why it skipped.
fn preconditions() -> Option<String> {
    let Ok(image) = std::env::var(IMAGE_VAR) else {
        eprintln!(
            "SKIPPED: {IMAGE_VAR} is unset, so there is no image to lint inside.\n         \
             Roteiro ships no default and will not choose one — no first-party Rust image \
             carries the `clippy` component (rust-lang/docker-rust builds every stable and \
             nightly variant `--profile minimal`), and picking a third party's would make \
             somebody else's container the boundary. See docs/SANDBOXED_LINTING.md for a \
             two-line image, then:\n           \
             export {IMAGE_VAR}=registry/you/rust-clippy@sha256:<64 hex>\n           \
             roteiro security prefetch --analyzer clippy --allow-download --image $ {IMAGE_VAR}"
        );
        return None;
    };

    match rto_exec::sandbox_probe() {
        rto_exec::SandboxProbe::Available => {}
        rto_exec::SandboxProbe::Unavailable(why) => {
            eprintln!(
                "SKIPPED: no microVM is available on this host, so a sandboxed build cannot \
                 run: {why}\n         (the expected state on a CI runner with no /dev/kvm)"
            );
            return None;
        }
    }

    // Asked before the run, not discovered during it: "the image was never
    // pulled" surfacing as a failed assertion is a missing setup step wearing
    // the costume of a broken backend, which is the failure `image_present`
    // exists to prevent for the reader backend too.
    match rto_exec::boxlite::reference_is_present(IMAGE_VAR, &image, &rto_exec::asset_root()) {
        Ok(true) => {}
        Ok(false) => {
            eprintln!(
                "SKIPPED: {image} is not in the local image store, and a run never pulls.\n         \
                 roteiro security prefetch --analyzer clippy --allow-download --image {image}"
            );
            return None;
        }
        Err(e) => {
            eprintln!("SKIPPED: the local image store could not be read: {e}");
            return None;
        }
    }
    Some(image)
}

/// A worktree that removes itself, so a failed assertion cannot leave a tree
/// behind that the next run then lints.
struct FixtureTree(std::path::PathBuf);

impl FixtureTree {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for FixtureTree {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// A one-crate worktree with a lockfile, since `roteiro lint` passes `--locked`.
///
/// Hand-rolled rather than `tempfile`, matching this crate's other fixtures:
/// ADR-0017 asks that a dependency be justified, and a test directory is not the
/// justification.
fn fixture() -> FixtureTree {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);

    let dir = std::env::temp_dir().join(format!(
        "rto-exec-sandboxed-builder-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("src")).expect("src");
    std::fs::write(dir.join("Cargo.toml"), MANIFEST).expect("manifest");
    std::fs::write(dir.join("src/main.rs"), MAIN).expect("main");
    // Generated rather than written by hand: the lockfile format is cargo's, and
    // a hand-written one would pin this test to whichever version was current
    // the day it was written.
    let generated = Command::new("cargo")
        .args(["generate-lockfile", "--quiet"])
        .current_dir(&dir)
        .output();
    assert!(
        matches!(&generated, Ok(out) if out.status.success()),
        "the fixture needs a lockfile, because `roteiro lint` passes --locked: {generated:?}"
    );
    FixtureTree(dir)
}

/// The digest of every file's **contents** under `root`, recursively.
///
/// Contents rather than names, for the reason in the module documentation.
fn digest_tree(root: &Path) -> String {
    let mut entries: Vec<(String, String)> = Vec::new();
    walk(root, root, &mut entries);
    entries.sort();
    let mut joined = String::new();
    for (path, digest) in &entries {
        use std::fmt::Write as _;
        let _ = writeln!(joined, "{path} {digest}");
    }
    rto_exec::sha256_hex(joined.as_bytes())
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(root, &path, out);
        } else if let Ok(bytes) = std::fs::read(&path) {
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .display()
                .to_string();
            out.push((relative, rto_exec::sha256_hex(&bytes)));
        }
    }
}

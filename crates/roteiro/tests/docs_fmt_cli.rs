//! End-to-end coverage for `roteiro docs fmt` (ADR-0023 step 1), driving the
//! real binary.
//!
//! The unit tests in `rto_spec::fmt` cover the canonical form, and
//! `docs_are_canonical` covers the checked-in tree. Neither exercises the
//! **command**: which files it selects, whether `--write` actually writes, what
//! it exits with, or that a symlink handed to it directly is refused. Raised on
//! #790, where all four were unverified.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_roteiro");

/// A scratch directory holding `docs/` with one drifted document.
fn fixture(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("roteiro-docs-fmt-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("docs")).expect("mkdir");
    std::fs::write(
        root.join("docs/DRIFTED.md"),
        "# Title\n\n|  a |b |\n| --- | --- |\n",
    )
    .expect("write");
    std::fs::write(
        root.join("docs/CLEAN.md"),
        "# Title\n\n| a | b |\n|---|---|\n",
    )
    .expect("write");
    root
}

fn run(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(BIN)
        .args(["docs", "fmt"])
        .args(args)
        .current_dir(dir)
        // An isolated home, as `config_cli.rs` and `debt_density_cli.rs` do:
        // `main` loads user configuration before dispatching, so without this a
        // developer's own `config.toml` decides whether these assertions hold.
        .env("ROTEIRO_HOME", dir)
        .env("HOME", dir)
        .output()
        .expect("run roteiro docs fmt");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

/// Without `--write`: prints a diff, changes nothing, and **fails**.
///
/// The exit code is the whole reason this is usable as a check rather than a
/// suggestion — `cargo fmt --check`'s contract.
#[test]
fn a_check_run_reports_and_refuses_without_writing() {
    let root = fixture("check");
    let before = std::fs::read_to_string(root.join("docs/DRIFTED.md")).expect("read");
    let (ok, out) = run(&root, &[]);
    assert!(!ok, "a drifted tree must fail the check:\n{out}");
    assert!(out.contains("@@"), "no unified diff hunk:\n{out}");
    assert!(out.contains("-| --- | --- |"), "{out}");
    assert!(out.contains("+|---|---|"), "{out}");
    assert_eq!(
        std::fs::read_to_string(root.join("docs/DRIFTED.md")).expect("read"),
        before,
        "a check run wrote to the tree"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// `--write` applies the change and succeeds, and a second run is clean.
#[test]
fn a_write_run_applies_the_change_and_then_has_nothing_to_do() {
    let root = fixture("write");
    let (ok, out) = run(&root, &["--write"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("1 of 2"),
        "the count must name what it did:\n{out}"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("docs/DRIFTED.md")).expect("read"),
        "# Title\n\n| a | b |\n|---|---|\n"
    );
    let (ok, out) = run(&root, &[]);
    assert!(ok, "the tree is canonical now:\n{out}");
    assert!(out.contains("2 file(s) already canonical"), "{out}");
    let _ = std::fs::remove_dir_all(&root);
}

/// With no paths it selects `docs/`; with a path it selects only that.
#[test]
fn the_default_is_docs_and_an_explicit_path_narrows_it() {
    let root = fixture("select");
    std::fs::create_dir_all(root.join("elsewhere")).expect("mkdir");
    std::fs::write(root.join("elsewhere/OUT.md"), "|  x |y |\n| --- | --- |\n").expect("write");

    // The default does not reach outside `docs/`.
    let (_, out) = run(&root, &[]);
    assert!(
        !out.contains("OUT.md"),
        "the default reached outside docs/:\n{out}"
    );

    // A named path does, and only it.
    let (ok, out) = run(&root, &["elsewhere"]);
    assert!(!ok, "{out}");
    assert!(out.contains("OUT.md"), "{out}");
    assert!(
        !out.contains("DRIFTED.md"),
        "an explicit path did not narrow:\n{out}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Overlapping roots name each file once.
///
/// `docs fmt docs docs/adr` reached every ADR twice, so a check run printed
/// each diff twice and counted it twice. Raised on #790.
#[test]
fn overlapping_roots_do_not_double_count() {
    let root = fixture("overlap");
    std::fs::create_dir_all(root.join("docs/adr")).expect("mkdir");
    std::fs::write(root.join("docs/adr/A.md"), "|  a |b |\n| --- | --- |\n").expect("write");

    let (ok, out) = run(&root, &["docs", "docs/adr"]);
    assert!(!ok, "{out}");
    assert_eq!(
        out.matches("--- docs/adr/A.md").count(),
        1,
        "the file was reported twice:\n{out}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A symlinked **parent** is refused too, not just a symlinked file.
///
/// `symlink_metadata("a/b.md")` reports a regular file even when `a` is a
/// symlink, so checking only the last component left the same escape open by a
/// different door. Raised on #790.
#[cfg(unix)]
#[test]
fn a_symlinked_parent_directory_is_not_followed() {
    let root = fixture("symlink-parent");
    let outside = root.join("outside");
    std::fs::create_dir_all(&outside).expect("mkdir");
    let target = outside.join("OUT.md");
    std::fs::write(&target, "|  a |b |\n| --- | --- |\n").expect("write");
    let before = std::fs::read_to_string(&target).expect("read");
    std::os::unix::fs::symlink(&outside, root.join("docs/linkdir")).expect("symlink");

    let (ok, out) = run(&root, &["--write", "docs/linkdir/OUT.md"]);
    assert!(ok, "{out}");
    assert_eq!(
        std::fs::read_to_string(&target).expect("read"),
        before,
        "a symlinked parent let the write escape the named tree"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A symlink handed to the command directly is refused, not followed.
///
/// The directory walk already skips them; this is the path that bypasses the
/// walk, and following it would let `--write` rewrite a file outside the tree
/// the caller named.
#[cfg(unix)]
#[test]
fn a_symlink_named_directly_is_not_followed() {
    let root = fixture("symlink");
    let outside = root.join("OUTSIDE.md");
    std::fs::write(&outside, "|  a |b |\n| --- | --- |\n").expect("write");
    let before = std::fs::read_to_string(&outside).expect("read");
    std::os::unix::fs::symlink(&outside, root.join("docs/LINK.md")).expect("symlink");

    let (ok, out) = run(&root, &["--write", "docs/LINK.md"]);
    assert!(ok, "{out}");
    assert_eq!(
        std::fs::read_to_string(&outside).expect("read"),
        before,
        "writing through a symlink escaped the named tree"
    );
    let _ = std::fs::remove_dir_all(&root);
}

//! Keep the derived per-file runtime pins honest about the archives they came
//! from.
//!
//! # Why this file exists
//!
//! `build.rs` verifies the files `boxlite` extracted against
//! `src/runtime_file_pins.rs`, which is generated from the archives in
//! `src/runtime_pins.rs`. Two tables, one derived from the other, is exactly the
//! shape that goes stale: bump the archive pin, forget to re-run
//! `scripts/derive-runtime-file-pins.py`, and the build script happily verifies
//! the **new** runtime against the **old** digests — reporting a clean check of
//! bytes nobody has looked at.
//!
//! Nothing about that failure is visible at a glance, so it is asserted here
//! instead: every archive has file pins, every set of file pins records the
//! archive digest it was derived from, and — when the archive is on this machine
//! — the digests are re-derived from it and compared.
//!
//! The last of those is the one that actually proves the generator. The others
//! prove the two files are talking about the same release.
//!
//! # Two oracles, and which invariant belongs to which
//!
//! The `boxlite`/`boxlite-shared` pairing is held against
//! **`crates/rto-exec/Cargo.toml`**, not against the lockfile. The break it
//! exists for is `cargo install roteiro --features exec-boxlite`, which
//! resolves the published manifest from scratch and consults no lockfile of
//! ours, so the condition is a property of the declared *requirement*. A guard
//! reading `Cargo.lock` watches the one axis that was never at risk.
//!
//! That manifest also travels with the package, so the requirement guard runs
//! unchanged in a published crate. The lockfile guards cannot: this file ships
//! inside `rto-exec-<version>.crate`, where there is no workspace above it.
//! They say so out loud rather than failing in a tree that is not ours.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// This repository's path, as it appears in a clone's remote URL — the one
/// signal a vendored copy cannot present, because a consumer's remote is their
/// own. Used only to decide whether the checkout assertions may speak.
const REPOSITORY_PATH: &str = "OffeneDatenmodellierung/Roteiro";

/// A skip that can be found in a log, written to **real** stderr.
///
/// `eprintln!` is captured by libtest and discarded for a test that *passes*,
/// so a skip written with it is invisible without `--nocapture`: it reaches
/// nobody on CI and reads exactly like a pass. `std::io::stderr()` writes to
/// the file descriptor, which the capture does not intercept. Same reasoning
/// and same fix as `crates/rto-graph/tests/review_corpus.rs`.
fn loud_skip(test: &str, what_went_unchecked: &str) {
    use std::io::Write;
    let mut err = std::io::stderr().lock();
    let _ = writeln!(
        err,
        "SKIP: {test} — this is a packaged crate, not a checkout of this \
         repository, so there is no workspace lockfile to read. \
         {what_went_unchecked} The requirement itself is still checked here: \
         `the_manifest_pins_both_halves_of_the_generated_api_exactly` reads the \
         manifest that ships in this package and does not skip."
    );
}

/// The workspace root for a crate at `manifest_dir`, two levels above it.
///
/// Takes the directory rather than reading `CARGO_MANIFEST_DIR` itself so the
/// packaged and vendored shapes can be laid out on disk and put through the
/// same arithmetic — see
/// [`the_workspace_marker_tells_a_packaged_crate_from_this_checkout`].
fn workspace_root_of(manifest_dir: &Path) -> PathBuf {
    manifest_dir
        .ancestors()
        .nth(2)
        .expect("a crate directory has at least two ancestors")
        .to_path_buf()
}

/// Where this crate's workspace root would be.
fn workspace_root() -> PathBuf {
    workspace_root_of(Path::new(env!("CARGO_MANIFEST_DIR")))
}

/// The workspace `Cargo.lock` that describes the crate at `manifest_dir`, or
/// `None` when no workspace above it claims that crate as a member.
///
/// # Why this can be absent at all
///
/// `rto-exec` is published and a published crate ships its tests — this file is
/// in `rto-exec-<version>.crate`. Unpacked, `CARGO_MANIFEST_DIR` is
/// `…/registry/src/<index>/rto-exec-<version>`, two levels above which is the
/// registry source directory: sibling crates, no workspace manifest, no
/// workspace lockfile. The crate's own packaged `Cargo.lock` sits at its root
/// and describes a different graph, so reading that instead would assert
/// against the wrong thing rather than decline.
///
/// # Why membership, and not "is this our repository"
///
/// The question that licenses these assertions is **not** "is this Roteiro".
/// It is "does the workspace above this crate resolve this crate's
/// dependencies" — because that, and only that, is what makes its `Cargo.lock`
/// the right file to read. Those two come apart in both directions, and the
/// identity question gets both wrong:
///
/// - A **fork** is a genuine checkout with a genuine workspace lockfile, and
///   its `repository =` is its own. Deciding on the URL would have skipped the
///   lockfile guards in every fork, silently — a guard that reads as coverage
///   and is not, which is the defect this whole file is about.
/// - A crate **vendored under a consumer's workspace** sits in their `vendor/`,
///   which their `members` list does not name. Their lockfile does not resolve
///   it, so these guards must stay quiet — and do.
///
/// Membership is derived from the relationship rather than from an identity
/// string, so it needs no constant that can go stale, and it survives a rename.
/// Where a consumer really has adopted this crate as a first-class member, the
/// guards speak *and are right*: their resolver had to honour this manifest's
/// `=` requirements, so their lockfile carries the same matched pair. A
/// lockfile records optional dependencies whether or not their feature is on,
/// so `boxlite` is in it even with `exec-boxlite` off, as it is in ours.
///
/// # Why the marker is the manifest and not the lockfile
///
/// **In a checkout, never skip.** A missing `Cargo.lock` inside a real checkout
/// is a broken checkout and must fail, so absence of the *lockfile* may never
/// be what authorises the skip. The workspace manifest is the thing a packaged
/// crate genuinely cannot have, so it is what decides — and a checkout missing
/// its lockfile then panics on the read, as it should. This returns a path that
/// may not exist, deliberately.
///
/// Only `NotFound` means absent; every other IO error panics. A marker that
/// read `false` on a permission error would turn "cannot read the repository"
/// into "this is not a repository" and skip in silence, which is the failure
/// these guards exist to prevent wearing the guard's own clothes.
fn workspace_lockfile_for(manifest_dir: &Path) -> Option<PathBuf> {
    let root = workspace_root_of(manifest_dir);
    let manifest = root.join("Cargo.toml");
    let text = match std::fs::read_to_string(&manifest) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => panic!(
            "cannot read {} ({:?}: {e}). Without it these guards cannot tell a packaged \
             crate from a workspace member, and guessing would make them skip in \
             silence — which is the failure they exist to rule out.",
            manifest.display(),
            e.kind(),
        ),
    };
    let relative = manifest_dir
        .strip_prefix(&root)
        .expect("the workspace root is an ancestor of the crate directory")
        .to_string_lossy()
        .replace('\\', "/");
    declares_member(&text, &relative).then(|| root.join("Cargo.lock"))
}

/// The workspace lockfile for the crate these tests were compiled from.
fn workspace_lockfile() -> Option<PathBuf> {
    workspace_lockfile_for(Path::new(env!("CARGO_MANIFEST_DIR")))
}

/// Whether a workspace manifest names `relative` in its `[workspace] members`.
///
/// A scan rather than a TOML parse, for the same reason as everything else
/// here. Only the `members` array inside `[workspace]` counts: `exclude`,
/// `default-members` and any other table's array of paths are not claims that
/// this crate's dependencies are resolved above.
///
/// Both forms our own manifest could take are read — the literal
/// `"crates/rto-exec"` it uses today, and the single-segment glob
/// `"crates/*"` it could be reformatted into. An `exclude` entry is not
/// subtracted: a path cannot be both, and reading one array is what keeps this
/// honest about what it does and does not know.
///
/// **A workspace with no `members` array reads as "not a member".** Cargo
/// infers members from path dependencies when the root is itself a package, and
/// this cannot see that. The direction of that unknown is a skip, which is why
/// [`the_workspace_marker_tells_a_packaged_crate_from_a_member`] asserts that
/// our own live manifest still satisfies this rule — a reformat that this could
/// not read would fail there rather than quietly stop checking.
fn declares_member(manifest: &str, relative: &str) -> bool {
    let mut in_workspace = false;
    let mut in_members = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && !line.starts_with("[[") {
            in_workspace = line == "[workspace]";
            in_members = false;
            continue;
        }
        if !in_workspace {
            continue;
        }
        let body = if let Some(rest) = line.strip_prefix("members") {
            let Some(rest) = rest.trim_start().strip_prefix('=') else {
                continue;
            };
            in_members = !rest.contains(']');
            rest
        } else if in_members {
            if line.starts_with(']') {
                in_members = false;
            }
            line
        } else {
            continue;
        };
        for entry in body.split(',') {
            let Some(entry) = quoted(entry.trim().trim_start_matches('[')) else {
                continue;
            };
            if entry == relative {
                return true;
            }
            if let Some(prefix) = entry.strip_suffix("/*")
                && let Some(tail) = relative.strip_prefix(prefix)
                && let Some(tail) = tail.strip_prefix('/')
                && !tail.is_empty()
                && !tail.contains('/')
            {
                return true;
            }
        }
    }
    false
}

/// Whether the tree above this crate was cloned from this repository.
///
/// **Not the marker** — membership is. This is the independent signal that lets
/// the one assertion which must not be wrong in *our* tree know it is looking
/// at our tree. A fork reports its own path and so declines to assert, which is
/// the right way for a guard to fail; a fork's lockfile guards still *run*,
/// because membership does not care whose repository this is.
fn cloned_from_this_repository() -> bool {
    Command::new("git")
        .arg("-C")
        .arg(workspace_root())
        .args(["remote", "get-url", "origin"])
        .output()
        .is_ok_and(|o| {
            o.status.success() && String::from_utf8_lossy(&o.stdout).contains(REPOSITORY_PATH)
        })
}

/// A scratch directory nothing else can be holding, under the shared
/// `CARGO_TARGET_TMPDIR`.
///
/// That root is shared by every test in this binary **and by every concurrent
/// `cargo test` against the same target directory**, so a fixed leaf name is a
/// race: one run's setup `remove_dir_all` can delete another's fixtures
/// mid-assertion. A name built from the pid and a timestamp does not fix it —
/// that has been tried in this repository and is not unique.
///
/// `create_dir` is, though: it fails with `AlreadyExists` rather than
/// succeeding onto an existing directory, and that check-and-create is atomic
/// in the kernel. Counting up until it succeeds is therefore unique by
/// construction, across threads and across processes, with no clock in it. The
/// shared root is never removed — only the leaf this call owns.
fn exclusive_scratch(stem: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR"));
    std::fs::create_dir_all(root).expect("the target tmp dir should be creatable");
    for attempt in 0u32.. {
        let candidate = root.join(format!("{stem}-{attempt}"));
        match std::fs::create_dir(&candidate) {
            Ok(()) => return candidate,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(e) => panic!("cannot create {}: {e}", candidate.display()),
        }
    }
    unreachable!("u32 is not exhausted by concurrent test runs")
}

/// The exact (`=`) version a manifest requires for `name`.
///
/// `None` covers every way the requirement stops being an exact pin — a caret,
/// a range, or the entry deleted outright — because the caller treats them the
/// same: each one frees a fresh resolution to float one half of a
/// prost-generated API away from the other.
///
/// # Both manifest shapes, because the guard runs in both
///
/// `cargo package` **normalizes** the manifest it ships: the inline
/// `boxlite = { version = "=0.10.2", optional = true }` written in
/// `crates/rto-exec/Cargo.toml` is published as a `[dependencies.boxlite]`
/// table with `version = "=0.10.2"` on a line of its own. A parser that read
/// only one form would be the oracle in a checkout and blind in a package —
/// the same shape of hole this guard exists to close. Both are read, and
/// [`the_requirement_rule_reads_both_manifest_shapes`] holds the rule against
/// a literal of each.
///
/// Only `[dependencies]` counts: a `[dev-dependencies]` entry of the same name
/// pins nothing that ships. Comment lines are skipped, because this manifest's
/// own prose quotes `boxlite-shared = "0.10.0"` while explaining the pin.
fn exact_requirement(manifest: &str, name: &str) -> Option<String> {
    let inline_key = format!("{name} = ");
    let table_header = format!("[dependencies.{name}]");
    let mut section = "";
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            section = line;
            continue;
        }
        let found = if section == table_header.as_str() {
            quoted_after(line, "version = ")
        } else if section == "[dependencies]" {
            line.strip_prefix(inline_key.as_str())
                .and_then(declared_requirement)
        } else {
            None
        };
        if let Some(requirement) = found {
            return exact_version(&requirement);
        }
    }
    None
}

/// The requirement on a dependency line's right-hand side: a bare string
/// (`"=0.10.2"`) or an inline table carrying a `version` key.
fn declared_requirement(rest: &str) -> Option<String> {
    let rest = rest.trim_start();
    if rest.starts_with('{') {
        quoted_after(rest, "version = ")
    } else {
        quoted(rest)
    }
}

/// The first `"…"` literal following `key`.
fn quoted_after(text: &str, key: &str) -> Option<String> {
    quoted(text.split_once(key)?.1)
}

/// The contents of the `"…"` literal `text` begins with.
fn quoted(text: &str) -> Option<String> {
    let rest = text.trim_start().strip_prefix('"')?;
    rest.split_once('"').map(|(value, _)| value.to_owned())
}

/// A requirement that pins exactly one release, as that release.
///
/// `=0.10.2` is the only accepted form. `0.10.2` is a caret in cargo's grammar
/// and floats the patch; `>=0.10.2, <0.11` names a range; `*` names anything.
/// Each of those is `None`, because none of them holds the two halves of the
/// API together.
fn exact_version(requirement: &str) -> Option<String> {
    let version = requirement.strip_prefix('=')?.trim();
    let bare = !version.is_empty()
        && version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'+');
    bare.then(|| version.to_owned())
}

/// **The manifest pins both halves of the generated API, exactly and alike.**
///
/// # Why the lockfile cannot be the oracle for this
///
/// The defect is `cargo install roteiro --features exec-boxlite`, and that path
/// resolves the published manifest from scratch — it consults **no lockfile of
/// ours**. So the condition that broke 6.0.1 is a property of the declared
/// requirement, and a guard whose oracle is the committed lockfile is measuring
/// the one axis that was never at risk.
///
/// That is measured rather than argued. Relaxing `=0.10.2` to `0.10.2` in
/// `crates/rto-exec/Cargo.toml` leaves the committed lockfile byte-identical,
/// leaves `cargo check --locked` green, and left every lockfile-reading guard
/// in this file passing — while an unlocked install floats the sibling and
/// fails to compile exactly as it did on 6.0.1.
///
/// # Why it is also the guard that survives packaging
///
/// `CARGO_MANIFEST_DIR/Cargo.toml` is present in **both** shapes this file runs
/// in, and `cargo package` preserves the `=` requirement into it. So this
/// assertion never skips, and the invariant is held here rather than in the
/// lockfile guards below, which can only run in a checkout.
#[test]
fn the_manifest_pins_both_halves_of_the_generated_api_exactly() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let manifest = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));

    let parent = exact_requirement(&manifest, "boxlite");
    let sibling = exact_requirement(&manifest, "boxlite-shared");

    assert!(
        parent.is_some(),
        "{} no longer requires `boxlite` with an exact `=` version. A caret there lets a \
         fresh resolution pick a release these runtime pins do not describe, and no \
         lockfile of ours is consulted by `cargo install`.",
        path.display()
    );
    assert!(
        sibling.is_some(),
        "{} no longer requires `boxlite-shared` with an exact `=` version — the entry is \
         either gone, deleted as an unused dependency, or relaxed to a range. It is a pin, \
         not a use, and it is load-bearing: `boxlite` requires its sibling with a caret, so \
         without this an unlocked resolution floats one half of a prost-generated API away \
         from the other. That is how `cargo install roteiro --features exec-boxlite` broke \
         on 6.0.1. See the manifest comment and ADR-0014 v1.9.",
        path.display()
    );
    assert_eq!(
        parent,
        sibling,
        "{} pins boxlite {parent:?} and boxlite-shared {sibling:?}. They are two halves of \
         one prost-generated API released in lockstep, and upstream ships required fields \
         on public structs in patch releases, so any skew between them is a compile error \
         waiting for the next one. Keep the two versions equal.",
        path.display()
    );
    assert_eq!(
        parent.as_deref(),
        Some(rto_exec::RUNTIME_VERSION),
        "{} pins boxlite {parent:?}, but the sandbox-runtime pins in \
         crates/rto-exec/src/runtime_pins.rs are for {}. Bumping one without the other \
         pairs a library with a shim and guest from a different release, which the digests \
         cannot tell apart because they are provisioned from the same file they are checked \
         against. Re-run scripts/derive-runtime-file-pins.py.",
        path.display(),
        rto_exec::RUNTIME_VERSION
    );
}

/// **The requirement rule, against a literal of each manifest shape.**
///
/// The live assertion above cannot catch a rule that reads only one shape: in a
/// checkout it would pass on the inline form and never reveal that it is blind
/// to the published one, and the failure would land only on somebody who
/// packages us. These literals are the two shapes, written out, plus the forms
/// that must read as "not a pin".
#[test]
fn the_requirement_rule_reads_both_manifest_shapes() {
    // What is written in crates/rto-exec/Cargo.toml — comment prose included,
    // because that prose quotes a caret requirement while explaining the pin.
    let checkout = "[dependencies]\n\
                    # 0.10.0 asks for `boxlite-shared = \"0.10.0\"`, and 0.10.2 for \"0.10.2\".\n\
                    boxlite = { version = \"=0.10.2\", optional = true }\n\
                    boxlite-shared = { version = \"=0.10.2\", optional = true }\n";
    assert_eq!(
        exact_requirement(checkout, "boxlite").as_deref(),
        Some("0.10.2"),
        "the inline form is what this repository writes"
    );
    assert_eq!(
        exact_requirement(checkout, "boxlite-shared").as_deref(),
        Some("0.10.2"),
        "a hyphenated name must not be confused with its prefix, in either direction"
    );

    // What `cargo package` ships: cargo normalizes every dependency into its own
    // table. Verified against an actual `cargo package` output, not assumed.
    let packaged = "[dependencies.boxlite]\n\
                    version = \"=0.10.2\"\n\
                    optional = true\n\
                    \n\
                    [dependencies.boxlite-shared]\n\
                    version = \"=0.10.2\"\n\
                    optional = true\n";
    assert_eq!(
        exact_requirement(packaged, "boxlite").as_deref(),
        Some("0.10.2"),
        "the published manifest's table form must read too, or this guard is blind in \
         exactly the tree it was rewritten to serve"
    );
    assert_eq!(
        exact_requirement(packaged, "boxlite-shared").as_deref(),
        Some("0.10.2")
    );

    // The injection this guard exists to fail, in both shapes.
    assert_eq!(
        exact_requirement(
            "[dependencies]\nboxlite = { version = \"0.10.2\" }\n",
            "boxlite"
        ),
        None,
        "a bare requirement is a caret in cargo's grammar and floats the patch"
    );
    assert_eq!(
        exact_requirement("[dependencies.boxlite]\nversion = \"0.10.2\"\n", "boxlite"),
        None,
        "and the same relaxation in the published form must read the same way"
    );
    assert_eq!(
        exact_requirement(
            "[dependencies]\nboxlite = { version = \">=0.10.2, <0.11\" }\n",
            "boxlite"
        ),
        None,
        "a range is not a pin either"
    );

    // Deleted outright, and declared somewhere that pins nothing which ships.
    assert_eq!(
        exact_requirement("[dependencies]\nserde = \"1\"\n", "boxlite"),
        None,
        "a missing entry must read as absent rather than as satisfied"
    );
    assert_eq!(
        exact_requirement(
            "[dev-dependencies]\nboxlite = { version = \"=0.10.2\" }\n",
            "boxlite"
        ),
        None,
        "a dev-dependency pins nothing an installer resolves"
    );
    assert_eq!(
        exact_requirement(
            "[dependencies]\nboxlite-shared = { version = \"=0.10.2\" }\n",
            "boxlite"
        ),
        None,
        "asking for `boxlite` must not be answered by `boxlite-shared`'s line"
    );
}

/// **The marker answers "is this crate a member above", in every shape.**
///
/// A skip that never skips and a skip that always skips both look green from
/// here, so each shape is laid out on disk and put through the same ancestor
/// arithmetic the guards use. The two that must speak and the two that must
/// stay quiet are asserted together, because the interesting property is that
/// the rule *separates* them.
///
/// The fork is the case the previous rule got wrong: it decided on this
/// repository's `repository =` URL, so a fork — a genuine checkout, with a
/// genuine workspace lockfile — would have skipped the lockfile guards in
/// silence. Membership does not care whose repository this is.
#[test]
fn the_workspace_marker_tells_a_packaged_crate_from_a_member() {
    let scratch = exclusive_scratch("pin-guard-shapes");

    // Quiet: the published shape. …/registry/src/<index>/rto-exec-<version>,
    // whose grandparent is the registry source directory and has no manifest.
    let packaged = scratch
        .join("registry")
        .join("src")
        .join("index.crates.io-1949cf8c6b5b557f")
        .join(format!("rto-exec-{}", env!("CARGO_PKG_VERSION")));
    std::fs::create_dir_all(&packaged).expect("create the packaged layout");
    std::fs::write(
        packaged.join("Cargo.toml"),
        "[package]\nname = \"rto-exec\"\n",
    )
    .expect("write the packaged manifest");
    std::fs::write(
        packaged.join("Cargo.lock"),
        "# the package's own, not ours\n",
    )
    .expect("write the packaged lockfile");
    assert_eq!(
        workspace_lockfile_for(&packaged),
        None,
        "an unpacked crate has no workspace above it, so the lockfile guards must decline \
         rather than read a path that is not there — and must not fall back to the \
         package's own lockfile, which describes a different graph"
    );

    // Quiet: vendored under a consumer. `cargo vendor` writes to `vendor/`,
    // which their `members` list does not name, so their lockfile does not
    // resolve this crate and these guards have nothing to say about it. Their
    // manifest carries our URL, because they depend on us by git — which is
    // exactly why the URL could never have been the question.
    let consumer = scratch.join("consumer");
    let vendored = consumer.join("vendor").join("rto-exec");
    std::fs::create_dir_all(&vendored).expect("create the vendored layout");
    std::fs::write(
        consumer.join("Cargo.toml"),
        format!(
            "[workspace]\nmembers = [\"app\", \"crates/their-lib\"]\n\n\
             [workspace.package]\nrepository = \"https://github.com/someone/their-app\"\n\n\
             [dependencies]\nroteiro = {{ git = \"https://github.com/{REPOSITORY_PATH}\" }}\n"
        ),
    )
    .expect("write the consumer manifest");
    std::fs::write(consumer.join("Cargo.lock"), "# theirs, no boxlite in it\n")
        .expect("write their lockfile");
    assert_eq!(
        workspace_lockfile_for(&vendored),
        None,
        "a vendored copy is not a member of the workspace it sits under, so its lockfile \
         does not resolve this crate. Reading it would find no boxlite and fail their \
         `cargo test` over a repository that is not theirs"
    );

    // Speaks: a fork. Different remote, different `repository =`, possibly a
    // different name — and a real workspace that really does resolve this
    // crate. The URL rule skipped every one of these.
    let fork_root = scratch.join("their-fork");
    let fork_crate = fork_root.join("crates").join("rto-exec");
    std::fs::create_dir_all(&fork_crate).expect("create the fork layout");
    std::fs::write(
        fork_root.join("Cargo.toml"),
        "[workspace]\nmembers = [\n    \"crates/rto-exec\",\n    \"crates/roteiro\",\n]\n\n\
         [workspace.package]\nrepository = \"https://gitlab.example/someone/their-fork\"\n",
    )
    .expect("write the fork manifest");
    assert_eq!(
        workspace_lockfile_for(&fork_crate),
        Some(fork_root.join("Cargo.lock")),
        "a fork is a genuine checkout with a genuine workspace lockfile. Deciding on this \
         repository's URL skipped the lockfile guards in every fork, silently — a guard \
         that reads as coverage and is not, which is the defect this file exists about"
    );

    // Speaks, and rightly: a consumer who has adopted this crate as a
    // first-class member. Their resolver had to honour this manifest's `=`
    // requirements, so their lockfile carries the same matched pair, and the
    // guards are answering a question their tree really does have an answer to.
    let adopter_root = scratch.join("adopter");
    let adopted = adopter_root.join("crates").join("rto-exec");
    std::fs::create_dir_all(&adopted).expect("create the adopter layout");
    std::fs::write(
        adopter_root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/*\"]\n",
    )
    .expect("write the adopter manifest");
    assert_eq!(
        workspace_lockfile_for(&adopted),
        Some(adopter_root.join("Cargo.lock")),
        "a single-segment glob is the other form our own members list could be written in, \
         and a workspace that really resolves this crate is one these guards can speak to"
    );

    // Quiet: `members` naming a sibling, not us. The glob arm must not widen
    // into "some member exists".
    let neighbour = scratch.join("neighbour");
    let not_a_member = neighbour.join("vendor").join("rto-exec");
    std::fs::create_dir_all(&not_a_member).expect("create the neighbour layout");
    std::fs::write(
        neighbour.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/*\", \"tools/cli\"]\n",
    )
    .expect("write the neighbour manifest");
    assert_eq!(
        workspace_lockfile_for(&not_a_member),
        None,
        "`crates/*` does not match `vendor/rto-exec`; a glob that matched anything would \
         put these guards back on every consumer's CI"
    );

    // The live tree. Two signals independent of the marker agree this is our
    // own checkout — the remote says so, and cargo compiled this file from a
    // `crates/rto-exec` inside it — so here the marker may not read "packaged".
    // In a package, and in a fork, this half is vacuous by construction, which
    // is correct and is said rather than hidden.
    if cloned_from_this_repository() && Path::new(env!("CARGO_MANIFEST_DIR")).ends_with("rto-exec")
    {
        let live = workspace_lockfile()
            .expect("a checkout of this repository is never the packaged shape");
        assert!(
            live.is_file(),
            "{} is missing. In a checkout that is a broken checkout, not a reason to skip — \
             the marker is the workspace manifest precisely so that this fails here",
            live.display()
        );
        // And the live manifest still satisfies the membership rule as written.
        // A reformat of `members` into a shape this scan cannot read would
        // otherwise turn every run into a silent skip; here it fails instead.
        let root = workspace_root();
        let text = std::fs::read_to_string(root.join("Cargo.toml"))
            .expect("a checkout of this repository has a workspace manifest");
        assert!(
            declares_member(&text, "crates/rto-exec"),
            "{}'s members list no longer names crates/rto-exec in a form this rule reads. \
             That is this rule's problem, not this assertion's: left alone it would skip \
             the lockfile guards on every run and say nothing",
            root.display()
        );
    }

    std::fs::remove_dir_all(&scratch).expect("the scratch leaf is ours to remove");
}

/// **A fork's lockfile guards run, and pass.**
///
/// The marker test above proves the fork shape is *selected*. This one proves
/// the guards then do their job there, by putting a fork-shaped tree — a
/// different `repository =`, a different remote — through the **same assertion
/// bodies** the real guards call. Asserting only that nothing failed would not
/// distinguish "ran and passed" from "never ran".
#[test]
fn the_lockfile_guards_run_in_a_fork() {
    let scratch = exclusive_scratch("pin-guard-fork");
    let fork_root = scratch.join("their-fork");
    let fork_crate = fork_root.join("crates").join("rto-exec");
    std::fs::create_dir_all(&fork_crate).expect("create the fork layout");
    std::fs::write(
        fork_root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/rto-exec\"]\n\n[workspace.package]\n\
         repository = \"https://codeberg.org/someone/roteiro-fork\"\n",
    )
    .expect("write the fork manifest");

    // A fork's lockfile is this repository's lockfile with their history on top,
    // so the real one is what a fork would actually have. Copied rather than
    // synthesised: a hand-written stub would prove the assertions run against a
    // fixture, not that they run against a lockfile.
    let ours = workspace_root().join("Cargo.lock");
    if !ours.is_file() {
        loud_skip(
            "the_lockfile_guards_run_in_a_fork",
            "There is no lockfile here to build a fork's tree from, so whether the guards \
             run in a fork went unchecked.",
        );
        std::fs::remove_dir_all(&scratch).expect("the scratch leaf is ours to remove");
        return;
    }
    let lockfile = fork_root.join("Cargo.lock");
    std::fs::copy(&ours, &lockfile).expect("copy the lockfile into the fork");

    let selected = workspace_lockfile_for(&fork_crate);
    assert_eq!(
        selected.as_deref(),
        Some(lockfile.as_path()),
        "the fork's own workspace lockfile is what these guards must read"
    );

    // The real bodies, not a re-implementation. Both must reach their
    // assertions and pass; a skew planted in the fork's lockfile fails them,
    // which is what `the_fork_guards_are_not_vacuous` below holds.
    assert_locked_boxlite_matches_the_pins(&lockfile);
    assert_locked_pair_agrees(&lockfile);

    std::fs::remove_dir_all(&scratch).expect("the scratch leaf is ours to remove");
}

/// **And they would have failed there.** Running and passing is only evidence
/// if the same code fails on a tree that deserves it, so the fork's lockfile is
/// given the skew the guards exist to catch.
#[test]
fn the_fork_guards_are_not_vacuous() {
    let scratch = exclusive_scratch("pin-guard-fork-skew");
    let lockfile = scratch.join("Cargo.lock");
    std::fs::write(
        &lockfile,
        "[[package]]\nname = \"boxlite\"\nversion = \"0.10.2\"\n\n\
         [[package]]\nname = \"boxlite-shared\"\nversion = \"0.10.1\"\n",
    )
    .expect("write a skewed lockfile");

    let skewed = std::panic::catch_unwind(|| assert_locked_pair_agrees(&lockfile));
    assert!(
        skewed.is_err(),
        "a lockfile resolving boxlite 0.10.2 against boxlite-shared 0.10.1 is the exact \
         pair that failed to compile on 6.0.1, so the assertion a fork runs must reject it"
    );

    std::fs::write(
        &lockfile,
        "[[package]]\nname = \"serde\"\nversion = \"1.0.0\"\n",
    )
    .expect("write a lockfile with no boxlite");
    let absent = std::panic::catch_unwind(|| assert_locked_pair_agrees(&lockfile));
    assert!(
        absent.is_err(),
        "finding no boxlite at all must fail loudly rather than pass as `no skew found`"
    );

    std::fs::remove_dir_all(&scratch).expect("the scratch leaf is ours to remove");
}

/// Every pinned archive has extracted-file pins, and nothing has pins without an
/// archive.
///
/// A target in one table and not the other is not a half-finished bump that
/// still mostly works: `build.rs` refuses to build a target it has no file pins
/// for, so the first symptom would be a platform that cannot build at all.
#[test]
fn every_pinned_archive_has_extracted_file_pins() {
    let archives: BTreeSet<&str> = rto_exec::RUNTIME_ARCHIVES
        .iter()
        .map(|a| a.target)
        .collect();
    let derived: BTreeSet<&str> = rto_exec::RUNTIME_FILES.iter().map(|f| f.target).collect();

    assert_eq!(
        archives, derived,
        "runtime_pins.rs and runtime_file_pins.rs disagree about which platforms are \
         pinned. Re-derive with scripts/derive-runtime-file-pins.py rather than editing \
         either table by hand."
    );
    assert!(
        !archives.is_empty(),
        "no platform is pinned at all, so both tables are vacuous"
    );
}

/// Each set of file pins names the archive digest it was derived from, and that
/// digest is the one currently pinned.
///
/// This is the check that catches a bumped archive with stale file pins — the
/// failure mode that would otherwise verify new bytes against old digests and
/// call it clean.
#[test]
fn the_file_pins_were_derived_from_the_archives_that_are_pinned_now() {
    for archive in rto_exec::RUNTIME_ARCHIVES {
        let pins = rto_exec::runtime_files_for(archive.target)
            .unwrap_or_else(|| panic!("no file pins for {}", archive.target));
        assert_eq!(
            pins.archive_sha256, archive.sha256,
            "the file pins for {} were derived from archive sha256 {}, but the archive now \
             pinned is {}. Re-run scripts/derive-runtime-file-pins.py; do not edit the \
             recorded digest to match, which would keep the stale file digests.",
            archive.target, pins.archive_sha256, archive.sha256
        );
    }

    assert_eq!(
        rto_exec::RUNTIME_FILES_VERSION,
        rto_exec::RUNTIME_VERSION,
        "the generated file pins are for a different boxlite release than runtime_pins.rs"
    );
}

/// The pins describe the `boxlite` release the lockfile actually resolves.
///
/// # Why the digests do not already cover this
///
/// They cover it on one path and not the other, and the one they miss is the
/// one CI takes. With `BOXLITE_RUNTIME_URL` unset, `boxlite` fetches the archive
/// for *its own* version and `build.rs` compares the extracted files against
/// these pins, so a skew is a digest mismatch. But the strict path provisions
/// the archive **from this file** and points `boxlite`'s `curl` at it — so a
/// bump that moved `boxlite` and forgot the pins would hand the old runtime to
/// the new library and check it against the digests it was provisioned from.
/// Every one would match. The build would be green, the archive genuine, and
/// the pairing wrong: a v0.10.0 library driving a v0.9.7 shim and guest.
///
/// `runtime_pins.rs` claimed this was checked against `boxlite`'s
/// `CARGO_PKG_VERSION` in `build.rs`. Nothing did — and nothing there can, since
/// cargo passes a `links` dependency's metadata keys and not its version. This
/// is the check the comment was describing.
///
/// # Why the lockfile, when the requirement guard reads the manifest
///
/// It is what `--locked` builds resolve, and it is what a dependency bump
/// *edits*. So this is the half that catches a manifest moved on with a stale
/// lockfile left behind — the one thing the requirement is silent about.
/// [`the_manifest_pins_both_halves_of_the_generated_api_exactly`] holds the
/// invariant itself, which is why this one may decline in a package.
#[test]
fn the_pins_are_for_the_boxlite_release_the_lockfile_resolves() {
    let Some(lockfile) = workspace_lockfile() else {
        loud_skip(
            "the_pins_are_for_the_boxlite_release_the_lockfile_resolves",
            "Whether the committed lockfile has been refreshed to the release the \
             sandbox-runtime pins describe went unchecked.",
        );
        return;
    };
    assert_locked_boxlite_matches_the_pins(&lockfile);
}

/// The body of the guard above, over whichever lockfile it was handed.
///
/// Separated so that [`the_lockfile_guards_run_in_a_fork`] can put a fork's
/// lockfile through the **same code**, rather than through a re-implementation
/// that could agree with a broken original.
fn assert_locked_boxlite_matches_the_pins(lockfile: &Path) {
    let source = std::fs::read_to_string(lockfile)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", lockfile.display()));

    let versions = locked_versions(&source, "boxlite");

    assert_eq!(
        versions.len(),
        1,
        "expected exactly one `boxlite` package in {}, found {versions:?}. Zero means this \
         test is looking at nothing and would pass however far the pins drifted; more than \
         one means the graph carries two boxlite releases and the pins can only describe \
         one of them.",
        lockfile.display()
    );

    assert_eq!(
        versions[0],
        rto_exec::RUNTIME_VERSION,
        "the lockfile resolves boxlite {} but the sandbox-runtime pins are for {}. Bump \
         crates/rto-exec/src/runtime_pins.rs to the matching release and re-run \
         scripts/derive-runtime-file-pins.py — never the other way round, and never by \
         editing RUNTIME_VERSION alone, which would leave a new library paired with an old \
         shim and guest that the digests cannot tell apart.",
        versions[0],
        rto_exec::RUNTIME_VERSION
    );
}

/// `boxlite` and `boxlite-shared` resolve to the **same** release.
///
/// # The defect this exists for
///
/// An exact pin binds one node and says nothing about that node's own
/// requirements. `rto-exec` pins `boxlite` exactly, but every `boxlite` release
/// to date requires its sibling with a **caret** — 0.10.0 asks for
/// `boxlite-shared = "0.10.0"`, 0.10.2 for `"0.10.2"` — so a fresh resolution is
/// free to float the sibling while the exact pin holds the parent still.
///
/// That is not hypothetical. It is how `cargo install roteiro --features
/// exec-boxlite` broke on 6.0.1: `boxlite-shared` 0.10.1 added a required sixth
/// field, `source_is_dir`, to the `UploadChunk` protobuf message, `boxlite`
/// 0.10.0's struct literal still listed five, and the mismatched pair failed to
/// compile with `error[E0063]`. The two crates are halves of one
/// prost-generated API released in lockstep; a version skew between them is a
/// compile error waiting for the next upstream patch release.
///
/// # Why a test and not just the manifest comment
///
/// `crates/rto-exec/Cargo.toml` pins both and explains why, but a comment is
/// not a gate: the `boxlite-shared` entry is a pin with no `use`, so the
/// standing temptation is to delete it as an unused dependency. ADR-0014 v1.9
/// records the duty.
///
/// # Why this is the second half and not the gate
///
/// The deletion above, and every relaxation of the requirement, is caught by
/// [`the_manifest_pins_both_halves_of_the_generated_api_exactly`], which reads
/// the manifest and runs in a package too. This test is what remains once that
/// one exists: proof that the committed lockfile has actually been resolved to
/// a matched pair, rather than left behind by a manifest that moved. That is a
/// checkout-only question, so in a package it declines out loud.
#[test]
fn boxlite_and_its_generated_api_sibling_resolve_together() {
    let Some(lockfile) = workspace_lockfile() else {
        loud_skip(
            "boxlite_and_its_generated_api_sibling_resolve_together",
            "Whether the committed lockfile resolves both halves to one version went \
             unchecked.",
        );
        return;
    };
    assert_locked_pair_agrees(&lockfile);
}

/// The body of the guard above, over whichever lockfile it was handed. Shared
/// with [`the_lockfile_guards_run_in_a_fork`] for the same reason as
/// [`assert_locked_boxlite_matches_the_pins`].
fn assert_locked_pair_agrees(lockfile: &Path) {
    let source = std::fs::read_to_string(lockfile)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", lockfile.display()));

    let parent = locked_versions(&source, "boxlite");
    let sibling = locked_versions(&source, "boxlite-shared");

    // Finding nothing must fail loudly rather than pass as "no skew found" —
    // the same reason the test above counts before it compares.
    assert_eq!(
        parent.len(),
        1,
        "expected exactly one `boxlite` package in {}, found {parent:?}",
        lockfile.display()
    );
    assert_eq!(
        sibling.len(),
        1,
        "expected exactly one `boxlite-shared` package in {}, found {sibling:?}. Zero means \
         the pin in crates/rto-exec/Cargo.toml was deleted as an unused dependency — \
         put it back; it is load-bearing and the manifest comment says why.",
        lockfile.display()
    );

    assert_eq!(
        parent[0], sibling[0],
        "the lockfile resolves boxlite {} against boxlite-shared {}. These two share one \
         prost-generated API and are released in lockstep, so a skew between them is \
         a compile error in boxlite's own source. Pin both to the same version in \
         crates/rto-exec/Cargo.toml — bumping only `boxlite` is what let the sibling \
         float and broke `cargo install` on 6.0.1.",
        parent[0], sibling[0]
    );
}

/// Every `[[package]]` block with this name, by its version line.
///
/// A scan rather than a TOML parse, to keep these tests free of a dependency for
/// one field. Returns all matches so callers can assert the count themselves:
/// collapsing to an `Option` here would turn "two releases in the graph" into a
/// silent pick of the first.
fn locked_versions<'a>(source: &'a str, name: &str) -> Vec<&'a str> {
    source
        .split("[[package]]")
        .filter_map(|block| {
            let mut found = None;
            let mut version = None;
            for line in block.lines() {
                if let Some(rest) = line.strip_prefix("name = \"") {
                    found = rest.strip_suffix('"');
                } else if let Some(rest) = line.strip_prefix("version = \"") {
                    version = rest.strip_suffix('"');
                }
            }
            (found == Some(name)).then_some(version).flatten()
        })
        .collect()
}

/// The pins are well formed: unique flat names, real digests, real sizes.
///
/// `build.rs` matches extracted files by name and refuses anything unpinned, so
/// a duplicate or a name with a path separator in it would not be a cosmetic
/// problem — it would be a rule that cannot be applied to a flat directory.
#[test]
fn every_pinned_file_is_well_formed() {
    for pins in rto_exec::RUNTIME_FILES {
        let mut names = BTreeSet::new();
        assert!(
            !pins.files.is_empty(),
            "{} has no pinned files, so verification of it would pass vacuously",
            pins.target
        );
        for file in pins.files {
            assert!(
                names.insert(file.name),
                "{} pins {:?} twice",
                pins.target,
                file.name
            );
            assert!(!file.name.is_empty(), "{} pins an empty name", pins.target);
            assert!(
                !file.name.contains('/') && !file.name.contains('\\'),
                "{} pins {:?}, but the runtime directory is flat",
                pins.target,
                file.name
            );
            assert!(
                file.name != ".boxlite-runtime-files",
                "{} pins boxlite's own generated manifest, which it does not embed",
                pins.target
            );
            assert_eq!(
                file.sha256.len(),
                64,
                "{} pins {:?} with a digest that is not a SHA-256",
                pins.target,
                file.name
            );
            assert!(
                file.sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "{} pins {:?} with a digest that is not lowercase hex",
                pins.target,
                file.name
            );
            assert!(
                file.bytes > 0,
                "{} pins {:?} at zero bytes",
                pins.target,
                file.name
            );
        }
    }
}

/// Re-derive this host's pins from the archive itself and compare.
///
/// The three tests above prove the two tables agree with each other. Only this
/// one proves either of them agrees with **the archive**, which is the thing
/// they are supposed to describe — so it is the one that would catch a generator
/// that mis-strips a path component, skips a member, or hashes the wrong bytes.
///
/// It needs the archive on disk and does not fetch: provisioning is a separate
/// act with its own consent, and a test that downloads 26 MB is a test people
/// turn off. When it is not there the skip is printed, in the house style — a
/// silent skip is indistinguishable from a pass.
#[test]
fn the_pins_match_the_archive_they_were_derived_from() {
    let Some(archive) = host_archive() else {
        eprintln!("SKIPPED: no sandbox runtime is pinned for this host");
        return;
    };
    let Some(path) = local_archive_copy() else {
        eprintln!(
            "SKIPPED: the pinned archive is not on this machine. Provision it with \
             `roteiro security prefetch --analyzer sandbox --allow-download`, or run \
             `scripts/derive-runtime-file-pins.py` which caches it, and this checks the \
             pins against the real bytes."
        );
        return;
    };

    // The archive is checked against its own pin before it is opened — the same
    // order the generator uses, and for the same reason.
    let body = std::fs::read(&path).expect("the archive should be readable");
    assert_eq!(
        rto_exec::sha256_hex(&body),
        archive.sha256,
        "the archive at {} is not the pinned one, so it cannot say anything about the pins",
        path.display()
    );

    // Unique by construction, for the same reason as every other fixture here:
    // this root is shared with every concurrent `cargo test` against the same
    // target directory, and a fixed name lets one run's cleanup delete another
    // run's extraction mid-comparison.
    let extracted = exclusive_scratch("runtime-pin-integrity");

    // Extracted the way boxlite extracts it, `--strip-components=1` included, so
    // the names compared here are the names that land in its runtime directory.
    let status = Command::new("tar")
        .args(["-xzf"])
        .arg(&path)
        .arg("-C")
        .arg(&extracted)
        .arg("--strip-components=1")
        .status()
        .expect("tar should be runnable");
    assert!(status.success(), "tar failed to extract {}", path.display());

    let pins = rto_exec::runtime_files_for(archive.target).expect("this host's target is pinned");
    let mut found: Vec<(String, String, u64)> = Vec::new();
    for entry in std::fs::read_dir(&extracted).expect("the extraction should be listable") {
        let entry = entry.expect("a directory entry should be readable");
        let meta = std::fs::symlink_metadata(entry.path()).expect("stat");
        if !meta.is_file() {
            continue;
        }
        let bytes = std::fs::read(entry.path()).expect("an extracted file should be readable");
        found.push((
            entry.file_name().to_string_lossy().into_owned(),
            rto_exec::sha256_hex(&bytes),
            bytes.len() as u64,
        ));
    }
    found.sort();

    let expected: Vec<(String, String, u64)> = pins
        .files
        .iter()
        .map(|f| (f.name.to_owned(), f.sha256.to_owned(), f.bytes))
        .collect();

    assert_eq!(
        found, expected,
        "the pins for {} do not describe the archive they claim to come from. Re-run \
         scripts/derive-runtime-file-pins.py.",
        archive.target
    );

    std::fs::remove_dir_all(&extracted).expect("the scratch leaf is ours to remove");
}

/// The pinned archive for the machine running the test, if there is one.
fn host_archive() -> Option<&'static rto_exec::PinnedArchive> {
    rto_exec::archive_for(std::env::consts::OS, std::env::consts::ARCH)
}

/// A local copy of this host's pinned archive: the asset cache first, then the
/// generator's own download cache.
fn local_archive_copy() -> Option<PathBuf> {
    let provisioned = rto_exec::asset_root()
        .join(rto_exec::RUNTIME_ASSET)
        .join(rto_exec::RUNTIME_FILE);
    if provisioned.is_file() {
        return Some(provisioned);
    }
    let target = host_archive()?.target;
    let cached = dirs_home()?
        .join(".cache")
        .join("roteiro")
        .join("runtime-archives")
        .join(format!("boxlite-runtime-{target}.tar.gz"));
    cached.is_file().then_some(cached)
}

/// The home directory, without taking a dependency for one variable.
fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

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

/// The workspace manifest's **own** `repository =` line, matched whole.
///
/// Searched-for rather than matched whole is the defect #831 fixed in the
/// corpus guards, and it is the reason `[workspace]` alone is not the marker: a
/// consumer that depends on Roteiro by git URL carries that URL in their
/// manifest too, and a crate vendored two levels under their root would then be
/// called *our* checkout. The guards below would read *their* `Cargo.lock`,
/// find no `boxlite` in it, and fail their `cargo test` over a repository that
/// is not theirs.
const REPOSITORY_FIELD: &str =
    "repository = \"https://github.com/OffeneDatenmodellierung/Roteiro\"";

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

/// The workspace `Cargo.lock` under `root`, or `None` when `root` is not a
/// checkout of **this** repository.
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
fn workspace_lockfile_at(root: &Path) -> Option<PathBuf> {
    let manifest = root.join("Cargo.toml");
    let ours = match std::fs::read_to_string(&manifest) {
        Ok(text) => manifest_is_ours(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => panic!(
            "cannot read {} ({:?}: {e}). Without it these guards cannot tell a packaged \
             crate from a repository checkout, and guessing would make them skip in \
             silence — which is the failure they exist to rule out.",
            manifest.display(),
            e.kind(),
        ),
    };
    ours.then(|| root.join("Cargo.lock"))
}

/// The workspace lockfile for the crate these tests were compiled from.
fn workspace_lockfile() -> Option<PathBuf> {
    workspace_lockfile_at(&workspace_root())
}

/// Whether this manifest is **this repository's** workspace manifest.
///
/// Both conditions are whole-line matches, and the second is the one that does
/// the work — see [`REPOSITORY_FIELD`].
fn manifest_is_ours(text: &str) -> bool {
    text.lines().any(|line| line.trim() == "[workspace]")
        && text.lines().any(|line| line.trim() == REPOSITORY_FIELD)
}

/// Whether the tree at `workspace_root()` was cloned from this repository.
///
/// Independent of the marker being held: not the manifest (a vendored crate
/// sits under a consumer's) and not the layout (this file ships in the
/// package). A fork reports its own path and so declines to assert, which is
/// the right way for a guard to fail.
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

/// **The lockfile skip fires in a package and cannot fire in a checkout.**
///
/// A skip that never skips and a skip that always skips both look green from
/// here, so both directions are laid out on disk and put through the same
/// ancestor arithmetic the guards use. The vendored shape is the one #831
/// settled and is why `[workspace]` alone is not the marker: a consumer's root
/// says `[workspace]` too, and calling it ours would run these guards against
/// their lockfile and fail their `cargo test`.
#[test]
fn the_workspace_marker_tells_a_packaged_crate_from_this_checkout() {
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join("pin-guard-shapes");
    let _ = std::fs::remove_dir_all(&scratch);

    // The published shape: …/registry/src/<index>/rto-exec-<version>, whose
    // grandparent is the registry source directory and has no manifest at all.
    let packaged = scratch
        .join("registry")
        .join("src")
        .join("index.crates.io-1949cf8c6b5b557f")
        .join(format!("rto-exec-{}", env!("CARGO_PKG_VERSION")));
    std::fs::create_dir_all(&packaged).expect("the target tmp dir should be writable");
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
        workspace_lockfile_at(&workspace_root_of(&packaged)),
        None,
        "an unpacked crate has no workspace above it, so the lockfile guards must decline \
         rather than read a path that is not there — and must not fall back to the \
         package's own lockfile, which describes a different graph"
    );

    // The vendored shape: our crate two levels under somebody else's workspace,
    // whose manifest depends on Roteiro by git URL. This is #831's case.
    let consumer = scratch.join("consumer");
    let vendored = consumer.join("vendor").join("rto-exec");
    std::fs::create_dir_all(&vendored).expect("create the vendored layout");
    std::fs::write(
        consumer.join("Cargo.toml"),
        format!(
            "[workspace]\nmembers = [\"app\"]\n\n[workspace.package]\n\
             repository = \"https://github.com/someone/their-app\"\n\n\
             [dependencies]\nroteiro = {{ git = \"{}\" }}\n",
            REPOSITORY_FIELD
                .trim_start_matches("repository = ")
                .trim_matches('"')
        ),
    )
    .expect("write the consumer manifest");
    std::fs::write(consumer.join("Cargo.lock"), "# theirs\n").expect("write their lockfile");
    assert_eq!(
        workspace_lockfile_at(&workspace_root_of(&vendored)),
        None,
        "a workspace that DEPENDS on Roteiro is not Roteiro. Calling it ours would read \
         their lockfile, find no boxlite in it, and fail their `cargo test` over a \
         repository that is not theirs — which is the defect #831 fixed in the corpus \
         guards, arriving here by the same route"
    );

    // And ours, synthesised rather than read, so the rule is held against the
    // text and not against whatever happens to be on disk.
    let ours = scratch.join("roteiro").join("crates").join("rto-exec");
    std::fs::create_dir_all(&ours).expect("create our layout");
    std::fs::write(
        scratch.join("roteiro").join("Cargo.toml"),
        format!(
            "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.package]\n{REPOSITORY_FIELD}\n"
        ),
    )
    .expect("write our manifest");
    assert_eq!(
        workspace_lockfile_at(&workspace_root_of(&ours)),
        Some(scratch.join("roteiro").join("Cargo.lock")),
        "our own workspace manifest must be recognised, or every run takes the packaged \
         exemption and these guards skip in silence"
    );

    // The live tree, where two signals independent of the marker agree this is
    // our checkout: the remote says so, and cargo compiled this file from a
    // `crates/rto-exec` inside it. In a package this half is vacuous by
    // construction, which is correct there and is said rather than hidden.
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
    }

    let _ = std::fs::remove_dir_all(&scratch);
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
    let source = std::fs::read_to_string(&lockfile)
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
    let source = std::fs::read_to_string(&lockfile)
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

    let extracted = Path::new(env!("CARGO_TARGET_TMPDIR")).join("runtime-pin-integrity");
    let _ = std::fs::remove_dir_all(&extracted);
    std::fs::create_dir_all(&extracted).expect("the target tmp dir should be writable");

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

    let _ = std::fs::remove_dir_all(&extracted);
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

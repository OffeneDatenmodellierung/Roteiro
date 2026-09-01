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

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

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
/// # Why the lockfile
///
/// It is what `--locked` builds resolve and what a dependency bump edits, so it
/// is the version that will actually be compiled. Reading `Cargo.toml`'s
/// requirement instead would assert against a range rather than a release.
#[test]
fn the_pins_are_for_the_boxlite_release_the_lockfile_resolves() {
    let lockfile = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("rto-exec lives two directories below the workspace root")
        .join("Cargo.lock");
    let source = std::fs::read_to_string(&lockfile)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", lockfile.display()));

    // Every `[[package]]` block whose name is boxlite, by its version line. A
    // scan rather than a TOML parse keeps this test free of a dependency for one
    // field; the shape it assumes is asserted below rather than assumed, because
    // finding nothing must fail loudly and not pass as "no skew found".
    let versions: Vec<&str> = source
        .split("[[package]]")
        .filter_map(|block| {
            let mut name = None;
            let mut version = None;
            for line in block.lines() {
                if let Some(rest) = line.strip_prefix("name = \"") {
                    name = rest.strip_suffix('"');
                } else if let Some(rest) = line.strip_prefix("version = \"") {
                    version = rest.strip_suffix('"');
                }
            }
            (name == Some("boxlite")).then_some(version).flatten()
        })
        .collect();

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
        versions[0], rto_exec::RUNTIME_VERSION,
        "the lockfile resolves boxlite {} but the sandbox-runtime pins are for {}. Bump \
         crates/rto-exec/src/runtime_pins.rs to the matching release and re-run \
         scripts/derive-runtime-file-pins.py — never the other way round, and never by \
         editing RUNTIME_VERSION alone, which would leave a new library paired with an old \
         shim and guest that the digests cannot tell apart.",
        versions[0],
        rto_exec::RUNTIME_VERSION
    );
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

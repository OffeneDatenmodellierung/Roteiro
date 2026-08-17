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

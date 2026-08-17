//! Refuse to build `exec-boxlite` against an unverified sandbox runtime.
//!
//! # What this exists to stop
//!
//! `boxlite` does not build a hypervisor when compiled from crates.io. Its own
//! build script fetches a ~25–29 MB prebuilt tarball with a bare `curl -fsSL`,
//! `include_bytes!`s it into the rlib, and extracts and executes it at run time.
//! That fetch verifies nothing, and its URL is overridable through
//! `BOXLITE_RUNTIME_URL` — so two builds of the same pinned crate version can
//! embed different executables, undetectably.
//!
//! Roteiro's answer is not to trust it but to *pre-empt* it: provision the
//! archive ourselves through the digest-pinned asset machinery
//! (`roteiro security prefetch --analyzer sandbox --allow-download`), then hand
//! `boxlite` a `file://` URL pointing at the verified copy. Its `curl` then
//! never reaches the network, because what it is asked to fetch is already on
//! disk.
//!
//! This script is the enforcement half of that. It runs before anything links
//! and fails the build unless:
//!
//! 1. `BOXLITE_RUNTIME_URL` is set — an unset variable means `boxlite` would go
//!    to GitHub unverified, which is the whole problem;
//! 2. it is a `file://` URL — an `http(s)://` one cannot be verified here, since
//!    the bytes would be fetched later by someone else's build script;
//! 3. the file's SHA-256 and size match the pin in `src/runtime_pins.rs` for
//!    this host platform.
//!
//! Failing at (1) is the common case for a first-time build, so the message is
//! a runnable recipe rather than a complaint.
//!
//! # It looks in the asset cache before it complains
//!
//! (1) used to be answered with a `$HOME`-shaped guess at where the archive
//! might be. That is how a build was refused on a machine that had everything it
//! needed: the archive was provisioned, at the canonical path, digest matching
//! the pin exactly — and the message said only that a variable was unset, in a
//! recipe that re-fetched a quarter-gigabyte of advisory databases to obtain a
//! file already on disk.
//!
//! So before saying anything, this now resolves the asset cache the way
//! `roteiro security prefetch` does — through the shared `src/asset_paths.rs` —
//! and looks. What it finds decides which of three things it says:
//!
//! - **present, and the digest matches**: the message names the path and the
//!   digest, and is one copy-pasteable `export` rather than a provisioning
//!   recipe. Nothing needs fetching.
//! - **present, and the digest does not**: the loudest failure here, naming both
//!   digests and both sizes. A file at the exact path `prefetch` writes to whose
//!   bytes are not the pinned ones is the case most worth stopping for, and
//!   falling through to "set `BOXLITE_RUNTIME_URL`" would bury it.
//! - **absent**: the provisioning recipe, which is where the bootstrap out of a
//!   bare `cargo install roteiro --all-features` is spelled out.
//!
//! # Why finding it is not the same as using it
//!
//! The obvious next step — having found and verified the archive, proceed and
//! let the variable go — does not work, and the reason is worth stating here
//! because it is not visible from this file.
//!
//! **This script is not what fetches the runtime.** `boxlite`'s own build script
//! reads `BOXLITE_RUNTIME_URL` from *its* environment and curls whatever it
//! names, defaulting to GitHub. A build script cannot set an environment
//! variable for another crate's build script, and cargo has already run
//! `boxlite`'s by the time it runs this one — `boxlite` declares `links =
//! "boxlite"`, so its metadata (`DEP_BOXLITE_RUNTIME_DIR`) is in this script's
//! environment, which is only possible if it went first.
//!
//! Measured, not reasoned about: with this script patched to proceed and the
//! variable unset, `cargo build -p rto-exec --features exec-boxlite` succeeds in
//! ~28 s, and `boxlite`'s build output reads `Downloading
//! https://github.com/boxlite-ai/boxlite/releases/...` followed by `Embedded
//! runtime: 5 files, 58.4 MB total`. The verified copy in the asset cache is
//! never opened.
//!
//! The digest check is what proves the local bytes are the pinned ones; the
//! variable is what makes `boxlite` consume *those* bytes instead of the
//! network's. Verifying a file the build then does not use would be a check with
//! nothing behind it, so discovery stops at telling the operator precisely what
//! to export.
//!
//! # Why the pins and the cache path are `include!`d
//!
//! A build script cannot depend on the crate it builds, and duplicating digests
//! is how digests drift. `src/runtime_pins.rs` and `src/asset_paths.rs` are both
//! written to be standalone — no `use`, no `crate::` paths — precisely so they
//! can be the single source of truth for both sides. The cache path especially:
//! a build script looking somewhere `prefetch` does not write is exactly the
//! failure above, with the two halves disagreeing instead of one being absent.

use std::path::{Path, PathBuf};

include!("src/runtime_pins.rs");
include!("src/asset_paths.rs");

fn main() {
    println!("cargo:rerun-if-changed=src/runtime_pins.rs");
    println!("cargo:rerun-if-changed=src/asset_paths.rs");
    println!("cargo:rerun-if-env-changed=BOXLITE_RUNTIME_URL");
    // Where the archive is looked for is an input to this script, so a build
    // that moves the cache must re-run it rather than keep the old answer.
    for var in ASSET_ROOT_VARS {
        println!("cargo:rerun-if-env-changed={var}");
    }

    // `CARGO_FEATURE_<NAME>` is set with `-` mapped to `_` and upper-cased.
    if std::env::var_os("CARGO_FEATURE_EXEC_BOXLITE").is_none() {
        return;
    }

    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let Some(archive) = archive_for(&os, &arch) else {
        fail(&format!(
            "the `exec-boxlite` feature has no pinned sandbox runtime for {os}/{arch}.\n\
             Pinned platforms are: {}.\n\
             Build without `exec-boxlite` on this host; the ingest and subprocess \
             backends are unaffected.",
            RUNTIME_ARCHIVES
                .iter()
                .map(|a| a.target)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    };

    let Some(url) = std::env::var_os("BOXLITE_RUNTIME_URL") else {
        // Resolved through the same code `prefetch` writes with, never a second
        // copy of the precedence — see the module docs.
        refuse_without_the_variable(
            &asset_root().join(RUNTIME_ASSET).join(RUNTIME_FILE),
            archive,
        );
    };

    let url = url.to_string_lossy().into_owned();
    let Some(path) = file_url_path(&url) else {
        fail(&format!(
            "BOXLITE_RUNTIME_URL must be a file:// URL so its bytes can be verified before they \
             are built in; got {url:?}.\n\
             A remote URL is fetched later by boxlite's build script, which checks nothing — \
             so there would be no point at which anything is verified.\n\n\
             Provision it with `roteiro security prefetch --analyzer sandbox --allow-download` \
             and point at the local copy."
        ));
    };

    match verify(&path, archive) {
        Ok(()) => {
            println!("cargo:rerun-if-changed={}", path.display());
            // Recorded so the runner can stamp the runtime it was built against
            // onto a run's evidence, rather than asserting it separately.
            println!(
                "cargo:rustc-env=ROTEIRO_SANDBOX_RUNTIME_TARGET={}",
                archive.target
            );
            println!(
                "cargo:rustc-env=ROTEIRO_SANDBOX_RUNTIME_SHA256={}",
                archive.sha256
            );
            println!("cargo:rustc-env=ROTEIRO_SANDBOX_RUNTIME_VERSION={RUNTIME_VERSION}");
        }
        Err(flaw) => fail(&format!(
            "the sandbox runtime at {path} is not the pinned artifact: {why}\n\n\
             Re-provision it with `roteiro security prefetch --analyzer sandbox \
             --allow-download`. If that keeps failing, the published artifact has changed and \
             the pin in crates/rto-exec/src/runtime_pins.rs must be re-derived deliberately — \
             never widened to make a build pass.",
            path = path.display(),
            why = flaw.summary(archive),
        )),
    }
}

/// Fail, having looked in the asset cache first — with whichever of the three
/// messages the archive on disk has earned.
///
/// Every one of them ends the build. What differs is whether the operator is
/// told to fetch something, to look at a file that is not what it should be, or
/// simply to export a path this script has already verified for them.
fn refuse_without_the_variable(provisioned: &Path, archive: &PinnedArchive) -> ! {
    let export = format!("BOXLITE_RUNTIME_URL=\"file://{}\"", provisioned.display());

    if !provisioned.exists() {
        fail(&format!(
            "`exec-boxlite` needs the pinned sandbox runtime provisioned before it can build, \
             and there is no copy at\n\n    \
             {provisioned}\n\n\
             Without one, boxlite's own build script downloads\n\n    \
             {url}\n\n\
             with no digest check of any kind and embeds whatever comes back into this \
             binary.\n\n\
             Provision it first. `security prefetch` is in the **default** install — it is \
             gated on `execution`, which is a default feature, and needs no execution backend \
             — so a first contact with the project bootstraps in three steps:\n\n    \
             cargo install roteiro\n    \
             roteiro security prefetch --analyzer sandbox --allow-download\n    \
             {export} \\\n      cargo install roteiro --all-features\n\n\
             `--analyzer sandbox` selects the shared runtime archive **alone**. A bare \
             `prefetch` also fetches the OSV and RustSec advisory databases — roughly 260 MB \
             that building `exec-boxlite` does not need.\n\n\
             In a checkout, the last step is:\n\n    \
             {export} \\\n      cargo build --features exec-boxlite\n\n\
             (expected sha256 {sha} for {target}. The path above is where `prefetch` writes; \
             it honours ROTEIRO_SECURITY_ASSETS and then ROTEIRO_HOME, and this script \
             resolves it with the same code `prefetch` does.)",
            provisioned = provisioned.display(),
            url = archive.url,
            sha = archive.sha256,
            target = archive.target,
        ));
    }

    if let Err(flaw) = verify(provisioned, archive) {
        fail(&format!(
            "the sandbox runtime in the asset cache is **not** the pinned artifact.\n\n    \
             {provisioned}\n\n    \
             pinned   sha256 {sha}  ({bytes} bytes, {target})\n    \
             on disk  {found}\n\n\
             This is refused rather than ignored, and the difference matters: a file at the \
             exact path `roteiro security prefetch` writes to, whose bytes are not the ones \
             pinned for this platform, is the single case here most worth stopping for. \
             Falling through to \"BOXLITE_RUNTIME_URL is unset\" would bury it under a \
             recipe.\n\n\
             Re-provision it:\n\n    \
             roteiro security prefetch --analyzer sandbox --allow-download\n\n\
             If that keeps producing these bytes, the published artifact has changed and the \
             pin in crates/rto-exec/src/runtime_pins.rs must be re-derived deliberately — \
             never widened to make a build pass.",
            provisioned = provisioned.display(),
            sha = archive.sha256,
            bytes = archive.bytes,
            target = archive.target,
            found = flaw.on_disk(),
        ));
    }

    fail(&format!(
        "`exec-boxlite` requires BOXLITE_RUNTIME_URL, and the archive it should name is \
         already provisioned and verified:\n\n    \
         {provisioned}\n    \
         sha256 {sha}  ({bytes} bytes, {target}) — matches the pin\n\n\
         Nothing needs fetching. This is one export:\n\n    \
         {export} \\\n      cargo build --features exec-boxlite\n\n\
         Why the variable is still required when this script can clearly find the file on its \
         own: **it is not this script that fetches the runtime.** boxlite's own build script \
         reads BOXLITE_RUNTIME_URL from its environment and curls whatever it names, \
         defaulting to GitHub with no digest check of any kind — and cargo has already run it \
         by the time it runs this one. A build script cannot set an environment variable for a \
         dependency's build script. Proceeding on the strength of the verified copy above \
         would therefore verify a file that the build does not use, while boxlite embedded \
         ~58 MB fetched over the network unpinned. The digest check proves these bytes are the \
         pinned ones; the variable is what points boxlite's curl at them. Both carry load.",
        provisioned = provisioned.display(),
        sha = archive.sha256,
        bytes = archive.bytes,
        target = archive.target,
    ));
}

/// The local path a `file://` URL names, or `None` for any other scheme.
///
/// Deliberately minimal: `file:///abs/path` and the `file://localhost/abs/path`
/// form. Percent-decoding is applied for the one character that actually turns
/// up in cache paths, a space; anything more exotic is refused by simply not
/// existing later, which is a clearer failure than a half-implemented decoder.
fn file_url_path(url: &str) -> Option<PathBuf> {
    let rest = url
        .strip_prefix("file://localhost")
        .or_else(|| url.strip_prefix("file://"))?;
    if !rest.starts_with('/') {
        return None;
    }
    Some(PathBuf::from(rest.replace("%20", " ")))
}

/// Why a candidate archive is not the pinned one.
///
/// Two arms rather than a string, because the digest-mismatch case has to be
/// reportable *beside* the pin — "found sha256 …" in a sentence is enough when
/// the path came from an explicit URL, and not enough when the file was
/// discovered in the cache and the operator has to compare the two by eye.
enum Flaw {
    /// The bytes could not be read at all.
    Unreadable(String),
    /// They were read, and are not the pinned artifact.
    Mismatch { sha256: String, bytes: u64 },
}

impl Flaw {
    /// One line, for a message that has already named the pin.
    ///
    /// The size is carried alongside the digest for the reason `runtime_pins.rs`
    /// keeps it: a truncated body is the common failure, and "expected 26520984
    /// bytes, found 1043" says so far more clearly than two digests that differ.
    fn summary(&self, archive: &PinnedArchive) -> String {
        match self {
            Self::Unreadable(err) => format!("cannot read it ({err})"),
            Self::Mismatch { sha256, bytes } => format!(
                "expected sha256 {} ({} bytes), found sha256 {sha256} ({bytes} bytes)",
                archive.sha256, archive.bytes
            ),
        }
    }

    /// Just what is on disk, for a message that prints the pin on the line above.
    fn on_disk(&self) -> String {
        match self {
            Self::Unreadable(err) => format!("unreadable ({err})"),
            Self::Mismatch { sha256, bytes } => format!("sha256 {sha256}  ({bytes} bytes)"),
        }
    }
}

/// Check a file against a pinned archive.
///
/// The digest is computed even when the size is already wrong, so that every
/// failure can name both digests. Hashing a truncated body costs nothing, and a
/// mismatch reported as a size alone leaves the reader unable to tell a partial
/// download from a different artifact that happens to be short.
fn verify(path: &Path, archive: &PinnedArchive) -> Result<(), Flaw> {
    let bytes = std::fs::read(path).map_err(|e| Flaw::Unreadable(e.to_string()))?;
    let actual_len = bytes.len() as u64;
    let digest = sha256_hex(&bytes);
    if actual_len != archive.bytes || digest != archive.sha256 {
        return Err(Flaw::Mismatch {
            sha256: digest,
            bytes: actual_len,
        });
    }
    Ok(())
}

/// Lowercase hex SHA-256, in the form the pins are written in.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let out = Sha256::digest(bytes);
    let mut hex = String::with_capacity(64);
    for byte in out {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Fail the build with a multi-line message that survives cargo's formatting.
///
/// `cargo:warning=` collapses newlines, so the recipe is printed to stderr —
/// where it stays readable — and the panic carries the one-line summary.
fn fail(message: &str) -> ! {
    eprintln!("\nerror: rto-exec/build.rs\n\n{message}\n");
    panic!(
        "{}",
        message
            .lines()
            .next()
            .unwrap_or("sandbox runtime is not verified")
    );
}

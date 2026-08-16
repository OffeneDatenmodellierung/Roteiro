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
//! (`roteiro security prefetch --allow-download`), then hand `boxlite` a
//! `file://` URL pointing at the verified copy. Its `curl` then never reaches
//! the network, because what it is asked to fetch is already on disk.
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
//! # Why the pins are `include!`d
//!
//! A build script cannot depend on the crate it builds, and duplicating digests
//! is how digests drift. `src/runtime_pins.rs` is written to be standalone —
//! no `use`, no `crate::` paths — precisely so it can be the single source of
//! truth for both sides.

use std::path::{Path, PathBuf};

include!("src/runtime_pins.rs");

fn main() {
    println!("cargo:rerun-if-changed=src/runtime_pins.rs");
    println!("cargo:rerun-if-env-changed=BOXLITE_RUNTIME_URL");

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
        fail(&format!(
            "`exec-boxlite` requires BOXLITE_RUNTIME_URL to point at a verified local copy of \
             the sandbox runtime.\n\n\
             Without it, boxlite's own build script downloads {url} with no digest check of any \
             kind and embeds whatever comes back into this binary.\n\n\
             Provision and verify it first:\n\n    \
             roteiro security prefetch --allow-download\n\n\
             then build against the verified copy:\n\n    \
             BOXLITE_RUNTIME_URL=\"file://$HOME/.roteiro/security/{asset}/{file}\" \\\n      \
             cargo build --features exec-boxlite\n\n\
             (expected sha256 {sha} for {target})",
            url = archive.url,
            asset = RUNTIME_ASSET,
            file = RUNTIME_FILE,
            sha = archive.sha256,
            target = archive.target,
        ));
    };

    let url = url.to_string_lossy().into_owned();
    let Some(path) = file_url_path(&url) else {
        fail(&format!(
            "BOXLITE_RUNTIME_URL must be a file:// URL so its bytes can be verified before they \
             are built in; got {url:?}.\n\
             A remote URL is fetched later by boxlite's build script, which checks nothing — \
             so there would be no point at which anything is verified.\n\n\
             Provision it with `roteiro security prefetch --allow-download` and point at the \
             local copy."
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
        Err(why) => fail(&format!(
            "the sandbox runtime at {path} is not the pinned artifact: {why}\n\n\
             Re-provision it with `roteiro security prefetch --allow-download`. \
             If that keeps failing, the published artifact has changed and the pin in \
             crates/rto-exec/src/runtime_pins.rs must be re-derived deliberately — never \
             widened to make a build pass.",
            path = path.display()
        )),
    }
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

/// Check a file against a pinned archive.
fn verify(path: &Path, archive: &PinnedArchive) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read it ({e})"))?;
    let actual_len = bytes.len() as u64;
    if actual_len != archive.bytes {
        return Err(format!(
            "expected {} bytes, found {actual_len}",
            archive.bytes
        ));
    }
    let digest = {
        use sha2::{Digest, Sha256};
        let out = Sha256::digest(&bytes);
        let mut hex = String::with_capacity(64);
        for byte in out {
            use std::fmt::Write as _;
            let _ = write!(hex, "{byte:02x}");
        }
        hex
    };
    if digest != archive.sha256 {
        return Err(format!(
            "expected sha256 {}, found {digest}",
            archive.sha256
        ));
    }
    Ok(())
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

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
//! # What is verified, and where
//!
//! **The files `boxlite` extracted, not the archive it got them from.** Those
//! are what `include_bytes!` puts in the binary, so those are what this checks —
//! every regular file in `DEP_BOXLITE_RUNTIME_DIR`, against the per-file digests
//! in `src/runtime_file_pins.rs`, before anything links. A mismatch, a missing
//! file, an unpinned extra file or a missing directory all stop the build.
//!
//! The "unpinned extra file" case is not tidiness. `boxlite`'s
//! `EmbeddedManifest::scan_entries` embeds *every* regular file in that
//! directory except its own `.boxlite-runtime-files`, so a file that appears
//! there is a file inside this binary.
//!
//! `src/runtime_file_pins.rs` is **generated** from the pinned archives by
//! `scripts/derive-runtime-file-pins.py`, which verifies each archive against
//! its own pin before opening it. The archives stay the source of truth; nobody
//! hand-types a digest.
//!
//! # Two paths, and the difference is egress
//!
//! - **`BOXLITE_RUNTIME_URL` set** — the strict path. It must be a `file://` URL
//!   whose bytes match the archive pin, and `boxlite`'s `curl` then reads that
//!   local file and opens no socket. Provision it with `roteiro security
//!   prefetch --analyzer sandbox --allow-download`. This is what an air-gapped
//!   or egress-controlled build wants, and it is checked *before* extraction as
//!   well as after.
//! - **unset** — the default. `boxlite` fetches the archive from GitHub over
//!   TLS, unpinned in transit, and the extracted files are then verified against
//!   the per-file pins. The build says so on its own output via
//!   `cargo:warning=`, names the licence notice, and prints the one-line recipe
//!   for the strict path.
//!
//! Enabling `exec-boxlite` is the consent for the second; the disclosure is what
//! it buys back. ADR-0014 v1.2 records this.
//!
//! # The trade, stated plainly
//!
//! Verification moved from **before the download** to **after extraction**, and
//! that is not a pure win.
//!
//! What is gained: the check now covers the bytes that are actually built in,
//! rather than a file that was merely *supposed* to be the input — and it holds
//! whether or not the operator knew to set a variable. The old arrangement
//! verified an archive and then trusted, without checking, that `boxlite` had
//! consumed that archive and nothing else.
//!
//! What is given up: on the default path the archive is downloaded by
//! `boxlite`'s `curl` and unpacked by `tar -xzf --strip-components=1` *before*
//! anything here looks at it. A malicious archive is therefore extracted on the
//! build machine before it is inspected, and this script only ever inspects the
//! runtime directory — a member that escaped it (a `../` path, a symlink
//! traversal) would be outside what these digests can speak for. Both `tar`
//! implementations in play refuse such members by default, and the transport is
//! TLS to a pinned release URL, so the window is narrow. It is not empty, and
//! the strict path above closes it: there, the bytes are verified before
//! `boxlite` is ever handed them.
//!
//! # Why the pins and the cache path are `include!`d
//!
//! A build script cannot depend on the crate it builds, and duplicating digests
//! is how digests drift. `src/runtime_pins.rs`, `src/runtime_file_pins.rs` and
//! `src/asset_paths.rs` are all written to be standalone — no `use`, no
//! `crate::` paths — precisely so they can be the single source of truth for
//! both sides. The cache path especially: a build script looking somewhere
//! `prefetch` does not write is how a build gets refused on a machine that had
//! the archive all along.
//!
//! # Why this script cannot simply point `boxlite` at the local copy
//!
//! It has already run. `boxlite` declares `links = "boxlite"`, so its
//! `cargo:runtime_dir=` metadata reaches this script as
//! `DEP_BOXLITE_RUNTIME_DIR` — which is only possible if cargo ran it first. A
//! build script cannot set an environment variable for another crate's build
//! script, so `BOXLITE_RUNTIME_URL` has to come from the environment or not at
//! all. That ordering is also exactly what makes the extracted files available
//! to check here.

use std::path::{Path, PathBuf};

include!("src/runtime_pins.rs");
include!("src/runtime_file_pins.rs");
include!("src/asset_paths.rs");

fn main() {
    println!("cargo:rerun-if-changed=src/runtime_pins.rs");
    println!("cargo:rerun-if-changed=src/runtime_file_pins.rs");
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

    // The pins the archive was derived into. A target with archive pins but no
    // file pins means the generated file was not re-run after a bump, and the
    // check below would silently have nothing to check.
    let Some(pinned) = runtime_files_for(archive.target) else {
        fail(&format!(
            "no extracted-file pins for {target}, though its archive is pinned.\n\
             crates/rto-exec/src/runtime_file_pins.rs is out of step with runtime_pins.rs — \
             re-derive it:\n\n    \
             scripts/derive-runtime-file-pins.py\n\n\
             Building on with no per-file pins would embed the runtime unverified, which is \
             the whole thing this refuses to do.",
            target = archive.target
        ));
    };

    // The archive half. Only when the operator asked for it: with the variable
    // set, boxlite's `curl` reads this local file and no socket is opened, and
    // the bytes are checked *before* anything extracts them.
    match std::env::var_os("BOXLITE_RUNTIME_URL") {
        Some(url) => verify_the_named_archive(&url.to_string_lossy(), archive),
        None => report_the_network_path(
            &asset_root().join(RUNTIME_ASSET).join(RUNTIME_FILE),
            archive,
        ),
    }

    // The extracted half, on both paths — because it is what actually ends up in
    // the binary. See the module docs for why this is where the guarantee now
    // lives, and for what it costs to check here rather than earlier.
    let dir = boxlite_runtime_dir();
    match verify_extracted(&dir, pinned) {
        Ok(checked) => {
            println!(
                "cargo:warning=rto-exec: verified {checked} extracted sandbox-runtime file(s) \
                 against the pins derived from the {target} archive (sha256 {sha})",
                target = archive.target,
                sha = archive.sha256,
            );
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
            "the sandbox runtime boxlite extracted is **not** what is pinned.\n\n    \
             {dir}\n\n\
             {why}\n\n\
             These are the files `include_bytes!` puts in this binary, so this stops the \
             build rather than reporting it. Nothing here is repaired by retrying: either \
             the bytes boxlite obtained are not the published artifact, or the published \
             artifact has changed and crates/rto-exec/src/runtime_pins.rs must be re-pinned \
             and `scripts/derive-runtime-file-pins.py` re-run — deliberately, never widened \
             to make a build pass.\n\n\
             To take the network out of it entirely, provision the archive and name it:\n\n    \
             roteiro security prefetch --analyzer sandbox --allow-download\n    \
             BOXLITE_RUNTIME_URL=\"file://{provisioned}\" cargo build --features exec-boxlite",
            dir = dir.display(),
            provisioned = asset_root()
                .join(RUNTIME_ASSET)
                .join(RUNTIME_FILE)
                .display(),
        )),
    }
}

/// Verify the archive `BOXLITE_RUNTIME_URL` names — the strict, no-egress path.
///
/// Unchanged in substance from when this was the only check: a `file://` URL,
/// and bytes matching the pin, or the build stops. What it no longer has to
/// carry alone is the guarantee, since the extracted files are verified too.
fn verify_the_named_archive(url: &str, archive: &PinnedArchive) {
    let Some(path) = file_url_path(url) else {
        fail(&format!(
            "BOXLITE_RUNTIME_URL must be a file:// URL so its bytes can be verified before they \
             are built in; got {url:?}.\n\
             A remote URL is fetched later by boxlite's build script, which checks nothing — so \
             setting one buys nothing over leaving the variable unset, and hides that a \
             download happened.\n\n\
             Provision it with `roteiro security prefetch --analyzer sandbox --allow-download` \
             and point at the local copy, or unset the variable and let the extracted files be \
             verified after the fetch."
        ));
    };

    match verify(&path, archive) {
        Ok(()) => {
            println!("cargo:rerun-if-changed={}", path.display());
            println!(
                "cargo:warning=rto-exec: BOXLITE_RUNTIME_URL names a verified local archive \
                 ({path}); boxlite's fetch stays on disk and no socket is opened",
                path = path.display()
            );
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

/// Say — on the build's own output — that this build fetched the runtime over
/// the network, and what it would take not to.
///
/// This is the disclosure half of the trade. Enabling `exec-boxlite` is the
/// consent; being told what it did is the least this owes in return, and
/// `cargo:warning=` is used because cargo shows it for a dependency's build
/// script on success where a plain `eprintln!` is swallowed.
///
/// It never fails the build. A wrong archive sitting unused in the asset cache
/// is worth saying loudly and is not a reason to refuse a build that does not
/// touch it — refusing over a file the build never opens is the mistake this
/// whole branch started from. The same bytes, once actually named by
/// `BOXLITE_RUNTIME_URL`, still stop the build hard in `verify_the_named_archive`.
fn report_the_network_path(provisioned: &Path, archive: &PinnedArchive) {
    println!(
        "cargo:warning=rto-exec: BOXLITE_RUNTIME_URL is unset, so boxlite fetched the sandbox \
         runtime from {url} — unpinned in transit, verified below against per-file digests \
         before anything links",
        url = archive.url
    );
    println!(
        "cargo:warning=rto-exec: this embeds third-party executables, GPL-2.0 and \
         LGPL-2.0 among them — see crates/rto-exec/NOTICE-boxlite-runtime.md for the \
         licence duties that creates"
    );

    match verify(provisioned, archive) {
        Ok(()) => println!(
            "cargo:warning=rto-exec: for a build with no network at all, name the verified \
             archive you already have: BOXLITE_RUNTIME_URL=\"file://{path}\"",
            path = provisioned.display()
        ),
        Err(Flaw::Unreadable(_)) => println!(
            "cargo:warning=rto-exec: for a build with no network at all: roteiro security \
             prefetch --analyzer sandbox --allow-download, then \
             BOXLITE_RUNTIME_URL=\"file://{path}\"",
            path = provisioned.display()
        ),
        Err(flaw) => println!(
            "cargo:warning=rto-exec: NOTE — the archive at {path} is not the pinned artifact \
             ({why}). This build did not use it, so this is not fatal; `roteiro security \
             prefetch --analyzer sandbox --allow-download` will replace it, and naming it \
             with BOXLITE_RUNTIME_URL while it is in this state would stop the build.",
            path = provisioned.display(),
            why = flaw.summary(archive),
        ),
    }
}

/// The directory `boxlite` extracted its runtime into.
///
/// Cargo hands this over because `boxlite` declares `links = "boxlite"` and its
/// build script emits `cargo:runtime_dir=…`; the `DEP_` form is only present for
/// a direct dependent, and only after the emitting script has run. That
/// ordering is what makes this check possible at all.
fn boxlite_runtime_dir() -> PathBuf {
    println!("cargo:rerun-if-env-changed=DEP_BOXLITE_RUNTIME_DIR");
    let Some(dir) = std::env::var_os("DEP_BOXLITE_RUNTIME_DIR") else {
        fail(
            "cargo did not pass DEP_BOXLITE_RUNTIME_DIR, so there is no way to tell what \
             boxlite extracted.\n\
             That variable comes from `links = \"boxlite\"` plus a `cargo:runtime_dir=` line in \
             boxlite's build script. If either has gone, this check is inert and the runtime \
             would be embedded unverified — which is not a thing to shrug at, so the build \
             stops. Re-pin boxlite deliberately, or build without `exec-boxlite`.",
        );
    };
    PathBuf::from(dir)
}

/// Verify every file in `boxlite`'s runtime directory against the pins.
///
/// Returns how many files were checked, so the build can say a number rather
/// than "ok" — a check that reports only success cannot be told from one that
/// looked at nothing.
///
/// The rules are deliberately closed rather than open. `boxlite`'s
/// `EmbeddedManifest::scan_entries` embeds **every regular file** in this
/// directory except its own `.boxlite-runtime-files` manifest, so an unexpected
/// file here is not clutter — it is a file that ends up inside this binary.
fn verify_extracted(dir: &Path, pinned: &PinnedRuntimeFiles) -> Result<usize, String> {
    if !dir.is_dir() {
        // `/nonexistent` is boxlite's own sentinel for stub mode, where it skips
        // dependency bundling entirely and embeds nothing at all.
        if dir == Path::new("/nonexistent") {
            return Err(
                "boxlite ran in stub mode (BOXLITE_DEPS_STUB=1) and extracted no runtime, so \
                 this binary would carry an `exec-boxlite` backend with nothing behind it.\n\
                 Stub mode exists for `cargo check`-style passes; it cannot produce a working \
                 sandbox. Unset BOXLITE_DEPS_STUB, or build without `exec-boxlite`."
                    .to_owned(),
            );
        }
        return Err(format!(
            "boxlite reported its runtime directory as {} and there is no directory there. \
             Nothing can be verified, and an unverifiable runtime is not a verified one.",
            dir.display()
        ));
    }

    let entries = std::fs::read_dir(dir).map_err(|e| format!("cannot list it ({e})"))?;
    let mut seen: Vec<String> = Vec::new();
    let mut problems: Vec<String> = Vec::new();

    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot read an entry ({e})"))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let meta =
            std::fs::symlink_metadata(&path).map_err(|e| format!("cannot stat {name} ({e})"))?;

        if meta.file_type().is_symlink() {
            // Not embedded — `scan_entries` skips anything that is not a regular
            // file — so a symlink cannot change the artifact. It is still held to
            // pointing at a pinned sibling, because a link out of this directory
            // has no honest reason to be here.
            match std::fs::read_link(&path) {
                Ok(target) => {
                    let target = target.to_string_lossy().into_owned();
                    if !pinned.files.iter().any(|f| f.name == target) {
                        problems.push(format!(
                            "  {name} is a symlink to {target:?}, which is not one of the \
                             pinned files"
                        ));
                    }
                }
                Err(e) => problems.push(format!("  {name} is a symlink that cannot be read ({e})")),
            }
            continue;
        }

        if meta.is_dir() {
            problems.push(format!(
                "  {name} is a directory; the runtime directory is flat and nothing pinned \
                 nests"
            ));
            continue;
        }

        if !meta.is_file() {
            problems.push(format!("  {name} is neither a regular file nor a symlink"));
            continue;
        }

        // boxlite writes this itself after extracting, and excludes it from what
        // it embeds. Allowed by name, and by nothing wider.
        if name == BOXLITE_FILE_MANIFEST {
            continue;
        }

        let Some(pin) = pinned.files.iter().find(|f| f.name == name) else {
            problems.push(format!(
                "  {name} is not pinned, and every regular file here is embedded — so this \
                 would be built in unverified"
            ));
            continue;
        };

        match std::fs::read(&path) {
            Ok(bytes) => {
                let actual = bytes.len() as u64;
                let digest = sha256_hex(&bytes);
                if actual != pin.bytes || digest != pin.sha256 {
                    problems.push(format!(
                        "  {name}\n      pinned  sha256 {} ({} bytes)\n      on disk sha256 \
                         {digest} ({actual} bytes)",
                        pin.sha256, pin.bytes
                    ));
                } else if let Some(mode) = setuid_or_setgid(&meta) {
                    // The mode itself is not pinned — `tar` applies the process
                    // umask, so it is a property of the extracting machine rather
                    // than of the archive. A set-user-ID bit is different: no
                    // umask adds one, and nothing published here has ever carried
                    // one.
                    problems.push(format!(
                        "  {name} is set-user-ID/set-group-ID (mode {mode:o}); no pinned \
                         runtime file carries one"
                    ));
                } else {
                    println!("cargo:rerun-if-changed={}", path.display());
                    seen.push(name);
                }
            }
            Err(e) => problems.push(format!("  {name} cannot be read ({e})")),
        }
    }

    for pin in pinned.files {
        if !seen.iter().any(|name| name == pin.name)
            && !problems.iter().any(|p| p.contains(pin.name))
        {
            problems.push(format!(
                "  {} is missing, and it is pinned for {}",
                pin.name, pinned.target
            ));
        }
    }

    if problems.is_empty() {
        return Ok(seen.len());
    }
    problems.sort();
    Err(format!(
        "{} of the extracted file(s) did not check out:\n\n{}",
        problems.len(),
        problems.join("\n")
    ))
}

/// The name `boxlite` writes its own file list under, which it excludes from
/// what it embeds — so this verifier excludes it too, and by name only.
const BOXLITE_FILE_MANIFEST: &str = ".boxlite-runtime-files";

/// The mode, when a file carries set-user-ID or set-group-ID.
#[cfg(unix)]
fn setuid_or_setgid(meta: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::MetadataExt as _;
    let mode = meta.mode();
    (mode & 0o6000 != 0).then_some(mode)
}

/// Windows has no such bit; the pinned platforms are all unix, and a host that
/// cross-compiles for one from Windows still gets every digest checked.
#[cfg(not(unix))]
fn setuid_or_setgid(_meta: &std::fs::Metadata) -> Option<u32> {
    None
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

// The pinned sandbox-runtime archives, and the host-platform selection.
//
// # Why this file is `include!`d rather than imported
//
// These pins are needed in two places that cannot share a crate graph: this
// library, which provisions and verifies the archive, and `build.rs`, which
// refuses to build `exec-boxlite` against anything else. A build script cannot
// depend on the crate it builds, so the single source of truth is this file and
// `build.rs` pulls it in with `include!`.
//
// That constrains what may appear here: **no `use`, no `crate::` paths, no
// references to anything outside this file.** It must compile standalone.
//
// # What is pinned, and why it has to be
//
// `boxlite` does not build a hypervisor when it is compiled from crates.io. Its
// three `-sys` crates each detect a published package (`.cargo_vcs_info.json`)
// and disable themselves, and `libkrun-sys` excludes the sources they would
// otherwise build. What actually runs is a prebuilt tarball that `boxlite`'s own
// `build.rs` fetches with a bare `curl -fsSL`, `include_bytes!`s into the rlib,
// and extracts and executes at run time.
//
// That fetch has **no expected digest of any kind** — searched for one four
// ways (`expected|_SHA256|checksum|digest`; `sha256|integrity|signature|cosign`;
// and two 64-hex-literal patterns over `build.rs` and `src/`), all NOT FOUND —
// and its URL is overridable through `BOXLITE_RUNTIME_URL`. Two builds of the
// same crate version can therefore embed different bytes, undetectably.
//
// Roteiro will not ship that. The digests below were computed from the real
// v0.9.7 release assets, and are what makes the embedded runtime reproducible:
// `roteiro security prefetch --allow-download` fetches and verifies the archive
// against them, and `build.rs` then refuses to build unless `BOXLITE_RUNTIME_URL`
// points at a local file whose bytes match. `boxlite`'s `curl` never reaches the
// network, because the `file://` URL it is given is already on disk.
//
// Bump these together with the `boxlite` pin in `Cargo.toml`; a version skew is
// caught by `build.rs` rather than discovered at run time.

/// One platform's prebuilt sandbox-runtime archive.
///
/// The `target` names are `boxlite`'s own, from its `runtime_target()` — they
/// are what appears in the release asset's filename, so they are the identifiers
/// that can actually be checked against upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinnedArchive {
    /// The platform, as the upstream release names it.
    pub target: &'static str,
    /// Where the archive is published.
    pub url: &'static str,
    /// Lowercase hex SHA-256 of the archive. Verified before it is installed and
    /// again before it is built against.
    pub sha256: &'static str,
    /// Its exact size. Redundant with the digest, and kept because a truncated
    /// body is the common failure and "expected 26520984 bytes, got 1043" says
    /// so far more clearly than two digests that differ.
    pub bytes: u64,
}

/// The `boxlite` release these archives belong to.
///
/// Checked against `boxlite`'s own `CARGO_PKG_VERSION` at build time, so a
/// dependency bump that forgets these pins fails the build instead of silently
/// pairing a new library with an old runtime.
pub const RUNTIME_VERSION: &str = "0.9.7";

/// The asset id the archive is provisioned under.
pub const RUNTIME_ASSET: &str = "boxlite-runtime";

/// The file name the archive is installed as.
pub const RUNTIME_FILE: &str = "boxlite-runtime.tar.gz";

/// Every platform Roteiro pins a sandbox runtime for.
///
/// These are the three `boxlite` publishes. A host outside this list cannot
/// build `exec-boxlite`, and is told so by name rather than by a link error.
pub const RUNTIME_ARCHIVES: &[PinnedArchive] = &[
    PinnedArchive {
        target: "darwin-arm64",
        url: "https://github.com/boxlite-ai/boxlite/releases/download/v0.9.7/boxlite-runtime-v0.9.7-darwin-arm64.tar.gz",
        sha256: "7f64529978cd2af420411ddfd4cc3b5799ca20234d90346c887cb596d52f8d4e",
        bytes: 26_520_984,
    },
    PinnedArchive {
        target: "linux-x64-gnu",
        url: "https://github.com/boxlite-ai/boxlite/releases/download/v0.9.7/boxlite-runtime-v0.9.7-linux-x64-gnu.tar.gz",
        sha256: "9ae495f55d363e6af04640ab55025ac80b4bf4762e38fa0b8ac80c7604e3148c",
        bytes: 24_957_005,
    },
    PinnedArchive {
        target: "linux-arm64-gnu",
        url: "https://github.com/boxlite-ai/boxlite/releases/download/v0.9.7/boxlite-runtime-v0.9.7-linux-arm64-gnu.tar.gz",
        sha256: "78e978d6398d5a78dc76d675941cb05287e8c70b1b647e98a479058a9652be28",
        bytes: 28_737_386,
    },
];

/// The upstream target name for an `(os, arch)` pair, or `None` for a platform
/// with no published runtime.
///
/// This mirrors `boxlite`'s own `runtime_target()`. It is spelled out rather
/// than derived so that a platform upstream adds later is a deliberate pin here,
/// not an automatic one.
#[must_use]
pub fn runtime_target(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("macos", "aarch64") => Some("darwin-arm64"),
        ("linux", "x86_64") => Some("linux-x64-gnu"),
        ("linux", "aarch64") => Some("linux-arm64-gnu"),
        _ => None,
    }
}

/// The pinned archive for an `(os, arch)` pair.
#[must_use]
pub fn archive_for(os: &str, arch: &str) -> Option<&'static PinnedArchive> {
    let target = runtime_target(os, arch)?;
    let mut index = 0;
    // A plain loop rather than an iterator: this file is `include!`d into a
    // build script, where keeping the surface to the language core is the point.
    while index < RUNTIME_ARCHIVES.len() {
        if str_eq(RUNTIME_ARCHIVES[index].target, target) {
            return Some(&RUNTIME_ARCHIVES[index]);
        }
        index += 1;
    }
    None
}

/// Byte equality for two `&str`, usable in the `const`-flavoured context above.
fn str_eq(a: &str, b: &str) -> bool {
    a.as_bytes() == b.as_bytes()
}

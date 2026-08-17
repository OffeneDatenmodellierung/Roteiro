//! Fail the build when a dependency's build script fetches something we have
//! not pinned.
//!
//! # The hole this closes
//!
//! `cargo deny --all-features check` reported `licenses ok` across the whole
//! resolved graph while `boxlite`'s build script was downloading ~25 MB of
//! GPL-2.0 and LGPL-2.0 executables and embedding them in the binary. Nothing
//! was wrong with `cargo deny`: it governs **crates**, and those binaries are
//! not crates. They enter through a `curl` in a build script, which no
//! crate-level gate can see.
//!
//! That hole is general, not specific to `boxlite`. Any dependency can add a
//! build script that fetches anything, at any version bump, and every
//! crate-metadata gate this project runs would stay green.
//!
//! So this checks the thing itself: it reads the build script of **every**
//! package in the `--all-features` dependency graph and fails on any that looks
//! like it fetches, unless that package and version is on the reviewed list
//! below with a reason.
//!
//! # What it does not cover — read this before trusting it
//!
//! This is a source scan, not a sandbox. It is deliberately the strongest cheap
//! check rather than a complete one, and an over-claimed gate is worse than a
//! narrow one:
//!
//! - **It reads only the build script itself** — the file cargo names as the
//!   package's `custom-build` target, plus a `build.rs` in the package root and
//!   any `.rs` under a `build/` directory. A script that `include!`s or `mod`s a
//!   file elsewhere in the crate is not followed.
//! - **It does not follow build-dependencies.** A build script that calls a
//!   helper crate which fetches is invisible here.
//! - **It cannot see obfuscation.** A URL assembled from parts, read from an
//!   environment variable, or decoded at run time will not match.
//! - **It says nothing about run time.** A crate that fetches when *called* is
//!   out of scope; this is about what enters the artifact at build time.
//! - **It says nothing about bytes already vendored** inside a published crate.
//! - **It reads the file now, not the bytes cargo compiled.** Nothing stops a
//!   script from changing between this scan and the build that uses it.
//!
//! What it does guarantee is narrower and still worth having: **no package in
//! this graph shells out to `curl`/`wget`, links an HTTP client, clones a git
//! repository, or writes an `http(s)://` URL as a *string literal* in its build
//! script without someone having written down why.** That is exactly the class
//! `boxlite` is in, and it would have caught it before the lockfile did not.
//!
//! # A skip is not a pass
//!
//! This file's first revision reached `continue` on three things it could not
//! read, and a `continue` here is indistinguishable from "inspected, clean". A
//! build script with one invalid byte in a comment, or one the process could not
//! open, was silently dropped and the audit reported success — the exact failure
//! this gate exists to prevent, one level down. Worse, the coverage line below
//! printed the *same* script count either way, so the number that was supposed to
//! make the gate checkable could not tell a skip from a pass either.
//!
//! Both are now closed, and on deliberately different terms:
//!
//! - **A build script whose bytes are not UTF-8 is scanned, not skipped.** It is
//!   read as bytes and matched lossily. Every marker in [`FETCH_MARKERS`] and
//!   [`GIT_REMOTE_MARKERS`] is ASCII, and `String::from_utf8_lossy` only replaces
//!   sequences containing bytes `>= 0x80`, so an ASCII substring survives byte
//!   for byte — the match is exact, not approximate. A stray byte in a comment is
//!   therefore read and *not* flagged, and needs no allow-list entry, because it
//!   is not an exception to anything. That is the right answer for a case that is
//!   legitimately possible: a gate should not fire on something harmless.
//! - **A build script that cannot be read at all fails the gate**, and there is
//!   deliberately **no allow-list for it.** The argument for allow-listing rested
//!   on non-UTF-8 being a legitimate thing for a crate to ship — and the bullet
//!   above removes non-UTF-8 from the failure path entirely. What remains is a
//!   path cargo named that this process cannot open: a broken checkout, a
//!   permissions fault, or tampering. None of those has a legitimate instance,
//!   and the answer to each is to repair the environment, never to bless it. An
//!   entry reading "could not read this one, shipped it anyway" would be the
//!   silent skip with paperwork, and this file already argues that a list nobody
//!   reads is how the next real fetch hides.
//!
//! The residue is stated rather than hidden: an unreadable script is reported as
//! **unaudited**, which is not the same claim as clean, and while one is
//! outstanding the flagged count is a lower bound. The audit says so in the
//! failure text instead of implying the list is complete.
//!
//! # It asks cargo where the build script is
//!
//! It used to guess: `build.rs` in the package root, a `build` key read from the
//! package's metadata, and any `.rs` under `build/`. The middle one never fired —
//! `cargo metadata` emits no `build` field (0 of 613 packages here), so that
//! branch was dead — and the first is simply wrong for a crate that keeps its
//! script somewhere else. **17 packages' build scripts were never read**, every
//! one a `tree-sitter*` crate whose script sits at `bindings/rust/build.rs`, and
//! every one of them compiling vendored C.
//!
//! Those 17 were not a skip that this file recorded and excused; they were a skip
//! it never knew it was making, which is why the count looked complete. The
//! authoritative answer is the `custom-build` target cargo already reports, so
//! that is what it reads, and it reads it **unconditionally** — no `is_file()`
//! guess first. If the path cargo names cannot be opened, that is an unreadable
//! script per the section above, not a package with nothing to scan. Coverage
//! went from 89 packages / 96 script files to **106 / 113**, with the flagged
//! count unchanged at 2 and no new false positives.
//!
//! # A URL in a comment does **not** trip this, and that is deliberate
//!
//! An earlier revision of these docs claimed it did. That was wrong, and the
//! matcher was the honest half: [`FETCH_MARKERS`] requires the quote that starts
//! a string literal (`"https://`), so a bare URL in a `//` comment is not
//! matched. Raw strings *are* — `r#"https://…` and `r"https://…` both contain
//! that quote.
//!
//! Widening it to a bare `https://` was measured over this repository's real
//! graph rather than argued about, and is the wrong trade:
//!
//! | matcher | flagged | false positives |
//! |---|---|---|
//! | quote-anchored (this one) | 2 | 0 |
//! | bare `http(s)://` | 29 | **27** |
//!
//! (613 packages, of which 106 have a build script; those 106 contain 113 script
//! files, because a package may have both a `build.rs` and a `build/` directory.
//! Both rows were measured over that same graph. The audit prints these numbers
//! on every run — a gate that says only "ok" cannot be told apart from one that
//! looked at nothing, which is exactly how the 17 missing packages above went
//! unnoticed.)
//!
//! The 27 are crates like `serde`, `quote`, `proc-macro2`, `anyhow`, `thiserror`
//! and `winapi`, every one of which merely cites a documentation or issue URL in
//! a comment. Twenty-seven allow-list entries of pure noise would not make this
//! gate stronger; it would make it unread, and the next real fetch would hide in
//! the list. **A gate nobody reads is the failure mode this file exists to
//! prevent**, so the claim is narrowed to what the code actually does instead.
//!
//! The residual gap — a build script that fetches using a URL that is never a
//! string literal, assembled from parts or read from the environment — is real,
//! and is the "cannot see obfuscation" limitation above. Matching comments would
//! not close it, because a comment cannot fetch anything.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// A package whose build script may fetch, and the reason that is accepted.
///
/// **Pinned to an exact version on purpose.** Allow-listing a crate by name
/// would let the next release add an unreviewed fetch and inherit the blessing;
/// a version bump has to come back through here.
struct Reviewed {
    name: &'static str,
    version: &'static str,
    reason: &'static str,
}

/// The reviewed exceptions. Each states *what* it fetches and *what pins it*.
///
/// An entry without a pin is not an exception, it is an unsolved problem — if a
/// dependency fetches something Roteiro cannot pin, the answer is to drop the
/// dependency, not to widen this list.
const REVIEWED: &[Reviewed] = &[
    Reviewed {
        name: "boxlite",
        version: "0.9.7",
        reason: "Downloads the prebuilt sandbox runtime with an unverified `curl` and embeds \
                 the extracted files with include_bytes!. Governed at the point the bytes \
                 enter the artifact: `crates/rto-exec/src/runtime_file_pins.rs` pins the \
                 SHA-256 and size of every file of every published archive — derived from the \
                 archive pins in `runtime_pins.rs` by `scripts/derive-runtime-file-pins.py`, \
                 never hand-written — and `crates/rto-exec/build.rs` verifies boxlite's \
                 extracted runtime directory against them before anything links, refusing a \
                 mismatch, a missing file or an unpinned extra one. Setting \
                 BOXLITE_RUNTIME_URL to a `file://` copy provisioned by `roteiro security \
                 prefetch --analyzer sandbox --allow-download` additionally verifies the \
                 archive *before* extraction and keeps the curl off the network entirely; \
                 without it the fetch does happen, over TLS to the pinned release URL, and \
                 the build says so on its own output. See NOTICE-boxlite-runtime.md for what \
                 the archive contains and the licence duties it creates.",
    },
    Reviewed {
        name: "libkrun-sys",
        version: "0.9.7",
        reason: "Would download libkrunfw and build vendored libkrun, but the published package \
                 excludes those sources and its build script detects a crates.io package and \
                 returns before fetching anything (stub mode). It is inert here; the runtime \
                 that actually executes comes through `boxlite` above. Re-review if the crate \
                 ever ships its `vendor/` directory.",
    },
];

/// Substrings that suggest a build script reaches the network.
///
/// Plain substrings rather than a regex: it keeps this test dependency-free and
/// fast, and the cost is only that it triggers slightly more eagerly, which is
/// the direction to err in.
const FETCH_MARKERS: &[&str] = &[
    // Shelling out to a downloader. The quotes are part of the marker: this is
    // the argv form, which is how `boxlite` does it.
    "\"curl\"",
    "\"wget\"",
    "\"aria2c\"",
    // Linking an HTTP client or opening a socket directly. Each of these was
    // measured over this repository's real graph and adds nothing to the flagged
    // count, so the extra coverage costs no noise.
    "reqwest",
    "ureq",
    "attohttpc",
    "isahc",
    "minreq",
    "curl::",
    "hyper::",
    "native_tls",
    "TcpStream",
    // A URL written as a string literal. The leading quote is load-bearing — see
    // the module docs for what dropping it costs (2 flagged becomes 29, of which
    // 27 are crates citing a docs URL in a comment). Raw strings are covered,
    // because `r#"` and `r"` both end in the quote this matches.
    "\"https://",
    "\"http://",
];

/// Git subcommands that reach a remote. Counted only when the script also
/// mentions `git`, so that a crate with a `"fetch"` string of its own — and
/// there are several — is not dragged in.
const GIT_REMOTE_MARKERS: &[&str] = &[
    "\"clone\"",
    "\"fetch\"",
    "\"pull\"",
    "\"submodule\"",
    "\"ls-remote\"",
];

#[test]
fn no_dependency_build_script_fetches_anything_unpinned() {
    let metadata = cargo_metadata();
    let packages = metadata["packages"]
        .as_array()
        .expect("cargo metadata should list packages");

    let mut scanned = 0usize;
    let mut offenders: Vec<String> = Vec::new();
    let mut unaudited: Vec<String> = Vec::new();
    let mut matched_reviews: BTreeSet<(String, String)> = BTreeSet::new();

    for package in packages {
        // `expect` rather than `unwrap_or_default`: metadata this file does not
        // understand is a reason to stop, not to scan a package called "" and
        // report it under a name no one can look up.
        let name = expect_str(package, "name").to_owned();
        let version = expect_str(package, "version").to_owned();
        let manifest = Path::new(expect_str(package, "manifest_path"));
        let root = manifest
            .parent()
            .unwrap_or_else(|| panic!("manifest path for {name} {version} has no directory"));

        let (scripts, unlistable) = build_scripts(root, &custom_build_sources(package));
        for dir in unlistable {
            unaudited.push(format!("  {name} {version}\n    {dir}"));
        }

        for script in scripts {
            scanned += 1;
            let marker = match scan_script(&script) {
                Scan::Inspected(Some(marker)) => marker,
                // Read, and nothing matched. The only arm that means "clean".
                Scan::Inspected(None) => continue,
                // Not read. Recorded, never skipped — see "A skip is not a pass".
                Scan::Unreadable(err) => {
                    unaudited.push(format!(
                        "  {name} {version}\n    {}\n    {err}",
                        script.display()
                    ));
                    continue;
                }
            };

            match REVIEWED
                .iter()
                .find(|r| r.name == name && r.version == version)
            {
                Some(_) => {
                    matched_reviews.insert((name.clone(), version.clone()));
                }
                None => offenders.push(format!(
                    "  {name} {version}\n    {}\n    matched: {marker}",
                    script.display()
                )),
            }
            break;
        }
    }

    assert!(
        scanned > 0,
        "no build scripts were scanned at all — the audit is not looking at anything, \
         which would make it pass vacuously"
    );

    // Report the coverage, always. A gate that says only "ok" cannot be told
    // apart from a gate that looked at nothing, and the numbers are what makes
    // a later "it flagged nothing" claim checkable rather than trusted.
    //
    // `unaudited` is printed here too, and that is the point of it: the old
    // version of this line was identical whether a script had been inspected or
    // silently dropped, so the counter that was meant to prove coverage was the
    // one thing that could not detect its absence.
    eprintln!(
        "build-script audit: {} packages, {scanned} build scripts, {} flagged, \
         {} reviewed exception(s) matched, {} unaudited",
        packages.len(),
        offenders.len() + matched_reviews.len(),
        matched_reviews.len(),
        unaudited.len()
    );

    // Before the flagged list, because an unaudited script makes that list a
    // lower bound: reporting "2 flagged" while something went unread is the
    // over-claim this file exists to refuse.
    assert!(
        unaudited.is_empty(),
        "{} build script(s) could not be read, so they were never audited:\n\n{}\n\n\
         This is not a pass. A script that cannot be inspected is unknown, not clean, and \
         the flagged count above is a lower bound while any of these is outstanding. There is \
         no allow-list for this on purpose — a script that is merely not UTF-8 is scanned \
         lossily and never lands here, so what is left is a broken checkout, a permissions \
         fault, or tampering. Fix the environment; do not add an exception.",
        unaudited.len(),
        unaudited.join("\n")
    );

    assert!(
        offenders.is_empty(),
        "{} dependency build script(s) look like they fetch, and are not reviewed:\n\n{}\n\n\
         A build script that downloads something is how {} of GPL binaries entered a build \
         that `cargo deny` called clean. If the fetch is real, pin what it fetches by digest \
         and record the pin — then add an entry to REVIEWED in this file saying what pins it. \
         Do not add an entry without one.",
        offenders.len(),
        offenders.join("\n"),
        "25 MB"
    );

    // A stale exception is its own bug: it reads as "we looked at this" when the
    // thing it describes is gone, and the next reader inherits false assurance.
    for review in REVIEWED {
        assert!(
            matched_reviews.contains(&(review.name.to_owned(), review.version.to_owned())),
            "REVIEWED lists {} {} but nothing in the graph matches it — either the dependency \
             is gone, or its version moved and the new one has not been reviewed. Remove the \
             entry or update it; do not leave it here.",
            review.name,
            review.version
        );
    }
}

/// The matcher matches exactly what the module docs say it matches.
///
/// This test exists because the docs and the code had already drifted once: an
/// earlier revision claimed a URL in a comment would trip the audit, and it
/// never did. That is the worst kind of documentation on a security gate —
/// it overstates the protection, so a reader stops looking for the hole.
///
/// Every case below is a line from the docs, turned into an assertion. If
/// someone widens or narrows [`FETCH_MARKERS`], this fails until the prose is
/// brought along with it.
#[test]
fn the_matcher_matches_exactly_what_the_docs_claim() {
    // Caught: fetch primitives, and URLs written as string literals.
    for caught in [
        r#"Command::new("curl").arg("-fsSL")"#,
        r#"Command::new("wget")"#,
        r"let body = reqwest::blocking::get(url)",
        r"ureq::get(&url).call()",
        r#"let u = "https://example.com/x.tgz";"#,
        r#"let u = "http://example.com/x.tgz";"#,
        // Raw strings end in the same quote, so they are covered.
        "let u = r#\"https://example.com/x.tgz\"#;",
        "let u = r\"https://example.com/x.tgz\";",
        // A remote git operation, but only alongside `git` itself.
        r#"Command::new("git").args(["clone", url])"#,
    ] {
        assert!(
            fetch_marker(caught).is_some(),
            "should have been flagged: {caught}"
        );
    }

    // Not caught, and the docs now say so rather than claiming otherwise.
    for missed in [
        "// see https://example.com/x.tgz for the artifact layout",
        "/* fetched from http://example.com by CI, not here */",
        "//! Upstream docs: https://example.com/",
        // Assembled at run time — the documented obfuscation limitation.
        r#"let u = format!("{HOST}/x.tgz");"#,
        // A local git read is not a remote one.
        r#"Command::new("git").args(["rev-parse", "HEAD"])"#,
    ] {
        assert!(
            fetch_marker(missed).is_none(),
            "should NOT have been flagged — the docs promise it is not: {missed}"
        );
    }

    // And the specific claim the docs make about the quote being load-bearing.
    assert!(fetch_marker(r#""https://x""#).is_some());
    assert!(fetch_marker("https://x").is_none());
}

/// A build script that is not valid UTF-8 is **scanned**, not skipped.
///
/// This is the bypass this file shipped with. `read_to_string` returns `Err` on
/// a single stray byte, the old code `continue`d on it, and a
/// `Command::new("curl")` one line below sailed through. Fault injection into a
/// real dependency confirmed it before the fix: the audit printed the same
/// script count and the same "2 flagged" whether the script was inspected or
/// dropped, so nothing about the output gave it away.
#[test]
fn a_build_script_that_is_not_utf8_is_still_scanned() {
    let script = Path::new(env!("CARGO_TARGET_TMPDIR")).join("not_utf8_build.rs");

    // 0xff is not a legal UTF-8 byte in any position, and here it sits in a
    // comment: the harmless case the docs promise is not punished, directly
    // above a fetch that must not be missed.
    //
    // Assembled rather than written as one literal so the bytes are not a
    // compile-time constant — `invalid_from_utf8` fires on a literal it can
    // decode itself, and the check below is worth more than the literal.
    let mut with_fetch = b"fn main() {\n    // stray byte: ".to_vec();
    with_fetch.push(0xff);
    with_fetch.extend_from_slice(b"\n    Command::new(\"curl\");\n}\n");
    assert!(
        std::str::from_utf8(&with_fetch).is_err(),
        "the fixture must really be invalid UTF-8, or this test proves nothing"
    );
    std::fs::write(&script, &with_fetch).expect("the target tmp dir should be writable");
    assert!(
        matches!(scan_script(&script), Scan::Inspected(Some("\"curl\""))),
        "a fetch behind one invalid byte must still be found"
    );

    // And the other half: being un-decodable is not itself a finding. The same
    // stray byte with nothing to match is clean, so this needs no allow-list.
    std::fs::write(&script, b"fn main() {\n    // stray byte: \xff\n}\n")
        .expect("the target tmp dir should be writable");
    assert!(
        matches!(scan_script(&script), Scan::Inspected(None)),
        "a non-UTF-8 script with no fetch must be clean, not an offender"
    );

    std::fs::remove_file(&script).ok();
}

/// A build script that cannot be read is **unaudited**, never clean.
#[test]
fn a_build_script_that_cannot_be_read_is_not_reported_as_clean() {
    // A directory rather than a chmod fixture: `read` fails on it everywhere
    // this builds, and it still fails when the suite runs as root, which a
    // permissions fixture would not.
    let a_directory = Path::new(env!("CARGO_TARGET_TMPDIR"));
    assert!(
        matches!(scan_script(a_directory), Scan::Unreadable(_)),
        "an unopenable path must be Unreadable, not Inspected(None) — that those \
         are different claims is the entire fix"
    );

    let missing = a_directory.join("no_such_build_script.rs");
    assert!(
        matches!(scan_script(&missing), Scan::Unreadable(_)),
        "a path that is not there is unknown, not clean"
    );
}

/// A build script cargo names is scanned even if it is not where we expect.
///
/// The 17 packages that went unread were lost to the dead `build`-key branch,
/// not to a stat check — but the `is_file()` guard that stood behind it would
/// have swallowed them just as quietly, because a package with nothing scanned
/// is indistinguishable from a package with nothing to scan. Taking cargo's path
/// unconditionally turns that class into an I/O error the audit has to report.
#[test]
fn a_declared_build_script_is_never_silently_dropped() {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let declared = root.join("declared_but_absent_build.rs");
    let (scripts, _) = build_scripts(root, std::slice::from_ref(&declared));
    assert!(
        scripts.contains(&declared),
        "a path cargo names must be scanned even when it is absent, so that it \
         surfaces as unreadable rather than as a package with no build script"
    );
}

/// An exception must carry its reasoning, on the same terms `deny.toml` demands
/// of an advisory ignore.
///
/// A bare name and version records that someone silenced the gate, not that
/// anyone examined it — and the entry is then indistinguishable from the drift
/// it was supposed to prevent. The floor is deliberately blunt: it cannot judge
/// whether a reason is *good*, only that one was written and says what pins the
/// fetch, which is the thing a reviewer needs in order to disagree.
#[test]
fn every_reviewed_exception_states_what_pins_it() {
    for review in REVIEWED {
        assert!(
            review.reason.len() > 80,
            "REVIEWED entry for {} {} has no real reasoning: {:?}",
            review.name,
            review.version,
            review.reason
        );
        let names_a_pin = ["pin", "digest", "sha256", "verif", "inert", "stub"]
            .iter()
            .any(|token| review.reason.to_ascii_lowercase().contains(token));
        assert!(
            names_a_pin,
            "REVIEWED entry for {} {} does not say what pins or neutralises the fetch. \
             An exception without one is an unsolved problem, not an exception.",
            review.name, review.version
        );
    }
}

/// The first fetch marker in `source`, if any.
fn fetch_marker(source: &str) -> Option<&'static str> {
    if let Some(marker) = FETCH_MARKERS.iter().find(|m| source.contains(**m)) {
        return Some(marker);
    }
    if source.contains("git") {
        return GIT_REMOTE_MARKERS
            .iter()
            .find(|m| source.contains(**m))
            .copied();
    }
    None
}

/// What looking at one build script file produced.
///
/// The two arms are the whole point. "Read it, found nothing" and "could not
/// read it" are different claims, and collapsing them into one `continue` is
/// what let an unaudited script pass — see "A skip is not a pass" above.
#[derive(Debug)]
enum Scan {
    /// Read and inspected. The marker found, if any; `None` means clean.
    Inspected(Option<&'static str>),
    /// Not read, and so not known to be anything. Carries the I/O error.
    Unreadable(String),
}

/// Inspect one build script.
///
/// Reads **bytes**, not a `String`. A build script is not required to be valid
/// UTF-8, and `read_to_string` reports that as an error indistinguishable from a
/// missing file — which is how a `curl` behind one stray byte used to pass. The
/// lossy conversion is exact for this matcher's purposes: every marker is ASCII,
/// and `from_utf8_lossy` only ever replaces sequences of bytes `>= 0x80`, so no
/// ASCII substring can be broken up or invented by it.
fn scan_script(path: &Path) -> Scan {
    match std::fs::read(path) {
        Ok(bytes) => Scan::Inspected(fetch_marker(&String::from_utf8_lossy(&bytes))),
        Err(err) => Scan::Unreadable(err.to_string()),
    }
}

/// Where cargo itself says a package's build scripts are.
///
/// This is the authoritative answer, and the reason to prefer it over guessing
/// `build.rs` is measured: 17 packages in this graph keep their build script
/// somewhere else and were never read at all.
fn custom_build_sources(package: &serde_json::Value) -> Vec<PathBuf> {
    package["targets"]
        .as_array()
        .expect("cargo metadata should list targets for every package")
        .iter()
        .filter(|target| {
            target["kind"]
                .as_array()
                .is_some_and(|kinds| kinds.iter().any(|k| k.as_str() == Some("custom-build")))
        })
        .filter_map(|target| target["src_path"].as_str())
        .map(PathBuf::from)
        .collect()
}

/// Every build script file a package has, and every directory that would not
/// list.
///
/// A `BTreeSet` so the result is deduplicated (a declared script may also sit
/// under `build/`) and ordered the same way on every machine — `read_dir` order
/// is not, and a gate whose reported match depends on the filesystem is harder
/// to trust than one that does not.
fn build_scripts(root: &Path, declared: &[PathBuf]) -> (Vec<PathBuf>, Vec<String>) {
    let mut found: BTreeSet<PathBuf> = BTreeSet::new();
    let mut unlistable = Vec::new();

    // Unconditionally, with no `is_file()` guard. If cargo names a path that
    // cannot be opened, that must surface as an unreadable script, not vanish
    // into "this package has no build script".
    found.extend(declared.iter().cloned());

    let default = root.join("build.rs");
    if default.is_file() {
        found.insert(default);
    }
    let dir = root.join("build");
    if dir.is_dir() {
        collect_rs(&dir, &mut found, &mut unlistable);
    }

    (found.into_iter().collect(), unlistable)
}

/// Every `.rs` file under `dir`, recursively, plus the directories that would
/// not list.
///
/// A directory this cannot read may contain a build script, so returning early
/// and quietly would hide exactly as much as an unreadable file does.
fn collect_rs(dir: &Path, into: &mut BTreeSet<PathBuf>, unlistable: &mut Vec<String>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            unlistable.push(format!("{} (directory): {err}", dir.display()));
            return;
        }
    };
    for entry in entries {
        let path = match entry {
            Ok(entry) => entry.path(),
            Err(err) => {
                unlistable.push(format!("{} (directory entry): {err}", dir.display()));
                continue;
            }
        };
        if path.is_dir() {
            collect_rs(&path, into, unlistable);
        } else if path.extension().is_some_and(|e| e == "rs") {
            into.insert(path);
        }
    }
}

/// A string field `cargo metadata` always emits, or a loud failure.
fn expect_str<'a>(package: &'a serde_json::Value, field: &str) -> &'a str {
    package[field]
        .as_str()
        .unwrap_or_else(|| panic!("cargo metadata should give every package a `{field}`"))
}

/// The resolved `--all-features` dependency graph.
///
/// `--locked` so the audit can never be the thing that rewrites `Cargo.lock`,
/// and so it reads the same graph the build did.
fn cargo_metadata() -> serde_json::Value {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("rto-exec lives two directories below the workspace root")
        .to_path_buf();

    let output = std::process::Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--all-features",
            "--format-version",
            "1",
            "--locked",
        ])
        .current_dir(&workspace)
        .output()
        .expect("cargo metadata should be runnable");

    assert!(
        output.status.success(),
        "cargo metadata failed, so the build-script audit could not run. It must fail loudly \
         rather than skip: a gate that quietly does nothing is what this test exists to \
         replace.\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    serde_json::from_slice(&output.stdout).expect("cargo metadata should emit JSON")
}

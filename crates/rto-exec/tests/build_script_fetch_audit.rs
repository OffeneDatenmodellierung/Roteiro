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
//! - **It reads only the build script itself** — `build.rs`, the `build` target
//!   named in the manifest, and any `.rs` under a `build/` directory. A script
//!   that `include!`s or `mod`s a file elsewhere in the crate is not followed.
//! - **It does not follow build-dependencies.** A build script that calls a
//!   helper crate which fetches is invisible here.
//! - **It cannot see obfuscation.** A URL assembled from parts, read from an
//!   environment variable, or decoded at run time will not match.
//! - **It says nothing about run time.** A crate that fetches when *called* is
//!   out of scope; this is about what enters the artifact at build time.
//! - **It says nothing about bytes already vendored** inside a published crate.
//!
//! What it does guarantee is narrower and still worth having: **no package in
//! this graph shells out to `curl`/`wget`, links an HTTP client, clones a git
//! repository, or writes an `http(s)://` URL as a *string literal* in its build
//! script without someone having written down why.** That is exactly the class
//! `boxlite` is in, and it would have caught it before the lockfile did not.
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
//! | matcher | script files | flagged | false positives |
//! |---|---|---|---|
//! | quote-anchored (this one) | 96 | 2 | 0 |
//! | bare `http(s)://` | 96 | 29 | **27** |
//!
//! (613 packages, of which 89 have a build script; those 89 contain 96 script
//! files, because a package may have both a `build.rs` and a `build/` directory.
//! The audit prints these numbers on every run — a gate that says only "ok"
//! cannot be told apart from one that looked at nothing.)
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
        reason: "Downloads the prebuilt sandbox runtime with an unverified `curl` and embeds it \
                 with include_bytes!. Governed: `crates/rto-exec/src/runtime_pins.rs` pins the \
                 SHA-256 and size of every published archive, `roteiro security prefetch \
                 --allow-download` verifies before installing, and `crates/rto-exec/build.rs` \
                 refuses to build unless BOXLITE_RUNTIME_URL names a local file matching the \
                 pin — so its curl never reaches the network. See NOTICE-boxlite-runtime.md \
                 for what the archive contains and the licence duties it creates.",
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
    let mut matched_reviews: BTreeSet<(String, String)> = BTreeSet::new();

    for package in packages {
        let name = package["name"].as_str().unwrap_or_default().to_owned();
        let version = package["version"].as_str().unwrap_or_default().to_owned();
        let manifest = Path::new(package["manifest_path"].as_str().unwrap_or_default());
        let Some(root) = manifest.parent() else {
            continue;
        };

        for script in build_scripts(root, package.get("build").and_then(|b| b.as_str())) {
            scanned += 1;
            let Ok(source) = std::fs::read_to_string(&script) else {
                continue;
            };
            let Some(marker) = fetch_marker(&source) else {
                continue;
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
    eprintln!(
        "build-script audit: {} packages, {scanned} build scripts, {} flagged, \
         {} reviewed exception(s) matched",
        packages.len(),
        offenders.len() + matched_reviews.len(),
        matched_reviews.len()
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

/// Every build script file a package has.
fn build_scripts(root: &Path, declared: Option<&str>) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let default = root.join("build.rs");
    if default.is_file() {
        found.push(default);
    }
    if let Some(declared) = declared
        && declared != "build.rs"
    {
        let path = root.join(declared);
        if path.is_file() {
            found.push(path);
        }
    }
    let dir = root.join("build");
    if dir.is_dir() {
        collect_rs(&dir, &mut found);
    }
    found
}

/// Every `.rs` file under `dir`, recursively.
fn collect_rs(dir: &Path, into: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs(&path, into);
        } else if path.extension().is_some_and(|e| e == "rs") {
            into.push(path);
        }
    }
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

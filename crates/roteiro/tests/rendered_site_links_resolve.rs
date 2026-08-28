//! Gate: every local link in the **rendered** site resolves (issue #459).
//!
//! # The gap this closes
//!
//! `roteiro check` reads **sources** — ADR and blueprint wiki-links, `@rto:`
//! annotations, version drift, `site-page:` slug collisions — and does real work
//! there. What it cannot do is resolve an `href` in the **emitted HTML**, and a
//! whole class of defect is correct in the source and broken only once served:
//! a `.md`→`.html` rewrite pointing at a page nothing emitted, a link into
//! repository source the site does not publish, an anchor written to GitHub's
//! slug rule rather than the renderer's.
//!
//! #459 counted four such defects. Three were caught by a link auditor somebody
//! wrote by hand during #454, ran once, and threw away; the fourth by a reviewer.
//! Its framing is the reason this exists: *"It protected this PR and would not
//! have protected the next one. The gate was the blind one, which is the part
//! that matters."*
//!
//! # Why a test rather than a `check` subcommand or a workflow step
//!
//! #459 offered those two and asked for a decision. A test is the third option
//! and costs least: it runs inside `checks`, which is already a **required**
//! status check, so it gates merges without a new entry point to remember and
//! without making `roteiro check` depend on a rendered site. The site is
//! rendered here from the working tree, so it fails **before** a merge rather
//! than after a deploy.
//!
//! # No allow-list
//!
//! #459 warned that a gate red on arrival gets switched off, and offered an
//! enumerated shrinking list as the alternative to fixing first. Neither was
//! needed: the twelve failures it recorded had since been fixed, and the two
//! that remained — anchors on `SANDBOXED_LINTING.md` written to GitHub's slug
//! rule — are fixed in the same change. This lands at zero.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const BIN: &str = env!("CARGO_BIN_EXE_roteiro");

/// Whether `rel` names an HTML page.
///
/// Via `Path::extension` rather than `ends_with(".html")`: clippy reads the
/// latter as a case-sensitive extension comparison, and it is right that the
/// question is about the extension rather than the string's tail.
fn is_html(rel: &str) -> bool {
    Path::new(rel)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("html"))
}

/// Render the site into a scratch directory, or `None` outside a checkout —
/// a packaged crate carries no `docs/`, and that is a skip rather than a failure.
fn render() -> Option<PathBuf> {
    let root = common::repo_root();
    if !root.join("docs").is_dir() || !root.join("website/pages").is_dir() {
        return None;
    }
    // Keyed by pid **and** a process-wide counter, not the pid alone. Rust runs
    // the tests in a binary in parallel, so a pid-only path is shared by every
    // test in this file: the moment a second one calls `render`, the two race to
    // delete and recreate the directory the other is reading. That failure needs
    // a second caller to exist before it bites, so it would arrive as a flake in
    // somebody else's change rather than as a mistake in this one.
    let out = common::scratch_dir("site-links");
    let result = std::process::Command::new(BIN)
        .args(["render", "docs", "--out", out.to_str().expect("utf-8 path")])
        .current_dir(&root)
        .output()
        .expect("run `roteiro render docs`");
    // `output()` rather than `status()`: when this gate fails in CI, the render's
    // own diagnosis is the whole of the evidence, and `status()` throws it away.
    // A gate that fails without saying why costs more than it saves.
    assert!(
        result.status.success(),
        "`render docs` failed: {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        result.status,
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr),
    );
    Some(out)
}

/// Every emitted file, and for HTML the set of `id` attributes it carries.
fn emitted(root: &Path) -> (BTreeSet<String>, BTreeMap<String, BTreeSet<String>>) {
    fn walk(dir: &Path, root: &Path, files: &mut BTreeSet<String>) {
        let rd =
            std::fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()));
        for entry in rd {
            let p = entry.expect("dir entry").path();
            if p.is_dir() {
                walk(&p, root, files);
            } else {
                files.insert(
                    p.strip_prefix(root)
                        .expect("under root")
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    let mut files = BTreeSet::new();
    walk(root, root, &mut files);
    let mut ids = BTreeMap::new();
    for rel in files.iter().filter(|f| is_html(f)) {
        let text = std::fs::read_to_string(root.join(rel)).expect("read page");
        // The leading space matters: a bare `id="` also matches `data-id="`.
        let set: BTreeSet<String> = text
            .match_indices(" id=\"")
            .filter_map(|(i, _)| {
                let rest = &text[i + 5..];
                rest.find('"').map(|e| rest[..e].to_owned())
            })
            .collect();
        ids.insert(rel.clone(), set);
    }
    (files, ids)
}

/// One hex digit, or `None` if that byte is not one.
fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Percent-decode, since an emitted `href` may escape characters a filename or
/// an `id` carries literally.
///
/// Decoding runs over **bytes** and converts once at the end. The obvious
/// spelling — `out.push(v as char)` into a `String` — is wrong for anything
/// outside ASCII: `as char` reinterprets a byte as a code point, so the three
/// bytes of `%E2%80%94` become `â`, `\u{80}`, `\u{94}` instead of the em dash
/// they encode. A percent-encoded non-ASCII anchor would then fail to match the
/// `id` the renderer emitted, and this gate would report a broken link that a
/// browser resolves perfectly well. A **false** failure is the worse kind here:
/// it trains people to distrust the gate.
///
/// Hex digits are read byte-wise rather than by slicing `&s[i + 1..i + 3]`,
/// which panics when those bytes are not a char boundary — a literal multi-byte
/// character immediately after a `%` was enough to bring the whole test down.
///
/// Raised by Copilot on PR #653.
fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    // Lossy rather than strict: a malformed escape in an authored href is a
    // broken link to report, not a reason for the gate to panic.
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn every_local_link_in_the_rendered_site_resolves() {
    let Some(root) = render() else {
        return;
    };
    let (files, ids) = emitted(&root);
    let html: Vec<&String> = files.iter().filter(|f| is_html(f)).collect();
    assert!(
        html.len() >= 20,
        "only {} pages rendered — the fixture is not looking at the real site, and \
         a link check over nothing passes",
        html.len()
    );

    let mut broken = Vec::new();
    let mut checked = 0usize;
    for rel in &html {
        let text = std::fs::read_to_string(root.join(rel)).expect("read page");
        for (i, _) in text.match_indices(" href=\"") {
            let rest = &text[i + 7..];
            let Some(end) = rest.find('"') else { continue };
            let href = rest[..end].replace("&amp;", "&");
            // External and protocol-relative links are somebody else's uptime.
            if href.starts_with("http://")
                || href.starts_with("https://")
                || href.starts_with("mailto:")
                || href.starts_with("//")
            {
                continue;
            }
            checked += 1;
            let (target, frag) = match href.split_once('#') {
                Some((t, f)) => (t, Some(decode(f))),
                None => (href.as_str(), None),
            };
            // A bare `#anchor` addresses the page it is written on.
            let page = if target.is_empty() {
                (*rel).clone()
            } else {
                let base = Path::new(rel).parent().unwrap_or(Path::new(""));
                let mut joined = base.join(decode(target));
                if target.ends_with('/') {
                    joined = joined.join("index.html");
                }
                normalise(&joined)
            };
            if !files.contains(&page) {
                broken.push(format!("{rel} -> {href}  (no such file: {page})"));
                continue;
            }
            if let Some(f) = frag
                && !f.is_empty()
                && is_html(&page)
                && !ids.get(&page).is_some_and(|s| s.contains(&f))
            {
                broken.push(format!("{rel} -> {href}  (no anchor `{f}` in {page})"));
            }
        }
    }

    assert!(
        broken.is_empty(),
        "these links are correct where written and broken where served — the class \
         `roteiro check` cannot see, because it reads sources and these exist only \
         in the rendered output:\n  {}",
        broken.join("\n  ")
    );
    // The relation above is satisfiable by finding no links at all, so say how
    // much was actually resolved.
    assert!(
        checked >= 100,
        "only {checked} local links resolved across {} pages — the scraper has \
         stopped matching, and a gate that checks nothing passes",
        html.len()
    );
    std::fs::remove_dir_all(&root).ok();
}

/// `a/b/../c` → `a/c`, without touching the filesystem: the target need not
/// exist, and whether it does is the question being asked.
fn normalise(p: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                parts.pop();
            }
            std::path::Component::CurDir => {}
            other => parts.push(other.as_os_str().to_string_lossy().into_owned()),
        }
    }
    parts.join("/")
}

/// Percent-decoding round-trips non-ASCII (#653).
///
/// The bug this pins was invisible on ASCII, which is every link in the site
/// today — so the gate would have kept passing while being wrong, until the
/// first heading with an em dash or an accent produced a percent-encoded anchor
/// and the gate reported it broken. A false failure from a gate is worse than
/// no gate: it is the one that gets switched off.
#[test]
fn decoding_handles_non_ascii_and_malformed_escapes() {
    // The em dash, which this repository's headings use heavily.
    assert_eq!(decode("a%E2%80%94b"), "a—b");
    // Literal non-ASCII passes through unchanged.
    assert_eq!(decode("café"), "café");
    // A `%` immediately before a multi-byte character: the old byte-slicing
    // version panicked here rather than returning anything.
    assert_eq!(decode("%é"), "%é");
    // Ordinary ASCII escapes still work.
    assert_eq!(decode("a%20b"), "a b");
    // A malformed escape is left alone rather than crashing the gate.
    assert_eq!(decode("100%"), "100%");
    assert_eq!(decode("%zz"), "%zz");
}

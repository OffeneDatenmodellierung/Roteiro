//! Guard: a `](self#anchor)` intra-doc link must name a heading that exists
//! (issue #397).
//!
//! rustdoc validates the **path** of an intra-doc link and never the
//! **fragment**. `[x](self#paging)` and `[x](self#no-such-anchor)` compile
//! identically clean: the first resolves to `index.html#paging`, the second to
//! `index.html#no-such-anchor`, and rustdoc has no opinion about whether that
//! anchor is on the page. Nothing warns, nothing tests, and the rot is
//! invisible in review because the *source* still looks right.
//!
//! That matters here because `graph_api` deliberately keeps **one** `# Paging`
//! section and points eleven parameter docs at it instead of restating the
//! contract eleven times. That is the better pattern — a stale duplicate is
//! worse than a stale pointer in every way except this one — but it
//! concentrates the risk: rename the heading and all eleven pointers die at
//! once, with a completely clean build.
//!
//! # What this checks
//!
//! For every `.rs` file in the workspace, every `](self#anchor)` in a doc
//! comment must correspond to a heading in that same module's `//!` docs whose
//! rustdoc-generated id is `anchor`. Bare `](#anchor)` in an *outer* (`///`)
//! doc comment is rejected outright: it points at the documented item's own
//! page rather than the module's, which is never what "see the module's paging
//! section" means.
//!
//! # Why source-level, and what that costs
//!
//! The obvious check is to build the docs and grep the HTML. That would read
//! rustdoc's *real* ids rather than a replica of its rule, but it is slow, and
//! here it is also awkward: `graph_api` is behind `--features explorer` (so
//! `cargo doc -p roteiro --no-deps` emits no `graph_api` page at all) and most
//! of the eleven pointers hang off private items, so it would additionally need
//! `--document-private-items`. A guard that needs a specific non-default
//! feature set is a guard that quietly stops running.
//!
//! So this reads the source instead, and pays for it by replicating rustdoc's
//! heading-to-id rule in [`slugify`] rather than reading it. **That is the
//! limitation**: for an exotic heading — one carrying raw HTML, a footnote, or
//! a reference-style link — this file's rule could disagree with rustdoc's, and
//! then the check is wrong in whichever direction the disagreement falls.
//!
//! It was measured rather than assumed. Every heading in the workspace's `//!`
//! docs that rustdoc will build without a C/C++ toolchain — 79 of them, taking
//! in bold, inline code, intra-doc links, colons, quotes and em-dashes — was
//! slugified by this file and looked up in the generated HTML: **79 exact
//! matches, no disagreements**. The 13 that were not compared are behind
//! `serve` / `exec-boxlite`. If a heading ever needs something stranger than
//! the forms above, check the generated id by hand and extend `slugify` in the
//! same change.
//!
//! Two narrower gaps, both of which fail loudly rather than quietly:
//! rustdoc's *synthetic* section ids (`#structs`, `#functions`, …) are not
//! headings anyone wrote, so a link to one is reported as dead; and `self`
//! inside an inline `mod` is a scope this file does not resolve, so it is
//! refused outright rather than answered wrongly.
//!
//! This test is **not feature-gated** — it reads files and links against
//! nothing — so it runs under `cargo test --workspace` on the default feature
//! set as well as under CI's `--all-features` job. That is deliberate: the
//! thing it guards is only built with `--features explorer`, and a guard that
//! shares its subject's feature gate is a silent pass in a different costume.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// rustdoc's heading-to-id rule, replicated
// ---------------------------------------------------------------------------

/// rustdoc's `slugify`, applied to a heading's plain text.
///
/// Alphanumerics are kept (ASCII ones lowercased), `-` and `_` are kept, ASCII
/// whitespace becomes `-`, and everything else is dropped. So `` # `limit` and
/// `offset` `` becomes `limit-and-offset`, and `# Paging` becomes `paging`.
fn slugify(text: &str) -> String {
    text.chars()
        .filter_map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                if c.is_ascii() {
                    Some(c.to_ascii_lowercase())
                } else {
                    Some(c)
                }
            } else if c.is_whitespace() && c.is_ascii() {
                Some('-')
            } else {
                None
            }
        })
        .collect()
}

/// Reduce a heading's markdown to the text rustdoc slugifies.
///
/// Only inline links need explicit handling — `[text](target)` contributes
/// `text` alone, whereas slugifying the raw source would fold the target in
/// too. Backticks, `*` and `#` need no stripping: `slugify` already drops every
/// character that is neither alphanumeric nor `-`/`_`/whitespace.
fn heading_text(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        // `[text](target)` — keep `text`, drop `(target)`. Anything else falls
        // through as literal text, which slugify then handles.
        match (after.find(']'), after.find("](")) {
            (Some(close), Some(link)) if close == link => {
                out.push_str(&after[..close]);
                match after[close + 2..].find(')') {
                    Some(end) => rest = &after[close + 2 + end + 1..],
                    None => return out,
                }
            }
            _ => {
                out.push('[');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// rustdoc's `IdMap::derive`: the first `paging` is `paging`, the next is
/// `paging-1`, and so on.
fn derive_id(seen: &mut HashMap<String, usize>, id: String) -> String {
    let n = seen.entry(id.clone()).or_insert(0);
    let derived = if *n == 0 { id } else { format!("{id}-{n}") };
    *n += 1;
    derived
}

// ---------------------------------------------------------------------------
// Source parsing
// ---------------------------------------------------------------------------

/// One doc-comment line: its text, whether it is inner (`//!`) or outer
/// (`///`), and the 1-based source line it came from.
struct DocLine<'a> {
    text: &'a str,
    inner: bool,
    /// Column 0, i.e. the file's own module rather than something nested.
    top_level: bool,
    line_no: usize,
}

/// Split a source line into its doc-comment payload, if it is one.
///
/// `////` is a plain comment to rustdoc, not a doc comment, so it is excluded.
fn doc_payload(line: &str) -> Option<(&str, bool)> {
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed.strip_prefix("//!") {
        Some((rest.strip_prefix(' ').unwrap_or(rest), true))
    } else if let Some(rest) = trimmed.strip_prefix("///") {
        if rest.starts_with('/') {
            None
        } else {
            Some((rest.strip_prefix(' ').unwrap_or(rest), false))
        }
    } else {
        None
    }
}

/// Does this line open an inline module at column 0 (`mod x {`, `pub mod x {`)?
///
/// `mod x;` does not — its docs live in another file, which this guard reads on
/// its own terms.
fn opens_inline_module(line: &str) -> bool {
    if line.starts_with([' ', '\t']) {
        return false;
    }
    let mut rest = line;
    for prefix in ["pub(crate) ", "pub(super) ", "pub(in ", "pub "] {
        if let Some(r) = rest.strip_prefix(prefix) {
            rest = r;
            break;
        }
    }
    rest.strip_prefix("mod ")
        .is_some_and(|r| r.trim_end().ends_with('{'))
}

/// An anchor-bearing link found in a doc comment.
struct AnchorLink {
    anchor: String,
    /// `true` for `](self#x)`, `false` for the bare `](#x)` form.
    qualified: bool,
    inner: bool,
    line_no: usize,
}

/// Blank out inline code spans, so a link written *about* links stays prose.
///
/// A backtick run shields until a run of the same length closes it; an
/// unterminated run is literal and shields nothing, matching the markdown spec
/// and `rto_render::docs`'s own renderer. Without this, documenting the very
/// pattern this file checks would trip the check.
fn without_code_spans(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('`') {
        let ticks = rest[open..].len() - rest[open..].trim_start_matches('`').len();
        let fence = &rest[open..open + ticks];
        let body = &rest[open + ticks..];
        let Some(close) = body.find(fence) else {
            // Unterminated: the rest of the line is literal text.
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..open]);
        rest = &body[close + ticks..];
    }
    out.push_str(rest);
    out
}

/// Pull every `self#anchor` / `#anchor` link target out of one doc-text line.
///
/// Covers both the inline form and the whole-line reference-definition form.
fn anchor_links(doc: &DocLine<'_>) -> Vec<AnchorLink> {
    let mut found = Vec::new();
    let stripped = without_code_spans(doc.text);
    let line_initial = stripped.trim_start().starts_with('[');
    let mut rest = stripped.as_str();
    while let Some(close) = rest.find(']') {
        let after = &rest[close + 1..];
        let target = if let Some(t) = after.strip_prefix('(') {
            t.split(')').next().unwrap_or("")
        } else if let Some(t) = after.strip_prefix(':') {
            // A reference definition is the whole line, so its target runs to
            // the end; mid-sentence `]:` is prose and never a link.
            if !line_initial {
                rest = after;
                continue;
            }
            t.trim()
        } else {
            rest = after;
            continue;
        };
        let qualified = target.starts_with("self#");
        let anchor = if qualified {
            Some(&target["self#".len()..])
        } else {
            target.strip_prefix('#')
        };
        if let Some(anchor) = anchor.filter(|a| !a.is_empty()) {
            found.push(AnchorLink {
                anchor: anchor.to_string(),
                qualified,
                inner: doc.inner,
                line_no: doc.line_no,
            });
        }
        rest = after;
    }
    found
}

/// Every doc-comment line in a file, with fenced code blocks removed.
///
/// Fences matter: a doctest's hidden lines start with `# `, and reading those
/// as headings would invent ids that rustdoc never generates.
fn doc_lines(src: &str) -> Vec<DocLine<'_>> {
    let mut out = Vec::new();
    let mut in_fence = false;
    for (idx, line) in src.lines().enumerate() {
        let Some((text, inner)) = doc_payload(line) else {
            continue;
        };
        if text.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        out.push(DocLine {
            text,
            inner,
            top_level: !line.starts_with([' ', '\t']),
            line_no: idx + 1,
        });
    }
    out
}

// ---------------------------------------------------------------------------
// The check
// ---------------------------------------------------------------------------

/// What one file contributed: its problems, and how many anchor links it held.
struct FileReport {
    problems: Vec<String>,
    links: usize,
}

/// Check one source file's module-scoped anchor links against its own headings.
fn check_file(rel: &str, src: &str) -> FileReport {
    // Links at or below the first column-0 inline module resolve `self` to that
    // module, not to the file's. Rather than replicate module resolution, this
    // guard refuses them loudly — a wrong answer that announces itself, instead
    // of the silent false pass the whole issue is about.
    let nested_from = src
        .lines()
        .position(opens_inline_module)
        .map_or(usize::MAX, |i| i + 1);

    let docs = doc_lines(src);

    // The module page's headings come from its own `//!` docs and nothing else.
    let mut seen = HashMap::new();
    let mut headings: Vec<String> = Vec::new();
    for doc in docs.iter().filter(|d| d.inner && d.top_level) {
        let body = doc.text.trim_start();
        let hashes = body.len() - body.trim_start_matches('#').len();
        if (1..=6).contains(&hashes) && body[hashes..].starts_with(' ') {
            let title = body[hashes..].trim().trim_end_matches('#').trim_end();
            headings.push(derive_id(&mut seen, slugify(&heading_text(title))));
        }
    }

    let mut problems = Vec::new();
    let mut links = 0;
    for link in docs.iter().flat_map(anchor_links) {
        links += 1;
        let at = format!("{rel}:{}", link.line_no);
        if link.line_no >= nested_from {
            problems.push(format!(
                "{at}: `#{}` sits at or below this file's first inline `mod`, \
                 where `self` no longer means the file's module. This guard \
                 does not resolve nested modules; move the link above the \
                 module, or teach `check_file` to track module scope.",
                link.anchor
            ));
            continue;
        }
        if !link.qualified && !link.inner {
            problems.push(format!(
                "{at}: `](#{})` on an item's own docs points at *that item's* \
                 page, not the module's. Write `](self#{})`.",
                link.anchor, link.anchor
            ));
            continue;
        }
        if !headings.contains(&link.anchor) {
            problems.push(format!(
                "{at}: `#{}` names no heading in this module's `//!` docs. \
                 Headings present: {}. A renamed heading does not warn — \
                 rustdoc checks an intra-doc link's path and never its \
                 fragment — so fix the pointer or restore the heading.",
                link.anchor,
                if headings.is_empty() {
                    "(none)".to_string()
                } else {
                    headings
                        .iter()
                        .map(|h| format!("`#{h}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            ));
        }
    }
    FileReport { problems, links }
}

// ---------------------------------------------------------------------------
// Walking the workspace
// ---------------------------------------------------------------------------

/// The tree to scan: the whole workspace when this is a source checkout, else
/// this crate's own `src/` — which is where every pointer lives today, and is
/// present even in a packaged crate. There is deliberately no "skip when the
/// tree is missing" branch: an empty scan is a green test that checked nothing.
fn scan_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let crates = manifest
        .ancestors()
        .nth(2)
        .expect("repo root is two levels above crates/roteiro")
        .join("crates");
    if crates.is_dir() {
        crates
    } else {
        manifest.join("src")
    }
}

/// Every `.rs` file under `dir`, recursively.
fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn doc_anchor_fragments_resolve_to_real_headings() {
    let root = scan_root();
    let mut files = Vec::new();
    rust_sources(&root, &mut files);
    files.sort();

    assert!(
        !files.is_empty(),
        "scanned no Rust sources under {}: this guard would pass without \
         checking anything",
        root.display()
    );

    let mut problems = Vec::new();
    let mut links = 0;
    for path in &files {
        let Ok(src) = std::fs::read_to_string(path) else {
            continue;
        };
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .display()
            .to_string();
        let report = check_file(&rel, &src);
        links += report.links;
        problems.extend(report.problems);
    }

    assert!(
        links > 0,
        "found no `](self#…)` links in {} files under {}. Either the pattern \
         is genuinely gone from the workspace — in which case delete this \
         guard deliberately — or `anchor_links` has stopped matching and this \
         test is decorative.",
        files.len(),
        root.display()
    );

    assert!(
        problems.is_empty(),
        "{} dead or mis-scoped doc anchor(s) across {links} link(s) in {} \
         files:\n\n{}\n",
        problems.len(),
        files.len(),
        problems.join("\n\n")
    );
}

#[test]
fn slugify_matches_rustdocs_rule() {
    // Verified against rustdoc's generated ids for this workspace's headings.
    assert_eq!(slugify(&heading_text("Paging")), "paging");
    assert_eq!(
        slugify(&heading_text("What this checks")),
        "what-this-checks"
    );
    assert_eq!(
        slugify(&heading_text("`limit` and `offset`")),
        "limit-and-offset"
    );
    assert_eq!(slugify(&heading_text("Why *this* costs")), "why-this-costs");
    assert_eq!(
        slugify(&heading_text("ADR-0008: workspaces")),
        "adr-0008-workspaces"
    );
    assert_eq!(
        slugify(&heading_text("See [paging](self#paging)")),
        "see-paging"
    );
    // Non-ASCII is kept verbatim — rustdoc lowercases only ASCII.
    assert_eq!(slugify(&heading_text("Ünicode kept")), "Ünicode-kept");
}

#[test]
fn duplicate_headings_get_rustdocs_numeric_suffix() {
    let mut seen = HashMap::new();
    assert_eq!(derive_id(&mut seen, "paging".into()), "paging");
    assert_eq!(derive_id(&mut seen, "paging".into()), "paging-1");
    assert_eq!(derive_id(&mut seen, "paging".into()), "paging-2");
}

#[test]
fn a_renamed_heading_is_caught() {
    // The exact failure #397 describes, in miniature: the pointer is untouched
    // and the heading moved out from under it.
    let intact = "//! # Paging\n//!\n//! text\n\n/// see [p](self#paging)\npub fn f() {}\n";
    assert!(check_file("x.rs", intact).problems.is_empty());

    let renamed = intact.replace("# Paging", "# Pagination");
    let report = check_file("x.rs", &renamed);
    assert_eq!(report.links, 1);
    assert!(
        report.problems[0].contains("names no heading"),
        "{:?}",
        report.problems
    );
}

#[test]
fn bare_fragments_and_nested_modules_are_reported() {
    // Row 3 of #397: a bare `#anchor` on an item points at the item's page.
    let bare = "//! # Paging\n\n/// see [p](#paging)\npub fn f() {}\n";
    assert!(check_file("x.rs", bare).problems[0].contains("Write `](self#paging)`"));

    // The same fragment in the module's *own* docs is fine — there, the item's
    // page and the module's page are the same page.
    let inner = "//! # Paging\n//!\n//! see [p](#paging)\n";
    assert!(check_file("x.rs", inner).problems.is_empty());

    // `self` inside an inline module is a scope this guard does not resolve.
    let nested =
        "//! # Paging\n\nmod inner {\n    /// see [p](self#paging)\n    pub fn f() {}\n}\n";
    assert!(check_file("x.rs", nested).problems[0].contains("inline `mod`"));
}

#[test]
fn doctest_hidden_lines_are_not_headings() {
    // `# ` inside a fence hides a doctest line; it is not an H1.
    let src = "//! ```\n//! # let x = 1;\n//! ```\n//!\n//! # Paging\n\n/// [p](self#paging)\npub fn f() {}\n";
    let report = check_file("x.rs", src);
    assert!(report.problems.is_empty(), "{:?}", report.problems);

    // And a link inside a fence is code, not a link.
    let fenced = "//! ```\n//! let s = \"[p](self#nope)\";\n//! ```\n";
    assert_eq!(check_file("x.rs", fenced).links, 0);
}

//! Small text helpers shared across the crates that build and render the graph.

/// A URL-safe slug: lowercase, non-alphanumeric runs collapsed to a single `-`,
/// trimmed of leading/trailing `-`.
///
/// # Why this lives here rather than in either caller
///
/// A document's `## ` heading becomes two things that have to agree: a section
/// **node key** in the authored layer (`rto_spec` builds `adr:0001#design`,
/// `site:modes#offline-mode`) and the **`id` attribute** of the rendered heading
/// (`rto_render` emits `<h2 id="design">`). A link into a section resolves
/// through one and lands through the other, so the moment the two slugifiers
/// disagree — on a `&`, on a trailing `?`, on a run of punctuation — the graph
/// says the section exists and the browser scrolls nowhere.
///
/// `rto_render` cannot borrow `rto_spec`'s copy: it depends on `rto_spec` only
/// under the `mcp` feature, so a default render build would have no slugifier at
/// all. Both depend on this crate unconditionally, so this is the one place the
/// rule can sit and be the only copy of itself.
#[must_use]
pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_owned()
}

#[cfg(test)]
mod tests {
    use super::slugify;

    #[test]
    fn collapses_punctuation_and_trims() {
        assert_eq!(slugify("Install & build"), "install-build");
        assert_eq!(
            slugify("The five ways to run it"),
            "the-five-ways-to-run-it"
        );
        assert_eq!(slugify("  §2 — Context!  "), "2-context");
        assert_eq!(
            slugify("Cross-repo: a hub and its spokes"),
            "cross-repo-a-hub-and-its-spokes"
        );
        assert_eq!(slugify("!!!"), "");
    }
}

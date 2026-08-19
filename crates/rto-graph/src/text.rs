//! Small text helpers shared across the crates that build and render the graph.

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

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

/// The Markdown dialect this project reads and renders with — the one answer to
/// "what does this source mean", for every surface that asks.
///
/// # Anyone parsing Markdown in this workspace must use this
///
/// Not as a convention: a *different* option set is a different language. With
/// `ENABLE_HEADING_ATTRIBUTES` off, `{#modes}` is four literal characters of
/// heading text rather than an attribute block, so a heading's text — and the
/// slug, node title and `id` derived from it — changes meaning with the flag.
/// Two parsers with two option sets do not fail; they quietly disagree about
/// where a heading's text ends, which is the defect #469 was.
///
/// That makes this the foundation [`first_h1`] and [`heading_text`] stand on,
/// and it is why it is `pub`: `rto_render` parses the same documents to render
/// them (the document body, every heading's `id`, the page `<title>`), and those
/// answers have to be the same answers. It cannot borrow the rule from
/// `rto_spec` — it depends on that crate only under `mcp` — so, exactly like
/// [`slugify`], this crate is the one place the dialect can sit and be the only
/// copy of itself.
///
/// Strikethrough and tables are here for that reason and no other: a
/// `~~retracted~~` heading has to reduce to the same text on every surface, not
/// because a heading contains a table.
#[must_use]
pub fn markdown_dialect() -> Options {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_HEADING_ATTRIBUTES);
    opts
}

/// The visible text of the first `# ` heading in `md`, or `None` when there is
/// none — including a `#` that opens an empty heading, which names nothing and
/// so defers to whatever fallback the caller has (a slug, a file stem, `ADR-nnnn`).
///
/// # Why this lives here rather than in either caller
///
/// The same argument as [`slugify`] directly above, one step earlier in the
/// pipeline: a document's H1 becomes both a **node title** in the authored layer
/// (`rto_spec` puts it on `site:`/`blueprint:` nodes, which is what `roteiro
/// search` prints) and the **`<title>`/`<h1>`** of the rendered page
/// (`rto_render`). Neither crate can borrow the other's copy — `rto_render`
/// depends on `rto_spec` only under `mcp` — so this is the one place the rule can
/// sit and be the only copy of itself.
///
/// **Read with the parser, never scanned.** A line scan cannot know that `#`
/// inside a fenced block is a code sample rather than a heading, that
/// `Title` over `===` *is* an H1, or where an attribute block ends — and it is
/// the last of those that put a literal `{#modes}` into graph node titles (#469).
#[must_use]
pub fn first_h1(md: &str) -> Option<String> {
    let mut text: Option<String> = None;
    for event in Parser::new_ext(md, markdown_dialect()) {
        match event {
            Event::Start(Tag::Heading {
                level: HeadingLevel::H1,
                ..
            }) => text = Some(String::new()),
            // Only accumulates once an H1 has opened; a code span is part of the
            // heading's text, exactly as it is for the heading's id.
            Event::Text(t) | Event::Code(t) => {
                if let Some(text) = text.as_mut() {
                    text.push_str(&t);
                }
            }
            Event::End(TagEnd::Heading(HeadingLevel::H1)) => break,
            _ => {}
        }
    }
    text.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty())
}

/// The visible text of a heading whose Markdown *source content* is `source` —
/// the part after the `## `, with the markup that produced it removed.
///
/// The same rule as [`first_h1`] and literally the same code path: `source` is
/// read back as a heading, so an attribute block, a code span or an inline link
/// reduces here exactly as it does for the document's H1 and for the heading
/// `rto_render` emits. Callers pass one line (a `## ` heading cannot span lines);
/// anything after a newline in `source` is a separate block and is ignored.
#[must_use]
pub fn heading_text(source: &str) -> String {
    first_h1(&format!("# {source}")).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{first_h1, heading_text, slugify};

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

    #[test]
    fn an_attribute_block_is_markup_not_part_of_the_title() {
        // The defect behind #469: a line scan yields `… {#modes}`, and that
        // string became a `site:` node title — invisible in the rendered page,
        // present in everything that reads the graph.
        let title = first_h1("# The five ways to run it {#modes}\n").expect("an h1");
        assert_eq!(title, "The five ways to run it");
        assert!(
            !title.contains("{#"),
            "an attribute block must not survive into a title: {title:?}"
        );
    }

    #[test]
    fn both_entry_points_agree_on_where_a_heading_ends() {
        // Not a literal assertion, deliberately. `# Sets like {#1, #2}` is a real
        // ambiguity and the *dialect* decides it — so what is worth pinning is
        // that the document rule and the `## `-heading rule cannot decide it
        // differently, whichever way the parser goes.
        for source in [
            "The five ways to run it {#modes}",
            "Sets like {#1, #2}",
            "The `--json` flag",
            "See [the docs](x.md)",
            "A ~~retracted~~ claim",
            "Install & build",
            "",
        ] {
            assert_eq!(
                first_h1(&format!("# {source}")).unwrap_or_default(),
                heading_text(source),
                "the two entry points disagreed about {source:?}"
            );
        }
    }

    #[test]
    fn a_heading_inside_a_fence_is_a_code_sample() {
        // A line scan cannot tell these apart; it is why a document *about*
        // blueprints could classify itself as one.
        let md = "```\n# Widget — Technical Implementation Plan\n```\n\n# Real title\n";
        assert_eq!(first_h1(md).as_deref(), Some("Real title"));
        assert_eq!(
            first_h1("```\n# Fenced only\n```\n"),
            None,
            "a fenced `#` is not a heading at all"
        );
    }

    #[test]
    fn a_setext_heading_is_an_h1() {
        assert_eq!(
            first_h1("Underlined title\n===\n").as_deref(),
            Some("Underlined title")
        );
    }

    #[test]
    fn an_empty_heading_names_nothing() {
        // Defers to the caller's fallback (a slug, a file stem, `ADR-nnnn`)
        // rather than titling a node with the empty string.
        assert_eq!(first_h1("#\n\n# Second\n"), None);
        assert_eq!(heading_text(""), "");
    }

    #[test]
    fn heading_text_feeds_slugify_the_text_a_reader_sees() {
        // The two rules in this module are one pipeline: the section key is the
        // slug of the visible text, so markup must be gone before slugify runs.
        assert_eq!(
            slugify(&heading_text("1 · Offline mode — the default {#offline}")),
            "1-offline-mode-the-default"
        );
    }
}

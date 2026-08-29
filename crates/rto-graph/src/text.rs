//! Small text helpers shared across the crates that build and render the graph.

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use std::collections::BTreeMap;

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

/// The `id` a heading claims, from its **explicit** `{#id}` attribute when the
/// author wrote one and its visible text otherwise.
///
/// # One rule, two callers — which is the whole point
///
/// `rto_render` puts this on the rendered heading as its `id` attribute, and
/// `rto_spec` builds the section's node key from it for **all three** document
/// classes it parses — ADRs, blueprints and site pages — so a `[[doc#section]]`
/// link resolves in the graph *and* lands in the browser.
///
/// The three are named rather than summarised because "universally" is the kind
/// of claim that goes quietly stale: #524's first fix reached site pages only,
/// and ADRs and blueprints kept slugifying the heading text, so an author who
/// wrote `{#id}` in an ADR would have got the same bug in a document class the
/// fix had not reached. Extending it moved **no** existing key — none of the
/// repository's 233 section keys changed — because no ADR or blueprint declares
/// an explicit id today. It removes the trap rather than repairing damage.
///
/// Both files already claimed that agreement in prose; before #524 the code only
/// had it on one of two branches. The renderer honoured an explicit `{#id}` and the graph slugified
/// the heading text regardless, so
///
/// ```text
/// ## 1 · Offline mode — the default {#offline}
///
///   graph  site:modes#1-offline-mode-the-default
///   html   id="offline"
/// ```
///
/// — **correct on the surface everyone looks at, wrong in the one tools read.**
/// Five of this repository's site headings diverged; the other eight agreed only
/// because their explicit id happened to equal the slug of their own text.
///
/// The explicit id is taken **verbatim**, not slugified: the author wrote an
/// address, and re-slugifying it would silently answer a different one — the
/// very move that produced the divergence.
///
/// # What this deliberately does not decide
///
/// It returns empty for a heading with no explicit id and no text that slugifies
/// to anything (`## ###`), and it does not de-duplicate. Both are **document**
/// questions — a heading's position, and whether an earlier heading already took
/// the id — and this sees one heading.
///
/// They are answered one level up, by [`headings`], which reads the whole
/// document. They used to be answered in `rto_render::docs` instead, on the
/// argument that only the renderer emits elements and so only the renderer can
/// have two of them share an `id`. That argument was wrong in its consequence:
/// the renderer suffixed the second `{#same}` to `same-2` and the graph upserted
/// one section over the other, so the surviving node named a place the page
/// addressed as something else (#629). A rule only one side applies is a
/// divergence with extra steps.
#[must_use]
pub fn heading_id_from(explicit: Option<&str>, text: &str) -> String {
    explicit
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map_or_else(|| slugify(text), ToOwned::to_owned)
}

/// [`heading_id_from`] for a heading whose Markdown **source content** is
/// `source` — the part after the `## `.
///
/// Parsed rather than scanned, by the same parser and dialect the renderer uses,
/// so "what id will this heading get" is answered once and identically on both
/// sides. A line scan would have to re-implement attribute-block parsing, which
/// is how a third rule gets born.
///
/// # The one heading it cannot answer for
///
/// The parse is of `# {source}` **alone**, so anything a heading inherits from
/// the rest of its document is invisible here. In practice that is one
/// construct: a **reference-style link**, whose definition lives elsewhere in the
/// file.
///
/// ```text
/// [plan]: plan.md
///
/// ## See [the plan][plan]
/// ```
///
/// The renderer parses the whole document, resolves the definition, and anchors
/// the heading at `see-the-plan`. This function sees no definition, so
/// pulldown-cmark keeps `[the plan][plan]` as literal text and it returns
/// `see-the-plan-plan`.
///
/// Left as a stated limit rather than fixed, because fixing it means threading
/// every document's reference definitions through this signature and giving each
/// of the three line-scanning parsers a pre-pass to collect them — a large change
/// against **zero** occurrences: the repository contains no reference-style link
/// definitions at all, in any document, and no heading anywhere uses the syntax.
///
/// It is not unguarded, either. `heading_anchor_agreement` renders every site
/// page in full and compares the emitted `id` attributes against the graph's
/// section keys, so a real instance in a site page fails that test rather than
/// diverging quietly. See also the blockquote divergence (#621), recorded the
/// same way.
#[must_use]
pub fn heading_id(source: &str) -> String {
    let md = format!("# {source}");
    let (mut explicit, mut text) = (None, String::new());
    let mut open = false;
    for event in Parser::new_ext(&md, markdown_dialect()) {
        match event {
            Event::Start(Tag::Heading { id, .. }) => {
                explicit = id.map(|i| i.to_string());
                open = true;
            }
            // A code span is part of the heading's text, exactly as it is for
            // [`first_h1`] and for the heading `rto_render` emits.
            Event::Text(t) | Event::Code(t) if open => text.push_str(&t),
            Event::End(TagEnd::Heading(_)) => break,
            _ => {}
        }
    }
    heading_id_from(explicit.as_deref(), text.trim())
}

#[cfg(test)]
mod tests {
    use super::{first_h1, heading_id, heading_text, headings, slugify};

    /// The three classes #621 measured, each a heading to a parser and invisible
    /// to a `strip_prefix("## ")` scan — plus the mirror error, a `## ` inside a
    /// fence, which a scan counts and a parser knows is code.
    #[test]
    fn a_heading_is_more_than_a_line_starting_with_two_hashes() {
        let md = "# Title\n\n\
                  > ## Quoted\n\n\
                  \u{20}\u{20}## Indented\n\n\
                  Setext\n---\n\n\
                  ```\n## Not a heading\n```\n\n\
                  ## Plain\n";
        let hs = headings(md);
        let ids: Vec<&str> = hs.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(
            ids,
            ["title", "quoted", "indented", "setext", "plain"],
            "blockquoted, indented and setext headings are headings; a fenced \
             `## ` is not"
        );
    }

    /// Offsets are each heading's own start, in document order — which is all a
    /// caller needs to attribute a later byte to the heading it falls under.
    #[test]
    fn heading_offsets_ascend_and_point_at_the_heading_not_its_container() {
        let md = "## First\n\ntext\n\n> ## Quoted\n\nmore\n";
        let hs = headings(md);
        assert_eq!(hs.len(), 2);
        assert!(hs[0].start < hs[1].start, "document order: {hs:?}");
        // The heading's own start, past the blockquote marker. Asserted because I
        // documented the opposite first and this test is what corrected it: a
        // caller slicing from here would otherwise get `> ## Quoted`.
        assert!(
            md[hs[1].start..].starts_with("## Quoted"),
            "offset points at the heading, not its container: {:?}",
            &md[hs[1].start..]
        );
    }

    /// A second heading claiming an id the first took is suffixed, and the
    /// numbering runs over **every** level (#629).
    ///
    /// The all-levels part is the half that cannot be reproduced by a caller that
    /// keeps only `##`: `# Same` before `## Same` puts the h2 at `same-2`, so a
    /// caller filtering to `##` after this ran agrees with the renderer and one
    /// deduplicating within its own subset does not. That asymmetry — the
    /// renderer counting all levels, `rto_spec` recording one — is precisely why
    /// the rule sits here instead of in either of them.
    #[test]
    fn a_repeated_id_is_suffixed_and_the_count_spans_every_level() {
        let ids = |md: &str| -> Vec<String> { headings(md).into_iter().map(|h| h.id).collect() };

        assert_eq!(ids("## A {#same}\n\n## B {#same}\n"), ["same", "same-2"]);
        assert_eq!(
            ids("## Dup\n\n## Dup\n\n## Dup\n"),
            ["dup", "dup-2", "dup-3"]
        );
        // Across levels, in both directions: an h1 or an h3 takes the bare id
        // just as an h2 would, and the h2 that follows is suffixed.
        assert_eq!(ids("# Same\n\n## Same\n"), ["same", "same-2"]);
        assert_eq!(ids("### Same\n\n## Same\n"), ["same", "same-2"]);
        // And a heading a `## ` scan cannot see still consumes its id, so the
        // one that follows is numbered against the page rather than against the
        // subset any caller happens to keep.
        assert_eq!(ids("> ## X\n\n## X\n"), ["x", "x-2"]);
    }

    /// A heading that names nothing is numbered by its position among **all**
    /// headings — the other document-level rule that moved here with the dedup.
    ///
    /// `## ###` slugifies to the empty string, and an empty id is not an address.
    /// `section-2` because the `# Title` above it is heading one.
    #[test]
    fn a_heading_that_names_nothing_falls_back_to_its_position() {
        let hs = headings("# Title\n\n## ###\n\n## Real\n");
        let ids: Vec<&str> = hs.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, ["title", "section-2", "real"]);
        // Position first, then uniqueness: two unnameable headings get distinct
        // positions rather than one name and a suffix.
        let hs = headings("## ###\n\n## ###\n");
        let ids: Vec<&str> = hs.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, ["section-1", "section-2"]);
    }

    /// Level and text come back too, and an explicit id still wins over the slug.
    #[test]
    fn a_heading_carries_its_level_text_and_declared_id() {
        let hs = headings("### Design *notes* {#arch}\n");
        assert_eq!(hs.len(), 1);
        assert_eq!(hs[0].level, 3);
        assert_eq!(hs[0].text, "Design notes", "markup reduced");
        assert_eq!(hs[0].id, "arch", "the declared anchor, not the slug");
    }

    /// The single construct the isolated parse cannot resolve, pinned so the
    /// limit is a recorded value rather than a surprise. See [`heading_id`]'s
    /// docs: the renderer, parsing the whole document, would anchor the same
    /// heading at `see-the-plan`.
    ///
    /// Asserted as the *divergent* value on purpose. Writing the aspirational
    /// `see-the-plan` here and marking it `#[ignore]` would leave the real
    /// behaviour untested, and the next person to touch this would have no way
    /// to tell a deliberate limit from an undiscovered bug.
    #[test]
    fn a_reference_style_link_cannot_resolve_without_its_document() {
        assert_eq!(heading_id("See [the plan][plan]"), "see-the-plan-plan");
        // Inline and collapsed forms need nothing from the document, so they
        // agree with the renderer already — the gap really is this narrow.
        assert_eq!(heading_id("See [the plan](plan.md)"), "see-the-plan");
        assert_eq!(heading_id("See [the plan]"), "see-the-plan");
    }

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

/// One heading found by **parsing** a document, with the byte offset at which it
/// begins.
///
/// See [`headings`] for why the offset is the useful part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heading {
    /// Heading level: 1 for `#`, 2 for `##`, and so on.
    pub level: u8,
    /// The id this heading **gets**, which is the id it can be linked by.
    ///
    /// [`heading_id_from`] answers what it claims — its explicit `{#id}` when the
    /// author wrote one, the slug of its text otherwise. This is that answer after
    /// the two questions only the document can settle: an unnameable heading falls
    /// back to its position, and a claim an earlier heading already took is
    /// suffixed. See [`headings`].
    pub id: String,
    /// The heading's visible text, with the markup that produced it removed.
    pub text: String,
    /// Byte offset into the source where this heading begins.
    ///
    /// The heading's own start, **not** its container's: for `> ## Quoted` it
    /// points at the `#`, past the blockquote marker. Callers use it to decide
    /// which heading a later byte falls under, which is a comparison rather than
    /// a slice, so what precedes it on the line does not concern them.
    pub start: usize,
}

/// Every heading in `md`, in document order, read with the shared dialect.
///
/// # Why parse rather than scan for `## `
///
/// Because a heading is not a line that starts with `## `. It is also
/// `> ## Quoted` inside a blockquote, `  ## Indented` under three spaces, and
/// `Title` over `---`. All three are headings to a parser and to the renderer,
/// which duly emits an addressable `<h2 id="…">` for each — while a
/// `strip_prefix("## ")` scan sees none of them, so the graph records no section
/// and a link naming that place cannot resolve even though the place exists
/// (#621). A `## ` inside a fenced block is the mirror error: a scan counts it,
/// a parser knows it is code.
///
/// # Why an offset rather than a line number
///
/// The callers that need this are attributing *other* things — wiki-links,
/// section body text — to the heading they fall under. Given the offsets, that is
/// a comparison against the next heading's start, and it works identically for a
/// heading the caller could not have found by scanning.
///
/// # Why the id is settled here and not per heading
///
/// Two of the three questions in "what is this heading's id" need the whole
/// document, so [`heading_id_from`] cannot answer them and this is the first
/// place that can:
///
/// - a heading that names nothing (`## ###`, no explicit id) falls back to its
///   **position**, `section-N`, 1-based over every heading in the document;
/// - a heading claiming an id an earlier heading already took is **suffixed**,
///   `same` then `same-2` then `same-3`.
///
/// Both used to live in `rto_render::docs::heading_ids`, which is where #629
/// found them: the renderer deduplicated and the graph did not, so
/// `## A {#same}` / `## B {#same}` rendered as two addressable anchors and
/// upserted into **one** graph node — section A gone, and the `same-2` anchor
/// addressable by nothing.
///
/// # It counts every level, and that is the load-bearing part
///
/// `# Same` followed by `## Same` renders as `same` / `same-2`. A caller that
/// wants only `##` sections — [`rto_spec`](https://docs.rs/rto-spec) does —
/// must filter **after** this ran, not dedupe within its own subset, or the h2
/// gets keyed `same` while the page addresses it as `same-2`. That is the
/// divergence a dedup local to either side reintroduces, and the reason this
/// numbering is over all headings rather than over the ones any one caller keeps.
#[must_use]
pub fn headings(md: &str) -> Vec<Heading> {
    let mut out: Vec<Heading> = Vec::new();
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut open: Option<(u8, Option<String>, String, usize)> = None;
    for (event, range) in Parser::new_ext(md, markdown_dialect()).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, id, .. }) => {
                let level = match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    HeadingLevel::H3 => 3,
                    HeadingLevel::H4 => 4,
                    HeadingLevel::H5 => 5,
                    HeadingLevel::H6 => 6,
                };
                open = Some((level, id.map(|i| i.to_string()), String::new(), range.start));
            }
            // A code span is part of a heading's text, exactly as it is for the
            // heading's id — the same rule `first_h1` applies.
            Event::Text(t) | Event::Code(t) => {
                if let Some((_, _, text, _)) = open.as_mut() {
                    text.push_str(&t);
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((level, explicit, text, start)) = open.take() {
                    let text = text.trim().to_owned();
                    let claimed = heading_id_from(explicit.as_deref(), &text);
                    // Position first, then uniqueness — in that order, because a
                    // heading that names nothing still has to be given a name
                    // before anything can ask whether the name is taken.
                    let claimed = if claimed.is_empty() {
                        format!("section-{}", out.len() + 1)
                    } else {
                        claimed
                    };
                    let n = seen.entry(claimed.clone()).or_insert(0);
                    *n += 1;
                    let id = if *n == 1 {
                        claimed
                    } else {
                        format!("{claimed}-{n}")
                    };
                    out.push(Heading {
                        level,
                        id,
                        text,
                        start,
                    });
                }
            }
            _ => {}
        }
    }
    out
}

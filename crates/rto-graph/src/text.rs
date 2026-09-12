//! Small text helpers shared across the crates that build and render the graph.

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use std::collections::BTreeMap;
use std::ops::Range;

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

// ---------------------------------------------------------------------------
// Markdown links
// ---------------------------------------------------------------------------

/// Which Markdown syntax a [`MarkdownLink`] was written in.
///
/// Both are links in this project's dialect and both are read by one scanner,
/// which is the point: "find a Markdown link" had five implementations sharing
/// no code, and the kinds were never all found by the same one.
///
/// # Deliberately closed, and not `#[non_exhaustive]`
///
/// `CommonMark` has link forms this does not read — reference links (`[a][b]`)
/// and autolinks (`<https://…>`) — so a fourth variant is imaginable, which is
/// exactly why the set is shut rather than left open. A caller decides what
/// to *do* per kind: [`crate::markdown_links`]' own callers rewrite an inline
/// link's text over the whole link and resolve a wiki-link's target against a
/// node key, and there is no behaviour that is right for a kind nobody has seen.
/// Left open, every one of them grows a wildcard arm and a new kind is silently
/// handled as whichever of these it is least like. Shut, adding one is a major
/// version and a compile error at each place that has to decide — which is the
/// cost that should be paid, and the same argument `rto_faithful::Segment` makes
/// for the same reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LinkKind {
    /// A Roteiro `[[target]]` wiki-link — the authored layer's citation into the
    /// graph, resolved by `rto_spec` against a node key.
    Wiki,
    /// A `CommonMark` `[text](destination)` inline link.
    Inline,
    /// A `CommonMark` `![alt](source)` image.
    ///
    /// Reported rather than skipped, and **distinct from [`Self::Inline`]** so
    /// that it can be: a citation list filters to `Inline` and a `.png` never
    /// reaches it, while a caller reducing markdown to its visible text — the
    /// rustdoc-anchor guard does — keeps the alt text and drops the source. Read
    /// as one kind and either of those is wrong.
    Image,
}

/// Whether a link destination addresses something this repository holds, or
/// somewhere outside it.
///
/// The distinction is one rule because it is asked in three places that each had
/// their own answer: the site renderer deciding whether a destination can be
/// rewritten to a page it serves, the rendered-site link gate deciding whose
/// uptime a href depends on, and — the reason this is `pub` rather than private
/// to either — a citation needing to know whether a locator names a work someone
/// else published.
///
/// # Deliberately closed, and not `#[non_exhaustive]`
///
/// The question is a yes/no one — a destination either names something this
/// repository is expected to contain or it does not — so this is a `bool` that
/// says which way round it is, and a `bool` cannot acquire a third value. Any
/// finer distinction anybody wants later (which scheme, which host, whether the
/// path exists) is a different question with a different answer type, not a
/// variant here; making it one would change what every existing arm means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LinkScope {
    /// A relative or root-relative path, or a bare `#fragment`: something this
    /// repository is expected to contain.
    Internal,
    /// A destination carrying a URL scheme (`https:`, `mailto:`, …) or written
    /// protocol-relative (`//host/…`): somebody else's.
    External,
}

impl LinkScope {
    /// Whether this scope is [`LinkScope::External`].
    #[must_use]
    pub fn is_external(self) -> bool {
        matches!(self, Self::External)
    }
}

/// One Markdown link found on one line by [`markdown_links`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownLink {
    /// Which syntax it was written in.
    pub kind: LinkKind,
    /// Where it points: the inner text for a wiki-link, the destination for an
    /// inline one. Trimmed, and never empty — a link naming nothing is not a
    /// link, which is the reading that cannot invent an edge out of stray
    /// punctuation.
    pub target: String,
    /// What a reader sees. For an inline link that is its bracketed text, which
    /// is the half a citation label needs and which no scanner here used to
    /// keep. For a wiki-link the visible text **is** the target, so this repeats
    /// it rather than being empty: a caller labelling links does not have to
    /// know which kind it is holding.
    pub text: String,
    /// Whether [`target`](Self::target) names something outside this repository.
    /// Always [`LinkScope::Internal`] for a wiki-link, which addresses a graph
    /// node by key and cannot name a URL.
    pub scope: LinkScope,
    /// The byte range the whole link occupies **in the line as given**, so a
    /// caller can rewrite it in place. Code spans are excluded from the scan but
    /// not from this range: a link whose brackets straddle one covers it.
    pub span: Range<usize>,
}

/// Every Markdown link on `line`, of both kinds, in the order they are written.
///
/// # The one scanner
///
/// "Find a Markdown link" was implemented five times across this workspace with
/// no shared code — a `[[…]]` scanner in `rto_spec`, a `pulldown-cmark` event
/// filter in `rto_render`, a hand-rolled target reader in the OKF bundle reader,
/// and two more in the test suite. This is that rule, once. The failure mode is
/// not that one of them is wrong: it is that they quietly disagree, which is
/// exactly the defect two Markdown *walkers* produced in #790 and which
/// `docs_are_canonical.rs` records verbatim.
///
/// It sits beside [`slugify`] and [`heading_text`] for the reason those do:
/// `rto_spec` and `rto_render` both depend on this crate unconditionally and on
/// each other only under a feature, so this is the one place the rule can be the
/// only copy of itself.
///
/// # What it does and does not read
///
/// **One line.** Every caller scanning a document already tracks its own fenced
/// code state and attributes each link to the section enclosing it, so a
/// document-level scan would answer a question none of them asked and would take
/// the fence rule away from the four scanners that disagree about it (see
/// [`is_code_fence`]).
///
/// **Inline code spans are not scanned**, so a `` `[[path#Symbol]]` `` or a
/// `` `[text](x)` `` written as a documentation example is not a link. That is
/// [`strip_code_spans`]' rule, and it is why this is not `pulldown-cmark`: a
/// destination is read here only where the source closes it, so a malformed line
/// yields no link rather than whatever a recovering parser makes of it.
///
/// A backslash escapes the delimiter after it, so `\[not a link](x)` is prose.
/// Escapes are **not** processed inside `[[…]]`, which is a Roteiro token rather
/// than `CommonMark` syntax and has never had them.
///
/// **A `[[…]]` inside an image's alt text is still reported**, so
/// `![alt [[docs/x.md]]](i.png)` yields an [`LinkKind::Image`] *and* a
/// [`LinkKind::Wiki`]. That is not an oversight and cannot be tidied here: a
/// `[[…]]` is a Roteiro token found anywhere on the line, the scanner this
/// replaced had no concept of images, and `roteiro check` counts what that
/// scanner found. Suppressing it would move the gate's number — see
/// `markdown_links_parity.rs`, which holds this function to that scanner over
/// the whole tree. The image rule applies to the `[…](…)` syntax it is part of.
#[must_use]
pub fn markdown_links(line: &str) -> Vec<MarkdownLink> {
    let (stripped, map) = strip_and_map(line);
    let wiki = wiki_spans(&stripped);
    let mut out: Vec<MarkdownLink> = wiki
        .iter()
        .map(|(range, target)| MarkdownLink {
            kind: LinkKind::Wiki,
            target: target.clone(),
            text: target.clone(),
            scope: LinkScope::Internal,
            span: map.start(range.start)..map.end(range.end),
        })
        .collect();
    out.extend(inline_spans(&stripped, &wiki).into_iter().map(
        |(kind, range, text, destination)| MarkdownLink {
            scope: link_scope(&destination),
            kind,
            target: destination,
            // Read back out of the **line**, not the stripped string: a
            // label like ``[the `Foo` type](x.md)`` is scanned with its code
            // span removed, so taking the text from there would cite "the
            // type". The span is excluded from the *scan* because a link
            // inside one is an example; its content is still the label.
            text: line[map.widest(&text)].to_owned(),
            span: map.start(range.start)..map.end(range.end),
        },
    ));
    out.sort_by_key(|l| l.span.start);
    out
}

/// The inner text of every `[[…]]` on `line`, ignoring any inside an inline code
/// span — [`markdown_links`] narrowed to the kind the authored-layer scanners
/// read.
///
/// A convenience over the one scanner rather than a second one: `rto_spec`'s ADR,
/// blueprint, site-page and lat.md parsers all want this exact list, and giving
/// each of them a filter to write is how a sixth implementation starts.
#[must_use]
pub fn wiki_link_targets(line: &str) -> Vec<String> {
    markdown_links(line)
        .into_iter()
        .filter(|l| l.kind == LinkKind::Wiki)
        .map(|l| l.target)
        .collect()
}

/// Whether `destination` names something outside this repository.
///
/// External is "carries a URL scheme" (RFC 3986 §3.1 — an ASCII letter then
/// letters, digits, `+`, `-` or `.`, then `:`) or "is protocol-relative"
/// (`//host/…`). Everything else — a relative path, a root-relative one, a bare
/// `#fragment` — is internal.
///
/// Written as the scheme rule rather than as a list of the four prefixes the two
/// call sites happened to enumerate (`http://`, `https://`, `mailto:`, `//`),
/// because a list is a thing to forget an entry from: `tel:`, `ftp:` and `data:`
/// were external before this and were classified internal by both of them.
#[must_use]
pub fn link_scope(destination: &str) -> LinkScope {
    let destination = destination.trim();
    if destination.starts_with("//") {
        return LinkScope::External;
    }
    let Some((scheme, _)) = destination.split_once(':') else {
        return LinkScope::Internal;
    };
    let mut chars = scheme.chars();
    let valid = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    if valid {
        LinkScope::External
    } else {
        LinkScope::Internal
    }
}

/// Whether `line` opens or closes a fenced code block — a run of three or more
/// backticks or tildes, after leading whitespace.
///
/// # This is the `CommonMark` rule, and three scanners in this workspace do not use it
///
/// `rto_render::docs` recognises both delimiters. `rto_spec`'s ADR, blueprint,
/// site-page and lat.md scanners each carry their own
/// `trim_start().starts_with("```")`, which recognises only backticks — noted at
/// the ADR one as a known narrowing. So a `~~~`-fenced example is code to the
/// renderer and prose to the gate, and a `[[…]]` inside one is a link the gate
/// resolves and the site renders literally.
///
/// That divergence is **not** closed here, because closing it changes what the
/// gate counts and this rule's introduction is not the change that should decide
/// it: no document in this repository fences with `~~~` today, so unifying them
/// moves nothing now and would move the count the first time somebody wrote one.
/// It is written down here instead of staying an accident of five copies.
#[must_use]
pub fn is_code_fence(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("```") || trimmed.starts_with("~~~")
}

/// Return `line` with inline code spans removed, so tokens documented as
/// examples (e.g. `` `[[path#Symbol]]` `` or ``` ``@rto:0001`` ```) are not
/// scanned as real links or annotations.
///
/// Follows the `CommonMark` rule for code spans: a span opens with a run of *n*
/// backticks and closes with the next run of exactly *n* backticks. An opening
/// run with no matching close is literal text and is kept. Non-backtick text is
/// preserved verbatim (backticks are ASCII, so all slice boundaries are valid).
#[must_use]
pub fn strip_code_spans(line: &str) -> String {
    strip_and_map(line).0
}

/// The byte ranges of `line`'s **matched** inline code spans, in order.
///
/// The `CommonMark` rule, in one place: a span opens with a run of *n* backticks
/// and closes with the next run of exactly *n*; an opening run with no matching
/// close is literal text and yields no span. A **backslash-escaped** backtick is
/// literal and opens nothing — without that, `` \` `` paired with a later real
/// opener and swallowed everything between them, which hid a table column from
/// `rto_spec::fmt` and would hide a `[[…]]` link or a `@rto:` annotation from
/// the scanners that read this. The rule is **asymmetric**: escapes do not work
/// *inside* a code span, so a backslash before the closing run is content and
/// the run still closes.
///
/// Separate from [`strip_code_spans`] because removing a span and knowing where
/// one *is* are different questions, and `rto_spec::fmt` needs the second — a
/// table row's `|` inside a code span is content rather than a column boundary.
/// It had its own backtick scanner until #790 found that it entered code mode on
/// an unmatched run and hid the rest of the row, which is exactly the case this
/// rule exists to get right.
#[must_use]
pub fn code_spans(line: &str) -> Vec<(usize, usize)> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'`' || is_escaped(bytes, i) {
            i += 1;
            continue;
        }
        // Measure the opening backtick run.
        let run_start = i;
        while i < bytes.len() && bytes[i] == b'`' {
            i += 1;
        }
        let run = i - run_start;
        // Find a closing run of exactly the same length.
        let mut j = i;
        let mut close = None;
        while j < bytes.len() {
            // **No escape check on the close.** `CommonMark`: backslash
            // escapes do not work inside a code span, so a backslash before the
            // closing run is literal content and the run still closes. Applying
            // the opener's rule here made `` `a\` `` run on to the next
            // backtick and swallow whatever lay between.
            if bytes[j] == b'`' {
                let s = j;
                while j < bytes.len() && bytes[j] == b'`' {
                    j += 1;
                }
                if j - s == run {
                    close = Some(j);
                    break;
                }
            } else {
                j += 1;
            }
        }
        // An unmatched opening run is literal, and the scan continues *after*
        // it rather than restarting inside it.
        if let Some(end) = close {
            out.push((run_start, end));
            i = end;
        }
    }
    out
}

/// The pieces of a line that survived [`code_spans`], and where each of them
/// started in the line itself.
///
/// Scanning happens on the stripped string so that the answer is the one the
/// `[[…]]` scanner has always given — a link whose brackets straddle a code span
/// is found, because removing the span joins its halves. Reporting happens in
/// the caller's coordinates, so a rewriter can act on what it was handed. The
/// two are different, so the stripping keeps a map rather than throwing it away.
struct SpanMap(
    /// `(offset in the stripped string, offset in the source line, length)`, in
    /// order and non-empty.
    Vec<(usize, usize, usize)>,
);

impl SpanMap {
    /// The source-line offset a stripped-string range **starts** at: the chunk
    /// holding that offset, which is the one the first byte of the link is in.
    fn start(&self, at: usize) -> usize {
        self.at(at, |chunk_start| at >= chunk_start)
    }

    /// The source-line offset a stripped-string range **ends** at.
    ///
    /// Deliberately a different lookup from [`Self::start`], and the difference
    /// is the whole reason the two exist. An exclusive end that lands exactly on
    /// a chunk boundary belongs to the chunk it closes, not the one beginning
    /// there — so it maps to the end of the text the link was read from. Taking
    /// the later chunk instead extends the range over the code span that
    /// separates them, which is how `[[…]]`` ::x` came back as a span running
    /// past its own `]]`.
    fn end(&self, at: usize) -> usize {
        self.at(at, |chunk_start| at > chunk_start)
    }

    /// The **widest** source range a stripped-string range came from: every byte
    /// that reduced to it, code spans included.
    ///
    /// The opposite of pairing [`Self::start`] with [`Self::end`], and needed
    /// for the opposite question. A link's *extent* should stop at its own
    /// delimiters; a link's *text* is what a reader sees, and a reader sees the
    /// code span the scan removed. `` [the `Foo` type](x.md) `` scans as the text
    /// `the  type` and reads as `` the `Foo` type ``, and it is the second that
    /// is the citation label.
    ///
    /// Also the only form that cannot invert: an empty stripped range sitting on
    /// a chunk boundary — `` [`x`](y) ``, whose whole label is one code span —
    /// has `start` land after `end`, and this widens to the span instead.
    fn widest(&self, range: &Range<usize>) -> Range<usize> {
        self.end(range.start)..self.start(range.end)
    }

    /// The source offset of `at`, in the last chunk `keep` accepts.
    fn at(&self, at: usize, keep: impl Fn(usize) -> bool) -> usize {
        self.0
            .iter()
            .rev()
            .find(|(start, _, _)| keep(*start))
            .map_or(at, |(start, source, _)| source + (at - start))
    }
}

/// `line` with its code spans removed, and the map back to it.
fn strip_and_map(line: &str) -> (String, SpanMap) {
    let mut stripped = String::with_capacity(line.len());
    let mut chunks = Vec::new();
    let mut at = 0;
    for (start, end) in code_spans(line) {
        if start > at {
            chunks.push((stripped.len(), at, start - at));
            stripped.push_str(&line[at..start]);
        }
        at = end;
    }
    if at < line.len() {
        chunks.push((stripped.len(), at, line.len() - at));
        stripped.push_str(&line[at..]);
    }
    (stripped, SpanMap(chunks))
}

/// Every `[[…]]` on an already-stripped line: its range there, and its trimmed
/// inner text.
///
/// Deliberately the scan `rto_spec::text::scan_wiki_links` ran before this
/// existed, down to the rest-of-line walk: an unclosed `[[` stops the scan
/// rather than being skipped past, and an empty `[[]]` is consumed without being
/// reported. `markdown_links_parity.rs` holds that claim to a frozen copy of the
/// original over every Markdown file in the tree.
fn wiki_spans(stripped: &str) -> Vec<(Range<usize>, String)> {
    let mut out = Vec::new();
    let mut base = 0usize;
    let mut rest = stripped;
    while let Some(open) = rest.find("[[") {
        let after = &rest[open + 2..];
        let Some(close) = after.find("]]") else {
            break;
        };
        let inner = after[..close].trim();
        let start = base + open;
        let end = start + 2 + close + 2;
        if !inner.is_empty() {
            out.push((start..end, inner.to_owned()));
        }
        base = end;
        rest = &after[close + 2..];
    }
    out
}

/// Every `[text](destination)` on an already-stripped line, as `(whole range,
/// text range, destination)`.
///
/// Skips the ranges `wiki` already claimed, so `[[a]]` is one wiki-link rather
/// than also an inline one with a bracket for text. An `![alt](src)` is reported
/// as [`LinkKind::Image`] and its range **starts at the `!`**, which is the half
/// that matters to a caller splicing over it.
fn inline_spans(
    stripped: &str,
    wiki: &[(Range<usize>, String)],
) -> Vec<(LinkKind, Range<usize>, Range<usize>, String)> {
    let bytes = stripped.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] != b'[' {
            i += 1;
            continue;
        }
        if let Some((claimed, _)) = wiki.iter().find(|(r, _)| r.contains(&i)) {
            i = claimed.end;
            continue;
        }
        let Some((text, destination, end)) = inline_at(stripped, i) else {
            i += 1;
            continue;
        };
        let image = i > 0 && bytes[i - 1] == b'!' && !is_escaped(bytes, i - 1);
        let (kind, start) = if image {
            (LinkKind::Image, i - 1)
        } else {
            (LinkKind::Inline, i)
        };
        out.push((kind, start..end, text, destination));
        i = end;
    }
    out
}

/// The inline link opening at `open`, as `(text range, destination, end)`.
///
/// Brackets and parentheses are matched by depth, so `[see [x]](y)` is one link
/// with the text `see [x]` rather than two half-read ones, and a destination may
/// hold the balanced parentheses a Wikipedia URL does. A backslash escapes the
/// delimiter after it. Anything reaching the end of the line unclosed, or a
/// parenthesised part that is not a destination and an optional title, yields no
/// link — the no-recovery reading this scanner exists to keep.
fn inline_at(line: &str, open: usize) -> Option<(Range<usize>, String, usize)> {
    let bytes = line.as_bytes();
    let close = matching(bytes, open, b'[', b']')?;
    if bytes.get(close + 1) != Some(&b'(') {
        return None;
    }
    let dest_end = destination_end(bytes, close + 1)?;
    let destination = destination_of(&line[close + 2..dest_end])?;
    Some((open + 1..close, destination, dest_end + 1))
}

/// The offset of the first **unescaped** `byte` in `s`.
fn unescaped(s: &str, byte: u8) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == byte {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Whether the byte at `at` is escaped by an unbalanced run of backslashes.
fn is_escaped(bytes: &[u8], at: usize) -> bool {
    bytes[..at]
        .iter()
        .rev()
        .take_while(|b| **b == b'\\')
        .count()
        % 2
        == 1
}

/// The offset of the `)` closing the `(` at `from`, counting nested parentheses
/// and **ignoring the ones inside a quoted title**.
///
/// Not [`matching`]: `CommonMark` allows `[t](x.md "a ) b")`, where the first
/// `)` is title text. Counting it closed the link early and left a range ending
/// inside itself, which a caller splicing over the span turns into rubble.
///
/// A quote only opens a title, and a title only begins after whitespace — so the
/// apostrophe in `(https://e.org/a'b)` is part of the destination rather than an
/// unterminated title swallowing the rest of the line.
fn destination_end(bytes: &[u8], from: usize) -> Option<usize> {
    let mut i = from + 1;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    // An angle-bracket destination is **opaque**: `CommonMark` lets it hold the
    // parentheses and quotes that close the link everywhere else, which is the
    // point of writing one. `[t](<https://e.org/a_(b)>)` is a valid link, and
    // counting its `)` against the outer depth rejected it.
    if bytes.get(i) == Some(&b'<') {
        i += 1;
        loop {
            let byte = *bytes.get(i)?;
            i += 1;
            if byte == b'\\' {
                i += 1;
            } else if byte == b'>' {
                break;
            }
        }
    }
    let mut depth = 1usize;
    let mut quote: Option<u8> = None;
    let mut after_space = false;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        match quote {
            Some(open) if bytes[i] == open => quote = None,
            Some(_) => {}
            None => match bytes[i] {
                b'"' | b'\'' if after_space && depth == 1 => quote = Some(bytes[i]),
                b'(' => depth += 1,
                b')' => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                b if b.is_ascii_whitespace() => after_space = true,
                _ => {}
            },
        }
        i += 1;
    }
    None
}

/// The offset of the `shut` byte closing the `open` byte at `from`, counting
/// nesting and honouring backslash escapes.
fn matching(bytes: &[u8], from: usize, open: u8, shut: u8) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = from;
    while i < bytes.len() {
        // A continuation byte of a multi-byte character is never one of the
        // ASCII delimiters below, so stepping over one byte after a backslash
        // cannot mis-read a character — and every offset returned is at an
        // ASCII delimiter, so it is always a char boundary.
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == open {
            depth += 1;
        } else if bytes[i] == shut {
            // A close with nothing open is malformed rather than a link, and
            // saying so here is also what keeps the subtraction from wrapping.
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// The destination out of an inline link's parenthesised part, or `None` when
/// that part is not a destination and an optional title.
///
/// `CommonMark` allows exactly two forms — a bare destination holding no
/// unescaped whitespace, or a `<…>`-wrapped one that may — each optionally
/// followed by a title in `"…"`, `'…'` or `(…)`. **Anything else is not a
/// link**, and saying so is the whole difference between this and a recovering
/// parser: `[t](foo bar)` reads as a citation of `foo` the moment the trailing
/// junk is ignored, and `[t](<unclosed)` as one of `<unclosed`. Neither names
/// anything, and a plausible wrong citation is worse than none.
fn destination_of(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let (destination, rest) = if let Some(rest) = raw.strip_prefix('<') {
        // The angle form must close, on an **unescaped** `>`; `[t](<a\>b>)` is
        // one destination, not one truncated at the escape. An unclosed one is
        // not a destination at all.
        let end = unescaped(rest, b'>')?;
        (&rest[..end], rest[end + 1..].trim_start())
    } else {
        let end = raw.find(char::is_whitespace).unwrap_or(raw.len());
        (&raw[..end], raw[end..].trim_start())
    };
    let titled = rest.is_empty()
        || (rest.len() >= 2
            && (rest.starts_with('"') && rest.ends_with('"')
                || rest.starts_with('\'') && rest.ends_with('\'')
                || rest.starts_with('(') && rest.ends_with(')')));
    if destination.is_empty() || !titled {
        return None;
    }
    Some(destination.to_owned())
}

#[cfg(test)]
mod link_tests {
    use super::{
        LinkKind, LinkScope, code_spans, is_code_fence, link_scope, markdown_links,
        strip_code_spans, wiki_link_targets,
    };

    /// Every link on a line, as `(kind, target, text, external)`, so a case can
    /// state the whole answer rather than one field of it.
    fn scanned(line: &str) -> Vec<(LinkKind, String, String, bool)> {
        markdown_links(line)
            .into_iter()
            .map(|l| (l.kind, l.target, l.text, l.scope.is_external()))
            .collect()
    }

    // -----------------------------------------------------------------------
    // Code spans — the rule `rto_spec::text` held before this, moved with it.
    // -----------------------------------------------------------------------

    /// A backslash-escaped backtick is literal and opens no span.
    ///
    /// It used to pair with the next real opener and swallow everything
    /// between, which hid a table column from `rto_spec::fmt` — and would hide
    /// a `[[…]]` link or a `@rto:` annotation from the scanners that read this,
    /// since they share this rule. Raised on #790.
    #[test]
    fn an_escaped_backtick_opens_no_span() {
        assert_eq!(code_spans(r"a \` b `code` c").len(), 1);
        assert_eq!(strip_code_spans(r"a \` b `code` c"), r"a \` b  c");
        // A doubled backslash escapes itself, so the backtick is real again.
        assert_eq!(code_spans(r"a \\`code` b").len(), 1);
        // And the link scanner is not fooled by one.
        assert_eq!(wiki_link_targets(r"\` [[docs/x.md]]"), vec!["docs/x.md"]);
    }

    /// The rule is asymmetric: an escape opens nothing, but closes normally.
    ///
    /// `CommonMark` does not process backslash escapes inside a code span, so a
    /// backslash before the closing run is literal content and the run still
    /// closes. Treating the close like the open made a span run on to the next
    /// backtick and swallow everything between. Raised on #790.
    #[test]
    fn an_escape_before_a_closing_run_still_closes_the_span() {
        // One span, ending at the backtick after the backslash.
        assert_eq!(code_spans(r"`a\` and [[docs/x.md]]").len(), 1);
        assert_eq!(
            wiki_link_targets(r"`a\` and [[docs/x.md]]"),
            vec!["docs/x.md"]
        );
        assert_eq!(strip_code_spans(r"`a\` rest"), " rest");
    }

    #[test]
    fn removes_single_and_multi_backtick_spans() {
        assert_eq!(strip_code_spans("a `code` b"), "a  b");
        // A run of two backticks (used to embed a literal backtick) is a span too.
        assert_eq!(strip_code_spans("see ``@rto:0001`` here"), "see  here");
        assert_eq!(strip_code_spans("x ```fenced inline``` y"), "x  y");
    }

    #[test]
    fn keeps_unmatched_backticks_and_plain_text() {
        assert_eq!(strip_code_spans("no code here"), "no code here");
        assert_eq!(strip_code_spans("unmatched ` tick"), "unmatched ` tick");
        // Mismatched run lengths do not close the span.
        assert_eq!(strip_code_spans("``open ` mid"), "``open ` mid");
    }

    #[test]
    fn preserves_utf8_outside_spans() {
        assert_eq!(strip_code_spans("café `x` — ok"), "café  — ok");
    }

    // -----------------------------------------------------------------------
    // Code-span exclusion, for both kinds
    // -----------------------------------------------------------------------

    /// A link of either kind inside single backticks is a documentation
    /// example, not a link — the property the four `rto_spec` scanners depend
    /// on and the one most likely to be lost in a rewrite of this scanner.
    #[test]
    fn a_link_inside_a_code_span_is_not_a_link() {
        assert!(scanned("see `[[docs/x.md#Sym]]` for the form").is_empty());
        assert!(scanned("write `[label](target.md)` like this").is_empty());
        // A run of two backticks is a span too, and so is a triple-backtick
        // *inline* run — which is not a fence, because a fence is a whole line.
        assert!(scanned("``[[a/b.md]]`` and ```[c](d.md)```").is_empty());
        // The example and a real link on one line: only the real one counts.
        assert_eq!(
            scanned("`[[example]]` but [[docs/real.md]] resolves")
                .iter()
                .map(|(_, t, _, _)| t.as_str())
                .collect::<Vec<_>>(),
            vec!["docs/real.md"]
        );
    }

    /// An **unmatched** backtick run shields nothing, so a link after one is
    /// still a link. This is the half #790 got wrong in `rto_spec::fmt`: a
    /// scanner that enters code mode on an opener it never closes hides the
    /// rest of the line.
    #[test]
    fn an_unclosed_code_span_hides_nothing() {
        assert_eq!(wiki_link_targets("` [[docs/x.md]]"), vec!["docs/x.md"]);
        assert_eq!(
            scanned("`` [text](docs/x.md)")
                .iter()
                .map(|(_, t, _, _)| t.as_str())
                .collect::<Vec<_>>(),
            vec!["docs/x.md"]
        );
    }

    /// Removing a code span **joins** what was either side of it, which is what
    /// the scan has always done and is therefore what it must keep doing: a
    /// link whose brackets straddle a span is found, and its reported range
    /// covers the span it straddles so a rewriter replaces the whole thing.
    #[test]
    fn a_link_straddling_a_code_span_is_one_link() {
        let line = "[[docs/`x`.md]]";
        assert_eq!(wiki_link_targets(line), vec!["docs/.md"]);
        assert_eq!(markdown_links(line)[0].span, 0..line.len());
    }

    /// A link whose close **abuts** a code span ends at its own `]]`, not at the
    /// far side of the span.
    ///
    /// The two offsets are the same number in the stripped string and different
    /// numbers in the line, so this is the one case where mapping an exclusive
    /// end like a start silently over-extends every such range. It was a real
    /// defect, found by `markdown_links_parity.rs` over
    /// `docs/adr/0009-…:232` while none of the cases here noticed; it is pinned
    /// here so the corpus is not the only thing standing between the bug and a
    /// rewriter splicing over a reader's backticks.
    #[test]
    fn a_link_ending_where_a_code_span_begins_stops_at_its_own_close() {
        let line = "[[crates/x.rs#Sym]]`::field` and prose";
        let links = markdown_links(line);
        assert_eq!(&line[links[0].span.clone()], "[[crates/x.rs#Sym]]");
        // The same shape for an inline link, whose close is a single `)`.
        let line = "[t](docs/x.md)`::field`";
        assert_eq!(
            &line[markdown_links(line)[0].span.clone()],
            "[t](docs/x.md)"
        );
    }

    /// A fenced or indented code block is **not** this function's business, and
    /// saying so is the point: it reads one line and cannot see a fence at all.
    ///
    /// # This is a reported inconsistency, not a design
    ///
    /// Every document scanner in `rto_spec` tracks fences itself and skips the
    /// lines inside them, so a fenced `[[…]]` is not an authored link. **No
    /// scanner in this workspace excludes an indented code block**, so a
    /// four-space-indented `[[…]]` *is* one, and the gate resolves a link the
    /// renderer shows as literal code. That predates this function, is
    /// unchanged by it, and is recorded here rather than silently fixed —
    /// fixing it moves what `roteiro check` counts.
    #[test]
    fn a_fence_is_not_visible_from_one_line() {
        // The fence delimiter itself, which callers key on.
        assert!(is_code_fence("```"));
        assert!(is_code_fence("   ```rust"));
        assert!(is_code_fence("~~~"));
        assert!(!is_code_fence("a ``` b"));
        // A line *inside* either kind of block still yields its link here…
        assert_eq!(
            wiki_link_targets("[[docs/fenced.md]]"),
            vec!["docs/fenced.md"]
        );
        // …including a four-space-indented one, which nothing filters today.
        assert_eq!(
            wiki_link_targets("    [[docs/indented.md]]"),
            vec!["docs/indented.md"]
        );
        // A line that both opens a fence and carries a link: the delimiter test
        // and the scan are independent, so a caller sees both facts.
        assert!(is_code_fence("``` [[docs/straddle.md]]"));
        assert_eq!(
            wiki_link_targets("``` [[docs/straddle.md]]"),
            vec!["docs/straddle.md"]
        );
    }

    // -----------------------------------------------------------------------
    // Wiki links — the semantics that must not move
    // -----------------------------------------------------------------------

    #[test]
    fn wiki_links_are_trimmed_and_never_empty() {
        assert_eq!(
            wiki_link_targets("[[  docs/x.md#Sym  ]]"),
            vec!["docs/x.md#Sym"]
        );
        assert!(wiki_link_targets("[[]] and [[   ]]").is_empty());
        assert_eq!(
            wiki_link_targets("[[a.md]] then [[b.md]]"),
            vec!["a.md", "b.md"]
        );
    }

    /// An unclosed `[[` **stops** the scan rather than being skipped past, and
    /// an *earlier* one swallows a later well-formed link into its own target
    /// instead of yielding two.
    ///
    /// Both are what `rto_spec::text::scan_wiki_links` did, so both are what
    /// this has to keep doing. Neither is what a reader would call right, and
    /// the second produces a target that resolves to nothing — which is why it
    /// is safe as well as required: it costs the gate a violation it already
    /// reported, not a link it already counted. Recorded rather than fixed;
    /// fixing it changes what `roteiro check` counts.
    #[test]
    fn an_unclosed_wiki_link_ends_the_scan() {
        // The close belongs to the *first* opener, so this is one target.
        assert_eq!(
            wiki_link_targets("[[unclosed and [[docs/x.md]]"),
            vec!["unclosed and [[docs/x.md"]
        );
        // An opener with no close at all ends the scan where it stands.
        assert_eq!(
            wiki_link_targets("[[docs/x.md]] then [[unclosed"),
            vec!["docs/x.md"]
        );
    }

    /// A wiki-link is never external and its text is its target, so a caller
    /// labelling links does not have to special-case the kind.
    #[test]
    fn a_wiki_link_is_internal_and_labels_itself() {
        assert_eq!(
            scanned("[[docs/adr/0001-x.md#Design]]"),
            vec![(
                LinkKind::Wiki,
                "docs/adr/0001-x.md#Design".to_owned(),
                "docs/adr/0001-x.md#Design".to_owned(),
                false,
            )]
        );
    }

    // -----------------------------------------------------------------------
    // Inline links — the new capability
    // -----------------------------------------------------------------------

    #[test]
    fn an_inline_link_yields_its_text_and_destination() {
        assert_eq!(
            scanned("see [the ADR](docs/adr/0026-x.md) for why"),
            vec![(
                LinkKind::Inline,
                "docs/adr/0026-x.md".to_owned(),
                "the ADR".to_owned(),
                false,
            )]
        );
    }

    /// The classification a citation needs: whose work this names.
    #[test]
    fn a_destination_is_internal_or_external_by_its_scheme() {
        for internal in [
            "docs/x.md",
            "../README.md",
            "/docs/x.md",
            "#a-section",
            "x.md#a:b",
            "C/x.md",
        ] {
            assert_eq!(
                link_scope(internal),
                LinkScope::Internal,
                "{internal} is in this repository"
            );
        }
        for external in [
            "https://example.org/a",
            "http://example.org",
            "mailto:a@b.c",
            "//example.org/a",
            "ftp://example.org",
            "tel:+441234",
        ] {
            assert_eq!(
                link_scope(external),
                LinkScope::External,
                "{external} is somebody else's"
            );
        }
    }

    /// Brackets and parentheses nest, and a title is metadata rather than a
    /// destination — both cases where reading to the *first* delimiter gives a
    /// target nothing resolves.
    #[test]
    fn nesting_and_titles_are_read_the_way_commonmark_writes_them() {
        assert_eq!(
            scanned("[see [x]](docs/y.md)"),
            vec![(
                LinkKind::Inline,
                "docs/y.md".to_owned(),
                "see [x]".to_owned(),
                false,
            )]
        );
        assert_eq!(
            scanned(r#"[t](docs/y.md "A title")"#)
                .iter()
                .map(|(_, t, _, _)| t.as_str())
                .collect::<Vec<_>>(),
            vec!["docs/y.md"]
        );
        // Balanced parentheses inside a destination, as a Wikipedia URL has.
        assert_eq!(
            scanned("[t](https://e.org/A_(b))")
                .iter()
                .map(|(_, t, _, _)| t.as_str())
                .collect::<Vec<_>>(),
            vec!["https://e.org/A_(b)"]
        );
        // The angle-bracket form, which may hold a space.
        assert_eq!(
            scanned("[t](<a b.md>)")
                .iter()
                .map(|(_, t, _, _)| t.as_str())
                .collect::<Vec<_>>(),
            vec!["a b.md"]
        );
    }

    /// A link that does not close is not a link, and neither is one naming
    /// nothing — the reading the OKF bundle reader argued for, kept here so it
    /// cannot invent an edge out of stray punctuation.
    ///
    /// The last three are the same rule reaching further than "closes": a
    /// parenthesised part that is not a destination and an optional title is
    /// **not a link either**. Ignoring the junk instead turns `[t](foo bar)`
    /// into a citation of `foo` and `[t](<unclosed)` into one of `<unclosed`,
    /// and a plausible wrong citation is worse than none (#801, raised in
    /// review on #806).
    #[test]
    fn malformed_and_empty_inline_links_are_not_links() {
        assert!(scanned("[text](unclosed").is_empty());
        assert!(scanned("[text without a destination]").is_empty());
        assert!(scanned("[text]()").is_empty());
        assert!(scanned(r"\[not a link](docs/x.md)").is_empty());
        assert!(scanned("[t](<unclosed)").is_empty());
        assert!(scanned("[t](foo bar)").is_empty());
        assert!(scanned("[t](docs/x.md not-a-title)").is_empty());
    }

    /// An image is its own kind, not an `Inline` link — so a `.png` cannot
    /// reach a reference list, and a caller reducing markdown to visible text
    /// still gets the alt text rather than the source folded in.
    ///
    /// It was read as an `Inline` link *starting one byte late*, which is both
    /// at once wrong. Raised in review on #806.
    #[test]
    fn an_image_is_its_own_kind_and_starts_at_the_bang() {
        let line = "![a diagram](docs/x.png)";
        assert_eq!(
            scanned(line),
            vec![(
                LinkKind::Image,
                "docs/x.png".to_owned(),
                "a diagram".to_owned(),
                false,
            )]
        );
        // The range covers the `!`, so a caller splicing over it drops the whole
        // image rather than leaving a stray bang behind.
        assert_eq!(&line[markdown_links(line)[0].span.clone()], line);
        // An escaped `!` is literal text, so what follows it *is* a link.
        assert_eq!(
            scanned(r"\![t](docs/x.md)")
                .iter()
                .map(|(_, t, _, _)| t.as_str())
                .collect::<Vec<_>>(),
            vec!["docs/x.md"]
        );
        // A real link on the same line as an image is still an `Inline` one.
        assert_eq!(
            scanned("![img](a.png) and [t](docs/x.md)")
                .iter()
                .map(|(k, t, _, _)| (*k, t.as_str()))
                .collect::<Vec<_>>(),
            vec![(LinkKind::Image, "a.png"), (LinkKind::Inline, "docs/x.md")]
        );
    }

    /// A `[[…]]` inside an image's alt text **is** still reported, and that is
    /// required rather than tolerated.
    ///
    /// A `[[…]]` is a Roteiro token found anywhere on the line; the scanner this
    /// replaced had no concept of images, and `roteiro check` counts what that
    /// scanner found. Suppressing it here would move the gate's number. Raised
    /// in review on #806, and answered by the parity contract rather than by a
    /// change. The image rule applies to the `[…](…)` syntax it is part of.
    #[test]
    fn a_wiki_link_in_alt_text_is_reported_because_the_gate_counts_it() {
        assert_eq!(
            scanned("![alt [[docs/x.md]]](i.png)")
                .iter()
                .map(|(k, t, _, _)| (*k, t.as_str()))
                .collect::<Vec<_>>(),
            vec![(LinkKind::Image, "i.png"), (LinkKind::Wiki, "docs/x.md")]
        );
        // The image is reported first because its range opens first, and that
        // range **encloses** the wiki-link's. Overlap is the honest report of an
        // overlap; a caller splicing ranges takes one kind, as both of this
        // function's rewriting callers do.
        let links = markdown_links("![alt [[docs/x.md]]](i.png)");
        assert!(links[0].span.start < links[1].span.start);
        assert!(links[0].span.end > links[1].span.end);
    }

    /// An angle-bracket destination is opaque: it may hold the parentheses and
    /// quotes that close the link everywhere else, which is the point of writing
    /// one. Counting them rejected `[t](<https://e.org/a_(b)>)`, a valid link.
    /// Raised in review on #806.
    #[test]
    fn an_angle_destination_may_hold_what_would_otherwise_close_the_link() {
        let line = "[t](<https://e.org/a_(b)>) after";
        let links = markdown_links(line);
        assert_eq!(links[0].target, "https://e.org/a_(b)");
        assert_eq!(&line[links[0].span.clone()], "[t](<https://e.org/a_(b)>)");
        assert!(links[0].scope.is_external());
        // A quote inside one is content, not a title nobody closed.
        assert_eq!(scanned(r#"[t](<a "b".md>)"#)[0].1, r#"a "b".md"#);
        // The closing `>` must be unescaped, so an escaped one is content.
        assert_eq!(scanned(r"[t](<a\>b.md>)")[0].1, r"a\>b.md");
        // And one that never closes is still not a link.
        assert!(scanned("[t](<https://e.org/a").is_empty());
    }

    /// A label is what a **reader** sees, so it keeps the code spans the scan
    /// removed.
    ///
    /// Excluding a code span from the scan and excluding it from the label are
    /// different decisions: a link *inside* backticks is an example, but
    /// backticks *inside a label* are how this repository writes the name of a
    /// type. Taking the text from the stripped string cited "the  type".
    /// Raised in review on #806.
    #[test]
    fn a_label_keeps_the_code_spans_the_scan_removed() {
        assert_eq!(
            scanned("see [the `Foo` type](docs/x.md)"),
            vec![(
                LinkKind::Inline,
                "docs/x.md".to_owned(),
                "the `Foo` type".to_owned(),
                false,
            )]
        );
        // A label that is *entirely* one code span: the stripped range is empty
        // and sits on a chunk boundary, which is the case that inverts if the
        // two ends are mapped the same way.
        assert_eq!(scanned("[`Foo`](docs/x.md)")[0].2, "`Foo`");
    }

    /// A title may contain the `)` that would otherwise close the link.
    ///
    /// `CommonMark` allows it, and counting it ended the span inside the link —
    /// which a caller splicing over that span (`doc_anchor_fragments.rs`'s
    /// heading reader does) turns into rubble. An apostrophe with no whitespace
    /// before it is destination content, not a title nobody closed. Raised in
    /// review on #806.
    #[test]
    fn a_title_may_hold_the_bracket_that_would_close_the_link() {
        let line = r#"[t](docs/x.md "a ) b") after"#;
        let links = markdown_links(line);
        assert_eq!(links[0].target, "docs/x.md");
        assert_eq!(&line[links[0].span.clone()], r#"[t](docs/x.md "a ) b")"#);
        // Single-quoted and parenthesised titles are the other two forms.
        assert_eq!(scanned("[t](docs/x.md 'a ) b')")[0].1, "docs/x.md");
        assert_eq!(scanned("[t](docs/x.md (a title))")[0].1, "docs/x.md");
        // An apostrophe inside a destination opens nothing.
        assert_eq!(scanned("[t](https://e.org/a'b)")[0].1, "https://e.org/a'b");
    }

    /// `[[a]]` is one wiki-link, not also an inline link whose text is `[a`.
    #[test]
    fn the_two_kinds_do_not_double_count_one_link() {
        assert_eq!(
            scanned("[[docs/x.md]]")
                .iter()
                .map(|(k, _, _, _)| *k)
                .collect::<Vec<_>>(),
            vec![LinkKind::Wiki]
        );
    }

    /// Links come back in source order however they are written, because a
    /// caller rewriting them in place walks the list once.
    #[test]
    fn links_are_reported_in_source_order_with_usable_ranges() {
        let line = "a [t](x.md) b [[y.md]] c [u](https://e.org)";
        let links = markdown_links(line);
        assert_eq!(
            links.iter().map(|l| l.target.as_str()).collect::<Vec<_>>(),
            vec!["x.md", "y.md", "https://e.org"]
        );
        // Every range addresses the link it was reported for, so a rewriter can
        // splice over it without re-finding anything.
        assert_eq!(&line[links[0].span.clone()], "[t](x.md)");
        assert_eq!(&line[links[1].span.clone()], "[[y.md]]");
        assert_eq!(&line[links[2].span.clone()], "[u](https://e.org)");
        assert!(links[2].scope.is_external());
    }
}

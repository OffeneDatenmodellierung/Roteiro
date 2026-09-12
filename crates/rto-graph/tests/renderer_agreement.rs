//! [`rto_graph::markdown_links`] and `pulldown-cmark` read the same line the same
//! way — and where they do not, this file is the list of exceptions.
//!
//! # Why the renderer is the oracle
//!
//! Every document in this repository is rendered to HTML by `pulldown-cmark`
//! (`rto_render::docs`). So when the shared scanner and the renderer disagree
//! about whether a line holds a link, one of two bad things happens: the gate
//! checks a link the site does not publish, or — worse — the site publishes a
//! link the gate never checked. That is the defect class #801 exists to remove,
//! and it is not hypothetical: the `[[…]]` scanner and the renderer's private one
//! disagreed on four shapes before this work, and #790 shipped a defect of the
//! same shape between two Markdown walkers.
//!
//! Three rounds of review on #806 each found more members of this class than
//! were reported, because each was checked one input at a time. This is that
//! check as a table instead: every shape runs through both readers, and the
//! answers must match unless this file says why not.
//!
//! # The list is two-sided on purpose
//!
//! A case marked [`Verdict::Diverges`] must **still** diverge. A test that only
//! caught new disagreements would let a deliberate one be "fixed" by accident
//! and lose the reasoning with it — which is how a documented decision decays
//! into an accident. Both directions fail here, so the list has to be edited by
//! whoever changes the behaviour, in the same change.

use pulldown_cmark::{Event, Options, Parser, Tag};
use rto_graph::{LinkKind, markdown_links};

/// What this file claims about one line.
enum Verdict {
    /// Both readers give the same answer.
    Agrees,
    /// They do not, and this is why. The string is the reason, printed on
    /// failure so that a case which stops diverging reports what it was for.
    Diverges(&'static str),
}

use Verdict::{Agrees, Diverges};

/// **Every** inline link and image on `line`, per the renderer, in order, as
/// `(is_image, destination)`.
///
/// The whole line, not the first link on it. These helpers returned only the
/// first until #806's sixth round, which meant a line whose *second* link
/// diverged compared equal and passed — a guard against silent disagreement that
/// was itself silent. ``[a](x`y) and [b](z`w)`` is the witness: both readers
/// agree on the first destination and only the renderer finds the second.
///
/// `Options::empty()` deliberately: `Options::all()` turns on `ENABLE_WIKILINKS`,
/// which reinterprets `[[a]]` and would make this compare the wrong two rules.
fn rendered(line: &str) -> Vec<(bool, String)> {
    Parser::new_ext(line, Options::empty())
        .filter_map(|ev| match ev {
            Event::Start(Tag::Link { dest_url, .. }) => Some((false, dest_url.to_string())),
            Event::Start(Tag::Image { dest_url, .. }) => Some((true, dest_url.to_string())),
            _ => None,
        })
        .collect()
}

/// The same, per the shared scanner.
///
/// Wiki-links are filtered out because they are a Roteiro token the renderer
/// rewrites before it parses; comparing them would compare nothing.
fn scanned(line: &str) -> Vec<(bool, String)> {
    markdown_links(line)
        .into_iter()
        .filter(|l| l.kind() != LinkKind::Wiki)
        .map(|l| (l.kind() == LinkKind::Image, l.target().to_owned()))
        .collect()
}

/// The reason every [`Diverges`] case below gives, because they are all one rule.
const NAMES_NOTHING: &str = "a destination that names nothing is not a link — \
     `MarkdownLink::target` is a graph key and a citation label, and the \
     renderer's `href=\"\"`/`href=\" \"` is neither. The same reading `[t]()` and \
     `[[  ]]` always had";

/// The third reason: a code span that swallows a later link.
///
/// Code spans are removed from the whole line before the scan, and
/// [`rto_graph::markdown_links`] reads a destination back out of the line but
/// still *scans* the stripped string. A backtick inside one link's destination
/// or title is not a code-span delimiter to `CommonMark` — the link parser
/// consumes it raw — but the standalone span lexer has no way to know that, so
/// it pairs with a later backtick and everything between them, including any
/// link, is erased before the scan sees it.
///
/// **Recorded rather than fixed, and measured before deciding.** Scanning every
/// `.md` and `.rs` in this repository — 326 files, 210,005 lines — for a line
/// where the scanner reports fewer inline links than the renderer finds **zero**
/// instances of this shape. The wiki half is not a regression either: the
/// scanner this replaced stripped code spans exactly the same way, which
/// `markdown_links_parity.rs` holds byte-for-byte over the same tree. And no
/// production caller reads an inline `target` yet — `docs.rs` takes wiki-links
/// only. Closing it means abandoning strip-then-scan for a single left-to-right
/// pass, which is a rewrite of the scanner rather than a fix to it. Raised in
/// review on #806; it belongs with #801's citation phase, which is the first
/// code that will read these destinations.
const SWALLOWED: &str = "a backtick inside a link's destination or title is not \
     a code-span delimiter to `CommonMark`, but the span lexer that runs before \
     the scan cannot know that, so it pairs with a later backtick and erases the \
     link between them. Zero instances in this repository across 210,005 lines, \
     and the wiki half matches the scanner this replaced exactly";

/// The second reason.
///
/// Pre-existing, and deliberately left: it belongs to #801's citation phase.
const UNESCAPED: &str = "a backslash is left in the target. The renderer \
     unescapes a destination because it is emitting an `href`; this reports what \
     the source says, and `markdown_links`' docs say a backslash escapes the \
     *delimiter* after it rather than that the escape is removed. No consumer \
     reads an inline `target` yet — `docs.rs` takes wiki-links only — so the \
     first caller that does is the one that gets to decide, alongside how a \
     citation renders. Recorded here rather than changed inside a review round";

/// Every shape this file compares, and what it claims about each.
fn cases() -> Vec<(&'static str, Verdict)> {
    vec![
        // -- destinations, bare and angle-wrapped ------------------------------
        ("[t](docs/x.md)", Agrees),
        ("[t](<docs/x.md>)", Agrees),
        ("[t](  docs/x.md  )", Agrees),
        ("[t](< docs/x.md >)", Diverges(NAMES_NOTHING)),
        ("[t](< >)", Diverges(NAMES_NOTHING)),
        ("[t](<  >)", Diverges(NAMES_NOTHING)),
        ("[t](<>)", Diverges(NAMES_NOTHING)),
        ("[t](   )", Diverges(NAMES_NOTHING)),
        ("[t]()", Diverges(NAMES_NOTHING)),
        // An unescaped `<` or a missing `>` is not an angle destination.
        ("[t](<a<b>)", Agrees),
        ("[t](<a\\<b>)", Diverges(UNESCAPED)),
        // -- more than one link on a line ---------------------------------
        ("[a](x.md) and [b](y.md)", Agrees),
        ("[a](x`y.md) then [b](z.md)", Agrees),
        ("see [a](`q`) and [b](z.md)", Agrees),
        ("![i](i.png) then [b](z.md)", Agrees),
        // …and the shape where a code span reaches across two of them.
        ("[a](x`y) and [b](z`w)", Diverges(SWALLOWED)),
        (
            r#"[a](x.md "t`1") and [b](y.md "t`2")"#,
            Diverges(SWALLOWED),
        ),
        ("[t](<https://e.org/a", Agrees),
        ("[t](<a>junk)", Agrees),
        ("[t](<a> junk)", Agrees),
        // A bare destination holds no unescaped whitespace.
        ("[t](a b)", Agrees),
        ("[t](a\\ b)", Agrees),
        // Balanced parentheses, and the angle form's opacity.
        ("[t](https://e.org/a_(b))", Agrees),
        ("[t](<https://e.org/a_(b)>)", Agrees),
        // -- titles -----------------------------------------------------------
        (r#"[t](x.md "title")"#, Agrees),
        (r"[t](x.md 'title')", Agrees),
        ("[t](x.md (title))", Agrees),
        (r#"[t](x.md "")"#, Agrees),
        (r"[t](x.md '')", Agrees),
        ("[t](x.md ())", Agrees),
        (r#"[t](x.md "a ) b")"#, Agrees),
        (r#"[t](x.md "a\"b")"#, Agrees),
        (r#"[t](x.md 'a"b')"#, Agrees),
        (r#"[t](x.md  "a")"#, Agrees),
        (r#"[t](x.md "a" )"#, Agrees),
        // One title, and not one that closes and reopens. Five shapes, one rule.
        (r#"[t](x.md "one" "two")"#, Agrees),
        (r#"[t](x.md "a"x"b")"#, Agrees),
        (r#"[t](<x.md> "a" "b")"#, Agrees),
        ("[t](x.md (a)b(c))", Agrees),
        ("[t](x.md (a(b)c))", Agrees),
        (r#"[t](x.md 'a' "b")"#, Agrees),
        (r#"[t](x.md "one" junk)"#, Agrees),
        // A title needs no separator after an angle destination — see
        // `a_title_may_follow_an_angle_destination_without_a_separator`.
        (r#"[t](<x.md>"title")"#, Agrees),
        (r#"[t](x.md"title")"#, Agrees),
        // …and that acceptance has to survive a `)` inside the title, which is
        // what made the divergence worth keeping rather than half-working.
        (r#"[t](<x.md>"a ) b")"#, Agrees),
        (r"[t](<x.md>'a ) b')", Agrees),
        ("[t](<x.md>(a b))", Agrees),
        (r"[t](<a>x'y)", Agrees),
        // -- code spans -------------------------------------------------------
        ("[x](a`b`c)", Agrees),
        ("[x](`docs/x.md`)", Agrees),
        (r#"[t](x.md "a `b`")"#, Agrees),
        ("[a]`x`(docs/b.md)", Agrees),
        ("[not a `link](/foo`)", Agrees),
        ("[the `Foo` type](docs/x.md)", Agrees),
        ("`q`![a](b)", Agrees),
        ("!`x`[label](target)", Agrees),
        // -- labels and images ------------------------------------------------
        ("[](x.md)", Agrees),
        ("[ ](x.md)", Agrees),
        ("![a](img.png)", Agrees),
        ("![](img.png)", Agrees),
        ("![ ](img.png)", Agrees),
        ("\\![a](b)", Agrees),
        ("\\[not a link](x)", Agrees),
        ("[see [x]](y)", Agrees),
        ("[[a]](target)", Agrees),
        ("![[a]](target)", Agrees),
    ]
}

#[test]
fn the_scanner_and_the_renderer_read_the_same_line_the_same_way() {
    let cases = cases();

    // The relation is satisfiable by an empty table, so say how much was
    // compared and how the two kinds of case are split.
    let diverging = cases
        .iter()
        .filter(|(_, v)| matches!(v, Diverges(_)))
        .count();
    assert!(
        cases.len() >= 50 && diverging >= 2,
        "compared only {} shape(s), {diverging} of them divergent — a renderer \
         agreement table this small is not evidence about the scanner",
        cases.len()
    );

    let mut wrong = Vec::new();
    for (line, verdict) in &cases {
        let (them, us) = (rendered(line), scanned(line));
        match verdict {
            Agrees if them != us => wrong.push(format!(
                "{line:?}\n    renderer: {them:?}\n    scanner:  {us:?}\n    \
                 This shape is listed as agreeing. Either the scanner regressed, \
                 or the divergence is intended — in which case move it to \
                 `Diverges` with the reason, in this change."
            )),
            Diverges(why) if them == us => wrong.push(format!(
                "{line:?} no longer diverges — both say {us:?}.\n    It was \
                 listed as divergent because: {why}\n    If that reasoning still \
                 holds this is a regression; if it does not, delete the entry \
                 and say why in the change that did it."
            )),
            _ => {}
        }
    }
    assert!(
        wrong.is_empty(),
        "the shared scanner and `pulldown-cmark` disagree about {} of {} shape(s) \
         in a way this file does not account for. Every document here is rendered \
         by `pulldown-cmark`, so an unlisted disagreement means the gate and the \
         site do not read the same links:\n\n{}",
        wrong.len(),
        cases.len(),
        wrong.join("\n\n")
    );
}

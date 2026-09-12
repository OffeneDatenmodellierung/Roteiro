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

/// The destination of the first inline link or image on `line`, per the renderer,
/// and whether it is an image.
///
/// `Options::empty()` deliberately: `Options::all()` turns on `ENABLE_WIKILINKS`,
/// which reinterprets `[[a]]` and would make this compare the wrong two rules.
fn rendered(line: &str) -> Option<(bool, String)> {
    Parser::new_ext(line, Options::empty()).find_map(|ev| match ev {
        Event::Start(Tag::Link { dest_url, .. }) => Some((false, dest_url.to_string())),
        Event::Start(Tag::Image { dest_url, .. }) => Some((true, dest_url.to_string())),
        _ => None,
    })
}

/// The same, per the shared scanner.
///
/// Wiki-links are filtered out because they are a Roteiro token the renderer
/// rewrites before it parses; comparing them would compare nothing.
fn scanned(line: &str) -> Option<(bool, String)> {
    markdown_links(line)
        .into_iter()
        .find(|l| l.kind() != LinkKind::Wiki)
        .map(|l| (l.kind() == LinkKind::Image, l.target().to_owned()))
}

/// The reason every [`Diverges`] case below gives, because they are all one rule.
const NAMES_NOTHING: &str = "a destination that names nothing is not a link — \
     `MarkdownLink::target` is a graph key and a citation label, and the \
     renderer's `href=\"\"`/`href=\" \"` is neither. The same reading `[t]()` and \
     `[[  ]]` always had";

/// The second reason, and the only other one.
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

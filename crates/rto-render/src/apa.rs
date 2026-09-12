//! APA 7 rendering of a [`Reference`] — reference-list entries and in-text
//! citations (issue #801, phase 3).
//!
//! A pure function from a record to either a formatted entry or a **refusal
//! naming the fields that stopped it**. No I/O, no clock, no graph, no network;
//! the same record renders to the same bytes on every machine and every run.
//!
//! # Refuse rather than fabricate
//!
//! The return type is a `Result` rather than a `String` because there is no
//! string that honestly represents an unresearched work. A citation with a
//! guessed year does not look guessed — it looks like every other line in the
//! list — so the failure is silent by construction, and in an academic context
//! a plausible fabricated citation is materially worse than a missing one.
//!
//! Concretely: **every field this module consults must be
//! [`Attested::Known`] or explicitly [`Attested::AbsentFromWork`]**. An
//! [`Attested::Unknown`] anywhere is a refusal, never an omission. That is the
//! whole rule, and it is why `n.d.` can only ever be reached from
//! `AbsentFromWork` — a claim somebody made about the work — and never from
//! "nobody looked". The editorial work of maintaining a reference register is
//! exactly the work of moving fields out of `Unknown`, and a refusal is the
//! worklist.
//!
//! One consequence is deliberate and worth stating: [`in_text`] validates the
//! **whole** record, not just the author and year it prints. An in-text
//! citation whose work has no reference-list entry is a dangling citation, and
//! `rto-faithful` already holds the line that a claim citing nothing is a
//! fabrication. It would be odd to build the other half of that machine with
//! the hole left in.
//!
//! # Why the output is not a `String`
//!
//! A reference list needs italics and a hanging indent, and the styling is
//! structural: *which* element is italicised is an APA rule, decided here,
//! where the rule lives. Returning `*asterisks*` or `<i>` would push that
//! decision into a renderer that would have to re-parse this output to find it
//! — and re-parsing emphasis out of prose containing titles, brackets and URLs
//! is the class of bug that produces italics running to the end of the entry.
//!
//! So an [`Entry`] is a sequence of typed [`EntrySpan`]s — plain, italic, or a
//! link with its target — and [`Entry::plain_text`] derives the flat string
//! from them. HTML, a terminal, and a plain-text bibliography each map the
//! spans their own way, and none of them parses. The hanging indent is a
//! property of the list rather than of any span, so it is not modelled here.
//!
//! # Which APA rules are implemented, and against what
//!
//! Verified against the APA Style site (7th edition), September 2026:
//!
//! - *Elements of reference list entries* — element order; surname-then-initials;
//!   "when there are 21 or more authors, include the first 19 authors' names,
//!   insert an ellipsis (but no ampersand), and then add the final author's
//!   name"; a group author spelled out in full; sentence case with italics for
//!   standalone works; a bracketed description after the title.
//! - *How many names to include in an APA Style reference* — up to 20 authors,
//!   all are listed; the worked 21-author example shows the ellipsis as three
//!   spaced dots.
//! - *Basic principles of citation* / *Author–date citation system* — `(Luna,
//!   2020)`, `(Salas & D'Agostino, 2020)`, `(Martin et al., 2020)`; narrative
//!   forms spell out "and"; and for three or more authors "include the name of
//!   only the first author plus 'et al.' in every citation (even the first
//!   citation)". That last is the APA 7 change from APA 6, which listed all
//!   authors on first use.
//! - *Missing information* — "for a work with no date, use 'n.d.' in both the
//!   reference list entry and the in-text citation".
//! - *DOIs and URLs* — a DOI is rendered `https://doi.org/xxxxx`, and "if an
//!   online work has both a DOI and a URL, include only the DOI".
//! - *When do you include a retrieval date in a citation?* and the webpage
//!   reference examples — a retrieval date only where the work is unarchived
//!   **and** designed to change, written `Retrieved January 9, 2020, from …`.
//!
//! # What is not implemented, and is therefore unspellable
//!
//! Each of these is a rule whose inputs the record does not carry, so the
//! alternative to leaving it out is emitting a reference with part of it
//! invented:
//!
//! - **Works inside a greater whole** — journal articles, book chapters. See
//!   [`WorkKind`]: the container is a second model, and half of it is worse
//!   than none.
//! - **Works with no author and works with no title.** APA moves the title up
//!   into the author position for the first, and substitutes a bracketed
//!   description for the second. Both are real rules; both need a judgement the
//!   register does not record yet, so both refuse.
//! - **Single-name (mononym) authors.** [`Author::Person`] renders initials, so
//!   an author with no given name refuses rather than being silently printed as
//!   a bare surname.
//! - **`2020a` / `2020b` disambiguation** for one author's works in one year.
//!   The suffix depends on the whole list and on the in-text citations that
//!   accompany it, so it belongs to the phase that renders both together.

use std::fmt;

use rto_graph::reference::{
    AccessDate, Attested, Author, Locator, PublicationDate, Reference, Stability, WorkKind,
};
use serde::Serialize;

/// One run of an entry, carrying how it should be presented.
///
/// Deliberately not `#[non_exhaustive]`: these are the three things an APA
/// reference can contain, and closing the set is what tells a UI at compile
/// time when that stops being true. A wildcard arm in a renderer would drop a
/// new span kind's text out of the page entirely.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EntrySpan {
    /// Text set as-is.
    Plain(String),
    /// Text set in italics — in APA, the title of a standalone work.
    Italic(String),
    /// A link. `text` is what is printed, `href` where it points; for a DOI
    /// both are the `https://doi.org/…` form, because APA prints the resolver
    /// URL itself rather than a label.
    Link {
        /// The printed text.
        text: String,
        /// The link target.
        href: String,
    },
}

impl EntrySpan {
    /// The text of this span, without its styling.
    #[must_use]
    pub fn text(&self) -> &str {
        match self {
            Self::Plain(text) | Self::Italic(text) | Self::Link { text, .. } => text,
        }
    }
}

/// A formatted reference-list entry or in-text citation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entry {
    /// The [`Reference::id`] this was rendered from, so a caller can pair an
    /// in-text citation with its list entry without matching on text.
    pub reference_id: String,
    /// The entry, in reading order.
    pub spans: Vec<EntrySpan>,
}

impl Entry {
    /// The entry as flat text, derived from the spans.
    ///
    /// A convenience for plain-text consumers, and the thing tests assert on.
    /// It is *derived*, never the source of truth: a renderer that wants
    /// italics reads [`Entry::spans`] rather than looking for markup in here,
    /// because there is none to find.
    #[must_use]
    pub fn plain_text(&self) -> String {
        self.spans.iter().map(EntrySpan::text).collect()
    }
}

/// A field APA requires that the record cannot supply.
///
/// "Cannot supply" means [`Attested::Unknown`] — nobody has looked — except
/// where noted. A field somebody established the work does not have is not
/// missing; it is absent, and absence is renderable.
///
/// Deliberately not `#[non_exhaustive]`: a refusal is a worklist, and a tool
/// that turns one into a task for a human has to know about every reason a
/// record can be refused. A new reason arriving as an unmatched wildcard is a
/// piece of editorial work that silently never gets scheduled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Missing {
    /// There are no authors, or one of them has an empty name.
    Author,
    /// A person author with no given names, so no initials can be formed.
    /// Position is 1-based, counting in the order the work lists them.
    AuthorInitials {
        /// Which author, counting from 1.
        position: usize,
    },
    /// Nobody has looked up when the work was published. Note that this is
    /// *not* the undated case: a work established to have no date renders
    /// `n.d.` and is perfectly citable.
    PublicationDate,
    /// The work has no title recorded.
    Title,
    /// Nobody has recorded whether the work carries a version.
    Version,
    /// A bracketed descriptor is required for this kind of work and the record
    /// does not carry one.
    ///
    /// Uniquely, [`Attested::AbsentFromWork`] does not satisfy this for a kind
    /// that requires a descriptor. A descriptor is a requirement of the
    /// *format* — it exists so a reader is not left thinking a data set is a
    /// book — rather than a property of the work, so "this work has no
    /// descriptor" is not a state it can be in.
    Descriptor,
    /// Nobody has recorded a publisher.
    Publisher,
    /// Nobody has recorded where the work can be found; or the kind of work
    /// requires a locator (a web page is nothing without its URL) and none was
    /// recorded; or the work is unarchived and changing, in which case there
    /// must be something for the retrieval clause to point at.
    Locator,
    /// The source element would be empty: the work is recorded as having
    /// neither a publisher nor a locator, which leaves a reader nothing to go
    /// on.
    Source,
}

impl Missing {
    /// A stable kebab-case token naming the field.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Author => "author",
            Self::AuthorInitials { .. } => "author-initials",
            Self::PublicationDate => "publication-date",
            Self::Title => "title",
            Self::Version => "version",
            Self::Descriptor => "descriptor",
            Self::Publisher => "publisher",
            Self::Locator => "locator",
            Self::Source => "source",
        }
    }
}

impl fmt::Display for Missing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AuthorInitials { position } => write!(f, "author-initials (author {position})"),
            other => f.write_str(other.as_str()),
        }
    }
}

/// Why a reference could not be rendered.
///
/// Carries *every* field that stopped it rather than only the first, because
/// the point of a refusal is to be a worklist for whoever maintains the
/// register, and one round trip per missing field is not that.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Refusal {
    /// The [`Reference::id`] that was refused.
    pub reference_id: String,
    /// The fields that stopped it, in a fixed order — never empty.
    pub missing: Vec<Missing>,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cannot cite {}: missing ", self.reference_id)?;
        for (index, missing) in self.missing.iter().enumerate() {
            if index > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{missing}")?;
        }
        Ok(())
    }
}

/// Which in-text form to render.
///
/// Deliberately not `#[non_exhaustive]`: APA's author–date system has exactly
/// these two shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CitationForm {
    /// `(Salas & D'Agostino, 2020)` — the citation sits inside parentheses, and
    /// two authors are joined by an ampersand.
    Parenthetical,
    /// `Salas and D'Agostino (2020)` — the authors are part of the sentence,
    /// and "and" is spelled out.
    Narrative,
}

/// A rendered reference list: what could be cited, and what could not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReferenceList {
    /// The entries, in APA order — see [`Reference::list_order`].
    pub entries: Vec<Entry>,
    /// The records that refused, in the same order they would have appeared in.
    pub refused: Vec<Refusal>,
}

/// Whether APA requires a bracketed descriptor for this kind of work.
///
/// An APA judgement, so it lives here rather than on [`WorkKind`]: another
/// style would draw this line somewhere else, and the record should not carry
/// one style's opinion.
fn requires_descriptor(kind: WorkKind) -> bool {
    match kind {
        WorkKind::Software | WorkKind::DataSet | WorkKind::FactSheet => true,
        WorkKind::Document | WorkKind::WebPage => false,
    }
}

/// Whether the kind of work is nothing without a locator.
fn requires_locator(kind: WorkKind) -> bool {
    match kind {
        WorkKind::WebPage => true,
        WorkKind::Document | WorkKind::Software | WorkKind::DataSet | WorkKind::FactSheet => false,
    }
}

/// Whether the title of this kind of work is italicised.
///
/// True for every kind currently modelled, because every kind currently
/// modelled is a standalone work. The rule is written down as a function rather
/// than assumed, because the branch that returns `false` — an article or a
/// chapter, whose *container* is italicised instead — is the one that arrives
/// with the next kind, and a reader should be able to see where it goes.
fn title_is_italic(kind: WorkKind) -> bool {
    match kind {
        WorkKind::Document
        | WorkKind::Software
        | WorkKind::DataSet
        | WorkKind::FactSheet
        | WorkKind::WebPage => true,
    }
}

/// Every field that stops this record from being cited, in a fixed order.
fn validate(reference: &Reference) -> Vec<Missing> {
    let mut missing = Vec::new();

    let nameless = reference
        .authors
        .iter()
        .any(|author| author.sort_key().trim().is_empty());
    if reference.authors.is_empty() || nameless {
        missing.push(Missing::Author);
    }
    for (index, author) in reference.authors.iter().enumerate() {
        if let Author::Person { surname, given } = author
            && !surname.trim().is_empty()
            && initials(given).is_none()
        {
            missing.push(Missing::AuthorInitials {
                position: index + 1,
            });
        }
    }

    if reference.published.is_unknown() {
        missing.push(Missing::PublicationDate);
    }
    if reference.title.trim().is_empty() {
        missing.push(Missing::Title);
    }
    if !attested_text(&reference.version).is_recorded() {
        missing.push(Missing::Version);
    }

    let descriptor = attested_text(&reference.descriptor);
    if requires_descriptor(reference.kind) {
        if descriptor.value().is_none() {
            missing.push(Missing::Descriptor);
        }
    } else if !descriptor.is_recorded() {
        missing.push(Missing::Descriptor);
    }

    let publisher = attested_text(&reference.publisher);
    if !publisher.is_recorded() {
        missing.push(Missing::Publisher);
    }

    let locator = printable_locator(reference);
    let locator_required = requires_locator(reference.kind)
        || matches!(reference.stability, Stability::UnarchivedAndChanging { .. });
    let locator_missing = match &reference.locator {
        Attested::Unknown => true,
        // Recorded, but there is nothing printable in it: a blank URL is a
        // field somebody typed nothing into, not a judgement that the work has
        // none, so it refuses on the same terms as a blank publisher.
        Attested::Known(_) => locator.is_none(),
        Attested::AbsentFromWork => locator_required,
    };
    if locator_missing {
        missing.push(Missing::Locator);
    }

    if reference.publisher.is_absent_from_work() && reference.locator.is_absent_from_work() {
        missing.push(Missing::Source);
    }

    missing
}

/// A string field reduced to the two questions the formatter asks of it: has
/// anybody recorded an answer, and if so is there text to print?
///
/// The same three states as [`Attested`], restated after trimming — a recorded
/// value that is blank is nobody's answer — so that the formatter asks those
/// two questions once rather than re-deriving them at every use.
enum Recorded<'a> {
    /// Somebody recorded text.
    Value(&'a str),
    /// Somebody recorded that the work has no such value.
    AbsentFromWork,
    /// Nobody recorded anything, or recorded only whitespace.
    Unrecorded,
}

impl<'a> Recorded<'a> {
    /// Whether somebody has recorded an answer, either a value or "the work has
    /// none". A blank recorded value counts as no answer: it is a field
    /// somebody typed nothing into, not a judgement that the work lacks one.
    fn is_recorded(&self) -> bool {
        !matches!(self, Self::Unrecorded)
    }

    /// The text to print, if any.
    fn value(&self) -> Option<&'a str> {
        match self {
            Self::Value(text) => Some(text),
            Self::AbsentFromWork | Self::Unrecorded => None,
        }
    }
}

/// Classify a string field.
fn attested_text(field: &Attested<String>) -> Recorded<'_> {
    match field {
        Attested::AbsentFromWork => Recorded::AbsentFromWork,
        Attested::Unknown => Recorded::Unrecorded,
        Attested::Known(text) => match text.trim() {
            "" => Recorded::Unrecorded,
            text => Recorded::Value(text),
        },
    }
}

/// The one URL APA prints for this record, if it prints one.
///
/// Where both a DOI and a URL are recorded the DOI wins: "if an online work has
/// both a DOI and a URL, include only the DOI".
///
/// A recorded URL is only printable if it is a **web address**. `Locator::Url`
/// holds an arbitrary `String`, and an [`EntrySpan::Link`] is a target some UI
/// will put in an `href` — so a `javascript:`, `data:` or `file:` locator that
/// travelled through here would be this module handing a renderer a hostile
/// destination and calling it a citation. Anything else is not printable, which
/// means the record refuses rather than silently losing its source element.
/// The accepted set is deliberately narrow: a reference needing another scheme
/// is a case to argue, not one to admit by default.
fn printable_locator(reference: &Reference) -> Option<String> {
    match reference.locator.known()? {
        // A DOI is rendered through the resolver, so it is an `https://` URL by
        // construction whatever the record spelled.
        Locator::Doi(doi) | Locator::Both { doi, .. } => Some(doi.url()),
        Locator::Url(url) => {
            let url = url.trim();
            is_web_url(url).then(|| url.to_owned())
        }
    }
}

/// `I. I.` from a list of given names, or `None` when there are none to work
/// from.
///
/// Each given name contributes its first character, uppercased, followed by a
/// period; a hyphenated given name keeps its hyphen and contributes both
/// (`Jean-Paul` → `J.-P.`). A name already recorded as an initial passes
/// through unchanged, so a register that only ever learned `M.` loses nothing
/// and one that learned `Mary` is not obliged to throw the rest away.
fn initials(given: &[String]) -> Option<String> {
    let names: Vec<String> = given
        .iter()
        .filter_map(|name| {
            let parts: Vec<String> = name
                .split('-')
                .filter_map(|part| {
                    let first = part.trim().chars().next()?;
                    Some(format!("{}.", first.to_uppercase()))
                })
                .collect();
            (!parts.is_empty()).then(|| parts.join("-"))
        })
        .collect();
    (!names.is_empty()).then(|| names.join(" "))
}

/// One author as a reference list spells them.
fn author_name(author: &Author) -> Option<String> {
    match author {
        Author::Person { surname, given } => {
            Some(format!("{}, {}", surname.trim(), initials(given)?))
        }
        // A group is an author in its own right and is never reduced to
        // initials: `World Health Organization`, not `W. H. O.`.
        Author::Group(name) => Some(name.trim().to_owned()),
    }
}

/// How many authors are listed in full before the ellipsis, when there are too
/// many to list.
const LISTED_BEFORE_ELLIPSIS: usize = 19;

/// The most authors APA lists without eliding any.
const MAX_LISTED_IN_FULL: usize = 20;

/// The author element: every author when there are 20 or fewer, and the first
/// 19 then an ellipsis then the last when there are 21 or more.
fn author_element(authors: &[Author]) -> Option<String> {
    let names: Vec<String> = authors.iter().map(author_name).collect::<Option<_>>()?;
    let (last, rest) = names.split_last()?;
    Some(match names.len() {
        1 => last.clone(),
        // Two through twenty: all of them, an ampersand before the last, and
        // the serial comma APA keeps in front of it.
        2..=MAX_LISTED_IN_FULL => format!("{}, & {last}", rest.join(", ")),
        // Twenty-one or more: the first nineteen, three spaced dots, and the
        // final author — with **no** ampersand, which is the part of this rule
        // everybody gets wrong.
        _ => format!(
            "{}, . . . {last}",
            rest[..LISTED_BEFORE_ELLIPSIS].join(", ")
        ),
    })
}

/// The date element, as it appears in a reference list: `2020`, `2020, August`,
/// `2020, August 26`, or `n.d.`.
///
/// `None` only for [`Attested::Unknown`], which [`validate`] has already
/// refused — this returns an `Option` rather than asserting so that no path
/// through this module can panic.
fn reference_date(published: &Attested<PublicationDate>) -> Option<String> {
    Some(match published {
        Attested::AbsentFromWork => "n.d.".to_owned(),
        Attested::Unknown => return None,
        Attested::Known(PublicationDate::Year(year)) => year.to_string(),
        Attested::Known(PublicationDate::YearMonth { year, month }) => {
            format!("{year}, {}", month.name())
        }
        Attested::Known(PublicationDate::Full { year, month, day }) => {
            format!("{year}, {} {}", month.name(), day.get())
        }
    })
}

/// `Retrieved January 9, 2020, from ` — the clause, including its trailing
/// space, ready to sit in front of a URL.
fn retrieval_clause(retrieved: AccessDate) -> String {
    format!(
        "Retrieved {} {}, {}, from ",
        retrieved.month.name(),
        retrieved.day.get(),
        retrieved.year
    )
}

/// Whether `url` names a scheme this will turn into a link.
///
/// Case-insensitively, because a URI scheme is case-insensitive by RFC 3986 and
/// `HTTPS://example.org` is a perfectly ordinary way to have written one down.
/// Matching exactly would refuse it as though it were a `javascript:` locator,
/// which is a true rule applied to a false case. The URL itself is printed as
/// recorded — recognising a scheme is not licence to rewrite it.
///
/// `str::get` rather than a slice, so a multi-byte character straddling the
/// scheme length is a `false` rather than a panic.
fn is_web_url(url: &str) -> bool {
    ["https://", "http://"].iter().any(|scheme| {
        url.get(..scheme.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(scheme))
    })
}

/// Whether the source element would repeat the author, in which case APA drops
/// it.
///
/// "Do not include the publisher when it is the same as the author" — a group
/// author's own website is the common case, and `World Health Organization.
/// (2018, May 24). The top 10 causes of death. World Health Organization.
/// https://…` says the name twice for no reader's benefit.
///
/// Only for a **single group author**: a person is not their own publisher, and
/// with several authors the name is not "the author" in the sense the rule
/// means.
///
/// It applies whether or not a locator follows. That looked wrong at first — it
/// can leave the source element empty — but APA's reasoning is that the element
/// is not lost, it is *already there*: a reader who wants the publisher reads
/// the author position, which is why the rule exists rather than tolerating the
/// name twice. Refusing such a record, or keeping the repetition to avoid an
/// empty-looking line, would both be this module preferring its own tidiness to
/// the style it claims to implement.
///
/// Encoding the rule here is what lets the record stay honest. Without it, the
/// only way to get APA's output is `publisher = AbsentFromWork`, which claims
/// the work *has* no publisher — a false statement about the world, made to
/// satisfy a layout rule, in a module whose entire subject is not doing that.
fn publisher_repeats_the_author(reference: &Reference, publisher: &str) -> bool {
    match reference.authors.as_slice() {
        [Author::Group(name)] => name.trim() == publisher.trim(),
        _ => false,
    }
}

/// `text` with its first character upper-cased.
///
/// The one place this module changes recorded text, and the line is worth
/// stating: a **title** is never recased, because which of its words are proper
/// nouns is a judgement about the words, and a formatter guessing at that is a
/// formatter rewriting what somebody wrote down. A **descriptor** is drawn from
/// the small controlled vocabulary APA itself supplies — `Computer software`,
/// `Data set`, `Fact sheet` — and "capitalize the first letter of the
/// description" is a rule about punctuation, with no proper-noun hazard in it.
/// Without this, a record carrying `computer software` renders `[computer
/// software]`, which is simply not APA.
fn capitalise_first(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// Whether a period should be added after `text`, which already ends the title
/// element.
fn needs_period(text: &str) -> bool {
    !matches!(text.chars().last(), Some('.' | '?' | '!'))
}

/// Render one reference-list entry.
///
/// # Errors
///
/// Returns a [`Refusal`] naming every field APA requires that the record does
/// not supply. See the module documentation: an unresearched field is always a
/// refusal and never an omission, so no `Ok` from this function can contain a
/// guessed year, an invented descriptor or a placeholder author.
pub fn entry(reference: &Reference) -> Result<Entry, Refusal> {
    let missing = validate(reference);
    if !missing.is_empty() {
        return Err(Refusal {
            reference_id: reference.id.clone(),
            missing,
        });
    }

    let refuse = |missing: Missing| Refusal {
        reference_id: reference.id.clone(),
        missing: vec![missing],
    };
    let authors = author_element(&reference.authors).ok_or_else(|| refuse(Missing::Author))?;
    let date =
        reference_date(&reference.published).ok_or_else(|| refuse(Missing::PublicationDate))?;

    let mut spans = Vec::new();
    let separator = if needs_period(&authors) { "." } else { "" };
    spans.push(EntrySpan::Plain(format!("{authors}{separator} ({date}). ")));

    let title = reference.title.trim().to_owned();
    let mut tail_of_title = title.clone();
    spans.push(if title_is_italic(reference.kind) {
        EntrySpan::Italic(title)
    } else {
        EntrySpan::Plain(title)
    });

    if let Some(version) = attested_text(&reference.version).value() {
        let text = format!(" (Version {version})");
        tail_of_title.clone_from(&text);
        spans.push(EntrySpan::Plain(text));
    }
    if let Some(descriptor) = attested_text(&reference.descriptor).value() {
        let text = format!(" [{}]", capitalise_first(descriptor));
        tail_of_title.clone_from(&text);
        spans.push(EntrySpan::Plain(text));
    }

    let mut tail = String::new();
    if needs_period(&tail_of_title) {
        tail.push('.');
    }
    let locator = printable_locator(reference);
    let publisher = attested_text(&reference.publisher)
        .value()
        .filter(|publisher| !publisher_repeats_the_author(reference, publisher));
    if let Some(publisher) = publisher {
        tail.push(' ');
        tail.push_str(publisher);
        if needs_period(publisher) {
            tail.push('.');
        }
    }
    if locator.is_some() {
        tail.push(' ');
        if let Stability::UnarchivedAndChanging { retrieved } = reference.stability {
            tail.push_str(&retrieval_clause(retrieved));
        }
    }
    spans.push(EntrySpan::Plain(tail));
    if let Some(url) = locator {
        spans.push(EntrySpan::Link {
            text: url.clone(),
            href: url,
        });
    }

    Ok(Entry {
        reference_id: reference.id.clone(),
        spans,
    })
}

/// Render an in-text citation.
///
/// # Errors
///
/// Returns a [`Refusal`] on exactly the conditions [`entry`] refuses on — the
/// whole record, not only the author and year printed here. An in-text citation
/// with no reference-list entry behind it is a dangling citation, which is the
/// defect this whole module exists to make unspellable.
pub fn in_text(reference: &Reference, form: CitationForm) -> Result<Entry, Refusal> {
    let missing = validate(reference);
    if !missing.is_empty() {
        return Err(Refusal {
            reference_id: reference.id.clone(),
            missing,
        });
    }

    let refuse = |missing: Missing| Refusal {
        reference_id: reference.id.clone(),
        missing: vec![missing],
    };
    // In text, a date is only ever its year — or `n.d.`, which APA uses in the
    // in-text citation exactly as it does in the reference list.
    let year = match &reference.published {
        Attested::AbsentFromWork => "n.d.".to_owned(),
        Attested::Known(published) => published.year().to_string(),
        Attested::Unknown => return Err(refuse(Missing::PublicationDate)),
    };

    let label = |author: &Author| match author {
        Author::Person { surname, .. } => surname.trim().to_owned(),
        Author::Group(name) => name.trim().to_owned(),
    };
    let first = reference
        .authors
        .first()
        .map(&label)
        .ok_or_else(|| refuse(Missing::Author))?;
    let authors = match reference.authors.len() {
        0 | 1 => first,
        2 => {
            let second = reference
                .authors
                .get(1)
                .map(&label)
                .ok_or_else(|| refuse(Missing::Author))?;
            match form {
                CitationForm::Parenthetical => format!("{first} & {second}"),
                CitationForm::Narrative => format!("{first} and {second}"),
            }
        }
        // Three or more: the first author and `et al.`, from the **first**
        // citation. APA 6 listed every author on first use and shortened
        // afterwards; APA 7 does not, and citing a first use the old way is the
        // commonest way to be wrong about this.
        _ => format!("{first} et al."),
    };

    let text = match form {
        CitationForm::Parenthetical => format!("({authors}, {year})"),
        CitationForm::Narrative => format!("{authors} ({year})"),
    };
    Ok(Entry {
        reference_id: reference.id.clone(),
        spans: vec![EntrySpan::Plain(text)],
    })
}

/// Render a whole reference list, in APA order.
///
/// Records that refuse are returned separately rather than dropped: a
/// bibliography that silently shrinks is how a citation goes missing without
/// anybody noticing.
///
/// The output is a pure function of the input *set*: the records are put into
/// [`Reference::list_order`], which is total, before any of them is rendered,
/// so the same set in a different order produces byte-identical output.
#[must_use]
pub fn reference_list(references: &[Reference]) -> ReferenceList {
    let mut ordered: Vec<&Reference> = references.iter().collect();
    ordered.sort_by(|a, b| Reference::list_order(a, b));

    let mut entries = Vec::new();
    let mut refused = Vec::new();
    for reference in ordered {
        match entry(reference) {
            Ok(rendered) => entries.push(rendered),
            Err(refusal) => refused.push(refusal),
        }
    }
    ReferenceList { entries, refused }
}

#[cfg(test)]
mod tests {
    use super::{
        CitationForm, Entry, EntrySpan, Missing, entry, in_text, reference_list,
        requires_descriptor, requires_locator, title_is_italic,
    };
    use rto_graph::reference::{
        AccessDate, Attested, Author, Day, Doi, Locator, Month, PublicationDate, Reference,
        Stability, WorkKind,
    };

    /// A person author. `given` is a space-separated list of given names, so a
    /// fixture reads the way the work prints it.
    fn person(surname: &str, given: &str) -> Author {
        Author::Person {
            surname: surname.to_owned(),
            given: given.split_whitespace().map(str::to_owned).collect(),
        }
    }

    fn day(day: u8) -> Day {
        Day::new(day).expect("a valid day")
    }

    fn doi(text: &str) -> Locator {
        Locator::Doi(Doi::new(text).expect("a valid DOI"))
    }

    /// A record with every field answered — the baseline each case mutates.
    /// Note what "answered" means: `AbsentFromWork` everywhere there is nothing
    /// to print, never `Unknown`.
    fn complete(id: &str, kind: WorkKind, title: &str) -> Reference {
        let mut reference = Reference::new(id, kind, title, Stability::FixedOrArchived);
        reference.authors = vec![person("Luna", "R")];
        reference.published = Attested::Known(PublicationDate::Year(2020));
        reference.version = Attested::AbsentFromWork;
        reference.descriptor = Attested::AbsentFromWork;
        reference.publisher = Attested::Known("Publisher Name".to_owned());
        reference.locator =
            Attested::Known(Locator::Url("https://example.invalid/work".to_owned()));
        reference
    }

    fn many_authors(count: usize) -> Vec<Author> {
        (1..=count)
            .map(|n| person(&format!("Author{n:02}"), "A"))
            .collect()
    }

    /// APA's own worked software example, from the *Publication Manual* §10.10
    /// entry the style site quotes: `Comprehensive meta-analysis (Version
    /// 3.3.070) [Computer software]`.
    fn software() -> Reference {
        let mut software = complete(
            "software",
            WorkKind::Software,
            "Comprehensive meta-analysis",
        );
        software.authors = vec![Author::Group("Biostat".to_owned())];
        software.published = Attested::Known(PublicationDate::Year(2014));
        software.version = Attested::Known("3.3.070".to_owned());
        software.descriptor = Attested::Known("Computer software".to_owned());
        software.publisher = Attested::AbsentFromWork;
        software
    }

    /// APA's own worked example of an undated, unarchived, changing page — the
    /// one case that exercises `n.d.`, a group author and a retrieval date at
    /// once.
    fn population_clock() -> Reference {
        let mut clock = Reference::new(
            "undated",
            WorkKind::WebPage,
            "U.S. and world population clock",
            Stability::UnarchivedAndChanging {
                retrieved: AccessDate {
                    year: 2020,
                    month: Month::January,
                    day: day(9),
                },
            },
        );
        clock.authors = vec![Author::Group("U.S. Census Bureau".to_owned())];
        // The *work* has no date. Somebody checked. This is citable.
        clock.published = Attested::AbsentFromWork;
        clock.version = Attested::AbsentFromWork;
        clock.descriptor = Attested::AbsentFromWork;
        clock.publisher = Attested::Known("U.S. Department of Commerce".to_owned());
        clock.locator =
            Attested::Known(Locator::Url("https://www.census.gov/popclock/".to_owned()));
        clock
    }

    /// Cases that turn on how many authors there are and what kind they are.
    ///
    /// Split from [`cases_by_work_type`] only to keep each function inside the
    /// line budget every function here is held to; the two are one table.
    fn cases_by_author_shape() -> Vec<(&'static str, Reference, &'static str)> {
        let one_author = complete("one", WorkKind::Document, "Title of the work");

        let mut two_authors = complete("two", WorkKind::Document, "A shared title");
        two_authors.authors = vec![person("Salas", "E"), person("D'Agostino", "R")];

        let mut three_authors = complete("three", WorkKind::Document, "A title by three");
        three_authors.authors = vec![
            person("Martin", "T"),
            person("Salas", "E"),
            person("D'Agostino", "R"),
        ];

        let mut corporate = complete("corporate", WorkKind::WebPage, "The top 10 causes of death");
        corporate.authors = vec![Author::Group("World Health Organization".to_owned())];
        corporate.published = Attested::Known(PublicationDate::Full {
            year: 2018,
            month: Month::May,
            day: day(24),
        });
        // The publisher is recorded truthfully — the WHO *is* the publisher of
        // its own fact sheet — and APA's "omit the publisher when it is the
        // author" rule is what keeps it out of the output. Setting this to
        // `AbsentFromWork` to get the same line would be a false statement
        // about the work, made to satisfy a layout rule.
        corporate.publisher = Attested::Known("World Health Organization".to_owned());
        corporate.locator = Attested::Known(Locator::Url(
            "https://www.who.int/news-room/fact-sheets/detail/the-top-10-causes-of-death"
                .to_owned(),
        ));

        vec![
            (
                "one author",
                one_author,
                "Luna, R. (2020). Title of the work. Publisher Name. https://example.invalid/work",
            ),
            (
                "two authors take an ampersand and the serial comma",
                two_authors,
                "Salas, E., & D'Agostino, R. (2020). A shared title. Publisher Name. \
                 https://example.invalid/work",
            ),
            (
                "three authors are all listed in the reference, however the in-text form shortens",
                three_authors,
                "Martin, T., Salas, E., & D'Agostino, R. (2020). A title by three. \
                 Publisher Name. https://example.invalid/work",
            ),
            (
                "a group author is not reduced to initials",
                corporate,
                "World Health Organization. (2018, May 24). The top 10 causes of death. \
                 https://www.who.int/news-room/fact-sheets/detail/the-top-10-causes-of-death",
            ),
        ]
    }

    /// Cases that turn on what kind of work it is and where it can be found.
    fn cases_by_work_type() -> Vec<(&'static str, Reference, &'static str)> {
        let mut with_doi = complete("doi", WorkKind::Document, "A work with a DOI");
        with_doi.locator = Attested::Known(doi("10.1037/abc123"));

        let mut doi_and_url = complete("doi-and-url", WorkKind::Document, "A work with both");
        doi_and_url.locator = Attested::Known(Locator::Both {
            doi: Doi::new("10.1037/abc123").expect("a valid DOI"),
            url: "https://example.invalid/also-here".to_owned(),
        });

        let url_only = complete("url", WorkKind::Document, "A work with a URL only");

        let mut dataset = complete(
            "dataset",
            WorkKind::DataSet,
            "Content analysis of undergraduate psychology textbooks",
        );
        dataset.authors = vec![person("O'Donohue", "W")];
        dataset.published = Attested::Known(PublicationDate::Year(2017));
        dataset.version = Attested::Known("V1".to_owned());
        dataset.descriptor = Attested::Known("Data set".to_owned());
        dataset.publisher = Attested::Known("ICPSR".to_owned());
        dataset.locator = Attested::Known(doi("10.3886/ICPSR36966.v1"));

        vec![
            (
                "a DOI is rendered through the resolver",
                with_doi,
                "Luna, R. (2020). A work with a DOI. Publisher Name. https://doi.org/10.1037/abc123",
            ),
            (
                "a DOI beats a URL when both are recorded",
                doi_and_url,
                "Luna, R. (2020). A work with both. Publisher Name. https://doi.org/10.1037/abc123",
            ),
            (
                "a URL is used when there is no DOI",
                url_only,
                "Luna, R. (2020). A work with a URL only. Publisher Name. \
                 https://example.invalid/work",
            ),
            (
                "a data set carries its version and its descriptor",
                dataset,
                "O'Donohue, W. (2017). Content analysis of undergraduate psychology textbooks \
                 (Version V1) [Data set]. ICPSR. https://doi.org/10.3886/ICPSR36966.v1",
            ),
            (
                "software carries its version and its descriptor",
                software(),
                "Biostat. (2014). Comprehensive meta-analysis (Version 3.3.070) \
                 [Computer software]. https://example.invalid/work",
            ),
            (
                "a genuinely undated work is n.d., and is citable",
                population_clock(),
                "U.S. Census Bureau. (n.d.). U.S. and world population clock. \
                 U.S. Department of Commerce. Retrieved January 9, 2020, from \
                 https://www.census.gov/popclock/",
            ),
        ]
    }

    /// Cases that must refuse, and the fields the refusal must name.
    fn cases_that_refuse() -> Vec<(&'static str, Reference, Vec<Missing>)> {
        let mut hostile_locator = complete("hostile-locator", WorkKind::Document, "A work");
        hostile_locator.locator = Attested::Known(Locator::Url("javascript:alert(1)".to_owned()));

        // The undated case again, except that nobody has looked the date up.
        let mut date_unknown = population_clock();
        date_unknown.id = "date-unknown".to_owned();
        date_unknown.published = Attested::Unknown;

        // Software whose descriptor nobody recorded: the kind requires one, and
        // `Computer software` must not be inferred from `WorkKind::Software`.
        let mut no_descriptor = software();
        no_descriptor.id = "no-descriptor".to_owned();
        no_descriptor.descriptor = Attested::Unknown;

        // …and one where somebody decided none applies, which for a kind that
        // requires one is not an answer either.
        let mut descriptor_absent = software();
        descriptor_absent.id = "descriptor-absent".to_owned();
        descriptor_absent.descriptor = Attested::AbsentFromWork;

        let mut no_author = complete("no-author", WorkKind::Document, "An anonymous work");
        no_author.authors = Vec::new();

        let mut mononym = complete("mononym", WorkKind::Document, "A work by one name");
        mononym.authors = vec![person("Plato", "")];

        let mut nothing_known = Reference::new(
            "nothing-known",
            WorkKind::Software,
            "",
            Stability::FixedOrArchived,
        );
        nothing_known.authors = Vec::new();

        let mut no_source = complete("no-source", WorkKind::Document, "A work from nowhere");
        no_source.publisher = Attested::AbsentFromWork;
        no_source.locator = Attested::AbsentFromWork;

        let mut blank_locator = complete("blank-locator", WorkKind::Document, "A work");
        blank_locator.locator = Attested::Known(Locator::Url("   ".to_owned()));

        vec![
            (
                "a date nobody has looked up refuses",
                date_unknown,
                vec![Missing::PublicationDate],
            ),
            (
                "a descriptor nobody recorded refuses for a kind that needs one",
                no_descriptor,
                vec![Missing::Descriptor],
            ),
            (
                "and so does a descriptor somebody decided does not apply",
                descriptor_absent,
                vec![Missing::Descriptor],
            ),
            ("no author refuses", no_author, vec![Missing::Author]),
            (
                "an author with no given names refuses rather than losing its initials",
                mononym,
                vec![Missing::AuthorInitials { position: 1 }],
            ),
            (
                "a record nobody has touched names every field at once",
                nothing_known,
                vec![
                    Missing::Author,
                    Missing::PublicationDate,
                    Missing::Title,
                    Missing::Version,
                    Missing::Descriptor,
                    Missing::Publisher,
                    Missing::Locator,
                ],
            ),
            (
                "neither a publisher nor a locator leaves no source element",
                no_source,
                vec![Missing::Source],
            ),
            (
                "a recorded but blank locator is nobody's answer, not an absence",
                blank_locator,
                vec![Missing::Locator],
            ),
            (
                "a locator that is not a web address is refused, never linked",
                hostile_locator,
                vec![Missing::Locator],
            ),
        ]
    }

    #[test]
    fn a_complete_record_formats_to_an_apa_entry() {
        let cases = cases_by_author_shape()
            .into_iter()
            .chain(cases_by_work_type());
        for (name, reference, expected) in cases {
            let rendered = entry(&reference)
                .unwrap_or_else(|refusal| panic!("{name}: expected an entry, got {refusal}"));
            assert_eq!(rendered.plain_text(), expected, "{name}");
            assert_eq!(rendered.reference_id, reference.id, "{name}");
        }
    }

    #[test]
    fn an_incomplete_record_refuses_and_names_the_field() {
        for (name, reference, expected) in cases_that_refuse() {
            let refusal =
                entry(&reference).expect_err(&format!("{name}: expected a refusal, got an entry"));
            assert_eq!(refusal.missing, expected, "{name}");
            assert_eq!(refusal.reference_id, reference.id, "{name}");
            assert!(
                !refusal.missing.is_empty(),
                "{name}: a refusal names a field"
            );
        }
    }

    #[test]
    fn an_unknown_date_never_renders_as_n_d() {
        // The whole point of the three-state model, asserted directly: the two
        // records differ in nothing but whether somebody looked, and only one
        // of them produces `n.d.`.
        let mut undated = complete("undated", WorkKind::Document, "A title");
        undated.published = Attested::AbsentFromWork;
        let mut unknown = undated.clone();
        unknown.id = "unknown".to_owned();
        unknown.published = Attested::Unknown;

        let rendered = entry(&undated)
            .expect("an undated work is citable")
            .plain_text();
        assert!(rendered.contains("(n.d.)"), "got {rendered}");

        let refusal = entry(&unknown).expect_err("an unresearched date must refuse");
        assert_eq!(refusal.missing, vec![Missing::PublicationDate]);
        let reported = format!("{refusal}");
        assert!(
            !reported.contains("n.d."),
            "a refusal must not leak the undated spelling: {reported}"
        );
        assert_eq!(reported, "cannot cite unknown: missing publication-date");

        // …and the same record refuses in text, rather than producing a
        // citation with no entry behind it.
        assert!(in_text(&unknown, CitationForm::Parenthetical).is_err());
    }

    #[test]
    fn twenty_authors_are_all_listed() {
        let mut twenty = complete("twenty", WorkKind::Document, "A title by twenty");
        twenty.authors = many_authors(20);
        let rendered = entry(&twenty).expect("renders").plain_text();
        for n in 1..=20 {
            assert!(
                rendered.contains(&format!("Author{n:02}, A.")),
                "author {n} must be listed in full: {rendered}"
            );
        }
        assert!(
            rendered.contains(", & Author20, A."),
            "the twentieth author takes the ampersand: {rendered}"
        );
        assert!(
            !rendered.contains(". . ."),
            "twenty authors are not elided: {rendered}"
        );
    }

    #[test]
    fn twenty_one_authors_are_elided_after_nineteen() {
        let mut twenty_one = complete("twenty-one", WorkKind::Document, "A title by twenty-one");
        twenty_one.authors = many_authors(21);
        let rendered = entry(&twenty_one).expect("renders").plain_text();

        for n in 1..=19 {
            assert!(
                rendered.contains(&format!("Author{n:02}, A.")),
                "the first nineteen are listed in full, and {n} is not: {rendered}"
            );
        }
        assert!(
            rendered.contains("Author19, A., . . . Author21, A."),
            "nineteen, three spaced dots, then the final author: {rendered}"
        );
        assert!(
            rendered.contains("Author21, A."),
            "the final author is always named: {rendered}"
        );
        for dropped in ["Author20, A.", "Author18, A., Author20"] {
            assert!(
                !rendered.contains(dropped),
                "the twentieth of twenty-one is elided, not printed: {rendered}"
            );
        }
        assert!(
            !rendered.contains('&'),
            "there is no ampersand before an elided final author: {rendered}"
        );
    }

    #[test]
    fn in_text_citations_take_et_al_from_the_first_use() {
        let mut reference = complete("three", WorkKind::Document, "A title by three");
        reference.authors = vec![
            person("Martin", "T"),
            person("Salas", "E"),
            person("D'Agostino", "R"),
        ];
        let parenthetical = in_text(&reference, CitationForm::Parenthetical)
            .expect("renders")
            .plain_text();
        let narrative = in_text(&reference, CitationForm::Narrative)
            .expect("renders")
            .plain_text();
        // APA 7 changed this: APA 6 listed all three on first use. There is no
        // "first use" parameter to get wrong, because there is no rule that
        // needs one.
        assert_eq!(parenthetical, "(Martin et al., 2020)");
        assert_eq!(narrative, "Martin et al. (2020)");
        assert!(
            !parenthetical.contains("Salas"),
            "only the first author is named: {parenthetical}"
        );
    }

    #[test]
    fn in_text_citations_by_number_of_authors() {
        let mut reference = complete("r", WorkKind::Document, "A title");
        let cases: Vec<(Vec<Author>, &str, &str)> = vec![
            (vec![person("Luna", "R")], "(Luna, 2020)", "Luna (2020)"),
            (
                vec![person("Salas", "E"), person("D'Agostino", "R")],
                "(Salas & D'Agostino, 2020)",
                "Salas and D'Agostino (2020)",
            ),
            (
                vec![
                    person("Martin", "T"),
                    person("Salas", "E"),
                    person("D'Agostino", "R"),
                ],
                "(Martin et al., 2020)",
                "Martin et al. (2020)",
            ),
            (
                vec![Author::Group("World Health Organization".to_owned())],
                "(World Health Organization, 2020)",
                "World Health Organization (2020)",
            ),
        ];
        for (authors, parenthetical, narrative) in cases {
            reference.authors = authors;
            assert_eq!(
                in_text(&reference, CitationForm::Parenthetical)
                    .expect("renders")
                    .plain_text(),
                parenthetical
            );
            assert_eq!(
                in_text(&reference, CitationForm::Narrative)
                    .expect("renders")
                    .plain_text(),
                narrative
            );
        }
    }

    #[test]
    fn an_undated_work_is_n_d_in_text_too() {
        let mut reference = complete("undated", WorkKind::Document, "A title");
        reference.published = Attested::AbsentFromWork;
        assert_eq!(
            in_text(&reference, CitationForm::Parenthetical)
                .expect("renders")
                .plain_text(),
            "(Luna, n.d.)"
        );
    }

    #[test]
    fn the_title_is_italic_as_structure_not_as_punctuation() {
        let reference = complete("one", WorkKind::Document, "Title of the work");
        let rendered = entry(&reference).expect("renders");
        let italics: Vec<&str> = rendered
            .spans
            .iter()
            .filter_map(|span| match span {
                EntrySpan::Italic(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(italics, ["Title of the work"]);
        assert!(
            !rendered.plain_text().contains('*'),
            "styling is a span kind, never markup in the text"
        );
        // …and the locator is a link span, so a UI does not have to find URLs
        // by scanning for `http`.
        let links: Vec<(&str, &str)> = rendered
            .spans
            .iter()
            .filter_map(|span| match span {
                EntrySpan::Link { text, href } => Some((text.as_str(), href.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(
            links,
            [(
                "https://example.invalid/work",
                "https://example.invalid/work"
            )]
        );
    }

    #[test]
    fn plain_text_is_derived_from_the_spans() {
        let rendered = Entry {
            reference_id: "r".to_owned(),
            spans: vec![
                EntrySpan::Plain("Luna, R. (2020). ".to_owned()),
                EntrySpan::Italic("A title".to_owned()),
                EntrySpan::Plain(". ".to_owned()),
                EntrySpan::Link {
                    text: "https://example.invalid/x".to_owned(),
                    href: "https://example.invalid/x".to_owned(),
                },
            ],
        };
        assert_eq!(
            rendered.plain_text(),
            "Luna, R. (2020). A title. https://example.invalid/x"
        );
    }

    #[test]
    fn a_retrieval_date_appears_only_where_apa_asks_for_one() {
        let mut fixed = complete("fixed", WorkKind::Document, "A fixed work");
        fixed.stability = Stability::FixedOrArchived;
        let rendered = entry(&fixed).expect("renders").plain_text();
        assert!(
            !rendered.contains("Retrieved"),
            "most references carry no retrieval date: {rendered}"
        );

        let mut changing = fixed.clone();
        changing.id = "changing".to_owned();
        changing.stability = Stability::UnarchivedAndChanging {
            retrieved: AccessDate {
                year: 2020,
                month: Month::January,
                day: day(9),
            },
        };
        let rendered = entry(&changing).expect("renders").plain_text();
        assert!(
            rendered.ends_with("Retrieved January 9, 2020, from https://example.invalid/work"),
            "the clause sits immediately in front of the locator: {rendered}"
        );

        // A changing work with nothing to retrieve from is not citable.
        let mut nowhere = changing;
        nowhere.id = "nowhere".to_owned();
        nowhere.locator = Attested::AbsentFromWork;
        assert_eq!(
            entry(&nowhere).expect_err("refuses").missing,
            vec![Missing::Locator]
        );
    }

    #[test]
    fn a_web_page_without_a_url_refuses() {
        let mut page = complete("page", WorkKind::WebPage, "A page");
        page.locator = Attested::AbsentFromWork;
        assert_eq!(
            entry(&page).expect_err("refuses").missing,
            vec![Missing::Locator]
        );
    }

    #[test]
    fn a_title_ending_in_its_own_punctuation_does_not_gain_a_period() {
        let mut question = complete("q", WorkKind::Document, "Who owns the future?");
        question.publisher = Attested::AbsentFromWork;
        question.locator = Attested::Known(Locator::Url("https://example.invalid/q".to_owned()));
        assert_eq!(
            entry(&question).expect("renders").plain_text(),
            "Luna, R. (2020). Who owns the future? https://example.invalid/q"
        );
    }

    #[test]
    fn initials_are_derived_without_rewriting_what_was_recorded() {
        let mut reference = complete("r", WorkKind::Document, "A title");
        reference.authors = vec![
            person("Ibáñez", "Luis Miguel"),
            Author::Person {
                surname: "Sartre".to_owned(),
                given: vec!["Jean-Paul".to_owned()],
            },
            Author::Person {
                surname: "Already".to_owned(),
                given: vec!["M.".to_owned()],
            },
        ];
        let rendered = entry(&reference).expect("renders").plain_text();
        assert!(
            rendered.starts_with("Ibáñez, L. M., Sartre, J.-P., & Already, M. "),
            "{rendered}"
        );
    }

    #[test]
    fn the_apa_rules_that_depend_on_the_kind_of_work() {
        let cases = [
            (WorkKind::Document, false, false, true),
            (WorkKind::Software, true, false, true),
            (WorkKind::DataSet, true, false, true),
            (WorkKind::FactSheet, true, false, true),
            (WorkKind::WebPage, false, true, true),
        ];
        for (kind, descriptor, locator, italic) in cases {
            assert_eq!(requires_descriptor(kind), descriptor, "{kind:?}");
            assert_eq!(requires_locator(kind), locator, "{kind:?}");
            assert_eq!(title_is_italic(kind), italic, "{kind:?}");
        }
    }

    #[test]
    fn only_a_web_address_becomes_a_link() {
        // An `EntrySpan::Link` is an `href` some UI will render. These are the
        // schemes that must never reach one, and a record carrying one refuses
        // rather than losing its source element quietly.
        for hostile in [
            "javascript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "file:///etc/passwd",
            "vbscript:msgbox(1)",
            "  javascript:alert(1)  ",
        ] {
            let mut reference = complete("r", WorkKind::Document, "A title");
            reference.locator = Attested::Known(Locator::Url(hostile.to_owned()));
            assert_eq!(
                entry(&reference).expect_err("refuses").missing,
                vec![Missing::Locator],
                "{hostile:?} must never become a link target"
            );
        }
        // …and the schemes a reference actually uses still work, in any case:
        // a URI scheme is case-insensitive, so refusing `HTTPS://` would be a
        // true rule applied to a false case.
        for good in [
            "https://example.invalid/a",
            "http://example.invalid/b",
            "HTTPS://example.invalid/c",
            "HtTp://example.invalid/d",
        ] {
            let mut reference = complete("r", WorkKind::Document, "A title");
            reference.locator = Attested::Known(Locator::Url(good.to_owned()));
            let rendered = entry(&reference).expect("renders");
            assert!(rendered.plain_text().ends_with(good), "{good}");
            assert!(
                rendered
                    .spans
                    .iter()
                    .any(|span| matches!(span, EntrySpan::Link { href, .. } if href == good))
            );
        }
    }

    #[test]
    fn a_descriptor_is_capitalised_but_a_title_is_never_recased() {
        let mut lowercase = complete("lowercase", WorkKind::Software, "a Deliberately odd TITLE");
        lowercase.version = Attested::AbsentFromWork;
        lowercase.descriptor = Attested::Known("computer software".to_owned());
        let rendered = entry(&lowercase).expect("renders").plain_text();
        assert!(
            rendered.contains("[Computer software]"),
            "APA capitalises the first letter of the description: {rendered}"
        );
        assert!(
            rendered.contains("a Deliberately odd TITLE"),
            "the title is printed exactly as recorded: {rendered}"
        );
    }

    #[test]
    fn a_publisher_that_repeats_a_group_author_is_dropped_rather_than_denied() {
        let mut page = complete("who", WorkKind::WebPage, "The top 10 causes of death");
        page.authors = vec![Author::Group("World Health Organization".to_owned())];
        page.publisher = Attested::Known("World Health Organization".to_owned());
        let rendered = entry(&page).expect("renders").plain_text();
        assert_eq!(
            rendered.matches("World Health Organization").count(),
            1,
            "APA omits the publisher when it is the author: {rendered}"
        );

        // A person is not their own publisher, and a publisher that merely
        // shares a word is a different name.
        let mut person_author = complete("person", WorkKind::Document, "A title");
        person_author.publisher = Attested::Known("Luna".to_owned());
        assert!(
            entry(&person_author)
                .expect("renders")
                .plain_text()
                .contains(". Luna."),
            "only a group author triggers the rule"
        );

        // The rule is unconditional: with no locator either, the entry simply
        // ends after the title. The publisher is not lost — a reader takes it
        // from the author position, which is the whole reason APA drops the
        // repetition rather than tolerating it.
        let mut no_locator = page.clone();
        no_locator.kind = WorkKind::Document;
        no_locator.locator = Attested::AbsentFromWork;
        let rendered = entry(&no_locator).expect("renders").plain_text();
        assert_eq!(
            rendered,
            "World Health Organization. (2020). The top 10 causes of death."
        );
        assert_eq!(rendered.matches("World Health Organization").count(), 1);
    }

    #[test]
    fn a_list_is_ordered_and_byte_identical_whatever_order_it_arrives_in() {
        let mut zhang = complete("zhang", WorkKind::Document, "Later work");
        zhang.authors = vec![person("Zhang", "I")];
        let mut abbott = complete("abbott", WorkKind::Document, "Earlier work");
        abbott.authors = vec![person("Abbott", "K")];
        let mut abbott_undated = abbott.clone();
        abbott_undated.id = "abbott-undated".to_owned();
        abbott_undated.published = Attested::AbsentFromWork;
        let mut martin = complete("martin", WorkKind::Document, "Unresearched work");
        martin.authors = vec![person("Martin", "T")];
        martin.published = Attested::Unknown;

        let forwards = vec![
            zhang.clone(),
            abbott.clone(),
            martin.clone(),
            abbott_undated.clone(),
        ];
        let backwards = vec![abbott_undated, martin, abbott, zhang];

        let rendered = |set: &[Reference]| {
            let list = reference_list(set);
            (
                list.entries
                    .iter()
                    .map(Entry::plain_text)
                    .collect::<Vec<_>>(),
                list.refused
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>(),
            )
        };
        let (entries, refusals) = rendered(&forwards);
        assert_eq!(
            entries
                .iter()
                .map(|text| text.split('.').next().unwrap_or_default())
                .collect::<Vec<_>>(),
            ["Abbott, K", "Abbott, K", "Zhang, I"],
            "alphabetical by surname"
        );
        assert!(
            entries[0].contains("(n.d.)"),
            "an undated work sorts before the same author's dated ones: {}",
            entries[0]
        );
        assert_eq!(refusals, ["cannot cite martin: missing publication-date"]);

        assert_eq!(
            rendered(&backwards),
            (entries, refusals),
            "the same set in a different order must render to the same bytes"
        );
    }

    #[test]
    fn rendering_the_same_list_twice_gives_the_same_bytes() {
        let mut set = Vec::new();
        for (index, surname) in ["Salas", "abbott", "Zhang", "Abbott"].iter().enumerate() {
            let mut reference =
                complete(&format!("r{index}"), WorkKind::Document, "A shared title");
            reference.authors = vec![person(surname, "A")];
            set.push(reference);
        }
        let once = serde_json::to_string(&reference_list(&set)).expect("serialize");
        let twice = serde_json::to_string(&reference_list(&set)).expect("serialize");
        assert_eq!(
            once, twice,
            "rendering must be a pure function of the input"
        );
    }
}

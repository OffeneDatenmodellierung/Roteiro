//! A citable external work — the record a reference list is rendered *from*
//! (issue #801, phase 3).
//!
//! This module holds the data and nothing else. It knows what we have been told
//! about a work; it has no opinion about how a reference is spelled, which
//! fields a citation style insists on, or whether the result is fit to publish.
//! Those are a renderer's judgements and live with the renderer — `rto_render`'s
//! `apa` module. That is deliberately not a link: this crate does not depend on
//! `rto-render`, and a link that resolved would mean the dependency ran the
//! wrong way.
//!
//! # The three states, and why two of them look alike
//!
//! The defect this module exists to prevent is a *plausible* citation. A
//! reference with a guessed year is worse than no reference at all, because it
//! survives review: it looks like the others.
//!
//! Almost every field of a real bibliographic record is in one of three states,
//! and the middle one is the trap:
//!
//! | state | meaning | citable? |
//! |---|---|---|
//! | [`Attested::Known`] | we hold the value | yes |
//! | [`Attested::AbsentFromWork`] | somebody looked, and the **work** has none | yes — APA writes `n.d.` for a dateless work, and that is an honest thing to write |
//! | [`Attested::Unknown`] | nobody has looked it up | **no** |
//!
//! `AbsentFromWork` and `Unknown` are both "there is no value here", which is
//! exactly why they must not share a spelling. `n.d.` is a claim about the work
//! — it asserts that the work carries no date. Rendering an unresearched field
//! as `n.d.` publishes that claim on no evidence, and it is indistinguishable
//! in the output from the honest case. So the two are separate variants, the
//! [`Default`] is [`Attested::Unknown`], and a record built field by field
//! refuses until somebody has been through it.
//!
//! There is no `Option` on any field a renderer consults, for the same reason:
//! `None` would collapse the two states back together.
//!
//! # What is unrepresentable rather than refused
//!
//! Two conditions are modelled so that the bad state cannot be written down at
//! all, which is stronger than catching it later:
//!
//! - **A retrieval date belongs to the condition that requires it.** APA asks
//!   for one only where a work is designed to change and is not archived, so
//!   the date lives *inside* that variant of [`Stability`]. "Changeable, with
//!   no retrieval date" and "stable, but here is a retrieval date I will
//!   quietly emit" are both unspellable.
//! - **A publication date is a year, or a year and a month, or a full date.**
//!   [`PublicationDate`] is an enum rather than a struct of `Option`s, so a day
//!   without a month cannot be recorded.
//!
//! # No clock, and no network
//!
//! Nothing here reads the time. A retrieval date is *data on the record*,
//! written down by whoever did the retrieving; `SystemTime::now()` would make a
//! rendered bundle differ on every build, and ADR-0013 is explicit that no
//! wall-clock value may reach a query. The module is a plain data definition:
//! no I/O, no clock, no graph access.
//!
//! It lives in this crate — not in the renderer — for two reasons. The
//! extraction layer that will eventually populate these records is here, and a
//! type cannot be shared upwards: `rto-render` depends on `rto-graph`, so the
//! record must sit in the lower crate for both to name it. And this is the
//! crate that *structurally* cannot reach the network — `gix` is pinned
//! `default-features = false` to exclude transports (ADR-0019 §2) — which is
//! the guarantee worth having over a type whose every field invites a lookup.

use std::cmp::Ordering;
use std::fmt;

use serde::{Deserialize, Serialize};

/// What is known about one field of a [`Reference`].
///
/// See the module documentation for why the absent and unknown cases are
/// separate variants rather than one `Option`.
///
/// Deliberately not `#[non_exhaustive]`. The three states *are* the model, and
/// the defect this type exists to prevent is precisely a caller folding two of
/// them into one arm. Closing the set makes every consumer's match a compile
/// error the day a fourth epistemic state is proposed — which is when that
/// proposal should be argued, rather than absorbed by a wildcard.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Attested<T> {
    /// We hold the value.
    Known(T),
    /// Somebody looked, and the work has none. A renderer may act on this: a
    /// work with no date is cited `n.d.`, a work with no publisher simply omits
    /// that element.
    AbsentFromWork,
    /// Nobody has looked this up. It is not a fact about the work at all — only
    /// a record of our own ignorance — and no renderer may turn it into output.
    Unknown,
}

/// [`Attested::Unknown`], always.
///
/// Implemented by hand rather than derived: `#[derive(Default)]` would demand
/// `T: Default` for no reason, and — more to the point — the default has to be
/// the state that refuses, so that a record assembled field by field cannot
/// acquire a citable-looking hole.
impl<T> Default for Attested<T> {
    fn default() -> Self {
        Self::Unknown
    }
}

impl<T> Attested<T> {
    /// The value, if we hold one.
    #[must_use]
    pub fn known(&self) -> Option<&T> {
        match self {
            Self::Known(value) => Some(value),
            Self::AbsentFromWork | Self::Unknown => None,
        }
    }

    /// Whether we hold a value.
    #[must_use]
    pub fn is_known(&self) -> bool {
        matches!(self, Self::Known(_))
    }

    /// Whether somebody established that the work has no such value.
    #[must_use]
    pub fn is_absent_from_work(&self) -> bool {
        matches!(self, Self::AbsentFromWork)
    }

    /// Whether nobody has looked this up.
    #[must_use]
    pub fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown)
    }
}

/// A calendar month, named rather than numbered so that an impossible month
/// cannot be recorded.
///
/// Deliberately not `#[non_exhaustive]`: the Gregorian calendar has twelve
/// months, and is not expected to grow one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Month {
    /// January.
    January,
    /// February.
    February,
    /// March.
    March,
    /// April.
    April,
    /// May.
    May,
    /// June.
    June,
    /// July.
    July,
    /// August.
    August,
    /// September.
    September,
    /// October.
    October,
    /// November.
    November,
    /// December.
    December,
}

impl Month {
    /// The English month name, capitalised — the spelling APA uses in a date
    /// element (`2020, August 26`).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::January => "January",
            Self::February => "February",
            Self::March => "March",
            Self::April => "April",
            Self::May => "May",
            Self::June => "June",
            Self::July => "July",
            Self::August => "August",
            Self::September => "September",
            Self::October => "October",
            Self::November => "November",
            Self::December => "December",
        }
    }

    /// The month number, 1 for January through 12 for December. Used for
    /// ordering, never for rendering.
    #[must_use]
    pub fn number(self) -> u8 {
        match self {
            Self::January => 1,
            Self::February => 2,
            Self::March => 3,
            Self::April => 4,
            Self::May => 5,
            Self::June => 6,
            Self::July => 7,
            Self::August => 8,
            Self::September => 9,
            Self::October => 10,
            Self::November => 11,
            Self::December => 12,
        }
    }

    /// Parse a month from its number, 1 through 12.
    #[must_use]
    pub fn from_number(number: u8) -> Option<Self> {
        Some(match number {
            1 => Self::January,
            2 => Self::February,
            3 => Self::March,
            4 => Self::April,
            5 => Self::May,
            6 => Self::June,
            7 => Self::July,
            8 => Self::August,
            9 => Self::September,
            10 => Self::October,
            11 => Self::November,
            12 => Self::December,
            _ => return None,
        })
    }
}

/// A day of the month, 1 through 31.
///
/// A newtype rather than a bare `u8` so that `0` and `47` are not writable, and
/// so that a record read back from disk is validated on the way in: the serde
/// representation is the number, parsed through [`Day::new`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct Day(u8);

impl Day {
    /// Construct a day of the month, rejecting anything outside 1..=31.
    ///
    /// The upper bound is the widest month rather than the right one for a
    /// given month and year: this type carries no calendar, and a bound that
    /// needed one would be a computation over data the record does not hold.
    #[must_use]
    pub fn new(day: u8) -> Option<Self> {
        (1..=31).contains(&day).then_some(Self(day))
    }

    /// The day number.
    #[must_use]
    pub fn get(self) -> u8 {
        self.0
    }
}

/// A day outside 1..=31.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("not a day of the month: {0} (expected 1..=31)")]
pub struct NotADay(pub u8);

impl TryFrom<u8> for Day {
    type Error = NotADay;

    fn try_from(day: u8) -> Result<Self, Self::Error> {
        Self::new(day).ok_or(NotADay(day))
    }
}

impl From<Day> for u8 {
    fn from(day: Day) -> Self {
        day.0
    }
}

/// When a work was published.
///
/// An enum rather than a struct of `Option`s so that a day without a month is
/// unrepresentable. Note that this models the *precision* of a known date, not
/// whether one is known at all — that is [`Attested`]'s job, and a
/// [`Reference::published`] of [`Attested::AbsentFromWork`] is what becomes
/// `n.d.`.
///
/// Deliberately not `#[non_exhaustive]`. APA recognises date shapes this does
/// not carry yet — a season, a range — so the set may well grow. That is the
/// reason for closing it: when it grows, every renderer stops compiling until
/// it says how the new shape is printed, where a wildcard arm would print it as
/// something else. Rendering a date as a date it is not is the failure this
/// whole module is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PublicationDate {
    /// The year alone — the commonest case, and all APA requires of most works.
    Year(i32),
    /// A year and a month.
    YearMonth {
        /// The year.
        year: i32,
        /// The month.
        month: Month,
    },
    /// A full calendar date.
    Full {
        /// The year.
        year: i32,
        /// The month.
        month: Month,
        /// The day of the month.
        day: Day,
    },
}

impl PublicationDate {
    /// The year, whatever the precision. An in-text citation uses only this.
    #[must_use]
    pub fn year(self) -> i32 {
        match self {
            Self::Year(year) | Self::YearMonth { year, .. } | Self::Full { year, .. } => year,
        }
    }
}

/// The date on which somebody retrieved a work, written down by whoever did it.
///
/// Always a full date: APA's retrieval clause is `Retrieved January 9, 2020,
/// from …`, so a partial one could not be rendered. It reaches a record only
/// through [`Stability::UnarchivedAndChanging`], which is the single condition
/// under which APA wants it at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AccessDate {
    /// The year.
    pub year: i32,
    /// The month.
    pub month: Month,
    /// The day of the month.
    pub day: Day,
}

/// Whether a work holds still.
///
/// This is the condition APA attaches a retrieval date to, and the date is
/// carried *inside* the variant that requires it. The two failure modes a
/// separate `retrieved` field would allow — a changeable work with no retrieval
/// date, and a stable work carrying one that gets emitted anyway — are then not
/// mistakes to catch but sentences that cannot be written.
///
/// Deliberately not `#[non_exhaustive]`: APA's condition is a yes-or-no
/// question about one work, so the set has exactly two answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Stability {
    /// The work is fixed, or an archived or versioned copy of what was read
    /// still exists. No retrieval date; this is the ordinary case, and APA is
    /// explicit that most references carry none.
    FixedOrArchived,
    /// The work is designed to change over time and is *not* archived, so the
    /// only honest way to cite it is to say when it was read.
    UnarchivedAndChanging {
        /// When it was read.
        retrieved: AccessDate,
    },
}

/// Who produced a work.
///
/// Deliberately not `#[non_exhaustive]`. The person/group distinction is the
/// one that decides whether a name may be reduced to initials, and a consumer
/// meeting an unknown third kind behind a wildcard would have to guess which
/// side of that line it fell on.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Author {
    /// An individual.
    Person {
        /// Family name, as the work spells it.
        surname: String,
        /// Given names, in order, each either written out (`Mary`) or already
        /// an initial (`M.`). A renderer that wants initials derives them from
        /// the first character either way, so recording the full name loses
        /// nothing and recording only the initial costs nothing.
        given: Vec<String>,
    },
    /// A group, organisation or company — a legitimate author in its own right,
    /// spelled out in full. It is not a person, so it is never reduced to
    /// initials, and the distinction is a variant rather than a flag so that
    /// no renderer can reach for the initials of `World Health Organization`.
    Group(String),
}

impl Author {
    /// The string a reference list alphabetises on: the surname of a person,
    /// or the full name of a group.
    #[must_use]
    pub fn sort_key(&self) -> &str {
        match self {
            Self::Person { surname, .. } => surname,
            Self::Group(name) => name,
        }
    }
}

/// A Digital Object Identifier, held bare (`10.3886/ICPSR36966.v1`).
///
/// A newtype for one reason: a DOI is rendered by prefixing
/// `https://doi.org/`, and a record that had already stored the prefixed form
/// would render `https://doi.org/https://doi.org/10.…`. [`Doi::new`] accepts
/// either spelling and stores the bare one, so the prefix is applied exactly
/// once no matter which form was written down.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Doi(String);

/// A string that is not a DOI.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "not a DOI: {0:?} (expected `10.<digits>/<suffix>`, optionally behind `https://doi.org/` or `doi:`)"
)]
pub struct NotADoi(pub String);

impl Doi {
    /// Parse a DOI, accepting the bare form, a `doi:` prefix, or a
    /// `https://doi.org/` (or `http://`, or `dx.doi.org`) resolver URL.
    ///
    /// The shape checked is the one the DOI Handbook defines: a `10.` prefix, a
    /// registrant code of digits (possibly dot-separated, as in
    /// `10.1000.10/123`), a `/`, and a non-empty suffix. That is deliberately
    /// stricter than "starts with `10.` and contains a slash", because
    /// everything this type accepts is rendered as a resolver link — and a link
    /// that resolves to nothing is exactly the plausible-looking citation this
    /// module exists to prevent. Beyond the shape it cannot go: whether a
    /// well-formed DOI is *registered* is a question only the network answers,
    /// and this crate has no network by construction.
    ///
    /// # Errors
    ///
    /// Returns [`NotADoi`] when what remains after the prefix is not that shape.
    pub fn new(text: &str) -> Result<Self, NotADoi> {
        let trimmed = text.trim();
        let bare = [
            "https://doi.org/",
            "http://doi.org/",
            "https://dx.doi.org/",
            "http://dx.doi.org/",
            "doi:",
        ]
        .iter()
        .find_map(|prefix| trimmed.strip_prefix(prefix))
        .unwrap_or(trimmed);
        let refused = || NotADoi(text.to_owned());
        let rest = bare.strip_prefix("10.").ok_or_else(refused)?;
        let (registrant, suffix) = rest.split_once('/').ok_or_else(refused)?;
        let registrant_is_numeric = registrant
            .split('.')
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()));
        if registrant_is_numeric && !suffix.is_empty() {
            Ok(Self(bare.to_owned()))
        } else {
            Err(refused())
        }
    }

    /// The bare DOI, with no resolver prefix.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The DOI as APA renders it: `https://doi.org/10.…`.
    #[must_use]
    pub fn url(&self) -> String {
        format!("https://doi.org/{}", self.0)
    }
}

impl TryFrom<String> for Doi {
    type Error = NotADoi;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::new(&text)
    }
}

impl From<Doi> for String {
    fn from(doi: Doi) -> Self {
        doi.0
    }
}

impl fmt::Display for Doi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where a work can be found online.
///
/// [`Locator::Both`] exists so that holding a URL *as well as* a DOI is
/// recordable without either being lost. Which one a given style prints is that
/// style's decision, not the record's — APA prints the DOI alone.
///
/// Deliberately not `#[non_exhaustive]`. Other persistent identifiers exist
/// (ISBN, Handle, arXiv), and each would have to be *routed* by every
/// formatter: which one wins when several are present is a style rule, not a
/// default. Closing the set is what makes each formatter answer that question
/// instead of falling through a wildcard and printing nothing.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Locator {
    /// A DOI.
    Doi(Doi),
    /// A URL.
    Url(String),
    /// A DOI and a URL, both recorded.
    Both {
        /// The DOI.
        doi: Doi,
        /// The URL.
        url: String,
    },
}

/// What kind of thing a work is.
///
/// Deliberately a short list of **standalone** works, because those are the
/// ones this phase can render correctly. A journal article or a book chapter is
/// a work inside a greater whole, and citing one needs a second model — the
/// container, its editors, the volume, the issue, the page range. Rather than
/// carry half of that and emit a reference with the container guessed at, the
/// kind is simply not spellable yet. Refusing at the type is the same rule this
/// module applies everywhere else, one level up.
///
/// Deliberately not `#[non_exhaustive]`, and this is the enum here most likely
/// to gain a variant. That is the reason rather than an objection: the kind
/// decides whether a bracketed descriptor is required, whether a locator is
/// required, and whether the title is italicised. A wildcard arm would answer
/// all three for a journal article the way it answers them for a book, and
/// quietly emit a wrong reference — so a new kind has to break the build in the
/// renderer until somebody says what it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
// Kebab-case on the wire so the serialised token is the one `WorkKind::as_str`
// documents. They were allowed to disagree once, and a token that two parts of
// one crate spell differently is a format nobody can rely on.
#[serde(rename_all = "kebab-case")]
pub enum WorkKind {
    /// A standalone document: a book, a report, a specification, a standard.
    Document,
    /// Computer software.
    Software,
    /// A data set.
    DataSet,
    /// A fact sheet.
    FactSheet,
    /// A page on a website.
    WebPage,
}

impl WorkKind {
    /// A stable lowercase token for this kind, for serialised reports and
    /// diagnostics.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Document => "document",
            Self::Software => "software",
            Self::DataSet => "data-set",
            Self::FactSheet => "fact-sheet",
            Self::WebPage => "web-page",
        }
    }
}

/// A citable external work.
///
/// Every field a renderer consults is either a plain value that must be present
/// (the identifier, the kind, the title, the stability) or an [`Attested`],
/// which is how a field says "nobody has checked" without that being
/// indistinguishable from "there is nothing to check".
///
/// The record carries no opinion about whether it is *complete*. Completeness
/// is a property of a record **and a citation style together** — APA insists on
/// a bracketed descriptor for software that another style would not ask for —
/// so it is decided by the renderer, which is where the style's rules are.
///
/// The derived [`Ord`] is **structural** — field order, so [`Reference::id`]
/// first — and exists to give [`Reference::list_order`] a last resort that is
/// total over the whole record. It is not the order a reference list is printed
/// in; sorting a slice with it directly gives id order, which is nobody's
/// bibliography.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Reference {
    /// A stable identifier for this work within a reference register. It never
    /// appears in rendered output; it is what a citation points at, what a
    /// refusal names, and the final tiebreak that makes list ordering total.
    pub id: String,
    /// What kind of thing the work is.
    pub kind: WorkKind,
    /// The authors, in the order the work presents them. That order is the
    /// work's own and is never sorted: authorship order carries meaning.
    pub authors: Vec<Author>,
    /// When the work was published. [`Attested::AbsentFromWork`] is a dateless
    /// work — APA's `n.d.` — and [`Attested::Unknown`] is a date nobody has
    /// looked for, which no renderer may print.
    pub published: Attested<PublicationDate>,
    /// The title, in the case it should be printed in.
    ///
    /// Stored ready to print. Recasing a title needs to know which words are
    /// proper nouns, and a renderer guessing at that is a renderer changing
    /// what somebody wrote down — so the register records sentence case and
    /// nothing downstream touches it.
    pub title: String,
    /// The version, edition or release designation, if the work has one,
    /// recorded bare — `3.3.070`, not `Version 3.3.070` and not `v3.3.070`.
    /// A renderer supplies whatever word its style puts in front, and one that
    /// found the word already there would print it twice.
    pub version: Attested<String>,
    /// A bracketed description of a non-standard work — `Computer software`,
    /// `Data set`, `Fact sheet` — stored **without** its brackets.
    ///
    /// This is a judgement, not a fact read off the work, and it is
    /// deliberately not derivable from [`Reference::kind`]: inferring
    /// `Computer software` from `WorkKind::Software` would be the renderer
    /// inventing the one element APA added to stop readers mistaking one kind
    /// of source for another.
    pub descriptor: Attested<String>,
    /// The publisher, repository or site responsible for making the work
    /// available.
    pub publisher: Attested<String>,
    /// Where the work can be found.
    pub locator: Attested<Locator>,
    /// Whether the work holds still, carrying the retrieval date when it does
    /// not.
    pub stability: Stability,
}

impl Reference {
    /// A record with the fields that have no honest default left
    /// [`Attested::Unknown`], and no authors.
    ///
    /// Such a record **refuses**: that is the point of it. Construction cannot
    /// produce something citable by accident, and every field that reaches
    /// output has to be filled in deliberately.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        kind: WorkKind,
        title: impl Into<String>,
        stability: Stability,
    ) -> Self {
        Self {
            id: id.into(),
            kind,
            authors: Vec::new(),
            published: Attested::Unknown,
            title: title.into(),
            version: Attested::Unknown,
            descriptor: Attested::Unknown,
            publisher: Attested::Unknown,
            locator: Attested::Unknown,
            stability,
        }
    }

    /// The total order a reference list is printed in.
    ///
    /// APA §9.47 is more than "alphabetical by surname", and the parts people
    /// leave out are the parts that make two correct-looking lists disagree:
    ///
    /// - a one-author entry precedes a multi-author entry beginning with the
    ///   same surname;
    /// - entries sharing a first author are ordered by the *second* author's
    ///   surname, and so on down the list;
    /// - only then does date break the tie, earliest first, with an undated
    ///   work before any dated one.
    ///
    /// Comparing the **whole** author list, element by element, gets the first
    /// two for free: `["salas"]` is a prefix of `["salas", "d'agostino"]` and
    /// so sorts before it, and `["salas", "a"]` sorts before `["salas", "b"]`.
    ///
    /// The keys, in order:
    ///
    /// 1. every author's sort key, in the order the work lists them,
    ///    case-insensitively — APA alphabetises letter by letter and does not
    ///    care about capitals;
    /// 2. the same list case-sensitively, so the fold in step 1 never decides a
    ///    tie by accident of iteration order;
    /// 3. the full publication date — year, then month, then day, so two works
    ///    from one year are not left to be separated by their titles. An
    ///    undated work sorts first (APA puts `n.d.` before any year for the
    ///    same author) and a record whose date nobody has looked up sorts last;
    ///    such a record never reaches a rendered list, but the order still has
    ///    to be defined for it. A year-only date precedes a more precise one in
    ///    the same year, because that is the only placement that does not
    ///    invent a month for it;
    /// 4. title, case-insensitively then not;
    /// 5. [`Reference::id`];
    /// 6. the record itself, structurally.
    ///
    /// Key 6 is what makes the order genuinely **total** rather than total
    /// only where ids happen to be unique. `id` is a public `String` and
    /// nothing enforces uniqueness, so two records could agree on keys 1–5 and
    /// still differ — in a publisher, say. Without a last resort they would
    /// compare `Equal`, and a stable sort would then order them by the order
    /// they arrived in, which is exactly the input-order dependence the
    /// byte-reproducibility rule forbids.
    ///
    /// Deliberately a named function rather than *the* `Ord` implementation:
    /// `Ord` must agree with `Eq`, and keys 1–5 are a strict subset of the
    /// fields `PartialEq` compares. The derived `Ord` on [`Reference`] — which
    /// key 6 uses — is structural and agrees with `Eq`; it is a tiebreak, not
    /// a reference-list order, and sorting with it directly gives id order.
    #[must_use]
    pub fn list_order(a: &Self, b: &Self) -> Ordering {
        let authors = |r: &Self| -> Vec<String> {
            r.authors
                .iter()
                .map(|author| author.sort_key().to_owned())
                .collect()
        };
        let folded = |names: &[String]| -> Vec<String> {
            names.iter().map(|name| name.to_lowercase()).collect()
        };
        let (left, right) = (authors(a), authors(b));
        // `Unknown` last, `AbsentFromWork` (n.d.) first, known dates between —
        // and a known date compares on all the precision it has.
        let date = |r: &Self| match &r.published {
            Attested::AbsentFromWork => (0_u8, 0_i32, 0_u8, 0_u8),
            Attested::Known(published) => {
                let (year, month, day) = match published {
                    PublicationDate::Year(year) => (*year, 0, 0),
                    PublicationDate::YearMonth { year, month } => (*year, month.number(), 0),
                    PublicationDate::Full { year, month, day } => {
                        (*year, month.number(), day.get())
                    }
                };
                (1, year, month, day)
            }
            Attested::Unknown => (2, 0, 0, 0),
        };
        folded(&left)
            .cmp(&folded(&right))
            .then_with(|| left.cmp(&right))
            .then_with(|| date(a).cmp(&date(b)))
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
            .then_with(|| a.title.cmp(&b.title))
            .then_with(|| a.id.cmp(&b.id))
            .then_with(|| a.cmp(b))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AccessDate, Attested, Author, Day, Doi, Month, Ordering, PublicationDate, Reference,
        Stability, WorkKind,
    };

    fn person(surname: &str) -> Author {
        Author::Person {
            surname: surname.to_owned(),
            given: vec!["A".to_owned()],
        }
    }

    fn dated(id: &str, surname: &str, year: i32) -> Reference {
        let mut reference = Reference::new(
            id,
            WorkKind::Document,
            "A title",
            Stability::FixedOrArchived,
        );
        reference.authors = vec![person(surname)];
        reference.published = Attested::Known(PublicationDate::Year(year));
        reference
    }

    #[test]
    fn a_fresh_record_knows_nothing_it_was_not_told() {
        let reference = Reference::new("r", WorkKind::Software, "T", Stability::FixedOrArchived);
        // Every one of these is `Unknown`, not `AbsentFromWork`: nobody has
        // looked. A renderer must refuse on all of them.
        assert!(reference.published.is_unknown());
        assert!(reference.version.is_unknown());
        assert!(reference.descriptor.is_unknown());
        assert!(reference.publisher.is_unknown());
        assert!(reference.locator.is_unknown());
        assert!(reference.authors.is_empty());
    }

    #[test]
    fn the_default_is_the_state_that_refuses() {
        let attested: Attested<String> = Attested::default();
        assert!(
            attested.is_unknown(),
            "a field nobody has filled in must default to unresearched, never to absent"
        );
    }

    #[test]
    fn absent_and_unknown_are_distinct_on_the_wire() {
        // A register is a file. If these two round-tripped through the same
        // token, the distinction the whole module rests on would survive only
        // until somebody saved.
        let absent: Attested<String> = Attested::AbsentFromWork;
        let unknown: Attested<String> = Attested::Unknown;
        let absent_json = serde_json::to_string(&absent).expect("serialize");
        let unknown_json = serde_json::to_string(&unknown).expect("serialize");
        assert_ne!(absent_json, unknown_json);
        assert_eq!(absent_json, "\"absent-from-work\"");
        assert_eq!(unknown_json, "\"unknown\"");
        let back: Attested<String> = serde_json::from_str(&absent_json).expect("deserialize");
        assert_eq!(back, Attested::AbsentFromWork);
    }

    #[test]
    fn a_doi_is_stored_bare_however_it_was_written() {
        let bare = Doi::new("10.3886/ICPSR36966.v1").expect("bare");
        assert_eq!(bare.as_str(), "10.3886/ICPSR36966.v1");
        assert_eq!(bare.url(), "https://doi.org/10.3886/ICPSR36966.v1");
        for spelling in [
            "https://doi.org/10.3886/ICPSR36966.v1",
            "http://doi.org/10.3886/ICPSR36966.v1",
            "https://dx.doi.org/10.3886/ICPSR36966.v1",
            "doi:10.3886/ICPSR36966.v1",
            "  10.3886/ICPSR36966.v1  ",
        ] {
            let parsed = Doi::new(spelling).expect("parses");
            assert_eq!(
                parsed.url(),
                "https://doi.org/10.3886/ICPSR36966.v1",
                "the resolver prefix must be applied exactly once, for {spelling:?}"
            );
        }
    }

    #[test]
    fn a_non_doi_is_refused_rather_than_prefixed() {
        for text in [
            "",
            "https://example.invalid/paper",
            "10.no-slash",
            "not a doi",
            // A `10.` and a slash is not enough: everything that parses here is
            // rendered as a resolver link, so these would publish a dead link
            // dressed as a citation.
            "10./suffix",
            "10.foo/suffix",
            "10.1037/",
        ] {
            assert!(Doi::new(text).is_err(), "{text:?} is not a DOI");
        }
        // …while a dot-separated numeric registrant is legitimate, and is the
        // reason the check is "digits and dots" rather than "digits".
        assert!(Doi::new("10.1000.10/123").is_ok());
        assert!(Doi::new("10.10.37/x").is_ok());
    }

    #[test]
    fn an_impossible_day_cannot_be_recorded() {
        assert!(Day::new(0).is_none());
        assert!(Day::new(32).is_none());
        assert_eq!(Day::new(1).map(Day::get), Some(1));
        assert_eq!(Day::new(31).map(Day::get), Some(31));
        // …including on the way back in from a file.
        let refused: Result<Day, _> = serde_json::from_str("0");
        assert!(refused.is_err(), "deserialisation must validate too");
    }

    #[test]
    fn a_date_carries_its_own_precision() {
        let day = Day::new(27).expect("valid");
        assert_eq!(PublicationDate::Year(2026).year(), 2026);
        assert_eq!(
            PublicationDate::YearMonth {
                year: 2026,
                month: Month::August
            }
            .year(),
            2026
        );
        assert_eq!(
            PublicationDate::Full {
                year: 2026,
                month: Month::August,
                day
            }
            .year(),
            2026
        );
        assert_eq!(Month::August.name(), "August");
        assert_eq!(Month::from_number(8), Some(Month::August));
        assert_eq!(Month::from_number(0), None);
        assert_eq!(Month::from_number(13), None);
        assert_eq!(Month::December.number(), 12);
    }

    #[test]
    fn a_retrieval_date_exists_only_where_apa_asks_for_one() {
        // Not an assertion about behaviour — an assertion that the other
        // combinations do not typecheck. `FixedOrArchived` has no field to put
        // a date in, and `UnarchivedAndChanging` cannot be built without one.
        let changing = Stability::UnarchivedAndChanging {
            retrieved: AccessDate {
                year: 2020,
                month: Month::January,
                day: Day::new(9).expect("valid"),
            },
        };
        match changing {
            Stability::UnarchivedAndChanging { retrieved } => {
                assert_eq!(retrieved.year, 2020);
                assert_eq!(retrieved.month.name(), "January");
                assert_eq!(retrieved.day.get(), 9);
            }
            Stability::FixedOrArchived => unreachable!("constructed as changing"),
        }
    }

    #[test]
    fn a_group_author_sorts_on_its_whole_name() {
        let group = Author::Group("World Health Organization".to_owned());
        assert_eq!(group.sort_key(), "World Health Organization");
        assert_eq!(person("Salas").sort_key(), "Salas");
    }

    #[test]
    fn a_list_is_ordered_alphabetically_and_totally() {
        let mut refs = [
            dated("c", "salas", 2019),
            dated("a", "Zhang", 1999),
            dated("b", "Salas", 2020),
            dated("d", "Abbott", 2020),
        ];
        refs.sort_by(Reference::list_order);
        let order: Vec<&str> = refs.iter().map(|r| r.id.as_str()).collect();
        // `Salas`/`salas` tie on the case-insensitive key and are then split by
        // the case-sensitive one, so the order is defined rather than left to
        // whichever the sort happened to see first.
        assert_eq!(order, ["d", "b", "c", "a"]);
    }

    #[test]
    fn an_undated_work_sorts_before_the_same_authors_dated_ones() {
        let mut undated = dated("u", "Salas", 0);
        undated.published = Attested::AbsentFromWork;
        let mut unresearched = dated("x", "Salas", 0);
        unresearched.published = Attested::Unknown;
        let mut refs = [dated("b", "Salas", 2020), unresearched, undated];
        refs.sort_by(Reference::list_order);
        let order: Vec<&str> = refs.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            order,
            ["u", "b", "x"],
            "n.d. first, then years; a date nobody looked up is not a date and sorts last"
        );
    }

    #[test]
    fn a_kinds_wire_token_is_the_one_it_documents() {
        // These were allowed to disagree once: serde wrote `"DataSet"` while
        // `as_str` promised `data-set`, so a report and a serialised record
        // named one kind two ways.
        for kind in [
            WorkKind::Document,
            WorkKind::Software,
            WorkKind::DataSet,
            WorkKind::FactSheet,
            WorkKind::WebPage,
        ] {
            let json = serde_json::to_string(&kind).expect("serialize");
            assert_eq!(
                json,
                format!("\"{}\"", kind.as_str()),
                "the wire token must be the documented one"
            );
        }
    }

    #[test]
    fn one_author_precedes_the_same_authors_collaborations() {
        // APA §9.47: a one-author entry comes before a multi-author entry
        // beginning with the same surname, and entries sharing a first author
        // are separated by the second author's surname — both before date is
        // even consulted.
        let mut solo = dated("solo", "Salas", 2020);
        solo.authors = vec![person("Salas")];
        let mut with_zhang = dated("with-zhang", "Salas", 1990);
        with_zhang.authors = vec![person("Salas"), person("Zhang")];
        let mut with_abbott = dated("with-abbott", "Salas", 1999);
        with_abbott.authors = vec![person("Salas"), person("Abbott")];

        let mut refs = [with_zhang, solo, with_abbott];
        refs.sort_by(Reference::list_order);
        let order: Vec<&str> = refs.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            order,
            ["solo", "with-abbott", "with-zhang"],
            "the solo work first despite its later year, then by second author"
        );
    }

    #[test]
    fn two_works_from_one_year_are_separated_by_month_not_by_title() {
        let mut december = dated("december", "Salas", 2020);
        december.title = "A title".to_owned();
        december.published = Attested::Known(PublicationDate::YearMonth {
            year: 2020,
            month: Month::December,
        });
        let mut january = dated("january", "Salas", 2020);
        // A title that sorts *after* December's, so the assertion fails if the
        // date key gives up at the year and lets the title decide.
        january.title = "Z title".to_owned();
        january.published = Attested::Known(PublicationDate::Full {
            year: 2020,
            month: Month::January,
            day: Day::new(9).expect("valid"),
        });

        let mut refs = [december, january];
        refs.sort_by(Reference::list_order);
        let order: Vec<&str> = refs.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(order, ["january", "december"]);
    }

    #[test]
    fn records_alike_in_every_key_still_have_a_defined_order() {
        // Nothing enforces that `id` is unique, so the last resort has to be
        // the record itself — otherwise these two compare `Equal`, and a stable
        // sort then orders them by however they happened to arrive.
        let mut one = dated("same-id", "Salas", 2020);
        one.publisher = Attested::Known("A Publisher".to_owned());
        let mut two = dated("same-id", "Salas", 2020);
        two.publisher = Attested::Known("B Publisher".to_owned());

        assert_eq!(Reference::list_order(&one, &two), Ordering::Less);
        let mut forwards = [one.clone(), two.clone()];
        forwards.sort_by(Reference::list_order);
        let mut backwards = [two, one];
        backwards.sort_by(Reference::list_order);
        assert_eq!(
            forwards, backwards,
            "the same pair in either order must sort the same way"
        );
    }

    #[test]
    fn ordering_is_stable_whatever_order_the_input_arrives_in() {
        let build = || {
            vec![
                dated("a", "Zhang", 1999),
                dated("b", "Salas", 2020),
                dated("d", "Abbott", 2020),
            ]
        };
        let mut forwards = build();
        forwards.sort_by(Reference::list_order);
        let mut backwards = build();
        backwards.reverse();
        backwards.sort_by(Reference::list_order);
        assert_eq!(forwards, backwards);
    }
}

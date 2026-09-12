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

/// A year a work was published or was retrieved in, 1 or later.
///
/// A newtype for the same reason [`Day`] is one, and it was missing for the
/// same reason `Day`'s bound nearly was: a bare `i32` let `0` and `-1` through
/// `serde`, and the formatter rendered them as `(0)` and `(-1, January 2)` —
/// strings that are not dates, printed with a date's authority, in a module
/// whose subject is not doing that.
///
/// Year zero is not a year in the numbering APA's readers use, and a negative
/// year is BCE, which APA writes as `400 B.C.E.` and not as `-400`. Rendering
/// either as a plain number states something false about the work, so neither
/// is constructible. BCE dates are not modelled at all: the alternative is an
/// era field, and half an era model is worse than none.
///
/// There is deliberately **no upper bound**. A year in the future is either a
/// forthcoming work — a legitimate thing to record — or a typo, and no type can
/// separate those without a clock. This crate has no clock and will not acquire
/// one: a record that rendered differently next year would not be reproducible,
/// which is a harder rule here than tidiness about typos.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "i32", into = "i32")]
pub struct Year(i32);

impl Year {
    /// Construct a year, rejecting zero and anything before it.
    #[must_use]
    pub fn new(year: i32) -> Option<Self> {
        (year >= 1).then_some(Self(year))
    }

    /// The year number.
    #[must_use]
    pub fn get(self) -> i32 {
        self.0
    }
}

/// A year of zero or less.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("not a year: {0} (expected 1 or later; BCE dates are not modelled)")]
pub struct NotAYear(pub i32);

impl TryFrom<i32> for Year {
    type Error = NotAYear;

    fn try_from(year: i32) -> Result<Self, Self::Error> {
        Self::new(year).ok_or(NotAYear(year))
    }
}

impl From<Year> for i32 {
    fn from(year: Year) -> Self {
        year.0
    }
}

impl fmt::Display for Year {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
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
    Year(Year),
    /// A year and a month.
    YearMonth {
        /// The year.
        year: Year,
        /// The month.
        month: Month,
    },
    /// A full calendar date.
    Full {
        /// The year.
        year: Year,
        /// The month.
        month: Month,
        /// The day of the month.
        day: Day,
    },
}

/// Whether `day` exists in that month of that year.
///
/// The proleptic Gregorian calendar, which is the one APA's dates are read in.
/// It lives here rather than on [`Day`] because it is not a property of a day:
/// `31` is a perfectly good day number, and only `31` *together with* February
/// is impossible. That is the shape of this defect and of the several before it
/// — an invariant enforced on a part while the composite goes unchecked — so it
/// is checked where the composite is, and by both types that build one.
fn day_exists(year: Year, month: Month, day: Day) -> bool {
    let leap = |y: i32| y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let length = match month {
        Month::January
        | Month::March
        | Month::May
        | Month::July
        | Month::August
        | Month::October
        | Month::December => 31,
        Month::April | Month::June | Month::September | Month::November => 30,
        Month::February => {
            if leap(year.get()) {
                29
            } else {
                28
            }
        }
    };
    day.get() <= length
}

impl PublicationDate {
    /// Whether this names a day that exists.
    ///
    /// Always true for the `Year` and `YearMonth` precisions, which cannot name
    /// a day at all. For `Full` it is the question [`Day`] cannot answer on its
    /// own: `Day::new(31)` is valid, and `February 31` is not.
    ///
    /// A method rather than a constructor because making the state
    /// unrepresentable means giving this enum an opaque shape — a smart
    /// constructor and private fields on a public enum — which is a wider change
    /// than the defect warrants. So the formatter refuses instead, which is the
    /// module's other setting and the one it uses wherever a record can be
    /// *written* but not *cited*.
    #[must_use]
    pub fn names_a_day_that_exists(self) -> bool {
        match self {
            Self::Year(_) | Self::YearMonth { .. } => true,
            Self::Full { year, month, day } => day_exists(year, month, day),
        }
    }

    /// The year, whatever the precision. An in-text citation uses only this.
    #[must_use]
    pub fn year(self) -> Year {
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
    pub year: Year,
    /// The month.
    pub month: Month,
    /// The day of the month.
    pub day: Day,
}

impl AccessDate {
    /// Whether this names a day that exists — see
    /// [`PublicationDate::names_a_day_that_exists`], which this shares its
    /// calendar with.
    ///
    /// A retrieval date is always full precision, so unlike a publication date
    /// there is no case where the question does not arise. Without it the
    /// formatter emitted `Retrieved February 31, 2021, from …` — a statement
    /// that somebody read a work on a day that did not happen.
    #[must_use]
    pub fn names_a_day_that_exists(self) -> bool {
        day_exists(self.year, self.month, self.day)
    }
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

/// One given name, as the work spells it: `Mary`, or `M.` where that is all
/// anybody recorded.
///
/// A validating newtype for the same reason [`Day`] is one, and the defect that
/// produced it is the sharpest example in this module of the rule the module
/// exists for. The formatter used to reduce given names to initials with a
/// `filter_map`, so a fragment it could not turn into an initial was **dropped**
/// — `given = ["Mary", ""]` rendered `Smith, M.` and `"Jean--Paul"` rendered
/// `J.-P.`. Both are plausible authors with part of the recorded name missing,
/// and a citation that looks authoritative and is partly invented is exactly
/// what this module refuses to emit. Handling it at the one call site would
/// have left the *spelling* available; making the value unconstructible means
/// no later renderer can reintroduce it, and `serde` refuses one on the way in
/// from a file as well.
///
/// What is accepted is stated as an **allowlist** — see
/// `is_given_name_char` — rather than as a list of the malformed shapes to
/// reject. The first attempt at this was the latter, and a space walked through
/// it: `"Mary Ann"` was one legal value that rendered `Smith, M.`, dropping
/// `Ann` by exactly the mechanism the newtype was introduced to close. A rule
/// that enumerates the bad separators is a rule waiting for the next separator.
///
/// Accepted: `Mary`, `M.`, `Jean-Paul`, `Ibáñez`, `N'Golo`. Rejected: the empty
/// string, whitespace alone, `Jean--Paul`, `-Paul`, `Jean-`, `"Mary Ann"`, and
/// anything carrying a digit, a separator or an invisible character.
///
/// Also rejected: a **generational suffix** (`Jr.`, `Sr.`, `II`, `III`, `IV`).
/// Those are not given names, and APA prints them in a position this record has
/// no field for — `Smith, J., Jr.`. Recorded among the given names they render
/// as an invented middle initial, `Smith, J. J.`, which is the same fabrication
/// by another route. Refusing means a real name is unrecordable until the record
/// grows a field for it, which is the trade this module already makes for a
/// mononym author: a visible refusal beats a quiet wrong answer.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct GivenName(String);

/// A string that is not a usable given name.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "not a given name: {0:?} (expected letters, optionally hyphen-joined, each part starting with a letter; no whitespace — two names go in two entries; a generational suffix such as `Jr.` has no field on this record)"
)]
pub struct NotAGivenName(pub String);

/// Whether `c` may appear inside one hyphen-separated part of a given name.
///
/// An **allowlist**, for the reason spelled out on [`is_printable_identifier`],
/// and this one earned it the same way. The rule this replaced split a name on
/// hyphens and required each part to be non-blank — an enumeration of the
/// separators somebody had thought of, which is a list with a space missing from
/// it. `"Mary Ann"` passed, and rendered `Smith, M.`, dropping `Ann`: the exact
/// silent drop the newtype had just been introduced to make unspellable, through
/// the one separator the rule did not name. Unicode has many more.
///
/// So: letters of **any** script, because this is not a rule about English.
/// Combining diacritics U+0300–U+036F, so a name recorded in decomposed form
/// (`e` followed by U+0301, which is what macOS hands you) is the same name as
/// its composed spelling rather than a refusal. A full stop, because `M.` is a
/// value this record is explicitly allowed to hold. An apostrophe in both its
/// spellings, because `N'Golo` and `N’Golo` are one name typed two ways.
///
/// Everything else is refused: digits, every kind of whitespace, every
/// separator, every format and control character. A name this turns away is a
/// name somebody can come and argue for; a name the old rule let through was a
/// citation with a piece missing and nothing to show for it.
fn is_given_name_char(c: char) -> bool {
    c.is_alphabetic()
        || ('\u{0300}'..='\u{036F}').contains(&c)
        || matches!(c, '.' | '\'' | '\u{2019}')
}

/// Whether `name` is a generational suffix rather than a given name.
///
/// A deliberately short, closed list, and not a rule about spelling — these are
/// perfectly good strings. It is a statement that the record has nowhere to put
/// them: APA writes them after the initials, and [`Author::Person`] has two
/// fields, neither of which is that position.
///
/// `Jr` and `Sr` match case-insensitively, with or without the period, because
/// that is the range of ways they are printed. The Roman numerals match exactly,
/// since lower-case `iii` is not how anybody writes one. `I` and `V` are left
/// out on purpose: `V.` is Victor's initial far more often than it is a fifth,
/// and refusing a real initial to catch a rare suffix is the wrong way round.
fn is_generational_suffix(name: &str) -> bool {
    ["jr", "jr.", "sr", "sr."].contains(&name.to_ascii_lowercase().as_str())
        || ["II", "III", "IV"].contains(&name)
}

impl GivenName {
    /// Parse a given name.
    ///
    /// Outer whitespace is trimmed and the trimmed form is what is stored: the
    /// renderer trims, and a leading space no reader can see must not be able to
    /// decide where an entry lands in a list somebody reads. Nothing *inside*
    /// the name is rewritten.
    ///
    /// Each hyphen-separated part must **begin with a letter** and otherwise
    /// contain only given-name characters (`is_given_name_char`). Beginning with
    /// a letter is what makes [`GivenName::initial`] total: every part has a
    /// letter to reduce to, so there is no part it can fail on and therefore
    /// none it could be tempted to skip.
    ///
    /// # One value is one name
    ///
    /// A given name holding internal whitespace is **refused**, and this is a
    /// decision rather than a side effect. `"Mary Ann"` has two readings — two
    /// given names that belong in two elements of `given`, rendering `M. A.`, or
    /// one compound name rendering `M.` — and a formatter picking between them
    /// is guessing at what somebody meant. The caller knows; this type does not.
    /// So the ambiguous spelling is not recordable, and the unambiguous one is:
    /// `given = [GivenName::new("Mary")?, GivenName::new("Ann")?]`.
    ///
    /// The alternative — accept it and render `M. A.` by splitting on whitespace
    /// — was rejected because it makes the compound reading unspellable instead,
    /// and a compound given name is a real thing. Refusing leaves *both* readings
    /// expressible, one of them per element; accepting would silently pick one.
    ///
    /// # Errors
    ///
    /// Returns [`NotAGivenName`] for a blank name, a part not starting with a
    /// letter, any character outside the allowlist (including whitespace), or a
    /// generational suffix.
    pub fn new(text: &str) -> Result<Self, NotAGivenName> {
        let trimmed = text.trim();
        let is_a_name = |part: &str| {
            part.starts_with(char::is_alphabetic) && part.chars().all(is_given_name_char)
        };
        let usable = !trimmed.is_empty()
            && trimmed.split('-').all(is_a_name)
            && !is_generational_suffix(trimmed);
        if usable {
            Ok(Self(trimmed.to_owned()))
        } else {
            Err(NotAGivenName(text.to_owned()))
        }
    }

    /// The name as recorded.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The hyphen-separated parts. Each begins with a letter, by construction,
    /// and a part carries no whitespace — so a part is exactly one name and
    /// there is exactly one initial to take from it.
    pub fn parts(&self) -> impl Iterator<Item = &str> {
        self.0.split('-')
    }

    /// The initial APA prints for this name: `M.` for `Mary` and for `M.`, and
    /// `J.-P.` for `Jean-Paul`.
    ///
    /// It lives on the record rather than in the formatter because two callers
    /// need it and they must not be allowed to disagree: the formatter prints
    /// it, and [`Reference::list_order`] alphabetises on it. APA alphabetises a
    /// list by what the list *prints*, so ordering on the recorded name instead
    /// lets `Mary` and `M.` — one rendered author, two spellings — come out in
    /// an order no reader of the page can account for. One derivation, both
    /// callers; two copies of it is how they came apart in the first place.
    ///
    /// `map` rather than `filter_map`, deliberately: a part is non-blank by
    /// construction, and if that ever stopped being true this should produce a
    /// visibly wrong initial rather than quietly one part short.
    #[must_use]
    pub fn initial(&self) -> String {
        self.parts()
            .map(|part| {
                part.chars()
                    .next()
                    .map_or_else(String::new, |first| format!("{}.", first.to_uppercase()))
            })
            .collect::<Vec<_>>()
            .join("-")
    }
}

impl TryFrom<String> for GivenName {
    type Error = NotAGivenName;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::new(&text)
    }
}

impl From<GivenName> for String {
    fn from(name: GivenName) -> Self {
        name.0
    }
}

impl fmt::Display for GivenName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
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
        given: Vec<GivenName>,
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
///
/// # What is stored is the identifier, not a URL
///
/// The value held here is the **DOI name itself**, raw: `10.1234/a#b` is a DOI
/// whose suffix contains a literal `#`. It is never the percent-encoded form.
/// [`Doi::url`] applies that encoding on the way out, and [`Doi::new`] undoes it
/// when the input arrived as a resolver URL, so the two are inverses and
/// `Doi::new(d.url())` returns `d`.
///
/// Stating it that way round is what keeps the type coherent. The alternative —
/// storing the encoded form — makes [`Doi::as_str`] not the identifier but a
/// fragment of a URL, and leaves no way to tell a DOI containing a literal `%`
/// from one whose `%` opens an escape. Recording what the thing *is* and
/// transforming at the boundary is the same rule [`GivenName`] follows.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Doi(String);

/// `text` without `prefix`, comparing the prefix case-insensitively.
///
/// `str::get` rather than a slice, so a multi-byte character straddling the
/// prefix length is a `None` rather than a panic.
fn strip_prefix_ignoring_ascii_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    text.get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .map(|_| &text[prefix.len()..])
}

/// Whether every character of `text` is one a reader can see and a URL can
/// carry: an ASCII graphic character, `!` through `~`.
///
/// Every identifier this model hands to a renderer — a DOI, a URL — is printed
/// as its own visible text *and* used as a link target, so the two have to be
/// the same string in the reader's eye as well as in the byte stream.
///
/// This is an **allowlist**, and the inversion is the whole point of it. It
/// replaced a denylist of whitespace, control and bidirectional characters,
/// which is a rule that cannot be finished: Unicode holds far more invisible
/// characters than a hand-written list will ever name — U+061C, the
/// U+206A–U+206F block, U+00AD, U+2060, the U+E0020 tag characters — and every
/// one missing from such a list is a link whose printed text is not where it
/// goes. Two were found by review, one after the other, which is the shape of a
/// rule that will keep producing a next one.
///
/// Inverting it puts the failure in the safe direction, which is the argument
/// rather than tidiness. An incomplete **denylist** emits a citation that is not
/// what it looks like, and does it silently. An incomplete **allowlist** refuses
/// a citation somebody can then come and argue for, visibly. Preferring the
/// second is this module's entire subject, so its guards should be shaped that
/// way too.
///
/// This is the rule for a value that is **emitted exactly as recorded** — a
/// `Locator::Url`, which becomes an `href` with no encoding step between the
/// record and the page. Such a value has to be URL-safe and printable already,
/// because nothing downstream will make it so.
///
/// The cost is real and worth stating plainly: a genuine IRI is refused and has
/// to be recorded in its percent-encoded form. That narrows what is
/// *recordable*, not what is correct, and it announces itself the moment
/// somebody tries.
///
/// A DOI is deliberately **not** held to this rule — see `is_doi_name_char`.
/// The difference is not an inconsistency but the reason the two exist
/// separately: a DOI is encoded at the boundary by [`Doi::url`], so it can be
/// recorded as the identifier itself, while a URL is not, so it cannot.
/// # What this does and does not promise a consumer
///
/// It promises the value contains **only characters RFC 3986 permits in a
/// URI**, and nothing invisible. In particular `"`, `<`, `>`, `\`, `^`,
/// `` ` ``, `{`, `|` and `}` are excluded — so a locator cannot close a
/// double-quoted HTML attribute or open a tag, which is how
/// `https://example.invalid/a"onmouseover="…` became an `href` before this rule
/// was narrowed. ASCII-graphic was the wrong bar: it admits all nine.
///
/// It does **not** promise the value is safe to interpolate into HTML
/// unescaped, and nothing here should be read as saying so. `&` and `'` are
/// legal URI characters and are allowed, so a consumer building markup must
/// still escape for its own context — `&` always, and `'` if it quotes
/// attributes with it. **Escaping is the consumer's duty.** What this rule buys
/// is that the characters which make forgetting that duty catastrophic are not
/// representable in the first place.
#[must_use]
pub fn is_printable_identifier(text: &str) -> bool {
    !text.is_empty() && text.chars().all(is_uri_char) && percent_escapes_are_well_formed(text)
}

/// Whether every `%` in `text` introduces a complete `%XX` escape.
///
/// `%` is a legal URI character, so [`is_uri_char`] admits it — but only as the
/// *introducer* of an escape, and whether it is one is a property of the three
/// characters together rather than of the `%`. Without this,
/// `https://example.invalid/a%ZZ` and a trailing `%` passed a predicate
/// documenting that its value holds "only characters RFC 3986 permits in a URI",
/// which was not true of them: a lone `%` makes the string not a URI at all.
///
/// The same shape as the calendar check on [`PublicationDate`] — an invariant
/// enforced on a part while the composite went unasked — and the same answer.
fn percent_escapes_are_well_formed(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            match text.get(index + 1..index + 3) {
                Some(hex) if hex.bytes().all(|byte| byte.is_ascii_hexdigit()) => index += 3,
                _ => return false,
            }
        } else {
            index += 1;
        }
    }
    true
}

/// Whether `c` is a character RFC 3986 permits to appear in a URI.
///
/// Unreserved, sub-delims, gen-delims, and `%` for an escape — which is the
/// whole of §2, stated positively. The nine ASCII graphics it leaves out
/// (`"`, `<`, `>`, `\`, `^`, `` ` ``, `{`, `|`, `}`) are the ones RFC 3986
/// excludes from a URI outright, and are exactly the ones that let a locator
/// escape an HTML attribute.
fn is_uri_char(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(
            c,
            // Unreserved.
            '-' | '.' | '_' | '~'
            // Sub-delims.
            | '!' | '$' | '&' | '\'' | '(' | ')' | '*' | '+' | ',' | ';' | '='
            // Gen-delims.
            | ':' | '/' | '?' | '#' | '[' | ']' | '@'
            // The escape introducer.
            | '%'
        )
}

/// Whether `c` may appear in a DOI name.
///
/// ASCII graphic characters, **or** any letter or digit of any script. So
/// `10.1234/中文` is recordable, because it is a legal DOI name and refusing it
/// would be this type inventing a rule the standard does not have.
///
/// This is **narrower than the standard**, and the doc used to overstate it by
/// saying "any printable character from the Unicode Standard" — which admits
/// non-ASCII punctuation this refuses, an em dash among them. The narrowing is
/// deliberate and the reason is the shape of the rule, not the characters:
/// "printable" has no allowlist spelling, only a denylist of the invisible, and
/// this module has already spent a round learning that such a list cannot be
/// finished. Letters and digits exclude the entire format category by
/// construction. A DOI carrying an em dash is therefore refused — visibly, and
/// arguably — rather than admitted by a rule that would also admit the next
/// zero-width character nobody listed.
///
/// Wider than [`is_printable_identifier`] on purpose, and safe to be wider for a
/// reason that is structural rather than a judgement call: a DOI is never
/// emitted as recorded. It reaches a page only through [`Doi::url`], which
/// percent-encodes everything outside RFC 3986's `pchar` — so whatever is
/// recorded here, the rendered link is pure ASCII, visible, and decodes back to
/// exactly this. A `Locator::Url` has no such boundary, which is why it keeps
/// the narrower rule.
///
/// Still an allowlist. Letters and digits exclude the whole format category
/// (`Cf`) — the zero-width set, the bidi controls, the tag characters — and
/// every separator, so the characters that walked through the denylist this
/// replaced are refused here by category rather than by enumeration.
fn is_doi_name_char(c: char) -> bool {
    c.is_ascii_graphic() || c.is_alphanumeric()
}

/// Reverse [`Doi::url`]'s escaping: `%XX` becomes the byte it names.
///
/// `None` for a truncated escape, a non-hexadecimal one, or bytes that are not
/// valid UTF-8 once decoded — each of which makes the input not a form
/// [`Doi::url`] could have produced, and so not one to guess at.
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = text.get(index + 1..index + 3)?;
            if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return None;
            }
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

/// Whether `byte` may stand for itself inside the path of a resolver URL.
///
/// RFC 3986's `pchar` — unreserved, sub-delims, `:` and `@` — plus `/`, which
/// separates the segments of a DOI suffix. An allowlist again, so a character
/// nobody considered is percent-encoded rather than emitted raw.
const fn is_url_path_safe(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            // Unreserved.
            b'-' | b'.' | b'_' | b'~'
            // Sub-delims.
            | b'!' | b'$' | b'&' | b'\'' | b'(' | b')' | b'*' | b'+' | b',' | b';' | b'='
            // The two pchar additions, and the segment separator.
            | b':' | b'@' | b'/'
        )
}

/// A string that is not a DOI.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "not a DOI: {0:?} (expected `10.<digits>/<suffix>`, optionally behind a doi.org resolver URL or a `doi:` prefix)"
)]
pub struct NotADoi(pub String);

impl Doi {
    /// Parse a DOI, accepting the bare form, a `doi:` prefix, or a
    /// `https://doi.org/` (or `http://`, or `dx.doi.org`) resolver URL.
    ///
    /// The shape checked is the one the DOI Handbook defines: a `10.` prefix, a
    /// registrant code of digits (possibly dot-separated, as in
    /// `10.1000.10/123`), a `/`, and a non-empty suffix **all of whose
    /// characters are printable** (`is_doi_name_char`). That is deliberately
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
        let refused = || NotADoi(text.to_owned());

        // Case-insensitively throughout: a URI scheme is case-insensitive by
        // RFC 3986, and `DOI:10.1234/x` is how plenty of publishers print one.
        // Matching exactly would send those down the "not a DOI at all" path,
        // where they are rejected for a reason that is not true of them.
        //
        // A **resolver URL carries the DOI percent-encoded**, so stripping the
        // prefix leaves the encoded form and it has to be decoded back to the
        // identifier. `doi:` and the bare form are the identifier already, and
        // are taken exactly as written. Without this split the two input forms
        // disagree: `https://doi.org/10.1234/a%23b` and `doi:10.1234/a#b` name
        // one DOI, and only one of them would round-trip through `url`.
        let bare = match [
            "https://doi.org/",
            "http://doi.org/",
            "https://dx.doi.org/",
            "http://dx.doi.org/",
        ]
        .iter()
        .find_map(|prefix| strip_prefix_ignoring_ascii_case(trimmed, prefix))
        {
            Some(encoded) => {
                // In a URL, `?` opens a query and `#` opens a fragment, and a
                // fragment is never sent to the server at all. So they are
                // *delimiters* here, not characters of the name:
                // `https://doi.org/10.1234/a#b` asks the resolver for
                // `10.1234/a`, and storing `10.1234/a#b` would record a DOI
                // nobody requested and re-emit it as `…/a%23b` — a different
                // record again. Truncating at the first of either is what the
                // resolver itself does.
                //
                // Note this makes the two input forms deliberately *disagree*:
                // the bare `10.1234/a#b` is a DOI name whose suffix contains a
                // literal `#`, while the URL form names `10.1234/a`. That is
                // correct — they are different statements — and it is the reason
                // the decode happens only on the URL path.
                let addressed = encoded.split(['?', '#']).next().unwrap_or_default();
                percent_decode(addressed).ok_or_else(refused)?
            }
            None => strip_prefix_ignoring_ascii_case(trimmed, "doi:")
                .unwrap_or(trimmed)
                .to_owned(),
        };

        // The DOI Handbook lets a DOI name incorporate any printable character
        // from the Unicode Standard, and this does not second-guess that beyond
        // the one question it must ask: can every character of it be printed?
        // `is_doi_name_char` is an allowlist, so the next exotic code point is
        // refused rather than discovered by a reviewer.
        let well_formed = {
            let Some(rest) = bare.strip_prefix("10.") else {
                return Err(refused());
            };
            let Some((registrant, suffix)) = rest.split_once('/') else {
                return Err(refused());
            };
            // Digits and dots, and deliberately **no minimum length**. Registrant
            // codes are in practice four digits or more (`10.1000` is the
            // lowest assigned), but that is how they have been handed out, not
            // a rule of the syntax: neither ISO 26324 nor ANSI/NISO Z39.84
            // states a digit count, Crossref's own documentation says members
            // create only the suffix and describes no prefix structure, and the
            // usual secondary sources say the prefix "*usually*" takes the form
            // `10.NNNN`. Enforcing a convention as though it were the standard
            // is how a type comes to refuse a registered identifier, which is a
            // worse failure than accepting an unassigned one — and whether a
            // well-formed DOI is *registered* is a question only the network
            // answers, which this crate does not have.
            let registrant_is_numeric = registrant
                .split('.')
                .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()));
            registrant_is_numeric && !suffix.is_empty() && suffix.chars().all(is_doi_name_char)
        };
        if well_formed {
            Ok(Self(bare))
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
    ///
    /// The identifier is percent-encoded on the way in, because a DOI may
    /// legitimately contain characters that mean something else inside a URL.
    /// The sharpest is `#`: concatenated raw, `10.1234/a#b` becomes
    /// `https://doi.org/10.1234/a#b`, whose `#b` is an HTTP fragment and is
    /// never sent to the resolver — so the link retrieves `10.1234/a`, *a
    /// different record*, silently and with every appearance of working. `?`
    /// opens a query string and does the same; a bare `%` invalidates the escape
    /// sequence it looks like.
    ///
    /// Encoded rather than refused, and the reason is evidence rather than
    /// taste: `10.1002/(SICI)1097-0258(19970815)16:15<1707::AID-SIM605>3.0.CO;2-Y`
    /// is a real registered DOI, and `<`, `>`, `(`, `;` and `:` are ordinary in
    /// the wild. A suffix rule narrow enough to be URL-safe by construction would
    /// refuse identifiers that exist, which is a worse failure than encoding
    /// them. [`Doi::as_str`] still returns the DOI exactly as recorded — this is
    /// a transport encoding applied where the transport is, the same kind of
    /// operation as applying the resolver prefix, and for the same reason it is
    /// applied exactly once.
    ///
    /// What passes through unescaped is RFC 3986's `pchar` (`is_url_path_safe`),
    /// plus `/`; everything else
    /// becomes `%XX`. Byte by byte, so a multi-byte character is encoded as the
    /// bytes a URL actually carries.
    #[must_use]
    pub fn url(&self) -> String {
        let mut url = String::from("https://doi.org/");
        for byte in self.0.bytes() {
            if is_url_path_safe(byte) {
                url.push(char::from(byte));
            } else {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                url.push('%');
                url.push(char::from(HEX[usize::from(byte >> 4)]));
                url.push(char::from(HEX[usize::from(byte & 0x0F)]));
            }
        }
        url
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
    /// - two different people with the same surname are ordered by their
    ///   initials, before date is consulted at all — `Smith, A.` precedes
    ///   `Smith, T.`, whatever year either published;
    /// - only then does date break the tie, earliest first, with an undated
    ///   work before any dated one.
    ///
    /// Comparing the **whole** author list, element by element, gets the first
    /// two for free: `["salas"]` is a prefix of `["salas", "d'agostino"]` and
    /// so sorts before it, and `["salas", "a"]` sorts before `["salas", "b"]`.
    ///
    /// The keys, in order:
    ///
    /// 1. every author, in the order the work lists them — surname then
    ///    *initials as rendered*, folded **letter by letter**: capitals ignored,
    ///    and spaces and punctuation dropped, which is what APA's "letter by
    ///    letter" means literally. `Olsen` therefore precedes `O'Malley`
    ///    precedes `O'Neil`. Rendered initials rather than recorded given names,
    ///    so that nothing invisible on the page can decide the order of the
    ///    page;
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
    /// 4. title, folded the same way then compared as written — trimmed, like
    ///    the author keys, because the renderer trims both and whitespace nobody
    ///    can see must not decide an order somebody reads. Folded the same way
    ///    because APA alphabetises a title letter by letter too, and one
    ///    function applied to both is what keeps them from disagreeing;
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
        fn title(reference: &Reference) -> &str {
            reference.title.trim()
        }
        // Surname **and** initials: a surname alone cannot separate two
        // different people who share one, and APA orders those by their
        // initials before it looks at the date.
        //
        // The **initials**, via `GivenName::initial`, and not the recorded given
        // names — because APA alphabetises a list by what the list prints, and
        // what it prints is `Smith, M.` whether the register learned `Mary` or
        // only `M.`. Keying on the recorded spelling let a difference the page
        // does not show decide an order the page does show: two entries reading
        // `Smith, M. (2020).` came out in an order no reader could account for,
        // and swapping one record's `Mary` for `M.` — no change to a single
        // rendered character — moved it. Two such entries are genuinely
        // indistinguishable to a reader, so they fall through to date, title and
        // id, which are things a reader can see.
        //
        // Trimmed, because the renderer trims: a leading space that no reader
        // can see must not decide where an entry lands either. `GivenName`
        // stores its trimmed form, so only the surname needs it here.
        let authors = |r: &Self| -> Vec<(String, Vec<String>)> {
            r.authors
                .iter()
                .map(|author| {
                    let given = match author {
                        Author::Person { given, .. } => {
                            given.iter().map(GivenName::initial).collect()
                        }
                        Author::Group(_) => Vec::new(),
                    };
                    (author.sort_key().trim().to_owned(), given)
                })
                .collect()
        };
        // Letter by letter, which APA means literally: spaces and punctuation are
        // ignored, so `Olsen` precedes `O'Malley` precedes `O'Neil` — Ol, OM,
        // ON. Keeping the apostrophe in the key sorted all three wrongly, since
        // `'` sorts below every letter and put both O'-names above `Olsen`.
        // Dropping the punctuation from the *fold* only: key 2 below still
        // compares the names as written, so two people who differ only in
        // punctuation are still ordered, and deterministically.
        let fold = |name: &str| -> String {
            name.chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect()
        };
        let folded = |names: &[(String, Vec<String>)]| -> Vec<(String, Vec<String>)> {
            names
                .iter()
                .map(|(surname, given)| {
                    (fold(surname), given.iter().map(|name| fold(name)).collect())
                })
                .collect()
        };
        let (left, right) = (authors(a), authors(b));
        // `Unknown` last, `AbsentFromWork` (n.d.) first, known dates between —
        // and a known date compares on all the precision it has.
        let date = |r: &Self| match &r.published {
            Attested::AbsentFromWork => (0_u8, 0_i32, 0_u8, 0_u8),
            Attested::Known(published) => {
                let (year, month, day) = match published {
                    PublicationDate::Year(year) => (year.get(), 0, 0),
                    PublicationDate::YearMonth { year, month } => (year.get(), month.number(), 0),
                    PublicationDate::Full { year, month, day } => {
                        (year.get(), month.number(), day.get())
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
            .then_with(|| fold(title(a)).cmp(&fold(title(b))))
            .then_with(|| title(a).cmp(title(b)))
            .then_with(|| a.id.cmp(&b.id))
            .then_with(|| a.cmp(b))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AccessDate, Attested, Author, Day, Doi, GivenName, Month, Ordering, PublicationDate,
        Reference, Stability, WorkKind, Year, is_printable_identifier,
    };

    fn year(year: i32) -> Year {
        Year::new(year).expect("a valid year")
    }

    fn given_name(name: &str) -> GivenName {
        GivenName::new(name).expect("a valid given name")
    }

    fn person(surname: &str) -> Author {
        Author::Person {
            surname: surname.to_owned(),
            given: vec![given_name("A")],
        }
    }

    fn dated(id: &str, surname: &str, published: i32) -> Reference {
        let mut reference = Reference::new(
            id,
            WorkKind::Document,
            "A title",
            Stability::FixedOrArchived,
        );
        reference.authors = vec![person(surname)];
        reference.published = Attested::Known(PublicationDate::Year(year(published)));
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
            // A URI scheme is case-insensitive, and publishers print `DOI:`
            // both ways. Rejecting these would refuse a real DOI for a reason
            // that is not true of it.
            "HTTPS://doi.org/10.3886/ICPSR36966.v1",
            "DOI:10.3886/ICPSR36966.v1",
            "Doi:10.3886/ICPSR36966.v1",
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
        assert_eq!(PublicationDate::Year(year(2026)).year().get(), 2026);
        assert_eq!(
            PublicationDate::YearMonth {
                year: year(2026),
                month: Month::August
            }
            .year()
            .get(),
            2026
        );
        assert_eq!(
            PublicationDate::Full {
                year: year(2026),
                month: Month::August,
                day
            }
            .year()
            .get(),
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
                year: year(2020),
                month: Month::January,
                day: Day::new(9).expect("valid"),
            },
        };
        match changing {
            Stability::UnarchivedAndChanging { retrieved } => {
                assert_eq!(retrieved.year.get(), 2020);
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
        // The year passed here is discarded by the next line in both cases; it
        // is only a placeholder, and `Year` no longer has a spelling for zero.
        let mut undated = dated("u", "Salas", 1999);
        undated.published = Attested::AbsentFromWork;
        let mut unresearched = dated("x", "Salas", 1999);
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
    fn a_given_name_that_cannot_be_rendered_cannot_be_written_down() {
        // The formatter used to reduce given names to initials with a
        // `filter_map`, so a fragment with no initial in it was dropped and the
        // author rendered anyway — `Smith, M.` from a record holding two given
        // names, one of them blank. Each of these is one of those fragments, and
        // none of them is constructible now.
        for refused in ["", "   ", "Jean--Paul", "-Paul", "Jean-", "-", " - "] {
            assert!(
                GivenName::new(refused).is_err(),
                "{refused:?} has a part with no initial in it, so it must not be recordable"
            );
        }
        // Not a spelling rule — a statement that the record has nowhere to put
        // these. APA prints them after the initials, and among the given names
        // they render as an invented middle initial: `Smith, J., Jr.` becomes
        // `Smith, J. J.`.
        for suffix in ["Jr", "Jr.", "jr.", "SR", "Sr.", "II", "III", "IV"] {
            assert!(
                GivenName::new(suffix).is_err(),
                "{suffix:?} is a generational suffix, and this record has no field for one"
            );
        }
        for accepted in [
            "Mary",
            "M.",
            "Jean-Paul",
            "Ibáñez",
            "N'Golo",
            "Iva",
            "Владимир",
        ] {
            assert!(GivenName::new(accepted).is_ok(), "{accepted:?}");
        }
        // Outer whitespace is trimmed, because the renderer trims and a leading
        // space nobody can see must not decide an order somebody reads. Nothing
        // inside the name is touched.
        assert_eq!(given_name("  Mary  ").as_str(), "Mary");
        assert!(GivenName::new("Jean - Paul").is_err());
        // And a file cannot smuggle one past construction either.
        assert!(serde_json::from_str::<GivenName>("\"Jean--Paul\"").is_err());
        assert!(serde_json::from_str::<GivenName>("\"\"").is_err());
        assert_eq!(
            serde_json::from_str::<GivenName>("\"Mary\"").expect("valid"),
            given_name("Mary")
        );
    }

    #[test]
    fn an_initial_is_derived_in_one_place_so_two_readers_cannot_disagree() {
        // `Reference::list_order` alphabetises on this and the formatter prints
        // it. Two copies of the derivation is how the printed order and the
        // sorted order came apart.
        for (recorded, initial) in [
            ("Mary", "M."),
            ("M.", "M."),
            ("Jean-Paul", "J.-P."),
            ("ibáñez", "I."),
        ] {
            assert_eq!(given_name(recorded).initial(), initial, "{recorded:?}");
        }
    }

    #[test]
    fn two_authors_the_page_prints_alike_are_not_ordered_by_what_it_hides() {
        // `Author` permits either `Mary` or `M.`, and both render as `M.` — so
        // these two entries are the same author, spelled two ways, and a reader
        // sees `Smith, M.` twice. Ordering on the *recorded* spelling let `M.`
        // precede `Mary` and put `B title` above `A title`, which is an order
        // nobody reading the page could account for. The key is the rendered
        // initial, so the tie falls through to title, which a reader can see.
        let mut spelled_out = dated("spelled-out", "Smith", 2020);
        spelled_out.authors = vec![Author::Person {
            surname: "Smith".to_owned(),
            given: vec![given_name("Mary")],
        }];
        spelled_out.title = "A title".to_owned();
        let mut initialled = dated("initialled", "Smith", 2020);
        initialled.authors = vec![Author::Person {
            surname: "Smith".to_owned(),
            given: vec![given_name("M.")],
        }];
        initialled.title = "B title".to_owned();

        let mut refs = [initialled, spelled_out];
        refs.sort_by(Reference::list_order);
        assert_eq!(
            refs.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["spelled-out", "initialled"],
            "the title decides, because it is the only difference the page shows"
        );
        // …and a genuine difference of initials still outranks the date, which
        // is the rule this must not have broken on the way past.
        let mut anne = dated("anne", "Smith", 2020);
        anne.authors = vec![Author::Person {
            surname: "Smith".to_owned(),
            given: vec![given_name("Anne")],
        }];
        let mut tom = dated("tom", "Smith", 1990);
        tom.authors = vec![Author::Person {
            surname: "Smith".to_owned(),
            given: vec![given_name("Tom")],
        }];
        let mut people = [tom, anne];
        people.sort_by(Reference::list_order);
        assert_eq!(
            people.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["anne", "tom"]
        );
    }

    #[test]
    fn a_year_that_is_not_a_year_cannot_be_written_down() {
        // A bare `i32` let these through `serde`, and the formatter rendered
        // them as `(0)` and `(-1, January 2)` — strings that are not dates,
        // printed with a date's authority. Year zero is not a year in the
        // numbering APA's readers use, and a negative year is BCE, which APA
        // writes `400 B.C.E.` and never `-400`.
        for refused in [0, -1, -400, i32::MIN] {
            assert!(Year::new(refused).is_none(), "{refused}");
            assert!(
                serde_json::from_str::<Year>(&refused.to_string()).is_err(),
                "deserialisation must validate too: {refused}"
            );
        }
        for accepted in [1, 1899, 2026, i32::MAX] {
            assert_eq!(Year::new(accepted).expect("a valid year").get(), accepted);
        }
        // No upper bound, deliberately: a forthcoming work is a real thing to
        // record, and separating one from a typo needs a clock this crate does
        // not have and must not acquire.
        assert!(Year::new(2999).is_some());
    }

    #[test]
    fn an_identifier_cannot_carry_a_character_a_reader_cannot_see() {
        // Everything that parses here is rendered as a resolver link whose
        // visible text *is* its target, so a suffix carrying a newline is a
        // citation printed across two lines and resolving to neither, and a bidi
        // override is a link that reads as one thing and resolves to another.
        for hidden in [
            "10.1234/foo\nbar",
            "10.1234/foo bar",
            "10.1234/a\tb",
            "10.1234/foo\u{7}bar",
            "10.1234/foo\u{202e}bar",
            "10.1234/foo\u{200b}bar",
            "10.1234/foo\u{feff}bar",
        ] {
            assert!(
                Doi::new(hidden).is_err(),
                "{hidden:?} would become a link that is not what it looks like"
            );
            assert!(
                serde_json::from_str::<Doi>(&serde_json::to_string(hidden).expect("json")).is_err(),
                "deserialisation must validate too: {hidden:?}"
            );
        }
        // The ordinary shapes are untouched — this is not a DOI grammar, and a
        // suffix is allowed to be almost anything that can be printed.
        for fine in [
            "10.3886/ICPSR36966.v1",
            "10.1037/abc123",
            "10.1000.10/123",
            "10.1234/a(b)c;d",
        ] {
            assert!(Doi::new(fine).is_ok(), "{fine:?}");
        }
        assert!(is_printable_identifier("https://example.org/a-b"));
        assert!(!is_printable_identifier("https://example.org/a b"));
    }

    /// Code points worth sweeping a guard against: the ASCII block, Latin-1
    /// (U+00AD SOFT HYPHEN), combining diacritics, the Arabic format controls
    /// (U+061C), general punctuation in full (U+2000–U+206F — every Unicode
    /// space, the zero-width set, the bidi embeddings, overrides and isolates,
    /// the word joiner, and the deprecated U+206A–U+206F block), the ideographic
    /// space, the byte-order mark, interlinear annotation, and the plane-14 tag
    /// characters. Two of these blocks held the characters that walked through a
    /// hand-written denylist; sweeping them is how a guard stops being a list of
    /// what somebody happened to think of.
    fn interesting_code_points() -> impl Iterator<Item = char> {
        [
            0x0000..=0x00FF_u32,
            0x0300..=0x036F,
            0x0590..=0x0620,
            0x2000..=0x2070,
            0x3000..=0x3002,
            0xFEFF..=0xFEFF,
            0xFFF9..=0xFFFC,
            0xE0000..=0xE0080,
        ]
        .into_iter()
        .flatten()
        .filter_map(char::from_u32)
    }

    #[test]
    fn a_given_name_is_accepted_whole_or_not_at_all() {
        // The invariant, rather than the four inputs review happened to name:
        // for *any* code point, putting it inside a name either refuses the name
        // or renders it losslessly. What must never happen is the third outcome
        // — accepted, and rendered with part of it gone — which is how both
        // `["Mary", ""]` and `"Mary Ann"` got through.
        let mut accepted = 0_u32;
        for c in interesting_code_points() {
            let candidate = format!("Ma{c}ry");
            let Ok(name) = GivenName::new(&candidate) else {
                continue;
            };
            accepted += 1;
            assert_eq!(
                name.as_str(),
                candidate,
                "U+{:04X} was accepted, so it must be stored exactly",
                c as u32
            );
            // One initial per part, always. A separator that slipped through
            // the allowlist would show up here as a name with more words than
            // the rendering has initials — which is the drop, stated as a count.
            // The hyphen itself is in the sweep and legitimately makes two
            // parts, so the rule is a ratio rather than a constant.
            assert_eq!(
                name.initial().matches('.').count(),
                candidate.split('-').count(),
                "U+{:04X}: {candidate:?} rendered {:?}, losing a part",
                c as u32,
                name.initial()
            );
            assert!(
                !candidate.chars().any(char::is_whitespace),
                "U+{:04X} is whitespace and must not be inside one name",
                c as u32
            );
            // Two parts in, two initials out — the hyphen case, swept the same
            // way, since that is where the empty-part drop lived.
            let joined = format!("Ma{c}ry-Jo");
            let hyphenated = GivenName::new(&joined).expect("a valid given name");
            assert_eq!(
                hyphenated.initial().matches('.').count(),
                joined.split('-').count(),
                "U+{:04X}: {joined:?} rendered {:?}, losing a part",
                c as u32,
                hyphenated.initial()
            );
        }
        // Not vacuous: the sweep really does accept letters and marks, so this
        // is a test of an allowlist rather than of a `return false`.
        assert!(
            accepted > 100,
            "the sweep accepted only {accepted} code points — the allowlist cannot be that narrow"
        );
    }

    #[test]
    fn a_doi_is_accepted_whole_or_not_at_all() {
        // The same invariant for the identifier that becomes a link: any code
        // point either refuses the DOI, or survives into a URL that still names
        // the same record. Nothing may be accepted and then quietly mean
        // something else — which is what `#` did, by becoming a fragment the
        // resolver never sees.
        let mut accepted = 0_u32;
        for c in interesting_code_points() {
            let candidate = format!("10.1234/a{c}b");
            let Ok(doi) = Doi::new(&candidate) else {
                continue;
            };
            accepted += 1;
            assert_eq!(doi.as_str(), "10.1234/a{c}b".replace("{c}", &c.to_string()));
            // The recorded identifier is itself printable. Encoding makes the
            // *link* safe, but a bare DOI gets printed too, and a citation
            // carrying a character nobody can see is not one anybody can check.
            // The recorded form is the identifier, so it may hold any letter
            // or digit — what must hold is that it carries nothing invisible.
            // Spelled out rather than asked of the production predicate: a
            // property checked through the same function that enforces it
            // cancels on both sides, and would pass however that function broke.
            assert!(
                doi.as_str()
                    .chars()
                    .all(|c| c.is_ascii_graphic() || c.is_alphanumeric()),
                "U+{:04X} was accepted into a DOI but cannot be printed",
                c as u32
            );
            let url = doi.url();
            let rest = url
                .strip_prefix("https://doi.org/")
                .expect("the resolver prefix");
            // Every character of the rendered link is one a reader can see and a
            // URL carries unambiguously — no fragment, no query, no bare escape.
            assert!(
                rest.chars().all(|c| c.is_ascii_graphic()),
                "U+{:04X} left something unprintable in {url:?}",
                c as u32
            );
            for reserved in ['#', '?'] {
                assert!(
                    !rest.contains(reserved),
                    "U+{:04X} left a bare {reserved:?} in {url:?}, which the resolver never receives",
                    c as u32
                );
            }
            // And the encoding is reversible, so the link names the DOI that was
            // recorded rather than one that merely looks like it.
            assert_eq!(
                decode_escapes_independently(rest),
                doi.as_str(),
                "U+{:04X}: the URL must decode back to the recorded DOI",
                c as u32
            );
            // The same property through the public API, which is what a caller
            // actually round-trips: rendering a DOI and parsing the result must
            // give the DOI back. This is what makes `new` and `url` inverses
            // rather than two functions that happen to agree on easy input.
            assert_eq!(
                Doi::new(&url).expect("a URL this type produced"),
                doi,
                "U+{:04X}: {url:?} did not parse back to the DOI it renders",
                c as u32
            );
        }
        assert!(
            accepted > 50,
            "the sweep accepted only {accepted} code points — too narrow to be testing an allowlist"
        );
    }

    /// Reverse [`Doi::url`]'s escaping, so a test can prove the encoding is
    /// lossless rather than merely well-formed.
    ///
    /// Deliberately a **second implementation**, not the production
    /// `percent_decode`. A round trip checked with the decoder the encoder
    /// ships with passes whenever the two share a defect — they cancel, and the
    /// test proves only that the module agrees with itself. This one is written
    /// independently, so the byte-level property is checked against something
    /// other than the code under test. The API-level round trip below
    /// (`Doi::new(&url) == doi`) uses the production pair on purpose, because
    /// *that* pairing is the guarantee a caller relies on.
    fn decode_escapes_independently(text: &str) -> String {
        let mut bytes = Vec::new();
        let mut rest = text.as_bytes();
        while let Some((first, tail)) = rest.split_first() {
            if *first == b'%' && tail.len() >= 2 {
                let hex = std::str::from_utf8(&tail[..2]).expect("ascii hex");
                bytes.push(u8::from_str_radix(hex, 16).expect("valid escape"));
                rest = &tail[2..];
            } else {
                bytes.push(*first);
                rest = tail;
            }
        }
        String::from_utf8(bytes).expect("valid utf-8")
    }

    #[test]
    fn a_doi_url_names_the_record_that_was_recorded() {
        // The four URI delimiters, by name. `#` is the one that matters: raw, it
        // is an HTTP fragment, so `https://doi.org/10.1234/a#b` asks the resolver
        // for `10.1234/a` — a different record, retrieved silently and with every
        // appearance of working.
        for (recorded, expected) in [
            ("10.1234/a#b", "https://doi.org/10.1234/a%23b"),
            ("10.1234/a?b", "https://doi.org/10.1234/a%3Fb"),
            ("10.1234/a%b", "https://doi.org/10.1234/a%25b"),
            ("10.1234/a\"b", "https://doi.org/10.1234/a%22b"),
        ] {
            let doi = Doi::new(recorded).expect("a printable DOI");
            assert_eq!(doi.as_str(), recorded, "the record keeps what was written");
            assert_eq!(doi.url(), expected);
        }
        // A real, registered Wiley DOI: `<`, `>`, `(`, `;` and `:` are ordinary
        // in the wild, which is why this encodes rather than refuses. The
        // sub-delims pass through; the two angle brackets are escaped.
        let wiley = Doi::new("10.1002/(SICI)1097-0258(19970815)16:15<1707::AID-SIM605>3.0.CO;2-Y")
            .expect("a real DOI");
        assert_eq!(
            wiley.url(),
            "https://doi.org/10.1002/(SICI)1097-0258(19970815)16:15%3C1707::AID-SIM605%3E3.0.CO;2-Y"
        );
        // The ordinary shapes are untouched, so this costs nothing in the common
        // case.
        assert_eq!(
            Doi::new("10.3886/ICPSR36966.v1").expect("valid").url(),
            "https://doi.org/10.3886/ICPSR36966.v1"
        );
    }

    #[test]
    fn what_is_stored_is_the_identifier_and_never_its_url_form() {
        // The two halves of this type have to agree on what the recorded value
        // *is*, and for one round they did not: the suffix rule admitted `%`
        // and the docs said a non-ASCII DOI should be recorded percent-encoded,
        // while `url` assumed the recorded form was raw and encoded the `%`
        // again — so `10.1234/a%23b` rendered `…a%2523b` and resolved to a
        // different record. The rule is now one sentence: what is stored is the
        // DOI name, raw.

        // A resolver URL carries the *encoded* form, so parsing one decodes it.
        // Both spellings of one DOI therefore land on the same value.
        let from_url = Doi::new("https://doi.org/10.1234/a%23b").expect("a resolver URL");
        let from_bare = Doi::new("doi:10.1234/a#b").expect("the identifier");
        assert_eq!(from_url, from_bare, "one DOI, two ways of writing it down");
        assert_eq!(from_url.as_str(), "10.1234/a#b", "stored raw, not encoded");
        assert_eq!(from_url.url(), "https://doi.org/10.1234/a%23b");

        // `new` and `url` are inverses, which is the property that makes the
        // link name the record it claims to.
        for recorded in [
            "10.1234/a#b",
            "10.1234/a%b",
            "10.1234/a?b",
            "10.1234/plain",
            "10.1234/ünïcode",
        ] {
            let doi = Doi::new(recorded).expect("a DOI");
            assert_eq!(doi.as_str(), recorded);
            assert_eq!(
                Doi::new(&doi.url()).expect("its own URL"),
                doi,
                "{recorded:?} did not survive a render-and-reparse"
            );
        }
        // A literal `%` is kept apart from an escape, which the encoded-storage
        // reading could not do: these are two different DOIs.
        assert_ne!(
            Doi::new("10.1234/a%23b").expect("a literal percent"),
            Doi::new("10.1234/a#b").expect("a literal hash")
        );
        // A malformed escape in a resolver URL is refused rather than guessed at.
        for broken in [
            "https://doi.org/10.1234/a%2",
            "https://doi.org/10.1234/a%zzb",
            "https://doi.org/10.1234/a%",
        ] {
            assert!(Doi::new(broken).is_err(), "{broken:?}");
        }
    }

    #[test]
    fn a_non_ascii_doi_is_recordable_as_itself() {
        // The DOI Handbook lets a DOI name incorporate any printable character
        // from the Unicode Standard, so these are legal identifiers and refusing
        // them would be this type inventing a rule the standard does not have.
        // They are safe to admit because a DOI is never emitted as recorded — it
        // reaches a page only through `url`, which encodes.
        for recorded in ["10.1234/中文", "10.1234/ünïcode", "10.1234/абв"] {
            let doi = Doi::new(recorded).expect("a legal DOI name");
            assert_eq!(doi.as_str(), recorded, "recorded as the identifier itself");
            let url = doi.url();
            assert!(
                url.chars().all(|c| c.is_ascii_graphic()),
                "the rendered link is always ASCII: {url:?}"
            );
            assert_eq!(Doi::new(&url).expect("its own URL"), doi);
        }
        // A *letter* is admitted; an invisible character still is not, whichever
        // block it comes from.
        for hidden in ["10.1234/a\u{200b}b", "10.1234/a\u{061c}b", "10.1234/a b"] {
            assert!(Doi::new(hidden).is_err(), "{hidden:?}");
        }
    }

    #[test]
    fn every_public_type_here_is_reachable_from_the_crate_root() {
        // `Year`, `GivenName` and their error types were public, constructible
        // and *not* re-exported, so a downstream caller could name the field
        // types of `PublicationDate` and `Author::Person` but could not build
        // one through the crate's own API. The assertion is the type check: this
        // names every public item of this module by its root path, so adding one
        // without re-exporting it stops compiling here.
        fn assert_reachable<T>() {}
        assert_reachable::<crate::Attested<String>>();
        assert_reachable::<crate::Month>();
        assert_reachable::<crate::Day>();
        assert_reachable::<crate::NotADay>();
        assert_reachable::<crate::Year>();
        assert_reachable::<crate::NotAYear>();
        assert_reachable::<crate::PublicationDate>();
        assert_reachable::<crate::AccessDate>();
        assert_reachable::<crate::Stability>();
        assert_reachable::<crate::GivenName>();
        assert_reachable::<crate::NotAGivenName>();
        assert_reachable::<crate::Author>();
        assert_reachable::<crate::Doi>();
        assert_reachable::<crate::NotADoi>();
        assert_reachable::<crate::Locator>();
        assert_reachable::<crate::WorkKind>();
        assert_reachable::<crate::Reference>();
        // A URL, not a DOI: this predicate is the rule for a value emitted
        // verbatim, and reaching for it with a DOI-shaped string is the
        // confusion the split exists to prevent.
        assert!(crate::is_printable_identifier("https://example.invalid/ok"));

        // And the round trip that omission actually blocked: building the two
        // public shapes entirely through the crate root.
        let published = crate::PublicationDate::Year(crate::Year::new(2020).expect("a year"));
        let author = crate::Author::Person {
            surname: "Luna".to_owned(),
            given: vec![crate::GivenName::new("R").expect("a given name")],
        };
        assert_eq!(published.year().get(), 2020);
        assert_eq!(author.sort_key(), "Luna");
    }

    #[test]
    fn a_locator_cannot_carry_a_character_that_escapes_an_attribute() {
        // This crate's output is structured spans destined for a web UI, so an
        // `href` is a value somebody interpolates into markup. ASCII-graphic was
        // the wrong bar: it admits the nine characters RFC 3986 excludes from a
        // URI outright, and `"` among them turns a locator into an attribute.
        for escaping in [
            "https://example.invalid/a\"onmouseover=\"alert(1)",
            "https://example.invalid/a\"",
            "https://example.invalid/a<script>",
            "https://example.invalid/a>b",
            "https://example.invalid/a\\b",
            "https://example.invalid/a{b}",
            "https://example.invalid/a^b",
            "https://example.invalid/a|b",
            "https://example.invalid/a`b",
        ] {
            assert!(
                !is_printable_identifier(escaping),
                "{escaping:?} can escape a quoted attribute and must not be a link target"
            );
        }
        // The characters a URL actually needs are all still there, including the
        // two the contract explicitly does *not* discharge the consumer of.
        for ordinary in [
            "https://example.invalid/a?b=c&d=e#f",
            "https://example.invalid/~user/a_b-c.d",
            "https://example.invalid/a'b",
            "https://example.invalid/(a)+b,c;d=e!f$g*h",
            "https://example.invalid/a%20b",
            "https://[::1]:8080/x",
        ] {
            assert!(is_printable_identifier(ordinary), "{ordinary:?}");
        }
    }

    #[test]
    fn a_percent_that_introduces_nothing_is_not_a_uri() {
        // `%` is a legal URI character, so the character allowlist admits it —
        // but only as the introducer of an escape, and whether it is one is a
        // property of the three characters together. A lone `%` makes the string
        // not a URI, so a predicate promising "only characters RFC 3986 permits
        // in a URI" was not telling the truth about these.
        for malformed in [
            "https://example.invalid/a%ZZ",
            "https://example.invalid/a%",
            "https://example.invalid/a%2",
            "https://example.invalid/%",
            "https://example.invalid/a%g0b",
        ] {
            assert!(
                !is_printable_identifier(malformed),
                "{malformed:?} carries a `%` that introduces no escape"
            );
        }
        // Well-formed escapes are untouched, in either case, and so is a `%`
        // that this type itself emits.
        for fine in [
            "https://example.invalid/a%20b",
            "https://example.invalid/100%25",
            "https://example.invalid/a%2Fb",
            "https://example.invalid/a%2fb",
        ] {
            assert!(is_printable_identifier(fine), "{fine:?}");
        }
        // Every URL `Doi::url` can produce satisfies it, which is what keeps the
        // two halves of this module consistent.
        for recorded in [
            "10.1234/a#b",
            "10.1234/a%b",
            "10.1234/中文",
            "10.1234/plain",
        ] {
            let url = Doi::new(recorded).expect("a DOI").url();
            assert!(is_printable_identifier(&url), "{url:?}");
        }
    }

    #[test]
    fn a_resolver_url_ends_at_a_query_or_a_fragment() {
        // In a URL `?` opens a query and `#` opens a fragment, and a fragment is
        // never sent to the server — so `https://doi.org/10.1234/a#b` asks the
        // resolver for `10.1234/a`. Storing `10.1234/a#b` recorded a DOI nobody
        // requested and re-emitted it as `…/a%23b`, a third record again.
        for (url, named) in [
            ("https://doi.org/10.1234/a#b", "10.1234/a"),
            ("https://doi.org/10.1234/a?b", "10.1234/a"),
            ("https://doi.org/10.1234/a#b?c", "10.1234/a"),
            ("https://doi.org/10.1234/a%23b", "10.1234/a#b"),
            ("https://doi.org/10.1234/plain", "10.1234/plain"),
        ] {
            assert_eq!(
                Doi::new(url).expect("a resolver URL").as_str(),
                named,
                "{url:?}"
            );
        }
        // The two input forms disagree on purpose: bare, `#` is a character of
        // the name; in a URL it is a delimiter. They are different statements.
        assert_ne!(
            Doi::new("10.1234/a#b").expect("a bare name"),
            Doi::new("https://doi.org/10.1234/a#b").expect("a URL")
        );
        // And the round trip holds for URL-shaped input too, which is the form
        // the property was not previously stated over.
        for recorded in ["10.1234/a#b", "10.1234/a?b", "10.1234/a%b"] {
            let doi = Doi::new(recorded).expect("a DOI");
            assert_eq!(
                Doi::new(&doi.url()).expect("its own URL"),
                doi,
                "{recorded:?} did not survive render-and-reparse as a URL"
            );
        }
    }

    #[test]
    fn a_registrant_code_is_not_held_to_a_length_convention() {
        // Registrant codes are four digits or more in practice, but that is how
        // they have been assigned rather than a rule of the syntax — no digit
        // count is stated by the standards, and secondary sources say the prefix
        // "usually" takes the form `10.NNNN`. Enforcing the convention would
        // refuse a registered identifier on no authority, which is a worse
        // failure than accepting an unassigned one.
        for short in ["10.1/x", "10.12/x", "10.123/x"] {
            assert!(Doi::new(short).is_ok(), "{short:?}");
        }
        assert!(Doi::new("10.1000/182").is_ok(), "the DOI Foundation's own");
        // The shape rules that *are* stated still hold.
        for malformed in ["10./x", "10.a/x", "10.1./x", "11.1234/x", "10.1234"] {
            assert!(Doi::new(malformed).is_err(), "{malformed:?}");
        }
    }

    #[test]
    fn a_date_that_did_not_happen_is_not_a_date() {
        // `Day` validates 1..=31 because it carries no calendar. `February 31`
        // is only impossible once the month is in hand, so the composite has to
        // ask — the same shape as every other invariant enforced on a part while
        // the whole went unchecked.
        let day = |d: u8| Day::new(d).expect("a day");
        for (y, m, d) in [
            (2021, Month::February, 31),
            (2021, Month::February, 30),
            (2021, Month::February, 29),
            (2021, Month::April, 31),
            (2021, Month::June, 31),
            (2021, Month::September, 31),
            (2021, Month::November, 31),
        ] {
            let date = PublicationDate::Full {
                year: year(y),
                month: m,
                day: day(d),
            };
            assert!(!date.names_a_day_that_exists(), "{m:?} {d}, {y}");
            assert!(
                !AccessDate {
                    year: year(y),
                    month: m,
                    day: day(d)
                }
                .names_a_day_that_exists(),
                "a retrieval date shares the calendar: {m:?} {d}, {y}"
            );
        }
        // Leap years, both rules of them.
        for (y, exists) in [(2020, true), (2021, false), (2000, true), (1900, false)] {
            let date = PublicationDate::Full {
                year: year(y),
                month: Month::February,
                day: day(29),
            };
            assert_eq!(date.names_a_day_that_exists(), exists, "29 February {y}");
        }
        // Ordinary dates, and the precisions that name no day at all.
        assert!(
            PublicationDate::Full {
                year: year(2021),
                month: Month::January,
                day: day(31)
            }
            .names_a_day_that_exists()
        );
        assert!(PublicationDate::Year(year(2021)).names_a_day_that_exists());
        assert!(
            PublicationDate::YearMonth {
                year: year(2021),
                month: Month::February
            }
            .names_a_day_that_exists()
        );
    }

    #[test]
    fn apa_alphabetises_letter_by_letter_ignoring_punctuation() {
        // APA means this literally: spaces and punctuation are skipped, so the
        // order is Ol, OM, ON. Keeping the apostrophe in the key put both
        // O'-names above `Olsen`, because `'` sorts below every letter.
        let named = |id: &str, surname: &str| {
            let mut reference = dated(id, surname, 2020);
            reference.authors = vec![Author::Person {
                surname: surname.to_owned(),
                given: vec![given_name("A")],
            }];
            reference
        };
        let mut refs = [
            named("oneil", "O'Neil"),
            named("olsen", "Olsen"),
            named("omalley", "O'Malley"),
        ];
        refs.sort_by(Reference::list_order);
        assert_eq!(
            refs.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["olsen", "omalley", "oneil"]
        );
        // Two names differing only in punctuation are still ordered, and
        // deterministically, because the unfolded key breaks the tie.
        let mut ties = [named("with", "O'Neil"), named("without", "ONeil")];
        ties.sort_by(Reference::list_order);
        let order: Vec<&str> = ties.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(order.len(), 2);
        assert_ne!(
            Reference::list_order(&ties[0], &ties[1]),
            Ordering::Equal,
            "the fold must not collapse two distinct names into one position"
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
            year: year(2020),
            month: Month::December,
        });
        let mut january = dated("january", "Salas", 2020);
        // A title that sorts *after* December's, so the assertion fails if the
        // date key gives up at the year and lets the title decide.
        january.title = "Z title".to_owned();
        january.published = Attested::Known(PublicationDate::Full {
            year: year(2020),
            month: Month::January,
            day: Day::new(9).expect("valid"),
        });

        let mut refs = [december, january];
        refs.sort_by(Reference::list_order);
        let order: Vec<&str> = refs.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(order, ["january", "december"]);
    }

    #[test]
    fn two_people_with_one_surname_are_ordered_by_their_given_names() {
        // A surname alone cannot separate two different people, and APA orders
        // them by initials before it looks at the date — so A. Smith's later
        // work still precedes T. Smith's earlier one.
        let mut anne = dated("anne", "Smith", 2020);
        anne.authors = vec![Author::Person {
            surname: "Smith".to_owned(),
            given: vec![given_name("Anne")],
        }];
        let mut tom = dated("tom", "Smith", 1990);
        tom.authors = vec![Author::Person {
            surname: "Smith".to_owned(),
            given: vec![given_name("Tom")],
        }];

        let mut refs = [tom, anne];
        refs.sort_by(Reference::list_order);
        assert_eq!(
            refs.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["anne", "tom"]
        );
    }

    #[test]
    fn whitespace_nobody_can_see_does_not_decide_the_order() {
        // The renderer trims the author names and the title, so a leading space
        // changes where an entry sorts without changing a character of what is
        // printed — two lists that render identically in a different order.
        let padded = {
            let mut r = dated("padded", "  Salas  ", 2020);
            r.title = "  A title  ".to_owned();
            r
        };
        let tidy = dated("padded", "Salas", 2020);
        assert_eq!(
            Reference::list_order(&padded, &tidy),
            Reference::list_order(&tidy, &padded).reverse(),
            "the comparison is symmetric"
        );
        let mut zhang = dated("zhang", "Zhang", 2020);
        zhang.title = "A title".to_owned();
        let mut refs = [zhang, padded];
        refs.sort_by(Reference::list_order);
        assert_eq!(
            refs.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["padded", "zhang"],
            "a padded `Salas` still sorts before `Zhang`"
        );
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

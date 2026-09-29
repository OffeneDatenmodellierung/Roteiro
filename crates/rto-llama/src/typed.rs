//! Typed questions: ask for one of a closed set of answers and read the answer
//! off the model's own label-token distribution, parsing no text (issue #897).
//!
//! # The property this buys
//!
//! A generated answer is a string, and a string need not be a member of the set
//! you asked about. Every mitigation for that is downstream of the text: a
//! grammar that constrains sampling, a parser that rejects, a retry that hopes.
//! `rto-serve` already declined the middle one on its own terms —
//! `openai_params.rs` refuses `response_format: json_schema` because *"there is
//! no grammar-constrained sampling on this endpoint, so … would return text that
//! need not parse"*.
//!
//! Reading the answer off the distribution is a **stronger** form of that fix,
//! because it removes the text rather than constraining it. One decode of the
//! prompt, then the logit at each option's marker token, softmaxed over those
//! markers alone. The argmax is an **index into the caller's own option list**,
//! so [`Answer::value`] hands back a `&T` the caller put there — and there is no
//! `String` path out of this module's return types at all. An off-schema answer
//! is therefore not *rejected*; it is not **representable**.
//!
//! That is arithmetic over logits and nothing else. It is not a property of any
//! particular model, nor of the project the interface was taken from: no new
//! dependency enters the build for it, because `llama-cpp-2` already exposes
//! `get_logits_ith`, and `crate::llama::LlamaEngine::ask_choice` is where the
//! four lines of arithmetic below meet it.
//!
//! # The number is `sharpness`, and calling it confidence would be wrong
//!
//! [`Answer::sharpness`] is the normalised-entropy figure `1 − H(p)/ln K`: `1.0`
//! when all the mass is on one option, `0.0` when the distribution is uniform.
//! It is **not** a calibrated probability that the answer is correct, and this
//! module will not name it as one.
//!
//! The interface's upstream ships per-model *fitted* parameters for exactly this
//! reason — one of its models carries "calibration per option count", another a
//! single fitted temperature of `1.532` — which is an admission that the raw
//! number is not calibrated by construction. Nothing here has been fitted
//! against a labelled set, so the honest reading of a high `sharpness` is "the
//! model's mass was concentrated", not "the model was probably right". Those
//! come apart precisely where it matters: a model that answers the same thing to
//! almost every question is *sharp* and *uninformative* at once, and this
//! repository has already measured a reviewer doing that — 71% of 1,995 findings
//! carried one class label of fourteen.
//!
//! Fit it and the fitted number may be called a confidence. Until then the name
//! states what was computed.
//!
//! # And on the models here it is worse than uncalibrated: it is saturated
//!
//! Measured, not feared. Asked its 14-way class question about each of the 27
//! rows of this repository's adjudicated review corpus, `qwen3-coder-30b-a3b`
//! returned a sharpness of **exactly `1.0` on 26 of 27** — including on four of
//! the five it got wrong. The classifier itself was good (22 of 27 against an
//! exact permutation null of 2.85), so the answer carries information while the
//! *sharpness of the answer* carries almost none: there is nothing left in it to
//! rank by.
//!
//! That is what a greedy logit distribution from a large model looks like. The
//! top option's logit leads the runner-up by tens of nats, `exp(-30)` is zero to
//! any purpose, and `H(p)` is then zero whatever the question was. A
//! **temperature** on the label logits is the thing that would unsaturate it —
//! which is precisely the fitted parameter the upstream ships and this does not.
//!
//! [`Answer::margin`] exists because of that measurement. It is the same
//! information on a scale that cannot saturate, and it is what distinguishes *the
//! number has no scale here* from *there is no signal here* — two conclusions the
//! entropy figure alone cannot tell apart. Nothing in this module applies a
//! temperature, because choosing one is a fit and a fit needs a labelled set
//! bigger than 27 rows.
//!
//! **And the answer turned out to be "it depends on the task", so the margin does
//! not inherit a general licence.** Asked a fourteen-option class question, it
//! ordered a correct answer above a wrong one on human-written corpus descriptions
//! at 0.81, and ordered the model's *own* 446 findings no better than chance (0.52
//! against a null of 0.4999, `P = 0.4411`). Both figures are on [`Answer::margin`],
//! with what separates them; read them before ranking anything by it.
//!
//! # `option_mass` is what stops the guarantee being vacuous
//!
//! "Off-schema is unrepresentable" is a claim about the return type, and a
//! return type cannot tell you the question was understood. Softmax over a
//! 14-element subset of a 150,000-element vocabulary always yields a
//! distribution, so a model that wanted to answer `<think>`, or `Sorry`, or a
//! newline still produces a confident-looking winner among the markers it never
//! considered.
//!
//! [`Answer::option_mass`] is the same logits softmaxed over the **whole**
//! vocabulary and then summed across the markers only — how much of the model's
//! actual next-token belief was on any legal answer. A reading with
//! `option_mass` near zero is a value in the closed set whose closedness did no
//! work, and a caller that ignores it has re-created the failure by a different
//! route. It is carried on the answer rather than checked here because this
//! module does not know what a caller should do about it, and a threshold chosen
//! here would be a claim about every model at once.
//!
//! A **reasoning** GGUF is the concrete case: its first generated token is
//! `<think>`, so `option_mass` is the whole of the signal that something is
//! wrong, and it is wrong in a way no amount of sharpness reveals.
//!
//! # Markers rather than the option words
//!
//! The reading needs one token per option, distinct across options. The option
//! *words* do not have that property: `contract-drift` and `cleanup-gap` are
//! several tokens each under a BPE vocabulary, and two options whose text shares
//! a first token are indistinguishable at the position being read. Scoring each
//! option's full token sequence instead would mean K decodes rather than one,
//! which is the cost this interface exists to avoid.
//!
//! So [`Choice`] assigns each option a single ASCII letter — `A`, `B`, `C` — and
//! renders the mapping into the prompt itself ([`Choice::render`]). Distinctness
//! is then structural rather than hoped for, and the caller never sees a marker:
//! it supplies `(label, value)` pairs and gets a `&T` back.
//!
//! Whether each letter is in fact a *single* token is a property of the model's
//! tokeniser, which this module cannot see. That check belongs to — and is made
//! by — the engine that has one.

use std::fmt::Write as _;

/// The most options one [`Choice`] may carry: one per ASCII letter.
///
/// Not a tuning parameter. `A`–`Z` is the marker alphabet, so 26 is where the
/// scheme runs out, and a 27th option has nowhere to go. The closed
/// `DefectClass` set this was built for has 14 members.
pub const MAX_OPTIONS: usize = 26;

/// Why a typed question could not be built.
///
/// Every variant is a statement about the *question*, made once at construction,
/// so that a built [`Choice`] is a question that can be asked. None of them can
/// arise from a model's answer — that is the point of the module.
///
/// `#[non_exhaustive]`, and not merely for form: the checks this module can make
/// are bounded by what it can see, and the one it *cannot* make — whether a
/// marker letter is a single token — belongs to whatever holds a tokeniser. If
/// that ever moves in here, or a new structural check is added, it arrives as a
/// variant. These crates are published, so the attribute is what keeps that a
/// minor change.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TypedError {
    /// Fewer than two options. A question with one answer is not a question, and
    /// the entropy figure it would report (`1.0`, maximally sharp) would be a
    /// statement about the option count rather than about the model.
    #[error("a choice needs at least two options, got {0}")]
    TooFewOptions(usize),
    /// More options than there are markers.
    #[error("a choice may carry at most {MAX_OPTIONS} options, got {0}")]
    TooManyOptions(usize),
    /// An option label was empty or whitespace. The labels are rendered into the
    /// prompt as the thing the model is choosing between; a blank one asks it to
    /// pick nothing in particular.
    #[error("option {0} has an empty label")]
    EmptyLabel(usize),
    /// Two options carry the same label. They would be different answers the
    /// model cannot tell apart, so the distribution over them would be
    /// meaningless however sharp it looked.
    #[error("options {0} and {1} share the label `{2}`")]
    DuplicateLabel(usize, usize, String),
    /// A label carries a control character — a newline, most consequentially.
    ///
    /// [`Choice::render`] writes **one `A) label` per line**, so a label
    /// containing a newline forges a second apparent mapping: `"safe\nB) forged"`
    /// puts a `B)` line in the prompt that belongs to option `A`. The prompt the
    /// model reads then disagrees with the marker-to-index mapping the logits are
    /// gathered against, and the answer is a confident reading of the wrong
    /// option.
    ///
    /// That is precisely the failure the module claims to have removed by having
    /// one piece of code both assign the markers and render them, so it is
    /// refused at construction rather than escaped at render time: an escaped
    /// newline would keep the grammar intact while showing the model a label its
    /// author did not write.
    #[error(
        "option {0}'s label contains a control character (U+{1:04X}), which would break \
         the one-line-per-option prompt grammar"
    )]
    ControlCharacter(usize, u32),
}

/// One option: what the model is shown, and what the caller gets back.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Opt<T> {
    /// The single ASCII letter this option is read off.
    marker: char,
    /// What the model is shown beside the marker.
    label: String,
    /// What [`Answer::value`] returns if this option wins. Never rendered.
    value: T,
}

/// A closed question: pick one of these named options.
///
/// Built once and asked many times. Construction is the only place an invalid
/// question can be reported, which is what makes the answer's membership in the
/// option set a property of the types rather than a check after the fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice<T> {
    options: Vec<Opt<T>>,
}

impl<T> Choice<T> {
    /// Build a question from `(label, value)` pairs, in the order they should be
    /// shown.
    ///
    /// Order is the caller's and is preserved: [`Answer::outcomes`] reports the
    /// distribution in it, so two answers to the same question line up field by
    /// field. Markers are assigned from `A` in that same order.
    ///
    /// # Errors
    /// [`TypedError`] if the option set is not a question that can be asked —
    /// too few, too many, blank labels, or two options a model could not tell
    /// apart. See that type; each variant says what it is about.
    pub fn new(options: impl IntoIterator<Item = (String, T)>) -> Result<Self, TypedError> {
        let pairs: Vec<(String, T)> = options.into_iter().collect();
        if pairs.len() < 2 {
            return Err(TypedError::TooFewOptions(pairs.len()));
        }
        if pairs.len() > MAX_OPTIONS {
            return Err(TypedError::TooManyOptions(pairs.len()));
        }
        for (i, (label, _)) in pairs.iter().enumerate() {
            if label.trim().is_empty() {
                return Err(TypedError::EmptyLabel(i));
            }
            if let Some(c) = label.chars().find(|c| c.is_control()) {
                return Err(TypedError::ControlCharacter(i, c as u32));
            }
            // Quadratic over at most 26 labels, which is cheaper than the
            // allocation a set would take and reports *both* indices — the thing
            // a caller staring at two identical strings needs.
            if let Some(j) = pairs[..i].iter().position(|(other, _)| other == label) {
                return Err(TypedError::DuplicateLabel(j, i, label.clone()));
            }
        }
        let options = pairs
            .into_iter()
            .enumerate()
            .map(|(i, (label, value))| Opt {
                // `i < MAX_OPTIONS` holds above, so this stays inside `A`–`Z`.
                marker: char::from(b'A' + u8::try_from(i).unwrap_or(0)),
                label,
                value,
            })
            .collect();
        Ok(Self { options })
    }

    /// How many options this question carries — the `K` in `1 − H(p)/ln K`.
    #[must_use]
    pub fn len(&self) -> usize {
        self.options.len()
    }

    /// Never true: [`Choice::new`] refuses fewer than two options.
    ///
    /// Present because clippy asks for it beside [`Choice::len`], and answering
    /// it honestly is cheaper than suppressing the lint.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.options.is_empty()
    }

    /// The marker letters, in option order — what a reader of the distribution
    /// has to look up in the model's vocabulary.
    ///
    /// For the engine. A caller asking a question does not need them.
    pub fn markers(&self) -> impl Iterator<Item = char> + '_ {
        self.options.iter().map(|o| o.marker)
    }

    /// The options as the prompt states them: one `A) label` per line.
    ///
    /// The mapping is rendered rather than assumed because the model is being
    /// asked to answer with a letter, and a letter means nothing unless the
    /// prompt said what it stands for.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        for o in &self.options {
            let _ = writeln!(out, "{}) {}", o.marker, o.label);
        }
        out
    }

    /// The whole user turn for this question over `state`: the state, the
    /// question, and the marker-to-label mapping.
    ///
    /// Built here rather than in the engine so that what the model is shown is
    /// testable in a build with no llama.cpp, and so that the mapping the reading
    /// depends on is written by the same code that assigned it. Those two coming
    /// apart is the one way this scheme fails silently: a prompt listing the
    /// options in one order while the logits are gathered in another reads a
    /// confident answer off the wrong letter.
    ///
    /// It states the one-letter instruction **after** the question and before the
    /// list, so that the last thing before the model's turn is the list itself.
    /// The instruction is not what makes the answer in-schema — the return type
    /// is — but a model told to reply with a letter puts more of its mass on one,
    /// and [`Answer::option_mass`] is how much.
    #[must_use]
    pub fn prompt(&self, state: &str, question: &str) -> String {
        format!(
            "{}\n\n{}\n\nReply with exactly one letter and nothing else:\n\n{}",
            state.trim_end(),
            question.trim(),
            self.render(),
        )
    }

    /// Turn a probability per option — in this question's own order — into an
    /// [`Answer`].
    ///
    /// `probs` is expected to be normalised over the options and `full_vocab` to
    /// be the same mass measured against the whole vocabulary; neither is
    /// re-derived here, because this module has no logits. The engine that does
    /// is the only caller.
    ///
    /// Ties go to the earlier option, which is the caller's own order. That is
    /// arbitrary but it is *stated* and deterministic, which a coin flip would
    /// not be.
    fn answer_from(&self, probs: &[f32], full_vocab: f32, margin: f32) -> Answer<T>
    where
        T: Clone,
    {
        debug_assert_eq!(
            probs.len(),
            self.options.len(),
            "one probability per option"
        );
        let best = probs
            .iter()
            .enumerate()
            .fold((0usize, f32::NEG_INFINITY), |(bi, bp), (i, &p)| {
                if p > bp { (i, p) } else { (bi, bp) }
            })
            .0;
        Answer {
            outcomes: self
                .options
                .iter()
                .zip(probs)
                .map(|(o, &p)| (o.label.clone(), p))
                .collect(),
            sharpness: sharpness(probs),
            option_mass: full_vocab,
            best_probability: probs.get(best).copied().unwrap_or(0.0),
            margin,
            value: self.options[best].value.clone(),
        }
    }
}

/// An answer to a [`Choice`]: which option won, the whole distribution, and two
/// numbers about the shape of it.
///
/// There is no constructor and no public field, and [`Answer::value`] is the
/// only way to the chosen option's value — so an `Answer` can only have come
/// from a reading over a question that was built, and the value it carries can
/// only be one the caller supplied.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer<T> {
    outcomes: Vec<(String, f32)>,
    sharpness: f32,
    option_mass: f32,
    best_probability: f32,
    margin: f32,
    value: T,
}

impl<T> Answer<T> {
    /// The chosen option's value — one the caller put into the [`Choice`].
    ///
    /// This is the whole schema guarantee, and it is a signature rather than a
    /// rule: there is no way to return something that was not an option, because
    /// the only `T` in scope came from the question.
    pub fn value(&self) -> &T {
        &self.value
    }

    /// `(label, probability)` per option, in the question's own order.
    ///
    /// The raw distribution, as asked for rather than reduced to its winner: two
    /// readings that agree on the argmax and disagree on everything else are
    /// different results, and a caller comparing them needs this.
    pub fn outcomes(&self) -> impl Iterator<Item = (&str, f32)> + '_ {
        self.outcomes.iter().map(|(l, p)| (l.as_str(), *p))
    }

    /// The winning option's probability, over the options.
    #[must_use]
    pub fn best_probability(&self) -> f32 {
        self.best_probability
    }

    /// `1 − H(p)/ln K` over the options: `1.0` all on one, `0.0` uniform.
    ///
    /// **Not a confidence, and on the models in this repository not informative
    /// either.** Measured over 27 adjudicated corpus rows on
    /// `qwen3-coder-30b-a3b` it returned exactly `1.0` on 26 of them, four of the
    /// five wrong answers included — so a `1.0` here says what this model almost
    /// always says and not that it was right. [`Answer::margin`] summarises the
    /// same distribution on a scale that does not saturate — not the same number
    /// rescaled, since this figure consumes every option and the margin keeps only
    /// the winner's lead over the runner-up. The module docs set both out at
    /// length.
    #[must_use]
    pub fn sharpness(&self) -> f32 {
        self.sharpness
    }

    /// The winner's logit lead over the runner-up, in **nats** — `ln(p₁/p₂)` on
    /// the option-only distribution.
    ///
    /// The same distribution [`Answer::sharpness`] summarises, read as the top-two
    /// gap rather than as entropy over every option — so the two are different
    /// summaries and this one does not saturate. Read it when sharpness is pinned
    /// at `1.0`, which is the ordinary
    /// case for a greedy distribution from a large model: a lead of 12 nats and a
    /// lead of 40 both give `H(p) = 0` to `f32`, and are very different readings.
    ///
    /// Unbounded above and `0.0` on a tie, so it is not a probability and must not
    /// be presented as one. It is also **not comparable across questions with
    /// different option counts** the way sharpness is normalised to be — a lead
    /// over one alternative and a lead over thirteen are different quantities.
    ///
    /// # Measured scope: NOT usable for self-assessment
    ///
    /// An earlier version of this paragraph said the number was good for
    /// "ordering two readings of the same question, which is exactly what ranking
    /// findings by class confidence would need". Being on one scale turned out to
    /// be necessary and not sufficient. Three separations were recorded downstream
    /// on `qwen3-coder-30b-a3b`, over **two different questions** — and which
    /// question was asked is part of the reading, not context for it:
    ///
    /// * a **fourteen-option** class question over 27 corpus rows a **human**
    ///   wrote: **0.81**, ordering a correct class above a wrong one;
    /// * that same fourteen-option question over 446 findings the **model itself**
    ///   wrote: **0.52**, ordering real above noise, against a random-ordering null
    ///   of 0.4999, `P = 0.4411`;
    /// * a **two-option** yes/no asking whether a finding was real, over those same
    ///   27 human-written rows: **0.98**, ordering real above known-false.
    ///
    /// So the number does **not** rank a model's assessment of its own output: a
    /// caller ranking by it there is ordering at random and cannot tell. The other
    /// two figures are not a licence for that, and neither is a licence for the
    /// other — 0.98 and 0.81 are *different questions* over the same rows, and
    /// between 0.81 and 0.52 both the author of the prose and the relation being
    /// separated changed, so neither variable alone accounts for the drop.
    /// `rto_graph::review_score::CandidateFinding::class_margin_micronats`
    /// carries the full figures.
    ///
    /// Computed from the logits rather than from the reported probabilities,
    /// because the probabilities are where the information was lost: `p₂`
    /// underflows to zero in `f32` well before the lead stops growing, and
    /// `ln(p₁/p₂)` would then be infinite.
    #[must_use]
    pub fn margin(&self) -> f32 {
        self.margin
    }

    /// How much of the model's next-token mass — over the **whole** vocabulary —
    /// was on any option at all.
    ///
    /// Read this before believing [`Answer::sharpness`]. A value near zero means
    /// the model was going to answer something else entirely and the closed set
    /// merely renormalised what little it left over; the winner is then in-schema
    /// and uninformative. A reasoning model, whose first token is `<think>`, is
    /// the case that makes this non-hypothetical.
    #[must_use]
    pub fn option_mass(&self) -> f32 {
        self.option_mass
    }
}

/// A yes/no question — the interface's `noul`.
///
/// A [`Choice`] over `bool` and deliberately nothing more, so that the arithmetic,
/// the marker scheme and both reported numbers are the same code as the general
/// case rather than a second implementation that agrees with it by inspection.
/// What it adds is [`Noul::p_yes`], which is the number a yes/no caller wants
/// and would otherwise have to find by scanning `outcomes` for a label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Noul {
    inner: Choice<bool>,
}

impl Default for Noul {
    fn default() -> Self {
        Self::new()
    }
}

impl Noul {
    /// The yes/no question. `yes` is option `A`, so the order is the one a reader
    /// of the prompt expects.
    ///
    /// # Panics
    /// Never, and the compiler cannot see why. The option set is two literals,
    /// so every arm of [`TypedError`] is decided here and none of them fires:
    /// two options is neither too few nor too many, `"yes"` and `"no"` are
    /// non-blank, and they differ. It goes through [`Choice::new`] anyway rather
    /// than building the fields directly, so that a `Noul` is a `Choice` that
    /// passed the same validation as every other one — and
    /// `the_yes_no_pair_passes_the_same_validation_as_any_other_choice` asserts
    /// that from the outside rather than leaving it to this paragraph.
    #[must_use]
    pub fn new() -> Self {
        Choice::new([("yes".to_owned(), true), ("no".to_owned(), false)])
            .map(|inner| Self { inner })
            .expect("two distinct non-empty labels is a valid choice")
    }

    /// The underlying question, for the engine that reads it.
    #[must_use]
    pub fn as_choice(&self) -> &Choice<bool> {
        &self.inner
    }

    /// P(yes) from an answer to this question.
    ///
    /// Takes the answer rather than reading a stored one because a `Noul` is a
    /// question and a question has no answer: the same one may be asked of
    /// several states.
    #[must_use]
    pub fn p_yes(answer: &Answer<bool>) -> f32 {
        answer
            .outcomes
            .iter()
            .zip([true, false])
            .find_map(|((_, p), is_yes)| is_yes.then_some(*p))
            .unwrap_or(0.0)
    }
}

/// Softmax over `logits`, shifted by the maximum so that a large logit cannot
/// overflow `exp`.
///
/// Returns an empty vector for empty input rather than a uniform distribution
/// over nothing. A degenerate total — every logit `-inf`, or a `NaN` in the
/// slice — yields a uniform distribution, because a caller reading probabilities
/// needs them to sum to one and `0/0` does not.
#[must_use]
pub fn softmax(logits: &[f32]) -> Vec<f32> {
    if logits.is_empty() {
        return Vec::new();
    }
    let max = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    if !max.is_finite() {
        return uniform(logits.len());
    }
    let exps: Vec<f32> = logits.iter().map(|l| (l - max).exp()).collect();
    let total: f32 = exps.iter().sum();
    if total <= 0.0 || !total.is_finite() {
        return uniform(logits.len());
    }
    exps.into_iter().map(|e| e / total).collect()
}

/// `n` equal probabilities.
fn uniform(n: usize) -> Vec<f32> {
    #[expect(
        clippy::cast_precision_loss,
        reason = "n is at most MAX_OPTIONS for a choice and a vocabulary size otherwise; \
                  neither reaches f32's integer limit"
    )]
    let p = 1.0 / n as f32;
    vec![p; n]
}

/// `1 − H(p)/ln K` over a distribution of `K` options.
///
/// `1.0` when all the mass is on one option and `0.0` when it is uniform, so it
/// reads in the direction a caller expects — but see [`Answer::sharpness`]: it is
/// a shape statistic and not a calibrated confidence.
///
/// `K < 2` returns `1.0`. There is no entropy to normalise by when there is one
/// option or none, and `ln 1 = 0` would otherwise divide. [`Choice::new`] refuses
/// that case, so this arm exists for the function's own sake rather than for a
/// question's.
#[must_use]
pub fn sharpness(probs: &[f32]) -> f32 {
    if probs.len() < 2 {
        return 1.0;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "probs.len() is at most MAX_OPTIONS here; f32 represents it exactly"
    )]
    let ln_k = (probs.len() as f32).ln();
    // `p ln p → 0` as `p → 0`, so a zero-probability option contributes nothing;
    // taking `ln 0` instead would make the whole sum `NaN`.
    let entropy: f32 = probs
        .iter()
        .filter(|p| **p > 0.0)
        .map(|p| -p * p.ln())
        .sum();
    (1.0 - entropy / ln_k).clamp(0.0, 1.0)
}

/// The arithmetic half of a reading: `(per-option probabilities, option mass)`
/// from the option logits and the full vocabulary's.
///
/// Split out from the engine so that the part with no llama.cpp in it is
/// testable in a build with no llama.cpp — which is the build CI runs at four of
/// its feature cells.
///
/// `option_logits` are the logits at the option markers, in the question's order;
/// `all_logits` is the whole row they were taken from. The two softmaxes are over
/// different denominators on purpose: the first is the answer, the second is
/// whether the answer meant anything.
#[must_use]
pub fn read_distribution(option_logits: &[f32], all_logits: &[f32]) -> (Vec<f32>, f32) {
    let probs = softmax(option_logits);
    // A softmax probability depends only on the logit's own value and the shared
    // normaliser, so each option's full-vocabulary probability is computed from
    // its logit directly. Nothing is looked up, and in particular this needs no
    // token ids — which is what keeps this function free of the vocabulary and so
    // free of llama.cpp. It does **not** materialise the 150,000-element
    // distribution to sum a handful of its entries.
    let max = all_logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let total: f32 = if max.is_finite() {
        all_logits.iter().map(|l| (l - max).exp()).sum()
    } else {
        0.0
    };
    // A degenerate row — empty, all `-inf`, or a `NaN` — has no meaningful mass
    // to report. `0.0` is the honest answer and reads as "no legal answer was
    // wanted", which is the conclusion a caller should draw from it.
    let mass = if total > 0.0 && total.is_finite() {
        option_logits.iter().map(|l| (l - max).exp() / total).sum()
    } else {
        0.0
    };
    (probs, mass.clamp(0.0, 1.0))
}

/// The largest logit's lead over the second largest, in nats — `0.0` on a tie,
/// on fewer than two finite values, or on a row [`softmax`] reads as degenerate.
///
/// # The last of those is not a detail, it is a correctness condition
///
/// This number and the [`Answer::value`] beside it must describe the **same**
/// winner. They are computed from the same slice by different code, so they can
/// disagree, and a `NaN` is where: `f32::max` ignores `NaN`, so `[NaN, 10.0, 0.0]`
/// has a finite maximum of `10.0` and a lead of `10.0` — while the softmax's
/// *total* is `NaN`, which sends it to the uniform fallback, whose argmax is
/// option `0`. A margin of 10 nats would then be reported for an option that did
/// not win, which is worse than no margin at all.
///
/// So a row carrying any non-finite value has no margin. That agrees with
/// `softmax` by construction rather than by coincidence: `softmax` answers
/// "uniform, no information" for exactly these rows, and `0.0` is what no
/// information means here.
#[must_use]
pub fn margin(logits: &[f32]) -> f32 {
    // Checked first, and over the whole slice: a single `NaN` makes the softmax
    // total `NaN` and so makes the distribution uniform, whatever the other
    // entries say.
    if logits.iter().any(|l| l.is_nan()) {
        return 0.0;
    }
    let mut best = f32::NEG_INFINITY;
    let mut second = f32::NEG_INFINITY;
    for l in logits.iter().copied().filter(|l| l.is_finite()) {
        if l > best {
            second = best;
            best = l;
        } else if l > second {
            second = l;
        }
    }
    if best.is_finite() && second.is_finite() {
        (best - second).max(0.0)
    } else {
        0.0
    }
}

/// Read an [`Answer`] from the logits of one position.
///
/// The public seam between the arithmetic here and the engine's FFI: the engine
/// supplies the row and which entries the markers sit at, and gets back a value
/// from the caller's own option set.
///
/// `option_logits` must be in `question`'s option order and **the same length**.
///
/// # Why that is a panic and not a tolerance
///
/// This used to take the shorter of the two and pad the rest with zeroes, on the
/// reasoning that a truncated reading is still in-schema. It is — and it is
/// *confidently wrong*, which is the one outcome this module exists to make
/// impossible. One supplied logit for a fourteen-option question softmaxes to
/// probability `1.0`, the other thirteen pad to `0.0`, and the answer reports a
/// **sharpness of exactly 1.0**: the most certain reading the type can express,
/// produced by an engine that supplied almost no data. `option_mass` does not
/// rescue it either, because the mass of the one logit that did arrive can be the
/// whole of the model's belief.
///
/// A mismatch is unreachable from outside: the only caller builds the vector by
/// mapping over `question.markers()`, so the lengths agree by construction. It is
/// therefore an engine bug, and the right answer to an engine bug is to say so
/// loudly rather than to return a number nobody can tell is wrong.
///
/// # Panics
/// If `option_logits.len()` differs from `question.len()`.
#[must_use]
pub fn read_answer<T: Clone>(
    question: &Choice<T>,
    option_logits: &[f32],
    all_logits: &[f32],
) -> Answer<T> {
    assert_eq!(
        option_logits.len(),
        question.len(),
        "the engine supplied {} logit(s) for a {}-option question; padding the rest \
         would report a maximally sharp answer for a reading that did not happen",
        option_logits.len(),
        question.len(),
    );
    let (probs, mass) = read_distribution(option_logits, all_logits);
    question.answer_from(&probs, mass, margin(option_logits))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How close two probabilities have to be to count as equal here.
    ///
    /// These tests compare against *hand-computed* values, not against another
    /// run of llama.cpp, so a tolerance is a statement about `f32` and nothing
    /// else. No test in this file compares two model readings for float
    /// equality; the ones that compare readings compare the argmax.
    const EPS: f32 = 1e-5;

    #[test]
    fn a_choice_refuses_fewer_than_two_options() {
        let err = Choice::new([("only".to_owned(), 1u8)]).expect_err("one option");
        assert_eq!(err, TypedError::TooFewOptions(1));
    }

    #[test]
    fn a_choice_refuses_more_options_than_there_are_markers() {
        let many: Vec<(String, usize)> = (0..=MAX_OPTIONS).map(|i| (format!("o{i}"), i)).collect();
        let err = Choice::new(many).expect_err("27 options");
        assert_eq!(err, TypedError::TooManyOptions(MAX_OPTIONS + 1));
    }

    #[test]
    fn a_choice_refuses_a_blank_label() {
        let err = Choice::new([("a".to_owned(), 0), ("   ".to_owned(), 1)]).expect_err("blank");
        assert_eq!(err, TypedError::EmptyLabel(1));
    }

    /// Two options a model cannot tell apart are refused **naming both**, because
    /// a caller looking at a list of fourteen needs to know which two.
    #[test]
    fn a_choice_refuses_two_options_with_one_label_and_names_both() {
        let err = Choice::new([
            ("same".to_owned(), 0),
            ("other".to_owned(), 1),
            ("same".to_owned(), 2),
        ])
        .expect_err("duplicate");
        assert_eq!(err, TypedError::DuplicateLabel(0, 2, "same".to_owned()));
    }

    /// **A newline in a label forges a marker mapping.** `render` is one line per
    /// option, so `"safe\nB) forged"` would put a `B)` line in the prompt that
    /// belongs to option `A` — and the logits are gathered against the real
    /// mapping, so the answer would be a confident read of the wrong option.
    /// **No error message carries a run of spaces.**
    ///
    /// A Rust `\`-continuation inside a string eats the newline *and* the leading
    /// whitespace — unless an editing tool drops the backslash, at which point the
    /// indentation becomes part of the message and nothing fails. That happened to
    /// `ControlCharacter` between one commit and the next: 14 literal spaces
    /// between "would" and "break", compiled, tested and shipped to a reviewer.
    /// So it is checked over every variant rather than fixed in one.
    #[test]
    fn no_error_message_carries_a_collapsed_line_continuation() {
        let messages = [
            TypedError::TooFewOptions(1).to_string(),
            TypedError::TooManyOptions(99).to_string(),
            TypedError::EmptyLabel(3).to_string(),
            TypedError::DuplicateLabel(0, 2, "x".to_owned()).to_string(),
            TypedError::ControlCharacter(0, u32::from(b'\n')).to_string(),
        ];
        for m in messages {
            assert!(
                !m.contains("  "),
                "a run of spaces in a user-facing message, which is a dropped \
                 line continuation rather than prose: {m:?}"
            );
            assert!(
                !m.contains('\n'),
                "a newline in a single-line message: {m:?}"
            );
        }
    }

    #[test]
    fn a_label_with_a_newline_is_refused_because_it_would_forge_a_second_marker() {
        let err = Choice::new([("safe\nB) forged".to_owned(), 0), ("other".to_owned(), 1)])
            .expect_err("a newline label");
        assert_eq!(err, TypedError::ControlCharacter(0, u32::from(b'\n')));
    }

    /// Every control character, not only the newline that motivated the check: a
    /// carriage return ends a line on its own in plenty of readers, and a
    /// denylist of the ones that happen to matter cannot be finished.
    #[test]
    fn every_control_character_is_refused_rather_than_a_chosen_few() {
        for bad in ['\n', '\r', '\t', '\u{0}', '\u{1b}'] {
            let err = Choice::new([(format!("a{bad}b"), 0), ("other".to_owned(), 1)])
                .expect_err("a control character");
            assert_eq!(
                err,
                TypedError::ControlCharacter(0, bad as u32),
                "for {bad:?}"
            );
        }
    }

    /// And the rendered prompt has exactly one line per option, which is the
    /// property the check exists to hold.
    #[test]
    fn the_rendered_prompt_has_one_line_per_option() {
        let q: Choice<u8> =
            Choice::new((0..14u8).map(|i| (format!("class-{i}"), i))).expect("valid");
        assert_eq!(q.render().lines().count(), q.len());
    }

    #[test]
    fn markers_run_from_a_in_the_callers_order() {
        let q = Choice::new([
            ("first".to_owned(), 10),
            ("second".to_owned(), 20),
            ("third".to_owned(), 30),
        ])
        .expect("valid");
        assert_eq!(q.markers().collect::<String>(), "ABC");
        assert_eq!(q.render(), "A) first\nB) second\nC) third\n");
    }

    /// The schema guarantee, stated as a test: the value read back is one of the
    /// values handed in, for **every** position the logits could favour. There is
    /// no input to this function that yields anything else, which is why the
    /// assertion is over the whole option set rather than over one case.
    #[test]
    fn every_reading_returns_a_value_the_caller_supplied() {
        let q = Choice::new([
            ("cleanup-gap".to_owned(), 'a'),
            ("contract-drift".to_owned(), 'b'),
            ("vacuous-test".to_owned(), 'c'),
        ])
        .expect("valid");
        for winner in 0..q.len() {
            let mut logits = vec![0.0f32; q.len()];
            logits[winner] = 10.0;
            let answer = read_answer(&q, &logits, &logits);
            assert!(
                ['a', 'b', 'c'].contains(answer.value()),
                "read a value that was never an option: {:?}",
                answer.value()
            );
            assert_eq!(*answer.value(), ['a', 'b', 'c'][winner]);
        }
    }

    #[test]
    fn softmax_is_a_distribution_and_puts_the_mass_on_the_largest_logit() {
        let p = softmax(&[1.0, 2.0, 3.0]);
        assert!(
            (p.iter().sum::<f32>() - 1.0).abs() < EPS,
            "sums to one: {p:?}"
        );
        assert!(p[2] > p[1] && p[1] > p[0], "monotone in the logit: {p:?}");
    }

    /// A logit large enough to overflow `exp` unshifted. `exp(800)` is `inf`, so
    /// an unshifted softmax would return `NaN`s here.
    #[test]
    fn softmax_survives_a_logit_that_would_overflow_exp() {
        let p = softmax(&[800.0, 799.0]);
        assert!(p.iter().all(|x| x.is_finite()), "finite: {p:?}");
        assert!(
            (p.iter().sum::<f32>() - 1.0).abs() < EPS,
            "sums to one: {p:?}"
        );
        assert!(p[0] > p[1]);
    }

    #[test]
    fn softmax_of_nothing_is_nothing_rather_than_a_uniform_over_nothing() {
        assert!(softmax(&[]).is_empty());
    }

    #[test]
    fn an_all_negative_infinity_row_reads_as_uniform_rather_than_nan() {
        let p = softmax(&[f32::NEG_INFINITY; 4]);
        assert!(p.iter().all(|x| (x - 0.25).abs() < EPS), "uniform: {p:?}");
    }

    #[test]
    fn sharpness_is_one_when_all_the_mass_is_on_one_option() {
        assert!((sharpness(&[1.0, 0.0, 0.0, 0.0]) - 1.0).abs() < EPS);
    }

    #[test]
    fn sharpness_is_zero_on_a_uniform_distribution() {
        assert!(
            sharpness(&[0.25; 4]).abs() < EPS,
            "{}",
            sharpness(&[0.25; 4])
        );
    }

    /// The normalisation is by `ln K`, so a uniform distribution reads `0.0`
    /// whatever `K` is — which is what makes two questions with different option
    /// counts comparable at all.
    #[test]
    fn a_uniform_distribution_reads_zero_at_every_option_count() {
        for k in 2..=MAX_OPTIONS {
            #[expect(clippy::cast_precision_loss, reason = "k <= 26")]
            let probs = vec![1.0 / k as f32; k];
            assert!(
                sharpness(&probs).abs() < 1e-4,
                "K={k} uniform read {}",
                sharpness(&probs)
            );
        }
    }

    #[test]
    fn sharpness_is_between_zero_and_one_and_orders_by_concentration() {
        let flat = sharpness(&[0.34, 0.33, 0.33]);
        let peaked = sharpness(&[0.9, 0.05, 0.05]);
        assert!((0.0..=1.0).contains(&flat), "flat {flat}");
        assert!(peaked > flat, "peaked {peaked} should exceed flat {flat}");
    }

    /// A zero-probability option must contribute nothing rather than `NaN`:
    /// `ln 0` is `-inf` and `0 × -inf` is `NaN`, which would poison the sum.
    #[test]
    fn a_zero_probability_option_does_not_make_the_entropy_nan() {
        let s = sharpness(&[0.5, 0.5, 0.0]);
        assert!(s.is_finite(), "NaN entropy from a zero probability");
    }

    #[test]
    fn a_single_option_reads_as_sharp_without_dividing_by_zero() {
        assert!((sharpness(&[1.0]) - 1.0).abs() < EPS);
    }

    /// `option_mass` is the whole point of the diagnostic: the same option
    /// logits, read out of a row where the model wanted something else entirely,
    /// still yield a sharp in-schema winner — and the mass says so.
    #[test]
    fn a_sharp_answer_out_of_a_row_the_model_spent_elsewhere_reports_a_thin_mass() {
        let q = Choice::new([("yes".to_owned(), true), ("no".to_owned(), false)]).expect("valid");
        // The markers are at 0.0 and -2.0; some other token is at 20.0.
        let all = vec![0.0f32, -2.0, 20.0];
        let answer = read_answer(&q, &all[..2], &all);
        assert!(
            answer.sharpness() > 0.2,
            "the reading still looks sharp: {}",
            answer.sharpness()
        );
        assert!(
            answer.option_mass() < 0.01,
            "but almost none of the mass was on a legal answer: {}",
            answer.option_mass()
        );
        assert!(*answer.value(), "and the value is still in-schema");
    }

    #[test]
    fn option_mass_is_one_when_the_options_are_the_whole_vocabulary() {
        let q = Choice::new([("a".to_owned(), 0), ("b".to_owned(), 1)]).expect("valid");
        let all = vec![1.0f32, 3.0];
        let answer = read_answer(&q, &all, &all);
        assert!(
            (answer.option_mass() - 1.0).abs() < EPS,
            "mass {}",
            answer.option_mass()
        );
    }

    #[test]
    fn outcomes_come_back_in_the_questions_own_order() {
        let q = Choice::new([
            ("alpha".to_owned(), 1),
            ("beta".to_owned(), 2),
            ("gamma".to_owned(), 3),
        ])
        .expect("valid");
        let answer = read_answer(&q, &[0.0, 5.0, 0.0], &[0.0, 5.0, 0.0]);
        let labels: Vec<&str> = answer.outcomes().map(|(l, _)| l).collect();
        assert_eq!(labels, ["alpha", "beta", "gamma"]);
        assert_eq!(*answer.value(), 2);
        let sum: f32 = answer.outcomes().map(|(_, p)| p).sum();
        assert!(
            (sum - 1.0).abs() < EPS,
            "the reported distribution sums to one"
        );
    }

    /// Ties go to the caller's earlier option, and the rule is a test rather than
    /// a comment so that a later refactor cannot make it a coin flip.
    #[test]
    fn a_tie_goes_to_the_earlier_option() {
        let q = Choice::new([("first".to_owned(), 1), ("second".to_owned(), 2)]).expect("valid");
        let answer = read_answer(&q, &[3.0, 3.0], &[3.0, 3.0]);
        assert_eq!(*answer.value(), 1);
    }

    /// `Noul::new`'s documented-impossible panic, asserted from outside rather
    /// than argued in its doc comment: the pair it hands to `Choice::new` is a
    /// pair `Choice::new` accepts.
    #[test]
    fn the_yes_no_pair_passes_the_same_validation_as_any_other_choice() {
        let built = Choice::new([("yes".to_owned(), true), ("no".to_owned(), false)]);
        assert!(built.is_ok(), "Noul::new's pair is refused: {built:?}");
        assert_eq!(built.expect("just asserted ok"), *Noul::new().as_choice());
    }

    #[test]
    fn a_noul_is_a_two_option_choice_and_p_yes_reads_the_yes_side() {
        let n = Noul::new();
        assert_eq!(n.as_choice().len(), 2);
        assert_eq!(n.as_choice().markers().collect::<String>(), "AB");
        let yes = read_answer(n.as_choice(), &[5.0, 0.0], &[5.0, 0.0]);
        assert!(*yes.value(), "A is yes");
        assert!(Noul::p_yes(&yes) > 0.9, "p_yes {}", Noul::p_yes(&yes));
        let no = read_answer(n.as_choice(), &[0.0, 5.0], &[0.0, 5.0]);
        assert!(!*no.value());
        assert!(Noul::p_yes(&no) < 0.1, "p_yes {}", Noul::p_yes(&no));
    }

    /// `p_yes` and `1 - p_no` are the same number, so a caller cannot read the
    /// question two ways and get two answers.
    #[test]
    fn p_yes_is_one_minus_the_no_probability() {
        let n = Noul::new();
        let a = read_answer(n.as_choice(), &[1.0, 0.4], &[1.0, 0.4]);
        let p_no = a
            .outcomes()
            .find_map(|(l, p)| (l == "no").then_some(p))
            .expect("a no option");
        assert!((Noul::p_yes(&a) - (1.0 - p_no)).abs() < EPS);
    }

    /// **The margin and the winner must agree.** `f32::max` ignores `NaN`, so a
    /// row with one would otherwise report a lead for an option the uniform
    /// fallback did not pick — a confident number about the wrong answer.
    #[test]
    fn a_nan_row_reports_no_margin_so_it_cannot_describe_the_wrong_winner() {
        let q = Choice::new([
            ("a".to_owned(), 0),
            ("b".to_owned(), 1),
            ("c".to_owned(), 2),
        ])
        .expect("valid");
        let row = [f32::NAN, 10.0, 0.0];
        assert!(
            margin(&row).abs() < EPS,
            "a NaN row still reports a margin: {}",
            margin(&row)
        );
        let answer = read_answer(&q, &row, &row);
        // The softmax total is NaN, so the distribution is uniform and the argmax
        // is the caller's first option. The margin must not claim otherwise.
        assert_eq!(
            *answer.value(),
            0,
            "the uniform fallback picks the first option"
        );
        assert!(answer.margin().abs() < EPS, "margin {}", answer.margin());
    }

    /// A short logit vector must not become a maximally sharp answer. Asserted as
    /// a panic because it is an engine bug, and a silent `sharpness == 1.0` is
    /// indistinguishable from a real one.
    #[test]
    #[should_panic(expected = "logit(s) for a")]
    fn a_short_logit_vector_is_refused_rather_than_padded() {
        let q: Choice<u8> = Choice::new((0..14u8).map(|i| (format!("c{i}"), i))).expect("valid");
        let _ = read_answer(&q, &[3.0], &[3.0]);
    }

    #[test]
    fn the_margin_is_the_leaders_lead_over_the_runner_up() {
        assert!((margin(&[1.0, 3.0, 2.5]) - 0.5).abs() < EPS);
        assert!((margin(&[10.0, -10.0]) - 20.0).abs() < EPS);
    }

    #[test]
    fn a_tie_has_no_margin_and_it_is_never_negative() {
        assert!(margin(&[4.0, 4.0]).abs() < EPS);
        for row in [&[][..], &[1.0][..], &[f32::NAN, f32::NAN][..]] {
            assert!(margin(row).abs() < EPS, "{row:?} produced a margin");
        }
    }

    /// A `-inf` option — which is what an unreachable marker reads as — must not
    /// become an infinite margin, because the margin is meant to be the number
    /// that still works when the probabilities have stopped being informative.
    #[test]
    fn an_unreachable_option_does_not_make_the_margin_infinite() {
        let m = margin(&[2.0, f32::NEG_INFINITY]);
        assert!(
            m.is_finite(),
            "infinite margin from an unreachable option: {m}"
        );
        assert!(
            m.abs() < EPS,
            "one finite option leaves no lead to measure: {m}"
        );
    }

    /// **The measurement this exists for, in miniature.** Two readings whose
    /// sharpness is indistinguishable in `f32` and whose margins are three times
    /// apart: the entropy figure cannot order them and the margin can, which is
    /// the whole reason `Answer::margin` is carried.
    #[test]
    fn the_margin_orders_two_readings_that_sharpness_cannot() {
        let q = Choice::new([("a".to_owned(), 1), ("b".to_owned(), 2)]).expect("valid");
        // 30 nats is already past where `f32` can tell `1 − H/ln K` from `1.0`:
        // `exp(-30)` is 9.4e-14, so the entropy term is ~3e-12 and the subtraction
        // rounds away. 15 nats is *not* — it still reads 0.99999285 — which is why
        // the gaps here are 30 and 90 and not something smaller that would make
        // this test pass by having no saturation to demonstrate.
        let near = [30.0f32, 0.0];
        let far = [90.0f32, 0.0];
        let near = read_answer(&q, &near, &near);
        let far = read_answer(&q, &far, &far);
        assert!(
            (near.sharpness() - far.sharpness()).abs() < f32::EPSILON,
            "sharpness already separates these, so the test proves nothing: {} vs {}",
            near.sharpness(),
            far.sharpness()
        );
        assert!(
            far.margin() > near.margin() * 2.0,
            "margin did not separate them: {} vs {}",
            near.margin(),
            far.margin()
        );
        assert_eq!(near.value(), far.value(), "same winner either way");
    }

    /// The reading is a pure function: same logits in, same answer out. Compared
    /// as the argmax **and** the label order, never as floats across two decodes
    /// — which is a different claim this function is not the place to make.
    #[test]
    fn two_readings_of_one_row_agree() {
        let q = Choice::new([
            ("a".to_owned(), 1),
            ("b".to_owned(), 2),
            ("c".to_owned(), 3),
        ])
        .expect("valid");
        let row = [0.3f32, 2.1, 1.4];
        let first = read_answer(&q, &row, &row);
        let second = read_answer(&q, &row, &row);
        assert_eq!(first.value(), second.value());
        assert_eq!(
            first.outcomes().map(|(l, _)| l).collect::<Vec<_>>(),
            second.outcomes().map(|(l, _)| l).collect::<Vec<_>>()
        );
        assert_eq!(first, second, "a pure function of its inputs");
    }
}

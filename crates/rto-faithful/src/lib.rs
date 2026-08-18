//! Rendering faithfulness: every claim in a rendered summary must trace to a
//! finding some deterministic tool actually produced.
//!
//! Roteiro's review architecture splits the work in two. **Deterministic tools
//! find defects; a model's only job is to render those defects as prose a human
//! wants to read.** The model does not review, does not judge, and does not
//! decide what is worth saying — it decides how to say what the tools found.
//!
//! That split is a claim about behaviour, and behaviour claims decay. This crate
//! is the guard that keeps it true: [`check`] takes the finding set the tools
//! produced and the rendering a model returned, and reports every claim that
//! rests on nothing and every citation that names nothing. Both are decidable by
//! **set membership alone** — no code is read, no network is touched, no model is
//! consulted.
//!
//! # Why this exists before the rules it polices
//!
//! One checker covers every rule ever written, so its cost does not scale with
//! the rule set. Built now, while the rule set is thin, it stops a model quietly
//! padding a report to look useful — which is not a hypothetical failure. A local
//! reviewer was measured emitting roughly **eleven findings per file, 71% of them
//! carrying a single label**, and two prompt revisions written specifically to
//! impose precision moved that number not at all. Prompting is not a control.
//! Set membership is.
//!
//! # What counts as a claim
//!
//! A claim is an **explicitly delimited span**: the renderer hands back a
//! sequence of [`Segment`]s and says, per segment, which findings it rests on.
//! This crate never parses prose to find claim boundaries.
//!
//! The alternatives were sentence and paragraph, and both require *computing* a
//! boundary. Computing one means guessing where a sentence ends in prose that is
//! about code — text carrying `rto_graph::findings.rs`, `Vec<String>`, `self.parts`
//! and `1.21.1`. A sentence splitter over that is a heuristic, heuristics are
//! wrong sometimes, and a guard that is wrong sometimes is the model-shaped
//! reasoning this architecture exists to remove. Paragraphs are computable
//! reliably (a blank line) but are far too coarse: a paragraph citing one finding
//! could carry ten sentences, nine of them invented.
//!
//! So the boundary is declared, not inferred. The renderer must commit to where
//! one claim stops and the next starts, and that commitment is part of its
//! output rather than something a parser reconstructs afterwards.
//!
//! **What that cannot catch**, stated plainly because a passing check must not be
//! mistaken for a true report:
//!
//! - **A fabricated clause riding inside a cited span.** *"`parse_config` unwraps
//!   at line 42, which ships user passwords to disk"* cites a real finding for
//!   the first clause and invents the second. The span is cited, so it passes.
//!   The renderer chooses its own granularity, and the coarser it chooses, the
//!   more room a rider has. This crate cannot bound that, because bounding it
//!   would mean deciding where the clauses are.
//! - **Aboutness.** [`check`] verifies a citation *resolves*, never that the
//!   claim's text has anything to do with the finding it names. A rendering that
//!   pairs every real claim with a real but unrelated key passes.
//! - **Omission.** A rendering that silently drops half the findings is faithful
//!   by this definition. Coverage is a different property with a different
//!   check.
//!
//! # The larger limit: this bounds fabrication, not distortion
//!
//! A rendering can cite every claim correctly and still mislead — by ordering
//! trivia first, by dwelling on one finding and burying five, by attaching a
//! wrong causal gloss to a right fact. Emphasis, proportion and causation are not
//! set membership, and nothing here measures them.
//!
//! **A passing verdict means no claim was invented. It does not mean the summary
//! is true, complete, or fairly weighted.** Anyone who reads a green result as
//! "the report is accurate" has read it wrong, and this paragraph exists so that
//! is a misreading rather than an honest mistake.
//!
//! # Not stored
//!
//! A rendering is ephemeral — local to the person who ran the review, not an
//! artifact kept for later (ADR-0020 §4 rules this class of output out of the
//! findings store). Nothing here writes a row, opens a file, or knows that a
//! store exists.

use std::collections::BTreeSet;

use rto_graph::FindingKey;
use serde::{Deserialize, Serialize};

/// Sentences a rendering may contain **without** citing a finding.
///
/// These are connectives: they introduce, they separate, they assert nothing
/// about the code, the findings, or how many there are. A segment matches only
/// by exact string equality after [`normalize`] — same characters, same case.
///
/// # This list is FROZEN
///
/// It must not grow to accommodate a rendering that failed the check. An
/// exemption list that expands under pressure stops being a list of things that
/// assert nothing and becomes a list of things nobody wanted to cite, which is
/// precisely how the failure in ADR-0015 recurred: a category created to hold
/// output that is *not a fact* survives only as long as nobody widens it to keep
/// a caller happy. When a rendering fails here, the rendering is wrong. Fix the
/// renderer.
///
/// # Two things deliberately left out
///
/// **Anything carrying a count.** *"Here are three findings:"* looks structural
/// and is not: the number is an assertion about the finding set, it can be wrong,
/// and being wrong about it is exactly the padding this crate was built to catch.
/// The number-free [`"Here are the findings:"`](STRUCTURAL_EXEMPTIONS) says the
/// same thing and asserts nothing, so it is in the list and the counting form is
/// not.
///
/// **Anything asserting emptiness** — *"No findings."*, *"Nothing to report."*
/// Same reason: those are claims about the set's cardinality. They also do not
/// need to exist here, because a review that found nothing has nothing to render.
/// The caller reports an empty finding set; the model is not asked for prose
/// about it.
///
/// Both exclusions cost a renderer one phrasing. Admitting either would mean
/// this list contains sentences that can be false, and then it is no longer an
/// exemption list.
pub const STRUCTURAL_EXEMPTIONS: [&str; 6] = [
    "Findings:",
    "Here are the findings:",
    "In summary:",
    "Summary:",
    "Details:",
    "Recommended next steps:",
];

/// Collapse a segment's whitespace so a rendering is not rejected over a line
/// wrap: leading and trailing whitespace is dropped and every internal run of
/// whitespace becomes one space.
///
/// Case is **not** folded. The exemption list is frozen literals, and a renderer
/// that must emit the literal exactly cannot drift the list by casing.
#[must_use]
pub fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// One piece of a [`Rendering`].
///
/// **Deliberately not `#[non_exhaustive]`.** The whole guarantee of this crate is
/// that a rendering contains exactly two kinds of thing: prose that names the
/// finding it rests on, and a connective from a frozen list. A third kind is an
/// escape hatch by construction, so adding one must cost a major version and be
/// argued for in the open — which is the point. `rto-faithful` is published at
/// 1.x and this set is closed on purpose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Segment {
    /// Prose that asserts something, together with the findings it rests on.
    Claim {
        /// The prose as it will be shown to a reader.
        text: String,
        /// The findings this span rests on. Must be non-empty, and every key must
        /// be in the finding set the tools produced.
        ///
        /// A list rather than a single key because one span can legitimately rest
        /// on several findings — *"the same unwrap pattern appears in the config
        /// loader and the cache"* rests on two. Every entry is checked
        /// identically, so allowing several weakens nothing; it does mean a span
        /// citing many findings is less precisely traceable than several spans
        /// citing one each, which is a distortion concern and therefore outside
        /// what this crate bounds.
        citations: Vec<FindingKey>,
    },
    /// A connective that asserts nothing, and must appear verbatim in
    /// [`STRUCTURAL_EXEMPTIONS`].
    Structural {
        /// The connective, matched against the frozen list after [`normalize`].
        text: String,
    },
}

/// A model's rendering of a finding set: prose, in order, with each claim's
/// support named.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Rendering {
    /// The segments, in the order a reader will meet them.
    pub segments: Vec<Segment>,
}

impl Rendering {
    /// A rendering made of these segments.
    #[must_use]
    pub fn new(segments: Vec<Segment>) -> Self {
        Self { segments }
    }
}

/// A way a rendering failed to trace back to the findings.
///
/// `#[non_exhaustive]` because the *set* of decidable-by-set-membership defects
/// is not closed — a later cardinality or coverage claim would land here — and
/// this crate is published at 1.x, where a bare variant addition is breaking.
/// Contrast [`Segment`], whose set is closed on purpose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "defect", rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Defect {
    /// A claim named no finding at all. This is the fabrication case: prose
    /// asserting something that no tool produced.
    UncitedClaim {
        /// Index into [`Rendering::segments`].
        segment: usize,
        /// The claim's prose, so the report can quote what was invented.
        text: String,
    },
    /// A claim whose prose is empty or whitespace. A span that says nothing
    /// cannot be traced to anything, and citing findings from it launders them
    /// into a rendering no reader can check.
    EmptyClaim {
        /// Index into [`Rendering::segments`].
        segment: usize,
    },
    /// A citation naming a finding the tools did not produce — a dangling
    /// reference. Either the key was invented, or it was carried over from a run
    /// whose findings are gone.
    DanglingCitation {
        /// Index into [`Rendering::segments`].
        segment: usize,
        /// The key that resolved to nothing.
        citation: FindingKey,
    },
    /// A segment declared structural whose text is not in
    /// [`STRUCTURAL_EXEMPTIONS`].
    ///
    /// Without this, the exemption list would be decorative: a renderer could
    /// label any invented sentence `structural` and skip citation entirely. The
    /// frozen list is what makes the structural kind safe to have at all.
    UnlistedStructuralSegment {
        /// Index into [`Rendering::segments`].
        segment: usize,
        /// The text that is not on the list.
        text: String,
    },
}

impl Defect {
    /// A short stable label for this defect, in the same kebab-case as the
    /// serialized form.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::UncitedClaim { .. } => "uncited-claim",
            Self::EmptyClaim { .. } => "empty-claim",
            Self::DanglingCitation { .. } => "dangling-citation",
            Self::UnlistedStructuralSegment { .. } => "unlisted-structural-segment",
        }
    }

    /// Which segment of the rendering this defect is in.
    #[must_use]
    pub fn segment(&self) -> usize {
        match self {
            Self::UncitedClaim { segment, .. }
            | Self::EmptyClaim { segment }
            | Self::DanglingCitation { segment, .. }
            | Self::UnlistedStructuralSegment { segment, .. } => *segment,
        }
    }
}

/// What [`check`] concluded about one rendering.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Verdict {
    /// Every defect found, in segment order, and within a segment in citation
    /// order. Empty means the rendering is faithful **in the narrow sense this
    /// crate defines** — see the crate docs on what that does not mean.
    pub defects: Vec<Defect>,
}

impl Verdict {
    /// Whether no defect was found.
    ///
    /// Read this as *"no claim was invented"*, never as *"the summary is
    /// accurate"*. See the crate-level docs: fabrication is bounded here,
    /// distortion is not.
    #[must_use]
    pub fn is_faithful(&self) -> bool {
        self.defects.is_empty()
    }
}

/// Check a rendering against the findings it claims to describe.
///
/// A pure function of its two arguments. It reads no source, opens no file,
/// makes no network call and runs no model; every decision below is membership
/// in `findings` or in [`STRUCTURAL_EXEMPTIONS`]. Duplicate keys in `findings`
/// are harmless — only the set matters.
///
/// Reported as defects:
///
/// - a [`Segment::Claim`] with no citations ([`Defect::UncitedClaim`]);
/// - a [`Segment::Claim`] whose text is blank ([`Defect::EmptyClaim`]);
/// - a citation not in `findings` ([`Defect::DanglingCitation`], one per bad
///   key, so a claim citing three unknown keys reports three);
/// - a [`Segment::Structural`] not on the frozen list
///   ([`Defect::UnlistedStructuralSegment`]).
///
/// **Not** reported, because none of them is decidable by set membership:
/// whether a claim is *about* the finding it cites, whether a cited span smuggles
/// an uncited clause, whether findings were left out, and whether the emphasis is
/// fair. The crate docs spell each of these out.
#[must_use]
pub fn check(findings: &[FindingKey], rendering: &Rendering) -> Verdict {
    let known: BTreeSet<&FindingKey> = findings.iter().collect();
    let exempt: BTreeSet<String> = STRUCTURAL_EXEMPTIONS.iter().map(|s| normalize(s)).collect();

    let mut defects = Vec::new();
    for (segment, part) in rendering.segments.iter().enumerate() {
        match part {
            Segment::Claim { text, citations } => {
                if normalize(text).is_empty() {
                    defects.push(Defect::EmptyClaim { segment });
                }
                if citations.is_empty() {
                    defects.push(Defect::UncitedClaim {
                        segment,
                        text: text.clone(),
                    });
                }
                for citation in citations {
                    if !known.contains(citation) {
                        defects.push(Defect::DanglingCitation {
                            segment,
                            citation: citation.clone(),
                        });
                    }
                }
            }
            Segment::Structural { text } => {
                if !exempt.contains(&normalize(text)) {
                    defects.push(Defect::UnlistedStructuralSegment {
                        segment,
                        text: text.clone(),
                    });
                }
            }
        }
    }
    Verdict { defects }
}

#[cfg(test)]
mod tests {
    use super::{Defect, Rendering, STRUCTURAL_EXEMPTIONS, Segment, Verdict, check, normalize};
    use rto_graph::FindingKey;

    fn key(rule: &str) -> FindingKey {
        FindingKey::new("roteiro-check", &[rule, "crates/rto-faithful/src/lib.rs"]).expect("key")
    }

    fn claim(text: &str, citations: Vec<FindingKey>) -> Segment {
        Segment::Claim {
            text: text.to_owned(),
            citations,
        }
    }

    #[test]
    fn a_cited_claim_and_a_listed_connective_are_faithful() {
        let found = vec![key("broken-link")];
        let rendering = Rendering::new(vec![
            Segment::Structural {
                text: "Here are the findings:".to_owned(),
            },
            claim(
                "The link in ADR-0015 resolves to nothing.",
                vec![key("broken-link")],
            ),
        ]);
        assert_eq!(check(&found, &rendering), Verdict::default());
        assert!(check(&found, &rendering).is_faithful());
    }

    #[test]
    fn a_claim_citing_nothing_is_a_fabrication() {
        let rendering = Rendering::new(vec![claim("The cache is probably too small.", vec![])]);
        assert_eq!(
            check(&[], &rendering).defects,
            vec![Defect::UncitedClaim {
                segment: 0,
                text: "The cache is probably too small.".to_owned(),
            }]
        );
    }

    #[test]
    fn a_citation_naming_no_finding_is_a_dangling_reference() {
        let found = vec![key("broken-link")];
        let rendering = Rendering::new(vec![claim(
            "An ADR is malformed.",
            vec![key("malformed-adr")],
        )]);
        assert_eq!(
            check(&found, &rendering).defects,
            vec![Defect::DanglingCitation {
                segment: 0,
                citation: key("malformed-adr"),
            }]
        );
    }

    #[test]
    fn every_bad_citation_in_one_claim_is_reported() {
        // One defect per key, not one per claim: a report that says "this claim
        // has a bad citation" leaves the reader to find which of three it was.
        let rendering = Rendering::new(vec![claim(
            "Three things are wrong.",
            vec![key("a"), key("b"), key("c")],
        )]);
        let defects = check(&[key("b")], &rendering).defects;
        assert_eq!(defects.len(), 2);
        assert!(defects.iter().all(|d| d.label() == "dangling-citation"));
        assert!(defects.iter().all(|d| d.segment() == 0));
    }

    #[test]
    fn a_blank_claim_is_a_defect_even_when_its_citations_resolve() {
        // Otherwise a rendering could carry a citation with no prose attached,
        // which reads to a tallying caller as coverage of that finding.
        let found = vec![key("broken-link")];
        let rendering = Rendering::new(vec![claim("   \n\t ", vec![key("broken-link")])]);
        assert_eq!(
            check(&found, &rendering).defects,
            vec![Defect::EmptyClaim { segment: 0 }]
        );
    }

    #[test]
    fn a_structural_segment_off_the_list_is_a_defect() {
        // This is the evasion the frozen list closes: without it, any invented
        // sentence could be labelled structural and skip citation entirely.
        let rendering = Rendering::new(vec![Segment::Structural {
            text: "This codebase is in good shape overall.".to_owned(),
        }]);
        assert_eq!(
            check(&[], &rendering).defects,
            vec![Defect::UnlistedStructuralSegment {
                segment: 0,
                text: "This codebase is in good shape overall.".to_owned(),
            }]
        );
    }

    #[test]
    fn a_counting_sentence_is_not_exempt() {
        // "Here are three findings:" is an assertion about the finding set and
        // can be false. The number-free form is on the list; this one is not,
        // and the list must not grow to admit it.
        let rendering = Rendering::new(vec![Segment::Structural {
            text: "Here are three findings:".to_owned(),
        }]);
        assert_eq!(check(&[], &rendering).defects.len(), 1);
        assert!(
            !STRUCTURAL_EXEMPTIONS
                .iter()
                .any(|s| s.chars().any(|c| c.is_ascii_digit()))
                && !STRUCTURAL_EXEMPTIONS.iter().any(|s| {
                    let lower = s.to_ascii_lowercase();
                    ["one", "two", "three", "several", "no ", "nothing"]
                        .iter()
                        .any(|w| lower.contains(w))
                }),
            "an exemption acquired a count or an emptiness claim; those can be false, and a \
             sentence that can be false is a claim and needs a citation"
        );
    }

    #[test]
    fn a_line_wrap_does_not_break_a_connective_but_a_case_change_does() {
        let wrapped = Rendering::new(vec![Segment::Structural {
            text: "  Here are\n  the findings:  ".to_owned(),
        }]);
        assert!(check(&[], &wrapped).is_faithful());

        let recased = Rendering::new(vec![Segment::Structural {
            text: "here are the findings:".to_owned(),
        }]);
        assert!(!check(&[], &recased).is_faithful());
        assert_eq!(normalize("  a \n b  "), "a b");
    }

    #[test]
    fn defects_are_reported_in_segment_order() {
        let rendering = Rendering::new(vec![
            claim("ok", vec![key("a")]),
            claim("invented", vec![]),
            Segment::Structural {
                text: "Also:".to_owned(),
            },
        ]);
        let defects = check(&[key("a")], &rendering).defects;
        assert_eq!(
            defects.iter().map(Defect::segment).collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn an_empty_rendering_over_no_findings_is_vacuously_faithful() {
        // Stated as a test because it is the shape the "No findings." exemption
        // was refused in favour of: a review that found nothing has nothing to
        // render, and the caller reports the empty set.
        assert!(check(&[], &Rendering::default()).is_faithful());
    }

    #[test]
    fn a_rendering_survives_a_json_round_trip() {
        // The renderer is a model returning JSON, so the wire form is the
        // contract, not a convenience.
        let rendering = Rendering::new(vec![
            Segment::Structural {
                text: "Summary:".to_owned(),
            },
            claim("A link is broken.", vec![key("broken-link")]),
        ]);
        let json = serde_json::to_string(&rendering).expect("serialize");
        assert!(json.contains("\"kind\":\"claim\""));
        assert_eq!(
            serde_json::from_str::<Rendering>(&json).expect("deserialize"),
            rendering
        );
    }

    /// This crate's direct dependencies are frozen, and the freeze is the
    /// argument.
    ///
    /// [`check`] is supposed to be incapable of reading code, reaching the
    /// network or invoking a model, and "supposed to" is worth nothing on its
    /// own. Reading the manifest is how that becomes checkable — the same reason
    /// `rto_remote`'s `remote_is_not_a_default_feature` reads a `Cargo.toml`
    /// rather than a value: whether somebody could add an HTTP client is a
    /// property of the text, not of any run.
    ///
    /// This is a bound on what this crate may reach for directly. `rto-graph` is
    /// here for [`rto_graph::FindingKey`] alone and does carry a database of its
    /// own; the claim is not that nothing in the tree can do I/O, it is that
    /// nothing in *this* crate can acquire the means to without editing this
    /// test.
    #[test]
    fn dependencies_are_frozen() {
        let manifest = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
        )
        .expect("read this crate's Cargo.toml");
        let deps: Vec<String> = manifest
            .lines()
            .skip_while(|l| l.trim() != "[dependencies]")
            .skip(1)
            .take_while(|l| !l.trim_start().starts_with('['))
            .filter_map(|l| l.split_once('=').map(|(name, _)| name.trim().to_owned()))
            .filter(|name| !name.is_empty() && !name.starts_with('#'))
            .collect();
        assert_eq!(
            deps,
            vec!["rto-graph".to_owned(), "serde".to_owned()],
            "the dependency list of `rto-faithful` changed. It is frozen so that \"this checker \
             cannot read code, reach the network, or run a model\" is a property of the manifest \
             rather than of good intentions. Adding one is allowed — but it is a decision, and \
             this assertion is where it gets made."
        );
    }

    /// Every public enum here has had the `#[non_exhaustive]` question answered.
    ///
    /// The same guard `rto-remote` carries (#391), applied to this crate's own
    /// source: the workspace publishes at 1.x, so a variant added to a bare
    /// public enum is a breaking change. Taking the attribute is a decision and
    /// saying in the docs why the set is closed is a decision; only silence is
    /// not. `Segment` is deliberately closed and `Defect` deliberately open, and
    /// both say so.
    #[test]
    fn every_public_enum_either_is_non_exhaustive_or_says_why_not() {
        let text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"),
        )
        .expect("read this crate's lib.rs");
        let lines: Vec<&str> = text.lines().collect();
        let mut seen = 0;
        for (i, line) in lines.iter().enumerate() {
            let Some(rest) = line.strip_prefix("pub enum ") else {
                continue;
            };
            seen += 1;
            let name = rest.split_whitespace().next().unwrap_or(rest);
            // Attributes and doc lines are kept apart: a deliberately-exhaustive
            // enum's docs *name* the attribute in order to refuse it, so a
            // combined scan reports such an enum as both marked and unmarked.
            let (mut attrs, mut docs) = (Vec::new(), Vec::new());
            for above in lines[..i].iter().rev() {
                let t = above.trim_start();
                if t.starts_with("///") {
                    docs.push(t);
                } else if t.starts_with("#[") || t.starts_with(')') {
                    attrs.push(t);
                } else if !t.starts_with("//") || t.starts_with("//!") {
                    break;
                }
            }
            let marked = attrs.contains(&"#[non_exhaustive]");
            let justified = docs.iter().any(|d| d.contains("not `#[non_exhaustive]`"));
            assert!(
                marked || justified,
                "`pub enum {name}` is neither `#[non_exhaustive]` nor documented as deliberately \
                 exhaustive. This workspace publishes at 1.x, so a variant added later is a \
                 breaking change. Take the attribute, or say in the enum's own docs why its set \
                 is closed."
            );
            assert!(
                !(marked && justified),
                "`pub enum {name}` both carries `#[non_exhaustive]` and documents itself as \
                 deliberately not; one of the two is stale"
            );
        }
        assert!(
            seen >= 2,
            "only found {seen} public enums — the scan stopped matching, which would let this \
             test pass by finding nothing"
        );
    }
}

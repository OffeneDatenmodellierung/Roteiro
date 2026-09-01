//! Automatic discovery of a workspace member's OKF bundle, and the consent
//! prompt that guards it (issue #706 phase 2, ADR-0021).
//!
//! # The two halves compose rather than conflict
//!
//! Discovery is **automatic**: a member that publishes a bundle should not need
//! anyone to remember `roteiro import --from okf`. Trust is **prompted**: reading
//! a foreign repository's files into our graph is a consent question, not a
//! convenience one, and the prompt is what stops automation from becoming
//! consent-by-installation.
//!
//! # Which pairs are discovered, and why not all of them
//!
//! A workspace of *n* members has *n×(n−1)* possible consuming-repo/peer pairs.
//! Asking about all of them would be twenty prompts in a five-repo workspace, on
//! first run, for bundles most of those repos have no use for — and a prompt
//! nobody can finish reading is a prompt answered `y` by reflex, which is a
//! worse gate than no gate at all (ADR-0019 §3 makes that argument itself).
//!
//! So discovery is scoped to the pairs where importing would actually *do*
//! something: a repo is asked about peer *P* only when it already holds an
//! `extref:P::…` placeholder. That is exactly the payoff issue #706 was opened
//! for — "if a workspace member ships an OKF bundle, those stubs can carry real
//! concepts" — and a bundle from a repo nothing here references has no
//! placeholder to fill, so importing it would add unreferenced nodes rather than
//! enhance the graph. `roteiro import --from okf <path>` remains the way to read
//! such a bundle deliberately.
//!
//! # Where the prompt goes when there is no terminal
//!
//! **Unprompted means `ignore`, and it is said once. Nothing is recorded.**
//!
//! A server (`serve`, `mcp`, `explorer`), a CI job and a piped invocation all
//! have the same property: no human. The rule is uniform across them rather than
//! special-cased for servers, because the thing that is missing is the same
//! thing.
//!
//! **Why `ignore` and not `acknowledge`.** The three answers are not symmetric in
//! what they cost when chosen wrongly. `ignore` leaves the graph exactly as it is
//! today — the placeholder stays a placeholder, which is the pre-#706 status quo
//! and a known-good state; the cost of being wrong is that a feature did not
//! happen. `acknowledge` writes a stranger's prose into a graph that a language
//! model reads through `search`, `explain` and `context`; the cost of being wrong
//! is that a model was handed text nobody has looked at. [`rto_graph::screen`]
//! exists because that text is a live injection surface. A default has to be the
//! answer whose failure mode is a missing feature, not a delivered payload.
//!
//! **Why not refuse to start.** ADR-0019's gate hard-errors when there is nobody
//! to ask, and that is right for an egress call: refusing is the *point* of the
//! command. Here the bundle is an enhancement to a graph the server is otherwise
//! ready to serve, so refusing to boot would take a service down over a directory
//! appearing in somebody else's repository — and would let any workspace member
//! wedge another person's server by adding one. The nearest honest analogue of
//! "refuse" is "do nothing, and say so".
//!
//! **Why nothing is recorded.** A server's non-answer must not become an answer.
//! If a startup wrote `ignore` into the consent table, the next interactive run
//! would read a decision a person never made and never ask. Silence is the
//! absence of consent, not a third kind of it, so it leaves no trace and the next
//! interactive scan asks properly.
//!
//! **Not spamming.** One line per undecided peer per process, capped, and only
//! for peers that are actually undecided — a peer whose answer is recorded is
//! silent. [`Noted`] dedupes within a process so a SIGHUP workspace reload does
//! not reprint it.

use std::collections::BTreeSet;
use std::path::Path;

use rto_graph::{ConsentState, OkfBundle, OkfDecision, Store};
use rto_render::okf::read;

/// A discovered bundle, screened, with the consuming graph's answer for it.
pub struct Discovered {
    /// The peer's bundle on disk.
    pub bundle: OkfBundle,
    /// Whether an answer is recorded, and why it does not apply if it does not.
    pub state: ConsentState,
    /// What reading the bundle found — including what the screen decided, which
    /// is the information the person answering most needs.
    pub screened: Result<read::OkfReport, String>,
}

impl Discovered {
    /// The bundle root as the consent record keys it.
    #[must_use]
    pub fn root_key(&self) -> String {
        crate::okf_root_key(&self.bundle.bundle)
    }

    /// The screening fingerprint for this bundle right now.
    #[must_use]
    pub fn screen_classes(&self) -> String {
        match &self.screened {
            Ok(r) => {
                let borrowed: Vec<&str> = r.screen_classes.iter().map(String::as_str).collect();
                rto_graph::screen_fingerprint(&borrowed)
            }
            // A bundle that would not read has no screening classes, and must not
            // be recorded as having screened clean.
            Err(_) => String::new(),
        }
    }

    /// The one-line summary shown in a prompt and in a non-interactive note.
    ///
    /// This is the sentence that makes the question answerable. "Import a peer's
    /// bundle?" is not a question anyone can answer well; "this bundle contains
    /// 3 concepts with hidden control characters" is.
    #[must_use]
    pub fn summary(&self) -> String {
        match &self.screened {
            Err(e) => format!("unreadable: {e}"),
            Ok(r) if r.concepts_quarantined > 0 || r.concepts_blocked > 0 => format!(
                "{} concept(s), {} quarantined, {} blocked by the content screen [{}]",
                r.concepts_read,
                r.concepts_quarantined,
                r.concepts_blocked,
                r.screen_classes.join(", ")
            ),
            Ok(r) => format!("{} concept(s), screened clean", r.concepts_read),
        }
    }
}

/// The peers a store holds an `extref:` placeholder for.
///
/// # Errors
/// Propagates a store read failure.
pub fn referenced_peers(store: &Store) -> anyhow::Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    for node in store.nodes_by_kind(&rto_graph::NodeKind::Other(
        rto_graph::EXTERNAL_REF_KIND.to_owned(),
    ))? {
        if let Some(qualified) = rto_graph::external_ref_target(&node)
            && let Some((project, _)) = rto_graph::parse_qualified(&qualified)
        {
            out.insert(project.to_owned());
        }
    }
    Ok(out)
}

/// Screen every bundle `store` references and pair it with the recorded answer.
///
/// Bundles whose answer holds are **excluded**: the whole point of recording is
/// that they are not raised again. What comes back is the set somebody has to
/// decide about.
///
/// # Errors
/// Propagates a store read failure. A bundle that will not *read* is not an
/// error — it is reported as one of the things to decide about, because a peer
/// publishing an unreadable bundle is exactly the case an operator should see.
pub fn undecided(store: &Store, bundles: &[OkfBundle]) -> anyhow::Result<Vec<Discovered>> {
    let referenced = referenced_peers(store)?;
    let mut out = Vec::new();
    for bundle in bundles {
        if !referenced.contains(&bundle.peer) {
            continue;
        }
        let screened = screen_bundle(&bundle.bundle);
        let root = crate::okf_root_key(&bundle.bundle);
        let classes = match &screened {
            Ok(r) => {
                let borrowed: Vec<&str> = r.screen_classes.iter().map(String::as_str).collect();
                rto_graph::screen_fingerprint(&borrowed)
            }
            Err(_) => String::new(),
        };
        let state = store.okf_consent_holds(&bundle.peer, &root, &classes)?;
        if state.decision().is_some() {
            continue;
        }
        out.push(Discovered {
            bundle: bundle.clone(),
            state,
            screened,
        });
    }
    Ok(out)
}

/// Read a bundle for its report only, discarding the facts.
///
/// `Trust::Acknowledge` because the screen does not depend on the trust mode and
/// this read exists only to produce the summary the question is asked with. The
/// real import re-reads with the answer that was given and with the placeholder
/// keys to fill, which this read deliberately does not have.
fn screen_bundle(root: &Path) -> Result<read::OkfReport, String> {
    let files = crate::read_bundle_files(root).map_err(|e| e.to_string())?;
    read::read_bundle(
        &root.display().to_string(),
        &files,
        &read::ReadOptions {
            trust: read::Trust::Acknowledge,
            peer: "",
            extref_keys: &[],
        },
    )
    .map(|i| i.report)
    .map_err(|e| e.to_string())
}

/// Whether there is a human to ask.
///
/// `stdin`, following the three prompts already in the tree (`ask_to_send`,
/// `model rm`, `model pull`): a prompt is only meaningful if the answer can be
/// typed, and it is the *input* side that decides that.
#[must_use]
pub fn may_prompt() -> bool {
    std::io::IsTerminal::is_terminal(&std::io::stdin())
}

/// The text of the consent question, written to stderr before it is asked.
///
/// Carries the peer, the path, why it is being raised, and the screening
/// summary — the last of which is the part that makes the question answerable.
#[must_use]
pub fn prompt_text(d: &Discovered) -> String {
    let why = d
        .state
        .why_asking()
        .unwrap_or_else(|| "not seen before".to_owned());
    format!(
        "\n{peer} publishes an OKF bundle, and this graph references it.\n\
         \n\
         bundle:   {path}\n\
         asking:   {why}\n\
         contains: {summary}\n\
         \n\
         Reading it puts {peer}'s prose into this graph, where the model-facing\n\
         tools return it as grounding. Their confirmations are theirs, not ours.\n\
         \n\
         [t] trust       import at `external-<their tier>`, keeping what they claimed\n\
         [a] acknowledge import at `external-inferred`: their information, not their\n\
         \x20               confirmation\n\
         [i] ignore      leave the cross-repo placeholder as it is\n\
         \n",
        peer = d.bundle.peer,
        path = d.bundle.bundle.display(),
        why = why,
        summary = d.summary(),
    )
}

/// The one-line note printed when there is nobody to ask.
///
/// Carries the same three facts the prompt does — who, what it contains, and why
/// it is being raised — because a note that only said "there is a bundle" would
/// leave the reader unable to tell "never asked" from "you answered, and it has
/// since changed", which are different problems with different fixes.
#[must_use]
pub fn note_text(d: &Discovered) -> String {
    format!(
        "roteiro: {peer} publishes an OKF bundle at {path} ({summary}), and it is \
         undecided: {why}. This run is not interactive, so it was **ignored** and nothing \
         was recorded — a graph does not adopt a stranger's concepts because nobody was \
         there to object. Decide it with `roteiro import --from okf {path}` (add --trust \
         to keep their tiers).",
        peer = d.bundle.peer,
        path = d.bundle.bundle.display(),
        summary = d.summary(),
        why = d
            .state
            .why_asking()
            .unwrap_or_else(|| "not seen before".to_owned()),
    )
}

/// Ask, and return the answer. `None` when the reader declined to answer at all
/// (EOF, or anything unrecognised), which is read as `ignore` by the caller.
///
/// Follows `ask_to_send`'s idiom exactly: the disclosure and the prompt go to
/// **stderr**, so piping the answer does not pipe the receipt, and the default on
/// an empty line is the cautious one.
///
/// # Errors
/// Propagates a failure to read stdin.
pub fn ask(d: &Discovered) -> anyhow::Result<OkfDecision> {
    use std::io::Write as _;
    eprint!("{}", prompt_text(d));
    eprint!("trust / acknowledge / ignore? [t/a/I] ");
    std::io::stderr().flush().ok();
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(match answer.trim().to_ascii_lowercase().as_str() {
        "t" | "trust" => OkfDecision::Trust,
        "a" | "ack" | "acknowledge" => OkfDecision::Acknowledge,
        // Everything else, including an empty line and EOF. An unrecognised
        // answer is not a grant.
        _ => OkfDecision::Ignore,
    })
}

/// Which peers have already been mentioned in this process.
///
/// A SIGHUP workspace reload re-scans, and a note printed on every reload is
/// noise that trains the reader to skip the line. One mention per peer per
/// process is the budget.
#[derive(Debug, Default)]
pub struct Noted(BTreeSet<String>);

impl Noted {
    /// Whether `peer` should be mentioned now, marking it as seen either way.
    ///
    /// Two caps in one: a peer already mentioned in this process is not
    /// mentioned again, and no more than [`NOTE_LIMIT`] peers are named at all.
    /// [`Noted::suppressed`] is how the rest are accounted for, because a cap
    /// that silently drops the remainder is a cap that hides them.
    pub fn should_note(&mut self, peer: &str) -> bool {
        self.0.insert(peer.to_owned()) && self.0.len() <= NOTE_LIMIT
    }

    /// How many peers were seen beyond [`NOTE_LIMIT`] and therefore not named.
    #[must_use]
    pub fn suppressed(&self) -> usize {
        self.0.len().saturating_sub(NOTE_LIMIT)
    }
}

/// How many undecided peers are named before the rest are counted.
pub const NOTE_LIMIT: usize = 3;

#[cfg(test)]
mod tests {
    use super::{NOTE_LIMIT, Noted};

    #[test]
    fn a_peer_is_noted_once_per_process() {
        let mut noted = Noted::default();
        assert!(noted.should_note("acme"));
        assert!(!noted.should_note("acme"), "a reload must not reprint it");
        assert_eq!(noted.suppressed(), 0);
    }

    #[test]
    fn beyond_the_limit_peers_are_counted_rather_than_named() {
        let mut noted = Noted::default();
        for i in 0..NOTE_LIMIT {
            assert!(noted.should_note(&format!("peer{i}")));
        }
        assert!(!noted.should_note("one-too-many"));
        assert!(!noted.should_note("two-too-many"));
        assert_eq!(
            noted.suppressed(),
            2,
            "the cap must account for what it did not print"
        );
    }
}

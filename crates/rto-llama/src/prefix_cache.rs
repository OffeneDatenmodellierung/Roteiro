//! Reuse a prompt's byte-identical preamble instead of re-prefilling it (#578).
//!
//! `serve` builds a context per generation and prefills the whole prompt every
//! time. For a one-shot question that is right. For a multi-turn agent client it
//! is not: the system prompt and the client's `tools` array are re-sent verbatim
//! on every turn, and re-prefilling them measured at **3.13 ms per prompt token**
//! — about 21 s per turn for a 32 KiB tool surface, ten minutes across a 30-turn
//! session, all of it spent recomputing a result the previous turn already had.
//!
//! # What the experiments decided, and what they ruled out
//!
//! `tests/state_reuse.rs` establishes three facts, each of which had to be true
//! for this module to be possible at all on the primary model:
//!
//! 1. **The saved state carries the recurrent half.** `qwen3.8-27b` is a hybrid:
//!    16 of its 64 layers carry KV and the other 48 carry Gated Delta Net
//!    recurrent state. `state_seq_get`/`set` round-trips both, so a restore
//!    continues token-for-token identically to a full prefill.
//! 2. **A snapshot is not welded to the window it was taken at.** It restores
//!    into a larger, a smaller and an equal `n_ctx` alike, all exact. Without
//!    this, reuse and per-request window sizing (#486) genuinely could not
//!    coexist and one would have had to be given up.
//! 3. **Every entry costs a fixed ~149.6 MiB** on top of 64 KiB/token, because
//!    the recurrent module serialises whole regardless of prompt length. That
//!    inverts the usual cache economics — there is no cheap small entry — and is
//!    why [`MIN_PREFIX_TOKENS`] exists and why the budget is in whole MiB.
//!
//! It also rules a shape out. Recurrent state is a running summary with no
//! per-position structure, so it **cannot be rewound**: `kv_cache_seq_rm` reaches
//! only the 16 attention layers, and trimming a restored sequence back to a
//! shorter shared prefix would leave the other 48 summarising tokens that are no
//! longer in the sequence — silently, with no error. So this module only ever
//! restores a snapshot **whole**, into a fresh context, at position 0. It never
//! trims and never gives partial credit for a prefix shorter than an entry.
//!
//! # Where the boundary comes from
//!
//! Nothing tells the engine where the preamble ends. It could be told — #578's
//! question 5 suggests `prompt_cache_key` — but it does not have to be: two
//! consecutive prompts from the same client *share* their preamble, so the
//! boundary is their longest common prefix and the cache can read it off the
//! traffic. [`PrefixCache::boundary_for`] is that, and it means a client gets the
//! benefit without cooperating, and a client whose prompts have nothing in common
//! is never charged for an entry it would not hit.
//!
//! The boundary needs no semantic meaning. A restore is exact at any token
//! position, so landing mid-sentence is harmless — fact 1 above is what makes
//! that true, and it is the reason this can be a purely mechanical rule.
//!
//! # Why this type is generic
//!
//! `S` is the saved state — `llama_cpp_2::context::session::SeqState` in
//! production. Generic so that *this* file, which holds every policy decision
//! worth arguing about, is testable without a 17 GiB model and without the
//! `llama` feature: the eviction rule, the floor, the boundary rule and the
//! match rule are all exercised below over a `Vec<u8>` stand-in.

use std::collections::HashMap;

/// The shortest preamble worth an entry, in tokens.
///
/// Every entry costs a fixed ~149.6 MiB whatever it covers (fact 3 above), so a
/// short prefix is nearly all overhead: at the measured 3.13 ms/token, 512 tokens
/// is ~1.6 s saved per turn for that 150 MiB, and 32 tokens would be 0.1 s for
/// the same 150 MiB. The floor is what stops a cache with a memory budget
/// spending it on entries that cannot repay it.
pub const MIN_PREFIX_TOKENS: usize = 512;

/// One cached preamble.
struct Entry<S> {
    /// The exact tokens this state was produced by. Compared in full on lookup:
    /// a hash would make a collision a silently wrong answer, and the comparison
    /// is a memcmp against a prompt already in hand.
    tokens: Vec<i32>,
    state: S,
    /// What the saved state occupies, for the budget. Supplied by the caller
    /// because only the native layer can measure it.
    bytes: usize,
    /// Monotonic tick of last use, for LRU.
    used: u64,
}

/// Saved preamble states, bounded by a byte budget and evicted least-recently-used.
///
/// Keyed by model id as well as tokens, because a state is a specific model's
/// internal representation and restoring one model's bytes into another is not a
/// wrong answer but undefined behaviour.
pub struct PrefixCache<S> {
    budget_bytes: usize,
    entries: Vec<(String, Entry<S>)>,
    /// The previous prompt seen per model, which is the other half of a boundary.
    /// Tokens only — a few tens of KiB against the entries' hundreds of MiB.
    last_prompt: HashMap<String, Vec<i32>>,
    tick: u64,
}

impl<S> PrefixCache<S> {
    /// A cache that may hold `budget_bytes` of saved state. `0` disables it: no
    /// entry is ever stored, and no prompt is ever retained for a boundary.
    #[must_use]
    pub fn new(budget_bytes: usize) -> Self {
        Self {
            budget_bytes,
            entries: Vec::new(),
            last_prompt: HashMap::new(),
            tick: 0,
        }
    }

    /// Whether this cache can hold anything at all.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.budget_bytes > 0
    }

    /// The longest cached preamble that `tokens` begins **strictly** within, for
    /// `model`.
    ///
    /// Returns the state to restore and how many of `tokens` it covers, so the
    /// caller batches only the rest. `None` when nothing applies — which includes
    /// three distinct cases worth naming, because each would be a defect if it
    /// returned a hit:
    ///
    /// - An entry sharing only a *shorter* prefix. An entry is restored whole or
    ///   not at all (see the module docs), so a partial match is a miss.
    /// - A prompt *shorter* than the entry: the state would run past the prompt.
    /// - A prompt **exactly equal** to the entry. The caller batches
    ///   `tokens[covered..]`, so full coverage leaves it with an empty batch and
    ///   no token to carry logits — it would decode nothing and then sample from
    ///   whatever was last in the logits buffer. Reachable in ordinary use: a
    ///   client that sends its system message with no question at all matches its
    ///   own cached preamble exactly. Raised in review of #578.
    ///
    /// Returns the state **by value** rather than by reference so the caller can
    /// drop the lock before restoring it. Restoring copies hundreds of MiB into a
    /// context, and generation locks are per *model* — holding one global lock
    /// across that native call would serialise models that otherwise run
    /// concurrently. With `S = Arc<_>` the clone is a refcount bump.
    pub fn longest_prefix(&mut self, model: &str, tokens: &[i32]) -> Option<(S, usize)>
    where
        S: Clone,
    {
        self.tick = self.tick.saturating_add(1);
        let tick = self.tick;
        let best = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, (m, e))| {
                m == model && tokens.len() > e.tokens.len() && tokens.starts_with(&e.tokens)
            })
            .max_by_key(|(_, (_, e))| e.tokens.len())
            .map(|(i, _)| i)?;
        let (_, entry) = &mut self.entries[best];
        entry.used = tick;
        Some((entry.state.clone(), entry.tokens.len()))
    }

    /// Where a snapshot of this prompt would be worth taking, if anywhere.
    ///
    /// The longest common prefix with the previous prompt for `model` — what two
    /// turns of one conversation share, which is the preamble. `None` when the
    /// cache is off, when there is no previous prompt to compare against, when
    /// the shared run is below [`MIN_PREFIX_TOKENS`], or when an entry already
    /// covers at least that much, since re-snapshotting the same boundary would
    /// buy nothing and cost another ~150 MiB.
    ///
    /// Records `tokens` as the previous prompt on the way through, so a caller
    /// asks once per request and the cache learns from every prompt including the
    /// ones it declines to act on.
    pub fn boundary_for(&mut self, model: &str, tokens: &[i32]) -> Option<usize> {
        if !self.enabled() {
            return None;
        }
        let shared = self
            .last_prompt
            .get(model)
            .map(|prev| common_prefix_len(prev, tokens));
        self.last_prompt.insert(model.to_owned(), tokens.to_vec());
        let shared = shared?;
        if shared < MIN_PREFIX_TOKENS {
            return None;
        }
        // "Already covered" means an entry that would actually *hit this prompt*,
        // not merely one of the same length. Comparing lengths alone would make
        // the cache one-entry-per-model: a second conversation with a different
        // preamble of similar size would never be snapshotted, however expensive.
        // Raised in review of #578.
        let covered = self
            .entries
            .iter()
            .filter(|(m, e)| m == model && tokens.len() > e.tokens.len())
            .filter(|(_, e)| tokens.starts_with(&e.tokens))
            .map(|(_, e)| e.tokens.len())
            .max()
            .unwrap_or(0);
        (shared > covered).then_some(shared)
    }

    /// Store `state`, the state after `tokens`, occupying `bytes`.
    ///
    /// Supersedes any entry for this model whose tokens are a prefix of these —
    /// and only those. Entries for other conversations survive and compete for
    /// the budget on their own merits, so one model may hold several preambles.
    ///
    /// Evicts least-recently-used entries until it fits. An entry larger than the
    /// whole budget is dropped rather than stored, and dropping it does not
    /// disturb what is already cached — otherwise one oversized preamble would
    /// evict a working set to make room for something that cannot be kept.
    pub fn store(&mut self, model: &str, tokens: Vec<i32>, state: S, bytes: usize) {
        if !self.enabled() || bytes > self.budget_bytes {
            return;
        }
        self.tick = self.tick.saturating_add(1);
        // Drop the entries this one grew out of: those whose tokens are a prefix
        // of it. Comparing *lengths* here would be wrong — two preambles of
        // similar size are usually different conversations rather than one
        // superseding the other — so the test is the prefix relation.
        //
        // This is a **trade, not a strict improvement**, and an earlier version of
        // this comment overstated it. A dropped 600-token entry is not merely
        // redundant against a new 900-token one: a prompt diverging at token 700
        // matches the short entry and not the long one, so dropping it does lose a
        // hit that was available. It is still the right default, for two reasons.
        // An entry costs a fixed ~150 MiB, so keeping every ancestor of a growing
        // preamble spends the whole budget on one conversation. And the loss is
        // self-healing: a client that really diverges at 700 has 700 as its own
        // learned boundary, `boundary_for` finds no entry covering its prompts,
        // and it is snapshotted again at the cost of one full prefill. Raised in
        // review of #578.
        self.entries
            .retain(|(m, e)| m != model || !tokens.starts_with(&e.tokens));
        while self.used_bytes().saturating_add(bytes) > self.budget_bytes {
            let Some(lru) = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, (_, e))| e.used)
                .map(|(i, _)| i)
            else {
                break;
            };
            self.entries.remove(lru);
        }
        self.entries.push((
            model.to_owned(),
            Entry {
                tokens,
                state,
                bytes,
                used: self.tick,
            },
        ));
    }

    /// Drop the entry for `model` whose tokens are exactly `tokens`.
    ///
    /// For a state llama.cpp declined to restore: keeping it would re-offer the
    /// same refusal on every subsequent turn, turning one wasted copy into a
    /// permanent one.
    ///
    /// **By identity, not by length.** Several preambles for one model coexist and
    /// nothing stops two of them being the same length, so dropping by length
    /// would let one refused restore evict an unrelated conversation's entry — a
    /// 150 MiB loss and a full re-prefill for a client that did nothing wrong.
    /// Raised in review of #578, and the third place in this module where a length
    /// stood in for an identity.
    pub fn forget(&mut self, model: &str, tokens: &[i32]) {
        self.entries
            .retain(|(m, e)| m != model || e.tokens != tokens);
    }

    /// What the cache is holding, in bytes.
    #[must_use]
    pub fn used_bytes(&self) -> usize {
        self.entries.iter().map(|(_, e)| e.bytes).sum()
    }

    /// How many preambles are cached.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache is holding nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// How many leading elements `a` and `b` share.
fn common_prefix_len(a: &[i32], b: &[i32]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

#[cfg(test)]
mod tests {
    use super::{MIN_PREFIX_TOKENS, PrefixCache, common_prefix_len};

    /// A prompt of `n` tokens beginning with `preamble`.
    fn prompt(preamble: &[i32], n: usize, salt: i32) -> Vec<i32> {
        let mut v = preamble.to_vec();
        v.extend((0..n).map(|i| salt * 1_000_000 + i32::try_from(i).unwrap()));
        v
    }

    fn preamble(n: usize) -> Vec<i32> {
        (0..n).map(|i| i32::try_from(i).unwrap()).collect()
    }

    /// A prompt that would hit an entry of `n` tokens: the preamble plus one more,
    /// because an entry only matches a prompt strictly longer than itself.
    fn asking(n: usize) -> Vec<i32> {
        let mut v = preamble(n);
        v.push(-1);
        v
    }

    #[test]
    fn the_boundary_is_what_two_consecutive_prompts_share() {
        let mut c: PrefixCache<Vec<u8>> = PrefixCache::new(1 << 30);
        let pre = preamble(MIN_PREFIX_TOKENS + 100);

        // The first prompt has nothing to be compared against.
        assert_eq!(c.boundary_for("m", &prompt(&pre, 20, 1)), None);
        // The second shares the preamble, and that is the boundary — not the
        // whole prompt, and not a guess about where a system message ends.
        assert_eq!(
            c.boundary_for("m", &prompt(&pre, 30, 2)),
            Some(pre.len()),
            "the shared run is the preamble"
        );
    }

    #[test]
    fn a_shared_run_below_the_floor_is_not_worth_an_entry() {
        let mut c: PrefixCache<Vec<u8>> = PrefixCache::new(1 << 30);
        let short = preamble(MIN_PREFIX_TOKENS - 1);
        c.boundary_for("m", &prompt(&short, 10, 1));
        assert_eq!(
            c.boundary_for("m", &prompt(&short, 10, 2)),
            None,
            "every entry costs a fixed ~150 MiB, so a short prefix cannot repay one"
        );
    }

    #[test]
    fn a_disabled_cache_stores_nothing_and_proposes_nothing() {
        let mut c: PrefixCache<Vec<u8>> = PrefixCache::new(0);
        let pre = preamble(MIN_PREFIX_TOKENS + 10);
        assert!(!c.enabled());
        c.boundary_for("m", &prompt(&pre, 5, 1));
        assert_eq!(c.boundary_for("m", &prompt(&pre, 5, 2)), None);
        c.store("m", pre.clone(), vec![0u8; 16], 16);
        assert!(c.is_empty(), "a disabled cache holds nothing");
        assert_eq!(c.longest_prefix("m", &prompt(&pre, 5, 3)), None);
    }

    #[test]
    fn a_hit_covers_the_preamble_and_leaves_the_rest_to_the_caller() {
        let mut c: PrefixCache<Vec<u8>> = PrefixCache::new(1 << 30);
        let pre = preamble(600);
        c.store("m", pre.clone(), vec![7u8; 8], 8);

        let (state, covered) = c
            .longest_prefix("m", &prompt(&pre, 40, 9))
            .expect("the prompt begins with the cached preamble");
        assert_eq!(covered, 600, "the caller batches only the remaining 40");
        assert_eq!(state, vec![7u8; 8]);
    }

    #[test]
    fn a_prompt_that_merely_overlaps_is_a_miss_not_a_partial_credit() {
        // Recurrent state cannot be rewound, so an entry is restored whole or not
        // at all. A prompt sharing the first 599 of a 600-token entry must miss —
        // restoring and trimming would be silently wrong on 48 of 64 layers.
        let mut c: PrefixCache<Vec<u8>> = PrefixCache::new(1 << 30);
        let pre = preamble(600);
        c.store("m", pre.clone(), vec![1u8; 8], 8);

        let mut diverges = pre.clone();
        diverges[599] = -1;
        assert_eq!(
            c.longest_prefix("m", &diverges),
            None,
            "one differing token in the entry is a miss"
        );
        // And a prompt *shorter* than the entry is a miss too, however much it
        // shares: the state runs past the end of the prompt.
        assert_eq!(c.longest_prefix("m", &pre[..599]), None);
    }

    /// The case review found: a prompt that *is* the cached preamble.
    ///
    /// Reachable in ordinary use — a client sending its system message with no
    /// question matches its own entry exactly — and a hit would leave the caller
    /// batching zero tokens, so nothing would carry logits.
    #[test]
    fn a_prompt_equal_to_the_entry_is_a_miss_because_it_would_leave_nothing_to_batch() {
        let mut c: PrefixCache<Vec<u8>> = PrefixCache::new(1 << 30);
        let pre = preamble(600);
        c.store("m", pre.clone(), vec![1u8; 8], 8);

        assert_eq!(
            c.longest_prefix("m", &pre),
            None,
            "exact equality is a miss"
        );
        // One token past it is the shortest prompt that may hit.
        let mut just_longer = pre.clone();
        just_longer.push(-7);
        assert_eq!(
            c.longest_prefix("m", &just_longer).map(|(_, n)| n),
            Some(600)
        );
    }

    #[test]
    fn one_model_never_answers_for_another() {
        let mut c: PrefixCache<Vec<u8>> = PrefixCache::new(1 << 30);
        let pre = preamble(600);
        c.store("qwen", pre.clone(), vec![1u8; 8], 8);
        assert!(c.longest_prefix("qwen", &prompt(&pre, 5, 1)).is_some());
        assert_eq!(
            c.longest_prefix("bge", &prompt(&pre, 5, 1)),
            None,
            "a saved state is one model's internal representation"
        );
    }

    #[test]
    fn the_budget_evicts_least_recently_used() {
        // Two 100-byte entries fit; the third must displace one.
        let mut c: PrefixCache<Vec<u8>> = PrefixCache::new(250);
        c.store("a", preamble(600), vec![0u8; 1], 100);
        c.store("b", preamble(601), vec![0u8; 1], 100);
        // Touch `a`, making `b` the least recently used.
        assert!(c.longest_prefix("a", &asking(600)).is_some());
        c.store("c", preamble(602), vec![0u8; 1], 100);

        assert_eq!(c.len(), 2, "{} bytes held", c.used_bytes());
        assert!(c.used_bytes() <= 250);
        assert!(
            c.longest_prefix("a", &asking(600)).is_some(),
            "recently used"
        );
        assert!(c.longest_prefix("c", &asking(602)).is_some(), "just stored");
        assert!(c.longest_prefix("b", &asking(601)).is_none(), "evicted");
    }

    #[test]
    fn an_entry_larger_than_the_budget_is_declined_without_disturbing_the_rest() {
        // Otherwise one oversized preamble empties a working set to make room for
        // something that cannot be kept — the worst of both.
        let mut c: PrefixCache<Vec<u8>> = PrefixCache::new(250);
        c.store("a", preamble(600), vec![0u8; 1], 100);
        c.store("big", preamble(700), vec![0u8; 1], 10_000);
        assert_eq!(c.len(), 1);
        assert!(c.longest_prefix("a", &asking(600)).is_some());
    }

    #[test]
    fn a_longer_preamble_supersedes_a_shorter_one_for_the_same_model() {
        // The shorter can only be hit by prompts the longer also matches, so
        // keeping both would spend ~150 MiB on an entry that can never win.
        let mut c: PrefixCache<Vec<u8>> = PrefixCache::new(1 << 30);
        c.store("m", preamble(600), vec![0u8; 1], 100);
        c.store("m", preamble(900), vec![0u8; 1], 100);
        assert_eq!(c.len(), 1);
        let (_, covered) = c
            .longest_prefix("m", &prompt(&preamble(900), 5, 1))
            .expect("hit");
        assert_eq!(covered, 900);
    }

    /// Two conversations, two preambles, one model. Neither supersedes the other,
    /// so keeping only the longer would silently make this cache
    /// one-entry-per-model — the second client would never see a hit.
    #[test]
    fn two_different_preambles_for_one_model_both_survive() {
        let mut c: PrefixCache<Vec<u8>> = PrefixCache::new(1 << 30);
        let a: Vec<i32> = (0..600).collect();
        let b: Vec<i32> = (0..600).map(|i| i + 5_000).collect();
        c.store("m", a.clone(), vec![1u8; 1], 100);
        c.store("m", b.clone(), vec![2u8; 1], 100);

        assert_eq!(c.len(), 2, "neither preamble supersedes the other");
        let mut ask_a = a.clone();
        ask_a.push(-1);
        let mut ask_b = b.clone();
        ask_b.push(-1);
        assert_eq!(
            c.longest_prefix("m", &ask_a).map(|(s, _)| s),
            Some(vec![1u8; 1])
        );
        assert_eq!(
            c.longest_prefix("m", &ask_b).map(|(s, _)| s),
            Some(vec![2u8; 1])
        );
    }

    /// And a second conversation is still *proposed*, even when the cache already
    /// holds a longer entry for the same model that cannot serve it.
    #[test]
    fn a_second_conversation_is_proposed_despite_a_longer_unrelated_entry() {
        let mut c: PrefixCache<Vec<u8>> = PrefixCache::new(1 << 30);
        c.store("m", (0..900).collect::<Vec<i32>>(), vec![0u8; 1], 100);

        let other: Vec<i32> = (0..MIN_PREFIX_TOKENS + 50)
            .map(|i| i32::try_from(i).unwrap() + 9_000)
            .collect();
        c.boundary_for("m", &prompt(&other, 5, 1));
        assert_eq!(
            c.boundary_for("m", &prompt(&other, 5, 2)),
            Some(other.len()),
            "a longer entry that cannot match this prompt does not cover it"
        );
    }

    /// A refused restore must drop exactly the entry that was refused.
    ///
    /// Two preambles of one model may be the same length — nothing prevents it,
    /// and the cache explicitly holds several per model — so identifying an entry
    /// by its length would let one refusal evict an unrelated conversation.
    #[test]
    fn forgetting_a_refused_entry_leaves_a_same_length_sibling_alone() {
        let mut c: PrefixCache<Vec<u8>> = PrefixCache::new(1 << 30);
        let a: Vec<i32> = (0..600).collect();
        let b: Vec<i32> = (0..600).map(|i| i + 5_000).collect();
        assert_eq!(a.len(), b.len(), "the case only exists at equal lengths");
        c.store("m", a.clone(), vec![1u8; 1], 100);
        c.store("m", b.clone(), vec![2u8; 1], 100);

        c.forget("m", &a);

        assert_eq!(c.len(), 1, "only the refused entry goes");
        let mut ask_b = b.clone();
        ask_b.push(-1);
        assert_eq!(
            c.longest_prefix("m", &ask_b).map(|(s, _)| s),
            Some(vec![2u8; 1]),
            "the sibling still serves its own client"
        );
    }

    #[test]
    fn a_boundary_already_covered_is_not_proposed_again() {
        let mut c: PrefixCache<Vec<u8>> = PrefixCache::new(1 << 30);
        let pre = preamble(600);
        c.store("m", pre.clone(), vec![0u8; 1], 100);
        c.boundary_for("m", &prompt(&pre, 5, 1));
        assert_eq!(
            c.boundary_for("m", &prompt(&pre, 5, 2)),
            None,
            "re-snapshotting the same boundary costs ~150 MiB and buys nothing"
        );
    }

    #[test]
    fn a_longer_shared_run_than_the_entry_is_proposed() {
        // The conversation settled on a longer stable preamble than the cache has;
        // that is worth the second snapshot, and it supersedes the first.
        let mut c: PrefixCache<Vec<u8>> = PrefixCache::new(1 << 30);
        c.store("m", preamble(600), vec![0u8; 1], 100);
        let longer = preamble(900);
        c.boundary_for("m", &prompt(&longer, 5, 1));
        assert_eq!(c.boundary_for("m", &prompt(&longer, 5, 2)), Some(900));
    }

    #[test]
    fn the_common_prefix_of_two_unrelated_prompts_is_nothing() {
        assert_eq!(common_prefix_len(&[1, 2, 3], &[9, 2, 3]), 0);
        assert_eq!(common_prefix_len(&[1, 2, 3], &[1, 2, 9]), 2);
        assert_eq!(common_prefix_len(&[1, 2], &[1, 2, 3]), 2);
        assert_eq!(common_prefix_len(&[], &[1]), 0);
    }
}

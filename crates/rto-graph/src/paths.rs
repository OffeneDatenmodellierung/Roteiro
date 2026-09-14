//! **Which repository paths the scan reads, and how much of each it mines** —
//! the one rule every reader of repository bytes consults (ADR-0007 `[paths]`,
//! ADR-0026 step 1, issue #840).
//!
//! # Why this exists
//!
//! Before this module there was **no mechanism to exclude a path from
//! extraction**. The only exclusion list in the configuration was `[debt]
//! ignore`, which is scoped to markers and filters the *report* rather than the
//! *graph*: [`crate::debt`] drops an ignored path when reporting, and
//! `debt_density` reuses it so the two agree — but the node is still in the
//! store, and `search`, `explain`, `list_kind`, `path` and **`export`** all
//! still see it. A repository could therefore suppress a false finding from the
//! report it reads and still publish it to a consumer (issue #840).
//!
//! An exclusion that works by *removing the node* has no such gap, and it needs
//! no per-surface honouring: a node that was never stored cannot be exported.
//! That is the whole reason this sits at extraction rather than beside
//! `[debt] ignore`.
//!
//! # Three states, because two requests are different
//!
//! *"Do not mine this as configuration"* and *"do not put this in the graph at
//! all"* are different requests, and two live decisions each need a different
//! one:
//!
//! | class | what it produces | the decision that needs it |
//! |---|---|---|
//! | [`PathClass::Extract`] | everything, as today | every path not named — the default |
//! | [`PathClass::Opaque`] | a `file` node and **nothing else** | issue #812 — a corpus manifest must be **committed in every storage mode**, so a missing `raw/` is *detectable rather than silent*. It must therefore stay in the graph while not being shredded into `config_key` nodes (#839) or scanned for markers (#838). |
//! | [`PathClass::Excluded`] | **no node, and the bytes are never read** | issue #817 — `raw/` is excluded from the standard scan so a source document is not graphed twice, once as its `knowledge/` summary and once as the raw file. |
//!
//! [`PathClass::Opaque`] is not "extract with some rules off". It is *identity
//! without content*: path, blob id, byte and line counts — the facts a
//! `(path, blob id, bytes)` derivation can state about a file it has agreed not
//! to read. That is exactly what #812 asks for, and nothing more.
//!
//! # Why the classes are not a fourth thing
//!
//! A fourth state — *in the graph, mined, but muted from the reports* — already
//! exists: it is `[debt] ignore`, and it is the defect #840 was raised about.
//! It is deliberately not reproduced here.
//!
//! # The rule is consulted, never copied
//!
//! There are two independent readers of committed blobs (ADR-0026 §"The
//! exclusion covers **two** scans, not one"), and excluding a path from one does
//! not exclude it from the other:
//!
//! 1. **Derived extraction** — [`crate::extract::Registry`], which produces
//!    `file` nodes, config keys, symbols, markers and `meta.content`.
//! 2. **The authored layer** — `rto_spec::authored_blobs` walks *every* path
//!    independently and `rto_spec::authored_docs_from` then classifies what it
//!    finds **by content, not by location**. A committed markdown file that
//!    declares `type: adr` is parsed as one of *ours*, wherever it sits, which
//!    is precisely what makes an ingested third-party document dangerous.
//!
//! Both consult *this* type. It travels inside [`crate::IngestConfig`], which
//! already flows to every entry point that reads repository bytes, so a reader
//! that has the ingestion configuration cannot fail to have the path policy too.

/// How much of a path the scan may read — the three states, most permissive
/// first.
///
/// Ordered by how much each admits, so the `Extract` → `Opaque` → `Excluded`
/// progression reads as a narrowing. See the module docs for which decision
/// needs which.
///
/// # Deliberately closed, and not `#[non_exhaustive]`
///
/// The attribute would push a downstream matcher onto a `_ =>` wildcard arm,
/// which is the precise thing this type exists to prevent. Every reader of
/// repository bytes asks [`PathClass::mines`] or [`PathClass::reads`]; a fourth
/// class is a claim that some reader should treat some path a fourth way, and
/// **every** such reader has to reconsider when one arrives. A wildcard arm is
/// how that reconsideration gets skipped silently — the same argument
/// `rto_exec`'s `Gate` is closed on, where folding `NotRun` into a wildcard is
/// the defect its third state exists to catch.
///
/// The set is three because the fourth candidate is already taken: *in the
/// graph, mined, but muted from the reports* is `[debt] ignore`, and reproducing
/// it here is what issue #840 was raised against. Adding a variant is breaking
/// for this published crate, and under `AGENTS.md`'s `rto-*` carve-out that
/// ships as a minor — so the cost of getting it wrong later is a compile error
/// at every call site, which is exactly the price this type wants paid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PathClass {
    /// Extract normally: every extractor, every miner, content capture, markers.
    /// The default for every path a policy does not name.
    #[default]
    Extract,
    /// A `file` node and nothing else — **identity without content**. No
    /// config-key mining, no symbols, no image/audio facts, no marker scan, no
    /// `meta.content`, and the authored classifier declines it.
    Opaque,
    /// Not in the graph at all. No node is produced and the bytes are never
    /// read, so nothing a screen would have caught can reach the store by this
    /// route either.
    Excluded,
}

impl PathClass {
    /// The class's name, as `roteiro config` and a node's `meta.scan` spell it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Extract => "extract",
            Self::Opaque => "opaque",
            Self::Excluded => "excluded",
        }
    }

    /// Whether a reader may derive anything **beyond a file's identity** from
    /// this path: config keys, symbols, markers, annotations, content.
    ///
    /// The question every miner asks. It is deliberately not `== Extract`
    /// spelled out at each call site: a fourth class added later must make every
    /// miner reconsider, and a named predicate is where that reconsideration
    /// lands.
    #[must_use]
    pub const fn mines(self) -> bool {
        matches!(self, Self::Extract)
    }

    /// Whether the bytes at this path are read at all.
    ///
    /// `false` only for [`PathClass::Excluded`]. An [`PathClass::Opaque`] path
    /// is read — its length and line count are facts about it — but nothing is
    /// derived from what the bytes *say*.
    #[must_use]
    pub const fn reads(self) -> bool {
        !matches!(self, Self::Excluded)
    }
}

/// The repository's declared path policy: which paths are excluded from the
/// scan, and which are read as opaque bytes (ADR-0007 `[paths]`).
///
/// Empty by default, and an empty policy is exactly today's behaviour — see
/// [`PathPolicy::fingerprint`], which returns `0` for one so that declaring
/// nothing leaves every existing extraction-cache key untouched.
///
/// # There is no built-in default, and that is a decision
///
/// No path is excluded unless a repository says so — not `raw/`, not
/// `manifest/`, not `vendor/`. A built-in default would be a **silent behaviour
/// change**: any existing repository that happens to have a directory of that
/// name would lose it from its graph on upgrade, with nothing said. Opting in is
/// a declaration somebody wrote and a reviewer can see; opting out of a default
/// requires knowing the default exists. ADR-0026's `raw/` exclusion is therefore
/// a line in `roteiro.toml`, not a constant here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PathPolicy {
    /// Globs whose matching paths are [`PathClass::Excluded`].
    exclude: Vec<String>,
    /// Globs whose matching paths are [`PathClass::Opaque`].
    opaque: Vec<String>,
}

/// The policy a build with nothing declared uses — see [`PathPolicy::empty`].
static EMPTY: PathPolicy = PathPolicy {
    exclude: Vec::new(),
    opaque: Vec::new(),
};

impl PathPolicy {
    /// A policy from the two declared glob lists.
    ///
    /// Patterns are matched anchored end-to-end against the whole repo-relative
    /// path, with the same semantics `[debt] ignore` uses — see [`glob_match`],
    /// which is the one implementation both consult.
    #[must_use]
    pub fn new(exclude: Vec<String>, opaque: Vec<String>) -> Self {
        Self { exclude, opaque }
    }

    /// The empty policy: every path is [`PathClass::Extract`].
    ///
    /// Returned by reference and `'static` so [`crate::IngestConfig`] can stay
    /// `Copy` while carrying a policy — the default it borrows outlives every
    /// caller.
    #[must_use]
    pub fn empty() -> &'static Self {
        &EMPTY
    }

    /// Whether nothing is declared, so every path extracts normally.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.exclude.is_empty() && self.opaque.is_empty()
    }

    /// How `path` (repo-relative, slash-separated) is read.
    ///
    /// `exclude` is tested first, so a path matching both lists is excluded:
    /// between two declarations the narrower one wins, because a repository that
    /// said "never read this" should not have that undone by a second, weaker
    /// statement about the same bytes.
    #[must_use]
    pub fn classify(&self, path: &str) -> PathClass {
        if self.exclude.iter().any(|g| glob_match(g, path)) {
            PathClass::Excluded
        } else if self.opaque.iter().any(|g| glob_match(g, path)) {
            PathClass::Opaque
        } else {
            PathClass::Extract
        }
    }

    /// The declared `exclude` globs, in declaration order.
    #[must_use]
    pub fn exclude_patterns(&self) -> &[String] {
        &self.exclude
    }

    /// The declared `opaque` globs, in declaration order.
    #[must_use]
    pub fn opaque_patterns(&self) -> &[String] {
        &self.opaque
    }

    /// A cache-key contribution that is **`0` for an empty policy**, so a
    /// repository declaring nothing keeps every extraction-cache key it already
    /// has — the same property [`crate::IngestConfig`]'s toggles have.
    ///
    /// This is load-bearing rather than an optimisation. Extraction is cached by
    /// `(path, blob id, env)`, and the policy changes what an *unchanged* blob
    /// extracts to: without it in `env`, adding `manifest/**` to `opaque` would
    /// serve the previously-mined `config_key` nodes straight back out of the
    /// cache, and `sync` would report itself up to date while the graph still
    /// held everything the declaration was written to remove.
    #[must_use]
    pub fn fingerprint(&self) -> u64 {
        if self.is_empty() {
            return 0;
        }
        // FNV-1a over the two lists with distinct per-list tags, so moving a
        // pattern from `opaque` to `exclude` changes the tag.
        let mut h = 0xcbf2_9ce4_8422_2325_u64;
        let mut fold = |bytes: &[u8]| {
            for &b in bytes {
                h ^= u64::from(b);
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
        };
        for pattern in &self.exclude {
            fold(b"x\0");
            fold(pattern.as_bytes());
        }
        for pattern in &self.opaque {
            fold(b"o\0");
            fold(pattern.as_bytes());
        }
        h
    }
}

/// Match a slash-separated `path` against a glob `pattern`, anchored end-to-end.
/// `?` matches one non-`/` character, `*` matches any run within a single path
/// segment, and `**` matches zero or more whole segments.
///
/// One implementation, two consumers: `[debt] ignore`'s report filter
/// ([`crate::debt`]) and `[paths]`'s extraction filter ([`PathPolicy`]). They
/// are different mechanisms deliberately — one mutes a report, the other removes
/// a node — but a user writing `vendor/**` in either is entitled to have it mean
/// the same thing, and this repository has closed "the same rule in two copies"
/// often enough to place the shared half here rather than beside one caller.
#[must_use]
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let pat: Vec<&str> = pattern.split('/').collect();
    let seg: Vec<&str> = path.split('/').collect();
    match_segments(&pat, &seg)
}

/// Anchored match of glob segments `pat` against path segments `seg`, with `**`
/// consuming zero or more segments.
fn match_segments(pat: &[&str], seg: &[&str]) -> bool {
    match pat.first() {
        None => seg.is_empty(),
        Some(&"**") => (0..=seg.len()).any(|i| match_segments(&pat[1..], &seg[i..])),
        Some(token) => {
            !seg.is_empty() && match_token(token, seg[0]) && match_segments(&pat[1..], &seg[1..])
        }
    }
}

/// Match a single path segment `s` against a `pattern` token containing `*`
/// (any run, no `/`) and `?` (one char, no `/`).
fn match_token(pattern: &str, s: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let chars: Vec<char> = s.chars().collect();
    match_token_chars(&pat, &chars)
}

/// Recursive char-slice matcher backing [`match_token`].
fn match_token_chars(pat: &[char], chars: &[char]) -> bool {
    match pat.first() {
        None => chars.is_empty(),
        Some('*') => (0..=chars.len()).any(|i| match_token_chars(&pat[1..], &chars[i..])),
        Some('?') => !chars.is_empty() && match_token_chars(&pat[1..], &chars[1..]),
        Some(&ch) => {
            !chars.is_empty() && chars[0] == ch && match_token_chars(&pat[1..], &chars[1..])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PathClass, PathPolicy, glob_match};

    /// The default is not "exclude nothing by accident" — it is "exclude nothing
    /// because nothing was declared", and an empty policy must be free.
    #[test]
    fn an_empty_policy_extracts_everything_and_costs_no_cache_key() {
        let policy = PathPolicy::default();
        assert!(policy.is_empty());
        assert_eq!(policy.classify("src/main.rs"), PathClass::Extract);
        assert_eq!(policy.classify("raw/paper.pdf"), PathClass::Extract);
        assert_eq!(
            policy.fingerprint(),
            0,
            "a declared-nothing policy must leave every existing cache key alone"
        );
        assert_eq!(PathPolicy::empty(), &policy);
    }

    #[test]
    fn exclude_and_opaque_classify_independently() {
        let policy = PathPolicy::new(vec!["raw/**".into()], vec!["manifest/**".into()]);
        assert_eq!(policy.classify("raw/paper.pdf"), PathClass::Excluded);
        assert_eq!(policy.classify("raw/nested/deep.md"), PathClass::Excluded);
        assert_eq!(policy.classify("manifest/papers.jsonl"), PathClass::Opaque);
        assert_eq!(policy.classify("src/main.rs"), PathClass::Extract);
        assert_eq!(policy.classify("rawish/a.rs"), PathClass::Extract);
    }

    /// The narrower declaration wins, so a second weaker statement about the same
    /// bytes cannot undo "never read this".
    #[test]
    fn exclude_beats_opaque_when_both_match() {
        let policy = PathPolicy::new(vec!["corpus/**".into()], vec!["corpus/**".into()]);
        assert_eq!(policy.classify("corpus/a.json"), PathClass::Excluded);
    }

    /// Moving a pattern between the lists must change the extraction identity, or
    /// the cache serves facts the new declaration was written to remove.
    #[test]
    fn the_fingerprint_separates_the_two_lists() {
        let excluded = PathPolicy::new(vec!["raw/**".into()], Vec::new());
        let opaque = PathPolicy::new(Vec::new(), vec!["raw/**".into()]);
        assert_ne!(excluded.fingerprint(), opaque.fingerprint());
        assert_ne!(excluded.fingerprint(), 0);
        assert_eq!(
            excluded.fingerprint(),
            PathPolicy::new(vec!["raw/**".into()], Vec::new()).fingerprint(),
            "deterministic for an identical declaration"
        );
    }

    #[test]
    fn the_three_classes_answer_the_two_questions_readers_ask() {
        assert!(PathClass::Extract.mines() && PathClass::Extract.reads());
        assert!(!PathClass::Opaque.mines() && PathClass::Opaque.reads());
        assert!(!PathClass::Excluded.mines() && !PathClass::Excluded.reads());
    }

    #[test]
    fn glob_matches_segments_and_wildcards() {
        assert!(glob_match("vendor/**", "vendor/lib/a.rs"));
        assert!(glob_match("vendor/**", "vendor"));
        assert!(glob_match("**/generated/*", "src/gen/generated/x.rs"));
        assert!(glob_match("**/*.jsonl", "manifest/papers.jsonl"));
        assert!(!glob_match("src/*.rs", "src/a/b.rs"));
    }
}

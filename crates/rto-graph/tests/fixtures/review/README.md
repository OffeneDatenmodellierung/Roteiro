# Review corpus

`review-corpus.jsonl` — inline review comments GitHub Copilot left on this
repository, each adjudicated against what the maintainer actually did about it.
The class table below carries the counts, and a test asserts it against the data.

**What the sample is, precisely**, because it decides what may be computed from
it: rows for PRs #292–#343 are the *complete* set of Copilot comments on those
twelve PRs over a single day — nothing filtered, so a precision figure over that
subset is meaningful. The later row on #352 is **one selected comment** out of the
eight that PR received; it was added because it extended the compile-failure
class, not because #352 was adjudicated end to end. So a ratio computed over
*all* rows is very slightly biased toward the false class, and anyone quoting one
should either say so or restrict to the twelve-PR subset (`pr <= 343`).

This exists so that "is an automated reviewer any good on *this* codebase?" is a
question with an answer rather than an impression. A review tool is otherwise
unmeasurable: its output is prose, its mistakes are plausible, and nobody
remembers last week's false positives well enough to count them. With a fixed
corpus of known verdicts, any candidate reviewer — a different model, a changed
prompt, a graph-grounded arm, a future `roteiro review` mode — is scored against
the same fixed set of comments, and the numbers are comparable across attempts.

The corpus is a **historical record**, not a live view. It is deliberately not
regenerated from the GitHub API: the rows describe what a reviewer said about a
particular tree at a particular moment, and that must not change because a
comment was later edited, a thread resolved, or a repository made private. The
validation test therefore never touches the network (see below).

## Reading a row

One JSON object per line, exactly these eleven fields:

| Field | Meaning |
|---|---|
| `id` | GitHub review-comment id — the primary key, unique across the corpus |
| `pr` | Pull-request number |
| `reviewer` | Which reviewer produced it (`github-copilot` throughout, so far) |
| `reviewed_sha` | **The commit the comment was made against** — see below |
| `path` | File the comment is anchored to |
| `line` | Line in that file, new-side |
| `verdict` | `real` or `false` |
| `defect_class` | One of the classes in the table below |
| `fix_commit` | Short sha of the commit that fixed it, where one is identifiable |
| `description` | One line stating the defect, or stating why the claim is wrong |
| `comment_url` | Permalink to the original comment |

## `reviewed_sha` is the review commit, never the PR head

**This is the field that makes the corpus usable, and the one it is easiest to
get wrong.** It is each comment's `original_commit_id`: the tree the reviewer was
looking at when it spoke.

Every PR here is merged, so it is tempting to reconstruct a diff with
`git diff <base>...<head>`. That is wrong, and quietly so. The merged head
contains the *fix commits* — a reviewer scored against it is being asked to find
defects that are no longer there, and will appear to have missed all of them.

### Reconstructing the diff, correctly

This paragraph previously recommended
`git diff $(git merge-base <base> <reviewed_sha>) <reviewed_sha>`. **That produces
an empty diff for 13 of the 15 review commits here**, which is the same silent zero
arrived at from the other side: these PRs were merged with merge commits, so each
`reviewed_sha` is an *ancestor* of `main`, which makes `merge-base main
<reviewed_sha>` the review commit itself.

The base you want is where the PR branch forked. Find the merge commit `M` that
brought the branch in — `reviewed_sha` is an ancestor of `M^2` but not of `M^1`,
and it is the earliest such merge on `git rev-list --merges --ancestry-path
<reviewed_sha>..main` — then diff from `git merge-base M^1 <reviewed_sha>`. Where
there is no such merge (a branch rebased or squashed away, as PR #293's two rows
were) the commit is no longer an ancestor and `merge-base main <reviewed_sha>` is
right after all.

Do not trust this paragraph either: `every_row_reconstructs_a_non_empty_reviewed_diff`
in [`../../review_corpus.rs`](../../review_corpus.rs) is the recipe's executable
form, and it requires every row's reconstructed diff to be non-empty *and* to touch
the file the comment is anchored to.

`fix_commit` is a different sha for the same reason: it points *forward*, to the
change that resolved the finding, and is what a reader follows to see the defect
and its repair.

## The adjudication rule

Recorded once, in [`docs/REVIEW_CHECKLIST.md`](../../../../../docs/REVIEW_CHECKLIST.md),
so that the rule and the corpus cannot drift apart. In short: a real defect has a
commit fixing it; a false positive has a maintainer reply refuting it; and for a
claim that the code *will not compile*, a green check at `reviewed_sha` is
decisive — but only one that actually compiled the code in question, which is
narrower than "CI is green" and is spelled out there. Follow that file, not this
paragraph, when adding rows.

Three rows carry an **empty `fix_commit`** — the PR #299 `vacuous-test` findings
(ids `3789173576`, `3789173583`, `3789173587`). All three were accepted and
fixed, with failure-injection evidence in the thread replies, but the fixes
landed inside a branch rework rather than as one attributable commit. The field
is left blank rather than filled with a plausible-looking guess: a corpus whose
provenance is partly invented is worse than one that admits a gap.

## Classes

| Class | n | real | false |
|---|---:|---:|---:|
| `contract-drift` — a doc, comment or ADR contradicts the code it describes | 5 | 5 | 0 |
| `false-compile-claim` — asserts the code will not build | 4 | 0 | 4 |
| `vacuous-test` — a test passes while the behaviour it names is broken | 3 | 3 | 0 |
| `error-text-drift` — an error message does not state the rule it enforces | 2 | 2 | 0 |
| `permissive-constraint` — a constraint permits the state it exists to forbid | 2 | 2 | 0 |
| `silent-truncation` — a read or copy drops a remainder without erroring | 2 | 2 | 0 |
| `cleanup-gap` — a guard stops a cleanup path doing its job | 1 | 1 | 0 |
| `lint-convention` — a suppression lacks the justification the house style requires | 1 | 1 | 0 |
| `lossy-identity` — a key derived from a lossy conversion, so distinct inputs collide | 1 | 1 | 0 |
| `missing-event` — an early return skips a documented side effect | 1 | 1 | 0 |
| `ordering-bug` — an aggregate computed after the mutation it must precede | 1 | 1 | 0 |
| `perf-contract` — the implementation defeats a field's stated design goal | 1 | 1 | 0 |
| `prose-clarity` — wording only | 1 | 1 | 0 |
| `ux-diagnostic` — a message tells the user to do the wrong thing | 1 | 1 | 0 |

## The result that gives the corpus its point

**Every false positive is a compile-failure claim, and every compile-failure
claim is a false positive.** Read that off the table above: the
`false-compile-claim` row is the only one with a non-zero `false` column, and its
`real` column is zero. Every other row is real and was accepted and fixed.

That is not a curiosity; it is a suppression rule with a measured cost of zero.
CI already computes the refutation: the `msrv` job is
`cargo check --workspace --all-features` and finishes in well under a minute. On
every one of those rows it had gone **green before the claim was posted** — by 65
seconds on `2b761ce`, and by 83 seconds on `c1481836`. So withholding a
compile-failure claim that a *covering* check has refuted costs no extra compute
and, on this evidence, discards nothing true. Each investigation those comments
triggered was avoidable by reading a status that already existed.

"Covering" is load-bearing and not a hedge: `msrv` is ubuntu-only,
`--all-features`, and has no `--all-targets`, so a green run says nothing about
macOS code, a `--no-default-features` build, or test code on the MSRV toolchain.
`rto_graph::compile_claim` is the rule with that model attached.

The corollary is the more useful half: the *remaining* classes are where an
automated reviewer earned its keep here. `contract-drift` is the largest of
them, and it is the class a diff-only reviewer is least equipped for, since
the doc making the claim and the code breaking it need not be adjacent.

## Keeping it honest

[`../../review_corpus.rs`](../../review_corpus.rs) validates the file on every
`cargo test` run: every line parses, the field set is exactly the eleven above,
`verdict` and `defect_class` are drawn from the documented vocabularies,
`reviewed_sha` is a 40-hex sha, `pr` and `line` are positive, and no `id`
repeats. It reads only this file — **no network, no GitHub API, no model** — so
it cannot flake on rate limits and cannot start failing because something changed
upstream.

It also asserts **the class table above against the data**, so a row added
without updating the table fails the build. That check exists because this file's
own review caught the table's totals disagreeing with the corpus — a
`contract-drift` defect in the change that ships a catalogue of `contract-drift`.
Only the table is parsed, never prose: counts therefore live in exactly one place
here, and `docs/REVIEW_CHECKLIST.md` links to this file rather than restating
them.

One check is gated rather than skipped silently: `reviewed_shas_resolve_in_this_repository`
confirms each `reviewed_sha` is a real object here, which catches a typo'd or
truncated sha. It needs the git history, so in a shallow clone it prints a `SKIP:`
line and passes, following the pattern the model-dependent tests in
`../audio_ingest.rs` use.

## Consumers (Stage 35)

The corpus is no longer test-only data. It is embedded into `rto-graph`
(`review_corpus::BUILTIN`) and read by three shipped modules:

| Module | What it does with the corpus |
|---|---|
| `rto_graph::review_corpus` | Loads it as typed rows. `deny_unknown_fields` with no optional fields, so the eleven-field schema is enforced by the type rather than by a second transcript of it in a test |
| `rto_graph::review_score` | Scores a candidate reviewer: **per-class recall**, plus the two precision-adjacent numbers the corpus can and cannot support |
| `rto_graph::compile_claim` | The suppression rule the `false-compile-claim` row licenses, with the coverage model that stops it becoming a "green build" check |

`roteiro review --score <run.json>` is the CLI surface. It takes a
`roteiro.review-run/v1` document — the commits a candidate was run against and what
it said about each — and needs no model, no graph and no network, so a score can be
recomputed anywhere.

### What may and may not be computed from this file

**Recall is well defined**: there are 22 real defects and a candidate either found a
given one or did not.

**Precision is not.** This file is a complete record of *what one reviewer said*
about these trees, never a complete inventory of the defects in them. A candidate
finding matching no row is therefore **unadjudicated**, not false — it may be a real
defect nobody commented on. Scoring one as a false positive would penalise a better
reviewer for being better. `review_score` reports three separate numbers for this
reason: per-class recall, how many of the 4 known-false claims a candidate repeated
(the only measured precision signal here), and how many findings the corpus cannot
judge.

And read the per-class table with its denominators in view. Eight classes hold a
single real row, so their recall is one bit rather than a rate. This corpus can
falsify a reviewer decisively; it cannot finely rank two good ones.

### Measured cost of using it

Reconstructing all 15 review diffs at `-U3` costs about **513k tokens in total**,
averaging **34k per commit** and reaching **103k** on PR #339. Against a measured
single-call budget of ~30k on this repository, **9 of the 15 diffs do not fit in one
call before any context is added**, which is why a whole-diff reviewer is not the
shape to build; the ~79k per-file budget is. (Figures are `len(diff) / 4`, so they
are an estimate of the same order, not a tokeniser's count.)

## Extending it

Add a row when a reviewer comment has been adjudicated under the rule above —
not when it is merely posted. Keep `id` unique, keep `reviewed_sha` the review
commit, and if the verdict is genuinely undecidable, leave the comment out rather
than guess: an unreliable row costs more than a missing one, because every future
score silently inherits it.

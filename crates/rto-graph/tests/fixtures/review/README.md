# Review corpus

`review-corpus.jsonl` — every inline review comment GitHub Copilot left on this
repository over a single day, each one adjudicated against what the maintainer
actually did about it. 26 rows: **22 real defects, 4 false positives.**

This exists so that "is an automated reviewer any good on *this* codebase?" is a
question with an answer rather than an impression. A review tool is otherwise
unmeasurable: its output is prose, its mistakes are plausible, and nobody
remembers last week's false positives well enough to count them. With a fixed
corpus of known verdicts, any candidate reviewer — a different model, a changed
prompt, a graph-grounded arm, a future `roteiro review` mode — is scored against
the same 26 comments and the numbers are comparable across attempts.

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
| `defect_class` | One of the fourteen classes below |
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
Reconstruct with `git diff $(git merge-base <base> <reviewed_sha>) <reviewed_sha>`
instead.

`fix_commit` is a different sha for the same reason: it points *forward*, to the
change that resolved the finding, and is what a reader follows to see the defect
and its repair.

## The adjudication rule

Recorded once, in [`docs/REVIEW_CHECKLIST.md`](../../../../../docs/REVIEW_CHECKLIST.md),
so that the rule and the corpus cannot drift apart. In short: a real defect has a
commit fixing it; a false positive has a maintainer reply refuting it; and for a
claim that the code *will not compile*, the `msrv` job's conclusion at
`reviewed_sha` is decisive. Follow that file, not this paragraph, when adding
rows.

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
claim is a false positive — 4 of 4.** The other 22 comments span thirteen classes
and every one was accepted and fixed.

That is not a curiosity; it is a suppression rule with a measured cost of zero.
CI already computes the refutation: the `msrv` job is
`cargo check --workspace --all-features` and finishes in well under a minute. On
all four rows it had gone **green before the claim was posted** — by 65 seconds
on `2b761ce`, and by 83 seconds on `c1481836`. So withholding a compile-failure
claim while the build is green costs no extra compute and, on this evidence,
discards nothing true. The four investigations those comments triggered were
avoidable by reading a status that already existed.

The corollary is the more useful half: the *remaining* classes are where an
automated reviewer earned its keep here. `contract-drift` alone is five real
defects, and it is the class a diff-only reviewer is least equipped for, since
the doc making the claim and the code breaking it need not be adjacent.

## Keeping it honest

[`../../review_corpus.rs`](../../review_corpus.rs) validates the file on every
`cargo test` run: every line parses, the field set is exactly the eleven above,
`verdict` and `defect_class` are drawn from the documented vocabularies,
`reviewed_sha` is a 40-hex sha, `pr` and `line` are positive, and no `id`
repeats. It reads only this file — **no network, no GitHub API, no model** — so
it cannot flake on rate limits and cannot start failing because something changed
upstream.

One check is gated rather than skipped silently: `reviewed_shas_resolve_in_this_repository`
confirms each `reviewed_sha` is a real object here, which catches a typo'd or
truncated sha. It needs the git history, so in a shallow clone it prints a `SKIP:`
line and passes, following the pattern the model-dependent tests in
`../audio_ingest.rs` use.

## Extending it

Add a row when a reviewer comment has been adjudicated under the rule above —
not when it is merely posted. Keep `id` unique, keep `reviewed_sha` the review
commit, and if the verdict is genuinely undecidable, leave the comment out rather
than guess: an unreliable row costs more than a missing one, because every future
score silently inherits it.

#!/usr/bin/env python3
"""What a pull request is still waiting on: failing checks, open threads, and the
findings that appear in no thread at all.

The last of those is why this exists. Copilot writes some findings **into the
review body** under a `Suppressed comments (N)` heading rather than as inline
comments. Those are invisible to both of the obvious APIs:

    gh api repos/O/R/pulls/N/comments    # inline comments — misses them
    gh pr view N --json reviewThreads    # threads — misses them

A pull request can therefore read `0 unresolved` while carrying a security
finding. On PR #738 of this repository, nine of fifteen findings arrived that
way, and three rounds of review passed before anyone noticed the pattern.

Usage:

    scripts/pr-review-status.py 738
    scripts/pr-review-status.py 738 --repo owner/name
    scripts/pr-review-status.py 738 --quiet     # only what needs attention

Exit codes, so it composes into a watch loop:

    0  clean — checks finished, no open threads, no suppressed findings
    1  something needs attention
    2  nothing wrong yet, but checks are still running

    until scripts/pr-review-status.py 738 --quiet; do sleep 60; done

The loop above needed `2` to exist. With pending checks reported as `0`, it
exited the moment CI started and called a pull request clean before a single
check had reported — the docstring and the behaviour disagreed, and the
docstring was the one that was right.

Requires `gh`, authenticated. Works against any repository, not only this one.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from datetime import datetime, timezone

# The heading Copilot writes above findings it did not post as inline comments.
# Matched loosely on purpose: the "(N)" count and any "**Previously missed (N)**"
# subheading move around between review versions, and a strict pattern that
# silently stopped matching would restore exactly the blind spot this closes.
SUPPRESSED = re.compile(
    r"#{1,6}\s*Suppressed comments\s*\((\d+)\)(.*?)(?=\n\s*-\s*\*\*Files reviewed|\Z)",
    re.S | re.I,
)
# Each finding inside that section starts with a bolded `path:line`.
FINDING = re.compile(r"\*\*([^*\n]+?:\d+)\*\*\s*\n\s*\*\s*(.+?)(?=\n\*\*|\Z)", re.S)


def gh(*args: str) -> str:
    """Run `gh` and return stdout, or exit with its stderr."""
    result = subprocess.run(
        ["gh", *args], capture_output=True, text=True, check=False
    )
    if result.returncode != 0:
        sys.exit(f"gh {' '.join(args)} failed:\n{result.stderr.strip()}")
    return result.stdout


def repo_slug(explicit: str | None) -> str:
    if explicit:
        return explicit
    return json.loads(gh("repo", "view", "--json", "nameWithOwner"))["nameWithOwner"]


def checks(repo: str, pr: int) -> tuple[list[str], list[str]]:
    """Returns (failing, pending) check names."""
    data = json.loads(
        gh("pr", "view", str(pr), "--repo", repo, "--json", "statusCheckRollup")
    )
    failing, pending = [], []
    for check in data.get("statusCheckRollup") or []:
        name = check.get("name") or check.get("context") or "?"
        verdict = check.get("conclusion") or check.get("state") or ""
        if verdict in ("FAILURE", "TIMED_OUT", "CANCELLED", "ACTION_REQUIRED", "ERROR"):
            failing.append(f"{name} ({verdict.lower()})")
        elif verdict in ("", "PENDING", "IN_PROGRESS", "QUEUED"):
            pending.append(name)
    return failing, pending


def open_threads(repo: str, pr: int) -> list[dict]:
    """Every unresolved review thread, following pagination to the end.

    A single page was the first version, and a cap that silently truncates is
    the wrong failure for a tool whose output is meant to be trusted as
    "nothing outstanding". A busy pull request would simply stop reporting the
    oldest threads.
    """
    owner, name = repo.split("/", 1)
    query = """
    query($owner:String!, $name:String!, $pr:Int!, $after:String) {
      repository(owner:$owner, name:$name) {
        pullRequest(number:$pr) {
          reviewThreads(first:100, after:$after) {
            pageInfo { hasNextPage endCursor }
            nodes {
              isResolved path line
              comments(first:1) { nodes { author { login } body url } }
            }
          }
        }
      }
    }"""
    nodes, after = [], None
    while True:
        args = [
            "api", "graphql", "-f", f"query={query}",
            "-F", f"owner={owner}", "-F", f"name={name}", "-F", f"pr={pr}",
        ]
        # `-F after=` sends an empty string, which GraphQL rejects for a cursor;
        # the first page has to omit the variable so it defaults to null.
        if after:
            args += ["-F", f"after={after}"]
        page = json.loads(gh(*args))["data"]["repository"]["pullRequest"][
            "reviewThreads"
        ]
        nodes.extend(page["nodes"])
        if not page["pageInfo"]["hasNextPage"]:
            break
        after = page["pageInfo"]["endCursor"]
    out = []
    for thread in nodes:
        if thread["isResolved"]:
            continue
        first = (thread["comments"]["nodes"] or [{}])[0]
        out.append(
            {
                "where": f"{thread['path']}:{thread.get('line') or 0}",
                "who": (first.get("author") or {}).get("login", "?"),
                "body": (first.get("body") or "").strip(),
                "url": first.get("url", ""),
            }
        )
    return out


def last_answered_at(repo: str, pr: int) -> str:
    """The latest moment at which a finding could have been answered.

    The dividing line between a finding that is certainly still open and one a
    later change may already have addressed. Without it, every suppressed
    finding a pull request ever received counts forever, the exit code can never
    return to zero, and a gate that is always red is a gate nobody reads.

    **Three sources, not one.** The first version used the newest commit alone,
    and reviewers also raise findings about the pull request's *description and
    title* — "this PR adds a script the description doesn't mention". Those
    cannot be answered by a commit, so such a finding stayed live for ever no
    matter how thoroughly it had been dealt with. Editing the body and renaming
    the title now count, which is what actually answers them.

    Deliberately **not** `updatedAt`: that moves on every comment, including the
    review that raised the finding, so it would mark everything answered the
    moment it appeared.

    Also not `pushedDate`, despite it being the more honest name for what this
    wants: GitHub has deprecated it and it comes back `null` here, so preferring
    it would be dead code carrying an implication it cannot keep. `committedDate`
    is used instead, and it does move on a rebase — checked, 19:13:30 to 19:13:32
    across one — so the usual worry about it does not apply.

    The worry that *does* apply is a timestamp in the future, from a skewed clock
    or a hand-set committer date. That would push the line past every finding and
    turn the tool green while work was outstanding, which is the one direction of
    error that matters here: a finding wrongly shown is noise, a finding wrongly
    hidden is the bug this tool exists to prevent. Future moments are therefore
    discarded.
    """
    owner, name = repo.split("/", 1)
    query = """
    query($owner:String!, $name:String!, $pr:Int!) {
      repository(owner:$owner, name:$name) {
        pullRequest(number:$pr) {
          commits(last:1) { nodes { commit { committedDate } } }
          userContentEdits(last:1) { nodes { editedAt } }
          timelineItems(last:1, itemTypes:[RENAMED_TITLE_EVENT]) {
            nodes { ... on RenamedTitleEvent { createdAt } }
          }
        }
      }
    }"""
    data = json.loads(
        gh(
            "api", "graphql", "-f", f"query={query}",
            "-F", f"owner={owner}", "-F", f"name={name}", "-F", f"pr={pr}",
        )
    )["data"]["repository"]["pullRequest"]

    moments = []
    for node in data["commits"]["nodes"] or []:
        moments.append((node.get("commit") or {}).get("committedDate"))
    for node in data["userContentEdits"]["nodes"] or []:
        moments.append(node.get("editedAt"))
    for node in data["timelineItems"]["nodes"] or []:
        moments.append(node.get("createdAt"))

    # Any of these can be absent — a deleted edit, an unexpected timeline node —
    # and `max()` over a list containing `None` raises rather than degrading.
    now = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    usable = [m for m in moments if m and m <= now]
    # ISO-8601 UTC throughout, so lexicographic order is chronological order.
    return max(usable) if usable else ""


def suppressed(repo: str, pr: int) -> list[dict]:
    """Findings written into review bodies, which appear in no thread.

    Every review is read, not just the newest, and the result is deduplicated.
    The tempting shortcut is to trust the latest review as the live set — it
    re-lists still-open findings under "Previously missed", so it usually is one.
    It is not reliably one: on PR #738 two `img-src data:` findings appeared in
    the 13:26 review and were absent from 13:55 without anything having been
    changed. A tool that showed only the newest round would have silently
    dropped them, which is the same failure as the one it exists to fix.

    So nothing is discarded. Repeats collapse into a single entry that records
    how many rounds raised it and when it was last seen.
    """
    # `--paginate` is safe to `json.loads` **here** because this endpoint returns
    # a top-level array, and gh merges those into one document — measured with
    # `per_page=1` across eight reviews, which parsed as a single 8-item array.
    # It does *not* merge object responses; those come back as several
    # concatenated documents. So this pattern is not general, and a future
    # endpoint added here needs the same check rather than the same assumption.
    reviews = json.loads(gh("api", "--paginate", f"repos/{repo}/pulls/{pr}/reviews"))
    since = last_answered_at(repo, pr)
    out = []
    for review in reviews:
        body = review.get("body") or ""
        section = SUPPRESSED.search(body)
        if not section:
            continue
        declared = int(section.group(1))
        if declared == 0:
            # A heading with nothing under it. Not a parse failure — a review
            # that suppressed nothing says so — and reporting it as one would
            # make the tool cry wolf on every clean round.
            continue
        findings = FINDING.findall(section.group(2))
        if not findings:
            # The heading was there but the shape changed. Say so rather than
            # reporting zero: a silent parse failure here is the whole bug.
            out.append(
                {
                    "when": review["submitted_at"],
                    "where": "(unparsed)",
                    "text": f"{section.group(1)} suppressed finding(s) present but "
                    f"could not be parsed — read the review body directly: "
                    f"{review.get('html_url', '')}",
                }
            )
            continue
        for where, text in findings:
            out.append(
                {
                    "when": review["submitted_at"],
                    "where": where.strip(),
                    "text": " ".join(text.split()),
                    # A review submitted after the last commit, body edit or
                    # rename cannot have been answered by any of them. Earlier
                    # ones might have been, so they are shown but do not hold the
                    # exit code red.
                    "live": bool(since) and review["submitted_at"] > since,
                }
            )
    return _dedupe(out)


def _dedupe(findings: list[dict]) -> list[dict]:
    """Collapse the same finding raised across several review rounds.

    Keyed on the location and the opening of the text rather than the whole of
    it, because the wording is rephrased between rounds while the finding is the
    same one — `graph_json` returning `{}` was described three different ways.
    Keying on the full text would report it three times.
    """
    seen: dict[tuple[str, str], dict] = {}
    for finding in findings:
        key = (finding["where"], finding["text"][:60].lower())
        if key in seen:
            seen[key]["rounds"] += 1
            seen[key]["when"] = max(seen[key]["when"], finding["when"])
            seen[key]["live"] = seen[key].get("live") or finding.get("live")
        else:
            seen[key] = {**finding, "rounds": 1}
    return sorted(seen.values(), key=lambda f: f["when"], reverse=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pr", type=int)
    parser.add_argument("--repo", help="owner/name; defaults to the current repo")
    parser.add_argument(
        "--quiet", action="store_true", help="print only what needs attention"
    )
    args = parser.parse_args()
    repo = repo_slug(args.repo)

    failing, pending = checks(repo, args.pr)
    threads = open_threads(repo, args.pr)
    hidden = suppressed(repo, args.pr)

    if failing:
        print(f"failing checks ({len(failing)}):")
        for check in failing:
            print(f"  {check}")
    if pending and not args.quiet:
        print(f"pending checks ({len(pending)}): {', '.join(pending)}")

    if threads:
        print(f"\nunresolved threads ({len(threads)}):")
        for thread in threads:
            print(f"  {thread['where']}  [{thread['who']}]")
            print(f"    {' '.join(thread['body'].split())[:240]}")
            if thread["url"]:
                print(f"    {thread['url']}")

    live = [f for f in hidden if f.get("live")]
    earlier = [f for f in hidden if not f.get("live")]

    def show(group: list[dict]) -> None:
        for finding in group:
            rounds = finding.get("rounds", 1)
            again = f", raised in {rounds} rounds" if rounds > 1 else ""
            print(f"  {finding['where']}  (last seen {finding['when']}{again})")
            print(f"    {finding['text'][:280]}")

    if live:
        print(
            f"\nsuppressed findings ({len(live)}) — raised after the last change "
            "to this PR, and in no thread:"
        )
        show(live)
    if earlier and not args.quiet:
        print(
            f"\nearlier suppressed findings ({len(earlier)}) — raised before the "
            "last commit, body edit or rename, so may already be answered:"
        )
        show(earlier)

    if failing or threads or live:
        return 1

    if pending:
        if not args.quiet:
            print(
                f"{repo}#{args.pr}: nothing outstanding, "
                f"{len(pending)} check(s) still running"
            )
        # Not clean yet: a pending check can still fail, and answering 0 here
        # would let a watch loop exit before anything had reported.
        return 2

    if not args.quiet:
        print(f"{repo}#{args.pr}: clean")
    return 0


if __name__ == "__main__":
    sys.exit(main())

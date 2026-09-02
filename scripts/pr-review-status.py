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

Exits 1 when anything needs attention, so it composes into a watch loop:

    until scripts/pr-review-status.py 738 --quiet; do sleep 60; done

Requires `gh`, authenticated. Works against any repository, not only this one.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys

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
    owner, name = repo.split("/", 1)
    query = """
    query($owner:String!, $name:String!, $pr:Int!) {
      repository(owner:$owner, name:$name) {
        pullRequest(number:$pr) {
          reviewThreads(last:100) {
            nodes {
              isResolved path line
              comments(first:1) { nodes { author { login } body url } }
            }
          }
        }
      }
    }"""
    data = json.loads(
        gh(
            "api",
            "graphql",
            "-f",
            f"query={query}",
            "-F",
            f"owner={owner}",
            "-F",
            f"name={name}",
            "-F",
            f"pr={pr}",
        )
    )
    nodes = data["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"]
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
    reviews = json.loads(gh("api", "--paginate", f"repos/{repo}/pulls/{pr}/reviews"))
    out = []
    for review in reviews:
        body = review.get("body") or ""
        section = SUPPRESSED.search(body)
        if not section:
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

    if hidden:
        print(f"\nsuppressed findings ({len(hidden)}) — in no thread, resolve by hand:")
        for finding in hidden:
            rounds = finding.get("rounds", 1)
            again = f", raised in {rounds} rounds" if rounds > 1 else ""
            print(f"  {finding['where']}  (last seen {finding['when']}{again})")
            print(f"    {finding['text'][:280]}")
        print("  note: these accumulate across rounds and are never withdrawn here,")
        print("        so some may already be fixed — check before re-doing work.")

    if not args.quiet and not (failing or threads or hidden):
        print(f"{repo}#{args.pr}: nothing outstanding"
              + (f"; {len(pending)} check(s) still running" if pending else ""))

    # Pending checks alone are not "needs attention" — they need waiting.
    return 1 if (failing or threads or hidden) else 0


if __name__ == "__main__":
    sys.exit(main())

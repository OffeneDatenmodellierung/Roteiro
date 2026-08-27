#!/usr/bin/env python3
# roteiro:ignore-file
#
# The fixture below asks a model to "List every TODO and FIXME marker" — a
# question about intent debt, not a declaration of any. Without this the debt
# scan counts the question itself, and the repository grows a marker every time
# someone writes a test *about* markers.
"""Measure tool-call accuracy for an advertised surface (issues #578, #590).

#590's acceptance is that prompt tokens fall *with tool-call accuracy
unchanged*. Token count is easy to measure and easy to improve; accuracy is the
half that decides whether a shorter surface ships, and nothing measured it.

Usage:
    scripts/tool-choice-accuracy.py TOOLS_JSON [--url URL] [--model M] [--timeout S]

TOOLS_JSON is an OpenAI `tools` array — dump the live one from the MCP endpoint,
or a variant with edited descriptions. Sending the surface as **client** tools is
deliberate: the server advertises its own graph tools only when the client sends
none, so this measures the surface under test rather than the built-in one, and
it needs no rebuild or restart to compare two renderings.

Prints one line per question and a final score. Exit status is non-zero if any
question chose the wrong tool, so it can gate a change.
"""

import argparse
import json
import sys
import time
import urllib.request

# One question per tool, phrased the way a user would ask rather than by naming
# the tool — a fixture that quotes the tool's own words measures nothing but
# string matching. `expect` is a set where two tools are defensibly correct.
#
# Every "miss" here has so far been the fixture's fault rather than the model's,
# twice over: `sandbox_clear` scored protocol-following behaviour as a failure
# (the server tells callers to show `sandbox_status` before a destructive verb),
# and `path`/`context` were asked about symbols by name when both take node
# **keys**, so resolving the name with `search` first was correct. Before
# treating a wrong answer as a defect in a description, ask whether the answer
# was in fact right.
CASES = [
    ("Which files here have the most intent-debt markers per thousand lines?", {"debt_density"}),
    ("List every TODO and FIXME marker left in the code.", {"debt"}),
    ("What does the ProjectGraph struct do and where is it used?", {"explain", "search"}),
    ("Find nodes mentioning 'heading id'.", {"search"}),
    # Both keys given in full, deliberately. `path` takes node **keys**, so a
    # question naming symbols informally is legitimately answered by `search`
    # first — the model has to resolve a name before it can call this at all.
    # Scoring that as a miss measured the fixture, not the surface: it is what
    # made this set read 13/15 when the surface picks correctly every time.
    (
        "How does sym:rust:crates/rto-graph/src/store.rs#Store::open connect to "
        "sym:rust:crates/rto-graph/src/sync.rs#sync_worktree?",
        {"path"},
    ),
    ("Show me every ADR in the graph.", {"list_kind"}),
    ("Which functions are called by the most other functions?", {"coupling"}),
    ("Is the authored layer in sync with the code right now?", {"check"}),
    ("Which projects does this server host?", {"list_projects"}),
    # Same reasoning: `context` takes `key` and nothing else.
    (
        "Give me the bounded neighbourhood of "
        "sym:rust:crates/rto-graph/src/sync.rs#sync_worktree for an LLM prompt.",
        {"context"},
    ),
    ("What config keys here look like secrets?", {"config_secrets"}),
    ("What security findings have been recorded for this repo?", {"security_list"}),
    ("Has anything been analyzed for vulnerabilities yet, and what is provisioned?", {"security_status"}),
    ("How much disk is the container image cache using?", {"sandbox_status"}),
    # Both are correct here, and that is not a hedge. `sandbox_clear` is the one
    # tool on this surface that destroys anything, and the server's own
    # instructions tell a caller to show `sandbox_status` first and quote the
    # bytes it reports freeing. A model that reaches for status before a
    # destructive verb is following the documented protocol, not missing the
    # tool — an expectation of `sandbox_clear` alone scored correct behaviour as
    # a failure.
    ("Delete the cached boxlite image to free space.", {"sandbox_clear", "sandbox_status"}),
]


def ask(url, model, tools, question, timeout):
    body = json.dumps({
        "model": model,
        "messages": [{"role": "user", "content": question}],
        "tools": tools,
        "max_tokens": 200,
        "temperature": 0,
    }).encode()
    req = urllib.request.Request(
        url, data=body, headers={"Content-Type": "application/json"}, method="POST"
    )
    with urllib.request.urlopen(req, timeout=timeout) as r:
        d = json.load(r)
    choice = (d.get("choices") or [{}])[0]
    calls = (choice.get("message") or {}).get("tool_calls") or []
    return (
        calls[0]["function"]["name"] if calls else None,
        choice.get("finish_reason"),
        (d.get("usage") or {}).get("prompt_tokens"),
    )


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("tools")
    ap.add_argument("--url", default="http://127.0.0.1:8017/v1/chat/completions")
    ap.add_argument("--model", default="qwen3.8-27b")
    ap.add_argument("--timeout", type=int, default=600)
    args = ap.parse_args()

    tools = json.load(open(args.tools))
    names = {t.get("function", t).get("name") for t in tools}
    missing = {n for _, exp in CASES for n in exp} - names
    if missing:
        # A question whose expected tool is not advertised can never pass, and a
        # score computed over such a fixture is meaningless rather than merely low.
        print(f"FIXTURE ERROR: expected tools not in the surface: {sorted(missing)}", file=sys.stderr)
        return 2

    right = 0
    prompt_tokens = None
    started = time.time()
    for question, expect in CASES:
        try:
            got, finish, ptok = ask(args.url, args.model, tools, question, args.timeout)
        except Exception as e:  # noqa: BLE001 - a transport failure is a result too
            print(f"  ERROR   {question[:58]:<58} {e}")
            continue
        prompt_tokens = prompt_tokens or ptok
        ok = got in expect
        right += ok
        print(f"  {'ok  ' if ok else 'WRONG'}  {question[:58]:<58} -> {got} ({finish})")

    elapsed = time.time() - started
    print(f"\n{right}/{len(CASES)} correct   prompt_tokens={prompt_tokens}   {elapsed:.0f}s")
    return 0 if right == len(CASES) else 1


if __name__ == "__main__":
    sys.exit(main())

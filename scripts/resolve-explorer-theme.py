#!/usr/bin/env python3
"""Resolve every var() in the explorer stylesheet under each rendering context.

A review aid for changes to `crates/roteiro/src/assets/index.html`'s palette.
Nobody here has a browser, so "the pixels did not change" has to be shown rather
than asserted. This emits a fully-literal, source-ordered projection of the
stylesheet per context; diffing two revisions' projections answers exactly one
question: does any declaration that can reach a real element resolve to a
different value?

Usage:
    git show HEAD~1:crates/roteiro/src/assets/index.html > /tmp/before.html
    scripts/resolve-explorer-theme.py /tmp/before.html \
        crates/roteiro/src/assets/index.html     # diff two revisions
    scripts/resolve-explorer-theme.py crates/roteiro/src/assets/index.html
                                                 # dump one revision's projection

Exit status is 1 when any context differs, so a difference is a thing you have
to look at and explain, not something to scroll past. This is NOT a CI gate —
`shell_colour_vars_are_one_namespace_declared_by_both_views` in
`crates/roteiro/src/explorer_app.rs` is the standing check. This is for the
review of a palette change, where the question is "what moved", not "is the
shape still right".

WHAT THIS COVERS
  * Every declaration of every rule, with all var() chains expanded to literals.
  * Three contexts, which are exhaustive: the only selectors defining custom
    properties are `:root`, `#view-project` and `#ws-ask-panel` (asserted at
    load), and the three views are mutually exclusive (`showSelectView` /
    `showWorkspaceView` / `showProjectView` in app.js each `hidden` the other
    two), so no element is ever inside both `#view-project` and `#ws-ask-panel`.

WHAT THIS DOES NOT COVER
  * The cascade. It compares declarations, not winners. A rule that is added or
    removed outright shows up as +/- lines and must be argued by hand.
  * Selector specificity/order (unchanged here — no selector is edited).
  * Anything outside the <style> block: inline styles and the cytoscape
    stylesheets in app.js carry their own copies of both palettes.
  * Reachability is a hand audit of the markup and of app.js, encoded below
    rather than derived. It was accurate for issue #512; re-check the two sets
    against the markup before trusting a later run.
"""

import re
import sys

CONTEXTS = {
    # name        -> variable scopes, in cascade order
    "workspace": [":root"],
    "ws-ask": [":root", "#ws-ask-panel"],
    "project": [":root", "#view-project"],
}

# Class tokens of the project-view component family. A selector carrying one of
# these matches nothing on the plain workspace view unless it is listed below.
P_TOKEN = re.compile(r"(?<![\w-])(p-[a-z0-9-]+|pbar)(?![\w-])")

# `.p-*` classes that DO render outside `#view-project`, from a read of the
# markup and of every `class:`/`className` assignment in app.js:
#
#   - `.p-toggle` + `.p-toggle input` — index.html's matrix header carries
#     `class="p-toggle ws-hide-tooling"` on the plain WORKSPACE view, outside
#     `#ws-ask-panel` and so outside the remap's reach entirely.
#   - everything `enableWorkspaceAsk` / `submitWorkspaceAsk` / `showAnswer` /
#     `renderAnswer` / `askModelControl` build into `#ws-ask-panel`.
WORKSPACE_P = {"p-toggle"}
WS_ASK_P = {
    "p-ask",
    "p-ask-form",
    "p-ask-row",
    "p-ask-answer",
    "p-ask-send",
    "p-ask-model",
    "p-ask-model-select",
    "p-ask-refs",
    "p-loading",
    "p-err",
}

# The only NON-`.p-*` selectors that reach an element inside `#view-project`.
# From the markup (`#view-project`'s subtree carries exactly one workspace class,
# `class="ws-badge"` on `#p-linkage`) and from app.js:1899, the only project-side
# writer of that class. `#view-workspace` and `#view-select` are `hidden` there.
# `body`, `body.on-project` and `*` are ANCESTORS of `#view-project`, not
# descendants, so they resolve in the `:root` scope and are compared under the
# `workspace` context instead.
PROJECT_NON_P = {
    "code, .mono",
    "#view-project",
    ".ws-badge",
    ".ws-badge.standalone",
}

REACHABLE = {
    "workspace": lambda sel, toks: toks <= WORKSPACE_P and "#view-project" not in sel,
    "ws-ask": lambda sel, toks: bool(toks) and toks <= WS_ASK_P,
    "project": lambda sel, toks: bool(toks) or sel in PROJECT_NON_P,
}


def style_block(path):
    text = open(path, encoding="utf-8").read()
    return text[text.index("<style>") + len("<style>") : text.index("</style>")]


def parse(css):
    """-> ordered list of (at_prelude, selector, [(prop, value)])."""
    css = re.sub(r"/\*.*?\*/", "", css, flags=re.S)
    rules, stack, buf = [], [], []
    for i, c in enumerate(css):
        if c == "{":
            stack.append(("".join(buf).strip(), i + 1))
            buf = []
        elif c == "}":
            prelude, body_start = stack.pop()
            if not prelude.startswith("@"):  # @media children emit themselves
                at = next((p for p, _ in stack if p.startswith("@")), "")
                body = re.sub(r"\{[^{}]*\}", "", css[body_start:i])
                decls = []
                for d in body.split(";"):
                    d = d.strip()
                    if d and ":" in d:
                        prop, _, val = d.partition(":")
                        decls.append((prop.strip(), val.strip()))
                rules.append((at, " ".join(prelude.split()), decls))
            buf = []
        else:
            buf.append(c)
    if stack:
        raise SystemExit(f"unbalanced braces: {len(stack)} block(s) open")
    return rules


def split_args(s):
    out, depth, cur = [], 0, []
    for ch in s:
        depth += (ch == "(") - (ch == ")")
        if ch == "," and depth == 0:
            out.append("".join(cur).strip())
            cur = []
        else:
            cur.append(ch)
    out.append("".join(cur).strip())
    return out


def resolve(value, scope, depth=0):
    if depth > 32:
        raise SystemExit(f"var() cycle resolving {value!r}")
    i = value.find("var(")
    if i < 0:
        return value
    d = 0
    for j in range(i + 3, len(value)):
        d += (value[j] == "(") - (value[j] == ")")
        if d == 0:
            break
    else:
        raise SystemExit(f"unclosed var() in {value!r}")
    args = split_args(value[i + 4 : j])
    name, fallback = args[0], (", ".join(args[1:]) if len(args) > 1 else None)
    if name in scope:
        sub = resolve(scope[name], scope, depth + 1)
    elif fallback is not None:
        sub = resolve(fallback, scope, depth + 1)
    else:
        # Undefined with no fallback: invalid at computed-value time, so the
        # property falls back to inherited/initial. Not the same as a colour.
        sub = f"<UNRESOLVED {name}>"
    return resolve(value[:i] + sub + value[j + 1 :], scope, depth + 1)


def build_scope(rules, selectors):
    scope = {}
    for sel in selectors:
        for at, prelude, decls in rules:
            if not at and prelude == sel:
                scope.update({p: v for p, v in decls if p.startswith("--")})
    return scope


def project(rules, scope, reachable, ctx):
    kept, skipped = [], 0
    for at, sel, decls in rules:
        # `#ws-ask-panel` lives in `#view-workspace`, which is `hidden` whenever
        # the project view is shown, so its rules reach nothing in that context.
        if ctx == "project" and "#ws-ask-panel" in sel:
            skipped += 1
            continue
        toks = set(P_TOKEN.findall(sel))
        if not reachable(sel, toks):
            skipped += 1
            continue
        for prop, val in decls:
            if prop.startswith("--"):
                continue  # machinery, not pixels
            prefix = f"{at} | " if at else ""
            kept.append(f"{prefix}{sel} {{ {prop}: {resolve(val, scope)} }}")
    return kept, skipped


def main():
    if len(sys.argv) not in (2, 3):
        raise SystemExit("usage: resolve_theme.py BEFORE.html [AFTER.html]")
    outs = {}
    for path in sys.argv[1:]:
        rules = parse(style_block(path))
        defs = sorted({s for _, s, d in rules if any(p.startswith("--") for p, _ in d)})
        if not set(defs) <= {":root", "#view-project", "#ws-ask-panel"}:
            raise SystemExit(f"{path}: unexpected variable-defining selector(s): {defs}")
        outs[path] = {}
        for name, sels in CONTEXTS.items():
            kept, skipped = project(rules, build_scope(rules, sels), REACHABLE[name], name)
            outs[path][name] = kept
            print(
                f"{path:24} {name:10} {len(kept):4} declarations compared, "
                f"{skipped} unreachable rule(s) skipped",
                file=sys.stderr,
            )
        print(f"{path:24} defines vars on {defs}", file=sys.stderr)

    if len(sys.argv) == 2:
        for name, lines in outs[sys.argv[1]].items():
            print(f"\n===== {name} =====")
            print("\n".join(lines))
        return 0

    before, after = sys.argv[1], sys.argv[2]
    differing = 0
    for name in CONTEXTS:
        b, a = set(outs[before][name]), set(outs[after][name])
        print(f"\n===== context: {name} =====")
        if b == a:
            print(f"IDENTICAL — {len(outs[before][name])} resolved declarations match")
            continue
        differing += 1
        for line in sorted(b - a):
            print(f"  - {line}")
        for line in sorted(a - b):
            print(f"  + {line}")
    print()
    print("RESULT:", "no differences" if not differing else f"{differing} context(s) differ")
    return 1 if differing else 0


if __name__ == "__main__":
    sys.exit(main())

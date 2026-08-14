"""Roteiro deny-unless-opted-in policy gate for Omnigent.

A custom Omnigent policy that flips the default from *allow* to *deny*: every
tool call is **blocked** unless the tool name has been explicitly opted in, and
a configured set of "write" tools returns **ASK** (park for human approval)
instead of running. It is the strict, opt-in half of the Roteiro preset — the
per-MCP-server ``tools:`` allowlists narrow *which* tools exist; this gate
decides *whether* the agent may call one at all.

Grounded in the installed Omnigent policy contract
(``omnigent.policies.schema``, omnigent 0.10.0.dev0):

* A policy callable receives the event as a **dict** and returns a response
  **dict** or ``None`` to abstain. Verbatim from ``schema.py``::

      def my_policy(event: PolicyEvent) -> PolicyResponse | None:
          if event["type"] != "tool_call":
              return None  # abstain
          tool = event["data"].get("tool", "")
          ...
          return {"result": "ALLOW"}

* ``PolicyEvent`` (a ``TypedDict``) for a tool call:
  ``type == "tool_call"``, ``target`` is the tool name, and
  ``data`` is ``{"name": "<tool-name>", "arguments": {...}}``.
* ``PolicyResponse`` result is one of ``"ALLOW"``, ``"DENY"``, ``"ASK"``
  (case-insensitive); ``None`` is treated as abstain (== ALLOW).
* The factory form is registered via a module-level ``POLICY_REGISTRY`` list of
  dicts (``handler`` / ``kind: "factory"`` / ``name`` / ``description`` /
  ``params_schema``), discovered by ``omnigent.policies.registry.load_registry``
  from the server's ``policy_modules`` config; the wired handler +
  ``factory_params`` shape is consumed by ``spec/omnigent.py``
  (``handler:`` → callable, ``factory_params:`` → factory kwargs).

The module deliberately depends on **no** Omnigent imports so it can be unit
tested in isolation (the event/response contract is plain dicts).
"""

from __future__ import annotations

from typing import Any

# Result strings, verbatim from omnigent.policies.schema.PolicyResponse.
ALLOW = "ALLOW"
DENY = "DENY"
ASK = "ASK"

# Separators an Omnigent/MCP runtime may use to namespace a tool under its
# server (e.g. ``github.create_issue``, ``github__create_issue``,
# ``github_create_issue``). We match a configured bare tool name against either
# the exact call name or any of these prefixed forms so the gate is robust to
# whichever naming the harness surfaces.
_PREFIX_SEPARATORS = ("__", ".", "-", "_", "/", ":")


def _tool_name(event: dict[str, Any]) -> str:
    """Return the tool name from a ``tool_call`` event, or ``""``.

    Prefers ``event["target"]`` (the schema's documented tool-name slot) and
    falls back to ``event["data"]["name"]``.
    """
    target = event.get("target")
    if isinstance(target, str) and target:
        return target
    data = event.get("data")
    if isinstance(data, dict):
        name = data.get("name")
        if isinstance(name, str):
            return name
    return ""


def _matches(call_name: str, configured: set[str]) -> bool:
    """True if *call_name* is (or is a server-prefixed form of) a configured name.

    Matches an exact name, or a ``<prefix><sep><configured>`` form for any of
    the known separators — so ``create_issue`` in the set also matches a call
    surfaced as ``github.create_issue`` / ``github__create_issue`` / etc.
    """
    if call_name in configured:
        return True
    for sep in _PREFIX_SEPARATORS:
        idx = call_name.rfind(sep)
        if idx != -1 and call_name[idx + len(sep):] in configured:
            return True
    return False


def deny_unless_opted_in(
    *,
    allow: list[str] | None = None,
    ask: list[str] | None = None,
    deny_reason: str = (
        "Denied by the Roteiro deny-unless-opted-in gate: this tool is not in "
        "the opted-in allow set. Add it to the gate's `allow` list (or opt in "
        "for the session) to enable it."
    ),
    ask_reason: str = (
        "This is a write/mutating tool and requires explicit human approval "
        "before it runs."
    ),
) -> Any:
    """Factory: build a deny-by-default tool gate.

    :param allow: Tool names opted in — these ``ALLOW``. Bare names match both
        the exact call name and server-prefixed forms (e.g. ``search`` also
        matches ``roteiro.search``).
    :param ask: Tool names that require approval — these return ``ASK`` even if
        also present in *allow*. Use for mutating/write tools (e.g. GitHub
        writes).
    :param deny_reason: Message returned on a ``DENY``.
    :param ask_reason: Message returned on an ``ASK``.
    :returns: A one-argument policy callable ``evaluate(event) -> dict | None``.
    """
    allow_set = set(allow or [])
    ask_set = set(ask or [])

    def evaluate(event: dict[str, Any]) -> dict[str, Any] | None:
        # Abstain on every phase except the tool-call gate, so other policies
        # (and other event types) are unaffected. Verbatim guard shape from the
        # schema docstring.
        if event.get("type") != "tool_call":
            return None

        name = _tool_name(event)
        if not name:
            # Unknown/unnamed tool call — fail closed under deny-by-default.
            return {"result": DENY, "reason": deny_reason}

        # ASK takes precedence over ALLOW: an opted-in write tool still needs
        # approval. This is what makes GitHub writes gate rather than run.
        if _matches(name, ask_set):
            return {"result": ASK, "reason": ask_reason}
        if _matches(name, allow_set):
            return {"result": ALLOW}
        return {"result": DENY, "reason": f"{deny_reason} (tool: {name!r})"}

    return evaluate


# ── Registry export ──────────────────────────────────────────────────────────
# Discovered by omnigent.policies.registry.load_registry() when this module is
# listed in the server's `policy_modules`. The `handler` dotted path is what the
# agent config's `policies:` block references; `kind: "factory"` means Omnigent
# calls it with the wired `factory_params` to produce the evaluator.
POLICY_REGISTRY = [
    {
        "handler": "roteiro_gate.deny_unless_opted_in",
        "kind": "factory",
        "name": "Roteiro Deny Unless Opted In",
        "description": (
            "Deny-by-default tool gate: blocks every tool unless its name is in "
            "the opted-in `allow` set; returns ASK for names in the `ask` set "
            "(write/mutating tools) so they require human approval."
        ),
        "params_schema": {
            "type": "object",
            "properties": {
                "allow": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Tool names opted in (ALLOW).",
                },
                "ask": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Tool names that require approval (ASK).",
                },
                "deny_reason": {
                    "type": "string",
                    "description": "Message returned on a DENY.",
                },
                "ask_reason": {
                    "type": "string",
                    "description": "Message returned on an ASK.",
                },
            },
        },
    },
]

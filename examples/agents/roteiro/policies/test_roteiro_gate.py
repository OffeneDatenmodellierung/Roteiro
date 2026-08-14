"""Unit tests for the Roteiro deny-unless-opted-in policy gate.

Runs with plain ``pytest`` and no Omnigent import — the policy contract is
plain dicts (see ``roteiro_gate`` module docstring), so we construct events by
hand exactly as ``omnigent.policies.schema.PolicyEvent`` documents them.

    cd examples/agents/roteiro/policies && python -m pytest -q
"""

from __future__ import annotations

import os
import sys

# Make ``roteiro_gate`` importable when pytest is invoked from the repo root as
# well as from this directory.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import roteiro_gate as gate  # noqa: E402


def _tool_call(name: str, **arguments: object) -> dict[str, object]:
    """Build a ``tool_call`` PolicyEvent as omnigent.policies.schema documents:
    ``type == "tool_call"``, ``target`` is the tool name, ``data`` is
    ``{"name": ..., "arguments": {...}}``.
    """
    return {
        "type": "tool_call",
        "target": name,
        "data": {"name": name, "arguments": arguments},
    }


# The allow/ask sets mirror the Roteiro preset's config.yaml factory_params:
# roteiro + GitHub reads are opted in; GitHub writes ASK.
ALLOW = ["search", "explain", "path", "debt", "get_file_contents", "list_commits"]
ASK = ["create_or_update_file", "create_pull_request", "merge_pull_request"]


def _gate():
    return gate.deny_unless_opted_in(allow=ALLOW, ask=ASK)


def test_un_opted_in_tool_is_denied():
    decision = _gate()(_tool_call("browser_navigate", url="https://example.com"))
    assert decision is not None
    assert decision["result"] == "DENY"
    assert "browser_navigate" in decision["reason"]


def test_opted_in_tool_is_allowed():
    decision = _gate()(_tool_call("search", query="Store"))
    assert decision == {"result": "ALLOW"}


def test_github_write_tool_returns_ask():
    decision = _gate()(_tool_call("create_or_update_file", path="README.md"))
    assert decision is not None
    assert decision["result"] == "ASK"


def test_ask_precedes_allow_even_if_opted_in():
    # A write tool listed in BOTH allow and ask must still ASK, never ALLOW.
    g = gate.deny_unless_opted_in(allow=["create_pull_request"], ask=["create_pull_request"])
    assert g(_tool_call("create_pull_request"))["result"] == "ASK"


def test_server_prefixed_names_match():
    # Bare configured names match harness-namespaced call names.
    g = _gate()
    assert g(_tool_call("roteiro.search"))["result"] == "ALLOW"
    assert g(_tool_call("github__create_pull_request"))["result"] == "ASK"
    assert g(_tool_call("playwright.browser_click"))["result"] == "DENY"


def test_non_tool_events_abstain():
    g = _gate()
    assert g({"type": "llm_request", "data": {"model": "qwen3-8b"}}) is None
    assert g({"type": "response", "data": "hello"}) is None


def test_unnamed_tool_call_fails_closed():
    decision = _gate()({"type": "tool_call", "target": None, "data": {}})
    assert decision["result"] == "DENY"


def test_registry_export_is_well_formed():
    # Shape required by omnigent.policies.registry.load_registry().
    assert isinstance(gate.POLICY_REGISTRY, list) and gate.POLICY_REGISTRY
    entry = gate.POLICY_REGISTRY[0]
    assert entry["handler"] == "roteiro_gate.deny_unless_opted_in"
    assert entry["kind"] == "factory"
    assert entry["params_schema"]["type"] == "object"
    assert {"allow", "ask"} <= set(entry["params_schema"]["properties"])

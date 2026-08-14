# Roteiro — Omnigent custom-agent preset

A self-contained [Omnigent](https://omnigent.ai) custom agent that answers
"what / why" questions about a codebase from Roteiro's provenance-tagged
knowledge graph, with **opt-in** GitHub reads and browser automation. It ships
with a strict **deny-unless-opted-in** policy so nothing runs unless you enable
it, and GitHub writes require human approval.

```
examples/agents/roteiro/
├── config.yaml                 # the agent spec (executor, MCP tools, policies)
├── policies/
│   ├── roteiro_gate.py         # custom deny-by-default policy + POLICY_REGISTRY
│   └── test_roteiro_gate.py    # unit test (deny / allow / ask)
└── README.md
```

Grounded against the live Omnigent docs *and* verified empirically against the
installed **omnigent 0.10.0.dev0**. Every claim below is marked **[confirmed]**
(verified against the installed package / Roteiro source) or
**[needs-confirmation]** (could not be verified in this environment).

---

## What it does

* **Roteiro graph** (default-on reads): `search`, `explain`, `path`, `debt`.
* **GitHub** (default-on reads; writes require approval): via the GitHub MCP
  server. Reads ALLOW; writes ASK; destructive/force-push/tag-push DENY.
* **Browser** (Playwright MCP): declared but **denied until you opt in** — the
  demonstrable opt-in path.

Two enforcement layers combine:

1. **Per-MCP-server `tools:` allowlists** narrow *which* tools each server
   exposes (omit the key and a server exposes *all* its tools — avoided here).
2. **A custom `deny_unless_opted_in` policy** blocks *every* tool unless its
   name is in the opted-in `allow` set; names in the `ask` set (GitHub writes)
   return **ASK**. This is the off-by-default gate.

---

## Prerequisites

| Requirement | Notes |
|---|---|
| **`omni` CLI** | Omnigent installed and on `PATH`. **[confirmed]** present as `omnigent 0.10.0.dev0`. |
| **`roteiro serve --models`** running with `qwen3-8b` pulled | Serves the OpenAI-compatible `/v1` endpoint at `http://127.0.0.1:8017` (default bind `127.0.0.1:8017`). This is the model backend for the agent. **[confirmed]** the default address and `/v1` endpoint from `crates/roteiro/src/main.rs`. The exposed model name must match what your `roteiro serve --models` advertises; the preset assumes it is `qwen3-8b`. |
| **`roteiro` on `PATH`** (for the graph MCP server) | The agent launches `roteiro serve` (stdio) from the repo you want served. |
| **Node / `npx`** | For the GitHub and Playwright MCP servers. |
| **A GitHub token** | `GITHUB_PERSONAL_ACCESS_TOKEN` (see below). |

### Start the model backend

```sh
# In the repo you want the graph to answer about (or configure a workspace):
roteiro serve --models --addr 127.0.0.1:8017
```

This exposes `http://127.0.0.1:8017/v1` (OpenAI-compatible). Keep it running.

---

## One-time Gateway credential (`omni setup`) — **supported path**

The agent uses the `openai-agents` harness pointed at the local `/v1` endpoint
through an Omnigent **Gateway** credential. Configure it once:

```sh
omni setup            # standard model/credential picker (per-harness providers)
```

When configuring the provider for the `openai-agents` / OpenAI-compatible
harness, enter:

* **Base URL:** `http://127.0.0.1:8017/v1`
* **Key:** any non-empty value (the local `roteiro serve` ignores it).

> **[confirmed]** `omni setup` is the standard model/credential flow (per
> `omni setup --help`). The docs state it "asks for a base URL and a key" for
> OpenAI-compatible gateways.
>
> **[needs-confirmation] Inline `auth:` block.** The docs only give a verbatim
> inline-`auth:` example for **Databricks**
> (`executor.auth: {type: databricks, profile: ...}`). No verbatim inline
> `auth:` shape for a generic OpenAI-compatible gateway (base_url + key) is
> documented, and it could not be verified here. **Use the `omni setup` path
> above** — it is the supported one. Do not hand-write an `auth:` gateway block
> unless you have confirmed its shape for your Omnigent version.

Why the model is pinned: the `openai-agents` harness treats an *unpinned* model
as a Databricks model and can silently fall back to ambient Databricks
credentials. Pinning `model: qwen3-8b` avoids that. **[confirmed]** from the
shipped `debby/agents/gpt` example comment in the installed package.

---

## GitHub token

`@modelcontextprotocol/server-github` reads **`GITHUB_PERSONAL_ACCESS_TOKEN`**.
Omnigent expands `${VAR}` in the MCP `env:` block at parse time and **errors if
the referenced variable is unset** (no `${VAR:-default}` support).
**[confirmed]** (`omnigent.spec.parser.expand_env_vars`).

So export exactly the one the config references. If you keep your token in
`GITHUB_TOKEN`, mirror it:

```sh
export GITHUB_PERSONAL_ACCESS_TOKEN="$GITHUB_TOKEN"
```

The read-only allowlist means the token is only ever used for read operations
unless you opt a write in (below).

---

## Register the custom policy module

The custom gate lives in `policies/roteiro_gate.py` as the importable module
`roteiro_gate`, exporting the factory `deny_unless_opted_in` and a
`POLICY_REGISTRY`.

### Local `omni run` (trusted) — put it on `PYTHONPATH`

Local runs resolve a policy `path:` directly by import and do **not** enforce
the registry allowlist, so the module just needs to be importable
**[confirmed]** (`omnigent.spec.load(..., enforce_handler_allowlist=False)` for
local runs):

```sh
export PYTHONPATH="$PWD/examples/agents/roteiro/policies:$PYTHONPATH"
```

### Server / multi-user (untrusted) — `policy_modules`

When running against a standalone/multi-user Omnigent **server**, custom handler
paths are only allowed if the module is registered via the **server config's**
`policy_modules` key, which feeds `load_registry(extra_modules=...)`.
**[confirmed]** (`omnigent/cli.py` → `cfg.get("policy_modules")`,
`omnigent/policies/registry.py`). Add the module to your server config and put
`policies/` on the server's `PYTHONPATH`:

```yaml
# omnigent server config
policy_modules:
  - roteiro_gate
```

`POLICY_REGISTRY` (in `roteiro_gate.py`) is what makes the handler browsable and
attachable on the server. **[needs-confirmation]** the exact server-config file
path/name for your deployment — the *key* (`policy_modules`) is confirmed; where
you place that key depends on how you launch the server.

---

## Launch

```sh
# from the Roteiro repo root, with the two exports above set:
omni run ./examples/agents/roteiro/
```

**[confirmed]** the spec loads and validates through Omnigent's real loader
(`omnigent.spec.load`): executor (`type: omnigent`, `harness: openai-agents`,
`model: qwen3-8b`), all three stdio MCP servers with their `tools:` allowlists,
and both policies attach, `validate()` is clean, and each policy resolves to a
live `FunctionPolicy` (including the custom handler off `PYTHONPATH`).

**[needs-confirmation]** a *full* interactive `omni run` session end-to-end — it
requires the live `roteiro serve --models` backend + a configured Gateway
credential + a pulled `qwen3-8b`, which were not all available in the
verification environment.

---

## Opting in to more capabilities

Everything is off by default. To enable a tool, add its name to the gate's
`allow` list (or, for writes, rely on the `ask` list which is already wired for
GitHub writes).

**Enable browser automation** — add the Playwright tools to
`guardrails.policies` → `deny_unless_opted_in` → `function.arguments.allow` in
`config.yaml`:

```yaml
          allow:
            - search
            # …existing…
            - browser_navigate      # ← opt in
            - browser_snapshot
            - browser_click
```

**Enable a GitHub write** — GitHub writes already return **ASK** (approve at the
prompt when the agent requests one). To let the write tool even be *surfaced* by
the server, also add it to the `github` server's `tools:` allowlist (writes are
intentionally omitted there by default):

```yaml
  github:
    type: mcp
    command: npx
    args: [-y, "@modelcontextprotocol/server-github"]
    tools:
      - get_file_contents
      # …reads…
      - create_pull_request   # ← surface the write tool; it will still ASK
```

Destructive deletes, `--force`/`+refspec` pushes, and tag pushes stay **denied**
by the builtin `github_policy` regardless. **[confirmed]** params
(`allow_destructive`, `deny_force_push`, `deny_tag_push`) from
`omnigent.policies.builtins.github.github_policy`.

**Session-level opt-in.** Omnigent also supports attaching/adjusting policies at
the session level (highest priority). Use that to opt a capability in for a
single session without editing `config.yaml`. **[needs-confirmation]** the exact
session-policy CLI/UI gesture for your version; the config-file path above is
the confirmed one.

---

## Run the unit test

Proves an un-opted-in tool is **DENIED**, an opted-in tool is **ALLOWED**, and a
GitHub write tool returns **ASK** (plus prefix-matching and abstain behavior):

```sh
cd examples/agents/roteiro/policies
python -m pytest -q          # 8 passed
```

The test imports only `roteiro_gate` (no Omnigent import) — the policy contract
is plain dicts, matching `omnigent.policies.schema.PolicyEvent` /
`PolicyResponse` exactly.

---

## Config form — an important verified detail

The public docs (`/docs/policies/overview`, `/docs/policies/custom`) show
policies as a **top-level `policies:`** block with `type: function` +
`handler:` + `factory_params:`. **That form is only honored by Omnigent's
single-file "omnigent YAML" adapter.** A **directory bundle with a
`config.yaml`** (what this preset is, and what the task requires) is parsed by
the agent-plane parser, which reads policies from **`guardrails.policies`** with
`function: {path, arguments}` (where `arguments` are the factory kwargs).

**[confirmed] empirically**: placing a top-level `policies:` block in a
directory `config.yaml` and loading it via `omnigent.spec.load()` produced
`guardrails is None` — the block was **silently dropped**. Using
`guardrails.policies` (as this preset does) attaches both policies and they
resolve and enforce. The mapping is: docs `handler:` → `function.path`, docs
`factory_params:` → `function.arguments`.

---

## Confirmed vs needs-confirmation — summary

**Confirmed** (installed omnigent 0.10.0.dev0 / Roteiro source):

* Harness identifier `openai-agents` is valid (`omni run --help`, `/docs/build/harnesses`).
* `executor.type: omnigent` + `config.harness` + `model` — spec loads.
* MCP `type: mcp` with stdio `command`/`args`/`env`/`tools:` allowlist — parsed correctly; `${VAR}` env expansion (errors if unset).
* `guardrails.policies` with `function: {path, arguments}` → factory called with `arguments` kwargs — both policies attach, validate clean, resolve to `FunctionPolicy`, and enforce ALLOW/DENY/ASK.
* Builtin `omnigent.policies.builtins.github.github_policy` handler + params (`read_all`, `allow_destructive`, `deny_force_push`, `deny_tag_push`).
* **Playwright MCP**: `npx @playwright/mcp@latest` (npm `@playwright/mcp`, bin `playwright-mcp`; verified via `npm view`).
* **GitHub MCP**: canonical package is **`@modelcontextprotocol/server-github`**. The task's shorthand `server-github` is an **unrelated npm security-hold placeholder** (`server-github@0.0.1-security`), so the canonical name is used here.
* **Roteiro MCP**: launched via **`roteiro serve`** (stdio is the default transport). There is **no `roteiro mcp` subcommand**; verified against `crates/roteiro/src/main.rs` and `crates/roteiro/tests/mcp_cli.rs`. Graph tools: `search` / `explain` / `path` / `debt`.
* `roteiro serve --models` default bind `127.0.0.1:8017`, OpenAI-compatible `/v1`.
* `policy_modules` is a **server-config** key (not agent config); local `omni run` resolves the handler by import (`PYTHONPATH`).

**Needs-confirmation** (not verifiable in this environment):

* Inline gateway `auth:` block shape for a generic OpenAI-compatible endpoint — **use `omni setup`** (only Databricks `auth:` is documented verbatim).
* A full interactive `omni run` session (needs the live model backend + Gateway credential + pulled `qwen3-8b`).
* The exact tool names the *running* GitHub MCP server advertises — the allowlists use the standard `@modelcontextprotocol/server-github` tool set; adjust if your server differs.
* Whether Omnigent namespaces MCP tool names with the server prefix at call time — the gate tolerates both (exact and `server<sep>tool` suffix matching).
* The exact server-config file location for `policy_modules`, and the session-level opt-in gesture.

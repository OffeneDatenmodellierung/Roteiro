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
| **`roteiro` on `PATH`** (for the graph MCP server) | The agent launches **`roteiro mcp`** (stdio MCP graph server, ADR-0002). With no `--workspace`/`-w` it serves the **current directory's repo**, so run `omni run` from the repo you want answered (or scope it — see [Which repo does the graph answer about?](#which-repo-does-the-graph-answer-about)). Note: this is **`roteiro mcp`**, not `roteiro serve` — `roteiro serve` is the HTTP model endpoint above and does not speak MCP over stdio. |
| **Node / `npx`** | For the GitHub and Playwright MCP servers. |
| **A GitHub token** | `GITHUB_PERSONAL_ACCESS_TOKEN` (see below). |

### Start the model backend

```sh
# In the repo you want the graph to answer about (or configure a workspace):
roteiro serve --models --addr 127.0.0.1:8017
```

This exposes `http://127.0.0.1:8017/v1` (OpenAI-compatible). Keep it running.

---

## Model endpoint — inline in `config.yaml` (no `omni setup` needed)

The agent's model endpoint is wired **inline** in `config.yaml`'s `executor`
block — there is **no `omni setup` step for it**. (`omni setup` lists native
coding-agent harnesses; there is no `openai-agents` entry and no top-level
"Gateway" prompt — the custom-base-URL option only exists nested inside another
harness's "Add a provider" submenu, which this preset does not use.)

```yaml
executor:
  type: omnigent
  config:
    harness: openai-agents
    use_responses: false          # REQUIRED — see below
  model: qwen3-8b                  # plain served id, NO provider prefix
  auth:
    type: api_key
    api_key: sk-local             # any non-empty value; roteiro serve ignores it
    base_url: http://127.0.0.1:8017/v1
```

* **`auth: {type: api_key, api_key, base_url}`** is the supported inline form for
  a custom OpenAI-compatible endpoint. **[confirmed]** the schema parses it
  (`omnigent/spec/parser.py`) and it is documented in the installed package as
  the "custom OpenAI-compatible endpoint" example (`omnigent/spec/types.py`
  `ApiKeyAuth.base_url`); it is honored end-to-end to
  `AsyncOpenAI(base_url=…, api_key=…)` (`runtime/workflow.py` →
  `inner/openai_agents_sdk_executor.py`).
* **`config.use_responses: false` is REQUIRED.** The `openai-agents` harness
  defaults to the OpenAI **Responses API** (`/v1/responses`) for non-`gpt`
  models, but `roteiro serve` only routes `/v1/chat/completions` (+ `/v1/models`,
  `/v1/embeddings`) — with the default, **every turn 404s**. This setting forces
  the Chat Completions endpoint. **[confirmed]** (`crates/rto-serve/src/server.rs`
  routes; `runtime/workflow.py` → `OpenAIProvider(use_responses=False)`).
* **`model: qwen3-8b`** is sent **verbatim** as the OpenAI model id (no provider
  prefix) and must appear in `GET http://127.0.0.1:8017/v1/models`. Avoid a
  `databricks-`/`databricks/` prefix — that string triggers Databricks routing.
  **[confirmed]** the model is passed raw (`inner/openai_agents_sdk_executor.py`).

### Alternative: environment variables (instead of the inline `auth:` block)

If you'd rather not hardcode the URL in `config.yaml`, delete the `auth:` block
and export the endpoint into `omni run`'s environment (keep `model` and
`use_responses: false` in the spec):

```sh
export OPENAI_BASE_URL="http://127.0.0.1:8017/v1"
export OPENAI_API_KEY="sk-local"     # optional; a placeholder is used if unset
```

**[confirmed]** the executor falls back to `OPENAI_BASE_URL`/`OPENAI_API_KEY`
(`inner/openai_agents_sdk_executor.py`).

Why the model is pinned: an *unpinned* model is treated as a Databricks model and
can silently fall back to ambient Databricks credentials. Pinning
`model: qwen3-8b` avoids that. **[confirmed]** from the shipped `debby/agents/gpt`
example comment in the installed package.

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
requires the live `roteiro serve --models` backend + a pulled `qwen3-8b`, which
were not all available in the verification environment.

### Which repo does the graph answer about?

The `roteiro` MCP server runs `roteiro mcp` with no `--workspace`, so it serves
**the repo of the current working directory** (`omni run` spawns the MCP
subprocess in its own cwd). Two ways to point it at the code you want answered:

* **Run from that repo.** `cd` into the target repo and launch the agent by
  absolute path, e.g. `omni run /path/to/roteiro/examples/agents/roteiro/`. The
  `roteiro mcp` subprocess then serves the target repo. (It must have a Roteiro
  graph — run `roteiro sync` there first if needed.)
* **Scope a workspace.** To serve one or many repos regardless of cwd, add
  `--workspace <ROOT>` (repeatable) or `-w <name>` to the `roteiro` server args
  in `config.yaml`, e.g. `args: [mcp, --workspace, /path/to/repo]`. See
  `roteiro mcp --help` (ADR-0008 workspace mode).

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
* **Roteiro MCP**: the stdio MCP graph server is **`roteiro mcp`** (ADR-0002; STDIO by default, `--http ADDR` for networked). `roteiro serve` is the separate HTTP model endpoint and does **not** speak MCP over stdio. Verified against `crates/roteiro/src/main.rs` (`Command::Mcp`) and `crates/roteiro/tests/mcp_cli.rs` (`mcp_answers_initialize_and_tools_call` drives `roteiro mcp` over stdio). Graph tools: `search` / `explain` / `path` / `debt`. With no `--workspace`/`-w`, `roteiro mcp` serves the current directory's repo.
* `roteiro serve --models` default bind `127.0.0.1:8017`, OpenAI-compatible `/v1` — routes `/v1/chat/completions`, `/v1/models`, `/v1/embeddings` only (**no `/v1/responses`**), so `use_responses: false` is required for the openai-agents harness.
* Inline `auth: {type: api_key, api_key, base_url}` parses (`spec/parser.py`) and is honored to `AsyncOpenAI(base_url=…)`; `OPENAI_BASE_URL`/`OPENAI_API_KEY` env fallback also works (`inner/openai_agents_sdk_executor.py`).
* `policy_modules` is a **server-config** key (not agent config); local `omni run` resolves the handler by import (`PYTHONPATH`).

**Needs-confirmation** (not verifiable in this environment):

* A full interactive `omni run` session (needs the live model backend + pulled `qwen3-8b`).
* The exact tool names the *running* GitHub MCP server advertises — the allowlists use the standard `@modelcontextprotocol/server-github` tool set; adjust if your server differs.
* Whether Omnigent namespaces MCP tool names with the server prefix at call time — the gate tolerates both (exact and `server<sep>tool` suffix matching).
* The exact server-config file location for `policy_modules`, and the session-level opt-in gesture.

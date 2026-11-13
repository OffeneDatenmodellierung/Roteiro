---
Title: Provider credentials — a token command from the user's own config, and extra headers from any layer
Space: ARCH
Parent: ADRs

# ADR-specific metadata (unknown keys are ignored; used for indexing/search)
type: adr
adr-id: "0032"
status: Accepted                    # Draft | For Review | Accepted | Rejected | Superseded
architectural-significance: HIGH    # SOFT | LOW | MEDIUM | HIGH | VERY HIGH
domain: Inference
decision-makers: ["The Roteiro Project Team (owner decision, 2026-11-13)"]
superseded-by:
version: "1.0"
last-modified: 2026-11-13
confluence-url:
---

# ADR-0032: Provider credentials — a token command from the user's own config, and extra headers from any layer

|  |  |
|---|---|
| **State** | Accepted |
| **Architectural Significance** | HIGH |
| **Domain** | Inference |
| **Document version** | 1.0 |

## Reference

- The engine every model call goes through: [[docs/adr/0028-one-engine-local-or-hosted.md]].
- Key classes and why a command is not a value: [[docs/adr/0007-configuration-file.md]].
- The configuration: [[crates/roteiro/src/config.rs#ProviderConfig]].
- Where it is applied: [[crates/roteiro/src/provider.rs#ProviderEngine::with_auth]],
  and the one constructor every caller uses, [[crates/roteiro/src/main.rs#provider_engine]].

## Summary

`ROTEIRO_PROVIDER_API_KEY` is a static key. Enterprise setups do not hand out
static keys: they use OAuth access tokens that expire in about an hour, minted by
a login (Anthropic's `ant auth login`), by a cloud identity (`gcloud`, `az`), or by
a corporate credential helper. Roteiro now accepts two `[provider]` keys:

1. **`token_command`** — an argument vector whose standard output is the bearer
   token. Run without a shell, cached for five minutes, and run again (with one
   retry) when the provider answers `401`. **Read only from the user layer**,
   `~/.roteiro/config.toml`.
2. **`headers`** — extra request headers, from any layer, refusing any header
   that carries a credential.

`ROTEIRO_PROVIDER_API_KEY`, when set, still wins, as an API key outranks a
profile in Anthropic's own CLI.

## Context

The owner asked for Claude, and other vendors, to work under enterprise OAuth as
well as API keys, as it does for the Claude CLI. A token Roteiro could be handed
by hand already works — an OAuth access token is a `Bearer` token — but it
expires within the hour, and Anthropic requires `anthropic-beta:
oauth-2025-04-20` beside its OAuth tokens, which Roteiro had no way to send.

Building each vendor's OAuth flow into Roteiro would put a browser login, a
refresh-token store and a per-vendor token exchange inside a code-graph tool. Every
vendor already ships a command that does this and prints a token. Git solved the
same problem the same way with credential helpers.

## Decision makers

The Roteiro Project Team; the owner decided the requirement (2026-11-13).

## Recommended option

### 1. A token command, not built-in OAuth flows

`[provider] token_command = ["ant", "auth", "print-credentials", "--access-token"]`
is run when a token is needed and its trimmed output sent as `Authorization:
Bearer`. The same mechanism covers:

| Provider | `token_command` |
|---|---|
| Anthropic (OAuth via `ant auth login`) | `["ant", "auth", "print-credentials", "--access-token"]` |
| Google Vertex AI (OpenAI-compatible endpoint) | `["gcloud", "auth", "print-access-token"]` |
| Azure / Microsoft Foundry (Entra ID) | `["az", "account", "get-access-token", "--resource", "<resource>", "--query", "accessToken", "-o", "tsv"]` |
| A corporate gateway | its credential helper |

Output must be one bare token: anything containing whitespace is refused, which
catches `ant auth print-credentials` run without `--access-token` (it prints
JSON). A failing command is reported by name, exit status and the tail of its
standard error — never its standard output, which may hold the token.

### 2. User layer only, because a command is code

A command in a committed `roteiro.toml` would run whatever a repository's author
chose the first time anyone asked that repository a question. Under ADR-0007
that is test 2 — it executes code that would not otherwise run — so the key is
not a value. It is stronger than a capability: no project file may grant it at
all. A project file that sets it is warned about on stderr and ignored.

### 3. Headers from any layer, never credentials

`[provider] headers` may be committed, so a team can standardise
`anthropic-beta = "oauth-2025-04-20"` or a gateway's routing header beside its
`base_url`. `authorization`, `proxy-authorization` and `x-api-key` are refused at
startup: credentials live in the environment or come from the user's command,
never in a file.

### 4. Precedence

`ROTEIRO_PROVIDER_API_KEY` set → it is the token. Otherwise `token_command`, if
the user layer sets one. Otherwise no `Authorization` header, as for a local
Ollama.

## Options considered + consequences

| Option | Why not |
|---|---|
| Built-in OAuth (device code, PKCE, Workload Identity Federation exchange) | Per-vendor flows and a refresh-token store inside Roteiro; every vendor's CLI already does it. Workload Identity Federation remains a candidate for CI, where no command runs — see Open questions. |
| A shell string | Quoting bugs and injection through config; an argument vector names the program. `["sh", "-c", "…"]` remains available deliberately. |
| `token_command` from any layer | A cloned repository could run code on first question. |
| Reuse Claude Code's login | Those credentials are issued to Claude Code; `ant auth login` and Workload Identity Federation are the documented routes for other tools. |

## Consequences

- An enterprise user runs `ant auth login` once and sets two lines in
  `~/.roteiro/config.toml`; tokens refresh without Roteiro storing one.
- A token is held in memory for at most five minutes between runs of the
  command, and is never written to disk or printed.
- **No timeout on the command yet**: a helper that hangs hangs the call (a
  `shortcut:` comment marks it).

## Open questions

1. **Does Anthropic's OpenAI-compatible endpoint accept OAuth tokens?** Anthropic
   documents OAuth (`Authorization: Bearer` plus `anthropic-beta:
   oauth-2025-04-20`) on its native Messages API. If the compatible endpoint
   refuses them, a native Messages engine is needed for OAuth against Anthropic
   directly; Vertex, Foundry and gateways are unaffected. To check, after `ant
   auth login`:

   ```sh
   curl -s https://api.anthropic.com/v1/chat/completions \
     -H "Authorization: Bearer $(ant auth print-credentials --access-token)" \
     -H "anthropic-beta: oauth-2025-04-20" -H "content-type: application/json" \
     -d '{"model":"claude-sonnet-5-5","max_tokens":16,"messages":[{"role":"user","content":"hi"}]}'
   ```

2. **Workload Identity Federation for CI** — exchanging an identity token at
   `/v1/oauth/token` with no command at all.

## Status

Accepted. Implemented: both keys, the user-only rule, the credential-header
refusal, and the cached, refresh-on-401 token.

## Version history

| Version | Date | Change |
|---|---|---|
| 1.0 | 2026-11-13 | Initial. `[provider] token_command` (user layer only; argv, no shell; five-minute cache; refresh and one retry on `401`) and `[provider] headers` (any layer; credential headers refused). `ROTEIRO_PROVIDER_API_KEY` keeps precedence. Records the open question of whether Anthropic's OpenAI-compatible endpoint accepts OAuth tokens. |

---
Title: One engine, local or hosted — the provider may be off this machine, and the remote tier is removed
Space: ARCH
Parent: ADRs

# ADR-specific metadata (unknown keys are ignored; used for indexing/search)
type: adr
adr-id: "0028"
status: Accepted                    # Draft | For Review | Accepted | Rejected | Superseded
architectural-significance: VERY HIGH  # SOFT | LOW | MEDIUM | HIGH | VERY HIGH
domain: Inference
decision-makers: ["The Roteiro Project Team"]
superseded-by:
version: "1.0"
last-modified: 2026-11-11
confluence-url:
---

# ADR-0028: One engine, local or hosted — the provider may be off this machine, and the remote tier is removed

|  |  |
|---|---|
| **State** | Accepted |
| **Architectural Significance** | VERY HIGH |
| **Domain** | Inference |
| **Document version** | 1.0 |

## Reference

Supersedes [[docs/adr/0019-remote-model-tier.md]]. Amends §1 of
[[docs/adr/0027-external-model-provider.md]] and the key table of
[[docs/adr/0007-configuration-file.md]]. Implemented in
[[crates/roteiro/src/provider.rs#ProviderEngine]].

## Summary

Roteiro has **one** model engine: the OpenAI-compatible
[[crates/roteiro/src/provider.rs#ProviderEngine]]. `[provider] base_url` may
name this machine (Ollama, `llama-server`) **or a hosted endpoint** (an
enterprise Anthropic or OpenAI subscription, a company gateway). Whatever the
local path builds — the full conversation, graph tool results, the `spec draft`
prompt — is what the provider receives.

The remote model tier of ADR-0019 is removed: the `rto-remote` crate, the
`remote` feature, `roteiro remote status|dry-run|call|log`,
`--allow-remote`/`--no-remote`, the `[remote]` table and the egress ledger.

## Context

ADR-0019 built a second path to a hosted model beside the local one, with its
own consent gate, payload allow-list, dry-run and ledger. Its purpose was to keep
Roteiro usable for sensitive repositories — content never leaves unless a person
opts in twice. The cost was a second engine whose answers are deliberately not
the local ones: a remote Ask dropped tool results and carried at most twelve
reduced nodes, and `review --llm` could not go remote at all.

ADR-0027 then moved every model out of process to a provider named by URL,
refusing non-loopback URLs until they were routed through ADR-0019's gate. The
result was two engines that both speak HTTP to an OpenAI-compatible server and
differ only in policy.

Teams with enterprise model subscriptions — where the provider's contract
already covers the data — want the full-context experience against a hosted
model. The local-by-default posture still matters for sensitive workloads.

## Decision makers

The Roteiro Project Team (owner decision, 2026-11-11).

## Recommended option

### 1. One engine; the URL decides where models run

`[provider] base_url` may name any host. The default stays unset, and the
documented setup is a local server, so a stock install sends nothing anywhere.

### 2. Any config layer may set it, and every run says so

`[provider] base_url` is a **value** under ADR-0007, not a capability: a
committed `roteiro.toml` may point a team at its enterprise endpoint. This is
a deliberate departure from ADR-0007's capability test, whose clause 1 it would
otherwise meet, chosen so a team can standardise on one endpoint.

What bounds it is disclosure, not precedence: whenever the host is not this
machine, constructing the engine prints that host and that repository content is
sent to it, on stderr, on every run.

### 3. What stays refused

- Plaintext `http://` to another machine. The request carries repository content
  and the credential (`ROTEIRO_PROVIDER_API_KEY`, sent as a Bearer token).
- Redirects, and environment proxies, as before: either could move the call,
  credential included, away from the host that was checked and announced.
- The credential as a config key. `roteiro.toml` is committed.

### 4. No egress ledger

Roteiro records nothing about what it sent. An enterprise provider keeps its own
audit log, and a local one sends nothing anywhere.

## Options considered + consequences

| Option | Outcome |
|---|---|
| Wire a non-loopback provider through ADR-0019's gate and widen its allow-list | Rejected: keeps two engines and every consent surface for a policy the owner no longer wants. |
| Allow hosted URLs from the user layer only | Rejected: prevents a team standardising on one enterprise endpoint in `roteiro.toml`. |
| **Any layer, announced on every run** | **Chosen.** One engine, one code path, and no run sends content off the machine silently. |

## Consequences

- **Breaking for `roteiro`:** the `remote` subcommands, `--allow-remote` /
  `--no-remote` on `serve` and `spec draft`, and the `[remote]` table are gone.
  A config that still carries `[remote]` loads and warns that it is inert.
- **A project's `[remote] enabled = false` is no longer enforced.** It was the
  way a repository said "never send this anywhere". There is no replacement in
  this decision; the warning says so.
- A merged `[provider] base_url` in `roteiro.toml` sends every teammate's
  repository content to that host. The stderr notice is the only safeguard.
- The `rto-remote` crate stops publishing. Its released versions stay on
  crates.io; nothing in the workspace depends on it.
- Offline-by-default holds as a claim about the default configuration, not as an
  enforced property of every configuration.

## Status

**Accepted** (2026-11-11), with its implementation.

## Version history

| Version | Date | Change |
|---|---|---|
| 1.0 | 2026-11-11 | Initial. Accepted with the implementation. |

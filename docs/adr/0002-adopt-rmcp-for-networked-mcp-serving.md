---
Title: Adopt the official rmcp SDK for networked MCP serving
Space: ARCH
Parent: ADRs

# ADR-specific metadata (unknown keys are ignored; used for indexing/search)
type: adr
adr-id: "0002"
status: Accepted                    # Draft | For Review | Accepted | Rejected | Superseded
architectural-significance: MEDIUM  # SOFT | LOW | MEDIUM | HIGH | VERY HIGH
domain: Developer Tooling
decision-makers: ["The Roteiro Project Team"]
superseded-by:
version: "1.0"
last-modified: 2026-08-08
confluence-url:
---

# ADR-0002: Adopt the official rmcp SDK for networked MCP serving

| | |
|---|---|
| **State** | Accepted |
| **Architectural Significance** | MEDIUM |
| **Domain** | Developer Tooling |
| **Document version** | 1.0 |

## Reference

Amends the MCP-server decision in [[docs/adr/0001-build-roteiro-unified-codebase-knowledge-graph.md]] (Stage 7). Stage 7 first shipped a dependency-free, hand-rolled JSON-RPC-over-stdio MCP server, chosen to honour ADR-0001's offline-by-default and lean-dependency principles. This ADR records the decision to replace it with the official Rust MCP SDK once **networked serving became a near-term goal**.

## Summary

Adopt **`rmcp`** (the official Rust MCP SDK, v3.x) for `roteiro serve`, behind the existing `mcp` feature. This brings an async (`tokio`) dependency and roughly 77 transitive crates into the feature-gated build, in exchange for protocol correctness maintained upstream, future MCP capabilities (resources, prompts, cancellation, progress), and — the deciding factor — the **streamable-HTTP transport**, enabling a networked, multi-client MCP service. The default build (no `mcp` feature) is unchanged.

## Context

Stage 7's hand-rolled server is correct and lean, but its ceiling is the **stdio** transport: a local subprocess an agent spawns, communicating over pipes. Pipes cannot carry TLS, so "TLS support" is not a property of the stdio server — it only becomes meaningful for a **networked HTTP** transport.

The project now wants networked MCP serving (multiple clients, remote access). Hand-rolling HTTP + SSE + session management + TLS termination would be a poor use of effort and error-prone; that is exactly what the SDK provides. We verified `rmcp` 3.1.2 builds on the 1.94 MSRV and that its stdio/server dependency tree passes the strict `cargo deny` licence allow-list.

## Decision makers

- The Roteiro Project Team

## Recommended option

**Adopt `rmcp` behind the `mcp` feature**, exposing the existing query surface (`explain`, `list_kind`) as MCP tools over **both** the stdio transport (default; for local agents) and the streamable-HTTP transport (for networked serving). TLS for the HTTP transport is terminated at a reverse proxy in the standard way; in-app TLS (rustls) can be added later as a further option.

## Options considered + consequences

### Option 1: Keep the hand-rolled stdio server

- Pros: zero new dependencies; synchronous; maximally lean and offline.
- Cons: stdio only — no networked/multi-client serving, which is now wanted; we own protocol correctness and must track MCP spec revisions by hand; no path to resources/prompts/cancellation.

### Option 2: Adopt rmcp (recommended)

- Pros: official SDK tracks the spec; **streamable-HTTP transport** unlocks networked serving and TLS-at-proxy; future MCP features come for free; less bespoke protocol code to maintain.
- Cons: pulls `tokio` + ~77 transitive crates into the `mcp`-featured build and makes `serve` async; a real deviation from ADR-0001's lean/offline principle. Mitigated by keeping it strictly feature-gated (default build unchanged), verified MSRV-clean (1.94) and licence-clean (`cargo deny`).

## Consequences

- The MCP server originally gained a transport choice on `roteiro serve` (stdio default; `--http <addr>` for networked serving), and the `mcp` feature implied an async runtime. *(Update, 2026-08-14: the MCP graph server moved to its own `roteiro mcp` command — stdio by default, `--http <addr>` for networked MCP — because bare `roteiro serve` is now the network HTTP model/graph server. `roteiro serve --http <addr>` stays a deprecated alias that still starts the MCP server.)*
- The offline-by-default guarantee is preserved for the **default** build and for local stdio use; networked HTTP is an explicit opt-in.
- `cargo deny` / `cargo audit` now cover the rmcp tree whenever the `mcp` feature is built in CI.

## Advice Received

Decision taken by the project team after weighing the dependency cost against the networked-serving requirement; the hand-rolled server was validated as a viable lean alternative but does not meet the networked goal.

## Document version history

| Version | Date | Notes |
|---------|------|-------|
| 1.0 | 2026-08-08 | Accepted. Adopt rmcp for stdio + streamable-HTTP MCP serving, feature-gated; amends ADR-0001 Stage 7. |

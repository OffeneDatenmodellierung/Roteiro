# rto-serve

Local **model serving** for [**Roteiro**](https://roteiro.dev) (ADR-0006): an
opt-in, loopback, **OpenAI-compatible `/v1` endpoint** over your installed GGUF
models, backed by llama.cpp.

Serve `chat/completions` (streaming), `models`, and `embeddings` over the same
models Roteiro uses internally, and optionally auto-register Roteiro's **graph
tools** so the served model can query your codebase during a conversation. Bound
to loopback by default; front a public bind with a reverse proxy. Enabled by
building/installing `roteiro` with the `serve` Cargo feature
(`cargo install roteiro --features serve`), then run `roteiro serve --models`.

## Client-supplied `tools`

A client may send its own OpenAI `tools` array, and two rules govern what happens:

- **Roteiro never executes a client's tool.** It returns the call with
  `finish_reason: "tool_calls"` and stops; the client runs it and sends the result
  back as a `role: "tool"` turn. This is why client tools need no consent gate or
  authorisation story — there is no code path that reaches one.
- **Client tools suppress the graph tools**, so the endpoint has two modes: **no
  `tools` → Ask mode**, graph tools, scoped routes; **`tools` present → general
  mode**, the client's tools only.

**The full contract — every declared divergence from OpenAI, the scoped routes, the
parameters that are accepted and dropped, and why the `tools` array is bounded —
is documented once, at <https://roteiro.dev/serving>** (source:
`docs/SERVING.md`). It lives there rather than here because the people who need it
are pointing a client at the endpoint, not depending on this crate.

## Stability

This crate is **an implementation detail of the `roteiro` CLI**. It is published
only because crates.io requires a published package's dependencies to be registry
packages, so `roteiro` cannot ship unless it does.

Its public API carries **no stability guarantee** — breaking changes ship as minor
version bumps. If you depend on it directly, pin an exact version.

- **Docs:** <https://roteiro.dev>
- **Source & issues:** <https://github.com/OffeneDatenmodellierung/Roteiro>

Dual-licensed under MIT or Apache-2.0.

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

A client may send its own OpenAI `tools` array. Two rules govern what happens:

- **Roteiro never executes a client's tool.** It returns the call with
  `finish_reason: "tool_calls"` and stops; the client runs it and sends the result
  back as a `role: "tool"` turn. This is why client tools need no consent gate or
  authorisation story — there is no code path that reaches one.
- **Client tools suppress the graph tools.** When `tools` is present, Roteiro's own
  graph tools are *not* injected. A client sending its own tools is using this as a
  general backend, and adding ~3,100 tokens of graph schemas it never asked for is
  the surprise this feature exists to remove. So the endpoint has two modes: **no
  `tools` → Ask mode**, graph tools, scoped routes; **`tools` present → general
  mode**, the client's tools only.

If any call in a turn names a client tool, *all* the calls in that turn are
returned and *none* are executed — never "run the graph half, return the client
half", which would hand the client a `tool_calls` response for an assistant turn
it cannot reconstruct.

### Declared divergences from OpenAI

Stated rather than half-implemented, so a gap is documented instead of discovered
on the wire.

| surface | status | why |
|---|---|---|
| `tools` | **supported** | client tools advertised to the model; calls returned, never run |
| `tool_calls` on the response and on a replayed assistant turn | **supported** | rendered to and from the in-band `<tool_call>` protocol |
| `role: "tool"` / `tool_call_id` / `name` | **supported** | mapped to a `<tool_response>` **user** turn — see below |
| `finish_reason: "tool_calls"` | **supported** | ends the loop when a client tool is called |
| `tool_choice` | **accepted, not enforced** | forcing a named function is grammar-constrained sampling; it lands with the grammar work, and half-implementing it would tell a client it was honoured |
| `parallel_tool_calls` | **accepted, not enforced** | at most one call is parsed per turn today, so a turn never carries more than one regardless |
| streamed `tool_calls` | **`arguments` never fragmented** — every call arrives complete in one chunk | OpenAI splits `arguments` across several chunks. Each call still carries its positional `index`, so a client accumulating by index is unaffected; one-shot is legal and works in mainstream clients |
| assistant prose alongside a tool call | **dropped** — `content` is `null` | OpenAI permits `content` *and* `tool_calls` together; Roteiro returns the calls alone rather than risk leaking `<tool_call>` markup into the answer |
| a client's `role: "tool"` content | **not truncated** | `MAX_TOOL_RESULT` caps results of tools *Roteiro* executes. A client's own result is its own context budget, and silently trimming it would corrupt the transcript the client is correlating against |
| a `tools` array over **128 entries or 32 KiB** | **400, never truncated** | a bound on what a caller can make Roteiro allocate — see below. Trimming instead would leave the model calling tools whose schemas no longer match what the client will execute |
| a tool whose `type` is not `"function"` | **400** | `function` is the only kind in OpenAI's envelope; coercing a `retrieval` tool into one would tell the client it was understood |

### Why the `tools` array is bounded

The limits are a **security bound, not a tidiness rule**, and they exist because of
how this interacts with per-request context sizing (#496). That change sizes the
context window to the prompt — `prompt_tokens + max_tokens + headroom`, capped at
the model's `n_ctx_train` — so with an unbounded `tools` array a *caller* would
choose Roteiro's memory allocation. On `qwen3.8-27b` the trained window is 262,144
tokens and KV runs about 64 KiB/token, which is roughly **16.4 GiB reserved for a
single request**. Neither change reaches that alone: before #496 the window is a
fixed 4,096, and before this one Roteiro only ever sizes to prompts it built
itself.

32 KiB of names, descriptions and schemas is roughly 8k tokens, keeping the tool
surface's contribution about 32x below that ceiling. Raising it re-opens the hole
in proportion.

**`role: "tool"` becomes a `user` turn carrying `<tool_response>`, deliberately.**
`llama_chat_apply_template` does not run a Jinja parser — it renders a fixed set
of templates and emits unknown roles literally, so passing `tool` through would
put a role token in the prompt that these models were never trained on. A
`<tool_response>` user turn is what every Qwen template emits natively for a tool
result, so this is the native form rather than a workaround.

## Stability

This crate is **an implementation detail of the `roteiro` CLI**. It is published
only because crates.io requires a published package's dependencies to be registry
packages, so `roteiro` cannot ship unless it does.

Its public API carries **no stability guarantee** — breaking changes ship as minor
version bumps. If you depend on it directly, pin an exact version.

- **Docs:** <https://roteiro.dev>
- **Source & issues:** <https://github.com/OffeneDatenmodellierung/Roteiro>

Dual-licensed under MIT or Apache-2.0.

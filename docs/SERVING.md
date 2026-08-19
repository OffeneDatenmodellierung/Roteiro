---
site-page: serving
site-nav: Serving
site-order: 21
---

# The `/v1` endpoint: what it is, and what it is not

`POST /v1/chat/completions` is OpenAI's path, and a path sets an expectation. This
page is the expectation stated in advance, so that the gaps are documented rather
than discovered on the wire.

Roteiro serves your installed GGUF models over an OpenAI-compatible `/v1`
endpoint, bound to loopback (ADR-0006). It is enabled by building with the
`serve` feature and started with `roteiro serve`.

## It has two modes, and the request chooses

This is the single most useful thing to know about it. The mode is decided by
whether you send a `tools` array — nothing else.

| you send | mode | the model's tools are | grounded in your graph |
| --- | --- | --- | --- |
| **no `tools`** | **Ask** | Roteiro's graph tools — `search`, `context`, `path`, `debt`, … | **yes** |
| **`tools` present** | **general** | **yours only** — the graph tools are not injected | no |

In Ask mode the endpoint is the Ask panel over HTTP: the server runs an agent
loop, calls the graph's own tools, and answers from what it finds. That is the
mode the [Ask page](ask.html) describes.

In general mode it is a local model backend. Your tools replace the graph tools
rather than joining them — a client sending its own tools is using this as a
backend, and adding roughly 3,100 tokens of graph schemas nobody asked for is the
surprise this behaviour exists to remove.

If you want both — an agent with its own tools *and* the graph — that is what
**MCP** is for. Run `roteiro mcp` and your agent gets `search`, `context`, `path`,
`debt` and the rest natively, alongside whatever else it already has.

## Roteiro never executes a tool you supplied

When the model calls one of your tools, Roteiro returns the call with
`finish_reason: "tool_calls"` and stops. You run it, and send the result back as a
`role: "tool"` turn.

This is why client tools need no consent gate and no authorisation story: there is
no code path that reaches one.

If a single turn calls both a client tool and a graph tool, **all** the calls are
returned and **none** are executed — never "run the graph half, return the client
half", which would hand you a `tool_calls` response for an assistant turn you
could not reconstruct, having never seen the graph call or its result.

## Scoped routes

Serving several repositories from one process (ADR-0008) scopes the graph tools by
route. The unscoped route answers about the single repository the server was
started in.

| route | scope |
| --- | --- |
| `/v1/chat/completions` | the server's own repository |
| `/v1/{project}/chat/completions` | one project |
| `/v1/workspaces/{ws}/chat/completions` | a workspace |
| `/v1/models`, `/v1/embeddings`, `/v1/projects` | — |

## Declared divergences from OpenAI

Stated rather than half-implemented, so that a gap is a documented decision rather
than a surprise.

| surface | status | why |
| --- | --- | --- |
| `tools` | **supported** | advertised to the model; calls returned, never run |
| `tool_calls` on the response and on a replayed assistant turn | **supported** | rendered to and from the in-band `<tool_call>` protocol |
| `role: "tool"` / `tool_call_id` / `name` | **supported** | mapped to a `<tool_response>` **user** turn — see below |
| `finish_reason: "tool_calls"` | **supported** | ends the loop when a client tool is called |
| `tool_choice` | **accepted, not enforced** | forcing a named function is grammar-constrained sampling; it lands with the grammar work, and half-implementing it would tell a client it was honoured |
| `parallel_tool_calls` | **accepted, not enforced** | at most one call is parsed per turn today, so a turn never carries more than one regardless |
| streamed `tool_calls` | **`arguments` never fragmented** — every call arrives complete in one chunk | OpenAI splits `arguments` across several chunks. Each call still carries its positional `index`, so a client accumulating by index is unaffected; one-shot is legal and works in mainstream clients |
| assistant prose alongside a tool call | **dropped** — `content` is `null` | OpenAI permits `content` *and* `tool_calls` together; Roteiro returns the calls alone rather than risk leaking `<tool_call>` markup into the answer |
| a client's `role: "tool"` content | **not truncated** | `MAX_TOOL_RESULT` caps results of tools *Roteiro* executes. A client's own result is its own context budget, and silently trimming it would corrupt the transcript the client is correlating against |
| a `tools` array over **128 entries or 32 KiB** | **400, never truncated** | a bound on what a caller can make Roteiro allocate — see below. Trimming instead would leave the model calling tools whose schemas no longer match what you will execute |
| a tool whose `type` is not `"function"` | **400** | `function` is the only kind in OpenAI's envelope; coercing a `retrieval` tool into one would tell the client it was understood |

**`role: "tool"` becomes a `user` turn carrying `<tool_response>`, deliberately.**
`llama_chat_apply_template` does not run a Jinja parser — it renders a fixed set of
templates and emits unknown roles literally, so passing `tool` through would put a
role token in the prompt these models were never trained on. A `<tool_response>`
user turn is what every Qwen template emits natively for a tool result, so this is
the native form rather than a workaround.

## A parameter this endpoint does not implement is dropped, not refused

The request type carries `model`, `messages`, `temperature`, `max_tokens`,
`stream`, `tools`, `tool_choice` and `parallel_tool_calls`. **Anything else you
send is discarded silently at deserialisation.**

Most of OpenAI's remaining parameters are simply an absent feature, which is fine.
Four are not, because a client can reasonably believe they took effect:

| you send | you may believe | actually |
| --- | --- | --- |
| `seed` | output is reproducible | it is not |
| `stop` | generation halts at your marker | it runs to `max_tokens` |
| `response_format: json_object` | the body parses as JSON | it is prose |
| `n` | you receive *n* choices | you receive one |

This is a known gap rather than a decision — it is tracked as
[issue #488](https://github.com/OffeneDatenmodellierung/Roteiro/issues/488), whose
deliverable is the declaration rather than twenty implementations. Until it is
closed, assume a parameter not listed above had no effect.

## Why the `tools` array is bounded

The limits are a **security bound, not a tidiness rule**. Roteiro sizes the model's
context window to the request — `prompt_tokens + max_tokens + headroom`, capped at
the model's trained window — so with an unbounded `tools` array a *caller* would
choose Roteiro's memory allocation. On `qwen3.8-27b` the trained window is 262,144
tokens and KV runs about 64 KiB per token: roughly **16.4 GiB reserved for a single
request**.

32 KiB of names, descriptions and schemas is about 8k tokens, keeping the tool
surface's contribution around 32× below that ceiling. Raising the bound re-opens
the hole in proportion.

## Not a hosted API

Two things follow from this being a local server rather than a service, and both
are deliberate (ADR-0006):

- **It binds loopback by default.** Front a public bind with a reverse proxy; TLS
  and authentication terminate there.
- **It speaks HTTP/1.1, not HTTP/2.** HTTP/2 in practice requires TLS, which is the
  proxy's job, and the multiplexing it buys is worth nothing over loopback against
  a bottleneck that is model inference. See ADR-0006 for the full reasoning.

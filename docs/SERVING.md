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
| a model's `<think>` reasoning block | **stripped** — never in `content`, streamed or not | the rule every other Roteiro consumer already applied. Not offered as `reasoning_content` either — see below |
| a generation that never leaves its `<think>` block | **refused** in the assistant slot, prefixed `Roteiro: ` | there is no answer in it; the deliberation is not returned as a consolation |
| an answer that merely *mentions* `<think>` or `</think>` | **untouched** | a block is text that *opened* with `<think>`. Ask this endpoint what the tags mean and the reply quotes them; treating a quoted tag as a block would truncate a correct answer with `finish_reason: "stop"` still saying nothing was cut |

**A model's reasoning never reaches you, and that is a decision.**
A reasoning-capable GGUF (Qwen3, DeepSeek-R1, …) writes a `<think>…</think>` block
before its answer. Roteiro drops it here exactly as it drops it for `spec draft`
and `review --llm`, and the fact that it did **not** used to was an omission rather
than a policy (#582): the same model producing the same block was cleaned for a CLI
consumer and passed through raw over HTTP. Measured live, that was ~95 of 105
completion tokens of deliberation for a one-word answer.

Three reasons this endpoint strips rather than forwarding:

* **It is Roteiro's Ask over HTTP, not a general model backend** — ADR-0006 and
  #487 settled that framing, and Ask's consumer wants the answer.
* **Multi-turn callers echo assistant turns back as history**, so a block passed
  through is re-sent verbatim on *every* subsequent turn, against a prompt budget
  with no prefix cache today (#578). The same compounding happens inside Roteiro's
  own tool loop, which is why the block is dropped before a turn is fed back into
  the next round.
* **The rule now lives in one place.** `rto_llama::thinking` is shared by this
  endpoint and by every CLI path, so the two cannot drift apart again.

**Stripping keys on a block being opened, never on a tag appearing.** The rule asks
whether the content *starts* with `<think>` — after leading whitespace — and only
then looks for the close tag. This is the whole reason you can ask this endpoint
about its own reasoning handling and get the answer back intact: a reply quoting
`</think>` is prose, and cutting everything before a quoted tag would be the silent
truncation `docs/REVIEW_CHECKLIST.md` §Refusals forbids. The streaming filter has
always read it this way; since #589 the non-streaming half does too, so the two
surfaces agree on what counts as a block as well as on what to do with one.

**`reasoning_content` is deliberately not implemented.** Returning the block under
a field name would be adopting a convention Roteiro does not otherwise speak, and
the block would still cost a multi-turn caller its budget the moment that caller
echoed the turn back. If you want a model's deliberation, run the model directly —
this endpoint is not the place to get it.

**An unterminated block is a refusal, not a short answer.** If generation stops
inside `<think>`, there is no answer in the completion at all, so `content` carries
a `Roteiro: `-prefixed sentence saying which budget ran out rather than the raw
deliberation (#583). This is not hypothetical for the models this registry serves:
`qwen3.8-27b` was measured spending an entire 1,200-token budget inside `<think>`
and emitting no answer. `finish_reason` and the token counts are left exactly as
the model produced them, so a machine client still sees `length` and still bills
for what was really spent.

**`role: "tool"` becomes a `user` turn carrying `<tool_response>`, deliberately.**
`llama_chat_apply_template` does not run a Jinja parser — it renders a fixed set of
templates and emits unknown roles literally, so passing `tool` through would put a
role token in the prompt these models were never trained on. A `<tool_response>`
user turn is what every Qwen template emits natively for a tool result, so this is
the native form rather than a workaround.

## Every request parameter, and what happens to it

This is the whole boundary. All 37 parameters of OpenAI's chat-completions
request body are below, and there is nothing else to send — a key in no row here
is either a typo or newer than this table, and is ignored.

Four statuses, the same four the divergence table above uses:

- **supported** — read and acted on.
- **accepted, not enforced** — parsed and carried, then deliberately not acted
  on. Declared, which is the entire difference between this and the silent drop
  the endpoint used to do.
- **dropped** — ignored, and ignoring it leaves you believing nothing false
  about the response. These are the parameters that drive OpenAI's own
  bookkeeping — storage, billing, cache bucketing, abuse attribution — which a
  loopback server has no analogue for and whose absence changes nothing you can
  observe.
- **400** — refused. Ignoring it would hand you a response that silently
  contradicts your request, and a wrong answer is worse than a refusal. **The
  cell quotes the message you will receive, verbatim.**

| parameter | status | what happens |
| --- | --- | --- |
| `audio` | **400** | `audio` is not supported: no audio is generated, so a request asking for it would come back as text with nothing to say the voice was ignored. This endpoint has no audio output path at all; there is no setting that enables one. |
| `frequency_penalty` | **400** | `frequency_penalty` is not supported: repetition penalties are not wired to the sampler, so the sampling you configured is not the sampling that ran. `temperature` is the one sampling control this endpoint honours. |
| `function_call` | **400** | `function_call` is not supported: the deprecated `function_call` field is not read, so a forced call would simply not be forced. Send `tool_choice` instead — though note that it too is accepted and not enforced here, so neither field will force a named function today. |
| `functions` | **400** | `functions` is not supported: the deprecated `functions` array is not read, so the model would be advertised no tools whatsoever and could never call one. Send the same functions as `tools`, which is supported. |
| `logit_bias` | **400** | `logit_bias` is not supported: no per-token bias reaches the sampler, so tokens you banned can still be generated. Steer with a system message instead; there is no per-token control here. |
| `logprobs` | **400** | `logprobs` is not supported: no log probabilities are computed, so the response carries none and a client reading them finds `null`. This endpoint returns no log probabilities, and there is no flag that turns them on. |
| `max_completion_tokens` | **supported** | OpenAI's current name for the generation budget; the same budget as `max_tokens`, read from the same field — send either, but sending both with different values is a `400` |
| `max_tokens` | **supported** | the generation budget, and the input that sizes the context window for the request; OpenAI deprecated this spelling in favour of `max_completion_tokens`, and both are read |
| `messages` | **supported** | the conversation, including replayed `tool_calls` and `role: "tool"` results |
| `metadata` | **dropped** | free-form labels for OpenAI's dashboard; never read, never echoed |
| `modalities` | **400** | `modalities` is not supported: only text is generated, so asking for another modality returns text with no indication the request was not met. Text is the only output modality this endpoint has; there is no setting that adds another. |
| `model` | **supported** | the model id to run; must be one of `/v1/models` |
| `moderation` | **400** | `moderation` is not supported: no moderation pass runs over the input or the output, so a response you believe was screened was not. A loopback server has no moderation backend to call; screen on your side if you need it. |
| `n` | **400** | `n` is not supported: exactly one choice is generated, so `choices` would come back shorter than you asked for. Send the request once per choice you need and collect the responses yourself; each one is a full generation and costs like one. |
| `parallel_tool_calls` | **accepted, not enforced** | at most one call is parsed per turn today, so a turn never carries more than one regardless |
| `prediction` | **dropped** | a speculative-decoding latency hint — the output is byte-identical with or without it, so ignoring it costs only the speed-up |
| `presence_penalty` | **400** | `presence_penalty` is not supported: repetition penalties are not wired to the sampler, so the sampling you configured is not the sampling that ran. `temperature` is the one sampling control this endpoint honours. |
| `prompt_cache_key` | **dropped** | a cache-bucketing hint for OpenAI's prompt cache; nothing about the response depends on it |
| `prompt_cache_options` | **dropped** | as `prompt_cache_key` |
| `prompt_cache_retention` | **dropped** | as `prompt_cache_key` |
| `reasoning_effort` | **400** | `reasoning_effort` is not supported: the served model's reasoning is not budgeted by this field, so asking for more or less thinking changes nothing. Raise `max_tokens`: a reasoning model spends that budget inside its `<think>` block before it writes a token of answer, so the budget is what actually governs how much it may think. |
| `response_format` | **400** | `response_format` is not supported: the body is prose whatever format you ask for — there is no grammar-constrained sampling on this endpoint, so `json_object` and `json_schema` would both return text that need not parse. Ask for JSON in the prompt and parse defensively. |
| `safety_identifier` | **dropped** | as `user`, which it replaces |
| `seed` | **400** | `seed` is not supported: sampling is not seeded, so repeated requests with the same seed need not agree and the output you believe is reproducible is not. `temperature: 0` selects greedy decoding, which is the nearest thing to reproducible output here — but it is not a seed, and no determinism is guaranteed. |
| `service_tier` | **dropped** | selects OpenAI's processing tier for latency and billing; there is one tier here and the output is unaffected |
| `stop` | **400** | `stop` is not supported: no stop sequence is applied, so generation runs to the `max_tokens` budget and your marker appears in the output rather than ending it. Truncate at your marker on the client side, and set `max_tokens` as the ceiling. |
| `store` | **dropped** | asks OpenAI to retain the completion for its evals products; Roteiro stores nothing and sends nothing anywhere |
| `stream` | **supported** | SSE chunks terminated by `data: [DONE]` |
| `stream_options` | **400** | `stream_options` is not supported: the extra `usage` chunk `include_usage` promises is never streamed, so a client waiting for one before `[DONE]` waits for something that will not arrive. Read `usage` from the non-streaming response, which does carry it. |
| `temperature` | **supported** | the one sampling control this endpoint honours; `0` (or omitted) is greedy |
| `tool_choice` | **accepted, not enforced** | forcing a named function is grammar-constrained sampling, which lands with the grammar work; half-implementing it would tell a client it was honoured |
| `tools` | **supported** | advertised to the model; calls are returned, never run — bounded at 128 entries / 32 KiB |
| `top_logprobs` | **400** | `top_logprobs` is not supported: no log probabilities are computed, so no alternatives come back at any position. This endpoint returns no log probabilities, and there is no flag that turns them on. |
| `top_p` | **400** | `top_p` is not supported: nucleus sampling is not wired to the sampler, so the sampling you configured is not the sampling that ran. `temperature` is the one sampling control this endpoint honours. |
| `user` | **dropped** | an end-user label for OpenAI's abuse tooling; a loopback server has no such tooling and the response is identical either way |
| `verbosity` | **400** | `verbosity` is not supported: response length is not constrained by this field, so a `low` request can return the same wall of text as a `high` one. Ask for the length you want in the prompt, and set `max_tokens` as the hard ceiling. |
| `web_search_options` | **400** | `web_search_options` is not supported: there is no web-search tool and the server never leaves the machine, so a response you believe was informed by a search was not. Omit `tools` to get Ask mode, whose tools search your own graph — that is the local search this endpoint does have. |

### Sending a default is not making a decision

A refusal that fires at a caller who chose nothing would be a worse defect than
the silence it replaces, and client libraries serialise defaults constantly. So
the check reads the **value**, never the presence of the key:

- `null` is never a decision, for any parameter.
- Neither is OpenAI's own documented default — `n: 1`, `top_p: 1`,
  `frequency_penalty: 0`, `logit_bias: {}`, `response_format: {"type":"text"}`,
  `modalities: ["text"]`, and the rest.

`n: 1` and `n: 3` are therefore answered differently, and both correctly: the
first asks for what this endpoint does, the second asks for something it cannot
do. If your client sends `n: 1` and `top_p: 1` on every request — most do — none
of this is aimed at you.

### Why not `deny_unknown_fields`

It was the obvious instrument and it is the wrong one. It would refuse `user`,
`store`, `metadata`, `service_tier` and `prompt_cache_key` — keys clients send
out of habit and whose absence harms nobody — so it would break working callers
in order to fix a correctness problem those callers do not have. The list above
is the narrower answer: refuse what would mislead you, ignore what would not,
and publish both.

The declaration is enforced from the same table that generates this page
(`crates/rto-serve/src/openai_params.rs`), and the tests beside it assert the
table against the request type and against this document in both directions —
so a parameter added to the code without a row here, or a row here without the
code behind it, fails the build rather than the reader.

### Tool arguments are the other way round

Everything above is about the **request envelope**, and none of it extends to the
**arguments of a tool call**. There, an unrecognised key *is* refused.

The two cases look alike and are not. A stray `user` on the request is a field
Roteiro has no use for, and dropping it costs nobody anything. A stray key inside
a tool call's `arguments` is a filter the model asked for and did not get.
Roteiro's two tool surfaces used to spell one of `debt`'s arguments differently —
`kind` on the MCP surface, `categories` here — and while unknown keys were
dropped, `{"categories":["todo"]}` sent to the MCP `debt` deserialised to an empty
filter, which means *every* category. The model asked for one kind of marker,
received the whole repository's debt presented as the filtered set, and had
nothing in the result to tell the two apart.

Both surfaces now spell it **`categories`**, and a test drives that comparison
from the two sets of definitions rather than from a hard-coded pair, so a future
divergence in any shared tool's argument names fails the build. Making unknown
keys an error is what allowed that rename: while they were dropped, renaming the
MCP argument would have left every existing caller parsing fine and silently
receiving *unfiltered* results. Refused, the same caller is told the new name.

So a tool that declares `"additionalProperties": false` has it enforced: a call
carrying a key the schema does not list is refused **by name**, with the keys that
would have worked, and the tool is not run.

```text
tool `debt` error: unknown argument `kind` — `debt` takes only `categories`,
`project`. Nothing was run: an argument this tool does not declare is refused
rather than ignored, because ignoring it would answer a narrower question than
the one you asked and give you no way to tell.
```

This is about Roteiro's own graph tools. A tool **you** supplied is unaffected in
either direction — Roteiro never executes one (see above), so it never inspects
its arguments, and the call comes back to you exactly as the model wrote it.

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

**The bound is measured on what you send, not on what the model reads.** Roteiro
renders a tool's arguments as a signature — `search(query: str, limit?: int
1..25)` — rather than as raw JSON Schema, so the same 32 KiB of wire bytes
becomes fewer prompt tokens than it used to. That makes the bound *more*
conservative, never less: it is applied to the compact JSON before any rendering
happens. Measured on Roteiro's own MCP surface driven back in as a client
payload, 20,045 wire bytes rendered to 21,180 prompt bytes before and 17,454
after, with tool-call accuracy unchanged on a fixed question set.

A schema the renderer cannot state without losing something — a nested object, a
`$ref`, a `oneOf`, `additionalProperties: true` — is sent **verbatim** instead.
Your argument shape is the contract Roteiro hands back for you to execute, and a
lossy summary of it would leave the model calling a tool whose arguments no
longer match what you will run. That is the same failure the size bound refuses
to truncate for.

## Not a hosted API

Two things follow from this being a local server rather than a service, and both
are deliberate (ADR-0006):

- **It binds loopback by default.** Front a public bind with a reverse proxy; TLS
  and authentication terminate there.
- **It speaks HTTP/1.1, not HTTP/2.** HTTP/2 in practice requires TLS, which is the
  proxy's job, and the multiplexing it buys is worth nothing over loopback against
  a bottleneck that is model inference. See ADR-0006 for the full reasoning.

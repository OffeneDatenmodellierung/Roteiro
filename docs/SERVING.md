---
site-page: serving
site-nav: Serving
site-order: 21
---

# The `/v1` endpoint: what it is, and what it is not

`POST /v1/chat/completions` is OpenAI's path, and a path sets an expectation. This
page is the expectation stated in advance, so that the gaps are documented rather
than discovered on the wire. Since #809 there is a second OpenAI path,
`POST /v1/responses`, adapted onto the first; it has [its own section
below](#responses-api) and everything else on this page
applies to it unchanged.

Roteiro serves your installed GGUF models over an OpenAI-compatible `/v1`
endpoint, bound to loopback (ADR-0006). It is enabled by building with the
`serve` feature and started with `roteiro serve`.

## It has two modes, and the request chooses

This is the single most useful thing to know about it. The mode is decided by
whether you send a `tools` array — nothing else.

| you send | mode | the model's tools are | grounded in your graph |
|---|---|---|---|
| **no `tools`**, or an empty `tools: []` | **Ask** | Roteiro's graph tools — `search`, `context`, `path`, `debt`, … | **yes** |
| **a non-empty `tools`** | **general** | **yours only** — the graph tools are not injected | no |

The question is *did you bring tools*, so an empty array answers no and is read
exactly as an absent one. Both wires agree on that, and a test pins them together
so they cannot drift apart on it.

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
|---|---|
| `/v1/chat/completions` | the server's own repository |
| `/v1/{project}/chat/completions` | one project |
| `/v1/workspaces/{ws}/chat/completions` | a workspace |
| `/v1/models`, `/v1/embeddings`, `/v1/projects` | — |
| `/v1/responses` | the server's own repository — **the only scope it has**; there are no scoped Responses routes |

## Declared divergences from OpenAI

Stated rather than half-implemented, so that a gap is a documented decision rather
than a surprise.

| surface | status | why |
|---|---|---|
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
| `POST /v1/responses` | **supported** | streaming-only and unscoped; an adapter over this same tool loop rather than a second implementation — see [its section below](#responses-api) |
| `response.output_text.delta` on a Responses turn that used tools | **fires exactly once, carrying the whole answer** | the tool loop runs several generations and nothing it produces may be published until it has finished, so there is no token-incremental text to stream. The same non-incrementality the chat wire's tooled stream already has; an untooled Responses turn *is* token-incremental, through the same code the chat stream uses |
| a `reasoning` item on a Responses response | **never emitted** | the `<think>` block is stripped on every Roteiro surface, so there is no deliberation left to put in one. A request's `reasoning` parameter is dropped for the same reason |
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
  with nothing reused unless `[serve] prefix_cache_mb` is set (#578). The same
  compounding happens inside Roteiro's
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
|---|---|---|
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

## The Responses API (`POST /v1/responses`) {#responses-api}

OpenAI has a second wire, and some clients now speak only that one: `codex-cli`
0.147 **removed** Chat Completions support — `wire_api = "chat"` is no longer
accepted — so for that family this path is not a convenience, it is the only one.

It is served here as an **adapter**, not a second implementation. A Responses
request is translated into the request the chat path builds, run through the same
tool loop, and rendered back out as Responses events. Everything above about
modes, about tools you supplied, about `<think>` blocks and about refusals is
therefore true of this path too, because it is the same code deciding it.

The surface is deliberately small, and what is missing is **refused by name**
rather than ignored.

| surface | status | why |
|---|---|---|
| `stream: true` | **supported** | the typed SSE event sequence, terminated by `response.completed` — or `response.incomplete` — and then `data: [DONE]` |
| a generation cut short by `max_output_tokens` | **`response.incomplete`, not `response.completed`** | with `status: "incomplete"` and `incomplete_details.reason: "max_output_tokens"`, and the same verdict on the item. This is what the chat wire's `finish_reason: "length"` says on this wire; the text produced so far is still delivered, truncated rather than lost |
| `stream: false`, or omitted | **400** | `stream` must be `true`: this endpoint serves the Responses API as a typed SSE event stream only, so a non-streaming request would have no body shape to return. Send `stream: true` and read `response.completed`, whose `response.output` carries exactly what a non-streaming body would have. |
| `input` as a bare string, or as items of type `message`, `function_call`, `function_call_output` | **supported** | mapped onto the chat wire's turns — see below |
| any other `input` item type — `reasoning`, `web_search_call`, `item_reference`, … | **400** | the item is named in the message. Dropping it silently would leave the model answering from a conversation it was never shown, and a replayed `reasoning` item is the likeliest case |
| a message content part other than `input_text` / `output_text` | **400** | an image or a file you believed was read would otherwise produce a confident answer about content the model never saw |
| a message `role` other than `system` / `developer` / `user` / `assistant` | **400** | `developer` is mapped to `system`; an unrecognised role would be rendered *literally* into the prompt by the chat template, arriving as text rather than as a turn |
| `instructions` | **supported** | mapped to a leading `system` turn |
| a `namespace` tool | **flattened** | a `namespace` groups `function` tools rather than naming one OpenAI executes — `codex-cli` sends `multi_agent_v1` holding five — so its members are advertised on their own names and schemas, and the namespace itself is not a callable tool. Read **one level**: a namespace inside a namespace is a `400`, because nothing has been seen to send one and walking an unobserved shape is a guess |
| a tool whose `type` is neither `function`, `namespace`, nor one of those | **400** | an unrecognised type is far likelier to be a misspelling (`web_serach`) than a hosted tool this list has not met, and dropping it would remove a tool you meant to send and answer anyway. The list is an allowlist read on 2026-09-13; a genuinely new hosted tool is refused by name until it is added, which is the failure worth having |
| a `tools` array in which **every** entry is hosted | **still general mode** | the graph tools stay suppressed. Sending `tools` is what chooses the mode, not what survives translation — a client that asked for its own tools does not get Roteiro's instead, and does not pay the ~3,100 tokens of graph schemas that behaviour exists to spare it. It gets no tools at all, which is the honest answer when none of the ones it sent can be served |
| `strict` on a function tool | **accepted, not enforced** | schema-constrained arguments are grammar-constrained sampling, which lands with the grammar work — the same reason `tool_choice` is not enforced. The chat wire ignores `function.strict` too; it is declared here rather than half-implemented, so a `strict: true` tool may still come back with arguments its schema would have rejected. Validate on your side before executing |
| a hosted tool — `web_search`, `file_search`, `code_interpreter`, `image_generation`, `local_shell`, `mcp`, `computer_use_preview` | **dropped** | Roteiro executes no hosted tool and never advertises one to the model, so the model cannot call it and no answer can be falsely attributed to it. **This diverges from the chat wire**, where a tool whose `type` is not `function` is a `400`: there, `tools` is function-only by construction and a `retrieval` entry is a mistake; here a hosted tool is a normal part of every request from a real client, and refusing it would fail every turn over a tool that was never going to be used |
| `/v1/{project}/responses`, `/v1/workspaces/{ws}/responses` | **404** | scope parity is not built, and there is no substitute: `/v1/{project}/chat/completions` speaks the other wire — it takes `messages`, not `input` — so a Responses-native client cannot reach a scoped graph at all today. Until it lands, a scoped answer needs a server started in that project. The seam these routes will reuse is the same `ChatScope` the chat routes already pass, not a second confinement mechanism (ADR-0008) |

### A tool round trip, and why it is byte-identical to the chat one

Responses models a tool call as typed items where Chat Completions puts
`tool_calls` on a message. Both are translated into the **in-band** protocol the
served models actually speak, by the same function:

| Responses item | becomes | reaches the model as |
|---|---|---|
| `{"type": "function_call", "call_id": …, "name": …, "arguments": …}` | an `assistant` turn carrying one `tool_calls` entry | `<tool_call>{"name":…,"arguments":…}</tool_call>` |
| `{"type": "function_call_output", "call_id": …, "output": …}` | a `role: "tool"` turn | a `user` turn wrapped in `<tool_response>…</tool_response>` |

This is not a parallel rendering that happens to agree. The adapter builds the
chat structures and hands them to the *same* normalisation the chat wire uses, so
there is one implementation of the byte format and a test asserts the two wires
produce identical prompt turns for the same conversation. A second copy would
have drifted, and the symptom would have been a multi-turn tool conversation
degrading at turn three with nothing on the wire to say so.

One inherited detail worth knowing: the keys inside `arguments` come back
**sorted**, because the rendering re-parses the `arguments` string into a JSON
object. That is true of the chat wire already; the adapter inherits it rather
than introducing it.

### The event sequence, and which parts a client actually needs

`response.created` → `response.output_item.added` →
(`response.output_text.delta` | `response.function_call_arguments.delta`) →
`response.output_item.done` → `response.completed`, then `data: [DONE]`. A
generation the token budget cut short ends on `response.incomplete` instead;
`codex-cli` 0.147.0 was measured reading that event and reporting
`Incomplete response returned, reason: max_output_tokens`.

Which of those are load-bearing was **measured**, by serving each event set from
a mock and running a real tool-using turn of `codex-cli` 0.147.0 against it,
rather than by reading a schema or grepping a binary for string literals — a
literal that is absent from a packed binary is not a literal the client does not
need.

| omitted | what happened |
|---|---|
| `response.created` | the turn completed normally — it is **not** required |
| `response.output_item.added` | `OutputTextDelta without active item`, then the turn completed |
| `response.output_text.delta` | the turn completed — deltas are optional |
| `response.output_item.done` | **the turn produced nothing**: the tool call was never dispatched |
| `response.completed` | **`stream disconnected before completion`**, then five reconnection attempts |

So the load-bearing pair is `response.output_item.done` and `response.completed`.
The full sequence is emitted anyway: it is what OpenAI's own wire carries, and a
client that reads the optional events gets a live stream instead of a silent wait.

### Every Responses request parameter

Read the four status words exactly as the chat table above defines them. This
table is shorter than that one on purpose: the chat table is exhaustive over a
frozen wire, and this one carries the parameters a Responses client sends plus
every one whose absence would mislead you. A key in no row here is passed
through untouched, exactly as an unknown chat key is — and for the same reason
(`deny_unknown_fields` would refuse the habitual keys clients send and harm
nobody).

**That rule puts a duty on this table rather than on you.** A parameter that
moves state or output *off the request* — `previous_response_id`, `conversation`
— is a silently wrong answer if it is missing here, not a harmless unknown, so
those are listed and refused. The rows below were read from the Responses
request schema on 2026-09-13 and cross-checked against a captured `codex-cli`
0.147.0 request.

| parameter | status | what happens |
|---|---|---|
| `background` | **400** | `background` is not supported: there is no job store behind this endpoint, so a background response would be started and then never be retrievable by the id you were given. Send `stream: true` and read the events as they arrive; a loopback server has nothing to gain by deferring the work. |
| `conversation` | **400** | `conversation` is not supported: a conversation object lives on the server that issued it and there is no such object here, so the turns it names are not in this request and the model would answer having never seen them. Send the whole conversation in `input` each turn, which is what a stateless endpoint needs. |
| `include` | **dropped** | asks for optional output fields — encrypted reasoning, log probabilities — that this endpoint never produces; the items simply do not appear |
| `input` | **supported** | the conversation, including `function_call` and `function_call_output` items |
| `instructions` | **supported** | the system prompt; mapped to a leading `system` turn |
| `max_output_tokens` | **supported** | the generation budget, and the input that sizes the context window for the request |
| `max_tool_calls` | **400** | `max_tool_calls` is not supported: the tool loop's own round budget is what bounds it, so a lower cap would not be applied and the model could call tools more times than you allowed. There is no per-request tool-call cap on this endpoint; the server-side budget is fixed at build time. |
| `metadata` | **dropped** | free-form labels for OpenAI's dashboard; never read, never echoed |
| `model` | **supported** | the model id to run; must be one of `/v1/models` |
| `parallel_tool_calls` | **accepted, not enforced** | at most one call is parsed per turn today, so a turn never carries more than one regardless |
| `previous_response_id` | **400** | `previous_response_id` is not supported: nothing is stored here, so the turns that id names are not on the server and the model would answer having never seen them. Send the whole conversation in `input` each turn — every prior `message`, `function_call` and `function_call_output` — which is what a stateless endpoint needs. |
| `prompt` | **400** | `prompt` is not supported: a stored prompt template lives in OpenAI's dashboard and cannot be resolved from here, so the instructions you believe were applied were not. Send the template's resolved text as `instructions`. |
| `prompt_cache_key` | **dropped** | a cache-bucketing hint for OpenAI's prompt cache; nothing about the response depends on it |
| `prompt_cache_retention` | **dropped** | as `prompt_cache_key` |
| `reasoning` | **dropped** | a model's `<think>` block is stripped on every Roteiro surface and no `reasoning` item is ever emitted, so neither the effort nor the summary setting has anything to act on — see the divergence table above |
| `safety_identifier` | **dropped** | an end-user label for OpenAI's abuse tooling; a loopback server has no such tooling and the response is identical either way |
| `service_tier` | **dropped** | selects OpenAI's processing tier for latency and billing; there is one tier here and the output is unaffected |
| `store` | **400** | `store` is not supported: Roteiro retains nothing and sends nothing anywhere, so the response you asked to keep is gone the moment the stream ends and the id you were given addresses nothing. Send `store: false` and keep the turns yourself, replaying them in `input`; that is the only conversation state this endpoint has. |
| `stream` | **supported** | the typed SSE event sequence; **`true` is the only value served** and `false` is a `400` — see the divergence table above |
| `stream_options` | **dropped** | its one field, `include_obfuscation`, pads events against traffic analysis on a network this endpoint does not cross — it is bound to loopback, so the padding would defend nothing and its absence changes no field you can read |
| `temperature` | **supported** | the one sampling control this endpoint honours; `0` (or omitted) is greedy |
| `text` | **400** | `text` is not supported: there is no grammar-constrained sampling on this endpoint, so a `json_schema` format would return prose that need not parse and a verbosity setting would not change the length. Ask for the shape and the length you want in the prompt, and parse defensively. |
| `tool_choice` | **accepted, not enforced** | forcing a named function is grammar-constrained sampling, which lands with the grammar work; half-implementing it would tell a client it was honoured |
| `tools` | **supported** | `function` tools are advertised to the model and their calls returned, never run — bounded at 128 entries / 32 KiB. A hosted tool (`web_search`, `code_interpreter`, …) is dropped rather than refused; see the divergence table above |
| `top_logprobs` | **400** | `top_logprobs` is not supported: no log probabilities are computed, so no alternatives come back at any position. This endpoint returns no log probabilities, and there is no flag that turns them on. |
| `top_p` | **400** | `top_p` is not supported: nucleus sampling is not wired to the sampler, so the sampling you configured is not the sampling that ran. `temperature` is the one sampling control this endpoint honours. |
| `truncation` | **400** | `truncation` is not supported: `auto` asks the server to drop turns from the middle of the conversation so that an over-long input still answers, and nothing here does that — an input past the context window is refused, which is the exact failure `auto` was set to avoid. Trim the conversation on your side and send the shortened `input`; only the caller knows which turns it can afford to lose. |
| `user` | **dropped** | an end-user label for OpenAI's abuse tooling; a loopback server has no such tooling and the response is identical either way |

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

## Reusing a preamble instead of re-prefilling it

A context is built per generation and the prompt is prefilled in full, so a
multi-turn client re-pays for the part of its prompt that never changes — its
system message and its `tools` array. Measured on `qwen3.8-27b` at **3.13 ms per
prompt token**, a 32 KiB tool surface is about 21 s per turn, every turn.

`[serve] prefix_cache_mb` stops that. Set it in `~/.roteiro/config.toml`:

```toml
[serve]
prefix_cache_mb = 512
```

Unset — the default — nothing is cached and behaviour is exactly as before.

**Size it in whole preambles.** An entry costs a fixed **~150 MiB** plus 64 KiB
per token, because `qwen3.8-27b` is a hybrid attention/SSM model whose recurrent
state serialises whole regardless of prompt length. There is no such thing as a
small entry: 256 holds roughly one preamble, 1024 roughly three, and anything
below ~200 stores nothing at all.

Several preambles for one model coexist, so two clients with different system
prompts each get their own — an entry is only displaced when a new one
that it is a prefix of arrives, or when the budget forces a least-recently-used
eviction.

**What to expect.** The boundary is learned by comparing consecutive prompts, so
the first two turns pay full price and reuse begins at the third. Measured end to
end on a 1,462-token prompt: **5.80 s → 2.11 s**, with byte-identical output.

Three things it deliberately does not do:

* **No partial credit.** A prompt that shares only part of a cached preamble is a
  miss and prefills in full. Recurrent state has no per-position structure and
  cannot be rewound, so a trimmed restore would be silently wrong on 48 of this
  model's 64 layers rather than an error. A prompt *equal* to a cached preamble
  misses too, for a different reason: with everything restored there would be no
  token left to batch, and nothing to carry logits.
* **No client key.** `prompt_cache_key` stays dropped. The preamble is found by
  comparing prompts, so a client gets this without asking and cannot mistakenly
  ask for another client's state.
* **Nothing under speculative decoding.** A speculative generation runs target and
  draft contexts whose states are not independent, so the cache stays inert there
  rather than restoring half a pair.

Everything it holds is recomputable, so a failed restore or a failed snapshot
costs a request nothing: it falls back to prefilling in full.

## Not a hosted API

Two things follow from this being a local server rather than a service, and both
are deliberate (ADR-0006):

- **It binds loopback by default.** Front a public bind with a reverse proxy; TLS
  and authentication terminate there.
- **It speaks HTTP/1.1, not HTTP/2.** HTTP/2 in practice requires TLS, which is the
  proxy's job, and the multiplexing it buys is worth nothing over loopback against
  a bottleneck that is model inference. See ADR-0006 for the full reasoning.

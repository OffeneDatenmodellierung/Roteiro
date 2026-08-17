# rto-remote

The remote model tier for [Roteiro](https://roteiro.dev) — the project's one
explicitly-consented egress path (ADR-0019).

Everything else Roteiro does runs on this machine. This crate is the exception,
and it is written as an exception: it holds the **policy** for sending
repository content to a hosted model, and it holds **no transport at all**.

## No transport, on purpose

`call_with` takes the transport as a caller-supplied closure, exactly as
`rto-exec` takes its `Fetcher`. The code that decides whether bytes may leave is
not the code that can make them leave, which turns "nothing goes out without
consent" from a promise into a property you can check by reading a
`Cargo.toml`. It also means every test here exercises the whole path with no
network, so a test cannot accidentally become the first thing that sends data.

`rto-graph` — extraction and the graph — gains nothing from this. Its `gix` is
still pinned `default-features = false` to exclude the transports, and this
crate depends *on* it rather than the other way round.

The socket lives in the `roteiro` binary, in `remote_transport.rs`, behind the
off-by-default `remote` feature: **one** function, handed to `call_with` as a
closure. Three commands can reach it — `roteiro remote call`, `roteiro spec draft
--allow-remote` and `roteiro serve --allow-remote` (the Ask panel's model) — and
all three go through the same gate, the same allow-list and the same ledger,
because there is only one path to that closure. Adding an HTTP client to *this*
crate's `Cargo.toml` is the change that would undo all of the above, which is why
the manifest says so where someone would be adding one.

## Reading the response is also here, where there is no socket

`response::parse` turns a chat-completion body into an answer, and refuses
anything that is not a whole one: a generation stopped at a token limit, an empty
completion, a body the endpoint filled with its own error, a body that is not the
shape it claims to be. It is a pure function of a `&str`, so every one of those
failures is tested against a string literal — the receive side gets the same
guarantee as the send side, that a test cannot become the first thing that talks
to a server.

Refusing a truncated answer rather than returning it is the point. A completion
that stopped early *reads* as finished, so handing it over would be the silent
downgrade ADR-0019 most needs to prevent: a different answer with no signal that
anything changed.

## The consent model, which is inverted for one key

ADR-0007's precedence is **CLI flag > project `roteiro.toml` > user
`~/.roteiro/config.toml` > built-in default**. For the remote-enable key, and
only that key, it inverts:

| Layer | May deny | May grant |
|---|---|---|
| Built-in default | denied by default | — |
| Project `roteiro.toml` | **yes** | **no** |
| User `~/.roteiro/config.toml` | yes | yes — necessary, not sufficient |
| Invocation (`--allow-remote`) | yes | yes — necessary, not sufficient |

`roteiro.toml` is committed and shared by design, so a merged line authorising
egress on every teammate's machine is not consent — it is consent by pull
request, granted by someone else, noticed by nobody. A project may still switch
the tier **off** for everyone: denial has none of the problems of grant.

## What may be sent

An explicit allow-list, not "whatever the local path happened to build".
`ContextItem::from_node` reads five named fields off a graph node — key, kind,
name, path, and up to 1,500 characters of `meta.content`. Everything else in a
node's free-form `meta` is unreachable from a payload.

`dry_run` prints the exact bytes a call would send, and `call_with` sends that
same string. Source code is not sent — function bodies are not in the graph.
**That is not the same as nothing identifying being sent**: symbol names and
paths identify a codebase, and there is no redaction chokepoint on a prompt.
`Payload::disclosure()` says so in full, and every surface that can send prints
it.

## Every call is recorded

An append-only JSONL ledger records the endpoint, the model string, the
`ProducerTrust`, the timestamp and a copy of the body — written **before** the
transport runs, so a call that hung is still a call you know about. A ledger
that cannot be written refuses the call rather than sending unrecorded.

## `ProducerTrust` lives in `rto-graph`

It was defined here, and it moved when `rto_graph::ModelSource::Remote` had to
carry it: this crate depends on `rto-graph`, so a variant in that crate's model
resolver cannot name a type defined here. `rto_remote::ProducerTrust` still
resolves — it is a re-export, and there is exactly one type behind both paths.

That single definition is the point rather than a tidiness preference. The grade
answers *"is this identity a measurement or a claim?"*, and an answer that a
ledger entry and a model resolution could disagree about is not an answer.

## Two things it deliberately does not do

**It does not probe reachability.** A probe *is* egress — a DNS lookup leaks the
query to a resolver, and doing it to decide whether egress is permitted inverts
the gate. Policy, not measurement.

**It does not route.** There is no classifier on the local→remote edge, at any
model quality. `escalation` measures a *finished* local attempt — empty output,
no tool call after `MAX_ROUNDS`, below a length floor — and records the number it
measured. A trigger is an input to the consent gate, never a substitute for it.

Licensed under MIT OR Apache-2.0.

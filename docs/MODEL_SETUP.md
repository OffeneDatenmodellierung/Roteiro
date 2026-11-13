# Model setup — pointing Roteiro at a model server

Roteiro does not need a model. Graph building, `search`, `query`, `check`,
`review` and the explorer all run without one, and `infer` falls back to a
built-in hashing embedding.

The features that do use a language model send it to an **OpenAI-compatible
server** — by default one already running on this machine:
[Ollama](https://ollama.com), `llama-server` from
[llama.cpp](https://github.com/ggml-org/llama.cpp), or LM Studio. It can also be
a hosted endpoint (see [Hosted models](#hosted-models)). Roteiro does not
download or run those models itself
([ADR-0027](adr/0027-external-model-provider.md),
[ADR-0028](adr/0028-one-engine-local-or-hosted.md)).

> **Migration status.** `roteiro serve` (the `/v1` endpoint and the explorer's
> Ask panel) and `roteiro spec draft` use the provider today. `review --llm`,
> `infer --model` and `media build` still use the in-process engine and
> `roteiro model pull` until they are moved over; this page grows as each one
> does.

## 1. Run a provider

**Ollama** (chat, embeddings, vision):

```sh
ollama serve                 # listens on 127.0.0.1:11434
ollama pull qwen3:8b         # chat / Ask
ollama pull bge-m3           # embeddings (optional)
ollama pull qwen2.5vl:3b     # image description (optional)
```

**llama-server** (everything above, plus audio — Ollama does not accept audio
input):

```sh
llama-server -hf ggml-org/Voxtral-Mini-3B-2507-GGUF --port 8080
```

## 2. Point Roteiro at it

In `~/.roteiro/config.toml` (or a project's `roteiro.toml`):

```toml
[provider]
base_url = "http://127.0.0.1:11434/v1"   # Ollama; llama-server is :8080/v1

[models]
generative = "qwen3:8b"
embedding  = "bge-m3"
vision     = "qwen2.5vl:3b"
```

`[models]` values are the **provider's** model ids — what `ollama list` shows —
not Roteiro registry names.

Two rules worth knowing:

- **A provider off this machine is announced on every run**, and must be
  `https://`. See [Hosted models](#hosted-models).
- **A credential is an environment variable**, `ROTEIRO_PROVIDER_API_KEY`, never
  a config key. Ollama and `llama-server` need none by default; a hosted
  endpoint is sent it as `Authorization: Bearer …`.

## 3. Check it

`roteiro serve` and the explorer are not in the default build, so install with
their features. `serve` still links the in-process engine, so it needs the same
C++ toolchain (cmake and clang) as before:

```sh
cargo install roteiro --features serve,explorer   # explorer: the Ask panel
roteiro serve
curl -s http://127.0.0.1:8017/v1/models
```

The model list is taken from `[models]`; Roteiro does not ask the provider at
startup. A model the provider does not have is reported on the first request,
by name.

## How a draft is checked

`roteiro spec draft` prints two report lines on stderr. Neither one changes the
document or the exit status (ADR-0031):

- `coverage:` lists the symbols and decisions the draft was given and never
  mentions.
- `style:` is a controlled-English score for sentence length, plain words, filler
  and one name for each concept. It measures style only, not correctness.

`--repair N` rewrites each section against its style findings, up to N passes
(at most 3). Roteiro keeps a rewrite only if it scores better and still
mentions everything the draft mentioned. Each pass is another model call.
Against a hosted provider, each pass also sends repository content again.

`roteiro docs prose` runs the same style lint over the authored documents.

## Hosted models

For teams whose enterprise subscription covers sending code to a hosted model,
point `base_url` at the vendor's OpenAI-compatible endpoint. For Claude:

```toml
[provider]
base_url = "https://api.anthropic.com/v1"

[models]
generative = "claude-sonnet-5-5"   # or claude-opus-5-5, claude-haiku-5-5
```

```sh
export ROTEIRO_PROVIDER_API_KEY=sk-ant-...
```

Everything the local path builds is sent: the full conversation, graph tool
results, symbol names, paths and captured prose. Roteiro prints the host on
stderr on every run, keeps no log of what it sent, and refuses plaintext
`http://` to another machine. A hosted endpoint is reached through
`HTTPS_PROXY`/`ALL_PROXY` when set (`NO_PROXY` is honoured); a local provider
never is.

A committed `roteiro.toml` can set this for a whole team, so check a
repository's `[provider]` table before running Roteiro in it. Anthropic serves no
embeddings, so leave `embedding` unset.

## Which models each feature needs

| Feature | `[models]` key | Suggested | Needed? |
|---|---|---|---|
| Ask panel, `/v1` chat, `spec draft` | `generative` | `qwen3:8b`; `qwen3:0.6b` on small machines; `qwen2.5-coder:3b` for code | for chat |
| `/v1/embeddings` | `embedding` | `bge-m3` or `nomic-embed-text` | optional |
| Image description | `vision` | `qwen2.5vl:3b` | optional |
| Audio transcription | `audio` | Voxtral Mini 3B via `llama-server` | optional |
| OCR | `ocr` | `ocrs-text` via `roteiro model pull` — runs inside Roteiro, not on the provider | optional |

Tags change upstream; check `ollama list` or the provider's model library for
the current names.

# Model setup — pointing Roteiro at a local model server

Roteiro does not need a model. Graph building, `search`, `query`, `check`,
`review` and the explorer all run without one, and `infer` falls back to a
built-in hashing embedding.

The features that do use a language model send it to an **OpenAI-compatible
server already running on this machine** — [Ollama](https://ollama.com),
`llama-server` from [llama.cpp](https://github.com/ggml-org/llama.cpp), or LM
Studio. Roteiro does not download or run those models itself
([ADR-0027](adr/0027-external-model-provider.md)).

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

- **The provider must be on this machine.** A `base_url` whose host is not
  `localhost`, `127.0.0.1` or `::1` is refused: sending repository content to
  another machine is egress, which the remote tier
  ([ADR-0019](adr/0019-remote-model-tier.md)) governs separately.
- **A credential is an environment variable**, `ROTEIRO_PROVIDER_API_KEY`, never
  a config key. Ollama and `llama-server` need none by default.

## 3. Check it

```sh
roteiro serve
curl -s http://127.0.0.1:8017/v1/models
```

The model list is taken from `[models]`; Roteiro does not ask the provider at
startup. A model the provider does not have is reported on the first request,
by name.

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

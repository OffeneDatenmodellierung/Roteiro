# rto-serve

Local **model serving** for [**Roteiro**](https://roteiro.dev) (ADR-0006): an
opt-in, loopback, **OpenAI-compatible `/v1` endpoint** over your installed GGUF
models, backed by llama.cpp.

Serve `chat/completions` (streaming), `models`, and `embeddings` over the same
models Roteiro uses internally, and optionally auto-register Roteiro's **graph
tools** so the served model can query your codebase during a conversation. Bound
to loopback by default; front a public bind with a reverse proxy. Pulled in via
the `roteiro serve --features serve` build.

- **Docs:** <https://roteiro.dev>
- **Source & issues:** <https://github.com/OffeneDatenmodellierung/Roteiro>

Dual-licensed under MIT or Apache-2.0.

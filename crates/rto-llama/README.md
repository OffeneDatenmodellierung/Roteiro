# rto-llama

Roteiro's **llama.cpp inference core** (ADR-0003 / ADR-0006): the `Engine` trait
and a llama.cpp-backed engine for **generation, embeddings, and vision** (mmproj),
shared by the model server and Roteiro's internal uses.

Deliberately free of HTTP/async dependencies — just the inference primitives —
so both the server (`rto-serve`) and the graph crate can run local GGUF models
without pulling a web stack. Models load on demand into a **memory-bounded LRU**
(unloading the least-recently-used past a byte budget), and embedding runs reuse
one context per batch. The real engine is behind the `llama` feature (which
compiles llama.cpp); without it, only the trait and types build.

## Stability

This crate is **an implementation detail of the `roteiro` CLI**. It is published
only because crates.io requires a published package's dependencies to be registry
packages, so `roteiro` cannot ship unless it does.

Its public API carries **no stability guarantee** — breaking changes ship as minor
version bumps. If you depend on it directly, pin an exact version.

- **Docs:** <https://roteiro.dev>
- **Source & issues:** <https://github.com/OffeneDatenmodellierung/Roteiro>

Dual-licensed under MIT or Apache-2.0.

---
site-page: models
site-nav: Models
site-order: 6
---

# Recommended local models {#models}

Roteiro runs no model in its own process except OCR (ADR-0033). Chat, `spec
draft`, `review --llm`, embeddings, image description and transcription go to
the **provider** you name with `[provider] base_url` — Ollama, `llama-server`,
LM Studio, or a hosted API — and `[models]` holds that provider's model ids. See
`docs/MODEL_SETUP.md` for a working setup.

## OCR — the one model `roteiro model pull` installs

<table>
<tr><th>Model</th><th>Size</th><th>Best for</th></tr>
<tr><td><code>ocrs-text</code></td><td>~12 MB</td><td>Pure-Rust OCR — literal text in screenshots (<code>image-ocr</code>). Horizontal text only: rotated text (margin stamps, vertical labels) is silently not read.</td></tr>
</table>

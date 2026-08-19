---
site-page: models
site-nav: Models
site-order: 6
---

# Recommended local models {#models}

Models are **pluggable and platform-aware**: `roteiro model list` recommends a
pick per section tuned to your machine (Apple-silicon Metal builds on macOS,
standard GGUF elsewhere) — treat it as the source of truth. All local models are
GGUF, run through the shared llama.cpp engine. A rough guide:

## Embedding — for `infer` / `duplicates`

<table>
<tr><th>Model</th><th>Size</th><th>Dim</th><th>Best for</th></tr>
<tr><td><code>bge-small-en-v1.5-gguf</code></td><td>~65 MB</td><td>384</td><td>Any laptop — the recommended default.</td></tr>
<tr><td><code>bge-base-en-v1.5</code></td><td>~200 MB</td><td>768</td><td>Stronger recall on a moderate machine.</td></tr>
<tr><td><code>bge-large-en-v1.5</code></td><td>~640 MB</td><td>1024</td><td>Best quality, for a workstation.</td></tr>
</table>

## Generative — for `spec draft` / serving

<table>
<tr><th>Model</th><th>Size</th><th>Role</th><th>Best for</th></tr>
<tr><td><code>qwen3-0.6b</code></td><td>~380 MB</td><td>instruct</td><td>Tiny, offline — the <code>spec draft</code> default.</td></tr>
<tr><td><code>qwen3-8b</code></td><td>~4.8 GB</td><td>instruct</td><td>Stronger drafting on a ~16 GB machine.</td></tr>
<tr><td><code>qwen3-32b</code></td><td>~18 GB</td><td>instruct</td><td>Best drafting, for a workstation.</td></tr>
<tr><td><code>qwen2.5-coder-3b</code></td><td>~1.8 GB</td><td>coding</td><td>Code completion &amp; code Q&amp;A.</td></tr>
<tr><td><code>deepseek-r1-distill-qwen-1.5b</code></td><td>~1.0 GB</td><td>reasoning</td><td>Small chain-of-thought reasoning.</td></tr>
</table>

## Vision & OCR — for image ingestion

<table>
<tr><th>Model</th><th>Size</th><th>Best for</th></tr>
<tr><td><code>ocrs-text</code></td><td>~12 MB</td><td>Pure-Rust OCR — literal text in screenshots (<code>image-ocr</code>).</td></tr>
<tr><td><code>smolvlm-500m-gguf</code></td><td>~520 MB</td><td>Describing diagrams &amp; photos (<code>image-vision</code>).</td></tr>
</table>

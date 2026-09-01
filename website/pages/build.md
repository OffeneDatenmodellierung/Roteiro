---
site-page: build
site-nav: Install
site-order: 1
---

# Install & build {#build}

Roteiro is a Rust workspace (MSRV 1.96). Install the lean default build from
crates.io, or build from source with the feature tiers you want.

<pre><code><span class="c"># Lean default build — pure Rust, no network call of its own,</span>
<span class="c"># and `roteiro model pull` included (--locked uses the published lockfile)</span>
cargo install roteiro --locked

<span class="c"># Pin a specific release</span>
cargo install roteiro@1.1.0 --locked

<span class="c"># Pick the capabilities you want</span>
cargo install roteiro --features "inference,mcp" --locked

<span class="c"># Everything on — MCP, serving, local models, PDF/OCR/vision.</span>
<span class="c"># Needs a C/C++ toolchain (the inference-local-models/serve/image-vision features build llama.cpp).</span>
cargo install roteiro --all-features --locked

<span class="c"># …or build from source</span>
git clone https://github.com/OffeneDatenmodellierung/Roteiro
cd Roteiro
cargo build --release --all-features</code></pre>

## Feature tiers

<table>
<tr><th>Feature</th><th>Adds</th></tr>
<tr><td><em>(default)</em></td><td>Graph build, query, check, render, the <code>roteiro model</code> registry, and <code>roteiro security ingest|list|prefetch|status|run</code> — smallest binary, no C++/cmake toolchain, and no network call unless you consent to a <code>pull</code> or a <code>prefetch --allow-download</code>.</td></tr>
<tr><td><code>inference</code></td><td><code>roteiro infer</code> / <code>duplicates</code> with a pure-Rust hashing embedder. Still fully offline; no download.</td></tr>
<tr><td><code>models</code> <em>(default)</em></td><td>The <code>roteiro model</code> registry: list and consent-gated <code>pull</code> of local models. On by default — <code>pull</code> is the prerequisite for working offline, so a stock install has it.</td></tr>
<tr><td><code>inference-local-models</code></td><td>Run pulled GGUF embedding &amp; generative models via the shared llama.cpp engine.</td></tr>
<tr><td><code>pdf-text</code> · <code>image-ocr</code> · <code>image-vision</code></td><td>Ingest text from PDFs, OCR text from images, and describe images with a local vision model.</td></tr>
<tr><td><code>exec-subprocess</code> <em>(default)</em></td><td>Runs an analyzer (<code>semgrep</code>, <code>osv-scanner</code>, <code>cargo audit</code>) as a child process on this host. You install the analyzer; Roteiro never does. <code>security run</code> is <strong>sandboxed by default</strong>, so a build without <code>exec-boxlite</code> refuses and tells you how to get one; <code>--allow-unsandboxed</code> is how you choose this host instead, and it records <code>isolation=none</code>. Use <code>--no-default-features --features execution</code> for a build that provisions and ingests but cannot execute.</td></tr>
<tr><td><code>exec-boxlite</code></td><td>The same analyzers inside a digest-pinned OCI image in a microVM — read-only worktree, no egress. Needs <code>protoc</code> and a provisioned runtime; see the README.</td></tr>
<tr><td><code>serve</code></td><td>An OpenAI-compatible <code>/v1</code> model endpoint over your installed models (llama.cpp).</td></tr>
<tr><td><code>remote</code></td><td><strong>The one feature that can send your repository's content off this machine</strong> — <code>roteiro remote status|dry-run|call|log</code> against a hosted model, plus <code>spec draft --allow-remote</code> and <code>serve --allow-remote</code> (Ask), which take the same gate (ADR-0019). Off by default and staying off. Compiling it does not enable it: a run needs your <code>~/.roteiro/config.toml</code> <em>and</em> the invocation, and a committed <code>roteiro.toml</code> may deny it but never grant it. <a href="./#remote-tier">Read the note on the home page</a> before turning it on.</td></tr>
<tr><td><code>mcp</code></td><td>The MCP graph server, exposing the graph to AI agents (stdio or HTTP).</td></tr>
<tr><td><code>--all-features</code></td><td>Every capability above at once — the largest build. Requires a C/C++ toolchain because the serving and vision features compile llama.cpp. <strong>This includes <code>remote</code></strong>, so such a build <em>can</em> call a hosted model once you grant it in your user config and per run; it still cannot without both.</td></tr>
</table>

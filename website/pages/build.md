---
site-page: build
site-nav: Install
site-order: 1
---

# Install & build {#build}

Roteiro is a Rust workspace (MSRV 1.96). Download a prebuilt binary, install
the lean default build from crates.io, or build from source with the feature
tiers you want.

## Download a binary {#download}

Every release from 8.0.0 on carries prebuilt binaries on its
[release page](https://github.com/OffeneDatenmodellierung/Roteiro/releases/latest),
built with **every feature** (`--all-features`) — none of which opens a socket
or runs an analyzer without your consent, exactly as in the default build.

<table>
<tr><th>Platform</th><th>File</th></tr>
<tr><td>macOS 11+, Apple silicon (Intel Macs are not supported)</td><td><code>roteiro-&lt;version&gt;-macos-arm64.tar.gz</code></td></tr>
<tr><td>Windows 10+, x86_64</td><td><code>roteiro-&lt;version&gt;-windows-x86_64.zip</code></td></tr>
</table>

Each has a `.sha256` beside it. The macOS binary is signed with a Developer ID
and notarized by Apple after 8.0.1; macOS checks it online the first time it
runs. The Windows binary is **not signed**, so its first run needs one step:

<pre><code><span class="c"># macOS: unpack and put it on your PATH</span>
tar -xzf roteiro-*-macos-arm64.tar.gz
sudo mv roteiro-*/roteiro /usr/local/bin/

<span class="c"># 8.0.1 and earlier are unsigned: if macOS cannot verify the developer,</span>
<span class="c"># clear the download quarantine once</span>
xattr -d com.apple.quarantine /usr/local/bin/roteiro

<span class="c"># Windows (PowerShell): unpack, unblock, run from the folder or add it to PATH</span>
Expand-Archive roteiro-*-windows-x86_64.zip -DestinationPath .
Get-ChildItem roteiro-*\roteiro.exe | Unblock-File</code></pre>

Linux users: `cargo install roteiro --locked` below — the default build needs
only the C compiler Rust itself requires.

## Install with cargo {#cargo}

<pre><code><span class="c"># Lean default build — pure Rust, no network call of its own,</span>
<span class="c"># and `roteiro model pull` included (--locked uses the published lockfile)</span>
cargo install roteiro --locked

<span class="c"># Pin a specific release</span>
cargo install roteiro@1.1.0 --locked

<span class="c"># Pick the capabilities you want</span>
cargo install roteiro --features "inference,mcp" --locked

<span class="c"># Everything on — MCP, serving, PDF/OCR, media generators. No C/C++ toolchain needed.</span>
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
<tr><td><code>models</code> <em>(default)</em></td><td>The <code>roteiro model</code> registry: list and consent-gated <code>pull</code> of the OCR weights, the one model Roteiro runs in-process.</td></tr>
<tr><td><code>pdf-text</code> · <code>image-ocr</code> · <code>image-vision</code> · <code>audio-transcribe</code></td><td>Ingest text from PDFs and OCR text from images during <code>sync</code>; describe images and transcribe audio through the provider for <code>media build</code>.</td></tr>
<tr><td><code>exec-subprocess</code> <em>(default)</em></td><td>Runs an analyzer (<code>semgrep</code>, <code>osv-scanner</code>, <code>cargo audit</code>) as a child process on this host. You install the analyzer; Roteiro never does. There is no sandbox (<a href="https://github.com/OffeneDatenmodellierung/Roteiro/blob/main/docs/adr/0030-remove-the-microvm-sandbox.md">ADR-0030</a>), so <code>security run</code> refuses unless you pass <code>--allow-unsandboxed</code> on every run, and the run records <code>isolation=none</code>. Use <code>--no-default-features --features execution</code> for a build that provisions and ingests but cannot execute.</td></tr>
<tr><td><code>serve</code> <em>(default)</em></td><td><code>roteiro serve</code>: the web UI, the graph API, and an OpenAI-compatible <code>/v1</code> that runs the graph tools in front of your provider.</td></tr>
<tr><td><code>mcp</code></td><td>The MCP graph server, exposing the graph to AI agents (stdio or HTTP).</td></tr>
<tr><td><code>--all-features</code></td><td>Every capability above at once — the largest build. Pure Rust plus <code>ring</code>'s C; no cmake or libclang.</td></tr>
</table>

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

## Hosted providers — Anthropic and others {#hosted}

A hosted `base_url` sends repository content to that host, and Roteiro says so
on every run. There are two ways to authenticate.

**With an API key.** Put it in the environment — never in a config file:

<pre><code>export ROTEIRO_PROVIDER_API_KEY=sk-ant-…        <span class="c"># PowerShell: $env:ROTEIRO_PROVIDER_API_KEY = "sk-ant-…"</span></code></pre>

**With OAuth (enterprise), through Anthropic's `ant` CLI.** Roteiro runs a
command you name to get a fresh access token (ADR-0032), so no long-lived key is
stored anywhere.

1. **Install `ant`:**

<pre><code><span class="c"># macOS</span>
brew install anthropics/tap/ant

<span class="c"># Windows (PowerShell) — x64; use windows_arm64 on Arm. VERSION from github.com/anthropics/anthropic-cli/releases</span>
$v = "2.9.0"
Invoke-WebRequest "https://github.com/anthropics/anthropic-cli/releases/download/v$v/ant_${v}_windows_amd64.zip" -OutFile ant.zip
Expand-Archive ant.zip -DestinationPath "$env:LOCALAPPDATA\Programs\ant"
<span class="c"># then add %LOCALAPPDATA%\Programs\ant to PATH and open a new terminal</span>

<span class="c"># Linux / WSL — VERSION from github.com/anthropics/anthropic-cli/releases</span>
VERSION=2.9.0
curl -fsSL "https://github.com/anthropics/anthropic-cli/releases/download/v${VERSION}/ant_${VERSION}_$(uname -s | tr A-Z a-z)_$(uname -m | sed -e s/x86_64/amd64/ -e s/aarch64/arm64/).tar.gz" \
  | sudo tar -xz -C /usr/local/bin ant

<span class="c"># Or from source (Go 1.25+)</span>
go install github.com/anthropics/anthropic-cli/v2/cmd/ant@latest</code></pre>

2. **Log in once** — it opens a browser; on a remote host add `--no-browser` —
   and check which organisation and workspace the token belongs to:

<pre><code>ant auth login
ant auth status</code></pre>

3. **Point Roteiro at it** in your *user* config, `~/.roteiro/config.toml`
   (Windows: `%USERPROFILE%\.roteiro\config.toml`). `token_command` is ignored
   in a project's `roteiro.toml`, because a committed command would run code from
   any repository you clone:

<pre><code>[provider]
base_url = "https://api.anthropic.com/v1"
token_command = ["ant", "auth", "print-credentials", "--access-token"]

[provider.headers]
anthropic-beta = "oauth-2025-04-20"   <span class="c"># Anthropic requires this beside an OAuth token</span>

[models]
generative = "claude-sonnet-5-5"</code></pre>

4. **Leave `ROTEIRO_PROVIDER_API_KEY` unset** — when set, it wins over the
   command. Always keep `--access-token`: without it `ant` prints JSON, which
   Roteiro refuses. A token is reused for five minutes and fetched again when the
   provider answers `401`.

Other providers take the same shape with their own command: Google Vertex AI
`["gcloud", "auth", "print-access-token"]`; Azure / Microsoft Foundry
`["az", "account", "get-access-token", "--resource", "<resource>", "--query", "accessToken", "-o", "tsv"]`;
a corporate gateway, its credential helper.

Then start the server over every configured workspace — see [Ask](ask.html):

<pre><code>roteiro serve --scope all</code></pre>

## OCR — the one model `roteiro model pull` installs

<table>
<tr><th>Model</th><th>Size</th><th>Best for</th></tr>
<tr><td><code>ocrs-text</code></td><td>~12 MB</td><td>Pure-Rust OCR — literal text in screenshots (<code>image-ocr</code>). Horizontal text only: rotated text (margin stamps, vertical labels) is silently not read.</td></tr>
</table>

---
site-page: modes
site-nav: Modes
site-order: 3
---

# The five ways to run it {#modes}

## 1 · Offline mode — the default {#offline}

No models, no network. Build the graph, query it, verify authored intent, and
render docs. This alone is a complete tool.

<pre><code><span class="c"># Scaffold the store, git hooks, an AGENTS.md block and an agent skill</span>
<span class="c"># (.agents/skills/roteiro/SKILL.md; also .github/skills when the repo uses .github)</span>
roteiro init

<span class="c"># Build / incrementally update the graph for the working tree</span>
roteiro sync

<span class="c"># Explain a node — its provenance-labelled neighbourhood</span>
roteiro query 'sym:rust:crates/rto-graph/src/store.rs#Store'
roteiro query --kind fn                 <span class="c"># list every function node</span>

<span class="c"># Verify authored links against the code; non-zero exit on drift (CI gate)</span>
roteiro check

<span class="c"># Shortest path between two nodes, and outstanding intent-debt markers</span>
roteiro path 'file:src/main.rs' 'adr:0001'
roteiro debt

<span class="c"># Render the graph to a docs site or an Obsidian vault</span>
roteiro render docs
roteiro render obsidian

<span class="c"># …or one vault spanning a whole workspace, members and all</span>
roteiro render obsidian --workspace-name payments</code></pre>

Without `--workspace-name`, `render obsidian` renders the current project, with
unqualified note names, even when that repository is a member of a configured
workspace. With one, every note is keyed `<project>::<key>` so two repositories'
`README.md` cannot claim the same note, and cross-repo links resolve inside the
vault. The filename is derived from that key — a readable lowercase hint, then a
hash of the whole key — so no filename contains `::`.

Two things to know before you rely on a vault: **the output directory is deleted
and rebuilt on every render**, so your own notes belong *outside* it and link
into it by name — and **note names changed in issue #574, with no migration**, to
stop two nodes silently landing on one file. Both are explained on the
[Obsidian vault](obsidian-vault.html) page.

A workspace vault is **shareable**, and says so about itself. Its `_Home`
carries a *Reproducing this vault* section — when it was rendered, and each
member's clone URL and the commit it was rendered from — so a reader can
reconstruct the workspace it describes rather than take its word for it, and can
tell a stale vault from a current one.

It also carries each member's **analyzer findings**, so the vault answers *what
is wrong with this workspace* as well as *what is in it*. Two things follow, and
the vault states both where a reader is about to act on them. It distinguishes
*"an analyzer ran and found nothing"* from *"nobody has ever looked"* — those are
opposite facts, and only the first is good news. And because it lists unpatched
weaknesses and where they are, a shared vault cannot be un-shared: treat it as
you would the analyzer reports themselves. Agent memory is the one thing left
out entirely (it has no redaction chokepoint at all), and config values are
redacted by **key name**, so a secret in a value whose key is not named like one
is not redacted — narrower than it sounds, and far more consequential in an
artifact you hand to someone than in a local store.

## 2 · Online mode — richer inference with local models {#online}

Pull a real embedding or generative model *once* (with consent), then run
everything locally — the “online” is a one-time, explicit download, after which
inference is offline again. Requires `--features inference-local-models`.

<pre><code><span class="c"># See models curated for THIS machine's resources, then pull one</span>
roteiro model list
roteiro model pull bge-small-en-v1.5-gguf
<span class="c">#   Download bge-small-en-v1.5-gguf (~65 MB, MIT) from …? [y/N]</span>

<span class="c"># Suggest inferred similarity edges using the pulled embedding model</span>
roteiro infer --model bge-small-en-v1.5-gguf --min-confidence 0.5

<span class="c"># Report likely-duplicate content (same blob, or near-identical embeddings)</span>
roteiro duplicates --min-similarity 0.9

<span class="c"># Draft a house-style, graph-grounded ADR/blueprint with a generative model</span>
roteiro spec scaffold "rate limiting" --kind adr
roteiro spec draft draft-adr.md         <span class="c"># fills placeholder sections</span></code></pre>

<div class="note"><strong>“Online mode” means the download, not the inference.</strong>
The one-time <code>model pull</code> is the only thing that touches the network here; after
it, every model above runs on this machine and nothing you ask is sent anywhere. The
separate, default-off capability that <em>does</em> call a hosted model at use time is the
<a href="./#remote-tier"><strong>remote model tier</strong></a> — a different feature, a
different name, and a consent model of its own. If you enabled
<code>inference-local-models</code>, you did not enable that.</div>

## 3 · Serving mode — the network HTTP server {#serving}

`roteiro serve` is the networked server: an OpenAI-compatible `/v1` API over your
installed models, the read-only `/v1/graph` API and the [explorer](#explorer) web
UI — all on one loopback port. Built with `--features serve` and a model
installed, it also enables the **Ask** tab; without the model feature (or none
installed) it degrades gracefully to the model-free graph API + UI. Binds
`[serve] addr` (default `127.0.0.1:8017`); set `[serve] tls_cert`/`tls_key` for
in-process HTTPS.

<pre><code><span class="c"># Network server on 127.0.0.1:8017 — /v1 + /v1/graph + the web UI</span>
roteiro serve                            <span class="c"># → prints the URL to open</span>

<span class="c"># Call /v1 on that default bind like any OpenAI endpoint</span>
curl http://127.0.0.1:8017/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{"model":"qwen3-0.6b","messages":[{"role":"user","content":"hello"}]}'

<span class="c"># Or override the bind address/port and terminate TLS in-process (then use https://…)</span>
roteiro serve --addr 0.0.0.0:8443 --tls-cert fullchain.pem --tls-key privkey.pem</code></pre>

<div class="note"><strong>No auth on <code>/v1</code>.</strong> A non-loopback bind like
<code>0.0.0.0:8443</code> is warned about — use in-process TLS
(<code>[serve] tls_cert</code>/<code>tls_key</code>) or a reverse proxy. In-process TLS is a
<code>serve</code> feature; <code>roteiro explorer</code> serves plain HTTP only.</div>

## 4 · MCP mode — the graph, for AI agents {#mcp}

Expose the knowledge graph to an MCP-capable agent (Claude, editors, custom
tooling). Agents get typed tools to query nodes, fetch a node's context bundle,
find paths and list debt — grounded in the *same* graph humans review.
Requires `--features mcp`.

<pre><code><span class="c"># Default: the MCP graph server over stdio (for a local agent to spawn)</span>
roteiro mcp

<span class="c"># Or networked over streamable HTTP (terminate TLS at a reverse proxy)</span>
roteiro mcp --http 127.0.0.1:8080

<span class="c"># Restrict what this server exposes — to a list, or to everything read-only</span>
roteiro mcp --tools search,explain,context,path
roteiro mcp --tools read-only            <span class="c"># drops sandbox_clear, the one mutating tool</span></code></pre>

<div class="note"><strong>The server decides what it exposes.</strong> <code>--tools</code>
(and <code>[mcp] tools</code> in config) bounds <em>both</em> <code>tools/list</code> and
<code>tools/call</code> — a tool that is not advertised is not callable either, because a client
that already knows the name is exactly the case a restriction is for. An unknown name, or a
restriction leaving nothing to serve, is a startup error rather than a server that quietly
advertises everything. It is a size lever as well as a permission one: on this repository the full
surface is 20,045 advertised bytes and a graph-only ten-tool list is 9,056.</div>

<div class="note"><strong>Renamed in v1.5.</strong> The MCP graph server moved from
<code>roteiro serve</code> to <code>roteiro mcp</code>; bare <code>roteiro serve</code> is now
the <a href="#serving">network HTTP server</a>. The old <code>serve --http</code> and
<code>serve --models</code> spellings still work as deprecated aliases (with a notice).</div>

## 5 · Explorer mode — the graph, in your browser {#explorer}

An interactive web view of the graph. Start at a workspace overview — the
cross-repo topology, the config override matrix and drift — then click any repo
to drop into its own node/edge graph: hotspots, intent-debt with reasons, a node
detail panel, and a *follow-the-link* hop that jumps from a spoke's config key
straight to the hub struct that defines it. Fully offline and model-free;
requires `--features explorer`.

<pre><code><span class="c"># Serve the explorer UI on loopback (no model needed)</span>
roteiro explorer                        <span class="c"># → prints the URL to open in your browser</span>

<span class="c"># Pick a workspace by name, or default to the repo you're in</span>
roteiro explorer --workspace-name payments</code></pre>

<div class="note"><strong>Chat to your graph in the browser.</strong> Build with
<code>--features serve,explorer</code> and run <code>roteiro serve</code>:
it serves the same explorer UI <em>and</em> the model endpoint on one port, which
enables the <strong>Ask</strong> tab — natural-language questions answered by a
local model calling the graph's tools. <code>roteiro explorer</code> on its own
stays model-free, with Ask disabled.</div>

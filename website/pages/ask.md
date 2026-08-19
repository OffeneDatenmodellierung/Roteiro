---
site-page: ask
site-nav: Ask
site-order: 4
---

# Ask questions of your code {#ask}

Two ways to interrogate the graph: **structured queries** that need no model and
run fully offline, and **natural-language questions** answered by a local model
that calls the graph's tools for you. Both read the *same* provenance-tagged
store, so answers are grounded in your actual code and decisions — not the
model's training data.

<div class="note">The natural-language path is also the <strong>Ask</strong> tab in the <a href="modes.html#explorer">graph explorer</a> — same tools, same grounding, in your browser.</div>

## Structured — offline, no model

Precise lookups straight against the graph. Deterministic, instant, and
scriptable (add `--json` to any of these).

<pre><code><span class="c"># Find, then explain: ranked text search — curated ADRs/blueprints rank first</span>
roteiro search "provenance model"

<span class="c"># Explain a returned node: its provenance-labelled neighbourhood</span>
roteiro query 'sym:rust:crates/rto-graph/src/store.rs#Store'

<span class="c"># Everything relevant to a symbol — callers, callees, governing ADRs</span>
roteiro context 'sym:rust:crates/rto-graph/src/sync.rs#sync'

<span class="c"># How are two things connected? Shortest path between nodes</span>
roteiro path 'file:src/main.rs' 'adr:0006'

<span class="c"># What decisions and docs exist? List by kind, or find open debt</span>
roteiro query --kind adr
roteiro debt

<span class="c"># What changed on this branch, and does it drift from authored intent?</span>
roteiro review --base main</code></pre>

## Natural language — via a local model

Serve your installed models over the OpenAI-compatible `/v1` endpoint, then ask
in plain English. The server runs an agent loop that calls the graph's `search`,
`context` and `path` tools and answers from what it finds — so *“what should this
project be used for?”* is answered from your ADRs and blueprints, not guessed.
Requires `--features serve`.

<pre><code><span class="c"># Start the network server (loopback; pick another port with --addr if 8017 is taken)</span>
roteiro serve

<span class="c"># Ask it like any OpenAI chat endpoint — grounded in your graph</span>
curl -s http://127.0.0.1:8017/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{"model":"qwen3-8b","messages":[{"role":"user",
       "content":"What should Roteiro be used for?"}]}' \
  | jq -r '.choices[0].message.content'

<span class="c"># More examples — each triggers a grounded tool call</span>
<span class="c">#   "Where is the git-native store implemented?"</span>
<span class="c">#   "How does sync relate to ADR-0006?"</span>
<span class="c">#   "What outstanding intent-debt is there?"</span></code></pre>

<div class="note"><strong>&ldquo;Like any OpenAI chat endpoint&rdquo; has one important exception.</strong>
Send no <code>tools</code> array and you get the mode above &mdash; Roteiro&rsquo;s graph tools,
grounded in your code. Send <strong>your own</strong> <code>tools</code> array and they
<em>replace</em> the graph tools, because a client bringing its own tools is using this
as a general backend. Roteiro never executes a tool you supplied; it returns the call
and stops. <a href="serving.html">The endpoint&rsquo;s full contract</a> sets out both
modes, the declared divergences from OpenAI, and the parameters that are accepted and
dropped.</div>

<div class="note"><strong>Pick a model that can call tools.</strong> Grounded answers
depend on the model actually invoking the graph tools. <code>qwen3-8b</code> is the
reliable default — it tool-calls consistently and runs comfortably on a
<strong>16&nbsp;GB MacBook Air (M3)</strong>. The tiny <code>qwen3-0.6b</code> is great for
<code>spec draft</code> but too small for agentic tool use — it tends to answer from
memory and hallucinate. Set it once in <code>roteiro.toml</code> under
<code>[models] generative = "qwen3-8b"</code>, or name it per request as above.</div>

<div class="note"><strong>Already using an AI agent?</strong> Skip curl entirely — run
<code>roteiro mcp</code> and your agent gets the same <code>search</code> /
<code>context</code> / <code>path</code> / <code>debt</code> tools natively, grounded in the same
graph. See <a href="modes.html#mcp">MCP mode</a>. <code>roteiro init</code> also drops an
<strong>agent skill</strong> at <code>.agents/skills/roteiro/SKILL.md</code> (the portable,
cross-tool location; also <code>.github/skills/</code> for GitHub Copilot when the repo
already uses <code>.github</code>) that teaches any agent how to drive the graph — when
to <code>search</code> vs <code>query</code> vs <code>context</code>, the provenance model, and
the plan/review flows.</div>

<div class="note"><strong>Many repos, one server.</strong> <code>roteiro serve
--workspace ~/code</code> (or a <code>[workspace]</code> config) hosts every repo under a
root from a <em>single</em> process — the model is loaded once and each project's
graph is opened on demand (ADR-0008). The graph tools gain a <code>project</code>
argument and a <code>list_projects</code> tool, so one endpoint answers questions about
any of your projects: <em>"in <code>beta</code>, where is auth handled?"</em>. Omit
<code>--workspace</code> for the single-repo default.</div>

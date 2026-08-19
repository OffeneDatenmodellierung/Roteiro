---
site-page: adopt
site-nav: Use it
site-order: 2
---

# Use it in your project {#adopt}

Adopting Roteiro in your own repository is one command. `roteiro init`
scaffolds the store, installs the git hooks that keep it fresh and gate drift,
and writes an `AGENTS.md` snippet so your AI agents are graph-aware.

<pre><code><span class="c"># From the root of your git repository</span>
roteiro init
<span class="c">#   builds the initial graph (stored under .git/, not committed)</span>
<span class="c">#   installs managed git hooks (see below)</span>
<span class="c">#   writes/updates an AGENTS.md section pointing agents at the graph</span></code></pre>

## What `init` sets up

<table>
<tr><th>Piece</th><th>What it does</th></tr>
<tr><td>The store</td><td>A content-addressed graph under <code>.git/</code> — per-worktree, shared cache, never committed.</td></tr>
<tr><td><code>post-checkout</code> · <code>post-merge</code> · <code>post-commit</code> hooks</td><td>Keep the graph fresh automatically as <code>HEAD</code> moves — no manual <code>sync</code>.</td></tr>
<tr><td><code>pre-commit</code> hook</td><td>Runs <code>roteiro check</code> and <strong>blocks a commit that introduces drift</strong> (a dangling ADR link or a stale <code>@rto:</code> annotation). Skip once with <code>git commit --no-verify</code>.</td></tr>
<tr><td><code>AGENTS.md</code></td><td>A managed section telling agents to query the graph and to run <code>roteiro review</code>/<code>check</code> — the cross-tool standard many agents read.</td></tr>
</table>

## The everyday loop

After `init`, the hooks keep the graph current for you. As you work:

<pre><code><span class="c"># Review your change against the graph — not just the diff:</span>
<span class="c"># each touched symbol's callers/callees, the ADRs governing it, the</span>
<span class="c"># drift &amp; intent-debt it adds, and the dependents to re-check.</span>
roteiro review

<span class="c"># Verify authored intent still holds (the pre-commit hook runs this too):</span>
roteiro check

<span class="c"># Explain any node, or trace how two are connected:</span>
roteiro query 'sym:rust:src/lib.rs#Thing'
roteiro path 'file:src/main.rs' 'adr:0001'</code></pre>

## In CI

Make the graph a merge gate — `check` exits non-zero on drift, so a PR
that breaks an ADR link or annotation fails the build:

<pre><code><span class="c"># In your pipeline (validates the committed HEAD tree)</span>
cargo install roteiro --locked <span class="c"># or download a release binary</span>
roteiro check --committed</code></pre>

<div class="note"><strong>Authoring intent.</strong> Link decisions to code with ADRs
(<code>&#91;&#91;path#Symbol&#93;&#93;</code> wiki-links) and inline <code>// @rto:&lt;adr-id&gt;</code>
annotations. <code>check</code> keeps them honest; <code>roteiro spec</code> helps you
draft house-style, graph-grounded ADRs to begin with.</div>

---
site-page: plan
site-nav: Plan
site-order: 5
---

# Plan a change {#plan}

Asking questions is one half; the other is **writing intent down**. The `spec`
workflow turns a topic into a house-style, drift-checked ADR or blueprint,
*grounded in the graph* — so a decision links to the real symbols it governs and
`roteiro check` can hold code to it later. Four steps; only one needs a model.

<pre><code><span class="c"># 1 · Gather grounded context for a topic — the symbols, files and</span>
<span class="c">#     ADRs already related to it, straight from the graph (offline)</span>
roteiro spec context "rate limiting"

<span class="c"># 2 · Scaffold a house-style, check-clean skeleton with resolving</span>
<span class="c">#     links and an interview checklist — no model needed (offline)</span>
roteiro spec scaffold "rate limiting" --kind adr --out draft-adr.md

<span class="c"># 3 · Draft the prose with a local generative model, filling the</span>
<span class="c">#     placeholders from the graph context (needs --features serve</span>
<span class="c">#     or inference-local-models, then a pulled model)</span>
roteiro spec draft draft-adr.md

<span class="c"># Then verify the authored links hold against the code (CI gate)</span>
roteiro check</code></pre>

What `spec context` surfaces is real: for *“concurrency”* in this repo it returns
the `Engine` trait, the concurrency test symbols that exercise it, and the
blueprint section that governs them — the exact material a good ADR should cite.
The skeleton it scaffolds is already `check`-clean, so you are filling in
reasoning, not wiring up metadata.

<div class="note"><strong>Offline gets you most of the way.</strong>
<code>spec context</code> and <code>spec scaffold</code> run with no model at all;
only <code>spec draft</code> (the prose generation) needs a generative model. On a
<strong>16&nbsp;GB MacBook Air (M3)</strong>, <code>qwen3-8b</code> drafts comfortably;
<code>qwen3-0.6b</code> is the tiny built-in default if you just want a first pass.</div>

---
site-page: config
site-nav: Config
site-order: 7
---

# Configuration {#config}

Defaults are sensible, so a config file is optional. When you want to pin
choices, Roteiro merges two layers: a per-user `~/.roteiro/config.toml` and a
per-project `roteiro.toml` at the repository root (project wins). `roteiro
config` prints the effective, merged values and labels which layer set each one.
**One key inverts that**: `[remote] enabled`, which opens Roteiro's only egress
path, may be switched *off* by the committed project file but never *on* — see
below.

<pre><code><span class="c"># roteiro.toml — every key is optional; shown with its default</span>

[models]                                <span class="c"># unset ⇒ offline defaults; `roteiro config` shows what each surface resolved to</span>
embedding  = "bge-small-en-v1.5-gguf"   <span class="c"># embedding model for `infer`</span>
generative = "qwen3-0.6b"               <span class="c"># model for `spec draft` / serving / Ask</span>
vision     = "smolvlm-500m-gguf"        <span class="c"># image description for `media build`</span>
audio      = "voxtral-mini-3b"          <span class="c"># speech transcription for `media build`</span>
ocr        = "ocrs-text"                <span class="c"># literal image text for `sync`</span>

[remote]                                <span class="c"># the optional remote model tier — OFF by default, and the one key</span>
enabled  = false                        <span class="c"># whose precedence inverts: roteiro.toml may set this false for</span>
                                        <span class="c"># everyone, and may NEVER set it true. Only your own</span>
                                        <span class="c"># ~/.roteiro/config.toml can grant it — and a run still needs</span>
                                        <span class="c"># --allow-remote. Both are required; neither alone is enough.</span>
endpoint = "https://…/v1/chat/completions"  <span class="c"># ordinary key: either layer may choose the destination</span>
model    = "vendor/model-name"          <span class="c"># a vendor model string is a mutable pointer, so anything recorded</span>
                                        <span class="c"># from this tier is `vendor_asserted`, never digest-pinned</span>

[ingest]                                <span class="c"># which blob content `sync` embeds</span>
prose  = true                           <span class="c"># Markdown / plain text bodies</span>
pdf    = true                           <span class="c"># PDF text   (needs the pdf-text feature)</span>
ocr    = true                           <span class="c"># image OCR  (needs the image-ocr feature)</span>
vision = true                           <span class="c"># image description (needs image-vision)</span>

[paths]                                  <span class="c"># which repository paths the scan reads at all</span>
exclude = ["raw/**"]                     <span class="c"># not in the graph at all — no node, bytes never read</span>
opaque  = ["manifest/**"]                <span class="c"># a file node and nothing else — no content, no config</span>
                                         <span class="c"># keys, no markers, not an ADR however it declares itself</span>

[infer]
min_confidence = 0.4                     <span class="c"># cosine-similarity floor for a suggestion</span>
top_k          = 5                       <span class="c"># max suggestions per node</span>

[duplicates]
min_similarity = 0.9                     <span class="c"># floor for a near-duplicate pair</span>
limit          = 50                      <span class="c"># max pairs reported</span>

[serve]                                  <span class="c"># the network server (roteiro serve): /v1 + graph API + UI</span>
addr   = "127.0.0.1:8017"               <span class="c"># bind address/port (CLI --addr overrides)</span>
scope  = "all"                           <span class="c"># what to serve: here (default) | all | workspace &lt;NAME&gt; | bundle &lt;PATH&gt;</span>
models = ["qwen3-0.6b"]                 <span class="c"># which installed models to expose (default: all)</span>
tools  = true                            <span class="c"># expose graph tools to the model</span>
tls_cert = "/etc/roteiro/tls/fullchain.pem"  <span class="c"># in-process HTTPS (with tls_key); omit both for plain HTTP</span>
tls_key  = "/etc/roteiro/tls/privkey.pem"    <span class="c"># PEM private key paired with tls_cert</span>

[mcp]                                    <span class="c"># the MCP graph server (roteiro mcp, serve --mcp)</span>
tools = ["query", "quality"]             <span class="c"># restrict the advertised surface — a class, a tool name, or "read-only"</span>

[workspace]                              <span class="c"># host many repos from one server (ADR-0008)</span>
roots = ["~/code"]                       <span class="c"># scanned ONE LEVEL deep — each immediate child that is a repo</span>
repos = []                               <span class="c"># …or list explicit repo paths (any depth)</span>
include_worktrees = false                <span class="c"># host linked git worktrees a root finds (default false)</span>

&#91;&#91;workspaces&#93;&#93;                          <span class="c"># several named workspaces (a hub + its spokes)</span>
name  = "payments"                      <span class="c"># select with --workspace-name, or by cwd</span>
repos = ["~/work/app", "~/work/deploy"]

[standalone]                            <span class="c"># repos with no cross-repo links (each its own graph)</span>
repos = ["~/code/dotfiles"]

&#91;&#91;links&#93;&#93;                                <span class="c"># authored cross-repo link (ADR-0009)</span>
to = "app::sym:rust:src/config.rs#ServeConfig"  <span class="c"># a &lt;project&gt;::&lt;key&gt; in another repo</span>
from = "file:k8s/deployment.yaml"       <span class="c"># optional local anchor in this repo</span>

[pins]                                   <span class="c"># map a deployed artifact → a hub git ref</span>
app = "release-{tag}"                    <span class="c"># image app:1.4 → git ref release-1.4</span></code></pre>

<div class="note"><strong>Interlink a hub app with its deployment repos.</strong> The
<code>&#91;&#91;links&#93;&#93;</code> and <code>[pins]</code> tables belong to Roteiro's cross-repo
layer — see <a href="cross-repo.html">Cross-repo: a hub and its spokes</a>.</div>

<div class="note"><strong><code>[mcp] tools</code> narrows and never widens.</strong> Naming a
tool declines every tool not named, so the layers <strong>intersect</strong>: <code>--tools</code>,
a project <code>roteiro.toml</code> and your <code>~/.roteiro/config.toml</code> each may narrow the
surface and none may widen it — a committed project file cannot restore a tool you removed. An
unknown name, or a restriction leaving nothing to serve, refuses to start rather than quietly
advertising everything. See <a href="https://github.com/OffeneDatenmodellierung/Roteiro/blob/main/docs/adr/0007-configuration-file.md">ADR-0007</a> v1.5.</div>

<div class="note"><strong>Name a class, not ten tools.</strong> <code>query</code>,
<code>quality</code>, <code>security</code> and <code>sandbox</code> are accepted wherever a tool
name is, and a hand-written list of names is exactly what goes stale when a tool is added. Every
advertised tool costs tokens on every turn whether the session could reach it or not, and
<code>security</code> + <code>sandbox</code> are roughly two fifths of it. The default is still
every class. Whatever you leave out, <code>list_tool_classes</code> stays advertised and says so
— see <a href="modes.html#mcp">MCP mode</a>.</div>

<div class="note"><strong><code>roots</code> is scanned one level deep.</strong> Each
<em>immediate</em> child of a root that holds a <code>.git</code> becomes a project, plus the root
itself if it is one — never recursively. A repo at <code>~/code/&lt;org&gt;/&lt;repo&gt;</code> is
<em>not</em> found: name each <code>&lt;org&gt;</code> directory as its own root, or list the repos
explicitly. <code>roteiro serve</code>, <code>roteiro mcp</code> and the
<a href="modes.html#explorer">explorer</a> print what each root offered beside the project count,
so a near-empty workspace says why.</div>

<div class="note"><strong>A <code>roots</code> scan walks past git worktrees.</strong> A
linked worktree — a second checkout made by <code>git worktree add</code>, whose <code>.git</code>
is a file pointing into the main checkout — is not an independent project, and hosting it presents
one repository as several peers at several revisions: coupling, hotspot and debt figures count its
symbols once per checkout, and a workspace-scoped question can retrieve one file on three branches
as three independent sources. <code>roteiro serve</code>, <code>roteiro mcp</code> and the explorer
say how many each root walked past, beside the project count.

<p>Two ways to host one anyway. <code>repos = ["…/my-worktree"]</code> names it directly and always
works — an explicit path is never <em>discovered</em>, so the rule does not apply to it; that is the
one-off case. <code>include_worktrees = true</code> sets the rule for one workspace's roots, for a
directory that holds a pool of them on purpose. The key sits beside <code>roots</code> in
<code>[workspace]</code>, in any <code>&#91;&#91;workspaces&#93;&#93;</code> entry and in
<code>[standalone]</code>, and belongs to that group alone — two workspaces may name the same root
and answer differently.</p>

<p><strong>Standing inside a worktree is different.</strong> Skipping applies to
<em>discovery</em>, not to selection: <code>cd &lt;worktree&gt; &amp;&amp; roteiro serve</code>
serves that worktree, scoped to it alone and on its own branch, because being there is as
explicit an act as naming it. The startup line says so, and names the repository the worktree
belongs to and the branch it is checked out on — a worktree presented as though it were the
repository is the same silent misrepresentation, from the other side. Sibling repositories beside
the worktree are not picked up: <code>--scope here</code> means this checkout.</p>

<p>A worktree whose main checkout is outside every root is skipped too. Nothing else in the scan
holds that content, so this is the one case where the rule drops something that used to be graphed;
the note names it and either escape hatch restores it. See
<a href="https://github.com/OffeneDatenmodellierung/Roteiro/issues/837">issue #837</a>.</p></div>

<div class="note"><strong>Many workspaces, one install.</strong> A lone <code>[workspace]</code> is the default workspace; add <code>&#91;&#91;workspaces&#93;&#93;</code> for several named hub-and-spoke groups and <code>[standalone]</code> for repos that stand alone. <code>roteiro links</code>, <code>roteiro serve</code>, <code>roteiro mcp</code> and the <a href="modes.html#explorer">explorer</a> pick one with <code>--workspace-name</code> (or the workspace containing your current directory).</div>

<div class="note">Turning an <code>[ingest]</code> class off changes what <code>sync</code>
extracts, so affected blobs are re-processed on the next run — the content-addressed
cache folds the ingestion config into its key. <code>[paths]</code> is folded into the same
key, for the same reason.</div>

<div class="note"><strong><code>[paths]</code> removes the node; <code>[debt] ignore</code>
only mutes the report.</strong> They look alike and are not: an ignored path still has its
nodes in the store, so <code>search</code>, <code>explain</code> and <code>roteiro export</code>
still see it. A path under <code>exclude</code> has none, and one under <code>opaque</code> has
only its identity — path, size, blob id — with nothing derived from what the bytes say.
<strong>Three states, because two of the requests differ:</strong> a committed data manifest
usually wants <code>opaque</code> (still findable, so a missing source is detectable, but not
mined into hundreds of "configuration settings" and false debt markers), while a corpus of source
documents you ingest another way wants <code>exclude</code>. Both are consulted by every reader,
including the one that decides whether a markdown file is one of your ADRs — so a downloaded
document carrying <code>type: adr</code> under a declared path is not adopted as your decision.
<strong>Nothing is excluded by default</strong>, including <code>raw/</code>: a built-in would
drop a directory out of your graph on upgrade without saying so.</div>

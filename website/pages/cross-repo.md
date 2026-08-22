---
site-page: cross-repo
site-nav: Cross-repo
site-order: 8
---

# Cross-repo: a hub and its spokes {#crossrepo}

A common shape: one **hub** application repo, and many **spoke**
deployment/config repos that each pin a version of it and override its
configuration. Roteiro joins their per-repo graphs at the workspace — **a
query-time join, never a merged store**, so each repo's isolation holds — and
answers the questions that boundary raises: *which deployment overrides
`serve.addr`, and to what? which reference config keys the app no longer
defines? and are they drifting from the version they actually deploy?*
([ADR-0009](adr/)).

## A spoke, as Roteiro sees it

A deployment repo is mostly config plus a pinned version — nothing to
hand-author. `roteiro sync` extracts it into graph nodes:

<pre><code><span class="c"># deploy-web/ — a spoke repo</span>
prod.env               <span class="c"># SERVE_ADDR, SERVE_TOOLS, …    → config_key nodes</span>
k8s/deployment.yaml    <span class="c"># image, env, ConfigMap data    → config_key nodes</span>
Dockerfile             <span class="c"># FROM registry/app:1.4         → image_ref  (the version it pins)</span>
app/ (+ .gitmodules)   <span class="c"># the hub vendored @ &lt;sha&gt;       → submodule  (a version pin)</span>
roteiro.toml           <span class="c"># &#91;&#91;links&#93;&#93; and [pins]          (optional)</span></code></pre>

YAML is *mined*, not blindly flattened: a Kubernetes manifest yields the
container `image`, its literal `env`, and ConfigMap/Secret `data` — the settings
a deployment actually overrides — while a Helm `values.yaml` flattens like any
config. Secret values are redacted. Inspect exactly what was extracted with
`roteiro query --kind config_key` (or `--kind image_ref` / `--kind submodule`).

## See the overrides & drift at a glance

Point `links` at your workspace. `--matrix` renders every hub key against each
spoke that overrides it, plus a drift list of keys the app no longer defines — as
a text table, `--json`, or a self-contained `--html` page you open in a browser:

<pre><code>roteiro links --matrix --hub app --workspace ~/code

<span class="c"># cross-repo config overrides (hub: app, 2 spoke(s))</span>
<span class="c">#</span>
<span class="c">#   serve.addr = 127.0.0.1:8017</span>
<span class="c">#     ≠ deploy-web:   0.0.0.0:8443     (0.90)   ← a real override</span>
<span class="c">#     = deploy-batch: 127.0.0.1:8017   (0.98)   ← redundant restatement</span>
<span class="c">#</span>
<span class="c">#   drift — 1 orphan key(s):</span>
<span class="c">#     deploy-web: DB_PASSWORD = &lt;redacted&gt;      ← the app doesn't define it</span></code></pre>

## Gate it in CI, or let it infer

For links that must never silently drift (a TLS path, the served model), declare
them in the spoke and fail the build when a target vanishes — the
authored-vs-reality check of `roteiro check`, run *between* repos:

<pre><code>roteiro links --workspace ~/code   <span class="c"># resolves every &#91;&#91;links&#93;&#93; entry; exits non-zero on drift</span>
roteiro links --workspace ~/code --write
                                   <span class="c"># …and persist them as durable **authored**</span>
                                   <span class="c">#    edges — gold, not candidates</span>

<span class="c"># Or skip the authoring — match config keys automatically:</span>
roteiro links --infer --hub app    <span class="c"># correspondences + orphan (drift) keys, confidence-scored</span>
roteiro links --infer --write      <span class="c"># …and persist them as durable inferred</span>
                                   <span class="c">#    cross-repo edges that survive re-sync</span></code></pre>

Without `--write`, `links` only reports — it is a CI gate, and a gate that
mutates as a side effect is a surprise, the more so because it writes into the
*other* repositories' graphs. With it, each resolved declaration becomes an
`authored` edge that survives re-sync and is replaced wholesale on the next run,
so deleting a `[[links]]` entry removes its edge.

The authored and inferred layers are **independent**: running one never
reclassifies or deletes the other's edges, which is what makes the
`authored` / `inferred` distinction worth reading. A declaration with no `from`
anchor has nothing local to attach an edge to; it is still reported, and the run
says how many were skipped for that reason.

## Resolve against the version actually deployed

A spoke rarely runs the hub's latest — it deploys a *pinned* version. Measuring
drift against the hub's `HEAD` is then wrong twice: a key renamed since the spoke
deployed looks like drift when it's fine, and a key the deployed version dropped
is missed. `--pinned` reads each spoke's own pin (its submodule sha, or its
Docker image tag) and resolves that spoke against exactly the hub version it
vendors — materialising the hub graph at that commit **in memory, no checkout**:

<pre><code>roteiro links --infer --pinned --hub app --workspace ~/code

<span class="c"># deploy-web — 4 match(es), 0 orphan(s)  @ 4e0d5a6afd (via submodule app)</span>

<span class="c"># …or pin one explicit version for the whole workspace:</span>
roteiro links --infer --hub app --hub-rev v1.4</code></pre>

The same resolution works on the **matrix** — the side-by-side view — which is
where it earns the most: spokes on different hub versions are exactly the case a
single shared version misreports. Each column names the version it was measured
against, and every cell is compared to *that* version rather than to `HEAD`:

<pre><code>roteiro links --matrix --pinned --hub app --workspace ~/code

<span class="c"># cross-repo config overrides (hub: app, 3 spoke(s))</span>
<span class="c">#   resolved per spoke against the hub version each pins (2 of 3 pinned one):</span>
<span class="c">#     deploy-dev @ HEAD (no pin detected)</span>
<span class="c">#     deploy-eu @ v2.1.0</span>
<span class="c">#     deploy-web @ 4e0d5a6afd (via submodule app)</span></code></pre>

A spoke that pins nothing is **named**, not omitted, and the count says how many
pinned anything — so a workspace where nothing is detectable reads as
`0 of 3 pinned one` rather than looking identical to an ordinary run. The hub
column stays a single column showing `HEAD` and says so; a cell measured against
a different version carries that version beside it.

`--pinned` cannot be combined with `--hub-rev`: one version for every spoke and
each spoke's own version are opposite requests.

When image tags don't match your git tags, map them in `[pins]`
(`app = "release-{tag}"`, shown in [Config](config.html)). And if the hub's CI
publishes a graph artifact per release, resolution loads that instead of
re-extracting — so it resolves even against a shallow clone that lacks the pinned
commit's blobs.

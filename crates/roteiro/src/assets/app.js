// Roteiro workspace-explorer UI (PR 4 workspace view + PR 5 project drill-in).
// Hand-written, dependency-free ES beyond the vendored global `cytoscape`
// (loaded from /vendor/cytoscape.min.js). It consumes ONLY the read-only data
// API this same server exposes:
//   GET /v1/graph/workspaces                          — the workspace switcher
//   GET /v1/graph/workspaces/{ws}/topology            — hub + spokes + links
//   GET /v1/graph/workspaces/{ws}/matrix              — override matrix + drift
//   GET /v1/graph/workspaces/{ws}/{project}           — a project's nodes + edges
//   GET /v1/graph/workspaces/{ws}/{project}/hotspots  — most-called (by degree)
//   GET /v1/graph/workspaces/{ws}/{project}/debt      — intent-debt markers
//   GET /v1/graph/workspaces/{ws}/{project}/node/{key}— one node + its neighbours
//   GET /v1/graph/workspaces/{ws}/follow?qualified=…  — the follow-the-link hop
// The nested `/workspaces/{ws}/…` form is always used so a workspace is picked by
// name (collision-safe), independent of the server's flat-route default.
//
// TWO VIEWS, hash-routed (so drill/back is linkable and the browser back button
// works):
//   #/  or  #/workspace/{ws}                    → the cross-repo WORKSPACE view
//   #/workspace/{ws}/project/{project}          → the single-project GRAPH view
// Clicking a repo box, a matrix column header, or a project chip drills in; the
// breadcrumb "← Workspace" backs out. Clicking a spoke's app-key target (or its
// node-detail cross-repo chip) FOLLOWS the link into the hub project, centring the
// struct that defines the key and pushing a breadcrumb crumb so `←`/browser-back
// walk out hub → spoke → Workspace (PR 7). The (llama-backed) Ask tab is a later
// PR — a clean seam.

"use strict";

(function () {
  const $ = (sel) => document.querySelector(sel);
  const el = (tag, attrs, ...kids) => {
    const node = document.createElement(tag);
    if (attrs) {
      for (const [k, v] of Object.entries(attrs)) {
        if (k === "class") node.className = v;
        else if (k === "text") node.textContent = v;
        else if (k === "html") node.innerHTML = v;
        else if (k.startsWith("on") && typeof v === "function")
          node.addEventListener(k.slice(2), v);
        else if (v != null) node.setAttribute(k, v);
      }
    }
    for (const kid of kids) if (kid != null) node.append(kid);
    return node;
  };

  // Preferred config-section order; anything unseen is appended alphabetically.
  const SECTION_ORDER = ["SERVE", "WORKSPACE", "MODELS", "DEBT", "PATHS"];

  const state = {
    workspaces: [],
    current: null,
    cy: null,
    // Project drill-in view.
    project: null, // the project currently drilled into
    projectWs: null, // the workspace that project belongs to
    pcy: null, // the project graph's cytoscape instance
    pGraph: null, // the last-loaded raw project graph (re-render on toggle change)
    hideToolingConfig: false, // opt-in filter: hide build/tooling config_key nodes
    pRendered: null, // `${ws}/${project}` currently rendered (guards reloads)
    searching: false, // a find-in-repo filter is active (suppresses hover trace)
    // Cross-repo links for the drilled-into project (PR 6). Several spoke config
    // keys can point at the SAME app-key target (they share one external-ref node),
    // so links are indexed three ways, none of which collapses siblings:
    //   linkByEdge  — `${from} ${to}` (space-joined; node keys carry no spaces) →
    //                 the single link for that config→app-key EDGE, so each edge is
    //                 styled with ITS OWN provenance/drift.
    //   linksByRef  — external-ref (app-key) node key → link[] pointing INTO it.
    //   linksByFrom — spoke config_key node key → link[] going OUT of it.
    // Both `linksBy*` are arrays so the node detail panel shows every link, and the
    // app-key node's styling folds over all its inbound links. Empty for a non-spoke.
    links: [],
    linkByEdge: new Map(),
    linksByRef: new Map(),
    linksByFrom: new Map(),
    // Follow-the-link hop (PR 7). `trail` is the breadcrumb chain of project views
    // the current one was reached THROUGH — e.g. after hopping a spoke's app-key
    // into the hub it is `[{project: spoke}, {project: hub, focus}]`, so `▸`
    // crumbs and the `←` back button walk out hub → spoke → Workspace. `pendingNav`
    // carries that trail (and a node to centre) across the hash navigation a hop
    // triggers; `pendingFocus` is the node key to centre + inspect once the target
    // graph has rendered. All reset to a plain single-crumb trail on a fresh drill
    // or a browser back/forward (no pending hop), so history stays consistent.
    trail: [],
    pendingNav: null,
    pendingFocus: null,
    // Ask tab (graph-grounded chat). `ask` is the capability read from
    // `/v1/graph/capabilities` at startup — true only in a `serve` build that
    // mounts the chat endpoint; the llama-free explorer leaves it false and the
    // tab stays disabled. `askModels` are the served model ids; `asking` guards
    // against overlapping in-flight questions.
    ask: false,
    askModels: [],
    asking: false,
  };

  // -- data ------------------------------------------------------------------

  async function getJson(path) {
    const res = await fetch(path, { headers: { accept: "application/json" } });
    if (!res.ok) {
      let detail = "";
      try {
        detail = (await res.json()).error || "";
      } catch (_) {}
      throw new Error(`${res.status} ${path}${detail ? ` — ${detail}` : ""}`);
    }
    return res.json();
  }

  const wsPath = (ws, tail) =>
    `/v1/graph/workspaces/${encodeURIComponent(ws)}/${tail}`;

  // A cross-repo link's provenance drives its colour (gold authored / slate
  // inferred). Both topology links and matrix cells now carry `provenance`
  // directly from the edge (PR 5 backend fix); fall back to `inferred` only if an
  // older payload omits it.
  const cellProvenance = (cell) =>
    (cell && cell.provenance) || "inferred";

  const sectionOf = (hubKey) => {
    const head = String(hubKey).split(".")[0];
    return head ? head.toUpperCase() : "GENERAL";
  };

  // -- tooling-config classifier (mirror of rto-graph's `is_tooling_config_path`)
  //
  // The "hide tooling config" toggle is a CLIENT-SIDE, opt-in filter (default off,
  // so nothing is hidden until it's checked). It classifies a `config_key` node
  // from the file path baked into its `cfgkey:<file>#<dotted>` key, so no endpoint
  // change is needed. Keep this in lock-step with the Rust classifier in
  // `crates/rto-graph/src/config_keys.rs` — it lists the SAME well-known files.

  // The `<file>` component of a `cfgkey:<file>#<dotted>` node key, else null.
  // Neither the path nor the dotted key contains `#`, so the first `#` splits them.
  function cfgkeyFile(key) {
    const s = String(key);
    if (!s.startsWith("cfgkey:")) return null;
    const rest = s.slice("cfgkey:".length);
    const hash = rest.indexOf("#");
    return hash === -1 ? rest : rest.slice(0, hash);
  }

  // Whether a repo-relative config path is build / tooling / CI config rather than
  // application config. Conservative: an allow-list of well-known names/dirs, so
  // real app config is never hidden. Mirrors `rto_graph::is_tooling_config_path`.
  const TOOLING_CONFIG_BASENAMES = new Set([
    "cargo.toml",
    "cargo.lock",
    "rust-toolchain",
    "rust-toolchain.toml",
    "rustfmt.toml",
    ".rustfmt.toml",
    "clippy.toml",
    "deny.toml",
    "release-plz.toml",
    ".gitlab-ci.yml",
  ]);
  function isToolingConfigPath(path) {
    const segments = String(path).split("/").filter((s) => s && s !== ".");
    const base = (segments[segments.length - 1] || path).toLowerCase();
    // Directory-scoped: `.github/` (CI) and `.config/` (nextest & friends).
    if (segments.some((s) => s === ".github" || s === ".config")) return true;
    // `.cargo/config` or `.cargo/config.toml` — cargo's own build config.
    if (
      segments.length >= 2 &&
      segments[segments.length - 2] === ".cargo" &&
      (base === "config" || base === "config.toml")
    ) {
      return true;
    }
    return TOOLING_CONFIG_BASENAMES.has(base);
  }

  // A `config_key` node whose file classifies as tooling — the row/node hidden when
  // the toggle is on. Any non-config node is always kept.
  function isToolingConfigNode(node) {
    if (!node || node.kind !== "config_key") return false;
    const file = cfgkeyFile(node.key);
    return file != null && isToolingConfigPath(file);
  }

  // Persisted toggle state (default OFF — show everything). localStorage is
  // best-effort; a private-mode failure just falls back to per-session state.
  const HIDE_TOOLING_KEY = "roteiro.hideToolingConfig";
  function loadHideTooling() {
    try {
      return localStorage.getItem(HIDE_TOOLING_KEY) === "1";
    } catch (_) {
      return false;
    }
  }
  function saveHideTooling(on) {
    try {
      localStorage.setItem(HIDE_TOOLING_KEY, on ? "1" : "0");
    } catch (_) {}
  }

  // -- hash routing + navigation ---------------------------------------------

  // Parse `location.hash` into a route. An empty/unknown hash is the landing:
  // the workspace SELECTOR, from which a choice routes by type (see `route`).
  // The explicit `#/workspace/{ws}` (cross-repo view) and `.../project/{p}`
  // (drill-in) forms still deep-link straight in. Operates on the RAW hash — the
  // captured segments are decoded per-segment via the guarded `decode`, so a
  // malformed `%` sequence can never throw out here and blank the UI (`decodeURI`
  // over the whole hash could).
  function parseHash() {
    const h = location.hash.replace(/^#/, "");
    let m = h.match(/^\/workspace\/([^/]+)\/project\/(.+?)\/?$/);
    if (m)
      return {
        view: "project",
        ws: decode(m[1]),
        project: decode(m[2]),
      };
    m = h.match(/^\/workspace\/([^/]+)\/?$/);
    if (m) return { view: "workspace", ws: decode(m[1]) };
    return { view: "select" };
  }

  const decode = (s) => {
    try {
      return decodeURIComponent(s);
    } catch (_) {
      return s;
    }
  };

  // Navigate by writing the hash — the `hashchange` handler does the loading, so
  // in-app links and the browser back/forward buttons share one code path.
  function goProject(ws, project) {
    if (!project) return;
    location.hash = `#/workspace/${encodeURIComponent(ws)}/project/${encodeURIComponent(project)}`;
  }
  function goWorkspace(ws) {
    location.hash = ws ? `#/workspace/${encodeURIComponent(ws)}` : "#/";
  }

  // The hash a workspace routes to, BY TYPE: a real cross-repo workspace (MORE
  // THAN ONE project) opens the cross-repo workspace view; a single/standalone
  // repo (exactly one project) jumps STRAIGHT INTO that project's graph, skipping
  // the empty cross-repo chrome. A projectless workspace falls back to the
  // (empty) workspace view rather than nowhere.
  function hashByType(ws) {
    const projects = (ws && ws.projects) || [];
    if (projects.length === 1) {
      return `#/workspace/${encodeURIComponent(ws.name)}/project/${encodeURIComponent(projects[0])}`;
    }
    return `#/workspace/${encodeURIComponent(ws.name)}`;
  }

  // Route to a workspace by name, choosing the view by its project count (see
  // `hashByType`). Used by the selector cards AND the header workspace switcher,
  // so both entry points obey the same route-by-type rule. Writes the hash (a
  // history push), so the browser back button returns to the selector.
  function goByType(name) {
    const ws = state.workspaces.find((w) => w.name === name);
    if (!ws) return;
    location.hash = hashByType(ws);
  }

  // Drill from the workspace view (a repo box, a matrix column header, or a
  // project chip) into that project's graph view.
  function navigateToProject(project) {
    if (!project) return;
    goProject(state.current, project);
  }

  // -- follow-the-link hop (PR 7) --------------------------------------------

  // Follow a spoke app-key target INTO the hub project that defines it. Called
  // when an app-key node (or a node-detail cross-repo chip) is clicked. `refKey`
  // is the external-ref (app-key) node's key; its inbound links carry the
  // project-qualified hub target we ask the server to resolve+bridge.
  async function followHop(refKey) {
    const link = (state.linksByRef.get(refKey) || [])[0];
    if (!link) return;
    if (link.drift) return showDrift(link.toQualified);
    const ws = state.projectWs;
    setPStatus(`Following ${appKeyLabel(link)}…`);
    let res;
    try {
      res = await getJson(
        wsPath(ws, `follow?qualified=${encodeURIComponent(link.toQualified)}`)
      );
    } catch (err) {
      return setPStatus(String(err.message || err), true);
    }
    if (!res || res.drift || !res.target) return showDrift(link.toQualified);
    setPStatus("");
    // Push the current view onto the trail, then hop to the target project,
    // centring the returned node (the defining struct, or the config key) once it
    // renders. Navigation goes through the hash so browser back/forward also walk
    // the chain.
    const trail = [...currentTrail(), { ws, project: res.project, focus: res.target.key }];
    state.pendingNav = { ws, project: res.project, trail, focus: res.target.key };
    goProject(ws, res.project);
  }

  // A follow to a drifted / unresolved target does NOT navigate — the target does
  // not resolve in the hub. Drift means the hub key is gone (renamed/removed) OR
  // the hub project isn't hosted/synced in this workspace (the resolver maps both
  // to drift), so word it as "can't be resolved" rather than asserting non-existence.
  function showDrift(qualified) {
    const label = String(qualified).replace("::", " · ");
    setPStatus(
      `drift: ${label} can't be resolved in the hub — the key isn't defined, ` +
        `or the hub isn't hosted/synced. Nothing to follow.`,
      true
    );
  }

  // The trail for the current view, defaulting to a single self-crumb when the
  // view wasn't reached via an in-app hop (fresh drill, deep link, browser nav).
  function currentTrail() {
    if (state.trail.length) return state.trail;
    if (state.project) return [{ ws: state.projectWs, project: state.project }];
    return [];
  }

  // Jump back to breadcrumb crumb `i`, truncating the trail there and restoring
  // that crumb's focused node. Used by the `▸` crumb links and the `←` button.
  function crumbTo(i) {
    const target = state.trail[i];
    if (!target) return;
    state.pendingNav = {
      ws: target.ws,
      project: target.project,
      trail: state.trail.slice(0, i + 1),
      focus: target.focus || null,
    };
    goProject(target.ws, target.project);
  }

  // The `←` back affordance: walk OUT one level of a follow chain (hub → spoke),
  // else leave the project view for the workspace.
  function crumbBack() {
    if (state.trail.length > 1) crumbTo(state.trail.length - 2);
    else goWorkspace(state.projectWs || state.current);
  }

  // Render the breadcrumb from `state.trail`: Roteiro · Workspace ▸ spoke ▸ hub,
  // where every crumb but the last links back to that point and the last is the
  // current view. Rebuilt on each project load (so the Workspace link is re-bound).
  function renderCrumbs() {
    const nav = document.querySelector("#view-project .p-crumbs");
    if (!nav) return;
    const kids = [
      el("span", { class: "p-crumb-root", text: "Roteiro" }),
      el("span", { class: "p-sep", text: "·" }),
      el("button", {
        id: "p-crumb-ws",
        class: "p-crumb-link",
        type: "button",
        text: "Workspace",
        onclick: () => goWorkspace(state.projectWs || state.current),
      }),
    ];
    const trail = currentTrail();
    trail.forEach((c, i) => {
      kids.push(el("span", { class: "p-sep", text: "▸" }));
      if (i === trail.length - 1) {
        kids.push(
          el("span", { id: "p-crumb-project", class: "p-crumb-current", text: c.project })
        );
      } else {
        kids.push(
          el("button", {
            class: "p-crumb-link",
            type: "button",
            title: `back to ${c.project}`,
            text: c.project,
            onclick: () => crumbTo(i),
          })
        );
      }
    });
    nav.replaceChildren(...kids);
    // The `←` label follows the chain: one level out, or the workspace.
    const back = $("#p-back");
    if (back)
      back.textContent =
        trail.length > 1 ? `← ${trail[trail.length - 2].project}` : "← Workspace";
  }

  // Adopt `pendingNav`'s trail + focus when it matches the project being loaded,
  // else reset to a plain single-crumb trail (a fresh drill / browser navigation).
  function applyTrail(ws, project) {
    const p = state.pendingNav;
    if (p && p.ws === ws && p.project === project) {
      state.trail = p.trail;
      state.pendingFocus = p.focus || null;
    } else {
      state.trail = [{ ws, project }];
      state.pendingFocus = null;
    }
    state.pendingNav = null;
  }

  // -- status ----------------------------------------------------------------

  function setStatus(msg, isErr) {
    const s = $("#status");
    s.textContent = msg || "";
    s.className = isErr ? "err" : "";
  }

  // -- stat tiles ------------------------------------------------------------

  function renderTiles(topology, matrix) {
    const spokeRepos = (topology.spokes || []).length;
    const rows = matrix.rows || [];
    const appKeys = rows.length;
    const links = (topology.links || []).length;
    const overridden = rows.filter((r) =>
      Object.values(r.cells || {}).some((c) => c.differs)
    ).length;
    const drift = (matrix.drift || []).length;

    const tiles = [
      { num: spokeRepos, lbl: "deployment / spoke repos" },
      { num: appKeys, lbl: "app config keys" },
      { num: links, lbl: "cross-repo links" },
      { num: overridden, lbl: "keys overridden" },
      { num: drift, lbl: "drift references", drift: true },
    ];
    const host = $("#tiles");
    host.replaceChildren(
      ...tiles.map((t) =>
        el(
          "div",
          { class: t.drift ? "tile drift" : "tile" },
          el("div", { class: "num", text: String(t.num) }),
          el("div", { class: "lbl", text: t.lbl })
        )
      )
    );
  }

  // -- topology (cytoscape) --------------------------------------------------

  function renderTopology(topology) {
    const host = $("#topology");
    if (state.cy) {
      state.cy.destroy();
      state.cy = null;
    }
    host.replaceChildren();

    const hub = topology.hub;
    if (!hub) {
      host.append(
        el("div", {
          class: "empty",
          text: "No cross-repo hub — this workspace has no interlinked deployments.",
        })
      );
      return;
    }

    const elements = [];
    // The ids of the actually-hosted project boxes. `graph_api`'s topology
    // includes links whose `to` project is unhosted/drift (the external-ref is
    // reported even when it doesn't resolve), so a topology edge is drawn only
    // when BOTH endpoints are in this set — an unhosted target must never
    // phantom-create a node or dangle an edge. Drift is already surfaced in the
    // stat tiles and the matrix, so dropping it from the diagram loses nothing.
    const hosted = new Set();
    const addNode = (data) => {
      hosted.add(data.id);
      elements.push({ data });
    };
    addNode({
      id: `p:${hub}`,
      label: hub,
      role: "hub",
      sub: "app · source of truth",
      drift: 0,
    });
    for (const s of topology.spokes || []) {
      addNode({
        id: `p:${s.name}`,
        label: s.label || s.name,
        role: "spoke",
        sub: `${s.keyCount || 0} keys`,
        drift: s.driftCount || 0,
      });
    }
    // Edges: colour by provenance (gold authored / slate inferred). `from`/`to`
    // are qualified node keys `project::…`; map them back to project boxes and
    // keep only links whose BOTH endpoints are hosted — an O(1) Set lookup, and
    // the guard that skips unhosted/drift targets.
    const projOf = (qualified) => String(qualified).split("::")[0];
    const seen = new Set();
    for (const link of topology.links || []) {
      const source = `p:${projOf(link.from)}`;
      const target = `p:${projOf(link.to)}`;
      if (!hosted.has(source) || !hosted.has(target)) continue; // drift/unhosted
      const id = `e:${source}->${target}`;
      if (seen.has(id)) continue; // one edge per repo pair drives the picture
      seen.add(id);
      elements.push({
        data: {
          id,
          source,
          target,
          prov: link.provenance === "authored" ? "authored" : "inferred",
        },
      });
    }

    const cy = cytoscape({
      container: host,
      elements,
      wheelSensitivity: 0.2,
      style: [
        {
          selector: "node",
          style: {
            shape: "round-rectangle",
            "background-color": "#ffffff",
            "border-width": 1.5,
            "border-color": "#d1d5db",
            width: "label",
            height: "label",
            padding: "10px",
            label: (n) => `${n.data("label")}\n${n.data("sub")}`,
            "text-wrap": "wrap",
            "text-valign": "center",
            "text-halign": "center",
            "font-size": 12,
            "font-weight": 600,
            color: "#1a1f2e",
            "text-margin-y": 0,
          },
        },
        {
          selector: 'node[role = "hub"]',
          style: {
            "background-color": "#eef2ff",
            "border-color": "#c7d2fe",
            "border-width": 2,
            "font-size": 14,
          },
        },
        {
          selector: "node[drift > 0]",
          style: { "border-color": "#dc2626", "border-width": 2 },
        },
        {
          selector: "edge",
          style: {
            width: 2,
            "curve-style": "straight",
            "line-color": "#64748b",
            "target-arrow-color": "#64748b",
            "target-arrow-shape": "triangle",
            "arrow-scale": 0.9,
          },
        },
        {
          selector: 'edge[prov = "authored"]',
          style: { "line-color": "#c99a2e", "target-arrow-color": "#c99a2e", width: 3 },
        },
        {
          selector: ".faded",
          style: { opacity: 0.25 },
        },
        {
          selector: ".trace",
          style: { "border-color": "#4f46e5", "line-color": "#4f46e5" },
        },
      ],
      layout: {
        name: "concentric",
        concentric: (n) => (n.data("role") === "hub" ? 2 : 1),
        levelWidth: () => 1,
        minNodeSpacing: 60,
        padding: 30,
      },
    });

    // ⚠ drift badge as a small overlay node label suffix (kept textual so the
    // render stays dependency-light). Boxes with drift already show a red border.
    cy.nodes("[drift > 0]").forEach((n) => {
      n.data("label", `⚠ ${n.data("label")}`);
    });

    // Click a box → drill into that project's graph view.
    cy.on("tap", "node", (evt) => {
      const label = evt.target.data("label").replace(/^⚠ /, "");
      navigateToProject(label);
    });

    // Hover a deployment to trace it into the matrix (nice-to-have highlight).
    cy.on("mouseover", 'node[role = "spoke"]', (evt) => {
      const name = evt.target.data("label").replace(/^⚠ /, "");
      highlightSpoke(name);
      evt.target.connectedEdges().addClass("trace");
    });
    cy.on("mouseout", 'node[role = "spoke"]', () => {
      highlightSpoke(null);
      cy.elements().removeClass("trace");
    });

    state.cy = cy;
  }

  function highlightSpoke(name) {
    document.querySelectorAll("table.matrix .spoke-col").forEach((th) => {
      th.style.background = th.dataset.spoke === name ? "#fdf6e3" : "";
    });
  }

  // -- override matrix -------------------------------------------------------

  function renderMatrix(matrix) {
    const host = $("#matrix");
    const rows = matrix.rows || [];
    const spokes = matrix.spokes || [];
    const drift = matrix.drift || [];

    if (!rows.length && !drift.length) {
      host.replaceChildren(
        el("div", { class: "empty", text: "No cross-repo overrides or drift in this workspace." })
      );
      return;
    }

    const table = el("table", { class: "matrix" });

    // Header: config key | hub | one column per spoke (clickable drill intent).
    const headCells = [
      el("th", { scope: "col", text: "config key" }),
      el("th", { scope: "col", text: "app (hub)" }),
    ];
    for (const s of spokes) {
      headCells.push(
        el("th", {
          scope: "col",
          class: "spoke-col",
          "data-spoke": s,
          title: `open ${s}`,
          text: s,
          onclick: () => navigateToProject(s),
        })
      );
    }
    table.append(el("thead", null, el("tr", null, ...headCells)));

    const tbody = el("tbody");

    // Group override rows by config section ([SERVE]/[WORKSPACE]/…), ordered.
    const bySection = new Map();
    for (const r of rows) {
      const sec = sectionOf(r.hub_key);
      if (!bySection.has(sec)) bySection.set(sec, []);
      bySection.get(sec).push(r);
    }
    const orderedSections = [
      ...SECTION_ORDER.filter((s) => bySection.has(s)),
      ...[...bySection.keys()].filter((s) => !SECTION_ORDER.includes(s)).sort(),
    ];

    const colSpan = 2 + spokes.length;
    for (const sec of orderedSections) {
      tbody.append(
        el("tr", { class: "section-head" }, el("th", { colspan: colSpan, text: `[${sec}]` }))
      );
      for (const r of bySection.get(sec)) {
        const cells = [
          el("td", { class: "keycell" }, el("code", { text: r.hub_key })),
          el("td", { class: "hubval" }, el("code", { text: r.hub_value || "" })),
        ];
        for (const s of spokes) {
          const c = (r.cells || {})[s];
          if (!c) {
            cells.push(el("td", { class: "cell inherit", text: "·", title: "inherits the default" }));
            continue;
          }
          const prov = cellProvenance(c);
          cells.push(
            el(
              "td",
              { class: "cell set" },
              el("code", { text: c.value || "" }),
              el("span", { class: `tag ${prov}`, text: prov })
            )
          );
        }
        tbody.append(el("tr", null, ...cells));
      }
    }

    // DRIFT band pinned at the very bottom: keys set by a deploy but absent from
    // the app schema. Grouped as one red section, one row per orphan key.
    if (drift.length) {
      tbody.append(
        el(
          "tr",
          { class: "drift-band" },
          el("th", {
            colspan: colSpan,
            text: "DRIFT — SET BY A DEPLOY, NOT IN THE APP SCHEMA",
          })
        )
      );
      for (const d of drift) {
        const cells = [
          el("td", { class: "drift-key" }, el("code", { text: d.key })),
          el("td", { class: "hubval", text: "—" }),
        ];
        for (const s of spokes) {
          if (s === d.spoke) {
            cells.push(
              el("td", { class: "cell set" }, el("code", { text: d.value || "" }))
            );
          } else {
            cells.push(el("td", { class: "cell inherit", text: "·" }));
          }
        }
        tbody.append(el("tr", { class: "drift-row" }, ...cells));
      }
    }

    table.append(tbody);
    host.replaceChildren(table);

    // Cross-repo drift explanation box + the "how the links are made" footer.
    const parent = host.parentElement.parentElement; // .matrix-scroll → section.panel
    parent.querySelectorAll(".explain").forEach((n) => n.remove());
    if (drift.length) {
      parent.append(
        el("div", {
          class: "explain drift-box",
          html:
            `<strong>Cross-repo drift — ${drift.length} reference` +
            `${drift.length === 1 ? "" : "s"} to config the app doesn't define.</strong> ` +
            "A deployment sets these keys, but the app schema has no matching key — a " +
            "rename or removal in the app can't warn the deploy, so they silently drift.",
        })
      );
    }
    parent.append(
      el("div", {
        class: "explain",
        html:
          "<strong>How the links are made.</strong> " +
          '<i style="color:#64748b">Inferred</i> — a deploy key matched to an app key by ' +
          "name/content similarity, confidence-scored. " +
          '<i style="color:#c99a2e">Authored</i> — a link declared in the deploy repo that ' +
          "must not silently drift. " +
          '<i style="color:#dc2626">Drift</i> — a deploy key with no app counterpart at all.',
      })
    );
  }

  // -- workspace switching ---------------------------------------------------

  async function loadWorkspace(name) {
    // Whether this is a switch to a DIFFERENT workspace, or a same-workspace reload
    // (as `persistLinks` triggers after a successful write). Capture before mutating.
    const switched = state.current !== name;
    state.current = name;
    setStatus(`Loading ${name}…`);
    const ws = state.workspaces.find((w) => w.name === name);
    const badge = $("#ws-linkage");
    if (ws) {
      badge.textContent = ws.linked ? "linked · multi-repo" : "standalone";
      badge.className = ws.linked ? "ws-badge" : "ws-badge standalone";
    }
    // "Persist links" only makes sense for a linked (cross-repo) workspace; a
    // standalone repo has no hub to infer against. Only clear the persist note when
    // actually switching workspaces — a same-workspace reload (post-persist) must
    // keep the "Persisted …" success message the user just triggered.
    const persistBtn = $("#persist-links");
    if (persistBtn) persistBtn.hidden = !(ws && ws.linked);
    const persistNote = $("#persist-note");
    if (persistNote && switched) persistNote.textContent = "";
    renderProjectChips(ws ? ws.projects || [] : []);
    try {
      const [topology, matrix] = await Promise.all([
        getJson(wsPath(name, "topology")),
        getJson(wsPath(name, "matrix")),
      ]);
      renderTiles(topology, matrix);
      renderTopology(topology);
      renderMatrix(matrix);
      setStatus("");
    } catch (err) {
      setStatus(String(err.message || err), true);
    }
  }

  // Persist the inferred cross-repo links: POST `…/links/write` (the one mutating
  // endpoint), then reload the workspace so the topology/matrix re-render from the
  // now-durable edges. The hub view already shows these links LIVE; persisting makes
  // them durable, so the follow-the-link hop and `roteiro check` gates see them too.
  async function persistLinks() {
    const name = state.current;
    if (!name) return;
    const btn = $("#persist-links");
    const note = $("#persist-note");
    if (btn) btn.disabled = true;
    if (note) {
      note.textContent = "Persisting…";
      note.className = "ws-note";
    }
    try {
      const res = await fetch(wsPath(name, "links/write"), {
        method: "POST",
        headers: { accept: "application/json" },
      });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) {
        throw new Error((data && data.error) || `HTTP ${res.status}`);
      }
      const written = data.written || 0;
      if (note) {
        note.textContent = `Persisted ${written} inferred link${written === 1 ? "" : "s"}.`;
        note.className = "ws-note";
      }
      // Re-render from the now-durable edges (they read as persisted inferred links).
      await loadWorkspace(name);
    } catch (err) {
      if (note) {
        note.textContent = `Could not persist: ${err.message || err}`;
        note.className = "err";
      }
    } finally {
      if (btn) btn.disabled = false;
    }
  }

  function pickDefault(workspaces) {
    // Prefer a linked (cross-repo) workspace; else the sole/first one — the
    // server already defaults the flat routes to the cwd workspace, and a lone
    // config resolves itself, so "the sole/cwd workspace" is simply the default.
    const linked = workspaces.find((w) => w.linked);
    return (linked || workspaces[0]).name;
  }

  // Clickable chips to drill into any hosted project — the drill affordance for a
  // standalone repo (no cross-repo boxes to click) and a shortcut in a linked one.
  function renderProjectChips(projects) {
    const host = $("#projects-bar");
    if (!host) return;
    if (!projects.length) {
      host.replaceChildren();
      return;
    }
    const kids = [el("span", { class: "plabel", text: "drill into" })];
    for (const p of projects) {
      kids.push(
        el(
          "button",
          {
            class: "proj-chip",
            type: "button",
            title: `open ${p}`,
            onclick: () => navigateToProject(p),
          },
          p
        )
      );
    }
    host.replaceChildren(...kids);
  }

  // -- workspace-selector landing --------------------------------------------

  // Render the landing picker: one card per workspace, labelled by type — a
  // multi-repo "hub · N repos", or a "standalone" single repo. Clicking a card
  // routes by type (`goByType`): a hub opens the cross-repo view, a standalone
  // drills straight into its project. This is only rendered when there is a
  // genuine choice; a lone workspace auto-enters (see `route`).
  function renderSelector() {
    const grid = $("#select-grid");
    if (!grid) return;
    const status = $("#select-status");
    const cards = state.workspaces.map((w) => {
      const projects = w.projects || [];
      const multi = projects.length > 1;
      const badge = el("span", {
        class: multi ? "ws-badge" : "ws-badge standalone",
        text: multi ? `hub · ${projects.length} repos` : "standalone",
      });
      const kids = [
        el("div", { class: "name", text: w.name }),
        el("div", { class: "meta" }, badge),
      ];
      if (projects.length) {
        kids.push(
          el("div", {
            class: "projects",
            title: projects.join(", "),
            text: projects.join(" · "),
          })
        );
      }
      return el(
        "button",
        {
          class: "select-card",
          type: "button",
          title: multi ? `open ${w.name} (cross-repo)` : `open ${w.name}`,
          onclick: () => goByType(w.name),
        },
        ...kids
      );
    });
    grid.replaceChildren(...cards);
    if (status) status.textContent = "";
  }

  // ==========================================================================
  // Project graph view (drill-in) — PR 5
  // ==========================================================================

  // Node/edge colour by provenance (the legend's "colour: provenance").
  const PROV_COLOR = {
    derived: "#6ea8fe",
    authored: "#e0b64d",
    inferred: "#3fb6a8",
  };

  // Cross-repo link colours (PR 6): a spoke's config→app-key edges are DASHED and
  // coloured GOLD when authored, SLATE when inferred; a link whose target the app
  // no longer defines is DRIFT — a RED dashed edge to a `?` node.
  const LINK_COLOR = {
    authored: "#e0b64d", // gold
    inferred: "#8b95a3", // slate
    drift: "#f85149", // red
  };

  // Stash the drilled-into project's cross-repo links and index them per-edge (for
  // styling) and per-node (for the detail chips) — see the `state` comment. Keyed
  // so that multiple config keys pointing at the same app-key target never
  // overwrite one another. Empty maps for a non-spoke project (its `/links` is `[]`).
  function setProjectLinks(links) {
    state.links = links;
    state.linkByEdge = new Map();
    state.linksByRef = new Map();
    state.linksByFrom = new Map();
    const push = (map, key, val) => {
      if (key == null) return;
      const arr = map.get(key);
      if (arr) arr.push(val);
      else map.set(key, [val]);
    };
    for (const l of links) {
      if (l.from != null && l.to != null) state.linkByEdge.set(`${l.from} ${l.to}`, l);
      push(state.linksByRef, l.to, l);
      push(state.linksByFrom, l.from, l);
    }
  }

  // The screenshot label for an app-key target node: `<project>::<short key>`
  // (e.g. `roteiro::serve.addr`), from the link's project-qualified target and the
  // resolved hub key name (falling back to the short key when it drifts).
  function appKeyLabel(link) {
    const q = String(link.toQualified || "");
    const sep = q.indexOf("::");
    const proj = sep >= 0 ? q.slice(0, sep) : q;
    const rest = sep >= 0 ? q.slice(sep + 2) : q;
    return `${proj}::${link.toName || shortKey(rest)}`;
  }

  const hasWorkspace = (name) => state.workspaces.some((w) => w.name === name);

  // Percent-encode a node key for the `/{project}/node/{*key}` catch-all route,
  // preserving `/` as path separators (the wildcard matches slashes) while
  // encoding `#` and friends — mirrors how `resolve` is called with `%23`.
  const encodeKey = (key) => String(key).split("/").map(encodeURIComponent).join("/");

  // A node's short, human label: the part after the last `#`, else after the last
  // `/`, else the whole key. Used when a node carries no name.
  function shortKey(key) {
    const k = String(key);
    const hash = k.lastIndexOf("#");
    if (hash >= 0 && hash < k.length - 1) return k.slice(hash + 1);
    const slash = k.lastIndexOf("/");
    return slash >= 0 && slash < k.length - 1 ? k.slice(slash + 1) : k;
  }

  const pPane = (name) =>
    document.querySelector(`#view-project .p-pane[data-pane="${name}"]`);

  function setPStatus(msg, isErr) {
    const s = $("#p-status");
    s.textContent = msg || "";
    s.className = isErr ? "p-status err" : "p-status";
  }

  // -- view show / hide ------------------------------------------------------

  function showSelectView() {
    $("#view-project").hidden = true;
    $("#view-workspace").hidden = true;
    $("#view-select").hidden = false;
    document.body.classList.remove("on-project");
  }

  function showProjectView() {
    $("#view-select").hidden = true;
    $("#view-workspace").hidden = true;
    $("#view-project").hidden = false;
    document.body.classList.add("on-project");
  }

  function showWorkspaceView() {
    $("#view-select").hidden = true;
    $("#view-project").hidden = true;
    $("#view-workspace").hidden = false;
    document.body.classList.remove("on-project");
    // Free the (potentially ~1,300-node) project graph when backing out. Also
    // drop the cached RAW graph (`pGraph`, kept only so the "hide tooling config"
    // toggle can re-render without a refetch) so it can be GC'd off-view.
    if (state.pcy) {
      state.pcy.destroy();
      state.pcy = null;
    }
    state.pGraph = null;
    state.pRendered = null;
    state.searching = false;
    setProjectLinks([]);
  }

  // -- graph rendering -------------------------------------------------------

  // A force layout for small graphs (readable clusters); deterministic concentric
  // rings by degree for large ones, where cose would churn for seconds on a hub
  // project. Never animated — a 1,300-node animation would spin forever.
  function chooseLayout(count) {
    if (count > 400) {
      return {
        name: "concentric",
        concentric: (n) => n.degree(false),
        levelWidth: () => 6,
        minNodeSpacing: 6,
        padding: 20,
        animate: false,
      };
    }
    if (count > 0) {
      return {
        name: "cose",
        animate: false,
        padding: 20,
        nodeRepulsion: 8000,
        idealEdgeLength: 60,
        numIter: count > 150 ? 500 : 1000,
        randomize: true,
      };
    }
    return { name: "grid" };
  }

  function renderProjectGraph(graph) {
    const host = $("#p-graph");
    if (state.pcy) {
      state.pcy.destroy();
      state.pcy = null;
    }
    host.replaceChildren();

    // Keep the raw graph so toggling "hide tooling config" can re-render without a
    // refetch. The toggle is off by default, so the full graph is shown unless set.
    state.pGraph = graph;

    let nodes = graph.nodes || [];
    const edges = graph.edges || [];
    // Opt-in filter: drop tooling/CI `config_key` nodes. Edges are already pruned
    // to surviving node ids below, so a dropped node's edges vanish with it.
    if (state.hideToolingConfig) {
      nodes = nodes.filter((n) => !isToolingConfigNode(n));
    }
    const ids = new Set(nodes.map((n) => n.key));
    const elements = [];
    for (const n of nodes) {
      const data = {
        id: n.key,
        label: n.name || shortKey(n.key),
        kind: n.kind,
        prov: n.provenance || "derived",
      };
      // An external-ref node is a cross-repo APP-KEY TARGET — the hub key a spoke
      // config key sets. Render it as a distinct outlined box labelled
      // `<proj>::<key>`, gold/slate by the link's provenance; a target the app no
      // longer defines is DRIFT — a red `?` node (PR 6). It stays selectable but
      // inert (no follow-the-hop jump into the hub yet — PR 7 seam).
      if (n.kind === "external_ref") {
        // Every link into this target shares its resolution, so drift and the
        // `<proj>::<key>` label come from any of them; the border reads authored
        // (gold) when ANY inbound link is authored, else inferred (slate).
        const inbound = state.linksByRef.get(n.key) || [];
        const drift = inbound.some((l) => l.drift) ? 1 : 0;
        data.role = "appkey";
        data.drift = drift;
        data.linkprov = inbound.some((l) => l.provenance === "authored")
          ? "authored"
          : "inferred";
        data.label = drift ? "?" : inbound[0] ? appKeyLabel(inbound[0]) : shortKey(n.name);
      }
      elements.push({ data });
    }
    // One edge per (src, dst, kind); never dangle an edge onto an absent node.
    // The id joins percent-encoded endpoints with a plain `->` — encoding means
    // no segment can contain the separator, so the id stays collision-safe while
    // remaining fully printable.
    const seen = new Set();
    for (const e of edges) {
      if (!ids.has(e.src) || !ids.has(e.dst)) continue;
      const id = `e:${encodeURIComponent(e.src)}->${encodeURIComponent(e.dst)}->${e.kind}`;
      if (seen.has(id)) continue;
      seen.add(id);
      const data = { id, source: e.src, target: e.dst, prov: e.provenance || "derived" };
      // An edge into an app-key target is a CROSS-REPO LINK — draw it dashed and
      // coloured by THIS edge's own provenance (gold/slate), red when it drifts.
      // Keyed per-edge so a sibling edge into the same target can't recolour it.
      const link = state.linkByEdge.get(`${e.src} ${e.dst}`);
      if (link) {
        data.link = 1;
        data.drift = link.drift ? 1 : 0;
        data.linkprov = link.provenance;
      }
      elements.push({ data });
    }

    const count = nodes.length;
    // Above this size, labels are hidden by default (drawn only on hover/select/
    // match) so a hub project stays legible and renders responsively.
    const bigGraph = count > 200;
    const edgeBase = bigGraph
      ? {
          width: 1,
          "line-color": "#30363d",
          "curve-style": "haystack",
          "haystack-radius": 0,
          opacity: 0.55,
        }
      : {
          width: 1.2,
          "line-color": "#30363d",
          "curve-style": "straight",
          "target-arrow-shape": "triangle",
          "target-arrow-color": "#30363d",
          "arrow-scale": 0.7,
          opacity: 0.85,
        };

    const cy = cytoscape({
      container: host,
      elements,
      wheelSensitivity: 0.2,
      // A zoom ceiling/floor keeps a large graph navigable rather than lost.
      minZoom: 0.05,
      maxZoom: 3,
      style: [
        {
          selector: "node",
          style: {
            "background-color": (n) => PROV_COLOR[n.data("prov")] || "#8b949e",
            width: 14,
            height: 14,
            "border-width": 0,
            label: bigGraph ? "" : "data(label)",
            "font-size": 7,
            color: "#c9d1d9",
            "text-valign": "bottom",
            "text-halign": "center",
            "text-margin-y": 2,
            "min-zoomed-font-size": 7,
          },
        },
        { selector: "edge", style: edgeBase },
        {
          selector: 'edge[prov = "authored"]',
          style: { "line-color": "#e0b64d", "target-arrow-color": "#e0b64d" },
        },
        {
          selector: 'edge[prov = "inferred"]',
          style: { "line-color": "#3fb6a8", "target-arrow-color": "#3fb6a8" },
        },
        // -- cross-repo links (PR 6) — placed after the generic edge/node rules so
        //    the dashed link styling wins for the config→app-key edges/targets.
        {
          selector: 'node[role = "appkey"]',
          style: {
            shape: "round-rectangle",
            "background-color": "#161b22",
            "background-opacity": 0.95,
            "border-width": 1.5,
            "border-style": "dashed",
            "border-color": LINK_COLOR.inferred,
            width: "label",
            height: "label",
            padding: "6px",
            label: "data(label)",
            "font-size": 8,
            color: "#c9d1d9",
            "text-valign": "center",
            "text-halign": "center",
            "text-margin-y": 0,
            "min-zoomed-font-size": 0,
          },
        },
        {
          selector: 'node[role = "appkey"][linkprov = "authored"]',
          style: { "border-color": LINK_COLOR.authored },
        },
        {
          selector: 'node[role = "appkey"][drift = 1]',
          style: {
            shape: "ellipse",
            "border-color": LINK_COLOR.drift,
            color: LINK_COLOR.drift,
            "font-size": 13,
            "font-weight": 700,
            padding: "8px",
          },
        },
        {
          selector: "edge[link = 1]",
          style: {
            "curve-style": "straight",
            "line-style": "dashed",
            "line-color": LINK_COLOR.inferred,
            "target-arrow-color": LINK_COLOR.inferred,
            "target-arrow-shape": "triangle",
            "arrow-scale": 0.7,
            width: 1.5,
            opacity: 0.95,
          },
        },
        {
          selector: 'edge[link = 1][linkprov = "authored"]',
          style: {
            "line-color": LINK_COLOR.authored,
            "target-arrow-color": LINK_COLOR.authored,
          },
        },
        {
          selector: "edge[link = 1][drift = 1]",
          style: {
            "line-color": LINK_COLOR.drift,
            "target-arrow-color": LINK_COLOR.drift,
            width: 2,
          },
        },
        {
          selector: "node:selected",
          style: {
            "border-width": 3,
            "border-color": "#ffffff",
            label: "data(label)",
            "font-size": 9,
            "z-index": 30,
          },
        },
        { selector: "node.nb", style: { label: "data(label)", "z-index": 20 } },
        {
          selector: "node.match",
          style: {
            "border-width": 3,
            "border-color": "#f0c000",
            label: "data(label)",
            "font-size": 9,
            "z-index": 25,
          },
        },
        { selector: ".dim", style: { opacity: 0.12, "text-opacity": 0 } },
        {
          selector: "edge.trace",
          style: { "line-color": "#7c8cff", width: 2, opacity: 1 },
        },
      ],
      layout: chooseLayout(count),
    });

    state.pcy = cy;

    // The cross-repo link legend (dashed gold/slate + `?` drift) is meaningful only
    // for a spoke — show it only when this project actually has links.
    const xlegend = $("#p-legend-xrepo");
    if (xlegend) xlegend.hidden = state.links.length === 0;

    // Click a node → inspect it; click an app-key TARGET → follow the hop into the
    // hub that defines it (PR 7) instead of merely inspecting the placeholder.
    cy.on("tap", "node", (evt) => {
      const t = evt.target;
      if (t.data("role") === "appkey") followHop(t.id());
      else selectNode(t.id());
    });

    // Hover a node → trace its neighbourhood. Computed client-side over the
    // already-loaded graph (instant; never re-fetches), and suppressed while a
    // find-in-repo filter is active so the two highlights don't fight.
    cy.on("mouseover", "node", (evt) => {
      if (state.searching) return;
      const n = evt.target;
      const nb = n.closedNeighborhood();
      cy.batch(() => {
        cy.elements().addClass("dim");
        nb.removeClass("dim").addClass("nb");
        n.connectedEdges().removeClass("dim").addClass("trace");
      });
    });
    cy.on("mouseout", "node", () => {
      if (state.searching) return;
      cy.elements().removeClass("dim nb trace");
    });

    // Hover an app-key target → a "click to follow into the hub" tooltip (PR 7).
    cy.on("mouseover", 'node[role = "appkey"]', (evt) => {
      // Any inbound link describes the target (they share it); the tooltip is the
      // same for all edges into this app-key node.
      const link = (state.linksByRef.get(evt.target.id()) || [])[0];
      const label = link && !link.drift ? appKeyLabel(link) : "this key";
      const msg = link && link.drift
        ? "drift — can't resolve in the hub (undefined, or hub not hosted/synced)"
        : `click to follow ${label} → into the hub`;
      showFollowTip(evt, msg);
    });
    cy.on("mouseout", 'node[role = "appkey"]', hideFollowTip);
    cy.on("pan zoom", hideFollowTip);

    updateCounter();
    cy.ready(() => {
      cy.fit(undefined, 30);
      focusPending(cy);
    });
  }

  // After a follow-hop renders the target graph, centre + inspect the returned
  // node (the defining struct, or the config key on fallback). Consumed once. If
  // the node somehow isn't in this graph, say so rather than silently doing
  // nothing — a landed hop must always be legible.
  function focusPending(cy) {
    const key = state.pendingFocus;
    if (!key) return;
    state.pendingFocus = null;
    const n = cy.getElementById(key);
    if (n && n.nonempty()) {
      cy.elements().unselect();
      n.select();
      cy.animate(
        { center: { eles: n }, zoom: Math.min(1.2, cy.maxZoom()) },
        { duration: 250 }
      );
      activateTab("node");
      loadNodeDetail(state.projectWs, state.project, key);
    } else {
      setPStatus(`jumped to ${state.project}, but its ${shortKey(key)} node isn't in view.`);
    }
  }

  // A tiny hover tooltip over the graph canvas, positioned at the cursor. Used by
  // the app-key "follow → (coming soon)" seam; appended to the graph host (which
  // cytoscape gives `position: relative`), so its coordinates match the render.
  function showFollowTip(evt, text) {
    const host = $("#p-graph");
    if (!host) return;
    let tip = host.querySelector(".p-tip");
    if (!tip) {
      tip = el("div", { class: "p-tip" });
      host.appendChild(tip);
    }
    tip.textContent = text;
    const pos = evt.renderedPosition || { x: 0, y: 0 };
    tip.style.left = `${pos.x + 12}px`;
    tip.style.top = `${pos.y + 12}px`;
    tip.hidden = false;
  }

  function hideFollowTip() {
    const host = $("#p-graph");
    const tip = host && host.querySelector(".p-tip");
    if (tip) tip.hidden = true;
  }

  function updateCounter(matchCount) {
    const cy = state.pcy;
    const host = $("#p-counter");
    if (!cy) {
      host.textContent = "";
      return;
    }
    const nn = cy.nodes().length;
    const ne = cy.edges().length;
    let txt = `${nn} node${nn === 1 ? "" : "s"} · ${ne} edge${ne === 1 ? "" : "s"}`;
    if (typeof matchCount === "number")
      txt += ` · ${matchCount} match${matchCount === 1 ? "" : "es"}`;
    host.textContent = txt;
  }

  // -- controls: zoom / fit / search -----------------------------------------

  function zoomBy(factor) {
    const cy = state.pcy;
    if (!cy) return;
    cy.zoom({
      level: cy.zoom() * factor,
      renderedPosition: { x: cy.width() / 2, y: cy.height() / 2 },
    });
  }

  function fitGraph() {
    if (state.pcy) state.pcy.fit(undefined, 30);
  }

  let searchTimer = null;
  function onSearchInput(value) {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(() => runSearch(value), 120);
  }

  // Filter the *already-loaded* graph by name/key substring — highlight matches,
  // fade the rest. No fetch: the whole graph is in cytoscape, so this stays
  // responsive even for a hub project (per-node detail still uses the node
  // endpoint, so we never re-fetch the whole graph for a lookup).
  function runSearch(raw) {
    const cy = state.pcy;
    if (!cy) return;
    const q = raw.trim().toLowerCase();
    cy.batch(() => {
      cy.elements().removeClass("dim nb match trace");
      if (!q) {
        state.searching = false;
        updateCounter();
        return;
      }
      state.searching = true;
      const matches = cy
        .nodes()
        .filter(
          (n) =>
            n.data("label").toLowerCase().includes(q) ||
            n.id().toLowerCase().includes(q)
        );
      cy.elements().addClass("dim");
      matches.removeClass("dim").addClass("match");
      updateCounter(matches.length);
    });
  }

  // -- node selection + tabs -------------------------------------------------

  const tabDisabled = (b) => b.getAttribute("aria-disabled") === "true";

  // Switch tabs, keeping the full ARIA state in sync: roving `tabindex` and
  // `aria-selected` on the tabs, and `hidden`/visibility on their panels. A
  // disabled tab (Ask) is never activated. Pass `focusTab` when the switch came
  // from keyboard navigation so focus follows the selection.
  function activateTab(name, focusTab) {
    const target = document.querySelector(`#view-project .p-tab[data-tab="${name}"]`);
    if (!target || tabDisabled(target)) return;
    document.querySelectorAll("#view-project .p-tab").forEach((b) => {
      const active = b.dataset.tab === name;
      b.classList.toggle("active", active);
      b.setAttribute("aria-selected", active ? "true" : "false");
      b.tabIndex = active ? 0 : -1;
      if (active && focusTab) b.focus();
    });
    document.querySelectorAll("#view-project .p-pane").forEach((p) => {
      const active = p.dataset.pane === name;
      p.classList.toggle("active", active);
      p.hidden = !active;
    });
  }

  function selectNode(key) {
    const cy = state.pcy;
    if (cy) {
      const n = cy.getElementById(key);
      if (n && n.nonempty()) {
        cy.elements().unselect();
        n.select();
        cy.animate({ center: { eles: n } }, { duration: 200 });
      }
    }
    activateTab("node");
    loadNodeDetail(state.projectWs, state.project, key);
  }

  const graphNodeName = (key) => {
    const cy = state.pcy;
    if (cy) {
      const n = cy.getElementById(key);
      if (n && n.nonempty()) return n.data("label");
    }
    return shortKey(key);
  };

  const graphNodeProv = (key) => {
    const cy = state.pcy;
    if (cy) {
      const n = cy.getElementById(key);
      if (n && n.nonempty()) return n.data("prov");
    }
    return null;
  };

  // -- right panel: hotspots + intent debt -----------------------------------

  async function loadHotspots(ws, project) {
    const pane = pPane("hotspots");
    pane.replaceChildren(el("div", { class: "p-loading", text: "Loading hotspots…" }));
    const base = encodeURIComponent(project);
    try {
      const [hot, debt] = await Promise.all([
        getJson(wsPath(ws, `${base}/hotspots?limit=15`)),
        getJson(wsPath(ws, `${base}/debt`)),
      ]);
      // The user may have drilled elsewhere while this was in flight.
      if (state.project !== project || state.projectWs !== ws) return;
      renderHotspots(hot.hotspots || [], debt || {});
    } catch (err) {
      pane.replaceChildren(el("div", { class: "p-err", text: String(err.message || err) }));
    }
  }

  function renderHotspots(hotspots, debt) {
    const pane = pPane("hotspots");
    const kids = [];

    // Most-called (ranked/sized by degree).
    kids.push(
      el(
        "div",
        { class: "p-sec-title" },
        "Most-called ",
        el("span", { class: "hint", text: "ranked by degree" })
      )
    );
    if (!hotspots.length) {
      kids.push(el("div", { class: "p-empty", text: "— none in this repo —" }));
    } else {
      const maxDeg = hotspots[0].degree || 1;
      const list = el("ul", { class: "p-hot" });
      hotspots.forEach((h, i) => {
        const w = Math.max(4, Math.round((h.degree / maxDeg) * 100));
        list.append(
          el(
            "li",
            { title: h.key, onclick: () => selectNode(h.key) },
            el("span", { class: "p-hot-rank", text: String(i + 1) }),
            el(
              "span",
              { class: "p-hot-main" },
              el("div", { class: "p-hot-name", text: h.name || shortKey(h.key) }),
              el("div", { class: "p-hot-kind", text: h.kind || "" })
            ),
            el("span", { class: "p-hot-bar" }, el("i", { style: `width:${w}%` })),
            el("span", { class: "p-hot-deg", text: String(h.degree) })
          )
        );
      });
      kids.push(list);
    }

    // Intent debt, with each marker's category/text/line expandable.
    const items = (debt && debt.items) || [];
    kids.push(
      el(
        "div",
        { class: "p-sec-title" },
        "Intent debt ",
        el("span", {
          class: "hint",
          text: items.length ? `${debt.total} marker${debt.total === 1 ? "" : "s"}` : "",
        })
      )
    );
    if (!items.length) {
      kids.push(el("div", { class: "p-empty", text: "— none in this repo —" }));
    } else {
      const wrap = el("div", { class: "p-debt" });
      for (const it of items) {
        const loc = [it.path, it.line]
          .filter((x) => x != null && x !== "")
          .join(":");
        wrap.append(
          el(
            "details",
            null,
            el(
              "summary",
              null,
              el("span", { class: "p-debt-cat", text: it.category || "note" }),
              el("span", { class: "p-debt-loc", text: loc })
            ),
            el("div", { class: "p-debt-body" }, el("code", { text: it.text || "" }))
          )
        );
      }
      kids.push(wrap);
    }

    pane.replaceChildren(...kids);
  }

  // -- right panel: node detail ----------------------------------------------

  async function loadNodeDetail(ws, project, key) {
    const pane = pPane("node");
    pane.replaceChildren(el("div", { class: "p-loading", text: "Loading node…" }));
    try {
      const exp = await getJson(
        wsPath(ws, `${encodeURIComponent(project)}/node/${encodeKey(key)}`)
      );
      if (state.project !== project || state.projectWs !== ws) return;
      renderNodeDetail(exp);
    } catch (err) {
      pane.replaceChildren(el("div", { class: "p-err", text: String(err.message || err) }));
    }
  }

  // The cross-repo link chips for a node that participates in one or more: a spoke
  // config_key linking OUT to app-key target(s), and/or an app-key target linked TO
  // by spoke key(s). ALL links are shown — a config key may set several hub keys,
  // and a hub key may be set by several spoke keys. Returns the section's children,
  // or `null` when the node has no cross-repo link. Chips are inert pointers within
  // the spoke graph — the follow-the-hop jump into the hub is PR 7.
  function crossRepoSection(nodeKey) {
    const out = state.linksByFrom.get(nodeKey) || []; // this config key → app-key target(s)
    const inbound = state.linksByRef.get(nodeKey) || []; // this node IS an app-key target
    if (!out.length && !inbound.length) return null;

    const chips = el("div", { class: "p-chips" });
    for (const link of out) {
      const prov = link.drift ? "drift" : link.provenance;
      chips.append(
        el(
          "button",
          {
            class: `p-chip xrepo ${prov}`,
            type: "button",
            title: link.drift
              ? `drift → ${link.toQualified} — can't resolve in the hub (undefined, or hub not hosted/synced)`
              : `${link.provenance} link → ${link.toQualified} · click to follow into the hub`,
            // A live link follows the hop into the hub; a drift chip explains why
            // it can't (followHop → showDrift), never jumping to a wrong node.
            onclick: () => followHop(link.to),
          },
          link.drift ? "? drift" : appKeyLabel(link),
          el("span", { class: "p-chip-kind", text: ` ${prov}` })
        )
      );
    }
    for (const link of inbound) {
      const prov = link.drift ? "drift" : link.provenance;
      chips.append(
        el(
          "button",
          {
            class: `p-chip xrepo ${prov}`,
            type: "button",
            title: `${link.provenance} link from ${link.fromName}`,
            onclick: () => selectNode(link.from),
          },
          link.fromName,
          el("span", { class: "p-chip-kind", text: ` ${prov}` })
        )
      );
    }
    return [
      el("div", { class: "p-sec-title", text: "Cross-repo link" }),
      chips,
      el("div", {
        class: "p-follow-hint",
        text: out.length
          ? "click a link to follow it into the hub →"
          : "linked from a deployment spoke",
      }),
    ];
  }

  function renderNodeDetail(exp) {
    const pane = pPane("node");
    const node = exp.node || {};
    // Provenance isn't in the node summary; read it off the loaded graph node.
    const prov = graphNodeProv(node.key) || node.provenance || "derived";
    // An app-key target node's own name is the long project-qualified target; show
    // the compact `<proj>::<key>` label instead when we have a link into it (any of
    // its inbound links carries the same target).
    const appLink = (state.linksByRef.get(node.key) || [])[0];
    const displayName = appLink
      ? appKeyLabel(appLink)
      : node.name || shortKey(node.key);
    const kids = [
      el("div", { class: "p-node-name", text: displayName }),
      el(
        "div",
        { class: "p-node-meta" },
        el("span", { class: "p-badge", text: node.kind || "node" }),
        el("span", { class: `p-badge prov-${prov}`, text: prov })
      ),
    ];
    if (node.path) kids.push(el("div", { class: "p-node-path", text: node.path }));

    const xrepo = crossRepoSection(node.key);
    if (xrepo) kids.push(...xrepo);

    // Neighbour chips — clicking one navigates to that node.
    const chipRow = (title, refs) => {
      const chips = el("div", { class: "p-chips" });
      if (!refs.length) {
        chips.append(el("span", { class: "p-empty", text: "— none —" }));
      } else {
        for (const r of refs) {
          chips.append(
            el(
              "button",
              {
                class: "p-chip",
                type: "button",
                title: `${r.kind}${r.provenance ? ` · ${r.provenance}` : ""} → ${r.node}`,
                onclick: () => selectNode(r.node),
              },
              graphNodeName(r.node),
              el("span", { class: "p-chip-kind", text: ` ${r.kind}` })
            )
          );
        }
      }
      return [el("div", { class: "p-sec-title", text: title }), chips];
    };
    kids.push(...chipRow(`Outgoing (${(exp.outgoing || []).length})`, exp.outgoing || []));
    kids.push(...chipRow(`Incoming (${(exp.incoming || []).length})`, exp.incoming || []));

    pane.replaceChildren(...kids);
  }

  // -- project load ----------------------------------------------------------

  async function loadProject(ws, project) {
    state.projectWs = ws;
    state.project = project;
    state.pRendered = `${ws}/${project}`;
    state.searching = false;
    setProjectLinks([]); // cleared until this project's `/links` returns
    const search = $("#p-search");
    if (search) search.value = "";
    // Adopt the follow-hop trail (or a fresh single crumb) and draw the breadcrumb.
    applyTrail(ws, project);
    renderCrumbs();

    const wsEntry = state.workspaces.find((w) => w.name === ws);
    const badge = $("#p-linkage");
    if (wsEntry) {
      badge.textContent = wsEntry.linked ? "linked · multi-repo" : "standalone";
      badge.className = wsEntry.linked ? "ws-badge" : "ws-badge standalone";
    } else {
      badge.textContent = "";
    }

    activateTab("hotspots");
    pPane("node").replaceChildren(
      el("p", { class: "p-muted", text: "Click a node in the graph to inspect it." })
    );
    setPStatus(`Loading ${project}…`);
    try {
      // The graph and its cross-repo links load together: the links annotate which
      // config→app-key edges are gold/slate and which targets drift, so the graph
      // is styled in one pass. A spoke has links; a hub/plain repo gets `[]`.
      const base = encodeURIComponent(project);
      const [graph, linkData] = await Promise.all([
        getJson(wsPath(ws, base)),
        getJson(wsPath(ws, `${base}/links`)),
      ]);
      if (state.pRendered !== `${ws}/${project}`) return; // navigated away mid-flight
      setProjectLinks(linkData.links || []);
      renderProjectGraph(graph);
      setPStatus("");
      loadHotspots(ws, project);
    } catch (err) {
      setPStatus(String(err.message || err), true);
      $("#p-graph").replaceChildren();
      updateCounter();
    }
  }

  // -- right panel: Ask (graph-grounded chat, serve build only) --------------
  //
  // The Ask tab is disabled in the llama-free `roteiro explorer` build. A full
  // `roteiro serve --models` build (`--features serve,explorer`) mounts the chat
  // endpoint beside this data API and advertises it at `/v1/graph/capabilities`
  // (`ask:true` + served model ids). We read that ONE signal at startup and, when
  // Ask is available, enable the tab and wire it to the project-scoped chat route
  // with the graph tools on — so a local model answers in prose, calling
  // search/explain/path/debt over the drilled-into project's graph (ADR-0006).

  // The system prompt steers the served model to answer FROM the graph via its
  // tools and to cite node keys, which we then linkify back into the graph.
  const ASK_SYSTEM = (project) =>
    `You are a code assistant answering questions about the "${project}" project ` +
    `using its Roteiro knowledge graph. Prefer the provided graph tools ` +
    `(search, explain, path, debt) over guessing: search for relevant nodes, ` +
    `explain the keys you find, and ground every claim in them. Answer in concise ` +
    `prose and cite the node keys you used (e.g. \`fn:foo\`, \`file:src/main.rs\`).`;

  // Read the build's capability signal. Any failure (older/llama-free server with
  // no such route) leaves Ask disabled — the default — so this never breaks the
  // explorer build. Ask is enabled only when the chat endpoint is advertised AND
  // at least one model is actually served: with no model there is nothing to send
  // (`submitAsk` would post a request with no `model`), so we keep the disabled
  // stub — whose guidance already points at `roteiro serve --models` — rather than
  // letting a doomed request go out.
  async function loadCapabilities() {
    try {
      const caps = await getJson("/v1/graph/capabilities");
      const models = (caps && caps.models) || [];
      state.askModels = models;
      state.ask = !!(caps && caps.ask === true && models.length > 0);
    } catch (_) {
      state.ask = false;
      state.askModels = [];
    }
    if (state.ask) enableAskTab();
  }

  // Flip the Ask tab from its disabled stub to a live question form. Idempotent.
  function enableAskTab() {
    const tab = $("#p-tab-ask");
    if (tab) {
      tab.removeAttribute("aria-disabled");
      tab.title = "ask a question about this project";
    }
    const pane = pPane("ask");
    if (!pane) return;
    pane.classList.remove("p-ask-disabled");

    const answer = el("div", { class: "p-ask-answer", hidden: "" });
    const input = el("textarea", {
      id: "p-ask-input",
      rows: "3",
      placeholder: "Ask about this project — e.g. “what does the serve command do?”",
      "aria-label": "Question about this project",
    });
    const send = el("button", { class: "p-ask-send", type: "submit", text: "Ask" });
    const modelNote = state.askModels.length
      ? el("span", { class: "p-ask-model", text: `model: ${state.askModels[0]}` })
      : el("span", { class: "p-ask-model" });

    const form = el(
      "form",
      {
        class: "p-ask-form",
        onsubmit: (e) => {
          e.preventDefault();
          submitAsk(input.value, answer, send);
        },
      },
      input,
      el("div", { class: "p-ask-row" }, modelNote, send)
    );
    // Ctrl/Cmd+Enter submits from the textarea (Enter alone inserts a newline).
    input.addEventListener("keydown", (e) => {
      if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
        e.preventDefault();
        submitAsk(input.value, answer, send);
      }
    });

    pane.replaceChildren(
      el("div", { class: "p-ask" }, form, answer)
    );
  }

  // Post the question to the project-scoped chat endpoint (graph tools on) and
  // render the prose answer, linkifying any node keys the model cited.
  async function submitAsk(raw, answer, send) {
    const question = String(raw || "").trim();
    if (!question) return;
    if (state.asking) return;
    if (!state.project) {
      showAnswer(answer, "Drill into a project first, then ask about it.", true);
      return;
    }
    state.asking = true;
    if (send) send.disabled = true;
    answer.hidden = false;
    answer.className = "p-ask-answer";
    answer.replaceChildren(el("span", { class: "p-loading", text: "Thinking…" }));

    const project = state.project;
    const body = {
      model: state.askModels[0],
      messages: [
        { role: "system", content: ASK_SYSTEM(project) },
        { role: "user", content: question },
      ],
      stream: false,
    };
    try {
      const res = await fetch(
        `/v1/${encodeURIComponent(project)}/chat/completions`,
        {
          method: "POST",
          headers: { "content-type": "application/json", accept: "application/json" },
          body: JSON.stringify(body),
        }
      );
      const data = await res.json().catch(() => ({}));
      if (!res.ok) {
        throw new Error(askError(data, res.status));
      }
      // The user may have drilled elsewhere while the model was thinking.
      if (state.project !== project) return;
      const content =
        (data.choices &&
          data.choices[0] &&
          data.choices[0].message &&
          data.choices[0].message.content) ||
        "(the model returned an empty answer)";
      renderAnswer(answer, content);
    } catch (err) {
      showAnswer(answer, String((err && err.message) || err), true);
    } finally {
      state.asking = false;
      if (send) send.disabled = false;
    }
  }

  // Extract a human message from a `/v1` error body (`{error:{message}}` from the
  // model endpoint, or a plain `{error:"…"}`), adding the pull-a-model hint when
  // the failure is a missing/unknown model — the guidance the serve path emits.
  function askError(data, status) {
    let msg = "";
    if (data && data.error) {
      msg = typeof data.error === "string" ? data.error : data.error.message || "";
    }
    msg = msg || `request failed (${status})`;
    if (/model|not served|pull/i.test(msg)) {
      msg += " — pull a model first: `roteiro model pull qwen3-0.6b`";
    }
    return msg;
  }

  function showAnswer(answer, text, isErr) {
    answer.hidden = false;
    answer.className = isErr ? "p-ask-answer p-err" : "p-ask-answer";
    answer.replaceChildren(text);
  }

  // Node keys the model cites (`fn:foo`, `file:src/main.rs`, `sym:rust:…#x`) —
  // `prefix:body`, where the body runs to the first whitespace/quote. Skips web
  // URLs (http/https/mailto), which share the `word:` shape but aren't graph keys.
  const KEY_RE = /\b([a-z][a-z0-9_]{1,24}):([A-Za-z0-9_./#:@+-]{2,})/g;
  const URL_PREFIX = /^(https?|mailto|ftp|ws|wss)$/i;
  // End-of-sentence punctuation the regex would otherwise pull into the key
  // (`See file:src/main.rs.` → the trailing `.`). Trimmed off before linkifying
  // and preserved as plain text after the link.
  const KEY_TRAIL_PUNCT = /[.,;:)\]}!?]+$/;

  // One clickable, keyboard-activatable link to a cited node key. `label` is the
  // visible text (the key itself inline, a short form in the list); `title` the
  // hover text. Shared by the inline links and the "referenced:" list so both
  // activate identically on click AND Enter/Space (they carry role="link").
  function keyLink(key, label, title) {
    return el("a", {
      role: "link",
      tabindex: "0",
      text: label,
      title: title || key,
      onclick: () => askGoToNode(key),
      onkeydown: (e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          askGoToNode(key);
        }
      },
    });
  }

  // Render the answer as prose with cited node keys turned into links that select
  // the node in the current project graph (surfacing which nodes were referenced).
  function renderAnswer(answer, text) {
    answer.hidden = false;
    answer.className = "p-ask-answer";
    const frag = document.createDocumentFragment();
    const refs = new Set();
    let last = 0;
    let m;
    KEY_RE.lastIndex = 0;
    while ((m = KEY_RE.exec(text)) !== null) {
      const raw = m[0];
      if (URL_PREFIX.test(m[1])) continue; // a URL, not a graph key
      const key = raw.replace(KEY_TRAIL_PUNCT, "");
      // Trimmed down to just a prefix (e.g. `file:...`) — not a real key; leave the
      // whole token as plain text (don't advance `last` past it).
      if (!key || key.endsWith(":")) continue;
      if (m.index > last) frag.append(text.slice(last, m.index));
      refs.add(key);
      frag.append(keyLink(key, key, `inspect ${key}`));
      // Preserve any trailing punctuation the match swallowed as plain text, and
      // advance the cursor by the FULL original match length.
      const trailing = raw.slice(key.length);
      if (trailing) frag.append(trailing);
      last = m.index + raw.length;
    }
    if (last < text.length) frag.append(text.slice(last));

    const kids = [el("div", {}, frag)];
    if (refs.size) {
      const list = el("div", { class: "p-ask-refs" }, "referenced: ");
      let first = true;
      refs.forEach((key) => {
        if (!first) list.append(", ");
        first = false;
        list.append(keyLink(key, shortKey(key), key));
      });
      kids.push(list);
    }
    answer.replaceChildren(...kids);
  }

  // Jump to a cited node: select it in the graph (centres + opens the Node tab if
  // it's present), reusing the same path a graph tap takes.
  function askGoToNode(key) {
    selectNode(key);
  }

  // -- router ----------------------------------------------------------------

  async function route() {
    const r = parseHash();
    // The landing is the workspace selector — UNLESS there is only one workspace
    // total, in which case there is nothing to choose: auto-enter it by type,
    // replacing the empty hash so the back button doesn't bounce off the (skipped)
    // selector. The same route-by-type rule then dispatches to the right view.
    if (r.view === "select") {
      if (state.workspaces.length === 1) {
        const target = hashByType(state.workspaces[0]);
        history.replaceState(null, "", location.pathname + location.search + target);
        return route();
      }
      showSelectView();
      renderSelector();
      return;
    }
    if (r.view === "project" && r.project) {
      const ws = r.ws && hasWorkspace(r.ws) ? r.ws : pickDefault(state.workspaces);
      showProjectView();
      if (state.pRendered !== `${ws}/${r.project}`) await loadProject(ws, r.project);
      return;
    }
    showWorkspaceView();
    const ws = r.ws && hasWorkspace(r.ws) ? r.ws : pickDefault(state.workspaces);
    const sel = $("#workspace");
    if (sel && sel.value !== ws) sel.value = ws;
    if (state.current !== ws) await loadWorkspace(ws);
  }

  // One-time wiring for the project view's static controls.
  function wireProjectControls() {
    // `←` walks OUT one hop of a follow chain, else back to the workspace. The
    // Workspace crumb (`#p-crumb-ws`) is (re)bound in `renderCrumbs`, which rebuilds
    // the breadcrumb per load, so it isn't wired here.
    $("#p-back").addEventListener("click", crumbBack);
    $("#p-zoom-in").addEventListener("click", () => zoomBy(1.25));
    $("#p-zoom-out").addEventListener("click", () => zoomBy(0.8));
    $("#p-fit").addEventListener("click", fitGraph);
    $("#p-search").addEventListener("input", (e) => onSearchInput(e.target.value));

    // "Hide tooling config" toggle: opt-in, persisted, default OFF (shows all).
    // Restore the saved state, reflect it in the checkbox, and on change re-render
    // the current project graph from the cached raw graph (no refetch), re-applying
    // any active find-in-repo filter so the toggle composes with search.
    const hideToggle = $("#p-hide-tooling");
    if (hideToggle) {
      state.hideToolingConfig = loadHideTooling();
      hideToggle.checked = state.hideToolingConfig;
      hideToggle.addEventListener("change", (e) => {
        state.hideToolingConfig = e.target.checked;
        saveHideTooling(state.hideToolingConfig);
        if (state.pGraph) {
          renderProjectGraph(state.pGraph);
          const q = $("#p-search");
          if (q && q.value) onSearchInput(q.value);
        }
      });
    }

    // ARIA tabs: click activates an enabled tab; arrow/Home/End keys move focus
    // between enabled tabs and activate on the move (automatic activation),
    // skipping the disabled Ask tab.
    const tablist = document.querySelector("#view-project .p-tabs");
    const tabs = Array.from(tablist.querySelectorAll(".p-tab"));
    tabs.forEach((b) => {
      b.addEventListener("click", () => {
        if (!tabDisabled(b)) activateTab(b.dataset.tab);
      });
    });
    tablist.addEventListener("keydown", (e) => {
      const enabled = tabs.filter((t) => !tabDisabled(t));
      if (!enabled.length) return;
      let idx = enabled.indexOf(document.activeElement);
      if (idx < 0) idx = enabled.findIndex((t) => t.classList.contains("active"));
      if (idx < 0) idx = 0;
      let next;
      switch (e.key) {
        case "ArrowRight":
          next = enabled[(idx + 1) % enabled.length];
          break;
        case "ArrowLeft":
          next = enabled[(idx - 1 + enabled.length) % enabled.length];
          break;
        case "Home":
          next = enabled[0];
          break;
        case "End":
          next = enabled[enabled.length - 1];
          break;
        default:
          return;
      }
      e.preventDefault();
      activateTab(next.dataset.tab, true);
    });
  }

  async function init() {
    try {
      const workspaces = await getJson("/v1/graph/workspaces");
      state.workspaces = workspaces;
      if (!workspaces.length) {
        const s = $("#select-status");
        if (s) {
          s.textContent = "No workspaces to show.";
          s.className = "err";
        }
        return;
      }
      const sel = $("#workspace");
      sel.replaceChildren(
        ...workspaces.map((w) =>
          el("option", { value: w.name, text: `${w.name}${w.linked ? "" : " (standalone)"}` })
        )
      );
      // Switching the header workspace selector routes BY TYPE — the same rule the
      // landing cards use — so a single-repo pick jumps straight into its graph
      // rather than the empty cross-repo view. Navigates by hash so it's linkable.
      sel.addEventListener("change", () => goByType(sel.value));
      const persistBtn = $("#persist-links");
      if (persistBtn) persistBtn.addEventListener("click", persistLinks);
      wireProjectControls();
      // Enable the Ask tab iff this build serves the chat endpoint (serve build).
      await loadCapabilities();
      window.addEventListener("hashchange", () => {
        route();
      });
      await route();
    } catch (err) {
      // The selector landing is what's on screen at load, so surface the failure
      // there (its `#status` is `#select-status`).
      const s = $("#select-status");
      if (s) {
        s.textContent = `Could not load workspaces: ${err.message || err}`;
        s.className = "err";
      }
    }
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }
})();

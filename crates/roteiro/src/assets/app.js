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
// The nested `/workspaces/{ws}/…` form is always used so a workspace is picked by
// name (collision-safe), independent of the server's flat-route default.
//
// TWO VIEWS, hash-routed (so drill/back is linkable and the browser back button
// works):
//   #/  or  #/workspace/{ws}                    → the cross-repo WORKSPACE view
//   #/workspace/{ws}/project/{project}          → the single-project GRAPH view
// Clicking a repo box, a matrix column header, or a project chip drills in; the
// breadcrumb "← Workspace" backs out. The cross-repo spoke-link rendering and the
// follow-the-link hop, and the (llama-backed) Ask tab, are later PRs — clean seams.

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
    pRendered: null, // `${ws}/${project}` currently rendered (guards reloads)
    searching: false, // a find-in-repo filter is active (suppresses hover trace)
    // Cross-repo links for the drilled-into project (PR 6). `linkByRef` is keyed by
    // the external-ref (app-key target) node key; `linkByFrom` by the spoke
    // config_key node key — so both the graph styling and the node detail panel can
    // look a link up in O(1). Empty for a non-spoke project.
    links: [],
    linkByRef: new Map(),
    linkByFrom: new Map(),
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

  // -- hash routing + navigation ---------------------------------------------

  // Parse `location.hash` into a route. Unknown/empty hashes fall back to the
  // workspace view with no explicit workspace (the default is chosen on load).
  // Operates on the RAW hash — the captured segments are decoded per-segment via
  // the guarded `decode`, so a malformed `%` sequence can never throw out here
  // and blank the UI (`decodeURI` over the whole hash could).
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
    return { view: "workspace", ws: null };
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

  // Drill from the workspace view (a repo box, a matrix column header, or a
  // project chip) into that project's graph view.
  function navigateToProject(project) {
    if (!project) return;
    goProject(state.current, project);
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
    state.current = name;
    setStatus(`Loading ${name}…`);
    const ws = state.workspaces.find((w) => w.name === name);
    const badge = $("#ws-linkage");
    if (ws) {
      badge.textContent = ws.linked ? "linked · multi-repo" : "standalone";
      badge.className = ws.linked ? "ws-badge" : "ws-badge standalone";
    }
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

  // Stash the drilled-into project's cross-repo links and index them two ways: by
  // the app-key (external-ref) node key they point at, and by the spoke config_key
  // node key they start from. Both the graph styling and the node detail panel
  // read these. Empty maps for a non-spoke project (its `/links` is `[]`).
  function setProjectLinks(links) {
    state.links = links;
    state.linkByRef = new Map();
    state.linkByFrom = new Map();
    for (const l of links) {
      if (l.to) state.linkByRef.set(l.to, l);
      if (l.from) state.linkByFrom.set(l.from, l);
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

  function showProjectView() {
    $("#view-workspace").hidden = true;
    $("#view-project").hidden = false;
    document.body.classList.add("on-project");
  }

  function showWorkspaceView() {
    $("#view-project").hidden = true;
    $("#view-workspace").hidden = false;
    document.body.classList.remove("on-project");
    // Free the (potentially ~1,300-node) project graph when backing out.
    if (state.pcy) {
      state.pcy.destroy();
      state.pcy = null;
    }
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

    const nodes = graph.nodes || [];
    const edges = graph.edges || [];
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
        const link = state.linkByRef.get(n.key);
        data.role = "appkey";
        data.drift = link && link.drift ? 1 : 0;
        data.linkprov = link ? link.provenance : "inferred";
        data.label = data.drift ? "?" : link ? appKeyLabel(link) : shortKey(n.name);
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
      // coloured by the link's provenance (gold/slate), red when it drifts.
      const link = state.linkByRef.get(e.dst);
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

    // Click a node → inspect it in the NODE tab.
    cy.on("tap", "node", (evt) => selectNode(evt.target.id()));

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

    // Hover an app-key target → a subtle "follow → (coming soon)" tooltip. The
    // cross-repo follow-the-hop jump into the hub is PR 7; this is its inert seam.
    cy.on("mouseover", 'node[role = "appkey"]', (evt) => {
      const link = state.linkByRef.get(evt.target.id());
      const label = link && !link.drift ? appKeyLabel(link) : "this key";
      const msg = link && link.drift
        ? "drift — the app defines no such key"
        : `follow ${label} → (coming soon)`;
      showFollowTip(evt, msg);
    });
    cy.on("mouseout", 'node[role = "appkey"]', hideFollowTip);
    cy.on("pan zoom", hideFollowTip);

    updateCounter();
    cy.ready(() => cy.fit(undefined, 30));
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

  // The cross-repo link chip(s) for a node that participates in one: a spoke
  // config_key linking OUT to an app-key target, and/or an app-key target linked
  // TO by a spoke key. Returns the section's children, or `null` when the node has
  // no cross-repo link (so a plain node shows nothing extra). Chips are inert
  // pointers within the spoke graph — the follow-the-hop jump into the hub is PR 7.
  function crossRepoSection(nodeKey) {
    const out = state.linkByFrom.get(nodeKey); // this config key → an app-key target
    const inbound = state.linkByRef.get(nodeKey); // this node IS an app-key target
    if (!out && !inbound) return null;

    const chips = el("div", { class: "p-chips" });
    if (out) {
      const prov = out.drift ? "drift" : out.provenance;
      chips.append(
        el(
          "button",
          {
            class: `p-chip xrepo ${prov}`,
            type: "button",
            title: out.drift
              ? `drift → ${out.toQualified} — the app defines no such key`
              : `${out.provenance} link → ${out.toQualified} · follow (coming soon)`,
            onclick: () => selectNode(out.to),
          },
          out.drift ? "? drift" : appKeyLabel(out),
          el("span", { class: "p-chip-kind", text: ` ${prov}` })
        )
      );
    }
    if (inbound) {
      const prov = inbound.drift ? "drift" : inbound.provenance;
      chips.append(
        el(
          "button",
          {
            class: `p-chip xrepo ${prov}`,
            type: "button",
            title: `${inbound.provenance} link from ${inbound.fromName}`,
            onclick: () => selectNode(inbound.from),
          },
          inbound.fromName,
          el("span", { class: "p-chip-kind", text: ` ${prov}` })
        )
      );
    }
    return [
      el("div", { class: "p-sec-title", text: "Cross-repo link" }),
      chips,
      el("div", { class: "p-follow-hint", text: "follow → (coming soon)" }),
    ];
  }

  function renderNodeDetail(exp) {
    const pane = pPane("node");
    const node = exp.node || {};
    // Provenance isn't in the node summary; read it off the loaded graph node.
    const prov = graphNodeProv(node.key) || node.provenance || "derived";
    // An app-key target node's own name is the long project-qualified target; show
    // the compact `<proj>::<key>` label instead when we have the link.
    const appLink = state.linkByRef.get(node.key);
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
    $("#p-crumb-project").textContent = project;

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

  // -- router ----------------------------------------------------------------

  async function route() {
    const r = parseHash();
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
    $("#p-back").addEventListener("click", () =>
      goWorkspace(state.projectWs || state.current)
    );
    $("#p-crumb-ws").addEventListener("click", () =>
      goWorkspace(state.projectWs || state.current)
    );
    $("#p-zoom-in").addEventListener("click", () => zoomBy(1.25));
    $("#p-zoom-out").addEventListener("click", () => zoomBy(0.8));
    $("#p-fit").addEventListener("click", fitGraph);
    $("#p-search").addEventListener("input", (e) => onSearchInput(e.target.value));

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
        setStatus("No workspaces to show.", true);
        return;
      }
      const sel = $("#workspace");
      sel.replaceChildren(
        ...workspaces.map((w) =>
          el("option", { value: w.name, text: `${w.name}${w.linked ? "" : " (standalone)"}` })
        )
      );
      // Selecting a workspace navigates by hash so the choice is linkable.
      sel.addEventListener("change", () => goWorkspace(sel.value));
      wireProjectControls();
      window.addEventListener("hashchange", () => {
        route();
      });
      await route();
    } catch (err) {
      setStatus(`Could not load workspaces: ${err.message || err}`, true);
    }
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }
})();

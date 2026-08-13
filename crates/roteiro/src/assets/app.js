// Roteiro workspace-explorer UI (PR 4). Hand-written, dependency-free ES beyond
// the vendored global `cytoscape` (loaded from /vendor/cytoscape.min.js). It
// consumes ONLY the read-only data API this same server exposes:
//   GET /v1/graph/workspaces                       — the workspace switcher
//   GET /v1/graph/workspaces/{ws}/topology         — hub + spokes + links
//   GET /v1/graph/workspaces/{ws}/matrix           — override matrix + drift
// The nested `/workspaces/{ws}/…` form is always used so a workspace is picked by
// name (collision-safe), independent of the server's flat-route default.
//
// SCOPE: the cross-repo WORKSPACE view only. Clicking a repo box or a matrix
// column header emits a *navigation intent* (`navigateToProject`) — the per-
// project drill-in view is a later PR, so this is a deliberate, clean seam.

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

  const state = { workspaces: [], current: null, cy: null };

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
  // inferred). Topology links carry `provenance` directly; matrix cells only
  // carry a confidence, so a declared (confidence 1.0) link reads as authored —
  // the seam to switch to explicit per-cell provenance when the API adds it.
  const cellProvenance = (cell) =>
    cell && cell.confidence >= 1 ? "authored" : "inferred";

  const sectionOf = (hubKey) => {
    const head = String(hubKey).split(".")[0];
    return head ? head.toUpperCase() : "GENERAL";
  };

  // -- navigation seam (later PR) --------------------------------------------

  function navigateToProject(project) {
    // Emit an intent only: update the hash route and surface it. The target
    // per-project drill-in view lands in a later PR; this leaves the seam clean.
    if (!project) return;
    const ws = state.current;
    location.hash = `#/workspace/${encodeURIComponent(ws)}/project/${encodeURIComponent(project)}`;
    setStatus(`→ open ${project} (drill-in view is a later PR)`);
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

    // Drift per spoke, so a hub-referencing box can show a ⚠ badge.
    const driftBy = {};
    for (const s of topology.spokes || []) driftBy[s.name] = s.driftCount || 0;

    const elements = [];
    elements.push({
      data: {
        id: `p:${hub}`,
        label: hub,
        role: "hub",
        sub: "app · source of truth",
        drift: 0,
      },
    });
    for (const s of topology.spokes || []) {
      elements.push({
        data: {
          id: `p:${s.name}`,
          label: s.label || s.name,
          role: "spoke",
          sub: `${s.keyCount || 0} keys`,
          drift: s.driftCount || 0,
        },
      });
    }
    // Edges: colour by provenance (gold authored / slate inferred). `from`/`to`
    // are qualified node keys `project::…`; map them back to project boxes.
    const projOf = (qualified) => String(qualified).split("::")[0];
    const seen = new Set();
    for (const link of topology.links || []) {
      const src = projOf(link.from);
      const dst = projOf(link.to);
      const id = `e:${src}->${dst}`;
      if (seen.has(id)) continue; // one edge per repo pair drives the picture
      seen.add(id);
      if (!elements.some((e) => e.data.id === `p:${src}`)) continue;
      elements.push({
        data: {
          id,
          source: `p:${src}`,
          target: `p:${dst}`,
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

    // Click a box → emit the drill intent (target view is a later PR).
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
      sel.addEventListener("change", () => loadWorkspace(sel.value));
      const def = pickDefault(workspaces);
      sel.value = def;
      await loadWorkspace(def);
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

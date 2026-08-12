//! Cross-repo **config override matrix + drift view** (ADR-0009 step 7, "views").
//!
//! The workspace overview the prototype validated: a grid of the hub's config
//! keys against each spoke repo, showing which spoke overrides which key (and to
//! what), plus a **drift** list of spoke keys the hub doesn't define. Built from
//! the same inferred matches `roteiro links --infer` produces, then rendered as a
//! self-contained HTML page (`--html`), a text table, or JSON.
//!
//! Pure data + rendering: the caller feeds in the already-matched inputs, so this
//! module has no workspace/graph dependencies and is fully unit-testable.

use std::collections::BTreeMap;

/// One matched override fed into the matrix.
pub struct MatchInput {
    /// The hub config key this override maps to.
    pub hub_key: String,
    /// The spoke's own key (its naming convention).
    pub spoke_key: String,
    /// The spoke's value for it.
    pub spoke_value: String,
    /// Match confidence in `0.0..=1.0`.
    pub confidence: f64,
}

/// One spoke's contribution to the matrix.
pub struct SpokeInput {
    /// The spoke project (repo dir name).
    pub name: String,
    /// Its matched overrides against the hub.
    pub matches: Vec<MatchInput>,
    /// Its orphan `(key, value)`s — no hub counterpart (the drift candidates).
    pub orphans: Vec<(String, String)>,
}

/// One spoke's overriding value for a hub key.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Cell {
    /// The spoke's value.
    pub value: String,
    /// The spoke's key (may differ in convention from the hub's).
    pub spoke_key: String,
    /// Match confidence.
    pub confidence: f64,
    /// The spoke value differs from the hub's default — a *real* override, not a
    /// redundant restatement. The signal a reader scans for.
    pub differs: bool,
}

/// One hub key's row: its default value and each spoke's override of it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Row {
    /// The hub config key.
    pub hub_key: String,
    /// The hub's own value (the default the spokes override).
    pub hub_value: String,
    /// Overriding cell per spoke name (only spokes that override this key).
    pub cells: BTreeMap<String, Cell>,
}

/// A spoke key with no hub counterpart — the drift candidate.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Drift {
    /// The spoke project.
    pub spoke: String,
    /// The orphan key.
    pub key: String,
    /// Its value.
    pub value: String,
}

/// The assembled cross-repo override matrix.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OverrideMatrix {
    /// The hub project (source of truth).
    pub hub: String,
    /// Spoke column order (only spokes that override at least one hub key).
    pub spokes: Vec<String>,
    /// One row per overridden hub key, sorted by key.
    pub rows: Vec<Row>,
    /// Orphan spoke keys (drift), sorted by `(spoke, key)`.
    pub drift: Vec<Drift>,
}

/// Assemble the matrix from each spoke's matches and orphans. `hub_values` maps a
/// hub key to its value (to flag which overrides actually *differ*). Deterministic:
/// rows sorted by hub key, columns and drift sorted by name.
#[must_use]
pub fn build(
    hub: &str,
    hub_values: &BTreeMap<String, String>,
    spokes: Vec<SpokeInput>,
) -> OverrideMatrix {
    let mut rows: BTreeMap<String, Row> = BTreeMap::new();
    let mut columns: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut drift: Vec<Drift> = Vec::new();

    for spoke in spokes {
        for m in spoke.matches {
            let hub_value = hub_values.get(&m.hub_key).cloned().unwrap_or_default();
            let differs = hub_value != m.spoke_value;
            let row = rows.entry(m.hub_key.clone()).or_insert_with(|| Row {
                hub_key: m.hub_key.clone(),
                hub_value: hub_value.clone(),
                cells: BTreeMap::new(),
            });
            row.cells.insert(
                spoke.name.clone(),
                Cell {
                    value: m.spoke_value,
                    spoke_key: m.spoke_key,
                    confidence: m.confidence,
                    differs,
                },
            );
            columns.insert(spoke.name.clone());
        }
        for (key, value) in spoke.orphans {
            drift.push(Drift {
                spoke: spoke.name.clone(),
                key,
                value,
            });
        }
    }
    drift.sort_by(|a, b| (&a.spoke, &a.key).cmp(&(&b.spoke, &b.key)));

    OverrideMatrix {
        hub: hub.to_owned(),
        spokes: columns.into_iter().collect(),
        rows: rows.into_values().collect(),
        drift,
    }
}

/// Whether the matrix has nothing to show (no overrides and no drift).
#[must_use]
pub fn is_empty(m: &OverrideMatrix) -> bool {
    m.rows.is_empty() && m.drift.is_empty()
}

/// Render the matrix as a plain-text table for the terminal.
#[must_use]
pub fn render_text(m: &OverrideMatrix) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "cross-repo config overrides (hub: {}, {} spoke(s))",
        m.hub,
        m.spokes.len()
    );
    for row in &m.rows {
        let _ = writeln!(out, "\n  {} = {}", row.hub_key, row.hub_value);
        for spoke in &m.spokes {
            if let Some(cell) = row.cells.get(spoke) {
                let flag = if cell.differs { "≠" } else { "=" };
                let _ = writeln!(
                    out,
                    "    {flag} {spoke}: {} ({:.2})",
                    cell.value, cell.confidence
                );
            }
        }
    }
    if !m.drift.is_empty() {
        let _ = writeln!(out, "\n  drift — {} orphan key(s):", m.drift.len());
        for d in &m.drift {
            let _ = writeln!(out, "    {}: {} = {}", d.spoke, d.key, d.value);
        }
    }
    out
}

/// Render the matrix as a **self-contained** HTML page (inline CSS, no external
/// assets) — the `render web-graph` output: open it straight in a browser.
#[must_use]
pub fn render_html(m: &OverrideMatrix) -> String {
    use std::fmt::Write as _;
    let mut thead = String::from("<th scope=\"col\">config key</th><th scope=\"col\">hub</th>");
    for s in &m.spokes {
        let _ = write!(thead, "<th scope=\"col\">{}</th>", esc(s));
    }

    let mut tbody = String::new();
    for row in &m.rows {
        let _ = write!(
            tbody,
            "<tr><th scope=\"row\"><code>{}</code></th><td class=\"hub\"><code>{}</code></td>",
            esc(&row.hub_key),
            esc(&row.hub_value)
        );
        for spoke in &m.spokes {
            match row.cells.get(spoke) {
                Some(cell) => {
                    let cls = if cell.differs {
                        "cell over"
                    } else {
                        "cell same"
                    };
                    let _ = write!(
                        tbody,
                        "<td class=\"{cls}\"><code>{}</code>\
                         <span class=\"conf\" title=\"confidence\">{:.2}</span></td>",
                        esc(&cell.value),
                        cell.confidence
                    );
                }
                None => tbody.push_str("<td class=\"cell none\">·</td>"),
            }
        }
        tbody.push_str("</tr>");
    }

    let drift = if m.drift.is_empty() {
        String::new()
    } else {
        let mut rows = String::new();
        for d in &m.drift {
            let _ = write!(
                rows,
                "<tr><td>{}</td><td><code>{}</code></td><td><code>{}</code></td></tr>",
                esc(&d.spoke),
                esc(&d.key),
                esc(&d.value)
            );
        }
        format!(
            "<h2>Drift — {} orphan key(s)</h2>\
             <p class=\"muted\">Spoke keys with no hub counterpart: the app doesn't \
             define these, so a rename or removal in the hub can't warn you.</p>\
             <table class=\"drift\"><thead><tr><th scope=\"col\">spoke</th>\
             <th scope=\"col\">key</th><th scope=\"col\">value</th></tr></thead>\
             <tbody>{rows}</tbody></table>",
            m.drift.len()
        )
    };

    let body = if is_empty(m) {
        "<p class=\"muted\">No cross-repo config overrides or drift found.</p>".to_owned()
    } else {
        format!(
            "<table class=\"matrix\"><thead><tr>{thead}</tr></thead><tbody>{tbody}</tbody></table>\
             <p class=\"legend\"><span class=\"swatch over\"></span> overrides the hub value \
             &nbsp; <span class=\"swatch same\"></span> matches it (redundant) \
             &nbsp; <span class=\"swatch none\"></span> not set</p>{drift}"
        )
    };

    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>Cross-repo config overrides — {hub}</title><style>{CSS}</style></head><body>\
         <main><h1>Cross-repo config overrides</h1>\
         <p class=\"muted\">Hub <strong>{hub}</strong> · {nspokes} spoke(s) · ADR-0009</p>\
         {body}</main></body></html>",
        hub = esc(&m.hub),
        nspokes = m.spokes.len(),
    )
}

/// Minimal, theme-aware, self-contained stylesheet for the overview page.
const CSS: &str = "\
:root{--bg:#fff;--fg:#1a1a2e;--muted:#6b7280;--line:#e5e7eb;--hub:#f3f4f6;\
--over:#fef3c7;--over-fg:#92400e;--same:#ecfdf5;--same-fg:#065f46;--accent:#4f46e5}\
@media(prefers-color-scheme:dark){:root{--bg:#0f1117;--fg:#e5e7eb;--muted:#9ca3af;\
--line:#262b36;--hub:#1a1d27;--over:#3b2f10;--over-fg:#fcd34d;--same:#0f2a1f;\
--same-fg:#6ee7b7;--accent:#a5b4fc}}\
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--fg);\
font:15px/1.5 ui-sans-serif,system-ui,-apple-system,Segoe UI,Roboto,sans-serif}\
main{max-width:1100px;margin:0 auto;padding:2rem 1.25rem}\
h1{font-size:1.5rem;margin:0 0 .25rem}h2{font-size:1.15rem;margin:2rem 0 .5rem}\
.muted{color:var(--muted);margin:.25rem 0 1.5rem}\
code{font:13px/1.4 ui-monospace,SFMono-Regular,Menlo,monospace}\
table{border-collapse:collapse;width:100%;overflow-x:auto;display:block}\
@media(min-width:720px){table{display:table}}\
th,td{border:1px solid var(--line);padding:.4rem .6rem;text-align:left;vertical-align:top}\
thead th{position:sticky;top:0;background:var(--bg);font-size:.8rem;\
text-transform:uppercase;letter-spacing:.03em;color:var(--muted)}\
tbody th[scope=row]{background:var(--hub);white-space:nowrap}\
td.hub{background:var(--hub);color:var(--muted)}\
td.cell{white-space:nowrap}td.over{background:var(--over);color:var(--over-fg)}\
td.same{background:var(--same);color:var(--same-fg)}td.none{color:var(--muted);text-align:center}\
.conf{display:inline-block;margin-left:.4rem;font-size:.7rem;opacity:.7;\
font-variant-numeric:tabular-nums}\
.legend{color:var(--muted);font-size:.85rem;margin:1rem 0}\
.swatch{display:inline-block;width:.8rem;height:.8rem;border-radius:3px;\
vertical-align:-1px;border:1px solid var(--line)}\
.swatch.over{background:var(--over)}.swatch.same{background:var(--same)}\
.swatch.none{background:var(--bg)}\
table.drift td:first-child{white-space:nowrap;color:var(--muted)}";

/// Escape text for HTML body/attribute content.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matrix() -> OverrideMatrix {
        let hub_values = BTreeMap::from([
            ("serve.addr".to_owned(), "127.0.0.1:8017".to_owned()),
            ("serve.tools".to_owned(), "true".to_owned()),
        ]);
        let spokes = vec![SpokeInput {
            name: "deploy".to_owned(),
            matches: vec![
                MatchInput {
                    hub_key: "serve.addr".to_owned(),
                    spoke_key: "SERVE_ADDR".to_owned(),
                    spoke_value: "0.0.0.0:8443".to_owned(), // differs → override
                    confidence: 0.9,
                },
                MatchInput {
                    hub_key: "serve.tools".to_owned(),
                    spoke_key: "SERVE_TOOLS".to_owned(),
                    spoke_value: "true".to_owned(), // same → redundant
                    confidence: 0.98,
                },
            ],
            orphans: vec![("MAX_CONNECTIONS".to_owned(), "512".to_owned())],
        }];
        build("app", &hub_values, spokes)
    }

    #[test]
    fn build_pivots_matches_into_rows_and_flags_real_overrides() {
        let m = matrix();
        assert_eq!(m.hub, "app");
        assert_eq!(m.spokes, vec!["deploy".to_owned()]);
        assert_eq!(m.rows.len(), 2);
        let addr = m.rows.iter().find(|r| r.hub_key == "serve.addr").unwrap();
        assert!(
            addr.cells["deploy"].differs,
            "different value is an override"
        );
        let tools = m.rows.iter().find(|r| r.hub_key == "serve.tools").unwrap();
        assert!(!tools.cells["deploy"].differs, "equal value is redundant");
        assert_eq!(m.drift.len(), 1);
        assert_eq!(m.drift[0].key, "MAX_CONNECTIONS");
    }

    #[test]
    fn render_html_is_self_contained_and_escapes() {
        let m = matrix();
        let html = render_html(&m);
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("<style>"), "inline CSS, no external asset");
        assert!(!html.contains("href=\"style.css\""));
        assert!(html.contains("serve.addr") && html.contains("0.0.0.0:8443"));
        assert!(html.contains("MAX_CONNECTIONS"), "drift is shown");
        // The differing override is class `over`, the redundant one `same`.
        assert!(html.contains("cell over") && html.contains("cell same"));
    }

    #[test]
    fn render_html_escapes_injected_markup() {
        let hub_values = BTreeMap::from([("k".to_owned(), "<v>".to_owned())]);
        let m = build("app", &hub_values, vec![]);
        let html = render_html(&m);
        assert!(!html.contains("<v>"), "hub value must be escaped");
    }

    #[test]
    fn text_table_marks_overrides_and_lists_drift() {
        let t = render_text(&matrix());
        assert!(t.contains("serve.addr = 127.0.0.1:8017"));
        assert!(t.contains("≠ deploy: 0.0.0.0:8443"));
        assert!(t.contains("= deploy: true"));
        assert!(t.contains("drift") && t.contains("MAX_CONNECTIONS"));
    }
}
